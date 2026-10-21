//! Runs the hum binary against a throwaway HOME. Only commands that do not
//! reach launchd are exercised here, so these run on any OS.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        let home = Home {
            dir: tempfile::tempdir().unwrap(),
        };
        fs::create_dir_all(home.path().join("work")).unwrap();
        home
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn service_file(&self, name: &str) -> PathBuf {
        self.path()
            .join(format!(".config/hum/services/{name}.toml"))
    }

    fn log_dir(&self) -> PathBuf {
        self.path().join("Library/Logs/hum")
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_hum"));
        c.args(args)
            .current_dir(self.path().join("work"))
            .env_clear()
            .env("HOME", self.path())
            .env("USER", "tester")
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().unwrap()
    }

    fn write_service(&self, name: &str, body: &str) {
        let file = self.service_file(name);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, body).unwrap();
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

// Requirement: New never overwrites, Warnings do not block writes, Created on first run
#[test]
fn new_warns_and_never_overwrites() {
    let h = Home::new();
    let o = h.run(&[
        "new",
        "demo",
        "--no-start",
        "--env-file",
        "missing.env",
        "--",
        "definitely-not-a-command",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    let err = stderr(&o);
    assert!(
        err.contains("definitely-not-a-command not found in global PATH"),
        "{err}"
    );
    assert!(err.contains("missing.env does not exist"), "{err}");
    let global = fs::read_to_string(h.path().join(".config/hum/config.toml")).unwrap();
    assert!(global.contains("PATH = \"/usr/bin:/bin\""), "{global}");

    let before = fs::read_to_string(h.service_file("demo")).unwrap();
    let o = h.run(&["new", "demo", "--no-start", "--", "true"]);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("already exists"));
    assert_eq!(fs::read_to_string(h.service_file("demo")).unwrap(), before);
}

// Requirement: Warnings do not block writes
#[test]
fn set_warns_about_a_missing_env_file_and_still_writes() {
    let h = Home::new();
    h.write_service("demo", "command = [\"true\"]\n");
    // After writing, `set` asks launchd about the service; that step is not
    // under test here (it fails off macOS), so the exit status is not checked.
    let o = h.run(&["set", "demo", "env_file=missing.env"]);
    assert!(
        stderr(&o).contains("missing.env does not exist"),
        "{}",
        stderr(&o)
    );
    let after = fs::read_to_string(h.service_file("demo")).unwrap();
    assert!(after.contains("env_file = [\"missing.env\"]"), "{after}");
}

// Requirement: Invalid results are not written, Exit status on failure
#[test]
fn invalid_set_leaves_the_file_alone() {
    let h = Home::new();
    h.write_service("demo", "command = [\"true\"]  # keep\n");
    let o = h.run(&["set", "demo", "restart=sometimes"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("left unchanged"), "{}", stderr(&o));
    let after = fs::read_to_string(h.service_file("demo")).unwrap();
    assert_eq!(after, "command = [\"true\"]  # keep\n");
}

// Requirement: Edit in an editor, Global config changes restart nothing
#[test]
fn edit_validates_and_global_edit_only_hints() {
    let h = Home::new();
    h.write_service("demo", "command = [\"true\"]\nrestart = \"never\"\n");
    // An invalid edit, then declining to edit again.
    let o = h
        .cmd(&["edit", "demo"])
        .env("EDITOR", "sed -i.bak s/never/bogus/")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("invalid config"), "{}", stderr(&o));
    let after = fs::read_to_string(h.service_file("demo")).unwrap();
    assert!(after.contains("\"never\""), "original kept: {after}");

    let o = h
        .cmd(&["edit", "--global"])
        .env("EDITOR", "sed -i.bak s/^#.keep.=.3/keep\\ =\\ 5/")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("takes effect when each service next restarts"));
    let global = fs::read_to_string(h.path().join(".config/hum/config.toml")).unwrap();
    assert!(global.contains("\nkeep = 5\n"), "{global}");
}

// Requirement: Log location, View logs, Rotate now, Multiple names and --all
#[test]
fn logs_view_and_rotate() {
    let h = Home::new();
    h.write_service("a", "command = [\"true\"]\n");
    h.write_service("b", "command = [\"true\"]\n");
    fs::create_dir_all(h.log_dir()).unwrap();
    fs::write(h.log_dir().join("a.log"), "1\n2\n3\n").unwrap();
    fs::write(h.log_dir().join("b.log"), "x\n").unwrap();

    assert_eq!(stdout(&h.run(&["logs", "a", "-n", "2"])), "2\n3\n");
    assert_eq!(
        stdout(&h.run(&["logs", "--all", "-n", "1"])),
        "[a] 3\n[b] x\n"
    );
    assert!(!h.run(&["logs", "a", "--all"]).status.success());

    assert_eq!(stdout(&h.run(&["logs", "a", "--rotate"])), "a: rotated\n");
    assert_eq!(fs::read_to_string(h.log_dir().join("a.log")).unwrap(), "");
    let copies: Vec<_> = fs::read_dir(h.log_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("a__"))
        .collect();
    assert_eq!(copies.len(), 1, "{copies:?}");
    let again = stdout(&h.run(&["logs", "a", "--rotate"]));
    assert!(again.contains("nothing to rotate"), "{again}");
}

// Requirement: Replace itself with the service, Environment built from scratch
#[test]
fn run_replaces_itself_with_the_service() {
    let h = Home::new();
    h.write_service(
        "demo",
        "command = [\"/bin/sh\", \"-c\", \"echo pid=$$ lang=${LANG:-unset} port=$PORT; exit 7\"]\n\
         [env]\nPORT = \"3773\"\n",
    );
    let child = h
        .cmd(&["run", "demo"])
        .env("LANG", "zh_CN.UTF-8")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(7), "the service's exit status");
    assert_eq!(stdout(&o), format!("pid={pid} lang=unset port=3773\n"));
}

// Requirement: Refuse to start without an env file, Exit status on failure
#[test]
fn run_refuses_to_start_without_its_env_file() {
    let h = Home::new();
    h.write_service(
        "demo",
        "command = [\"/bin/sh\", \"-c\", \"echo started\"]\ncwd = \"~/work\"\nenv_file = [\".env\"]\n",
    );
    let o = h.run(&["run", "demo"]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stdout(&o), "");
    let err = stderr(&o);
    assert!(
        err.contains("not starting") && err.contains("work/.env"),
        "{err}"
    );
}

// Requirement: Completion scripts
#[test]
fn completion_scripts() {
    let h = Home::new();
    for shell in ["fish", "zsh", "bash"] {
        let o = h.run(&["completion", shell]);
        assert!(
            o.status.success() && stdout(&o).contains("COMPLETE="),
            "{shell}"
        );
    }
    h.write_service("demo", "command = [\"true\"]\n");
    let o = h
        .cmd(&["--", "hum", "start", ""])
        .env("COMPLETE", "fish")
        .output()
        .unwrap();
    assert!(stdout(&o).lines().any(|l| l == "demo"), "{}", stdout(&o));
}
