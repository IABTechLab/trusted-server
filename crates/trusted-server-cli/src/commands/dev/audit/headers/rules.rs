//! Content-type taxonomy and per-group cacheability rules.
//!
//! The audit classifies each origin response into a [`ContentTypeGroup`] by its
//! `Content-Type`, then evaluates the cache-related response headers against the
//! expected posture for that group. Evaluation is pure — it takes already-parsed
//! header values (see [`ResponseHeaders`]) and returns a list of per-header
//! [`HeaderVerdict`]s, so it is exercised entirely by unit tests without any I/O.

use std::collections::BTreeSet;

use serde::Serialize;

/// One year in seconds — the `max-age` floor for immutable, hashed assets.
const IMMUTABLE_MAX_AGE: u64 = 31_536_000;

/// One day in seconds — the `max-age` floor for creatives/images.
const IMAGE_MIN_MAX_AGE: u64 = 86_400;

/// The content-type family a response belongs to.
///
/// Classification strips `Content-Type` parameters (`text/html; charset=utf-8`
/// becomes `text/html`) and lowercases the value before matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum ContentTypeGroup {
    /// `text/html` — personalized, consent-sensitive.
    Html,
    /// Hashed, immutable script bundles.
    JavaScript,
    /// `image/*` creatives.
    Image,
    /// CSS and fonts.
    StaticAsset,
    /// `application/json` real-time bidding responses.
    RtbJson,
    /// Everything else — reported for information, never evaluated.
    Other,
}

impl ContentTypeGroup {
    /// Classifies a raw `Content-Type` header value into a group.
    ///
    /// Parameters after the first `;` are ignored and matching is
    /// case-insensitive, so `text/html; charset=utf-8` maps to
    /// [`ContentTypeGroup::Html`].
    pub(crate) fn classify(content_type: &str) -> Self {
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or(content_type)
            .trim()
            .to_ascii_lowercase();

        match mime.as_str() {
            "text/html" => Self::Html,
            "application/javascript" | "text/javascript" | "application/x-javascript" => {
                Self::JavaScript
            }
            "application/json" => Self::RtbJson,
            "text/css" => Self::StaticAsset,
            other if other.starts_with("image/") => Self::Image,
            other if other.starts_with("font/") || other.starts_with("application/font-") => {
                Self::StaticAsset
            }
            _ => Self::Other,
        }
    }

    /// Whether this group is expected to be CDN-cacheable, and therefore
    /// evaluated for `Surrogate-Key` purge coverage. Non-cacheable groups
    /// (`Html`, `RtbJson`) never need selective purge, so the check is skipped.
    fn is_cacheable(self) -> bool {
        matches!(self, Self::JavaScript | Self::Image | Self::StaticAsset)
    }

    /// A short, stable display label for terminal output.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Html => "HTML",
            Self::JavaScript => "JS Bundle",
            Self::Image => "Image",
            Self::StaticAsset => "Static Asset",
            Self::RtbJson => "RTB/JSON",
            Self::Other => "Other",
        }
    }
}

/// The outcome of a single header (or group) check.
///
/// Unit variants only — the human-readable message lives on
/// [`HeaderVerdict::recommendation`], and serializes to a bare string
/// (`"Pass"`, `"Warn"`, `"Fail"`) for the `--json` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum Verdict {
    /// Header posture matches the expected value.
    Pass,
    /// Suboptimal but not incorrect — hit ratio or purge ergonomics suffer.
    Warn,
    /// Incorrect — risks stale or cross-user content.
    Fail,
}

impl Verdict {
    /// Combines two verdicts into the more severe of the two.
    fn worst(self, other: Self) -> Self {
        match (self, other) {
            (Self::Fail, _) | (_, Self::Fail) => Self::Fail,
            (Self::Warn, _) | (_, Self::Warn) => Self::Warn,
            _ => Self::Pass,
        }
    }

    /// Folds an iterator of verdicts into a single worst-of rollup, defaulting
    /// to [`Verdict::Pass`] when empty.
    pub(crate) fn rollup(verdicts: impl IntoIterator<Item = Self>) -> Self {
        verdicts.into_iter().fold(Self::Pass, Self::worst)
    }
}

