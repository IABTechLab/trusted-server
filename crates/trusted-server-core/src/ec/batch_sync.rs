//! Server-to-server batch sync endpoint (`POST /_ts/api/v1/batch-sync`).
//!
//! Partners send authenticated batch ID sync requests via Bearer token.
//! Each mapping associates an `ec_id` (`{64hex}.{6alnum}`)
//! with the partner's user ID. Mappings are individually validated and
//! written to the KV identity graph, with per-mapping rejection reasons
//! reported in the response.
//!
//! Mapping timestamps are retained in the request schema for client
//! compatibility, but the EC identity graph no longer stores per-partner sync
//! timestamps. Valid mappings therefore use idempotent last-write-wins
//! semantics: unchanged UIDs are accepted without a write; different UIDs
//! replace the stored value regardless of timestamp.

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{Request, Response, StatusCode};
use serde::{Deserialize, Serialize};

use crate::error::TrustedServerError;

use super::auth::authenticate_bearer;
use super::generation::{is_valid_ec_id, normalize_ec_id_for_kv};
use super::kv::{KvIdentityGraph, UpsertResult};
use super::log_id;
use super::rate_limiter::RateLimiter;
use super::registry::PartnerRegistry;

const REASON_INVALID_EC_ID: &str = "invalid_ec_id";
const REASON_INVALID_PARTNER_UID: &str = "invalid_partner_uid";
const REASON_INELIGIBLE: &str = "ineligible";
const REASON_KV_UNAVAILABLE: &str = "kv_unavailable";

/// Maximum number of mappings allowed in a single batch request.
const MAX_BATCH_SIZE: usize = 1000;

use super::kv_types::MAX_UID_LENGTH;

trait BatchSyncWriter {
    fn upsert_partner_id_if_exists(
        &self,
        ec_id: &str,
        partner_id: &str,
        uid: &str,
    ) -> Result<UpsertResult, Report<TrustedServerError>>;
}

impl BatchSyncWriter for KvIdentityGraph {
    fn upsert_partner_id_if_exists(
        &self,
        ec_id: &str,
        partner_id: &str,
        uid: &str,
    ) -> Result<UpsertResult, Report<TrustedServerError>> {
        KvIdentityGraph::upsert_partner_id_if_exists(self, ec_id, partner_id, uid)
    }
}

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct BatchSyncRequest {
    mappings: Vec<SyncMapping>,
}

#[derive(Debug, Deserialize)]
struct SyncMapping {
    ec_id: String,
    partner_uid: String,
    // Retained for API compatibility. The EC KV body no longer stores
    // per-partner timestamps, so this does not order writes.
    #[allow(dead_code)]
    timestamp: u64,
}

#[derive(Debug, Serialize)]
struct BatchSyncResponse {
    accepted: usize,
    rejected: usize,
    errors: Vec<MappingError>,
}

#[derive(Debug, Serialize)]
struct MappingError {
    index: usize,
    reason: &'static str,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Handles `POST /_ts/api/v1/batch-sync`.
///
/// # Errors
///
/// Returns [`TrustedServerError`] on serialization or KV store failures.
pub fn handle_batch_sync(
    kv: &KvIdentityGraph,
    registry: &PartnerRegistry,
    rate_limiter: &dyn RateLimiter,
    req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    handle_batch_sync_with_writer(kv, registry, rate_limiter, req)
}

fn handle_batch_sync_with_writer(
    writer: &dyn BatchSyncWriter,
    registry: &PartnerRegistry,
    rate_limiter: &dyn RateLimiter,
    req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    // 1. Authenticate
    let Some(partner) = authenticate_bearer(registry, &req) else {
        return Ok(error_response(StatusCode::UNAUTHORIZED, "invalid_token"));
    };

    // 2. Rate limit (per-partner, per-minute via batch_rate_limit)
    let rate_key = format!("batch:{}", partner.source_domain);
    if rate_limiter.exceeded_per_minute(&rate_key, partner.batch_rate_limit)? {
        return Ok(error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_exceeded",
        ));
    }

