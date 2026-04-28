use std::io::{self, Write as _};

use error_stack::{Report, ResultExt};
use serde::Serialize;

use crate::error::CliError;

pub fn write_stdout_line(line: impl AsRef<str>) -> Result<(), Report<CliError>> {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{}", line.as_ref()).change_context(CliError::Io)
}

pub fn write_stderr_line(line: impl AsRef<str>) -> Result<(), Report<CliError>> {
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "{}", line.as_ref()).change_context(CliError::Io)
}

pub fn write_json<T>(value: &T) -> Result<(), Report<CliError>>
where
    T: Serialize,
{
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value).change_context(CliError::Json)?;
    writeln!(stdout).change_context(CliError::Io)
}

pub fn format_report(error: &Report<CliError>) -> String {
    format!("{error:?}")
}
