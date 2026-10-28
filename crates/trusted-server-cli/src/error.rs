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

/// Formats a TOML error's location without the parser's message.
///
/// Parser and deserializer messages quote the offending source line or value,
/// which can hold data an operator keeps private, so only the 1-based line and
/// column of the error within `source` are reported.
pub(crate) fn toml_error_location(source: &str, span: Option<core::ops::Range<usize>>) -> String {
    let Some(start) = span.map(|span| span.start) else {
        return String::new();
    };
    let before = source.get(..start).unwrap_or(source);
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |tail| tail.chars().count())
        + 1;
    format!(" at line {line}, column {column}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_error_location_reports_line_and_column() {
        let source = "a = 1\nb = \"x\n";

        let location = toml_error_location(source, Some(10..11));

        assert_eq!(
            location, " at line 2, column 5",
            "should map offset to line and column"
        );
    }

    #[test]
    fn toml_error_location_is_empty_without_span() {
        assert_eq!(
            toml_error_location("a = 1", None),
            "",
            "should omit location without span"
        );
    }

    #[test]
    fn toml_error_location_counts_columns_in_characters() {
        // "é" is two bytes, so counting bytes would report column 10.
        let source = "a = \"é\" x\n";

        let location = toml_error_location(source, Some(9..10));

        assert_eq!(
            location, " at line 1, column 9",
            "should count the column in characters, not bytes"
        );
    }
}