/// A single header's evaluation result within a content-type group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HeaderVerdict {
    /// The header this verdict is about (e.g. `Cache-Control`).
    pub(crate) header: String,
    /// Pass / warn / fail for this header.
    pub(crate) verdict: Verdict,
    /// The observed header value, if the header was present.
    pub(crate) actual: Option<String>,
    /// The expected posture, described briefly.
    pub(crate) expected: String,
    /// What to change to satisfy the rule (empty when passing).
    pub(crate) recommendation: String,
}

impl HeaderVerdict {
    fn pass(header: &str, actual: Option<String>, expected: &str) -> Self {
        Self {
            header: header.to_owned(),
            verdict: Verdict::Pass,
            actual,
            expected: expected.to_owned(),
            recommendation: String::new(),
        }
    }

    fn flagged(
        header: &str,
        verdict: Verdict,
        actual: Option<String>,
        expected: &str,
        recommendation: &str,
    ) -> Self {
        Self {
            header: header.to_owned(),
            verdict,
            actual,
            expected: expected.to_owned(),
            recommendation: recommendation.to_owned(),
        }
    }
}

/// The parsed, cache-relevant response headers for one origin response.
///
/// Extracted from the transport layer in `fetch.rs` so this module stays free
/// of any HTTP-client types and is unit-testable in isolation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ResponseHeaders {
    /// `Content-Type` — drives group classification.
    pub(crate) content_type: Option<String>,
    /// `Cache-Control`.
    pub(crate) cache_control: Option<String>,
    /// `Surrogate-Control` (Fastly CDN-only directives).
    pub(crate) surrogate_control: Option<String>,
    /// `Surrogate-Key` (Fastly purge tags).
    pub(crate) surrogate_key: Option<String>,
    /// `Vary`.
    pub(crate) vary: Option<String>,
    /// `ETag`.
    pub(crate) etag: Option<String>,
}

/// A parsed `Cache-Control` (or `Surrogate-Control`) value.
///
/// Directives are lowercased tokens; `max-age` and `s-maxage` are captured
/// separately because Fastly resolves the shared-cache TTL from
/// `Surrogate-Control` first, then `s-maxage`, then `max-age`.
struct CacheDirectives {
    tokens: BTreeSet<String>,
    max_age: Option<u64>,
    s_maxage: Option<u64>,
}

impl CacheDirectives {
    fn parse(value: &str) -> Self {
        let mut tokens = BTreeSet::new();
        let mut max_age = None;
        let mut s_maxage = None;

        for raw in value.split(',') {
            let token = raw.trim().to_ascii_lowercase();
            if token.is_empty() {
                continue;
            }
            if let Some((name, seconds)) = token.split_once('=') {
                let seconds = seconds.trim_matches('"').parse::<u64>().ok();
                match name.trim() {
                    "max-age" => max_age = seconds,
                    "s-maxage" => s_maxage = seconds,
                    _ => {}
                }
                tokens.insert(name.trim().to_owned());
            } else {
                tokens.insert(token);
            }
        }

        Self {
            tokens,
            max_age,
            s_maxage,
        }
    }

    fn has(&self, directive: &str) -> bool {
        self.tokens.contains(directive)
    }
}

/// Whether a `Vary` value contains a hit-ratio-destroying token.
fn vary_has_high_cardinality(vary: &str) -> bool {
    vary.split(',').any(|token| {
        let token = token.trim().to_ascii_lowercase();
        token == "user-agent" || token == "cookie"
    })
}

/// Evaluates the cache posture of a single response for its content-type group.
///
/// Returns one [`HeaderVerdict`] per header the group cares about.
/// [`ContentTypeGroup::Other`] is never evaluated and yields an empty list.
pub(crate) fn evaluate(group: ContentTypeGroup, headers: &ResponseHeaders) -> Vec<HeaderVerdict> {
    match group {
        ContentTypeGroup::Html => evaluate_html(headers),
        ContentTypeGroup::JavaScript => evaluate_immutable(group, headers, "JS bundles"),
        ContentTypeGroup::Image => evaluate_image(headers),
        ContentTypeGroup::StaticAsset => evaluate_immutable(group, headers, "static assets"),
        ContentTypeGroup::RtbJson => evaluate_rtb(headers),
        ContentTypeGroup::Other => Vec::new(),
    }
}

