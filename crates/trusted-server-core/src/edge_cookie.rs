//! Edge Cookie (EC) ID generation using HMAC.
//!
//! This module provides functionality for generating privacy-preserving EC IDs
//! based on the client IP address and a secret key.

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::Request;

use crate::constants::{COOKIE_TS_EC, HEADER_X_TS_EC};
use crate::cookies::handle_request_cookies;
use crate::ec::cookies::ec_id_has_only_allowed_chars;
#[cfg(test)]
use crate::ec::generation::normalize_ip;
#[cfg(test)]
use crate::ec::provider::IdentityInput;
use crate::ec::provider::{provider_owns_id, request_provider};
use crate::error::TrustedServerError;
#[cfg(test)]
use crate::evidence::BorrowedRequestInfo;
use crate::platform::RuntimeServices;
use crate::settings::Settings;

/// Generates a fresh EC ID using the configured Edge Cookie provider.
///
/// Routes through the pluggable provider model: the active `[ec] provider`
/// selection decides the outcome. Returns `Ok(None)` when no provider is
/// configured, so Trusted Server runs statelessly and creates no Edge Cookie.
/// `request_headers` lets a provider that derives identity from request
/// evidence read it; the built-in HMAC provider ignores it and uses only the
/// normalized client IP.
///
/// # Errors
///
/// - [`TrustedServerError::EdgeCookie`] if provider generation fails
///
/// Currently exercised only by tests: the production EC lifecycle generates IDs
/// through [`crate::ec`]/`EcContext` rather than this edge-cookie helper.
#[cfg(test)]
pub async fn generate_ec_id(
    settings: &Settings,
    services: &RuntimeServices,
    request_headers: Option<&http::HeaderMap>,
) -> Result<Option<String>, Report<TrustedServerError>> {
    // Fall back to "unknown" when the client IP is unavailable (for example in
    // local testing). All such requests share the same HMAC base; the random
    // suffix provides uniqueness.
    let client_ip = services
        .client_info()
        .client_ip
        .map(normalize_ip)
        .unwrap_or_else(|| "unknown".to_string());

    log::trace!("Generating fresh EC ID from normalized client context");

    let Some(provider) = request_provider(&settings.ec, services)? else {
        log::info!("No Edge Cookie provider configured; running statelessly");
        return Ok(None);
    };

    // The provider reads request data (the client IP and the request headers)
    // borrowed at call time, so nothing is cloned. A provider that also reads
    // host signals takes them from the injected `HostSignals` service rather
    // than from this request info.
    let request_info = BorrowedRequestInfo::new(&client_ip, request_headers);
    // The publisher path applies the permission gate at the call site, and the
    // built-in provider reads neither the resolved permissions nor consent, so
    // they are not threaded here.
    let generated = provider
        .generate(&request_info, &IdentityInput::default(), services)
        .await?;
    let generated = crate::ec::provider::GeneratedEdgeCookie {
        id: generated
            .id
            .map(|value| crate::ec::provider::apply_provider_code(provider.as_ref(), &value)),
        response_headers: generated.response_headers,
    };
    Ok(generated.id)
}

/// Reads whatever the request offers as an Edge Cookie identifier, before any
/// check that this deployment could have issued it.
///
/// Reads the `x-ts-ec` header first and then the `ts-ec` cookie. Both are
/// client-controlled. `x-ts-ec` is stripped from responses but is not stripped
/// from inbound requests, so a caller must treat the result as an attacker's
/// choice of string.
///
/// The only checks applied here are the global cookie bounds, the length cap
/// and the cookie-safe alphabet in
/// [`ec_id_has_only_allowed_chars`](crate::ec::cookies::ec_id_has_only_allowed_chars),
/// which every identifier must satisfy whichever provider created it. Those
/// bounds are a backstop on what may travel in a cookie, not a test of
/// authenticity, and on their own they accept any run of `[A-Za-z0-9._~-]`.
///
/// Deciding whether this deployment issued the value needs the selected
/// provider, which this function does not have, so it is deliberately not
/// public. Use [`recognized_ec_id`], which applies provider ownership on top.
///
/// # Errors
///
/// - [`TrustedServerError::InvalidHeaderValue`] if cookie parsing fails
pub(crate) fn unvalidated_ec_id_from_request(
    req: &Request<EdgeBody>,
) -> Result<Option<String>, Report<TrustedServerError>> {
    if let Some(ec_id) = req
        .headers()
        .get(HEADER_X_TS_EC)
        .and_then(|h| h.to_str().ok())
    {
        if ec_id_has_only_allowed_chars(ec_id) {
            log::trace!("Using existing EC ID from header");
            return Ok(Some(ec_id.to_string()));
        }
        log::warn!("Rejected EC ID from x-ts-ec header with disallowed characters");
    }

    match handle_request_cookies(req)? {
        Some(jar) => {
            if let Some(cookie) = jar.get(COOKIE_TS_EC) {
                let value = cookie.value();
                if ec_id_has_only_allowed_chars(value) {
                    log::trace!("Using existing EC ID from cookie");
                    return Ok(Some(value.to_string()));
                }
                log::warn!("Rejected EC ID from cookie with disallowed characters");
            }
        }
        None => {
            log::debug!("No cookie header found in request");
        }
    }

    Ok(None)
}

