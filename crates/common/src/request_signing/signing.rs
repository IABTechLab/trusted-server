//! Request signing and verification utilities.
//!
//! This module provides Ed25519-based signing and verification of HTTP requests
//! using keys stored in Fastly Config and Secret stores.

use base64::{engine::general_purpose, Engine};
use ed25519_dalek::{Signature, Signer as Ed25519Signer, SigningKey, Verifier, VerifyingKey};
use error_stack::{Report, ResultExt};

use crate::error::TrustedServerError;
use crate::fastly_storage::{FastlyConfigStore, FastlySecretStore};

/// Retrieves the current active key ID from the config store.
///
/// # Errors
///
/// Returns an error if the config store cannot be accessed or the current-kid key is not found.
pub fn get_current_key_id() -> Result<String, Report<TrustedServerError>> {
    let store = FastlyConfigStore::new("jwks_store");
    store.get("current-kid")
}

fn parse_ed25519_signing_key(key_bytes: Vec<u8>) -> Result<SigningKey, Report<TrustedServerError>> {
    let bytes = if key_bytes.len() > 32 {
        general_purpose::STANDARD.decode(&key_bytes).map_err(|_| {
            Report::new(TrustedServerError::Configuration {
                message: "Failed to decode base64 key".into(),
            })
        })?
    } else {
        key_bytes
    };

    let key_array: [u8; 32] = bytes.try_into().map_err(|_| {
        Report::new(TrustedServerError::Configuration {
            message: "Invalid key length (expected 32 bytes for Ed25519)".into(),
        })
    })?;

    Ok(SigningKey::from_bytes(&key_array))
}

pub struct RequestSigner {
    key: SigningKey,
    pub kid: String,
}

/// Current version of the signing protocol
pub const SIGNING_VERSION: &str = "1.1";

/// Parameters for enhanced request signing
#[derive(Debug, Clone)]
pub struct SigningParams {
    pub request_id: String,
    pub request_host: String,
    pub request_scheme: String,
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
    /// Format: `kid:request_host:request_scheme:id:ts`
    #[must_use]
    pub fn build_payload(&self, kid: &str) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            kid, self.request_host, self.request_scheme, self.request_id, self.timestamp
        )
    }
}

