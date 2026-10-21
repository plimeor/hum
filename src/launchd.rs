//! Wraps launchctl behind a trait so the rest of hum can be tested without
//! macOS.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

/// What launchd reports about one job.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub loaded: bool,
    pub running: bool,
    pub pid: Option<u32>,
    /// Plist path launchd loaded the job from.
    pub path: Option<String>,
    /// None when the job never exited.
    pub last_exit: Option<i32>,
    /// e.g. "Killed: 9".
    pub last_signal: Option<String>,
}

impl Status {
    /// Loaded, not running, and last ended abnormally.
    pub fn crashed(&self) -> bool {
        self.loaded
            && !self.running
            && (self.last_exit.is_some_and(|c| c != 0) || self.last_signal.is_some())
    }
}

/// Controls launchd jobs in the user's GUI domain.
pub trait Manager {
    fn status(&self, label: &str) -> Result<Status>;
    /// Labels launchd has marked disabled.
    fn disabled(&self) -> Result<HashSet<String>>;
    fn bootstrap(&self, plist: &Path) -> Result<()>;
    /// Unloads the job and waits until launchd has let go of it. Unloading
    /// a job that is not loaded is not an error.
    fn bootout(&self, label: &str) -> Result<()>;
    fn enable(&self, label: &str) -> Result<()>;
    fn disable(&self, label: &str) -> Result<()>;
    /// Starts a loaded job if it is not running.
    fn kickstart(&self, label: &str) -> Result<()>;
}

/// The launchctl-backed manager, or one that always fails off macOS.
pub fn new() -> Box<dyn Manager> {
    if cfg!(target_os = "macos") {
        let uid = unsafe { libc::getuid() };
        Box::new(Launchctl {
            domain: format!("gui/{uid}"),
            bootout_timeout: Duration::from_secs(10),
        })
    } else {
        Box::new(Unsupported)
    }
}

struct Unsupported;

fn unsupported<T>() -> Result<T> {
    bail!("launchd is only available on macOS")
}

impl Manager for Unsupported {
    fn status(&self, _: &str) -> Result<Status> {
        unsupported()
    }
    fn disabled(&self) -> Result<HashSet<String>> {
        unsupported()
    }
    fn bootstrap(&self, _: &Path) -> Result<()> {
        unsupported()
    }
    fn bootout(&self, _: &str) -> Result<()> {
        unsupported()
    }
    fn enable(&self, _: &str) -> Result<()> {
        unsupported()
    }
    fn disable(&self, _: &str) -> Result<()> {
        unsupported()
    }
    fn kickstart(&self, _: &str) -> Result<()> {
        unsupported()
    }
}

pub struct Launchctl {
    domain: String,
    bootout_timeout: Duration,
}

/// `launchctl print` exits with this when the service does not exist.
const EXIT_NO_SUCH_SERVICE: i32 = 113;

impl Launchctl {
    /// Runs launchctl; returns stdout, or the exit code and message.
    fn run(&self, args: &[&str]) -> Result<std::result::Result<String, (i32, String)>> {
        let out = Command::new("launchctl").args(args).output()?;
        if out.status.success() {
            return Ok(Ok(String::from_utf8_lossy(&out.stdout).into_owned()));
        }
        let mut msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if msg.is_empty() {
            msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
        Ok(Err((out.status.code().unwrap_or(-1), msg)))
    }

    fn checked(&self, args: &[&str]) -> Result<String> {
        match self.run(args)? {
            Ok(out) => Ok(out),
            Err((code, msg)) => bail!("launchctl {}: {msg} (exit {code})", args.join(" ")),
        }
    }

    fn target(&self, label: &str) -> String {
        format!("{}/{label}", self.domain)
    }
}

impl Manager for Launchctl {
    fn status(&self, label: &str) -> Result<Status> {
        match self.run(&["print", &self.target(label)])? {
            Ok(out) => Ok(parse_print(&out)),
            Err((EXIT_NO_SUCH_SERVICE, _)) => Ok(Status::default()),
            Err((code, msg)) => bail!("launchctl print {label}: {msg} (exit {code})"),
        }
    }

    fn disabled(&self) -> Result<HashSet<String>> {
        Ok(parse_disabled(
            &self.checked(&["print-disabled", &self.domain])?,
        ))
    }

    fn bootstrap(&self, plist: &Path) -> Result<()> {
        self.checked(&["bootstrap", &self.domain, &plist.to_string_lossy()])
            .map(drop)
    }

