// NOTE: This file is also included in build.rs via #[path].
// It must remain self-contained (no `crate::` imports).

//! A wrapper type that redacts sensitive values in [`Debug`] and [`Display`] output.
//!
//! Use [`Redacted`] for secrets, passwords, API keys, and other sensitive values
//! that must never appear in logs or error messages.

use core::fmt;

use serde::{Deserialize, Serialize};

/// Wraps a value so that [`Debug`] and [`Display`] print `[REDACTED]`
/// instead of the inner contents.
///
/// Access the real value via [`expose`](Redacted::expose). Callers must
/// never log or display the returned reference.
///
/// # Examples
///
/// ```
/// use trusted_server_common::redacted::Redacted;
///
/// let secret = Redacted::new("my-secret-key".to_string());
/// assert_eq!(format!("{:?}", secret), "[REDACTED]");
/// assert_eq!(secret.expose(), "my-secret-key");
/// ```
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    /// Creates a new [`Redacted`] value.
    #[allow(dead_code)]
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Exposes the inner value for use in operations that need the actual secret.
    ///
    /// Callers should never log or display the returned reference.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T: Default> Default for Redacted<T> {
    fn default() -> Self {
        Self(T::default())
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[REDACTED]")
    }
}

impl From<String> for Redacted<String> {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_is_redacted() {
        let secret = Redacted::new("super-secret".to_string());
        assert_eq!(
            format!("{:?}", secret),
            "[REDACTED]",
            "should print [REDACTED] in debug output"
        );
    }

    #[test]
    fn display_output_is_redacted() {
        let secret = Redacted::new("super-secret".to_string());
        assert_eq!(
            format!("{}", secret),
            "[REDACTED]",
            "should print [REDACTED] in display output"
        );
    }

    #[test]
    fn expose_returns_inner_value() {
        let secret = Redacted::new("super-secret".to_string());
        assert_eq!(
            secret.expose(),
            "super-secret",
            "should return the inner value"
        );
    }

    #[test]
    fn default_creates_empty_redacted() {
        let secret: Redacted<String> = Redacted::default();
        assert_eq!(secret.expose(), "", "should default to empty string");
    }

    #[test]
    fn from_string_creates_redacted() {
        let secret = Redacted::from("my-key".to_string());
        assert_eq!(secret.expose(), "my-key", "should create from String");
    }

    #[test]
    fn clone_preserves_inner_value() {
        let secret = Redacted::new("cloneable".to_string());
        let cloned = secret.clone();
        assert_eq!(
            cloned.expose(),
            "cloneable",
            "should preserve value after clone"
        );
    }

    #[test]
    fn serde_roundtrip() {
        let secret = Redacted::new("serialize-me".to_string());
        let json = serde_json::to_string(&secret).expect("should serialize");
        assert_eq!(json, "\"serialize-me\"", "should serialize transparently");

        let deserialized: Redacted<String> =
            serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(
            deserialized.expose(),
            "serialize-me",
            "should deserialize transparently"
        );
    }

    #[test]
    fn struct_with_redacted_field_debug() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Config {
            name: String,
            api_key: Redacted<String>,
        }

        let config = Config {
            name: "test".to_string(),
            api_key: Redacted::new("secret-key-123".to_string()),
        };

        let debug = format!("{:?}", config);
        assert!(
            debug.contains("[REDACTED]"),
            "should contain [REDACTED] for the api_key field"
        );
        assert!(
            !debug.contains("secret-key-123"),
            "should not contain the actual secret"
        );
    }
}
