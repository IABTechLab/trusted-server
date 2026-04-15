//! EC response finalization.
//!
//! Centralizes post-routing EC behavior so all handlers get consistent cookie
//! and KV semantics.

use std::collections::HashSet;

use fastly::Response;

use super::consent::ec_consent_granted;
use crate::constants::HEADER_X_TS_EC;
use crate::settings::Settings;

use super::cookies::{expire_ec_cookie, set_ec_cookie};
use super::current_timestamp;
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

    // Withdrawal path: no consent + cookie was present.
    if !consent_allows_ec && ec_context.cookie_was_present() {
        clear_ec_on_response(settings, response);

        // Compute once — used for both consent store cleanup and tombstoning.
        let ids_to_withdraw = withdrawal_ec_ids(ec_context);

        if let Some(store_name) = settings.consent.consent_store.as_deref() {
            if let Ok(store) = fastly::kv_store::KVStore::open(store_name) {
                if let Some(store) = store {
                    for ec_id in &ids_to_withdraw {
                        if let Err(err) = store.delete(ec_id) {
                            log::warn!(
                                "Failed to delete consent KV entry for '{}': {err:?}",
                                log_id(ec_id)
                            );
                        } else {
                            log::info!(
                                "Deleted consent KV entry for '{}' (consent revoked)",
                                log_id(ec_id)
                            );
                        }
                    }
                }
            } else {
                log::warn!("Failed to open consent store '{store_name}' for withdrawal cleanup");
            }
        }

        if let Some(graph) = kv {
            for ec_id in &ids_to_withdraw {
                if let Err(err) = graph.write_withdrawal_tombstone(ec_id) {
                    log::error!(
                        "Failed to write withdrawal tombstone for EC ID '{}': {err:?}",
                        log_id(ec_id),
                    );
                }
            }
        }

        return;
    }

    // Returning user: consent is granted and EC came from request.
    if ec_context.ec_was_present() && !ec_context.ec_generated() && consent_allows_ec {
        if let (Some(graph), Some(ec_id)) = (kv, ec_context.ec_value()) {
            if let Err(err) =
                graph.update_last_seen(ec_id, current_timestamp(), &settings.publisher.domain)
            {
                log::error!(
                    "Failed to update last_seen for EC ID '{}': {err:?}",
                    log_id(ec_id)
                );
            }

            // Ingest Prebid EIDs from cookie if present.
            if let Some(cookie) = eids_cookie {
                ingest_prebid_eids(cookie, ec_id, graph, registry);
            }
            if let Some(cookie) = sharedid_cookie {
                ingest_sharedid_cookie(cookie, ec_id, graph, registry);
            }
        }

        // Always set the EC header and refresh the cookie so downstream
        // consumers (Prebid, frontend JS) can read it on every response.
        set_ec_on_response(settings, ec_context, response);

        return;
    }

    // Newly generated EC in this request.
    if ec_context.ec_generated() {
        if let (Some(graph), Some(ec_id)) = (kv, ec_context.ec_value()) {
            if let Some(cookie) = eids_cookie {
                ingest_prebid_eids(cookie, ec_id, graph, registry);
            }
            if let Some(cookie) = sharedid_cookie {
                ingest_sharedid_cookie(cookie, ec_id, graph, registry);
            }
        }
        set_ec_on_response(settings, ec_context, response);
    }
}

/// Sets EC header + cookie on response when an EC ID is available.
pub fn set_ec_on_response(settings: &Settings, ec_context: &EcContext, response: &mut Response) {
    if let Some(ec_id) = ec_context.ec_value() {
        response.set_header(HEADER_X_TS_EC, ec_id);
        set_ec_cookie(settings, response, ec_id);
    }
}

