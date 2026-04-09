//! Server-to-server batch sync endpoint (`POST /_ts/api/v1/batch-sync`).
//!
//! Partners send authenticated batch ID sync requests via Bearer token.
//! Each mapping associates an `ec_id` (`{64hex}.{6alnum}`)
//! with the partner's user ID. Mappings are individually validated and
//! written to the KV identity graph, with per-mapping rejection reasons
//! reported in the response.

use error_stack::{Report, ResultExt};
use fastly::http::StatusCode;
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};

use crate::error::TrustedServerError;

use super::generation::{is_valid_ec_id, normalize_ec_id_for_kv};
use super::kv::{KvIdentityGraph, UpsertResult};
use super::log_id;
use super::partner::{hash_api_key, PartnerRecord, PartnerStore};
use super::sync_pixel::RateLimiter;

const REASON_INVALID_EC_ID: &str = "invalid_ec_id";
const REASON_INVALID_PARTNER_UID: &str = "invalid_partner_uid";
const REASON_EC_ID_NOT_FOUND: &str = "ec_id_not_found";
const REASON_CONSENT_WITHDRAWN: &str = "consent_withdrawn";
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
        synced: u64,
    ) -> Result<UpsertResult, Report<TrustedServerError>>;
}

impl BatchSyncWriter for KvIdentityGraph {
    fn upsert_partner_id_if_exists(
        &self,
        ec_id: &str,
        partner_id: &str,
        uid: &str,
        synced: u64,
    ) -> Result<UpsertResult, Report<TrustedServerError>> {
        KvIdentityGraph::upsert_partner_id_if_exists(self, ec_id, partner_id, uid, synced)
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
// Auth
// ---------------------------------------------------------------------------

/// Extracts and validates a `Bearer` token from the `Authorization` header,
/// returning the authenticated [`PartnerRecord`].
fn authenticate_bearer(
    partner_store: &PartnerStore,
    req: &Request,
) -> Result<Option<PartnerRecord>, Report<TrustedServerError>> {
    let header_value = match req.get_header_str("authorization") {
        Some(v) => v.to_owned(),
        None => return Ok(None),
    };

    let token = match parse_bearer_token(&header_value) {
        Some(t) => t,
        None => return Ok(None),
    };

    let key_hash = hash_api_key(token);
    partner_store.find_by_api_key_hash(&key_hash)
}

fn parse_bearer_token(header_value: &str) -> Option<&str> {
    let mut parts = header_value.split_whitespace();
    let scheme = parts.next()?;
    let token = parts.next()?;

    if !scheme.eq_ignore_ascii_case("bearer") || token.is_empty() {
        return None;
    }
    if parts.next().is_some() {
        return None;
    }

    Some(token)
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
    partner_store: &PartnerStore,
    rate_limiter: &dyn RateLimiter,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    handle_batch_sync_with_writer(kv, partner_store, rate_limiter, &mut req)
}

fn handle_batch_sync_with_writer(
    writer: &dyn BatchSyncWriter,
    partner_store: &PartnerStore,
    rate_limiter: &dyn RateLimiter,
    req: &mut Request,
) -> Result<Response, Report<TrustedServerError>> {
    // 1. Authenticate
    let partner = match authenticate_bearer(partner_store, req)? {
        Some(p) => p,
        None => return Ok(error_response(StatusCode::UNAUTHORIZED, "invalid_token")),
    };

    // 2. Rate limit (per-partner, per-minute via batch_rate_limit)
    let rate_key = format!("batch:{}", partner.id);
    if rate_limiter.exceeded_per_minute(&rate_key, partner.batch_rate_limit)? {
        return Ok(error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_exceeded",
        ));
    }

    // 3. Parse body (with size limit to prevent OOM before validation)
    const MAX_BODY_SIZE: usize = 2 * 1024 * 1024; // 2 MB
    let body_bytes = req.take_body_bytes();
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
    let (accepted, errors) = process_mappings(writer, &partner.id, &body.mappings);

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

fn process_mappings(
    writer: &dyn BatchSyncWriter,
    partner_id: &str,
    mappings: &[SyncMapping],
) -> (usize, Vec<MappingError>) {
    let mut accepted: usize = 0;
    let mut errors = Vec::new();

    for (idx, mapping) in mappings.iter().enumerate() {
        if !is_valid_ec_id(&mapping.ec_id) {
            errors.push(MappingError {
                index: idx,
                reason: REASON_INVALID_EC_ID,
            });
            continue;
        }

        if mapping.partner_uid.is_empty() || mapping.partner_uid.len() > MAX_UID_LENGTH {
            errors.push(MappingError {
                index: idx,
                reason: REASON_INVALID_PARTNER_UID,
            });
            continue;
        }

        let ec_id = normalize_ec_id_for_kv(&mapping.ec_id);
        match writer.upsert_partner_id_if_exists(
            &ec_id,
            partner_id,
            &mapping.partner_uid,
            mapping.timestamp,
        ) {
            Ok(UpsertResult::Written | UpsertResult::Stale) => {
                accepted += 1;
            }
            Ok(UpsertResult::NotFound) => {
                errors.push(MappingError {
                    index: idx,
                    reason: REASON_EC_ID_NOT_FOUND,
                });
            }
            Ok(UpsertResult::ConsentWithdrawn) => {
                errors.push(MappingError {
                    index: idx,
                    reason: REASON_CONSENT_WITHDRAWN,
                });
            }
            Err(err) => {
                log::warn!(
                    "Batch sync KV write failed for index {idx} (ec_id '{}…'): {err:?}",
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
) -> Result<Response, Report<TrustedServerError>> {
    let body = serde_json::to_string(body).change_context(TrustedServerError::EdgeCookie {
        message: "Failed to serialize batch sync response".to_owned(),
    })?;

    Ok(Response::from_status(status)
        .with_content_type(fastly::mime::APPLICATION_JSON)
        .with_body(body))
}

fn error_response(status: StatusCode, reason: &str) -> Response {
    let body = serde_json::json!({ "error": reason });
    Response::from_status(status)
        .with_content_type(fastly::mime::APPLICATION_JSON)
        .with_body(body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use crate::error::TrustedServerError;

    // EC ID validation tests are in generation.rs (is_valid_ec_id).
    // Verify the import works here with a basic smoke test.
    #[test]
    fn is_valid_ec_id_smoke_test() {
        let valid = format!("{}.ABC123", "a".repeat(64));
        assert!(is_valid_ec_id(&valid));
        assert!(!is_valid_ec_id(&"a".repeat(64)));
    }

    #[test]
    fn parse_bearer_token_accepts_case_insensitive_scheme() {
        assert_eq!(parse_bearer_token("Bearer tok"), Some("tok"));
        assert_eq!(parse_bearer_token("bearer tok"), Some("tok"));
        assert_eq!(parse_bearer_token("BEARER tok"), Some("tok"));
    }

    #[test]
    fn parse_bearer_token_rejects_invalid_shapes() {
        assert_eq!(parse_bearer_token("Bearer"), None);
        assert_eq!(parse_bearer_token("Bearer "), None);
        assert_eq!(parse_bearer_token("Basic abc"), None);
        assert_eq!(parse_bearer_token("Bearer a b"), None);
    }

    #[test]
    fn authenticate_bearer_returns_none_for_missing_header() {
        let partner_store = PartnerStore::new("test_store");
        let req = Request::new("POST", "https://edge.example.com/_ts/api/v1/batch-sync");

        let result =
            authenticate_bearer(&partner_store, &req).expect("should not error on missing header");
        assert!(result.is_none(), "should return None without auth header");
    }

    #[test]
    fn authenticate_bearer_returns_none_for_malformed_header() {
        let partner_store = PartnerStore::new("test_store");
        let mut req = Request::new("POST", "https://edge.example.com/_ts/api/v1/batch-sync");
        req.set_header("authorization", "Basic dXNlcjpwYXNz");

        let result = authenticate_bearer(&partner_store, &req)
            .expect("should not error on malformed header");
        assert!(
            result.is_none(),
            "should return None for non-Bearer auth scheme"
        );
    }

    #[test]
    fn authenticate_bearer_returns_none_for_empty_token() {
        let partner_store = PartnerStore::new("test_store");
        let mut req = Request::new("POST", "https://edge.example.com/_ts/api/v1/batch-sync");
        req.set_header("authorization", "Bearer ");

        let result =
            authenticate_bearer(&partner_store, &req).expect("should not error on empty token");
        assert!(
            result.is_none(),
            "should return None for empty Bearer token"
        );
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
            _synced: u64,
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
        let partner_store = PartnerStore::new("test_store");
        let limiter = MockRateLimiter {
            should_exceed: false,
        };
        let req = Request::new("POST", "https://edge.example.com/_ts/api/v1/batch-sync");

        let response =
            handle_batch_sync(&kv, &partner_store, &limiter, req).expect("should return response");
        assert_eq!(
            response.get_status(),
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
                reason: REASON_EC_ID_NOT_FOUND,
            }],
        };

        let json: serde_json::Value =
            serde_json::to_value(&response).expect("should serialize batch sync response");
        assert_eq!(json["accepted"], 5);
        assert_eq!(json["rejected"], 1);
        assert_eq!(json["errors"][0]["index"], 3);
        assert_eq!(json["errors"][0]["reason"], REASON_EC_ID_NOT_FOUND);
    }
}
