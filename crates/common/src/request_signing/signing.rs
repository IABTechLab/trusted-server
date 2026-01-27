//! Request signing and verification utilities.
//!
//! This module provides Ed25519-based signing and verification of HTTP requests
//! using keys stored in Fastly Config and Secret stores.

use base64::{engine::general_purpose, Engine};
use ed25519_dalek::{Signature, Signer as Ed25519Signer, SigningKey, Verifier, VerifyingKey};
use error_stack::{Report, ResultExt};

use crate::error::TrustedServerError;
use crate::fastly_storage::{FastlyConfigStore, FastlySecretStore};

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

impl RequestSigner {
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

    pub fn sign(&self, payload: &[u8]) -> Result<String, Report<TrustedServerError>> {
        let signature_bytes = self.key.sign(payload).to_bytes();

        Ok(general_purpose::URL_SAFE_NO_PAD.encode(signature_bytes))
    }
}

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
        let signer = RequestSigner::from_config().unwrap();
        let signature = signer
            .sign(b"these pretzels are making me thirsty")
            .unwrap();
        assert!(!signature.is_empty());
        assert!(signature.len() > 32);
    }

    #[test]
    fn test_request_signer_from_config() {
        let signer = RequestSigner::from_config().unwrap();
        assert!(!signer.kid.is_empty());
    }

    #[test]
    fn test_sign_and_verify() {
        let payload = b"test payload for verification";
        let signer = RequestSigner::from_config().unwrap();
        let signature = signer.sign(payload).unwrap();

        let result = verify_signature(payload, &signature, &signer.kid).unwrap();
        assert!(result, "Signature should be valid");
    }

    #[test]
    fn test_verify_invalid_signature() {
        let payload = b"test payload";
        let signer = RequestSigner::from_config().unwrap();

        let wrong_signature = signer.sign(b"different payload").unwrap();

        let result = verify_signature(payload, &wrong_signature, &signer.kid).unwrap();
        assert!(!result, "Invalid signature should not verify");
    }

    #[test]
    fn test_verify_wrong_payload() {
        let original_payload = b"original payload";
        let signer = RequestSigner::from_config().unwrap();
        let signature = signer.sign(original_payload).unwrap();

        let wrong_payload = b"wrong payload";
        let result = verify_signature(wrong_payload, &signature, &signer.kid).unwrap();
        assert!(!result, "Signature should not verify with wrong payload");
    }

    #[test]
    fn test_verify_missing_key() {
        let payload = b"test payload";
        let signer = RequestSigner::from_config().unwrap();
        let signature = signer.sign(payload).unwrap();
        let nonexistent_kid = "nonexistent-key-id";

        let result = verify_signature(payload, &signature, nonexistent_kid);
        assert!(result.is_err(), "Should error for missing key");
    }

    #[test]
    fn test_verify_malformed_signature() {
        let payload = b"test payload";
        let signer = RequestSigner::from_config().unwrap();
        let malformed_signature = "not-valid-base64!!!";

        let result = verify_signature(payload, malformed_signature, &signer.kid);
        assert!(result.is_err(), "Should error for malformed signature");
    }
}
