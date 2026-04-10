//! Pull sync background dispatch.
//!
//! Launches partner pull-sync requests for organic traffic after the client
//! response has been sent. Dispatch is best-effort and never affects client
//! response status.

use fastly::http::request::PendingRequest;
use fastly::http::{Method, StatusCode};
use fastly::Request;
use serde::Deserialize;
use url::Url;

use crate::backend::BackendConfig;
use crate::settings::Settings;

use super::generation::is_valid_ec_id;
use super::kv::KvIdentityGraph;
use super::kv_types::KvEntry;
use super::partner::{PartnerRecord, PartnerStore};
use super::sync_pixel::{current_timestamp, RateLimiter};
use super::EcContext;

/// Inputs needed to dispatch pull sync after response flush.
#[derive(Debug, Clone)]
pub struct PullSyncContext {
    ec_id: String,
    client_ip: String,
}

impl PullSyncContext {
    /// Returns the EC ID for the request.
    #[must_use]
    pub fn ec_id(&self) -> &str {
        &self.ec_id
    }

    /// Returns the normalized client IP for pull endpoint query parameters.
    #[must_use]
    pub fn client_ip(&self) -> &str {
        &self.client_ip
    }
}

struct InFlightPull {
    partner_id: String,
    pending: PendingRequest,
}

#[derive(Debug, Deserialize)]
struct PullSyncResponse {
    uid: Option<String>,
}

/// Builds post-send pull-sync context from the route EC context.
///
/// Returns `None` when consent denies EC, there is no active EC ID, or no
/// client IP is available.
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
    let client_ip = ec_context.client_ip()?.to_owned();
    Some(PullSyncContext { ec_id, client_ip })
}

/// Dispatches partner pull-sync requests in the background.
///
/// This function is best-effort: all errors are logged and swallowed.
pub fn dispatch_pull_sync(
    settings: &Settings,
    kv: &KvIdentityGraph,
    partner_store: &PartnerStore,
    rate_limiter: &dyn RateLimiter,
    context: &PullSyncContext,
) {
    let now = current_timestamp();
    let kv_entry = match kv.get(context.ec_id()) {
        Ok(entry) => entry.map(|(entry, _)| entry),
        Err(err) => {
            log::warn!(
                "Pull sync: failed to read identity graph for '{}': {err:?}",
                context.ec_id()
            );
            return;
        }
    };

    // Use the _pull_enabled secondary index for O(1+N) reads instead of
    // scanning all partners (§13.1). Falls back to list_registered() if
    // the index is missing or unreadable.
    let mut pull_partners = match partner_store.pull_enabled_partners() {
        Ok(partners) => partners,
        Err(err) => {
            log::warn!("Pull sync: failed to list pull-enabled partners: {err:?}");
            return;
        }
    };

    // Sort by ID for deterministic ordering, then apply a rotating hourly
    // offset so that different partners get dispatch priority (§10.3).
    pull_partners.sort_by(|a, b| a.id.cmp(&b.id));

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

    for partner in pull_partners {
        if !is_partner_pull_eligible(&partner, kv_entry.as_ref(), now) {
            continue;
        }

        let Some(url) = validated_pull_sync_url(&partner) else {
            continue;
        };

        let rate_key = format!("pull:{}:{}", partner.id, context.ec_id());
        match rate_limiter.exceeded(&rate_key, partner.pull_sync_rate_limit) {
            Ok(true) => {
                log::debug!(
                    "Pull sync: rate-limited partner '{}' for ec_id '{}'",
                    partner.id,
                    context.ec_id()
                );
                continue;
            }
            Ok(false) => {}
            Err(err) => {
                log::warn!(
                    "Pull sync: failed to read rate limit for partner '{}': {err:?}",
                    partner.id
                );
                continue;
            }
        }

        let Some(token) = partner.ts_pull_token.as_deref() else {
            log::warn!(
                "Pull sync: partner '{}' enabled but missing ts_pull_token",
                partner.id
            );
            continue;
        };

        let request_url = build_pull_request_url(url, context.ec_id(), context.client_ip());
        let mut request = Request::new(Method::GET, request_url.as_str());
        request.set_header("authorization", format!("Bearer {token}"));

        let backend_name =
            match BackendConfig::from_url(request_url.as_str(), settings.proxy.certificate_check) {
                Ok(name) => name,
                Err(err) => {
                    log::warn!(
                        "Pull sync: failed to resolve backend for partner '{}': {err:?}",
                        partner.id
                    );
                    continue;
                }
            };

        let pending = match request.send_async(backend_name) {
            Ok(pending) => pending,
            Err(err) => {
                log::warn!(
                    "Pull sync: failed to dispatch partner '{}': {err:?}",
                    partner.id
                );
                continue;
            }
        };

        in_flight.push(InFlightPull {
            partner_id: partner.id,
            pending,
        });

        if in_flight.len() >= max_concurrency {
            drain_pull_batch(kv, context.ec_id(), &mut in_flight);
        }
    }

    drain_pull_batch(kv, context.ec_id(), &mut in_flight);
}

