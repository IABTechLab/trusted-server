//! EC response finalization.
//!
//! Centralizes post-routing EC behavior so all handlers get consistent cookie
//! and KV semantics. Every browser-driven KV write goes through the
//! navigation's single page-load write ([`KvIdentityGraph::commit_page_load`]),
//! so a request without a navigation permit never writes. The exceptions are a
//! consent withdrawal, whose tombstone is written on any request, and the
//! navigation's after-response follow-up write.

use std::collections::BTreeSet;

use edgezero_core::body::Body as EdgeBody;
use http::Response;

use super::consent::{ec_consent_granted, ec_consent_withdrawn};
use crate::consent::{ConsentContext, allows_eid_persistence};
use crate::constants::{COOKIE_TS_EC_PULL_COMPLETE, COOKIE_TS_EIDS_MIRROR};
use crate::settings::Settings;

use super::EcContext;
use super::cookies::{COOKIE_MAX_AGE, append_cookie, expire_ec_cookie, set_ec_cookie};
use super::eid_types::UnixSeconds;
use super::generation::{generate_ec_id, is_valid_ec_id};
use super::kv::{KvIdentityGraph, PartnerIdUpdate};
use super::kv_types::KvEntry;
use super::mirror::{MirrorCookie, build_mirror, mirror_consent, mirror_sources};
use super::page_load::{PageLoadWrite, RowChange};
use super::prebid_eids::collect_eid_updates;
use super::registry::{PartnerConfig, PartnerRegistry};
use super::sharedid::{
    SharedIdRequest, apply_sharedid_write_back, created_sharedid_update,
    drop_opted_out_sharedid_updates, expire_sharedid_on_denied_consent,
};
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
/// cookie, the browser response clears the EC cookie immediately and the KV
/// tombstone is the authoritative revocation marker.
///
/// `eids_cookie` should be the raw value of the `ts-eids` cookie extracted
/// from the request *before* routing consumes it. `sharedid` carries the
/// request's `SharedID` cookies; on a navigation the response writes them
/// back from the EC record under the `SharedID` consent rules.
pub async fn ec_finalize_response(
    settings: &Settings,
    ec_context: &mut EcContext,
    kv: Option<&KvIdentityGraph>,
    registry: &PartnerRegistry,
    eids_cookie: Option<&str>,
    sharedid: &SharedIdRequest,
    response: &mut Response<EdgeBody>,
) {
    let navigation = finalize_response_inner(
        settings,
        ec_context,
        kv,
        registry,
        eids_cookie,
        sharedid,
        response,
    )
    .await;
    apply_mirror(ec_context, registry, navigation, response);
    expire_retired_pull_marker(ec_context, response);
}

/// Sets or expires the mirror cookie (spec §10). Best effort.
///
/// Navigations rebuild it from the row as it stands after the page-load
/// write: the committed row is stored as the `Present` snapshot, so
/// [`super::EcKvSnapshot::entry_for`] returns it, or the read snapshot when no
/// write landed. Without EC and EID consent it is expired on any response
/// whose request carried it.
///
/// The cookie is only expired when the request carried it, and only set when
/// its value differs from the one the request carried, so responses that
/// leave the mirror as it was stay free of `Set-Cookie` and keep their
/// cacheability. An unchanged mirror is therefore not re-extended; once it
/// lapses, the next navigation sets it again. A UID without a stated expiry
/// is mirrored with an `exp` 30 days after the start of the current UTC day,
/// so a returning reader whose mirror holds such UIDs gets at most one
/// mirror `Set-Cookie` per UTC day.
///
/// Where the mirror cannot be rebuilt (non-navigations, or a navigation
/// without a readable row), a carried mirror is expired when the request
/// denies TCF vendor consent to a vendor gating any mirrored partner it holds,
/// when a provider excludes such a partner's source on this GDPR visit, or
/// when a provider's cookies on this request record an opt-out for it;
/// otherwise it is left untouched.
fn apply_mirror(
    ec_context: &EcContext,
    registry: &PartnerRegistry,
    navigation: bool,
    response: &mut Response<EdgeBody>,
) {
    let decision = if !mirror_consent(ec_context.consent()) {
        MirrorCookie::Expire
    } else if navigation && let Some(row) = mirror_row(ec_context) {
        build_mirror(row, ec_context, registry, mirror_now())
    } else if mirror_vendor_denied(ec_context, registry) {
        // The mirror cannot be rebuilt here, and it may hold UIDs of a
        // provider whose TCF vendor consent is now denied, whose source is
        // excluded on this GDPR visit or whose cookies now record an opt-out.
        MirrorCookie::Expire
    } else {
        if navigation {
            log::debug!("Leaving the mirror cookie untouched: identity row is unavailable");
        }
        return;
    };
    match decision {
        MirrorCookie::Set(value) if ec_context.mirror_cookie() == Some(value.as_str()) => {}
        MirrorCookie::Set(value) => append_cookie(
            response,
            &format!(
                "{COOKIE_TS_EIDS_MIRROR}={}; Path=/; Secure; SameSite=Lax; Max-Age={COOKIE_MAX_AGE}",
                value.as_str()
            ),
        ),
        MirrorCookie::Expire if ec_context.mirror_cookie_present() => {
            append_cookie(
                response,
                &format!("{COOKIE_TS_EIDS_MIRROR}=; Path=/; Secure; SameSite=Lax; Max-Age=0"),
            );
        }
        MirrorCookie::Expire => {}
    }
}

/// The time the mirror is built at: the clock, or the time a test pins.
fn mirror_now() -> UnixSeconds {
    #[cfg(test)]
    if let Some(now) = tests::MIRROR_NOW.get() {
        return now;
    }
    UnixSeconds::new(current_timestamp())
}

/// Whether the request carries a mirror that holds UIDs of a mirrored partner
/// with a denied gating TCF vendor (that of any configured provider claiming
/// its source, enabled or disabled), a source excluded where GDPR applies
/// for this visitor or a source a provider's cookies on this request opt out
/// of, so those UIDs must no longer reach the page.
///
/// A carried value that does not decode as a mirror may hold anything, so it
/// counts as holding denied UIDs whenever any mirrored partner is denied.
fn mirror_vendor_denied(ec_context: &EcContext, registry: &PartnerRegistry) -> bool {
    let Some(value) = ec_context.mirror_cookie() else {
        return false;
    };
    let denied = |partner: &PartnerConfig| {
        partner.bidstream_enabled
            && partner.mirror_enabled
            && (partner.owner_vendor_consent_denied(ec_context.consent())
                || partner.excluded_for(ec_context.consent())
                || ec_context.source_opted_out_in_cookies(&partner.source_domain))
    };
    if !registry.in_config_order().any(denied) {
        return false;
    }
    mirror_sources(value).is_none_or(|sources| {
        sources
            .iter()
            .any(|source| registry.find_by_source_domain(source).is_some_and(denied))
    })
}

/// The row the mirror is built from.
///
/// Returns `Some(None)` when there is authoritatively no row (no EC or a
/// missing row), and `None` when the row is unknown because the read failed
/// or never happened.
fn mirror_row(ec_context: &EcContext) -> Option<Option<&KvEntry>> {
    let Some(ec_id) = ec_context.ec_value() else {
        return Some(None);
    };
    let snapshot = ec_context.kv_snapshot();
    match snapshot {
        EcKvSnapshot::Present { .. } | EcKvSnapshot::Missing { .. }
            if snapshot.belongs_to(ec_id) =>
        {
            Some(snapshot.entry_for(ec_id))
        }
        _ => None,
    }
}

/// Expires a leftover pull-sync completeness marker; the marker is retired.
fn expire_retired_pull_marker(ec_context: &EcContext, response: &mut Response<EdgeBody>) {
    if ec_context.pull_marker_present() {
        append_cookie(
            response,
            &format!(
                "{COOKIE_TS_EC_PULL_COMPLETE}=; Path=/; Secure; SameSite=Lax; Max-Age=0; HttpOnly"
            ),
        );
    }
}

