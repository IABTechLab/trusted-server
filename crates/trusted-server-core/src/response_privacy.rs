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

use crate::cache_policy::{
    CacheControlPolicy, EdgeCacheHeader, cache_control_headers_are_private_or_no_store,
    is_edge_cache_header_name, remove_edge_cache_headers,
};
use crate::settings::Settings;

/// Marks a response whose `no-store, private` policy belongs to Trusted Server.
///
/// Platform terminal hooks use this out-of-band marker to re-enforce the policy after
/// late integrations run without rewriting unrelated origin-private responses.
#[derive(Clone, Copy, Debug)]
pub struct TerminalPrivateResponse;

const INACTIVE_AD_STACK_BROWSER_CACHE_CONTROL: &str = "private, max-age=60";

/// Operator `response_headers` names that Trusted Server never sends.
///
/// Response egress owns message framing: it refuses a response that declares
/// `Trailer`, frames the body itself, and strips the hop-by-hop fields. A
/// `Connection` value also removes the fields it names, and a malformed one
/// fails the response with a 500 before commit.
const FORBIDDEN_OPERATOR_RESPONSE_HEADERS: [&str; 11] = [
    "trailer",
    "content-length",
    "connection",
    "keep-alive",
    "te",
    "transfer-encoding",
    "upgrade",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "http2-settings",
];

/// Whether `name` is an operator response header that Trusted Server never sends.
///
/// Matches [`FORBIDDEN_OPERATOR_RESPONSE_HEADERS`] case-insensitively.
pub(crate) fn is_forbidden_operator_response_header(name: &str) -> bool {
    FORBIDDEN_OPERATOR_RESPONSE_HEADERS
        .iter()
        .any(|forbidden| name.eq_ignore_ascii_case(forbidden))
}

#[derive(Debug, Clone, Copy)]
struct GeneratedInactiveAdStackBrowserCachePolicy;

fn cache_control_is_private_or_no_store(response: &Response) -> bool {
    cache_control_headers_are_private_or_no_store(response.headers())
}