fn is_partner_pull_eligible(partner: &PartnerRecord, kv_entry: Option<&KvEntry>, now: u64) -> bool {
    let Some(entry) = kv_entry else {
        return true;
    };

    let Some(existing) = entry.ids.get(&partner.id) else {
        return true;
    };

    now.saturating_sub(existing.synced) >= partner.pull_sync_ttl_sec
}

fn validated_pull_sync_url(partner: &PartnerRecord) -> Option<Url> {
    let pull_sync_url = partner.pull_sync_url.as_deref()?;
    let parsed = match Url::parse(pull_sync_url) {
        Ok(url) => url,
        Err(err) => {
            log::error!(
                "Pull sync: partner '{}' has invalid pull_sync_url '{}': {err}",
                partner.id,
                pull_sync_url
            );
            return None;
        }
    };

    if parsed.scheme() != "https" {
        log::error!(
            "Pull sync: partner '{}' pull_sync_url must use HTTPS, got scheme '{}'",
            partner.id,
            parsed.scheme()
        );
        return None;
    }

    let Some(hostname) = parsed.host_str() else {
        log::error!(
            "Pull sync: partner '{}' pull_sync_url has no hostname: {}",
            partner.id,
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
            partner.id,
            hostname
        );
        return None;
    }

    Some(parsed)
}

fn build_pull_request_url(mut base_url: Url, ec_id: &str, client_ip: &str) -> Url {
    base_url
        .query_pairs_mut()
        .append_pair("ec_id", ec_id)
        .append_pair("ip", client_ip);
    base_url
}

fn drain_pull_batch(kv: &KvIdentityGraph, ec_id: &str, in_flight: &mut Vec<InFlightPull>) {
    for pending in in_flight.drain(..) {
        let partner_id = pending.partner_id;
        let response = match pending.pending.wait() {
            Ok(response) => response,
            Err(err) => {
                log::warn!(
                    "Pull sync: request failed for partner '{}': {err:?}",
                    partner_id
                );
                continue;
            }
        };

        let Some(uid) = extract_pull_uid(response, &partner_id) else {
            continue;
        };

        if let Err(err) = kv.upsert_partner_id(ec_id, &partner_id, &uid, current_timestamp()) {
            log::warn!(
                "Pull sync: failed to upsert partner '{}' for ec_id '{}': {err:?}",
                partner_id,
                ec_id
            );
        }
    }
}

