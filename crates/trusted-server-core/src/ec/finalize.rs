//! EC response finalization.
//!
//! Centralizes post-routing EC behavior so all handlers get consistent cookie
//! and KV semantics.

use std::collections::HashSet;

use edgezero_core::body::Body as EdgeBody;
use http::Response;

use super::consent::{ec_consent_granted, ec_consent_withdrawn};
use crate::settings::Settings;

use super::cookies::{expire_ec_cookie, set_ec_cookie};
use super::generation::{generate_ec_id, is_valid_ec_id};
use super::kv::{apply_partner_id_updates, CreateIfAbsentOutcome, KvIdentityGraph};
use super::kv_types::KvEntry;
use super::prebid_eids::collect_eid_cookie_updates;
use super::registry::PartnerRegistry;
use super::{current_timestamp, EcContext, EcKvSnapshot};

/// TS-managed response headers tied to EC identity output.
const EC_RESPONSE_HEADERS: &[&str] = &[
    "x-ts-ec",
    "x-ts-eids",
    "x-ts-ec-consent",
    "x-ts-eids-truncated",
];

/// Finalizes EC response behavior for all routes.
///
/// Applies withdrawal handling, last-seen updates, cookie reconciliation,
/// Prebid EID ingestion, and cookie writes for new EC generation.
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
    let consent_allows_ec = ec_consent_granted(ec_context.consent());
    let consent_withdrawn = ec_consent_withdrawn(ec_context.consent());

    if !consent_allows_ec {
        // Always strip EC-specific response headers when consent is not
        // currently usable for this request. This covers both explicit
        // revocation and fail-closed cases such as missing geo or undecodable
        // consent input.
        clear_ec_headers_on_response(response, Some(registry));

        // Only expire the browser cookie and tombstone the identity-graph row
        // when the request carries an explicit withdrawal signal.
        if consent_withdrawn && ec_context.cookie_was_present() {
            expire_ec_cookie(settings, response);

            // Compute once for the authoritative identity-graph tombstones.
            let ids_to_withdraw = withdrawal_ec_ids(ec_context);

            // The identity-graph tombstone is the authoritative withdrawal marker
            // for subsequent EC behavior.
            if let Some(graph) = kv {
                apply_withdrawal_tombstones(&ids_to_withdraw, |ec_id| {
                    let initial = if ec_context.kv_snapshot().belongs_to(ec_id) {
                        ec_context.kv_snapshot().clone()
                    } else {
                        EcKvSnapshot::NotRead
                    };
                    let outcome = graph.tombstone_existing_from_snapshot(ec_id, initial);
                    if ec_context.ec_value() == Some(ec_id) {
                        ec_context.set_kv_snapshot(outcome);
                    }
                });
            }
        }

        return;
    }

    // Returning user: consent is granted and EC came from request.
    if ec_context.ec_was_present() && !ec_context.ec_generated() && consent_allows_ec {
        if let (Some(graph), Some(ec_id)) = (kv, ec_context.ec_value().map(str::to_owned)) {
            let updates = collect_eid_cookie_updates(eids_cookie, sharedid_cookie, registry);
            let snapshot = graph.upsert_partner_ids_from_snapshot(
                &ec_id,
                &updates,
                ec_context.kv_snapshot().clone(),
            );
            ec_context.set_kv_snapshot(snapshot);
            if matches!(ec_context.kv_snapshot(), EcKvSnapshot::Missing { .. })
                && ec_context.recovery_eligible()
            {
                confirm_then_recover_orphaned_ec(
                    settings, ec_context, graph, &ec_id, &updates, response,
                );
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
        let (Some(graph), Some(ec_id)) = (kv, ec_context.ec_value().map(str::to_owned)) else {
            log::info!("Skipping generated EC response write because KV graph is unavailable");
            return;
        };

        let updates = collect_eid_cookie_updates(eids_cookie, sharedid_cookie, registry);
        let snapshot = graph.upsert_partner_ids_from_snapshot(
            &ec_id,
            &updates,
            ec_context.kv_snapshot().clone(),
        );
        ec_context.set_kv_snapshot(snapshot);
        if ec_context.kv_snapshot().entry_for(&ec_id).is_some() {
            set_ec_cookie_on_response(settings, ec_context, response);
        } else {
            log::warn!("Skipping generated EC cookie because backing row is not authoritative");
        }
    }
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
    let orphan_id = ec_context.ec_value().unwrap_or_default().to_owned();
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

/// Confirms an orphaned cookie is genuinely absent before rotating it.
///
/// The origin-overlapped preload reads the identity-graph row while the
/// publisher origin is still in flight. Fastly edge data stores are eventually
/// consistent, so a recently created live key can transiently read `Missing` at
/// one POP. Before rotating a year-lived identity, this performs one more
/// authoritative read — separated from the preload by the full origin round
/// trip, which gives replication time to converge:
///
/// - a now-visible row is adopted, with any pending updates merged, and is
///   never rotated;
/// - a confirmed authoritative miss rotates through [`recover_orphaned_ec`];
/// - a read failure is not a miss and never rotates.
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
            // merge any pending updates rather than rotating a valid identity.
            let merged = graph.upsert_partner_ids_from_snapshot(ec_id, updates, confirmed);
            ec_context.set_kv_snapshot(merged);
        }
        EcKvSnapshot::Missing { .. } => {
            recover_orphaned_ec(settings, ec_context, graph, updates, response);
        }
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

fn withdrawal_ec_ids(ec_context: &EcContext) -> HashSet<String> {
    let mut hashes = HashSet::new();

    if let Some(cookie_ec_id) = ec_context.existing_cookie_ec_id() {
        if is_valid_ec_id(cookie_ec_id) {
            hashes.insert(cookie_ec_id.to_owned());
        }
    }

    if let Some(active_ec_id) = ec_context.ec_value() {
        if is_valid_ec_id(active_ec_id) {
            hashes.insert(active_ec_id.to_owned());
        }
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

    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};
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
            api_token: Redacted::new(format!("token-{source_domain}-32-bytes-minimum-value")),
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
            "the present active ID should be tombstoned via its carried snapshot"
        );
        assert!(
            graph.get(&cookie_ec).expect("should read store").is_none(),
            "a missing second ID must never be created by withdrawal"
        );
    }

    fn live_entry() -> KvEntry {
        let mut entry = KvEntry::tombstone(1000);
        entry.consent.ok = true;
        entry
    }
}
