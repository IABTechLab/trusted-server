use base64::{engine::general_purpose, Engine};
use ed25519_dalek::{Signer as Ed25519Signer, SigningKey};
use error_stack::{Report, ResultExt};
use fastly::{ConfigStore, Request, Response, SecretStore};

use crate::error::TrustedServerError;
use crate::settings::Settings;

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

/// Gets all active JWK public keys from the config store.
///
/// Returns a JSON string containing the JWKS (JSON Web Key Set) with all
/// currently active public keys. The active keys are determined by the
/// "active-kids" config store entry which contains a comma-separated list.
pub fn get_active_jwks() -> Result<String, TrustedServerError> {
    let store = ConfigStore::open("jwks_store");

    // Get the comma-separated list of active key IDs
    let active_kids =
        store
            .get("active-kids")
            .ok_or_else(|| TrustedServerError::Configuration {
                message: "active-kids not found in config store".into(),
            })?;

    // Split by comma and fetch each JWK
    let mut jwks = Vec::new();
    for kid in active_kids.split(',') {
        let kid = kid.trim();
        if kid.is_empty() {
            continue;
        }

        let jwk = store
            .get(kid)
            .ok_or_else(|| TrustedServerError::Configuration {
                message: format!("JWK '{}' not found in config store", kid),
            })?;

        jwks.push(jwk);
    }

    // Build the JWKS response
    let keys_json = jwks.join(",");
    Ok(format!(r#"{{"keys":[{}]}}"#, keys_json))
}

/// Handles requests to the JWKS endpoint.
///
/// This endpoint serves the JSON Web Key Set (JWKS) containing all currently
/// active public keys that can be used to verify signatures.
pub fn handle_jwks_endpoint(
    _settings: &Settings,
    _req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let jwks_json = get_active_jwks().change_context(TrustedServerError::Configuration {
        message: "Failed to retrieve JWKS".into(),
    })?;

    Ok(Response::from_status(200)
        .with_content_type(fastly::mime::APPLICATION_JSON)
        .with_body_text_plain(&jwks_json))
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