/// Gets an existing EC ID from the request, but only one the deployment's
/// selected provider recognizes.
///
/// [`unvalidated_ec_id_from_request`] applies the global cookie bounds alone,
/// the length cap and the cookie-safe alphabet, which any value a browser can
/// be persuaded to carry will pass. This adds provider ownership on top, so the
/// identifier's `{code}~` prefix is dispatched to the provider that owns it,
/// which decides whether the value is one of its own. That is the same test the
/// EC lifecycle applies when it reads the cookie back, so both agree on what
/// this deployment issued.
///
/// This is the validated way to read an inbound identifier from outside this
/// module, because the bounds alone cannot tell an identifier this deployment
/// issued from one an attacker typed. The module also exposes
/// [`get_or_generate_ec_id`], which is `pub` and returns the raw cookie value
/// without this ownership check, so prefer this function wherever the identifier
/// will be trusted or egressed. A vendor identifier is not required to match the
/// built-in HMAC shape, so the right test is the selected provider's own
/// [`accepts_id`](crate::ec::provider::EdgeCookieProvider::accepts_id) rather
/// than the built-in strict format check.
///
/// Returns `None` for a value carrying another deployment's provider code, for a
/// value the selected provider does not recognize, and for every value at all
/// when no provider is selected, because a stateless deployment issues no
/// identifier and so has none to hand on.
///
/// Use this wherever the identifier leaves the edge (an outbound origin URL, a
/// click target, a proxied request body), so nothing is egressed that this
/// deployment did not issue.
///
/// # Errors
///
/// - [`TrustedServerError::InvalidHeaderValue`] if cookie parsing fails
/// - [`TrustedServerError::EdgeCookie`] if the selected provider cannot be built
pub fn recognized_ec_id(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request<EdgeBody>,
) -> Result<Option<String>, Report<TrustedServerError>> {
    let Some(ec_id) = unvalidated_ec_id_from_request(req)? else {
        return Ok(None);
    };

    let Some(provider) = request_provider(&settings.ec, services)? else {
        log::debug!(
            "No Edge Cookie provider configured; withholding the request's EC ID from egress"
        );
        return Ok(None);
    };

    if provider_owns_id(provider.as_ref(), &ec_id) {
        return Ok(Some(ec_id));
    }

    log::debug!(
        "Withholding an EC ID provider `{}` does not recognize from egress",
        provider.id(),
    );
    Ok(None)
}

/// Gets or creates an EC ID from the request.
///
/// Attempts to retrieve an existing EC ID from:
/// 1. The `x-ts-ec` header
/// 2. The `ts-ec` cookie
///
/// If neither exists, generates a new EC ID via the configured provider.
///
/// Returns `Ok(None)` when no existing EC ID is present and no Edge Cookie
/// provider is configured, so the caller proceeds statelessly.
///
/// # Errors
///
/// Returns an error if ID generation fails.
#[cfg(test)]
pub(crate) async fn get_or_generate_ec_id_from_http_request(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request<EdgeBody>,
) -> Result<Option<String>, Report<TrustedServerError>> {
    if let Some(id) = unvalidated_ec_id_from_request(req)? {
        return Ok(Some(id));
    }

    // If no existing EC ID found, generate a fresh one through the provider.
    let ec_id = generate_ec_id(settings, services, Some(req.headers())).await?;
    if ec_id.is_some() {
        log::trace!("No existing EC ID found; generated a fresh EC ID");
    }
    Ok(ec_id)
}