/// Clears EC cookie and removes EC-specific response headers.
///
/// In addition to the fixed [`EC_RESPONSE_HEADERS`], this also strips any
/// dynamic `X-ts-<partner_id>` headers (matching the `x-ts-` prefix) to
/// prevent leaking EC identity data when consent is withdrawn.
pub fn clear_ec_on_response(settings: &Settings, response: &mut Response) {
    expire_ec_cookie(settings, response);

    for header in EC_RESPONSE_HEADERS {
        response.remove_header(*header);
    }

    // Strip any dynamic x-ts-<partner_id> headers set by /identify or
    // earlier processing. Collect names first to avoid borrow conflict.
    let dynamic_ts_headers: Vec<String> = response
        .get_header_names()
        .filter_map(|name| {
            let s = name.as_str();
            if s.starts_with("x-ts-") {
                Some(s.to_owned())
            } else {
                None
            }
        })
        .collect();

    for header in &dynamic_ts_headers {
        response.remove_header(header.as_str());
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::ConsentSource;
    use crate::test_support::tests::create_test_settings;

    fn make_context(
        ec_value: Option<&str>,
        cookie_ec_value: Option<&str>,
        ec_was_present: bool,
        ec_generated: bool,
        jurisdiction: Jurisdiction,
    ) -> EcContext {
        let consent = crate::consent::types::ConsentContext {
            jurisdiction,
            source: ConsentSource::Cookie,
            ..Default::default()
        };

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

    #[test]
    fn clear_ec_on_response_removes_headers_and_expires_cookie() {
        let settings = create_test_settings();
        let mut response = Response::new();
        response.set_header("x-ts-ec", "abc");
        response.set_header("x-ts-eids", "[]");
        // Dynamic partner headers that should also be stripped
        response.set_header("x-ts-ssp_x", "partner-uid-123");
        response.set_header("x-ts-liveramp", "lr-uid-456");

        clear_ec_on_response(&settings, &mut response);

        assert!(
            response.get_header("x-ts-ec").is_none(),
            "should remove x-ts-ec"
        );
        assert!(
            response.get_header("x-ts-eids").is_none(),
            "should remove x-ts-eids"
        );
        assert!(
            response.get_header("x-ts-ssp_x").is_none(),
            "should remove dynamic x-ts-<partner_id> headers"
        );
        assert!(
            response.get_header("x-ts-liveramp").is_none(),
            "should remove dynamic x-ts-<partner_id> headers"
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
        let ec_context = make_context(
            Some(&ec_id),
            Some(&ec_id),
            true,
            false,
            Jurisdiction::Unknown,
        );
        let mut response = Response::new();
        response.set_header("x-ts-ec", "stale");
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
            "withdrawal should clear x-ts-ec header"
        );
        assert!(
            response.get_header("x-ts-eids").is_none(),
            "withdrawal should clear x-ts-eids header"
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
    fn finalize_returning_user_with_cookie_mismatch_rewrites_cookie_and_header() {
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

        let header = response
            .get_header("x-ts-ec")
            .expect("mismatch should set x-ts-ec")
            .to_str()
            .expect("x-ts-ec should be utf-8");
        assert_eq!(header, active_ec, "should set active EC on header");

        let set_cookie = response
            .get_header("set-cookie")
            .expect("mismatch should rewrite cookie")
            .to_str()
            .expect("set-cookie should be utf-8");
        assert!(
            set_cookie.contains(&format!("ts-ec={active_ec}")),
            "cookie should be rewritten to active EC"
        );
    }

    #[test]
    fn finalize_returning_user_refreshes_cookie_and_header_when_matching() {
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

        let header = response
            .get_header("x-ts-ec")
            .expect("returning user should set x-ts-ec")
            .to_str()
            .expect("x-ts-ec should be utf-8");
        assert_eq!(header, ec_id, "header should contain active EC");

        let set_cookie = response
            .get_header("set-cookie")
            .expect("returning user should refresh cookie")
            .to_str()
            .expect("set-cookie should be utf-8");
        assert!(
            set_cookie.contains(&format!("ts-ec={ec_id}")),
            "cookie should be refreshed to active EC"
        );
    }

    #[test]
    fn finalize_generated_ec_sets_cookie_and_header() {
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

        let header = response
            .get_header("x-ts-ec")
            .expect("generated EC should set response header")
            .to_str()
            .expect("x-ts-ec should be utf-8");
        assert_eq!(header, generated_ec, "header should contain generated EC");

        assert!(
            response.get_header("set-cookie").is_some(),
            "generated EC should set cookie"
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
}
