//! Reading an inbound Edge Cookie (EC) identifier.
//!
//! [`recognized_ec_id`] reads the identifier a request carries and recognizes
//! it through the selected module, so only an identifier this deployment
//! issued is handed on. The generation helpers here are compiled for tests
//! only, and the production lifecycle creates identifiers through
//! [`EcContext`](crate::ec::EcContext).

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::Request;

use crate::constants::{COOKIE_TS_EC, HEADER_X_TS_EC};
use crate::cookies::handle_request_cookies;
use crate::ec::cookies::ec_id_has_only_allowed_chars;
#[cfg(test)]
use crate::ec::generation::normalize_ip;
#[cfg(test)]
use crate::ec::module::IdentityInput;
use crate::ec::module::{module_owns_id, request_module};
use crate::error::TrustedServerError;
#[cfg(test)]
use crate::evidence::BorrowedRequestInfo;
use crate::platform::RuntimeServices;
use crate::settings::Settings;

/// Test helper that generates a fresh EC ID with the configured Edge Cookie
/// module.
///
/// The `[ec] module` selection decides the outcome. Returns `Ok(None)` when
/// no module is configured, so no Edge Cookie is created. `request_headers`
/// lets a module that derives identity from request evidence read it. The
/// built-in HMAC module ignores it and uses only the normalized client IP.
/// The production lifecycle creates identifiers through
/// [`EcContext`](crate::ec::EcContext) instead.
///
/// # Errors
///
/// - [`TrustedServerError::EdgeCookie`] if module generation fails
#[cfg(test)]
pub fn generate_ec_id(
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

    let Some(module) = request_module(&settings.ec, services)? else {
        log::info!("No Edge Cookie module configured; running statelessly");
        return Ok(None);
    };

    // The module reads request data (the client IP and the request headers)
    // borrowed at call time, so nothing is cloned. A module that also reads
    // host signals takes them from the injected `HostSignals` service rather
    // than from this request info.
    let request_info = BorrowedRequestInfo::new(&client_ip, request_headers);
    // This helper skips the consent gate, and the built-in module does not
    // read the consent context, so it is not threaded here.
    let generated = module.generate(&request_info, &IdentityInput::default())?;
    let generated = crate::ec::module::GeneratedEdgeCookie {
        id: generated
            .id
            .map(|value| crate::ec::module::apply_module_code(module.as_ref(), &value)),
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
/// which every identifier must satisfy whichever module created it. Those
/// bounds are a backstop on what may travel in a cookie, not a test of
/// authenticity, and on their own they accept any run of `[A-Za-z0-9._~-]`.
///
/// Deciding whether this deployment issued the value needs the selected
/// module, which this function does not have, so it is deliberately not
/// public. Use [`recognized_ec_id`], which applies module ownership on top.
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
/// selected module recognizes.
///
/// [`unvalidated_ec_id_from_request`] applies the global cookie bounds alone,
/// the length cap and the cookie-safe alphabet, which any value a browser can
/// be persuaded to carry will pass. This adds module ownership on top, so the
/// identifier's `{code}~` prefix is dispatched to the module that owns it,
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
/// built-in HMAC shape, so the right test is the selected module's own
/// [`accepts_id`](crate::ec::module::EdgeCookieModule::accepts_id) rather
/// than the built-in strict format check.
///
/// Returns `None` for a value carrying another deployment's module code, for a
/// value the selected module does not recognize, and for every value at all
/// when no module is selected, because a stateless deployment issues no
/// identifier and so has none to hand on.
///
/// Use this wherever the identifier leaves the edge (an outbound origin URL, a
/// click target, a proxied request body), so nothing is egressed that this
/// deployment did not issue.
///
/// # Errors
///
/// - [`TrustedServerError::InvalidHeaderValue`] if cookie parsing fails
/// - [`TrustedServerError::EdgeCookie`] if the selected module cannot be built
pub fn recognized_ec_id(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request<EdgeBody>,
) -> Result<Option<String>, Report<TrustedServerError>> {
    let Some(ec_id) = unvalidated_ec_id_from_request(req)? else {
        return Ok(None);
    };

    let Some(module) = request_module(&settings.ec, services)? else {
        log::debug!(
            "No Edge Cookie module configured; withholding the request's EC ID from egress"
        );
        return Ok(None);
    };

    if module_owns_id(module.as_ref(), &ec_id) {
        return Ok(Some(ec_id));
    }

    log::debug!(
        "Withholding an EC ID module `{}` does not recognize from egress",
        module.id(),
    );
    Ok(None)
}

/// Gets or creates an EC ID from the request.
///
/// Attempts to retrieve an existing EC ID from:
/// 1. The `x-ts-ec` header
/// 2. The `ts-ec` cookie
///
/// If neither exists, generates a new EC ID via the configured module.
///
/// Returns `Ok(None)` when no existing EC ID is present and no Edge Cookie
/// module is configured, so the caller proceeds statelessly.
///
/// # Errors
///
/// Returns an error if ID generation fails.
#[cfg(test)]
pub(crate) fn get_or_generate_ec_id_from_http_request(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request<EdgeBody>,
) -> Result<Option<String>, Report<TrustedServerError>> {
    if let Some(id) = unvalidated_ec_id_from_request(req)? {
        return Ok(Some(id));
    }

    // If no existing EC ID found, generate a fresh one through the module.
    let ec_id = generate_ec_id(settings, services, Some(req.headers()))?;
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
pub fn get_or_generate_ec_id(
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request<EdgeBody>,
) -> Result<Option<String>, Report<TrustedServerError>> {
    get_or_generate_ec_id_from_http_request(settings, services, req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use edgezero_core::body::Body as EdgeBody;
    use http::{HeaderName, header};
    use std::net::{IpAddr, Ipv4Addr};

    use crate::platform::test_support::{noop_services, noop_services_with_client_ip};
    use crate::test_support::tests::create_test_settings;

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

    // `generate_ec_id`, `get_or_generate_ec_id` and
    // `get_or_generate_ec_id_from_http_request` are test helpers compiled for
    // tests only, so the tests that call them test those helpers and not the
    // production path. The production path, including how the client IP is
    // normalized before it is hashed, is tested through `EcContext` in
    // `crate::ec`.

    #[test]
    fn test_generate_ec_id() {
        let settings: Settings = create_test_settings();

        let ec_id = generate_ec_id(&settings, &noop_services(), None)
            .expect("should generate EC ID")
            .expect("should configure the hmac module in test settings");
        log::debug!("Generated EC ID: {}", ec_id);
        assert!(
            is_ec_id_format(&ec_id),
            "should match the coded EC ID format: hmac~{{64hex}}.{{6alnum}}"
        );
    }

    #[test]
    fn generate_ec_id_returns_none_when_no_module_is_configured() {
        let mut settings = create_test_settings();
        // No module selected: Trusted Server runs statelessly.
        settings.ec.module = None;

        let id = generate_ec_id(&settings, &noop_services(), None)
            .expect("generation should not error when no module is configured");
        assert!(
            id.is_none(),
            "no Edge Cookie module should mean no Edge Cookie is created"
        );
    }

    #[test]
    fn test_generate_ec_id_uses_client_ip() {
        let settings = create_test_settings();
        let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1));

        let id_with_ip = generate_ec_id(&settings, &noop_services_with_client_ip(ip), None)
            .expect("should generate EC ID with client IP")
            .expect("should configure the hmac module in test settings");
        let id_without_ip = generate_ec_id(&settings, &noop_services(), None)
            .expect("should generate EC ID without client IP")
            .expect("should configure the hmac module in test settings");

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
            "a freshly created identifier always carries the module code"
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

        let invalid_suffix = format!("hmac~{}.{}", "a".repeat(64), "ab-129");
        assert!(
            !is_ec_id_format(&invalid_suffix),
            "should reject non-alphanumeric suffix"
        );

        let extra_segment = format!("hmac~{}.{}.{}", "a".repeat(64), "Ab12z9", "zz");
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
        // deployment created from one an attacker typed. Module ownership is
        // what draws that line.
        let settings = create_test_settings();
        let services = noop_services();

        for forged in [
            // Passes the alphabet and the length cap, owned by nobody.
            "not-an-identifier",
            // The built-in shape under another deployment's module code.
            "zz00~aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.Ab1234",
            // This deployment's code carrying a value its module never creates.
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

        // A value the selected module does own is still recognized, so the
        // check rejects forgeries rather than everything.
        let issued = format!("hmac~{}.Ab1234", "a".repeat(64));
        let req = create_test_request(&[(HEADER_X_TS_EC, issued.as_str())]);
        assert_eq!(
            recognized_ec_id(&settings, &services, &req)
                .expect("should decide without erroring")
                .as_deref(),
            Some(issued.as_str()),
            "an identifier the selected module owns should still be recognized"
        );
    }

    #[test]
    fn test_get_ec_id_with_header() {
        let settings = create_test_settings();
        let req = create_test_request(&[(HEADER_X_TS_EC, "existing_ec_id")]);

        let ec_id = unvalidated_ec_id_from_request(&req).expect("should get EC ID");
        assert_eq!(ec_id, Some("existing_ec_id".to_string()));

        let ec_id = get_or_generate_ec_id(&settings, &noop_services(), &req)
            .expect("should reuse header EC ID")
            .expect("an existing EC should be present");
        assert_eq!(ec_id, "existing_ec_id");
    }

    #[test]
    fn test_get_ec_id_with_cookie() {
        let settings = create_test_settings();
        let req = create_test_request(&[(
            header::COOKIE,
            &format!("{}=existing_cookie_id", COOKIE_TS_EC),
        )]);

        let ec_id = unvalidated_ec_id_from_request(&req).expect("should get EC ID");
        assert_eq!(ec_id, Some("existing_cookie_id".to_string()));

        let ec_id = get_or_generate_ec_id(&settings, &noop_services(), &req)
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

    #[test]
    fn test_get_or_generate_ec_id_from_http_request_reuses_cookie() {
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

    #[test]
    fn test_get_or_generate_ec_id_generate_new() {
        let settings = create_test_settings();
        let req = create_test_request(&[]);

        let ec_id = get_or_generate_ec_id(&settings, &noop_services(), &req)
            .expect("should get or generate EC ID")
            .expect("should configure the hmac module in test settings");
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

    #[test]
    fn test_get_or_generate_ec_id_replaces_invalid_header() {
        let settings = create_test_settings();
        let req = create_test_request(&[(HEADER_X_TS_EC, "evil;injected")]);

        let ec_id = get_or_generate_ec_id(&settings, &noop_services(), &req)
            .expect("should generate fresh ID on invalid header")
            .expect("should configure the hmac module in test settings");
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
