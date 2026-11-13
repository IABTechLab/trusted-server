//! EC response finalization.
//!
//! Centralizes post-routing EC behavior so all handlers get consistent cookie
//! and KV semantics. Every browser-driven KV write goes through the
//! navigation's single page-load write ([`KvIdentityGraph::commit_page_load`]),
//! so a request without a navigation permit never writes. The one exception is
//! a consent withdrawal, whose tombstone is written on any request.

use std::collections::BTreeSet;

use edgezero_core::body::Body as EdgeBody;
use http::Response;

use super::consent::{ec_consent_granted, ec_consent_withdrawn};
use crate::consent::{ConsentContext, allows_eid_persistence};
use crate::settings::Settings;

use super::EcContext;
use super::cookies::{expire_ec_cookie, set_ec_cookie};
use super::eid_types::UnixSeconds;
use super::generation::{generate_ec_id, is_valid_ec_id};
use super::kv::{KvIdentityGraph, PartnerIdUpdate};
use super::kv_types::KvEntry;
use super::page_load::{PageLoadWrite, RowChange};
use super::prebid_eids::collect_eid_updates;
use super::pull_sync_marker::{expire_marker, reconcile_marker};
use super::registry::PartnerRegistry;
use super::write_plan::{WritePlanInput, plan_write};
use super::{EcKvSnapshot, current_timestamp, log_id};

/// TS-managed response headers tied to EC identity output.
const EC_RESPONSE_HEADERS: &[&str] = &[
    "x-ts-ec",
    "x-ts-eids",
    "x-ts-ec-consent",
    "x-ts-eids-truncated",
];

/// Finalizes EC response behavior for all routes.
///
/// Applies withdrawal handling, browser EID ingestion, provider lookup
/// results, orphan recovery and cookie writes for new EC generation.
///
/// The identity row is written at most once, and only when the request
/// carries a navigation permit (see [`super::page_load`]) and the final
/// response is not a redirect. Without a permit a newly generated EC is not
/// issued.
///
/// Consent withdrawal is the one exception: on any request carrying the EC
/// cookie, the KV tombstone is written as the authoritative revocation marker
/// and the EC cookie is expired once every tombstone is in place (or the row
/// is proven absent).
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
    ec_context.validate_pull_sync_marker(settings, registry);
    let (permit, after_response) = ec_context
        .take_navigation_permit()
        .and_then(|permit| permit.confirm_with_after_response(response.status()))
        .unzip();
    let consent_allows_ec = ec_consent_granted(ec_context.consent());
    let consent_withdrawn = ec_consent_withdrawn(ec_context.consent());

    if !consent_allows_ec {
        // Expire the request-local marker independently of the EC cookie: a
        // withdrawal must stop any pending pull-sync disclosure window.
        if consent_withdrawn && ec_context.pull_sync_marker().was_present() {
            expire_marker(ec_context.pull_sync_marker_mut(), response);
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

    if let (Some(graph), Some(ec_id)) = (kv, ec_context.ec_value().map(str::to_owned)) {
        let collected = if ec_context.eid_sync_source().is_some() || ec_context.ec_generated() {
            collect_eid_updates(
                eids_cookie,
                sharedid_cookie,
                ec_context.client_eids(),
                registry,
            )
        } else {
            Vec::new()
        };
        let had_eid_updates = !collected.is_empty();
        let updates = gate_eid_updates_by_consent(collected, ec_context.consent());
        let commit = PageLoadCommit {
            settings,
            graph,
            registry,
            ec_id: &ec_id,
            updates: &updates,
            had_eid_updates,
        };
        let page_write_ok = if ec_context.ec_generated() {
            commit_generated(&commit, ec_context, permit, response)
        } else if ec_context.ec_was_present() {
            commit_returning(&commit, ec_context, permit, response)
        } else {
            false
        };
        // The follow-up write is allowed only once consent allows the EC and
        // the page-load write succeeded or had nothing to change.
        if page_write_ok && let Some(after_response) = after_response {
            ec_context.set_after_response_permit(after_response);
        }
    } else if ec_context.ec_generated() {
        // Do not emit a generated EC when there is no KV graph: that would
        // mint a browser cookie with no backing identity-graph row.
        log::info!("Skipping generated EC response write because KV graph is unavailable");
    }

    reconcile_pull_sync_marker(settings, registry, ec_context, response);
}

/// Shared inputs for the navigation's single write.
struct PageLoadCommit<'a> {
    settings: &'a Settings,
    graph: &'a KvIdentityGraph,
    registry: &'a PartnerRegistry,
    ec_id: &'a str,
    /// Browser ingest, already consent-gated.
    updates: &'a [PartnerIdUpdate],
    /// Whether the request carried EID data before consent gating.
    had_eid_updates: bool,
}

impl PageLoadCommit<'_> {
    /// Plans the row change for `ec_id` against `snapshot`.
    fn plan(
        &self,
        ec_context: &EcContext,
        ec_id: &str,
        snapshot: &EcKvSnapshot,
        new_entry: Option<&KvEntry>,
    ) -> Option<RowChange> {
        plan_write(&WritePlanInput {
            ec_id,
            snapshot,
            new_entry,
            browser_ingest: self.updates,
            lookups: ec_context.lookup_results(),
            registry: self.registry,
            now: UnixSeconds::new(current_timestamp()),
        })
    }
}

