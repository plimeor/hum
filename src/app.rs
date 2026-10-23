//! hum's operations on top of config, plist and launchd. Operations return
//! what happened; the CLI layer parses arguments and prints.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::{self, Paths, ROTATE_NAME, Restart, Service, ServiceName, label};
use crate::environ;
use crate::launchd::{Manager, Status};
use crate::{logfile, plist};

/// How often the rotate job checks log sizes, in seconds
/// (pm2-logrotate's workerInterval default).
pub const ROTATE_INTERVAL: u64 = 30;

/// Arguments the rotate job passes to hum; a CLI test parses them.
pub const ROTATE_ARGS: [&str; 4] = ["logs", "--rotate", "--all", "--if-oversize"];

/// The plist variable naming the hum the launcher copy hands over to.
pub const EXEC_VAR: &str = "HUM_EXEC";

pub struct App {
    pub paths: Paths,
    pub ld: Box<dyn Manager>,
    /// The hum every job hands over to, written into plists as `HUM_EXEC`.
    pub hum_path: String,
    /// The running binary, which the launcher copy is made from.
    pub hum_binary: PathBuf,
}

/// The hum a launcher copy started by launchd should become: the one in
/// `HUM_EXEC`, unless that is this binary or cannot be run.
pub fn handover_target(exec_var: Option<&std::ffi::OsStr>, current: &Path) -> Option<PathBuf> {
    let target = PathBuf::from(exec_var?);
    let real = fs::canonicalize(&target).ok()?;
    let runnable = environ::look_path(&target.to_string_lossy(), "").is_some();
    (runnable && real != current).then_some(target)
}

