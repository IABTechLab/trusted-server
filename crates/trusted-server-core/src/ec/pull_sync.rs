//! Pull sync background dispatch.
//!
//! Launches partner pull-sync requests for organic traffic after the client
//! response has been sent. Dispatch is best-effort and never affects client
//! response status.
//!
//! Pull sync currently fills missing partner UIDs only. Once a partner UID is
//! present in the EC identity entry, it is not periodically refreshed because
//! the entry no longer stores per-partner sync timestamps.

use std::time::Duration;

use edgezero_core::error::EdgeError;
use edgezero_core::outbound::{
    HttpClient, OutboundCollectedSlot, OutboundRequest, OutboundResponse, ResponseDecoding,
};
use edgezero_core::time::MonotonicClock;
use http::{Method, StatusCode, header};
use serde::Deserialize;
use url::Url;

use crate::consent::allows_eid_persistence;
use crate::error::TrustedServerError;
use crate::outbound::{
    BUFFERED_TIMEOUT, OutboundRequestSource, buffered, outbound_error, outbound_request,
};
use crate::settings::Settings;

use super::generation::{ec_hash, is_valid_ec_id};
use super::kv::{KvIdentityGraph, PartnerIdUpdate};
use super::kv_types::{KvEntry, MAX_UID_LENGTH};
use super::rate_limiter::RateLimiter;
use super::registry::{PartnerConfig, PartnerRegistry};

// `current_timestamp` is defined in the parent `ec` module.
use super::EcContext;
use super::EcKvSnapshot;
use super::current_timestamp;

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

/// One partner request waiting for its group to be sent.
struct QueuedPull {
    source_domain: String,
    request: OutboundRequest,
}

/// How long a group of partner requests may run before it is cut off.
///
/// Each request also carries it as its own timeout.
const PULL_SYNC_GROUP_CUTOFF: Duration = BUFFERED_TIMEOUT;

#[derive(Debug, Deserialize)]
struct PullSyncResponse {
    uid: Option<String>,
}

