/// Reports a closed release execution gate or invalid release input.
#[derive(Debug, derive_more::Display)]
pub(crate) enum ReleaseError {
    #[display(
        "Invalid release invocation; Rust release execution remains disabled pending \
         authenticated bootstrap and preflight"
    )]
    Usage,
    #[display("Invalid release document")]
    Document,
    #[cfg(test)]
    #[display("Release source is not authenticated")]
    Source,
    #[display("Git object verification failed")]
    GitObject,
    #[display("Original release material is not verified")]
    Material,
    #[display("Bounded release child process failed")]
    Process,
}

impl core::error::Error for ReleaseError {}
