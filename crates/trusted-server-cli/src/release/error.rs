//! Concrete release verification errors.

use std::fmt;

use error_stack::Report;

/// Reports why release verification refused an input, policy or process.
///
/// Every variant is fail-closed: no verified value is constructed after any of
/// them. Diagnostic attachments never contain credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub(crate) enum ReleaseError {
    /// A JSON document is malformed, ambiguous, oversized or schema-invalid.
    #[display("invalid release document")]
    Document,
    /// The embedded consumer policy or a release index violates the compiled contract.
    #[display("release policy rejects the input")]
    Policy,
    /// The producer policy identity is not current or reviewed historical policy.
    #[display("producer release policy is not accepted")]
    ProducerPolicy,
    /// The approved-source policy could not be acquired from the fixed endpoint.
    #[display("approved source policy acquisition failed")]
    PolicyFetch,
    /// The caller environment cannot safely run the GitHub CLI.
    #[display("GitHub CLI environment is rejected")]
    Environment,
    /// The resolved GitHub CLI is missing or not the qualified version.
    #[display("unsupported GitHub CLI")]
    UnsupportedTool,
    /// A child process could not be spawned, signalled or reaped.
    #[display("GitHub CLI process failed")]
    Process,
    /// A child or attempt deadline elapsed.
    #[display("release verification deadline elapsed")]
    Timeout,
    /// The caller cancelled the verification attempt.
    #[display("release verification was cancelled")]
    Cancelled,
    /// Child stdout or stderr exceeded its byte bound.
    #[display("GitHub CLI output exceeds its byte bound")]
    OutputLimit,
    /// A descendant left the child process group and kept its pipes open.
    #[display("a GitHub CLI descendant escaped process-group termination")]
    DescendantEscaped,
    /// The child exited unsuccessfully; its output is never trusted.
    #[display("GitHub CLI exited unsuccessfully")]
    ExitFailure,
    /// Successful child output is not exactly one attestation result.
    #[display("GitHub CLI output is rejected")]
    Output,
}

impl core::error::Error for ReleaseError {}

/// Builds a report for `context` with one diagnostic attachment.
///
/// Callers must never pass credentials or secret context in `message`.
pub(super) fn report(context: ReleaseError, message: impl fmt::Display) -> Report<ReleaseError> {
    Report::new(context).attach(message.to_string())
}
