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

/// JSON request body for the signature verification endpoint.
#[derive(Debug, Deserialize, Serialize)]
pub struct VerifySignatureRequest {
    /// Canonical payload that was signed.
    pub payload: String,
    /// Base64-encoded Ed25519 signature to verify.
    pub signature: String,
    /// Key identifier used to look up the public JWK.
    pub kid: String,
}

/// JSON response body for the signature verification endpoint.
#[derive(Debug, Deserialize, Serialize)]
pub struct VerifySignatureResponse {
    /// Whether signature verification succeeded.
    pub verified: bool,
    /// Key identifier that was used during verification.
    pub kid: String,
    /// Human-readable verification result summary.
    pub message: String,
    /// Error detail when verification fails unexpectedly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Will verify a signature given a payload and kid
/// Useful for testing integration with signatures
///
/// # Errors
///
/// Returns an error if the request body cannot be parsed as JSON or if the
/// response body cannot be serialized.
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
        Err(e) => {
            log::warn!("signature verification failed: {e}");
            VerifySignatureResponse {
                verified: false,
                kid: verify_req.kid,
                message: "Verification error".into(),
                error: Some("internal verification error".into()),
            }
        }
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

/// JSON request body for the key-rotation endpoint.
#[derive(Debug, Deserialize, Serialize)]
pub struct RotateKeyRequest {
    /// Optional explicit key identifier for the new signing key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kid: Option<String>,
}

/// JSON response body for the key-rotation endpoint.
#[derive(Debug, Deserialize, Serialize)]
pub struct RotateKeyResponse {
    /// Whether the rotation operation succeeded.
    pub success: bool,
    /// Human-readable summary of the rotation result.
    pub message: String,
    /// Newly generated or supplied key identifier.
    pub new_kid: String,
    /// Previously active key identifier, if one existed.
    pub previous_kid: Option<String>,
    /// Active key identifiers after the rotation completes.
    pub active_kids: Vec<String>,
    /// Public JWK associated with the newly active key.
    pub jwk: serde_json::Value,
    /// Error detail when rotation fails.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

struct SigningStoreIds<'a> {
    config_store_id: &'a str,
    secret_store_id: &'a str,
}

fn signing_store_ids(
    settings: &Settings,
) -> Result<SigningStoreIds<'_>, Report<TrustedServerError>> {
    settings
        .request_signing
        .as_ref()
        .map(|setting| SigningStoreIds {
            config_store_id: setting.config_store_id.as_str(),
            secret_store_id: setting.secret_store_id.as_str(),
        })
        .ok_or_else(|| {
            TrustedServerError::Configuration {
                message: "missing signing storage configuration".to_string(),
            }
            .into()
        })
}

