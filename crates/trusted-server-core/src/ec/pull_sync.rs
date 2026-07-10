//! Pull sync background dispatch.
//!
//! Launches partner pull-sync requests for organic traffic after the client
//! response has been sent. Dispatch is best-effort and never affects client
//! response status.
//!
//! Pull sync currently fills missing partner UIDs only. Once a partner UID is
//! present in the EC identity entry, it is not periodically refreshed because
//! the entry no longer stores per-partner sync timestamps.

use edgezero_core::body::Body as EdgeBody;
use http::{header, Method, StatusCode};
use serde::Deserialize;
use url::Url;

use crate::platform::{
    PlatformBackendSpec, PlatformHttpRequest, PlatformPendingRequest, PlatformResponse,
    RuntimeServices, DEFAULT_FIRST_BYTE_TIMEOUT,
};
use crate::settings::Settings;

use super::generation::{ec_hash, is_valid_ec_id};
use super::kv::{KvIdentityGraph, PartnerIdUpdate};
use super::kv_types::KvEntry;
use super::rate_limiter::RateLimiter;
use super::registry::{PartnerConfig, PartnerRegistry};

// `current_timestamp` is defined in the parent `ec` module.
use super::current_timestamp;
use super::EcContext;
use super::EcKvSnapshot;

/// Inputs needed to dispatch pull sync after response flush.
#[derive(Debug, Clone)]
pub struct PullSyncContext {
    ec_id: String,
    snapshot: EcKvSnapshot,
}

impl PullSyncContext {
    /// Returns the EC ID for the request.
    #[must_use]
    pub fn ec_id(&self) -> &str {
        &self.ec_id
    }
}

struct InFlightPull {
    source_domain: String,
    pending: PlatformPendingRequest,
}

#[derive(Debug, Deserialize)]
struct PullSyncResponse {
    uid: Option<String>,
}

/// Builds post-send pull-sync context from the route EC context.
///
/// Returns `None` when consent denies EC or there is no active EC ID.
#[must_use]
pub fn build_pull_sync_context(ec_context: &EcContext) -> Option<PullSyncContext> {
    if !ec_context.ec_allowed() {
        return None;
    }

    let ec_id_ref = ec_context.ec_value()?;
    if !is_valid_ec_id(ec_id_ref) {
        log::debug!("Pull sync: skipping dispatch because active EC ID is invalid format");
        return None;
    }

    let ec_id = ec_id_ref.to_owned();
    let snapshot = ec_context.kv_snapshot().clone();
    Some(PullSyncContext { ec_id, snapshot })
}

