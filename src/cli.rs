//! Command definitions, handlers and output.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::{ArgValueCandidates, CompletionCandidate};
use serde::Serialize;

use crate::app::{self, App, ChangeOutcome, ConfigState, Severity, StartOutcome, State};
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
    version,
    about = "Run background commands as launchd services, configured by TOML files"
)]
pub struct Cli {
    /// Print the result as JSON (all commands except edit, run, completion and logs --follow)
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

/// Runs a command. With `--json`, a failure that left nothing on standard
/// output prints `{"error": ...}` there, so standard output always holds
/// one JSON document.
pub fn run(cli: Cli) -> Result<()> {
    let json = cli.json;
    let result = dispatch(cli);
    if json
        && let Err(e) = &result
        && e.downcast_ref::<Exit>().is_none()
    {
        print_json(&serde_json::json!({ "error": format!("{e:#}") }))?;
    }
    result
}

fn dispatch(cli: Cli) -> Result<()> {
    let interactive = match &cli.cmd {
        Cmd::Edit { .. } => Some("edit"),
        Cmd::Run { .. } => Some("run"),
        Cmd::Completion { .. } => Some("completion"),
        Cmd::Logs {
            follow: true,
            rotate: false,
            ..
        } => Some("logs --follow"),
        _ => None,
    };
    if cli.json
        && let Some(cmd) = interactive
    {
        bail!("--json is not supported by `hum {cmd}`");
    }
    if let Cmd::Completion { shell } = cli.cmd {
        return completion(shell);
    }
    let app = App::new()?;
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
            no_start,
            command,
        } => {
            let restart = match restart {
                RestartArg::Always => Restart::Always,
                RestartArg::OnFailure => Restart::OnFailure,
                RestartArg::Never => Restart::Never,
            };
            let (s, mut d) = ctx.new_service(name, cwd, &env, env_file, restart, command)?;
            let started = if no_start {
                ctx.app.write_new_plist(&s)
            } else {
                ctx.start(&mut d)
            };
            if let Err(e) = started {
                d.fail(&e);
            }
            ctx.emit_one(d)
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
        Cmd::Start(t) => ctx.each(&ctx.targets(&t)?, |n| {
            let mut d = Done::new(n, Outcome::Started);
            ctx.start(&mut d)?;
            Ok(d)
        }),
        Cmd::Stop(t) => ctx.each(&ctx.targets(&t)?, |n| {
            ctx.app.stop(n)?;
            let mut d = Done::new(n, Outcome::Stopped);
            d.say(format!("{n}: stopped"));
            Ok(d)
        }),
        Cmd::Restart(t) => ctx.each(&ctx.targets(&t)?, |n| {
            ctx.app.restart(n)?;
            let mut d = Done::new(n, Outcome::Restarted);
            ctx.report_started(&mut d, Outcome::Restarted);
            Ok(d)
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

fn missing_env_files(app: &App, s: &Service) -> Vec<String> {
    s.resolved_env_files(&app.paths.home)
        .iter()
        .filter(|f| !f.exists())
        .map(|f| {
            format!(
                "env_file {} does not exist; `hum run` will refuse to start until it does",
                f.display()
            )
        })
        .collect()
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// What a command did to one service. Text output prints `lines` on
/// standard output; JSON output prints the other fields. Warnings and the
/// error go to standard error either way.
#[derive(Serialize)]
struct Done {
    name: ServiceName,
    outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// The config file the command wrote or deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    /// With `needs_restart`: whether the service is running with the old
    /// config, rather than launchd holding the old definition.
    #[serde(skip_serializing_if = "Option::is_none")]
    running: Option<bool>,
    /// Set when launchd was asked to run the service but `hum run` refuses
    /// to start it.
    #[serde(skip_serializing_if = "Option::is_none")]
    not_running: Option<NotRunning>,
    warnings: Vec<String>,
    #[serde(skip)]
    lines: Vec<String>,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Created,
    Started,
    AlreadyRunning,
    Restarted,
    NeedsRestart,
    Applied,
    Unchanged,
    Stopped,
    Removed,
    Unregistered,
    Rotated,
    NotRotated,
    Failed,
}

#[derive(Serialize)]
struct NotRunning {
    reason: String,
    /// Whether launchd keeps trying, so the service starts once the reason
    /// is fixed.
    retrying: bool,
}

impl Done {
    fn new(name: &ServiceName, outcome: Outcome) -> Self {
        Done {
            name: name.clone(),
            outcome,
            error: None,
            config: None,
            pid: None,
            running: None,
            not_running: None,
            warnings: vec![],
            lines: vec![],
        }
    }

    fn say(&mut self, line: String) {
        self.lines.push(line);
    }

    fn fail(&mut self, e: &anyhow::Error) {
        self.outcome = Outcome::Failed;
        self.error = Some(format!("{e:#}"));
    }

    fn print_stderr(&self) {
        for w in &self.warnings {
            eprintln!("warning: {w}");
        }
        if let Some(e) = &self.error {
            eprintln!("hum: {}: {e}", self.name);
        }
    }

    fn print_lines(&self) {
        for l in &self.lines {
            println!("{l}");
        }
    }
}

/// `hum logs` output for one service.
#[derive(Serialize)]
struct Tail<'a> {
    name: &'a ServiceName,
    lines: Vec<String>,
}

/// `hum apply` output.
#[derive(Serialize)]
struct ApplyReport {
    dry_run: bool,
    /// The changes made, or with `dry_run` the changes that would be made.
    /// Applying stops at the first change that fails, which is listed last
    /// with its error.
    actions: Vec<ActionDone>,
    /// Config files that were skipped, with the reason.
    skipped: Vec<String>,
}

#[derive(Serialize)]
struct ActionDone {
    kind: &'static str,
    subject: String,
    reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
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

    /// Runs `f` for every name, reporting failures and continuing. Text
    /// output prints each result as it comes; JSON output prints them all
    /// as one array.
    fn each(&self, names: &[ServiceName], f: impl Fn(&ServiceName) -> Result<Done>) -> Result<()> {
        let mut results = vec![];
        for n in names {
            let d = f(n).unwrap_or_else(|e| {
                let mut d = Done::new(n, Outcome::Failed);
                d.fail(&e);
                d
            });
            d.print_stderr();
            if !self.json {
                d.print_lines();
            }
            results.push(d);
        }
        if self.json {
            print_json(&results)?;
        }
        let failed = results.iter().filter(|d| d.error.is_some()).count();
        match failed {
            0 => Ok(()),
            _ if self.json || names.len() == 1 => Err(Exit(1).into()),
            _ => bail!("{failed} of {} failed", names.len()),
        }
    }

    /// Prints the result of a command on a single service, and fails when
    /// it carries an error.
    fn emit_one(&self, d: Done) -> Result<()> {
        d.print_stderr();
        if self.json {
            print_json(&d)?;
        } else {
            d.print_lines();
        }
        match d.error {
            None => Ok(()),
            Some(_) => Err(Exit(1).into()),
        }
    }

    fn confirm(&self, prompt: &str) -> bool {
        if self.yes {
            return true;
        }
        eprint!("{prompt} [y/N] ");
        matches!(read_line().as_deref(), Some("y" | "yes"))
    }

    fn start(&self, d: &mut Done) -> Result<()> {
        let name = d.name.clone();
        match self.app.start(&name)? {
            StartOutcome::AlreadyRunning { pid } => {
                d.outcome = Outcome::AlreadyRunning;
                d.pid = pid;
                let pid = pid.map_or("?".into(), |p| p.to_string());
                d.say(format!("{name}: already running (pid {pid})"));
            }
            StartOutcome::Started => self.report_started(d, Outcome::Started),
        }
        Ok(())
    }

    /// Reports a service launchd was just asked to run, unless `hum run`
    /// would refuse to start it: then says why, and whether launchd keeps
    /// trying.
    fn report_started(&self, d: &mut Done, outcome: Outcome) {
        d.outcome = outcome;
        let name = d.name.clone();
        let Some(why) = self.app.run_blocker(&name) else {
            let verb = match outcome {
                Outcome::Restarted => "restarted",
                _ => "started",
            };
            d.say(format!("{name}: {verb}"));
            return;
        };
        let retrying = !matches!(
            config::load_service(&self.app.paths, &name).map(|s| s.restart),
            Ok(Restart::Never)
        );
        let next = if retrying {
            "launchd retries it, so it starts once that is fixed".into()
        } else {
            format!("run `hum start {name}` once that is fixed")
        };
        d.say(format!("{name}: registered but not running: {why}; {next}"));
        d.not_running = Some(NotRunning {
            reason: why,
            retrying,
        });
    }

    fn report_change(&self, d: &mut Done, outcome: ChangeOutcome) {
        let name = d.name.clone();
        match outcome {
            ChangeOutcome::Restarted => self.report_started(d, Outcome::Restarted),
            ChangeOutcome::NeedsRestart { running } => {
                d.outcome = Outcome::NeedsRestart;
                d.running = Some(running);
                let state = if running {
                    "still running with the old config"
                } else {
                    "launchd keeps the old definition"
                };
                d.say(format!(
                    "{name}: {state}; run `hum restart {name}` to apply"
                ));
            }
            ChangeOutcome::Applied => d.outcome = Outcome::Applied,
        }
    }

    /// Writes a new service config; returns it for starting, with the
    /// result so far.
    fn new_service(
        &self,
        name: ServiceName,
        cwd: Option<String>,
        env: &[String],
        env_file: Vec<String>,
        restart: Restart,
        command: Vec<String>,
    ) -> Result<(Service, Done)> {
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
        // launchd gives services a short PATH, so record the shell's.
        if !vars.contains_key("PATH")
            && let Ok(path) = std::env::var("PATH")
            && !path.is_empty()
        {
            vars.insert("PATH".into(), config::captured_path(&path));
        }
        let s = Service {
            name,
            command: Command::Argv(command),
            cwd: Some(config::contract_path(&cwd, &paths.home)),
            env_file,
            restart,
            env: vars,
            log: Default::default(),
        };
        let text = config::render_service(&s);
        let s = config::parse_service(&s.name, &text)?;
        let global = config::load_global(paths)?;
        let mut d = Done::new(&s.name, Outcome::Created);
        let argv0 = &s.argv()[0];
        let (env, _) = environ::assemble(&environ::base(), &global, &s, &paths.home);
        if environ::look_path(argv0, env.vars.get("PATH").map_or("", |p| p)).is_none() {
            d.warnings.push(format!(
                "{argv0} not found in the service's PATH; writing it anyway"
            ));
        }
        d.warnings.extend(missing_env_files(&self.app, &s));
        config::write_atomic(&file, text.as_bytes())?;
        d.say(format!("created {}", file.display()));
        d.config = Some(file);
        Ok((s, d))
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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => config::global_template(),
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
                let mut d = Done::new(name, Outcome::Applied);
                d.warnings = missing_env_files(&self.app, &config::parse_service(name, &edited)?);
                let outcome = self.app.apply_config_change(name, !no_restart)?;
                self.report_change(&mut d, outcome);
                self.emit_one(d)
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
        self.edit_service(name, no_restart, |doc, _| {
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
        self.edit_service(name, no_restart, |doc, warnings| {
            for k in keys {
                if k == "command" {
                    bail!("command is required and cannot be unset");
                }
                let key = EditKey::parse(k)?;
                let path: Vec<&str> = key.path.iter().map(String::as_str).collect();
                if !doc.unset(&path)? {
                    warnings.push(format!("{k} is not set"));
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
        f: impl FnOnce(&mut tomledit::Doc, &mut Vec<String>) -> Result<()>,
    ) -> Result<()> {
        let path = self.app.paths.service_file(name);
        let orig = match std::fs::read_to_string(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                bail!("service {name:?} not found")
            }
            r => r?,
        };
        let mut doc = tomledit::Doc::parse(&orig)?;
        let mut d = Done::new(name, Outcome::Unchanged);
        f(&mut doc, &mut d.warnings)?;
        let out = doc.to_string();
        if out == orig {
            d.say("no changes".into());
            return self.emit_one(d);
        }
        let s = config::parse_service(name, &out).with_context(|| {
            format!(
                "resulting config is invalid, {} left unchanged",
                path.display()
            )
        })?;
        config::write_atomic(&path, out.as_bytes())?;
        d.outcome = Outcome::Applied;
        d.say(format!("saved {}", path.display()));
        d.config = Some(path);
        d.warnings.extend(missing_env_files(&self.app, &s));
        match self.app.apply_config_change(name, !no_restart) {
            Ok(outcome) => self.report_change(&mut d, outcome),
            Err(e) => d.fail(&e),
        }
        self.emit_one(d)
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
            let cfg = self.app.paths.service_file(n);
            let mut d;
            if keep_config {
                d = Done::new(n, Outcome::Unregistered);
                d.say(format!(
                    "{n}: unregistered (config kept at {})",
                    cfg.display()
                ));
            } else {
                d = Done::new(n, Outcome::Removed);
                d.say(format!("{n}: removed"));
            }
            d.config = Some(cfg);
            Ok(d)
        })
    }

    fn ls(&self) -> Result<()> {
        let rows = self.app.states()?;
        if self.json {
            return print_json(&rows);
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
                    ConfigState::Invalid { error } => format!(
                        "invalid config: {}",
                        one_line(error, &self.app.paths.services_dir())
                    ),
                    ConfigState::Missing => "(no config)".into(),
                };
                [format!("{}{mark}", r.name), state, pid, exit, command]
            })
            .collect();
        print_table(&["NAME", "STATE", "PID", "LAST EXIT", "COMMAND"], &table);
        if rows.iter().any(|r| r.out_of_sync()) {
            println!(
                "\n* not in sync with its config: `hum show <name>` says why; `hum apply` removes plists without a config"
            );
        }
        Ok(())
    }

    fn show(&self, name: &ServiceName, reveal: bool) -> Result<()> {
        let r = self.app.show(name, reveal)?;
        if self.json {
            return print_json(&r);
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
            let fix = app::sync_fix(name, r.launchd.state == State::Disabled);
            info.push(["launchd".into(), format!("{reason}; {fix}")]);
        }
        print_table(&[], &info);
        println!("\nenvironment:");
        let rows: Vec<[String; 3]> = r
            .env
            .iter()
            .map(|v| [format!("  {}", v.key), v.source.clone(), v.value.clone()])
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
                let mut d;
                if rotated {
                    d = Done::new(n, Outcome::Rotated);
                    d.say(format!("{n}: rotated"));
                } else {
                    d = Done::new(n, Outcome::NotRotated);
                    if !if_oversize {
                        d.say(format!("{n}: log is empty, nothing to rotate"));
                    }
                }
                Ok(d)
            });
        }
        if self.json {
            let mut tails = vec![];
            for n in &names {
                let (lines, _) = logfile::tail(&paths.log_file(n), lines)?;
                tails.push(Tail { name: n, lines });
            }
            return print_json(&tails);
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
        let mut report = ApplyReport {
            dry_run,
            actions: vec![],
            skipped: problems.iter().map(|p| format!("{p:#}")).collect(),
        };
        for p in &report.skipped {
            eprintln!("skipped: {p}");
        }
        let text = !self.json;
        if actions.is_empty() && text {
            println!(
                "{}",
                if dry_run {
                    "nothing to do"
                } else {
                    "launchd is in sync"
                }
            );
        }
        let mut failed = !problems.is_empty();
        for a in &actions {
            let mut done = ActionDone {
                kind: a.kind.as_str(),
                subject: a.subject.to_string(),
                reason: a.reason.clone(),
                error: None,
            };
            if dry_run {
                if text {
                    println!("would {} {} ({})", done.kind, done.subject, done.reason);
                }
            } else if let Err(e) = self.app.apply_action(a) {
                eprintln!("hum: {}: {e:#}", done.subject);
                done.error = Some(format!("{e:#}"));
                report.actions.push(done);
                failed = true;
                break;
            } else if text {
                println!("{}: {} ({})", done.subject, done.kind, done.reason);
            }
            report.actions.push(done);
        }
        if self.json {
            print_json(&report)?;
        }
        if failed && !dry_run {
            Err(Exit(1).into())
        } else {
            Ok(())
        }
    }

    fn doctor(&self) -> Result<()> {
        let findings = self.app.doctor()?;
        if self.json {
            print_json(&findings)?;
        } else {
            for f in &findings {
                let mark = match f.severity {
                    Severity::Ok => "ok   ",
                    Severity::Warn => "warn ",
                    Severity::Error => "error",
                };
                println!("{mark}  {:<12} {}", f.subject, f.message);
            }
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

/// A config error on one table row: the file path, which the row already
/// names, and the source excerpt of a TOML parse error are dropped.
fn one_line(error: &str, services_dir: &Path) -> String {
    let error = error
        .strip_prefix(&format!("{}/", services_dir.display()))
        .and_then(|e| e.split_once(": ").map(|(_, rest)| rest))
        .unwrap_or(error);
    error
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('|') && !is_excerpt(l))
        .collect::<Vec<_>>()
        .join(": ")
}

/// A `12 | source text` line of a TOML parse error.
fn is_excerpt(line: &str) -> bool {
    line.split_once(" |")
        .is_some_and(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
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

    // Requirement: ls output
    #[test]
    fn ls_shows_a_config_error_on_one_line() {
        let error = "/h/.config/hum/services/x.toml: TOML parse error at line 2, column 1\n  |\n2 | autostart = true\n  | ^^^^^^^^^\nunknown field `autostart`, expected one of `command`\n";
        assert_eq!(
            one_line(error, Path::new("/h/.config/hum/services")),
            "TOML parse error at line 2, column 1: unknown field `autostart`, expected one of `command`"
        );
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

    // Requirement: JSON results per service
    #[test]
    fn service_results_serialize_only_what_applies() {
        let name = ServiceName::parse("t3").unwrap();
        let mut d = Done::new(&name, Outcome::Started);
        d.say("t3: registered but not running".into());
        d.not_running = Some(NotRunning {
            reason: "cwd /x does not exist".into(),
            retrying: true,
        });
        assert_eq!(
            serde_json::to_value(&d).unwrap(),
            serde_json::json!({
                "name": "t3",
                "outcome": "started",
                "not_running": { "reason": "cwd /x does not exist", "retrying": true },
                "warnings": [],
            })
        );
        let mut d = Done::new(&name, Outcome::NeedsRestart);
        d.running = Some(false);
        d.fail(&anyhow::anyhow!("boom"));
        assert_eq!(
            serde_json::to_value(&d).unwrap(),
            serde_json::json!({
                "name": "t3",
                "outcome": "failed",
                "error": "boom",
                "running": false,
                "warnings": [],
            })
        );
    }
}
