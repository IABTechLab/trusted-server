//! Structured cache-policy rendering helpers.
//!
//! Cache policy is expressed once as typed data and then rendered into the
//! header the running platform reads a shared cache policy from. The helpers
//! in this module only write cache-control headers; response privacy
//! hardening still runs later so personalized or cookie-bearing responses
//! cannot be made shared-cacheable by accident.

use std::time::Duration;

use http::header::{self, HeaderName};
use http::{HeaderMap, HeaderValue};

use crate::platform::platform_cache_headers;

/// String name of the shared-cache control header from the Edge Architecture
/// Specification, which core reads for every platform.
pub const HEADER_SURROGATE_CONTROL_NAME: &str = "surrogate-control";
/// String name of the CDN-only shared-cache control header from RFC 9213,
/// which core reads for every platform.
pub const HEADER_CDN_CACHE_CONTROL_NAME: &str = "cdn-cache-control";

/// Header name of the shared-cache control header from the Edge Architecture
/// Specification.
pub const HEADER_SURROGATE_CONTROL: HeaderName =
    HeaderName::from_static(HEADER_SURROGATE_CONTROL_NAME);
/// Header name of the CDN-only shared-cache control header from RFC 9213.
pub const HEADER_CDN_CACHE_CONTROL: HeaderName =
    HeaderName::from_static(HEADER_CDN_CACHE_CONTROL_NAME);

/// Every header that carries a shared cache policy: the two core reads for
/// every platform, then the ones the adapter declared through
/// [`crate::platform::declare_platform_cache_headers`].
#[must_use]
pub fn edge_cache_header_names() -> Vec<&'static str> {
    let mut names = vec![HEADER_SURROGATE_CONTROL_NAME, HEADER_CDN_CACHE_CONTROL_NAME];
    names.extend(platform_cache_headers());
    names
}

/// Cache-control value used when a response must not be stored.
pub const NO_STORE_PRIVATE_CACHE_CONTROL: &str = "no-store, private";

/// The header the running platform reads a shared cache policy from.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum EdgeCacheHeader {
    /// Emit the `Surrogate-Control` header.
    SurrogateControl,
    /// Emit the `CDN-Cache-Control` header.
    CdnCacheControl,
    /// Emit the platform's own header, named in lower case, which its adapter
    /// declared through [`crate::platform::declare_platform_cache_headers`].
    Platform(&'static str),
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
            Self::Platform(name) => {
                let parsed = HeaderName::from_bytes(name.as_bytes()).ok();
                if parsed.is_none() {
                    log::warn!("the platform's cache header `{name}` is not a header name");
                }
                parsed
            }
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

/// Remove every header that carries a shared cache policy, see
/// [`edge_cache_header_names`].
pub fn remove_edge_cache_headers(headers: &mut HeaderMap) {
    for name in edge_cache_header_names() {
        headers.remove(name);
    }
}

/// Return true when `name` is a header that carries a shared cache policy,
/// see [`edge_cache_header_names`].
#[must_use]
pub fn is_edge_cache_header_name(name: &str) -> bool {
    edge_cache_header_names()
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// Return true when a `Cache-Control` field value contains `directive`.
///
/// Matching is directive-name exact and case-insensitive. Pseudo-directives such
/// as `not-private` or `no-storey` do not match `private` / `no-store`.
#[must_use]
pub fn cache_control_value_has_directive(value: &str, directive: &str) -> bool {
    let part_has_directive = |part: &str| {
        let part = part.trim();
        let directive_name = part
            .find(['=', ';'])
            .map_or(part, |end| &part[..end])
            .trim();
        directive_name.eq_ignore_ascii_case(directive)
    };

    let mut quoted = false;
    let mut escaped = false;
    let mut part_start = 0;
    for (index, character) in value.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
        } else if character == '"' {
            quoted = true;
        } else if character == ',' {
            if part_has_directive(&value[part_start..index]) {
                return true;
            }
            part_start = index + character.len_utf8();
        }
    }

    part_has_directive(&value[part_start..])
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
    fn public_immutable_renders_browser_and_surrogate_control_headers() {
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
            "should render the edge TTL in Surrogate-Control"
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
    fn a_platform_header_renders_cdn_only_policy() {
        let policy = CachePolicy::public_short_with_stale(
            Duration::from_secs(300),
            Duration::from_secs(60),
            Duration::from_secs(86_400),
        );
        let mut headers = HeaderMap::new();

        policy.apply_to_headers(
            &mut headers,
            EdgeCacheHeader::Platform("example-cdn-cache-control"),
        );

        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("public, max-age=300, stale-while-revalidate=60, stale-if-error=86400"),
            "should keep CDN TTL out of browser cache-control when using targeted CDN header"
        );
        assert_eq!(
            headers
                .get("example-cdn-cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("max-age=300, stale-while-revalidate=60, stale-if-error=86400"),
            "should render the policy in the platform's own header"
        );
        assert!(
            headers.get(HEADER_CDN_CACHE_CONTROL).is_none(),
            "should not also emit the generic CDN cache header"
        );
    }

    #[test]
    fn a_declared_platform_header_is_stripped_with_the_standard_ones() {
        crate::platform::declare_platform_cache_headers(&["example-platform-cache-control"]);
        let mut headers = HeaderMap::new();
        headers.insert(
            HEADER_SURROGATE_CONTROL,
            HeaderValue::from_static("max-age=60"),
        );
        headers.insert(
            "example-platform-cache-control",
            HeaderValue::from_static("max-age=60"),
        );

        assert!(is_edge_cache_header_name("Example-Platform-Cache-Control"));
        CacheControlPolicy::NoStorePrivate.apply_to_headers(&mut headers, EdgeCacheHeader::None);

        assert!(
            headers.get(HEADER_SURROGATE_CONTROL).is_none()
                && headers.get("example-platform-cache-control").is_none(),
            "should remove the platform's own header as it removes the standard ones"
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
            "should remove Surrogate-Control for private responses"
        );
        assert!(
            headers.get(HEADER_CDN_CACHE_CONTROL).is_none(),
            "should remove generic CDN cache headers for private responses"
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
            HEADER_CDN_CACHE_CONTROL,
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
                && headers.get(HEADER_CDN_CACHE_CONTROL).is_none(),
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
        assert!(
            !cache_control_value_has_directive("public, ext=\"a,no-store,b\"", "no-store"),
            "should ignore directive-shaped text inside quoted extension values"
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
