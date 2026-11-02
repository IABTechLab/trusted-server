//! EC response finalization.
//!
//! Centralizes post-routing EC behavior so all handlers get consistent cookie
//! and KV semantics.

use std::collections::HashSet;

use edgezero_core::body::Body as EdgeBody;
use http::Response;

use super::consent::{ec_consent_granted, ec_consent_withdrawn};
use crate::consent::{ConsentContext, allows_eid_persistence};
use crate::settings::Settings;

use super::EcContext;
use super::cookies::{expire_ec_cookie, expire_revoke_marker, set_ec_cookie, set_revoke_marker};
use super::generation::{generate_ec_id, is_valid_ec_id};
use super::identity::ids::SourceDomain;
use super::identity::mutation::{IdentityMutation, PartnerIdChange};
use super::kv::{
    CreateIfAbsentOutcome, EidCookieSyncOutcome, KvIdentityGraph, PartnerIdUpdate,
    apply_partner_id_updates,
};
use super::kv_types::{KvEntry, KvPartnerId};
use super::prebid_eids::collect_eid_updates;
use super::registry::PartnerRegistry;
use super::{EcKvSnapshot, EidSyncSource, checked_current_timestamp, current_timestamp, log_id};

/// TS-managed response headers tied to EC identity output.
const EC_RESPONSE_HEADERS: &[&str] = &[
    "x-ts-ec",
    "x-ts-eids",
    "x-ts-ec-consent",
    "x-ts-eids-truncated",
];

/// Finalizes EC response behavior for all routes.
///
/// Applies withdrawal handling, cookie reconciliation, orphan recovery and the
/// `ts-ec` cookie for a new EC.
///
/// A returning visitor's response writes no KV row. On the writer request the
/// browser ingest from `ts-eids` and sharedId joins the request's identity
/// mutation, which phase 4 flushes; every other request ingests nothing. A new
/// EC's ingest rode in its create ([`BrowserSeed`]).
///
/// On consent withdrawal, the browser response clears the EC cookie
/// immediately and the EC identity-graph KV tombstone is the authoritative
/// revocation marker. There is no separate consent KV store to clean up.
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
    // A minted EC whose navigation never reached its create gets it here,
    // before its cookie is set (spec 4.7).
    if let Err(err) = ec_context.complete_deferred_create(settings, kv) {
        log::warn!("Deferred EC create failed at finalize: {err:?}");
    }
    let consent_allows_ec = ec_consent_granted(ec_context.consent());
    // A global revoke captured from any client (spec 8) is a withdrawal:
    // the same tombstone, cookie expiry and after-send run, with whatever the
    // request staged dropped.
    let global_revoke = ec_context.global_revoke_requested();
    let consent_withdrawn = ec_consent_withdrawn(ec_context.consent()) || global_revoke;

    if !consent_allows_ec || global_revoke {
        if global_revoke {
            *ec_context.identity_mut().mutation_mut() = IdentityMutation::default();
            // No EC is minted until the visitor's consent changes (spec 8).
            set_revoke_marker(settings, response, ec_context.consent());
        }
        finalize_unusable_consent(
            settings,
            ec_context,
            kv,
            registry,
            consent_withdrawn,
            response,
        );
        return;
    }

    // Returning user: consent is granted and EC came from request.
    if ec_context.ec_was_present() && !ec_context.ec_generated() && consent_allows_ec {
        if let (Some(graph), Some(ec_id)) = (kv, ec_context.ec_value().map(str::to_owned)) {
            // Only the writer request ingests browser data (spec 5.3). Every
            // other request leaves `ts-eids` and sharedId for the writer.
            let writer = ec_context.identity().writer();
            let collected = if writer {
                collect_eid_updates(
                    eids_cookie,
                    sharedid_cookie,
                    None,
                    registry,
                    checked_current_timestamp(),
                )
            } else {
                Vec::new()
            };
            let had_eid_updates = !collected.is_empty();
            let updates = gate_eid_updates_by_consent(collected, ec_context.consent());
            // When consent gating (e.g. TCF Purpose 4 denial) is what emptied
            // `updates`, the request *did* have EID data, and orphan recovery
            // below still needs an actual `Missing` read to detect an
            // orphaned cookie. An unread `NotRead` snapshot proves nothing, so
            // consent gating must not also skip that one read on a
            // recovery-eligible request.
            if updates.is_empty()
                && had_eid_updates
                && ec_context.recovery_eligible()
                && matches!(ec_context.kv_snapshot(), EcKvSnapshot::NotRead)
            {
                let snapshot = graph.load_snapshot(&ec_id);
                ec_context.set_kv_snapshot(snapshot);
            }
            if matches!(ec_context.kv_snapshot(), EcKvSnapshot::Missing { .. })
                && ec_context.recovery_eligible()
            {
                confirm_then_recover_orphaned_ec(
                    settings, ec_context, graph, &ec_id, &updates, response,
                );
            } else {
                queue_browser_ingest(ec_context, &ec_id, &updates);
            }
        }

        // Ordinary returning-user page views no longer refresh the browser
        // cookie, emit the EC header, or update KV TTL.
        return;
    }

    // Newly generated EC in this request. Do not emit a generated EC when
    // there is no KV graph: that would mint a browser cookie with no backing
    // identity-graph row, producing a phantom ID on later requests.
    if ec_context.ec_generated() {
        let (Some(_), Some(ec_id)) = (kv, ec_context.ec_value().map(str::to_owned)) else {
            log::info!("Skipping generated EC response write because KV graph is unavailable");
            return;
        };

        // The create in `generate_if_needed` (or the integration proxy's)
        // already carried the browser ingest, so finalize writes nothing.
        if let Some(entry) = ec_context.kv_snapshot().entry_for(&ec_id) {
            let seeded = gate_eid_updates_by_consent(
                collect_eid_updates(
                    eids_cookie,
                    sharedid_cookie,
                    None,
                    registry,
                    checked_current_timestamp(),
                ),
                ec_context.consent(),
            );
            if !seeded.is_empty() && partner_id_updates_stored(entry, &seeded) {
                record_eid_sync_terminal(EidSyncSource::NewEc, EidCookieSyncOutcome::Written);
            }
            set_ec_cookie_on_response(settings, ec_context, response);
            if ec_context.has_revoke_marker() {
                expire_revoke_marker(settings, response);
            }
        } else {
            log::warn!("Skipping generated EC cookie because backing row is not authoritative");
        }
    }
}

/// The browser ingest of one request, ready to seed a new identity row.
///
/// Opaque on purpose: it holds UIDs, so it prints only a count. Build it with
/// [`browser_seed`], or use [`BrowserSeed::empty`] when there is nothing to seed.
#[derive(Clone, Default)]
pub struct BrowserSeed(Vec<PartnerIdUpdate>);

impl BrowserSeed {
    /// A seed that carries nothing.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// The updates to apply to the new row.
    pub(crate) fn updates(&self) -> &[PartnerIdUpdate] {
        &self.0
    }
}

impl core::fmt::Debug for BrowserSeed {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("BrowserSeed")
            .field("updates", &self.0.len())
            .finish()
    }
}

/// Collects the consent-gated browser ingest from `ts-eids` and sharedId.
///
/// One UID per registered source: the `/auction` body is not an ingest input.
/// `now` is the acquisition time stored on each record.
#[must_use]
pub fn browser_seed(
    _settings: &Settings,
    registry: &PartnerRegistry,
    consent: &ConsentContext,
    eids_cookie: Option<&str>,
    sharedid_cookie: Option<&str>,
    now: u64,
) -> BrowserSeed {
    BrowserSeed(gate_eid_updates_by_consent(
        collect_eid_updates(eids_cookie, sharedid_cookie, None, registry, Some(now)),
        consent,
    ))
}

