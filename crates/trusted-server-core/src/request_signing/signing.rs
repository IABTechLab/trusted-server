//! Request signing and verification utilities.
//!
//! This module provides Ed25519-based signing and verification of HTTP requests
//! using keys stored via platform store primitives.

use base64::{engine::general_purpose, Engine};
use ed25519_dalek::{Signature, Signer as Ed25519Signer, SigningKey, Verifier, VerifyingKey};
use error_stack::{Report, ResultExt};
use serde::Serialize;

use crate::error::TrustedServerError;
use crate::platform::RuntimeServices;
use crate::request_signing::{JWKS_STORE_NAME, SIGNING_STORE_NAME};

/// Retrieves the current active key ID from the config store.
///
/// # Errors
///
/// Returns an error if the config store cannot be accessed or the current-kid key is not found.
pub fn get_current_key_id(
    services: &RuntimeServices,
) -> Result<String, Report<TrustedServerError>> {
    services
        .config_store()
        .get(&JWKS_STORE_NAME, "current-kid")
        .change_context(TrustedServerError::Configuration {
            message: "failed to read current-kid from config store".into(),
        })
}

/// Parses an Ed25519 signing key from secret-store bytes.
///
/// Request-signing rotation always stores private keys as standard base64 text
/// via [`crate::request_signing::rotation::KeyRotationManager`]. A non-base64
/// value in the secret store indicates data corruption and is surfaced as an
/// explicit error rather than silently falling back to a length heuristic.
fn parse_ed25519_signing_key(key_bytes: &[u8]) -> Result<SigningKey, Report<TrustedServerError>> {
    let bytes = general_purpose::STANDARD.decode(key_bytes).map_err(|_| {
        Report::new(TrustedServerError::Configuration {
            message: "signing key is not valid base64 — corrupt key material in secret store"
                .into(),
        })
    })?;

    let key_array: [u8; 32] = bytes.try_into().map_err(|_| {
        Report::new(TrustedServerError::Configuration {
            message: "signing key must be 32 bytes after base64 decoding".into(),
        })
    })?;

    Ok(SigningKey::from_bytes(&key_array))
}

/// Signs request payloads using the current Ed25519 private key.
pub struct RequestSigner {
    key: SigningKey,
    /// Key identifier associated with the loaded private key.
    pub kid: String,
}

/// Current version of the signing protocol
pub const SIGNING_VERSION: &str = "1.1";

/// Canonical payload structure for request signing.
///
/// Serialized as JSON to prevent signature confusion attacks that could
/// exploit delimiter-based formats.
#[derive(Serialize)]
struct SigningPayload<'a> {
    version: &'a str,
    kid: &'a str,
    host: &'a str,
    scheme: &'a str,
    id: &'a str,
    ts: u64,
}

/// Parameters for enhanced request signing
#[derive(Debug, Clone)]
pub struct SigningParams {
    /// Request identifier to bind into the signature payload.
    pub request_id: String,
    /// Host header value expected by the receiving service.
    pub request_host: String,
    /// Request scheme bound into the signature payload.
    pub request_scheme: String,
    /// Signature timestamp in Unix milliseconds.
    pub timestamp: u64,
}

impl SigningParams {
    /// Creates a new `SigningParams` with the current timestamp in milliseconds
    #[must_use]
    pub fn new(request_id: String, request_host: String, request_scheme: String) -> Self {
        Self {
            request_id,
            request_host,
            request_scheme,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }

    /// Builds the canonical payload string for signing.
    ///
    /// The payload is JSON-serialized to prevent signature confusion attacks
    /// that could exploit delimiter-based formats.
    ///
    /// # Errors
    ///
    /// Returns an error if the payload cannot be serialized to JSON.
    pub fn build_payload(&self, kid: &str) -> Result<String, Report<TrustedServerError>> {
        let payload = SigningPayload {
            version: SIGNING_VERSION,
            kid,
            host: &self.request_host,
            scheme: &self.request_scheme,
            id: &self.request_id,
            ts: self.timestamp,
        };
        serde_json::to_string(&payload).map_err(|e| {
            Report::new(TrustedServerError::Configuration {
                message: format!("Failed to serialize signing payload: {}", e),
            })
        })
    }
}

impl RequestSigner {
    /// Creates a `RequestSigner` from the current key ID stored in platform stores.
    ///
    /// # Errors
    ///
    /// Returns an error if the key ID cannot be retrieved or the key cannot be parsed.
    pub fn from_services(services: &RuntimeServices) -> Result<Self, Report<TrustedServerError>> {
        let key_id =
            get_current_key_id(services).change_context(TrustedServerError::Configuration {
                message: "failed to get current-kid".into(),
            })?;

        let key_bytes = services
            .secret_store()
            .get_bytes(&SIGNING_STORE_NAME, &key_id)
            .change_context(TrustedServerError::Configuration {
                message: format!("failed to get signing key for kid: {}", key_id),
            })?;

        let signing_key = parse_ed25519_signing_key(&key_bytes)?;

        Ok(Self {
            key: signing_key,
            kid: key_id,
        })
    }

