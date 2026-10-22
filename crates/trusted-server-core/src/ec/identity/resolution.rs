//! Provider results and errors.

use error_stack::Report;
use http::StatusCode;

use super::descriptor::{Access, Input};

/// Marker message of [`IdentityProviderError::not_implemented`]. The harness
/// matches on it to tell a missing override from a real failure.
pub(crate) const NOT_IMPLEMENTED_MARKER: &str = "capability declared but not implemented: ";

/// A provider failure. Built through the constructors below.
#[derive(Debug, derive_more::Display)]
#[non_exhaustive]
pub enum IdentityProviderError {
    /// Configuration or provider code makes the call impossible.
    #[display("identity provider configuration error: {message}")]
    Config {
        /// What is wrong. Never contains a secret, UID or URL query.
        message: String,
    },
    /// The provider read something its descriptor did not declare.
    #[display("`{access}` was read but not declared")]
    UndeclaredInput {
        /// What was read.
        access: Access,
    },
    /// A declared input is not available for this request.
    #[display("input `{input}` is unavailable for this request")]
    InputUnavailable {
        /// The missing input.
        input: Input,
    },
    /// The provider refused the request.
    #[display("provider rejected the request with status {status}")]
    Rejected {
        /// The response status.
        status: StatusCode,
    },
    /// The provider's response could not be parsed.
    #[display("provider response could not be parsed")]
    Malformed,
    /// The stored private state does not decode as the provider's type.
    #[display("stored provider state could not be decoded")]
    InvalidState,
    /// Set by the coordinator only. Providers never see transport failures.
    #[display("provider request failed in transport")]
    Transport,
}

impl core::error::Error for IdentityProviderError {}

impl IdentityProviderError {
    /// A [`Self::Config`] error.
    pub fn config(message: impl Into<String>) -> Report<Self> {
        Report::new(Self::Config {
            message: message.into(),
        })
    }

    /// A [`Self::Rejected`] error.
    pub fn rejected(status: StatusCode) -> Report<Self> {
        Report::new(Self::Rejected { status })
    }

    /// A [`Self::Malformed`] error.
    pub fn malformed() -> Report<Self> {
        Report::new(Self::Malformed)
    }

    /// The default body of a capability method: a `Config` error with a
    /// fixed marker message, so the harness can tell a missing override from
    /// a real failure.
    pub fn not_implemented(method: &'static str) -> Report<Self> {
        Self::config(format!("{NOT_IMPLEMENTED_MARKER}{method}"))
    }

    /// Whether this is the [`Self::not_implemented`] marker.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn is_not_implemented(&self) -> bool {
        matches!(self, Self::Config { message } if message.starts_with(NOT_IMPLEMENTED_MARKER))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_implemented_marker_is_recognised() {
        let marker = IdentityProviderError::not_implemented("capture");
        assert!(
            marker.current_context().is_not_implemented(),
            "should recognise the marker"
        );
        let other = IdentityProviderError::config("bad partner id");
        assert!(
            !other.current_context().is_not_implemented(),
            "an ordinary Config error is not the marker"
        );
    }
}
