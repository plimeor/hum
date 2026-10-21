//! Assembles a service's environment from scratch. Layers, lowest priority
//! first: base variables, global `[env]`, the service's env files in order,
//! the service's `[env]`.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::config::{self, Global, Service};
use crate::dotenv;

/// The only variables taken from the calling process.
pub const BASE_KEYS: [&str; 4] = ["HOME", "USER", "SHELL", "TMPDIR"];

pub const SOURCE_BASE: &str = "base";
pub const SOURCE_GLOBAL: &str = "global [env]";
pub const SOURCE_SERVICE: &str = "service [env]";

/// An env file that is missing, unreadable or malformed.
#[derive(Debug)]
pub struct EnvFileError {
    pub path: PathBuf,
    pub message: String,
}

impl std::fmt::Display for EnvFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "env_file {}: {}", self.path.display(), self.message)
    }
}

impl std::error::Error for EnvFileError {}

#[derive(Debug, PartialEq)]
pub struct Undefined {
    pub file: String,
    pub line: usize,
    pub name: String,
}

#[derive(Debug, Default)]
pub struct Env {
    pub vars: BTreeMap<String, String>,
    /// Key -> the layer that set its final value.
    pub sources: BTreeMap<String, String>,
    pub undefined: Vec<Undefined>,
}

impl Env {
    fn set(&mut self, k: &str, v: &str, source: &str) {
        self.vars.insert(k.into(), v.into());
        self.sources.insert(k.into(), source.into());
    }
}

/// The base variables of the current process.
pub fn base() -> BTreeMap<String, String> {
    BASE_KEYS
        .iter()
        .filter_map(|k| Some((k.to_string(), std::env::var(k).ok()?)))
        .collect()
}

/// Builds the environment for a service. On a bad env file it returns the
/// error together with the layers assembled so far, so `hum show` can
/// still display them.
pub fn assemble(
    base: &BTreeMap<String, String>,
    global: &Global,
    service: &Service,
    home: &Path,
) -> (Env, Option<EnvFileError>) {
    let mut env = Env::default();
    for (k, v) in base {
        env.set(k, v, SOURCE_BASE);
    }
    for (k, v) in &global.env {
        env.set(k, v, SOURCE_GLOBAL);
    }
    for path in service.resolved_env_files(home) {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                let message = if e.kind() == std::io::ErrorKind::NotFound {
                    "no such file".into()
                } else {
                    e.to_string()
                };
                return (env, Some(EnvFileError { path, message }));
            }
        };
        let parsed = match dotenv::parse(&text, |k| env.vars.get(k).cloned()) {
            Ok(p) => p,
            Err(e) => {
                return (
                    env,
                    Some(EnvFileError {
                        path,
                        message: e.to_string(),
                    }),
                );
            }
        };
        let label = config::contract_path(&path, home);
        for v in parsed.vars {
            env.set(&v.key, &v.value, &label);
        }
        for u in parsed.undefined {
            env.undefined.push(Undefined {
                file: label.clone(),
                line: u.line,
                name: u.name,
            });
        }
    }
    for (k, v) in &service.env {
        env.set(k, v, SOURCE_SERVICE);
    }
    (env, None)
}

/// Finds `file` in the PATH value `path`, like `which` but against the
/// assembled environment instead of the caller's.
pub fn look_path(file: &str, path: &str) -> Option<PathBuf> {
    let executable = |p: &Path| {
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if file.contains('/') {
        return executable(Path::new(file)).then(|| PathBuf::from(file));
    }
    std::env::split_paths(path)
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join(file))
        .find(|p| executable(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // Requirement: 环境从零组装
    #[test]
    fn layers_priority_and_sources() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let cwd = home.join("work");
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            cwd.join(".env"),
            "A=env1\nB=env1\nDATA=${HOME}/data\nP=$PATH:/extra\n",
        )
        .unwrap();
        fs::write(cwd.join(".env.local"), "B=env2\nC=$A-$SVC\n").unwrap();
        let base = BTreeMap::from([
            ("HOME".to_string(), home.display().to_string()),
            ("USER".into(), "me".into()),
        ]);
        let global = config::parse_global(
            "[env]\nPATH = \"/usr/bin\"\nA = \"global\"\nLANG = \"en_US.UTF-8\"",
        )
        .unwrap();
        let svc = config::parse_service(&"t3".parse().unwrap(),
            &format!(
                "command = [\"a\"]\ncwd = \"{}\"\nenv_file = [\".env\", \".env.local\"]\n[env]\nA = \"svc\"\nSVC = \"literal $HOME\"\n",
                cwd.display()
            ),
        )
        .unwrap();
        let (env, err) = assemble(&base, &global, &svc, home);
        assert!(err.is_none());
        let want = [
            ("USER", "me", SOURCE_BASE),
            ("LANG", "en_US.UTF-8", SOURCE_GLOBAL),
            ("PATH", "/usr/bin", SOURCE_GLOBAL),
            ("A", "svc", SOURCE_SERVICE),
            ("B", "env2", "~/work/.env.local"),
            ("DATA", &format!("{}/data", home.display()), "~/work/.env"),
            ("P", "/usr/bin:/extra", "~/work/.env"),
            // $A is the .env value; the service's own [env] is not visible.
            ("C", "env1-", "~/work/.env.local"),
            ("SVC", "literal $HOME", SOURCE_SERVICE),
        ];
        for (k, v, src) in want {
            assert_eq!(
                (env.vars[k].as_str(), env.sources[k].as_str()),
                (v, src),
                "{k}"
            );
        }
        assert_eq!(env.undefined.len(), 1);
        assert_eq!(env.undefined[0].name, "SVC");
    }

    // Requirement: env 文件缺失时拒绝启动
    #[test]
    fn missing_env_file() {
        let home = tempfile::tempdir().unwrap();
        let svc = config::parse_service(
            &"x".parse().unwrap(),
            "command = [\"a\"]\nenv_file = [\".env\"]",
        )
        .unwrap();
        let (_, err) = assemble(&BTreeMap::new(), &Global::default(), &svc, home.path());
        let err = err.unwrap();
        assert_eq!(err.message, "no such file");
        assert_eq!(err.path, home.path().join(".env"));
    }

    // Requirement: 用组装后的 PATH 查找命令
    #[test]
    fn look_path_uses_the_given_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("tool");
        fs::write(&bin, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.path().join("noexec"), "").unwrap();
        let path = format!("/nonexistent:{}", dir.path().display());
        assert_eq!(look_path("tool", &path), Some(bin.clone()));
        assert_eq!(look_path("noexec", &path), None);
        assert_eq!(look_path("tool", ""), None);
        assert_eq!(look_path(bin.to_str().unwrap(), ""), Some(bin));
    }
}