/// Gets or creates an EC ID from the request.
///
/// # Errors
///
/// Returns an error if ID generation fails.
#[cfg(test)]
pub async fn get_or_generate_ec_id(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request<EdgeBody>,
) -> Result<Option<String>, Report<TrustedServerError>> {
    get_or_generate_ec_id_from_http_request(settings, services, req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use edgezero_core::body::Body as EdgeBody;
    use http::{HeaderName, header};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use crate::ec::generation::generate_ec_id as generate_canonical_ec_id;
    use crate::platform::test_support::{noop_services, noop_services_with_client_ip};
    use crate::test_support::tests::create_test_settings;

    #[tokio::test]
    async fn test_generate_ec_id_matches_canonical_generator_for_ipv6() {
        // Regression guard: this module must hash the same normalized IP as
        // the canonical generator in ec::generation. A divergent IPv6 /64
        // normalization would mint non-correlating identity prefixes for the
        // same client depending on which path generated the ID.
        let settings = create_test_settings();
        let ip = IpAddr::V6(Ipv6Addr::new(
            0x2001, 0x0db8, 0x85a3, 0x0000, 0x8a2e, 0x0370, 0x7334, 0x1234,
        ));

        let id_here = generate_ec_id(&settings, &noop_services_with_client_ip(ip), None)
            .await
            .expect("should generate EC ID via edge_cookie")
            .expect("should configure the hmac provider in test settings");
        let passphrase = settings
            .ec
            .providers
            .hmac
            .as_ref()
            .map(|hmac| hmac.passphrase.expose().as_str())
            .unwrap_or("");
        let id_canonical = generate_canonical_ec_id(passphrase, &normalize_ip(ip))
            .expect("should generate EC ID via canonical generator");

        let bare_here = id_here
            .strip_prefix("hmac~")
            .expect("should carry the hmac provider code");
        assert_eq!(
            crate::ec::ec_hash(bare_here),
            crate::ec::ec_hash(&id_canonical),
            "should produce the same identity hash prefix as the canonical generator"
        );
    }

    fn create_test_request(headers: &[(HeaderName, &str)]) -> Request<EdgeBody> {
        let mut builder = Request::builder().method("GET").uri("http://example.com");
        for (key, value) in headers {
            builder = builder.header(key, *value);
        }
        builder
            .body(EdgeBody::empty())
            .expect("should build test request")
    }

    fn is_ec_id_format(value: &str) -> bool {
        // The coded envelope: hmac~<64hex>.<6alnum>.
        let Some(value) = value.strip_prefix("hmac~") else {
            return false;
        };
        let mut parts = value.split('.');
        let hmac_part = match parts.next() {
            Some(part) => part,
            None => return false,
        };
        let suffix_part = match parts.next() {
            Some(part) => part,
            None => return false,
        };
        if parts.next().is_some() {
            return false;
        }
        if hmac_part.len() != 64 || suffix_part.len() != 6 {
            return false;
        }
        if !hmac_part.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
        if !suffix_part.chars().all(|c| c.is_ascii_alphanumeric()) {
            return false;
        }
        true
    }

    #[tokio::test]
    async fn test_generate_ec_id() {
        let settings: Settings = create_test_settings();

        let ec_id = generate_ec_id(&settings, &noop_services(), None)
            .await
            .expect("should generate EC ID")
            .expect("should configure the hmac provider in test settings");
        log::debug!("Generated EC ID: {}", ec_id);
        assert!(
            is_ec_id_format(&ec_id),
            "should match the coded EC ID format: hmac~{{64hex}}.{{6alnum}}"
        );
    }

    #[tokio::test]
    async fn generate_ec_id_returns_none_when_no_provider_is_configured() {
        let mut settings = create_test_settings();
        // No provider selected: Trusted Server runs statelessly.
        settings.ec.provider = None;

        let id = generate_ec_id(&settings, &noop_services(), None)
            .await
            .expect("generation should not error when no provider is configured");
        assert!(
            id.is_none(),
            "no Edge Cookie provider should mean no Edge Cookie is created"
        );
    }

    #[tokio::test]
    async fn test_generate_ec_id_uses_client_ip() {
        let settings = create_test_settings();
        let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1));

        let id_with_ip = generate_ec_id(&settings, &noop_services_with_client_ip(ip), None)
            .await
            .expect("should generate EC ID with client IP")
            .expect("should configure the hmac provider in test settings");
        let id_without_ip = generate_ec_id(&settings, &noop_services(), None)
            .await
            .expect("should generate EC ID without client IP")
            .expect("should configure the hmac provider in test settings");

        let hmac_with_ip = id_with_ip.split_once('.').expect("should contain dot").0;
        let hmac_without_ip = id_without_ip.split_once('.').expect("should contain dot").0;

        assert_ne!(
            hmac_with_ip, hmac_without_ip,
            "should produce different HMAC when client IP differs"
        );
    }

    #[test]
    fn test_is_ec_id_format_accepts_valid_value() {
        let value = format!("hmac~{}.{}", "a".repeat(64), "Ab12z9");
        assert!(
            is_ec_id_format(&value),
            "should accept a valid coded EC ID format"
        );
    }

    #[test]
    fn test_is_ec_id_format_rejects_invalid_values() {
        let bare_legacy_shape = format!("{}.{}", "a".repeat(64), "Ab12z9");
        assert!(
            !is_ec_id_format(&bare_legacy_shape),
            "a freshly created identifier always carries the provider code"
        );

        let missing_suffix = format!("hmac~{}", "a".repeat(64));
        assert!(
            !is_ec_id_format(&missing_suffix),
            "should reject missing suffix"
        );

        let invalid_hex = format!("hmac~{}.{}", "a".repeat(63) + "g", "Ab12z9");
        assert!(
            !is_ec_id_format(&invalid_hex),
            "should reject non-hex HMAC content"
        );

        let invalid_suffix = format!("{}.{}", "a".repeat(64), "ab-129");
        assert!(
            !is_ec_id_format(&invalid_suffix),
            "should reject non-alphanumeric suffix"
        );

        let extra_segment = format!("{}.{}.{}", "a".repeat(64), "Ab12z9", "zz");
        assert!(
            !is_ec_id_format(&extra_segment),
            "should reject extra segments"
        );
    }

    #[test]
    fn an_identifier_this_deployment_never_issued_is_not_recognized() {
        // `x-ts-ec` is stripped from responses but not from inbound requests,
        // so a client can put whatever it likes in it, and the raw reader
        // prefers the header over the cookie. The global cookie bounds accept
        // any run of `[A-Za-z0-9._~-]`, so they cannot tell an identifier this
        // deployment created from one an attacker typed. Provider ownership is
        // what draws that line.
        let settings = create_test_settings();
        let services = noop_services();

        for forged in [
            // Passes the alphabet and the length cap, owned by nobody.
            "not-an-identifier",
            // The built-in shape under another deployment's provider code.
            "zz00~aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Ab1234",
            // This deployment's code carrying a value its provider never creates.
            "hmac~not-the-hmac-shape",
        ] {
            let req = create_test_request(&[(HEADER_X_TS_EC, forged)]);

            // The raw reader hands it straight back, which is exactly why it is
            // not the check anything may rely on.
            assert_eq!(
                unvalidated_ec_id_from_request(&req)
                    .expect("should read the header")
                    .as_deref(),
                Some(forged),
                "the global bounds alone should accept `{forged}`"
            );

            assert_eq!(
                recognized_ec_id(&settings, &services, &req)
                    .expect("should decide without erroring"),
                None,
                "`{forged}` was never issued here and must not be recognized"
            );
        }

        // A value the selected provider does own is still recognized, so the
        // check rejects forgeries rather than everything.
        let issued = format!("hmac~{}.Ab1234", "a".repeat(64));
        let req = create_test_request(&[(HEADER_X_TS_EC, issued.as_str())]);
        assert_eq!(
            recognized_ec_id(&settings, &services, &req)
                .expect("should decide without erroring")
                .as_deref(),
            Some(issued.as_str()),
            "an identifier the selected provider owns should still be recognized"
        );
    }

    #[tokio::test]
    async fn test_get_ec_id_with_header() {
        let settings = create_test_settings();
        let req = create_test_request(&[(HEADER_X_TS_EC, "existing_ec_id")]);

        let ec_id = unvalidated_ec_id_from_request(&req).expect("should get EC ID");
        assert_eq!(ec_id, Some("existing_ec_id".to_string()));

        let ec_id = get_or_generate_ec_id(&settings, &noop_services(), &req)
            .await
            .expect("should reuse header EC ID")
            .expect("an existing EC should be present");
        assert_eq!(ec_id, "existing_ec_id");
    }

    #[tokio::test]
    async fn test_get_ec_id_with_cookie() {
        let settings = create_test_settings();
        let req = create_test_request(&[(
            header::COOKIE,
            &format!("{}=existing_cookie_id", COOKIE_TS_EC),
        )]);

        let ec_id = unvalidated_ec_id_from_request(&req).expect("should get EC ID");
        assert_eq!(ec_id, Some("existing_cookie_id".to_string()));

        let ec_id = get_or_generate_ec_id(&settings, &noop_services(), &req)
            .await
            .expect("should reuse cookie EC ID")
            .expect("an existing EC should be present");
        assert_eq!(ec_id, "existing_cookie_id");
    }

    #[test]
    fn test_get_ec_id_from_http_request_with_header() {
        let req = http::Request::builder()
            .method("GET")
            .uri("http://example.com")
            .header(HEADER_X_TS_EC, "existing_http_ec_id")
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");

        let ec_id =
            unvalidated_ec_id_from_request(&req).expect("should get EC ID from http request");

        assert_eq!(ec_id, Some("existing_http_ec_id".to_string()));
    }

    #[tokio::test]
    async fn test_get_or_generate_ec_id_from_http_request_reuses_cookie() {
        let settings = create_test_settings();
        let req = http::Request::builder()
            .method("GET")
            .uri("http://example.com")
            .header(
                header::COOKIE,
                format!("{}=existing_http_cookie_id", COOKIE_TS_EC),
            )
            .body(edgezero_core::body::Body::empty())
            .expect("should build test request");

        let ec_id = get_or_generate_ec_id_from_http_request(&settings, &noop_services(), &req)
            .await
            .expect("should reuse cookie EC ID from http request")
            .expect("an existing EC should be present");

        assert_eq!(ec_id, "existing_http_cookie_id");
    }

    #[test]
    fn test_get_ec_id_none() {
        let req = create_test_request(&[]);
        let ec_id = unvalidated_ec_id_from_request(&req).expect("should handle missing ID");
        assert!(ec_id.is_none());
    }

    #[tokio::test]
    async fn test_get_or_generate_ec_id_generate_new() {
        let settings = create_test_settings();
        let req = create_test_request(&[]);

        let ec_id = get_or_generate_ec_id(&settings, &noop_services(), &req)
            .await
            .expect("should get or generate EC ID")
            .expect("should configure the hmac provider in test settings");
        assert!(!ec_id.is_empty());
    }

    #[test]
    fn test_get_ec_id_rejects_invalid_header_and_falls_back_to_cookie() {
        let req = create_test_request(&[
            (HEADER_X_TS_EC, "evil;injected"),
            (header::COOKIE, &format!("{}=valid_cookie_id", COOKIE_TS_EC)),
        ]);

        let ec_id =
            unvalidated_ec_id_from_request(&req).expect("should handle invalid header gracefully");
        assert_eq!(
            ec_id,
            Some("valid_cookie_id".to_string()),
            "should reject tampered header and fall back to valid cookie"
        );
    }

    #[tokio::test]
    async fn test_get_or_generate_ec_id_replaces_invalid_header() {
        let settings = create_test_settings();
        let req = create_test_request(&[(HEADER_X_TS_EC, "evil;injected")]);

        let ec_id = get_or_generate_ec_id(&settings, &noop_services(), &req)
            .await
            .expect("should generate fresh ID on invalid header")
            .expect("should configure the hmac provider in test settings");
        assert_ne!(
            ec_id, "evil;injected",
            "should not use tampered header value"
        );
        assert!(
            is_ec_id_format(&ec_id),
            "should generate a valid EC ID format when header is rejected"
        );
    }

    #[test]
    fn test_get_ec_id_rejects_invalid_cookie() {
        let req = create_test_request(&[(
            header::COOKIE,
            &format!("{}=bad<script>value", COOKIE_TS_EC),
        )]);

        let ec_id =
            unvalidated_ec_id_from_request(&req).expect("should handle invalid cookie gracefully");
        assert!(
            ec_id.is_none(),
            "should reject cookie with disallowed characters"
        );
    }
}
