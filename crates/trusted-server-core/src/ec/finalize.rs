//! EC response finalization.
//!
//! Centralizes post-routing EC behavior so all handlers get consistent cookie
//! and KV semantics.

use std::collections::HashSet;

use fastly::Response;

use crate::consent::allows_ec_creation;
use crate::consent::kv::delete_consent_from_kv;
use crate::constants::HEADER_X_TS_EC;
use crate::geo::GeoInfo;
use crate::settings::Settings;

use super::cookies::{expire_ec_cookie, set_ec_cookie};
use super::generation::{ec_hash, is_valid_ec_id};
use super::kv::KvIdentityGraph;
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
/// and cookie writes for new EC generation.
pub fn ec_finalize_response(
    settings: &Settings,
    _geo_info: Option<&GeoInfo>, // reserved for future route-specific finalize behavior
    ec_context: &EcContext,
    kv: Option<&KvIdentityGraph>,
    response: &mut Response,
) {
    let consent_allows_ec = allows_ec_creation(ec_context.consent());

    // Withdrawal path: no consent + cookie was present.
    if !consent_allows_ec && ec_context.cookie_was_present() {
        clear_ec_on_response(settings, response);

        if let Some(store_name) = settings.consent.consent_store.as_deref() {
            for ec_id in withdrawal_ec_ids(ec_context) {
                delete_consent_from_kv(store_name, &ec_id);
            }
        }

        if let Some(graph) = kv {
            for hash in withdrawal_hashes(ec_context) {
                if let Err(err) = graph.write_withdrawal_tombstone(&hash) {
                    log::error!(
                        "Failed to write withdrawal tombstone for hash '{}': {err:?}",
                        hash,
                    );
                }
            }
        }

        return;
    }

    // Returning user: consent is granted and EC came from request.
    if ec_context.ec_was_present() && !ec_context.ec_generated() && consent_allows_ec {
        if let (Some(graph), Some(ec_id)) = (kv, ec_context.ec_value()) {
            let hash = ec_hash(ec_id);
            if is_valid_ec_hash(hash) {
                if let Err(err) = graph.update_last_seen(hash, current_timestamp()) {
                    log::error!("Failed to update last_seen for hash '{}': {err:?}", hash,);
                }
            }
        }

        // If header/cookie were mismatched, rewrite cookie to active EC value.
        if ec_context.has_cookie_mismatch() {
            set_ec_on_response(settings, ec_context, response);
        }

        return;
    }

    // Newly generated EC in this request.
    if ec_context.ec_generated() {
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
pub fn clear_ec_on_response(settings: &Settings, response: &mut Response) {
    expire_ec_cookie(settings, response);

    for header in EC_RESPONSE_HEADERS {
        response.remove_header(*header);
    }
}

fn withdrawal_hashes(ec_context: &EcContext) -> HashSet<String> {
    withdrawal_ec_ids(ec_context)
        .into_iter()
        .map(|ec_id| ec_hash(&ec_id).to_owned())
        .collect()
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

fn is_valid_ec_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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

        EcContext {
            ec_value: ec_value.map(str::to_owned),
            cookie_ec_value: cookie_ec_value.map(str::to_owned),
            ec_was_present,
            ec_generated,
            consent,
            client_ip: None,
            geo_info: None,
        }
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

        clear_ec_on_response(&settings, &mut response);

        assert!(
            response.get_header("x-ts-ec").is_none(),
            "should remove x-ts-ec"
        );
        assert!(
            response.get_header("x-ts-eids").is_none(),
            "should remove x-ts-eids"
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
        let ec_id = sample_ec_id("ABC123");
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

        ec_finalize_response(&settings, None, &ec_context, None, &mut response);

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
        let active_ec = sample_ec_id("ACTIVE1");
        let cookie_ec = sample_ec_id("COOKIE1");
        let ec_context = make_context(
            Some(&active_ec),
            Some(&cookie_ec),
            true,
            false,
            Jurisdiction::NonRegulated,
        );
        let mut response = Response::new();

        ec_finalize_response(&settings, None, &ec_context, None, &mut response);

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
    fn finalize_generated_ec_sets_cookie_and_header() {
        let settings = create_test_settings();
        let generated_ec = sample_ec_id("GEN123");
        let ec_context = make_context(
            Some(&generated_ec),
            None,
            false,
            true,
            Jurisdiction::NonRegulated,
        );
        let mut response = Response::new();

        ec_finalize_response(&settings, None, &ec_context, None, &mut response);

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

        ec_finalize_response(&settings, None, &ec_context, None, &mut response);

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
