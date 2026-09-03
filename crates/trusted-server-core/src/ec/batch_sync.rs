//! Server-to-server batch sync endpoint (`POST /_ts/api/v1/batch-sync`).
//!
//! Partners send authenticated batch ID sync requests via Bearer token.
//! Each mapping associates an `ec_id` (`{64hex}.{6alnum}`)
//! with the partner's user ID. Mappings are individually validated, then valid
//! mappings are grouped by normalized EC ID before one call to the KV update
//! path per group.
//! Responses still report outcomes per original mapping index.
//!
//! Mapping timestamps are retained in the request schema for client
//! compatibility, but the EC identity graph no longer stores per-partner sync
//! timestamps. The last valid mapping in request order supplies each group's
//! UID; unchanged UIDs are accepted without a write, and different UIDs replace
//! the stored value regardless of timestamp.

use std::collections::HashMap;

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

/// Valid mappings sharing one normalized EC ID.
///
/// `indexes` stays in request order, while the groups themselves stay ordered
/// by first valid occurrence. The lookup map used to locate this structure is
/// never used for processing order.
struct MappingGroup {
    ec_id: String,
    partner_uid: String,
    indexes: Vec<usize>,
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

    let body_bytes = req.into_body().into_bytes().unwrap_or_default();
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

/// Validates all mappings, then processes each normalized EC ID once.
///
/// Successful and eligibility outcomes fan out to every valid input in a
/// group. On infrastructure failure, the failing group and all unprocessed
/// valid groups are rejected as unavailable; already validated invalid inputs
/// keep their specific errors. Errors are sorted by original input index.
fn process_mappings(
    writer: &dyn BatchSyncWriter,
    partner_id: &str,
    mappings: &[SyncMapping],
) -> (usize, Vec<MappingError>) {
    let mut errors = Vec::new();
    let mut groups: Vec<MappingGroup> = Vec::new();
    let mut group_indexes: HashMap<String, usize> = HashMap::new();

    // Validate all inputs before beginning KV work. The vector preserves group
    // order; the map only locates an existing group in constant time.
    for (index, mapping) in mappings.iter().enumerate() {
        let ec_id = normalize_ec_id_for_kv(&mapping.ec_id);
        if !is_valid_ec_id(&ec_id) {
            errors.push(MappingError {
                index,
                reason: REASON_INVALID_EC_ID,
            });
            continue;
        }

        if mapping.partner_uid.trim().is_empty() || mapping.partner_uid.len() > MAX_UID_LENGTH {
            errors.push(MappingError {
                index,
                reason: REASON_INVALID_PARTNER_UID,
            });
            continue;
        }

        if let Some(&group_index) = group_indexes.get(&ec_id) {
            let group = &mut groups[group_index];
            group.partner_uid.clone_from(&mapping.partner_uid);
            group.indexes.push(index);
        } else {
            group_indexes.insert(ec_id.clone(), groups.len());
            groups.push(MappingGroup {
                ec_id,
                partner_uid: mapping.partner_uid.clone(),
                indexes: vec![index],
            });
        }
    }

    let mut accepted = 0;
    for (group_index, group) in groups.iter().enumerate() {
        match writer.upsert_partner_id_if_exists(&group.ec_id, partner_id, &group.partner_uid) {
            Ok(UpsertResult::Written | UpsertResult::Unchanged) => {
                accepted += group.indexes.len();
            }
            Ok(UpsertResult::NotFound | UpsertResult::ConsentWithdrawn) => {
                errors.extend(group.indexes.iter().map(|&index| MappingError {
                    index,
                    reason: REASON_INELIGIBLE,
                }));
            }
            Err(err) => {
                log::warn!(
                    "Batch sync KV write failed for group starting at index {} (ec_id '{}'): {err:?}",
                    group.indexes[0],
                    log_id(&group.ec_id),
                );
                for unavailable_group in &groups[group_index..] {
                    errors.extend(unavailable_group.indexes.iter().map(|&index| MappingError {
                        index,
                        reason: REASON_KV_UNAVAILABLE,
                    }));
                }
                break;
            }
        }
    }

    errors.sort_by_key(|error| error.index);
    debug_assert_eq!(accepted + errors.len(), mappings.len());
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

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct WriterCall {
        ec_id: String,
        partner_id: String,
        uid: String,
    }

    struct MockWriter {
        results: std::cell::RefCell<VecDeque<Result<UpsertResult, Report<TrustedServerError>>>>,
        calls: std::cell::RefCell<Vec<WriterCall>>,
    }

    impl MockWriter {
        fn new(results: Vec<Result<UpsertResult, Report<TrustedServerError>>>) -> Self {
            Self {
                results: std::cell::RefCell::new(results.into()),
                calls: std::cell::RefCell::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<WriterCall> {
            self.calls.borrow().clone()
        }
    }

    impl BatchSyncWriter for MockWriter {
        fn upsert_partner_id_if_exists(
            &self,
            ec_id: &str,
            partner_id: &str,
            uid: &str,
        ) -> Result<UpsertResult, Report<TrustedServerError>> {
            self.calls.borrow_mut().push(WriterCall {
                ec_id: ec_id.to_owned(),
                partner_id: partner_id.to_owned(),
                uid: uid.to_owned(),
            });
            self.results
                .borrow_mut()
                .pop_front()
                .expect("should provide mock result for each group")
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

    fn response_json(response: Response<EdgeBody>) -> serde_json::Value {
        let body = response
            .into_body()
            .into_bytes()
            .expect("should contain batch-sync response");
        serde_json::from_slice(&body).expect("should serialize batch-sync response")
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
        let kv = KvIdentityGraph::failing("test_store");
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
        let missing_ec_id = format!("{}.ABC123", "a".repeat(64));
        let withdrawn_ec_id = format!("{}.ABC123", "b".repeat(64));
        let mappings = vec![
            mapping(&missing_ec_id, "uid-1", 100),
            mapping(&withdrawn_ec_id, "uid-2", 101),
        ];

        let (accepted, errors) = process_mappings(&writer, "partner", &mappings);

        assert_eq!(accepted, 0, "should not accept ineligible mappings");
        assert_eq!(errors.len(), 2, "should report both errors");
        assert_eq!(errors[0].index, 0);
        assert_eq!(errors[0].reason, REASON_INELIGIBLE);
        assert_eq!(errors[1].index, 1);
        assert_eq!(errors[1].reason, REASON_INELIGIBLE);
        assert_eq!(writer.calls().len(), 2, "should exercise both outcomes");
    }

    #[test]
    fn process_mappings_fans_out_unchanged_to_group_members() {
        let writer = MockWriter::new(vec![Ok(UpsertResult::Unchanged)]);
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mappings = vec![mapping(&ec_id, "uid-1", 100), mapping(&ec_id, "uid-1", 101)];

        let (accepted, errors) = process_mappings(&writer, "partner", &mappings);

        assert_eq!(accepted, 2, "should accept every unchanged group member");
        assert!(
            errors.is_empty(),
            "should report no errors for unchanged mappings"
        );
        assert_eq!(writer.calls().len(), 1, "should call once for the group");
    }

    #[test]
    fn process_mappings_does_not_order_by_timestamp() {
        let writer = MockWriter::new(vec![Ok(UpsertResult::Written)]);
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
        assert_eq!(
            writer.calls(),
            vec![WriterCall {
                ec_id,
                partner_id: "partner".to_owned(),
                uid: "uid-old".to_owned(),
            }],
            "should persist the last valid UID with one writer call"
        );
    }

    #[test]
    fn process_mappings_groups_normalized_ids_in_first_occurrence_order() {
        let writer = MockWriter::new(vec![Ok(UpsertResult::Written), Ok(UpsertResult::Unchanged)]);
        let ec_id_a = format!("{}.ABC123", "a".repeat(64));
        let ec_id_a_upper = format!("{}.ABC123", "A".repeat(64));
        let ec_id_b = format!("{}.ABC123", "b".repeat(64));
        let mappings = vec![
            mapping(&ec_id_a, "a-first", 1),
            mapping(&ec_id_b, "b-only", 2),
            mapping(&ec_id_a_upper, "a-last", 3),
        ];

        let (accepted, errors) = process_mappings(&writer, "partner", &mappings);

        assert_eq!(accepted, 3, "should accept every valid group member");
        assert!(errors.is_empty(), "should report no errors");
        assert_eq!(
            writer.calls(),
            vec![
                WriterCall {
                    ec_id: ec_id_a,
                    partner_id: "partner".to_owned(),
                    uid: "a-last".to_owned(),
                },
                WriterCall {
                    ec_id: ec_id_b,
                    partner_id: "partner".to_owned(),
                    uid: "b-only".to_owned(),
                },
            ],
            "should make one ordered call per normalized EC ID"
        );
    }

    #[test]
    fn process_mappings_keeps_suffix_case_distinct() {
        let writer = MockWriter::new(vec![Ok(UpsertResult::Written), Ok(UpsertResult::Written)]);
        let upper_suffix = format!("{}.ABC123", "a".repeat(64));
        let mixed_suffix = format!("{}.AbC123", "a".repeat(64));
        let mappings = vec![
            mapping(&upper_suffix, "upper", 1),
            mapping(&mixed_suffix, "mixed", 2),
        ];

        let (accepted, errors) = process_mappings(&writer, "partner", &mappings);

        assert_eq!(accepted, 2, "should accept both distinct EC IDs");
        assert!(errors.is_empty(), "should report no errors");
        assert_eq!(
            writer.calls(),
            vec![
                WriterCall {
                    ec_id: upper_suffix,
                    partner_id: "partner".to_owned(),
                    uid: "upper".to_owned(),
                },
                WriterCall {
                    ec_id: mixed_suffix,
                    partner_id: "partner".to_owned(),
                    uid: "mixed".to_owned(),
                },
            ],
            "normalization must preserve suffix case"
        );
    }

    #[test]
    fn process_mappings_invalid_duplicate_does_not_replace_last_valid_uid() {
        let writer = MockWriter::new(vec![Ok(UpsertResult::Written)]);
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mappings = vec![
            mapping(&ec_id, "first", 1),
            mapping(&ec_id, "last", 2),
            mapping(&ec_id, "   ", 3),
        ];

        let (accepted, errors) = process_mappings(&writer, "partner", &mappings);

        assert_eq!(accepted, 2, "should accept valid group members");
        assert_eq!(errors.len(), 1, "should retain the invalid UID error");
        assert_eq!(errors[0].index, 2, "should retain original error index");
        assert_eq!(errors[0].reason, REASON_INVALID_PARTNER_UID);
        assert_eq!(
            writer.calls()[0].uid,
            "last",
            "invalid duplicates must not replace the final valid UID"
        );
    }

    #[test]
    fn process_mappings_fans_out_ineligible_outcomes_to_group_members() {
        let writer = MockWriter::new(vec![
            Ok(UpsertResult::NotFound),
            Ok(UpsertResult::ConsentWithdrawn),
        ]);
        let ec_id_a = format!("{}.ABC123", "a".repeat(64));
        let ec_id_b = format!("{}.ABC123", "b".repeat(64));
        let mappings = vec![
            mapping(&ec_id_a, "a-1", 1),
            mapping(&ec_id_b, "b-1", 2),
            mapping(&ec_id_a, "a-2", 3),
            mapping(&ec_id_b, "b-2", 4),
        ];

        let (accepted, errors) = process_mappings(&writer, "partner", &mappings);

        assert_eq!(accepted, 0, "should reject all ineligible group members");
        assert_eq!(
            errors.len(),
            mappings.len(),
            "should account for every input"
        );
        assert_eq!(
            errors.iter().map(|error| error.index).collect::<Vec<_>>(),
            vec![0, 1, 2, 3],
            "should sort errors by input index"
        );
        assert!(
            errors.iter().all(|error| error.reason == REASON_INELIGIBLE),
            "should fan out ineligible outcomes"
        );
        assert_eq!(writer.calls().len(), 2, "should call once per group");
    }

    #[test]
    fn process_mappings_aborts_by_group_and_preserves_sorted_accounting() {
        let writer = MockWriter::new(vec![
            Ok(UpsertResult::Written),
            Err(Report::new(TrustedServerError::KvStore {
                store_name: "ec_store".to_owned(),
                message: "down".to_owned(),
            })),
        ]);
        let ec_id_a = format!("{}.ABC123", "a".repeat(64));
        let ec_id_b = format!("{}.ABC123", "b".repeat(64));
        let ec_id_c = format!("{}.ABC123", "c".repeat(64));
        let mappings = vec![
            mapping("invalid", "bad-id", 1),
            mapping(&ec_id_a, "a-first", 2),
            mapping(&ec_id_b, "b-only", 3),
            mapping(&ec_id_a, "a-last", 4),
            mapping(&ec_id_c, "c-only", 5),
            mapping(&ec_id_c, "", 6),
        ];

        let (accepted, errors) = process_mappings(&writer, "partner", &mappings);

        assert_eq!(
            accepted, 2,
            "successful groups should accept every member, including later duplicates"
        );
        assert_eq!(
            errors
                .iter()
                .map(|error| (error.index, error.reason))
                .collect::<Vec<_>>(),
            vec![
                (0, REASON_INVALID_EC_ID),
                (2, REASON_KV_UNAVAILABLE),
                (4, REASON_KV_UNAVAILABLE),
                (5, REASON_INVALID_PARTNER_UID),
            ],
            "should preserve validation errors and fan out failed/unprocessed groups in input order"
        );
        assert_eq!(
            accepted + errors.len(),
            mappings.len(),
            "should account for every input exactly once"
        );
        assert_eq!(
            writer.calls().len(),
            2,
            "should stop after the failing group"
        );
    }

    #[test]
    fn handle_batch_sync_reports_grouped_success_and_rejection_counts() {
        let registry = test_registry();
        let limiter = MockRateLimiter {
            should_exceed: false,
        };
        let ec_id_a = format!("{}.ABC123", "a".repeat(64));
        let ec_id_b = format!("{}.ABC123", "b".repeat(64));
        let success_writer = MockWriter::new(vec![Ok(UpsertResult::Written)]);
        let success_body = format!(
            r#"{{"mappings":[{{"ec_id":"{ec_id_a}","partner_uid":"one","timestamp":1}},{{"ec_id":"{ec_id_a}","partner_uid":"two","timestamp":2}}]}}"#
        );
        let success_response = handle_batch_sync_with_writer(
            &success_writer,
            &registry,
            &limiter,
            authorized_batch_request(&success_body),
        )
        .expect("should return success response");
        assert_eq!(success_response.status(), StatusCode::OK);
        let success_body = success_response
            .into_body()
            .into_bytes()
            .expect("should contain grouped success response");
        let success_json: serde_json::Value = serde_json::from_slice(&success_body)
            .expect("should serialize grouped success response");
        assert_eq!(success_json["accepted"], 2);
        assert_eq!(success_json["rejected"], 0);

        let rejected_writer = MockWriter::new(vec![Ok(UpsertResult::NotFound)]);
        let rejected_body = format!(
            r#"{{"mappings":[{{"ec_id":"{ec_id_b}","partner_uid":"one","timestamp":1}},{{"ec_id":"{ec_id_b}","partner_uid":"two","timestamp":2}}]}}"#
        );
        let rejected_response = handle_batch_sync_with_writer(
            &rejected_writer,
            &registry,
            &limiter,
            authorized_batch_request(&rejected_body),
        )
        .expect("should return multi-status response");
        assert_eq!(rejected_response.status(), StatusCode::MULTI_STATUS);
        let rejected_body = rejected_response
            .into_body()
            .into_bytes()
            .expect("should contain grouped multi-status response");
        let rejected_json: serde_json::Value = serde_json::from_slice(&rejected_body)
            .expect("should serialize grouped multi-status response");
        assert_eq!(rejected_json["accepted"], 0);
        assert_eq!(rejected_json["rejected"], 2);
        assert_eq!(
            rejected_json["errors"],
            serde_json::json!([
                {"index": 0, "reason": REASON_INELIGIBLE},
                {"index": 1, "reason": REASON_INELIGIBLE},
            ])
        );
    }

    #[test]
    fn handle_batch_sync_reports_validation_errors_without_writer_calls() {
        let writer = MockWriter::new(vec![]);
        let registry = test_registry();
        let limiter = MockRateLimiter {
            should_exceed: false,
        };
        let valid_ec_id = format!("{}.ABC123", "a".repeat(64));
        let body = format!(
            r#"{{"mappings":[{{"ec_id":"invalid","partner_uid":"one","timestamp":1}},{{"ec_id":"{valid_ec_id}","partner_uid":"","timestamp":2}}]}}"#
        );

        let response = handle_batch_sync_with_writer(
            &writer,
            &registry,
            &limiter,
            authorized_batch_request(&body),
        )
        .expect("should return validation response");

        assert_eq!(response.status(), StatusCode::MULTI_STATUS);
        let response = response_json(response);
        assert_eq!(response["accepted"], 0);
        assert_eq!(response["rejected"], 2);
        assert_eq!(
            response["errors"],
            serde_json::json!([
                {"index": 0, "reason": REASON_INVALID_EC_ID},
                {"index": 1, "reason": REASON_INVALID_PARTNER_UID},
            ])
        );
        assert!(
            writer.calls().is_empty(),
            "invalid-only requests should not call the writer"
        );
    }

    #[test]
    fn handle_batch_sync_reports_grouped_infrastructure_failure() {
        let writer = MockWriter::new(vec![
            Ok(UpsertResult::Written),
            Err(Report::new(TrustedServerError::KvStore {
                store_name: "ec_store".to_owned(),
                message: "down".to_owned(),
            })),
        ]);
        let registry = test_registry();
        let limiter = MockRateLimiter {
            should_exceed: false,
        };
        let ec_id_a = format!("{}.ABC123", "a".repeat(64));
        let ec_id_b = format!("{}.ABC123", "b".repeat(64));
        let ec_id_c = format!("{}.ABC123", "c".repeat(64));
        let body = format!(
            r#"{{"mappings":[{{"ec_id":"{ec_id_a}","partner_uid":"a-first","timestamp":1}},{{"ec_id":"{ec_id_b}","partner_uid":"b","timestamp":2}},{{"ec_id":"{ec_id_a}","partner_uid":"a-last","timestamp":3}},{{"ec_id":"{ec_id_c}","partner_uid":"c","timestamp":4}}]}}"#
        );

        let response = handle_batch_sync_with_writer(
            &writer,
            &registry,
            &limiter,
            authorized_batch_request(&body),
        )
        .expect("should return infrastructure failure response");

        assert_eq!(response.status(), StatusCode::MULTI_STATUS);
        let response = response_json(response);
        assert_eq!(response["accepted"], 2);
        assert_eq!(response["rejected"], 2);
        assert_eq!(
            response["errors"],
            serde_json::json!([
                {"index": 1, "reason": REASON_KV_UNAVAILABLE},
                {"index": 3, "reason": REASON_KV_UNAVAILABLE},
            ])
        );
        assert_eq!(
            writer.calls(),
            vec![
                WriterCall {
                    ec_id: ec_id_a,
                    partner_id: "ssp.example.com".to_owned(),
                    uid: "a-last".to_owned(),
                },
                WriterCall {
                    ec_id: ec_id_b,
                    partner_id: "ssp.example.com".to_owned(),
                    uid: "b".to_owned(),
                },
            ],
            "should stop after the failing group and accept A's later duplicate"
        );
    }
}
