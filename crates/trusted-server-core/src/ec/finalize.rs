//! EC response finalization.
//!
//! Centralizes post-routing EC behavior so all handlers get consistent cookie
//! and KV semantics.

use std::collections::HashSet;

use fastly::Response;

use super::consent::{ec_consent_granted, ec_consent_withdrawn};
use crate::settings::Settings;

use super::cookies::{expire_ec_cookie, set_ec_cookie};
use super::generation::is_valid_ec_id;
use super::kv::KvIdentityGraph;
use super::log_id;
use super::prebid_eids::{ingest_prebid_eids, ingest_sharedid_cookie};
use super::registry::PartnerRegistry;
use super::EcContext;

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
    ec_context: &EcContext,
    kv: Option<&KvIdentityGraph>,
    registry: &PartnerRegistry,
    eids_cookie: Option<&str>,
    sharedid_cookie: Option<&str>,
    response: &mut Response,
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
                    if let Err(err) = graph.write_withdrawal_tombstone(ec_id) {
                        log::error!(
                            "Failed to write withdrawal tombstone for EC ID '{}': {err:?}",
                            log_id(ec_id),
                        );
                    }
                });
            }
        }

        return;
    }

    // Returning user: consent is granted and EC came from request.
    if ec_context.ec_was_present() && !ec_context.ec_generated() && consent_allows_ec {
        if let (Some(graph), Some(ec_id)) = (kv, ec_context.ec_value()) {
            // Ingest Prebid EIDs from cookie if present.
            if let Some(cookie) = eids_cookie {
                ingest_prebid_eids(cookie, ec_id, graph, registry);
            }
            if let Some(cookie) = sharedid_cookie {
                ingest_sharedid_cookie(cookie, ec_id, graph, registry);
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
        let (Some(graph), Some(ec_id)) = (kv, ec_context.ec_value()) else {
            log::info!("Skipping generated EC response write because KV graph is unavailable");
            return;
        };

        if let Some(cookie) = eids_cookie {
            ingest_prebid_eids(cookie, ec_id, graph, registry);
        }
        if let Some(cookie) = sharedid_cookie {
            ingest_sharedid_cookie(cookie, ec_id, graph, registry);
        }
        set_ec_cookie_on_response(settings, ec_context, response);
    }
}

/// Sets the EC cookie on response when an EC ID is available.
pub fn set_ec_cookie_on_response(
    settings: &Settings,
    ec_context: &EcContext,
    response: &mut Response,
) {
    if let Some(ec_id) = ec_context.ec_value() {
        set_ec_cookie(settings, response, ec_id);
    }
}

/// Removes EC-specific response headers.
///
/// In addition to the fixed [`EC_RESPONSE_HEADERS`], this also strips dynamic
/// `X-ts-<partner_id>` headers for registered partners. Other `x-ts-*` headers
/// are intentionally preserved because they may be set by non-EC middleware.
fn clear_ec_headers_on_response(response: &mut Response, registry: Option<&PartnerRegistry>) {
    for header in EC_RESPONSE_HEADERS {
        response.remove_header(*header);
    }

    if let Some(registry) = registry {
        for partner in registry.all() {
            response.remove_header(partner_response_header(&partner.id).as_str());
        }
    }
}

fn partner_response_header(partner_id: &str) -> String {
    format!("x-ts-{partner_id}")
}

/// Clears EC cookie and removes EC-specific response headers.
///
/// Used when the request carries an explicit withdrawal signal.
pub fn clear_ec_on_response(settings: &Settings, response: &mut Response) {
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
    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};
    use crate::redacted::Redacted;
    use crate::settings::EcPartner;
    use crate::test_support::tests::create_test_settings;

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

    fn make_partner(id: &str) -> EcPartner {
        EcPartner {
            id: id.to_owned(),
            name: format!("Partner {id}"),
            source_domain: format!("{id}.example.com"),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: Redacted::new(format!("token-{id}-32-bytes-minimum-value")),
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
        let mut response = Response::new();
        response.set_header("x-ts-ec", "abc");
        response.set_header("x-ts-eids", "[]");
        response.set_header("x-ts-unrelated", "keep-me");

        clear_ec_on_response(&settings, &mut response);

        assert!(
            response.get_header("x-ts-ec").is_none(),
            "should remove x-ts-ec"
        );
        assert!(
            response.get_header("x-ts-eids").is_none(),
            "should remove x-ts-eids"
        );
        assert_eq!(
            response.get_header_str("x-ts-unrelated"),
            Some("keep-me"),
            "should preserve unrelated x-ts headers without a partner registry"
        );

        let set_cookie = response
            .get_header("set-cookie")
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
        let ec_context =
            make_context_with_consent(Some(&ec_id), Some(&ec_id), true, false, consent);
        let mut response = Response::new();
        response.set_header("x-ts-ec", "stale");
        response.set_header("x-ts-eids", "[]");
        response.set_header("x-ts-ssp_x", "partner-uid-123");
        response.set_header("x-ts-unrelated", "keep-me");

        let partners = vec![make_partner("ssp_x")];
        let test_registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        ec_finalize_response(
            &settings,
            &ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            response.get_header("x-ts-ec").is_none(),
            "withdrawal should clear x-ts-ec header"
        );
        assert!(
            response.get_header("x-ts-eids").is_none(),
            "withdrawal should clear x-ts-eids header"
        );
        assert!(
            response.get_header("x-ts-ssp_x").is_none(),
            "withdrawal should clear registered partner header"
        );
        assert_eq!(
            response.get_header_str("x-ts-unrelated"),
            Some("keep-me"),
            "withdrawal should preserve unrelated x-ts header"
        );
        let set_cookie = response
            .get_header("set-cookie")
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
        let ec_context = make_context(
            Some(&active_ec),
            Some(&cookie_ec),
            true,
            false,
            Jurisdiction::NonRegulated,
        );
        let mut response = Response::new();

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            response.get_header("x-ts-ec").is_none(),
            "returning user should not set x-ts-ec"
        );
        assert!(
            response.get_header("set-cookie").is_none(),
            "returning user should not refresh or repair cookie"
        );
    }

    #[test]
    fn finalize_returning_user_sets_no_header_or_cookie() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("mtch01");
        let ec_context = make_context(
            Some(&ec_id),
            Some(&ec_id),
            true,
            false,
            Jurisdiction::NonRegulated,
        );
        let mut response = Response::new();

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            response.get_header("x-ts-ec").is_none(),
            "returning user should not set x-ts-ec"
        );
        assert!(
            response.get_header("set-cookie").is_none(),
            "returning user should not refresh cookie"
        );
    }

    #[test]
    fn finalize_generated_ec_without_kv_skips_cookie_and_header() {
        let settings = create_test_settings();
        let generated_ec = sample_ec_id("gen123");
        let ec_context = make_context(
            Some(&generated_ec),
            None,
            false,
            true,
            Jurisdiction::NonRegulated,
        );
        let mut response = Response::new();

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            response.get_header("x-ts-ec").is_none(),
            "generated EC without KV should not set response header"
        );
        assert!(
            response.get_header("set-cookie").is_none(),
            "generated EC without KV should not set cookie"
        );
    }

    #[test]
    fn finalize_denied_without_cookie_is_noop() {
        let settings = create_test_settings();
        let ec_context = make_context(None, None, false, false, Jurisdiction::Unknown);
        let mut response = Response::new();

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            response.get_header("x-ts-ec").is_none(),
            "should not set EC header"
        );
        assert!(
            response.get_header("set-cookie").is_none(),
            "should not mutate cookie when there is nothing to revoke"
        );
    }

    #[test]
    fn finalize_unknown_jurisdiction_strips_headers_without_expiring_cookie() {
        let settings = create_test_settings();
        let ec_id = sample_ec_id("unk001");
        let ec_context = make_context(
            Some(&ec_id),
            Some(&ec_id),
            true,
            false,
            Jurisdiction::Unknown,
        );
        let mut response = Response::new();
        response.set_header("x-ts-ec", &ec_id);
        response.set_header("x-ts-eids", "[]");

        let test_registry = PartnerRegistry::empty();
        ec_finalize_response(
            &settings,
            &ec_context,
            None,
            &test_registry,
            None,
            None,
            &mut response,
        );

        assert!(
            response.get_header("x-ts-ec").is_none(),
            "should strip EC header when consent cannot be verified"
        );
        assert!(
            response.get_header("x-ts-eids").is_none(),
            "should strip EID header when consent cannot be verified"
        );
        assert!(
            response.get_header("set-cookie").is_none(),
            "should not expire the cookie without an explicit withdrawal signal"
        );
    }
}