/// Whether `entry` already holds every update's UID.
fn partner_id_updates_stored(entry: &KvEntry, updates: &[PartnerIdUpdate]) -> bool {
    updates.iter().all(|update| {
        entry
            .ids
            .get(&update.partner_id)
            .is_some_and(|stored| stored.first_uid() == Some(update.uid.as_str()))
    })
}

/// Adds the browser ingest to the writer's mutation, for phase 4 to flush.
///
/// Nothing is queued against a withdrawn, missing or failed row. A `NotRead`
/// snapshot still queues: phase 4 re-reads before it writes.
fn queue_browser_ingest(ec_context: &mut EcContext, ec_id: &str, updates: &[PartnerIdUpdate]) {
    if updates.is_empty() || !ec_context.identity().writer() {
        return;
    }
    let usable = match ec_context.kv_snapshot() {
        EcKvSnapshot::NotRead => true,
        snapshot => snapshot
            .entry_for(ec_id)
            .is_some_and(|entry| entry.consent.ok),
    };
    if !usable {
        return;
    }
    let mutation = ec_context.identity_mut().mutation_mut();
    for update in updates {
        let Ok(source) = update.partner_id.parse::<SourceDomain>() else {
            log::warn!("Browser ingest skipped a source that is not a valid domain");
            continue;
        };
        mutation.add_change(PartnerIdChange::FillMissing {
            source,
            record: KvPartnerId::browser(update.uid.as_str(), update.acquired_at),
        });
    }
}

/// Logs the one remaining `EID sync measurement` line: a new EC whose
/// create carried the browser ingest. Returning visitors log the
/// `identity request measurement` line instead.
fn record_eid_sync_terminal(source: EidSyncSource, outcome: EidCookieSyncOutcome) {
    log::info!(
        "EID sync measurement: source={source} outcome={outcome} attempted=1 already_matched=0 \
         written=1 conflict_duplicate=0 deferred=0",
    );
}

/// Withholds EID-derived KV updates when consent does not allow EID
/// persistence, on top of the Purpose 1 (EC) gate `ec_finalize_response`
/// already enforces before reaching this point.
///
/// `consent_allows_ec` only requires Purpose 1 (device storage), but EIDs
/// additionally require Purpose 4 (personalized ads) — the same rule
/// [`gate_eids_by_consent`](crate::consent::gate_eids_by_consent) applies to
/// the outbound `/auction` bid request, evaluated here through
/// [`allows_eid_persistence`]. Without this, a user who denies Purpose 4
/// would have their EIDs correctly stripped from the bid request but still
/// written to the identity graph from the `ts-eids`/`sharedId` cookies or the
/// `/auction` request body.
fn gate_eid_updates_by_consent(
    updates: Vec<super::kv::PartnerIdUpdate>,
    consent: &ConsentContext,
) -> Vec<super::kv::PartnerIdUpdate> {
    if updates.is_empty() || allows_eid_persistence(consent) {
        return updates;
    }
    log::debug!(
        "EC KV: withholding {} EID updates, EID consent (TCF Purpose 1 + 4) missing",
        updates.len()
    );
    Vec::new()
}