    /// Signs a payload using the Ed25519 signing key.
    ///
    /// # Errors
    ///
    /// Returns an error if signing fails.
    pub fn sign(&self, payload: &[u8]) -> Result<String, Report<TrustedServerError>> {
        let signature_bytes = self.key.sign(payload).to_bytes();

        Ok(general_purpose::URL_SAFE_NO_PAD.encode(signature_bytes))
    }

    /// Signs a request using the enhanced v1.1 signing protocol.
    ///
    /// The signed payload is a JSON object containing version, kid, host,
    /// scheme, id, and ts fields.
    ///
    /// # Errors
    ///
    /// Returns an error if signing fails.
    pub fn sign_request(
        &self,
        params: &SigningParams,
    ) -> Result<String, Report<TrustedServerError>> {
        let payload = params.build_payload(&self.kid)?;
        self.sign(payload.as_bytes())
    }
}

/// Verifies a signature using the public key associated with the given key ID.
///
/// # Errors
///
/// Returns an error if the JWK cannot be retrieved, parsed, or if signature verification fails.
pub fn verify_signature(
    payload: &[u8],
    signature_b64: &str,
    kid: &str,
    services: &RuntimeServices,
) -> Result<bool, Report<TrustedServerError>> {
    let jwk_json = services
        .config_store()
        .get(&JWKS_STORE_NAME, kid)
        .change_context(TrustedServerError::Configuration {
            message: format!("failed to get JWK for kid: {}", kid),
        })?;

    let jwk: serde_json::Value = serde_json::from_str(&jwk_json).map_err(|e| {
        Report::new(TrustedServerError::Configuration {
            message: format!("Failed to parse JWK: {}", e),
        })
    })?;

    let x_b64 = jwk.get("x").and_then(|v| v.as_str()).ok_or_else(|| {
        Report::new(TrustedServerError::Configuration {
            message: "JWK missing 'x' parameter".into(),
        })
    })?;

    let public_key_bytes = general_purpose::URL_SAFE_NO_PAD
        .decode(x_b64)
        .map_err(|e| {
            Report::new(TrustedServerError::Configuration {
                message: format!("Failed to decode public key: {}", e),
            })
        })?;

    let verifying_key_bytes: [u8; 32] = public_key_bytes.try_into().map_err(|_| {
        Report::new(TrustedServerError::Configuration {
            message: "Public key must be 32 bytes".into(),
        })
    })?;

    let verifying_key = VerifyingKey::from_bytes(&verifying_key_bytes).map_err(|e| {
        Report::new(TrustedServerError::Configuration {
            message: format!("Failed to create verifying key: {}", e),
        })
    })?;

    let signature_bytes = general_purpose::URL_SAFE_NO_PAD
        .decode(signature_b64)
        .or_else(|_| general_purpose::STANDARD.decode(signature_b64))
        .map_err(|e| {
            Report::new(TrustedServerError::Configuration {
                message: format!("Failed to decode signature: {}", e),
            })
        })?;

    let signature_array: [u8; 64] = signature_bytes.try_into().map_err(|_| {
        Report::new(TrustedServerError::Configuration {
            message: "Signature must be 64 bytes".into(),
        })
    })?;

    let signature = Signature::from_bytes(&signature_array);

    Ok(verifying_key.verify(payload, &signature).is_ok())
}

#[cfg(test)]
mod tests {
    use crate::platform::test_support::build_request_signing_services;

    use super::*;

    #[test]
    fn from_services_loads_kid_from_config_store() {
        let services = build_request_signing_services();
        let signer =
            RequestSigner::from_services(&services).expect("should create signer from services");

        assert_eq!(signer.kid, "test-kid", "should load kid from config store");
    }

    #[test]
    fn sign_produces_non_empty_url_safe_base64_signature() {
        let services = build_request_signing_services();
        let signer =
            RequestSigner::from_services(&services).expect("should create signer from services");

        let signature = signer
            .sign(b"these pretzels are making me thirsty")
            .expect("should sign payload");

        assert!(!signature.is_empty(), "should produce non-empty signature");
        assert!(
            signature.len() > 32,
            "should produce a full-length signature"
        );
    }