/// Dispatches partner pull-sync requests in the background.
///
/// This function is best-effort: all errors are logged and swallowed.
///
/// # Panics
///
/// Panics if the HTTP request builder produces an invalid request, which
/// cannot happen with the hardcoded method and well-formed URI used here.
pub fn dispatch_pull_sync(
    settings: &Settings,
    kv: &KvIdentityGraph,
    registry: &PartnerRegistry,
    rate_limiter: &dyn RateLimiter,
    context: &PullSyncContext,
    services: &RuntimeServices,
) {
    let now = current_timestamp();
    let Some(kv_entry) = context.snapshot.entry_for(context.ec_id()) else {
        return;
    };
    if !kv_entry.consent.ok {
        return;
    }

    let mut pull_partners = registry.pull_enabled_partners();

    // Sort by source domain for deterministic ordering, then apply a rotating
    // hourly offset so that different partners get dispatch priority (§10.3).
    pull_partners.sort_by(|a, b| a.source_domain.cmp(&b.source_domain));

    log::debug!(
        "Pull sync: {} pull-enabled partners after filtering",
        pull_partners.len(),
    );

    if pull_partners.is_empty() {
        return;
    }

    // Rotate the partner list so that the starting partner changes each
    // hour. This ensures fair distribution when max_concurrency limits
    // how many partners are dispatched per request.
    let offset = (now / 3600) as usize % pull_partners.len();
    pull_partners.rotate_left(offset);

    let max_concurrency = settings.ec.pull_sync_concurrency.max(1);
    let mut in_flight: Vec<InFlightPull> = Vec::new();
    let mut updates = Vec::new();

    for partner in pull_partners {
        if !is_partner_pull_eligible(partner, Some(kv_entry)) {
            continue;
        }

        let Some(url) = validated_pull_sync_url(partner) else {
            continue;
        };

        let rate_key = pull_rate_limit_key(&partner.source_domain, context.ec_id());
        match rate_limiter.exceeded(&rate_key, partner.pull_sync_rate_limit) {
            Ok(true) => {
                log::debug!(
                    "Pull sync: rate-limited partner '{}' for ec_id '{}'",
                    partner.source_domain,
                    super::log_id(context.ec_id())
                );
                continue;
            }
            Ok(false) => {}
            Err(err) => {
                log::warn!(
                    "Pull sync: failed to read rate limit for partner '{}': {err:?}",
                    partner.source_domain
                );
                continue;
            }
        }

        let Some(token) = partner.ts_pull_token.as_ref() else {
            log::warn!(
                "Pull sync: partner '{}' enabled but missing ts_pull_token",
                partner.source_domain
            );
            continue;
        };

        let request_url = build_pull_request_url(url, context.ec_id());
        let scheme = request_url.scheme().to_string();
        let host = request_url.host_str().unwrap_or_default().to_string();
        let port = request_url.port();

        let backend_name = match services.backend().ensure(&PlatformBackendSpec {
            scheme,
            host,
            port,
            host_header_override: None,
            certificate_check: settings.proxy.certificate_check,
            first_byte_timeout: DEFAULT_FIRST_BYTE_TIMEOUT,
            between_bytes_timeout: DEFAULT_FIRST_BYTE_TIMEOUT,
        }) {
            Ok(name) => name,
            Err(err) => {
                log::warn!(
                    "Pull sync: failed to resolve backend for partner '{}': {err:?}",
                    partner.source_domain
                );
                continue;
            }
        };

        let request = http::Request::builder()
            .method(Method::GET)
            .uri(request_url.as_str())
            .header("authorization", format!("Bearer {}", token.expose()))
            .body(EdgeBody::empty())
            .expect("should build pull sync request");

        let pending = match futures::executor::block_on(
            services
                .http_client()
                .send_async(PlatformHttpRequest::new(request, backend_name)),
        ) {
            Ok(pending) => pending,
            Err(err) => {
                log::warn!(
                    "Pull sync: failed to dispatch partner '{}': {err:?}",
                    partner.source_domain
                );
                continue;
            }
        };

        in_flight.push(InFlightPull {
            source_domain: partner.source_domain.clone(),
            pending,
        });

        if in_flight.len() >= max_concurrency {
            drain_pull_batch(&mut in_flight, services, &mut updates);
        }
    }

    drain_pull_batch(&mut in_flight, services, &mut updates);
    if !updates.is_empty() {
        let outcome = kv.upsert_partner_ids_from_snapshot(
            context.ec_id(),
            &updates,
            context.snapshot.clone(),
        );
        if matches!(outcome, EcKvSnapshot::Failed { .. }) {
            log::warn!(
                "Pull sync: failed to persist partner updates for '{}'",
                super::log_id(context.ec_id())
            );
        }
    }
}

fn is_partner_pull_eligible(partner: &PartnerConfig, kv_entry: Option<&KvEntry>) -> bool {
    kv_entry
        .and_then(|entry| entry.ids.get(&partner.source_domain))
        .is_none()
}