    fn bootout(&self, label: &str) -> Result<()> {
        if !self.status(label)?.loaded {
            return Ok(());
        }
        if let Err(e) = self.checked(&["bootout", &self.target(label)]) {
            // It may have gone away between print and bootout.
            if self.status(label)?.loaded {
                return Err(e);
            }
        }
        // bootout returns before launchd has torn the job down; bootstrap
        // right after it can fail, so wait.
        let deadline = Instant::now() + self.bootout_timeout;
        while self.status(label)?.loaded {
            if Instant::now() > deadline {
                bail!(
                    "timed out after {:?} waiting for {label} to unload",
                    self.bootout_timeout
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    fn enable(&self, label: &str) -> Result<()> {
        self.checked(&["enable", &self.target(label)]).map(drop)
    }

    fn disable(&self, label: &str) -> Result<()> {
        self.checked(&["disable", &self.target(label)]).map(drop)
    }

    fn kickstart(&self, label: &str) -> Result<()> {
        self.checked(&["kickstart", &self.target(label)]).map(drop)
    }
}

/// Parses the top-level fields of `launchctl print` (lines indented by
/// exactly one tab).
pub fn parse_print(out: &str) -> Status {
    let mut st = Status {
        loaded: true,
        ..Default::default()
    };
    for line in out.lines() {
        let Some(rest) = line.strip_prefix('\t').filter(|r| !r.starts_with('\t')) else {
            continue;
        };
        let Some((key, val)) = rest.split_once(" = ") else {
            continue;
        };
        let val = val.trim();
        match key {
            "state" => st.running = val == "running",
            "pid" => st.pid = val.parse().ok(),
            "path" => st.path = Some(val.to_string()),
            // "1", "78: EX_CONFIG" or "(never exited)".
            "last exit code" => {
                st.last_exit = val.split(':').next().and_then(|n| n.trim().parse().ok())
            }
            "last terminating signal" => st.last_signal = Some(val.to_string()),
            _ => {}
        }
    }
    st.running |= st.pid.is_some();
    st
}

/// Parses `launchctl print-disabled`: newer macOS prints `=> disabled`,
/// older `=> true`.
pub fn parse_disabled(out: &str) -> HashSet<String> {
    out.lines()
        .filter_map(|line| {
            let (label, state) = line.trim().split_once(" => ")?;
            let label = label.strip_prefix('"')?.strip_suffix('"')?;
            matches!(state.trim(), "disabled" | "true").then(|| label.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUNNING: &str = "gui/501/hum.t3 = {\n\tactive count = 1\n\tpath = /Users/me/Library/LaunchAgents/hum.t3.plist\n\ttype = LaunchAgent\n\tstate = running\n\n\tprogram = /opt/homebrew/bin/hum\n\targuments = {\n\t\t/opt/homebrew/bin/hum\n\t\trun\n\t}\n\n\tpid = 4242\n\tlast exit code = (never exited)\n\tenvironment = {\n\t\tstate = nested-should-be-ignored\n\t}\n}\n";

    #[test]
    fn print_running() {
        let st = parse_print(RUNNING);
        assert!(st.loaded && st.running);
        assert_eq!(st.pid, Some(4242));
        assert_eq!(st.last_exit, None);
        assert_eq!(
            st.path.as_deref(),
            Some("/Users/me/Library/LaunchAgents/hum.t3.plist")
        );
    }

    // Requirement: 服务状态
    #[test]
    fn print_crashed() {
        let st = parse_print(
            "gui/501/hum.api = {\n\tstate = not running\n\truns = 3\n\tlast exit code = 78: EX_CONFIG\n}\n",
        );
        assert!(!st.running && st.crashed());
        assert_eq!(st.last_exit, Some(78));
        let st = parse_print(
            "gui/501/x = {\n\tstate = not running\n\tlast terminating signal = Killed: 9\n}\n",
        );
        assert!(st.crashed());
        assert_eq!(st.last_signal.as_deref(), Some("Killed: 9"));
    }

    #[test]
    fn disabled_both_formats() {
        let d = parse_disabled(
            "disabled services = {\n\t\"hum.t3\" => disabled\n\t\"hum.api\" => enabled\n\t\"com.old\" => true\n\t\"com.older\" => false\n}\n",
        );
        assert_eq!(
            d,
            HashSet::from(["hum.t3".to_string(), "com.old".to_string()])
        );
    }
}