/// HTML must not be shared across users: `no-store` alone is sufficient
/// (RFC 9111 §5.2.2.5), or `private` combined with `no-cache`.
fn evaluate_html(headers: &ResponseHeaders) -> Vec<HeaderVerdict> {
    let mut verdicts = Vec::new();

    let expected = "no-store, or private + no-cache";
    match headers.cache_control.as_deref() {
        Some(value) => {
            let directives = CacheDirectives::parse(value);
            let safe = directives.has("no-store")
                || (directives.has("private") && directives.has("no-cache"));
            if safe {
                verdicts.push(HeaderVerdict::pass(
                    "Cache-Control",
                    Some(value.to_owned()),
                    expected,
                ));
            } else {
                verdicts.push(HeaderVerdict::flagged(
                    "Cache-Control",
                    Verdict::Fail,
                    Some(value.to_owned()),
                    expected,
                    "HTML served without no-store (or private + no-cache) risks sharing personalized content across users",
                ));
            }
        }
        None => verdicts.push(HeaderVerdict::flagged(
            "Cache-Control",
            Verdict::Fail,
            None,
            expected,
            "HTML has no Cache-Control; set `no-store` to prevent shared caching of personalized content",
        )),
    }

    verdicts.extend(evaluate_vary(headers, /* flag_wildcard */ true));
    verdicts.extend(evaluate_surrogate_no_store(
        headers,
        "CDN may cache personalized HTML; set Surrogate-Control to `no-store` or `private`",
    ));

    verdicts
}

/// JS bundles and static assets share the immutable, long-lived posture.
fn evaluate_immutable(
    group: ContentTypeGroup,
    headers: &ResponseHeaders,
    noun: &str,
) -> Vec<HeaderVerdict> {
    let mut verdicts = Vec::new();

    let expected = "public, max-age>=31536000, immutable";
    match headers.cache_control.as_deref() {
        Some(value) => {
            let directives = CacheDirectives::parse(value);
            let immutable = directives.has("public")
                && directives.has("immutable")
                && directives
                    .max_age
                    .is_some_and(|age| age >= IMMUTABLE_MAX_AGE);
            if immutable {
                verdicts.push(HeaderVerdict::pass(
                    "Cache-Control",
                    Some(value.to_owned()),
                    expected,
                ));
            } else {
                verdicts.push(HeaderVerdict::flagged(
                    "Cache-Control",
                    Verdict::Warn,
                    Some(value.to_owned()),
                    expected,
                    &format!("Hashed {noun} should use long-lived immutable caching"),
                ));
            }
        }
        None => verdicts.push(HeaderVerdict::flagged(
            "Cache-Control",
            Verdict::Warn,
            None,
            expected,
            &format!("{noun} have no Cache-Control; set `public, max-age=31536000, immutable`"),
        )),
    }

    if matches!(group, ContentTypeGroup::JavaScript) {
        verdicts.push(evaluate_etag(headers));
    }
    verdicts.extend(evaluate_surrogate_key(group, headers));
    verdicts.extend(evaluate_vary(headers, /* flag_wildcard */ false));

    verdicts
}

