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
use crate::platform::{RuntimeServices, StoreName};
use crate::request_signing::JWKS_CONFIG_STORE_NAME;

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
    let store_name = StoreName::from(JWKS_CONFIG_STORE_NAME);
    let active_kids_str = services
        .config_store()
        .get(&store_name, "active-kids")
        .change_context(TrustedServerError::Configuration {
            message: "failed to read active-kids from config store".into(),
        })
        .attach("while fetching active kids list")?;

    let active_kids: Vec<&str> = active_kids_str
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    let mut jwks = Vec::new();
    for kid in active_kids {
        let jwk = services
            .config_store()
            .get(&store_name, kid)
            .change_context(TrustedServerError::Configuration {
                message: format!("failed to get JWK for kid: {}", kid),
            })?;
        jwks.push(jwk);
    }

    let keys_json = jwks.join(",");
    Ok(format!(r#"{{"keys":[{}]}}"#, keys_json))
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::sync::Arc;

    use ed25519_dalek::{Signer, Verifier};
    use error_stack::Report;
    use jose_jwk::Key;

    use crate::platform::{
        ClientInfo, GeoInfo, PlatformBackend, PlatformBackendSpec, PlatformConfigStore,
        PlatformError, PlatformGeo, PlatformHttpClient, PlatformHttpRequest,
        PlatformPendingRequest, PlatformResponse, PlatformSecretStore, PlatformSelectResult,
        RuntimeServices, StoreId, StoreName,
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

    struct NoopSecretStore;

    impl PlatformSecretStore for NoopSecretStore {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct NoopBackend;

    impl PlatformBackend for NoopBackend {
        fn predict_name(
            &self,
            _spec: &PlatformBackendSpec,
        ) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn ensure(&self, _spec: &PlatformBackendSpec) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct NoopHttpClient;

    #[async_trait::async_trait(?Send)]
    impl PlatformHttpClient for NoopHttpClient {
        async fn send(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformResponse, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn send_async(
            &self,
            _request: PlatformHttpRequest,
        ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        async fn select(
            &self,
            _pending_requests: Vec<PlatformPendingRequest>,
        ) -> Result<PlatformSelectResult, Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    struct NoopGeo;

    impl PlatformGeo for NoopGeo {
        fn lookup(
            &self,
            _client_ip: Option<IpAddr>,
        ) -> Result<Option<GeoInfo>, Report<PlatformError>> {
            Ok(None)
        }
    }

    fn build_services_with_config(
        config_store: impl PlatformConfigStore + 'static,
    ) -> RuntimeServices {
        RuntimeServices::builder()
            .config_store(Arc::new(config_store))
            .secret_store(Arc::new(NoopSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(Arc::new(NoopBackend))
            .http_client(Arc::new(NoopHttpClient))
            .geo(Arc::new(NoopGeo))
            .client_info(ClientInfo {
                client_ip: None,
                tls_protocol: None,
                tls_cipher: None,
            })
            .build()
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
        let jwk = Keypair::generate().get_jwk("test-kid".to_string());

        assert_eq!(
            jwk.prm.kid,
            Some("test-kid".to_string()),
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
