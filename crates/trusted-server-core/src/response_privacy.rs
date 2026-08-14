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

/// CDN-targeted cache headers stripped from every cookie-bearing response.
///
/// A single source of truth so the adapter copies of the privacy downgrade
/// cannot drift apart.
pub const CDN_CACHE_HEADERS: &[&str] = &[
    "surrogate-control",
    "fastly-surrogate-control",
    "cdn-cache-control",
    "cloudflare-cdn-cache-control",
];

const APS_INTEGRATION_ID: &str = "aps";
const APS_PUBLISHER_FRAME_ANCESTORS_CSP: &str = "frame-ancestors 'self';";

fn enforce_aps_publisher_frame_ancestors(settings: &Settings, response: &mut Response) {
    let aps_enabled = settings
        .integrations
        .get(APS_INTEGRATION_ID)
        .and_then(|value| value.get("enabled"))
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    if !aps_enabled {
        return;
    }

    let already_present = response
        .headers()
        .get_all(header::CONTENT_SECURITY_POLICY)
        .iter()
        .any(|value| value.as_bytes() == APS_PUBLISHER_FRAME_ANCESTORS_CSP.as_bytes());
    if !already_present {
        response.headers_mut().append(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(APS_PUBLISHER_FRAME_ANCESTORS_CSP),
        );
    }
}

fn strip_cdn_cache_headers(response: &mut Response) {
    for name in CDN_CACHE_HEADERS {
        response.headers_mut().remove(*name);
    }
}

