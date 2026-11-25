/// Reports a closed release execution gate or invalid release input.
#[derive(Debug, derive_more::Display)]
pub(crate) enum ReleaseError {
    #[display("Invalid release tool invocation")]
    Usage,
    #[display("Invalid release document")]
    Document,
    #[display("Ordinary Fastly rebuild bytes differ")]
    RebuildMismatch,
    #[display("Ordinary build evidence does not join the current run")]
    RebuildIdentity,
    #[display("Final artifact inspection rejected the executable")]
    Inspect,
    #[display("Bounded release child process failed")]
    Process,
    #[display("Release recipe inputs or execution plan are rejected")]
    Recipe,
}

impl core::error::Error for ReleaseError {}
