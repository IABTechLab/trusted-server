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
    #[display("Release input policy rejects the candidate inputs")]
    Policy,
    #[display("Inherited release environment is rejected")]
    Environment,
    #[display("Release source is not authenticated")]
    Source,
    #[display("Git object verification failed")]
    GitObject,
    #[display("Production release selection is not qualified")]
    Unqualified,
    #[display("Release inputs are incomplete")]
    Incomplete,
    #[display("Final artifact inspection rejected the executable")]
    Inspect,
    #[display("Original release material is not verified")]
    Material,
    #[display("Bounded release child process failed")]
    Process,
    #[display("Release network-denial boundary rejected the execution")]
    Boundary,
    #[display("Release recipe inputs or execution plan are rejected")]
    Recipe,
    #[display("Final Fastly runtime acceptance rejected the executable")]
    Runtime,
    #[display("Artifact composition evidence is rejected")]
    Composition,
    #[display("Release transfer collection is rejected")]
    Collection,
    #[display("SPDX generation inputs or document are rejected")]
    Spdx,
    #[display("Candidate release distribution evidence is rejected")]
    Distribution,
}

impl core::error::Error for ReleaseError {}
