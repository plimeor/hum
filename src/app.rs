//! hum's operations on top of config, plist and launchd. The CLI layer only
//! parses arguments and prints.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::{self, Paths, ROTATE_NAME, Restart, Service, label};
use crate::environ;
use crate::launchd::{Manager, Status};
use crate::{logfile, plist};

/// How often the rotate job checks log sizes, in seconds
/// (pm2-logrotate's workerInterval default).
pub const ROTATE_INTERVAL: u64 = 30;

pub struct App {
    pub paths: Paths,
    pub ld: Box<dyn Manager>,
    /// Absolute path written into plists.
    pub hum_path: String,
}

/// The path launchd should run. A package manager symlink on PATH (e.g.
/// /opt/homebrew/bin/hum) survives upgrades; the resolved binary path does
/// not, so the symlink wins when it points at this binary.
pub fn hum_path() -> String {
    let Ok(exe) = std::env::current_exe() else {
        return "hum".into();
    };
    let real = fs::canonicalize(&exe).unwrap_or(exe.clone());
    let path = std::env::var("PATH").unwrap_or_default();
    if let Some(found) = environ::look_path("hum", &path) {
        let found = std::path::absolute(found).unwrap_or_default();
        if fs::canonicalize(&found).is_ok_and(|r| r == real) {
            return found.display().to_string();
        }
    }
    exe.display().to_string()
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| path.display().to_string())
        }
        _ => Ok(()),
    }
}

impl App {
    pub fn new() -> Result<Self> {
        Ok(App {
            paths: Paths::from_env()?,
            ld: crate::launchd::new(),
            hum_path: hum_path(),
        })
    }

    fn job_env(&self) -> BTreeMap<String, String> {
        self.paths
            .hum_home
            .iter()
            .map(|h| ("HUM_HOME".to_string(), h.display().to_string()))
            .collect()
    }

    fn log_path(&self, name: &str) -> Option<String> {
        Some(self.paths.log_file(name).display().to_string())
    }

    /// The plist a service should have.
    pub fn render_plist(&self, s: &Service) -> Vec<u8> {
        plist::render(&plist::Job {
            label: label(&s.name),
            program_arguments: vec![self.hum_path.clone(), "run".into(), s.name.clone()],
            environment_variables: self.job_env(),
            run_at_load: s.autostart,
            keep_alive: match s.restart {
                Restart::Always => Some(plist::KeepAlive::Always(true)),
                Restart::OnFailure => Some(plist::KeepAlive::OnFailure {
                    successful_exit: false,
                }),
                Restart::Never => None,
            },
            standard_out_path: self.log_path(&s.name),
            standard_error_path: self.log_path(&s.name),
            ..Default::default()
        })
    }

    fn render_rotate_plist(&self) -> Vec<u8> {
        plist::render(&plist::Job {
            label: label(ROTATE_NAME),
            program_arguments: [
                self.hum_path.as_str(),
                "logs",
                "--rotate",
                "--all",
                "--if-oversize",
            ]
            .map(String::from)
            .into(),
            environment_variables: self.job_env(),
            start_interval: Some(ROTATE_INTERVAL),
            standard_out_path: self.log_path(ROTATE_NAME),
            standard_error_path: self.log_path(ROTATE_NAME),
            ..Default::default()
        })
    }

    /// Writes a plist if it differs; reports whether it changed.
    fn write_plist(&self, name: &str, data: &[u8]) -> Result<bool> {
        let path = self.paths.plist_file(name);
        if fs::read(&path).is_ok_and(|cur| cur == data) {
            return Ok(false);
        }
        fs::create_dir_all(&self.paths.log_dir)?;
        config::write_atomic(&path, data)?;
        Ok(true)
    }

    /// Regenerates a service's plist file without touching launchd.
    pub fn sync_plist(&self, s: &Service) -> Result<bool> {
        self.write_plist(&s.name, &self.render_plist(s))
    }

