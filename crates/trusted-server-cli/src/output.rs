//! User-facing console output for the `ts` binary.
//!
//! This is the only module permitted to write to stdout/stderr directly;
//! everything else uses `log`.
#![allow(clippy::print_stdout, clippy::print_stderr)]

/// Prints an informational line to stdout.
pub fn info(message: &str) {
    println!("{message}");
}

/// Prints a warning line to stderr.
pub fn warn(message: &str) {
    eprintln!("warning: {message}");
}

/// Prints an error line to stderr.
pub fn error(message: &str) {
    eprintln!("error: {message}");
}
