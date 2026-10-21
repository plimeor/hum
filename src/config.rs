//! Loads, validates and writes hum's TOML configuration and knows where
//! every file hum touches lives.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// Prefix of every launchd label hum owns.
pub const LABEL_PREFIX: &str = "hum.";
/// The internal log rotation job. Service names cannot start with "_".
pub const ROTATE_NAME: &str = "_rotate";
pub const DEFAULT_LOG_MAX_SIZE: u64 = 10 << 20;
const DEFAULT_LOG_MAX_SIZE_TEXT: &str = "10MB";
pub const DEFAULT_LOG_KEEP: u64 = 3;

/// A service name matching `[a-z0-9][a-z0-9_-]*`, so it is safe in file
/// names and launchd labels. Checked once, where a name enters hum.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ServiceName(String);

impl ServiceName {
    pub fn parse(name: &str) -> Result<Self> {
        let mut chars = name.chars();
        let ok = chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
        if !ok {
            bail!("invalid service name {name:?}: must match [a-z0-9][a-z0-9_-]*");
        }
        Ok(ServiceName(name.to_string()))
    }
}

impl std::str::FromStr for ServiceName {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        ServiceName::parse(s)
    }
}

impl std::ops::Deref for ServiceName {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ServiceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn validate_env_key(key: &str) -> Result<()> {
    let mut chars = key.chars();
    let ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ok {
        bail!("invalid environment variable name {key:?}");
    }
    Ok(())
}

pub fn label(name: &str) -> String {
    format!("{LABEL_PREFIX}{name}")
}

/// Every location hum reads or writes.
#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
    /// `~/.config/hum`, or `$HUM_HOME`.
    pub config_dir: PathBuf,
    pub launch_agents: PathBuf,
    pub log_dir: PathBuf,
    /// `$HUM_HOME` when set; passed on to launchd jobs so `hum run` reads
    /// the same directory.
    pub hum_home: Option<PathBuf>,
}

impl Paths {
    pub fn from_env() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(std::env::home_dir)
            .context("cannot determine the home directory")?;
        let hum_home = match std::env::var("HUM_HOME") {
            Ok(h) if !h.is_empty() => Some(std::path::absolute(expand_path(&h, &home))?),
            _ => None,
        };
        Ok(Self::with_home(home, hum_home))
    }

    pub fn with_home(home: PathBuf, hum_home: Option<PathBuf>) -> Self {
        Paths {
            config_dir: hum_home.clone().unwrap_or_else(|| home.join(".config/hum")),
            launch_agents: home.join("Library/LaunchAgents"),
            log_dir: home.join("Library/Logs/hum"),
            hum_home,
            home,
        }
    }

    pub fn services_dir(&self) -> PathBuf {
        self.config_dir.join("services")
    }

    pub fn service_file(&self, name: &str) -> PathBuf {
        self.services_dir().join(format!("{name}.toml"))
    }

    pub fn global_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn plist_file(&self, name: &str) -> PathBuf {
        self.launch_agents.join(format!("{}.plist", label(name)))
    }

    pub fn log_file(&self, name: &str) -> PathBuf {
        self.log_dir.join(format!("{name}.log"))
    }

    /// Names of the files in `services/`, sorted. A name that is not a
    /// valid service name is returned as the error for that entry.
    pub fn service_files(&self) -> Result<Vec<Result<ServiceName, String>>> {
        let names = list_names(&self.services_dir(), "", ".toml")?;
        Ok(names
            .into_iter()
            .map(|n| ServiceName::parse(&n).map_err(|_| n))
            .collect())
    }

    /// Valid service names that have a config file, sorted.
    pub fn service_names(&self) -> Result<Vec<ServiceName>> {
        Ok(self.service_files()?.into_iter().flatten().collect())
    }

    /// Services that have a hum plist, sorted. Plists whose name is not a
    /// valid service name, including the rotate job's, are excluded.
    pub fn plist_names(&self) -> Result<Vec<ServiceName>> {
        let names = list_names(&self.launch_agents, LABEL_PREFIX, ".plist")?;
        Ok(names
            .iter()
            .filter_map(|n| ServiceName::parse(n).ok())
            .collect())
    }
}