    /// (plist exists, plist matches the config)
    pub fn plist_current(&self, s: &Service) -> (bool, bool) {
        match fs::read(self.paths.plist_file(&s.name)) {
            Ok(cur) => (true, cur == self.render_plist(s)),
            Err(_) => (false, false),
        }
    }

    /// Enables the service, regenerates its plist and starts it. A running
    /// service is left alone.
    pub fn start(&self, name: &str) -> Result<()> {
        let s = config::load_service(&self.paths, name)?;
        let label = label(name);
        let st = self.ld.status(&label)?;
        if st.running {
            println!(
                "{name}: already running (pid {})",
                st.pid.unwrap_or_default()
            );
            return Ok(());
        }
        let changed = self.sync_plist(&s)?;
        self.ld.enable(&label)?;
        let plist = self.paths.plist_file(name);
        let mut loaded = st.loaded;
        if loaded && (changed || st.path.as_deref() != Some(&*plist.to_string_lossy())) {
            // Loaded with an outdated definition: reload it.
            self.ld.bootout(&label)?;
            loaded = false;
        }
        if !loaded {
            self.ld.bootstrap(&plist)?;
        }
        // bootstrap only runs the job when RunAtLoad is set.
        self.ld.kickstart(&label)?;
        println!("{name}: started");
        self.ensure_rotate_job()
    }

    /// Unloads the service and disables it so it stays stopped after login.
    pub fn stop(&self, name: &str) -> Result<()> {
        config::validate_name(name)?;
        let label = label(name);
        self.ld.bootout(&label)?;
        self.ld.disable(&label)?;
        println!("{name}: stopped");
        Ok(())
    }

    /// Regenerates the plist and reloads the job. A stopped (disabled)
    /// service is enabled and started.
    pub fn restart(&self, name: &str) -> Result<()> {
        let s = config::load_service(&self.paths, name)?;
        let label = label(name);
        self.sync_plist(&s)?;
        self.ld.bootout(&label)?;
        self.ld.enable(&label)?;
        self.ld.bootstrap(&self.paths.plist_file(name))?;
        self.ld.kickstart(&label)?;
        println!("{name}: restarted");
        self.ensure_rotate_job()
    }

    /// Unloads the service, deletes its plist and, unless `keep_config`,
    /// its config file.
    pub fn remove(&self, name: &str, keep_config: bool) -> Result<()> {
        config::validate_name(name)?;
        let (cfg, plist) = (self.paths.service_file(name), self.paths.plist_file(name));
        if !cfg.exists() && !plist.exists() {
            bail!("service {name:?} not found");
        }
        let label = label(name);
        self.ld.bootout(&label)?;
        // Drop the disabled override so a future service with this name
        // does not inherit it.
        self.ld.enable(&label)?;
        remove_if_exists(&plist)?;
        if keep_config {
            println!("{name}: unregistered (config kept at {})", cfg.display());
        } else {
            remove_if_exists(&cfg)?;
            println!("{name}: removed");
        }
        self.prune_rotate_job()
    }

    /// Makes a config edit take effect: the plist is regenerated, and a
    /// running service is restarted when `restart` is set.
    pub fn apply_config_change(&self, name: &str, restart: bool) -> Result<()> {
        let s = config::load_service(&self.paths, name)?;
        if !self.ld.status(&label(name))?.running {
            // Keep the file current for the next login; launchd re-reads it
            // on start/restart.
            if self.paths.plist_file(name).exists() {
                self.sync_plist(&s)?;
            }
            return Ok(());
        }
        if !restart {
            println!("{name}: running; run `hum restart {name}` to apply");
            return Ok(());
        }
        self.restart(name)
    }

