/// Reports a closed release execution gate or invalid release input.
#[derive(Debug, derive_more::Display)]
pub(crate) enum ReleaseError {
    #[display(
        "Rust release execution remains disabled pending authenticated bootstrap and preflight"
    )]
    Pending,
    #[display("Invalid release document")]
    Document,
    #[display("Release input policy rejects the candidate inputs")]
    Policy,
    #[display("Inherited release environment is rejected")]
    Environment,
    #[display("Release source is not authenticated")]
    Source,
    #[display("Production release selection is not qualified")]
    Unqualified,
    #[display("Release inputs are incomplete")]
    Incomplete,
}

impl core::error::Error for ReleaseError {}