/// Builds post-send pull-sync context from the route EC context.
///
/// Returns `None` when there are no pull-enabled partners, when consent denies
/// EC, when there is no valid active EC ID, or when the request snapshot holds
/// no consented row that is still missing at least one pull-partner UID.
#[must_use]
pub fn build_pull_sync_context(
    ec_context: &EcContext,
    registry: &PartnerRegistry,
) -> Option<PullSyncContext> {
    // Without EID-persistence consent (TCF Purpose 1 + 4), finalization
    // withholds browser-supplied partner UIDs. That leaves partner slots empty,
    // which pull sync would otherwise read as eligible and disclose the EC ID
    // to partners for exactly the users who opted out.
    if registry.pull_enabled_partners().is_empty()
        || !ec_context.ec_allowed()
        || !allows_eid_persistence(ec_context.consent())
    {
        return None;
    }

    let ec_id_ref = ec_context.ec_value()?;
    if !is_valid_ec_id(ec_id_ref) {
        log::debug!("Pull sync: skipping dispatch because active EC ID is invalid format");
        return None;
    }

    let entry = ec_context.kv_snapshot().entry_for(ec_id_ref)?;
    if !entry.consent.ok || entry_is_pull_complete(entry, registry) {
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
/// Partners are sent in groups of `ec.pull_sync_concurrency` through
/// [`HttpClient::send_all_until`]. Each group is cut off
/// [`PULL_SYNC_GROUP_CUTOFF`] after it starts, on `clock`, and a partner that
/// has not answered by then is skipped.
///
/// # Consent revalidation
///
/// Pull sync runs *after* the page response has been flushed to the browser and
/// discloses the raw `ec_id` to partners, so it cannot be authorized by the
/// request snapshot alone: a concurrent CMP withdrawal can tombstone the row in
/// the window between snapshot capture and dispatch. The live row is re-read
/// immediately before the first partner request and anything short of a live,
/// consenting entry cancels the dispatch. The read is deferred until after the
/// snapshot-based eligibility filter, so a request with nothing to pull still
/// costs no KV operation, and the fresh snapshot is reused for the write-back
/// instead of the stale one.
pub async fn dispatch_pull_sync(
    settings: &Settings,
    kv: &KvIdentityGraph,
    registry: &PartnerRegistry,
    rate_limiter: &dyn RateLimiter,
    context: &PullSyncContext,
    client: &HttpClient,
    clock: &MonotonicClock,
) {
    let now = current_timestamp();
    let mut pull_partners = registry.pull_enabled_partners();

    if pull_partners.is_empty() {
        return;
    }

    let Some(request_entry) = context.snapshot.entry_for(context.ec_id()) else {
        log::debug!(
            "Pull sync: skipping dispatch for '{}' because the request captured no usable \
             identity snapshot",
            super::log_id(context.ec_id())
        );
        return;
    };
    if !request_entry.consent.ok {
        return;
    }

    // Sort by source domain for deterministic ordering, then apply a rotating
    // hourly offset so that different partners get dispatch priority (§10.3).
    pull_partners.sort_by(|a, b| a.source_domain.cmp(&b.source_domain));

    log::debug!(
        "Pull sync: {} pull-enabled partners after filtering",
        pull_partners.len(),
    );

    // Rotate the partner list so that the starting partner changes each
    // hour. This ensures fair distribution when max_concurrency limits
    // how many partners are dispatched per request.
    let offset = (now / 3600) as usize % pull_partners.len();
    pull_partners.rotate_left(offset);

    // Drop partners the request snapshot already has a UID for before paying
    // for the revalidation read: a dispatch with nothing to pull must not add a
    // KV operation.
    pull_partners.retain(|partner| is_partner_pull_eligible(partner, Some(request_entry)));
    if pull_partners.is_empty() {
        return;
    }

    let live_snapshot = kv.load_snapshot(context.ec_id()).await;
    let Some(live_entry) = live_snapshot.entry_for(context.ec_id()) else {
        log::warn!(
            "Pull sync: skipping dispatch for '{}' because the live identity row could not be \
             confirmed",
            super::log_id(context.ec_id())
        );
        return;
    };
    if !live_entry.consent.ok {
        log::info!(
            "Pull sync: skipping dispatch for '{}' because consent was withdrawn after the \
             request snapshot was captured",
            super::log_id(context.ec_id())
        );
        return;
    }

    let max_concurrency = settings.ec.pull_sync_concurrency.max(1);
    let mut group: Vec<QueuedPull> = Vec::new();
    let mut updates = Vec::new();

    for partner in pull_partners {
        // Re-checked against the live row: a concurrent request may have filled
        // this partner's UID since the request snapshot was captured.
        if !is_partner_pull_eligible(partner, Some(live_entry)) {
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
        let request = match pull_request(&request_url, token.expose()) {
            Ok(request) => request,
            Err(err) => {
                log_pull_failure(
                    err,
                    format!(
                        "request for partner '{}' could not be built",
                        partner.source_domain
                    ),
                );
                continue;
            }
        };

        group.push(QueuedPull {
            source_domain: partner.source_domain.clone(),
            request,
        });

        if group.len() >= max_concurrency {
            send_pull_group(client, clock, &mut group, &mut updates).await;
        }
    }

    send_pull_group(client, clock, &mut group, &mut updates).await;
    if !updates.is_empty() {
        // Write back from the revalidated snapshot, not the request one: its
        // generation is current, so the first CAS attempt is not spent losing a
        // conflict against the read that authorized this dispatch.
        let outcome = kv
            .upsert_partner_ids_from_snapshot(context.ec_id(), &updates, live_snapshot.clone())
            .await;
        if matches!(outcome, EcKvSnapshot::Failed { .. }) {
            log::warn!(
                "Pull sync: failed to persist partner updates for '{}'",
                super::log_id(context.ec_id())
            );
        }
    }
}

/// Returns whether a consented live entry holds an ID for every pull-enabled
/// partner. Always returns `false` when there are no pull-enabled partners.
#[must_use]
pub(crate) fn entry_is_pull_complete(entry: &KvEntry, registry: &PartnerRegistry) -> bool {
    let pull_partners = registry.pull_enabled_partners();
    !pull_partners.is_empty()
        && entry.consent.ok
        && pull_partners
            .iter()
            .all(|partner| !is_partner_pull_eligible(partner, Some(entry)))
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

/// Builds one partner's pull-sync request.
///
/// # Errors
///
/// Returns the [`EdgeError`] that building the request or its
/// `Authorization` header raised.
fn pull_request(url: &Url, token: &str) -> Result<OutboundRequest, EdgeError> {
    let request = outbound_request(Method::GET, url.as_str())?
        .header(header::AUTHORIZATION, format!("Bearer {token}"))?;
    Ok(buffered(
        request,
        PULL_SYNC_GROUP_CUTOFF,
        MAX_PULL_RESPONSE_BYTES,
        ResponseDecoding::Decode,
    ))
}

/// Logs one failed pull-sync step at warn, through [`outbound_error`].
///
/// `outbound_error` logs the failure's status, kind, budget source, and
/// provider detail, never the error's message, which can hold provider text
/// or a URL. This then logs the report with the `failed` step.
fn log_pull_failure(error: EdgeError, failed: String) {
    let report = outbound_error(
        error,
        TrustedServerError::EdgeCookie { message: failed },
        OutboundRequestSource::Internal,
    );
    log::warn!("Pull sync failed: {report:?}");
}

/// Sends the queued `group` and collects the UIDs its partners answer with.
///
/// The group ends [`PULL_SYNC_GROUP_CUTOFF`] after it starts. A partner that
/// has not answered by then is logged and skipped.
async fn send_pull_group(
    client: &HttpClient,
    clock: &MonotonicClock,
    group: &mut Vec<QueuedPull>,
    updates: &mut Vec<PartnerIdUpdate>,
) {
    if group.is_empty() {
        return;
    }
    let (source_domains, requests): (Vec<String>, Vec<OutboundRequest>) = group
        .drain(..)
        .map(|queued| (queued.source_domain, queued.request))
        .unzip();
    let cutoff = clock.deadline_after(PULL_SYNC_GROUP_CUTOFF);
    let slots = match client.send_all_until(requests, cutoff).await {
        Ok(results) => results.into_parts().0,
        Err(failure) => {
            let (error, slots) = failure.into_parts();
            log_pull_failure(error, "partner group failed".to_owned());
            slots
        }
    };

    for (source_domain, slot) in source_domains.into_iter().zip(slots) {
        let response = match slot {
            OutboundCollectedSlot::Resolved(result) => match result.outcome {
                Ok(response) => response,
                Err(err) => {
                    log_pull_failure(err, format!("request for partner '{source_domain}' failed"));
                    continue;
                }
            },
            OutboundCollectedSlot::Unresolved => {
                log::warn!(
                    "Pull sync: partner '{source_domain}' did not answer within the {}s group cutoff",
                    PULL_SYNC_GROUP_CUTOFF.as_secs()
                );
                continue;
            }
            OutboundCollectedSlot::AlreadyDelivered => continue,
        };

        if let Some(uid) = extract_pull_uid(response, &source_domain).await {
            updates.push(PartnerIdUpdate::new(source_domain, uid));
        }
    }
}

/// Maximum response body size accepted from pull sync partners (64 KiB).
///
/// The expected response is `{"uid":"<string>"}`, so 64 KiB is generous.
/// Each request carries it as its response cap, so a misbehaving partner
/// cannot exhaust WASM memory.
const MAX_PULL_RESPONSE_BYTES: u64 = 64 * 1024;

async fn extract_pull_uid(response: OutboundResponse, source_domain: &str) -> Option<String> {
    let status = response.status();
    let body = match response.into_bytes_bounded(MAX_PULL_RESPONSE_BYTES).await {
        Ok(body) => body,
        Err(err) => {
            log_pull_failure(
                err,
                format!("response from partner '{source_domain}' could not be read"),
            );
            return None;
        }
    };
    pull_uid(status, &body, source_domain)
}

/// Reads the UID a partner answered with `status` and `body`.
fn pull_uid(status: StatusCode, body: &[u8], source_domain: &str) -> Option<String> {
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

    let payload = match serde_json::from_slice::<PullSyncResponse>(body) {
        Ok(payload) => payload,
        Err(err) => {
            log::warn!(
                "Pull sync: partner '{}' returned invalid JSON body: {err}",
                source_domain
            );
            return None;
        }
    };

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
    use crate::redacted::Redacted;
    use crate::test_support::tests::purpose_one_only_consent;

    fn pull_partner(ttl_sec: u64) -> PartnerConfig {
        PartnerConfig {
            name: "SSP X".to_owned(),
            api_key_hash: Some("deadbeef".to_owned()),
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

    #[tokio::test(flavor = "current_thread")]
    async fn build_pull_sync_context_returns_context_when_valid() {
        let consent = ConsentContext {
            jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
            ..ConsentContext::default()
        };
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mut ec_context = EcContext::new_for_test(Some(ec_id.clone()), consent);
        let graph = KvIdentityGraph::in_memory("pull_store");
        ec_context.set_kv_snapshot(seed_present_snapshot(&graph, &ec_id).await);
        let registry = PartnerRegistry::from_config(&[pull_enabled_ec_partner("ssp.example.com")])
            .expect("should build registry");

        let context = build_pull_sync_context(&ec_context, &registry)
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
        let registry = PartnerRegistry::from_config(&[pull_enabled_ec_partner("ssp.example.com")])
            .expect("should build registry");

        let context = build_pull_sync_context(&ec_context, &registry);
        assert!(
            context.is_none(),
            "should reject pull sync context when EC ID format is invalid"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn build_pull_sync_context_skips_when_eid_persistence_is_denied() {
        // Purpose 1 granted keeps the EC, but Purpose 4 denied withholds EID
        // writes and leaves partner slots empty. Pull sync must not read those
        // empty slots as eligible and disclose the EC ID to partners.
        let consent = purpose_one_only_consent();
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mut ec_context = EcContext::new_for_test(Some(ec_id.clone()), consent);
        let graph = KvIdentityGraph::in_memory("pull_store");
        ec_context.set_kv_snapshot(seed_present_snapshot(&graph, &ec_id).await);
        let registry = PartnerRegistry::from_config(&[pull_enabled_ec_partner("ssp.example.com")])
            .expect("should build registry");
        assert!(
            ec_context.ec_allowed(),
            "should keep the EC allowed when Purpose 1 is granted"
        );

        let context = build_pull_sync_context(&ec_context, &registry);

        assert!(
            context.is_none(),
            "should skip pull sync when TCF Purpose 4 is denied"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn build_pull_sync_context_skips_empty_registry_and_complete_snapshot() {
        let consent = ConsentContext {
            jurisdiction: crate::consent::jurisdiction::Jurisdiction::NonRegulated,
            ..ConsentContext::default()
        };
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mut ec_context = EcContext::new_for_test(Some(ec_id.clone()), consent);
        let graph = KvIdentityGraph::in_memory("pull_store");
        let mut snapshot = seed_present_snapshot(&graph, &ec_id).await;
        let registry = PartnerRegistry::from_config(&[pull_enabled_ec_partner("ssp.example.com")])
            .expect("should build registry");

        assert!(
            build_pull_sync_context(&ec_context, &PartnerRegistry::empty()).is_none(),
            "no pull partners should skip before graph construction"
        );
        assert!(
            build_pull_sync_context(&ec_context, &registry).is_none(),
            "an unread snapshot should skip before graph construction"
        );

        if let EcKvSnapshot::Present { entry, .. } = &mut snapshot {
            entry.ids.insert(
                "ssp.example.com".to_owned(),
                crate::ec::kv_types::KvPartnerId {
                    uid: "uid".to_owned(),
                },
            );
        }
        ec_context.set_kv_snapshot(snapshot);
        assert!(
            build_pull_sync_context(&ec_context, &registry).is_none(),
            "a complete snapshot should skip post-send work"
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
    fn completeness_requires_all_pull_partner_ids() {
        let registry = PartnerRegistry::from_config(&[
            pull_enabled_ec_partner("a.example.com"),
            pull_enabled_ec_partner("b.example.com"),
        ])
        .expect("should build registry");
        let mut entry = KvEntry::minimal("a.example.com", "uid-a", 1_000);

        assert!(
            !entry_is_pull_complete(&entry, &registry),
            "entry missing a pull partner ID should be incomplete"
        );

        entry.ids.insert(
            "b.example.com".to_owned(),
            crate::ec::kv_types::KvPartnerId {
                uid: "uid-b".to_owned(),
            },
        );

        assert!(
            entry_is_pull_complete(&entry, &registry),
            "entry with every pull partner ID should be complete"
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
    fn pull_uid_treats_404_as_noop() {
        let uid = pull_uid(StatusCode::NOT_FOUND, b"", "ssp.example.com");
        assert!(uid.is_none(), "should treat 404 as no-op");
    }

    #[test]
    fn pull_uid_treats_uid_null_as_noop() {
        let uid = pull_uid(StatusCode::OK, b"{\"uid\":null}", "ssp.example.com");
        assert!(uid.is_none(), "should treat uid=null as no-op");
    }

    #[test]
    fn pull_uid_rejects_oversized_uid() {
        let long_uid = "x".repeat(513);
        let body = format!("{{\"uid\":\"{long_uid}\"}}");

        let uid = pull_uid(StatusCode::OK, body.as_bytes(), "ssp.example.com");
        assert!(uid.is_none(), "should reject uid exceeding 512 bytes");
    }

    #[test]
    fn pull_uid_reads_uid_from_success_body() {
        let uid = pull_uid(StatusCode::OK, b"{\"uid\":\"abc123\"}", "ssp.example.com");
        assert_eq!(
            uid.as_deref(),
            Some("abc123"),
            "should parse uid from 200 body"
        );
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

    use crate::ec::kv::TombstoneOutcome;
    use crate::settings::EcPartner;
    use crate::test_support::logging::WarningCapture;
    use crate::test_support::scripted_client::{
        ExpectedTransport, ScriptedHttpClient, ScriptedReply, TestClock,
    };
    use crate::test_support::tests::create_test_settings;
    use edgezero_core::outbound::{OutboundCachePolicy, ResponseMode};
    use error_stack::Report;

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
            api_token: Some(Redacted::new(format!(
                "{source_domain}-api-token-32-bytes-minimum"
            ))),
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

    /// The pull-sync URL `source_domain`'s partner receives for [`snapshot_ec_id`].
    fn pull_url(source_domain: &str) -> String {
        format!("https://{source_domain}/sync?ec_id={}", snapshot_ec_id())
    }

    async fn seed_present_snapshot(graph: &KvIdentityGraph, ec_id: &str) -> EcKvSnapshot {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        graph
            .create(ec_id, &entry)
            .await
            .expect("should seed live entry");
        graph.load_snapshot(ec_id).await
    }

    /// Runs pull sync to completion on `clock`, advancing it as the requests wait.
    fn run_pull_sync(
        settings: &Settings,
        graph: &KvIdentityGraph,
        registry: &PartnerRegistry,
        context: &PullSyncContext,
        client: &ScriptedHttpClient,
        clock: &TestClock,
    ) {
        clock.drive(dispatch_pull_sync(
            settings,
            graph,
            registry,
            &AllowAllRateLimiter,
            context,
            &client.http_client(),
            &clock.monotonic_clock(),
        ));
    }

    #[test]
    fn dispatch_pull_sync_runs_inside_the_fastly_executor_boundary() {
        futures::executor::block_on(async {
            let settings = create_test_settings();
            let registry =
                PartnerRegistry::from_config(&[pull_enabled_ec_partner("alpha.example.com")])
                    .expect("should build registry");
            let graph = KvIdentityGraph::in_memory("pull_store");
            let ec_id = snapshot_ec_id();
            let snapshot = seed_present_snapshot(&graph, &ec_id).await;
            let clock = TestClock::new();
            let client = ScriptedHttpClient::new(&clock);
            client.respond_to(
                Method::GET,
                &pull_url("alpha.example.com"),
                ScriptedReply::status(StatusCode::OK).body(r#"{"uid":"synced-uid"}"#),
            );
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
                &client.http_client(),
                &clock.monotonic_clock(),
            )
            .await;

            assert_eq!(
                client.requests().len(),
                1,
                "should dispatch once through the direct client"
            );
            let (entry, _) = graph
                .get(&ec_id)
                .await
                .expect("should read row")
                .expect("row should exist");
            assert_eq!(
                entry.ids.get("alpha.example.com").map(|id| id.uid.as_str()),
                Some("synced-uid"),
            );
        });
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_pull_sync_aggregates_batches_into_one_bulk_write() {
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
        let snapshot = seed_present_snapshot(&graph, &ec_id).await;

        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        // One JSON response per partner, collected across two concurrency batches.
        for source_domain in ["alpha.example.com", "beta.example.com"] {
            client.respond_to(
                Method::GET,
                &pull_url(source_domain),
                ScriptedReply::status(StatusCode::OK).body(r#"{"uid":"synced-uid"}"#),
            );
        }

        let context = PullSyncContext {
            ec_id: ec_id.clone(),
            snapshot,
        };
        run_pull_sync(&settings, &graph, &registry, &context, &client, &clock);

        let (entry, generation) = graph
            .get(&ec_id)
            .await
            .expect("should read store")
            .expect("should keep the entry");
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

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_pull_sync_sends_each_partner_its_bearer_token_and_transport() {
        let mut settings = create_test_settings();
        settings.ec.pull_sync_concurrency = 4;
        let registry =
            PartnerRegistry::from_config(&[pull_enabled_ec_partner("alpha.example.com")])
                .expect("should build pull registry");
        let graph = KvIdentityGraph::in_memory("pull_store");
        let ec_id = snapshot_ec_id();
        let snapshot = seed_present_snapshot(&graph, &ec_id).await;
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client.respond_to(
            Method::GET,
            &pull_url("alpha.example.com"),
            ScriptedReply::status(StatusCode::OK).body(r#"{"uid":"synced-uid"}"#),
        );

        let context = PullSyncContext { ec_id, snapshot };
        run_pull_sync(&settings, &graph, &registry, &context, &client, &clock);

        let requests = client.requests();
        let [request] = requests.as_slice() else {
            panic!("should send one pull request, got {requests:?}");
        };
        assert_eq!(request.method, Method::GET, "should pull with GET");
        assert_eq!(
            request.uri.to_string(),
            pull_url("alpha.example.com"),
            "should append the EC ID to the partner's pull URL"
        );
        request.assert_headers(&[("authorization", "Bearer outbound-token")]);
        request.assert_transport(ExpectedTransport {
            timeout: PULL_SYNC_GROUP_CUTOFF,
            response_mode: ResponseMode::Buffered {
                max_bytes: MAX_PULL_RESPONSE_BYTES,
            },
            response_decoding: ResponseDecoding::Decode,
            cache_policy: OutboundCachePolicy::PlatformDefault,
        });
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pull_sync_group_ends_at_fifteen_second_cutoff() {
        let mut settings = create_test_settings();
        settings.ec.pull_sync_concurrency = 2;
        let registry = PartnerRegistry::from_config(&[
            pull_enabled_ec_partner("alpha.example.com"),
            pull_enabled_ec_partner("beta.example.com"),
        ])
        .expect("should build pull registry");
        let graph = KvIdentityGraph::in_memory("pull_store");
        let ec_id = snapshot_ec_id();
        let snapshot = seed_present_snapshot(&graph, &ec_id).await;
        let clock = TestClock::new();
        let started = clock.monotonic_clock().now();
        let client = ScriptedHttpClient::new(&clock);
        client
            .respond_to(
                Method::GET,
                &pull_url("alpha.example.com"),
                ScriptedReply::status(StatusCode::OK)
                    .body(r#"{"uid":"fast-uid"}"#)
                    .after(Duration::from_millis(100)),
            )
            .respond_to(
                Method::GET,
                &pull_url("beta.example.com"),
                ScriptedReply::status(StatusCode::OK)
                    .body(r#"{"uid":"slow-uid"}"#)
                    .after(Duration::from_secs(20)),
            );

        let context = PullSyncContext {
            ec_id: ec_id.clone(),
            snapshot,
        };
        run_pull_sync(&settings, &graph, &registry, &context, &client, &clock);

        assert_eq!(
            clock
                .monotonic_clock()
                .now()
                .saturating_duration_since(started),
            Duration::from_secs(15),
            "should stop waiting for the group at its 15 s cutoff"
        );
        let cutoff = started
            .checked_add(Duration::from_secs(15))
            .expect("should represent the cutoff");
        for request in client.requests() {
            assert_eq!(
                request.budget.deadline.instant(),
                cutoff,
                "should bound {} by the group cutoff",
                request.uri
            );
        }
        let (entry, _) = graph
            .get(&ec_id)
            .await
            .expect("should read store")
            .expect("should keep the entry");
        assert_eq!(
            entry.ids.get("alpha.example.com").map(|id| id.uid.as_str()),
            Some("fast-uid"),
            "should keep the partner that answered before the cutoff"
        );
        assert!(
            !entry.ids.contains_key("beta.example.com"),
            "should drop the partner that answered after the cutoff"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pull_sync_failures_log_through_outbound_error() {
        let mut settings = create_test_settings();
        // One partner per group, so the failing batch ends only its own group.
        settings.ec.pull_sync_concurrency = 1;
        let mut unbuildable = pull_enabled_ec_partner("gamma.example.com");
        unbuildable.ts_pull_token = Some(Redacted::new("outbound\ntoken".to_owned()));
        let registry = PartnerRegistry::from_config(&[
            pull_enabled_ec_partner("alpha.example.com"),
            pull_enabled_ec_partner("beta.example.com"),
            unbuildable,
        ])
        .expect("should build pull registry");
        let graph = KvIdentityGraph::in_memory("pull_store");
        let ec_id = snapshot_ec_id();
        let snapshot = seed_present_snapshot(&graph, &ec_id).await;
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client
            .respond_to(
                Method::GET,
                &pull_url("alpha.example.com"),
                ScriptedReply::failing_batch(EdgeError::bad_gateway("alpha provider text")),
            )
            .respond_to(
                Method::GET,
                &pull_url("beta.example.com"),
                ScriptedReply::error(EdgeError::bad_gateway("beta provider text")),
            );
        let context = PullSyncContext { ec_id, snapshot };
        let capture = WarningCapture::start();

        run_pull_sync(&settings, &graph, &registry, &context, &client, &clock);

        let warnings = capture.take();
        let typed = warnings
            .iter()
            .filter(|warning| warning.message.starts_with("Outbound request failed with"))
            .count();
        assert_eq!(
            typed, 3,
            "should log the group failure, the request failure, and the build failure through outbound_error"
        );
        for step in [
            "partner group failed",
            "request for partner 'beta.example.com' failed",
            "request for partner 'gamma.example.com' could not be built",
        ] {
            assert!(
                warnings.iter().any(|warning| {
                    warning.message.starts_with("Pull sync failed:")
                        && warning.message.contains(step)
                }),
                "should log the report for `{step}`"
            );
        }
        assert!(
            warnings
                .iter()
                .all(|warning| !warning.message.contains("provider text")),
            "should keep the provider text out of the logs"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_pull_sync_drops_a_response_over_the_pull_cap() {
        let mut settings = create_test_settings();
        settings.ec.pull_sync_concurrency = 4;
        let registry =
            PartnerRegistry::from_config(&[pull_enabled_ec_partner("alpha.example.com")])
                .expect("should build pull registry");
        let graph = KvIdentityGraph::in_memory("pull_store");
        let ec_id = snapshot_ec_id();
        let snapshot = seed_present_snapshot(&graph, &ec_id).await;
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        let oversized = format!(
            "{{\"uid\":\"{}\"}}",
            "x".repeat(usize::try_from(MAX_PULL_RESPONSE_BYTES).expect("should fit usize"))
        );
        client.respond_to(
            Method::GET,
            &pull_url("alpha.example.com"),
            ScriptedReply::status(StatusCode::OK).body(oversized),
        );

        let context = PullSyncContext {
            ec_id: ec_id.clone(),
            snapshot,
        };
        run_pull_sync(&settings, &graph, &registry, &context, &client, &clock);

        let (entry, generation) = graph
            .get(&ec_id)
            .await
            .expect("should read store")
            .expect("should keep the entry");
        assert!(
            entry.ids.is_empty(),
            "should persist no UID from a response over the 64 KiB cap"
        );
        assert_eq!(generation, 1, "should not write the row back");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_pull_sync_calls_and_persists_only_missing_partner() {
        let mut settings = create_test_settings();
        settings.ec.pull_sync_concurrency = 4;
        let registry = PartnerRegistry::from_config(&[
            pull_enabled_ec_partner("alpha.example.com"),
            pull_enabled_ec_partner("beta.example.com"),
        ])
        .expect("should build pull registry");
        let graph = KvIdentityGraph::in_memory("pull_store");
        let ec_id = snapshot_ec_id();
        graph
            .create(
                &ec_id,
                &KvEntry::minimal("alpha.example.com", "existing-alpha", 1_000),
            )
            .await
            .expect("should seed partial entry");
        let snapshot = graph.load_snapshot(&ec_id).await;
        let clock = TestClock::new();
        let client = ScriptedHttpClient::new(&clock);
        client.respond_to(
            Method::GET,
            &pull_url("beta.example.com"),
            ScriptedReply::status(StatusCode::OK).body(r#"{"uid":"new-beta"}"#),
        );
        let context = PullSyncContext {
            ec_id: ec_id.clone(),
            snapshot,
        };

        run_pull_sync(&settings, &graph, &registry, &context, &client, &clock);

        let requested: Vec<String> = client
            .requests()
            .iter()
            .map(|request| request.uri.to_string())
            .collect();
        assert_eq!(
            requested,
            vec![pull_url("beta.example.com")],
            "should call only the missing partner"
        );
        let (entry, generation) = graph
            .get(&ec_id)
            .await
            .expect("should read store")
            .expect("should keep the entry");
        assert_eq!(
            entry
                .ids
                .get("alpha.example.com")
                .map(|partner_id| partner_id.uid.as_str()),
            Some("existing-alpha"),
            "existing UID should remain unchanged"
        );
        assert_eq!(
            entry
                .ids
                .get("beta.example.com")
                .map(|partner_id| partner_id.uid.as_str()),
            Some("new-beta"),
            "missing UID should persist"
        );
        assert_eq!(generation, 2, "missing UID should use one bulk CAS write");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_pull_sync_skips_non_present_snapshots() {
        let mut settings = create_test_settings();
        settings.ec.pull_sync_concurrency = 4;
        let registry =
            PartnerRegistry::from_config(&[pull_enabled_ec_partner("alpha.example.com")])
                .expect("should build registry");
        let graph = KvIdentityGraph::in_memory("pull_store");
        let ec_id = snapshot_ec_id();
        let client = ScriptedHttpClient::forbidding();
        let clock = TestClock::new();

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
            run_pull_sync(&settings, &graph, &registry, &context, &client, &clock);
        }

        client.assert_no_requests();
        assert!(
            graph
                .get(&ec_id)
                .await
                .expect("should read store")
                .is_none(),
            "no snapshot state should create a missing root"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_pull_sync_skips_tombstone_snapshot() {
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
        let client = ScriptedHttpClient::forbidding();
        let clock = TestClock::new();

        let context = PullSyncContext {
            ec_id: ec_id.clone(),
            snapshot,
        };
        run_pull_sync(&settings, &graph, &registry, &context, &client, &clock);

        client.assert_no_requests();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_pull_sync_skips_dispatch_when_ec_is_tombstoned_after_snapshot_capture() {
        // The request snapshot authorized pull sync, then a concurrent request
        // completed a CMP withdrawal while the page response was in flight.
        // Pull sync runs post-send and discloses the raw `ec_id` to partners, so
        // it must revalidate the live row and cancel rather than leak an
        // identity the user just withdrew.
        let mut settings = create_test_settings();
        settings.ec.pull_sync_concurrency = 4;
        let registry =
            PartnerRegistry::from_config(&[pull_enabled_ec_partner("alpha.example.com")])
                .expect("should build registry");
        let graph = KvIdentityGraph::in_memory("pull_store");
        let ec_id = snapshot_ec_id();
        let snapshot = seed_present_snapshot(&graph, &ec_id).await;

        // Concurrent withdrawal lands after the snapshot was captured.
        assert_eq!(
            graph
                .write_withdrawal_tombstone(&ec_id, drop)
                .await
                .expect("should tombstone the row"),
            TombstoneOutcome::Written,
            "should tombstone the row the snapshot still reports as present"
        );

        let client = ScriptedHttpClient::forbidding();
        let clock = TestClock::new();

        let context = PullSyncContext {
            ec_id: ec_id.clone(),
            snapshot,
        };
        run_pull_sync(&settings, &graph, &registry, &context, &client, &clock);

        client.assert_no_requests();
        let (entry, _) = graph
            .get(&ec_id)
            .await
            .expect("should read store")
            .expect("should keep the tombstone");
        assert!(
            entry.ids.is_empty(),
            "no partner UID may be written back onto a tombstoned row"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dispatch_pull_sync_skips_revalidation_read_when_no_partner_is_eligible() {
        // Every pull-enabled partner already has a UID in the request snapshot,
        // so there is nothing to dispatch. The revalidation read exists to
        // authorize outbound calls; with no calls to authorize it must not cost
        // a KV operation.
        let mut settings = create_test_settings();
        settings.ec.pull_sync_concurrency = 4;
        let registry =
            PartnerRegistry::from_config(&[pull_enabled_ec_partner("alpha.example.com")])
                .expect("should build registry");
        let graph = KvIdentityGraph::in_memory("pull_store");
        let ec_id = snapshot_ec_id();
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry.ids.insert(
            "alpha.example.com".to_owned(),
            crate::ec::kv_types::KvPartnerId {
                uid: "already-known".to_owned(),
            },
        );
        graph
            .create(&ec_id, &entry)
            .await
            .expect("should seed live entry");
        let snapshot = EcKvSnapshot::Present {
            ec_id: ec_id.clone(),
            entry: Box::new(entry),
            generation: Some(1),
        };

        let client = ScriptedHttpClient::forbidding();
        let clock = TestClock::new();

        let context = PullSyncContext {
            ec_id: ec_id.clone(),
            snapshot,
        };
        run_pull_sync(&settings, &graph, &registry, &context, &client, &clock);

        client.assert_no_requests();
    }
}