    /// Installs and loads the periodic rotate job.
    pub fn ensure_rotate_job(&self) -> Result<()> {
        let label = label(ROTATE_NAME);
        let changed = self.write_plist(ROTATE_NAME, &self.render_rotate_plist())?;
        let st = self.ld.status(&label)?;
        if st.loaded && !changed {
            return Ok(());
        }
        if st.loaded {
            self.ld.bootout(&label)?;
        }
        self.ld.enable(&label)?;
        self.ld.bootstrap(&self.paths.plist_file(ROTATE_NAME))
    }

    /// Removes the rotate job once no service is left.
    fn prune_rotate_job(&self) -> Result<()> {
        if !self.paths.service_names()?.is_empty() {
            return Ok(());
        }
        self.ld.bootout(&label(ROTATE_NAME))?;
        remove_if_exists(&self.paths.plist_file(ROTATE_NAME))
    }
}

// --- state ---

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Running,
    Stopped,
    Crashed,
    Disabled,
}

impl State {
    pub fn of(st: &Status, disabled: bool) -> State {
        if st.running {
            State::Running
        } else if disabled {
            State::Disabled
        } else if st.crashed() {
            State::Crashed
        } else {
            State::Stopped
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            State::Running => "running",
            State::Stopped => "stopped",
            State::Crashed => "crashed",
            State::Disabled => "disabled",
        }
    }
}

/// One row of `hum ls`.
#[derive(Serialize, Debug)]
pub struct ServiceState {
    pub name: String,
    pub state: Option<State>,
    pub pid: Option<u32>,
    pub last_exit: Option<i32>,
    pub last_signal: Option<String>,
    pub command: Option<String>,
    /// Set when launchd does not match the config directory.
    pub out_of_sync: Option<String>,
    pub error: Option<String>,
}

pub const NO_CONFIG: &str = "plist without config";

impl App {
    pub fn states(&self) -> Result<Vec<ServiceState>> {
        let disabled = self.ld.disabled()?;
        let mut rows = vec![];
        let cfg_names = self.paths.service_names()?;
        for name in &cfg_names {
            let mut row = ServiceState {
                name: name.clone(),
                state: None,
                pid: None,
                last_exit: None,
                last_signal: None,
                command: None,
                out_of_sync: None,
                error: None,
            };
            if let Err(e) = config::validate_name(name) {
                row.error = Some(e.to_string());
                rows.push(row);
                continue;
            }
            let st = self.fill_status(&mut row, &disabled)?;
            match config::load_service(&self.paths, name) {
                Ok(s) => {
                    row.command = Some(s.command_string());
                    row.out_of_sync = self.sync_reason(&s, &st);
                }
                Err(e) => row.error = Some(format!("{e:#}")),
            }
            rows.push(row);
        }
        for name in self.paths.plist_names()? {
            if cfg_names.contains(&name) {
                continue;
            }
            let mut row = ServiceState {
                name,
                state: None,
                pid: None,
                last_exit: None,
                last_signal: None,
                command: None,
                out_of_sync: Some(NO_CONFIG.into()),
                error: None,
            };
            self.fill_status(&mut row, &disabled)?;
            rows.push(row);
        }
        Ok(rows)
    }

    fn fill_status(&self, row: &mut ServiceState, disabled: &HashSet<String>) -> Result<Status> {
        let label = label(&row.name);
        let st = self.ld.status(&label)?;
        row.state = Some(State::of(&st, disabled.contains(&label)));
        row.pid = st.pid;
        row.last_exit = st.last_exit;
        row.last_signal = st.last_signal.clone();
        Ok(st)
    }

    pub fn sync_reason(&self, s: &Service, st: &Status) -> Option<String> {
        let plist = self.paths.plist_file(&s.name);
        match self.plist_current(s) {
            (false, _) => Some("plist missing".into()),
            (true, false) => Some("plist outdated".into()),
            _ => match &st.path {
                Some(p) if st.loaded && Path::new(p) != plist => Some(format!("loaded from {p}")),
                _ => None,
            },
        }
    }
}

// --- apply ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Register,
    Update,
    Unregister,
}