/// Hands this process over to the hum in `HUM_EXEC`, with the same
/// arguments. A process keeps the identity launchd started it with across
/// exec, so grants stay tied to the launcher copy while the latest hum
/// runs. When the hand-over is not possible the copy carries on itself.
pub fn hand_over() {
    let exec_var = std::env::var_os(EXEC_VAR);
    let current = std::env::current_exe()
        .and_then(fs::canonicalize)
        .unwrap_or_default();
    let Some(target) = handover_target(exec_var.as_deref(), &current) else {
        return;
    };
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(&target)
        .args(std::env::args_os().skip(1))
        .env_remove(EXEC_VAR)
        .exec();
    eprintln!(
        "hum: handing over to {}: {err}; carrying on as {}",
        target.display(),
        current.display()
    );
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

/// When the content behind path last changed: the latest of the file's
/// modification time and that of every symbolic link met while resolving
/// path, since pointing a link elsewhere changes the content without
/// touching the new file. None when the file does not exist.
fn last_change(path: &Path) -> Option<std::time::SystemTime> {
    let modified = fs::metadata(path).and_then(|m| m.modified()).ok()?;
    Some(link_change(path).map_or(modified, |t| t.max(modified)))
}

/// The latest modification time of the symbolic links met while resolving
/// path: when path last started resolving to where it does now. None when
/// path meets no link or does not resolve.
fn link_change(path: &Path) -> Option<std::time::SystemTime> {
    use std::path::Component;
    let mut latest: Option<std::time::SystemTime> = None;
    let mut base = PathBuf::new();
    let mut rest: Vec<std::ffi::OsString> = path
        .components()
        .rev()
        .map(|c| c.as_os_str().to_owned())
        .collect();
    let mut links = 0;
    while let Some(part) = rest.pop() {
        match Path::new(&part).components().next() {
            Some(Component::RootDir) => base = PathBuf::from("/"),
            Some(Component::CurDir) => {}
            Some(Component::ParentDir) => {
                base.pop();
            }
            _ => {
                let next = base.join(&part);
                match fs::symlink_metadata(&next) {
                    // Bounds the walk on a link loop.
                    Ok(m) if m.file_type().is_symlink() && links < 40 => {
                        links += 1;
                        if let Ok(t) = m.modified() {
                            latest = latest.max(Some(t));
                        }
                        let target = fs::read_link(&next).ok()?;
                        rest.extend(target.components().rev().map(|c| c.as_os_str().to_owned()));
                    }
                    _ => base = next,
                }
            }
        }
    }
    latest
}

/// What brings an out-of-sync service in line with its config. A stopped
/// (disabled) service only needs its plist rewritten, which `apply` does
/// without starting it; `restart` would start it.
pub fn sync_fix(name: &str, disabled: bool) -> String {
    if disabled {
        "run `hum apply` to update it; it stays stopped".into()
    } else {
        format!("run `hum restart {name}` to apply")
    }
}

pub enum StartOutcome {
    AlreadyRunning { pid: Option<u32> },
    Started,
}

pub enum ChangeOutcome {
    /// The running service was restarted.
    Restarted,
    /// The change waits for a restart: the service is running with the
    /// old config (`running`), or launchd holds the old definition of a job
    /// that is loaded but not running.
    NeedsRestart { running: bool },
    /// Nothing waits: the service reads the new config when it next starts.
    Applied,
}

impl App {
    pub fn new() -> Result<Self> {
        Ok(App {
            paths: Paths::from_env()?,
            ld: crate::launchd::new(),
            hum_path: hum_path(),
            hum_binary: std::env::current_exe()
                .and_then(fs::canonicalize)
                .unwrap_or_default(),
        })
    }

    fn log_path(&self, name: &str) -> Option<String> {
        Some(self.paths.log_file(name).display().to_string())
    }

    fn launcher_path(&self) -> String {
        self.paths.launcher().display().to_string()
    }

    fn exec_env(hum_path: &str) -> BTreeMap<String, String> {
        [(EXEC_VAR.to_string(), hum_path.to_string())].into()
    }

    /// The plist a service should have.
    pub fn render_plist(&self, s: &Service) -> Vec<u8> {
        self.render_plist_with(s, &self.hum_path)
    }

    fn render_plist_with(&self, s: &Service, hum_path: &str) -> Vec<u8> {
        plist::render(&plist::Job {
            label: label(&s.name),
            program_arguments: vec![self.launcher_path(), "run".into(), s.name.to_string()],
            environment_variables: Self::exec_env(hum_path),
            // Login brings a service back unless `hum stop` disabled it.
            run_at_load: true,
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
        self.render_rotate_plist_with(&self.hum_path)
    }

    fn render_rotate_plist_with(&self, hum_path: &str) -> Vec<u8> {
        let mut args = vec![self.launcher_path()];
        args.extend(ROTATE_ARGS.map(String::from));
        plist::render(&plist::Job {
            label: label(ROTATE_NAME),
            program_arguments: args,
            environment_variables: Self::exec_env(hum_path),
            start_interval: Some(ROTATE_INTERVAL),
            standard_out_path: self.log_path(ROTATE_NAME),
            standard_error_path: self.log_path(ROTATE_NAME),
            ..Default::default()
        })
    }

    /// Makes the launcher copy a copy of the running hum. It is replaced
    /// in place, since a replacement at another path loses its grants.
    fn write_launcher(&self) -> Result<()> {
        let copy = self.paths.launcher();
        let data = fs::read(&self.hum_binary)
            .with_context(|| format!("reading {}", self.hum_binary.display()))?;
        if fs::read(&copy).is_ok_and(|cur| cur == data) {
            return Ok(());
        }
        fs::create_dir_all(&self.paths.support_dir)?;
        let mut tmp = tempfile::NamedTempFile::new_in(&self.paths.support_dir)?;
        std::io::Write::write_all(&mut tmp, &data)?;
        use std::os::unix::fs::PermissionsExt;
        tmp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o755))?;
        tmp.persist(&copy)
            .with_context(|| format!("writing {}", copy.display()))?;
        Ok(())
    }

    /// Writes the launcher copy and a plist file if it differs; reports
    /// whether the plist changed.
    fn write_plist(&self, name: &str, data: &[u8]) -> Result<bool> {
        self.write_launcher()?;
        let path = self.paths.plist_file(name);
        if fs::read(&path).is_ok_and(|cur| cur == data) {
            return Ok(false);
        }
        fs::create_dir_all(&self.paths.log_dir)?;
        config::write_atomic(&path, data)?;
        Ok(true)
    }

    /// Writes a job's plist and makes launchd hold that definition.
    ///
    /// launchd keeps the definition it loaded and does not re-read the
    /// file, so a loaded job whose plist changed (or that was loaded from
    /// another path) is reloaded here, and kept running if it was. Every
    /// write of a plist whose job may be loaded goes through here, which
    /// keeps "loaded definition = plist on disk" true. Returns the status
    /// afterwards.
    fn install(&self, name: &str, data: &[u8]) -> Result<Status> {
        let label = label(name);
        let plist = self.paths.plist_file(name);
        let st = self.ld.status(&label)?;
        let changed = self.write_plist(name, data)?;
        let foreign = st.path.as_deref().is_some_and(|p| Path::new(p) != plist);
        if st.loaded && (changed || foreign) {
            self.ld.bootout(&label)?;
            self.ld.bootstrap(&plist)?;
            if st.running {
                self.ld.kickstart(&label)?;
            }
        }
        self.ld.status(&label)
    }

    /// Enables the service, brings its plist up to date and starts it. A
    /// running service is left alone.
    pub fn start(&self, name: &ServiceName) -> Result<StartOutcome> {
        let s = config::load_service(&self.paths, name)?;
        let label = label(name);
        let st = self.ld.status(&label)?;
        if st.running {
            return Ok(StartOutcome::AlreadyRunning { pid: st.pid });
        }
        let st = self.install(name, &self.render_plist(&s))?;
        self.clear_disabled(&label)?;
        if !st.loaded {
            self.ld.bootstrap(&self.paths.plist_file(name))?;
        }
        // A job that is loaded but exited (e.g. crashed) is not restarted
        // by bootstrap; kickstart runs it, and leaves a running job alone.
        self.ld.kickstart(&label)?;
        self.ensure_rotate_job()?;
        Ok(StartOutcome::Started)
    }

    /// Unloads the service and disables it so it stays stopped after login.
    pub fn stop(&self, name: &ServiceName) -> Result<()> {
        let label = label(name);
        self.ld.bootout(&label)?;
        self.ld.disable(&label)
    }

    /// Regenerates the plist and reloads the job. A stopped (disabled)
    /// service is enabled and started.
    pub fn restart(&self, name: &ServiceName) -> Result<()> {
        let s = config::load_service(&self.paths, name)?;
        let label = label(name);
        self.ld.bootout(&label)?;
        self.write_plist(name, &self.render_plist(&s))?;
        self.clear_disabled(&label)?;
        self.ld.bootstrap(&self.paths.plist_file(name))?;
        self.ld.kickstart(&label)?;
        self.ensure_rotate_job()
    }

    /// Unloads the service, deletes its plist and, unless `keep_config`,
    /// its config file and logs.
    pub fn remove(&self, name: &ServiceName, keep_config: bool) -> Result<()> {
        let (cfg, plist) = (self.paths.service_file(name), self.paths.plist_file(name));
        if !cfg.exists() && !plist.exists() {
            bail!("service {name:?} not found");
        }
        let label = label(name);
        self.ld.bootout(&label)?;
        // A future service with this name must not inherit `hum stop`.
        self.clear_disabled(&label)?;
        remove_if_exists(&plist)?;
        if !keep_config {
            remove_if_exists(&cfg)?;
            let log = self.paths.log_file(name);
            logfile::remove(&log).with_context(|| format!("deleting {}", log.display()))?;
        }
        self.prune_rotate_job()
    }

    /// Makes a config edit take effect.
    ///
    /// A running service is restarted when `restart` is set. Otherwise the
    /// change waits, and `hum ls` flags it: a running service started
    /// before its config changed, and a loaded job keeps its plist as is,
    /// because reloading it would also start it under `RunAtLoad`. A job
    /// that is not loaded just gets its plist rewritten.
    pub fn apply_config_change(&self, name: &ServiceName, restart: bool) -> Result<ChangeOutcome> {
        let s = config::load_service(&self.paths, name)?;
        let st = self.ld.status(&label(name))?;
        if st.running {
            if !restart {
                return Ok(ChangeOutcome::NeedsRestart { running: true });
            }
            self.restart(name)?;
            return Ok(ChangeOutcome::Restarted);
        }
        let plist = self.paths.plist_file(name);
        if !plist.exists() {
            return Ok(ChangeOutcome::Applied);
        }
        let data = self.render_plist(&s);
        if st.loaded {
            // Only `restart` lives in the plist; everything
            // else is read by `hum run` at the next start.
            if fs::read(&plist).is_ok_and(|cur| cur == data) {
                return Ok(ChangeOutcome::Applied);
            }
            return Ok(ChangeOutcome::NeedsRestart { running: false });
        }
        self.write_plist(name, &data)?;
        Ok(ChangeOutcome::Applied)
    }

    /// Writes the plist of a service that is not registered yet; launchd
    /// is not touched.
    pub fn write_new_plist(&self, s: &Service) -> Result<()> {
        self.write_plist(&s.name, &self.render_plist(s)).map(drop)
    }

    /// Installs and loads the periodic rotate job.
    pub fn ensure_rotate_job(&self) -> Result<()> {
        let st = self.install(ROTATE_NAME, &self.render_rotate_plist())?;
        if !st.loaded {
            self.clear_disabled(&label(ROTATE_NAME))?;
            self.ld.bootstrap(&self.paths.plist_file(ROTATE_NAME))?;
        }
        Ok(())
    }

    /// Clears a disable left by `hum stop`. launchd keeps every enable or
    /// disable as a record that only root can delete, so a job that is
    /// not disabled is left alone rather than given an "enabled" record.
    fn clear_disabled(&self, label: &str) -> Result<()> {
        if self.ld.disabled()?.contains(label) {
            self.ld.enable(label)?;
        }
        Ok(())
    }

    /// Removes the rotate job once no service is left.
    fn prune_rotate_job(&self) -> Result<()> {
        if !self.paths.service_files()?.is_empty() {
            return Ok(());
        }
        self.remove_rotate_job()
    }

    fn remove_rotate_job(&self) -> Result<()> {
        self.ld.bootout(&label(ROTATE_NAME))?;
        remove_if_exists(&self.paths.plist_file(ROTATE_NAME))?;
        remove_if_exists(&self.paths.launcher())?;
        remove_if_exists(&self.paths.log_file(ROTATE_NAME))
    }

    /// Why what runs does not match the service's config, if it does not:
    /// launchd's definition differs, or the running process started before
    /// one of its config files last changed.
    pub fn sync_reason(&self, s: &Service, st: &Status) -> Option<String> {
        self.plist_reason(s, st)
            .or_else(|| self.stale_reason(s, st))
    }

    /// The config files a running service read when it started that have
    /// changed since, and its cwd if a link on that path now points
    /// elsewhere. `hum run` reads them all at start, so any later change
    /// waits for a restart.
    fn stale_reason(&self, s: &Service, st: &Status) -> Option<String> {
        let started = st.started_at.filter(|_| st.running)?;
        let mut files = vec![self.paths.service_file(&s.name), self.paths.global_file()];
        files.extend(s.resolved_env_files(&self.paths.home));
        let mut changed: Vec<String> = files
            .iter()
            .filter(|f| last_change(f).is_some_and(|t| t > started))
            .map(|f| config::contract_path(f, &self.paths.home))
            .collect();
        // Files inside cwd change all the time; only a link on its path
        // moving elsewhere changes where the service runs.
        let cwd = s.resolved_cwd(&self.paths.home);
        if link_change(&cwd).is_some_and(|t| t > started) {
            changed.push(format!(
                "cwd {}",
                config::contract_path(&cwd, &self.paths.home)
            ));
        }
        (!changed.is_empty()).then(|| format!("started before {} changed", changed.join(", ")))
    }

    /// Why launchd's definition of the job does not match the config.
    fn plist_reason(&self, s: &Service, st: &Status) -> Option<String> {
        self.definition_reason(&s.name, st, |prog| self.render_plist_with(s, prog))
    }

    /// Why the job's plist on disk, or the definition launchd loaded, is
    /// not what `render` produces with this hum's path.
    fn definition_reason(
        &self,
        name: &str,
        st: &Status,
        render: impl Fn(&str) -> Vec<u8>,
    ) -> Option<String> {
        let plist_path = self.paths.plist_file(name);
        let Ok(data) = fs::read(&plist_path) else {
            return Some("plist missing".into());
        };
        if data != render(&self.hum_path) {
            // Name the hum path when it is the only difference: running a
            // different hum binary (a dev build) makes every plist differ.
            if let Some(prog) = plist::env_var(&data, EXEC_VAR)
                && prog != self.hum_path
                && data == render(&prog)
            {
                return Some(format!("plist runs {prog}, this hum is {}", self.hum_path));
            }
            return Some("plist outdated".into());
        }
        match &st.path {
            Some(p) if st.loaded && Path::new(p) != plist_path => Some(format!("loaded from {p}")),
            _ => None,
        }
    }

    /// What `apply` has to do so the rotate job exists exactly when some
    /// service config does, and matches this hum.
    fn rotate_action(&self) -> Result<Option<Action>> {
        let st = self.ld.status(&label(ROTATE_NAME))?;
        let installed = self.paths.plist_file(ROTATE_NAME).exists();
        let action = |kind, reason: &str| {
            Some(Action {
                kind,
                subject: Subject::RotateJob,
                reason: reason.into(),
            })
        };
        if self.paths.service_files()?.is_empty() {
            return Ok(if installed || st.loaded {
                action(ActionKind::Unregister, "no service configs left")
            } else {
                None
            });
        }
        if !installed {
            return Ok(action(
                ActionKind::Register,
                "log rotation job not installed",
            ));
        }
        if let Some(reason) =
            self.definition_reason(ROTATE_NAME, &st, |prog| self.render_rotate_plist_with(prog))
        {
            return Ok(action(ActionKind::Update, &reason));
        }
        Ok(if st.loaded {
            None
        } else {
            action(ActionKind::Register, "log rotation job not loaded")
        })
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

/// What launchd reports about a service, as `hum ls` / `show` present it.
#[derive(Serialize, Debug)]
pub struct LaunchdState {
    pub state: State,
    pub pid: Option<u32>,
    pub last_exit: Option<i32>,
    pub last_signal: Option<String>,
}

/// The config side of a `hum ls` row.
#[derive(Serialize, Debug)]
#[serde(tag = "config", rename_all = "lowercase")]
pub enum ConfigState {
    Ok {
        command: String,
        /// Set when launchd does not match the config.
        out_of_sync: Option<String>,
    },
    Invalid {
        error: String,
    },
    /// A hum plist with no config file.
    Missing,
}

/// One row of `hum ls`.
#[derive(Serialize, Debug)]
pub struct ServiceState {
    pub name: String,
    /// None when the file name is not a valid service name.
    #[serde(flatten)]
    pub launchd: Option<LaunchdState>,
    #[serde(flatten)]
    pub config: ConfigState,
}

impl ServiceState {
    pub fn out_of_sync(&self) -> bool {
        matches!(
            self.config,
            ConfigState::Missing
                | ConfigState::Ok {
                    out_of_sync: Some(_),
                    ..
                }
        )
    }
}

impl App {
    fn launchd_state(
        &self,
        name: &ServiceName,
        disabled: &HashSet<String>,
    ) -> Result<(Status, LaunchdState)> {
        let label = label(name);
        let st = self.ld.status(&label)?;
        let ls = LaunchdState {
            state: State::of(&st, disabled.contains(&label)),
            pid: st.pid,
            last_exit: st.last_exit,
            last_signal: st.last_signal.clone(),
        };
        Ok((st, ls))
    }

    pub fn states(&self) -> Result<Vec<ServiceState>> {
        let disabled = self.ld.disabled()?;
        let mut rows = vec![];
        let mut configured = vec![];
        for file in self.paths.service_files()? {
            let name = match file {
                Ok(name) => name,
                Err(raw) => {
                    rows.push(ServiceState {
                        name: raw.clone(),
                        launchd: None,
                        config: ConfigState::Invalid {
                            error: format!("invalid service name {raw:?}"),
                        },
                    });
                    continue;
                }
            };
            let (st, launchd) = self.launchd_state(&name, &disabled)?;
            let config = match config::load_service(&self.paths, &name) {
                Ok(s) => ConfigState::Ok {
                    command: s.command_string(),
                    out_of_sync: self.sync_reason(&s, &st),
                },
                Err(e) => ConfigState::Invalid {
                    error: format!("{e:#}"),
                },
            };
            rows.push(ServiceState {
                name: name.to_string(),
                launchd: Some(launchd),
                config,
            });
            configured.push(name);
        }
        for name in self.paths.plist_names()? {
            if configured.contains(&name) {
                continue;
            }
            let (_, launchd) = self.launchd_state(&name, &disabled)?;
            rows.push(ServiceState {
                name: name.to_string(),
                launchd: Some(launchd),
                config: ConfigState::Missing,
            });
        }
        Ok(rows)
    }
}

// --- show ---

#[derive(Serialize, Debug)]
pub struct EnvVar {
    pub key: String,
    /// Masked unless revealed.
    pub value: String,
    pub source: String,
}

#[derive(Serialize, Debug)]
pub struct UndefinedRef {
    pub file: String,
    pub line: usize,
    pub name: String,
}

/// Everything `hum show` reports; the text and JSON outputs both render it.
#[derive(Serialize, Debug)]
pub struct Report {
    pub name: ServiceName,
    pub label: String,
    pub config: PathBuf,
    pub plist: PathBuf,
    pub log: PathBuf,
    pub command: Vec<String>,
    pub shell: bool,
    #[serde(skip)]
    pub command_display: String,
    pub cwd: PathBuf,
    pub env_file: Vec<PathBuf>,
    pub restart: Restart,
    pub log_max_size: u64,
    pub log_keep: u64,
    #[serde(flatten)]
    pub launchd: LaunchdState,
    pub out_of_sync: Option<String>,
    pub env: Vec<EnvVar>,
    pub env_error: Option<String>,
    pub undefined_refs: Vec<UndefinedRef>,
}

pub const MASK: &str = "********";

impl App {
    pub fn show(&self, name: &ServiceName, reveal: bool) -> Result<Report> {
        let s = config::load_service(&self.paths, name)?;
        let global = config::load_global(&self.paths)?;
        let (st, launchd) = self.launchd_state(name, &self.ld.disabled()?)?;
        let (log_max_size, log_keep) = config::effective_log(&global, Some(&s));
        let (env, env_err) = environ::assemble(&environ::base(), &global, &s, &self.paths.home);
        Ok(Report {
            name: name.clone(),
            label: label(name),
            config: self.paths.service_file(name),
            plist: self.paths.plist_file(name),
            log: self.paths.log_file(name),
            command: s.argv(),
            shell: matches!(s.command, config::Command::Shell(_)),
            command_display: s.command_string(),
            cwd: s.resolved_cwd(&self.paths.home),
            env_file: s.resolved_env_files(&self.paths.home),
            restart: s.restart,
            log_max_size,
            log_keep,
            launchd,
            out_of_sync: self.sync_reason(&s, &st),
            env: env
                .vars
                .iter()
                .map(|(k, v)| EnvVar {
                    key: k.clone(),
                    value: if reveal { v.clone() } else { MASK.into() },
                    source: env.sources[k].clone(),
                })
                .collect(),
            env_error: env_err.map(|e| e.to_string()),
            undefined_refs: env
                .undefined
                .into_iter()
                .map(|u| UndefinedRef {
                    file: u.file,
                    line: u.line,
                    name: u.name,
                })
                .collect(),
        })
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

/// The launchd job an action is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    Service(ServiceName),
    /// hum's log rotation job.
    RotateJob,
}

impl std::fmt::Display for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Subject::Service(name) => write!(f, "{name}"),
            Subject::RotateJob => f.write_str(ROTATE_NAME),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub kind: ActionKind,
    pub subject: Subject,
    /// Why the action is needed.
    pub reason: String,
}

impl App {
    /// What `apply` would do, and the configs it has to skip.
    pub fn plan(&self) -> Result<(Vec<Action>, Vec<anyhow::Error>)> {
        let mut actions = vec![];
        let mut problems = vec![];
        let mut configured = vec![];
        for file in self.paths.service_files()? {
            let name = match file {
                Ok(name) => name,
                Err(raw) => {
                    problems.push(anyhow::anyhow!("invalid service name {raw:?}"));
                    continue;
                }
            };
            configured.push(name.clone());
            let s = match config::load_service(&self.paths, &name) {
                Ok(s) => s,
                Err(e) => {
                    problems.push(e);
                    continue;
                }
            };
            let st = self.ld.status(&label(&name))?;
            let Some(reason) = self.plist_reason(&s, &st) else {
                continue;
            };
            let kind = if self.paths.plist_file(&name).exists() {
                ActionKind::Update
            } else {
                ActionKind::Register
            };
            actions.push(Action {
                kind,
                subject: Subject::Service(name),
                reason,
            });
        }
        for name in self.paths.plist_names()? {
            if !configured.contains(&name) {
                actions.push(Action {
                    kind: ActionKind::Unregister,
                    subject: Subject::Service(name),
                    reason: "plist without config".into(),
                });
            }
        }
        actions.extend(self.rotate_action()?);
        Ok((actions, problems))
    }

    /// Carries out one planned action.
    pub fn apply_action(&self, act: &Action) -> Result<()> {
        let name = match &act.subject {
            Subject::Service(name) => name,
            Subject::RotateJob => {
                return match act.kind {
                    ActionKind::Register | ActionKind::Update => self.ensure_rotate_job(),
                    ActionKind::Unregister => self.remove_rotate_job(),
                };
            }
        };
        let label = label(name);
        match act.kind {
            ActionKind::Register | ActionKind::Update => {
                let s = config::load_service(&self.paths, name)?;
                let st = self.install(name, &self.render_plist(&s))?;
                // A service `hum stop` disabled keeps only its plist, so
                // it stays stopped until `hum start`.
                if act.kind == ActionKind::Register
                    && !st.loaded
                    && !self.ld.disabled()?.contains(&label)
                {
                    self.ld.bootstrap(&self.paths.plist_file(name))?;
                }
                Ok(())
            }
            ActionKind::Unregister => {
                self.ld.bootout(&label)?;
                self.clear_disabled(&label)?;
                remove_if_exists(&self.paths.plist_file(name))
            }
        }
    }
}

// --- doctor ---

#[derive(Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Ok,
    Warn,
    Error,
}

#[derive(Serialize, Debug)]
pub struct Finding {
    pub severity: Severity,
    pub subject: String,
    pub message: String,
}

/// Logs this many times larger than max_size mean rotation is not running.
const OVERSIZE_FACTOR: u64 = 2;

/// Checks the hum path a plist runs.
fn check_program(plist_path: &Path) -> Vec<(Severity, String)> {
    let Ok(data) = fs::read(plist_path) else {
        return vec![];
    };
    let Some(prog) = plist::program(&data) else {
        return vec![(
            Severity::Error,
            format!("cannot read ProgramArguments from {}", plist_path.display()),
        )];
    };
    if environ::look_path(&prog, "").is_none() {
        return vec![(
            Severity::Error,
            format!("plist runs {prog}, which is missing or not executable; run `hum apply`"),
        )];
    }
    let Some(hum) = plist::env_var(&data, EXEC_VAR) else {
        return vec![];
    };
    if environ::look_path(&hum, "").is_none() {
        vec![(
            Severity::Warn,
            format!(
                "plist hands over to {hum}, which is missing or not executable, so {prog} runs instead; run `hum apply`"
            ),
        )]
    } else if hum.contains("/Cellar/") {
        vec![(
            Severity::Warn,
            format!(
                "plist runs versioned path {hum}; it breaks on upgrade (put hum on PATH and run `hum apply`)"
            ),
        )]
    } else {
        vec![]
    }
}

/// Checks the PATH a service runs with.
fn check_path(path: &str) -> Vec<(Severity, String)> {
    if path.is_empty() {
        return vec![(
            Severity::Warn,
            "no PATH in its environment; launchd gives services none".into(),
        )];
    }
    path.split(':')
        .filter(|e| config::is_per_shell_path(e))
        .map(|e| {
            (
                Severity::Warn,
                format!(
                    "PATH entry {e} belongs to a single shell session; replace it with the directory it resolves to"
                ),
            )
        })
        .collect()
}

impl App {
    pub fn doctor(&self) -> Result<Vec<Finding>> {
        let disabled = self.ld.disabled()?;
        let mut out = vec![];
        let mut add = |subject: &str, (severity, message): (Severity, String)| {
            out.push(Finding {
                severity,
                subject: subject.into(),
                message,
            });
        };
        let global = match config::load_global(&self.paths) {
            Ok(g) => {
                add("global", (Severity::Ok, "config ok".into()));
                g
            }
            Err(e) => {
                add("global", (Severity::Error, format!("{e:#}")));
                Default::default()
            }
        };

        let mut configured = vec![];
        for file in self.paths.service_files()? {
            let name = match file {
                Ok(name) => name,
                Err(raw) => {
                    add(
                        &raw,
                        (Severity::Error, format!("invalid service name {raw:?}")),
                    );
                    continue;
                }
            };
            configured.push(name.clone());
            let s = match config::load_service(&self.paths, &name) {
                Ok(s) => s,
                Err(e) => {
                    add(&name, (Severity::Error, format!("{e:#}")));
                    continue;
                }
            };
            let (env, _) = environ::assemble(&environ::base(), &global, &s, &self.paths.home);
            let path = env.vars.get("PATH").map_or("", |p| p);
            let mut problems = check_path(path);
            if let config::Command::Argv(argv) = &s.command
                && environ::look_path(&argv[0], path).is_none()
            {
                problems.push((
                    Severity::Error,
                    format!("command {} not found in its PATH", argv[0]),
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
            if let Some(reason) = self.sync_reason(&s, &self.ld.status(&label(&name))?) {
                let fix = sync_fix(&name, disabled.contains(&label(&name)));
                problems.push((
                    Severity::Warn,
                    format!("out of sync with its config ({reason}); {fix}"),
                ));
            }
            problems.extend(check_program(&self.paths.plist_file(&name)));
            let (max_size, _) = config::effective_log(&global, Some(&s));
            if let Ok(m) = fs::metadata(self.paths.log_file(&name))
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
                add(&name, p);
            }
        }
        for name in self.paths.plist_names()? {
            if !configured.contains(&name) {
                add(
                    &name,
                    (
                        Severity::Warn,
                        "plist without config; run `hum apply` to unregister".into(),
                    ),
                );
            }
        }
        if !configured.is_empty() {
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
            for f in check_program(&rp) {
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
    /// Problems that do not stop the service from starting.
    pub warnings: Vec<String>,
}

impl App {
    /// Loads the service, rotates its log if oversized and assembles the
    /// environment from scratch.
    pub fn prepare_run(&self, name: &ServiceName, base: &BTreeMap<String, String>) -> Result<Exec> {
        let s = config::load_service(&self.paths, name)?;
        let global = config::load_global(&self.paths)?;
        let (max_size, keep) = config::effective_log(&global, Some(&s));
        let now = chrono::Local::now();
        let rotated = logfile::rotate_if_needed(&self.paths.log_file(name), max_size, keep, now);
        let mut ex = self
            .resolve_run(&s, &global, base)
            .with_context(|| format!("{name}: not starting"))?;
        if let Err(e) = rotated {
            ex.warnings.insert(0, format!("log rotation: {e}"));
        }
        Ok(ex)
    }

    /// Why `hum run` would refuse to start the service right now, if it
    /// would. Checks what `prepare_run` checks, without rotating the log.
    pub fn run_blocker(&self, name: &ServiceName) -> Option<String> {
        let s = config::load_service(&self.paths, name).ok()?;
        let global = config::load_global(&self.paths).ok()?;
        self.resolve_run(&s, &global, &environ::base())
            .err()
            .map(|e| format!("{e:#}"))
    }

    fn resolve_run(
        &self,
        s: &Service,
        global: &config::Global,
        base: &BTreeMap<String, String>,
    ) -> Result<Exec> {
        let dir = s.resolved_cwd(&self.paths.home);
        if !dir.is_dir() {
            bail!("cwd {} does not exist", dir.display());
        }
        let (env, err) = environ::assemble(base, global, s, &self.paths.home);
        if let Some(e) = err {
            return Err(e.into());
        }
        let warnings = env
            .undefined
            .iter()
            .map(|u| {
                format!(
                    "{}:{}: ${{{}}} is undefined, expanded to \"\"",
                    u.file, u.line, u.name
                )
            })
            .collect();
        let argv = s.argv();
        let path = env.vars.get("PATH").map(String::as_str).unwrap_or("");
        let program = environ::look_path(&argv[0], path)
            .with_context(|| format!("{} not found in PATH", argv[0]))?;
        Ok(Exec {
            program,
            argv,
            env: env.vars,
            dir,
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::environ::EnvFileError;
    use std::cell::RefCell;

    /// Models the parts of launchd hum relies on, including that a loaded
    /// job keeps the definition it was loaded with.
    #[derive(Default)]
    struct FakeLd {
        jobs: RefCell<BTreeMap<String, Status>>,
        /// The plist bytes each loaded job was loaded from.
        loaded_defs: RefCell<BTreeMap<String, Vec<u8>>>,
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
            let data = fs::read(plist)?;
            let run_at_load =
                String::from_utf8_lossy(&data).contains("<key>RunAtLoad</key>\n\t<true/>");
            let st = Status {
                loaded: true,
                running: run_at_load,
                pid: run_at_load.then_some(100),
                path: Some(plist.display().to_string()),
                started_at: run_at_load.then(std::time::SystemTime::now),
                ..Default::default()
            };
            self.jobs.borrow_mut().insert(label.clone(), st);
            self.loaded_defs.borrow_mut().insert(label, data);
            Ok(())
        }
        fn bootout(&self, label: &str) -> Result<()> {
            if self.jobs.borrow_mut().remove(label).is_some() {
                self.log(format!("bootout {label}"));
            }
            self.loaded_defs.borrow_mut().remove(label);
            Ok(())
        }
        fn enable(&self, label: &str) -> Result<()> {
            self.log(format!("enable {label}"));
            self.disabled.borrow_mut().remove(label);
            Ok(())
        }
        fn disable(&self, label: &str) -> Result<()> {
            self.log(format!("disable {label}"));
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
                st.started_at = Some(std::time::SystemTime::now());
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
        let paths = Paths::with_home(dir.path().into());
        config::write_atomic(&paths.global_file(), b"[env]\nPATH = \"/usr/bin:/bin\"\n").unwrap();
        let ld: &'static FakeLd = Box::leak(Box::default());
        let hum_binary = dir.path().join("hum-binary");
        fs::write(&hum_binary, b"hum 1").unwrap();
        Fixture {
            app: App {
                paths,
                ld: Box::new(ld),
                hum_path: "/opt/homebrew/bin/hum".into(),
                hum_binary,
            },
            ld,
            _dir: dir,
        }
    }

    fn n(name: &str) -> ServiceName {
        ServiceName::parse(name).unwrap()
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
        /// Whether launchd holds the definition currently on disk.
        fn loaded_def_current(&self, name: &str) -> bool {
            let on_disk = fs::read(self.app.paths.plist_file(name)).unwrap();
            self.ld.loaded_defs.borrow().get(&label(name)) == Some(&on_disk)
        }
        fn row(&self, name: &str) -> ServiceState {
            let rows = self.app.states().unwrap();
            rows.into_iter().find(|r| r.name == name).unwrap()
        }
    }

    // Requirement: start, stop, restart, rm
    #[test]
    fn start_stop_restart_remove() {
        let f = fixture();
        let a = &f.app;
        f.write("t3", r#"command = ["sleep", "1000"]"#);
        assert!(matches!(a.start(&n("t3")).unwrap(), StartOutcome::Started));
        assert!(f.status("t3").running);
        assert!(a.paths.plist_file("t3").exists());
        assert!(f.status(ROTATE_NAME).loaded);

        f.take_calls();
        let again = a.start(&n("t3")).unwrap();
        assert!(matches!(again, StartOutcome::AlreadyRunning { .. }));
        assert!(
            f.take_calls().is_empty(),
            "start on a running service is a no-op"
        );

        a.stop(&n("t3")).unwrap();
        assert_eq!(f.row("t3").launchd.unwrap().state, State::Disabled);

        a.restart(&n("t3")).unwrap();
        assert!(f.status("t3").running);
        assert!(!f.ld.disabled.borrow().contains("hum.t3"));

        a.remove(&n("t3"), false).unwrap();
        for p in [
            a.paths.plist_file("t3"),
            a.paths.service_file("t3"),
            a.paths.plist_file(ROTATE_NAME),
        ] {
            assert!(!p.exists(), "{} still exists", p.display());
        }
        assert!(!f.status("t3").loaded && !f.status(ROTATE_NAME).loaded);
    }

    // Requirement: rm
    #[test]
    fn rm_deletes_logs_unless_the_config_is_kept() {
        let f = fixture();
        let a = &f.app;
        let logs = &a.paths.log_dir;
        f.write("t3", r#"command = ["sleep", "1"]"#);
        f.write("t3_", r#"command = ["sleep", "1"]"#);
        f.write("kept", r#"command = ["sleep", "1"]"#);
        for name in ["t3", "t3_", "kept"] {
            a.start(&n(name)).unwrap();
        }
        let files = [
            "t3.log",
            "t3__2026-10-21_10-00-00.log",
            "t3__2026-10-21_10-00-00_1.log",
            "t3_.log",
            "t3___2026-10-21_10-00-00.log",
            "kept.log",
            "kept__2026-10-21_10-00-00.log",
        ];
        for file in files {
            fs::write(logs.join(file), "x\n").unwrap();
        }
        fs::write(logs.join("_rotate.log"), "").unwrap();
        a.remove(&n("t3"), false).unwrap();
        a.remove(&n("kept"), true).unwrap();
        let mut left: Vec<_> = fs::read_dir(logs)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "_rotate.log",
                "kept.log",
                "kept__2026-10-21_10-00-00.log",
                "t3_.log",
                "t3___2026-10-21_10-00-00.log",
            ]
        );
        a.remove(&n("t3_"), false).unwrap();
        assert!(logs.join("_rotate.log").exists(), "kept's config remains");
        a.remove(&n("kept"), false).unwrap();
        assert!(
            !logs.join("_rotate.log").exists(),
            "goes with the rotate job"
        );
        assert!(!logs.join("kept__2026-10-21_10-00-00.log").exists());
    }

    // Requirement: start
    #[test]
    fn start_reloads_an_outdated_loaded_job() {
        let f = fixture();
        f.write("x", r#"command = ["sleep", "1"]"#);
        f.app.start(&n("x")).unwrap();
        f.ld.jobs.borrow_mut().get_mut("hum.x").unwrap().running = false;
        f.write("x", "command = [\"sleep\", \"1\"]\nrestart = \"never\"\n");
        f.take_calls();
        f.app.start(&n("x")).unwrap();
        assert_eq!(f.take_calls()[..2], ["bootout hum.x", "bootstrap hum.x"]);
        assert!(f.status("x").running);
        assert!(f.loaded_def_current("x"));
    }

    // Requirement: When a service config change takes effect
    #[test]
    fn config_change_on_a_loaded_stopped_job() {
        // A crashed job stays loaded. Its config change must not start it,
        // must stay visible as pending, and must reach launchd on start.
        let f = fixture();
        f.write("x", r#"command = ["sleep", "1"]"#);
        f.app.start(&n("x")).unwrap();
        f.ld.jobs.borrow_mut().get_mut("hum.x").unwrap().running = false;
        f.write("x", "command = [\"sleep\", \"1\"]\nrestart = \"never\"\n");
        f.take_calls();
        let outcome = f.app.apply_config_change(&n("x"), true).unwrap();
        assert!(matches!(outcome, ChangeOutcome::NeedsRestart { .. }));
        assert!(f.take_calls().is_empty() && !f.status("x").running);
        assert!(f.row("x").out_of_sync(), "ls flags the pending change");
        f.app.start(&n("x")).unwrap();
        assert_eq!(f.take_calls()[..2], ["bootout hum.x", "bootstrap hum.x"]);
        assert!(f.status("x").running);
        assert!(f.loaded_def_current("x"));
        assert!(!f.row("x").out_of_sync());
    }

    // Requirement: When a service config change takes effect
    #[test]
    fn config_change_on_an_unloaded_job_rewrites_its_plist() {
        let f = fixture();
        f.write("x", r#"command = ["sleep", "1"]"#);
        f.app.start(&n("x")).unwrap();
        f.app.stop(&n("x")).unwrap();
        f.write("x", "command = [\"sleep\", \"1\"]\nrestart = \"never\"\n");
        let outcome = f.app.apply_config_change(&n("x"), true).unwrap();
        assert!(matches!(outcome, ChangeOutcome::Applied));
        assert!(!f.status("x").loaded);
        assert!(
            !f.row("x").out_of_sync(),
            "the next login loads the new plist"
        );
    }

    // Requirement: When a service config change takes effect, Flag mismatches with launchd
    #[test]
    fn config_change_on_a_running_job() {
        let f = fixture();
        f.write("x", r#"command = ["sleep", "1"]"#);
        f.app.start(&n("x")).unwrap();
        f.write(
            "x",
            "command = [\"sleep\", \"1\"]\nrestart = \"on-failure\"\n",
        );
        f.take_calls();
        let outcome = f.app.apply_config_change(&n("x"), false).unwrap();
        assert!(matches!(outcome, ChangeOutcome::NeedsRestart { .. }));
        assert!(
            f.take_calls().is_empty(),
            "--no-restart leaves launchd alone"
        );
        assert!(
            f.row("x").out_of_sync(),
            "ls keeps flagging the pending change"
        );
        let outcome = f.app.apply_config_change(&n("x"), true).unwrap();
        assert!(matches!(outcome, ChangeOutcome::Restarted));
        assert!(f.loaded_def_current("x") && f.status("x").running);
        assert!(!f.row("x").out_of_sync());
    }

    // Requirement: When a service config change takes effect, Flag mismatches with launchd
    #[test]
    fn env_only_change_on_a_running_service_is_flagged_until_restart() {
        // The plist does not hold the environment, so only the process's
        // start time can show that it runs with the old config.
        let f = fixture();
        f.write("x", r#"command = ["sleep", "1"]"#);
        f.app.start(&n("x")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        f.write("x", "command = [\"sleep\", \"1\"]\n[env]\nPORT = \"1\"\n");
        f.take_calls();
        let outcome = f.app.apply_config_change(&n("x"), false).unwrap();
        assert!(matches!(
            outcome,
            ChangeOutcome::NeedsRestart { running: true }
        ));
        assert!(f.take_calls().is_empty());
        let reason = match f.row("x").config {
            ConfigState::Ok { out_of_sync, .. } => out_of_sync.unwrap(),
            other => panic!("{other:?}"),
        };
        assert!(
            reason.starts_with("started before") && reason.contains("x.toml"),
            "{reason}"
        );
        assert!(
            f.app.plan().unwrap().0.is_empty(),
            "apply has nothing to do for it"
        );
        f.app.restart(&n("x")).unwrap();
        assert!(!f.row("x").out_of_sync());
    }

    // Requirement: Flag mismatches with launchd
    #[test]
    fn pointing_an_env_file_link_at_an_older_file_is_flagged() {
        let f = fixture();
        let home = &f.app.paths.home;
        fs::write(home.join("a.env"), "X=a\n").unwrap();
        fs::write(home.join("old.env"), "X=old\n").unwrap();
        fs::File::options()
            .write(true)
            .open(home.join("old.env"))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000))
            .unwrap();
        std::os::unix::fs::symlink(home.join("a.env"), home.join(".env")).unwrap();
        f.write("x", "command = [\"sleep\", \"1\"]\nenv_file = [\".env\"]\n");
        f.app.start(&n("x")).unwrap();
        assert!(!f.row("x").out_of_sync());
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::remove_file(home.join(".env")).unwrap();
        std::os::unix::fs::symlink("old.env", home.join(".env")).unwrap();
        let reason = match f.row("x").config {
            ConfigState::Ok { out_of_sync, .. } => out_of_sync.unwrap(),
            other => panic!("{other:?}"),
        };
        assert!(
            reason.starts_with("started before") && reason.contains(".env"),
            "{reason}"
        );
    }

    // Requirement: When a service config change takes effect
    #[test]
    fn env_only_change_on_a_loaded_stopped_job_needs_nothing() {
        let f = fixture();
        f.write("x", "command = [\"sleep\", \"1\"]\nrestart = \"never\"\n");
        f.app.start(&n("x")).unwrap();
        f.ld.jobs.borrow_mut().get_mut("hum.x").unwrap().running = false;
        f.write(
            "x",
            "command = [\"sleep\", \"1\"]\nrestart = \"never\"\n[env]\nX = \"1\"\n",
        );
        f.take_calls();
        let outcome = f.app.apply_config_change(&n("x"), true).unwrap();
        assert!(matches!(outcome, ChangeOutcome::Applied));
        assert!(f.take_calls().is_empty() && !f.status("x").running);
        assert!(
            !f.row("x").out_of_sync(),
            "the next start reads the new config"
        );
    }

    // Requirement: Flag mismatches with launchd, apply aligns launchd, apply skips invalid configs
    #[test]
    fn states_and_apply() {
        let f = fixture();
        let a = &f.app;
        f.write("new1", r#"command = ["sleep", "1"]"#);
        f.write("changed", r#"command = ["sleep", "1"]"#);
        f.write("broken", "command = 3");
        f.write("Bad", r#"command = ["x"]"#);
        a.start(&n("changed")).unwrap();
        f.write(
            "changed",
            "command = [\"sleep\", \"2\"]\nrestart = \"on-failure\"\n",
        );
        fs::write(a.paths.plist_file("orphan"), "<plist/>").unwrap();
        let orphan = Status {
            loaded: true,
            running: true,
            pid: Some(9),
            ..Default::default()
        };
        f.ld.jobs.borrow_mut().insert("hum.orphan".into(), orphan);

        let reason = |name: &str| match f.row(name).config {
            ConfigState::Ok { out_of_sync, .. } => out_of_sync,
            other => panic!("{name}: {other:?}"),
        };
        assert_eq!(reason("new1").as_deref(), Some("plist missing"));
        assert_eq!(reason("changed").as_deref(), Some("plist outdated"));
        assert_eq!(f.row("changed").launchd.unwrap().state, State::Running);
        assert!(matches!(
            f.row("broken").config,
            ConfigState::Invalid { .. }
        ));
        assert!(matches!(f.row("Bad").config, ConfigState::Invalid { .. }));
        assert!(matches!(f.row("orphan").config, ConfigState::Missing));

        let (actions, problems) = a.plan().unwrap();
        assert_eq!(problems.len(), 2);
        let summary: Vec<_> = actions
            .iter()
            .map(|x| format!("{} {}", x.kind.as_str(), x.subject))
            .collect();
        assert_eq!(
            summary,
            ["update changed", "register new1", "unregister orphan"]
        );
        for act in &actions {
            a.apply_action(act).unwrap();
        }
        assert!(
            f.status("changed").running,
            "running service keeps running after reload"
        );
        assert!(f.loaded_def_current("changed"));
        assert!(f.status("new1").running, "registered and started");
        assert!(!f.status("orphan").loaded);
        assert!(!a.paths.plist_file("orphan").exists());
        assert!(a.plan().unwrap().0.is_empty());
    }

    // Requirement: launchd records stay bounded, rm
    #[test]
    fn only_a_disabled_job_is_enabled() {
        let f = fixture();
        let a = &f.app;
        let enables = || -> Vec<String> {
            f.take_calls()
                .into_iter()
                .filter(|c| c.starts_with("enable"))
                .collect()
        };
        f.write("x", r#"command = ["sleep", "1"]"#);
        a.start(&n("x")).unwrap();
        a.restart(&n("x")).unwrap();
        assert!(enables().is_empty(), "never stopped, never enabled");
        a.stop(&n("x")).unwrap();
        a.start(&n("x")).unwrap();
        assert_eq!(enables(), ["enable hum.x"]);
        a.stop(&n("x")).unwrap();
        a.remove(&n("x"), false).unwrap();
        assert_eq!(enables(), ["enable hum.x"], "rm clears the stop");
        assert!(!f.ld.disabled.borrow().contains("hum.x"));
    }

    // Requirement: apply aligns launchd, apply dry run
    #[test]
    fn apply_plans_the_rotate_job() {
        let f = fixture();
        let a = &f.app;
        let rotate = |app: &App| -> Vec<String> {
            app.plan()
                .unwrap()
                .0
                .iter()
                .filter(|x| x.subject == Subject::RotateJob)
                .map(|x| format!("{} {}", x.kind.as_str(), x.reason))
                .collect()
        };
        assert!(rotate(a).is_empty(), "no services, no rotate job");

        f.write("x", r#"command = ["sleep", "1"]"#);
        a.start(&n("x")).unwrap();
        assert!(rotate(a).is_empty());

        let dev = App {
            paths: a.paths.clone(),
            ld: Box::new(f.ld),
            hum_path: "/src/hum/target/release/hum".into(),
            hum_binary: PathBuf::new(),
        };
        assert!(
            rotate(&dev).contains(
                &"update plist runs /opt/homebrew/bin/hum, this hum is /src/hum/target/release/hum"
                    .to_string()
            ),
            "{:?}",
            rotate(&dev)
        );

        f.ld.bootout(&label(ROTATE_NAME)).unwrap();
        assert_eq!(rotate(a), ["register log rotation job not loaded"]);
        fs::remove_file(a.paths.plist_file(ROTATE_NAME)).unwrap();
        let (actions, _) = a.plan().unwrap();
        assert_eq!(rotate(a), ["register log rotation job not installed"]);
        for act in &actions {
            a.apply_action(act).unwrap();
        }
        assert!(f.status(ROTATE_NAME).loaded);
        assert!(rotate(a).is_empty());

        fs::remove_file(a.paths.service_file(&n("x"))).unwrap();
        let (actions, _) = a.plan().unwrap();
        let summary: Vec<_> = actions
            .iter()
            .map(|x| format!("{} {}", x.kind.as_str(), x.subject))
            .collect();
        assert_eq!(summary, ["unregister x", "unregister _rotate"]);
        for act in &actions {
            a.apply_action(act).unwrap();
        }
        assert!(!f.status(ROTATE_NAME).loaded);
        assert!(!a.paths.plist_file(ROTATE_NAME).exists());
        assert!(a.plan().unwrap().0.is_empty());
    }

    // Requirement: Launcher copy, rm
    #[test]
    fn jobs_run_the_launcher_copy() {
        let f = fixture();
        let a = &f.app;
        f.write("x", r#"command = ["sleep", "1"]"#);
        a.start(&n("x")).unwrap();
        let copy = a.paths.home.join("Library/Application Support/hum/hum");
        assert_eq!(fs::read(&copy).unwrap(), b"hum 1");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&copy).unwrap().permissions().mode() & 0o777,
            0o755
        );
        for name in ["x", ROTATE_NAME] {
            let plist = fs::read(a.paths.plist_file(name)).unwrap();
            assert_eq!(plist::program(&plist).unwrap(), copy.display().to_string());
            assert_eq!(
                plist::env_var(&plist, EXEC_VAR).unwrap(),
                "/opt/homebrew/bin/hum"
            );
        }

        fs::write(&a.hum_binary, b"hum 2").unwrap();
        a.restart(&n("x")).unwrap();
        assert_eq!(fs::read(&copy).unwrap(), b"hum 2", "replaced in place");

        a.remove(&n("x"), false).unwrap();
        assert!(!copy.exists(), "removed with the last job");
    }

    // Requirement: Launcher copy
    #[test]
    fn the_copy_hands_over_to_another_runnable_hum() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (copy, hum) = (dir.path().join("copy"), dir.path().join("hum"));
        for p in [&copy, &hum] {
            fs::write(p, b"").unwrap();
            fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let copy = fs::canonicalize(&copy).unwrap();
        let var = |p: &Path| Some(p.as_os_str().to_owned());
        assert_eq!(
            handover_target(var(&hum).as_deref(), &copy),
            Some(hum.clone())
        );
        assert_eq!(handover_target(None, &copy), None, "no HUM_EXEC");
        assert_eq!(
            handover_target(var(&copy).as_deref(), &copy),
            None,
            "itself"
        );
        let gone = dir.path().join("gone");
        assert_eq!(
            handover_target(var(&gone).as_deref(), &copy),
            None,
            "missing"
        );
    }

    // Requirement: Flag mismatches with launchd
    #[test]
    fn a_different_hum_binary_is_named_as_the_reason() {
        let f = fixture();
        f.write("x", r#"command = ["sleep", "1"]"#);
        f.app.start(&n("x")).unwrap();
        let dev = App {
            paths: f.app.paths.clone(),
            ld: Box::new(f.ld),
            hum_path: "/src/hum/target/release/hum".into(),
            hum_binary: PathBuf::new(),
        };
        let s = config::load_service(&dev.paths, &n("x")).unwrap();
        let reason = dev.sync_reason(&s, &f.status("x")).unwrap();
        assert_eq!(
            reason,
            "plist runs /opt/homebrew/bin/hum, this hum is /src/hum/target/release/hum"
        );
        assert_eq!(dev.plan().unwrap().0[0].reason, reason);
    }

    // Requirement: show output, Environment values masked by default
    #[test]
    fn show_masks_values_unless_revealed() {
        let f = fixture();
        f.write(
            "x",
            "command = [\"sleep\", \"1\"]\n[env]\nTOKEN = \"secret\"\n",
        );
        let token = |r: &Report| {
            r.env
                .iter()
                .find(|v| v.key == "TOKEN")
                .unwrap()
                .value
                .clone()
        };
        let masked = f.app.show(&n("x"), false).unwrap();
        assert_eq!(token(&masked), MASK);
        assert!(!serde_json::to_string(&masked).unwrap().contains("secret"));
        assert_eq!(token(&f.app.show(&n("x"), true).unwrap()), "secret");
    }

    // Requirement: Environment built from scratch, Refuse to start without an env file
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
        let ex = f.app.prepare_run(&n("t3"), &base).unwrap();
        assert!(ex.program.ends_with("bin/sh"), "{}", ex.program.display());
        assert_eq!(ex.dir, cwd);
        assert_eq!(ex.env["PATH"], "/usr/bin:/bin");
        assert_eq!(ex.env["PORT"], "3773");
        assert_eq!(ex.env["FROM_FILE"], format!("{}/x", home.display()));
        assert_eq!(ex.env["USER"], "me");
        assert!(ex.warnings.is_empty());

        fs::remove_file(cwd.join(".env")).unwrap();
        let err = f.app.prepare_run(&n("t3"), &base).err().unwrap();
        assert!(err.downcast_ref::<EnvFileError>().is_some(), "{err:#}");
    }

    // Requirement: Refuse to start without its cwd, Report a service that cannot start
    #[test]
    fn run_blockers_are_found_before_start() {
        let f = fixture();
        f.write("x", "command = [\"sleep\", \"1\"]\ncwd = \"~/nope\"\n");
        let err = f.app.prepare_run(&n("x"), &BTreeMap::new()).err().unwrap();
        assert!(format!("{err:#}").contains("not starting: cwd"), "{err:#}");
        let why = f.app.run_blocker(&n("x")).unwrap();
        assert!(
            why.starts_with("cwd ") && why.ends_with("nope does not exist"),
            "{why}"
        );
        f.write("x", "command = [\"sleep\", \"1\"]\nenv_file = [\".env\"]\n");
        assert!(f.app.run_blocker(&n("x")).unwrap().contains(".env"));
        f.write("x", "command = [\"no-such-command\"]\n");
        assert!(
            f.app
                .run_blocker(&n("x"))
                .unwrap()
                .contains("not found in PATH")
        );
        f.write("x", "command = [\"sleep\", \"1\"]\n");
        assert_eq!(f.app.run_blocker(&n("x")), None);
    }

    // Requirement: apply aligns launchd, Suggested fixes keep stopped services stopped
    #[test]
    fn a_stopped_service_stays_stopped_through_doctor_and_apply() {
        let f = fixture();
        f.write("x", r#"command = ["sleep", "1"]"#);
        f.app.start(&n("x")).unwrap();
        f.app.stop(&n("x")).unwrap();
        fs::remove_file(f.app.paths.plist_file("x")).unwrap();
        let findings = f.app.doctor().unwrap();
        let x = findings.iter().find(|d| d.subject == "x").unwrap();
        assert!(
            x.message
                .contains("run `hum apply` to update it; it stays stopped"),
            "{}",
            x.message
        );
        let (actions, _) = f.app.plan().unwrap();
        assert_eq!(actions[0].kind, ActionKind::Register);
        f.app.apply_action(&actions[0]).unwrap();
        assert!(f.app.paths.plist_file("x").exists());
        assert!(!f.status("x").loaded);
        assert!(f.ld.disabled.borrow().contains("hum.x"));
    }

    // Requirement: Flag mismatches with launchd
    #[test]
    fn pointing_the_cwd_link_elsewhere_is_flagged() {
        let f = fixture();
        let home = &f.app.paths.home;
        fs::create_dir_all(home.join("a")).unwrap();
        fs::create_dir_all(home.join("b")).unwrap();
        std::os::unix::fs::symlink("a", home.join("current")).unwrap();
        f.write("x", "command = [\"sleep\", \"1\"]\ncwd = \"~/current\"\n");
        f.app.start(&n("x")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(home.join("a/data"), "x").unwrap();
        assert!(!f.row("x").out_of_sync(), "files in cwd are not config");
        fs::remove_file(home.join("current")).unwrap();
        std::os::unix::fs::symlink("b", home.join("current")).unwrap();
        let reason = match f.row("x").config {
            ConfigState::Ok { out_of_sync, .. } => out_of_sync.unwrap(),
            other => panic!("{other:?}"),
        };
        assert_eq!(reason, "started before cwd ~/current changed");
    }

    // Requirement: Check the log size before starting
    #[test]
    fn prepare_run_rotates_an_oversized_log() {
        let f = fixture();
        f.write(
            "x",
            "command = [\"sh\"]\n[log]\nmax_size = \"10\"\nkeep = 2\n",
        );
        fs::create_dir_all(&f.app.paths.log_dir).unwrap();
        fs::write(f.app.paths.log_file("x"), "more than ten bytes").unwrap();
        f.app.prepare_run(&n("x"), &BTreeMap::new()).unwrap();
        let files = logfile::rotated(&f.app.paths.log_file("x")).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(
            fs::read_to_string(&files[0]).unwrap(),
            "more than ten bytes"
        );
    }

    // Requirement: doctor checks
    #[test]
    fn doctor_reports_problems() {
        let f = fixture();
        f.write(
            "bad",
            "command = [\"definitely-not-a-command\"]\ncwd = \"/nonexistent\"\nenv_file = [\".env\"]\n",
        );
        let findings = f.app.doctor().unwrap();
        let msgs: Vec<&str> = findings
            .iter()
            .filter(|x| x.subject == "bad")
            .map(|x| x.message.as_str())
            .collect();
        let joined = msgs.join("\n");
        for want in [
            "not found in its PATH",
            "cwd /nonexistent does not exist",
            "env_file /nonexistent/.env",
            "plist missing",
        ] {
            assert!(joined.contains(want), "missing {want:?} in:\n{joined}");
        }
    }

    // Requirement: doctor checks
    #[test]
    fn doctor_warns_about_service_path() {
        let f = fixture();
        f.write(
            "shell",
            "command = [\"true\"]\n[env]\nPATH = \"/state/fnm_multishells/81142_1/bin:/usr/bin\"\n",
        );
        f.write("bare", "command = [\"/usr/bin/true\"]\n");
        let warnings = |name: &str| -> Vec<String> {
            f.app
                .doctor()
                .unwrap()
                .into_iter()
                .filter(|x| x.subject == name && x.severity == Severity::Warn)
                .map(|x| x.message)
                .collect()
        };
        let shell = warnings("shell");
        assert!(
            shell.iter().any(
                |m| m.contains("fnm_multishells/81142_1/bin belongs to a single shell session")
            ),
            "{shell:?}"
        );
        assert!(!warnings("bare").iter().any(|m| m.contains("PATH")));

        config::write_atomic(&f.app.paths.global_file(), b"").unwrap();
        let bare = warnings("bare");
        assert!(
            bare.iter()
                .any(|m| m.contains("no PATH in its environment")),
            "{bare:?}"
        );
    }
}