    // 3. Parse body (with size limit to prevent OOM before validation)
    const MAX_BODY_SIZE: usize = 2 * 1024 * 1024; // 2 MB
    if content_length_exceeds_limit(&req, MAX_BODY_SIZE) {
        return Ok(error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body_too_large",
        ));
    }

    let body_bytes = req.into_body().into_bytes();
    if body_bytes.len() > MAX_BODY_SIZE {
        return Ok(error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "body_too_large",
        ));
    }
    let body: BatchSyncRequest = serde_json::from_slice(&body_bytes).map_err(|e| {
        Report::new(TrustedServerError::BadRequest {
            message: format!("Invalid request body: {e}"),
        })
    })?;

    if body.mappings.len() > MAX_BATCH_SIZE {
        return Ok(error_response(StatusCode::BAD_REQUEST, "batch_too_large"));
    }

    // 4. Process mappings with per-item validation and rejection reasons.
    let (accepted, errors) = process_mappings(writer, &partner.source_domain, &body.mappings);

    let rejected = errors.len();
    let status = if rejected > 0 {
        StatusCode::MULTI_STATUS
    } else {
        StatusCode::OK
    };

    let response_body = BatchSyncResponse {
        accepted,
        rejected,
        errors,
    };

    json_response(status, &response_body)
}

fn content_length_exceeds_limit(req: &Request<EdgeBody>, max_body_size: usize) -> bool {
    req.headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|content_length| content_length > max_body_size)
}

fn process_mappings(
    writer: &dyn BatchSyncWriter,
    partner_id: &str,
    mappings: &[SyncMapping],
) -> (usize, Vec<MappingError>) {
    let mut accepted: usize = 0;
    let mut errors = Vec::new();

    for (idx, mapping) in mappings.iter().enumerate() {
        let ec_id = normalize_ec_id_for_kv(&mapping.ec_id);
        if !is_valid_ec_id(&ec_id) {
            errors.push(MappingError {
                index: idx,
                reason: REASON_INVALID_EC_ID,
            });
            continue;
        }

        if mapping.partner_uid.trim().is_empty() || mapping.partner_uid.len() > MAX_UID_LENGTH {
            errors.push(MappingError {
                index: idx,
                reason: REASON_INVALID_PARTNER_UID,
            });
            continue;
        }
        match writer.upsert_partner_id_if_exists(&ec_id, partner_id, &mapping.partner_uid) {
            Ok(UpsertResult::Written | UpsertResult::Unchanged) => {
                accepted += 1;
            }
            Ok(UpsertResult::NotFound | UpsertResult::ConsentWithdrawn) => {
                errors.push(MappingError {
                    index: idx,
                    reason: REASON_INELIGIBLE,
                });
            }
            Err(err) => {
                log::warn!(
                    "Batch sync KV write failed for index {idx} (ec_id '{}'): {err:?}",
                    log_id(&mapping.ec_id),
                );
                errors.push(MappingError {
                    index: idx,
                    reason: REASON_KV_UNAVAILABLE,
                });
                // Abort remaining mappings on infrastructure failure.
                for remaining_idx in (idx + 1)..mappings.len() {
                    errors.push(MappingError {
                        index: remaining_idx,
                        reason: REASON_KV_UNAVAILABLE,
                    });
                }
                break;
            }
        }
    }

    (accepted, errors)
}

fn json_response<T: serde::Serialize>(
    status: StatusCode,
    body: &T,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    let body_str = serde_json::to_string(body).change_context(TrustedServerError::EdgeCookie {
        message: "Failed to serialize batch sync response".to_owned(),
    })?;
    Ok(Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(EdgeBody::from(body_str))
        .expect("should build json response"))
}

