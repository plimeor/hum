//! hum registers long-running commands as launchd services and manages them
//! through one directory of TOML files.

mod app;
mod cli;
mod config;
mod dotenv;
mod environ;
mod launchd;
mod logfile;
mod plist;
mod tomledit;

use std::process::ExitCode;

use clap::{CommandFactory, Parser};

fn main() -> ExitCode {
    app::hand_over();
    // Answers shell completion requests (COMPLETE=<shell> hum ...).
    clap_complete::CompleteEnv::with_factory(cli::Cli::command).complete();
    match cli::run(cli::Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if let Some(cli::Exit(code)) = e.downcast_ref::<cli::Exit>() {
                return ExitCode::from(*code as u8);
            }
            eprintln!("hum: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Tests name the spec requirements they cover in a
/// `// Requirement: <name>, <name>` comment. This keeps those names in step
/// with `docs/specs/`, so renaming a requirement fails here until its tests
/// are updated.
#[cfg(test)]
mod spec_links {
    use std::collections::BTreeSet;
    use std::path::Path;

    fn files(dir: &Path, ext: &str, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, ext, out);
            } else if path.extension().is_some_and(|e| e == ext) {
                out.push(path);
            }
        }
    }

    fn lines_after(root: &str, ext: &str, marker: &str) -> Vec<String> {
        let mut paths = vec![];
        files(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join(root),
            ext,
            &mut paths,
        );
        paths
            .iter()
            .flat_map(|p| {
                let text = std::fs::read_to_string(p).unwrap();
                text.lines()
                    .filter_map(|l| l.trim().strip_prefix(marker).map(|s| s.trim().to_string()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[test]
    fn requirement_references_exist() {
        let names: BTreeSet<String> = lines_after("docs/specs", "md", "### Requirement:")
            .into_iter()
            .collect();
        assert!(!names.is_empty());
        assert!(
            names.iter().all(|n| !n.contains(", ")),
            "requirement names are split on \", \""
        );
        let marker = ["//", "Requirement:"].join(" ");
        let unknown: Vec<String> = lines_after("src", "rs", &marker)
            .iter()
            .flat_map(|l| l.split(", ").map(str::to_string).collect::<Vec<_>>())
            .filter(|r| !names.contains(r))
            .collect();
        assert!(unknown.is_empty(), "not in docs/specs: {unknown:?}");
    }
}
