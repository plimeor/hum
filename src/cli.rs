//! Command definitions and handlers.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::{ArgValueCandidates, CompletionCandidate};

use crate::app::{self, App, Severity};
use crate::config::{self, Command, Restart, Service};
use crate::{environ, logfile, tomledit};

/// Exit with this status without printing anything more.
#[derive(Debug)]
pub struct Exit(pub i32);

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit status {}", self.0)
    }
}

impl std::error::Error for Exit {}

fn service_candidates() -> Vec<CompletionCandidate> {
    config::Paths::from_env()
        .and_then(|p| p.service_names())
        .unwrap_or_default()
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

fn names_arg() -> ArgValueCandidates {
    ArgValueCandidates::new(service_candidates)
}

#[derive(Parser)]
#[command(
    name = "hum",
    about = "Run background commands as launchd services, configured by TOML files"
)]
pub struct Cli {
    /// Output JSON (ls, show)
    #[arg(long, global = true)]
    json: bool,
    /// Skip confirmation prompts
    #[arg(short = 'y', long = "yes", global = true)]
    yes: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args)]
struct Targets {
    #[arg(add = names_arg())]
    names: Vec<String>,
    /// All services
    #[arg(long)]
    all: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a service config and start it
    #[command(
        after_help = "Examples:\n  hum new t3 -- t3 server\n  hum new api --cwd ~/work/api --env PORT=4000 --env-file .env --restart on-failure -- bun run start"
    )]
    New {
        name: String,
        /// Working directory (default: current directory)
        #[arg(long)]
        cwd: Option<String>,
        /// Environment variable KEY=VALUE (repeatable)
        #[arg(long = "env", value_name = "KEY=VALUE")]
        env: Vec<String>,
        /// Env file, relative to --cwd (repeatable)
        #[arg(long = "env-file", value_name = "PATH")]
        env_file: Vec<String>,
        /// Restart policy
        #[arg(long, value_enum, default_value_t = RestartArg::Always)]
        restart: RestartArg,
        /// Do not start at login
        #[arg(long)]
        no_autostart: bool,
        /// Write the config without starting the service
        #[arg(long)]
        no_start: bool,
        #[arg(last = true, required = true, value_name = "CMD")]
        command: Vec<String>,
    },
    /// Edit a config in $EDITOR, validate it, apply it
    Edit {
        #[arg(add = names_arg(), required_unless_present = "global", conflicts_with = "global")]
        name: Option<String>,
        /// Edit config.toml
        #[arg(long)]
        global: bool,
        /// Do not restart the service if it is running
        #[arg(long)]
        no_restart: bool,
    },
    /// Change config fields from the command line
    #[command(
        after_help = "Examples:\n  hum set t3 env.PORT=4000 restart=on-failure\n  hum set t3 env_file=.env,.env.local\n  hum set t3 command -- t3 server --port 1"
    )]
    Set {
        #[arg(add = names_arg())]
        name: String,
        #[arg(required = true, value_name = "KEY=VALUE")]
        pairs: Vec<String>,
        /// Do not restart the service if it is running
        #[arg(long)]
        no_restart: bool,
        #[arg(last = true, value_name = "CMD")]
        command: Vec<String>,
    },
    /// Remove config fields
    Unset {
        #[arg(add = names_arg())]
        name: String,
        #[arg(required = true, value_name = "KEY")]
        keys: Vec<String>,
        /// Do not restart the service if it is running
        #[arg(long)]
        no_restart: bool,
    },
    /// Stop and delete services
    Rm {
        #[command(flatten)]
        targets: Targets,
        /// Unregister but keep the config file
        #[arg(long)]
        keep_config: bool,
    },
    /// Start services (and clear a previous stop)
    Start(Targets),
    /// Stop services and keep them stopped after login
    Stop(Targets),
    /// Regenerate plists and restart services
    Restart(Targets),
    /// List services and their state
    Ls,
    /// Show the resolved config, environment and launchd state
    Show {
        #[arg(add = names_arg())]
        name: String,
        /// Show environment variable values
        #[arg(long)]
        reveal: bool,
    },
    /// Show, follow or rotate logs
    Logs {
        #[command(flatten)]
        targets: Targets,
        /// Keep printing new lines
        #[arg(short = 'f', long)]
        follow: bool,
        /// Number of lines to show
        #[arg(short = 'n', long, default_value_t = 100)]
        lines: usize,
        /// Rotate now, regardless of size
        #[arg(long)]
        rotate: bool,
        /// With --rotate: only rotate logs at or over max_size (used by the periodic job)
        #[arg(long, hide = true)]
        if_oversize: bool,
    },
    /// Align launchd with the config directory
    Apply {
        /// Only list the changes
        #[arg(long)]
        dry_run: bool,
    },
    /// Check for common problems
    Doctor,
    /// Run a service in the foreground (what launchd invokes)
    Run {
        #[arg(add = names_arg())]
        name: String,
    },
    /// Print a shell completion script
    Completion { shell: Shell },
}

