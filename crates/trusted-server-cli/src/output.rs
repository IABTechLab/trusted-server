//! User-facing console output for the `ts` binary.
//!
//! This is the only module permitted to write to stdout/stderr directly;
//! everything else uses `log`.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::{self, Write as _};

use error_stack::{Report, ResultExt as _};
use serde::Serialize;

use crate::error::CliError;

/// Prints an informational line to stdout.
#[cfg(target_os = "macos")]
pub fn info(message: &str) {
    println!("{message}");
}

/// Prints a warning line to stderr.
#[cfg(target_os = "macos")]
pub fn warn(message: &str) {
    eprintln!("warning: {message}");
}

/// Writes a single line to stdout, followed by a newline.
///
/// # Errors
///
/// Returns [`CliError::Io`] if the underlying write fails.
pub fn write_stdout_line(line: impl AsRef<str>) -> Result<(), Report<CliError>> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{}", line.as_ref()).change_context(CliError::Io)
}

/// Writes a single line to stderr, followed by a newline.
///
/// # Errors
///
/// Returns [`CliError::Io`] if the underlying write fails.
pub fn write_stderr_line(line: impl AsRef<str>) -> Result<(), Report<CliError>> {
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "{}", line.as_ref()).change_context(CliError::Io)
}

/// Serializes `value` as pretty JSON to stdout, followed by a newline.
///
/// # Errors
///
/// Returns [`CliError::Json`] if serialization fails, or [`CliError::Io`] if
/// writing the trailing newline fails.
pub fn write_json<T>(value: &T) -> Result<(), Report<CliError>>
where
    T: Serialize,
{
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value).change_context(CliError::Json)?;
    writeln!(stdout).change_context(CliError::Io)
}