impl ActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::Register => "register",
            ActionKind::Update => "update",
            ActionKind::Unregister => "unregister",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub kind: ActionKind,
    pub name: String,
}

impl App {
    /// What `apply` would do, and the configs it has to skip.
    pub fn plan(&self) -> Result<(Vec<Action>, Vec<anyhow::Error>)> {
        let mut actions = vec![];
        let mut problems = vec![];
        let cfg_names = self.paths.service_names()?;
        for name in &cfg_names {
            let s = match config::load_service(&self.paths, name) {
                Ok(s) => s,
                Err(e) => {
                    problems.push(e);
                    continue;
                }
            };
            let kind = match self.plist_current(&s) {
                (false, _) => ActionKind::Register,
                (true, false) => ActionKind::Update,
                _ => continue,
            };
            actions.push(Action {
                kind,
                name: name.clone(),
            });
        }
        for name in self.paths.plist_names()? {
            if !cfg_names.contains(&name) {
                actions.push(Action {
                    kind: ActionKind::Unregister,
                    name,
                });
            }
        }
        Ok((actions, problems))
    }

    /// Aligns launchd with the config directory.
    pub fn apply(&self, actions: &[Action]) -> Result<()> {
        for act in actions {
            let label = label(&act.name);
            let plist = self.paths.plist_file(&act.name);
            match act.kind {
                ActionKind::Register => {
                    self.sync_plist(&config::load_service(&self.paths, &act.name)?)?;
                    self.ld.enable(&label)?;
                    self.ld.bootstrap(&plist)?;
                }
                ActionKind::Update => {
                    let s = config::load_service(&self.paths, &act.name)?;
                    let st = self.ld.status(&label)?;
                    self.sync_plist(&s)?;
                    if st.loaded {
                        self.ld.bootout(&label)?;
                        self.ld.bootstrap(&plist)?;
                        if st.running {
                            self.ld.kickstart(&label)?;
                        }
                    }
                }
                ActionKind::Unregister => {
                    self.ld.bootout(&label)?;
                    self.ld.enable(&label)?;
                    remove_if_exists(&plist)?;
                }
            }
            println!("{}: {}", act.name, act.kind.as_str());
        }
        if self.paths.service_names()?.is_empty() {
            self.prune_rotate_job()
        } else {
            self.ensure_rotate_job()
        }
    }
}

// --- doctor ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warn,
    Error,
}

#[derive(Debug)]
pub struct Finding {
    pub severity: Severity,
    pub subject: String,
    pub message: String,
}

/// Logs this many times larger than max_size mean rotation is not running.
const OVERSIZE_FACTOR: u64 = 2;

/// Checks the hum path a plist runs.
fn check_program(plist_path: &Path) -> Option<(Severity, String)> {
    let data = fs::read(plist_path).ok()?;
    let Some(prog) = plist::program(&data) else {
        return Some((
            Severity::Error,
            format!("cannot read ProgramArguments from {}", plist_path.display()),
        ));
    };
    if environ::look_path(&prog, "").is_none() {
        Some((
            Severity::Error,
            format!("plist runs {prog}, which is missing or not executable; run `hum apply`"),
        ))
    } else if prog.contains("/Cellar/") {
        Some((
            Severity::Warn,
            format!(
                "plist runs versioned path {prog}; it breaks on upgrade (put hum on PATH and run `hum apply`)"
            ),
        ))
    } else {
        None
    }
}