/// Forces synthesized HTML to be private and non-storable.
///
/// Use this exact policy whenever Trusted Server changes an origin HTML
/// representation with request-specific content: force `private, no-store`,
/// remove origin validators, and remove all CDN-targeted cache directives.
pub(crate) fn enforce_synthesized_html_cache_privacy(response: &mut Response) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().remove(header::ETAG);
    response.headers_mut().remove(header::LAST_MODIFIED);
    strip_cdn_cache_headers(response);
}

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
    // Shared-cache control headers must come off every cookie-bearing response, even
    // one already carrying a stricter `no-store`/`private` directive — they are
    // independent of Cache-Control and would otherwise let a shared cache store
    // and replay one visitor's Set-Cookie.
    strip_cdn_cache_headers(response);
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
                || CDN_CACHE_HEADERS
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

    // APS creatives retain their HTTPS origin. A creative can therefore frame
    // a publisher URL beneath the opaque renderer unless every publisher
    // response rejects the cross-origin ancestor chain. Append this independent
    // policy after operator headers so configuration cannot weaken it.
    enforce_aps_publisher_frame_ancestors(settings, response);
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
    fn synthesized_html_is_forced_no_store_without_validators_or_cdn_headers() {
        let mut response = response_builder()
            .header(header::CACHE_CONTROL, "private, max-age=600")
            .header(header::ETAG, "\"origin\"")
            .header(header::LAST_MODIFIED, "Wed, 21 Oct 2015 07:28:00 GMT")
            .header("surrogate-control", "max-age=600")
            .header("fastly-surrogate-control", "max-age=600")
            .header("cdn-cache-control", "max-age=600")
            .header("cloudflare-cdn-cache-control", "max-age=600")
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");

        enforce_synthesized_html_cache_privacy(&mut response);

        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "private, no-store",
            "synthesized HTML should always be non-storable"
        );
        for header_name in [header::ETAG.as_str(), header::LAST_MODIFIED.as_str()]
            .into_iter()
            .chain(CDN_CACHE_HEADERS.iter().copied())
        {
            assert!(
                !response.headers().contains_key(header_name),
                "synthesized HTML should remove {header_name}"
            );
        }
    }

    #[test]
    fn downgrades_public_cache_control_on_cookie_response() {
        let settings = settings_with_response_headers(&[("cache-control", "public, max-age=600")]);
        let mut response = response_builder()
            .header(header::SET_COOKIE, "id=abc")
            .header("surrogate-control", "max-age=600")
            .header("fastly-surrogate-control", "max-age=600")
            .header("cdn-cache-control", "max-age=600")
            .header("cloudflare-cdn-cache-control", "max-age=600")
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
        for header_name in CDN_CACHE_HEADERS {
            assert!(
                !response.headers().contains_key(*header_name),
                "CDN cache header {header_name} must be stripped on cookie responses"
            );
        }
    }

    #[test]
    fn downgrades_operator_configured_set_cookie_with_public_cache_headers() {
        // Operator headers that add Set-Cookie plus shared-cache directives to
        // a cookieless response must be re-downgraded after they are applied.
        let settings = settings_with_response_headers(&[
            ("set-cookie", "operator=abc"),
            ("cache-control", "public, max-age=600"),
            ("surrogate-control", "max-age=600"),
            ("fastly-surrogate-control", "max-age=600"),
            ("cdn-cache-control", "public, max-age=600"),
            ("cloudflare-cdn-cache-control", "public, max-age=600"),
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
        for header_name in CDN_CACHE_HEADERS {
            assert!(
                !response.headers().contains_key(*header_name),
                "CDN cache header {header_name} must be stripped when operator headers add Set-Cookie"
            );
        }
        assert!(
            response.headers().contains_key(header::SET_COOKIE),
            "the operator Set-Cookie itself should still be applied"
        );
    }

    #[test]
    fn preserves_private_no_store_against_operator_cache_headers_without_cookie() {
        let settings = settings_with_response_headers(&[
            ("cache-control", "public, max-age=600"),
            ("surrogate-control", "max-age=600"),
            ("fastly-surrogate-control", "max-age=600"),
            ("cdn-cache-control", "public, max-age=600"),
            ("cloudflare-cdn-cache-control", "public, max-age=600"),
        ]);
        let mut response = response_builder()
            .header(header::CACHE_CONTROL, "private, no-store")
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");

        apply_response_headers_with_cache_privacy(&settings, &mut response);

        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "private, no-store",
            "operator cache headers must not weaken an existing private response"
        );
        for header_name in CDN_CACHE_HEADERS {
            assert!(
                !response.headers().contains_key(*header_name),
                "operator headers must not restore shared caching through {header_name}"
            );
        }
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

    #[test]
    fn aps_enabled_appends_an_independent_publisher_frame_policy() {
        let mut settings =
            settings_with_response_headers(&[("content-security-policy", "default-src 'self'")]);
        settings
            .integrations
            .insert_config(
                APS_INTEGRATION_ID,
                &serde_json::json!({"enabled": true, "account_id": "example-account"}),
            )
            .expect("should enable APS");
        let mut response = response_builder()
            .header(header::CONTENT_SECURITY_POLICY, "script-src 'self'")
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");

        apply_response_headers_with_cache_privacy(&settings, &mut response);
        apply_response_headers_with_cache_privacy(&settings, &mut response);

        let policies = response
            .headers()
            .get_all(header::CONTENT_SECURITY_POLICY)
            .iter()
            .map(|value| value.to_str().expect("should contain valid CSP"))
            .collect::<Vec<_>>();
        assert_eq!(
            policies,
            vec!["default-src 'self'", APS_PUBLISHER_FRAME_ANCESTORS_CSP]
        );
    }

    #[test]
    fn uncacheable_response_rejects_operator_cdn_cache_headers() {
        let settings = settings_with_response_headers(&[
            ("cdn-cache-control", "public, max-age=600"),
            ("cloudflare-cdn-cache-control", "public, max-age=600"),
        ]);
        let mut response = response_builder()
            .header(header::CACHE_CONTROL, "private, no-store")
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");

        apply_response_headers_with_cache_privacy(&settings, &mut response);

        for header_name in ["cdn-cache-control", "cloudflare-cdn-cache-control"] {
            assert!(
                !response.headers().contains_key(header_name),
                "operator headers must not restore shared caching through {header_name}"
            );
        }
    }
}