impl RequestSigner {
    /// Creates a `RequestSigner` from the current key ID stored in config.
    ///
    /// # Errors
    ///
    /// Returns an error if the key ID cannot be retrieved or the key cannot be parsed.
    pub fn from_config() -> Result<Self, Report<TrustedServerError>> {
        let config_store = FastlyConfigStore::new("jwks_store");
        let key_id =
            config_store
                .get("current-kid")
                .change_context(TrustedServerError::Configuration {
                    message: "Failed to get current-kid".into(),
                })?;

        let secret_store = FastlySecretStore::new("signing_keys");
        let key_bytes = secret_store
            .get(&key_id)
            .attach(format!("Failed to get signing key for kid: {}", key_id))?;
        let signing_key = parse_ed25519_signing_key(key_bytes)?;

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
    /// The signed payload format is: `kid:request_host:request_scheme:id:ts`
    ///
    /// # Errors
    ///
    /// Returns an error if signing fails.
    pub fn sign_request(
        &self,
        params: &SigningParams,
    ) -> Result<String, Report<TrustedServerError>> {
        let payload = params.build_payload(&self.kid);
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
) -> Result<bool, Report<TrustedServerError>> {
    let store = FastlyConfigStore::new("jwks_store");
    let jwk_json = store
        .get(kid)
        .change_context(TrustedServerError::Configuration {
            message: format!("Failed to get JWK for kid: {}", kid),
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
    use super::*;

    #[test]
    fn test_request_signer_sign() {
        // Report unwraps print full error chain on test failure
        // Note: unwrapping a Report prints it nicely if test fails.
        let signer = RequestSigner::from_config().expect("should create signer from config");
        let signature = signer
            .sign(b"these pretzels are making me thirsty")
            .expect("should sign payload");
        assert!(!signature.is_empty());
        assert!(signature.len() > 32);
    }

    #[test]
    fn test_request_signer_from_config() {
        let signer = RequestSigner::from_config().expect("should create signer from config");
        assert!(!signer.kid.is_empty());
    }

    #[test]
    fn test_sign_and_verify() {
        let payload = b"test payload for verification";
        let signer = RequestSigner::from_config().expect("should create signer from config");
        let signature = signer.sign(payload).expect("should sign payload");

        let result =
            verify_signature(payload, &signature, &signer.kid).expect("should verify signature");
        assert!(result, "Signature should be valid");
    }

    #[test]
    fn test_verify_invalid_signature() {
        let payload = b"test payload";
        let signer = RequestSigner::from_config().expect("should create signer from config");

        let wrong_signature = signer
            .sign(b"different payload")
            .expect("should sign different payload");

        let result = verify_signature(payload, &wrong_signature, &signer.kid)
            .expect("should attempt verification");
        assert!(!result, "Invalid signature should not verify");
    }

    #[test]
    fn test_verify_wrong_payload() {
        let original_payload = b"original payload";
        let signer = RequestSigner::from_config().expect("should create signer from config");
        let signature = signer
            .sign(original_payload)
            .expect("should sign original payload");

        let wrong_payload = b"wrong payload";
        let result = verify_signature(wrong_payload, &signature, &signer.kid)
            .expect("should attempt verification");
        assert!(!result, "Signature should not verify with wrong payload");
    }

    #[test]
    fn test_verify_missing_key() {
        let payload = b"test payload";
        let signer = RequestSigner::from_config().expect("should create signer from config");
        let signature = signer.sign(payload).expect("should sign payload");
        let nonexistent_kid = "nonexistent-key-id";

        let result = verify_signature(payload, &signature, nonexistent_kid);
        assert!(result.is_err(), "Should error for missing key");
    }

    #[test]
    fn test_verify_malformed_signature() {
        let payload = b"test payload";
        let signer = RequestSigner::from_config().expect("should create signer from config");
        let malformed_signature = "not-valid-base64!!!";

        let result = verify_signature(payload, malformed_signature, &signer.kid);
        assert!(result.is_err(), "Should error for malformed signature");
    }

    #[test]
    fn test_signing_params_build_payload() {
        let params = SigningParams {
            request_id: "req-123".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            timestamp: 1706900000,
        };

        let payload = params.build_payload("kid-abc");
        assert_eq!(payload, "kid-abc:example.com:https:req-123:1706900000");
    }

    #[test]
    fn test_signing_params_new_creates_timestamp() {
        let params = SigningParams::new(
            "req-123".to_string(),
            "example.com".to_string(),
            "https".to_string(),
        );

        assert_eq!(params.request_id, "req-123");
        assert_eq!(params.request_host, "example.com");
        assert_eq!(params.request_scheme, "https");
        // Timestamp should be recent (within last minute), in milliseconds
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!(params.timestamp <= now_ms);
        assert!(params.timestamp >= now_ms - 60_000);
    }

    #[test]
    fn test_sign_request_enhanced() {
        let signer = RequestSigner::from_config().unwrap();
        let params = SigningParams::new(
            "auction-123".to_string(),
            "publisher.com".to_string(),
            "https".to_string(),
        );

        let signature = signer.sign_request(&params).unwrap();
        assert!(!signature.is_empty());

        // Verify the signature is valid by reconstructing the payload
        let payload = params.build_payload(&signer.kid);
        let result = verify_signature(payload.as_bytes(), &signature, &signer.kid).unwrap();
        assert!(result, "Enhanced signature should be valid");
    }

    #[test]
    fn test_sign_request_different_params_different_signature() {
        let signer = RequestSigner::from_config().unwrap();

        let params1 = SigningParams {
            request_id: "req-1".to_string(),
            request_host: "host1.com".to_string(),
            request_scheme: "https".to_string(),
            timestamp: 1706900000,
        };

        let params2 = SigningParams {
            request_id: "req-1".to_string(),
            request_host: "host2.com".to_string(), // Different host
            request_scheme: "https".to_string(),
            timestamp: 1706900000,
        };

        let sig1 = signer.sign_request(&params1).unwrap();
        let sig2 = signer.sign_request(&params2).unwrap();

        assert_ne!(
            sig1, sig2,
            "Different hosts should produce different signatures"
        );
    }

    #[test]
    fn test_signing_version_constant() {
        assert_eq!(SIGNING_VERSION, "1.1");
    }
}