impl App {
    pub fn doctor(&self) -> Result<Vec<Finding>> {
        let mut out = vec![];
        let mut add = |subject: &str, (severity, message): (Severity, String)| {
            out.push(Finding {
                severity,
                subject: subject.into(),
                message,
            });
        };
        let global = match config::load_global(&self.paths) {
            Ok(g) if g.env.get("PATH").is_none_or(|p| p.is_empty()) => {
                let msg = format!(
                    "no PATH in {} [env]; services get no PATH under launchd",
                    self.paths.global_file().display()
                );
                add("global", (Severity::Warn, msg));
                g
            }
            Ok(g) => {
                add("global", (Severity::Ok, "config ok".into()));
                g
            }
            Err(e) => {
                add("global", (Severity::Error, format!("{e:#}")));
                Default::default()
            }
        };
        let global_path = global.env.get("PATH").cloned().unwrap_or_default();

        let names = self.paths.service_names()?;
        for name in &names {
            let s = match config::load_service(&self.paths, name) {
                Ok(s) => s,
                Err(e) => {
                    add(name, (Severity::Error, format!("{e:#}")));
                    continue;
                }
            };
            let mut problems = vec![];
            if let config::Command::Argv(argv) = &s.command
                && environ::look_path(&argv[0], &global_path).is_none()
            {
                problems.push((
                    Severity::Error,
                    format!("command {} not found in global PATH", argv[0]),
                ));
            }
            let cwd = s.resolved_cwd(&self.paths.home);
            if !cwd.is_dir() {
                problems.push((
                    Severity::Error,
                    format!("cwd {} does not exist", cwd.display()),
                ));
            }
            for f in s.resolved_env_files(&self.paths.home) {
                if !f.exists() {
                    problems.push((
                        Severity::Error,
                        format!(
                            "env_file {} does not exist; `hum run` will refuse to start",
                            f.display()
                        ),
                    ));
                }
            }
            if let Some(reason) = self.sync_reason(&s, &self.ld.status(&label(name))?) {
                problems.push((
                    Severity::Warn,
                    format!("launchd out of sync ({reason}); run `hum apply`"),
                ));
            }
            problems.extend(check_program(&self.paths.plist_file(name)));
            let (max_size, _) = config::effective_log(&global, Some(&s));
            if let Ok(m) = fs::metadata(self.paths.log_file(name))
                && m.len() > OVERSIZE_FACTOR * max_size
            {
                problems.push((
                    Severity::Warn,
                    format!(
                        "log is {} bytes, over {OVERSIZE_FACTOR}× max_size; is rotation running?",
                        m.len()
                    ),
                ));
            }
            if problems.is_empty() {
                problems.push((Severity::Ok, "ok".into()));
            }
            for p in problems {
                add(name, p);
            }
        }
        for name in self.paths.plist_names()? {
            if !names.contains(&name) {
                add(
                    &name,
                    (
                        Severity::Warn,
                        "plist without config; run `hum apply` to unregister".into(),
                    ),
                );
            }
        }
        if !names.is_empty() {
            let rp = self.paths.plist_file(ROTATE_NAME);
            if !rp.exists() {
                add(
                    ROTATE_NAME,
                    (
                        Severity::Warn,
                        "log rotation job not installed; run `hum apply`".into(),
                    ),
                );
            }
            if let Some(f) = check_program(&rp) {
                add(ROTATE_NAME, f);
            }
        }
        Ok(out)
    }
}

// --- run ---

/// Everything `hum run` needs to replace itself with the service.
pub struct Exec {
    pub program: PathBuf,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub dir: PathBuf,
}

