use derive_more::Display;

/// Root error type for platform service operations.
///
/// Use with `error-stack`'s `Report` to attach context before propagating.
#[derive(Debug, Display)]
pub enum PlatformError {
    /// Config store access failed.
    #[display("config store error")]
    ConfigStore,
    /// Secret store access failed.
    #[display("secret store error")]
    SecretStore,
    /// Geo lookup failed.
    #[display("geo lookup error")]
    Geo,
}

impl core::error::Error for PlatformError {}