#[derive(Clone, Copy, ValueEnum)]
enum RestartArg {
    Always,
    OnFailure,
    Never,
}

#[derive(Clone, Copy, ValueEnum)]
enum Shell {
    Fish,
    Zsh,
    Bash,
}

pub fn run(cli: Cli) -> Result<()> {
    // `hum run` is what launchd invokes, with launchd's short PATH, so it
    // must not be the one to capture PATH.
    if let Cmd::Completion { shell } = cli.cmd {
        return completion(shell);
    }
    let app = App::new()?;
    if !matches!(cli.cmd, Cmd::Run { .. })
        && config::ensure_global(&app.paths, &std::env::var("PATH").unwrap_or_default())?
    {
        eprintln!(
            "created {} with the current shell's PATH",
            app.paths.global_file().display()
        );
    }
    let ctx = Ctx {
        app,
        json: cli.json,
        yes: cli.yes,
    };
    match cli.cmd {
        Cmd::New {
            name,
            cwd,
            env,
            env_file,
            restart,
            no_autostart,
            no_start,
            command,
        } => {
            let restart = match restart {
                RestartArg::Always => Restart::Always,
                RestartArg::OnFailure => Restart::OnFailure,
                RestartArg::Never => Restart::Never,
            };
            ctx.new_service(
                &name,
                cwd,
                &env,
                env_file,
                restart,
                !no_autostart,
                !no_start,
                command,
            )
        }
        Cmd::Edit {
            name,
            global,
            no_restart,
        } => ctx.edit(name.as_deref(), global, no_restart),
        Cmd::Set {
            name,
            pairs,
            no_restart,
            command,
        } => ctx.set(&name, &pairs, &command, no_restart),
        Cmd::Unset {
            name,
            keys,
            no_restart,
        } => ctx.unset(&name, &keys, no_restart),
        Cmd::Rm {
            targets,
            keep_config,
        } => ctx.rm(&targets, keep_config),
        Cmd::Start(t) => ctx.each(&ctx.targets(&t)?, |n| ctx.app.start(n)),
        Cmd::Stop(t) => ctx.each(&ctx.targets(&t)?, |n| ctx.app.stop(n)),
        Cmd::Restart(t) => ctx.each(&ctx.targets(&t)?, |n| ctx.app.restart(n)),
        Cmd::Ls => ctx.ls(),
        Cmd::Show { name, reveal } => ctx.show(&name, reveal),
        Cmd::Logs {
            targets,
            follow,
            lines,
            rotate,
            if_oversize,
        } => ctx.logs(&targets, follow, lines, rotate, if_oversize),
        Cmd::Apply { dry_run } => ctx.apply(dry_run),
        Cmd::Doctor => ctx.doctor(),
        Cmd::Run { name } => ctx.run_service(&name),
        Cmd::Completion { .. } => unreachable!(),
    }
}

fn completion(shell: Shell) -> Result<()> {
    use clap_complete::env::{Bash, EnvCompleter, Fish, Zsh};
    let completer: &dyn EnvCompleter = match shell {
        Shell::Fish => &Fish,
        Shell::Zsh => &Zsh,
        Shell::Bash => &Bash,
    };
    completer.write_registration("COMPLETE", "hum", "hum", "hum", &mut std::io::stdout())?;
    Ok(())
}

