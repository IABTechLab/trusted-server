//! EC response finalization.
//!
//! Centralizes post-routing EC behavior so all handlers get consistent cookie
//! and KV semantics.

use std::collections::HashSet;

use edgezero_core::body::Body as EdgeBody;
use http::Response;

use crate::constants::EC_RESPONSE_HEADERS;
use crate::settings::Settings;

use super::EcContext;
use super::cookies::{expire_ec_cookie, set_ec_cookie};
use super::kv::{
    CreateIfAbsentOutcome, EidCookieSyncOutcome, KvIdentityGraph, PartnerIdUpdate,
    apply_partner_id_updates,
};
use super::kv_types::KvEntry;
use super::module::{apply_module_response_headers, module_kv_key};
use super::prebid_eids::collect_eid_cookie_updates;
use super::pull_sync_marker::{expire_marker, reconcile_marker};
use super::registry::PartnerRegistry;
use super::{EcKvSnapshot, EidSyncSource, current_timestamp, log_id};

/// Finalizes EC response behavior for all routes.
///
/// Applies the resolved permission state, cookie reconciliation, Prebid EID
/// ingestion, and cookie writes for new EC generation.
///
/// When the request carries an explicit withdrawal signal (a storage opt-out or
/// a TCF record refusing storage) and the client presented a cookie, the browser
/// response clears the EC cookie immediately and the EC identity-graph KV
/// tombstone is the authoritative revocation marker. A request that is merely
/// not permitted (pre-consent or fail-closed) strips EC response headers but
/// leaves an already-issued cookie intact. There is no separate consent KV
/// store to clean up.
///
/// `eids_cookie` should be the raw value of the `ts-eids` cookie extracted
/// from the request *before* routing consumes it.
pub fn ec_finalize_response(
    settings: &Settings,
    ec_context: &mut EcContext,
    kv: Option<&KvIdentityGraph>,
    registry: &PartnerRegistry,
    eids_cookie: Option<&str>,
    sharedid_cookie: Option<&str>,
    response: &mut Response<EdgeBody>,
) {
    // Apply any response headers the active module asked for during
    // generation (for example to request more client evidence). This is empty
    // unless a module produced headers, so it is safe on every path. Each
    // one was checked against core's reserved response surface at capture
    // time in `EcContext::candidate_id`, so nothing here can set a
    // managed `ts-` cookie, an `x-ts-` header, or a framing or hop-by-hop
    // header. They accumulate with whatever the origin returned rather than
    // replacing it, for the reasons on
    // `module::apply_module_response_headers`.
    apply_module_response_headers(
        response.headers_mut(),
        ec_context.response_headers().iter().cloned(),
    );

    ec_context.validate_pull_sync_marker(settings, registry);
    let ec_permitted = ec_context.ec_allowed();

    if !ec_permitted {
        // The providers answer withdrawal when the permissions are assembled, so
        // the answer is read here rather than decoded again from the consent record.
        let storage_withdrawn = ec_context.storage_withdrawn();
        // Expire the request-local marker independently of the EC cookie: a
        // withdrawal must stop any pending pull-sync disclosure window.
        if storage_withdrawn && ec_context.pull_sync_marker().was_present() {
            expire_marker(ec_context.pull_sync_marker_mut(), response);
        }

        finalize_unusable_consent(
            settings,
            ec_context,
            kv,
            registry,
            storage_withdrawn,
            response,
        );
        return;
    }

    // Returning user: EC is permitted and came from the request.
    if ec_context.ec_was_present() && !ec_context.ec_generated() && ec_permitted {
        // Key the snapshot, EID ingestion and orphan recovery by the module's
        // canonical form of the identifier, the key the identity-graph row is
        // stored under, so an ingested EID lands on the live row rather than on
        // a second row keyed by the value the browser carries.
        if let (Some(graph), Some(kv_key)) = (kv, ec_context.ec_kv_key()) {
            let source = ec_context.eid_sync_source();
            let updates = source
                .map(|_| collect_eid_cookie_updates(eids_cookie, sharedid_cookie, registry))
                .unwrap_or_default();
            if let Some(source) = source {
                sync_eid_cookie_updates(graph, ec_context, &kv_key, &updates, source);
            }
            if matches!(ec_context.kv_snapshot(), EcKvSnapshot::Missing { .. })
                && ec_context.recovery_eligible()
            {
                confirm_then_recover_orphaned_ec(
                    settings, ec_context, graph, &kv_key, &updates, response,
                );
            }
        }

        reconcile_pull_sync_marker(settings, registry, ec_context, response);

        // Ordinary returning-user page views no longer refresh the browser
        // cookie, emit the EC header, or update KV TTL.
        return;
    }

    // Newly generated EC in this request. Do not emit a generated EC when
    // there is no KV graph: that would mint a browser cookie with no backing
    // identity-graph row, producing a phantom ID on later requests.
    if ec_context.ec_generated() {
        let (Some(graph), Some(kv_key)) = (kv, ec_context.ec_kv_key()) else {
            log::info!(
                "Skipping generated EC response write because the KV graph or the \
                 identity-graph key is unavailable"
            );
            reconcile_pull_sync_marker(settings, registry, ec_context, response);
            return;
        };

        let updates = collect_eid_cookie_updates(eids_cookie, sharedid_cookie, registry);
        sync_eid_cookie_updates(graph, ec_context, &kv_key, &updates, EidSyncSource::NewEc);
        if ec_context.kv_snapshot().entry_for(&kv_key).is_some() {
            set_ec_cookie_on_response(settings, ec_context, response);
        } else {
            log::warn!("Skipping generated EC cookie because backing row is not authoritative");
        }
    }

    reconcile_pull_sync_marker(settings, registry, ec_context, response);
}

