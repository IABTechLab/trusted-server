//! Trusted Server developer CLI library. The `ts` binary is a thin wrapper;
//! all logic lives here so integration tests can exercise it.
pub mod commands;
pub mod output;

use clap::Parser;
use commands::dev::DevCommand;

/// The `ts` command-line interface.
#[derive(Debug, Parser)]
#[command(name = "ts", version, about = "Trusted Server developer CLI")]
pub struct Cli {
    #[command(subcommand)]
    command: TopCommand,
}

#[derive(Debug, clap::Subcommand)]
enum TopCommand {
    /// Local development tools.
    #[command(subcommand)]
    Dev(DevCommand),
}

impl Cli {
    /// Runs the parsed CLI, returning a process exit code.
    #[must_use]
    pub fn run(self) -> i32 {
        let result = match self.command {
            TopCommand::Dev(dev) => commands::dev::run(dev),
        };
        if let Err(report) = result {
            output::warn(&format!("{report:?}"));
            return 1;
        }
        0
    }
}
