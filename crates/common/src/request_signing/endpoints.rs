//! HTTP endpoint handlers for request signing operations.
//!
//! This module provides endpoint handlers for JWKS retrieval, signature verification,
//! key rotation, and key deactivation operations.

use error_stack::{Report, ResultExt};
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};

use crate::error::TrustedServerError;
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
    _req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    // Get JWKS
    let jwks_json = crate::request_signing::jwks::get_active_jwks().change_context(
        TrustedServerError::Configuration {
            message: "Failed to retrieve JWKS".into(),
        },
    )?;

    let jwks_value: serde_json::Value =
        serde_json::from_str(&jwks_json).change_context(TrustedServerError::Configuration {
            message: "Failed to parse JWKS JSON".into(),
        })?;

    let discovery = TrustedServerDiscovery::new(jwks_value);

    let json = serde_json::to_string_pretty(&discovery).change_context(
        TrustedServerError::Configuration {
            message: "Failed to serialize discovery document".into(),
        },
    )?;

    Ok(Response::from_status(200)
        .with_content_type(fastly::mime::APPLICATION_JSON)
        .with_body_text_plain(&json))
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
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let body = req.take_body_str();
    let verify_req: VerifySignatureRequest =
        serde_json::from_str(&body).change_context(TrustedServerError::Configuration {
            message: "Invalid JSON request body".into(),
        })?;

    let verification_result = signing::verify_signature(
        verify_req.payload.as_bytes(),
        &verify_req.signature,
        &verify_req.kid,
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
            message: format!("Failed to serialize response: {}", e),
        })
    })?;

    Ok(Response::from_status(200)
        .with_content_type(fastly::mime::APPLICATION_JSON)
        .with_body_text_plain(&response_json))
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
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let (config_store_id, secret_store_id) = match &settings.request_signing {
        Some(setting) => (&setting.config_store_id, &setting.secret_store_id),
        None => {
            return Err(TrustedServerError::Configuration {
                message: "Missing signing storage configuration.".to_string(),
            }
            .into());
        }
    };

    let body = req.take_body_str();
    let rotate_req: RotateKeyRequest = if body.is_empty() {
        RotateKeyRequest { kid: None }
    } else {
        serde_json::from_str(&body).change_context(TrustedServerError::Configuration {
            message: "Invalid JSON request body".into(),
        })?
    };

    let manager = KeyRotationManager::new(config_store_id, secret_store_id).change_context(
        TrustedServerError::Configuration {
            message: "Failed to create KeyRotationManager".into(),
        },
    )?;

    match manager.rotate_key(rotate_req.kid) {
        Ok(result) => {
            let jwk_value = serde_json::to_value(&result.jwk).map_err(|e| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("Failed to serialize JWK: {}", e),
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
                    message: format!("Failed to serialize response: {}", e),
                })
            })?;

            Ok(Response::from_status(200)
                .with_content_type(fastly::mime::APPLICATION_JSON)
                .with_body_text_plain(&response_json))
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
                    message: format!("Failed to serialize response: {}", e),
                })
            })?;

            Ok(Response::from_status(500)
                .with_content_type(fastly::mime::APPLICATION_JSON)
                .with_body_text_plain(&response_json))
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
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    let (config_store_id, secret_store_id) = match &settings.request_signing {
        Some(setting) => (&setting.config_store_id, &setting.secret_store_id),
        None => {
            return Err(TrustedServerError::Configuration {
                message: "Missing signing storage configuration.".to_string(),
            }
            .into());
        }
    };

    let body = req.take_body_str();
    let deactivate_req: DeactivateKeyRequest =
        serde_json::from_str(&body).change_context(TrustedServerError::Configuration {
            message: "Invalid JSON request body".into(),
        })?;

    let manager = KeyRotationManager::new(config_store_id, secret_store_id).change_context(
        TrustedServerError::Configuration {
            message: "Failed to create KeyRotationManager".into(),
        },
    )?;

    let result = if deactivate_req.delete {
        manager.delete_key(&deactivate_req.kid)
    } else {
        manager.deactivate_key(&deactivate_req.kid)
    };

    match result {
        Ok(()) => {
            let remaining_keys = manager.list_active_keys().unwrap_or_else(|_| vec![]);

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
                    message: format!("Failed to serialize response: {}", e),
                })
            })?;

            Ok(Response::from_status(200)
                .with_content_type(fastly::mime::APPLICATION_JSON)
                .with_body_text_plain(&response_json))
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
                    message: format!("Failed to serialize response: {}", e),
                })
            })?;

            Ok(Response::from_status(500)
                .with_content_type(fastly::mime::APPLICATION_JSON)
                .with_body_text_plain(&response_json))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fastly::http::{Method, StatusCode};

    #[test]
    fn test_handle_verify_signature_valid() {
        let settings = crate::test_support::tests::create_test_settings();

        // First, create a valid signature
        let payload = "test message";
        let signer = crate::request_signing::RequestSigner::from_config()
            .expect("should create signer from config");
        let signature = signer
            .sign(payload.as_bytes())
            .expect("should sign payload");

        // Create verification request
        let verify_req = VerifySignatureRequest {
            payload: payload.to_string(),
            signature,
            kid: signer.kid.clone(),
        };

        let body = serde_json::to_string(&verify_req).expect("should serialize verify request");
        let mut req = Request::new(Method::POST, "https://test.com/verify-signature");
        req.set_body(body);

        // Handle the request
        let mut resp =
            handle_verify_signature(&settings, req).expect("should handle verification request");
        assert_eq!(resp.get_status(), StatusCode::OK);

        // Parse response
        let resp_body = resp.take_body_str();
        let verify_resp: VerifySignatureResponse =
            serde_json::from_str(&resp_body).expect("should deserialize verify response");

        assert!(verify_resp.verified, "Signature should be verified");
        assert_eq!(verify_resp.kid, signer.kid);
        assert!(verify_resp.error.is_none());
    }

    #[test]
    fn test_handle_verify_signature_invalid() {
        let settings = crate::test_support::tests::create_test_settings();
        let signer = crate::request_signing::RequestSigner::from_config()
            .expect("should create signer from config");

        // Create a signature for a different payload
        let wrong_signature = signer
            .sign(b"different payload")
            .expect("should sign different payload");

        // Create request with signature that does not match the payload
        let verify_req = VerifySignatureRequest {
            payload: "test message".to_string(),
            signature: wrong_signature,
            kid: signer.kid.clone(),
        };

        let body = serde_json::to_string(&verify_req).expect("should serialize verify request");
        let mut req = Request::new(Method::POST, "https://test.com/verify-signature");
        req.set_body(body);

        // Handle the request
        let mut resp =
            handle_verify_signature(&settings, req).expect("should handle verification request");
        assert_eq!(resp.get_status(), StatusCode::OK);

        // Parse response
        let resp_body = resp.take_body_str();
        let verify_resp: VerifySignatureResponse =
            serde_json::from_str(&resp_body).expect("should deserialize verify response");

        assert!(!verify_resp.verified, "Invalid signature should not verify");
        assert_eq!(verify_resp.kid, signer.kid);
        assert!(verify_resp.error.is_some());
    }

    #[test]
    fn test_handle_verify_signature_malformed_request() {
        let settings = crate::test_support::tests::create_test_settings();

        let mut req = Request::new(Method::POST, "https://test.com/verify-signature");
        req.set_body("not valid json");

        // Should return an error response
        let result = handle_verify_signature(&settings, req);
        assert!(result.is_err(), "Malformed JSON should error");
    }

    #[test]
    fn test_handle_rotate_key_with_empty_body() {
        let settings = crate::test_support::tests::create_test_settings();
        let req = Request::new(Method::POST, "https://test.com/admin/keys/rotate");

        let result = handle_rotate_key(&settings, req);
        match result {
            Ok(mut resp) => {
                let body = resp.take_body_str();
                let response: RotateKeyResponse =
                    serde_json::from_str(&body).expect("should deserialize rotate response");
                println!(
                    "Rotation response: success={}, message={}",
                    response.success, response.message
                );
            }
            Err(e) => println!("Expected error in test environment: {}", e),
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

        let result = handle_rotate_key(&settings, req);
        match result {
            Ok(mut resp) => {
                let body = resp.take_body_str();
                let response: RotateKeyResponse =
                    serde_json::from_str(&body).expect("should deserialize rotate response");
                println!(
                    "Custom KID rotation: success={}, new_kid={}",
                    response.success, response.new_kid
                );
            }
            Err(e) => println!("Expected error in test environment: {}", e),
        }
    }

    #[test]
    fn test_handle_rotate_key_invalid_json() {
        let settings = crate::test_support::tests::create_test_settings();
        let mut req = Request::new(Method::POST, "https://test.com/admin/keys/rotate");
        req.set_body("invalid json");

        let result = handle_rotate_key(&settings, req);
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

        let result = handle_deactivate_key(&settings, req);
        match result {
            Ok(mut resp) => {
                let body = resp.take_body_str();
                let response: DeactivateKeyResponse =
                    serde_json::from_str(&body).expect("should deserialize deactivate response");
                println!(
                    "Deactivate response: success={}, message={}",
                    response.success, response.message
                );
            }
            Err(e) => println!("Expected error in test environment: {}", e),
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

        let result = handle_deactivate_key(&settings, req);
        match result {
            Ok(mut resp) => {
                let body = resp.take_body_str();
                let response: DeactivateKeyResponse =
                    serde_json::from_str(&body).expect("should deserialize deactivate response");
                println!(
                    "Delete response: success={}, deleted={}",
                    response.success, response.deleted
                );
            }
            Err(e) => println!("Expected error in test environment: {}", e),
        }
    }

    #[test]
    fn test_handle_deactivate_key_invalid_json() {
        let settings = crate::test_support::tests::create_test_settings();
        let mut req = Request::new(Method::POST, "https://test.com/admin/keys/deactivate");
        req.set_body("invalid json");

        let result = handle_deactivate_key(&settings, req);
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

        let result = handle_trusted_server_discovery(&settings, req);
        match result {
            Ok(mut resp) => {
                assert_eq!(resp.get_status(), StatusCode::OK);
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
            Err(e) => println!("Expected error in test environment: {}", e),
        }
    }
}
