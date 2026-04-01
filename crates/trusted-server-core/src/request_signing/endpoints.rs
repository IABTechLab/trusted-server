//! HTTP endpoint handlers for request signing operations.
//!
//! This module provides endpoint handlers for JWKS retrieval, signature verification,
//! key rotation, and key deactivation operations.

use error_stack::{Report, ResultExt};
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};

use crate::error::TrustedServerError;
use crate::platform::RuntimeServices;
use crate::request_signing::discovery::TrustedServerDiscovery;
use crate::request_signing::rotation::KeyRotationManager;
use crate::request_signing::signing;
use crate::settings::Settings;

/// Retrieves and returns the trusted-server discovery document.
///
/// This endpoint provides a standardized discovery mechanism following the IAB
/// Data Subject Rights framework pattern. It returns JWKS keys and API endpoints
/// in a single discoverable location.
///
/// # Errors
///
/// Returns an error if JWKS cannot be retrieved, parsed, or serialized.
pub fn handle_trusted_server_discovery(
    _settings: &Settings,
    services: &RuntimeServices,
    _req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let jwks_json = crate::request_signing::jwks::get_active_jwks(services).change_context(
        TrustedServerError::Configuration {
            message: "failed to retrieve JWKS".into(),
        },
    )?;

    let jwks_value: serde_json::Value =
        serde_json::from_str(&jwks_json).change_context(TrustedServerError::Configuration {
            message: "failed to parse JWKS JSON".into(),
        })?;

    let discovery = TrustedServerDiscovery::new(jwks_value);

    let json = serde_json::to_string_pretty(&discovery).change_context(
        TrustedServerError::Configuration {
            message: "failed to serialize discovery document".into(),
        },
    )?;

    Ok(Response::from_status(200)
        .with_content_type(fastly::mime::APPLICATION_JSON)
        .with_body(json))
}

#[derive(Debug, Deserialize, Serialize)]
pub struct VerifySignatureRequest {
    pub payload: String,
    pub signature: String,
    pub kid: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct VerifySignatureResponse {
    pub verified: bool,
    pub kid: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Will verify a signature given a payload and kid
/// Useful for testing integration with signatures
///
/// # Errors
///
/// Returns an error if the request body cannot be parsed as JSON or if verification fails.
pub fn handle_verify_signature(
    _settings: &Settings,
    services: &RuntimeServices,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let body = req.take_body_str();
    let verify_req: VerifySignatureRequest =
        serde_json::from_str(&body).change_context(TrustedServerError::Configuration {
            message: "invalid JSON request body".into(),
        })?;

    let verification_result = signing::verify_signature(
        verify_req.payload.as_bytes(),
        &verify_req.signature,
        &verify_req.kid,
        services,
    );

    let response = match verification_result {
        Ok(true) => VerifySignatureResponse {
            verified: true,
            kid: verify_req.kid,
            message: "Signature verified successfully".into(),
            error: None,
        },
        Ok(false) => VerifySignatureResponse {
            verified: false,
            kid: verify_req.kid,
            message: "Signature verification failed".into(),
            error: Some("Invalid signature".into()),
        },
        Err(e) => VerifySignatureResponse {
            verified: false,
            kid: verify_req.kid,
            message: "Verification error".into(),
            error: Some(format!("{}", e)),
        },
    };

    let response_json = serde_json::to_string(&response).map_err(|e| {
        Report::new(TrustedServerError::Configuration {
            message: format!("failed to serialize response: {}", e),
        })
    })?;

    Ok(Response::from_status(200)
        .with_content_type(fastly::mime::APPLICATION_JSON)
        .with_body(response_json))
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RotateKeyRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kid: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RotateKeyResponse {
    pub success: bool,
    pub message: String,
    pub new_kid: String,
    pub previous_kid: Option<String>,
    pub active_kids: Vec<String>,
    pub jwk: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Rotates the current active kid by generating and saving a new one
///
/// # Errors
///
/// Returns an error if the request signing settings are missing, JSON parsing fails, or key rotation fails.
pub fn handle_rotate_key(
    settings: &Settings,
    services: &RuntimeServices,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let (config_store_id, secret_store_id) = match &settings.request_signing {
        Some(setting) => (&setting.config_store_id, &setting.secret_store_id),
        None => {
            return Err(TrustedServerError::Configuration {
                message: "missing signing storage configuration".to_string(),
            }
            .into());
        }
    };

    let body = req.take_body_str();
    let rotate_req: RotateKeyRequest = if body.is_empty() {
        RotateKeyRequest { kid: None }
    } else {
        serde_json::from_str(&body).change_context(TrustedServerError::Configuration {
            message: "invalid JSON request body".into(),
        })?
    };

    let manager = KeyRotationManager::new(config_store_id, secret_store_id);

    match manager.rotate_key(services, rotate_req.kid) {
        Ok(result) => {
            let jwk_value = serde_json::to_value(&result.jwk).map_err(|e| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("failed to serialize JWK: {}", e),
                })
            })?;

            let response = RotateKeyResponse {
                success: true,
                message: "Key rotated successfully".to_string(),
                new_kid: result.new_kid,
                previous_kid: result.previous_kid,
                active_kids: result.active_kids,
                jwk: jwk_value,
                error: None,
            };

            let response_json = serde_json::to_string(&response).map_err(|e| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("failed to serialize response: {}", e),
                })
            })?;