fn extract_pull_uid(mut response: fastly::Response, partner_id: &str) -> Option<String> {
    let status = response.get_status();

    if status == StatusCode::NOT_FOUND {
        log::debug!(
            "Pull sync: partner '{}' returned 404, treating as no-op",
            partner_id
        );
        return None;
    }

    if !status.is_success() {
        log::warn!(
            "Pull sync: partner '{}' returned non-success status {}",
            partner_id,
            status
        );
        return None;
    }

    let body = response.take_body_bytes();
    let payload = match serde_json::from_slice::<PullSyncResponse>(&body) {
        Ok(payload) => payload,
        Err(err) => {
            log::warn!(
                "Pull sync: partner '{}' returned invalid JSON body: {err}",
                partner_id
            );
            return None;
        }
    };

    use super::kv_types::MAX_UID_LENGTH;

    let uid = payload.uid.filter(|value| !value.is_empty());
    match uid {
        None => {
            log::debug!(
                "Pull sync: partner '{}' returned null/empty uid, treating as no-op",
                partner_id
            );
            None
        }
        Some(ref value) if value.len() > MAX_UID_LENGTH => {
            log::warn!(
                "Pull sync: partner '{}' returned uid exceeding {} bytes (got {}), rejecting",
                partner_id,
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

    fn pull_partner(ttl_sec: u64) -> PartnerRecord {
        PartnerRecord {
            id: "ssp_x".to_owned(),
            name: "SSP X".to_owned(),
            allowed_return_domains: vec!["sync.example.com".to_owned()],
            api_key_hash: "deadbeef".to_owned(),
            bidstream_enabled: true,
            source_domain: "ssp.example.com".to_owned(),
            openrtb_atype: 3,
            sync_rate_limit: 60,
            batch_rate_limit: 60,
            pull_sync_enabled: true,
            pull_sync_url: Some("https://sync.partner.test/pull".to_owned()),
            pull_sync_allowed_domains: vec!["sync.partner.test".to_owned()],
            pull_sync_ttl_sec: ttl_sec,
            pull_sync_rate_limit: 20,
            ts_pull_token: Some("token".to_owned()),
            fp_signal_cookie_names: vec![],
            fp_signal_json_path: None,
            fp_signal_ttl_sec: 86400,
        }
    }

    #[test]
    fn build_pull_sync_context_requires_ec_and_ip() {
        let consent = ConsentContext {
            jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
            ..ConsentContext::default()
        };
        let ec_context =
            EcContext::new_for_test(Some(format!("{}.ABC123", "a".repeat(64))), consent);

        let context = build_pull_sync_context(&ec_context);
        assert!(
            context.is_none(),
            "should require client_ip for pull sync dispatch context"
        );
    }

    #[test]
    fn build_pull_sync_context_returns_context_when_valid() {
        let consent = ConsentContext {
            jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
            ..ConsentContext::default()
        };
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context =
            EcContext::new_for_test_with_ip(Some(ec_id), consent, Some("1.2.3.4".to_owned()));

        let context = build_pull_sync_context(&ec_context)
            .expect("should build pull sync context for valid EC with IP");
        assert_eq!(
            context.ec_id(),
            ec_context.ec_value().expect("ec should be present")
        );
        assert_eq!(context.client_ip(), "1.2.3.4");
    }

    #[test]
    fn build_pull_sync_context_rejects_invalid_ec_id() {
        let consent = ConsentContext {
            jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
            ..ConsentContext::default()
        };
        let ec_context = EcContext::new_for_test_with_ip(
            Some("invalid-ec".to_owned()),
            consent,
            Some("1.2.3.4".to_owned()),
        );

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
            is_partner_pull_eligible(&partner, Some(&entry), 200),
            "should dispatch when partner has no stored sync"
        );
    }

    #[test]
    fn partner_is_not_eligible_when_not_stale() {
        let partner = pull_partner(3600);
        let entry = KvEntry::minimal("ssp_x", "uid-1", 1000);

        assert!(
            !is_partner_pull_eligible(&partner, Some(&entry), 1500),
            "should skip dispatch when sync is fresher than ttl"
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
    fn build_pull_request_url_appends_query_pairs() {
        let url = Url::parse("https://sync.partner.test/pull?x=1").expect("should parse URL");
        let result = build_pull_request_url(url, "ecid123", "1.2.3.4");

        let query = result.query().expect("should have query string");
        assert!(query.contains("x=1"), "should preserve existing query");
        assert!(query.contains("ec_id=ecid123"), "should append ec_id");
        assert!(query.contains("ip=1.2.3.4"), "should append ip");
    }

    #[test]
    fn extract_pull_uid_treats_404_as_noop() {
        let response = fastly::Response::from_status(StatusCode::NOT_FOUND);

        let uid = extract_pull_uid(response, "ssp_x");
        assert!(uid.is_none(), "should treat 404 as no-op");
    }

    #[test]
    fn extract_pull_uid_treats_uid_null_as_noop() {
        let response = fastly::Response::from_status(StatusCode::OK).with_body("{\"uid\":null}");

        let uid = extract_pull_uid(response, "ssp_x");
        assert!(uid.is_none(), "should treat uid=null as no-op");
    }

    #[test]
    fn extract_pull_uid_rejects_oversized_uid() {
        let long_uid = "x".repeat(513);
        let body = format!("{{\"uid\":\"{long_uid}\"}}");
        let response = fastly::Response::from_status(StatusCode::OK).with_body(body);

        let uid = extract_pull_uid(response, "ssp_x");
        assert!(uid.is_none(), "should reject uid exceeding 512 bytes");
    }

    #[test]
    fn extract_pull_uid_reads_uid_from_success_body() {
        let response =
            fastly::Response::from_status(StatusCode::OK).with_body("{\"uid\":\"abc123\"}");

        let uid = extract_pull_uid(response, "ssp_x");
        assert_eq!(
            uid.as_deref(),
            Some("abc123"),
            "should parse uid from 200 body"
        );
    }

    #[test]
    fn rotating_offset_distributes_partners_across_hours() {
        // Simulate 3 partners sorted by ID: alpha, beta, gamma.
        let ids = vec!["alpha", "beta", "gamma"];

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
            vec!["beta", "gamma", "alpha"],
            "hour 1 rotation should move beta to front"
        );
    }
}
