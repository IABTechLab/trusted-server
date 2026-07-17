//! Structured cache-policy rendering helpers.
//!
//! Cache policy is expressed once as typed data and then rendered into the
//! runtime-specific headers used by each edge platform. The helpers in this
//! module only write cache-control headers; response privacy hardening still
//! runs later so personalized or cookie-bearing responses cannot be made
//! shared-cacheable by accident.

use std::time::Duration;

use http::header::{self, HeaderName};
use http::{HeaderMap, HeaderValue};

/// String name Fastly uses for shared-cache control.
pub const HEADER_SURROGATE_CONTROL_NAME: &str = "surrogate-control";
/// String name Fastly may use for shared-cache control in some configurations.
pub const HEADER_FASTLY_SURROGATE_CONTROL_NAME: &str = "fastly-surrogate-control";
/// String name for the standards-track CDN-only shared-cache control header.
pub const HEADER_CDN_CACHE_CONTROL_NAME: &str = "cdn-cache-control";
/// String name for Cloudflare-specific CDN-only shared-cache control.
pub const HEADER_CLOUDFLARE_CDN_CACHE_CONTROL_NAME: &str = "cloudflare-cdn-cache-control";

/// Runtime edge-cache header names owned by this crate.
pub const EDGE_CACHE_HEADER_NAMES: &[&str] = &[
    HEADER_SURROGATE_CONTROL_NAME,
    HEADER_FASTLY_SURROGATE_CONTROL_NAME,
    HEADER_CDN_CACHE_CONTROL_NAME,
    HEADER_CLOUDFLARE_CDN_CACHE_CONTROL_NAME,
];

/// Header name Fastly uses for shared-cache control.
pub const HEADER_SURROGATE_CONTROL: HeaderName =
    HeaderName::from_static(HEADER_SURROGATE_CONTROL_NAME);
/// Header name Fastly may use for shared-cache control in some configurations.
pub const HEADER_FASTLY_SURROGATE_CONTROL: HeaderName =
    HeaderName::from_static(HEADER_FASTLY_SURROGATE_CONTROL_NAME);
/// Standards-track header name for CDN-only shared-cache control.
pub const HEADER_CDN_CACHE_CONTROL: HeaderName =
    HeaderName::from_static(HEADER_CDN_CACHE_CONTROL_NAME);
/// Cloudflare-specific header name for CDN-only shared-cache control.
pub const HEADER_CLOUDFLARE_CDN_CACHE_CONTROL: HeaderName =
    HeaderName::from_static(HEADER_CLOUDFLARE_CDN_CACHE_CONTROL_NAME);

/// Cache-control value used when a response must not be stored.
pub const NO_STORE_PRIVATE_CACHE_CONTROL: &str = "no-store, private";

/// Shared-cache header family emitted for the current runtime.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EdgeCacheHeader {
    /// Emit Fastly's `Surrogate-Control` header.
    SurrogateControl,
    /// Emit the standards-track `CDN-Cache-Control` header.
    CdnCacheControl,
    /// Emit Cloudflare's `Cloudflare-CDN-Cache-Control` header.
    CloudflareCdnCacheControl,
    /// Put `s-maxage` into `Cache-Control` instead of emitting a separate edge header.
    SMaxageFallback,
    /// Do not emit edge-cache directives.
    None,
}

/// Cache visibility for the browser-facing `Cache-Control` header.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CacheVisibility {
    /// Response may be stored by shared caches when edge directives allow it.
    Public,
    /// Response is private to the requesting browser.
    Private,
}

impl CacheVisibility {
    fn directive(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
        }
    }
}

impl EdgeCacheHeader {
    fn header_name(self) -> Option<HeaderName> {
        match self {
            Self::SurrogateControl => Some(HEADER_SURROGATE_CONTROL),
            Self::CdnCacheControl => Some(HEADER_CDN_CACHE_CONTROL),
            Self::CloudflareCdnCacheControl => Some(HEADER_CLOUDFLARE_CDN_CACHE_CONTROL),
            Self::SMaxageFallback | Self::None => None,
        }
    }
}

/// Structured browser/edge cache policy.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct CachePolicy {
    /// Whether the browser-facing response is public or private.
    pub visibility: CacheVisibility,
    /// Browser cache TTL rendered as `max-age`.
    pub browser_ttl: Option<Duration>,
    /// Shared edge cache TTL rendered as an edge header or `s-maxage` fallback.
    pub edge_ttl: Option<Duration>,
    /// Optional `stale-while-revalidate` duration.
    pub stale_while_revalidate: Option<Duration>,
    /// Optional `stale-if-error` duration.
    pub stale_if_error: Option<Duration>,
    /// Whether to render `immutable` for browser caches.
    pub immutable: bool,
}