fn validated_pull_sync_url(partner: &PartnerConfig) -> Option<Url> {
    let pull_sync_url = partner.pull_sync_url.as_deref()?;
    let parsed = match Url::parse(pull_sync_url) {
        Ok(url) => url,
        Err(err) => {
            log::error!(
                "Pull sync: partner '{}' has invalid pull_sync_url '{}': {err}",
                partner.source_domain,
                pull_sync_url
            );
            return None;
        }
    };

    if parsed.scheme() != "https" {
        log::error!(
            "Pull sync: partner '{}' pull_sync_url must use HTTPS, got scheme '{}'",
            partner.source_domain,
            parsed.scheme()
        );
        return None;
    }

    let Some(hostname) = parsed.host_str() else {
        log::error!(
            "Pull sync: partner '{}' pull_sync_url has no hostname: {}",
            partner.source_domain,
            pull_sync_url
        );
        return None;
    };

    let hostname = hostname.trim_end_matches('.').to_ascii_lowercase();
    if !partner.pull_sync_allowed_domains.iter().any(|domain| {
        domain
            .trim()
            .trim_end_matches('.')
            .eq_ignore_ascii_case(&hostname)
    }) {
        log::error!(
            "Pull sync: partner '{}' URL host '{}' not in pull_sync_allowed_domains",
            partner.source_domain,
            hostname
        );
        return None;
    }

    Some(parsed)
}

fn build_pull_request_url(mut base_url: Url, ec_id: &str) -> Url {
    base_url.query_pairs_mut().append_pair("ec_id", ec_id);
    base_url
}

fn pull_rate_limit_key(source_domain: &str, ec_id: &str) -> String {
    format!("pull:{source_domain}:{}", ec_hash(ec_id))
}

fn drain_pull_batch(
    in_flight: &mut Vec<InFlightPull>,
    services: &RuntimeServices,
    updates: &mut Vec<PartnerIdUpdate>,
) {
    for pending in in_flight.drain(..) {
        let source_domain = pending.source_domain;
        // All requests were dispatched up front via send_async, so waiting on
        // each in turn does not change concurrency.
        let response =
            match futures::executor::block_on(services.http_client().wait(pending.pending)) {
                Ok(response) => response,
                Err(err) => {
                    log::warn!(
                        "Pull sync: request failed for partner '{}': {err:?}",
                        source_domain
                    );
                    continue;
                }
            };

        let Some(uid) = extract_pull_uid(response, &source_domain) else {
            continue;
        };

        updates.push(PartnerIdUpdate::new(source_domain, uid));
    }
}

/// Maximum response body size accepted from pull sync partners (64 KiB).
///
/// The expected response is `{"uid":"<string>"}`, so 64 KiB is generous.
/// This prevents a misbehaving partner from exhausting WASM memory.
const MAX_PULL_RESPONSE_BYTES: usize = 64 * 1024;

fn response_content_length_exceeds_limit(response: &PlatformResponse, source_domain: &str) -> bool {
    let Some(value) = response.response.headers().get(header::CONTENT_LENGTH) else {
        return false;
    };

    let Some(value) = value.to_str().ok() else {
        log::warn!(
            "Pull sync: partner '{}' returned invalid Content-Length header, rejecting",
            source_domain
        );
        return true;
    };

    let Ok(length) = value.parse::<usize>() else {
        log::warn!(
            "Pull sync: partner '{}' returned malformed Content-Length header, rejecting",
            source_domain
        );
        return true;
    };

    if length > MAX_PULL_RESPONSE_BYTES {
        log::warn!(
            "Pull sync: partner '{}' returned oversized Content-Length ({} bytes), rejecting",
            source_domain,
            length
        );
        return true;
    }

    false
}

