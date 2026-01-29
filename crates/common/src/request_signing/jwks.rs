//! JSON Web Key Set (JWKS) management.
//!
//! This module provides functionality for generating, storing, and retrieving
//! Ed25519 keypairs in JWK format for request signing.

use ed25519_dalek::{SigningKey, VerifyingKey};
use error_stack::{Report, ResultExt};
use jose_jwk::{
    jose_jwa::{Algorithm, Signing},
    Jwk, Key, Okp, OkpCurves, Parameters,
};
use rand::rngs::OsRng;

use crate::error::TrustedServerError;
use crate::fastly_storage::FastlyConfigStore;

pub struct Keypair {
    pub signing_key: SigningKey,
    pub verifying_key: VerifyingKey,
}

impl Keypair {
    #[must_use]
    pub fn generate() -> Self {
        let mut csprng = OsRng;

        let signing_key = SigningKey::generate(&mut csprng);
        let verifying_key = signing_key.verifying_key();

        Self {
            signing_key,
            verifying_key,
        }
    }

    #[must_use]
    pub fn get_jwk(&self, kid: String) -> Jwk {
        let public_key_bytes = self.verifying_key.as_bytes();

        let okp = Okp {
            crv: OkpCurves::Ed25519,
            x: public_key_bytes.to_vec().into(),
            d: None, // No private key in JWK (public only)
        };

        Jwk {
            key: Key::Okp(okp),
            prm: Parameters {
                kid: Some(kid),
                alg: Some(Algorithm::Signing(Signing::EdDsa)),
                ..Default::default()
            },
        }
    }
}

/// Retrieves active JSON Web Keys from the config store.
///
/// # Errors
///
/// Returns an error if the config store cannot be accessed or if active keys cannot be retrieved.
pub fn get_active_jwks() -> Result<String, Report<TrustedServerError>> {
    let store = FastlyConfigStore::new("jwks_store");
    let active_kids_str = store
        .get("active-kids")
        .attach("while fetching active kids list")?;

    let active_kids: Vec<&str> = active_kids_str
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    let mut jwks = Vec::new();
    for kid in active_kids {
        let jwk = store
            .get(kid)
            .attach(format!("Failed to get JWK for kid: {}", kid))?;
        jwks.push(jwk);
    }

    let keys_json = jwks.join(",");
    Ok(format!(r#"{{"keys":[{}]}}"#, keys_json))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, Verifier};
    use jose_jwk::Key;

    #[test]
    fn test_key_pair_generation() {
        let keypair = Keypair::generate();

        let message = b"test message";
        let signature = keypair.signing_key.sign(message);

        assert!(keypair.verifying_key.verify(message, &signature).is_ok());
    }

    #[test]
    fn test_create_jwk_from_verifying_key() {
        let jwk = Keypair::generate().get_jwk("test-kid".to_string());

        // Verify JWK structure
        assert_eq!(jwk.prm.kid, Some("test-kid".to_string()));
        assert_eq!(
            jwk.prm.alg,
            Some(jose_jwk::jose_jwa::Algorithm::Signing(
                jose_jwk::jose_jwa::Signing::EdDsa
            ))
        );

        // Verify it's an OKP key with Ed25519 curve
        match jwk.key {
            Key::Okp(okp) => {
                assert_eq!(okp.crv, OkpCurves::Ed25519);
                assert_eq!(okp.x.len(), 32); // Ed25519 public keys are 32 bytes
                assert!(okp.d.is_none()); // No private key component
            }
            _ => panic!("Expected OKP key type"),
        }
    }
}