fn sync_eid_cookie_updates(
    graph: &KvIdentityGraph,
    ec_context: &mut EcContext,
    ec_id: &str,
    updates: &[PartnerIdUpdate],
    source: EidSyncSource,
) {
    if updates.is_empty() {
        return;
    }

    let (snapshot, outcome) = graph.sync_eid_cookie_updates_from_snapshot(
        ec_id,
        updates,
        ec_context.kv_snapshot().clone(),
    );
    ec_context.set_kv_snapshot(snapshot);
    record_eid_sync_terminal(source, outcome);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EidSyncMeasurement {
    source: EidSyncSource,
    outcome: EidCookieSyncOutcome,
    already_matched: u8,
    written: u8,
    conflict_duplicate: u8,
    deferred: u8,
}

impl EidSyncMeasurement {
    fn new(source: EidSyncSource, outcome: EidCookieSyncOutcome) -> Self {
        Self {
            source,
            outcome,
            already_matched: u8::from(matches!(outcome, EidCookieSyncOutcome::AlreadyMatched)),
            written: u8::from(matches!(
                outcome,
                EidCookieSyncOutcome::Written | EidCookieSyncOutcome::WrittenWithDeferredFreshness
            )),
            conflict_duplicate: u8::from(matches!(outcome, EidCookieSyncOutcome::ConflictMatched)),
            deferred: u8::from(matches!(
                outcome,
                EidCookieSyncOutcome::WrittenWithDeferredFreshness
                    | EidCookieSyncOutcome::DeferredConflict
                    | EidCookieSyncOutcome::DeferredFreshness
                    | EidCookieSyncOutcome::DeferredStaleRead
            )),
        }
    }
}

fn record_eid_sync_terminal(source: EidSyncSource, outcome: EidCookieSyncOutcome) {
    let measurement = EidSyncMeasurement::new(source, outcome);
    log::info!(
        "EID sync measurement: source={} outcome={} attempted=1 already_matched={} written={} \
         conflict_duplicate={} deferred={}",
        measurement.source,
        measurement.outcome,
        measurement.already_matched,
        measurement.written,
        measurement.conflict_duplicate,
        measurement.deferred,
    );
}

fn reconcile_pull_sync_marker(
    settings: &Settings,
    registry: &PartnerRegistry,
    ec_context: &mut EcContext,
    response: &mut Response<EdgeBody>,
) {
    let ec_id = ec_context.ec_value().map(str::to_owned);
    let snapshot = ec_context.kv_snapshot().clone();
    reconcile_marker(
        settings,
        registry,
        ec_id.as_deref(),
        &snapshot,
        ec_context.pull_sync_marker_mut(),
        response,
    );
}

/// Rotates an orphaned identifier to a new one created by the selected module.
///
/// Called only once [`confirm_then_recover_orphaned_ec`] has proved the
/// orphan's row absent. The replacement goes through the same checks as a new
/// identifier, and its row is created only when no row already holds its key.
/// Every exit that does not rotate leaves a failed snapshot bound to the
/// orphan's key.
fn recover_orphaned_ec(
    settings: &Settings,
    ec_context: &mut EcContext,
    graph: &KvIdentityGraph,
    orphan_kv_key: &str,
    updates: &[super::kv::PartnerIdUpdate],
    response: &mut Response<EdgeBody>,
) {
    // A deployment with no module selected has nothing to rotate to.
    let Some(module) = ec_context.selected_module() else {
        return;
    };

    // Ask the selected module rather than the built-in generator, so a
    // vendor deployment never rotates an orphaned cookie into a built-in
    // identifier. Whether the client IP is needed is that module's decision.
    const MAX_RECOVERY_ATTEMPTS: usize = 5;
    for _attempt in 0..MAX_RECOVERY_ATTEMPTS {
        let ec_id = match ec_context.candidate_id(module.as_ref()) {
            Ok(Some(ec_id)) => ec_id,
            Ok(None) => {
                log::info!("Orphan EC recovery skipped because the module produced no identifier");
                ec_context.set_kv_snapshot(EcKvSnapshot::Failed {
                    ec_id: orphan_kv_key.to_owned(),
                });
                return;
            }
            Err(err) => {
                log::warn!("Orphan EC recovery ID generation failed: {err:?}");
                ec_context.set_kv_snapshot(EcKvSnapshot::Failed {
                    ec_id: orphan_kv_key.to_owned(),
                });
                return;
            }
        };
        let kv_key = module_kv_key(module.as_ref(), &ec_id);
        let mut entry = KvEntry::new(
            ec_context.consent(),
            ec_context.geo_info(),
            current_timestamp(),
            &settings.publisher.domain,
        );
        entry.device = ec_context
            .device_signals()
            .map(super::device::DeviceSignals::to_kv_device);
        apply_partner_id_updates(&mut entry, updates);

        match graph.create_if_absent(&kv_key, &entry) {
            Ok(CreateIfAbsentOutcome::Written) => {
                let snapshot = EcKvSnapshot::Present {
                    ec_id: kv_key,
                    entry: Box::new(entry),
                    generation: None,
                };
                ec_context.replace_with_generated(ec_id, snapshot);
                // A returning visitor runs no generation earlier in the
                // request, so these are only the headers the module asked
                // for while creating the replacement.
                apply_module_response_headers(
                    response.headers_mut(),
                    ec_context.response_headers().iter().cloned(),
                );
                set_ec_cookie_on_response(settings, ec_context, response);
                return;
            }
            Ok(CreateIfAbsentOutcome::AlreadyExists) => continue,
            Err(err) => {
                log::warn!("Orphan EC recovery failed: {err:?}");
                ec_context.set_kv_snapshot(EcKvSnapshot::Failed {
                    ec_id: orphan_kv_key.to_owned(),
                });
                return;
            }
        }
    }

    log::warn!("Orphan EC recovery exhausted collision retries");
    ec_context.set_kv_snapshot(EcKvSnapshot::Failed {
        ec_id: orphan_kv_key.to_owned(),
    });
}

/// Proves an orphaned cookie is genuinely absent before rotating it.
///
/// Rotation abandons a year-lived identity graph and its accumulated EIDs, so
/// it must never run on a stale read. The origin-overlapped preload reads the
/// row while the publisher origin is still in flight, and edge data stores are
/// eventually consistent: a recently created live key can read `Missing` at a
/// POP that has not converged. Two point reads do not fix that — both can be
/// stale — so absence has to be *proved*, not observed twice:
///
/// - a row that became visible after the origin round trip is adopted, with any
///   pending updates merged, and is never rotated;
/// - a second miss is escalated to
///   [`key_exists_confirmed`](KvIdentityGraph::key_exists_confirmed), which
///   reads the primary data source. Only a proven-absent key rotates;
/// - a key the store still lists is left alone: the point reads were stale, so
///   the identity stays intact and recovery is retried on a later navigation;
/// - neither a read failure nor a failed existence check is a miss, and neither
///   rotates.
fn confirm_then_recover_orphaned_ec(
    settings: &Settings,
    ec_context: &mut EcContext,
    graph: &KvIdentityGraph,
    kv_key: &str,
    updates: &[super::kv::PartnerIdUpdate],
    response: &mut Response<EdgeBody>,
) {
    let confirmed = graph.load_snapshot(kv_key);
    match confirmed {
        EcKvSnapshot::Present { .. } => {
            // The row became visible after the origin round trip: adopt it and
            // merge any pending updates rather than rotating a valid identity.
            ec_context.set_kv_snapshot(confirmed);
            sync_eid_cookie_updates(
                graph,
                ec_context,
                kv_key,
                updates,
                EidSyncSource::Navigation,
            );
        }
        EcKvSnapshot::Missing { .. } => match graph.key_exists_confirmed(kv_key) {
            Ok(false) => {
                recover_orphaned_ec(settings, ec_context, graph, kv_key, updates, response)
            }
            Ok(true) => {
                log::warn!(
                    "Orphan EC recovery skipped: both point reads missed a row the store still \
                     lists; leaving the identity intact for a later navigation"
                );
            }
            Err(err) => {
                log::warn!("Orphan EC recovery skipped: existence check failed: {err:?}");
            }
        },
        // A failed or not-read confirmation is not an authoritative miss: leave
        // the existing snapshot in place and do not rotate an unconfirmed miss.
        EcKvSnapshot::Failed { .. } | EcKvSnapshot::NotRead => {}
    }
}

/// Sets the EC cookie on response when an EC ID is available.
pub fn set_ec_cookie_on_response(
    settings: &Settings,
    ec_context: &EcContext,
    response: &mut Response<EdgeBody>,
) {
    if let Some(ec_id) = ec_context.ec_value() {
        set_ec_cookie(settings, response, ec_id);
    }
}

/// Removes EC-specific response headers.
///
/// In addition to the fixed [`EC_RESPONSE_HEADERS`], this also strips dynamic
/// `X-ts-<source_domain>` headers for registered partners. Other `x-ts-*`
/// headers are intentionally preserved because they may be set by non-EC middleware.
fn clear_ec_headers_on_response(
    response: &mut Response<EdgeBody>,
    registry: Option<&PartnerRegistry>,
) {
    for header in EC_RESPONSE_HEADERS {
        response.headers_mut().remove(*header);
    }

    if let Some(registry) = registry {
        for partner in registry.all() {
            response
                .headers_mut()
                .remove(partner_response_header(&partner.source_domain).as_str());
        }
    }
}

fn partner_response_header(source_domain: &str) -> String {
    format!("x-ts-{source_domain}")
}

/// Clears EC cookie and removes EC-specific response headers.
///
/// Used when the request carries an explicit withdrawal signal.
pub fn clear_ec_on_response(settings: &Settings, response: &mut Response<EdgeBody>) {
    expire_ec_cookie(settings, response);
    clear_ec_headers_on_response(response, None);
}

/// Finalizes a response whose consent does not currently permit an EC.
///
/// Covers explicit revocation and fail-closed cases alike, such as missing geo
/// or undecodable consent input: EC response headers always come off. The
/// browser cookie is expired and the identity-graph row tombstoned only when
/// the request carries an explicit withdrawal signal, so a visitor who has
/// simply not decided yet is not stripped of an identity they already hold.
fn finalize_unusable_consent(
    settings: &Settings,
    ec_context: &mut EcContext,
    kv: Option<&KvIdentityGraph>,
    registry: &PartnerRegistry,
    consent_withdrawn: bool,
    response: &mut Response<EdgeBody>,
) {
    clear_ec_headers_on_response(response, Some(registry));

    if !(consent_withdrawn && ec_context.cookie_was_present()) {
        return;
    }

    expire_ec_cookie(settings, response);

    // Compute once for the authoritative identity-graph tombstones, keyed by
    // the canonical form each row is stored under.
    let keys_to_withdraw = withdrawal_kv_keys(ec_context);
    let active_kv_key = ec_context.ec_kv_key();

    // The identity-graph tombstone is the authoritative withdrawal marker
    // for subsequent EC behavior.
    if let Some(graph) = kv {
        apply_withdrawal_tombstones(&keys_to_withdraw, |kv_key| {
            // The graph hands back the post-withdrawal snapshot rather than
            // leaving the caller to rebuild it, so post-send work that reads
            // the context, such as pull sync, which discloses the raw EC ID to
            // partners, sees the tombstone that was just written. Only the
            // active identifier has a snapshot in the context to correct.
            let initial = if ec_context.kv_snapshot().belongs_to(kv_key) {
                ec_context.kv_snapshot().clone()
            } else {
                EcKvSnapshot::NotRead
            };
            let outcome = graph.tombstone_existing_from_snapshot(kv_key, initial);
            // The browser cookie is already cleared, so a failed tombstone
            // leaves a live row that server-side consumers still read as
            // consented. Report every failure, including the non-active cookie
            // key whose outcome is not retained on the request context.
            if matches!(outcome, EcKvSnapshot::Failed { .. }) {
                log::warn!(
                    "EC withdrawal tombstone failed for '{}': the identity-graph row may \
                     still be live with consent granted",
                    log_id(kv_key)
                );
            }
            if active_kv_key.as_deref() == Some(kv_key) {
                ec_context.set_kv_snapshot(outcome);
            }
        });
    }
}

/// The identity-graph keys a withdrawal must tombstone.
///
/// Both the `ts-ec` cookie the request carried and the active identifier are
/// turned into keys by the module that owns them, so the tombstone lands on
/// the row the live identifier is stored under rather than on the raw cookie
/// value. An identifier no module this deployment reads owns produces no key
/// and is dropped, and with no module selected the built-in HMAC grammar
/// decides, as
/// [`AcceptedModules::canonical_kv_key`](super::module::AcceptedModules::canonical_kv_key)
/// does. The two collapse to one key when they are the same identity written
/// two ways.
fn withdrawal_kv_keys(ec_context: &EcContext) -> HashSet<String> {
    let mut keys = HashSet::new();

    if let Some(cookie_kv_key) = ec_context.cookie_ec_kv_key() {
        keys.insert(cookie_kv_key);
    }

    if let Some(active_kv_key) = ec_context.ec_kv_key() {
        keys.insert(active_kv_key);
    }

    keys
}

fn apply_withdrawal_tombstones<F>(kv_keys: &HashSet<String>, mut write_tombstone: F)
where
    F: FnMut(&str),
{
    for kv_key in kv_keys {
        write_tombstone(kv_key);
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};
    use crate::ec::tests::{CANONICAL_COOKIE_VALUE, CANONICAL_KV_KEY};
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;
    use crate::test_support::tests::create_test_settings;

    fn empty_response() -> Response<EdgeBody> {
        Response::builder()
            .status(200)
            .body(EdgeBody::empty())
            .expect("should build test response")
    }

    fn set_header(response: &mut Response<EdgeBody>, name: &str, value: &str) {
        response.headers_mut().insert(
            http::header::HeaderName::from_bytes(name.as_bytes())
                .expect("should parse header name"),
            HeaderValue::from_str(value).expect("should parse header value"),
        );
    }

    fn get_header<'a>(response: &'a Response<EdgeBody>, name: &str) -> Option<&'a HeaderValue> {
        response.headers().get(name)
    }

    fn get_header_str<'a>(response: &'a Response<EdgeBody>, name: &str) -> Option<&'a str> {
        response.headers().get(name).and_then(|v| v.to_str().ok())
    }

    fn make_context(
        ec_value: Option<&str>,
        cookie_ec_value: Option<&str>,
        ec_was_present: bool,
        ec_generated: bool,
        jurisdiction: Jurisdiction,
        ec_allowed: bool,
    ) -> EcContext {
        let consent = ConsentContext {
            jurisdiction,
            source: ConsentSource::Cookie,
            ..Default::default()
        };

        make_context_with_consent(
            ec_value,
            cookie_ec_value,
            ec_was_present,
            ec_generated,
            consent,
            ec_allowed,
        )
    }

    fn make_context_with_consent(
        ec_value: Option<&str>,
        cookie_ec_value: Option<&str>,
        ec_was_present: bool,
        ec_generated: bool,
        consent: ConsentContext,
        ec_allowed: bool,
    ) -> EcContext {
        EcContext::new_for_test_with_cookie(
            ec_value.map(str::to_owned),
            cookie_ec_value.map(str::to_owned),
            ec_was_present,
            ec_generated,
            consent,
            ec_allowed,
        )
    }

    fn canonicalizing_context(
        ec_was_present: bool,
        ec_generated: bool,
        consent: ConsentContext,
        ec_allowed: bool,
    ) -> EcContext {
        make_context_with_consent(
            Some(CANONICAL_COOKIE_VALUE),
            Some(CANONICAL_COOKIE_VALUE),
            ec_was_present,
            ec_generated,
            consent,
            ec_allowed,
        )
        .with_module_for_test(std::sync::Arc::new(crate::ec::tests::CanonicalizingModule))
    }

    fn graph_with_live_canonical_row() -> KvIdentityGraph {
        let graph = KvIdentityGraph::in_memory("finalize-canonical-store");
        graph
            .create(
                CANONICAL_KV_KEY,
                &crate::ec::kv_types::KvEntry::minimal(
                    "ssp.example.com",
                    "partner-uid-123",
                    1_741_824_000,
                ),
            )
            .expect("should write the row generation keys by the canonical form");
        graph
    }

    fn sample_ec_id(suffix: &str) -> String {
        format!("{}.{suffix}", "a".repeat(64))
    }

    fn make_partner(source_domain: &str) -> EcPartner {
        EcPartner {
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: Some(Redacted::new(format!(
                "token-{source_domain}-32-bytes-minimum-value"
            ))),
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        }
    }

    #[test]
    fn withdrawal_kv_keys_covers_the_cookie_and_the_active_identifier() {
        let active = sample_ec_id("activ1");
        let cookie = sample_ec_id("cook1e");
        let same = sample_ec_id("same01");
        let valid = sample_ec_id("valid1");
        for (case, active_ec, cookie_ec, expected) in [
            (
                "the cookie alone when no identifier is active",
                None,
                Some(cookie.as_str()),
                vec![cookie.as_str()],
            ),
            (
                "one key when the cookie and the active identifier match",
                Some(same.as_str()),
                Some(same.as_str()),
                vec![same.as_str()],
            ),
            (
                "both keys when the cookie and the active identifier differ",
                Some(active.as_str()),
                Some(cookie.as_str()),
                vec![active.as_str(), cookie.as_str()],
            ),
            (
                "no key for a malformed value",
                Some(valid.as_str()),
                Some("not-an-ec-id"),
                vec![valid.as_str()],
            ),
        ] {
            let ec_context = make_context(
                active_ec,
                cookie_ec,
                true,
                false,
                Jurisdiction::Unknown,
                false,
            );
            let expected: HashSet<String> = expected.into_iter().map(str::to_owned).collect();
            assert_eq!(withdrawal_kv_keys(&ec_context), expected, "{case}");
        }
    }

    #[test]
    fn apply_withdrawal_tombstones_invokes_writer_for_each_ec_id() {
        let first = sample_ec_id("first1");
        let second = sample_ec_id("second");
        let mut ids = HashSet::new();
        ids.insert(first.clone());
        ids.insert(second.clone());

        let mut written = Vec::new();
        apply_withdrawal_tombstones(&ids, |ec_id| written.push(ec_id.to_owned()));
        written.sort();

        let mut expected = vec![first, second];
        expected.sort();
        assert_eq!(written, expected, "should write a tombstone for each EC ID");
    }

    #[test]
    fn clear_ec_on_response_removes_headers_and_expires_cookie() {
        let settings = create_test_settings();
        let mut response = empty_response();
        set_header(&mut response, "x-ts-ec", "abc");
        set_header(&mut response, "x-ts-eids", "[]");
        set_header(&mut response, "x-ts-unrelated", "keep-me");

        clear_ec_on_response(&settings, &mut response);

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "should remove x-ts-ec"
        );
        assert!(
            get_header(&response, "x-ts-eids").is_none(),
            "should remove x-ts-eids"
        );
        assert_eq!(
            get_header_str(&response, "x-ts-unrelated"),
            Some("keep-me"),
            "should preserve unrelated x-ts headers without a partner registry"
        );

        let set_cookie = get_header(&response, "set-cookie")
            .expect("should append Set-Cookie for expiry")
            .to_str()
            .expect("should render set-cookie as utf-8");

        assert!(
            set_cookie.contains("Max-Age=0"),
            "should expire the EC cookie"
        );
    }

    #[test]
    fn finalize_withdrawal_does_not_create_a_row_for_an_unheld_identity() {
        let settings = create_test_settings();
        // The cookie value is chosen by the client, so a withdrawal naming an
        // identity this deployment never issued must not put a row in the
        // identity graph.
        let ec_id = sample_ec_id("zz9999");
        // A TCF record refusing storage under the requires-signal floor is
        // the withdrawal trigger. The TCF provider answers that at assembly,
        // and core links no provider, so the answer is stated here.
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::Gdpr,
            tcf: Some(refusing_tcf()),
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context =
            make_context_with_consent(Some(&ec_id), Some(&ec_id), true, false, consent, false)
                .with_storage_withdrawn_for_test(true);
        let kv = KvIdentityGraph::in_memory("test-store");
        let mut response = empty_response();
        let registry = PartnerRegistry::from_config(&[]).expect("should build registry");

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&kv),
            &registry,
            None,
            None,
            &mut response,
        );

        assert!(
            kv.get(&ec_id).expect("should read back").is_none(),
            "should not write a tombstone for an identity that was never issued"
        );
        assert!(
            matches!(ec_context.kv_snapshot(), EcKvSnapshot::Missing { .. }),
            "should record the confirmed missing identity"
        );
        let set_cookie = get_header_str(&response, "set-cookie").unwrap_or_default();
        assert!(
            set_cookie.contains("Max-Age=0"),
            "should still expire the browser cookie, which is the primary enforcement"
        );
    }

    #[test]
    fn finalize_withdrawal_tombstones_a_held_identity() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("held01");
        // A TCF record refusing storage under the requires-signal floor is
        // the withdrawal trigger. The TCF provider answers that at assembly,
        // and core links no provider, so the answer is stated here.
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::Gdpr,
            tcf: Some(refusing_tcf()),
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context =
            make_context_with_consent(Some(&ec_id), Some(&ec_id), true, false, consent, false)
                .with_storage_withdrawn_for_test(true);
        let kv = KvIdentityGraph::stale_lookup("test-store", 1);
        kv.create(
            &ec_id,
            &crate::ec::kv_types::KvEntry::minimal("p.example", "uid", 1),
        )
        .expect("should seed the identity");
        ec_context.set_kv_snapshot(kv.load_snapshot(&ec_id));
        ec_context.set_eid_sync_source(EidSyncSource::Auction);
        assert!(matches!(
            ec_context.kv_snapshot(),
            EcKvSnapshot::Missing { .. }
        ));
        let mut response = empty_response();
        let registry = PartnerRegistry::from_config(&[]).expect("should build registry");

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&kv),
            &registry,
            None,
            None,
            &mut response,
        );

        let (entry, _) = kv
            .get(&ec_id)
            .expect("should read back")
            .expect("should still hold the identity");
        assert!(
            !entry.consent.ok,
            "a genuine withdrawal must still tombstone the identity"
        );
        let snapshot_entry = ec_context
            .kv_snapshot()
            .entry_for(&ec_id)
            .expect("should replace the stale miss with a tombstone snapshot");
        assert!(!snapshot_entry.consent.ok && snapshot_entry.ids.is_empty());
        assert_eq!(ec_context.kv_snapshot().generation_for(&ec_id), None);
    }

    #[test]
    fn withdrawal_still_expires_the_cookie_when_the_store_is_unavailable() {
        // Cookie expiry is the primary enforcement, so it has to survive a
        // store that cannot answer at all — the case where the identity-graph
        // marker is exactly what goes missing.
        let settings = create_test_settings();
        let ec_id = sample_ec_id("dead01");
        // A TCF record refusing storage under the requires-signal floor is
        // the withdrawal trigger. The TCF provider answers that at assembly,
        // and core links no provider, so the answer is stated here.
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::Gdpr,
            tcf: Some(refusing_tcf()),
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context =
            make_context_with_consent(Some(&ec_id), Some(&ec_id), true, false, consent, false)
                .with_storage_withdrawn_for_test(true);
        ec_context.set_kv_snapshot(EcKvSnapshot::Present {
            ec_id: ec_id.clone(),
            entry: Box::new(live_entry()),
            generation: Some(1),
        });
        let kv = KvIdentityGraph::failing("test-store");
        let mut response = empty_response();
        set_header(&mut response, "x-ts-ec", "stale");
        let registry = PartnerRegistry::from_config(&[]).expect("should build registry");

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&kv),
            &registry,
            None,
            None,
            &mut response,
        );

        let set_cookie = get_header_str(&response, "set-cookie").unwrap_or_default();
        assert!(
            set_cookie.contains("Max-Age=0"),
            "should expire the EC cookie even when the store is unavailable: {set_cookie}"
        );
        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "should still strip EC response headers"
        );
        assert!(
            matches!(ec_context.kv_snapshot(), EcKvSnapshot::Failed { .. }),
            "should invalidate the live snapshot when withdrawal cannot be confirmed"
        );
    }

    #[test]
    fn finalize_withdrawal_clears_cookie_and_headers() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("aBc123");
        // A TCF record refusing storage is the withdrawal trigger, under a
        // storage baseline at the requires-signal floor, where refusing the
        // signal storage depends on is destructive. The TCF provider answers
        // that at assembly, and core links no provider, so the answer is
        // stated here and what finalization does with it is what is tested.
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::Gdpr,
            tcf: Some(refusing_tcf()),
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context =
            make_context_with_consent(Some(&ec_id), Some(&ec_id), true, false, consent, false)
                .with_storage_withdrawn_for_test(true);
        let mut response = empty_response();
        set_header(&mut response, "x-ts-ec", "stale");
        set_header(&mut response, "x-ts-eids", "[]");
        set_header(&mut response, "x-ts-ssp.example.com", "partner-uid-123");
        set_header(&mut response, "x-ts-unrelated", "keep-me");

        let partners = vec![make_partner("ssp.example.com")];
        let test_registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        ec_finalize_response(
            &settings,
            &mut ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "withdrawal should clear x-ts-ec header"
        );
        assert!(
            get_header(&response, "x-ts-eids").is_none(),
            "withdrawal should clear x-ts-eids header"
        );
        assert!(
            get_header(&response, "x-ts-ssp.example.com").is_none(),
            "withdrawal should clear registered partner header"
        );
        assert_eq!(
            get_header_str(&response, "x-ts-unrelated"),
            Some("keep-me"),
            "withdrawal should preserve unrelated x-ts header"
        );
        let set_cookie = get_header(&response, "set-cookie")
            .expect("withdrawal should expire cookie")
            .to_str()
            .expect("set-cookie should be utf-8");
        assert!(
            set_cookie.contains("Max-Age=0"),
            "withdrawal should set Max-Age=0"
        );
    }

    /// A decoded TCF record refusing every purpose, storage included.
    fn refusing_tcf() -> crate::consent::TcfConsent {
        crate::consent::TcfConsent {
            version: 2,
            cmp_id: 0,
            cmp_version: 0,
            consent_screen: 0,
            consent_language: "EN".to_owned(),
            vendor_list_version: 0,
            tcf_policy_version: 2,
            created_ds: 0,
            last_updated_ds: 0,
            purpose_consents: vec![false; 24],
            purpose_legitimate_interests: vec![false; 24],
            vendor_consents: Vec::new(),
            vendor_legitimate_interests: Vec::new(),
            special_feature_opt_ins: vec![false; 12],
        }
    }

    #[test]
    fn finalize_gpc_suppresses_headers_but_keeps_the_cookie() {
        // A US-style opt-out suppresses use (headers cleared, nothing egressed)
        // but is never destructive: the browser cookie is not expired, so a
        // visitor who later withdraws the opt-out keeps their identity.
        let settings = create_test_settings();
        let ec_id = sample_ec_id("aBc123");
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context =
            make_context_with_consent(Some(&ec_id), Some(&ec_id), true, false, consent, false);
        let mut response = empty_response();
        set_header(&mut response, "x-ts-ec", "stale");

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &mut ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "the opt-out should clear the EC header"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "the opt-out should not expire the browser cookie"
        );
    }

    #[test]
    fn finalize_returning_user_with_cookie_mismatch_sets_no_header_or_cookie() {
        let settings = create_test_settings();
        let active_ec = sample_ec_id("activ1");
        let cookie_ec = sample_ec_id("cook1e");
        let mut ec_context = make_context(
            Some(&active_ec),
            Some(&cookie_ec),
            true,
            false,
            Jurisdiction::NonRegulated,
            true,
        );
        let mut response = empty_response();

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &mut ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "returning user should not set x-ts-ec"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "returning user should not refresh or repair cookie"
        );
    }

    #[test]
    fn finalize_returning_user_sets_no_header_or_cookie() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("mtch01");
        let mut ec_context = make_context(
            Some(&ec_id),
            Some(&ec_id),
            true,
            false,
            Jurisdiction::NonRegulated,
            true,
        );
        let mut response = empty_response();

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &mut ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "returning user should not set x-ts-ec"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "returning user should not refresh cookie"
        );
    }

    #[test]
    fn finalize_generated_ec_without_kv_skips_cookie_and_header() {
        let settings = create_test_settings();
        let generated_ec = sample_ec_id("gen123");
        let mut ec_context = make_context(
            Some(&generated_ec),
            None,
            false,
            true,
            Jurisdiction::NonRegulated,
            true,
        );
        let mut response = empty_response();

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &mut ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "generated EC without KV should not set response header"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "generated EC without KV should not set cookie"
        );
    }

    #[test]
    fn finalize_rotates_orphaned_cookie_to_new_backed_ec() {
        let settings = create_test_settings();
        let orphaned_ec = sample_ec_id("orphn1");
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context = EcContext::new_for_test_with_ip(
            Some(orphaned_ec.clone()),
            consent,
            Some("192.0.2.10".to_owned()),
        )
        .with_module_for_test(crate::ec::tests::hmac_module());
        ec_context.set_recovery_eligible(true);
        ec_context.set_kv_snapshot(EcKvSnapshot::Missing {
            ec_id: orphaned_ec.clone(),
        });
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        let replacement = ec_context.ec_value().expect("should rotate orphan");
        assert_ne!(replacement, orphaned_ec);
        assert!(
            graph
                .get(replacement)
                .expect("should read replacement")
                .is_some(),
            "replacement cookie should have a backing row"
        );
        assert!(
            get_header(&response, "set-cookie").is_some(),
            "should emit replacement cookie after persistence"
        );
    }

    #[test]
    fn valid_marker_with_unread_snapshot_defers_orphan_recovery() {
        let settings = create_test_settings();
        let orphaned_ec = sample_ec_id("orphn2");
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context = EcContext::new_for_test_with_ip(
            Some(orphaned_ec.clone()),
            consent,
            Some("192.0.2.10".to_owned()),
        );
        ec_context.set_recovery_eligible(true);
        ec_context.set_pull_sync_marker_for_test(
            crate::ec::pull_sync_marker::PullSyncMarkerState::Valid { expires_at: 4_600 },
        );
        let mut partner = make_partner("pull.example.com");
        partner.pull_sync_enabled = true;
        partner.pull_sync_url = Some("https://sync.example.com/pull".to_owned());
        partner.pull_sync_allowed_domains = vec!["sync.example.com".to_owned()];
        partner.ts_pull_token = Some(Redacted::new("pull-token".to_owned()));
        let registry = PartnerRegistry::from_config(&[partner]).expect("should build registry");
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            None,
            None,
            &mut response,
        );

        assert_eq!(
            ec_context.ec_value(),
            Some(orphaned_ec.as_str()),
            "an unread snapshot should defer orphan rotation until marker expiry"
        );
        assert!(
            matches!(ec_context.kv_snapshot(), EcKvSnapshot::NotRead),
            "a marker-skipped request should leave the snapshot unread"
        );
        assert!(
            response.headers().get(http::header::SET_COOKIE).is_none(),
            "bounded orphan deferral should not rewrite browser identity state"
        );
    }

    #[test]
    fn finalize_returning_user_subresource_does_not_persist_eid_updates() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("subeid");
        let graph = KvIdentityGraph::in_memory("test_store");
        let live = KvEntry::new(
            &granting_consent(),
            None,
            current_timestamp(),
            &settings.publisher.domain,
        );
        graph
            .create(&ec_id, &live)
            .expect("should seed the live row");
        let mut ec_context = returning_user_context(&ec_id, graph.load_snapshot(&ec_id), false);
        let partners = vec![make_partner("sharedid.org")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            None,
            Some("shared-cookie-id"),
            &mut response,
        );

        let (stored, _) = graph
            .get(&ec_id)
            .expect("should read store")
            .expect("row should remain");
        assert!(
            !stored.ids.contains_key("sharedid.org"),
            "a subresource response must not persist request EID cookies"
        );
    }

    #[test]
    fn finalize_navigation_routes_persist_returning_user_eid_updates() {
        for (source, suffix, cookie_id) in [
            (EidSyncSource::Navigation, "naveid", "navigation-cookie-id"),
            (EidSyncSource::PageBids, "spaeid", "page-bids-cookie-id"),
        ] {
            let settings = create_test_settings();
            let ec_id = sample_ec_id(suffix);
            let graph = KvIdentityGraph::in_memory("test_store");
            let live = KvEntry::new(
                &granting_consent(),
                None,
                current_timestamp(),
                &settings.publisher.domain,
            );
            graph
                .create(&ec_id, &live)
                .expect("should seed the live row");
            let mut ec_context = returning_user_context(&ec_id, graph.load_snapshot(&ec_id), true);
            ec_context.set_eid_sync_source(source);
            let partners = vec![make_partner("sharedid.org")];
            let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
            let mut response = empty_response();

            ec_finalize_response(
                &settings,
                &mut ec_context,
                Some(&graph),
                &registry,
                None,
                Some(cookie_id),
                &mut response,
            );

            let (stored, _) = graph
                .get(&ec_id)
                .expect("should read store")
                .expect("row should remain");
            assert_eq!(
                stored.ids.get("sharedid.org").map(|id| id.uid.as_str()),
                Some(cookie_id),
                "{source} should persist the returning-user EID cookie"
            );
        }
    }

    #[test]
    fn finalize_generated_ec_persists_eid_updates() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("geneid");
        let graph = KvIdentityGraph::in_memory("test_store");
        let live = KvEntry::new(
            &granting_consent(),
            None,
            current_timestamp(),
            &settings.publisher.domain,
        );
        graph
            .create(&ec_id, &live)
            .expect("should seed generated row");
        let mut ec_context = make_context(
            Some(&ec_id),
            None,
            false,
            true,
            Jurisdiction::NonRegulated,
            true,
        );
        ec_context.set_kv_snapshot(graph.load_snapshot(&ec_id));
        let partners = vec![make_partner("sharedid.org")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            None,
            Some("generated-cookie-id"),
            &mut response,
        );

        let (stored, _) = graph
            .get(&ec_id)
            .expect("should read store")
            .expect("row should remain");
        assert_eq!(
            stored.ids.get("sharedid.org").map(|id| id.uid.as_str()),
            Some("generated-cookie-id")
        );
    }

    #[test]
    fn eid_sync_measurement_dimensions_are_bounded_and_identity_free() {
        let sources = [
            EidSyncSource::Navigation,
            EidSyncSource::Auction,
            EidSyncSource::PageBids,
            EidSyncSource::NewEc,
        ];
        let outcomes = [
            EidCookieSyncOutcome::AlreadyMatched,
            EidCookieSyncOutcome::Written,
            EidCookieSyncOutcome::WrittenWithDeferredFreshness,
            EidCookieSyncOutcome::ConflictMatched,
            EidCookieSyncOutcome::DeferredConflict,
            EidCookieSyncOutcome::DeferredFreshness,
            EidCookieSyncOutcome::DeferredStaleRead,
            EidCookieSyncOutcome::Missing,
            EidCookieSyncOutcome::ConsentWithdrawn,
            EidCookieSyncOutcome::Failed,
        ];

        assert_eq!(
            sources.map(|source| source.to_string()),
            ["navigation", "auction", "page_bids", "new_ec"]
        );
        assert_eq!(
            outcomes.map(|outcome| outcome.to_string()),
            [
                "already_matched",
                "written",
                "written_with_deferred_freshness",
                "conflict_matched",
                "deferred_conflict",
                "deferred_freshness",
                "deferred_stale_read",
                "missing",
                "consent_withdrawn",
                "failed",
            ]
        );

        assert_eq!(
            EidSyncMeasurement::new(
                EidSyncSource::Navigation,
                EidCookieSyncOutcome::AlreadyMatched,
            ),
            EidSyncMeasurement {
                source: EidSyncSource::Navigation,
                outcome: EidCookieSyncOutcome::AlreadyMatched,
                already_matched: 1,
                written: 0,
                conflict_duplicate: 0,
                deferred: 0,
            }
        );
        assert_eq!(
            EidSyncMeasurement::new(
                EidSyncSource::Auction,
                EidCookieSyncOutcome::WrittenWithDeferredFreshness,
            ),
            EidSyncMeasurement {
                source: EidSyncSource::Auction,
                outcome: EidCookieSyncOutcome::WrittenWithDeferredFreshness,
                already_matched: 0,
                written: 1,
                conflict_duplicate: 0,
                deferred: 1,
            }
        );
        assert_eq!(
            EidSyncMeasurement::new(EidSyncSource::NewEc, EidCookieSyncOutcome::ConflictMatched,),
            EidSyncMeasurement {
                source: EidSyncSource::NewEc,
                outcome: EidCookieSyncOutcome::ConflictMatched,
                already_matched: 0,
                written: 0,
                conflict_duplicate: 1,
                deferred: 0,
            }
        );
        assert_eq!(
            EidSyncMeasurement::new(
                EidSyncSource::PageBids,
                EidCookieSyncOutcome::DeferredStaleRead,
            ),
            EidSyncMeasurement {
                source: EidSyncSource::PageBids,
                outcome: EidCookieSyncOutcome::DeferredStaleRead,
                already_matched: 0,
                written: 0,
                conflict_duplicate: 0,
                deferred: 1,
            }
        );
    }

    #[test]
    fn finalize_auction_transient_miss_still_persists_eid_updates() {
        // `/auction` saves its first lookup into the context and is never
        // recovery eligible, so a stale miss there has no later chance to
        // retry. Finalization must revalidate before dropping collected IDs.
        let settings = create_test_settings();
        let ec_id = sample_ec_id("named1");
        let graph = KvIdentityGraph::in_memory("test_store");
        let live = KvEntry::new(
            &granting_consent(),
            None,
            current_timestamp(),
            &settings.publisher.domain,
        );
        graph
            .create(&ec_id, &live)
            .expect("should seed the live row the endpoint lookup missed");
        let mut ec_context = returning_user_context(
            &ec_id,
            EcKvSnapshot::Missing {
                ec_id: ec_id.clone(),
            },
            false,
        );
        ec_context.set_eid_sync_source(EidSyncSource::Auction);
        let partners = vec![make_partner("sharedid.org")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            None,
            Some("shared-cookie-id"),
            &mut response,
        );

        assert_did_not_rotate(&ec_context, &ec_id, &response);
        let (stored, _) = graph
            .get(&ec_id)
            .expect("should read store")
            .expect("row should remain");
        assert_eq!(
            stored.ids.get("sharedid.org").map(|id| id.uid.as_str()),
            Some("shared-cookie-id"),
            "a stale endpoint miss must not suppress EID persistence"
        );
    }

    #[test]
    fn finalize_auction_confirmed_miss_does_not_create_a_row() {
        // The same path with a genuinely absent row must stay a no-op: a route
        // without orphan recovery must never mint an identity-graph entry.
        let settings = create_test_settings();
        let ec_id = sample_ec_id("named2");
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut ec_context = returning_user_context(
            &ec_id,
            EcKvSnapshot::Missing {
                ec_id: ec_id.clone(),
            },
            false,
        );
        ec_context.set_eid_sync_source(EidSyncSource::Auction);
        let partners = vec![make_partner("sharedid.org")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            None,
            Some("shared-cookie-id"),
            &mut response,
        );

        assert_did_not_rotate(&ec_context, &ec_id, &response);
        assert!(
            graph.get(&ec_id).expect("should read store").is_none(),
            "a confirmed miss must not create a root entry"
        );
    }

    #[test]
    fn finalize_transient_missing_row_confirms_present_and_does_not_rotate() {
        // The origin-overlapped preload transiently read `Missing` on an
        // eventually-consistent store, but the row actually exists. The
        // confirming re-read at finalize must adopt the live row instead of
        // rotating a valid identity (transient Add -> Missing -> Present).
        let settings = create_test_settings();
        let orphan = sample_ec_id("trans1");
        let graph = KvIdentityGraph::in_memory("test_store");
        let live = KvEntry::new(
            &granting_consent(),
            None,
            current_timestamp(),
            &settings.publisher.domain,
        );
        graph
            .create(&orphan, &live)
            .expect("should seed the live row the preload missed");
        let mut ec_context = returning_user_context(
            &orphan,
            EcKvSnapshot::Missing {
                ec_id: orphan.clone(),
            },
            true,
        );
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        assert_did_not_rotate(&ec_context, &orphan, &response);
        assert!(
            matches!(ec_context.kv_snapshot(), EcKvSnapshot::Present { .. }),
            "confirming read must adopt the now-visible row rather than rotating"
        );
    }

    #[test]
    fn finalize_two_missing_reads_do_not_rotate_a_row_the_store_still_lists() {
        // Both the origin-overlapped preload and the confirming re-read missed,
        // but the row is live — the point reads were stale. Two stale reads are
        // not an absence proof, so the identity graph must be left intact and
        // recovery retried on a later navigation rather than fragmented behind
        // a replacement ID.
        let settings = create_test_settings();
        let orphan = sample_ec_id("stale1");
        // One stale read: the preload miss is the `Missing` snapshot below, and
        // this makes the confirming re-read miss too.
        let graph = KvIdentityGraph::stale_lookup("test_store", 1);
        let live = KvEntry::new(
            &granting_consent(),
            None,
            current_timestamp(),
            &settings.publisher.domain,
        );
        graph
            .create(&orphan, &live)
            .expect("should seed the live row both point reads miss");
        let mut ec_context = returning_user_context(
            &orphan,
            EcKvSnapshot::Missing {
                ec_id: orphan.clone(),
            },
            true,
        );
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        assert_did_not_rotate(&ec_context, &orphan, &response);
        assert_eq!(
            graph
                .get(&orphan)
                .expect("should read store")
                .map(|(entry, _)| entry.consent.ok),
            Some(true),
            "the original identity row must survive two stale point reads"
        );
    }

    #[test]
    fn finalize_rotates_when_only_a_longer_key_shares_the_orphan_prefix() {
        // `key_exists_confirmed` gates orphan recovery as well as withdrawal.
        // It matches whole keys, so a neighbouring key that merely starts with
        // the orphaned ID cannot answer for it. Under the prefix count this
        // path used before, that neighbour reported the orphan as still held
        // and suppressed a rotation the visitor needed — the same collision
        // this PR closes on the withdrawal path.
        let settings = create_test_settings();
        let orphan = sample_ec_id("prefix");
        let neighbour = format!("{orphan}-longer");
        let graph = KvIdentityGraph::in_memory("test_store");
        let live = KvEntry::new(
            &granting_consent(),
            None,
            current_timestamp(),
            &settings.publisher.domain,
        );
        graph
            .create(&neighbour, &live)
            .expect("should seed the neighbouring row");
        assert!(
            !graph
                .key_exists_confirmed(&orphan)
                .expect("should confirm against the store"),
            "a longer key sharing the prefix must not prove the orphan exists"
        );
        let mut ec_context = returning_user_context(
            &orphan,
            EcKvSnapshot::Missing {
                ec_id: orphan.clone(),
            },
            true,
        );
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        let replacement = ec_context.ec_value().expect("should rotate the orphan");
        assert_ne!(
            replacement, orphan,
            "a proven-absent orphan must rotate even with a prefix neighbour present"
        );
        assert!(
            graph
                .get(replacement)
                .expect("should read the replacement")
                .is_some(),
            "the replacement cookie should have a backing row"
        );
        assert!(
            graph
                .get(&neighbour)
                .expect("should read the neighbour")
                .is_some(),
            "the neighbouring identity must be left untouched"
        );
    }

    /// A returning visitor's context on a recovery-eligible navigation whose
    /// row is missing, with `module` selected.
    fn orphan_context(
        orphan: &str,
        client_ip: Option<&str>,
        module: std::sync::Arc<dyn crate::ec::module::EdgeCookieModule>,
    ) -> EcContext {
        let mut ec = EcContext::new_for_test_with_ip(
            Some(orphan.to_owned()),
            granting_consent(),
            client_ip.map(str::to_owned),
        )
        .with_module_for_test(module);
        ec.set_recovery_eligible(true);
        ec.set_kv_snapshot(EcKvSnapshot::Missing {
            ec_id: orphan.to_owned(),
        });
        ec
    }

    /// A store that holds no row and refuses every write, so a proven-absent
    /// orphan reaches the replacement write and that write fails.
    struct EmptyUnwritableEcKv;

    impl crate::ec::kv_backend::EcKvStore for EmptyUnwritableEcKv {
        fn store_name(&self) -> &str {
            "empty-unwritable-store"
        }

        fn lookup(
            &self,
            _key: &str,
        ) -> Result<
            Option<crate::ec::kv_backend::EcKvLookup>,
            error_stack::Report<crate::error::TrustedServerError>,
        > {
            Ok(None)
        }

        fn key_exists(
            &self,
            _key: &str,
        ) -> Result<bool, error_stack::Report<crate::error::TrustedServerError>> {
            Ok(false)
        }

        fn insert(
            &self,
            _key: &str,
            _write: crate::ec::kv_backend::EcKvWrite<'_>,
        ) -> Result<
            crate::ec::kv_backend::EcKvWriteOutcome,
            error_stack::Report<crate::error::TrustedServerError>,
        > {
            Err(error_stack::Report::new(
                crate::error::TrustedServerError::KvStore {
                    store_name: "empty-unwritable-store".to_owned(),
                    message: "the test store refuses every write".to_owned(),
                },
            ))
        }

        fn list_keys_with_prefix(
            &self,
            _prefix: &str,
            _limit: u32,
        ) -> Result<Vec<String>, error_stack::Report<crate::error::TrustedServerError>> {
            Ok(Vec::new())
        }

        fn delete(
            &self,
            _key: &str,
        ) -> Result<(), error_stack::Report<crate::error::TrustedServerError>> {
            Ok(())
        }
    }

    #[test]
    fn an_orphan_recovery_that_cannot_rotate_leaves_a_failed_snapshot() {
        // Recovery runs only once the orphan's row is proven absent. Each of
        // these exits gives up without a replacement, and each must leave the
        // orphan in place, write no cookie and record a failed snapshot for
        // the orphan's key, so nothing later treats the miss as authoritative.
        let settings = create_test_settings();
        let hmac_orphan = sample_ec_id("orphn2");
        let cases: [(&str, EcContext, KvIdentityGraph); 3] = [
            (
                "the HMAC module with no client IP",
                orphan_context(&hmac_orphan, None, crate::ec::tests::hmac_module()),
                KvIdentityGraph::in_memory("test_store"),
            ),
            (
                "a store whose replacement write fails",
                orphan_context(
                    &hmac_orphan,
                    Some("192.0.2.10"),
                    crate::ec::tests::hmac_module(),
                ),
                KvIdentityGraph::new(EmptyUnwritableEcKv),
            ),
            (
                "a replacement that always collides",
                orphan_context(
                    &hmac_orphan,
                    Some("192.0.2.10"),
                    crate::ec::tests::hmac_module(),
                ),
                KvIdentityGraph::new(crate::ec::tests::AddCollidingEcKv::new(u32::MAX)),
            ),
        ];
        for (case, mut ec_context, graph) in cases {
            let orphan = ec_context
                .ec_value()
                .expect("the context should carry the orphan")
                .to_owned();
            let orphan_kv_key = ec_context
                .ec_kv_key()
                .expect("the selected module should own the orphan");
            let mut response = empty_response();

            ec_finalize_response(
                &settings,
                &mut ec_context,
                Some(&graph),
                &PartnerRegistry::empty(),
                None,
                None,
                &mut response,
            );

            assert_eq!(
                ec_context.ec_value(),
                Some(orphan.as_str()),
                "{case}: the orphan must not rotate"
            );
            assert!(
                !ec_context.ec_generated(),
                "{case}: nothing should be marked as generated"
            );
            assert!(
                get_header(&response, "set-cookie").is_none(),
                "{case}: no replacement cookie should be written"
            );
            assert_eq!(
                ec_context.kv_snapshot(),
                &EcKvSnapshot::Failed {
                    ec_id: orphan_kv_key,
                },
                "{case}: the orphan's key should carry a failed snapshot"
            );
        }
    }

    #[test]
    fn an_orphan_rotation_applies_the_replacement_modules_response_headers() {
        // A returning visitor runs no generation earlier in the request, so the
        // headers the module asks for while creating the replacement reach
        // the response through the rotation alone.
        let settings = create_test_settings();
        let mut ec_context = orphan_context(
            "t0eh~orphaned-value",
            Some("192.0.2.10"),
            std::sync::Arc::new(EvidenceHeaderModule),
        );
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        assert_eq!(
            ec_context.ec_value(),
            Some("t0eh~evidence-id"),
            "the orphan should rotate to the module's replacement"
        );
        let cookies: Vec<&str> = response
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().expect("should render set-cookie as utf-8"))
            .collect();
        assert!(
            cookies
                .iter()
                .any(|cookie| cookie.starts_with("vendor-ev=abc")),
            "the module's cookie should reach the response, got {cookies:?}"
        );
        assert!(
            cookies
                .iter()
                .any(|cookie| cookie.starts_with("ts-ec=t0eh~evidence-id")),
            "the replacement Edge Cookie should be written, got {cookies:?}"
        );
        assert!(
            response
                .headers()
                .get_all(http::header::VARY)
                .iter()
                .any(|value| value == "sec-ch-ua"),
            "the module's Vary should reach the response"
        );
    }

    #[test]
    fn finalize_does_not_rotate_when_the_existence_check_fails() {
        // Absence is unprovable when the list itself errors. Rotation abandons a
        // year-lived identity, so it must not run on an unproven miss.
        let settings = create_test_settings();
        let orphan = sample_ec_id("nolist");
        let graph = KvIdentityGraph::unprovable_absence("test_store");
        let mut ec_context = returning_user_context(
            &orphan,
            EcKvSnapshot::Missing {
                ec_id: orphan.clone(),
            },
            true,
        );
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        assert_did_not_rotate(&ec_context, &orphan, &response);
    }

    #[test]
    fn finalize_generated_ec_does_not_emit_cookie_for_authoritative_missing_row() {
        let settings = create_test_settings();
        let generated_ec = sample_ec_id("genmis");
        let mut ec_context = make_context(
            Some(&generated_ec),
            None,
            false,
            true,
            Jurisdiction::NonRegulated,
            true,
        );
        ec_context.set_kv_snapshot(EcKvSnapshot::Missing {
            ec_id: generated_ec,
        });
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        assert!(
            get_header(&response, "set-cookie").is_none(),
            "must not emit a cookie without an authoritative backing row"
        );
    }

    #[test]
    fn finalize_denied_without_cookie_is_noop() {
        let settings = create_test_settings();
        let mut ec_context = make_context(None, None, false, false, Jurisdiction::Unknown, false);
        let mut response = empty_response();

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &mut ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "should not set EC header"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "should not mutate cookie when there is nothing to revoke"
        );
    }

    #[test]
    fn finalize_not_permitted_without_withdrawal_keeps_cookie() {
        // When EC is not permitted (here a fail-closed unknown jurisdiction with
        // no geo) but the request carries no explicit withdrawal signal, the
        // response strips EC headers yet must leave an already-issued cookie
        // intact. A pre-consent or transient fail-closed request must not
        // permanently withdraw a returning user before they get to consent.
        let settings = create_test_settings();
        let ec_id = sample_ec_id("unk001");
        let mut ec_context = make_context(
            Some(&ec_id),
            Some(&ec_id),
            true,
            false,
            Jurisdiction::Unknown,
            false,
        );
        let mut response = empty_response();
        set_header(&mut response, "x-ts-ec", &ec_id);
        set_header(&mut response, "x-ts-eids", "[]");

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &mut ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "should strip EC header when EC is not permitted"
        );
        assert!(
            get_header(&response, "x-ts-eids").is_none(),
            "should strip EID header when EC is not permitted"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "a not-permitted request without a withdrawal signal should keep the cookie"
        );
    }

    #[test]
    fn set_ec_cookie_on_response_writes_the_ts_ec_cookie() {
        // The positive case: when an EC value is present, the finalize path
        // writes the ts-ec cookie to the browser, carrying the EC id.
        let settings = create_test_settings();
        let ec_id = sample_ec_id("setck1");
        let ec_context = make_context(
            Some(&ec_id),
            None,
            false,
            true,
            Jurisdiction::NonRegulated,
            true,
        );
        let mut response = empty_response();

        set_ec_cookie_on_response(&settings, &ec_context, &mut response);

        let set_cookie =
            get_header_str(&response, "set-cookie").expect("an EC value should write a Set-Cookie");
        assert!(
            set_cookie.contains("ts-ec=") && set_cookie.contains(&ec_id),
            "should write the ts-ec cookie carrying the EC id, got: {set_cookie}"
        );
    }

    #[test]
    fn the_permission_gate_decides_whether_a_generated_ec_cookie_is_written() {
        // The generated identifier has a backing row and a snapshot bound to
        // it, so the backing-row guard would let the cookie through and the
        // permission gate is the only thing that can withhold it.
        let settings = create_test_settings();
        for (case, gate_open) in [("an open gate", true), ("a closed gate", false)] {
            let ec_id = sample_ec_id("gated1");
            let graph = KvIdentityGraph::in_memory("test_store");
            graph
                .create(&ec_id, &live_entry())
                .expect("should seed the generated identifier's row");
            let mut ec_context = make_context(
                Some(&ec_id),
                None,
                false,
                true,
                Jurisdiction::NonRegulated,
                gate_open,
            );
            ec_context.set_kv_snapshot(EcKvSnapshot::Present {
                ec_id: ec_id.clone(),
                entry: Box::new(live_entry()),
                generation: None,
            });
            let mut response = empty_response();

            ec_finalize_response(
                &settings,
                &mut ec_context,
                Some(&graph),
                &PartnerRegistry::empty(),
                None,
                None,
                &mut response,
            );

            let wrote_ec_cookie = response
                .headers()
                .get_all(http::header::SET_COOKIE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .any(|cookie| cookie.starts_with("ts-ec=") && cookie.contains(&ec_id));
            assert_eq!(
                wrote_ec_cookie, gate_open,
                "{case}: the ts-ec cookie should be written only when the permission gate is open"
            );
        }
    }

    #[test]
    fn withdrawal_tombstones_the_canonical_row_not_the_cookie_value() {
        // The tombstone is the authoritative revocation marker, so it has to
        // land on the key the live row uses. Written under the raw cookie
        // value it creates a second row nothing reads, and the revocation
        // never takes effect for a module whose canonical form differs.
        //
        // Destructive withdrawal is narrow, so the trigger here is a TCF record
        // refusing storage, the same one
        // `finalize_withdrawal_clears_cookie_and_headers` uses. An opt-out such
        // as GPC suppresses use without destroying an issued identifier, so it
        // would write no tombstone for this test to place.
        let settings = create_test_settings();
        let graph = graph_with_live_canonical_row();
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::Gdpr,
            tcf: Some(refusing_tcf()),
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        // The TCF provider answers the withdrawal at assembly, and core links
        // no provider, so the answer is stated here.
        let mut ec_context = canonicalizing_context(true, false, consent, false)
            .with_storage_withdrawn_for_test(true);
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        let (live_row, _) = graph
            .get(CANONICAL_KV_KEY)
            .expect("should read the canonical row")
            .expect("the canonical row should still exist");
        assert!(
            !live_row.consent.ok,
            "withdrawal should tombstone the row the live identifier is keyed by"
        );
        assert!(
            graph
                .get(CANONICAL_COOKIE_VALUE)
                .expect("should read the graph")
                .is_none(),
            "withdrawal should not write a tombstone under the raw cookie value"
        );
    }

    #[test]
    fn eid_ingestion_keys_by_the_modules_canonical_form() {
        // An ingested EID must join the row the identifier already has. Keyed
        // by the raw cookie value the upsert finds no row and the partner ID
        // is dropped.
        let settings = create_test_settings();
        let graph = graph_with_live_canonical_row();
        let partners = vec![make_partner("sharedid.org")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context = canonicalizing_context(true, false, consent, true);
        ec_context.set_eid_sync_source(EidSyncSource::Navigation);
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            None,
            Some("shared-cookie-id"),
            &mut response,
        );

        let (row, _) = graph
            .get(CANONICAL_KV_KEY)
            .expect("should read the canonical row")
            .expect("the canonical row should still exist");
        assert_eq!(
            row.ids.get("sharedid.org").map(|id| id.uid.as_str()),
            Some("shared-cookie-id"),
            "the ingested EID should land on the row keyed by the canonical form"
        );
        assert!(
            graph
                .get(CANONICAL_COOKIE_VALUE)
                .expect("should read the graph")
                .is_none(),
            "EID ingestion should not create a row under the raw cookie value"
        );
    }

    /// A module that sets one cookie of its own and one `Vary` entry, the
    /// two response effects a module realistically asks for, so a test can
    /// watch both land on a response the origin already wrote headers to.
    #[derive(Debug)]
    struct EvidenceHeaderModule;

    impl crate::ec::module::EdgeCookieModule for EvidenceHeaderModule {
        fn id(&self) -> &'static str {
            "evidence-header"
        }

        fn code(&self) -> crate::ec::module::ModuleCode {
            crate::module_code!("t0eh")
        }

        fn generate(
            &self,
            _request_info: &dyn crate::evidence::RequestInfo,
            _input: &crate::ec::module::IdentityInput<'_>,
        ) -> Result<
            crate::ec::module::GeneratedEdgeCookie,
            error_stack::Report<crate::error::TrustedServerError>,
        > {
            Ok(crate::ec::module::GeneratedEdgeCookie {
                id: Some("evidence-id".to_owned()),
                response_headers: vec![
                    (
                        http::header::SET_COOKIE,
                        HeaderValue::from_static("vendor-ev=abc; Path=/"),
                    ),
                    (http::header::VARY, HeaderValue::from_static("sec-ch-ua")),
                ],
            })
        }

        fn accepts_id(&self, value: &str) -> bool {
            !value.is_empty()
        }

        fn normalize_id_for_kv(&self, value: &str) -> String {
            value.to_owned()
        }
    }

    #[test]
    fn module_response_headers_reach_the_response_without_dropping_the_origins() {
        // The response finalization runs on is the finished one, so it already
        // carries the publisher origin's own headers. A module effect must
        // add to those, never replace them: replacing `Set-Cookie` would drop
        // the publisher's session and sign-in cookies, and replacing `Vary`
        // would break the caching the origin asked for.
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("finalize-module-headers-store");
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context = make_context_with_consent(None, None, false, false, consent, true)
            .with_module_for_test(std::sync::Arc::new(EvidenceHeaderModule));
        ec_context
            .generate_if_needed(&settings, Some(&graph))
            .expect("should create the identifier through the module");

        // What the publisher's origin returned, before EC finalization runs.
        let mut response = empty_response();
        response.headers_mut().append(
            http::header::SET_COOKIE,
            HeaderValue::from_static("publisher_session=origin-value; Path=/; HttpOnly"),
        );
        response.headers_mut().append(
            http::header::VARY,
            HeaderValue::from_static("accept-encoding"),
        );

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        let cookies: Vec<&str> = response
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().expect("should render set-cookie as utf-8"))
            .collect();
        assert!(
            cookies
                .iter()
                .any(|cookie| cookie.starts_with("publisher_session=origin-value")),
            "the origin's own cookie must survive a module effect, got {cookies:?}"
        );
        assert!(
            cookies
                .iter()
                .any(|cookie| cookie.starts_with("vendor-ev=abc")),
            "the module's cookie must reach the response, got {cookies:?}"
        );
        assert!(
            cookies.iter().any(|cookie| cookie.starts_with("ts-ec=")),
            "core's own managed cookie must still be written, got {cookies:?}"
        );

        let vary: Vec<&str> = response
            .headers()
            .get_all(http::header::VARY)
            .iter()
            .map(|value| value.to_str().expect("should render vary as utf-8"))
            .collect();
        assert!(
            vary.contains(&"accept-encoding"),
            "the origin's Vary must survive a module effect, got {vary:?}"
        );
        assert!(
            vary.contains(&"sec-ch-ua"),
            "the module's Vary must reach the response, got {vary:?}"
        );
    }

    /// A module standing in for the one a deployment switched *to*, with a
    /// different registered code from the module that created the live row.
    #[derive(Debug)]
    struct SwitchedModule;

    impl crate::ec::module::EdgeCookieModule for SwitchedModule {
        fn id(&self) -> &'static str {
            "switched"
        }

        fn code(&self) -> crate::ec::module::ModuleCode {
            crate::module_code!("t0sw")
        }

        fn generate(
            &self,
            _request_info: &dyn crate::evidence::RequestInfo,
            _input: &crate::ec::module::IdentityInput<'_>,
        ) -> Result<
            crate::ec::module::GeneratedEdgeCookie,
            error_stack::Report<crate::error::TrustedServerError>,
        > {
            Ok(crate::ec::module::GeneratedEdgeCookie::default())
        }

        fn accepts_id(&self, value: &str) -> bool {
            !value.is_empty()
        }

        fn normalize_id_for_kv(&self, value: &str) -> String {
            value.to_ascii_lowercase()
        }
    }

    #[test]
    fn switching_module_leaves_the_previous_modules_row_beyond_withdrawal() {
        // A retired module's identifier is owned by nobody this deployment
        // reads, so a later withdrawal expires the browser cookie but cannot
        // tombstone the row, and the identifier is never adopted either.
        let settings = create_test_settings();
        let graph = graph_with_live_canonical_row();
        // A TCF record consenting to nothing, under GDPR, so the request
        // carries an explicit refusal of storage. That is the narrow,
        // destructive kind of withdrawal, the one that tombstones rather than
        // merely suppressing, which is the behavior under test.
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::Gdpr,
            tcf: Some(crate::consent::TcfConsent {
                version: 2,
                cmp_id: 0,
                cmp_version: 0,
                consent_screen: 0,
                consent_language: "EN".to_owned(),
                vendor_list_version: 0,
                tcf_policy_version: 2,
                created_ds: 0,
                last_updated_ds: 0,
                purpose_consents: vec![false; 24],
                purpose_legitimate_interests: vec![false; 24],
                vendor_consents: Vec::new(),
                vendor_legitimate_interests: Vec::new(),
                special_feature_opt_ins: vec![false; 12],
            }),
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        // The browser still carries the identifier the previous module
        // created, but the deployment now runs a module with a different
        // code, so read-back treats the cookie as absent and the active
        // identifier is empty.
        // The TCF provider answers the withdrawal at assembly, and core links
        // no provider, so the answer is stated here.
        let mut ec_context = make_context_with_consent(
            None,
            Some(CANONICAL_COOKIE_VALUE),
            false,
            false,
            consent,
            false,
        )
        .with_module_for_test(std::sync::Arc::new(SwitchedModule))
        .with_storage_withdrawn_for_test(true);
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        assert!(
            ec_context.ec_value().is_none(),
            "the retired module's identifier must never be adopted by the new one"
        );

        let (row, _) = graph
            .get(CANONICAL_KV_KEY)
            .expect("should read the previous module's row")
            .expect("the previous module's row should still exist");
        assert!(
            row.consent.ok,
            "withdrawal cannot reach a retired module's row without the module that owns the code"
        );

        let cookies: Vec<&str> = response
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().expect("should render set-cookie as utf-8"))
            .collect();
        assert!(
            cookies
                .iter()
                .any(|cookie| cookie.starts_with("ts-ec=") && cookie.contains("Max-Age=0")),
            "withdrawal should still expire the browser cookie after a switch, got {cookies:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Orphan-recovery gating and two-ID withdrawal
    // -----------------------------------------------------------------------

    fn granting_consent() -> ConsentContext {
        ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            source: ConsentSource::Cookie,
            ..Default::default()
        }
    }

    fn returning_user_context(
        orphan: &str,
        snapshot: EcKvSnapshot,
        recovery_eligible: bool,
    ) -> EcContext {
        let mut ec = EcContext::new_for_test_with_ip(
            Some(orphan.to_owned()),
            granting_consent(),
            Some("192.0.2.10".to_owned()),
        )
        .with_module_for_test(crate::ec::tests::hmac_module());
        ec.set_recovery_eligible(recovery_eligible);
        ec.set_kv_snapshot(snapshot);
        ec
    }

    fn assert_did_not_rotate(ec_context: &EcContext, orphan: &str, response: &Response<EdgeBody>) {
        assert_eq!(
            ec_context.ec_value(),
            Some(orphan),
            "must not rotate the active EC ID"
        );
        assert!(!ec_context.ec_generated(), "must not mark a rotated EC");
        assert!(
            get_header(response, "set-cookie").is_none(),
            "must not emit a replacement cookie"
        );
    }

    #[test]
    fn finalize_not_read_snapshot_does_not_rotate() {
        let settings = create_test_settings();
        let orphan = sample_ec_id("notrd1");
        let mut ec_context = returning_user_context(&orphan, EcKvSnapshot::NotRead, true);
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        assert_did_not_rotate(&ec_context, &orphan, &response);
    }

    #[test]
    fn finalize_failed_snapshot_does_not_rotate() {
        let settings = create_test_settings();
        let orphan = sample_ec_id("faild1");
        let mut ec_context = returning_user_context(
            &orphan,
            EcKvSnapshot::Failed {
                ec_id: orphan.clone(),
            },
            true,
        );
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        assert_did_not_rotate(&ec_context, &orphan, &response);
    }

    #[test]
    fn finalize_tombstone_snapshot_does_not_rotate() {
        let settings = create_test_settings();
        let orphan = sample_ec_id("tomb01");
        let tombstone = EcKvSnapshot::Present {
            ec_id: orphan.clone(),
            entry: Box::new(KvEntry::tombstone(current_timestamp())),
            generation: Some(1),
        };
        let mut ec_context = returning_user_context(&orphan, tombstone, true);
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        assert_did_not_rotate(&ec_context, &orphan, &response);
    }

    #[test]
    fn finalize_subresource_missing_row_does_not_rotate() {
        let settings = create_test_settings();
        let orphan = sample_ec_id("subrs1");
        // Missing row, but the request is not a recovery-eligible browser navigation.
        let mut ec_context = returning_user_context(
            &orphan,
            EcKvSnapshot::Missing {
                ec_id: orphan.clone(),
            },
            false,
        );
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        assert_did_not_rotate(&ec_context, &orphan, &response);
        assert!(
            graph.get(&orphan).expect("should read store").is_none(),
            "a non-eligible request must not create the missing root"
        );
    }

    #[test]
    fn finalize_withdrawal_tombstones_present_id_and_skips_missing_other() {
        let settings = create_test_settings();
        let active_ec = sample_ec_id("activ2");
        let cookie_ec = sample_ec_id("cook2e");
        // A TCF record refusing storage under the requires-signal floor is
        // the withdrawal trigger. The TCF provider answers that at assembly,
        // and core links no provider, so the answer is stated here.
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::Gdpr,
            tcf: Some(refusing_tcf()),
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context = make_context_with_consent(
            Some(&active_ec),
            Some(&cookie_ec),
            true,
            false,
            consent,
            false,
        )
        .with_storage_withdrawn_for_test(true);
        // Carry a snapshot only for the active ID; the other ID must be looked up
        // independently and never created if absent.
        let graph = KvIdentityGraph::in_memory("test_store");
        graph
            .create(&active_ec, &live_entry())
            .expect("should seed active row");
        ec_context.set_kv_snapshot(graph.load_snapshot(&active_ec));
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        let (active_stored, _) = graph
            .get(&active_ec)
            .expect("should read active row")
            .expect("active row should remain as a tombstone");
        assert!(
            !active_stored.consent.ok,
            "the present active ID should be tombstoned after strong confirmation"
        );
        assert!(
            graph.get(&cookie_ec).expect("should read store").is_none(),
            "a missing second ID must never be created by withdrawal"
        );
    }

    #[test]
    fn finalize_withdrawal_tombstones_both_present_ids_once() {
        let settings = create_test_settings();
        let active_ec = sample_ec_id("activ3");
        let cookie_ec = sample_ec_id("cook3e");
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context = make_context_with_consent(
            Some(&active_ec),
            Some(&cookie_ec),
            true,
            false,
            consent,
            false,
        )
        .with_storage_withdrawn_for_test(true);
        let graph = KvIdentityGraph::in_memory("test_store");
        graph
            .create(
                &active_ec,
                &KvEntry::minimal("active.example.com", "active-uid", 1_000),
            )
            .expect("should seed active row");
        graph
            .create(
                &cookie_ec,
                &KvEntry::minimal("cookie.example.com", "cookie-uid", 1_000),
            )
            .expect("should seed cookie row");
        ec_context.set_kv_snapshot(graph.load_snapshot(&active_ec));
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        let (active_tombstone, active_generation) = graph
            .get(&active_ec)
            .expect("should read active row")
            .expect("should retain active tombstone");
        let (cookie_tombstone, cookie_generation) = graph
            .get(&cookie_ec)
            .expect("should read cookie row")
            .expect("should retain cookie tombstone");
        assert!(
            !active_tombstone.consent.ok,
            "active row should be withdrawn"
        );
        assert!(
            active_tombstone.ids.is_empty(),
            "active IDs should be cleared"
        );
        assert!(
            !cookie_tombstone.consent.ok,
            "cookie row should be withdrawn"
        );
        assert!(
            cookie_tombstone.ids.is_empty(),
            "cookie IDs should be cleared"
        );

        let mut repeated_response = empty_response();
        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut repeated_response,
        );

        assert_eq!(
            graph
                .get(&active_ec)
                .expect("should read active row")
                .expect("should retain active tombstone")
                .1,
            active_generation,
            "repeated finalization should not rewrite active tombstone"
        );
        assert_eq!(
            graph
                .get(&cookie_ec)
                .expect("should read cookie row")
                .expect("should retain cookie tombstone")
                .1,
            cookie_generation,
            "repeated finalization should not rewrite cookie tombstone"
        );
    }

    #[test]
    fn finalize_withdrawal_keeps_cookie_deletion_on_kv_failure() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("failw1");
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context =
            make_context_with_consent(Some(&ec_id), Some(&ec_id), true, false, consent, false)
                .with_storage_withdrawn_for_test(true);
        ec_context.set_pull_sync_marker_for_test(
            crate::ec::pull_sync_marker::PullSyncMarkerState::Invalid,
        );
        let graph = KvIdentityGraph::failing("unavailable-store");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        let cookies = response
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>();
        assert_eq!(
            response.status(),
            200,
            "KV failure should not change response status"
        );
        assert!(
            cookies
                .iter()
                .any(|cookie| { cookie.starts_with("ts-ec=;") && cookie.contains("Max-Age=0") }),
            "KV failure should not prevent EC cookie deletion"
        );
        assert!(
            cookies.iter().any(|cookie| {
                cookie.starts_with("ts-ec-pull-complete=;") && cookie.contains("Max-Age=0")
            }),
            "KV failure should not prevent marker deletion"
        );
    }

    #[test]
    fn finalize_sets_marker_for_complete_pull_partner_snapshot() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("compl1");
        let mut partner = make_partner("ssp.example.com");
        partner.pull_sync_enabled = true;
        partner.pull_sync_url = Some("https://sync.example.com/pull".to_owned());
        partner.pull_sync_allowed_domains = vec!["sync.example.com".to_owned()];
        partner.ts_pull_token = Some(Redacted::new("pull-token".to_owned()));
        let registry = PartnerRegistry::from_config(&[partner]).expect("should build registry");
        let mut ec_context = make_context(
            Some(&ec_id),
            Some(&ec_id),
            true,
            false,
            Jurisdiction::NonRegulated,
            true,
        );
        let mut entry = live_entry();
        entry.ids.insert(
            "ssp.example.com".to_owned(),
            crate::ec::kv_types::KvPartnerId {
                uid: "partner-uid".to_owned(),
            },
        );
        ec_context.set_kv_snapshot(EcKvSnapshot::Present {
            ec_id,
            entry: Box::new(entry),
            generation: Some(1),
        });
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            None,
            &registry,
            None,
            None,
            &mut response,
        );

        let cookies = response
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>();
        assert!(
            cookies
                .iter()
                .any(|cookie| cookie.starts_with("ts-ec-pull-complete=v1.")),
            "complete snapshot should issue the marker"
        );
    }

    #[test]
    fn explicit_withdrawal_without_marker_or_ec_cookie_does_not_set_cookie() {
        let settings = create_test_settings();
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context = make_context_with_consent(None, None, false, false, consent, false);
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            None,
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        assert!(
            response.headers().get(http::header::SET_COOKIE).is_none(),
            "withdrawal without browser identity state should not add a cookie"
        );
    }

    #[test]
    fn explicit_withdrawal_expires_marker_without_ec_cookie() {
        let settings = create_test_settings();
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context = make_context_with_consent(None, None, false, false, consent, false)
            .with_storage_withdrawn_for_test(true);
        ec_context.set_pull_sync_marker_for_test(
            crate::ec::pull_sync_marker::PullSyncMarkerState::Invalid,
        );
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            None,
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        let cookies = response
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>();
        assert!(
            cookies.iter().any(|cookie| {
                cookie.starts_with("ts-ec-pull-complete=;") && cookie.contains("Max-Age=0")
            }),
            "withdrawal should expire the marker independently of EC cookie state"
        );
        assert!(
            cookies.iter().all(|cookie| !cookie.starts_with("ts-ec=;")),
            "missing EC cookie should not add an EC-cookie expiry"
        );
    }

    fn live_entry() -> KvEntry {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry
    }
}