type Validator<'a> = Box<dyn Fn(&str) -> Result<()> + 'a>;

struct Ctx {
    app: App,
    json: bool,
    yes: bool,
}

fn warn_missing_env_files(app: &App, s: &Service) {
    for f in s.resolved_env_files(&app.paths.home) {
        if !f.exists() {
            eprintln!(
                "warning: env_file {} does not exist; `hum run` will refuse to start until it does",
                f.display()
            );
        }
    }
}

fn read_line() -> String {
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line.trim().to_ascii_lowercase()
}

impl Ctx {
    fn targets(&self, t: &Targets) -> Result<Vec<String>> {
        match (t.all, t.names.is_empty()) {
            (true, false) => bail!("give service names or --all, not both"),
            (true, true) => {
                let names = self.app.paths.service_names()?;
                if names.is_empty() {
                    bail!("no services configured");
                }
                Ok(names)
            }
            (false, true) => bail!("give at least one service name, or --all"),
            (false, false) => {
                for n in &t.names {
                    config::validate_name(n)?;
                }
                Ok(t.names.clone())
            }
        }
    }

    /// Runs `f` for every name, reporting failures and continuing.
    fn each(&self, names: &[String], f: impl Fn(&str) -> Result<()>) -> Result<()> {
        let mut failed = 0;
        for n in names {
            if let Err(e) = f(n) {
                eprintln!("hum: {n}: {e:#}");
                failed += 1;
            }
        }
        match failed {
            0 => Ok(()),
            _ if names.len() == 1 => Err(Exit(1).into()),
            _ => bail!("{failed} of {} failed", names.len()),
        }
    }

    fn confirm(&self, prompt: &str) -> bool {
        if self.yes {
            return true;
        }
        eprint!("{prompt} [y/N] ");
        matches!(read_line().as_str(), "y" | "yes")
    }

    #[allow(clippy::too_many_arguments)]
    fn new_service(
        &self,
        name: &str,
        cwd: Option<String>,
        env: &[String],
        env_file: Vec<String>,
        restart: Restart,
        autostart: bool,
        start: bool,
        command: Vec<String>,
    ) -> Result<()> {
        let paths = &self.app.paths;
        config::validate_name(name)?;
        let file = paths.service_file(name);
        if file.exists() {
            bail!("service {name:?} already exists; use `hum edit {name}` or `hum set {name}`");
        }
        let cwd = match cwd {
            Some(c) => std::path::absolute(config::expand_path(&c, &paths.home))?,
            None => std::env::current_dir()?,
        };
        let mut vars = BTreeMap::new();
        for kv in env {
            let (k, v) = kv
                .split_once('=')
                .with_context(|| format!("--env {kv:?}: expected KEY=VALUE"))?;
            config::validate_env_key(k)?;
            vars.insert(k.to_string(), v.to_string());
        }
        let s = Service {
            name: name.into(),
            command: Command::Argv(command),
            cwd: Some(config::contract_path(&cwd, &paths.home)),
            env_file,
            autostart,
            restart,
            env: vars,
            log: Default::default(),
        };
        let text = config::render_service(&s);
        config::parse_service(name, &text)?;
        let global = config::load_global(paths)?;
        let argv0 = &s.argv()[0];
        if environ::look_path(argv0, global.env.get("PATH").map_or("", |p| p)).is_none() {
            eprintln!("warning: {argv0} not found in global PATH; writing it anyway");
        }
        warn_missing_env_files(&self.app, &s);
        config::write_atomic(&file, text.as_bytes())?;
        println!("created {}", file.display());
        if start {
            self.app.start(name)
        } else {
            self.app.sync_plist(&s).map(drop)
        }
    }