fn extract_pull_uid(response: PlatformResponse, source_domain: &str) -> Option<String> {
    let status = response.response.status();

    if status == StatusCode::NOT_FOUND {
        log::debug!(
            "Pull sync: partner '{}' returned 404, treating as no-op",
            source_domain
        );
        return None;
    }

    if !status.is_success() {
        log::warn!(
            "Pull sync: partner '{}' returned non-success status {}",
            source_domain,
            status
        );
        return None;
    }

    if response_content_length_exceeds_limit(&response, source_domain) {
        return None;
    }

    let body = response
        .response
        .into_body()
        .into_bytes()
        .unwrap_or_default();
    if body.len() > MAX_PULL_RESPONSE_BYTES {
        log::warn!(
            "Pull sync: partner '{}' returned oversized response ({} bytes), rejecting",
            source_domain,
            body.len()
        );
        return None;
    }
    let payload = match serde_json::from_slice::<PullSyncResponse>(&body) {
        Ok(payload) => payload,
        Err(err) => {
            log::warn!(
                "Pull sync: partner '{}' returned invalid JSON body: {err}",
                source_domain
            );
            return None;
        }
    };

    use super::kv_types::MAX_UID_LENGTH;

    let uid = payload.uid.filter(|value| !value.trim().is_empty());
    match uid {
        None => {
            log::debug!(
                "Pull sync: partner '{}' returned null/empty uid, treating as no-op",
                source_domain
            );
            None
        }
        Some(ref value) if value.len() > MAX_UID_LENGTH => {
            log::warn!(
                "Pull sync: partner '{}' returned uid exceeding {} bytes (got {}), rejecting",
                source_domain,
                MAX_UID_LENGTH,
                value.len()
            );
            None
        }
        _ => uid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::types::ConsentContext;
    use crate::ec::kv_types::KvEntry;
    use crate::platform::PlatformResponse;
    use crate::redacted::Redacted;

    fn make_response(status: u16, body: &[u8]) -> PlatformResponse {
        PlatformResponse::new(
            edgezero_core::http::response_builder()
                .status(status)
                .body(EdgeBody::from(body.to_vec()))
                .expect("should build test response"),
        )
    }

    fn make_response_with_content_length(
        status: u16,
        content_length: usize,
        body: &[u8],
    ) -> PlatformResponse {
        PlatformResponse::new(
            edgezero_core::http::response_builder()
                .status(status)
                .header(header::CONTENT_LENGTH, content_length.to_string())
                .body(EdgeBody::from(body.to_vec()))
                .expect("should build test response"),
        )
    }

    fn pull_partner(ttl_sec: u64) -> PartnerConfig {
        PartnerConfig {
            name: "SSP X".to_owned(),
            api_key_hash: "deadbeef".to_owned(),
            bidstream_enabled: true,
            source_domain: "ssp.example.com".to_owned(),
            openrtb_atype: 3,
            batch_rate_limit: 60,
            pull_sync_enabled: true,
            pull_sync_url: Some("https://sync.partner.test/pull".to_owned()),
            pull_sync_allowed_domains: vec!["sync.partner.test".to_owned()],
            pull_sync_ttl_sec: ttl_sec,
            pull_sync_rate_limit: 20,
            ts_pull_token: Some(Redacted::new("token".to_owned())),
        }
    }

    #[test]
    fn build_pull_sync_context_returns_context_when_valid() {
        let consent = ConsentContext {
            jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
            ..ConsentContext::default()
        };
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = EcContext::new_for_test(Some(ec_id), consent);

        let context = build_pull_sync_context(&ec_context)
            .expect("should build pull sync context for valid EC");
        assert_eq!(
            context.ec_id(),
            ec_context.ec_value().expect("ec should be present"),
            "should capture the EC ID from context"
        );
    }

    #[test]
    fn build_pull_sync_context_rejects_invalid_ec_id() {
        let consent = ConsentContext {
            jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
            ..ConsentContext::default()
        };
        let ec_context = EcContext::new_for_test(Some("invalid-ec".to_owned()), consent);

        let context = build_pull_sync_context(&ec_context);
        assert!(
            context.is_none(),
            "should reject pull sync context when EC ID format is invalid"
        );
    }

    #[test]
    fn partner_is_eligible_when_missing_from_entry() {
        let partner = pull_partner(3600);
        let entry = KvEntry::minimal("other_partner", "uid-1", 100);

        assert!(
            is_partner_pull_eligible(&partner, Some(&entry)),
            "should dispatch when partner has no stored UID"
        );
    }

    #[test]
    fn partner_is_not_eligible_when_already_present() {
        let partner = pull_partner(3600);
        let entry = KvEntry::minimal("ssp.example.com", "uid-1", 1000);

        assert!(
            !is_partner_pull_eligible(&partner, Some(&entry)),
            "should skip dispatch when partner already has a stored UID"
        );
    }

    #[test]
    fn validated_pull_sync_url_rejects_http_scheme() {
        let mut partner = pull_partner(3600);
        partner.pull_sync_url = Some("http://sync.partner.test/pull".to_owned());

        let validated = validated_pull_sync_url(&partner);
        assert!(
            validated.is_none(),
            "should reject pull_sync_url with HTTP scheme"
        );
    }

    #[test]
    fn validated_pull_sync_url_rejects_non_allowlisted_host() {
        let mut partner = pull_partner(3600);
        partner.pull_sync_url = Some("https://evil.test/pull".to_owned());

        let validated = validated_pull_sync_url(&partner);
        assert!(
            validated.is_none(),
            "should reject runtime pull_sync_url host outside allowlist"
        );
    }

    #[test]
    fn validated_pull_sync_url_accepts_normalized_allowlist_match() {
        let mut partner = pull_partner(3600);
        partner.pull_sync_url = Some("https://SYNC.PARTNER.TEST./pull".to_owned());
        partner.pull_sync_allowed_domains = vec!["sync.partner.test".to_owned()];

        let validated = validated_pull_sync_url(&partner);
        assert!(
            validated.is_some(),
            "should accept allowlist match after hostname normalization"
        );
    }

    #[test]
    fn build_pull_request_url_appends_ec_id() {
        let url = Url::parse("https://sync.partner.test/pull?x=1").expect("should parse URL");
        let result = build_pull_request_url(url, "ecid123");

        let query = result.query().expect("should have query string");
        assert!(query.contains("x=1"), "should preserve existing query");
        assert!(query.contains("ec_id=ecid123"), "should append ec_id");
        assert!(
            !query.contains("ip="),
            "should not forward client IP to partners"
        );
    }

    #[test]
    fn pull_rate_limit_key_uses_ec_hash_only() {
        let first_ec_id = format!("{}.ABC123", "a".repeat(64));
        let second_ec_id = format!("{}.XYZ789", "a".repeat(64));

        let first_key = pull_rate_limit_key("ssp.example.com", &first_ec_id);
        let second_key = pull_rate_limit_key("ssp.example.com", &second_ec_id);

        assert_eq!(
            first_key, second_key,
            "should bucket different suffixes for the same EC hash together"
        );
        assert_eq!(
            first_key,
            format!("pull:ssp.example.com:{}", "a".repeat(64)),
            "should key pull-sync rate limiting by source domain and EC hash"
        );
    }

    #[test]
    fn extract_pull_uid_treats_404_as_noop() {
        let response = make_response(404, b"");

        let uid = extract_pull_uid(response, "ssp.example.com");
        assert!(uid.is_none(), "should treat 404 as no-op");
    }

    #[test]
    fn extract_pull_uid_treats_uid_null_as_noop() {
        let response = make_response(200, b"{\"uid\":null}");

        let uid = extract_pull_uid(response, "ssp.example.com");
        assert!(uid.is_none(), "should treat uid=null as no-op");
    }

    #[test]
    fn extract_pull_uid_rejects_oversized_uid() {
        let long_uid = "x".repeat(513);
        let body = format!("{{\"uid\":\"{long_uid}\"}}");
        let response = make_response(200, body.as_bytes());

        let uid = extract_pull_uid(response, "ssp.example.com");
        assert!(uid.is_none(), "should reject uid exceeding 512 bytes");
    }

    #[test]
    fn extract_pull_uid_reads_uid_from_success_body() {
        let response = make_response(200, b"{\"uid\":\"abc123\"}");

        let uid = extract_pull_uid(response, "ssp.example.com");
        assert_eq!(
            uid.as_deref(),
            Some("abc123"),
            "should parse uid from 200 body"
        );
    }

    #[test]
    fn extract_pull_uid_rejects_oversized_content_length_before_body_read() {
        let response = make_response_with_content_length(
            200,
            MAX_PULL_RESPONSE_BYTES + 1,
            b"{\"uid\":\"abc123\"}",
        );

        let uid = extract_pull_uid(response, "ssp.example.com");
        assert!(
            uid.is_none(),
            "should reject oversized Content-Length before parsing body"
        );
    }

    #[test]
    fn extract_pull_uid_accepts_small_body_without_content_length() {
        let response = make_response(200, b"{\"uid\":\"abc123\"}");

        let uid = extract_pull_uid(response, "ssp.example.com");
        assert_eq!(
            uid.as_deref(),
            Some("abc123"),
            "should accept small valid response without Content-Length"
        );
    }

    #[test]
    fn extract_pull_uid_rejects_body_larger_than_limit() {
        let body = format!("{{\"uid\":\"{}\"}}", "x".repeat(MAX_PULL_RESPONSE_BYTES));
        let response = make_response(200, body.as_bytes());

        let uid = extract_pull_uid(response, "ssp.example.com");
        assert!(uid.is_none(), "should reject body larger than limit");
    }

    #[test]
    fn rotating_offset_distributes_partners_across_hours() {
        // Simulate 3 partners sorted by source domain: alpha, beta, gamma.
        let ids = vec!["alpha.example.com", "beta.example.com", "gamma.example.com"];

        // Hour 0: offset = 0 % 3 = 0 → [alpha, beta, gamma]
        let ts_h0: u64 = 100; // within hour 0
        let offset_h0 = (ts_h0 / 3600) as usize % ids.len();
        assert_eq!(offset_h0, 0, "hour 0 should start at index 0");

        // Hour 1: offset = (3600 / 3600) % 3 = 1 → [beta, gamma, alpha]
        let offset_h1 = (3600u64 / 3600) as usize % ids.len();
        assert_eq!(offset_h1, 1, "hour 1 should start at index 1");

        // Hour 2: offset = (7200 / 3600) % 3 = 2 → [gamma, alpha, beta]
        let offset_h2 = (7200u64 / 3600) as usize % ids.len();
        assert_eq!(offset_h2, 2, "hour 2 should start at index 2");

        // Hour 3: offset = (10800 / 3600) % 3 = 0 → wraps back to [alpha, beta, gamma]
        let offset_h3 = (10800u64 / 3600) as usize % ids.len();
        assert_eq!(offset_h3, 0, "hour 3 should wrap back to index 0");

        // Verify rotate_left produces expected ordering
        let mut rotated = ids.clone();
        rotated.rotate_left(offset_h1);
        assert_eq!(
            rotated,
            vec!["beta.example.com", "gamma.example.com", "alpha.example.com"],
            "hour 1 rotation should move beta to front"
        );
    }

    // -----------------------------------------------------------------------
    // Snapshot-driven eligibility and request-wide aggregation
    // -----------------------------------------------------------------------

    use crate::error::TrustedServerError;
    use crate::platform::test_support::{build_services_with_http_client, StubHttpClient};
    use crate::settings::EcPartner;
    use crate::test_support::tests::create_test_settings;
    use error_stack::Report;
    use std::sync::Arc;

    struct AllowAllRateLimiter;

    impl RateLimiter for AllowAllRateLimiter {
        fn exceeded(
            &self,
            _key: &str,
            _hourly_limit: u32,
        ) -> Result<bool, Report<TrustedServerError>> {
            Ok(false)
        }
    }

    fn pull_enabled_ec_partner(source_domain: &str) -> EcPartner {
        EcPartner {
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: Redacted::new(format!("{source_domain}-api-token-32-bytes-minimum")),
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: true,
            pull_sync_url: Some(format!("https://{source_domain}/sync")),
            pull_sync_allowed_domains: vec![source_domain.to_owned()],
            pull_sync_ttl_sec: 3600,
            pull_sync_rate_limit: 100,
            ts_pull_token: Some(Redacted::new("outbound-token".to_owned())),
        }
    }

    fn snapshot_ec_id() -> String {
        format!("{}.ABC123", "a".repeat(64))
    }

    fn seed_present_snapshot(graph: &KvIdentityGraph, ec_id: &str) -> EcKvSnapshot {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        graph.create(ec_id, &entry).expect("should seed live entry");
        graph.load_snapshot(ec_id)
    }

    #[test]
    fn dispatch_pull_sync_aggregates_batches_into_one_bulk_write() {
        let mut settings = create_test_settings();
        // Force one partner per concurrency batch so responses span batches.
        settings.ec.pull_sync_concurrency = 1;
        let registry = PartnerRegistry::from_config(&[
            pull_enabled_ec_partner("alpha.example.com"),
            pull_enabled_ec_partner("beta.example.com"),
        ])
        .expect("should build pull registry");

        let graph = KvIdentityGraph::in_memory("pull_store");
        let ec_id = snapshot_ec_id();
        let snapshot = seed_present_snapshot(&graph, &ec_id);

        let stub = Arc::new(StubHttpClient::new());
        // One JSON response per partner, drained across two concurrency batches.
        stub.push_response(200, br#"{"uid":"synced-uid"}"#.to_vec());
        stub.push_response(200, br#"{"uid":"synced-uid"}"#.to_vec());
        let services = build_services_with_http_client(stub.clone());

        let context = PullSyncContext {
            ec_id: ec_id.clone(),
            snapshot,
        };
        dispatch_pull_sync(
            &settings,
            &graph,
            &registry,
            &AllowAllRateLimiter,
            &context,
            &services,
        );

        let (entry, generation) = graph
            .get(&ec_id)
            .expect("should read store")
            .expect("entry should exist");
        assert_eq!(
            entry.ids.get("alpha.example.com").map(|id| id.uid.as_str()),
            Some("synced-uid"),
            "first partner UID should persist"
        );
        assert_eq!(
            entry.ids.get("beta.example.com").map(|id| id.uid.as_str()),
            Some("synced-uid"),
            "second partner UID should persist"
        );
        assert_eq!(
            generation, 2,
            "two partner responses across batches must persist in exactly one bulk write"
        );
    }

    #[test]
    fn dispatch_pull_sync_skips_non_present_snapshots() {
        let mut settings = create_test_settings();
        settings.ec.pull_sync_concurrency = 4;
        let registry =
            PartnerRegistry::from_config(&[pull_enabled_ec_partner("alpha.example.com")])
                .expect("should build registry");
        let graph = KvIdentityGraph::in_memory("pull_store");
        let ec_id = snapshot_ec_id();
        let stub = Arc::new(StubHttpClient::new());
        let services = build_services_with_http_client(stub.clone());

        for snapshot in [
            EcKvSnapshot::NotRead,
            EcKvSnapshot::Missing {
                ec_id: ec_id.clone(),
            },
            EcKvSnapshot::Failed {
                ec_id: ec_id.clone(),
            },
        ] {
            let context = PullSyncContext {
                ec_id: ec_id.clone(),
                snapshot,
            };
            dispatch_pull_sync(
                &settings,
                &graph,
                &registry,
                &AllowAllRateLimiter,
                &context,
                &services,
            );
        }

        assert!(
            stub.recorded_backend_names().is_empty(),
            "not-read, missing, and failed snapshots must not dispatch pull sync"
        );
        assert!(
            graph.get(&ec_id).expect("should read store").is_none(),
            "no snapshot state should create a missing root"
        );
    }

    #[test]
    fn dispatch_pull_sync_skips_tombstone_snapshot() {
        let mut settings = create_test_settings();
        settings.ec.pull_sync_concurrency = 4;
        let registry =
            PartnerRegistry::from_config(&[pull_enabled_ec_partner("alpha.example.com")])
                .expect("should build registry");
        let graph = KvIdentityGraph::in_memory("pull_store");
        let ec_id = snapshot_ec_id();
        let snapshot = EcKvSnapshot::Present {
            ec_id: ec_id.clone(),
            entry: Box::new(KvEntry::tombstone(1000)),
            generation: Some(1),
        };
        let stub = Arc::new(StubHttpClient::new());
        let services = build_services_with_http_client(stub.clone());

        let context = PullSyncContext {
            ec_id: ec_id.clone(),
            snapshot,
        };
        dispatch_pull_sync(
            &settings,
            &graph,
            &registry,
            &AllowAllRateLimiter,
            &context,
            &services,
        );

        assert!(
            stub.recorded_backend_names().is_empty(),
            "a tombstone snapshot must not dispatch pull sync"
        );
    }
}