/// Rotates the current active kid by generating and saving a new one.
///
/// # Response contract
///
/// Returns `200 OK` with `success: true` on success, or `500 Internal Server Error`
/// with `success: false` and a populated `error` field when rotation fails. Unlike
/// [`handle_verify_signature`], the error field contains internal detail — this is
/// intentional because this endpoint is auth-gated and operator-facing only.
///
/// # Errors
///
/// Returns an error if the request signing settings are missing or JSON parsing fails.
pub fn handle_rotate_key(
    settings: &Settings,
    services: &RuntimeServices,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let SigningStoreIds {
        config_store_id,
        secret_store_id,
    } = signing_store_ids(settings)?;

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

/// JSON request body for the key-deactivation endpoint.
#[derive(Debug, Deserialize, Serialize)]
pub struct DeactivateKeyRequest {
    /// Key identifier to deactivate or delete.
    pub kid: String,
    /// Whether the key should be deleted from storage after deactivation.
    #[serde(default)]
    pub delete: bool,
}

/// JSON response body for the key-deactivation endpoint.
#[derive(Debug, Deserialize, Serialize)]
pub struct DeactivateKeyResponse {
    /// Whether the deactivation or deletion succeeded.
    pub success: bool,
    /// Human-readable summary of the operation result.
    pub message: String,
    /// Key identifier that was deactivated or deleted.
    pub deactivated_kid: String,
    /// Whether the key was deleted from storage.
    pub deleted: bool,
    /// Active key identifiers remaining after the operation.
    pub remaining_active_kids: Vec<String>,
    /// Error detail when the operation fails.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Deactivates or deletes an active signing key.
///
/// # Response contract
///
/// Returns `200 OK` with `success: true` on success, or `500 Internal Server Error`
/// with `success: false` and a populated `error` field when deactivation fails. Like
/// [`handle_rotate_key`] and unlike [`handle_verify_signature`], the error field
/// contains internal detail — this is intentional because this endpoint is
/// auth-gated and operator-facing only.
///
/// # Errors
///
/// Returns an error if the request signing settings are missing or JSON parsing fails.
pub fn handle_deactivate_key(
    settings: &Settings,
    services: &RuntimeServices,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let SigningStoreIds {
        config_store_id,
        secret_store_id,
    } = signing_store_ids(settings)?;

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
    use crate::platform::{
        test_support::{build_request_signing_services, build_services_with_config, noop_services},
        PlatformConfigStore, PlatformError, StoreId, StoreName,
    };

    use super::*;
    use fastly::http::{Method, StatusCode};

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
        let services = build_request_signing_services();

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
        let services = build_request_signing_services();

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

        assert!(
            !verify_resp.verified,
            "should not verify an invalid signature"
        );
        assert_eq!(verify_resp.kid, signer.kid);
        assert!(verify_resp.error.is_some());
    }

    #[test]
    fn test_handle_verify_signature_hides_internal_error_details() {
        let settings = crate::test_support::tests::create_test_settings();

        let verify_req = VerifySignatureRequest {
            payload: "test message".to_string(),
            signature: "any-signature".to_string(),
            kid: "missing-kid".to_string(),
        };

        let body = serde_json::to_string(&verify_req).expect("should serialize verify request");
        let mut req = Request::new(Method::POST, "https://test.com/verify-signature");
        req.set_body(body);

        let services = noop_services();
        let mut resp = handle_verify_signature(&settings, &services, req)
            .expect("should return a verification response for internal errors");

        assert_eq!(resp.get_status(), StatusCode::OK, "should return 200 OK");

        let resp_body = resp.take_body_str();
        let verify_resp: VerifySignatureResponse =
            serde_json::from_str(&resp_body).expect("should deserialize verify response");

        assert!(
            !verify_resp.verified,
            "should mark internal verification errors as unverified"
        );
        assert_eq!(verify_resp.kid, "missing-kid");
        assert_eq!(verify_resp.message, "Verification error");
        assert_eq!(
            verify_resp.error.as_deref(),
            Some("internal verification error"),
            "should return a generic error to unauthenticated callers"
        );
        assert!(
            !resp_body.contains("failed"),
            "should not leak internal error details in the response body"
        );
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

        let mut resp = handle_rotate_key(&settings, &noop_services(), req)
            .expect("should return a response even when stores are unavailable");

        assert_eq!(
            resp.get_status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "should return 500 when store writes fail"
        );

        let body = resp.take_body_str();
        let response: RotateKeyResponse =
            serde_json::from_str(&body).expect("should deserialize rotate response");

        assert!(
            !response.success,
            "should report failure when store writes fail"
        );
        assert!(
            response.error.is_some(),
            "should include error detail in failure response"
        );
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

        let mut resp = handle_rotate_key(&settings, &noop_services(), req)
            .expect("should return a response even when stores are unavailable");

        assert_eq!(
            resp.get_status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "should return 500 when store writes fail"
        );

        let body = resp.take_body_str();
        let response: RotateKeyResponse =
            serde_json::from_str(&body).expect("should deserialize rotate response");

        assert!(
            !response.success,
            "should report failure when store writes fail"
        );
        assert!(
            response.error.is_some(),
            "should include error detail in failure response"
        );
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

        let mut resp = handle_deactivate_key(&settings, &noop_services(), req)
            .expect("should return a response even when stores are unavailable");

        assert_eq!(
            resp.get_status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "should return 500 when active-kids cannot be read"
        );

        let body = resp.take_body_str();
        let response: DeactivateKeyResponse =
            serde_json::from_str(&body).expect("should deserialize deactivate response");

        assert!(
            !response.success,
            "should report failure when store reads fail"
        );
        assert!(
            response.error.is_some(),
            "should include error detail in failure response"
        );
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

        let mut resp = handle_deactivate_key(&settings, &noop_services(), req)
            .expect("should return a response even when stores are unavailable");

        assert_eq!(
            resp.get_status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "should return 500 when active-kids cannot be read"
        );

        let body = resp.take_body_str();
        let response: DeactivateKeyResponse =
            serde_json::from_str(&body).expect("should deserialize deactivate response");

        assert!(
            !response.success,
            "should report failure when store reads fail"
        );
        assert!(
            !response.deleted,
            "should not report deletion when the operation failed"
        );
        assert!(
            response.error.is_some(),
            "should include error detail in failure response"
        );
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

        // noop_services() config store always returns Err, so the discovery
        // handler propagates the error rather than absorbing it into a 500.
        let result = handle_trusted_server_discovery(&settings, &noop_services(), req);

        assert!(
            result.is_err(),
            "should propagate store errors when JWKS cannot be retrieved"
        );
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
