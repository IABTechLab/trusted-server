use derive_more::Display;
use error_stack::Report;

/// Root error type for platform service operations.
///
/// Use with `error-stack`'s `Report` to attach context before propagating.
#[derive(Debug, Display)]
pub enum PlatformError {
    /// Input validation failed before delegating to the platform.
    #[display("validation error")]
    Validation,
    /// Config store access failed.
    #[display("config store error")]
    ConfigStore,
    /// Secret store access failed.
    #[display("secret store error")]
    SecretStore,
    /// The requested key was not present in an otherwise-reachable store.
    ///
    /// Distinct from [`PlatformError::ConfigStore`]/[`PlatformError::SecretStore`],
    /// which signal that the store itself could not be read. Callers that must
    /// fail closed on an unreachable store (for example, refusing to delete a
    /// signing key when they cannot confirm it is not the active one) rely on
    /// this distinction: a genuinely-absent key is safe to treat as absent,
    /// while an unreachable store is not.
    #[display("store key not found")]
    NotFound,
    /// Backend registration or name computation failed.
    #[display("backend error")]
    Backend,
    /// HTTP client request failed.
    #[display("http client error")]
    HttpClient,
    /// Geo lookup failed.
    #[display("geo lookup error")]
    Geo,
    /// Operation is not supported by this platform adapter.
    #[display("unsupported platform operation")]
    Unsupported,
}

impl core::error::Error for PlatformError {}

/// Returns `true` when `report`'s current context is [`PlatformError::NotFound`].
///
/// Lets callers distinguish a genuinely-absent key from a store that could not
/// be read, so they can fail closed on the latter (see
/// `request_signing::rotation`).
#[must_use]
pub fn is_not_found(report: &Report<PlatformError>) -> bool {
    matches!(report.current_context(), PlatformError::NotFound)
}
