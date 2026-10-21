//! Command definitions, handlers and output.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::{ArgValueCandidates, CompletionCandidate};

use crate::app::{App, ChangeOutcome, ConfigState, Severity, StartOutcome};
use crate::config::{self, Command, EditKey, Restart, Service, ServiceName};
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
        .iter()
        .map(|n| CompletionCandidate::new(n.to_string()))
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
    names: Vec<ServiceName>,
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
        name: ServiceName,
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
        name: Option<ServiceName>,
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
        name: ServiceName,
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
        name: ServiceName,
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
        name: ServiceName,
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
        name: ServiceName,
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
    if let Cmd::Completion { shell } = cli.cmd {
        return completion(shell);
    }
    let app = App::new()?;
    // `hum run` is what launchd invokes, with launchd's short PATH, so it
    // must not be the one to capture PATH.
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
            let s = ctx.new_service(name, cwd, &env, env_file, restart, !no_autostart, command)?;
            if no_start {
                ctx.app.write_new_plist(&s)
            } else {
                ctx.start(&s.name)
            }
        }
        Cmd::Edit {
            name,
            global,
            no_restart,
        } => ctx.edit(name.as_ref(), global, no_restart),
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
        Cmd::Start(t) => ctx.each(&ctx.targets(&t)?, |n| ctx.start(n)),
        Cmd::Stop(t) => ctx.each(&ctx.targets(&t)?, |n| {
            ctx.app.stop(n)?;
            println!("{n}: stopped");
            Ok(())
        }),
        Cmd::Restart(t) => ctx.each(&ctx.targets(&t)?, |n| {
            ctx.app.restart(n)?;
            println!("{n}: restarted");
            Ok(())
        }),
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

/// A line from stdin, lowercased; None at end of input, which callers
/// treat as declining so a closed stdin never loops or confirms.
fn read_line() -> Option<String> {
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim().to_ascii_lowercase()),
    }
}

impl Ctx {
    fn targets(&self, t: &Targets) -> Result<Vec<ServiceName>> {
        match (t.all, t.names.is_empty()) {
            (true, false) => bail!("give service names or --all, not both"),
            (true, true) => {
                let mut names = vec![];
                for file in self.app.paths.service_files()? {
                    match file {
                        Ok(name) => names.push(name),
                        Err(raw) => eprintln!("warning: skipping {raw:?}: invalid service name"),
                    }
                }
                if names.is_empty() {
                    bail!("no services configured");
                }
                Ok(names)
            }
            (false, true) => bail!("give at least one service name, or --all"),
            (false, false) => Ok(t.names.clone()),
        }
    }

    /// Runs `f` for every name, reporting failures and continuing.
    fn each(&self, names: &[ServiceName], f: impl Fn(&ServiceName) -> Result<()>) -> Result<()> {
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
        matches!(read_line().as_deref(), Some("y" | "yes"))
    }

    fn start(&self, name: &ServiceName) -> Result<()> {
        match self.app.start(name)? {
            StartOutcome::AlreadyRunning { pid } => {
                let pid = pid.map_or("?".into(), |p| p.to_string());
                println!("{name}: already running (pid {pid})");
            }
            StartOutcome::Started => println!("{name}: started"),
        }
        Ok(())
    }

    fn report_change(&self, name: &ServiceName, outcome: ChangeOutcome) {
        match outcome {
            ChangeOutcome::Restarted => println!("{name}: restarted"),
            ChangeOutcome::NeedsRestart => {
                println!(
                    "{name}: launchd keeps the old definition; run `hum restart {name}` to apply"
                )
            }
            ChangeOutcome::Applied => {}
        }
    }

