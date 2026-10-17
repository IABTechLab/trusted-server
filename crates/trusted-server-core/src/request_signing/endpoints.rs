//! HTTP endpoint handlers for request signing operations.
//!
//! This module provides endpoint handlers for JWKS retrieval and signature
//! verification, and the key ID rules key creation follows.

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{Request, Response, StatusCode, header};
use serde::{Deserialize, Serialize};

use crate::error::TrustedServerError;
use crate::http_util::enforce_max_body_size;
use crate::platform::RuntimeServices;
use crate::request_signing::discovery::TrustedServerDiscovery;
use crate::request_signing::signing;
use crate::settings::Settings;

fn json_response(status: StatusCode, body: String) -> Response<EdgeBody> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime::APPLICATION_JSON.as_ref())
        .body(EdgeBody::from(body.into_bytes()))
        .expect("should build json response")
}

fn request_body_bytes(
    body: EdgeBody,
    endpoint: &str,
) -> Result<bytes::Bytes, Report<TrustedServerError>> {
    if body.is_stream() {
        return Err(Report::new(TrustedServerError::BadRequest {
            message: format!("{endpoint} request body must be buffered, not streamed"),
        }));
    }

    Ok(body.into_bytes().unwrap_or_default())
}

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
    _req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
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

    Ok(json_response(StatusCode::OK, json))
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

const VERIFY_MAX_BODY_BYTES: usize = 4096;

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
    req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let body = request_body_bytes(req.into_body(), "verify-signature")?;
    enforce_max_body_size(&body, VERIFY_MAX_BODY_BYTES, "verify-signature")?;
    let verify_req: VerifySignatureRequest =
        serde_json::from_slice(&body).change_context(TrustedServerError::Configuration {
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

    Ok(json_response(StatusCode::OK, response_json))
}

const MAX_KID_LENGTH: usize = 128;

/// Validates the structural constraints every kid must satisfy: non-empty,
/// length-bounded, and limited to a safe charset so a kid cannot smuggle a CSV
/// separator or other control characters into storage.
fn validate_kid_format(kid: &str) -> Result<(), Report<TrustedServerError>> {
    if kid.is_empty() || kid.len() > MAX_KID_LENGTH {
        return Err(Report::new(TrustedServerError::BadRequest {
            message: format!("kid must be 1..={MAX_KID_LENGTH} characters"),
        }));
    }

    if !kid
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err(Report::new(TrustedServerError::BadRequest {
            message: "kid must contain only ASCII alphanumerics, '-', '_', '.', ':'".into(),
        }));
    }

    Ok(())
}

/// Validates a kid for a new key.
///
/// Enforces the **portable-KID contract**: a new kid must be storable by every
/// platform's key-name encoder, so creation validates against the strictest
/// common denominator. Concretely the kid must start with a lowercase ASCII
/// letter, on top of the structural [`validate_kid_format`] charset checks.
///
/// That floor is currently set by the Fermyon Spin variable encoder
/// (`spin_variable_name` in the Spin adapter's `platform.rs`), which requires a
/// lowercase ASCII leading character: it would otherwise alias digit-leading keys
/// (`1foo` and `n1foo` both map to `v_n1foo`) or reject uppercase/punctuation
/// leading keys (`KidA`, `_kid`, `-kid`, `.kid`, `:kid`) at storage time.
/// Checking the floor here refuses such a kid for every platform rather than
/// leaving it to fail at storage time on Spin. [`kid_is_creatable`] exposes
/// this predicate so adapter crates can pin the contract with a test, because
/// core must never accept a kid its encoder rejects. System-generated KIDs
/// (`ts-YYYY-MM-DD`) start with `t` and are unaffected.
fn validate_kid(kid: &str) -> Result<(), Report<TrustedServerError>> {
    validate_kid_format(kid)?;

    if !kid.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(Report::new(TrustedServerError::BadRequest {
            message: "kid must start with a lowercase ASCII letter".into(),
        }));
    }

    Ok(())
}

