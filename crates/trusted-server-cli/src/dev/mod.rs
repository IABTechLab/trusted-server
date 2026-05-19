//! `ts dev` subcommand group: developer-workflow commands.
//!
//! Subcommands:
//! - `serve`: launches the local dev server (formerly `ts dev`).
//! - `lint domains`: URL-host linter (Phase 2+).
//! - `install-hooks`: pre-commit hook installer (Phase 6).

pub mod serve;

// Re-export what `lib.rs` consumes via `crate::dev::*`. Other public
// items in `serve` (FASTLY_LOCAL_MANIFEST, render_local_fastly_manifest,
// write_local_fastly_manifest, run_fastly_dev) remain accessible via
// `crate::dev::serve::*` for tests and any future internal consumers.
pub use serve::{Adapter, run_dev_command};