/// Images should be publicly cacheable for at least a day, and the CDN TTL
/// should not undercut the browser TTL.
fn evaluate_image(headers: &ResponseHeaders) -> Vec<HeaderVerdict> {
    let mut verdicts = Vec::new();

    let expected = "public, max-age>=86400";
    let cache_control = headers.cache_control.as_deref().map(CacheDirectives::parse);
    match (&headers.cache_control, &cache_control) {
        (Some(value), Some(directives)) => {
            let cacheable = directives.has("public")
                && directives
                    .max_age
                    .is_some_and(|age| age >= IMAGE_MIN_MAX_AGE);
            if cacheable {
                verdicts.push(HeaderVerdict::pass(
                    "Cache-Control",
                    Some(value.clone()),
                    expected,
                ));
            } else {
                verdicts.push(HeaderVerdict::flagged(
                    "Cache-Control",
                    Verdict::Warn,
                    Some(value.clone()),
                    expected,
                    "Images re-fetched too frequently; set `public, max-age=86400` or longer",
                ));
            }
        }
        _ => verdicts.push(HeaderVerdict::flagged(
            "Cache-Control",
            Verdict::Warn,
            None,
            expected,
            "Images have no Cache-Control; set `public, max-age=86400` or longer",
        )),
    }

    // The CDN TTL (Surrogate-Control max-age, else s-maxage) should be at least
    // the browser TTL, so the edge does not re-fetch more often than clients.
    if let Some(surrogate) = headers.surrogate_control.as_deref() {
        let surrogate_directives = CacheDirectives::parse(surrogate);
        let cdn_ttl = surrogate_directives
            .max_age
            .or(surrogate_directives.s_maxage);
        let browser_ttl = cache_control
            .as_ref()
            .and_then(|directives| directives.max_age);
        if let (Some(cdn), Some(browser)) = (cdn_ttl, browser_ttl) {
            if cdn < browser {
                verdicts.push(HeaderVerdict::flagged(
                    "Surrogate-Control",
                    Verdict::Warn,
                    Some(surrogate.to_owned()),
                    "CDN max-age >= browser max-age",
                    "CDN caching shorter than browser caching; raise Surrogate-Control max-age (or s-maxage)",
                ));
            } else {
                verdicts.push(HeaderVerdict::pass(
                    "Surrogate-Control",
                    Some(surrogate.to_owned()),
                    "CDN max-age >= browser max-age",
                ));
            }
        }
    }

    verdicts.extend(evaluate_surrogate_key(ContentTypeGroup::Image, headers));
    verdicts.extend(evaluate_vary(headers, /* flag_wildcard */ false));

    verdicts
}

/// RTB/JSON must never be cached: `no-store` is required (RFC 9111 §5.2.2.5).
fn evaluate_rtb(headers: &ResponseHeaders) -> Vec<HeaderVerdict> {
    let mut verdicts = Vec::new();

    let expected = "no-store";
    match headers.cache_control.as_deref() {
        Some(value) if CacheDirectives::parse(value).has("no-store") => {
            verdicts.push(HeaderVerdict::pass(
                "Cache-Control",
                Some(value.to_owned()),
                expected,
            ));
        }
        Some(value) => verdicts.push(HeaderVerdict::flagged(
            "Cache-Control",
            Verdict::Fail,
            Some(value.to_owned()),
            expected,
            "RTB responses cached means stale bids served to users; set `no-store`",
        )),
        None => verdicts.push(HeaderVerdict::flagged(
            "Cache-Control",
            Verdict::Fail,
            None,
            expected,
            "RTB responses have no Cache-Control; set `no-store`",
        )),
    }

    verdicts.extend(evaluate_surrogate_no_store(
        headers,
        "CDN may cache RTB responses; set Surrogate-Control to `no-store`",
    ));

    verdicts
}

/// `Surrogate-Control`, when present, must not permit CDN caching for
/// non-cacheable groups (HTML, RTB).
fn evaluate_surrogate_no_store(
    headers: &ResponseHeaders,
    recommendation: &str,
) -> Option<HeaderVerdict> {
    let value = headers.surrogate_control.as_deref()?;
    let directives = CacheDirectives::parse(value);
    let safe = directives.has("no-store") || directives.has("private");
    Some(if safe {
        HeaderVerdict::pass(
            "Surrogate-Control",
            Some(value.to_owned()),
            "no-store or private",
        )
    } else {
        HeaderVerdict::flagged(
            "Surrogate-Control",
            Verdict::Fail,
            Some(value.to_owned()),
            "no-store or private",
            recommendation,
        )
    })
}

/// `Surrogate-Key` enables targeted CDN purge; absence is a warning on
/// cacheable groups only.
fn evaluate_surrogate_key(
    group: ContentTypeGroup,
    headers: &ResponseHeaders,
) -> Option<HeaderVerdict> {
    if !group.is_cacheable() {
        return None;
    }
    Some(match headers.surrogate_key.as_deref() {
        Some(value) => HeaderVerdict::pass("Surrogate-Key", Some(value.to_owned()), "present"),
        None => HeaderVerdict::flagged(
            "Surrogate-Key",
            Verdict::Warn,
            None,
            "present",
            "Without Surrogate-Key, this content type can't be selectively purged from the CDN — full-cache purges required",
        ),
    })
}

