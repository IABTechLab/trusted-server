pub(crate) type CliResult<T> = Result<T, String>;

pub(crate) fn cli_error<T>(message: impl Into<String>) -> CliResult<T> {
    Err(message.into())
}

pub(crate) fn report_error(message: impl Into<String>) -> String {
    let message = message.into();
    log::error!("{message}");
    message
}
