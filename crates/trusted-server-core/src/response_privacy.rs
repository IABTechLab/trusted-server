//! Shared response cache-privacy hardening for every platform adapter.
//!
//! The server-side ad stack and EC identity lifecycle emit per-user responses
//! (assembled HTML, `page-bids` JSON, cookie-bearing navigations) that must
//! never reach a shared cache. Each adapter's `apply_finalize_headers` applies
//! operator-configured `settings.response_headers`, so the cookie-privacy
//! downgrade and the uncacheable-operator-header guard have to live in one place
//! and run byte-identically on Fastly, Cloudflare, Axum, and Spin — a shared
//! cache such as Cloudflare would otherwise serve an operator/origin
//! `Cache-Control: public` on a cookie-bearing response as-is.

use edgezero_core::http::{HeaderName, HeaderValue, Response, header};

use crate::settings::Settings;

/// Surrogate cache headers stripped from every cookie-bearing response.
///
/// A single source of truth so the adapter copies of the privacy downgrade
/// cannot drift apart.
pub const SURROGATE_CACHE_HEADERS: &[&str] = &[
    "surrogate-control",
    "fastly-surrogate-control",
    "cdn-cache-control",
    "cloudflare-cdn-cache-control",
];

/// Forces cookie-bearing responses to stay private to shared caches.
///
/// Any response that sets a per-user cookie (notably the EC identity cookie)
/// must never be shared-cached, or a shared cache could replay one user's
/// `Set-Cookie` to others.
///
/// Idempotent: a response already marked `private`/`no-store` keeps its stricter
/// `Cache-Control`, but the surrogate cache headers are stripped regardless so a
/// `no-store` cookie response can never retain shared cacheability.
pub fn enforce_set_cookie_cache_privacy(response: &mut Response) {
    if !response.headers().contains_key(header::SET_COOKIE) {
        return;
    }
    // Surrogate cache headers must come off every cookie-bearing response, even
    // one already carrying a stricter `no-store`/`private` directive — they are
    // independent of Cache-Control and would otherwise let a shared cache store
    // and replay one visitor's Set-Cookie.
    for name in SURROGATE_CACHE_HEADERS {
        response.headers_mut().remove(*name);
    }
    // Cache-Control directives are case-insensitive (RFC 9111 §5.2), so match
    // against a lowercased copy — `No-Store` / `Private` must count.
    let already_uncacheable = response
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .map(str::to_ascii_lowercase)
        .is_some_and(|v| v.contains("private") || v.contains("no-store"));
    if !already_uncacheable {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=0"),
        );
    }
}

/// Applies operator-configured `settings.response_headers` with cookie-privacy
/// hardening.
///
/// First downgrades cookie-bearing responses via
/// [`enforce_set_cookie_cache_privacy`], then applies operator headers — but on
/// an uncacheable (`private`/`no-store`) response the cache-controlling headers
/// (`Cache-Control` and the surrogate cache headers) are skipped so operators
/// cannot re-enable shared caching for per-user payloads. After the operator
/// headers are applied the cookie-privacy downgrade runs once more, so a
/// configured `Set-Cookie` combined with public/surrogate cache headers cannot
/// produce a shared-cacheable cookie-bearing response.
///
/// Invalid header names/values are logged and skipped rather than panicking, so
/// a misconfigured operator header can never take down a request.
pub fn apply_response_headers_with_cache_privacy(settings: &Settings, response: &mut Response) {
    enforce_set_cookie_cache_privacy(response);

    let response_is_uncacheable = response
        .headers()
        .get(header::CACHE_CONTROL)
        .and_then(|v| v.to_str().ok())
        .map(str::to_ascii_lowercase)
        .is_some_and(|v| v.contains("private") || v.contains("no-store"));

    for (key, value) in &settings.response_headers {
        if response_is_uncacheable
            && (key.eq_ignore_ascii_case(header::CACHE_CONTROL.as_str())
                || SURROGATE_CACHE_HEADERS
                    .iter()
                    .any(|name| key.eq_ignore_ascii_case(name)))
        {
            continue;
        }
        let header_name = match HeaderName::from_bytes(key.as_bytes()) {
            Ok(name) => name,
            Err(_) => {
                log::warn!("Skipping invalid configured response header name {key}");
                continue;
            }
        };
        let header_value = match HeaderValue::from_str(value) {
            Ok(value) => value,
            Err(_) => {
                log::warn!("Skipping invalid configured response header value for {key}");
                continue;
            }
        };
        response.headers_mut().insert(header_name, header_value);
    }

    // Operator headers can themselves introduce Set-Cookie (alongside public
    // or surrogate cache headers) onto a previously cookieless response, which
    // the pre-apply pass could not see. Re-run the downgrade so the final
    // response can never pair Set-Cookie with shared cacheability.
    enforce_set_cookie_cache_privacy(response);
}

#[cfg(test)]
mod tests {
    use super::*;

    use edgezero_core::http::response_builder;

    fn settings_with_response_headers(headers: &[(&str, &str)]) -> Settings {
        let mut s = Settings::from_toml(
            r#"
                [[handlers]]
                path = "^/_ts/admin"
                username = "admin"
                password = "admin-pass"

                [publisher]
                domain = "test-publisher.example.com"
                cookie_domain = ".test-publisher.example.com"
                origin_url = "https://origin.test-publisher.example.com"
                proxy_secret = "unit-test-proxy-secret"

                [ec]
                passphrase = "test-secret-key-32-bytes-minimum"
            "#,
        )
        .expect("should load test settings");
        s.response_headers = headers
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        s
    }

    #[test]
    fn downgrades_public_cache_control_on_cookie_response() {
        let settings = settings_with_response_headers(&[("cache-control", "public, max-age=600")]);
        let mut response = response_builder()
            .header(header::SET_COOKIE, "id=abc")
            .header("surrogate-control", "max-age=600")
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");

        apply_response_headers_with_cache_privacy(&settings, &mut response);

        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("private, max-age=0"),
            "operator public Cache-Control must not override cookie privacy downgrade"
        );
        assert!(
            !response.headers().contains_key("surrogate-control"),
            "surrogate cache headers must be stripped on cookie responses"
        );
    }

    #[test]
    fn downgrades_operator_configured_set_cookie_with_public_cache_headers() {
        // Operator headers that add Set-Cookie plus shared-cache directives to
        // a cookieless response must be re-downgraded after they are applied.
        let settings = settings_with_response_headers(&[
            ("set-cookie", "operator=abc"),
            ("cache-control", "public, max-age=600"),
            ("surrogate-control", "max-age=600"),
        ]);
        let mut response = response_builder()
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");

        apply_response_headers_with_cache_privacy(&settings, &mut response);

        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("private, max-age=0"),
            "operator Set-Cookie plus public Cache-Control must be re-downgraded to private"
        );
        assert!(
            !response.headers().contains_key("surrogate-control"),
            "surrogate cache headers must be stripped when operator headers add Set-Cookie"
        );
        assert!(
            response.headers().contains_key(header::SET_COOKIE),
            "the operator Set-Cookie itself should still be applied"
        );
    }

    #[test]
    fn applies_operator_headers_on_cookieless_response() {
        let settings = settings_with_response_headers(&[("x-operator", "value")]);
        let mut response = response_builder()
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");

        apply_response_headers_with_cache_privacy(&settings, &mut response);

        assert_eq!(
            response
                .headers()
                .get("x-operator")
                .and_then(|v| v.to_str().ok()),
            Some("value"),
            "operator headers should still apply to cacheable responses"
        );
    }
}