fn present_snapshot(ec_id: &str, entry: KvEntry) -> EcKvSnapshot {
    EcKvSnapshot::Present {
        ec_id: ec_id.to_owned(),
        entry: Box::new(entry),
        generation: None,
    }
}

/// Creates the row of a newly generated EC and issues its cookie on success.
///
/// Returns whether the row was created.
fn commit_generated(
    commit: &PageLoadCommit<'_>,
    ec_context: &mut EcContext,
    permit: Option<PageLoadWrite>,
    response: &mut Response<EdgeBody>,
) -> bool {
    let Some(new_entry) = ec_context.take_pending_entry() else {
        log::warn!("Skipping generated EC cookie because no pending row was built");
        return false;
    };
    let Some(permit) = permit else {
        log::info!("Skipping generated EC: not a page-load navigation");
        return false;
    };
    let Some(change) = commit.plan(
        ec_context,
        commit.ec_id,
        ec_context.kv_snapshot(),
        Some(&new_entry),
    ) else {
        log::warn!("Skipping generated EC: no row change was planned");
        return false;
    };
    match commit.graph.commit_page_load(permit, commit.ec_id, change) {
        Ok(committed) => {
            ec_context.set_kv_snapshot(present_snapshot(commit.ec_id, committed));
            set_ec_cookie_on_response(commit.settings, ec_context, response);
            true
        }
        Err(report) => {
            log::warn!(
                "Page-load write failed for new EC '{}': {}",
                log_id(commit.ec_id),
                report.current_context()
            );
            false
        }
    }
}

/// Applies a returning EC's page-load write, or recovers an orphaned EC.
///
/// Returns whether the row is live afterwards: written, recovered, or left
/// unchanged because there was nothing to write.
fn commit_returning(
    commit: &PageLoadCommit<'_>,
    ec_context: &mut EcContext,
    permit: Option<PageLoadWrite>,
    response: &mut Response<EdgeBody>,
) -> bool {
    let Some(permit) = permit else {
        return false;
    };
    // An unread snapshot is read only when this navigation has something to
    // write, or when consent gating emptied real EID data on a request that
    // may still need orphan recovery. A navigation with nothing to do costs
    // no KV operation.
    let has_changes = !commit.updates.is_empty() || !ec_context.lookup_results().is_empty();
    if matches!(ec_context.kv_snapshot(), EcKvSnapshot::NotRead)
        && (has_changes || (commit.had_eid_updates && ec_context.recovery_eligible()))
    {
        let snapshot = commit.graph.load_snapshot(commit.ec_id);
        ec_context.set_kv_snapshot(snapshot);
    }
    if matches!(ec_context.kv_snapshot(), EcKvSnapshot::Missing { .. }) {
        return ec_context.recovery_eligible()
            && confirm_then_recover_orphaned_ec(commit, ec_context, permit, response);
    }
    if matches!(ec_context.kv_snapshot(), EcKvSnapshot::Failed { .. }) {
        return false;
    }
    update_returning(commit, ec_context, permit)
}