impl App {
    /// Loads the service, rotates its log if oversized and assembles the
    /// environment from scratch.
    pub fn prepare_run(&self, name: &str, base: &BTreeMap<String, String>) -> Result<Exec> {
        let s = config::load_service(&self.paths, name)?;
        let global = config::load_global(&self.paths)?;
        let (max_size, keep) = config::effective_log(&global, Some(&s));
        if let Err(e) = logfile::rotate_if_needed(
            &self.paths.log_file(name),
            max_size,
            keep,
            chrono::Local::now(),
        ) {
            eprintln!("warning: log rotation: {e}");
        }
        let (env, err) = environ::assemble(base, &global, &s, &self.paths.home);
        if let Some(e) = err {
            return Err(anyhow::Error::new(e).context(format!("{name}: not starting")));
        }
        for u in &env.undefined {
            eprintln!(
                "warning: {}:{}: ${{{}}} is undefined, expanded to \"\"",
                u.file, u.line, u.name
            );
        }
        let argv = s.argv();
        let path = env.vars.get("PATH").map(String::as_str).unwrap_or("");
        let program = environ::look_path(&argv[0], path)
            .with_context(|| format!("{name}: {} not found in PATH", argv[0]))?;
        Ok(Exec {
            program,
            argv,
            env: env.vars,
            dir: s.resolved_cwd(&self.paths.home),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environ::EnvFileError;
    use std::cell::RefCell;

    /// Models the parts of launchd hum relies on.
    #[derive(Default)]
    struct FakeLd {
        jobs: RefCell<BTreeMap<String, Status>>,
        disabled: RefCell<HashSet<String>>,
        calls: RefCell<Vec<String>>,
    }

    impl FakeLd {
        fn log(&self, s: String) {
            self.calls.borrow_mut().push(s);
        }
    }

    impl Manager for &'static FakeLd {
        fn status(&self, label: &str) -> Result<Status> {
            Ok(self.jobs.borrow().get(label).cloned().unwrap_or_default())
        }
        fn disabled(&self) -> Result<HashSet<String>> {
            Ok(self.disabled.borrow().clone())
        }
        fn bootstrap(&self, plist: &Path) -> Result<()> {
            let label = plist.file_stem().unwrap().to_string_lossy().into_owned();
            self.log(format!("bootstrap {label}"));
            if self.disabled.borrow().contains(&label) {
                bail!("service is disabled");
            }
            if self.jobs.borrow().contains_key(&label) {
                bail!("already loaded");
            }
            let data = String::from_utf8(fs::read(plist)?)?;
            let run_at_load = data.contains("<key>RunAtLoad</key>\n\t<true/>");
            let st = Status {
                loaded: true,
                running: run_at_load,
                pid: run_at_load.then_some(100),
                path: Some(plist.display().to_string()),
                ..Default::default()
            };
            self.jobs.borrow_mut().insert(label, st);
            Ok(())
        }
        fn bootout(&self, label: &str) -> Result<()> {
            if self.jobs.borrow_mut().remove(label).is_some() {
                self.log(format!("bootout {label}"));
            }
            Ok(())
        }
        fn enable(&self, label: &str) -> Result<()> {
            self.disabled.borrow_mut().remove(label);
            Ok(())
        }
        fn disable(&self, label: &str) -> Result<()> {
            self.disabled.borrow_mut().insert(label.into());
            Ok(())
        }
        fn kickstart(&self, label: &str) -> Result<()> {
            let mut jobs = self.jobs.borrow_mut();
            let st = jobs.get_mut(label).context("not loaded")?;
            if !st.running {
                self.log(format!("kickstart {label}"));
                st.running = true;
                st.pid = Some(200);
            }
            Ok(())
        }
    }

    struct Fixture {
        app: App,
        ld: &'static FakeLd,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::with_home(dir.path().into(), None);
        config::ensure_global(&paths, "/usr/bin:/bin").unwrap();
        let ld: &'static FakeLd = Box::leak(Box::default());
        Fixture {
            app: App {
                paths,
                ld: Box::new(ld),
                hum_path: "/opt/homebrew/bin/hum".into(),
            },
            ld,
            _dir: dir,
        }
    }

    impl Fixture {
        fn write(&self, name: &str, body: &str) {
            config::write_atomic(&self.app.paths.service_file(name), body.as_bytes()).unwrap();
        }
        fn status(&self, name: &str) -> Status {
            self.ld.status(&label(name)).unwrap()
        }
        fn take_calls(&self) -> Vec<String> {
            std::mem::take(&mut *self.ld.calls.borrow_mut())
        }
    }

    // Requirement: start, stop, restart, rm
    #[test]
    fn start_stop_restart_remove() {
        let f = fixture();
        let a = &f.app;
        f.write("t3", r#"command = ["sleep", "1000"]"#);
        a.start("t3").unwrap();
        assert!(f.status("t3").running);
        assert!(a.paths.plist_file("t3").exists());
        assert!(a.paths.plist_file(ROTATE_NAME).exists());
        assert!(f.status(ROTATE_NAME).loaded);

        f.take_calls();
        a.start("t3").unwrap();
        assert!(
            f.take_calls().is_empty(),
            "start on a running service is a no-op"
        );

        a.stop("t3").unwrap();
        assert_eq!(a.states().unwrap()[0].state, Some(State::Disabled));

        a.restart("t3").unwrap();
        assert!(f.status("t3").running);
        assert!(!f.ld.disabled.borrow().contains("hum.t3"));

        a.remove("t3", false).unwrap();
        for p in [
            a.paths.plist_file("t3"),
            a.paths.service_file("t3"),
            a.paths.plist_file(ROTATE_NAME),
        ] {
            assert!(!p.exists(), "{} still exists", p.display());
        }
        assert!(!f.status("t3").loaded && !f.status(ROTATE_NAME).loaded);
    }

    #[test]
    fn start_runs_services_without_autostart() {
        let f = fixture();
        f.write("x", "command = [\"sleep\", \"1\"]\nautostart = false\n");
        f.app.start("x").unwrap();
        assert!(f.status("x").running);
    }

    #[test]
    fn start_reloads_an_outdated_loaded_job() {
        let f = fixture();
        f.write("x", "command = [\"sleep\", \"1\"]\nautostart = false\n");
        f.app.start("x").unwrap();
        f.ld.jobs.borrow_mut().get_mut("hum.x").unwrap().running = false;
        f.write(
            "x",
            "command = [\"sleep\", \"1\"]\nautostart = false\nrestart = \"never\"\n",
        );
        f.take_calls();
        f.app.start("x").unwrap();
        assert_eq!(
            f.take_calls(),
            ["bootout hum.x", "bootstrap hum.x", "kickstart hum.x"]
        );
    }

    // Requirement: 标出与 launchd 不一致, apply 对齐, apply 跳过无效配置
    #[test]
    fn states_and_apply() {
        let f = fixture();
        let a = &f.app;
        f.write("new1", r#"command = ["sleep", "1"]"#);
        f.write("changed", r#"command = ["sleep", "1"]"#);
        f.write("broken", "command = 3");
        a.start("changed").unwrap();
        f.write(
            "changed",
            "command = [\"sleep\", \"2\"]\nrestart = \"on-failure\"\n",
        );
        fs::write(a.paths.plist_file("orphan"), "<plist/>").unwrap();
        f.ld.jobs.borrow_mut().insert(
            "hum.orphan".into(),
            Status {
                loaded: true,
                running: true,
                pid: Some(9),
                ..Default::default()
            },
        );

        let rows = a.states().unwrap();
        let row = |n: &str| rows.iter().find(|r| r.name == n).unwrap();
        assert_eq!(row("new1").out_of_sync.as_deref(), Some("plist missing"));
        assert_eq!(
            row("changed").out_of_sync.as_deref(),
            Some("plist outdated")
        );
        assert_eq!(row("changed").state, Some(State::Running));
        assert!(row("broken").error.is_some());
        assert_eq!(row("orphan").out_of_sync.as_deref(), Some(NO_CONFIG));

        let (actions, problems) = a.plan().unwrap();
        assert_eq!(problems.len(), 1);
        let summary: Vec<_> = actions
            .iter()
            .map(|x| format!("{} {}", x.kind.as_str(), x.name))
            .collect();
        assert_eq!(
            summary,
            ["update changed", "register new1", "unregister orphan"]
        );
        a.apply(&actions).unwrap();
        assert!(
            f.status("changed").running,
            "running service keeps running after reload"
        );
        assert!(
            f.status("new1").running,
            "registered and started (autostart)"
        );
        assert!(!f.status("orphan").loaded);
        assert!(!a.paths.plist_file("orphan").exists());
        assert!(a.plan().unwrap().0.is_empty());
    }

    // Requirement: 服务配置改动的生效
    #[test]
    fn config_change_restarts_only_when_asked() {
        let f = fixture();
        f.write("x", r#"command = ["sleep", "1"]"#);
        f.app.start("x").unwrap();
        f.write("x", "command = [\"sleep\", \"1\"]\nautostart = false\n");
        f.take_calls();
        f.app.apply_config_change("x", false).unwrap();
        assert!(f.take_calls().is_empty());
        f.app.apply_config_change("x", true).unwrap();
        assert!(
            f.take_calls()
                .join(";")
                .contains("bootout hum.x;bootstrap hum.x")
        );
        let s = config::load_service(&f.app.paths, "x").unwrap();
        assert_eq!(f.app.plist_current(&s), (true, true));
    }

    // Requirement: 环境从零组装, env 文件缺失时拒绝启动
    #[test]
    fn prepare_run() {
        let f = fixture();
        let home = f.app.paths.home.clone();
        let cwd = home.join("work");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(cwd.join(".env"), "FROM_FILE=${HOME}/x\n").unwrap();
        f.write(
            "t3",
            "command = [\"sh\", \"-c\", \"env\"]\ncwd = \"~/work\"\nenv_file = [\".env\"]\n[env]\nPORT = \"3773\"\n",
        );
        let base = BTreeMap::from([
            ("HOME".to_string(), home.display().to_string()),
            ("USER".into(), "me".into()),
        ]);
        let ex = f.app.prepare_run("t3", &base).unwrap();
        assert!(ex.program.ends_with("bin/sh"), "{}", ex.program.display());
        assert_eq!(ex.dir, cwd);
        assert_eq!(ex.env["PATH"], "/usr/bin:/bin");
        assert_eq!(ex.env["PORT"], "3773");
        assert_eq!(ex.env["FROM_FILE"], format!("{}/x", home.display()));
        assert_eq!(ex.env["USER"], "me");

        fs::remove_file(cwd.join(".env")).unwrap();
        let err = f.app.prepare_run("t3", &base).err().unwrap();
        assert!(err.downcast_ref::<EnvFileError>().is_some(), "{err:#}");
    }

    // Requirement: 启动前检查日志大小
    #[test]
    fn prepare_run_rotates_an_oversized_log() {
        let f = fixture();
        f.write(
            "x",
            "command = [\"sh\"]\n[log]\nmax_size = \"10\"\nkeep = 2\n",
        );
        fs::create_dir_all(&f.app.paths.log_dir).unwrap();
        fs::write(f.app.paths.log_file("x"), "more than ten bytes").unwrap();
        f.app.prepare_run("x", &BTreeMap::new()).unwrap();
        let files = logfile::rotated(&f.app.paths.log_file("x")).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(
            fs::read_to_string(&files[0]).unwrap(),
            "more than ten bytes"
        );
    }

    // Requirement: doctor 检查项
    #[test]
    fn doctor_reports_problems() {
        let f = fixture();
        f.write("bad", "command = [\"definitely-not-a-command\"]\ncwd = \"/nonexistent\"\nenv_file = [\".env\"]\n");
        let msgs: Vec<String> = f
            .app
            .doctor()
            .unwrap()
            .into_iter()
            .filter(|x| x.subject == "bad")
            .map(|x| x.message)
            .collect();
        let joined = msgs.join("\n");
        for want in [
            "not found in global PATH",
            "cwd /nonexistent does not exist",
            "env_file /nonexistent/.env",
            "plist missing",
        ] {
            assert!(joined.contains(want), "missing {want:?} in:\n{joined}");
        }
    }
}
