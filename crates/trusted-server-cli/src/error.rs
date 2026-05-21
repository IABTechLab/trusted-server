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
    #[display("environment error")]
    EnvironmentError,
    #[display("found {count} disallowed host(s)")]
    ViolationsFound {
        /// Number of disallowed hosts found across all scanned files.
        count: usize,
    },
}

impl Error for CliError {}