fn error_response(status: StatusCode, reason: &str) -> Response<EdgeBody> {
    let body = serde_json::json!({ "error": reason });
    Response::builder()
        .status(status)
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(EdgeBody::from(body.to_string()))
        .expect("should build error response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use crate::error::TrustedServerError;
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;

    // EC ID validation tests are in generation.rs (is_valid_ec_id).
    // Verify the import works here with a basic smoke test.
    #[test]
    fn is_valid_ec_id_smoke_test() {
        let valid = format!("{}.ABC123", "a".repeat(64));
        assert!(is_valid_ec_id(&valid));
        assert!(!is_valid_ec_id(&"a".repeat(64)));
    }

    struct MockRateLimiter {
        should_exceed: bool,
    }

    impl RateLimiter for MockRateLimiter {
        fn exceeded(
            &self,
            _key: &str,
            _hourly_limit: u32,
        ) -> Result<bool, Report<TrustedServerError>> {
            Ok(self.should_exceed)
        }

        fn exceeded_per_minute(
            &self,
            _key: &str,
            _per_minute_limit: u32,
        ) -> Result<bool, Report<TrustedServerError>> {
            Ok(self.should_exceed)
        }
    }

    struct MockWriter {
        results: std::cell::RefCell<VecDeque<Result<UpsertResult, Report<TrustedServerError>>>>,
    }

    impl MockWriter {
        fn new(results: Vec<Result<UpsertResult, Report<TrustedServerError>>>) -> Self {
            Self {
                results: std::cell::RefCell::new(results.into()),
            }
        }
    }

    impl BatchSyncWriter for MockWriter {
        fn upsert_partner_id_if_exists(
            &self,
            _ec_id: &str,
            _partner_id: &str,
            _uid: &str,
        ) -> Result<UpsertResult, Report<TrustedServerError>> {
            self.results
                .borrow_mut()
                .pop_front()
                .expect("should provide mock result for each mapping")
        }
    }

    fn mapping(ec_id: &str, partner_uid: &str, timestamp: u64) -> SyncMapping {
        SyncMapping {
            ec_id: ec_id.to_owned(),
            partner_uid: partner_uid.to_owned(),
            timestamp,
        }
    }

    fn make_test_partner(source_domain: &str, api_token: &str) -> EcPartner {
        EcPartner {
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: Redacted::new(api_token.to_owned()),
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        }
    }

    fn authorized_batch_request(body: &str) -> Request<EdgeBody> {
        Request::builder()
            .method("POST")
            .uri("https://edge.example.com/_ts/api/v1/batch-sync")
            .header("authorization", "Bearer test-token-32-bytes-minimum-value")
            .body(EdgeBody::from(body.to_owned()))
            .expect("should build authorized batch request")
    }

    fn test_registry() -> PartnerRegistry {
        let partners = vec![make_test_partner(
            "ssp.example.com",
            "test-token-32-bytes-minimum-value",
        )];
        PartnerRegistry::from_config(&partners).expect("should build registry")
    }

    #[test]
    fn content_length_exceeds_limit_detects_oversized_header() {
        let req = Request::builder()
            .method("POST")
            .uri("https://edge.example.com/_ts/api/v1/batch-sync")
            .header("authorization", "Bearer test-token-32-bytes-minimum-value")
            .header("content-length", "2097153")
            .body(EdgeBody::from("{}"))
            .expect("should build test request");

        assert!(
            content_length_exceeds_limit(&req, 2 * 1024 * 1024),
            "should reject oversized content-length before reading body"
        );
    }

    #[test]
    fn content_length_exceeds_limit_ignores_missing_or_malformed_header() {
        let missing = authorized_batch_request("{}");
        let malformed = Request::builder()
            .method("POST")
            .uri("https://edge.example.com/_ts/api/v1/batch-sync")
            .header("authorization", "Bearer test-token-32-bytes-minimum-value")
            .header("content-length", "not-a-number")
            .body(EdgeBody::from("{}"))
            .expect("should build test request");

        assert!(
            !content_length_exceeds_limit(&missing, 2 * 1024 * 1024),
            "missing content-length should fall back to post-read size check"
        );
        assert!(
            !content_length_exceeds_limit(&malformed, 2 * 1024 * 1024),
            "malformed content-length should fall back to post-read size check"
        );
    }

    #[test]
    fn handle_batch_sync_rejects_oversized_content_length_before_body_parse() {
        let writer = MockWriter::new(vec![]);
        let registry = test_registry();
        let limiter = MockRateLimiter {
            should_exceed: false,
        };
        let req = Request::builder()
            .method("POST")
            .uri("https://edge.example.com/_ts/api/v1/batch-sync")
            .header("authorization", "Bearer test-token-32-bytes-minimum-value")
            .header("content-length", "2097153")
            .body(EdgeBody::from("not-json"))
            .expect("should build test request");

        let response = handle_batch_sync_with_writer(&writer, &registry, &limiter, req)
            .expect("should return oversized response");

        assert_eq!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "should reject from content-length before parsing body"
        );
    }

    #[test]
    fn handle_batch_sync_uses_post_read_limit_for_malformed_content_length() {
        let writer = MockWriter::new(vec![]);
        let registry = test_registry();
        let limiter = MockRateLimiter {
            should_exceed: false,
        };
        let oversized_body = "{".repeat((2 * 1024 * 1024) + 1);
        let req = Request::builder()
            .method("POST")
            .uri("https://edge.example.com/_ts/api/v1/batch-sync")
            .header("authorization", "Bearer test-token-32-bytes-minimum-value")
            .header("content-length", "not-a-number")
            .body(EdgeBody::from(oversized_body))
            .expect("should build test request");

        let response = handle_batch_sync_with_writer(&writer, &registry, &limiter, req)
            .expect("should return oversized response");

        assert_eq!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "should reject oversized body even when content-length is malformed"
        );
    }

    #[test]
    fn process_mappings_returns_multistatus_errors_per_mapping() {
        let writer = MockWriter::new(vec![Ok(UpsertResult::Written)]);
        let mappings = vec![
            mapping("x", "u1", 1),
            mapping(&format!("{}.ABC123", "a".repeat(64)), "", 1),
            mapping(&format!("{}.ABC123", "a".repeat(64)), "u3", 1),
        ];

        let (accepted, errors) = process_mappings(&writer, "partner", &mappings);

        assert_eq!(accepted, 1, "should count successful writes as accepted");
        assert_eq!(errors.len(), 2, "should reject invalid mappings only");
        assert_eq!(errors[0].index, 0);
        assert_eq!(errors[0].reason, REASON_INVALID_EC_ID);
        assert_eq!(errors[1].index, 1);
        assert_eq!(errors[1].reason, REASON_INVALID_PARTNER_UID);
    }

    #[test]
    fn process_mappings_aborts_on_kv_unavailable() {
        let writer = MockWriter::new(vec![
            Ok(UpsertResult::Written),
            Err(Report::new(TrustedServerError::KvStore {
                store_name: "ec_store".to_owned(),
                message: "down".to_owned(),
            })),
            Ok(UpsertResult::Written),
        ]);

        let mappings = vec![
            mapping(&format!("{}.ABC123", "a".repeat(64)), "u1", 1),
            mapping(&format!("{}.ABC123", "b".repeat(64)), "u2", 1),
            mapping(&format!("{}.ABC123", "c".repeat(64)), "u3", 1),
        ];

        let (accepted, errors) = process_mappings(&writer, "partner", &mappings);

        assert_eq!(accepted, 1, "should keep accepted count before failure");
        assert_eq!(
            errors.len(),
            2,
            "should mark current and remaining as unavailable"
        );
        assert_eq!(errors[0].index, 1);
        assert_eq!(errors[0].reason, REASON_KV_UNAVAILABLE);
        assert_eq!(errors[1].index, 2);
        assert_eq!(errors[1].reason, REASON_KV_UNAVAILABLE);
    }

    #[test]
    fn handle_batch_sync_rejects_missing_auth() {
        let kv = KvIdentityGraph::new("test_store");
        let registry = PartnerRegistry::empty();
        let limiter = MockRateLimiter {
            should_exceed: false,
        };
        let req = Request::builder()
            .method("POST")
            .uri("https://edge.example.com/_ts/api/v1/batch-sync")
            .body(EdgeBody::empty())
            .expect("should build test request");

        let response =
            handle_batch_sync(&kv, &registry, &limiter, req).expect("should return response");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "should return 401 for missing auth"
        );
    }

    #[test]
    fn batch_sync_request_deserializes_correctly() {
        let json = r#"{"mappings": [{"ec_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123", "partner_uid": "u1", "timestamp": 100}]}"#;
        let parsed: BatchSyncRequest =
            serde_json::from_str(json).expect("should deserialize batch sync request");
        assert_eq!(parsed.mappings.len(), 1);
        assert_eq!(
            parsed.mappings[0].ec_id,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123"
        );
        assert_eq!(parsed.mappings[0].partner_uid, "u1");
        assert_eq!(parsed.mappings[0].timestamp, 100);
    }

    #[test]
    fn batch_sync_request_rejects_missing_timestamp() {
        let json = r#"{"mappings": [{"ec_id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.ABC123", "partner_uid": "u2"}]}"#;
        let result = serde_json::from_str::<BatchSyncRequest>(json);
        assert!(
            result.is_err(),
            "should reject mapping without required timestamp"
        );
    }

    #[test]
    fn batch_sync_response_serializes_correctly() {
        let response = BatchSyncResponse {
            accepted: 5,
            rejected: 1,
            errors: vec![MappingError {
                index: 3,
                reason: REASON_INELIGIBLE,
            }],
        };

        let json: serde_json::Value =
            serde_json::to_value(&response).expect("should serialize batch sync response");
        assert_eq!(json["accepted"], 5);
        assert_eq!(json["rejected"], 1);
        assert_eq!(json["errors"][0]["index"], 3);
        assert_eq!(json["errors"][0]["reason"], REASON_INELIGIBLE);
    }

    #[test]
    fn process_mappings_collapses_missing_and_withdrawn_to_ineligible() {
        let writer = MockWriter::new(vec![
            Ok(UpsertResult::NotFound),
            Ok(UpsertResult::ConsentWithdrawn),
        ]);
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mappings = vec![mapping(&ec_id, "uid-1", 100), mapping(&ec_id, "uid-2", 101)];

        let (accepted, errors) = process_mappings(&writer, "partner", &mappings);

        assert_eq!(accepted, 0, "should not accept ineligible mappings");
        assert_eq!(errors.len(), 2, "should report both errors");
        assert_eq!(errors[0].index, 0);
        assert_eq!(errors[0].reason, REASON_INELIGIBLE);
        assert_eq!(errors[1].index, 1);
        assert_eq!(errors[1].reason, REASON_INELIGIBLE);
    }

    #[test]
    fn process_mappings_counts_unchanged_as_accepted() {
        let writer = MockWriter::new(vec![Ok(UpsertResult::Unchanged)]);
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mappings = vec![mapping(&ec_id, "uid-1", 100)];

        let (accepted, errors) = process_mappings(&writer, "partner", &mappings);

        assert_eq!(accepted, 1, "should count unchanged mappings as accepted");
        assert!(
            errors.is_empty(),
            "should report no errors for unchanged mappings"
        );
    }

    #[test]
    fn process_mappings_does_not_order_by_timestamp() {
        let writer = MockWriter::new(vec![Ok(UpsertResult::Written), Ok(UpsertResult::Written)]);
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mappings = vec![
            mapping(&ec_id, "uid-new", 200),
            mapping(&ec_id, "uid-old", 100),
        ];

        let (accepted, errors) = process_mappings(&writer, "partner", &mappings);

        assert_eq!(
            accepted, 2,
            "timestamps are compatibility fields and should not reject older mappings"
        );
        assert!(errors.is_empty(), "should accept valid mappings");
    }
}
