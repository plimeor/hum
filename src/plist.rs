//! Renders launchd property lists. Output is deterministic, so comparing
//! bytes tells whether a plist on disk is current.

use serde::{Deserialize, Serialize};

/// One launchd job.
#[derive(Serialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct Job {
    pub label: String,
    pub program_arguments: Vec<String>,
    pub run_at_load: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<KeepAlive>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_interval: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standard_out_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standard_error_path: Option<String>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum KeepAlive {
    Always(bool),
    OnFailure {
        #[serde(rename = "SuccessfulExit")]
        successful_exit: bool,
    },
}

pub fn render(job: &Job) -> Vec<u8> {
    let mut out = Vec::new();
    plist::to_writer_xml(&mut out, job).expect("serializing a plist to memory");
    out.push(b'\n');
    out
}

/// `ProgramArguments[0]` of a plist.
pub fn program(data: &[u8]) -> Option<String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Args {
        program_arguments: Vec<String>,
    }
    plist::from_bytes::<Args>(data)
        .ok()?
        .program_arguments
        .into_iter()
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn golden(name: &str, job: &Job) {
        let path = format!("{}/testdata/{name}", env!("CARGO_MANIFEST_DIR"));
        let got = render(job);
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(&path, &got).unwrap();
        }
        let want = std::fs::read(&path).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&got),
            String::from_utf8_lossy(&want),
            "{name}"
        );
    }

    // Requirement: Restart policy, Start at login
    #[test]
    fn always() {
        golden(
            "always.plist",
            &Job {
                label: "hum.t3".into(),
                program_arguments: vec![
                    "/Users/me/Library/Application Support/hum/bin/hum (t3)".into(),
                    "run".into(),
                    "t3".into(),
                ],
                run_at_load: true,
                keep_alive: Some(KeepAlive::Always(true)),
                standard_out_path: Some("/Users/me/Library/Logs/hum/t3.log".into()),
                standard_error_path: Some("/Users/me/Library/Logs/hum/t3.log".into()),
                ..Default::default()
            },
        );
    }

    // Requirement: Restart policy
    #[test]
    fn on_failure() {
        golden(
            "on-failure.plist",
            &Job {
                label: "hum.api".into(),
                program_arguments: vec![
                    "/Users/me/Library/Application Support/hum/bin/hum (api)".into(),
                    "run".into(),
                    "api".into(),
                ],
                run_at_load: true,
                keep_alive: Some(KeepAlive::OnFailure {
                    successful_exit: false,
                }),
                standard_out_path: Some("/Users/me/Library/Logs/hum/api.log".into()),
                standard_error_path: Some("/Users/me/Library/Logs/hum/api.log".into()),
                ..Default::default()
            },
        );
    }

    // Requirement: Automatic rotation
    #[test]
    fn rotate_job() {
        golden(
            "rotate.plist",
            &Job {
                label: "hum._rotate".into(),
                program_arguments: [
                    "/Users/me/Library/Application Support/hum/bin/hum (log rotation)",
                    "logs",
                    "--rotate",
                    "--all",
                    "--if-oversize",
                ]
                .map(String::from)
                .into(),
                start_interval: Some(30),
                ..Default::default()
            },
        );
    }

    #[test]
    fn program_is_read_back() {
        let job = Job {
            label: "x".into(),
            program_arguments: vec!["/a & b/hum".into(), "run".into()],
            ..Default::default()
        };
        assert_eq!(program(&render(&job)).as_deref(), Some("/a & b/hum"));
    }
}
