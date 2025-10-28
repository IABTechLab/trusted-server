use base64::{engine::general_purpose, Engine};
use ed25519_dalek::{Signer as Ed25519Signer, SigningKey};
use fastly::{ConfigStore, SecretStore};

use crate::error::TrustedServerError;

pub fn sign(payload: &[u8]) -> Result<String, TrustedServerError> {
    let signing_key = get_signing_key_from_fastly()?;
    let signature_bytes = signing_key.sign(payload).to_bytes();

    Ok(general_purpose::URL_SAFE_NO_PAD.encode(signature_bytes))
}

pub fn get_current_key_id() -> Result<String, TrustedServerError> {
    let store = ConfigStore::open("jwks_store");

    store
        .get("current-kid")
        .ok_or_else(|| TrustedServerError::Configuration {
            message: "current-kid not found in config store".into(),
        })
}

fn get_signing_key_from_fastly() -> Result<SigningKey, TrustedServerError> {
    let key_id = get_current_key_id()?;

    let store =
        SecretStore::open("signing_keys").map_err(|_| TrustedServerError::Configuration {
            message: "Failed to open signing_keys SecretStore".into(),
        })?;

    let pk = store
        .get(&key_id)
        .ok_or_else(|| TrustedServerError::Configuration {
            message: format!("Private key '{}' not found in secret store", key_id),
        })?
        .try_plaintext()
        .map_err(|_| TrustedServerError::Configuration {
            message: "Failed to get private key plaintext".into(),
        })?;

    // decode base64 if that's what we got
    let bytes = if pk.len() > 32 {
        general_purpose::STANDARD
            .decode(pk)
            .map_err(|_| TrustedServerError::Configuration {
                message: "Failed to decode base64 key".into(),
            })?
    } else {
        pk.into_iter().collect()
    };

    let signing_key = SigningKey::from_bytes(&bytes.try_into().map_err(|_| {
        TrustedServerError::Configuration {
            message: "failed to create signing key".into(),
        }
    })?);

    Ok(signing_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign() {
        let signature = sign(b"these pretzles are making me thirsty").unwrap();

        assert_eq!(signature, "oqxiXJub6osQsBNhius0Ho8G1tR6wepnFKbxHDnKjViuBXz9xl6Zp1T0CMuwI11U58aiRiR690HZFGw9_j3fBg");
    }

    #[test]
    fn test_get_or_init_signing_key() {
        assert!(SIGNING_KEY.get().is_none());

        let key1 = get_or_init_signing_key().unwrap();

        assert!(SIGNING_KEY.get().is_some());

        let key2 = get_or_init_signing_key().unwrap();

        assert_eq!(key1, key2);
    }

    #[test]
    fn test_get_signing_key_from_fastly() {
        let key = get_signing_key_from_fastly().unwrap();

        assert_eq!(key.len(), 32);
    }
}
