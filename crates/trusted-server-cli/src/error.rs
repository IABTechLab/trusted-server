use core::error::Error;

use error_stack::Report;

#[derive(Debug, derive_more::Display)]
#[display("{message}")]
pub struct CliError {
    message: String,
}

impl Error for CliError {}

pub type CliResult<T> = Result<T, Report<CliError>>;

pub fn cli_error<T>(message: impl Into<String>) -> CliResult<T> {
    Err(Report::new(CliError {
        message: message.into(),
    }))
}

pub fn report_error(message: impl Into<String>) -> Report<CliError> {
    Report::new(CliError {
        message: message.into(),
    })
}
