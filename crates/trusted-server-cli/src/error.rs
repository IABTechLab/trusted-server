use core::error::Error;

#[derive(Debug, derive_more::Display)]
pub enum CliError {
    #[display("invalid arguments")]
    Arguments,
    #[display("I/O error")]
    Io,
    #[display("configuration error")]
    Configuration,
    #[display("authentication error")]
    Authentication,
    #[display("Fastly API error")]
    FastlyApi,
    #[display("provisioning error")]
    Provisioning,
    #[display("audit error")]
    Audit,
    #[display("development error")]
    Development,
    #[display("JSON serialization error")]
    Json,
    #[display("operation cancelled")]
    Cancelled,
}

impl Error for CliError {}
