//! Single-blob config-store payloads for Trusted Server settings.
//!
//! The `ts` CLI validates [`Settings`] and serializes them into one `EdgeZero`
//! [`BlobEnvelope`] value. Runtime loading verifies that envelope and
//! deserializes the contained settings data, so push-time and runtime semantics
//! cannot drift.

use edgezero_core::blob_envelope::BlobEnvelope;
use error_stack::{Report, ResultExt};

use crate::error::TrustedServerError;
use crate::settings::Settings;

/// Default config-store key containing the Trusted Server app-config blob.
pub const CONFIG_BLOB_KEY: &str = "app_config";

/// Trusted Server config payload ready for config-store publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPayload {
    /// Serialized [`BlobEnvelope`] JSON containing the full [`Settings`] data.
    pub envelope_json: String,
    /// `sha256:<hex>` over the envelope's canonical `data` value.
    pub hash: String,
}

/// Build a single config-store blob payload from validated settings.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when settings cannot be
/// serialized into an `EdgeZero` blob envelope.
pub fn build_config_payload(
    settings: &Settings,
) -> Result<ConfigPayload, Report<TrustedServerError>> {
    let data =
        serde_json::to_value(settings).change_context(TrustedServerError::Configuration {
            message: "failed to serialize settings to JSON".to_string(),
        })?;
    let envelope = BlobEnvelope::new(data, generated_at_rfc3339());
    let hash = format!("sha256:{}", envelope.sha256);
    let envelope_json =
        serde_json::to_string(&envelope).change_context(TrustedServerError::Configuration {
            message: "failed to serialize config blob envelope".to_string(),
        })?;

    Ok(ConfigPayload {
        envelope_json,
        hash,
    })
}

/// Reconstruct validated [`Settings`] from a serialized config blob envelope.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] when the envelope cannot be
/// parsed, fails integrity verification, or contains invalid settings data.
pub fn settings_from_config_blob(
    envelope_json: &str,
) -> Result<Settings, Report<TrustedServerError>> {
    let envelope: BlobEnvelope =
        serde_json::from_str(envelope_json).change_context(TrustedServerError::Configuration {
            message: "failed to parse Trusted Server app-config blob envelope".to_string(),
        })?;
    envelope.verify().map_err(|error| {
        Report::new(TrustedServerError::Configuration {
            message: "Trusted Server app-config blob failed integrity verification".to_string(),
        })
        .attach(error.to_string())
    })?;

    let settings = Settings::from_json_value(envelope.into_data())?;
    settings.reject_placeholder_secrets()?;
    Ok(settings)
}

fn generated_at_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redacted::Redacted;
    use crate::test_support::tests::crate_test_settings_str;

    fn test_settings() -> Settings {
        Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings")
    }

    #[test]
    fn builds_single_blob_payload() {
        let payload = build_config_payload(&test_settings()).expect("should build payload");
        let envelope: BlobEnvelope =
            serde_json::from_str(&payload.envelope_json).expect("should parse envelope");

        envelope.verify().expect("should verify envelope");
        assert_eq!(
            payload.hash,
            format!("sha256:{}", envelope.sha256),
            "payload hash should mirror envelope data hash"
        );
    }

    #[test]
    fn payload_round_trips_through_blob_envelope() {
        let original = test_settings();
        let payload = build_config_payload(&original).expect("should build payload");
        let reconstructed =
            settings_from_config_blob(&payload.envelope_json).expect("should reconstruct settings");

        assert_eq!(
            reconstructed.publisher.domain, original.publisher.domain,
            "should preserve publisher domain"
        );
        assert_eq!(
            reconstructed.ec.pull_sync_concurrency, original.ec.pull_sync_concurrency,
            "should preserve numeric fields"
        );
        assert_eq!(
            reconstructed.handlers.len(),
            original.handlers.len(),
            "should preserve arrays"
        );
    }

    #[test]
    fn strings_that_look_like_json_scalars_round_trip_as_strings() {
        let mut original = test_settings();
        original.publisher.proxy_secret = Redacted::new("1234567890".to_string());
        original.ec.passphrase = Redacted::new("12345678901234567890123456789012".to_string());
        original.handlers[0].password = Redacted::new("true".to_string());

        let payload = build_config_payload(&original).expect("should build payload");
        let reconstructed =
            settings_from_config_blob(&payload.envelope_json).expect("should reconstruct settings");

        assert_eq!(
            reconstructed.publisher.proxy_secret.expose(),
            original.publisher.proxy_secret.expose(),
            "numeric-looking proxy secret should remain a string"
        );
        assert_eq!(
            reconstructed.ec.passphrase.expose(),
            original.ec.passphrase.expose(),
            "numeric-looking passphrase should remain a string"
        );
        assert_eq!(
            reconstructed.handlers[0].password.expose(),
            original.handlers[0].password.expose(),
            "boolean-looking handler password should remain a string"
        );
    }

    #[test]
    fn hash_is_stable_for_equivalent_toml_ordering() {
        let first = r#"
[[handlers]]
path = "^/_ts/admin"
username = "admin"
password = "production-admin-password-32-bytes"

[publisher]
domain = "example.com"
cookie_domain = ".example.com"
origin_url = "https://origin.example.com"
proxy_secret = "unit-test-proxy-secret"

[ec]
passphrase = "test-secret-key-32-bytes-minimum"
pull_sync_concurrency = 5
"#;
        let second = r#"
[ec]
pull_sync_concurrency = 5
passphrase = "test-secret-key-32-bytes-minimum"

[publisher]
proxy_secret = "unit-test-proxy-secret"
origin_url = "https://origin.example.com"
cookie_domain = ".example.com"
domain = "example.com"

[[handlers]]
password = "production-admin-password-32-bytes"
username = "admin"
path = "^/_ts/admin"
"#;
        let first_settings = Settings::from_toml(first).expect("should parse first settings");
        let second_settings = Settings::from_toml(second).expect("should parse second settings");
        let first_payload = build_config_payload(&first_settings).expect("should build first");
        let second_payload = build_config_payload(&second_settings).expect("should build second");

        assert_eq!(first_payload.hash, second_payload.hash);
    }

    #[test]
    fn tampered_blob_hash_is_rejected() {
        let payload = build_config_payload(&test_settings()).expect("should build payload");
        let mut envelope: BlobEnvelope =
            serde_json::from_str(&payload.envelope_json).expect("should parse envelope");
        envelope.sha256 = "ff".repeat(32);
        let tampered =
            serde_json::to_string(&envelope).expect("should serialize tampered envelope");

        let err = settings_from_config_blob(&tampered).expect_err("should reject hash mismatch");

        assert!(
            err.to_string().contains("integrity verification"),
            "error should mention integrity verification"
        );
    }
}