impl CachePolicy {
    /// Create a public immutable policy for content-addressed static assets.
    #[must_use]
    pub const fn public_immutable(ttl: Duration) -> Self {
        Self {
            visibility: CacheVisibility::Public,
            browser_ttl: Some(ttl),
            edge_ttl: Some(ttl),
            stale_while_revalidate: None,
            stale_if_error: None,
            immutable: true,
        }
    }

    /// Create the current short TSJS fallback policy for unversioned/mismatched requests.
    #[must_use]
    pub const fn public_short_with_stale(
        ttl: Duration,
        stale_while_revalidate: Duration,
        stale_if_error: Duration,
    ) -> Self {
        Self {
            visibility: CacheVisibility::Public,
            browser_ttl: Some(ttl),
            edge_ttl: Some(ttl),
            stale_while_revalidate: Some(stale_while_revalidate),
            stale_if_error: Some(stale_if_error),
            immutable: false,
        }
    }

    /// Create a private revalidation policy for personalized browser responses.
    #[must_use]
    pub const fn private_revalidate() -> Self {
        Self {
            visibility: CacheVisibility::Private,
            browser_ttl: Some(Duration::from_secs(0)),
            edge_ttl: None,
            stale_while_revalidate: None,
            stale_if_error: None,
            immutable: false,
        }
    }

    /// Render the browser-facing `Cache-Control` value.
    #[must_use]
    pub fn cache_control_value(self, edge_header: EdgeCacheHeader) -> String {
        let mut directives = Vec::new();
        directives.push(self.visibility.directive().to_string());

        if let Some(ttl) = self.browser_ttl {
            directives.push(format!("max-age={}", ttl.as_secs()));
        }

        if edge_header == EdgeCacheHeader::SMaxageFallback
            && let Some(ttl) = self
                .edge_ttl
                .filter(|_| self.visibility == CacheVisibility::Public)
        {
            directives.push(format!("s-maxage={}", ttl.as_secs()));
        }

        if let Some(ttl) = self.stale_while_revalidate {
            directives.push(format!("stale-while-revalidate={}", ttl.as_secs()));
        }

        if let Some(ttl) = self.stale_if_error {
            directives.push(format!("stale-if-error={}", ttl.as_secs()));
        }

        if self.immutable && self.browser_ttl.is_some_and(|ttl| ttl.as_secs() > 0) {
            directives.push("immutable".to_string());
        }

        directives.join(", ")
    }

    /// Render the separate edge-cache header value, if this policy should emit one.
    #[must_use]
    pub fn edge_header_value(self, edge_header: EdgeCacheHeader) -> Option<String> {
        if self.visibility != CacheVisibility::Public {
            return None;
        }
        if matches!(
            edge_header,
            EdgeCacheHeader::None | EdgeCacheHeader::SMaxageFallback
        ) {
            return None;
        }

        let edge_ttl = self.edge_ttl?;
        let mut directives = vec![format!("max-age={}", edge_ttl.as_secs())];

        if let Some(ttl) = self.stale_while_revalidate {
            directives.push(format!("stale-while-revalidate={}", ttl.as_secs()));
        }

        if let Some(ttl) = self.stale_if_error {
            directives.push(format!("stale-if-error={}", ttl.as_secs()));
        }

        Some(directives.join(", "))
    }

    /// Apply the policy to response headers for the selected runtime edge header.
    ///
    /// # Panics
    ///
    /// Panics if the internally-rendered cache header values are not valid HTTP
    /// header values. This should not happen because values are generated from
    /// fixed directive names and numeric durations.
    pub fn apply_to_headers(self, headers: &mut HeaderMap, edge_header: EdgeCacheHeader) {
        let cache_control = self.cache_control_value(edge_header);
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_str(&cache_control)
                .expect("should render a valid cache-control header"),
        );

        remove_edge_cache_headers(headers);
        if let Some(header_name) = edge_header.header_name()
            && let Some(value) = self.edge_header_value(edge_header)
        {
            headers.insert(
                header_name,
                HeaderValue::from_str(&value)
                    .expect("should render a valid edge cache-control header"),
            );
        }
    }
}

/// Cache-control mode, including explicitly uncacheable responses.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum CacheControlPolicy {
    /// Apply a regular TTL-based cache policy.
    Store(CachePolicy),
    /// Apply `Cache-Control: no-store, private` and strip shared-cache headers.
    NoStorePrivate,
}

