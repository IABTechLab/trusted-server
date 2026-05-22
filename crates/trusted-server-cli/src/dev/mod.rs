//! `ts dev` subcommand group: developer-workflow commands.
//!
//! Subcommands:
//! - `serve`: launches the local dev server (formerly `ts dev`).
//! - `lint domains`: URL-host linter (Phase 2+).
//! - `install-hooks`: pre-commit hook installer (Phase 6).

use std::path::PathBuf;

use clap::{Args, Subcommand};

pub mod lint;
pub mod serve;

// Re-export what `lib.rs` consumes via `crate::dev::*`. Other public
// items in `serve` (FASTLY_LOCAL_MANIFEST, render_local_fastly_manifest,
// write_local_fastly_manifest, run_fastly_dev) remain accessible via
// `crate::dev::serve::*` for tests and any future internal consumers.
pub use serve::{Adapter, run_dev_command};

/// Subcommands under `ts dev`.
#[derive(Debug, Subcommand)]
pub enum DevCommand {
    /// Launch the local dev server (formerly `ts dev`).
    Serve(ServeArgs),
    /// Linters for source, config, and documentation.
    Lint {
        /// The lint to run.
        #[command(subcommand)]
        command: lint::LintCommand,
    },
}

/// Arguments for `ts dev serve`. Preserves byte-for-byte the flags
/// of today's `ts dev` leaf — see spec §"This PR must make the
/// CLI-surface change".
#[derive(Debug, Args)]
pub struct ServeArgs {
    #[arg(long, short = 'a', default_value = "fastly")]
    pub adapter: Adapter,
    #[arg(long)]
    pub config: Option<PathBuf>,
    #[arg(long, default_value = "local")]
    pub env: String,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub passthrough: Vec<String>,
}