            Ok(Response::from_status(200)
                .with_content_type(fastly::mime::APPLICATION_JSON)
                .with_body(response_json))
        }
        Err(e) => {
            let response = RotateKeyResponse {
                success: false,
                message: "Key rotation failed".to_string(),
                new_kid: String::new(),
                previous_kid: None,
                active_kids: vec![],
                jwk: serde_json::json!({}),
                error: Some(format!("{}", e)),
            };

            let response_json = serde_json::to_string(&response).map_err(|e| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("failed to serialize response: {}", e),
                })
            })?;

            Ok(Response::from_status(500)
                .with_content_type(fastly::mime::APPLICATION_JSON)
                .with_body(response_json))
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DeactivateKeyRequest {
    pub kid: String,
    #[serde(default)]
    pub delete: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DeactivateKeyResponse {
    pub success: bool,
    pub message: String,
    pub deactivated_kid: String,
    pub deleted: bool,
    pub remaining_active_kids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Deactivates an active key
///
/// # Errors
///
/// Returns an error if the request signing settings are missing, JSON parsing fails, or key deactivation fails.
pub fn handle_deactivate_key(
    settings: &Settings,
    services: &RuntimeServices,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let (config_store_id, secret_store_id) = match &settings.request_signing {
        Some(setting) => (&setting.config_store_id, &setting.secret_store_id),
        None => {
            return Err(TrustedServerError::Configuration {
                message: "missing signing storage configuration".to_string(),
            }
            .into());
        }
    };

    let body = req.take_body_str();
    let deactivate_req: DeactivateKeyRequest =
        serde_json::from_str(&body).change_context(TrustedServerError::Configuration {
            message: "invalid JSON request body".into(),
        })?;

    let manager = KeyRotationManager::new(config_store_id, secret_store_id);

    let result = if deactivate_req.delete {
        manager.delete_key(services, &deactivate_req.kid)
    } else {
        manager.deactivate_key(services, &deactivate_req.kid)
    };

    match result {
        Ok(()) => {
            let remaining_keys = manager.list_active_keys(services).unwrap_or_else(|e| {
                log::warn!("failed to list active keys after deactivation: {}", e);
                vec![]
            });

            let response = DeactivateKeyResponse {
                success: true,
                message: if deactivate_req.delete {
                    "Key deleted successfully".to_string()
                } else {
                    "Key deactivated successfully".to_string()
                },
                deactivated_kid: deactivate_req.kid,
                deleted: deactivate_req.delete,
                remaining_active_kids: remaining_keys,
                error: None,
            };

            let response_json = serde_json::to_string(&response).map_err(|e| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("failed to serialize response: {}", e),
                })
            })?;

            Ok(Response::from_status(200)
                .with_content_type(fastly::mime::APPLICATION_JSON)
                .with_body(response_json))
        }
        Err(e) => {
            let response = DeactivateKeyResponse {
                success: false,
                message: if deactivate_req.delete {
                    "Key deletion failed".to_string()
                } else {
                    "Key deactivation failed".to_string()
                },
                deactivated_kid: deactivate_req.kid.clone(),
                deleted: false,
                remaining_active_kids: vec![],
                error: Some(format!("{}", e)),
            };

            let response_json = serde_json::to_string(&response).map_err(|e| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("failed to serialize response: {}", e),
                })
            })?;

            Ok(Response::from_status(500)
                .with_content_type(fastly::mime::APPLICATION_JSON)
                .with_body(response_json))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use error_stack::Report;

    use crate::platform::{
        test_support::{build_services_with_config, build_services_with_config_and_secret, noop_services},
        PlatformConfigStore, PlatformError, PlatformSecretStore, StoreId, StoreName,
    };

    use super::*;
    use fastly::http::{Method, StatusCode};

    /// Build `RuntimeServices` pre-loaded with a real Ed25519 keypair for
    /// testing signature creation and verification in endpoint handlers.
    fn build_signing_services_for_test() -> crate::platform::RuntimeServices {
        use base64::{engine::general_purpose, Engine};
        use ed25519_dalek::SigningKey;
        use rand::rngs::OsRng;

        struct MapConfigStore(HashMap<String, String>);
        impl PlatformConfigStore for MapConfigStore {
            fn get(&self, _: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
                self.0.get(key).cloned().ok_or_else(|| Report::new(PlatformError::ConfigStore))
            }
            fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
                Err(Report::new(PlatformError::Unsupported))
            }
            fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
                Err(Report::new(PlatformError::Unsupported))
            }
        }

        struct MapSecretStore(HashMap<String, Vec<u8>>);
        impl PlatformSecretStore for MapSecretStore {
            fn get_bytes(&self, _: &StoreName, key: &str) -> Result<Vec<u8>, Report<PlatformError>> {
                self.0.get(key).cloned().ok_or_else(|| Report::new(PlatformError::SecretStore))
            }
            fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
                Err(Report::new(PlatformError::Unsupported))
            }
            fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
                Err(Report::new(PlatformError::Unsupported))
            }
        }

        let signing_key = SigningKey::generate(&mut OsRng);
        let key_b64 = general_purpose::STANDARD.encode(signing_key.as_bytes());
        let x_b64 = general_purpose::URL_SAFE_NO_PAD.encode(signing_key.verifying_key().as_bytes());
        let jwk_json = format!(
            r#"{{"kty":"OKP","crv":"Ed25519","x":"{}","kid":"test-kid","alg":"EdDSA"}}"#,
            x_b64
        );

        let mut cfg = HashMap::new();
        cfg.insert("current-kid".to_string(), "test-kid".to_string());
        cfg.insert("test-kid".to_string(), jwk_json);

        let mut sec = HashMap::new();
        sec.insert("test-kid".to_string(), key_b64.into_bytes());

        build_services_with_config_and_secret(MapConfigStore(cfg), MapSecretStore(sec))
    }

    /// Config store stub that returns a minimal JWKS with one Ed25519 key.
    struct StubJwksConfigStore;

    impl PlatformConfigStore for StubJwksConfigStore {
        fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
            match key {
                "active-kids" => Ok("test-kid-1".to_string()),
                "test-kid-1" => Ok(
                    r#"{"kty":"OKP","crv":"Ed25519","x":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","kid":"test-kid-1","alg":"EdDSA"}"#
                        .to_string(),
                ),
                _ => Err(Report::new(PlatformError::ConfigStore)),
            }
        }

        fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }

        fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::Unsupported))
        }
    }

    #[test]
    fn test_handle_verify_signature_valid() {
        let settings = crate::test_support::tests::create_test_settings();
        let services = build_signing_services_for_test();

        let payload = "test message";
        let signer = crate::request_signing::RequestSigner::from_services(&services)
            .expect("should create signer from services");
        let signature = signer
            .sign(payload.as_bytes())
            .expect("should sign payload");

        let verify_req = VerifySignatureRequest {
            payload: payload.to_string(),
            signature,
            kid: signer.kid.clone(),
        };

        let body = serde_json::to_string(&verify_req).expect("should serialize verify request");
        let mut req = Request::new(Method::POST, "https://test.com/verify-signature");
        req.set_body(body);

        let mut resp = handle_verify_signature(&settings, &services, req)
            .expect("should handle verification request");
        assert_eq!(resp.get_status(), StatusCode::OK);
        assert_eq!(
            resp.get_content_type(),
            Some(fastly::mime::APPLICATION_JSON),
            "should return application/json content type"
        );

        let resp_body = resp.take_body_str();
        let verify_resp: VerifySignatureResponse =
            serde_json::from_str(&resp_body).expect("should deserialize verify response");

        assert!(verify_resp.verified, "should verify a valid signature");
        assert_eq!(verify_resp.kid, signer.kid);
        assert!(verify_resp.error.is_none());
    }

    #[test]
    fn test_handle_verify_signature_invalid() {
        let settings = crate::test_support::tests::create_test_settings();
        let services = build_signing_services_for_test();

        let signer = crate::request_signing::RequestSigner::from_services(&services)
            .expect("should create signer from services");

        let wrong_signature = signer
            .sign(b"different payload")
            .expect("should sign different payload");

        let verify_req = VerifySignatureRequest {
            payload: "test message".to_string(),
            signature: wrong_signature,
            kid: signer.kid.clone(),
        };

        let body = serde_json::to_string(&verify_req).expect("should serialize verify request");
        let mut req = Request::new(Method::POST, "https://test.com/verify-signature");
        req.set_body(body);

        let mut resp = handle_verify_signature(&settings, &services, req)
            .expect("should handle verification request");
        assert_eq!(resp.get_status(), StatusCode::OK);
        assert_eq!(
            resp.get_content_type(),
            Some(fastly::mime::APPLICATION_JSON),
            "should return application/json content type"
        );

        let resp_body = resp.take_body_str();
        let verify_resp: VerifySignatureResponse =
            serde_json::from_str(&resp_body).expect("should deserialize verify response");

        assert!(!verify_resp.verified, "should not verify an invalid signature");
        assert_eq!(verify_resp.kid, signer.kid);
        assert!(verify_resp.error.is_some());
    }

    #[test]
    fn test_handle_verify_signature_malformed_request() {
        let settings = crate::test_support::tests::create_test_settings();

        let mut req = Request::new(Method::POST, "https://test.com/verify-signature");
        req.set_body("not valid json");

        let result = handle_verify_signature(&settings, &noop_services(), req);
        assert!(result.is_err(), "Malformed JSON should error");
    }

    #[test]
    fn test_handle_rotate_key_with_empty_body() {
        let settings = crate::test_support::tests::create_test_settings();
        let req = Request::new(Method::POST, "https://test.com/admin/keys/rotate");

        let result = handle_rotate_key(&settings, &noop_services(), req);
        match result {
            Ok(mut resp) => {
                let body = resp.take_body_str();
                let response: RotateKeyResponse =
                    serde_json::from_str(&body).expect("should deserialize rotate response");
                log::debug!(
                    "Rotation response: success={}, message={}",
                    response.success,
                    response.message
                );
            }
            Err(e) => log::debug!("Expected error in test environment: {}", e),
        }
    }

    #[test]
    fn test_handle_rotate_key_with_custom_kid() {
        let settings = crate::test_support::tests::create_test_settings();

        let req_body = RotateKeyRequest {
            kid: Some("test-custom-key".to_string()),
        };

        let body_json = serde_json::to_string(&req_body).expect("should serialize rotate request");
        let mut req = Request::new(Method::POST, "https://test.com/admin/keys/rotate");
        req.set_body(body_json);

        let result = handle_rotate_key(&settings, &noop_services(), req);
        match result {
            Ok(mut resp) => {
                let body = resp.take_body_str();
                let response: RotateKeyResponse =
                    serde_json::from_str(&body).expect("should deserialize rotate response");
                log::debug!(
                    "Custom KID rotation: success={}, new_kid={}",
                    response.success,
                    response.new_kid
                );
            }
            Err(e) => log::debug!("Expected error in test environment: {}", e),
        }
    }

    #[test]
    fn test_handle_rotate_key_invalid_json() {
        let settings = crate::test_support::tests::create_test_settings();
        let mut req = Request::new(Method::POST, "https://test.com/admin/keys/rotate");
        req.set_body("invalid json");

        let result = handle_rotate_key(&settings, &noop_services(), req);
        assert!(result.is_err(), "Invalid JSON should return error");
    }

    #[test]
    fn test_handle_deactivate_key_request() {
        let settings = crate::test_support::tests::create_test_settings();

        let req_body = DeactivateKeyRequest {
            kid: "test-old-key".to_string(),
            delete: false,
        };

        let body_json =
            serde_json::to_string(&req_body).expect("should serialize deactivate request");
        let mut req = Request::new(Method::POST, "https://test.com/admin/keys/deactivate");
        req.set_body(body_json);

        let result = handle_deactivate_key(&settings, &noop_services(), req);
        match result {
            Ok(mut resp) => {
                let body = resp.take_body_str();
                let response: DeactivateKeyResponse =
                    serde_json::from_str(&body).expect("should deserialize deactivate response");
                log::debug!(
                    "Deactivate response: success={}, message={}",
                    response.success,
                    response.message
                );
            }
            Err(e) => log::debug!("Expected error in test environment: {}", e),
        }
    }

    #[test]
    fn test_handle_deactivate_key_with_delete() {
        let settings = crate::test_support::tests::create_test_settings();

        let req_body = DeactivateKeyRequest {
            kid: "test-old-key".to_string(),
            delete: true,
        };

        let body_json =
            serde_json::to_string(&req_body).expect("should serialize deactivate request");
        let mut req = Request::new(Method::POST, "https://test.com/admin/keys/deactivate");
        req.set_body(body_json);

        let result = handle_deactivate_key(&settings, &noop_services(), req);
        match result {
            Ok(mut resp) => {
                let body = resp.take_body_str();
                let response: DeactivateKeyResponse =
                    serde_json::from_str(&body).expect("should deserialize deactivate response");
                log::debug!(
                    "Delete response: success={}, deleted={}",
                    response.success,
                    response.deleted
                );
            }
            Err(e) => log::debug!("Expected error in test environment: {}", e),
        }
    }

    #[test]
    fn test_handle_deactivate_key_invalid_json() {
        let settings = crate::test_support::tests::create_test_settings();
        let mut req = Request::new(Method::POST, "https://test.com/admin/keys/deactivate");
        req.set_body("invalid json");

        let result = handle_deactivate_key(&settings, &noop_services(), req);
        assert!(result.is_err(), "Invalid JSON should return error");
    }

    #[test]
    fn test_rotate_key_request_deserialization() {
        let json = r#"{"kid":"custom-key"}"#;
        let req: RotateKeyRequest =
            serde_json::from_str(json).expect("should deserialize rotate key request");
        assert_eq!(req.kid, Some("custom-key".to_string()));
    }

    #[test]
    fn test_deactivate_key_request_deserialization() {
        let json = r#"{"kid":"old-key","delete":true}"#;
        let req: DeactivateKeyRequest =
            serde_json::from_str(json).expect("should deserialize deactivate key request");
        assert_eq!(req.kid, "old-key");
        assert!(req.delete);
    }

    #[test]
    fn test_handle_trusted_server_discovery() {
        let settings = crate::test_support::tests::create_test_settings();
        let req = Request::new(
            Method::GET,
            "https://test.com/.well-known/trusted-server.json",
        );

        let services = noop_services();
        let result = handle_trusted_server_discovery(&settings, &services, req);
        match result {
            Ok(mut resp) => {
                assert_eq!(resp.get_status(), StatusCode::OK);
                assert_eq!(
                    resp.get_content_type(),
                    Some(fastly::mime::APPLICATION_JSON),
                    "should return application/json content type"
                );
                let body = resp.take_body_str();

                // Parse the discovery document
                let discovery: serde_json::Value =
                    serde_json::from_str(&body).expect("should parse discovery document");

                // Verify structure - only version and jwks
                assert_eq!(discovery["version"], "1.0");
                assert!(discovery["jwks"].is_object());

                // Verify no extra fields
                assert!(discovery.get("endpoints").is_none());
                assert!(discovery.get("capabilities").is_none());
            }
            Err(e) => log::debug!("Expected error in test environment: {}", e),
        }
    }

    #[test]
    fn test_handle_trusted_server_discovery_returns_jwks_document() {
        let settings = crate::test_support::tests::create_test_settings();
        let req = Request::new(
            Method::GET,
            "https://test.com/.well-known/trusted-server.json",
        );

        let services = build_services_with_config(StubJwksConfigStore);
        let mut resp = handle_trusted_server_discovery(&settings, &services, req)
            .expect("should return discovery document when config store is populated");

        assert_eq!(resp.get_status(), StatusCode::OK, "should return 200 OK");

        let body = resp.take_body_str();
        let discovery: serde_json::Value =
            serde_json::from_str(&body).expect("should parse discovery document as JSON");

        assert_eq!(discovery["version"], "1.0", "should return version 1.0");

        let keys = discovery["jwks"]["keys"]
            .as_array()
            .expect("should have jwks.keys array");
        assert_eq!(keys.len(), 1, "should contain exactly one key");
        assert_eq!(
            keys[0]["kid"], "test-kid-1",
            "should include the active key ID"
        );
        assert_eq!(keys[0]["crv"], "Ed25519", "should be an Ed25519 key");
    }
}