/// Writes the planned update for a returning EC whose row snapshot is held.
///
/// Returns `false` when a planned write failed, or when there was nothing to
/// write because the row is withdrawn.
fn update_returning(
    commit: &PageLoadCommit<'_>,
    ec_context: &mut EcContext,
    permit: PageLoadWrite,
) -> bool {
    let Some(change) = commit.plan(ec_context, commit.ec_id, ec_context.kv_snapshot(), None) else {
        // Nothing to write. A withdrawn row is not live and must not unlock
        // the follow-up write.
        return ec_context
            .kv_snapshot()
            .entry_for(commit.ec_id)
            .is_none_or(|entry| entry.consent.ok);
    };
    match commit.graph.commit_page_load(permit, commit.ec_id, change) {
        Ok(committed) => {
            ec_context.set_kv_snapshot(present_snapshot(commit.ec_id, committed));
            true
        }
        Err(report) => {
            log::info!(
                "Page-load write skipped for '{}': {}",
                log_id(commit.ec_id),
                report.current_context()
            );
            false
        }
    }
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
/// written to the identity graph from the `ts-eids`/`sharedId` cookies.
fn gate_eid_updates_by_consent(
    updates: Vec<PartnerIdUpdate>,
    consent: &ConsentContext,
) -> Vec<PartnerIdUpdate> {
    if updates.is_empty() || allows_eid_persistence(consent) {
        return updates;
    }
    log::debug!(
        "EC KV: withholding {} EID updates, EID consent (TCF Purpose 1 + 4) missing",
        updates.len()
    );
    Vec::new()
}

/// Replaces a proven-orphaned EC with a new ID whose row is created by this
/// navigation's single write. One attempt: a collision leaves the orphan in
/// place for a later navigation.
fn recover_orphaned_ec(
    commit: &PageLoadCommit<'_>,
    ec_context: &mut EcContext,
    permit: PageLoadWrite,
    response: &mut Response<EdgeBody>,
) {
    let orphan_id = commit.ec_id.to_owned();
    let Some(client_ip) = ec_context.client_ip().map(str::to_owned) else {
        log::warn!("Orphan EC recovery skipped because client IP is unavailable");
        ec_context.set_kv_snapshot(EcKvSnapshot::Failed { ec_id: orphan_id });
        return;
    };
    let new_id = match generate_ec_id(commit.settings, &client_ip) {
        Ok(ec_id) => ec_id,
        Err(err) => {
            log::warn!("Orphan EC recovery ID generation failed: {err:?}");
            ec_context.set_kv_snapshot(EcKvSnapshot::Failed { ec_id: orphan_id });
            return;
        }
    };
    let mut new_entry = KvEntry::new(
        ec_context.consent(),
        ec_context.geo_info(),
        current_timestamp(),
        &commit.settings.publisher.domain,
    );
    new_entry.device = ec_context
        .device_signals()
        .map(super::device::DeviceSignals::to_kv_device);

    let missing = EcKvSnapshot::Missing {
        ec_id: new_id.clone(),
    };
    let Some(change) = commit.plan(ec_context, &new_id, &missing, Some(&new_entry)) else {
        log::warn!("Orphan EC recovery skipped: no row change was planned");
        return;
    };
    match commit.graph.commit_page_load(permit, &new_id, change) {
        Ok(committed) => {
            let snapshot = present_snapshot(&new_id, committed);
            ec_context.replace_with_generated(new_id, snapshot);
            set_ec_cookie_on_response(commit.settings, ec_context, response);
        }
        Err(report) => {
            log::warn!(
                "Orphan EC recovery write failed: {}",
                report.current_context()
            );
            ec_context.set_kv_snapshot(EcKvSnapshot::Failed { ec_id: orphan_id });
        }
    }
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
/// - a row that became visible after the origin round trip is adopted, and the
///   navigation's single write is planned against it; it is never rotated;
/// - a second miss is escalated to
///   [`key_exists_confirmed`](KvIdentityGraph::key_exists_confirmed), which
///   reads the primary data source. Only a proven-absent key rotates;
/// - a key the store still lists is left alone: the point reads were stale, so
///   the identity stays intact and recovery is retried on a later navigation;
/// - neither a read failure nor a failed existence check is a miss, and neither
///   rotates.
///
/// Returns whether the navigation's write succeeded or had nothing to change,
/// which is what allows the after-response follow-up write.
fn confirm_then_recover_orphaned_ec(
    commit: &PageLoadCommit<'_>,
    ec_context: &mut EcContext,
    permit: PageLoadWrite,
    response: &mut Response<EdgeBody>,
) -> bool {
    let confirmed = commit.graph.load_snapshot(commit.ec_id);
    match confirmed {
        EcKvSnapshot::Present { .. } => {
            // The row became visible after the origin round trip: adopt it and
            // plan the navigation's write against it rather than rotating a
            // valid identity.
            ec_context.set_kv_snapshot(confirmed);
            update_returning(commit, ec_context, permit)
        }
        EcKvSnapshot::Missing { .. } => match commit.graph.key_exists_confirmed(commit.ec_id) {
            Ok(false) => {
                recover_orphaned_ec(commit, ec_context, permit, response);
                matches!(ec_context.kv_snapshot(), EcKvSnapshot::Present { .. })
            }
            Ok(true) => {
                log::warn!(
                    "Orphan EC recovery skipped: both point reads missed a row the store still \
                     lists; leaving the identity intact for a later navigation"
                );
                false
            }
            Err(err) => {
                log::warn!("Orphan EC recovery skipped: existence check failed: {err:?}");
                false
            }
        },
        // A failed or not-read confirmation is not an authoritative miss: leave
        // the existing snapshot in place and do not rotate an unconfirmed miss.
        EcKvSnapshot::Failed { .. } | EcKvSnapshot::NotRead => false,
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
/// or undecodable consent input: EC response headers always come off. Only an
/// explicit withdrawal with an EC cookie tombstones the row, so a visitor who
/// has simply not decided yet is not stripped of an identity they already
/// hold.
///
/// The tombstone is a privacy write outside the one-write-per-page-load rule:
/// it is written on any finalized request (`/auction`, a redirect, a request
/// without Fetch Metadata), so a withdrawal never waits for a later
/// navigation. The EC cookie is expired once every row is tombstoned or
/// proven absent; a failed tombstone keeps the cookie so the next request
/// retries.
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
    let Some(graph) = kv else {
        // No identity graph holds a row to tombstone, so the cookie is the
        // only identity state left to clear.
        expire_ec_cookie(settings, response);
        return;
    };

    let mut complete = true;
    for ec_id in withdrawal_ec_ids(ec_context) {
        // Only the active ID has a snapshot in the context to start from and
        // to correct, so post-send work sees the tombstone just written.
        let initial = if ec_context.kv_snapshot().belongs_to(&ec_id) {
            ec_context.kv_snapshot().clone()
        } else {
            EcKvSnapshot::NotRead
        };
        let outcome = graph.tombstone_existing_from_snapshot(&ec_id, initial);
        if matches!(outcome, EcKvSnapshot::Failed { .. }) {
            log::warn!(
                "EC withdrawal tombstone for '{}' failed; keeping the cookie so the next \
                 request retries",
                log_id(&ec_id)
            );
            complete = false;
        }
        if ec_context.ec_value() == Some(ec_id.as_str()) {
            ec_context.set_kv_snapshot(outcome);
        }
    }
    if complete {
        expire_ec_cookie(settings, response);
    }
}

/// The EC IDs a withdrawal applies to: the cookie's and the active one.
fn withdrawal_ec_ids(ec_context: &EcContext) -> BTreeSet<String> {
    [ec_context.existing_cookie_ec_id(), ec_context.ec_value()]
        .into_iter()
        .flatten()
        .filter(|ec_id| is_valid_ec_id(ec_id))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use base64::Engine as _;

    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};
    use crate::ec::EidSyncSource;
    use crate::ec::coordinator::results::{
        DropCounts, LookupResults, LookupStatus, OptOutMemory, ProviderLookup,
    };
    use crate::ec::eid_types::{AgentType, EidFields, Expiry, ProviderId, SourceDomain, UidValue};
    use crate::ec::kv_types::{KvEid, KvUid};
    use crate::ec::page_load::NavigationPermit;
    use crate::openrtb::{Eid, Uid};
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;
    use crate::test_support::tests::{create_test_settings, purpose_one_only_consent};

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
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        }
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
        ec_context.set_navigation_permit(NavigationPermit::for_test());
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
        ec_context.set_navigation_permit(NavigationPermit::for_test());
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
        assert!(
            ec_context.take_after_response_permit().is_none(),
            "should not allow after-response lookups after a withdrawal"
        );
    }

    #[test]
    fn withdrawal_keeps_the_cookie_when_the_tombstone_write_fails() {
        // A tombstone write that still fails after its bounded CAS retries
        // keeps the EC cookie, so the next request that carries it retries;
        // clearing it would orphan a live row (spec §9.2).
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
        ec_context.set_navigation_permit(NavigationPermit::for_test());
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

        assert!(
            get_header(&response, "set-cookie").is_none(),
            "should keep the EC cookie when the tombstone write fails"
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
        ec_context.set_navigation_permit(NavigationPermit::for_test());
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
        ec_context.set_navigation_permit(NavigationPermit::for_test());
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
        ec_context.set_eid_sync_source(EidSyncSource::Navigation);
        ec_context.set_client_eids(vec![Eid::new(
            "id5-sync.com".to_owned(),
            vec![Uid {
                id: "ID5_should_not_persist".to_owned(),
                atype: Some(1),
                ext: None,
            }],
        )]);
        let partners = vec![make_partner("id5-sync.com")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let graph = KvIdentityGraph::in_memory("test_store");
        ec_context.set_navigation_permit(NavigationPermit::for_test());
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
        ec_context.set_navigation_permit(NavigationPermit::for_test());
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
            .expect("should keep the row");
        assert!(
            !stored.ids.contains_key("sharedid.org"),
            "a subresource response must not persist request EID cookies"
        );
    }

    #[test]
    fn finalize_persists_returning_user_eid_updates_only_on_navigation() {
        // `/_ts/page-bids` belongs to a page load whose navigation already
        // made the write, so it never receives a permit (spec D1).
        for (source, suffix, cookie_id, permit) in [
            (
                EidSyncSource::Navigation,
                "naveid",
                "navigation-cookie-id",
                true,
            ),
            (
                EidSyncSource::PageBids,
                "spaeid",
                "page-bids-cookie-id",
                false,
            ),
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
            // A recovery-eligible context is a navigation and carries a permit.
            let mut ec_context =
                returning_user_context(&ec_id, graph.load_snapshot(&ec_id), permit);
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
                .expect("should keep the row");
            assert_eq!(
                stored.ids.get("sharedid.org").map(|id| id.uid.as_str()),
                permit.then_some(cookie_id),
                "{source} should persist the returning-user EID cookie only with a permit"
            );
        }
    }

    #[test]
    fn finalize_generated_ec_persists_eid_updates() {
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut ec_context = EcContext::new_for_test_with_ip(
            None,
            granting_consent(),
            Some("192.0.2.10".to_owned()),
        );
        ec_context
            .generate_if_needed(&settings, Some(&graph))
            .expect("should mint a new EC");
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        let ec_id = ec_context.ec_value().expect("should mint").to_owned();
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
            .expect("should create the row");
        assert_eq!(
            stored.ids.get("sharedid.org").map(|id| id.uid.as_str()),
            Some("generated-cookie-id"),
            "should ingest the browser EID in the create"
        );
    }

    #[test]
    fn finalize_auction_transient_miss_does_not_persist_eid_updates() {
        // `/auction` never writes KV (spec D1): browser EIDs are persisted only
        // by the page load's navigation write, whatever its snapshot says.
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
            .expect("should keep the row");
        assert!(
            stored.ids.is_empty(),
            "an auction request must leave the row unchanged"
        );
    }

    #[test]
    fn finalize_never_persists_auction_body_eids() {
        // `/auction` body EIDs go to bidders on that request only and are
        // never persisted (spec §5.1); neither is the `ts-eids` cookie on a
        // non-navigation request.
        let settings = create_test_settings();
        let ec_id = sample_ec_id("cleids1");
        let graph = KvIdentityGraph::in_memory("test_store");
        let live = KvEntry::new(
            &granting_consent(),
            None,
            current_timestamp(),
            &settings.publisher.domain,
        );
        graph
            .create(&ec_id, &live)
            .expect("should seed the live row this request updates");
        let mut ec_context = returning_user_context(
            &ec_id,
            EcKvSnapshot::Missing {
                ec_id: ec_id.clone(),
            },
            false,
        );
        ec_context.set_eid_sync_source(EidSyncSource::Auction);
        // Only `id5-sync.com` "fit" in the (simulated) trimmed cookie;
        // `liveramp.com` was dropped by the browser's size cap.
        let eids_cookie = base64::engine::general_purpose::STANDARD.encode(
            serde_json::to_vec(&serde_json::json!([
                {"source": "id5-sync.com", "uids": [{"id": "ID5_from_cookie", "atype": 1}]}
            ]))
            .expect("should serialize test cookie payload"),
        );
        ec_context.set_client_eids(vec![
            Eid::new(
                "id5-sync.com".to_owned(),
                vec![Uid {
                    id: "ID5_from_body".to_owned(),
                    atype: Some(1),
                    ext: None,
                }],
            ),
            Eid::new(
                "liveramp.com".to_owned(),
                vec![Uid {
                    id: "LR_from_body".to_owned(),
                    atype: Some(3),
                    ext: None,
                }],
            ),
        ]);
        let partners = vec![make_partner("id5-sync.com"), make_partner("liveramp.com")];
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
            .expect("should keep the row");
        assert!(
            stored.ids.is_empty(),
            "should persist neither the auction body nor the cookie EIDs"
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
        ec_context.set_eid_sync_source(EidSyncSource::Navigation);
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        ec_context.set_client_eids(vec![Eid::new(
            "id5-sync.com".to_owned(),
            vec![Uid {
                id: "ID5_should_not_persist".to_owned(),
                atype: Some(1),
                ext: None,
            }],
        )]);
        let partners = vec![make_partner("id5-sync.com")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
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

        let (stored, _) = graph
            .get(&ec_id)
            .expect("should read store")
            .expect("should keep the row");
        assert!(
            !stored.ids.contains_key("id5-sync.com"),
            "denying TCF Purpose 4 must keep the EID out of KV even though Purpose 1 \
             (EC) consent is granted"
        );
    }

    #[test]
    fn finalize_withholds_provider_owned_fallback_when_purpose_four_is_denied() {
        // Arrange
        let settings = create_test_settings();
        let ec_id = sample_ec_id("purp4o");
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
        ec_context.set_eid_sync_source(EidSyncSource::Navigation);
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        ec_context.set_client_eids(vec![Eid::new(
            "id5-sync.com".to_owned(),
            vec![Uid {
                id: "ID5_should_not_persist".to_owned(),
                atype: Some(1),
                ext: None,
            }],
        )]);
        let mut registry = PartnerRegistry::from_config(&[make_partner("id5-sync.com")])
            .expect("should build registry");
        assert!(
            registry.mark_provider_owned("id5-sync.com", None),
            "should mark the provider source"
        );
        let mut response = empty_response();

        // Act
        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            None,
            None,
            &mut response,
        );

        // Assert
        let (stored, _) = graph
            .get(&ec_id)
            .expect("should read store")
            .expect("should keep the row");
        assert!(
            !stored.ids.contains_key("id5-sync.com"),
            "should keep the provider-owned fallback out of KV without Purpose 4"
        );
    }

    #[test]
    fn finalize_skips_browser_uid_for_provider_owned_source() {
        // Arrange
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut ec_context = EcContext::new_for_test_with_ip(
            None,
            granting_consent(),
            Some("192.0.2.10".to_owned()),
        );
        ec_context
            .generate_if_needed(&settings, Some(&graph))
            .expect("should mint a new EC");
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        let ec_id = ec_context.ec_value().expect("should mint").to_owned();
        let mut registry = PartnerRegistry::from_config(&[make_partner("sharedid.org")])
            .expect("should build registry");
        assert!(
            registry.mark_provider_owned("sharedid.org", None),
            "should mark the provider source"
        );
        let mut response = empty_response();

        // Act
        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            None,
            Some("fallback-cookie-id"),
            &mut response,
        );

        // Assert
        let (stored, _) = graph
            .get(&ec_id)
            .expect("should read store")
            .expect("should create the row");
        assert!(
            !stored.ids.contains_key("sharedid.org"),
            "should not store a browser UID for a provider-owned source"
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
        ec_context.set_navigation_permit(NavigationPermit::for_test());
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
        if recovery_eligible {
            // Only a top-level navigation is recovery eligible, and only a
            // navigation carries the page-load write permit.
            ec.set_navigation_permit(NavigationPermit::for_test());
        }
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
        ec_context.set_navigation_permit(NavigationPermit::for_test());
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
    fn finalize_withdrawal_tombstones_the_active_and_cookie_ids_once() {
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
        ec_context.set_navigation_permit(NavigationPermit::for_test());
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
            "the cookie's row should be withdrawn too"
        );

        ec_context.set_navigation_permit(NavigationPermit::for_test());
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
            "repeated finalization should not rewrite the cookie tombstone"
        );
    }

    #[test]
    fn finalize_withdrawal_keeps_ec_cookie_but_expires_marker_on_kv_failure() {
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
        ec_context.set_pull_sync_marker_for_test(
            crate::ec::pull_sync_marker::PullSyncMarkerState::Invalid,
        );
        ec_context.set_navigation_permit(NavigationPermit::for_test());
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
            cookies.iter().all(|cookie| !cookie.starts_with("ts-ec=;")),
            "KV failure should keep the EC cookie so a later navigation can tombstone the row"
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
        );
        let mut entry = live_entry();
        entry.ids.insert(
            "ssp.example.com".to_owned(),
            crate::ec::kv_types::KvPartnerId::single("partner-uid".to_owned()),
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

    #[test]
    fn explicit_withdrawal_expires_marker_without_ec_cookie() {
        let settings = create_test_settings();
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context = make_context_with_consent(None, None, false, false, consent);
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

    fn registry_for(source: &str) -> PartnerRegistry {
        PartnerRegistry::from_config(&[make_partner(source)]).expect("should build registry")
    }

    fn eids_cookie(source: &str, uid: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(
            serde_json::json!([{ "source": source, "uids": [{ "id": uid, "atype": 1 }] }])
                .to_string(),
        )
    }

    #[test]
    fn auction_request_never_writes_browser_eids() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("auc001");
        let graph = KvIdentityGraph::in_memory("finalize");
        graph
            .create(&ec_id, &live_entry())
            .expect("should seed row");
        let mut ec_context = EcContext::new_for_test(Some(ec_id.clone()), granting_consent());
        ec_context.set_eid_sync_source(EidSyncSource::Auction);
        ec_context.set_kv_snapshot(graph.load_snapshot(&ec_id));
        let cookie = eids_cookie("partner.example.com", "u-1");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry_for("partner.example.com"),
            Some(&cookie),
            None,
            &mut response,
        );

        let (entry, _) = graph
            .get(&ec_id)
            .expect("should read")
            .expect("should exist");
        assert!(
            entry.ids.is_empty(),
            "should not write from a non-navigation request"
        );
    }

    #[test]
    fn navigation_writes_browser_eids_once() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("nav001");
        let graph = KvIdentityGraph::in_memory("finalize");
        graph
            .create(&ec_id, &live_entry())
            .expect("should seed row");
        let mut ec_context = EcContext::new_for_test(Some(ec_id.clone()), granting_consent());
        ec_context.set_eid_sync_source(EidSyncSource::Navigation);
        ec_context.set_kv_snapshot(graph.load_snapshot(&ec_id));
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        let cookie = eids_cookie("partner.example.com", "u-1");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry_for("partner.example.com"),
            Some(&cookie),
            None,
            &mut response,
        );

        let (entry, _) = graph
            .get(&ec_id)
            .expect("should read")
            .expect("should exist");
        assert_eq!(
            entry.ids["partner.example.com"].uid, "u-1",
            "should persist browser uid"
        );
        assert!(
            ec_context.take_navigation_permit().is_none(),
            "should spend the permit"
        );
    }

    #[test]
    fn redirect_navigation_does_not_write() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("red001");
        let graph = KvIdentityGraph::in_memory("finalize");
        graph
            .create(&ec_id, &live_entry())
            .expect("should seed row");
        let mut ec_context = EcContext::new_for_test(Some(ec_id.clone()), granting_consent());
        ec_context.set_eid_sync_source(EidSyncSource::Navigation);
        ec_context.set_kv_snapshot(graph.load_snapshot(&ec_id));
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        let cookie = eids_cookie("partner.example.com", "u-1");
        let mut response = empty_response();
        *response.status_mut() = http::StatusCode::FOUND;

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry_for("partner.example.com"),
            Some(&cookie),
            None,
            &mut response,
        );

        let (entry, _) = graph
            .get(&ec_id)
            .expect("should read")
            .expect("should exist");
        assert!(entry.ids.is_empty(), "should not write on a redirect");
    }

    #[test]
    fn generated_ec_without_permit_gets_no_cookie_or_row() {
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("finalize");
        let mut ec_context = EcContext::new_for_test_with_ip(
            None,
            granting_consent(),
            Some("192.0.2.10".to_owned()),
        );
        ec_context
            .generate_if_needed(&settings, Some(&graph))
            .expect("should mint");
        let ec_id = ec_context.ec_value().expect("should mint").to_owned();
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
            graph.get(&ec_id).expect("should read").is_none(),
            "should not create without a permit"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "should not issue the EC"
        );
    }

    #[test]
    fn generated_ec_with_permit_creates_row_and_sets_cookie() {
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("finalize");
        let mut ec_context = EcContext::new_for_test_with_ip(
            None,
            granting_consent(),
            Some("192.0.2.10".to_owned()),
        );
        ec_context
            .generate_if_needed(&settings, Some(&graph))
            .expect("should mint");
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        let ec_id = ec_context.ec_value().expect("should mint").to_owned();
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
            graph.get(&ec_id).expect("should read").is_some(),
            "should create the row"
        );
        assert!(
            get_header(&response, "set-cookie").is_some(),
            "should issue the EC"
        );
    }

    #[test]
    fn generated_ec_collision_is_not_retried_or_issued() {
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("finalize");
        let mut ec_context = EcContext::new_for_test_with_ip(
            None,
            granting_consent(),
            Some("192.0.2.10".to_owned()),
        );
        ec_context
            .generate_if_needed(&settings, Some(&graph))
            .expect("should mint");
        let ec_id = ec_context.ec_value().expect("should mint").to_owned();
        graph
            .create(&ec_id, &live_entry())
            .expect("should seed a colliding row");
        ec_context.set_navigation_permit(NavigationPermit::for_test());
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
            "should not issue an EC whose row was not created"
        );
    }

    /// A minted EC with a page-phase lookup that found one UID.
    fn minted_context_with_lookup(
        settings: &Settings,
        graph: &KvIdentityGraph,
    ) -> (EcContext, String) {
        let mut ec_context = EcContext::new_for_test_with_ip(
            None,
            granting_consent(),
            Some("192.0.2.10".to_owned()),
        );
        ec_context
            .generate_if_needed(settings, Some(graph))
            .expect("should mint");
        let ec_id = ec_context.ec_value().expect("should mint").to_owned();
        let uid = KvUid::new(
            UidValue::try_from("p-first-visit").expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            Some(ProviderId::new("example-provider")),
            Expiry::Never,
        );
        ec_context.set_lookup_results(LookupResults::new(
            &ec_id,
            vec![ProviderLookup {
                provider: ProviderId::new("example-provider"),
                status: LookupStatus::Found {
                    kept: 1,
                    dropped: DropCounts::default(),
                },
                entries: vec![(
                    SourceDomain::try_from("provider.example.com").expect("should build source"),
                    vec![KvEid::new(EidFields::default(), vec![uid])],
                )],
                cleared: Vec::new(),
                opt_out: OptOutMemory::Unchanged,
            }],
        ));
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        (ec_context, ec_id)
    }

    #[test]
    fn minted_ec_row_is_created_with_its_lookup_results() {
        // Arrange
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("finalize");
        let (mut ec_context, ec_id) = minted_context_with_lookup(&settings, &graph);
        let mut response = empty_response();

        // Act
        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry_for("provider.example.com"),
            None,
            None,
            &mut response,
        );

        // Assert
        let (entry, _) = graph
            .get(&ec_id)
            .expect("should read row")
            .expect("should create row");
        assert!(
            entry.ids.contains_key("provider.example.com"),
            "should store the first navigation's lookup result in the created row"
        );
        assert!(
            get_header(&response, "set-cookie").is_some(),
            "should issue the minted EC"
        );
    }

    #[test]
    fn minted_ec_collision_drops_its_lookup_results() {
        // Arrange
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("finalize");
        let (mut ec_context, ec_id) = minted_context_with_lookup(&settings, &graph);
        graph
            .create(&ec_id, &live_entry())
            .expect("should seed a colliding row");
        let mut response = empty_response();

        // Act
        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry_for("provider.example.com"),
            None,
            None,
            &mut response,
        );

        // Assert
        let (entry, _) = graph
            .get(&ec_id)
            .expect("should read row")
            .expect("should keep the colliding row");
        assert!(
            !entry.ids.contains_key("provider.example.com"),
            "should not write lookup results into another visitor's row"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "should not issue an EC whose row was not created"
        );
    }

    fn withdrawing_context(ec_id: &str) -> EcContext {
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        make_context_with_consent(Some(ec_id), Some(ec_id), true, false, consent)
    }

    fn assert_withdrawn(graph: &KvIdentityGraph, ec_id: &str, response: &Response<EdgeBody>) {
        let (entry, _) = graph
            .get(ec_id)
            .expect("should read")
            .expect("should keep the tombstone");
        assert!(!entry.consent.ok, "should tombstone the row");
        assert!(
            get_header_str(response, "set-cookie")
                .is_some_and(|cookie| cookie.contains("Max-Age=0")),
            "should expire the EC cookie"
        );
    }

    #[test]
    fn withdrawal_without_permit_tombstones_row_and_expires_cookie() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("wd0001");
        let graph = KvIdentityGraph::in_memory("finalize");
        graph
            .create(&ec_id, &live_entry())
            .expect("should seed row");
        let mut ec_context = withdrawing_context(&ec_id);
        let mut response = empty_response();
        set_header(&mut response, "x-ts-ec", "stale");

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        assert_withdrawn(&graph, &ec_id, &response);
        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "should still strip EC response headers"
        );
    }

    #[test]
    fn withdrawal_on_a_redirect_tombstones_row_and_expires_cookie() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("wd0002");
        let graph = KvIdentityGraph::in_memory("finalize");
        graph
            .create(&ec_id, &live_entry())
            .expect("should seed row");
        let mut ec_context = withdrawing_context(&ec_id);
        ec_context.set_kv_snapshot(graph.load_snapshot(&ec_id));
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        let mut response = empty_response();
        *response.status_mut() = http::StatusCode::FOUND;

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        assert_withdrawn(&graph, &ec_id, &response);
    }

    #[test]
    fn withdrawal_retries_after_a_concurrent_write() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("wd0003");
        let graph = KvIdentityGraph::in_memory("finalize");
        graph
            .create(&ec_id, &live_entry())
            .expect("should seed row");
        let mut ec_context = withdrawing_context(&ec_id);
        ec_context.set_kv_snapshot(graph.load_snapshot(&ec_id));
        graph
            .upsert_partner_id(&ec_id, "partner.example.com", "late-uid")
            .expect("should write concurrently after the snapshot");
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

        assert_withdrawn(&graph, &ec_id, &response);
    }

    fn live_entry() -> KvEntry {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry
    }

    #[test]
    fn finalize_withholds_after_response_lookups_when_the_confirmed_write_fails() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("after3");
        let graph = KvIdentityGraph::write_failing_with(
            &ec_id,
            &crate::ec::kv_types::KvEntry::minimal("p.example", "uid", 1),
        );
        // The preload missed a row the confirmation read then finds, and the
        // navigation's write against it fails.
        let mut ec_context = returning_user_context(
            &ec_id,
            EcKvSnapshot::Missing {
                ec_id: ec_id.clone(),
            },
            true,
        );
        ec_context.set_eid_sync_source(EidSyncSource::Navigation);
        let cookie = eids_cookie("partner.example.com", "u-1");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry_for("partner.example.com"),
            Some(&cookie),
            None,
            &mut response,
        );

        assert!(
            ec_context.take_after_response_permit().is_none(),
            "should not allow a follow-up write after the page-load write failed"
        );
    }

    #[test]
    fn finalize_allows_after_response_lookups_on_a_live_row() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("after1");
        let graph = KvIdentityGraph::in_memory("test_store");
        graph
            .create(
                &ec_id,
                &crate::ec::kv_types::KvEntry::minimal("p.example", "uid", 1),
            )
            .expect("should seed the identity");
        let mut ec_context = returning_user_context(&ec_id, graph.load_snapshot(&ec_id), true);
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
            ec_context.take_after_response_permit().is_some(),
            "should allow after-response lookups when the page write had nothing to change"
        );
    }

    #[test]
    fn finalize_withholds_after_response_lookups_when_the_row_read_failed() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("after2");
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut ec_context = returning_user_context(
            &ec_id,
            EcKvSnapshot::Failed {
                ec_id: ec_id.clone(),
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

        assert!(
            ec_context.take_after_response_permit().is_none(),
            "should not allow after-response lookups when the page phase failed"
        );
    }

    #[test]
    fn finalize_withholds_after_response_lookups_on_a_tombstoned_row() {
        // Arrange: a consented navigation reads a withdrawn row.
        let settings = create_test_settings();
        let ec_id = sample_ec_id("after4");
        let graph = KvIdentityGraph::in_memory("test_store");
        graph
            .create(&ec_id, &KvEntry::tombstone(1000))
            .expect("should seed the tombstone");
        let mut ec_context = returning_user_context(&ec_id, graph.load_snapshot(&ec_id), true);
        let mut response = empty_response();

        // Act
        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        // Assert
        assert!(
            ec_context.take_after_response_permit().is_none(),
            "should not allow after-response lookups for a withdrawn row"
        );
    }

    #[test]
    fn finalize_adopted_row_keeps_a_concurrent_provider_opt_out() {
        // Arrange: the preload missed the row; meanwhile another request
        // recorded provider P's opt-out on it, and P's lookup found a UID.
        let settings = create_test_settings();
        let ec_id = sample_ec_id("optout");
        let graph = KvIdentityGraph::in_memory("test_store");
        let provider = ProviderId::new("example-provider");
        let mut live = live_entry();
        live.opt_outs
            .insert(provider.clone(), current_timestamp() + 86_400);
        graph.create(&ec_id, &live).expect("should seed the row");
        let mut ec_context = returning_user_context(
            &ec_id,
            EcKvSnapshot::Missing {
                ec_id: ec_id.clone(),
            },
            true,
        );
        let uid = KvUid::new(
            UidValue::try_from("p-restored").expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            Some(provider.clone()),
            Expiry::Never,
        );
        ec_context.set_lookup_results(LookupResults::new(
            &ec_id,
            vec![ProviderLookup {
                provider,
                status: LookupStatus::Found {
                    kept: 1,
                    dropped: DropCounts::default(),
                },
                entries: vec![(
                    SourceDomain::try_from("provider.example.com").expect("should build source"),
                    vec![KvEid::new(EidFields::default(), vec![uid])],
                )],
                cleared: Vec::new(),
                opt_out: OptOutMemory::Unchanged,
            }],
        ));
        let mut response = empty_response();

        // Act
        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry_for("provider.example.com"),
            None,
            None,
            &mut response,
        );

        // Assert
        let (entry, _) = graph
            .get(&ec_id)
            .expect("should read row")
            .expect("should keep row");
        assert!(
            !entry.ids.contains_key("provider.example.com"),
            "should not restore the opted-out provider's UID"
        );
    }
}
