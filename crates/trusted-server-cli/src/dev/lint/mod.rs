//! `ts dev lint` subcommand group: linters for source/config/docs.
//!
//! Subcommands:
//! - `domains`: URL-host linter (this design).

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use error_stack::Report;

use crate::error::CliError;

pub mod domains;

#[cfg(test)]
pub(crate) mod test_support;

/// Subcommands under `ts dev lint`.
#[derive(Debug, Subcommand)]
pub enum LintCommand {
    /// Lint URL hosts in source/config/docs.
    Domains(DomainsArgs),
}

/// Arguments for `ts dev lint domains`.
#[derive(Debug, Args)]
pub struct DomainsArgs {
    /// Pre-commit mode: scan only staged-added lines.
    #[arg(long, conflicts_with_all = ["changed_vs", "paths"])]
    pub staged: bool,

    /// CI/PR mode: scan only lines added relative to merge-base(<ref>, HEAD).
    #[arg(long, value_name = "REF", conflicts_with_all = ["staged", "paths"])]
    pub changed_vs: Option<String>,

    /// Explicit paths to scan in full. Mutually exclusive with
    /// `--staged` / `--changed-vs`.
    #[arg(value_name = "PATH", conflicts_with_all = ["staged", "changed_vs"])]
    pub paths: Vec<PathBuf>,

    /// Output format.
    #[arg(long, value_enum, default_value = "human")]
    pub format: OutputFormat,

    /// Print per-file scan progress on stderr. Has no effect on the
    /// exit code or violation count.
    #[arg(long)]
    pub verbose: bool,
}

/// Output format for `ts dev lint domains`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    /// Human-readable `path:line: disallowed host <host>` lines.
    Human,
    /// Structured JSON report.
    Json,
}

/// Dispatch a `ts dev lint` subcommand.
///
/// # Errors
///
/// Propagates the error from the chosen linter.
pub fn run(command: LintCommand) -> Result<(), Report<CliError>> {
    match command {
        LintCommand::Domains(args) => domains::run(&args),
    }
}