/// `ETag` enables conditional revalidation; absence is a warning.
fn evaluate_etag(headers: &ResponseHeaders) -> HeaderVerdict {
    match headers.etag.as_deref() {
        Some(value) => HeaderVerdict::pass("ETag", Some(value.to_owned()), "present"),
        None => HeaderVerdict::flagged(
            "ETag",
            Verdict::Warn,
            None,
            "present",
            "Missing ETag means no conditional revalidation",
        ),
    }
}

/// `Vary` checks: `*` disables all caching (HTML only), and `User-Agent` /
/// `Cookie` destroy the CDN hit ratio on any group.
fn evaluate_vary(headers: &ResponseHeaders, flag_wildcard: bool) -> Option<HeaderVerdict> {
    let value = headers.vary.as_deref()?;

    if flag_wildcard && value.split(',').any(|token| token.trim() == "*") {
        return Some(HeaderVerdict::flagged(
            "Vary",
            Verdict::Warn,
            Some(value.to_owned()),
            "no `*`",
            "Vary: * disables all caching including the CDN",
        ));
    }

    if vary_has_high_cardinality(value) {
        return Some(HeaderVerdict::flagged(
            "Vary",
            Verdict::Warn,
            Some(value.to_owned()),
            "no User-Agent or Cookie",
            "Vary on User-Agent/Cookie destroys the CDN hit ratio",
        ));
    }

    Some(HeaderVerdict::pass(
        "Vary",
        Some(value.to_owned()),
        "no `*`, User-Agent, or Cookie",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with(cache_control: Option<&str>) -> ResponseHeaders {
        ResponseHeaders {
            cache_control: cache_control.map(str::to_owned),
            ..ResponseHeaders::default()
        }
    }

    fn verdict_for(verdicts: &[HeaderVerdict], header: &str) -> Verdict {
        verdicts
            .iter()
            .find(|verdict| verdict.header == header)
            .unwrap_or_else(|| panic!("should have a verdict for {header}"))
            .verdict
    }

    #[test]
    fn classify_strips_parameters_and_lowercases() {
        assert_eq!(
            ContentTypeGroup::classify("text/HTML; charset=utf-8"),
            ContentTypeGroup::Html,
            "should ignore charset parameter and case"
        );
        assert_eq!(
            ContentTypeGroup::classify("image/png"),
            ContentTypeGroup::Image,
            "should match image/* prefix"
        );
        assert_eq!(
            ContentTypeGroup::classify("font/woff2"),
            ContentTypeGroup::StaticAsset,
            "should treat fonts as static assets"
        );
        assert_eq!(
            ContentTypeGroup::classify("application/font-woff"),
            ContentTypeGroup::StaticAsset,
            "should treat legacy font MIME as static assets"
        );
        assert_eq!(
            ContentTypeGroup::classify("application/octet-stream"),
            ContentTypeGroup::Other,
            "should fall through to Other"
        );
    }

    #[test]
    fn html_passes_on_no_store_alone() {
        let verdicts = evaluate(ContentTypeGroup::Html, &headers_with(Some("no-store")));
        assert_eq!(
            verdict_for(&verdicts, "Cache-Control"),
            Verdict::Pass,
            "no-store alone should satisfy RFC 9111 for HTML"
        );
    }

    #[test]
    fn html_passes_on_private_no_cache() {
        let verdicts = evaluate(
            ContentTypeGroup::Html,
            &headers_with(Some("private, no-cache")),
        );
        assert_eq!(
            verdict_for(&verdicts, "Cache-Control"),
            Verdict::Pass,
            "private + no-cache should satisfy HTML"
        );
    }

    #[test]
    fn html_fails_on_public_cacheable() {
        let verdicts = evaluate(
            ContentTypeGroup::Html,
            &headers_with(Some("public, max-age=3600")),
        );
        assert_eq!(
            verdict_for(&verdicts, "Cache-Control"),
            Verdict::Fail,
            "publicly cacheable HTML should fail"
        );
    }

    #[test]
    fn rtb_passes_on_no_store_without_private() {
        let verdicts = evaluate(ContentTypeGroup::RtbJson, &headers_with(Some("no-store")));
        assert_eq!(
            verdict_for(&verdicts, "Cache-Control"),
            Verdict::Pass,
            "bare no-store should pass for RTB"
        );
    }

    #[test]
    fn javascript_warns_without_immutable() {
        let verdicts = evaluate(
            ContentTypeGroup::JavaScript,
            &headers_with(Some("public, max-age=31536000")),
        );
        assert_eq!(
            verdict_for(&verdicts, "Cache-Control"),
            Verdict::Warn,
            "missing immutable should warn"
        );
    }

    #[test]
    fn javascript_passes_on_full_immutable_posture() {
        let verdicts = evaluate(
            ContentTypeGroup::JavaScript,
            &ResponseHeaders {
                cache_control: Some("public, max-age=31536000, immutable".to_owned()),
                surrogate_key: Some("js".to_owned()),
                etag: Some("\"abc\"".to_owned()),
                ..ResponseHeaders::default()
            },
        );
        assert_eq!(
            Verdict::rollup(verdicts.iter().map(|verdict| verdict.verdict)),
            Verdict::Pass,
            "full immutable posture with ETag and Surrogate-Key should pass"
        );
    }

    #[test]
    fn surrogate_key_skipped_for_non_cacheable_groups() {
        let verdicts = evaluate(ContentTypeGroup::Html, &headers_with(Some("no-store")));
        assert!(
            !verdicts
                .iter()
                .any(|verdict| verdict.header == "Surrogate-Key"),
            "HTML should not be checked for Surrogate-Key"
        );
    }

    #[test]
    fn vary_wildcard_warns_only_for_html() {
        let html = evaluate(
            ContentTypeGroup::Html,
            &ResponseHeaders {
                cache_control: Some("no-store".to_owned()),
                vary: Some("*".to_owned()),
                ..ResponseHeaders::default()
            },
        );
        assert_eq!(
            verdict_for(&html, "Vary"),
            Verdict::Warn,
            "Vary: * should warn on HTML"
        );
    }

    #[test]
    fn vary_high_cardinality_warns_on_assets() {
        let verdicts = evaluate(
            ContentTypeGroup::JavaScript,
            &ResponseHeaders {
                cache_control: Some("public, max-age=31536000, immutable".to_owned()),
                surrogate_key: Some("js".to_owned()),
                etag: Some("\"abc\"".to_owned()),
                vary: Some("User-Agent".to_owned()),
                ..ResponseHeaders::default()
            },
        );
        assert_eq!(
            verdict_for(&verdicts, "Vary"),
            Verdict::Warn,
            "Vary: User-Agent should warn on JS assets"
        );
    }

    #[test]
    fn image_warns_when_cdn_ttl_below_browser_ttl() {
        let verdicts = evaluate(
            ContentTypeGroup::Image,
            &ResponseHeaders {
                cache_control: Some("public, max-age=86400".to_owned()),
                surrogate_control: Some("max-age=60".to_owned()),
                surrogate_key: Some("img".to_owned()),
                ..ResponseHeaders::default()
            },
        );
        assert_eq!(
            verdict_for(&verdicts, "Surrogate-Control"),
            Verdict::Warn,
            "CDN TTL below browser TTL should warn"
        );
    }

    #[test]
    fn other_group_is_never_evaluated() {
        let verdicts = evaluate(ContentTypeGroup::Other, &headers_with(Some("whatever")));
        assert!(verdicts.is_empty(), "Other should yield no verdicts");
    }

    #[test]
    fn rollup_is_worst_of() {
        assert_eq!(
            Verdict::rollup([Verdict::Pass, Verdict::Warn, Verdict::Fail]),
            Verdict::Fail,
            "any fail should dominate"
        );
        assert_eq!(
            Verdict::rollup([Verdict::Pass, Verdict::Warn]),
            Verdict::Warn,
            "warn should dominate pass"
        );
        assert_eq!(
            Verdict::rollup(std::iter::empty()),
            Verdict::Pass,
            "empty should default to pass"
        );
    }
}