    fn edit(&self, name: Option<&str>, global: bool, no_restart: bool) -> Result<()> {
        let paths = &self.app.paths;
        let (path, validate): (_, Validator) = match name {
            None => (
                paths.global_file(),
                Box::new(|t| config::parse_global(t).map(drop)),
            ),
            Some(name) => {
                config::validate_name(name)?;
                (
                    paths.service_file(name),
                    Box::new(move |t| config::parse_service(name, t).map(drop)),
                )
            }
        };
        let orig = match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !global => {
                bail!(
                    "service {:?} not found; create it with `hum new`",
                    name.unwrap_or_default()
                )
            }
            r => r.with_context(|| path.display().to_string())?,
        };
        let Some(edited) = self.edit_loop(&path, &orig, &*validate)? else {
            return Ok(());
        };
        config::write_atomic(&path, edited.as_bytes())?;
        println!("saved {}", path.display());
        match name {
            None => {
                println!(
                    "global config takes effect when each service next restarts; run `hum restart --all` to apply it now"
                );
                Ok(())
            }
            Some(name) => {
                warn_missing_env_files(&self.app, &config::parse_service(name, &edited)?);
                self.app.apply_config_change(name, !no_restart)
            }
        }
    }

    /// Opens a temp copy in the editor until it validates or the user
    /// gives up. None when nothing changed.
    fn edit_loop(
        &self,
        path: &Path,
        orig: &str,
        validate: &dyn Fn(&str) -> Result<()>,
    ) -> Result<Option<String>> {
        let suffix = format!(
            "-{}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        let mut tmp = tempfile::Builder::new()
            .prefix("hum-")
            .suffix(&suffix)
            .tempfile()?;
        tmp.write_all(orig.as_bytes())?;
        let editor = ["VISUAL", "EDITOR"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
            .unwrap_or_else(|| "vi".into());
        loop {
            // Through sh so that editors with arguments ("code -w") work.
            let status = std::process::Command::new("/bin/sh")
                .args(["-c", &format!("{editor} \"$@\""), "editor"])
                .arg(tmp.path())
                .status()?;
            if !status.success() {
                bail!("editor {editor:?} exited with {status}");
            }
            let text = std::fs::read_to_string(tmp.path())?;
            if text == orig {
                println!("no changes");
                return Ok(None);
            }
            let Err(e) = validate(&text) else {
                return Ok(Some(text));
            };
            eprintln!("invalid config: {e:#}");
            let again = self.yes || {
                eprint!("edit again? [Y/n] ");
                matches!(read_line().as_str(), "" | "y" | "yes")
            };
            if !again {
                eprintln!("discarded changes; {} is unchanged", path.display());
                return Err(Exit(1).into());
            }
        }
    }

    fn set(
        &self,
        name: &str,
        pairs: &[String],
        command: &[String],
        no_restart: bool,
    ) -> Result<()> {
        self.edit_service(name, no_restart, |doc| {
            let mut used_command = false;
            for kv in pairs {
                if kv == "command" {
                    if command.is_empty() {
                        bail!("command: give the new command after --, e.g. `hum set t3 command -- t3 server`");
                    }
                    doc.set(&["command"], command.iter().collect::<toml_edit::Array>().into())?;
                    used_command = true;
                    continue;
                }
                let (k, v) = kv.split_once('=').with_context(|| format!("{kv:?}: expected key=value"))?;
                let value: toml_edit::Value = match k {
                    "autostart" => v
                        .parse::<bool>()
                        .map_err(|_| anyhow::anyhow!("autostart: expected true or false, got {v:?}"))?
                        .into(),
                    "log.keep" => v
                        .parse::<i64>()
                        .map_err(|_| anyhow::anyhow!("log.keep: expected an integer, got {v:?}"))?
                        .into(),
                    "env_file" => {
                        v.split(',').map(str::trim).filter(|s| !s.is_empty()).collect::<toml_edit::Array>().into()
                    }
                    _ => v.into(),
                };
                doc.set(&key_path(k)?, value)?;
            }
            if !command.is_empty() && !used_command {
                bail!("arguments after -- are only used with the `command` key");
            }
            Ok(())
        })
    }

    fn unset(&self, name: &str, keys: &[String], no_restart: bool) -> Result<()> {
        self.edit_service(name, no_restart, |doc| {
            for k in keys {
                if k == "command" {
                    bail!("command is required and cannot be unset");
                }
                if !doc.unset(&key_path(k)?)? {
                    eprintln!("warning: {k} is not set");
                }
            }
            Ok(())
        })
    }

    /// Applies `f` to the service file, validates the result and writes it
    /// back, then applies the change like `hum edit`.
    fn edit_service(
        &self,
        name: &str,
        no_restart: bool,
        f: impl FnOnce(&mut tomledit::Doc) -> Result<()>,
    ) -> Result<()> {
        config::validate_name(name)?;
        let path = self.app.paths.service_file(name);
        let orig = match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                bail!("service {name:?} not found")
            }
            r => r?,
        };
        let mut doc = tomledit::Doc::parse(&orig)?;
        f(&mut doc)?;
        let out = doc.to_string();
        if out == orig {
            println!("no changes");
            return Ok(());
        }
        let s = config::parse_service(name, &out).with_context(|| {
            format!(
                "resulting config is invalid, {} left unchanged",
                path.display()
            )
        })?;
        config::write_atomic(&path, out.as_bytes())?;
        println!("saved {}", path.display());
        warn_missing_env_files(&self.app, &s);
        self.app.apply_config_change(name, !no_restart)
    }

    fn rm(&self, targets: &Targets, keep_config: bool) -> Result<()> {
        let names = self.targets(targets)?;
        let what = if keep_config {
            "stop and unregister"
        } else {
            "stop and delete"
        };
        if !self.confirm(&format!("{what} {}?", names.join(", "))) {
            bail!("aborted");
        }
        self.each(&names, |n| self.app.remove(n, keep_config))
    }

    fn ls(&self) -> Result<()> {
        let rows = self.app.states()?;
        if self.json {
            println!("{}", serde_json::to_string_pretty(&rows)?);
            return Ok(());
        }
        if rows.is_empty() {
            println!("no services; create one with `hum new <name> -- <cmd>`");
            return Ok(());
        }
        let table: Vec<[String; 5]> = rows
            .iter()
            .map(|r| {
                let mark = if r.out_of_sync.is_some() { " *" } else { "" };
                let command = match (&r.error, r.out_of_sync.as_deref()) {
                    (Some(e), _) => format!("invalid config: {e}"),
                    (None, Some(app::NO_CONFIG)) => "(no config)".into(),
                    _ => r.command.clone().unwrap_or_default(),
                };
                [
                    format!("{}{mark}", r.name),
                    r.state.map_or("-", |s| s.as_str()).into(),
                    r.pid.map_or("-".into(), |p| p.to_string()),
                    last_exit(r.last_exit, r.last_signal.as_deref()),
                    command,
                ]
            })
            .collect();
        print_table(&["NAME", "STATE", "PID", "LAST EXIT", "COMMAND"], &table);
        if rows.iter().any(|r| r.out_of_sync.is_some()) {
            println!("\n* launchd does not match the config; run `hum apply`");
        }
        Ok(())
    }

    fn show(&self, name: &str, reveal: bool) -> Result<()> {
        let app = &self.app;
        let s = config::load_service(&app.paths, name)?;
        let global = config::load_global(&app.paths)?;
        let label = config::label(name);
        let st = app.ld.status(&label)?;
        let state = app::State::of(&st, app.ld.disabled()?.contains(&label));
        let (max_size, keep) = config::effective_log(&global, Some(&s));
        let (env, env_err) = environ::assemble(&environ::base(), &global, &s, &app.paths.home);
        let out_of_sync = match app.plist_current(&s) {
            (false, _) => Some("plist missing"),
            (true, false) => Some("plist outdated"),
            _ => None,
        };
        let value = |k: &str| {
            if reveal {
                env.vars[k].clone()
            } else {
                "********".into()
            }
        };
        if self.json {
            let vars: Vec<_> = env
                .vars
                .keys()
                .map(|k| serde_json::json!({"key": k, "value": value(k), "source": env.sources[k]}))
                .collect();
            let undefined: Vec<_> = env
                .undefined
                .iter()
                .map(|u| serde_json::json!({"file": u.file, "line": u.line, "name": u.name}))
                .collect();
            let doc = serde_json::json!({
                "name": name, "label": label,
                "config": app.paths.service_file(name), "plist": app.paths.plist_file(name), "log": app.paths.log_file(name),
                "command": s.argv(), "shell": matches!(s.command, Command::Shell(_)),
                "cwd": s.resolved_cwd(&app.paths.home), "env_file": s.resolved_env_files(&app.paths.home),
                "autostart": s.autostart, "restart": s.restart.as_str(),
                "log_max_size": max_size, "log_keep": keep,
                "state": state, "pid": st.pid, "last_exit": st.last_exit, "last_signal": st.last_signal,
                "out_of_sync": out_of_sync,
                "env": vars, "env_error": env_err.as_ref().map(|e| e.to_string()), "undefined_refs": undefined,
            });
            println!("{}", serde_json::to_string_pretty(&doc)?);
            return Ok(());
        }
        let mut info = vec![
            ["name".into(), name.to_string()],
            [
                "state".into(),
                match st.pid {
                    Some(pid) => format!("{} (pid {pid})", state.as_str()),
                    None => state.as_str().into(),
                },
            ],
            [
                "last exit".into(),
                last_exit(st.last_exit, st.last_signal.as_deref()),
            ],
            ["command".into(), s.command_string()],
            [
                "cwd".into(),
                s.resolved_cwd(&app.paths.home).display().to_string(),
            ],
        ];
        if !s.env_file.is_empty() {
            let files: Vec<_> = s
                .resolved_env_files(&app.paths.home)
                .iter()
                .map(|p| p.display().to_string())
                .collect();
            info.push(["env_file".into(), files.join(", ")]);
        }
        info.extend([
            ["autostart".into(), s.autostart.to_string()],
            ["restart".into(), s.restart.as_str().into()],
            [
                "log rotate".into(),
                format!("max_size {max_size} bytes, keep {keep}"),
            ],
            [
                "config".into(),
                app.paths.service_file(name).display().to_string(),
            ],
            [
                "plist".into(),
                app.paths.plist_file(name).display().to_string(),
            ],
            ["log".into(), app.paths.log_file(name).display().to_string()],
        ]);
        if let Some(reason) = out_of_sync {
            info.push([
                "launchd".into(),
                format!("{reason}; run `hum apply` or `hum restart {name}`"),
            ]);
        }
        print_table(&[], &info);
        println!("\nenvironment:");
        let rows: Vec<[String; 3]> = env
            .vars
            .keys()
            .map(|k| [format!("  {k}"), value(k), env.sources[k].clone()])
            .collect();
        print_table(&[], &rows);
        if let Some(e) = env_err {
            println!("  error: {e} (hum run would refuse to start)");
        }
        for u in &env.undefined {
            println!(
                "  warning: {}:{} references undefined ${{{}}} (expands to \"\")",
                u.file, u.line, u.name
            );
        }
        Ok(())
    }

    fn logs(
        &self,
        targets: &Targets,
        follow: bool,
        lines: usize,
        rotate: bool,
        if_oversize: bool,
    ) -> Result<()> {
        let names = self.targets(targets)?;
        let paths = &self.app.paths;
        if rotate {
            let global = config::load_global(paths)?;
            return self.each(&names, |n| {
                // An invalid service config still rotates with global settings.
                let s = config::load_service(paths, n).ok();
                let (max_size, keep) = config::effective_log(&global, s.as_ref());
                let now = chrono::Local::now();
                let log = paths.log_file(n);
                let rotated = if if_oversize {
                    logfile::rotate_if_needed(&log, max_size, keep, now)?
                } else {
                    logfile::rotate(&log, keep, now)?
                };
                match (rotated, if_oversize) {
                    (true, _) => println!("{n}: rotated"),
                    (false, false) => println!("{n}: log is empty, nothing to rotate"),
                    (false, true) => {}
                }
                Ok(())
            });
        }
        let prefix = names.len() > 1 || targets.all;
        let emit = move |n: &str, line: &str| {
            let mut out = std::io::stdout().lock();
            let _ = if prefix {
                writeln!(out, "[{n}] {line}")
            } else {
                writeln!(out, "{line}")
            };
        };
        let mut offsets = vec![];
        for n in &names {
            let (tail, offset) = logfile::tail(&paths.log_file(n), lines)?;
            for l in tail {
                emit(n, &l);
            }
            offsets.push(offset);
        }
        if !follow {
            return Ok(());
        }
        // Runs until interrupted.
        std::thread::scope(|scope| {
            let handles: Vec<_> = names
                .iter()
                .zip(offsets)
                .map(|(n, off)| {
                    scope.spawn(move || logfile::follow(&paths.log_file(n), off, |l| emit(n, l)))
                })
                .collect();
            for h in handles {
                h.join().expect("follow thread panicked")?;
            }
            Ok(())
        })
    }

    fn apply(&self, dry_run: bool) -> Result<()> {
        let (actions, problems) = self.app.plan()?;
        for p in &problems {
            eprintln!("skipped: {p:#}");
        }
        if dry_run {
            if actions.is_empty() {
                println!("nothing to do");
            }
            for a in &actions {
                println!("would {} {}", a.kind.as_str(), a.name);
            }
            return Ok(());
        }
        if actions.is_empty() {
            println!("launchd is in sync");
        }
        self.app.apply(&actions)?;
        if problems.is_empty() {
            Ok(())
        } else {
            Err(Exit(1).into())
        }
    }

    fn doctor(&self) -> Result<()> {
        let findings = self.app.doctor()?;
        for f in &findings {
            let mark = match f.severity {
                Severity::Ok => "ok   ",
                Severity::Warn => "warn ",
                Severity::Error => "error",
            };
            println!("{mark}  {:<12} {}", f.subject, f.message);
        }
        if findings.iter().any(|f| f.severity == Severity::Error) {
            Err(Exit(1).into())
        } else {
            Ok(())
        }
    }

    fn run_service(&self, name: &str) -> Result<()> {
        let ex = self.app.prepare_run(name, &environ::base())?;
        let mut argv = ex.argv.into_iter();
        let err = std::process::Command::new(&ex.program)
            .arg0(argv.next().unwrap_or_default())
            .args(argv)
            .env_clear()
            .envs(&ex.env)
            .current_dir(&ex.dir)
            .exec();
        Err(err).with_context(|| format!("{name}: exec {}", ex.program.display()))
    }
}