async fn finalize_response_inner(
    settings: &Settings,
    ec_context: &mut EcContext,
    kv: Option<&KvIdentityGraph>,
    registry: &PartnerRegistry,
    eids_cookie: Option<&str>,
    sharedid: &SharedIdRequest,
    response: &mut Response<EdgeBody>,
) -> bool {
    let (permit, after_response) = ec_context
        .take_navigation_permit()
        .and_then(|permit| permit.confirm_with_after_response(response.status()))
        .unzip();
    let navigation = permit.is_some();
    let consent_allows_ec = ec_consent_granted(ec_context.consent());
    let consent_withdrawn = ec_consent_withdrawn(ec_context.consent());

    if !consent_allows_ec {
        expire_sharedid_on_denied_consent(settings, sharedid, navigation, response);
        finalize_unusable_consent(
            settings,
            ec_context,
            kv,
            registry,
            consent_withdrawn,
            response,
        )
        .await;
        return navigation;
    }

    if let (Some(graph), Some(ec_id)) = (kv, ec_context.ec_value().map(str::to_owned)) {
        let collected = if ec_context.eid_sync_source().is_some() || ec_context.ec_generated() {
            drop_opted_out_sharedid_updates(
                collect_eid_updates(
                    eids_cookie,
                    sharedid.readable_value(),
                    ec_context.client_eids(),
                    registry,
                ),
                sharedid,
            )
        } else {
            Vec::new()
        };
        let had_eid_updates = !collected.is_empty();
        let mut collected = collected;
        // A reader with no SharedID anywhere gets one in this write.
        if navigation && let Some(created) = created_sharedid_update(sharedid, registry) {
            collected.push(created);
        }
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
            commit_generated(&commit, ec_context, permit, response).await
        } else if ec_context.ec_was_present() {
            commit_returning(&commit, ec_context, permit, response).await
        } else {
            false
        };
        // The follow-up write is allowed only once consent allows the EC and
        // the page-load write succeeded or had nothing to change.
        if page_write_ok && let Some(after_response) = after_response {
            ec_context.set_after_response_permit(after_response);
        }
        // Write-back reads the row as the page-load write left it.
        if navigation {
            apply_sharedid_write_back(settings, ec_context, graph, registry, sharedid, response)
                .await;
        }
    } else if ec_context.ec_generated() {
        // Do not emit a generated EC when there is no KV graph: that would
        // mint a browser cookie with no backing identity-graph row.
        log::info!("Skipping generated EC response write because KV graph is unavailable");
    }
    navigation
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
async fn commit_generated(
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
    let Some(RowChange::Create(entry)) = commit.plan(
        ec_context,
        commit.ec_id,
        ec_context.kv_snapshot(),
        Some(&new_entry),
    ) else {
        log::warn!("Skipping generated EC: no row change was planned");
        return false;
    };
    // A collision retries with a fresh ID inside the one page-load write.
    let outcome = commit
        .graph
        .commit_page_load_create(permit, (commit.ec_id.to_owned(), entry), || {
            fresh_create_candidate(commit, ec_context, &new_entry)
        })
        .await;
    match outcome {
        Ok((ec_id, committed)) => {
            let snapshot = present_snapshot(&ec_id, committed);
            ec_context.replace_with_generated(ec_id, snapshot);
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

/// Builds a create candidate for a fresh EC ID: the ID and the row planned
/// for it from `new_entry`.
///
/// Returns `None` when no ID can be generated or no create is planned.
fn fresh_create_candidate(
    commit: &PageLoadCommit<'_>,
    ec_context: &EcContext,
    new_entry: &KvEntry,
) -> Option<(String, KvEntry)> {
    let Some(client_ip) = ec_context.client_ip() else {
        log::warn!("Fresh EC ID skipped because client IP is unavailable");
        return None;
    };
    let ec_id = generate_ec_id(commit.settings, client_ip)
        .inspect_err(|err| log::warn!("Fresh EC ID generation failed: {err:?}"))
        .ok()?;
    let missing = EcKvSnapshot::Missing {
        ec_id: ec_id.clone(),
    };
    match commit.plan(ec_context, &ec_id, &missing, Some(new_entry)) {
        Some(RowChange::Create(entry)) => Some((ec_id, entry)),
        _ => {
            log::warn!("Fresh EC ID skipped: no row creation was planned");
            None
        }
    }
}

/// Applies a returning EC's page-load write, or recovers an orphaned EC.
///
/// Returns whether the row is live afterwards: written, recovered, or left
/// unchanged because there was nothing to write.
async fn commit_returning(
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
        let snapshot = commit.graph.load_snapshot(commit.ec_id).await;
        ec_context.set_kv_snapshot(snapshot);
    }
    if matches!(ec_context.kv_snapshot(), EcKvSnapshot::Missing { .. }) {
        return ec_context.recovery_eligible()
            && confirm_then_recover_orphaned_ec(commit, ec_context, permit, response).await;
    }
    if matches!(ec_context.kv_snapshot(), EcKvSnapshot::Failed { .. }) {
        return false;
    }
    update_returning(commit, ec_context, permit).await
}

/// Writes the planned update for a returning EC whose row snapshot is held.
///
/// Returns `false` when a planned write failed, or when there was nothing to
/// write because the row is withdrawn.
async fn update_returning(
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
    match commit
        .graph
        .commit_page_load(permit, commit.ec_id, change)
        .await
    {
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
/// navigation's single write. An ID collision retries with a fresh ID, up to
/// [`MAX_CREATE_ATTEMPTS`] attempts; when all fail the orphan stays in place
/// for a later navigation.
///
/// [`MAX_CREATE_ATTEMPTS`]: super::kv::MAX_CREATE_ATTEMPTS
async fn recover_orphaned_ec(
    commit: &PageLoadCommit<'_>,
    ec_context: &mut EcContext,
    permit: PageLoadWrite,
    response: &mut Response<EdgeBody>,
) {
    let orphan_id = commit.ec_id.to_owned();
    let mut new_entry = KvEntry::new(
        ec_context.consent(),
        ec_context.geo_info(),
        current_timestamp(),
        &commit.settings.publisher.domain,
    );
    new_entry.device = ec_context
        .device_signals()
        .map(super::device::DeviceSignals::to_kv_device);

    let Some(first) = fresh_create_candidate(commit, ec_context, &new_entry) else {
        log::warn!("Orphan EC recovery skipped: no replacement ID could be built");
        ec_context.set_kv_snapshot(EcKvSnapshot::Failed { ec_id: orphan_id });
        return;
    };
    let outcome = commit
        .graph
        .commit_page_load_create(permit, first, || {
            fresh_create_candidate(commit, ec_context, &new_entry)
        })
        .await;
    match outcome {
        Ok((new_id, committed)) => {
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
async fn confirm_then_recover_orphaned_ec(
    commit: &PageLoadCommit<'_>,
    ec_context: &mut EcContext,
    permit: PageLoadWrite,
    response: &mut Response<EdgeBody>,
) -> bool {
    let confirmed = commit.graph.load_snapshot(commit.ec_id).await;
    match confirmed {
        EcKvSnapshot::Present { .. } => {
            // The row became visible after the origin round trip: adopt it and
            // plan the navigation's write against it rather than rotating a
            // valid identity.
            ec_context.set_kv_snapshot(confirmed);
            update_returning(commit, ec_context, permit).await
        }
        EcKvSnapshot::Missing { .. } => match commit.graph.key_exists_confirmed(commit.ec_id).await
        {
            Ok(false) => {
                recover_orphaned_ec(commit, ec_context, permit, response).await;
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
/// navigation. The EC cookie is expired even when a tombstone fails: a failed
/// or unavailable tombstone is logged and marks a consent-update response
/// failed (503).
async fn finalize_unusable_consent(
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

    let ids_to_withdraw = withdrawal_ec_ids(ec_context);

    // The identity-graph tombstone is the authoritative withdrawal marker.
    // A consent-update acknowledgment must reflect failed/unavailable writes.
    let mut withdrawal_failed = !ids_to_withdraw.is_empty() && kv.is_none();
    if let Some(graph) = kv {
        for ec_id in &ids_to_withdraw {
            // The graph hands back the post-withdrawal snapshot, so post-send
            // work that reads the context sees the tombstone just written.
            // Only the active ID has a snapshot in the context to correct.
            let initial = if ec_context.kv_snapshot().belongs_to(ec_id) {
                ec_context.kv_snapshot().clone()
            } else {
                EcKvSnapshot::NotRead
            };
            let outcome = graph.tombstone_existing_from_snapshot(ec_id, initial).await;
            // The browser cookie is already cleared, so a failed tombstone
            // leaves a live row that server-side consumers still read as
            // consented. Report every failure, including the non-active cookie
            // ID whose outcome is not retained on the request context.
            if matches!(outcome, EcKvSnapshot::Failed { .. }) {
                withdrawal_failed = true;
                log::warn!(
                    "EC withdrawal tombstone failed for '{}': the identity-graph row may \
                     still be live with consent granted",
                    log_id(ec_id)
                );
            }
            if ec_context.ec_value() == Some(ec_id.as_str()) {
                ec_context.set_kv_snapshot(outcome);
            }
        }
    }
    if withdrawal_failed {
        super::consent_update::mark_reconciliation_failed(response);
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

    use std::cell::Cell;
    use std::num::NonZeroU16;
    use std::rc::Rc;
    use std::sync::Mutex;

    use error_stack::Report;

    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};
    use crate::ec::EidSyncSource;
    use crate::ec::coordinator::results::{
        DropCounts, LookupResults, LookupStatus, OptOutMemory, ProviderLookup,
    };
    use crate::ec::eid_types::{AgentType, EidFields, Expiry, ProviderId, SourceDomain, UidValue};
    use crate::ec::kv::MAX_CREATE_ATTEMPTS;
    use crate::ec::kv_backend::test_support::InMemoryEcKv;
    use crate::ec::kv_backend::{
        EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome,
    };
    use crate::ec::kv_types::KvPartnerId;
    use crate::ec::kv_types::{KvEid, KvUid};
    use crate::ec::page_load::NavigationPermit;
    use crate::ec::sharedid::SharedIdStorage;
    use crate::error::TrustedServerError;
    use crate::openrtb::{Eid, Uid};
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;
    use crate::test_support::tests::consent_granting_vendors;
    use crate::test_support::tests::{create_test_settings, purpose_one_only_consent};

    thread_local! {
        /// The time [`mirror_now`] returns while a [`PinnedMirrorClock`] lives.
        pub(super) static MIRROR_NOW: Cell<Option<UnixSeconds>> = const { Cell::new(None) };
    }

    /// Start of a UTC day, for tests that pin the mirror clock.
    const MIRROR_DAY_START: u64 = 1_767_225_600;

    /// Pins the mirror clock on this thread until dropped.
    struct PinnedMirrorClock;

    impl PinnedMirrorClock {
        fn at(now: u64) -> Self {
            MIRROR_NOW.set(Some(UnixSeconds::new(now)));
            Self
        }
    }

    impl Drop for PinnedMirrorClock {
        fn drop(&mut self) {
            MIRROR_NOW.set(None);
        }
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
            mirror_enabled: false,
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

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_withdrawal_does_not_create_a_row_for_an_unheld_identity() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert!(
            kv.get(&ec_id).await.expect("should read back").is_none(),
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

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_withdrawal_tombstones_a_held_identity() {
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
        .await
        .expect("should seed the identity");
        ec_context.set_kv_snapshot(kv.load_snapshot(&ec_id).await);
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        let (entry, _) = kv
            .get(&ec_id)
            .await
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

    #[tokio::test(flavor = "current_thread")]
    async fn withdrawal_still_expires_the_cookie_when_the_store_is_unavailable() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

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

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_withdrawal_clears_cookie_and_headers() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

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

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_returning_user_with_cookie_mismatch_sets_no_header_or_cookie() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "returning user should not set x-ts-ec"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "returning user should not refresh or repair cookie"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_returning_user_sets_no_header_or_cookie() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "returning user should not set x-ts-ec"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "returning user should not refresh cookie"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_generated_ec_without_kv_skips_cookie_and_header() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "generated EC without KV should not set response header"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "generated EC without KV should not set cookie"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_rotates_orphaned_cookie_to_new_backed_ec() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        let replacement = ec_context.ec_value().expect("should rotate orphan");
        assert_ne!(replacement, orphaned_ec);
        assert!(
            graph
                .get(replacement)
                .await
                .expect("should read replacement")
                .is_some(),
            "replacement cookie should have a backing row"
        );
        assert!(
            get_header(&response, "set-cookie").is_some(),
            "should emit replacement cookie after persistence"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_recovers_orphaned_ec_when_purpose_four_denial_empties_updates() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        let replacement = ec_context.ec_value().expect(
            "should rotate the orphan even though Purpose 4 denial emptied the gated update list",
        );
        assert_ne!(replacement, &orphaned_ec);
        let (stored, _) = graph
            .get(replacement)
            .await
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

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_returning_user_subresource_does_not_persist_eid_updates() {
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
            .await
            .expect("should seed the live row");
        let mut ec_context =
            returning_user_context(&ec_id, graph.load_snapshot(&ec_id).await, false);
        let partners = vec![make_partner("sharedid.org")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            None,
            &SharedIdRequest::with_value_for_test("shared-cookie-id"),
            &mut response,
        )
        .await;

        let (stored, _) = graph
            .get(&ec_id)
            .await
            .expect("should read store")
            .expect("should keep the row");
        assert!(
            !stored.ids.contains_key("sharedid.org"),
            "a subresource response must not persist request EID cookies"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_persists_returning_user_eid_updates_only_on_navigation() {
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
                .await
                .expect("should seed the live row");
            // A recovery-eligible context is a navigation and carries a permit.
            let mut ec_context =
                returning_user_context(&ec_id, graph.load_snapshot(&ec_id).await, permit);
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
                &SharedIdRequest::with_value_for_test(cookie_id),
                &mut response,
            )
            .await;

            let (stored, _) = graph
                .get(&ec_id)
                .await
                .expect("should read store")
                .expect("should keep the row");
            assert_eq!(
                stored.ids.get("sharedid.org").map(|id| id.uid.as_str()),
                permit.then_some(cookie_id),
                "{source} should persist the returning-user EID cookie only with a permit"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_generated_ec_persists_eid_updates() {
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut ec_context = EcContext::new_for_test_with_ip(
            None,
            granting_consent(),
            Some("192.0.2.10".to_owned()),
        );
        ec_context
            .generate_if_needed(&settings, Some(&graph))
            .await
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
            &SharedIdRequest::with_value_for_test("generated-cookie-id"),
            &mut response,
        )
        .await;

        let (stored, _) = graph
            .get(&ec_id)
            .await
            .expect("should read store")
            .expect("should create the row");
        assert_eq!(
            stored.ids.get("sharedid.org").map(|id| id.uid.as_str()),
            Some("generated-cookie-id"),
            "should ingest the browser EID in the create"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_auction_transient_miss_does_not_persist_eid_updates() {
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
            .await
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
            &SharedIdRequest::with_value_for_test("shared-cookie-id"),
            &mut response,
        )
        .await;

        assert_did_not_rotate(&ec_context, &ec_id, &response);
        let (stored, _) = graph
            .get(&ec_id)
            .await
            .expect("should read store")
            .expect("should keep the row");
        assert!(
            stored.ids.is_empty(),
            "an auction request must leave the row unchanged"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_never_persists_auction_body_eids() {
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
            .await
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        let (stored, _) = graph
            .get(&ec_id)
            .await
            .expect("should read store")
            .expect("should keep the row");
        assert!(
            stored.ids.is_empty(),
            "should persist neither the auction body nor the cookie EIDs"
        );
    }

    /// Runs a returning navigation's finalize against a live record and
    /// returns the `SharedID` it stores under `pubcid.org`.
    async fn stored_sharedid_after_navigation(
        consent: ConsentContext,
        sharedid: &SharedIdRequest,
        client_eids: Vec<Eid>,
    ) -> Option<String> {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("shrdid");
        let graph = KvIdentityGraph::in_memory("test_store");
        let live = KvEntry::new(
            &consent,
            None,
            current_timestamp(),
            &settings.publisher.domain,
        );
        graph
            .create(&ec_id, &live)
            .await
            .expect("should seed the live row");
        let mut ec_context = EcContext::new_for_test_with_cookie(
            Some(ec_id.clone()),
            Some(ec_id.clone()),
            true,
            false,
            consent,
        );
        ec_context.set_kv_snapshot(graph.load_snapshot(&ec_id).await);
        ec_context.set_eid_sync_source(EidSyncSource::Navigation);
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        ec_context.set_client_eids(client_eids);
        let registry = PartnerRegistry::from_config(&[make_partner("pubcid.org")])
            .expect("should build registry");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            None,
            sharedid,
            &mut response,
        )
        .await;

        let (stored, _) = graph
            .get(&ec_id)
            .await
            .expect("should read store")
            .expect("should keep the row");
        stored.ids.get("pubcid.org").map(|id| id.uid.clone())
    }

    fn sharedid_body_eid(id: &str) -> Eid {
        Eid::new(
            "pubcid.org",
            vec![Uid {
                id: id.to_owned(),
                atype: Some(1),
                ext: None,
            }],
        )
    }

    const STORED_SHAREDID: &str = "11111111-2222-4333-8444-555555555555";
    const OTHER_SHAREDID: &str = "66666666-7777-4888-9999-aaaaaaaaaaaa";

    fn pubcid_request(cookies: &str) -> SharedIdRequest {
        let req = http::Request::builder()
            .uri("https://example.com/")
            .header(http::header::COOKIE, cookies)
            .body(())
            .expect("should build request");
        SharedIdRequest::for_write_back(&SharedIdStorage::Cookie("_pubcid".to_owned()), &req)
    }

    fn pubcid_set_cookies(response: &Response<EdgeBody>) -> Vec<String> {
        response
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter(|value| value.starts_with("_pubcid"))
            .map(str::to_owned)
            .collect()
    }

    /// One `SharedID` write-back case: what the record holds, the request and
    /// whether it is a navigation.
    struct WriteBackCase<'a> {
        consent: ConsentContext,
        stored: Option<&'a str>,
        sharedid: SharedIdRequest,
        navigation: bool,
        lookups: LookupResults,
    }

    /// Runs a returning reader's finalize and returns the `_pubcid` cookies
    /// the response sets and the `SharedID` the record holds afterwards.
    async fn run_write_back(case: WriteBackCase<'_>) -> (Vec<String>, Option<String>) {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("wrback");
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut live = KvEntry::new(
            &granting_consent(),
            None,
            current_timestamp(),
            &settings.publisher.domain,
        );
        if let Some(stored) = case.stored {
            live.ids
                .insert("pubcid.org".to_owned(), KvPartnerId::single(stored));
        }
        graph
            .create(&ec_id, &live)
            .await
            .expect("should seed the live row");
        let mut ec_context = EcContext::new_for_test_with_cookie(
            Some(ec_id.clone()),
            Some(ec_id.clone()),
            true,
            false,
            case.consent,
        );
        ec_context.set_eid_sync_source(EidSyncSource::Navigation);
        if case.navigation {
            ec_context.set_navigation_permit(NavigationPermit::for_test());
        }
        ec_context.set_lookup_results(case.lookups);
        let registry = PartnerRegistry::from_config(&[make_partner("pubcid.org")])
            .expect("should build registry");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            None,
            &case.sharedid,
            &mut response,
        )
        .await;

        let stored = graph
            .get(&ec_id)
            .await
            .expect("should read store")
            .and_then(|(entry, _)| entry.ids.get("pubcid.org").map(|id| id.uid.clone()));
        (pubcid_set_cookies(&response), stored)
    }

    fn navigation_case<'a>(stored: Option<&'a str>, cookies: &str) -> WriteBackCase<'a> {
        WriteBackCase {
            consent: granting_consent(),
            stored,
            sharedid: pubcid_request(cookies),
            navigation: true,
            lookups: LookupResults::default(),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sharedid_generated_by_the_provider_is_written_back_in_the_same_navigation() {
        let generated = KvUid::new(
            UidValue::try_from(STORED_SHAREDID).expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            Some(ProviderId::new("sharedid")),
            Expiry::Never,
        );
        let lookups = LookupResults::new(
            &sample_ec_id("wrback"),
            vec![ProviderLookup {
                provider: ProviderId::new("sharedid"),
                status: LookupStatus::Found {
                    kept: 1,
                    dropped: DropCounts::default(),
                },
                entries: vec![(
                    SourceDomain::try_from("pubcid.org").expect("should build source"),
                    vec![KvEid::new(EidFields::default(), vec![generated])],
                )],
                cleared: Vec::new(),
                opt_out: OptOutMemory::Unchanged,
            }],
        );

        let (cookies, _) = run_write_back(WriteBackCase {
            lookups,
            ..navigation_case(None, "other=1")
        })
        .await;

        assert!(
            cookies
                .first()
                .is_some_and(|cookie| cookie.starts_with(&format!("_pubcid={STORED_SHAREDID};"))),
            "should set the SharedID the provider generated in this navigation's write: {cookies:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sharedid_record_present_and_cookie_absent_sets_both_cookies() {
        let settings = create_test_settings();
        let domain = settings.publisher.ec_cookie_domain();

        let (cookies, _) = run_write_back(navigation_case(Some(STORED_SHAREDID), "other=1")).await;

        assert_eq!(
            cookies.len(),
            2,
            "should set the cookie and its companion: {cookies:?}"
        );
        assert_eq!(
            cookies[0],
            format!(
                "_pubcid={STORED_SHAREDID}; Domain={domain}; Path=/; Secure; SameSite=Lax; Max-Age=31536000"
            ),
            "should set the stored SharedID, readable by Prebid, for a year"
        );
        assert!(
            cookies[1].starts_with("_pubcid_cst=")
                && cookies[1].ends_with("; Path=/; Secure; SameSite=Lax; Max-Age=31536000"),
            "should set the consent-hash companion: {}",
            cookies[1]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sharedid_unchanged_returning_reader_gets_no_set_cookie() {
        let (first, _) = run_write_back(navigation_case(Some(STORED_SHAREDID), "other=1")).await;
        let consent_hash = first[1]
            .split(';')
            .next()
            .and_then(|pair| pair.split_once('='))
            .map(|(_, value)| value.to_owned())
            .expect("should set a consent hash");

        let (cookies, _) = run_write_back(navigation_case(
            Some(STORED_SHAREDID),
            &format!("_pubcid={STORED_SHAREDID}; _pubcid_cst={consent_hash}"),
        ))
        .await;

        assert!(
            cookies.is_empty(),
            "should set no cookie for unchanged cookies: {cookies:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sharedid_equal_cookie_without_a_companion_sets_only_the_companion() {
        let (cookies, _) = run_write_back(navigation_case(
            Some(STORED_SHAREDID),
            &format!("_pubcid={STORED_SHAREDID}"),
        ))
        .await;

        assert_eq!(cookies.len(), 1, "should set one cookie: {cookies:?}");
        assert!(
            cookies[0].starts_with("_pubcid_cst="),
            "should re-set only the companion: {cookies:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sharedid_stored_value_wins_over_a_different_cookie() {
        let (cookies, stored) = run_write_back(navigation_case(
            Some(STORED_SHAREDID),
            &format!("_pubcid={OTHER_SHAREDID}"),
        ))
        .await;

        assert_eq!(
            stored.as_deref(),
            Some(STORED_SHAREDID),
            "should keep the stored SharedID"
        );
        assert!(
            cookies[0].starts_with(&format!("_pubcid={STORED_SHAREDID};")),
            "should restore the stored value in the cookie: {cookies:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sharedid_opt_out_expires_both_cookies() {
        let (cookies, _) = run_write_back(navigation_case(
            Some(STORED_SHAREDID),
            &format!("_pubcid={STORED_SHAREDID}; _pubcid_optout=1"),
        ))
        .await;

        assert_eq!(cookies.len(), 2, "should expire both cookies: {cookies:?}");
        assert!(
            cookies.iter().all(|cookie| cookie.ends_with("Max-Age=0")),
            "should expire both cookies: {cookies:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sharedid_denied_ec_consent_expires_both_cookies() {
        let denied = ConsentContext {
            jurisdiction: Jurisdiction::Gdpr,
            gdpr_applies: true,
            ..ConsentContext::default()
        };

        let (cookies, _) = run_write_back(WriteBackCase {
            consent: denied,
            ..navigation_case(Some(STORED_SHAREDID), &format!("_pubcid={STORED_SHAREDID}"))
        })
        .await;

        assert_eq!(cookies.len(), 2, "should expire both cookies: {cookies:?}");
        assert!(
            cookies.iter().all(|cookie| cookie.ends_with("Max-Age=0")),
            "should expire both cookies: {cookies:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sharedid_purpose_four_denied_leaves_the_cookie_alone() {
        for cookies in ["other=1".to_owned(), format!("_pubcid={OTHER_SHAREDID}")] {
            let (set, _) = run_write_back(WriteBackCase {
                consent: purpose_one_only_consent(),
                ..navigation_case(Some(STORED_SHAREDID), &cookies)
            })
            .await;

            assert!(
                set.is_empty(),
                "Purpose 4 denied should set no SharedID cookie for `{cookies}`: {set:?}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sharedid_is_not_written_on_a_subresource_response() {
        let (cookies, _) = run_write_back(WriteBackCase {
            navigation: false,
            ..navigation_case(Some(STORED_SHAREDID), "other=1")
        })
        .await;

        assert!(
            cookies.is_empty(),
            "should not write back without a navigation"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sharedid_is_not_written_without_cookie_storage() {
        let req = http::Request::builder()
            .uri("https://example.com/")
            .header(http::header::COOKIE, "other=1")
            .body(())
            .expect("should build request");

        let (cookies, _) = run_write_back(WriteBackCase {
            sharedid: SharedIdRequest::for_write_back(&SharedIdStorage::LocalStorageOnly, &req),
            ..navigation_case(Some(STORED_SHAREDID), "other=1")
        })
        .await;

        assert!(
            cookies.is_empty(),
            "should not write a cookie Prebid keeps in local storage"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sharedid_is_created_stored_and_set_on_a_consented_first_visit() {
        let (cookies, stored) = run_write_back(navigation_case(None, "other=1")).await;

        let created = stored.expect("should store a created SharedID");
        assert!(
            uuid::Uuid::parse_str(&created).is_ok_and(|id| id.get_version_num() == 4),
            "should create a UUID v4 as Prebid writes: {created}"
        );
        assert!(
            cookies
                .first()
                .is_some_and(|cookie| cookie.starts_with(&format!("_pubcid={created};"))),
            "should set the created SharedID: {cookies:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sharedid_is_not_created_without_consent_storage_or_with_an_opt_out() {
        let html5_request = || {
            let req = http::Request::builder()
                .uri("https://example.com/")
                .header(http::header::COOKIE, "other=1")
                .body(())
                .expect("should build request");
            SharedIdRequest::for_write_back(&SharedIdStorage::LocalStorageOnly, &req)
        };
        let cases = [
            (
                "opt-out",
                WriteBackCase {
                    sharedid: pubcid_request("_pubcid_optout=1"),
                    ..navigation_case(None, "other=1")
                },
            ),
            (
                "Purpose 4 denied",
                WriteBackCase {
                    consent: purpose_one_only_consent(),
                    ..navigation_case(None, "other=1")
                },
            ),
            (
                "local storage only",
                WriteBackCase {
                    sharedid: html5_request(),
                    ..navigation_case(None, "other=1")
                },
            ),
            (
                "subresource",
                WriteBackCase {
                    navigation: false,
                    ..navigation_case(None, "other=1")
                },
            ),
        ];

        for (label, case) in cases {
            let (cookies, stored) = run_write_back(case).await;

            assert_eq!(stored, None, "should create no SharedID with {label}");
            assert!(
                cookies.iter().all(|cookie| cookie.ends_with("Max-Age=0")),
                "should set no SharedID with {label}: {cookies:?}"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_stores_the_sharedid_cookie_under_pubcid() {
        let stored = stored_sharedid_after_navigation(
            granting_consent(),
            &SharedIdRequest::with_value_for_test("cookie-shared"),
            Vec::new(),
        )
        .await;

        assert_eq!(
            stored.as_deref(),
            Some("cookie-shared"),
            "should store the SharedID cookie under pubcid.org"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_stores_no_sharedid_for_an_opted_out_reader() {
        let stored = stored_sharedid_after_navigation(
            granting_consent(),
            &SharedIdRequest::opted_out_for_test("cookie-shared"),
            vec![sharedid_body_eid("body-shared")],
        )
        .await;

        assert_eq!(
            stored, None,
            "should store no SharedID from the cookie or the request EIDs after an opt-out"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_stores_no_sharedid_when_purpose_four_is_denied() {
        let stored = stored_sharedid_after_navigation(
            purpose_one_only_consent(),
            &SharedIdRequest::with_value_for_test("cookie-shared"),
            vec![sharedid_body_eid("body-shared")],
        )
        .await;

        assert_eq!(
            stored, None,
            "should store no SharedID with Purpose 1 granted and Purpose 4 denied"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_withholds_eid_kv_writes_when_purpose_four_is_denied() {
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
            .await
            .expect("should seed the live row this request updates");
        let mut ec_context = EcContext::new_for_test_with_cookie(
            Some(ec_id.clone()),
            Some(ec_id.clone()),
            true,
            false,
            purpose1_only_consent,
        );
        ec_context.set_kv_snapshot(graph.load_snapshot(&ec_id).await);
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        let (stored, _) = graph
            .get(&ec_id)
            .await
            .expect("should read store")
            .expect("should keep the row");
        assert!(
            !stored.ids.contains_key("id5-sync.com"),
            "denying TCF Purpose 4 must keep the EID out of KV even though Purpose 1 \
             (EC) consent is granted"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_withholds_provider_owned_fallback_when_purpose_four_is_denied() {
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
            .await
            .expect("should seed the live row this request updates");
        let mut ec_context = EcContext::new_for_test_with_cookie(
            Some(ec_id.clone()),
            Some(ec_id.clone()),
            true,
            false,
            purpose1_only_consent,
        );
        ec_context.set_kv_snapshot(graph.load_snapshot(&ec_id).await);
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        // Assert
        let (stored, _) = graph
            .get(&ec_id)
            .await
            .expect("should read store")
            .expect("should keep the row");
        assert!(
            !stored.ids.contains_key("id5-sync.com"),
            "should keep the provider-owned fallback out of KV without Purpose 4"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_skips_browser_uid_for_provider_owned_source() {
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
            .await
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
            &SharedIdRequest::with_value_for_test("fallback-cookie-id"),
            &mut response,
        )
        .await;

        // Assert
        let (stored, _) = graph
            .get(&ec_id)
            .await
            .expect("should read store")
            .expect("should create the row");
        assert!(
            !stored.ids.contains_key("sharedid.org"),
            "should not store a browser UID for a provider-owned source"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_auction_confirmed_miss_does_not_create_a_row() {
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
            &SharedIdRequest::with_value_for_test("shared-cookie-id"),
            &mut response,
        )
        .await;

        assert_did_not_rotate(&ec_context, &ec_id, &response);
        assert!(
            graph
                .get(&ec_id)
                .await
                .expect("should read store")
                .is_none(),
            "a confirmed miss must not create a root entry"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_transient_missing_row_confirms_present_and_does_not_rotate() {
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
            .await
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert_did_not_rotate(&ec_context, &orphan, &response);
        assert!(
            matches!(ec_context.kv_snapshot(), EcKvSnapshot::Present { .. }),
            "confirming read must adopt the now-visible row rather than rotating"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_two_missing_reads_do_not_rotate_a_row_the_store_still_lists() {
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
            .await
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert_did_not_rotate(&ec_context, &orphan, &response);
        assert_eq!(
            graph
                .get(&orphan)
                .await
                .expect("should read store")
                .map(|(entry, _)| entry.consent.ok),
            Some(true),
            "the original identity row must survive two stale point reads"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_rotates_when_only_a_longer_key_shares_the_orphan_prefix() {
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
            .await
            .expect("should seed the neighbouring row");
        assert!(
            !graph
                .key_exists_confirmed(&orphan)
                .await
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        let replacement = ec_context.ec_value().expect("should rotate the orphan");
        assert_ne!(
            replacement, orphan,
            "a proven-absent orphan must rotate even with a prefix neighbour present"
        );
        assert!(
            graph
                .get(replacement)
                .await
                .expect("should read the replacement")
                .is_some(),
            "the replacement cookie should have a backing row"
        );
        assert!(
            graph
                .get(&neighbour)
                .await
                .expect("should read the neighbour")
                .is_some(),
            "the neighbouring identity must be left untouched"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_does_not_rotate_when_the_existence_check_fails() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert_did_not_rotate(&ec_context, &orphan, &response);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_generated_ec_does_not_emit_cookie_for_authoritative_missing_row() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert!(
            !ec_cookie_emitted(&response),
            "must not emit a cookie without an authoritative backing row"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_denied_without_cookie_is_noop() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "should not set EC header"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "should not mutate cookie when there is nothing to revoke"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_unknown_jurisdiction_strips_headers_without_expiring_cookie() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

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

    fn mirror_cookies(response: &Response<EdgeBody>) -> Vec<String> {
        response
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter(|cookie| cookie.starts_with("__Host-ts-eids-mirror="))
            .map(str::to_owned)
            .collect()
    }

    fn provider_found(source: &str) -> LookupResults {
        provider_found_by(source, "example-provider")
    }

    fn provider_found_by(source: &str, owner: &str) -> LookupResults {
        let uid = KvUid::new(
            UidValue::try_from("provider-uid").expect("should build uid"),
            AgentType::try_from(1).expect("should build atype"),
            None,
            Some(ProviderId::new(owner)),
            Expiry::Never,
        );
        // Made for the EC ID `run_mirror_navigation` seeds.
        LookupResults::new(
            &sample_ec_id("mirr01"),
            vec![ProviderLookup {
                provider: ProviderId::new("example-provider"),
                status: LookupStatus::Found {
                    kept: 1,
                    dropped: DropCounts::default(),
                },
                entries: vec![(
                    SourceDomain::try_from(source).expect("should build source"),
                    vec![KvEid::new(EidFields::default(), vec![uid])],
                )],
                cleared: Vec::new(),
                opt_out: OptOutMemory::Unchanged,
            }],
        )
    }

    async fn run_mirror_navigation(
        lookups: LookupResults,
        mirror: Option<&str>,
        kv_read_failed: bool,
    ) -> Response<EdgeBody> {
        run_mirror_request(lookups, mirror, kv_read_failed, true).await
    }

    async fn run_mirror_request(
        lookups: LookupResults,
        mirror: Option<&str>,
        kv_read_failed: bool,
        navigation: bool,
    ) -> Response<EdgeBody> {
        run_mirror_request_with_owner(
            lookups,
            mirror,
            kv_read_failed,
            navigation,
            MirrorGate::Open,
        )
        .await
    }

    /// What withholds the mirrored source in [`run_mirror_request_with_owner`].
    #[derive(Clone, Copy)]
    enum MirrorGate {
        /// Nothing withholds the source.
        Open,
        /// The source's owning provider has TCF vendor 9999 and the request's
        /// TCF string keeps Purposes 1 and 4 but denies that vendor.
        VendorDenied,
        /// A provider excludes the source where GDPR applies, and the request
        /// is a GDPR visit granting Purposes 1 and 4.
        RegionExcluded,
        /// The source's provider's cookies on this request record an opt-out.
        CookieOptedOut,
    }

    /// Runs a mirror request with the source withheld as `gate` says.
    async fn run_mirror_request_with_owner(
        lookups: LookupResults,
        mirror: Option<&str>,
        kv_read_failed: bool,
        navigation: bool,
        gate: MirrorGate,
    ) -> Response<EdgeBody> {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("mirr01");
        let graph = KvIdentityGraph::in_memory("mirror_store");
        let live = KvEntry::new(
            &granting_consent(),
            None,
            current_timestamp(),
            &settings.publisher.domain,
        );
        graph
            .create(&ec_id, &live)
            .await
            .expect("should seed the live row");
        let snapshot = if kv_read_failed {
            EcKvSnapshot::Failed {
                ec_id: ec_id.clone(),
            }
        } else {
            graph.load_snapshot(&ec_id).await
        };
        let mut ec_context = returning_user_context(&ec_id, snapshot, navigation);
        ec_context.set_mirror_cookie_for_test(mirror);
        ec_context.set_lookup_results(lookups);
        let mut partner = make_partner("ids.example.com");
        partner.mirror_enabled = true;
        let mut registry = PartnerRegistry::from_config(&[partner]).expect("should build registry");
        let owner_vendor = match gate {
            MirrorGate::Open => None,
            MirrorGate::VendorDenied => {
                *ec_context.consent_mut() = consent_granting_vendors(Vec::new());
                NonZeroU16::new(9999)
            }
            MirrorGate::RegionExcluded => {
                *ec_context.consent_mut() = consent_granting_vendors(Vec::new());
                assert!(
                    registry.exclude_where_gdpr_applies("ids.example.com"),
                    "should exclude the source"
                );
                None
            }
            MirrorGate::CookieOptedOut => {
                ec_context.set_cookie_opted_out_sources(vec!["ids.example.com".to_owned()]);
                None
            }
        };
        registry.mark_provider_owned("ids.example.com", owner_vendor);
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;
        response
    }

    #[tokio::test(flavor = "current_thread")]
    async fn navigation_sets_mirror_from_committed_row() {
        let response = run_mirror_navigation(provider_found("ids.example.com"), None, false).await;

        let cookies = mirror_cookies(&response);
        assert_eq!(cookies.len(), 1, "should set one mirror cookie");
        assert!(
            !cookies[0].contains("Max-Age=0") && cookies[0].contains("Max-Age="),
            "should set the mirror with a non-zero Max-Age"
        );
        assert!(
            !cookies[0].contains("HttpOnly") && !cookies[0].contains("Domain="),
            "should be JS-readable and host-only"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pull_sync_result_is_mirrored_with_its_owner() {
        let response = run_mirror_navigation(
            provider_found_by("ids.example.com", "pull-sync:ids.example.com"),
            None,
            false,
        )
        .await;

        assert_eq!(
            mirror_cookies(&response).len(),
            1,
            "should mirror a provider-owned pull-sync UID"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vendor_withdrawal_expires_the_mirror_on_a_non_navigation() {
        let response = run_mirror_request_with_owner(
            provider_found("ids.example.com"),
            Some("stale"),
            false,
            false,
            MirrorGate::VendorDenied,
        )
        .await;

        let cookies = mirror_cookies(&response);
        assert_eq!(cookies.len(), 1, "should emit one mirror cookie");
        assert!(
            cookies[0].contains("Max-Age=0"),
            "should expire a mirror whose owning vendor lost consent"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vendor_withdrawal_expires_the_mirror_when_the_row_is_unavailable() {
        let response = run_mirror_request_with_owner(
            provider_found("ids.example.com"),
            Some("stale"),
            true,
            true,
            MirrorGate::VendorDenied,
        )
        .await;

        let cookies = mirror_cookies(&response);
        assert_eq!(cookies.len(), 1, "should emit one mirror cookie");
        assert!(
            cookies[0].contains("Max-Age=0"),
            "should expire the mirror without a readable row"
        );
    }

    /// Runs a mirror request with two mirrored partners: `a.example.com`,
    /// whose owning vendor 1111 is granted, and `b.example.com`, whose owning
    /// vendor 9999 is denied. The lookup finds UIDs for both.
    async fn run_split_vendor_mirror_request(
        mirror: Option<&str>,
        navigation: bool,
    ) -> Response<EdgeBody> {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("mirr01");
        let graph = KvIdentityGraph::in_memory("mirror_store");
        let live = KvEntry::new(
            &granting_consent(),
            None,
            current_timestamp(),
            &settings.publisher.domain,
        );
        graph
            .create(&ec_id, &live)
            .await
            .expect("should seed the live row");
        let mut ec_context =
            returning_user_context(&ec_id, graph.load_snapshot(&ec_id).await, navigation);
        *ec_context.consent_mut() = consent_granting_vendors(vec![1111]);
        ec_context.set_mirror_cookie_for_test(mirror);
        let entry = |source: &str| {
            let uid = KvUid::new(
                UidValue::try_from("provider-uid").expect("should build uid"),
                AgentType::try_from(1).expect("should build atype"),
                None,
                Some(ProviderId::new("example-provider")),
                Expiry::Never,
            );
            (
                SourceDomain::try_from(source).expect("should build source"),
                vec![KvEid::new(EidFields::default(), vec![uid])],
            )
        };
        ec_context.set_lookup_results(LookupResults::new(
            &ec_id,
            vec![ProviderLookup {
                provider: ProviderId::new("example-provider"),
                status: LookupStatus::Found {
                    kept: 2,
                    dropped: DropCounts::default(),
                },
                entries: vec![entry("a.example.com"), entry("b.example.com")],
                cleared: Vec::new(),
                opt_out: OptOutMemory::Unchanged,
            }],
        ));
        let partners = ["a.example.com", "b.example.com"].map(|source| {
            let mut partner = make_partner(source);
            partner.mirror_enabled = true;
            partner
        });
        let mut registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        registry.mark_provider_owned("a.example.com", NonZeroU16::new(1111));
        registry.mark_provider_owned("b.example.com", NonZeroU16::new(9999));
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;
        response
    }

    fn mirror_value(cookie: &str) -> &str {
        cookie
            .strip_prefix("__Host-ts-eids-mirror=")
            .and_then(|rest| rest.split(';').next())
            .expect("should carry a mirror value")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn partial_vendor_denial_keeps_a_mirror_without_the_denied_vendor() {
        // Arrange
        let _clock = PinnedMirrorClock::at(MIRROR_DAY_START + 60);
        let navigation = run_split_vendor_mirror_request(None, true).await;
        let set = mirror_cookies(&navigation);
        assert_eq!(set.len(), 1, "should set the mirror on navigation");
        let value = mirror_value(&set[0]).to_owned();
        let sources = mirror_sources(&value).expect("should decode the mirror");
        assert_eq!(
            sources,
            vec!["a.example.com".to_owned()],
            "should mirror only the granted vendor's partner"
        );

        // Act
        let non_navigation = run_split_vendor_mirror_request(Some(&value), false).await;
        let next_navigation = run_split_vendor_mirror_request(Some(&value), true).await;

        // Assert
        assert!(
            mirror_cookies(&non_navigation).is_empty(),
            "should keep a mirror that holds no denied vendor's UIDs"
        );
        assert!(
            mirror_cookies(&next_navigation).is_empty(),
            "should not re-set an unchanged mirror on the next navigation"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn no_expiry_mirror_is_kept_within_a_utc_day_and_re_set_once_on_the_next() {
        // Arrange
        let first_clock = PinnedMirrorClock::at(MIRROR_DAY_START + 60);
        let first = run_mirror_navigation(provider_found("ids.example.com"), None, false).await;
        let set = mirror_cookies(&first);
        assert_eq!(
            set.len(),
            1,
            "should set the mirror on the first navigation"
        );
        let value = mirror_value(&set[0]).to_owned();
        drop(first_clock);

        // Act
        let same_day = {
            let _clock = PinnedMirrorClock::at(MIRROR_DAY_START + 86_399);
            run_mirror_navigation(provider_found("ids.example.com"), Some(&value), false).await
        };
        let next_day = {
            let _clock = PinnedMirrorClock::at(MIRROR_DAY_START + 86_400 + 60);
            run_mirror_navigation(provider_found("ids.example.com"), Some(&value), false).await
        };

        // Assert
        assert!(
            mirror_cookies(&same_day).is_empty(),
            "should not re-set an unchanged mirror later the same UTC day"
        );
        let next_day_cookies = mirror_cookies(&next_day);
        assert_eq!(
            next_day_cookies.len(),
            1,
            "should re-set the mirror once on the next UTC day"
        );
        assert_ne!(
            mirror_value(&next_day_cookies[0]),
            value,
            "should carry the next day's exp"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn partial_vendor_denial_expires_a_mirror_holding_the_denied_vendor() {
        // Arrange
        let json = serde_json::json!({
            "v": 1,
            "eids": [
                {"source": "a.example.com", "uids": [{"id": "provider-uid", "atype": 1}]},
                {"source": "b.example.com", "uids": [{"id": "provider-uid", "atype": 1}]},
            ],
        });
        let value = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string());

        // Act
        let response = run_split_vendor_mirror_request(Some(&value), false).await;

        // Assert
        let cookies = mirror_cookies(&response);
        assert_eq!(cookies.len(), 1, "should emit one mirror cookie");
        assert!(
            cookies[0].contains("Max-Age=0"),
            "should expire a mirror holding a denied vendor's UIDs"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn region_exclusion_expires_the_mirror_on_a_non_navigation() {
        let response = run_mirror_request_with_owner(
            provider_found("ids.example.com"),
            Some("stale"),
            false,
            false,
            MirrorGate::RegionExcluded,
        )
        .await;

        let cookies = mirror_cookies(&response);
        assert_eq!(cookies.len(), 1, "should emit one mirror cookie");
        assert!(
            cookies[0].contains("Max-Age=0"),
            "should expire a mirror holding a source excluded where GDPR applies"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cookie_opt_out_expires_the_mirror_on_a_non_navigation() {
        let response = run_mirror_request_with_owner(
            provider_found("ids.example.com"),
            Some("stale"),
            false,
            false,
            MirrorGate::CookieOptedOut,
        )
        .await;

        let cookies = mirror_cookies(&response);
        assert_eq!(cookies.len(), 1, "should emit one mirror cookie");
        assert!(
            cookies[0].contains("Max-Age=0"),
            "should expire a mirror holding a source opted out in provider cookies"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vendor_withdrawal_without_a_mirror_cookie_emits_nothing() {
        let response = run_mirror_request_with_owner(
            provider_found("ids.example.com"),
            None,
            false,
            false,
            MirrorGate::VendorDenied,
        )
        .await;

        assert!(
            mirror_cookies(&response).is_empty(),
            "should not expire a mirror the request did not carry"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn consented_non_navigation_leaves_the_mirror_untouched() {
        let response = run_mirror_request(
            provider_found("ids.example.com"),
            Some("stale"),
            false,
            false,
        )
        .await;

        assert!(
            mirror_cookies(&response).is_empty(),
            "should neither set nor expire the mirror outside a navigation"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn navigation_with_an_unchanged_mirror_emits_no_cookie() {
        // Arrange
        let first = run_mirror_navigation(provider_found("ids.example.com"), None, false).await;
        let set = mirror_cookies(&first);
        let value = set[0]
            .strip_prefix("__Host-ts-eids-mirror=")
            .and_then(|rest| rest.split(';').next())
            .expect("should set a mirror value");

        // Act
        let unchanged =
            run_mirror_navigation(provider_found("ids.example.com"), Some(value), false).await;
        let changed =
            run_mirror_navigation(provider_found("ids.example.com"), Some("stale"), false).await;

        // Assert
        assert!(
            mirror_cookies(&unchanged).is_empty(),
            "should not re-set a mirror whose value is unchanged"
        );
        assert_eq!(
            mirror_cookies(&changed).len(),
            1,
            "should set the mirror when its value changed"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn navigation_without_provider_uids_expires_mirror() {
        let response = run_mirror_navigation(LookupResults::default(), Some("stale"), false).await;

        let cookies = mirror_cookies(&response);
        assert_eq!(cookies.len(), 1, "should emit one mirror cookie");
        assert!(
            cookies[0].contains("Max-Age=0"),
            "should expire the mirror when no provider UID qualifies"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn navigation_without_mirror_cookie_emits_no_expiry() {
        let response = run_mirror_navigation(LookupResults::default(), None, false).await;

        assert!(
            mirror_cookies(&response).is_empty(),
            "should not expire a mirror the request did not carry"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn kv_read_failure_leaves_mirror_untouched() {
        let response =
            run_mirror_navigation(provider_found("ids.example.com"), Some("stale"), true).await;

        assert!(
            mirror_cookies(&response).is_empty(),
            "should leave the mirror untouched when the row could not be read"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn withdrawn_consent_without_mirror_cookie_emits_nothing() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert!(
            mirror_cookies(&response).is_empty(),
            "should not expire a mirror the request did not carry"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn withdrawn_consent_expires_mirror_on_any_response() {
        let settings = create_test_settings();
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context = make_context_with_consent(None, None, false, false, consent);
        ec_context.set_mirror_cookie_for_test(Some("stale"));
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            None,
            &PartnerRegistry::empty(),
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        let cookies = mirror_cookies(&response);
        assert_eq!(cookies.len(), 1, "should emit one mirror cookie");
        assert!(
            cookies[0].contains("Max-Age=0"),
            "should expire the mirror without a navigation"
        );
    }

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

    /// Whether the response sets or expires the `ts-ec` cookie; other
    /// cookies, such as the mirror, are ignored.
    fn ec_cookie_emitted(response: &Response<EdgeBody>) -> bool {
        response
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|cookie| cookie.starts_with("ts-ec="))
    }

    fn assert_did_not_rotate(ec_context: &EcContext, orphan: &str, response: &Response<EdgeBody>) {
        assert_eq!(
            ec_context.ec_value(),
            Some(orphan),
            "must not rotate the active EC ID"
        );
        assert!(!ec_context.ec_generated(), "must not mark a rotated EC");
        assert!(
            !ec_cookie_emitted(response),
            "must not emit a replacement cookie"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_not_read_snapshot_does_not_rotate() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert_did_not_rotate(&ec_context, &orphan, &response);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_failed_snapshot_does_not_rotate() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert_did_not_rotate(&ec_context, &orphan, &response);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_tombstone_snapshot_does_not_rotate() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert_did_not_rotate(&ec_context, &orphan, &response);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_subresource_missing_row_does_not_rotate() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert_did_not_rotate(&ec_context, &orphan, &response);
        assert!(
            graph
                .get(&orphan)
                .await
                .expect("should read store")
                .is_none(),
            "a non-eligible request must not create the missing root"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_withdrawal_tombstones_present_id_and_skips_missing_other() {
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
            .await
            .expect("should seed active row");
        ec_context.set_kv_snapshot(graph.load_snapshot(&active_ec).await);
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        let (active_stored, _) = graph
            .get(&active_ec)
            .await
            .expect("should read active row")
            .expect("active row should remain as a tombstone");
        assert!(
            !active_stored.consent.ok,
            "the present active ID should be tombstoned after strong confirmation"
        );
        assert!(
            graph
                .get(&cookie_ec)
                .await
                .expect("should read store")
                .is_none(),
            "a missing second ID must never be created by withdrawal"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_withdrawal_tombstones_the_active_and_cookie_ids_once() {
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
            .await
            .expect("should seed active row");
        graph
            .create(
                &cookie_ec,
                &KvEntry::minimal("cookie.example.com", "cookie-uid", 1_000),
            )
            .await
            .expect("should seed cookie row");
        ec_context.set_kv_snapshot(graph.load_snapshot(&active_ec).await);
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        let (active_tombstone, active_generation) = graph
            .get(&active_ec)
            .await
            .expect("should read active row")
            .expect("should retain active tombstone");
        let (cookie_tombstone, cookie_generation) = graph
            .get(&cookie_ec)
            .await
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
            &SharedIdRequest::default(),
            &mut repeated_response,
        )
        .await;

        assert_eq!(
            graph
                .get(&active_ec)
                .await
                .expect("should read active row")
                .expect("should retain active tombstone")
                .1,
            active_generation,
            "repeated finalization should not rewrite active tombstone"
        );
        assert_eq!(
            graph
                .get(&cookie_ec)
                .await
                .expect("should read cookie row")
                .expect("should retain cookie tombstone")
                .1,
            cookie_generation,
            "repeated finalization should not rewrite the cookie tombstone"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_withdrawal_keeps_cookie_deletion_on_kv_failure() {
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
        ec_context.set_pull_marker_present_for_test(true);
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        let graph = KvIdentityGraph::failing("unavailable-store");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

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

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_withdrawal_without_marker_or_ec_cookie_does_not_set_cookie() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert!(
            response.headers().get(http::header::SET_COOKIE).is_none(),
            "withdrawal without browser identity state should not add a cookie"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_withdrawal_expires_marker_without_ec_cookie() {
        let settings = create_test_settings();
        let consent = ConsentContext {
            jurisdiction: Jurisdiction::UsState("CA".to_owned()),
            gpc: true,
            source: ConsentSource::Cookie,
            ..Default::default()
        };
        let mut ec_context = make_context_with_consent(None, None, false, false, consent);
        ec_context.set_pull_marker_present_for_test(true);
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            None,
            &PartnerRegistry::empty(),
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

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

    #[tokio::test(flavor = "current_thread")]
    async fn auction_request_never_writes_browser_eids() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("auc001");
        let graph = KvIdentityGraph::in_memory("finalize");
        graph
            .create(&ec_id, &live_entry())
            .await
            .expect("should seed row");
        let mut ec_context = EcContext::new_for_test(Some(ec_id.clone()), granting_consent());
        ec_context.set_eid_sync_source(EidSyncSource::Auction);
        ec_context.set_kv_snapshot(graph.load_snapshot(&ec_id).await);
        let cookie = eids_cookie("partner.example.com", "u-1");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry_for("partner.example.com"),
            Some(&cookie),
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        let (entry, _) = graph
            .get(&ec_id)
            .await
            .expect("should read")
            .expect("should exist");
        assert!(
            entry.ids.is_empty(),
            "should not write from a non-navigation request"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn navigation_writes_browser_eids_once() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("nav001");
        let graph = KvIdentityGraph::in_memory("finalize");
        graph
            .create(&ec_id, &live_entry())
            .await
            .expect("should seed row");
        let mut ec_context = EcContext::new_for_test(Some(ec_id.clone()), granting_consent());
        ec_context.set_eid_sync_source(EidSyncSource::Navigation);
        ec_context.set_kv_snapshot(graph.load_snapshot(&ec_id).await);
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        let cookie = eids_cookie("partner.example.com", "u-1");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry_for("partner.example.com"),
            Some(&cookie),
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        let (entry, _) = graph
            .get(&ec_id)
            .await
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

    #[tokio::test(flavor = "current_thread")]
    async fn redirect_navigation_does_not_write() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("red001");
        let graph = KvIdentityGraph::in_memory("finalize");
        graph
            .create(&ec_id, &live_entry())
            .await
            .expect("should seed row");
        let mut ec_context = EcContext::new_for_test(Some(ec_id.clone()), granting_consent());
        ec_context.set_eid_sync_source(EidSyncSource::Navigation);
        ec_context.set_kv_snapshot(graph.load_snapshot(&ec_id).await);
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        let (entry, _) = graph
            .get(&ec_id)
            .await
            .expect("should read")
            .expect("should exist");
        assert!(entry.ids.is_empty(), "should not write on a redirect");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generated_ec_without_permit_gets_no_cookie_or_row() {
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("finalize");
        let mut ec_context = EcContext::new_for_test_with_ip(
            None,
            granting_consent(),
            Some("192.0.2.10".to_owned()),
        );
        ec_context
            .generate_if_needed(&settings, Some(&graph))
            .await
            .expect("should mint");
        let ec_id = ec_context.ec_value().expect("should mint").to_owned();
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert!(
            graph.get(&ec_id).await.expect("should read").is_none(),
            "should not create without a permit"
        );
        assert!(
            get_header(&response, "set-cookie").is_none(),
            "should not issue the EC"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generated_ec_with_permit_creates_row_and_sets_cookie() {
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("finalize");
        let mut ec_context = EcContext::new_for_test_with_ip(
            None,
            granting_consent(),
            Some("192.0.2.10".to_owned()),
        );
        ec_context
            .generate_if_needed(&settings, Some(&graph))
            .await
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert!(
            graph.get(&ec_id).await.expect("should read").is_some(),
            "should create the row"
        );
        assert!(
            get_header(&response, "set-cookie").is_some(),
            "should issue the EC"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generated_ec_collision_retries_with_a_fresh_id() {
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("finalize");
        let mut ec_context = EcContext::new_for_test_with_ip(
            None,
            granting_consent(),
            Some("192.0.2.10".to_owned()),
        );
        ec_context
            .generate_if_needed(&settings, Some(&graph))
            .await
            .expect("should mint");
        let colliding_id = ec_context.ec_value().expect("should mint").to_owned();
        graph
            .create(&colliding_id, &live_entry())
            .await
            .expect("should seed a colliding row");
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        let issued = ec_context.ec_value().expect("should keep an EC").to_owned();
        assert_ne!(issued, colliding_id, "should retry with a fresh EC ID");
        assert!(
            graph
                .get(&issued)
                .await
                .expect("should read the fresh row")
                .is_some(),
            "should create the fresh ID's row before issuing it"
        );
        assert!(
            get_header_str(&response, "set-cookie")
                .is_some_and(|cookie| cookie.starts_with(&format!("ts-ec={issued}"))),
            "should issue the fresh EC ID"
        );
    }

    /// [`EcKvStore`] wrapper whose first `collisions` `Add` writes report a
    /// precondition failure, forcing a create to retry with a fresh ID.
    struct AddCollidingEcKv {
        inner: InMemoryEcKv,
        collisions_remaining: Mutex<u32>,
        add_attempts: Mutex<u32>,
    }

    impl AddCollidingEcKv {
        fn new(collisions: u32) -> Self {
            Self {
                inner: InMemoryEcKv::new("add-colliding-store"),
                collisions_remaining: Mutex::new(collisions),
                add_attempts: Mutex::new(0),
            }
        }
    }

    #[async_trait::async_trait(?Send)]
    impl EcKvStore for Rc<AddCollidingEcKv> {
        fn store_name(&self) -> &str {
            self.inner.store_name()
        }
        async fn lookup(
            &self,
            key: &str,
        ) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            self.inner.lookup(key).await
        }
        async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
            self.inner.key_exists(key).await
        }
        async fn insert(
            &self,
            key: &str,
            write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            if matches!(write.mode, EcKvWriteMode::Add) {
                *self
                    .add_attempts
                    .lock()
                    .expect("should lock attempt counter") += 1;
                let mut remaining = self
                    .collisions_remaining
                    .lock()
                    .expect("should lock collision counter");
                if *remaining > 0 {
                    *remaining -= 1;
                    return Ok(EcKvWriteOutcome::PreconditionFailed);
                }
            }
            self.inner.insert(key, write).await
        }
        async fn list_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<Vec<String>, Report<TrustedServerError>> {
            self.inner.list_keys_with_prefix(prefix, limit).await
        }
        async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            self.inner.delete(key).await
        }
    }

    fn colliding_graph(collisions: u32) -> (KvIdentityGraph, Rc<AddCollidingEcKv>) {
        let store = Rc::new(AddCollidingEcKv::new(collisions));
        (KvIdentityGraph::new(Rc::clone(&store)), store)
    }

    fn add_attempts(store: &AddCollidingEcKv) -> u32 {
        *store
            .add_attempts
            .lock()
            .expect("should lock attempt counter")
    }

    async fn finalize_minted_navigation(
        settings: &Settings,
        graph: &KvIdentityGraph,
        ip: &str,
    ) -> (EcContext, Response<EdgeBody>) {
        let mut ec_context =
            EcContext::new_for_test_with_ip(None, granting_consent(), Some(ip.to_owned()));
        ec_context
            .generate_if_needed(settings, Some(graph))
            .await
            .expect("should mint");
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        let mut response = empty_response();
        ec_finalize_response(
            settings,
            &mut ec_context,
            Some(graph),
            &PartnerRegistry::empty(),
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;
        (ec_context, response)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_if_needed_retries_id_collision_then_persists() {
        let settings = create_test_settings();
        let (graph, store) = colliding_graph(2);

        let (ec, response) = finalize_minted_navigation(&settings, &graph, "192.0.2.5").await;

        assert!(ec.ec_value().is_some(), "should allocate a fresh EC ID");
        assert!(ec.ec_generated(), "should mark the EC as generated");
        assert!(
            matches!(ec.kv_snapshot(), EcKvSnapshot::Present { .. }),
            "generation should seed a present snapshot"
        );
        assert_eq!(
            add_attempts(&store),
            3,
            "should make one Add per collision plus the one that lands"
        );
        assert!(
            ec_cookie_emitted(&response),
            "should issue the EC once its row is persisted"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn generate_if_needed_errors_after_collision_exhaustion() {
        let settings = create_test_settings();
        // Collide on every attempt so the bounded retry is exhausted.
        let (graph, store) = colliding_graph(u32::MAX);

        let (_ec, response) = finalize_minted_navigation(&settings, &graph, "192.0.2.6").await;

        assert_eq!(
            add_attempts(&store),
            MAX_CREATE_ATTEMPTS,
            "should stop after the bounded number of attempts"
        );
        assert!(
            !ec_cookie_emitted(&response),
            "must not issue an EC ID it could not persist"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn orphan_recovery_retries_id_collision_then_persists() {
        let settings = create_test_settings();
        let orphaned_ec = sample_ec_id("orphn2");
        let (graph, store) = colliding_graph(2);
        let mut ec_context = EcContext::new_for_test_with_ip(
            Some(orphaned_ec.clone()),
            granting_consent(),
            Some("192.0.2.11".to_owned()),
        );
        ec_context.set_recovery_eligible(true);
        ec_context.set_kv_snapshot(EcKvSnapshot::Missing {
            ec_id: orphaned_ec.clone(),
        });
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        let replacement = ec_context.ec_value().expect("should rotate orphan");
        assert_ne!(replacement, orphaned_ec, "should rotate to a fresh ID");
        assert!(
            graph
                .get(replacement)
                .await
                .expect("should read replacement")
                .is_some(),
            "replacement cookie should have a backing row"
        );
        assert_eq!(
            add_attempts(&store),
            3,
            "should make one Add per collision plus the one that lands"
        );
        assert!(
            ec_cookie_emitted(&response),
            "should emit replacement cookie after persistence"
        );
    }

    /// A minted EC with a page-phase lookup that found one UID.
    async fn minted_context_with_lookup(
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
            .await
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

    #[tokio::test(flavor = "current_thread")]
    async fn minted_ec_row_is_created_with_its_lookup_results() {
        // Arrange
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("finalize");
        let (mut ec_context, ec_id) = minted_context_with_lookup(&settings, &graph).await;
        let mut response = empty_response();

        // Act
        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry_for("provider.example.com"),
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        // Assert
        let (entry, _) = graph
            .get(&ec_id)
            .await
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

    #[tokio::test(flavor = "current_thread")]
    async fn minted_ec_collision_retries_without_its_lookup_results() {
        // Arrange
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("finalize");
        let (mut ec_context, ec_id) = minted_context_with_lookup(&settings, &graph).await;
        graph
            .create(&ec_id, &live_entry())
            .await
            .expect("should seed a colliding row");
        let mut response = empty_response();

        // Act
        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry_for("provider.example.com"),
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        // Assert
        let (entry, _) = graph
            .get(&ec_id)
            .await
            .expect("should read row")
            .expect("should keep the colliding row");
        assert!(
            !entry.ids.contains_key("provider.example.com"),
            "should not write lookup results into another visitor's row"
        );
        let issued = ec_context.ec_value().expect("should keep an EC").to_owned();
        assert_ne!(issued, ec_id, "should retry with a fresh EC ID");
        let (fresh, _) = graph
            .get(&issued)
            .await
            .expect("should read the fresh row")
            .expect("should create the fresh row");
        assert!(
            !fresh.ids.contains_key("provider.example.com"),
            "should not carry lookups made for the colliding ID into the fresh row"
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

    async fn assert_withdrawn(graph: &KvIdentityGraph, ec_id: &str, response: &Response<EdgeBody>) {
        let (entry, _) = graph
            .get(ec_id)
            .await
            .expect("should read")
            .expect("should keep the tombstone");
        assert!(!entry.consent.ok, "should tombstone the row");
        assert!(
            get_header_str(response, "set-cookie")
                .is_some_and(|cookie| cookie.contains("Max-Age=0")),
            "should expire the EC cookie"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn withdrawal_without_permit_tombstones_row_and_expires_cookie() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("wd0001");
        let graph = KvIdentityGraph::in_memory("finalize");
        graph
            .create(&ec_id, &live_entry())
            .await
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert_withdrawn(&graph, &ec_id, &response).await;
        assert!(
            get_header(&response, "x-ts-ec").is_none(),
            "should still strip EC response headers"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn withdrawal_on_a_redirect_tombstones_row_and_expires_cookie() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("wd0002");
        let graph = KvIdentityGraph::in_memory("finalize");
        graph
            .create(&ec_id, &live_entry())
            .await
            .expect("should seed row");
        let mut ec_context = withdrawing_context(&ec_id);
        ec_context.set_kv_snapshot(graph.load_snapshot(&ec_id).await);
        ec_context.set_navigation_permit(NavigationPermit::for_test());
        let mut response = empty_response();
        *response.status_mut() = http::StatusCode::FOUND;

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert_withdrawn(&graph, &ec_id, &response).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn withdrawal_retries_after_a_concurrent_write() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("wd0003");
        let graph = KvIdentityGraph::in_memory("finalize");
        graph
            .create(&ec_id, &live_entry())
            .await
            .expect("should seed row");
        let mut ec_context = withdrawing_context(&ec_id);
        ec_context.set_kv_snapshot(graph.load_snapshot(&ec_id).await);
        graph
            .upsert_partner_id(&ec_id, "partner.example.com", "late-uid")
            .await
            .expect("should write concurrently after the snapshot");
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert_withdrawn(&graph, &ec_id, &response).await;
    }

    fn live_entry() -> KvEntry {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_withholds_after_response_lookups_when_the_confirmed_write_fails() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("after3");
        let graph = KvIdentityGraph::write_failing_with(
            &ec_id,
            &crate::ec::kv_types::KvEntry::minimal("p.example", "uid", 1),
        )
        .await;
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert!(
            ec_context.take_after_response_permit().is_none(),
            "should not allow a follow-up write after the page-load write failed"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_allows_after_response_lookups_on_a_live_row() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("after1");
        let graph = KvIdentityGraph::in_memory("test_store");
        graph
            .create(
                &ec_id,
                &crate::ec::kv_types::KvEntry::minimal("p.example", "uid", 1),
            )
            .await
            .expect("should seed the identity");
        let mut ec_context =
            returning_user_context(&ec_id, graph.load_snapshot(&ec_id).await, true);
        let mut response = empty_response();

        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert!(
            ec_context.take_after_response_permit().is_some(),
            "should allow after-response lookups when the page write had nothing to change"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_withholds_after_response_lookups_when_the_row_read_failed() {
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        assert!(
            ec_context.take_after_response_permit().is_none(),
            "should not allow after-response lookups when the page phase failed"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_withholds_after_response_lookups_on_a_tombstoned_row() {
        // Arrange: a consented navigation reads a withdrawn row.
        let settings = create_test_settings();
        let ec_id = sample_ec_id("after4");
        let graph = KvIdentityGraph::in_memory("test_store");
        graph
            .create(&ec_id, &KvEntry::tombstone(1000))
            .await
            .expect("should seed the tombstone");
        let mut ec_context =
            returning_user_context(&ec_id, graph.load_snapshot(&ec_id).await, true);
        let mut response = empty_response();

        // Act
        ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &PartnerRegistry::empty(),
            None,
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        // Assert
        assert!(
            ec_context.take_after_response_permit().is_none(),
            "should not allow after-response lookups for a withdrawn row"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finalize_adopted_row_keeps_a_concurrent_provider_opt_out() {
        // Arrange: the preload missed the row; meanwhile another request
        // recorded provider P's opt-out on it, and P's lookup found a UID.
        let settings = create_test_settings();
        let ec_id = sample_ec_id("optout");
        let graph = KvIdentityGraph::in_memory("test_store");
        let provider = ProviderId::new("example-provider");
        let mut live = live_entry();
        live.opt_outs
            .insert(provider.clone(), current_timestamp() + 86_400);
        graph
            .create(&ec_id, &live)
            .await
            .expect("should seed the row");
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
            &SharedIdRequest::default(),
            &mut response,
        )
        .await;

        // Assert
        let (entry, _) = graph
            .get(&ec_id)
            .await
            .expect("should read row")
            .expect("should keep row");
        assert!(
            !entry.ids.contains_key("provider.example.com"),
            "should not restore the opted-out provider's UID"
        );
    }
}
