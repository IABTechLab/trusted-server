//! JSON Web Key Set (JWKS) management.
//!
//! This module provides functionality for generating, storing, and retrieving
//! Ed25519 keypairs in JWK format for request signing.

use ed25519_dalek::{SigningKey, VerifyingKey};
use error_stack::{Report, ResultExt as _};
use jose_jwk::{
    Jwk, Key, Okp, OkpCurves, Parameters,
    jose_jwa::{Algorithm, Signing},
};
use rand::rngs::OsRng;

use crate::error::TrustedServerError;
use crate::platform::RuntimeServices;
use crate::request_signing::{JWKS_STORE_NAME, read_active_kids};

/// An Ed25519 keypair used for request signing.
pub struct Keypair {
    pub signing_key: SigningKey,
    pub verifying_key: VerifyingKey,
}

impl Keypair {
    /// Generate a new random Ed25519 keypair.
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

    /// Produce a public JWK from the verifying key, tagged with the given `kid`.
    #[must_use]
    pub fn get_jwk(&self, kid: String) -> Jwk {
        let public_key_bytes = self.verifying_key.as_bytes();

        let okp = Okp {
            crv: OkpCurves::Ed25519,
            x: public_key_bytes.to_vec().into(),
            d: None,
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
/// Reads the `active-kids` entry from the platform config store, then fetches
/// each referenced JWK and assembles a JWKS JSON document.
///
/// # Errors
///
/// Returns [`TrustedServerError::Configuration`] if the config store is
/// unavailable, the `active-kids` key is missing, or any referenced JWK entry
/// cannot be read. The underlying [`crate::platform::PlatformError`] is
/// preserved as context in the error chain.
pub fn get_active_jwks(services: &RuntimeServices) -> Result<String, Report<TrustedServerError>> {
    let active_kids = read_active_kids(services)?;
    let mut jwks = Vec::new();
    for kid in active_kids {
        let jwk = services
            .config_store()
            .get(&JWKS_STORE_NAME, &kid)
            .change_context(TrustedServerError::Configuration {
                message: format!("failed to get JWK for kid: {kid}"),
            })?;
        jwks.push(jwk);
    }

    let keys_json = jwks.join(",");
    Ok(format!(r#"{{"keys":[{keys_json}]}}"#))
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer as _, Verifier as _};
    use error_stack::Report;
    use jose_jwk::Key;

    use crate::platform::{
        PlatformConfigStore, PlatformError, StoreId, StoreName,
        test_support::build_services_with_config,
    };

    use super::*;

    // ---------------------------------------------------------------------------
    // Test doubles
    // ---------------------------------------------------------------------------

    struct FailingConfigStore;

    impl PlatformConfigStore for FailingConfigStore {
        fn get(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::ConfigStore))
        }

        fn put(
            &self,
            _store_id: &StoreId,
            _key: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    // ---------------------------------------------------------------------------
    // Tests
    // ---------------------------------------------------------------------------

    #[test]
    fn get_active_jwks_fails_with_configuration_error_when_store_unavailable() {
        let services = build_services_with_config(FailingConfigStore);
        let result = get_active_jwks(&services);

        assert!(
            result.is_err(),
            "should fail when config store is unavailable"
        );
        let err = result.expect_err("should be an error");
        assert!(
            err.contains::<TrustedServerError>(),
            "should surface as TrustedServerError"
        );
        assert!(
            err.contains::<PlatformError>(),
            "should preserve platform error context in the error chain"
        );
    }

    #[test]
    fn key_pair_generates_valid_signing_key() {
        let keypair = Keypair::generate();

        let message = b"test message";
        let signature = keypair.signing_key.sign(message);

        assert!(
            keypair.verifying_key.verify(message, &signature).is_ok(),
            "should verify signature produced by generated key"
        );
    }

    #[test]
    fn get_jwk_produces_correct_structure() {
        let jwk = Keypair::generate().get_jwk("test-kid".to_owned());

        assert_eq!(
            jwk.prm.kid,
            Some("test-kid".to_owned()),
            "should set kid parameter"
        );
        assert_eq!(
            jwk.prm.alg,
            Some(jose_jwk::jose_jwa::Algorithm::Signing(
                jose_jwk::jose_jwa::Signing::EdDsa
            )),
            "should set EdDSA algorithm"
        );

        match jwk.key {
            Key::Okp(okp) => {
                assert_eq!(okp.crv, OkpCurves::Ed25519, "should use Ed25519 curve");
                assert_eq!(okp.x.len(), 32, "should be 32-byte Ed25519 public key");
                assert!(okp.d.is_none(), "should have no private key component");
            }
            _ => panic!("should be OKP key type"),
        }
    }
}