impl CacheControlPolicy {
    /// Apply this cache-control mode to response headers.
    ///
    /// # Panics
    ///
    /// Panics if an internally-rendered cache header value is not valid. This
    /// should not happen because values are generated from fixed directive names
    /// and numeric durations.
    pub fn apply_to_headers(self, headers: &mut HeaderMap, edge_header: EdgeCacheHeader) {
        match self {
            Self::Store(policy) => policy.apply_to_headers(headers, edge_header),
            Self::NoStorePrivate => apply_no_store_private_to_headers(headers),
        }
    }
}

impl From<CachePolicy> for CacheControlPolicy {
    fn from(policy: CachePolicy) -> Self {
        Self::Store(policy)
    }
}

/// Remove every runtime-specific shared-cache header owned by this crate.
pub fn remove_edge_cache_headers(headers: &mut HeaderMap) {
    for name in EDGE_CACHE_HEADER_NAMES {
        headers.remove(*name);
    }
}

/// Return true when `name` is an edge-cache header owned by this crate.
#[must_use]
pub fn is_edge_cache_header_name(name: &str) -> bool {
    EDGE_CACHE_HEADER_NAMES
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// Return true when a `Cache-Control` field value contains `directive`.
///
/// Matching is directive-name exact and case-insensitive. Pseudo-directives such
/// as `not-private` or `no-storey` do not match `private` / `no-store`.
#[must_use]
pub fn cache_control_value_has_directive(value: &str, directive: &str) -> bool {
    value.split(',').any(|part| {
        let part = part.trim();
        let directive_name = part
            .find(['=', ';'])
            .map_or(part, |end| &part[..end])
            .trim();
        directive_name.eq_ignore_ascii_case(directive)
    })
}

/// Return true when any `Cache-Control` header value contains `directive`.
#[must_use]
pub fn cache_control_headers_have_directive(headers: &HeaderMap, directive: &str) -> bool {
    headers
        .get_all(header::CACHE_CONTROL)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| cache_control_value_has_directive(value, directive))
}

/// Return true when response cache-control contains exact `private` or `no-store`.
#[must_use]
pub fn cache_control_headers_are_private_or_no_store(headers: &HeaderMap) -> bool {
    cache_control_headers_have_directive(headers, "private")
        || cache_control_headers_have_directive(headers, "no-store")
}

