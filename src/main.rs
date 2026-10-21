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