fn list_names(dir: &Path, prefix: &str, suffix: &str) -> Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        r => r.with_context(|| format!("reading {}", dir.display()))?,
    };
    let mut names = vec![];
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            continue;
        }
        let file = entry.file_name().to_string_lossy().into_owned();
        if let Some(name) = file
            .strip_prefix(prefix)
            .and_then(|f| f.strip_suffix(suffix))
        {
            names.push(name.to_string());
        }
    }
    names.sort();
    Ok(names)
}

/// Expands a leading `~` and `$HOME` / `${HOME}`.
pub fn expand_path(path: &str, home: &Path) -> PathBuf {
    let home_str = home.to_string_lossy();
    let path = if path == "~" {
        home_str.to_string()
    } else if let Some(rest) = path.strip_prefix("~/") {
        format!("{home_str}/{rest}")
    } else {
        path.to_string()
    };
    PathBuf::from(
        path.replace("${HOME}", &home_str)
            .replace("$HOME", &home_str),
    )
}

/// Rewrites a path under home to the `~/...` form.
pub fn contract_path(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Writes data to a temp file next to path, then renames it into place,
/// keeping the existing file's permissions.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(data)?;
    let mode = fs::metadata(path)
        .map(|m| m.permissions().mode())
        .unwrap_or(0o644);
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    tmp.persist(path)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Restart {
    Always,
    OnFailure,
    Never,
}

impl Restart {
    pub fn as_str(self) -> &'static str {
        match self {
            Restart::Always => "always",
            Restart::OnFailure => "on-failure",
            Restart::Never => "never",
        }
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLog {
    max_size: Option<String>,
    keep: Option<i64>,
}

/// The `[log]` table, in the global or a service config.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogSettings {
    /// Bytes.
    pub max_size: Option<u64>,
    pub keep: Option<u64>,
}

fn parse_log(raw: RawLog) -> Result<LogSettings> {
    let max_size = match raw.max_size {
        Some(s) => Some(parse_size(&s).context("[log] max_size")?),
        None => None,
    };
    let keep = match raw.keep {
        Some(k) => Some(u64::try_from(k).map_err(|_| anyhow::anyhow!("[log] keep must be >= 0"))?),
        None => None,
    };
    Ok(LogSettings { max_size, keep })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawService {
    command: Option<toml::Value>,
    cwd: Option<String>,
    #[serde(default)]
    env_file: Vec<String>,
    autostart: Option<bool>,
    restart: Option<Restart>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    log: RawLog,
}

/// The program a service runs.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Argv(Vec<String>),
    /// Run through `/bin/sh -c`.
    Shell(String),
}

/// A parsed `services/<name>.toml`.
#[derive(Clone, Debug)]
pub struct Service {
    pub name: ServiceName,
    pub command: Command,
    /// As written; `None` means `$HOME`.
    pub cwd: Option<String>,
    pub env_file: Vec<String>,
    pub autostart: bool,
    pub restart: Restart,
    pub env: BTreeMap<String, String>,
    pub log: LogSettings,
}

impl Service {
    pub fn argv(&self) -> Vec<String> {
        match &self.command {
            Command::Argv(a) => a.clone(),
            Command::Shell(s) => vec!["/bin/sh".into(), "-c".into(), s.clone()],
        }
    }

    pub fn command_string(&self) -> String {
        match &self.command {
            Command::Shell(s) => s.clone(),
            Command::Argv(a) => a
                .iter()
                .map(|s| {
                    if s.is_empty() || s.contains([' ', '\t', '\n', '"', '\'', '\\', '$']) {
                        format!("{s:?}")
                    } else {
                        s.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    pub fn resolved_cwd(&self, home: &Path) -> PathBuf {
        match &self.cwd {
            Some(c) => expand_path(c, home),
            None => home.to_path_buf(),
        }
    }

    /// env_file entries, expanded and resolved against the working directory.
    pub fn resolved_env_files(&self, home: &Path) -> Vec<PathBuf> {
        let cwd = self.resolved_cwd(home);
        self.env_file
            .iter()
            .map(|f| cwd.join(expand_path(f, home)))
            .collect()
    }
}

pub fn parse_service(name: &ServiceName, text: &str) -> Result<Service> {
    let raw: RawService = toml::from_str(text)?;
    let command = match raw.command {
        None => bail!("command is required"),
        Some(toml::Value::String(s)) if s.trim().is_empty() => bail!("command must not be empty"),
        Some(toml::Value::String(s)) => Command::Shell(s),
        Some(toml::Value::Array(items)) => {
            let argv = items
                .into_iter()
                .enumerate()
                .map(|(i, v)| match v {
                    toml::Value::String(s) => Ok(s),
                    _ => bail!("command[{i}] must be a string"),
                })
                .collect::<Result<Vec<_>>>()?;
            if argv.first().is_none_or(|a| a.is_empty()) {
                bail!("command must not be empty");
            }
            Command::Argv(argv)
        }
        Some(_) => bail!("command must be an array of strings or a string"),
    };
    if raw.env_file.iter().any(|f| f.is_empty()) {
        bail!("env_file entries must not be empty");
    }
    validate_env(&raw.env)?;
    let log = parse_log(raw.log)?;
    Ok(Service {
        name: name.clone(),
        command,
        cwd: raw.cwd,
        env_file: raw.env_file,
        autostart: raw.autostart.unwrap_or(true),
        restart: raw.restart.unwrap_or(Restart::Always),
        env: raw.env,
        log,
    })
}

pub fn load_service(paths: &Paths, name: &ServiceName) -> Result<Service> {
    let file = paths.service_file(name);
    let text = match fs::read_to_string(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("service {name:?} not found (no {})", file.display())
        }
        r => r?,
    };
    parse_service(name, &text).with_context(|| file.display().to_string())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGlobal {
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    log: RawLog,
}

/// The parsed `config.toml`.
#[derive(Clone, Debug, Default)]
pub struct Global {
    pub env: BTreeMap<String, String>,
    pub log: LogSettings,
}

pub fn parse_global(text: &str) -> Result<Global> {
    let raw: RawGlobal = toml::from_str(text)?;
    validate_env(&raw.env)?;
    Ok(Global {
        env: raw.env,
        log: parse_log(raw.log)?,
    })
}

/// Reads `config.toml`; a missing file is an empty config.
pub fn load_global(paths: &Paths) -> Result<Global> {
    let file = paths.global_file();
    match fs::read_to_string(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Global::default()),
        r => parse_global(&r?).with_context(|| file.display().to_string()),
    }
}

/// Creates `config.toml` with the given PATH unless it exists. Reports
/// whether it was created.
pub fn ensure_global(paths: &Paths, path_env: &str) -> Result<bool> {
    let file = paths.global_file();
    if file.exists() {
        return Ok(false);
    }
    let mut text = String::from(
        "# hum global config, shared by every service.\n\
         # PATH was captured from the shell that first ran hum.\n\n[env]\n",
    );
    if !path_env.is_empty() {
        text += &format!("PATH = {}\n", toml_edit::Value::from(path_env));
    }
    text += &format!(
        "\n[log]\n# max_size = \"{DEFAULT_LOG_MAX_SIZE_TEXT}\"\n# keep = {DEFAULT_LOG_KEEP}\n"
    );
    fs::create_dir_all(paths.services_dir())?;
    write_atomic(&file, text.as_bytes())?;
    Ok(true)
}

/// Rotation settings: service over global over default.
pub fn effective_log(global: &Global, service: Option<&Service>) -> (u64, u64) {
    let layers = [Some(&global.log), service.map(|s| &s.log)];
    let mut size = DEFAULT_LOG_MAX_SIZE;
    let mut keep = DEFAULT_LOG_KEEP;
    for l in layers.into_iter().flatten() {
        size = l.max_size.unwrap_or(size);
        keep = l.keep.unwrap_or(keep);
    }
    (size, keep)
}

fn validate_env(env: &BTreeMap<String, String>) -> Result<()> {
    for k in env.keys() {
        validate_env_key(k).context("[env]")?;
    }
    Ok(())
}

/// Parses sizes such as "10MB", "512KB", "1G" or "4096" (powers of 1024).
pub fn parse_size(s: &str) -> Result<u64> {
    let upper = s.trim().to_ascii_uppercase();
    let digits = upper
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(upper.len());
    let (num, unit) = upper.split_at(digits);
    let shift = match unit.trim_start() {
        "" | "B" => 0,
        "K" | "KB" | "KIB" => 10,
        "M" | "MB" | "MIB" => 20,
        "G" | "GB" | "GIB" => 30,
        _ => bail!("invalid size {s:?} (e.g. \"10MB\")"),
    };
    match num.parse::<u64>() {
        Ok(n) if n > 0 => Ok(n << shift),
        _ => bail!("invalid size {s:?} (e.g. \"10MB\")"),
    }
}

/// How `hum set` turns a command-line value into TOML.
#[derive(Clone, Copy)]
enum ValueKind {
    Str,
    Bool,
    Int,
    /// Comma-separated list of strings.
    List,
}

/// Keys of a service config that `hum set` / `hum unset` accept, besides
/// `env.<NAME>`. Kept next to `RawService` so the two change together.
const EDIT_KEYS: [(&str, ValueKind); 7] = [
    ("command", ValueKind::Str),
    ("cwd", ValueKind::Str),
    ("env_file", ValueKind::List),
    ("autostart", ValueKind::Bool),
    ("restart", ValueKind::Str),
    ("log.max_size", ValueKind::Str),
    ("log.keep", ValueKind::Int),
];

/// A service config key addressed from the command line.
pub struct EditKey {
    pub path: Vec<String>,
    kind: ValueKind,
}

impl EditKey {
    pub fn parse(key: &str) -> Result<Self> {
        if let Some(var) = key.strip_prefix("env.") {
            validate_env_key(var)?;
            return Ok(EditKey {
                path: vec!["env".into(), var.into()],
                kind: ValueKind::Str,
            });
        }
        match EDIT_KEYS.iter().find(|(k, _)| *k == key) {
            Some((k, kind)) => Ok(EditKey {
                path: k.split('.').map(String::from).collect(),
                kind: *kind,
            }),
            None => bail!(
                "unknown key {key:?} (known: {}, env.<NAME>)",
                EDIT_KEYS.map(|(k, _)| k).join(", ")
            ),
        }
    }

    /// The TOML value for a command-line value of this key.
    pub fn value(&self, raw: &str) -> Result<toml_edit::Value> {
        let key = self.path.join(".");
        Ok(match self.kind {
            ValueKind::Str => raw.into(),
            ValueKind::Bool => raw
                .parse::<bool>()
                .map_err(|_| anyhow::anyhow!("{key}: expected true or false, got {raw:?}"))?
                .into(),
            ValueKind::Int => raw
                .parse::<i64>()
                .map_err(|_| anyhow::anyhow!("{key}: expected an integer, got {raw:?}"))?
                .into(),
            ValueKind::List => raw
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect::<toml_edit::Array>()
                .into(),
        })
    }
}

/// Renders a freshly created service config.
pub fn render_service(s: &Service) -> String {
    let mut doc = toml_edit::DocumentMut::new();
    doc["command"] = match &s.command {
        Command::Argv(a) => toml_edit::value(a.iter().collect::<toml_edit::Array>()),
        Command::Shell(c) => toml_edit::value(c.as_str()),
    };
    if let Some(cwd) = &s.cwd {
        doc["cwd"] = toml_edit::value(cwd.as_str());
    }
    if !s.env_file.is_empty() {
        doc["env_file"] = toml_edit::value(s.env_file.iter().collect::<toml_edit::Array>());
    }
    doc["autostart"] = toml_edit::value(s.autostart);
    doc["restart"] = toml_edit::value(s.restart.as_str());
    if !s.env.is_empty() {
        let mut env = toml_edit::Table::new();
        for (k, v) in &s.env {
            env[k] = toml_edit::value(v.as_str());
        }
        doc["env"] = toml_edit::Item::Table(env);
    }
    doc.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(name: &str) -> ServiceName {
        ServiceName::parse(name).unwrap()
    }

    #[test]
    fn defaults() {
        let s = parse_service(&n("t3"), r#"command = ["t3", "server"]"#).unwrap();
        assert!(s.autostart);
        assert_eq!(s.restart, Restart::Always);
        assert_eq!(s.argv(), ["t3", "server"]);
    }

    // Requirement: 字符串形式的 command
    #[test]
    fn shell_command() {
        let s = parse_service(&n("x"), r#"command = "echo hi && sleep 1""#).unwrap();
        assert_eq!(s.argv(), ["/bin/sh", "-c", "echo hi && sleep 1"]);
    }

    // Requirement: 严格解析
    #[test]
    fn strict_parsing_rejects_invalid_files() {
        let cases = [
            ("missing command", r#"cwd = "/tmp""#),
            ("empty array", "command = []"),
            ("non-string arg", r#"command = ["a", 1]"#),
            ("bad restart", "command = [\"a\"]\nrestart = \"sometimes\""),
            ("unknown field", "command = [\"a\"]\nport = 1"),
            ("non-string env", "command = [\"a\"]\n[env]\nPORT = 3773"),
            ("bad env key", "command = [\"a\"]\n[env]\n\"A-B\" = \"1\""),
            (
                "bad log size",
                "command = [\"a\"]\n[log]\nmax_size = \"huge\"",
            ),
            ("negative keep", "command = [\"a\"]\n[log]\nkeep = -1"),
            ("unknown log field", "command = [\"a\"]\n[log]\nsize = 1"),
        ];
        for (what, src) in cases {
            assert!(parse_service(&n("x"), src).is_err(), "{what}");
        }
    }

    // Requirement: 文件名即服务名
    #[test]
    fn service_names_must_match_the_pattern() {
        for ok in ["t3", "api-server", "a_b", "0x"] {
            assert!(ServiceName::parse(ok).is_ok(), "{ok}");
        }
        for bad in ["", "_rotate", "-x", "A", "a.b", "a/b", "a b"] {
            assert!(ServiceName::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn sizes() {
        for (s, want) in [
            ("10MB", 10 << 20),
            ("512kb", 512 << 10),
            ("1G", 1 << 30),
            ("4096", 4096),
            ("2MiB", 2 << 20),
        ] {
            assert_eq!(parse_size(s).unwrap(), want, "{s}");
        }
        for bad in ["", "10TB", "x", "0"] {
            assert!(parse_size(bad).is_err(), "{bad:?}");
        }
    }

    // Requirement: 服务配置覆盖全局日志设置
    #[test]
    fn service_log_settings_override_global() {
        assert_eq!(effective_log(&Global::default(), None), (10 << 20, 3));
        let g = parse_global("[log]\nmax_size = \"1MB\"\nkeep = 5").unwrap();
        let s = parse_service(&n("x"), "command = [\"a\"]\n[log]\nkeep = 1").unwrap();
        assert_eq!(effective_log(&g, Some(&s)), (1 << 20, 1));
    }

    #[test]
    fn default_size_text_matches_value() {
        assert_eq!(
            parse_size(DEFAULT_LOG_MAX_SIZE_TEXT).unwrap(),
            DEFAULT_LOG_MAX_SIZE
        );
    }

    // Requirement: 命令行修改字段
    #[test]
    fn every_edit_key_produces_a_valid_config() {
        let samples = [
            ("command", "echo hi"),
            ("cwd", "/tmp"),
            ("env_file", ".env, b.env"),
            ("autostart", "false"),
            ("restart", "never"),
            ("log.max_size", "1MB"),
            ("log.keep", "2"),
            ("env.PORT", "1"),
        ];
        assert_eq!(samples.len(), EDIT_KEYS.len() + 1, "every key has a sample");
        let mut doc = crate::tomledit::Doc::parse("command = [\"a\"]\n").unwrap();
        for (key, raw) in samples {
            let k = EditKey::parse(key).unwrap();
            let path: Vec<&str> = k.path.iter().map(String::as_str).collect();
            doc.set(&path, k.value(raw).unwrap()).unwrap();
        }
        let s = parse_service(&n("x"), &doc.to_string()).unwrap();
        assert_eq!(s.command, Command::Shell("echo hi".into()));
        assert_eq!(s.env_file, [".env", "b.env"]);
        assert!(!s.autostart);
        assert_eq!(s.restart, Restart::Never);
        assert_eq!(
            s.log,
            LogSettings {
                max_size: Some(1 << 20),
                keep: Some(2)
            }
        );
        assert_eq!(s.env["PORT"], "1");
        assert!(EditKey::parse("port").is_err());
        assert!(EditKey::parse("env.A-B").is_err());
        assert!(EditKey::parse("autostart").unwrap().value("yes").is_err());
    }

    // Requirement: 路径展开
    #[test]
    fn path_expansion() {
        let home = Path::new("/Users/me");
        assert_eq!(expand_path("~/work", home), Path::new("/Users/me/work"));
        assert_eq!(expand_path("$HOME/x", home), Path::new("/Users/me/x"));
        assert_eq!(
            contract_path(Path::new("/Users/me/work/t3"), home),
            "~/work/t3"
        );
        assert_eq!(contract_path(Path::new("/opt/x"), home), "/opt/x");
        let s = parse_service(&n("x"),
            "command = [\"a\"]\ncwd = \"~/work/t3\"\nenv_file = [\".env\", \"/etc/x.env\", \"~/shared.env\"]",
        )
        .unwrap();
        let want: Vec<PathBuf> = [
            "/Users/me/work/t3/.env",
            "/etc/x.env",
            "/Users/me/shared.env",
        ]
        .map(PathBuf::from)
        .into();
        assert_eq!(s.resolved_env_files(home), want);
        let s = parse_service(&n("x"), "command = [\"a\"]").unwrap();
        assert_eq!(s.resolved_cwd(home), home);
    }

    #[test]
    fn rendered_service_round_trips() {
        let s = Service {
            name: n("api"),
            command: Command::Argv(vec!["bun".into(), "run".into(), "start \"x\"".into()]),
            cwd: Some("~/work/api".into()),
            env_file: vec![".env".into()],
            autostart: false,
            restart: Restart::OnFailure,
            env: BTreeMap::from([("PORT".into(), "4000".into()), ("B".into(), "a\\b".into())]),
            log: LogSettings::default(),
        };
        let got = parse_service(&n("api"), &render_service(&s)).unwrap();
        assert_eq!(got.command, s.command);
        assert_eq!(got.cwd, s.cwd);
        assert_eq!(got.env, s.env);
        assert_eq!(got.env_file, s.env_file);
        assert!(!got.autostart);
        assert_eq!(got.restart, Restart::OnFailure);
    }

    // Requirement: 首次运行时创建
    #[test]
    fn global_is_created_once_with_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = Paths::with_home(dir.path().into(), None);
        assert!(ensure_global(&p, "/opt/homebrew/bin:/usr/bin").unwrap());
        assert_eq!(
            load_global(&p).unwrap().env["PATH"],
            "/opt/homebrew/bin:/usr/bin"
        );
        assert!(p.services_dir().is_dir());
        assert!(!ensure_global(&p, "/other").unwrap());
    }
}
