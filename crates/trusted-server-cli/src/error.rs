use core::error::Error;

pub(crate) type CliResult<T> = Result<T, String>;

pub(crate) fn cli_error<T>(message: impl Into<String>) -> CliResult<T> {
    Err(message.into())
}

pub(crate) fn report_error(message: impl Into<String>) -> String {
    let message = message.into();
    log::error!("{message}");
    message
}

/// Error context for the pure-Rust `ts dev lint` / `ts dev install-hooks`
/// commands, which use `error-stack` internally and are rendered into the
/// process exit contract by [`crate::commands::dev`].
#[derive(Debug, derive_more::Display)]
pub enum CliError {
    /// An I/O operation (reading a file, writing to stdout/stderr) failed.
    #[display("I/O error")]
    Io,
    /// Serializing a structured report to JSON failed.
    #[display("JSON serialization error")]
    Json,
    /// The environment could not satisfy the command (e.g. not a git
    /// repository, unreadable git state). Maps to exit code 2.
    #[display("environment error")]
    EnvironmentError,
    /// The linter found one or more disallowed hosts. Maps to exit code 1.
    #[display("found {count} disallowed host(s)")]
    ViolationsFound {
        /// Number of disallowed hosts found across all scanned files.
        count: usize,
    },
}

impl Error for CliError {}