/// Returns whether `kid` satisfies the portable-KID contract that
/// [`validate_kid`] checks for a new key.
///
/// Exposed for the tool that creates keys, which asks before it writes
/// anything, and so platform adapter crates can assert their key-name encoder
/// accepts every kid this validation admits, pinning the cross-adapter contract
/// against silent drift between this validation floor and an adapter's storage
/// encoder.
#[must_use]
pub fn kid_is_creatable(kid: &str) -> bool {
    validate_kid(kid).is_ok()
}

/// Returns whether `kid` has the shape of a stored key id, which
/// [`validate_kid_format`] checks.
///
/// Looser than [`kid_is_creatable`], so that a key created under an earlier
/// rule can still be deactivated and deleted. Exposed for the tool that
/// retires keys, which asks before it reads or writes anything.
#[must_use]
pub fn kid_is_well_formed(kid: &str) -> bool {
    validate_kid_format(kid).is_ok()
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use edgezero_core::body::Body as EdgeBody;
    use error_stack::Report;
    use http::{Method, Request as HttpRequest, StatusCode, header};

    use crate::error::IntoHttpResponse;
    use crate::platform::{
        PlatformConfigStore, PlatformError, StoreId, StoreName,
        test_support::{build_request_signing_services, build_services_with_config, noop_services},
    };

    use super::*;

    fn build_request(method: Method, uri: &str, body: Option<&str>) -> HttpRequest<EdgeBody> {
        let body = match body {
            Some(body) => EdgeBody::from(body.as_bytes().to_vec()),
            None => EdgeBody::empty(),
        };

        HttpRequest::builder()
            .method(method)
            .uri(uri)
            .body(body)
            .expect("should build request")
    }

    fn build_streaming_request(method: Method, uri: &str) -> HttpRequest<EdgeBody> {
        let stream = futures::stream::iter(vec![Bytes::from_static(b"{}")]);
        HttpRequest::builder()
            .method(method)
            .uri(uri)
            .body(EdgeBody::stream(stream))
            .expect("should build streaming request")
    }

    fn response_body_string(response: http::Response<EdgeBody>) -> String {
        String::from_utf8(
            response
                .into_body()
                .into_bytes()
                .unwrap_or_default()
                .to_vec(),
        )
        .expect("should decode response body")
    }

    fn assert_json_content_type(response: &http::Response<EdgeBody>) {
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(mime::APPLICATION_JSON.as_ref()),
            "should return application/json content type"
        );
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
        let req = build_request(
            Method::POST,
            "https://test.com/verify-signature",
            Some(&body),
        );

        let resp = handle_verify_signature(&settings, &services, req)
            .expect("should handle verification request");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_json_content_type(&resp);

        let resp_body = response_body_string(resp);
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
        let req = build_request(
            Method::POST,
            "https://test.com/verify-signature",
            Some(&body),
        );

        let resp = handle_verify_signature(&settings, &services, req)
            .expect("should handle verification request");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_json_content_type(&resp);

        let resp_body = response_body_string(resp);
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
        let req = build_request(
            Method::POST,
            "https://test.com/verify-signature",
            Some(&body),
        );

        let services = noop_services();
        let resp = handle_verify_signature(&settings, &services, req)
            .expect("should return a verification response for internal errors");

        assert_eq!(resp.status(), StatusCode::OK, "should return 200 OK");

        let resp_body = response_body_string(resp);
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

        let req = build_request(
            Method::POST,
            "https://test.com/verify-signature",
            Some("not valid json"),
        );

        let result = handle_verify_signature(&settings, &noop_services(), req);
        assert!(result.is_err(), "Malformed JSON should error");
    }

    #[test]
    fn verify_signature_rejects_oversized_body() {
        let settings = crate::test_support::tests::create_test_settings();
        let oversized = "x".repeat(VERIFY_MAX_BODY_BYTES + 1);
        let req = build_request(
            Method::POST,
            "https://test.com/verify-signature",
            Some(&oversized),
        );
        let err = handle_verify_signature(&settings, &noop_services(), req)
            .expect_err("should reject oversized body");
        assert_eq!(
            err.current_context().status_code(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "should return 413 for verify-signature body over limit"
        );
    }

    #[test]
    fn verify_signature_rejects_streaming_body() {
        let settings = crate::test_support::tests::create_test_settings();
        let req = build_streaming_request(Method::POST, "https://test.com/verify-signature");
        let err = handle_verify_signature(&settings, &noop_services(), req)
            .expect_err("should reject streaming verify body");
        assert_eq!(
            err.current_context().status_code(),
            StatusCode::BAD_REQUEST,
            "should return 400 for streaming verify body"
        );
        assert!(
            matches!(
                err.current_context(),
                TrustedServerError::BadRequest { message }
                    if message == "verify-signature request body must be buffered, not streamed"
            ),
            "should explain that verify bodies must be buffered"
        );
    }

    #[test]
    fn validate_kid_accepts_allowed_operator_supplied_ids() {
        validate_kid("azAZ09-_.:").expect("should accept allowed kid characters");
    }

    #[test]
    fn validate_kid_rejects_empty_ids() {
        let result = validate_kid("");

        assert!(result.is_err(), "should reject empty kid values");
    }

    #[test]
    fn validate_kid_rejects_overlong_ids() {
        let result = validate_kid(&"a".repeat(129));

        assert!(result.is_err(), "should reject kids longer than 128 chars");
    }

    #[test]
    fn validate_kid_rejects_csv_separator() {
        let result = validate_kid("kid-a,kid-b");

        assert!(result.is_err(), "should reject commas in kid values");
    }

    #[test]
    fn validate_kid_rejects_digit_leading_ids() {
        for kid in &["2026-key", "0abc", "9xyz", "1-key"] {
            assert!(
                validate_kid(kid).is_err(),
                "should reject digit-leading kid value: {kid}"
            );
        }
    }

    #[test]
    fn validate_kid_rejects_non_lowercase_leading_ids() {
        // The Spin variable encoder requires a lowercase ASCII leading character,
        // so uppercase- and punctuation-leading KIDs that pass validate_kid_format
        // must be rejected for a new key, which Spin could not store.
        for kid in &["KidA", "_kid", "-kid", ".kid", ":kid"] {
            assert!(
                validate_kid(kid).is_err(),
                "should reject non-lowercase-leading kid value: {kid}"
            );
            validate_kid_format(kid)
                .unwrap_or_else(|e| panic!("format check should still accept {kid}: {e:?}"));
        }
    }

    #[test]
    fn validate_kid_format_allows_digit_leading_ids() {
        // The leading-letter rule belongs to validate_kid alone, so the
        // structural check accepts a digit-leading kid that a new key may not
        // take.
        for kid in &["2026-key", "0abc", "9xyz", "1-key"] {
            validate_kid_format(kid)
                .unwrap_or_else(|e| panic!("format check should accept kid {kid}: {e:?}"));
            assert!(
                validate_kid(kid).is_err(),
                "new-key validation must still reject digit-leading kid: {kid}"
            );
        }
    }

    #[test]
    fn a_well_formed_kid_need_not_be_one_a_new_key_may_take() {
        for kid in ["2026-key", "KidA", "-kid", "ts-2026-01-01"] {
            assert!(kid_is_well_formed(kid), "should accept {kid}");
        }
        for kid in ["2026-key", "KidA", "-kid"] {
            assert!(!kid_is_creatable(kid), "should not create {kid}");
        }
        let too_long = "a".repeat(129);
        for kid in ["", "kid-a,kid-b", "kid a", "kid/a", &too_long] {
            assert!(!kid_is_well_formed(kid), "should refuse {kid:?}");
        }
    }

    #[test]
    fn test_handle_trusted_server_discovery() {
        let settings = crate::test_support::tests::create_test_settings();
        let req = build_request(
            Method::GET,
            "https://test.com/.well-known/trusted-server.json",
            None,
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
        let req = build_request(
            Method::GET,
            "https://test.com/.well-known/trusted-server.json",
            None,
        );

        let services = build_services_with_config(StubJwksConfigStore);
        let resp = handle_trusted_server_discovery(&settings, &services, req)
            .expect("should return discovery document when config store is populated");

        assert_eq!(resp.status(), StatusCode::OK, "should return 200 OK");

        let body = response_body_string(resp);
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
