//! Trusted Server developer CLI library. The `ts` binary is a thin wrapper;
//! all logic lives here so integration tests can exercise it.

pub mod output;

// `ts dev proxy` — the crate's sole command — is macOS-only (CA trust via the
// login keychain, Safari automation via `networksetup`, a native TLS/networking
// stack). Its dependencies are scoped to macOS in `Cargo.toml`, so the command
// module only exists there; on other targets the crate builds as an empty shell.
// Cross-platform support is future work (design spec §16).
#[cfg(target_os = "macos")]
pub mod commands;

#[cfg(target_os = "macos")]
use clap::Parser;
#[cfg(target_os = "macos")]
use commands::dev::DevCommand;

/// The `ts` command-line interface.
#[cfg(target_os = "macos")]
#[derive(Debug, Parser)]
#[command(name = "ts", version, about = "Trusted Server developer CLI")]
pub struct Cli {
    #[command(subcommand)]
    command: TopCommand,
}

#[cfg(target_os = "macos")]
#[derive(Debug, clap::Subcommand)]
enum TopCommand {
    /// Local development tools.
    #[command(subcommand)]
    Dev(DevCommand),
}

#[cfg(target_os = "macos")]
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