fn recover_orphaned_ec(
    settings: &Settings,
    ec_context: &mut EcContext,
    graph: &KvIdentityGraph,
    updates: &[super::kv::PartnerIdUpdate],
    response: &mut Response<EdgeBody>,
) {
    // Snapshot the orphaned ID once so every fail-closed exit binds the failed
    // snapshot to the same key.
    let Some(orphan_id) = ec_context.ec_value().map(str::to_owned) else {
        return;
    };
    let Some(client_ip) = ec_context.client_ip().map(str::to_owned) else {
        log::warn!("Orphan EC recovery skipped because client IP is unavailable");
        ec_context.set_kv_snapshot(EcKvSnapshot::Failed {
            ec_id: orphan_id.clone(),
        });
        return;
    };

    const MAX_RECOVERY_ATTEMPTS: usize = 5;
    for _attempt in 0..MAX_RECOVERY_ATTEMPTS {
        let ec_id = match generate_ec_id(settings, &client_ip) {
            Ok(ec_id) => ec_id,
            Err(err) => {
                log::warn!("Orphan EC recovery ID generation failed: {err:?}");
                ec_context.set_kv_snapshot(EcKvSnapshot::Failed {
                    ec_id: orphan_id.clone(),
                });
                return;
            }
        };
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

        match graph.create_if_absent(&ec_id, &entry) {
            Ok(CreateIfAbsentOutcome::Written) => {
                let snapshot = EcKvSnapshot::Present {
                    ec_id: ec_id.clone(),
                    entry: Box::new(entry),
                    generation: None,
                };
                ec_context.replace_with_generated(ec_id, snapshot);
                // The create carried this request's ingest: it is the page
                // load's one write, so phase 4 has nothing left to flush.
                *ec_context.identity_mut().mutation_mut() = IdentityMutation::default();
                set_ec_cookie_on_response(settings, ec_context, response);
                return;
            }
            Ok(CreateIfAbsentOutcome::AlreadyExists) => continue,
            Err(err) => {
                log::warn!("Orphan EC recovery failed: {err:?}");
                ec_context.set_kv_snapshot(EcKvSnapshot::Failed {
                    ec_id: orphan_id.clone(),
                });
                return;
            }
        }
    }

    log::warn!("Orphan EC recovery exhausted collision retries");
    ec_context.set_kv_snapshot(EcKvSnapshot::Failed {
        ec_id: orphan_id.clone(),
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
    ec_id: &str,
    updates: &[super::kv::PartnerIdUpdate],
    response: &mut Response<EdgeBody>,
) {
    let confirmed = graph.load_snapshot(ec_id);
    match confirmed {
        EcKvSnapshot::Present { .. } => {
            // The row became visible after the origin round trip: adopt it and
            // queue the ingest rather than rotating a valid identity.
            ec_context.set_kv_snapshot(confirmed);
            queue_browser_ingest(ec_context, ec_id, updates);
        }
        EcKvSnapshot::Missing { .. } => match graph.key_exists_confirmed(ec_id) {
            Ok(false) => recover_orphaned_ec(settings, ec_context, graph, updates, response),
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

    // Compute once for the authoritative identity-graph tombstones.
    let ids_to_withdraw = withdrawal_ec_ids(ec_context);

    // The identity-graph tombstone is the authoritative withdrawal marker
    // for subsequent EC behavior.
    if let Some(graph) = kv {
        apply_withdrawal_tombstones(&ids_to_withdraw, |ec_id| {
            // The browser cookie is already cleared, so a failed tombstone
            // leaves a live row that server-side consumers still read as
            // consented. Report every failure, including the non-active cookie
            // ID whose outcome is not retained on the request context.
            if !tombstone_withdrawn_id(ec_context, graph, ec_id) {
                log::warn!(
                    "EC withdrawal tombstone failed for '{}': the identity-graph row may \
                     still be live with consent granted",
                    log_id(ec_id)
                );
                ec_context.record_failed_withdrawal(ec_id);
            }
        });
    }
}

/// Runs the tombstone path once for `ec_id` and returns whether it succeeded.
///
/// The graph hands back the post-withdrawal snapshot rather than leaving the
/// caller to rebuild it, so post-send work that reads the context — pull sync
/// discloses the raw EC ID to partners — sees the tombstone that was just
/// written. Only the active ID has a snapshot in the context to correct.
fn tombstone_withdrawn_id(
    ec_context: &mut EcContext,
    graph: &KvIdentityGraph,
    ec_id: &str,
) -> bool {
    let initial = if ec_context.kv_snapshot().belongs_to(ec_id) {
        ec_context.kv_snapshot().clone()
    } else {
        EcKvSnapshot::NotRead
    };
    let outcome = graph.tombstone_existing_from_snapshot(ec_id, initial);
    let succeeded = !matches!(outcome, EcKvSnapshot::Failed { .. });
    if ec_context.ec_value() == Some(ec_id) {
        ec_context.set_kv_snapshot(outcome);
    }
    succeeded
}

/// Retries each withdrawal tombstone that failed before send, once, after send.
///
/// Each failed ID re-reads its row (a [`EcKvSnapshot::NotRead`] start) and runs
/// the same tombstone path as before send, with its own CAS attempts and
/// completion marker. This is the last attempt: a success clears the ID from
/// the context's failed list, and a second failure is logged and leaves it
/// there for the failure counters.
/// An ID that was never recorded as failed is not touched.
pub fn retry_failed_withdrawals(ec_context: &mut EcContext, graph: &KvIdentityGraph) {
    let failed = ec_context.failed_withdrawals().to_vec();
    for ec_id in &failed {
        if tombstone_withdrawn_id(ec_context, graph, ec_id) {
            ec_context.clear_failed_withdrawal(ec_id);
        } else {
            log::warn!(
                "EC withdrawal tombstone failed again after send for '{}': the identity-graph \
                 row may still be live with consent granted",
                log_id(ec_id)
            );
        }
    }
}

fn withdrawal_ec_ids(ec_context: &EcContext) -> HashSet<String> {
    let mut hashes = HashSet::new();

    if let Some(cookie_ec_id) = ec_context.existing_cookie_ec_id()
        && is_valid_ec_id(cookie_ec_id)
    {
        hashes.insert(cookie_ec_id.to_owned());
    }

    if let Some(active_ec_id) = ec_context.ec_value()
        && is_valid_ec_id(active_ec_id)
    {
        hashes.insert(active_ec_id.to_owned());
    }

    hashes
}

fn apply_withdrawal_tombstones<F>(ec_ids: &HashSet<String>, mut write_tombstone: F)
where
    F: FnMut(&str),
{
    for ec_id in ec_ids {
        write_tombstone(ec_id);
    }
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use base64::Engine as _;

    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};
    use crate::ec::identity::request::WriterDecision;
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;
    use crate::test_support::tests::{create_test_settings, purpose_one_only_consent};

    #[test]
    fn finalize_creates_a_deferred_ec_its_navigation_did_not() {
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("ec-store");
        let mut context =
            EcContext::new_for_test_with_ip(None, granting_consent(), Some("192.0.2.5".to_owned()));
        context
            .mint_deferred(&settings, &BrowserSeed::empty())
            .expect("should mint");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        let ec_id = context.ec_value().expect("should keep the EC").to_owned();
        assert!(
            graph.get(&ec_id).expect("should read").is_some(),
            "should create the row"
        );
        assert!(
            response
                .headers()
                .get_all(http::header::SET_COOKIE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .any(|cookie| cookie.starts_with(&format!("ts-ec={ec_id};"))),
            "should set the EC cookie once its row exists"
        );
    }

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
        )
    }

    fn make_context_with_consent(
        ec_value: Option<&str>,
        cookie_ec_value: Option<&str>,
        ec_was_present: bool,
        ec_generated: bool,
        consent: ConsentContext,
    ) -> EcContext {
        EcContext::new_for_test_with_cookie(
            ec_value.map(str::to_owned),
            cookie_ec_value.map(str::to_owned),
            ec_was_present,
            ec_generated,
            consent,
        )
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
            removed_pull_sync_ttl_sec: None,
            removed_pull_sync_rate_limit: None,
            owner: None,
            ts_pull_token: None,
        }
    }

    #[test]
    fn withdrawal_ec_ids_returns_cookie_ec_only_when_active_missing() {
        let cookie_ec = sample_ec_id("cook1e");
        let ec_context = make_context(None, Some(&cookie_ec), true, false, Jurisdiction::Unknown);

        let ids = withdrawal_ec_ids(&ec_context);

        assert_eq!(ids.len(), 1, "should include exactly one EC ID");
        assert!(
            ids.contains(&cookie_ec),
            "should include the cookie EC value"
        );
    }

    #[test]
    fn withdrawal_ec_ids_deduplicates_matching_cookie_and_active_ec() {
        let ec_id = sample_ec_id("same01");
        let ec_context = make_context(
            Some(&ec_id),
            Some(&ec_id),
            true,
            false,
            Jurisdiction::Unknown,
        );

        let ids = withdrawal_ec_ids(&ec_context);

        assert_eq!(ids.len(), 1, "should deduplicate identical EC IDs");
        assert!(ids.contains(&ec_id), "should retain the shared EC ID");
    }

    #[test]
    fn withdrawal_ec_ids_includes_both_cookie_and_active_when_different() {
        let active_ec = sample_ec_id("activ1");
        let cookie_ec = sample_ec_id("cook1e");
        let ec_context = make_context(
            Some(&active_ec),
            Some(&cookie_ec),
            true,
            false,
            Jurisdiction::Unknown,
        );

        let ids = withdrawal_ec_ids(&ec_context);

        assert_eq!(ids.len(), 2, "should include both distinct EC IDs");
        assert!(ids.contains(&active_ec), "should include active EC ID");
        assert!(ids.contains(&cookie_ec), "should include cookie EC ID");
    }

    #[test]
    fn withdrawal_ec_ids_filters_invalid_values() {
        let valid_ec = sample_ec_id("valid1");
        let ec_context = make_context(
            Some(&valid_ec),
            Some("not-an-ec-id"),
            true,
            false,
            Jurisdiction::Unknown,
        );

        let ids = withdrawal_ec_ids(&ec_context);

        assert_eq!(ids.len(), 1, "should ignore malformed EC values");
        assert!(ids.contains(&valid_ec), "should keep the valid EC ID");
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
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context =
            make_context_with_consent(Some(&ec_id), Some(&ec_id), true, false, consent);
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
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context =
            make_context_with_consent(Some(&ec_id), Some(&ec_id), true, false, consent);
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
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context =
            make_context_with_consent(Some(&ec_id), Some(&ec_id), true, false, consent);
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
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context =
            make_context_with_consent(Some(&ec_id), Some(&ec_id), true, false, consent);
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
        );
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
    fn finalize_recovers_orphaned_ec_when_purpose_four_denial_empties_updates() {
        // Regression test: this request has real EID data (a configured
        // partner and a captured client EID), but TCF Purpose 4 denial gates
        // it away, emptying the update list. That must not also suppress the
        // snapshot refresh that orphan recovery depends on. Without a
        // preloaded snapshot (a non-GET publisher navigation never calls
        // `should_preload_ec_snapshot`), the context starts at `NotRead`; only
        // an actual KV read can prove the row is missing and let recovery run.
        //
        // This is distinct from `finalize_not_read_snapshot_does_not_rotate`,
        // which covers a request with no EID data at all (nothing gated it
        // away) and must still not rotate.
        let settings = create_test_settings();
        let orphaned_ec = sample_ec_id("orphn2");
        let purpose1_only_consent = purpose_one_only_consent();
        let mut ec_context = EcContext::new_for_test_with_ip(
            Some(orphaned_ec.clone()),
            purpose1_only_consent,
            Some("192.0.2.11".to_owned()),
        );
        ec_context.set_recovery_eligible(true);
        set_writer(&mut ec_context, true);
        let partners = vec![make_partner("id5-sync.com")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let graph = KvIdentityGraph::in_memory("test_store");
        let eids_cookie = base64::engine::general_purpose::STANDARD.encode(
            serde_json::to_vec(&serde_json::json!([
                { "source": "id5-sync.com", "uids": [{ "id": "ID5_should_not_persist", "atype": 1 }] }
            ]))
            .expect("should serialize test cookie payload"),
        );
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            Some(&eids_cookie),
            None,
            &mut response,
        );

        let replacement = ec_context.ec_value().expect(
            "should rotate the orphan even though Purpose 4 denial emptied the gated update list",
        );
        assert_ne!(replacement, &orphaned_ec);
        let (stored, _) = graph
            .get(replacement)
            .expect("should read replacement")
            .expect("replacement cookie should have a backing row");
        assert!(
            get_header(&response, "set-cookie").is_some(),
            "should emit replacement cookie after persistence"
        );
        assert!(
            !stored.ids.contains_key("id5-sync.com"),
            "denied Purpose 4 EID must not be persisted even on the recovered row"
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
    fn finalize_navigation_routes_never_write_returning_user_eid_updates_directly() {
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
            set_writer(&mut ec_context, true);
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
            assert!(
                !stored.ids.contains_key("sharedid.org"),
                "{source} should leave the flush to phase 4"
            );
            assert_eq!(
                ec_context.identity().mutation().change_count(),
                1,
                "{source} should queue the returning-user EID cookie"
            );
        }
    }

    #[test]
    fn generate_if_needed_seeds_the_ts_eids_cookie() {
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut ec_context = EcContext::new_for_test_with_ip(
            None,
            granting_consent(),
            Some("192.0.2.10".to_owned()),
        );
        let registry = PartnerRegistry::from_config(&[make_partner("ids.example.com")])
            .expect("should build registry");
        let eids_cookie = base64::engine::general_purpose::STANDARD.encode(
            serde_json::to_vec(&serde_json::json!([
                { "source": "ids.example.com", "uids": [{ "id": "cookie-uid" }] }
            ]))
            .expect("should serialize test cookie payload"),
        );
        let seed = browser_seed(
            &settings,
            &registry,
            ec_context.consent(),
            Some(&eids_cookie),
            None,
            current_timestamp(),
        );

        ec_context
            .generate_if_needed(&settings, Some(&graph), &seed)
            .expect("should generate an EC");

        let ec_id = ec_context.ec_value().expect("should hold the new EC");
        let (stored, _) = graph
            .get(ec_id)
            .expect("should read store")
            .expect("row should exist");
        assert_eq!(
            stored
                .ids
                .iter()
                .map(|(source, id)| (source.as_str(), id.first_uid().unwrap_or_default()))
                .collect::<Vec<_>>(),
            vec![("ids.example.com", "cookie-uid")],
            "a new EC should carry the ts-eids cookie in its create"
        );
    }

    #[test]
    fn finalize_generated_ec_writes_no_ingest_of_its_own() {
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
        let mut ec_context =
            make_context(Some(&ec_id), None, false, true, Jurisdiction::NonRegulated);
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
        assert!(
            !stored.ids.contains_key("sharedid.org"),
            "finalize should leave a new EC's ingest to the create"
        );
        assert!(
            get_header(&response, "set-cookie").is_some(),
            "should still set the ts-ec cookie"
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

        assert_eq!(
            sources.map(|source| source.to_string()),
            ["navigation", "auction", "page_bids", "new_ec"],
            "should label each source with a bounded name"
        );
        assert_eq!(
            EidCookieSyncOutcome::Written.to_string(),
            "written",
            "should label the one outcome the new-EC line reports"
        );
    }

    #[test]
    fn finalize_auction_stale_miss_neither_ingests_nor_writes() {
        // `/auction` is a stager: even with a stale endpoint miss in its
        // snapshot it does not re-read, ingest or write.
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
        assert!(
            !stored.ids.contains_key("sharedid.org"),
            "a stager must not write ingest"
        );
        assert!(
            ec_context.identity().mutation().is_empty(),
            "a stager must not queue ingest"
        );
    }

    #[test]
    fn finalize_withholds_eid_kv_writes_when_purpose_four_is_denied() {
        // A GDPR user can grant TCF Purpose 1 (storage/EC) while denying
        // Purpose 4 (personalized ads). `gate_eids_by_consent` already strips
        // EIDs from the outbound /auction bid request in that case; the KV
        // write path must apply the same Purpose 4 check, or a user's opt-out
        // is silently ignored for what gets persisted to the identity graph.
        let settings = create_test_settings();
        let ec_id = sample_ec_id("purp4x");
        let graph = KvIdentityGraph::in_memory("test_store");
        let purpose1_only_consent = purpose_one_only_consent();
        let live = KvEntry::new(
            &purpose1_only_consent,
            None,
            current_timestamp(),
            &settings.publisher.domain,
        );
        graph
            .create(&ec_id, &live)
            .expect("should seed the live row this request updates");
        let mut ec_context = EcContext::new_for_test_with_cookie(
            Some(ec_id.clone()),
            Some(ec_id.clone()),
            true,
            false,
            purpose1_only_consent,
        );
        ec_context.set_kv_snapshot(graph.load_snapshot(&ec_id));
        set_writer(&mut ec_context, true);
        let eids_cookie = base64::engine::general_purpose::STANDARD.encode(
            serde_json::to_vec(&serde_json::json!([
                { "source": "id5-sync.com", "uids": [{ "id": "ID5_should_not_persist", "atype": 1 }] }
            ]))
            .expect("should serialize test cookie payload"),
        );
        let partners = vec![make_partner("id5-sync.com")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            Some(&eids_cookie),
            None,
            &mut response,
        );

        let (stored, _) = graph
            .get(&ec_id)
            .expect("should read store")
            .expect("row should remain");
        assert!(
            !stored.ids.contains_key("id5-sync.com"),
            "denying TCF Purpose 4 must keep the EID out of KV even though Purpose 1 \
             (EC) consent is granted"
        );
        assert!(
            ec_context.identity().mutation().is_empty(),
            "denying TCF Purpose 4 must keep the EID out of the mutation"
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
        let mut ec_context = make_context(None, None, false, false, Jurisdiction::Unknown);
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
    fn finalize_unknown_jurisdiction_strips_headers_without_expiring_cookie() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("unk001");
        let mut ec_context = make_context(
            Some(&ec_id),
            Some(&ec_id),
            true,
            false,
            Jurisdiction::Unknown,
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
            "should strip EC header when consent cannot be verified"
        );
        assert!(
            get_header(&response, "x-ts-eids").is_none(),
            "should strip EID header when consent cannot be verified"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "should not expire the cookie without an explicit withdrawal signal"
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
        );
        ec.set_recovery_eligible(recovery_eligible);
        ec.set_kv_snapshot(snapshot);
        ec
    }

    fn set_writer(ec_context: &mut EcContext, writer: bool) {
        ec_context
            .identity_mut()
            .set_writer_decision(WriterDecision {
                writer,
                is_real_browser: writer,
                user_agent: None,
                dnt: false,
                page_url: None,
            });
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
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context =
            make_context_with_consent(Some(&active_ec), Some(&cookie_ec), true, false, consent);
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
        let mut ec_context =
            make_context_with_consent(Some(&active_ec), Some(&cookie_ec), true, false, consent);
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
            make_context_with_consent(Some(&ec_id), Some(&ec_id), true, false, consent);
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
    }

    #[test]
    fn explicit_withdrawal_without_ec_cookie_does_not_set_cookie() {
        let settings = create_test_settings();
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context = make_context_with_consent(None, None, false, false, consent);
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

    mod identity_flow {
        use serde_json::json;

        use super::*;
        use crate::ec::identity::catalog::ProviderCatalog;
        use crate::ec::identity::ids::SourceDomain;
        use crate::ec::identity::input::SeededRandom;
        use crate::ec::identity::mutation::{
            ApplyContext, IdentityMutation, PartnerIdChange, apply,
        };
        use crate::ec::kv_types::KvPartnerId;

        const SHARED_ID_SOURCE: &str = "sharedid.org";

        fn settings_with_partner(_source: &str) -> Settings {
            create_test_settings()
        }

        fn registry_with_partner(source: &str) -> PartnerRegistry {
            PartnerRegistry::from_config(&[make_partner(source)]).expect("should build registry")
        }

        fn ts_eids_cookie(value: &serde_json::Value) -> String {
            base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_vec(value).expect("should serialize test cookie payload"))
        }

        fn queued_records(mutation: &IdentityMutation) -> Vec<(SourceDomain, KvPartnerId)> {
            mutation
                .positive_changes()
                .iter()
                .filter_map(|change| match change {
                    PartnerIdChange::FillMissing { source, record }
                    | PartnerIdChange::OwnerWrite { source, record, .. } => {
                        Some((source.clone(), record.clone()))
                    }
                    PartnerIdChange::OwnerRemove { .. } | PartnerIdChange::WriterRemove { .. } => {
                        None
                    }
                })
                .collect()
        }

        fn set_cookie_names(response: &Response<EdgeBody>) -> Vec<String> {
            response
                .headers()
                .get_all(http::header::SET_COOKIE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .filter_map(|cookie| cookie.split('=').next())
                .map(str::to_owned)
                .collect()
        }

        fn html_response() -> Response<EdgeBody> {
            Response::builder()
                .status(200)
                .header(http::header::CONTENT_TYPE, "text/html; charset=utf-8")
                .body(EdgeBody::empty())
                .expect("should build test response")
        }

        fn seed_live_row(graph: &KvIdentityGraph, entry: &KvEntry) -> String {
            let ec_id = sample_ec_id("idflow");
            graph
                .create(&ec_id, entry)
                .expect("should seed the live row");
            ec_id
        }

        fn live_row() -> KvEntry {
            KvEntry::minimal("other.example.com", "x", 1)
        }

        fn returning_context(ec_id: &str, writer: bool) -> EcContext {
            let mut context = returning_user_context(ec_id, EcKvSnapshot::NotRead, false);
            set_writer(&mut context, writer);
            context
        }

        fn finalize_with_shared_id(
            context: &mut EcContext,
            graph: &KvIdentityGraph,
            shared_id: Option<&str>,
        ) -> Response<EdgeBody> {
            let mut response = html_response();
            ec_finalize_response(
                &settings_with_partner(SHARED_ID_SOURCE),
                context,
                Some(graph),
                &registry_with_partner(SHARED_ID_SOURCE),
                None,
                shared_id,
                &mut response,
            );
            response
        }

        #[test]
        fn returning_writer_navigation_adds_ingest_to_the_mutation_and_writes_nothing() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let ec_id = seed_live_row(&graph, &live_row());
            store.reset_counts();
            let mut context = returning_context(&ec_id, true);
            context.set_kv_snapshot(graph.load_snapshot(&ec_id));

            let response = finalize_with_shared_id(&mut context, &graph, Some("example-shared-id"));

            assert_eq!(
                store.writes().total(),
                0,
                "should not write in finalize for a returning writer"
            );
            let records = queued_records(context.identity().mutation());
            assert_eq!(records.len(), 1, "should queue the sharedId fill");
            assert_eq!(
                records[0].1.first_uid(),
                Some("example-shared-id"),
                "should queue the cookie's UID"
            );
            assert!(
                set_cookie_names(&response).is_empty(),
                "should set no cookie on a returning HTML response"
            );
        }

        #[test]
        fn writer_ingest_applied_to_the_live_row_matches_the_old_direct_write() {
            let (graph, _store) = KvIdentityGraph::write_counting("ec-store");
            let ec_id = seed_live_row(&graph, &live_row());
            let mut context = returning_context(&ec_id, true);
            context.set_kv_snapshot(graph.load_snapshot(&ec_id));
            let eids_cookie = ts_eids_cookie(&json!([
                { "source": SHARED_ID_SOURCE, "uids": [{ "id": "example-prebid" }] }
            ]));
            let mut response = html_response();
            let now = checked_current_timestamp().expect("should read the clock");

            ec_finalize_response(
                &settings_with_partner(SHARED_ID_SOURCE),
                &mut context,
                Some(&graph),
                &registry_with_partner(SHARED_ID_SOURCE),
                Some(&eids_cookie),
                Some("example-shared-id"),
                &mut response,
            );

            let live = live_row();
            let mut direct = live.clone();
            apply_partner_id_updates(
                &mut direct,
                &[PartnerIdUpdate::new(SHARED_ID_SOURCE, "example-shared-id")
                    .with_acquired_at(now)],
            );
            let catalog = ProviderCatalog::empty();
            let random = SeededRandom::new(7);
            let applied = apply(
                context.identity().mutation(),
                &live,
                &ApplyContext {
                    now,
                    random: &random,
                    catalog: &catalog,
                },
            )
            .expect("should apply to a live row");
            assert!(applied.changed, "should change the live row");
            assert_eq!(
                applied.entry.ids.keys().collect::<Vec<_>>(),
                direct.ids.keys().collect::<Vec<_>>(),
                "should store the same sources as the old direct write"
            );
            let staged = applied
                .entry
                .ids
                .get(SHARED_ID_SOURCE)
                .expect("should store the source");
            let written = direct
                .ids
                .get(SHARED_ID_SOURCE)
                .expect("should store the source");
            assert_eq!(
                staged.first_uid(),
                written.first_uid(),
                "should store the same UID"
            );
            assert_eq!(
                staged.at, written.at,
                "should store the same acquisition time"
            );
        }

        #[test]
        fn golden_browser_ingest_stays_one_uid_whatever_the_browser_sent() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let ec_id = seed_live_row(&graph, &live_row());
            store.reset_counts();
            let mut context = returning_context(&ec_id, true);
            context.set_kv_snapshot(graph.load_snapshot(&ec_id));
            let eids_cookie = ts_eids_cookie(&json!([
                {
                    "source": "partner.example.com",
                    "uids": [
                        { "id": "example-a", "atype": 1, "ext": { "stype": "ppuid" } },
                        { "id": "example-b", "atype": 3 }
                    ],
                    "inserter": "example.com",
                    "mm": 3
                },
                { "source": "unregistered.example.com", "uids": [{ "id": "example-c", "atype": 1 }] }
            ]));
            let mut response = html_response();

            ec_finalize_response(
                &settings_with_partner("partner.example.com"),
                &mut context,
                Some(&graph),
                &registry_with_partner("partner.example.com"),
                Some(&eids_cookie),
                None,
                &mut response,
            );

            assert_eq!(store.writes().total(), 0, "should not write in finalize");
            let records = queued_records(context.identity().mutation());
            assert_eq!(
                records.len(),
                1,
                "should queue only the registered source, as today"
            );
            let (source, record) = &records[0];
            assert_eq!(
                source.as_str(),
                "partner.example.com",
                "should queue the registered source"
            );
            assert_eq!(
                serde_json::to_value(record).expect("should serialize the record"),
                json!({ "at": record.at, "eids": [{ "uids": [{ "id": "example-a" }] }] }),
                "should store the UID collect_eid_updates picks today, with no atype, ext or EID-level field"
            );
        }

        #[test]
        fn stagers_ingest_nothing_and_write_nothing() {
            for source in [
                Some(EidSyncSource::Auction),
                Some(EidSyncSource::PageBids),
                None,
            ] {
                let (graph, store) = KvIdentityGraph::write_counting("ec-store");
                let ec_id = seed_live_row(&graph, &live_row());
                store.reset_counts();
                let mut context = returning_context(&ec_id, false);
                context.set_kv_snapshot(graph.load_snapshot(&ec_id));
                if let Some(source) = source {
                    context.set_eid_sync_source(source);
                }

                let response =
                    finalize_with_shared_id(&mut context, &graph, Some("example-shared-id"));

                assert_eq!(
                    store.writes().total(),
                    0,
                    "{source:?} should not write in finalize"
                );
                assert!(
                    context.identity().mutation().is_empty(),
                    "{source:?} should leave the mutation empty"
                );
                assert!(
                    set_cookie_names(&response).is_empty(),
                    "{source:?} should set no cookie"
                );
            }
        }

        #[test]
        fn unusable_consent_writes_nothing_and_queues_nothing() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let ec_id = seed_live_row(&graph, &live_row());
            store.reset_counts();
            let mut context = make_context(
                Some(&ec_id),
                Some(&ec_id),
                true,
                false,
                Jurisdiction::Unknown,
            );
            set_writer(&mut context, true);
            context.set_kv_snapshot(graph.load_snapshot(&ec_id));

            finalize_with_shared_id(&mut context, &graph, Some("example-shared-id"));

            assert_eq!(store.writes().total(), 0, "should not write");
            assert!(
                context.identity().mutation().is_empty(),
                "should queue nothing"
            );
        }

        #[test]
        fn withdrawal_still_writes_its_tombstone_on_any_request() {
            for writer in [true, false] {
                let (graph, store) = KvIdentityGraph::write_counting("ec-store");
                let ec_id = seed_live_row(&graph, &live_row());
                store.reset_counts();
                let consent = ConsentContext {
                    jurisdiction: Jurisdiction::UsState("CA".to_owned()),
                    gpc: true,
                    source: ConsentSource::Cookie,
                    ..Default::default()
                };
                let mut context =
                    make_context_with_consent(Some(&ec_id), Some(&ec_id), true, false, consent);
                set_writer(&mut context, writer);
                context.set_kv_snapshot(graph.load_snapshot(&ec_id));

                finalize_with_shared_id(&mut context, &graph, Some("example-shared-id"));

                assert!(
                    store.writes().total() > 0,
                    "writer={writer} should write the tombstone"
                );
                let (stored, _) = graph
                    .get(&ec_id)
                    .expect("should read store")
                    .expect("row should remain");
                assert!(
                    !stored.consent.ok,
                    "writer={writer} should tombstone the row"
                );
                assert!(
                    context.identity().mutation().is_empty(),
                    "writer={writer} should queue nothing"
                );
            }
        }

        #[test]
        fn tombstone_read_leaves_the_mutation_empty() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let ec_id = seed_live_row(&graph, &KvEntry::tombstone(1000));
            store.reset_counts();
            let mut context = returning_context(&ec_id, true);
            context.set_kv_snapshot(graph.load_snapshot(&ec_id));

            finalize_with_shared_id(&mut context, &graph, Some("example-shared-id"));

            assert_eq!(store.writes().total(), 0, "should not write");
            assert!(
                context.identity().mutation().is_empty(),
                "should leave the mutation empty"
            );
        }

        #[test]
        fn orphan_rotation_carries_browser_ingest_in_one_create() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let orphan = sample_ec_id("orphn2");
            let mut context = returning_context(&orphan, true);
            context.set_recovery_eligible(true);
            context.set_kv_snapshot(EcKvSnapshot::Missing {
                ec_id: orphan.clone(),
            });

            let response = finalize_with_shared_id(&mut context, &graph, Some("example-shared-id"));

            let replacement = context.ec_value().expect("should keep an EC ID").to_owned();
            assert_ne!(replacement, orphan, "should rotate the orphan");
            let writes = store.writes();
            assert_eq!(writes.add, 1, "should write one create");
            assert_eq!(writes.total(), 1, "should write nothing else");
            let (stored, _) = graph
                .get(&replacement)
                .expect("should read store")
                .expect("should hold the replacement row");
            assert_eq!(
                stored
                    .ids
                    .get(SHARED_ID_SOURCE)
                    .and_then(KvPartnerId::first_uid),
                Some("example-shared-id"),
                "should carry the sharedId as a one-UID record"
            );
            assert!(
                context.identity().mutation().is_empty(),
                "should leave the mutation empty"
            );
            assert_eq!(
                set_cookie_names(&response),
                vec!["ts-ec".to_owned()],
                "should set only the replacement cookie"
            );
        }

        #[test]
        fn confirm_read_that_finds_the_row_queues_ingest_instead_of_writing() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let ec_id = seed_live_row(&graph, &live_row());
            store.reset_counts();
            let mut context = returning_context(&ec_id, true);
            context.set_recovery_eligible(true);
            context.set_kv_snapshot(EcKvSnapshot::Missing {
                ec_id: ec_id.clone(),
            });

            let response = finalize_with_shared_id(&mut context, &graph, Some("example-shared-id"));

            assert_eq!(store.writes().total(), 0, "should not write");
            assert_eq!(
                context.ec_value(),
                Some(ec_id.as_str()),
                "should not rotate"
            );
            assert_eq!(
                queued_records(context.identity().mutation()).len(),
                1,
                "should queue the ingest"
            );
            assert!(
                context.kv_snapshot().entry_for(&ec_id).is_some(),
                "should adopt the confirmed row as the snapshot"
            );
            assert!(
                set_cookie_names(&response).is_empty(),
                "should set no cookie"
            );
        }

        #[test]
        fn integration_proxy_writer_never_rotates_on_missing_row() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let orphan = sample_ec_id("orphn3");
            let mut context = returning_context(&orphan, true);
            context.set_recovery_eligible(false);
            context.set_kv_snapshot(EcKvSnapshot::Missing {
                ec_id: orphan.clone(),
            });

            let response = finalize_with_shared_id(&mut context, &graph, Some("example-shared-id"));

            assert_eq!(store.writes().total(), 0, "should not create");
            assert_did_not_rotate(&context, &orphan, &response);
            assert!(
                context.identity().mutation().is_empty(),
                "should queue nothing against a missing row"
            );
        }

        #[test]
        fn new_visitor_create_carries_browser_ingest_in_one_write() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let settings = create_test_settings();
            let registry = registry_with_partner(SHARED_ID_SOURCE);
            let mut context = EcContext::new_for_test_with_ip(
                None,
                granting_consent(),
                Some("192.0.2.10".to_owned()),
            );
            let seed = browser_seed(
                &settings,
                &registry,
                context.consent(),
                None,
                Some("example-shared-id"),
                1_760_000_000,
            );

            context
                .generate_if_needed(&settings, Some(&graph), &seed)
                .expect("should generate an EC");
            let mut response = html_response();
            ec_finalize_response(
                &settings,
                &mut context,
                Some(&graph),
                &registry,
                None,
                Some("example-shared-id"),
                &mut response,
            );

            let writes = store.writes();
            assert_eq!(writes.add, 1, "should write one create");
            assert_eq!(writes.total(), 1, "should write nothing else");
            let ec_id = context.ec_value().expect("should hold the new EC");
            let (stored, _) = graph
                .get(ec_id)
                .expect("should read store")
                .expect("should hold the new row");
            assert_eq!(
                stored
                    .ids
                    .get(SHARED_ID_SOURCE)
                    .and_then(KvPartnerId::first_uid),
                Some("example-shared-id"),
                "should carry the sharedId as a one-UID record"
            );
            assert_eq!(
                set_cookie_names(&response),
                vec!["ts-ec".to_owned()],
                "should set the ts-ec cookie as today"
            );
        }
    }

    mod after_send_withdrawal_retry {
        use super::*;
        use crate::ec::kv::MAX_CAS_RETRIES;

        fn withdrawn_context(active: &str, cookie: &str) -> EcContext {
            let consent = ConsentContext {
                jurisdiction: Jurisdiction::UsState("CA".to_owned()),
                gpc: true,
                source: ConsentSource::Cookie,
                ..Default::default()
            };
            make_context_with_consent(Some(active), Some(cookie), true, false, consent)
        }

        fn seed(graph: &KvIdentityGraph, ec_id: &str) {
            graph
                .create(ec_id, &KvEntry::minimal("p.example.com", "uid", 1_000))
                .expect("should seed the row");
        }

        fn finalize(context: &mut EcContext, graph: &KvIdentityGraph) {
            let mut response = empty_response();
            ec_finalize_response(
                &create_test_settings(),
                context,
                Some(graph),
                &PartnerRegistry::empty(),
                None,
                None,
                &mut response,
            );
        }

        fn is_tombstone(graph: &KvIdentityGraph, ec_id: &str) -> bool {
            graph
                .get(ec_id)
                .expect("should read the row")
                .is_some_and(|(entry, _)| !entry.consent.ok)
        }

        #[test]
        fn failed_withdrawal_is_recorded_for_after_send() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let ec_id = sample_ec_id("fail01");
            seed(&graph, &ec_id);
            store.refuse_conditional_writes(&ec_id, usize::MAX);
            let mut context = withdrawn_context(&ec_id, &ec_id);
            context.set_kv_snapshot(graph.load_snapshot(&ec_id));

            finalize(&mut context, &graph);

            assert_eq!(
                context.failed_withdrawals(),
                std::slice::from_ref(&ec_id),
                "should record the active ID whose tombstone failed"
            );
            assert!(
                !is_tombstone(&graph, &ec_id),
                "the row should still be live"
            );
        }

        #[test]
        fn successful_withdrawal_records_nothing() {
            let (graph, _store) = KvIdentityGraph::write_counting("ec-store");
            let ec_id = sample_ec_id("good01");
            seed(&graph, &ec_id);
            let mut context = withdrawn_context(&ec_id, &ec_id);
            context.set_kv_snapshot(graph.load_snapshot(&ec_id));

            finalize(&mut context, &graph);

            assert!(
                context.failed_withdrawals().is_empty(),
                "should record nothing for a tombstone that was written"
            );
            assert!(is_tombstone(&graph, &ec_id), "should tombstone the row");
        }

        #[test]
        fn nothing_failed_means_the_retry_touches_nothing() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let ec_id = sample_ec_id("none01");
            seed(&graph, &ec_id);
            store.reset_counts();
            let mut context = withdrawn_context(&ec_id, &ec_id);

            retry_failed_withdrawals(&mut context, &graph);

            assert_eq!(
                store.writes().total(),
                0,
                "should write nothing of any mode"
            );
            assert_eq!(store.writes().lookups, 0, "should read nothing");
        }

        #[test]
        fn after_send_retry_writes_the_tombstone_once() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let ec_id = sample_ec_id("retry1");
            seed(&graph, &ec_id);
            store.refuse_conditional_writes(&ec_id, MAX_CAS_RETRIES as usize);
            let mut context = withdrawn_context(&ec_id, &ec_id);
            context.set_kv_snapshot(graph.load_snapshot(&ec_id));
            finalize(&mut context, &graph);
            assert_eq!(
                context.failed_withdrawals(),
                std::slice::from_ref(&ec_id),
                "should record the failure before send"
            );
            store.reset_counts();

            retry_failed_withdrawals(&mut context, &graph);

            assert!(is_tombstone(&graph, &ec_id), "should read as a tombstone");
            assert!(
                context.failed_withdrawals().is_empty(),
                "should clear an ID whose retry succeeded"
            );
            let writes = store.writes();
            assert_eq!(
                writes.if_generation_match, 1,
                "should write the tombstone once"
            );
            assert_eq!(writes.add, 1, "should write the completion marker once");
            assert_eq!(writes.overwrite, 0, "should not overwrite");
            assert!(
                context
                    .kv_snapshot()
                    .entry_for(&ec_id)
                    .is_some_and(|entry| !entry.consent.ok),
                "should update the active snapshot to the tombstone"
            );
        }

        #[test]
        fn after_send_retry_is_the_last_attempt() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let ec_id = sample_ec_id("last01");
            seed(&graph, &ec_id);
            store.refuse_conditional_writes(&ec_id, usize::MAX);
            let mut context = withdrawn_context(&ec_id, &ec_id);
            context.set_kv_snapshot(graph.load_snapshot(&ec_id));
            finalize(&mut context, &graph);
            store.reset_counts();

            retry_failed_withdrawals(&mut context, &graph);

            let writes = store.writes();
            assert_eq!(
                writes.if_generation_match, MAX_CAS_RETRIES as usize,
                "should make one run of the CAS attempts, never a second retry"
            );
            assert_eq!(writes.add, 0, "should write no completion marker");
            assert_eq!(writes.overwrite, 0, "should not overwrite");
            assert_eq!(
                context.failed_withdrawals(),
                std::slice::from_ref(&ec_id),
                "should leave the failure recorded for the counters"
            );
            assert!(
                !is_tombstone(&graph, &ec_id),
                "the row should still be live"
            );
        }

        #[test]
        fn only_the_id_that_fails_twice_remains_after_the_retry() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let active = sample_ec_id("activ5");
            let cookie = sample_ec_id("cook5e");
            seed(&graph, &active);
            seed(&graph, &cookie);
            store.refuse_conditional_writes(&cookie, MAX_CAS_RETRIES as usize);
            store.refuse_conditional_writes(&active, usize::MAX);
            let mut context = withdrawn_context(&active, &cookie);
            context.set_kv_snapshot(graph.load_snapshot(&active));
            finalize(&mut context, &graph);
            assert_eq!(
                context.failed_withdrawals().len(),
                2,
                "should record both IDs before send"
            );

            retry_failed_withdrawals(&mut context, &graph);

            assert_eq!(
                context.failed_withdrawals(),
                std::slice::from_ref(&active),
                "should keep only the ID that failed twice"
            );
            assert!(
                is_tombstone(&graph, &cookie),
                "should withdraw the cookie row"
            );
            assert!(
                !is_tombstone(&graph, &active),
                "the active row should be live"
            );
        }

        #[test]
        fn withdrawal_of_two_ids_retries_only_the_failed_one() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let active = sample_ec_id("activ4");
            let cookie = sample_ec_id("cook4e");
            seed(&graph, &active);
            seed(&graph, &cookie);
            store.refuse_conditional_writes(&cookie, MAX_CAS_RETRIES as usize);
            let mut context = withdrawn_context(&active, &cookie);
            context.set_kv_snapshot(graph.load_snapshot(&active));
            finalize(&mut context, &graph);
            assert_eq!(
                context.failed_withdrawals(),
                std::slice::from_ref(&cookie),
                "should record only the cookie ID"
            );
            assert!(
                is_tombstone(&graph, &active),
                "the active row should be withdrawn"
            );
            let active_generation = graph.get(&active).expect("should read").map(|row| row.1);
            store.reset_counts();

            retry_failed_withdrawals(&mut context, &graph);

            assert!(
                is_tombstone(&graph, &cookie),
                "should withdraw the cookie row"
            );
            assert!(
                context.failed_withdrawals().is_empty(),
                "should hold no ID once the only failed one succeeds"
            );
            assert_eq!(
                graph.get(&active).expect("should read").map(|row| row.1),
                active_generation,
                "should not rewrite the active row"
            );
            assert_eq!(
                store.writes().if_generation_match,
                1,
                "should write one tombstone, for the cookie ID only"
            );
        }
    }

    mod capture_revoke {
        use super::*;
        use crate::ec::identity::mutation::{IdentityMutation, PartnerIdChange};
        use crate::ec::identity::revision::Revision;
        use crate::ec::identity::test_support::{pid, source};

        fn finalize(context: &mut EcContext, graph: &KvIdentityGraph) -> Response<EdgeBody> {
            let mut response = empty_response();
            ec_finalize_response(
                &create_test_settings(),
                context,
                Some(graph),
                &PartnerRegistry::empty(),
                None,
                None,
                &mut response,
            );
            response
        }

        fn consenting_context(ec_id: &str) -> EcContext {
            let mut context = make_context_with_consent(
                Some(ec_id),
                Some(ec_id),
                true,
                false,
                granting_consent(),
            );
            context
                .identity_mut()
                .mutation_mut()
                .add_change(PartnerIdChange::OwnerRemove {
                    source: source("a.example.com"),
                    provider: pid("lockr"),
                    expected: Revision::Unset,
                });
            context
        }

        fn expires_the_ec_cookie(response: &Response<EdgeBody>) -> bool {
            response
                .headers()
                .get_all("set-cookie")
                .iter()
                .filter_map(|value| value.to_str().ok())
                .any(|cookie| cookie.starts_with("ts-ec=") && cookie.contains("Max-Age=0"))
        }

        #[test]
        fn capture_global_revoke_from_a_real_browser_tombstones_the_row_and_clears_the_ec_cookie() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let ec_id = sample_ec_id("revoke");
            graph
                .create(&ec_id, &KvEntry::minimal("p.example.com", "uid", 1_000))
                .expect("should seed the row");
            store.reset_counts();
            let mut context = consenting_context(&ec_id);
            context.set_kv_snapshot(graph.load_snapshot(&ec_id));
            context.request_global_revoke();

            let response = finalize(&mut context, &graph);

            let (stored, _) = graph
                .get(&ec_id)
                .expect("should read the row")
                .expect("should keep the tombstone");
            assert!(!stored.consent.ok, "should tombstone the row");
            assert!(stored.ids.is_empty(), "should remove every ID");
            assert_eq!(
                store.writes().if_generation_match,
                1,
                "should write one tombstone"
            );
            assert!(
                expires_the_ec_cookie(&response),
                "should expire the EC cookie"
            );
            assert!(
                context.failed_withdrawals().is_empty(),
                "should record no failed tombstone"
            );
            assert_eq!(
                context.identity().mutation(),
                &IdentityMutation::default(),
                "should drop the request's mutation"
            );

            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            graph
                .create(&ec_id, &KvEntry::minimal("p.example.com", "uid", 1_000))
                .expect("should seed the row");
            store.reset_counts();
            let mut control = consenting_context(&ec_id);
            control.set_kv_snapshot(graph.load_snapshot(&ec_id));
            let response = finalize(&mut control, &graph);
            assert!(
                graph
                    .get(&ec_id)
                    .expect("should read the row")
                    .is_some_and(|(row, _)| row.consent.ok),
                "control: without a revoke the row stays live"
            );
            assert!(
                !expires_the_ec_cookie(&response),
                "control: without a revoke the cookie stays"
            );
            assert_eq!(store.writes().total(), 0, "control: writes nothing");
        }

        fn revoke_marker_cookie(response: &Response<EdgeBody>) -> Option<String> {
            response
                .headers()
                .get_all(http::header::SET_COOKIE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .find(|cookie| cookie.starts_with("ts-ec-revoked="))
                .map(str::to_owned)
        }

        #[test]
        fn capture_global_revoke_sets_the_minting_marker() {
            let graph = KvIdentityGraph::in_memory("ec-store");
            let ec_id = sample_ec_id("marker");
            graph
                .create(&ec_id, &KvEntry::minimal("p.example.com", "uid", 1_000))
                .expect("should seed the row");
            let mut context = consenting_context(&ec_id);
            context.set_kv_snapshot(graph.load_snapshot(&ec_id));
            context.request_global_revoke();

            let response = finalize(&mut context, &graph);

            let marker = revoke_marker_cookie(&response).expect("should set the marker");
            assert!(
                marker.starts_with(&format!(
                    "ts-ec-revoked={};",
                    crate::ec::cookies::revoke_marker_value(context.consent())
                )),
                "should hold the fingerprint of the consent the revoke saw"
            );
            assert!(
                marker.contains("HttpOnly") && marker.contains("Secure"),
                "should be HttpOnly and Secure"
            );

            let mut control = consenting_context(&ec_id);
            control.set_kv_snapshot(graph.load_snapshot(&ec_id));
            assert!(
                revoke_marker_cookie(&finalize(&mut control, &graph)).is_none(),
                "control: without a revoke no marker is set"
            );
        }

        #[test]
        fn minting_under_changed_consent_expires_a_stale_marker() {
            let graph = KvIdentityGraph::in_memory("ec-store");
            let mut context = EcContext::new_for_test_with_ip(
                None,
                granting_consent(),
                Some("192.0.2.5".to_owned()),
            );
            context.set_revoke_marker_for_test("ffff");
            context
                .generate_if_needed(&create_test_settings(), Some(&graph), &BrowserSeed::empty())
                .expect("should mint");

            let response = finalize(&mut context, &graph);

            assert!(
                revoke_marker_cookie(&response).is_some_and(|cookie| cookie.contains("Max-Age=0")),
                "should expire the marker once a new EC is minted"
            );
        }

        #[test]
        fn capture_global_revoke_failed_tombstone_gets_the_after_send_run() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let ec_id = sample_ec_id("rvfail");
            graph
                .create(&ec_id, &KvEntry::minimal("p.example.com", "uid", 1_000))
                .expect("should seed the row");
            store.refuse_conditional_writes(&ec_id, 5);
            let mut context = consenting_context(&ec_id);
            context.set_kv_snapshot(graph.load_snapshot(&ec_id));
            context.request_global_revoke();

            let response = finalize(&mut context, &graph);

            assert_eq!(
                context.failed_withdrawals(),
                std::slice::from_ref(&ec_id),
                "should record the failed tombstone for the after-send run"
            );
            assert!(
                expires_the_ec_cookie(&response),
                "should still expire the cookie"
            );

            retry_failed_withdrawals(&mut context, &graph);

            assert!(
                graph
                    .get(&ec_id)
                    .expect("should read the row")
                    .is_some_and(|(row, _)| !row.consent.ok),
                "should tombstone the row after send"
            );
        }

        #[test]
        fn capture_global_revoke_without_an_ec_cookie_expires_nothing() {
            let (graph, store) = KvIdentityGraph::write_counting("ec-store");
            let ec_id = sample_ec_id("nocook");
            graph
                .create(&ec_id, &KvEntry::minimal("p.example.com", "uid", 1_000))
                .expect("should seed the row");
            store.reset_counts();
            let mut context =
                make_context_with_consent(Some(&ec_id), None, false, true, granting_consent());
            context.request_global_revoke();

            let response = finalize(&mut context, &graph);

            assert!(
                !expires_the_ec_cookie(&response),
                "should expire no cookie the request did not carry"
            );
            assert_eq!(store.writes().total(), 0, "should tombstone no row");
        }
    }

    fn live_entry() -> KvEntry {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry
    }
}