/// Returns whether `Cache-Control` prohibits storage by shared caches.
pub(crate) fn cache_control_forbids_shared_storage(headers: &HeaderMap) -> bool {
    cache_control_headers_are_private_or_no_store(headers)
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

/// Reassert the terminal privacy invariant for a synthesized per-reader response.
///
/// Call this after every configurable response mutation. It deliberately overwrites
/// `Cache-Control` and strips validators, expiry metadata, and runtime edge-cache
/// directives so a later integration cannot turn an assembled document into something a
/// shared cache would store.
pub fn enforce_private_no_store(response: &mut Response) {
    CacheControlPolicy::NoStorePrivate
        .apply_to_headers(response.headers_mut(), EdgeCacheHeader::None);
    for name in [
        header::ETAG.as_str(),
        header::LAST_MODIFIED.as_str(),
        header::EXPIRES.as_str(),
        header::AGE.as_str(),
    ] {
        response.headers_mut().remove(name);
    }
}

/// Marks a Trusted Server response as terminal-private and applies its cache policy.
pub(crate) fn enforce_terminal_private_cache_privacy(response: &mut Response) {
    enforce_private_no_store(response);
    response.extensions_mut().insert(TerminalPrivateResponse);
}

/// Forces synthesized HTML to be private and non-storable.
///
/// Use this exact policy whenever Trusted Server changes an origin HTML
/// representation with request-specific content: force `no-store, private`,
/// remove origin validators, and remove all runtime edge-cache directives.
pub(crate) fn enforce_synthesized_html_cache_privacy(response: &mut Response) {
    enforce_terminal_private_cache_privacy(response);
}

/// Removes runtime edge-cache headers from a response finalized as uncacheable.
///
/// Call this after any late response-header mutations so a final `private` or
/// `no-store` directive cannot coexist with an independently authoritative edge
/// cache header.
pub fn enforce_uncacheable_cache_privacy(response: &mut Response) {
    if cache_control_is_private_or_no_store(response) {
        remove_edge_cache_headers(response.headers_mut());
    }
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
    // Edge-cache headers must come off every cookie-bearing response, even one
    // already carrying a stricter `no-store`/`private` directive — they are
    // independent of Cache-Control and would otherwise let a shared cache store
    // and replay one visitor's Set-Cookie.
    remove_edge_cache_headers(response.headers_mut());
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
/// (`Cache-Control` and runtime edge-cache headers) are skipped so operators
/// cannot re-enable shared caching for per-user payloads. After the operator
/// headers are applied the cookie-privacy downgrade runs once more, so a
/// configured `Set-Cookie` combined with public edge-cache headers cannot
/// produce a shared-cacheable cookie-bearing response.
///
/// The entries [`warn_skipped_operator_response_headers`] reports are skipped
/// here without a log, so a misconfigured operator header never takes down a
/// request and a request that applies the headers twice still warns once.
pub fn apply_response_headers_with_cache_privacy(settings: &Settings, response: &mut Response) {
    enforce_set_cookie_cache_privacy(response);

    let response_is_uncacheable = cache_control_forbids_shared_storage(response.headers());
    enforce_uncacheable_cache_privacy(response);

    for (key, value) in &settings.response_headers {
        let Ok((name, value)) = operator_response_header(key, value) else {
            continue;
        };
        if response_is_uncacheable
            && (name == header::CACHE_CONTROL || is_edge_cache_header_name(name.as_str()))
        {
            continue;
        }
        response.headers_mut().insert(name, value);
    }

    enforce_uncacheable_cache_privacy(response);

    // Operator headers can themselves introduce Set-Cookie (alongside public
    // edge-cache headers) onto a previously cookieless response, which the
    // pre-apply pass could not see. Re-run the downgrade so the final response
    // can never pair Set-Cookie with shared cacheability.
    enforce_set_cookie_cache_privacy(response);
}

/// Logs one warning for each configured response header that
/// [`apply_response_headers_with_cache_privacy`] skips.
///
/// `Trailer`, `Content-Length`, and the hop-by-hop fields are skipped because
/// response egress owns them, and an invalid name or value because it cannot be
/// sent. Validation rejects them all, but a config that predates the rejection
/// can still carry them. Each adapter calls this once per request, so a skipped
/// entry warns once however often the request applies the operator headers.
pub fn warn_skipped_operator_response_headers(settings: &Settings) {
    for (key, value) in &settings.response_headers {
        match operator_response_header(key, value) {
            Ok(_) => {}
            Err(SkippedOperatorHeader::Forbidden) => log::warn!(
                "Skipping configured response header `{key}`: framing and hop-by-hop fields belong to response egress"
            ),
            Err(SkippedOperatorHeader::InvalidName) => {
                log::warn!("Skipping invalid configured response header name {key}");
            }
            Err(SkippedOperatorHeader::InvalidValue) => {
                log::warn!("Skipping invalid configured response header value for {key}");
            }
        }
    }
}

/// Why a configured response header entry is never sent.
enum SkippedOperatorHeader {
    /// A framing or hop-by-hop field, which response egress owns.
    Forbidden,
    /// A name that is not a valid header name.
    InvalidName,
    /// A value that is not a valid header value.
    InvalidValue,
}

/// Parses the configured response header entry `key: value`, or says why it is skipped.
fn operator_response_header(
    key: &str,
    value: &str,
) -> Result<(HeaderName, HeaderValue), SkippedOperatorHeader> {
    if is_forbidden_operator_response_header(key) {
        return Err(SkippedOperatorHeader::Forbidden);
    }
    let name =
        HeaderName::from_bytes(key.as_bytes()).map_err(|_| SkippedOperatorHeader::InvalidName)?;
    let value = HeaderValue::from_str(value).map_err(|_| SkippedOperatorHeader::InvalidValue)?;
    Ok((name, value))
}

#[cfg(test)]
mod tests {
    use super::*;

    use edgezero_core::http::response_builder;

    use crate::cache_policy::EDGE_CACHE_HEADER_NAMES;
    use crate::test_support::logging::LogCapture;

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
    fn synthesized_html_is_forced_no_store_without_validators_or_edge_headers() {
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
            "no-store, private",
            "synthesized HTML should always be non-storable"
        );
        assert!(
            response
                .extensions()
                .get::<TerminalPrivateResponse>()
                .is_some(),
            "should mark synthesized HTML for terminal privacy re-enforcement"
        );
        for header_name in [header::ETAG.as_str(), header::LAST_MODIFIED.as_str()]
            .into_iter()
            .chain(EDGE_CACHE_HEADER_NAMES.iter().copied())
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
        assert!(
            !response.headers().contains_key("surrogate-control")
                && !response.headers().contains_key("cdn-cache-control")
                && !response
                    .headers()
                    .contains_key("cloudflare-cdn-cache-control"),
            "edge cache headers must be stripped on cookie responses"
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
            ("cdn-cache-control", "max-age=600"),
            ("cloudflare-cdn-cache-control", "max-age=600"),
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
            !response.headers().contains_key("surrogate-control")
                && !response.headers().contains_key("cdn-cache-control")
                && !response
                    .headers()
                    .contains_key("cloudflare-cdn-cache-control"),
            "edge cache headers must be stripped when operator headers add Set-Cookie"
        );
        assert!(
            response.headers().contains_key(header::SET_COOKIE),
            "the operator Set-Cookie itself should still be applied"
        );
    }

    #[test]
    fn cookie_privacy_does_not_treat_pseudo_directives_as_uncacheable() {
        let settings = settings_with_response_headers(&[]);
        let mut response = response_builder()
            .header(header::SET_COOKIE, "id=abc")
            .header(
                header::CACHE_CONTROL,
                "public, max-age=600, no-storey, not-private",
            )
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
            "pseudo-directives must not prevent the cookie privacy downgrade"
        );
        assert!(
            !response.headers().contains_key("surrogate-control"),
            "cookie privacy downgrade should still strip edge-cache headers"
        );
    }

    #[test]
    fn cookie_privacy_ignores_quoted_extension_directives() {
        let settings = settings_with_response_headers(&[]);
        let mut response = response_builder()
            .header(header::SET_COOKIE, "id=abc")
            .header(
                header::CACHE_CONTROL,
                "public, max-age=600, ext=\"a,no-store,b\"",
            )
            .header("surrogate-control", "max-age=600")
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");

        apply_response_headers_with_cache_privacy(&settings, &mut response);

        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("private, max-age=0"),
            "quoted extension text must not prevent the cookie privacy downgrade"
        );
        assert!(
            !response.headers().contains_key("surrogate-control"),
            "cookie privacy downgrade should strip edge-cache headers"
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
        for header_name in EDGE_CACHE_HEADER_NAMES {
            assert!(
                !response.headers().contains_key(*header_name),
                "operator headers must not restore shared caching through {header_name}"
            );
        }
    }

    #[test]
    fn strips_edge_headers_from_uncacheable_cookieless_response() {
        let settings = settings_with_response_headers(&[
            ("cdn-cache-control", "max-age=600"),
            ("cloudflare-cdn-cache-control", "max-age=600"),
        ]);
        let mut response = response_builder()
            .header(header::CACHE_CONTROL, "private, max-age=0")
            .header("surrogate-control", "max-age=600")
            .header("cdn-cache-control", "max-age=600")
            .header("cloudflare-cdn-cache-control", "max-age=600")
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");

        apply_response_headers_with_cache_privacy(&settings, &mut response);

        assert!(
            !response.headers().contains_key("surrogate-control")
                && !response.headers().contains_key("cdn-cache-control")
                && !response
                    .headers()
                    .contains_key("cloudflare-cdn-cache-control"),
            "uncacheable responses must not retain or receive edge-cache headers"
        );
    }

    #[test]
    fn final_uncacheable_guard_strips_edge_headers_without_a_cookie() {
        let mut response = response_builder()
            .header(header::CACHE_CONTROL, "no-store")
            .header("surrogate-control", "max-age=600")
            .header("cdn-cache-control", "max-age=600")
            .header("cloudflare-cdn-cache-control", "max-age=600")
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");

        enforce_uncacheable_cache_privacy(&mut response);

        assert_eq!(
            response
                .headers()
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store"),
            "final guard should preserve the uncacheable directive"
        );
        assert!(
            EDGE_CACHE_HEADER_NAMES
                .iter()
                .all(|name| !response.headers().contains_key(*name)),
            "final guard should remove every edge-cache header"
        );
    }

    #[test]
    fn terminal_private_stamp_removes_every_cache_and_validator_header() {
        let mut response = response_builder()
            .header(header::CACHE_CONTROL, "public, s-maxage=600")
            .header(header::ETAG, "\"origin\"")
            .header(header::LAST_MODIFIED, "Wed, 12 Aug 2026 00:00:00 GMT")
            .header(header::EXPIRES, "Wed, 12 Aug 2026 01:00:00 GMT")
            .header(header::AGE, "30")
            .header("surrogate-control", "max-age=600")
            .header("cdn-cache-control", "public, max-age=600")
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");

        enforce_private_no_store(&mut response);

        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "no-store, private"
        );
        for name in [
            header::ETAG.as_str(),
            header::LAST_MODIFIED.as_str(),
            header::EXPIRES.as_str(),
            header::AGE.as_str(),
        ]
        .into_iter()
        .chain(EDGE_CACHE_HEADER_NAMES.iter().copied())
        {
            assert!(
                !response.headers().contains_key(name),
                "terminal private stamp must strip {name}"
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
    fn skips_operator_framing_and_hop_by_hop_entries_and_warns_once_each() {
        let forbidden = [
            "Trailer",
            "Content-Length",
            "Connection",
            "Keep-Alive",
            "TE",
            "Transfer-Encoding",
            "Upgrade",
            "Proxy-Connection",
            "Proxy-Authenticate",
            "Proxy-Authorization",
            "HTTP2-Settings",
        ];
        let mut entries: Vec<(&str, &str)> = forbidden
            .iter()
            .map(|name| (*name, "example-value"))
            .collect();
        entries.push(("x-example", "example-value"));
        entries.push(("x example", "example-value"));
        entries.push(("x-invalid-value", "example\nvalue"));
        let settings = settings_with_response_headers(&entries);
        let mut response = response_builder()
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");
        let capture = LogCapture::start();

        apply_response_headers_with_cache_privacy(&settings, &mut response);
        let applying = capture.take_all();
        warn_skipped_operator_response_headers(&settings);
        let warnings = capture.take_warnings();

        assert!(
            applying.is_empty(),
            "should skip the entries without logging while applying them"
        );
        for name in forbidden {
            assert!(
                !response.headers().contains_key(name),
                "should skip the operator {name} entry"
            );
            assert_eq!(
                warnings
                    .iter()
                    .filter(|warning| warning.message.contains(&format!("`{name}`")))
                    .count(),
                1,
                "should warn once that the operator {name} entry is skipped"
            );
        }
        assert!(
            !response.headers().contains_key("x-invalid-value"),
            "should skip the operator entry with an invalid value"
        );
        for message in [
            "Skipping invalid configured response header name x example",
            "Skipping invalid configured response header value for x-invalid-value",
        ] {
            assert!(
                warnings.iter().any(|warning| warning.message == message),
                "should warn `{message}`"
            );
        }
        assert_eq!(
            warnings.len(),
            forbidden.len() + 2,
            "should warn about the skipped entries only"
        );
        assert_eq!(
            response
                .headers()
                .get("x-example")
                .and_then(|value| value.to_str().ok()),
            Some("example-value"),
            "should still apply the other operator entries"
        );
    }
}
