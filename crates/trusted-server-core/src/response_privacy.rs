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

use edgezero_core::http::{HeaderMap, HeaderName, HeaderValue, Response, header};

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

const INACTIVE_AD_STACK_BROWSER_CACHE_CONTROL: &str = "private, max-age=60";

#[derive(Debug, Clone, Copy)]
struct GeneratedInactiveAdStackBrowserCachePolicy;

fn cache_control_segment_has_directive(segment: &[u8], target: &[u8]) -> bool {
    let name_end = segment
        .iter()
        .position(|byte| *byte == b'=')
        .unwrap_or(segment.len());
    segment[..name_end]
        .trim_ascii()
        .eq_ignore_ascii_case(target)
}

fn cache_control_value_has_directive(value: &[u8], target: &[u8]) -> bool {
    let mut segment_start = 0;
    let mut in_quotes = false;
    let mut escaped = false;

    for (index, byte) in value.iter().copied().enumerate() {
        if in_quotes {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_quotes = false;
            }
        } else if byte == b'"' {
            in_quotes = true;
        } else if byte == b',' {
            if cache_control_segment_has_directive(&value[segment_start..index], target) {
                return true;
            }
            segment_start = index + 1;
        }
    }

    cache_control_segment_has_directive(&value[segment_start..], target)
}

fn cache_control_has_directive(headers: &HeaderMap, target: &str) -> bool {
    headers
        .get_all(header::CACHE_CONTROL)
        .iter()
        .any(|value| cache_control_value_has_directive(value.as_bytes(), target.as_bytes()))
}

/// Returns whether `Cache-Control` prohibits storage by shared caches.
pub(crate) fn cache_control_forbids_shared_storage(headers: &HeaderMap) -> bool {
    cache_control_has_directive(headers, "private")
        || cache_control_has_directive(headers, "no-store")
}

fn has_generated_inactive_ad_stack_browser_cache_policy(response: &Response) -> bool {
    response
        .extensions()
        .get::<GeneratedInactiveAdStackBrowserCachePolicy>()
        .is_some()
        && response
            .headers()
            .get(header::CACHE_CONTROL)
            .is_some_and(|value| {
                value
                    .as_bytes()
                    .eq_ignore_ascii_case(INACTIVE_AD_STACK_BROWSER_CACHE_CONTROL.as_bytes())
            })
}

/// Applies the browser-only cache policy for structurally inactive ad templates.
pub fn apply_inactive_ad_stack_browser_cache_policy(response: &mut Response) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(INACTIVE_AD_STACK_BROWSER_CACHE_CONTROL),
    );
    response
        .extensions_mut()
        .insert(GeneratedInactiveAdStackBrowserCachePolicy);
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
/// Origin `private`/`no-store` policies remain unchanged, but the generated
/// inactive-stack browser policy is downgraded to `private, max-age=0`. CDN
/// cache headers are always stripped so a cookie response cannot retain shared
/// cacheability.
pub fn enforce_set_cookie_cache_privacy(response: &mut Response) {
    if !response.headers().contains_key(header::SET_COOKIE) {
        return;
    }
    // Shared-cache control headers must come off every cookie-bearing response, even
    // one already carrying a stricter `no-store`/`private` directive — they are
    // independent of Cache-Control and would otherwise let a shared cache store
    // and replay one visitor's Set-Cookie.
    strip_cdn_cache_headers(response);
    // Cookie privacy takes precedence over the generated inactive-stack browser
    // policy, while unrelated origin private/no-store policies remain unchanged.
    let already_forbids_shared_storage = cache_control_forbids_shared_storage(response.headers());
    let has_generated_inactive_browser_policy =
        has_generated_inactive_ad_stack_browser_cache_policy(response);
    if !already_forbids_shared_storage || has_generated_inactive_browser_policy {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=0"),
        );
    }
    response
        .extensions_mut()
        .remove::<GeneratedInactiveAdStackBrowserCachePolicy>();
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

    let response_is_uncacheable = cache_control_forbids_shared_storage(response.headers());

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
    fn identifies_cache_control_that_forbids_shared_storage() {
        for (cache_control, expected) in [
            ("public, max-age=60", false),
            ("no-cache", false),
            ("public, x=notprivate", false),
            ("public, x=\"private, no-store\"", false),
            (r#"public, x="private\", no-store", max-age=60"#, false),
            (r#"public, x="private\\", no-store"#, true),
            ("Private=\"set-cookie\", max-age=60", true),
            ("No-Store", true),
        ] {
            let response = response_builder()
                .header(header::CACHE_CONTROL, cache_control)
                .body(edgezero_core::body::Body::empty())
                .expect("should build response");

            assert_eq!(
                cache_control_forbids_shared_storage(response.headers()),
                expected,
                "should classify {cache_control} shared-storage policy"
            );
        }
    }

    #[test]
    fn downgrades_inactive_browser_cache_policy_on_cookie_response() {
        let mut response = response_builder()
            .header("surrogate-control", "max-age=600")
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");
        apply_inactive_ad_stack_browser_cache_policy(&mut response);
        response
            .headers_mut()
            .insert(header::SET_COOKIE, HeaderValue::from_static("id=abc"));

        enforce_set_cookie_cache_privacy(&mut response);

        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("private, max-age=0"),
            "inactive browser cache policy should yield to cookie privacy"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "cookie privacy should strip CDN cache headers"
        );
    }

    #[test]
    fn preserves_identical_origin_private_policy_on_cookie_response() {
        let mut response = response_builder()
            .header(header::SET_COOKIE, "id=abc")
            .header(header::CACHE_CONTROL, "private, max-age=60")
            .header("surrogate-control", "max-age=600")
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");

        enforce_set_cookie_cache_privacy(&mut response);

        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("private, max-age=60"),
            "origin private policy should remain unchanged"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "cookie privacy should still strip CDN cache headers"
        );
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
