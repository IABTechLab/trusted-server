//! A wrapper type that redacts sensitive values in [`Debug`] and [`fmt::Display`] output,
//! and the marking that hides sensitive settings from the configuration view.
//!
//! Use [`Redacted`] for secrets, passwords, API keys, and other sensitive values
//! that must never appear in logs or error messages.
//!
//! A settings field holding a value that is sensitive but is not a
//! [`Redacted`] carries `#[serde(serialize_with = "crate::redacted::sensitive")]`.

use core::cell::Cell;
use core::fmt;

use serde::ser::SerializeMap as _;
use serde::{Deserialize, Serialize, Serializer};

/// The key a sensitive value is wrapped under while [`to_marked_value`]
/// runs. It starts with a control character, so no setting can share it.
pub const SENSITIVE_MARKER: &str = "\u{1}sensitive";

std::thread_local! {
    static MARKING: Cell<bool> = const { Cell::new(false) };
}

/// Restores the marking flag when a marked serialization ends, including
/// by a panic.
struct MarkingGuard {
    previous: bool,
}

impl MarkingGuard {
    fn begin() -> Self {
        Self {
            previous: MARKING.with(|marking| marking.replace(true)),
        }
    }
}

impl Drop for MarkingGuard {
    fn drop(&mut self) {
        MARKING.with(|marking| marking.set(self.previous));
    }
}

/// Serializes `value` to JSON with every sensitive value wrapped as
/// `{SENSITIVE_MARKER: value}`.
///
/// Only this call marks. Every other serialization, the configuration push
/// included, writes sensitive values as they are.
///
/// # Errors
///
/// When `value` cannot be represented as JSON.
pub fn to_marked_value<T>(value: &T) -> Result<serde_json::Value, serde_json::Error>
where
    T: Serialize + ?Sized,
{
    let _guard = MarkingGuard::begin();
    serde_json::to_value(value)
}

/// Whether a [`to_marked_value`] call is running on this thread.
#[must_use]
pub fn is_marking() -> bool {
    MARKING.with(Cell::get)
}

/// Serializes a sensitive settings field, for
/// `#[serde(serialize_with = "crate::redacted::sensitive")]`, marked as
/// [`to_marked_value`] says.
///
/// # Errors
///
/// As the serializer's own.
pub fn sensitive<T, S>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    T: Serialize + ?Sized,
    S: Serializer,
{
    if is_marking() {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry(SENSITIVE_MARKER, value)?;
        map.end()
    } else {
        value.serialize(serializer)
    }
}

/// Wraps a value so that [`Debug`] and [`fmt::Display`] print `[REDACTED]`
/// instead of the inner contents.
///
/// Access the real value via [`expose`](Redacted::expose). Callers must
/// never log or display the returned reference.
///
/// Serializes as the inner value, marked as [`to_marked_value`] says.
///
/// # Examples
///
/// ```
/// use trusted_server_core::redacted::Redacted;
///
/// let secret = Redacted::new("my-secret-key".to_string());
/// assert_eq!(format!("{:?}", secret), "[REDACTED]");
/// assert_eq!(secret.expose(), "my-secret-key");
/// ```
#[derive(Clone, Deserialize)]
#[serde(transparent)]
pub struct Redacted<T>(T);

impl<T: Serialize> Serialize for Redacted<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        sensitive(&self.0, serializer)
    }
}

impl<T> Redacted<T> {
    /// Creates a new [`Redacted`] value.
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
        let secret = Redacted::new("super-secret".to_owned());
        assert_eq!(
            format!("{secret:?}"),
            "[REDACTED]",
            "should print [REDACTED] in debug output"
        );
    }

    #[test]
    fn display_output_is_redacted() {
        let secret = Redacted::new("super-secret".to_owned());
        assert_eq!(
            format!("{secret}"),
            "[REDACTED]",
            "should print [REDACTED] in display output"
        );
    }

    #[test]
    fn expose_returns_inner_value() {
        let secret = Redacted::new("super-secret".to_owned());
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
        let secret = Redacted::from("my-key".to_owned());
        assert_eq!(secret.expose(), "my-key", "should create from String");
    }

    #[test]
    fn clone_preserves_inner_value() {
        let secret = Redacted::new("cloneable".to_owned());
        let cloned = secret.clone();
        assert_eq!(
            cloned.expose(),
            "cloneable",
            "should preserve value after clone"
        );
    }

    #[test]
    fn serde_roundtrip() {
        let secret = Redacted::new("serialize-me".to_owned());
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
    fn serialization_is_transparent_outside_a_marked_serialization() {
        let secret = Redacted::new("serialize-me".to_owned());

        let value = serde_json::to_value(&secret).expect("should serialize");

        assert_eq!(value, serde_json::json!("serialize-me"));
        assert!(!is_marking(), "should not be marking outside the call");
    }

    #[test]
    fn a_marked_serialization_wraps_redacted_and_sensitive_values() {
        #[derive(Serialize)]
        struct Fixture {
            plain: String,
            #[serde(serialize_with = "sensitive")]
            marked: String,
            secret: Redacted<String>,
        }
        let fixture = Fixture {
            plain: "open".to_owned(),
            marked: "hidden".to_owned(),
            secret: Redacted::new("secret".to_owned()),
        };

        let marked = to_marked_value(&fixture).expect("should serialize");
        let normal = serde_json::to_value(&fixture).expect("should serialize");

        assert_eq!(
            marked,
            serde_json::json!({
                "plain": "open",
                "marked": { SENSITIVE_MARKER: "hidden" },
                "secret": { SENSITIVE_MARKER: "secret" },
            }),
            "should wrap only the marked values"
        );
        assert_eq!(
            normal,
            serde_json::json!({
                "plain": "open",
                "marked": "hidden",
                "secret": "secret",
            }),
            "should leave an ordinary serialization unchanged"
        );
        assert!(!is_marking(), "should clear the flag when the call ends");
    }

    #[test]
    fn struct_with_redacted_field_debug() {
        #[derive(Debug)]
        #[allow(
            dead_code,
            reason = "test fixture fields are read only through derived Debug output"
        )]
        struct Config {
            name: String,
            api_key: Redacted<String>,
        }

        let config = Config {
            name: "test".to_owned(),
            api_key: Redacted::new("secret-key-123".to_owned()),
        };

        let debug = format!("{config:?}");
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