    /// Writes a new service config; returns it for starting.
    #[allow(clippy::too_many_arguments)]
    fn new_service(
        &self,
        name: ServiceName,
        cwd: Option<String>,
        env: &[String],
        env_file: Vec<String>,
        restart: Restart,
        autostart: bool,
        command: Vec<String>,
    ) -> Result<Service> {
        let paths = &self.app.paths;
        let file = paths.service_file(&name);
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
            name,
            command: Command::Argv(command),
            cwd: Some(config::contract_path(&cwd, &paths.home)),
            env_file,
            autostart,
            restart,
            env: vars,
            log: Default::default(),
        };
        let text = config::render_service(&s);
        let s = config::parse_service(&s.name, &text)?;
        let global = config::load_global(paths)?;
        let argv0 = &s.argv()[0];
        if environ::look_path(argv0, global.env.get("PATH").map_or("", |p| p)).is_none() {
            eprintln!("warning: {argv0} not found in global PATH; writing it anyway");
        }
        warn_missing_env_files(&self.app, &s);
        config::write_atomic(&file, text.as_bytes())?;
        println!("created {}", file.display());
        Ok(s)
    }

    fn edit(&self, name: Option<&ServiceName>, global: bool, no_restart: bool) -> Result<()> {
        let paths = &self.app.paths;
        let (path, validate): (_, Validator) = match name {
            None => (
                paths.global_file(),
                Box::new(|t| config::parse_global(t).map(drop)),
            ),
            Some(name) => (
                paths.service_file(name),
                Box::new(move |t| config::parse_service(name, t).map(drop)),
            ),
        };
        let orig = match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !global => {
                bail!(
                    "service {:?} not found; create it with `hum new`",
                    name.map_or("", |n| n)
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
                let outcome = self.app.apply_config_change(name, !no_restart)?;
                self.report_change(name, outcome);
                Ok(())
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
                matches!(read_line().as_deref(), Some("" | "y" | "yes"))
            };
            if !again {
                eprintln!("discarded changes; {} is unchanged", path.display());
                return Err(Exit(1).into());
            }
        }
    }

    fn set(
        &self,
        name: &ServiceName,
        pairs: &[String],
        command: &[String],
        no_restart: bool,
    ) -> Result<()> {
        self.edit_service(name, no_restart, |doc| {
            let mut used_command = false;
            for kv in pairs {
                if kv == "command" {
                    if command.is_empty() {
                        bail!(
                            "command: give the new command after --, e.g. `hum set t3 command -- t3 server`"
                        );
                    }
                    let argv: toml_edit::Array = command.iter().collect();
                    doc.set(&["command"], argv.into())?;
                    used_command = true;
                    continue;
                }
                let (k, v) = kv
                    .split_once('=')
                    .with_context(|| format!("{kv:?}: expected key=value"))?;
                let key = EditKey::parse(k)?;
                let path: Vec<&str> = key.path.iter().map(String::as_str).collect();
                doc.set(&path, key.value(v)?)?;
            }
            if !command.is_empty() && !used_command {
                bail!("arguments after -- are only used with the `command` key");
            }
            Ok(())
        })
    }

    fn unset(&self, name: &ServiceName, keys: &[String], no_restart: bool) -> Result<()> {
        self.edit_service(name, no_restart, |doc| {
            for k in keys {
                if k == "command" {
                    bail!("command is required and cannot be unset");
                }
                let key = EditKey::parse(k)?;
                let path: Vec<&str> = key.path.iter().map(String::as_str).collect();
                if !doc.unset(&path)? {
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
        name: &ServiceName,
        no_restart: bool,
        f: impl FnOnce(&mut tomledit::Doc) -> Result<()>,
    ) -> Result<()> {
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
        let outcome = self.app.apply_config_change(name, !no_restart)?;
        self.report_change(name, outcome);
        Ok(())
    }

    fn rm(&self, targets: &Targets, keep_config: bool) -> Result<()> {
        let names = self.targets(targets)?;
        let what = if keep_config {
            "stop and unregister"
        } else {
            "stop and delete"
        };
        let list: Vec<&str> = names.iter().map(|n| &**n).collect();
        if !self.confirm(&format!("{what} {}?", list.join(", "))) {
            bail!("aborted");
        }
        self.each(&names, |n| {
            self.app.remove(n, keep_config)?;
            if keep_config {
                let cfg = self.app.paths.service_file(n);
                println!("{n}: unregistered (config kept at {})", cfg.display());
            } else {
                println!("{n}: removed");
            }
            Ok(())
        })
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
                let mark = if r.out_of_sync() { " *" } else { "" };
                let (state, pid, exit) = match &r.launchd {
                    Some(l) => (
                        l.state.as_str().to_string(),
                        l.pid.map_or("-".into(), |p| p.to_string()),
                        last_exit(l.last_exit, l.last_signal.as_deref()),
                    ),
                    None => ("-".into(), "-".into(), "-".into()),
                };
                let command = match &r.config {
                    ConfigState::Ok { command, .. } => command.clone(),
                    ConfigState::Invalid { error } => format!("invalid config: {error}"),
                    ConfigState::Missing => "(no config)".into(),
                };
                [format!("{}{mark}", r.name), state, pid, exit, command]
            })
            .collect();
        print_table(&["NAME", "STATE", "PID", "LAST EXIT", "COMMAND"], &table);
        if rows.iter().any(|r| r.out_of_sync()) {
            println!("\n* launchd does not match the config; run `hum apply`");
        }
        Ok(())
    }

    fn show(&self, name: &ServiceName, reveal: bool) -> Result<()> {
        let r = self.app.show(name, reveal)?;
        if self.json {
            println!("{}", serde_json::to_string_pretty(&r)?);
            return Ok(());
        }
        let state = match r.launchd.pid {
            Some(pid) => format!("{} (pid {pid})", r.launchd.state.as_str()),
            None => r.launchd.state.as_str().into(),
        };
        let mut info = vec![
            ["name".into(), r.name.to_string()],
            ["state".into(), state],
            [
                "last exit".into(),
                last_exit(r.launchd.last_exit, r.launchd.last_signal.as_deref()),
            ],
            ["command".into(), r.command_display.clone()],
            ["cwd".into(), r.cwd.display().to_string()],
        ];
        if !r.env_file.is_empty() {
            let files: Vec<_> = r.env_file.iter().map(|p| p.display().to_string()).collect();
            info.push(["env_file".into(), files.join(", ")]);
        }
        info.extend([
            ["autostart".into(), r.autostart.to_string()],
            ["restart".into(), r.restart.as_str().into()],
            [
                "log rotate".into(),
                format!("max_size {} bytes, keep {}", r.log_max_size, r.log_keep),
            ],
            ["config".into(), r.config.display().to_string()],
            ["plist".into(), r.plist.display().to_string()],
            ["log".into(), r.log.display().to_string()],
        ]);
        if let Some(reason) = &r.out_of_sync {
            info.push([
                "launchd".into(),
                format!("{reason}; run `hum apply` or `hum restart {name}`"),
            ]);
        }
        print_table(&[], &info);
        println!("\nenvironment:");
        let rows: Vec<[String; 3]> = r
            .env
            .iter()
            .map(|v| [format!("  {}", v.key), v.value.clone(), v.source.clone()])
            .collect();
        print_table(&[], &rows);
        if let Some(e) = &r.env_error {
            println!("  error: {e} (hum run would refuse to start)");
        }
        for u in &r.undefined_refs {
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
                println!("would {} {} ({})", a.kind.as_str(), a.name, a.reason);
            }
            return Ok(());
        }
        if actions.is_empty() {
            println!("launchd is in sync");
        }
        for a in &actions {
            self.app.apply_action(a)?;
            println!("{}: {} ({})", a.name, a.kind.as_str(), a.reason);
        }
        self.app.finish_apply()?;
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

    fn run_service(&self, name: &ServiceName) -> Result<()> {
        let ex = self.app.prepare_run(name, &environ::base())?;
        for w in &ex.warnings {
            eprintln!("warning: {w}");
        }
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

    // Requirement: Create a service
    #[test]
    fn new_requires_a_command_after_dashes() {
        assert!(Cli::try_parse_from(["hum", "new", "t3", "t3", "server"]).is_err());
        let ok = [
            "hum", "new", "t3", "--env", "A=1", "--", "t3", "--port", "1",
        ];
        assert!(Cli::try_parse_from(ok).is_ok());
    }

    // Requirement: File name is the service name
    #[test]
    fn service_names_are_checked_while_parsing() {
        assert!(Cli::try_parse_from(["hum", "start", "Bad.Name"]).is_err());
        assert!(Cli::try_parse_from(["hum", "show", "_rotate"]).is_err());
    }

    // Requirement: Automatic rotation
    #[test]
    fn rotate_job_arguments_parse() {
        let args = std::iter::once("hum").chain(crate::app::ROTATE_ARGS);
        let cli = Cli::try_parse_from(args).expect("the rotate job's arguments must parse");
        let Cmd::Logs {
            targets,
            rotate,
            if_oversize,
            follow,
            ..
        } = cli.cmd
        else {
            panic!("rotate job does not run `hum logs`");
        };
        assert!(targets.all && rotate && if_oversize && !follow);
    }
}