/// Apply `Cache-Control: no-store, private` and strip all shared-cache headers.
///
/// # Panics
///
/// Panics if the fixed no-store cache-control value is not a valid HTTP header
/// value. This should not happen for a static ASCII value.
pub fn apply_no_store_private_to_headers(headers: &mut HeaderMap) {
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(NO_STORE_PRIVATE_CACHE_CONTROL),
    );
    remove_edge_cache_headers(headers);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_immutable_renders_browser_and_fastly_headers() {
        let policy = CachePolicy::public_immutable(Duration::from_secs(31_536_000));
        let mut headers = HeaderMap::new();

        policy.apply_to_headers(&mut headers, EdgeCacheHeader::SurrogateControl);

        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("public, max-age=31536000, immutable"),
            "should render immutable browser policy"
        );
        assert_eq!(
            headers
                .get(HEADER_SURROGATE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("max-age=31536000"),
            "should render Fastly edge TTL"
        );
    }

    #[test]
    fn s_maxage_fallback_renders_edge_ttl_inside_cache_control() {
        let policy = CachePolicy::public_short_with_stale(
            Duration::from_secs(300),
            Duration::from_secs(60),
            Duration::from_secs(86_400),
        );

        assert_eq!(
            policy.cache_control_value(EdgeCacheHeader::SMaxageFallback),
            "public, max-age=300, s-maxage=300, stale-while-revalidate=60, stale-if-error=86400",
            "should render portable two-tier fallback"
        );
    }

    #[test]
    fn generic_cdn_header_renders_cdn_only_policy() {
        let policy = CachePolicy::public_short_with_stale(
            Duration::from_secs(300),
            Duration::from_secs(60),
            Duration::from_secs(86_400),
        );
        let mut headers = HeaderMap::new();

        policy.apply_to_headers(&mut headers, EdgeCacheHeader::CdnCacheControl);

        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("public, max-age=300, stale-while-revalidate=60, stale-if-error=86400"),
            "should keep CDN TTL out of browser cache-control when using targeted CDN header"
        );
        assert_eq!(
            headers
                .get(HEADER_CDN_CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("max-age=300, stale-while-revalidate=60, stale-if-error=86400"),
            "should render generic CDN cache policy"
        );
    }

    #[test]
    fn cloudflare_specific_header_renders_cdn_only_policy() {
        let policy = CachePolicy::public_short_with_stale(
            Duration::from_secs(300),
            Duration::from_secs(60),
            Duration::from_secs(86_400),
        );
        let mut headers = HeaderMap::new();

        policy.apply_to_headers(&mut headers, EdgeCacheHeader::CloudflareCdnCacheControl);

        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("public, max-age=300, stale-while-revalidate=60, stale-if-error=86400"),
            "should keep CDN TTL out of browser cache-control when using targeted CDN header"
        );
        assert_eq!(
            headers
                .get(HEADER_CLOUDFLARE_CDN_CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("max-age=300, stale-while-revalidate=60, stale-if-error=86400"),
            "should render Cloudflare-specific CDN cache policy"
        );
        assert!(
            headers.get(HEADER_CDN_CACHE_CONTROL).is_none(),
            "should not also emit the generic CDN cache header"
        );
    }

    #[test]
    fn private_policy_removes_stale_edge_headers() {
        let policy = CachePolicy::private_revalidate();
        let mut headers = HeaderMap::new();
        headers.insert(
            HEADER_SURROGATE_CONTROL,
            HeaderValue::from_static("max-age=60"),
        );
        headers.insert(
            HEADER_CDN_CACHE_CONTROL,
            HeaderValue::from_static("max-age=60"),
        );
        headers.insert(
            HEADER_CLOUDFLARE_CDN_CACHE_CONTROL,
            HeaderValue::from_static("max-age=60"),
        );

        policy.apply_to_headers(&mut headers, EdgeCacheHeader::SurrogateControl);

        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("private, max-age=0"),
            "should render private browser policy"
        );
        assert!(
            headers.get(HEADER_SURROGATE_CONTROL).is_none(),
            "should remove Fastly shared-cache headers for private responses"
        );
        assert!(
            headers.get(HEADER_CDN_CACHE_CONTROL).is_none(),
            "should remove generic CDN cache headers for private responses"
        );
        assert!(
            headers.get(HEADER_CLOUDFLARE_CDN_CACHE_CONTROL).is_none(),
            "should remove Cloudflare cache headers for private responses"
        );
    }

    #[test]
    fn no_store_policy_removes_stale_edge_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HEADER_SURROGATE_CONTROL,
            HeaderValue::from_static("max-age=60"),
        );
        headers.insert(
            HEADER_FASTLY_SURROGATE_CONTROL,
            HeaderValue::from_static("max-age=60"),
        );
        headers.insert(
            HEADER_CDN_CACHE_CONTROL,
            HeaderValue::from_static("max-age=60"),
        );
        headers.insert(
            HEADER_CLOUDFLARE_CDN_CACHE_CONTROL,
            HeaderValue::from_static("max-age=60"),
        );

        CacheControlPolicy::NoStorePrivate.apply_to_headers(&mut headers, EdgeCacheHeader::None);

        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some(NO_STORE_PRIVATE_CACHE_CONTROL),
            "should render no-store cache policy"
        );
        assert!(
            headers.get(HEADER_SURROGATE_CONTROL).is_none()
                && headers.get(HEADER_FASTLY_SURROGATE_CONTROL).is_none()
                && headers.get(HEADER_CDN_CACHE_CONTROL).is_none()
                && headers.get(HEADER_CLOUDFLARE_CDN_CACHE_CONTROL).is_none(),
            "should remove all shared-cache headers"
        );
    }

    #[test]
    fn immutable_is_omitted_without_positive_browser_ttl() {
        let policy = CachePolicy {
            visibility: CacheVisibility::Public,
            browser_ttl: Some(Duration::from_secs(0)),
            edge_ttl: Some(Duration::from_secs(60)),
            stale_while_revalidate: None,
            stale_if_error: None,
            immutable: true,
        };

        assert_eq!(
            policy.cache_control_value(EdgeCacheHeader::None),
            "public, max-age=0",
            "should not render immutable without a positive browser TTL"
        );
    }

    #[test]
    fn cache_control_directive_matching_is_exact() {
        assert!(
            cache_control_value_has_directive("public, max-age=60, No-Store", "no-store"),
            "should match real no-store directives case-insensitively"
        );
        assert!(
            cache_control_value_has_directive("private=\"set-cookie\", max-age=0", "private"),
            "should match directives with arguments"
        );
        assert!(
            !cache_control_value_has_directive("public, no-storey, not-private", "no-store"),
            "should not match pseudo-directives by substring"
        );
        assert!(
            !cache_control_value_has_directive("public, no-storey, not-private", "private"),
            "should not match pseudo-private directives by substring"
        );
    }

    #[test]
    fn cache_control_header_matching_checks_all_values() {
        let mut headers = HeaderMap::new();
        headers.append(header::CACHE_CONTROL, HeaderValue::from_static("public"));
        headers.append(
            header::CACHE_CONTROL,
            HeaderValue::from_static("max-age=60"),
        );
        headers.append(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));

        assert!(
            cache_control_headers_are_private_or_no_store(&headers),
            "should inspect every Cache-Control field value"
        );
    }
}