/// Key paths `hum set` / `unset` accept.
fn key_path(key: &str) -> Result<Vec<&str>> {
    if let Some(var) = key.strip_prefix("env.") {
        config::validate_env_key(var)?;
        return Ok(vec!["env", var]);
    }
    match key {
        "command" | "cwd" | "env_file" | "autostart" | "restart" | "log.max_size" | "log.keep" => {
            Ok(key.split('.').collect())
        }
        _ => bail!(
            "unknown key {key:?} (known: command, cwd, env_file, autostart, restart, env.<NAME>, log.max_size, log.keep)"
        ),
    }
}

fn last_exit(code: Option<i32>, signal: Option<&str>) -> String {
    match (signal, code) {
        (Some(s), _) => s.into(),
        (None, Some(c)) => c.to_string(),
        (None, None) => "-".into(),
    }
}

/// Prints rows in aligned columns, with an optional header.
fn print_table<const N: usize>(header: &[&str], rows: &[[String; N]]) {
    let mut widths = [0; N];
    for (i, h) in header.iter().enumerate() {
        widths[i] = h.chars().count();
    }
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let line = |cells: Vec<&str>| {
        let mut s = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i + 1 == cells.len() {
                s += c;
            } else {
                s += &format!("{c:<w$}  ", w = widths[i]);
            }
        }
        println!("{}", s.trim_end());
    };
    if !header.is_empty() {
        line(header.to_vec());
    }
    for row in rows {
        line(row.iter().map(String::as_str).collect());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn new_requires_a_command_after_dashes() {
        assert!(Cli::try_parse_from(["hum", "new", "t3", "t3", "server"]).is_err());
        assert!(
            Cli::try_parse_from([
                "hum", "new", "t3", "--env", "A=1", "--", "t3", "--port", "1"
            ])
            .is_ok()
        );
    }

    #[test]
    fn set_keys() {
        assert_eq!(key_path("env.PORT").unwrap(), ["env", "PORT"]);
        assert_eq!(key_path("log.keep").unwrap(), ["log", "keep"]);
        assert!(key_path("port").is_err());
        assert!(key_path("env.A-B").is_err());
    }
}
