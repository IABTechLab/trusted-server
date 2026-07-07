use derive_more::Display;

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
    /// Config store value was read but cannot be decoded (present but corrupt).
    ///
    /// Unlike [`PlatformError::ConfigStore`], this is terminal: the store and
    /// key are reachable, so retrying cannot succeed until the value is
    /// reseeded.
    #[display("config value invalid")]
    ConfigValueInvalid,
    /// Secret store access failed.
    #[display("secret store error")]
    SecretStore,
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