    #[test]
    fn sign_and_verify_roundtrip_succeeds() {
        let services = build_request_signing_services();
        let signer =
            RequestSigner::from_services(&services).expect("should create signer from services");
        let payload = b"test payload for verification";

        let signature = signer.sign(payload).expect("should sign payload");
        let verified = verify_signature(payload, &signature, &signer.kid, &services)
            .expect("should attempt verification");

        assert!(verified, "should verify a valid signature");
    }

    #[test]
    fn verify_returns_false_for_wrong_payload() {
        let services = build_request_signing_services();
        let signer =
            RequestSigner::from_services(&services).expect("should create signer from services");
        let signature = signer.sign(b"original").expect("should sign");

        let verified = verify_signature(b"wrong payload", &signature, &signer.kid, &services)
            .expect("should attempt verification");

        assert!(!verified, "should not verify signature for wrong payload");
    }

    #[test]
    fn verify_errors_for_unknown_kid() {
        let services = build_request_signing_services();
        let signer =
            RequestSigner::from_services(&services).expect("should create signer from services");
        let signature = signer.sign(b"payload").expect("should sign");

        let result = verify_signature(b"payload", &signature, "nonexistent-kid", &services);

        assert!(result.is_err(), "should error for unknown kid");
    }

    #[test]
    fn verify_errors_for_malformed_signature() {
        let services = build_request_signing_services();
        let signer =
            RequestSigner::from_services(&services).expect("should create signer from services");

        let result = verify_signature(b"payload", "not-valid-base64!!!", &signer.kid, &services);

        assert!(result.is_err(), "should error for malformed signature");
    }

    #[test]
    fn signing_params_build_payload_serializes_all_fields() {
        let params = SigningParams {
            request_id: "req-123".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            timestamp: 1706900000,
        };

        let payload = params
            .build_payload("kid-abc")
            .expect("should build payload");
        let parsed: serde_json::Value =
            serde_json::from_str(&payload).expect("should be valid JSON");

        assert_eq!(parsed["version"], SIGNING_VERSION);
        assert_eq!(parsed["kid"], "kid-abc");
        assert_eq!(parsed["host"], "example.com");
        assert_eq!(parsed["scheme"], "https");
        assert_eq!(parsed["id"], "req-123");
        assert_eq!(parsed["ts"], 1706900000);
    }

    #[test]
    fn signing_params_new_creates_recent_timestamp() {
        let params = SigningParams::new(
            "req-123".to_string(),
            "example.com".to_string(),
            "https".to_string(),
        );

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("should get system time")
            .as_millis() as u64;

        assert!(
            params.timestamp <= now_ms,
            "timestamp should not be in the future"
        );
        assert!(
            params.timestamp >= now_ms - 60_000,
            "timestamp should be within the last minute"
        );
    }

    #[test]
    fn sign_request_enhanced_produces_verifiable_signature() {
        let services = build_request_signing_services();
        let signer =
            RequestSigner::from_services(&services).expect("should create signer from services");
        let params = SigningParams::new(
            "auction-123".to_string(),
            "publisher.com".to_string(),
            "https".to_string(),
        );

        let signature = signer.sign_request(&params).expect("should sign request");
        let payload = params
            .build_payload(&signer.kid)
            .expect("should build payload");

        let verified = verify_signature(payload.as_bytes(), &signature, &signer.kid, &services)
            .expect("should verify");

        assert!(verified, "enhanced request signature should be verifiable");
    }

    #[test]
    fn sign_request_different_hosts_produce_different_signatures() {
        let services = build_request_signing_services();
        let signer =
            RequestSigner::from_services(&services).expect("should create signer from services");

        let params1 = SigningParams {
            request_id: "req-1".to_string(),
            request_host: "host1.com".to_string(),
            request_scheme: "https".to_string(),
            timestamp: 1706900000,
        };
        let params2 = SigningParams {
            request_id: "req-1".to_string(),
            request_host: "host2.com".to_string(),
            request_scheme: "https".to_string(),
            timestamp: 1706900000,
        };

        let sig1 = signer.sign_request(&params1).expect("should sign params1");
        let sig2 = signer.sign_request(&params2).expect("should sign params2");

        assert_ne!(
            sig1, sig2,
            "different hosts should produce different signatures"
        );
    }
}
