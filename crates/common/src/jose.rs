use base64::{engine::general_purpose, Engine};
use ed25519_dalek::{Signer as Ed25519Signer, SigningKey};
use fastly::{ConfigStore, SecretStore};
use std::sync::OnceLock;

use crate::error::TrustedServerError;

// Hard coding for now use Fastly KV later
static SIGNING_KEY: OnceLock<SigningKey> = OnceLock::new();

pub fn set_signing_key(bytes: &[u8]) -> Result<(), TrustedServerError> {
    let bytes = bytes
        .try_into()
        .map_err(|e| TrustedServerError::Configuration {
            message: format!("Could not set signing key: {:?}", e),
        })?;

    SIGNING_KEY
        .set(SigningKey::from_bytes(bytes))
        .map_err(|e| TrustedServerError::Configuration {
            message: format!("Could not set signing key: {:?}", e),
        })
}

pub fn sign(payload: &[u8]) -> Result<String, TrustedServerError> {
    let signing_key = match SIGNING_KEY.get() {
        Some(key) => key,
        None => {
            return Err(TrustedServerError::Configuration {
                message: "Signing key not set".into(),
            });
        }
    };

    let signature_bytes = signing_key.sign(payload).to_bytes();

    Ok(general_purpose::URL_SAFE_NO_PAD.encode(signature_bytes))
}

pub fn get_current_key_id() -> Result<String, TrustedServerError> {
    let store = ConfigStore::open("jwks_store");
    
    store.get("current-kid")
        .ok_or_else(|| TrustedServerError::Configuration {
            message: "current-kid not found in config store".into(),
        })
}

pub fn get_signing_key_from_fastly() -> Result<Vec<u8>, TrustedServerError> {
    let key_id = get_current_key_id()?;
    
    let store = SecretStore::open("signing_keys")
        .map_err(|_| TrustedServerError::Configuration {
            message: "Failed to open signing_keys SecretStore".into(),
        })?;
    
    let pk = store.get(&key_id)
        .ok_or_else(|| TrustedServerError::Configuration {
            message: format!("Private key '{}' not found in secret store", key_id),
        })?
        .try_plaintext()
        .map_err(|_| TrustedServerError::Configuration {
            message: "Failed to get private key plaintext".into(),
        })?;
    
    general_purpose::STANDARD.decode(pk)
        .map_err(|_| TrustedServerError::Configuration {
            message: "Failed to decode base64 private key".into(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_test_key() {
        let key = get_signing_key_from_fastly().unwrap();

        set_signing_key(&key)
        .expect("signing key should not be initialized");
    }

    #[test]
    fn test_sign() {
        set_test_key();

        let signature = sign(b"these pretzles are making me thirsty").unwrap();

        assert_eq!(signature, "oqxiXJub6osQsBNhius0Ho8G1tR6wepnFKbxHDnKjViuBXz9xl6Zp1T0CMuwI11U58aiRiR690HZFGw9_j3fBg");
    }

    #[test]
    fn test_get_signing_key_from_fastly() {
        let key = get_signing_key_from_fastly().unwrap();

        assert_eq!(key.len(), 32);
    }
}
