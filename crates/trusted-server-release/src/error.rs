/// Reports a closed release execution gate or invalid document input.
#[derive(Debug, derive_more::Display)]
pub(crate) enum ReleaseError {
    #[display(
        "Rust release execution remains disabled pending authenticated bootstrap and preflight"
    )]
    Pending,
    #[display("Invalid release document")]
    Document,
}

impl core::error::Error for ReleaseError {}
