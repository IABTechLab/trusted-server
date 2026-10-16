//! Read-only verification of official release assets.

use clap::{Args, Subcommand};

use crate::release;

/// Read-only operations over the fixed official release cohort.
#[derive(Debug, Subcommand)]
pub(crate) enum ReleaseCommand {
    /// Verify an official release asset against an independently approved commit.
    Verify(VerifyArgs),
}

/// An independently approved source and the exact release asset to verify.
#[derive(Debug, Args)]
pub(crate) struct VerifyArgs {
    /// Exact release tag, such as v1.2.0.
    tag: String,
    /// Exact executable name in the embedded release cohort.
    #[arg(long)]
    asset: String,
    /// Independently approved full, lowercase source commit.
    #[arg(long)]
    expected_source_commit: String,
}

/// Runs release verification without invoking provider or build operations.
///
/// # Errors
///
/// Returns a diagnostic when any input, evidence or publication check fails.
pub(crate) fn run(command: ReleaseCommand) -> Result<(), String> {
    let ReleaseCommand::Verify(args) = command;
    release::verify(&args.tag, &args.asset, &args.expected_source_commit)
}
