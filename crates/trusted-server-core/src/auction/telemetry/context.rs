//! Builds an `AuctionObservationContext` from request, geo, and consent inputs.
//!
//! This is the only telemetry code that mints the telemetry id and normalizes
//! the page path. It performs no I/O.

use std::borrow::Cow;

use uuid::Uuid;

use crate::auction::telemetry::types::{AuctionObservationContext, AuctionSource};
use crate::consent::ConsentContext;
use crate::platform::GeoInfo;

/// Build a PII-free observation context for one auction.
///
/// `is_mobile` and `is_known_browser` use `0`/`1`/`2` (`2` = unknown); a later
/// plan threads real device signals. `consent` is optional because a
/// non-regulated auction may carry no consent context.
#[must_use]
pub fn build_observation_context(
    source: AuctionSource,
    publisher_domain: &str,
    page_url: Option<&str>,
    geo: Option<&GeoInfo>,
    consent: Option<&ConsentContext>,
    is_mobile: u8,
    is_known_browser: u8,
) -> AuctionObservationContext {
    AuctionObservationContext {
        auction_id: Uuid::new_v4(),
        source,
        publisher_domain: publisher_domain.to_string(),
        page_path: page_url
            .map(normalize_page_path)
            .unwrap_or_else(|| "/".to_string()),
        country: geo.map(|info| info.country.clone()).unwrap_or_default(),
        region: geo.and_then(|info| info.region.clone()),
        is_mobile,
        is_known_browser,
        gdpr_applies: consent.is_some_and(|context| context.gdpr_applies),
        consent_present: consent.is_some_and(|context| !context.is_empty()),
    }
}

const MAX_PAGE_PATH_CHARS: usize = 512;
const MAX_PAGE_PATH_INPUT_CHARS: usize = 2048;
const REDACTED_PATH_SEGMENT: &str = "{redacted}";
const SENSITIVE_PARENT_SEGMENTS: &[&str] = &[
    "account", "accounts", "invite", "invites", "member", "members", "order", "orders", "profile",
    "profiles", "reset", "session", "sessions", "user", "users",
];

/// Reduce a page URL or path to a bounded path with no scheme, host, query, or
/// fragment. Empty or path-less inputs normalize to `/`.
fn normalize_page_path(page_url: &str) -> String {
    let without_fragment = page_url.split('#').next().unwrap_or("");
    let without_query = without_fragment.split('?').next().unwrap_or("");
    let path = match without_query.find("://") {
        Some(scheme_end) => {
            let after_scheme = &without_query[scheme_end + 3..];
            match after_scheme.find('/') {
                Some(slash) => &after_scheme[slash..],
                None => "/",
            }
        }
        None => without_query,
    };
    let path = if path.is_empty() { "/" } else { path };
    let bounded_path: String = path.chars().take(MAX_PAGE_PATH_INPUT_CHARS).collect();
    let normalized_path = if bounded_path.starts_with('/') {
        bounded_path
    } else {
        format!("/{bounded_path}")
    };
    redact_sensitive_path_segments(&normalized_path)
        .chars()
        .take(MAX_PAGE_PATH_CHARS)
        .collect()
}

fn redact_sensitive_path_segments(path: &str) -> String {
    let mut redacted = String::with_capacity(path.len().min(MAX_PAGE_PATH_CHARS));
    let mut previous_segment_is_sensitive_parent = false;
    for (index, segment) in path.split('/').enumerate() {
        if index > 0 {
            redacted.push('/');
        }
        if previous_segment_is_sensitive_parent && !segment.is_empty() {
            redacted.push_str(REDACTED_PATH_SEGMENT);
        } else {
            redacted.push_str(&redact_path_segment(segment));
        }
        previous_segment_is_sensitive_parent = is_sensitive_parent_segment(segment);
    }
    if redacted.is_empty() {
        "/".to_string()
    } else {
        redacted
    }
}

fn redact_path_segment(segment: &str) -> Cow<'_, str> {
    if should_redact_path_segment(segment) {
        Cow::Borrowed(REDACTED_PATH_SEGMENT)
    } else {
        Cow::Borrowed(segment)
    }
}

fn should_redact_path_segment(segment: &str) -> bool {
    let decoded = urlencoding::decode(segment).unwrap_or(Cow::Borrowed(segment));
    let segment = decoded.trim();
    if segment.is_empty() {
        return false;
    }
    segment.contains('@')
        || segment.chars().all(|ch| ch.is_ascii_digit())
        || uuid::Uuid::parse_str(segment).is_ok()
        || looks_like_hex_token(segment)
        || looks_like_high_entropy_token(segment)
}

fn is_sensitive_parent_segment(segment: &str) -> bool {
    let decoded = urlencoding::decode(segment).unwrap_or(Cow::Borrowed(segment));
    let normalized = decoded.trim().to_ascii_lowercase();
    SENSITIVE_PARENT_SEGMENTS.contains(&normalized.as_str())
}

fn looks_like_hex_token(segment: &str) -> bool {
    segment.len() >= 16 && segment.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn looks_like_high_entropy_token(segment: &str) -> bool {
    if segment.len() < 20 {
        return false;
    }
    let mut has_alpha = false;
    let mut has_digit = false;
    let mut token_chars = 0usize;
    let mut separators = 0usize;
    for ch in segment.chars() {
        if ch.is_ascii_alphabetic() {
            has_alpha = true;
            token_chars += 1;
        } else if ch.is_ascii_digit() {
            has_digit = true;
            token_chars += 1;
        } else if matches!(ch, '-' | '_' | '=' | '.') {
            separators += 1;
        } else {
            return false;
        }
    }
    has_alpha && has_digit && token_chars >= 16 && separators <= 4
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::telemetry::types::AuctionSource;
    use crate::consent::ConsentContext;
    use crate::platform::GeoInfo;

    fn geo() -> GeoInfo {
        GeoInfo {
            city: "Springfield".to_string(),
            country: "US".to_string(),
            continent: "NA".to_string(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: Some("CA".to_string()),
            asn: None,
        }
    }

    #[test]
    fn normalizes_full_url_to_path_without_query_or_fragment() {
        assert_eq!(
            normalize_page_path("https://www.example.com/news/article?utm=x#top"),
            "/news/article",
            "should keep only the path"
        );
        assert_eq!(
            normalize_page_path("/already/a/path?q=1"),
            "/already/a/path",
            "should strip the query from a bare path"
        );
        assert_eq!(
            normalize_page_path("https://example.com"),
            "/",
            "a URL with no path normalizes to /"
        );
        assert_eq!(normalize_page_path(""), "/", "empty input normalizes to /");
    }

    #[test]
    fn redacts_sensitive_dynamic_path_segments() {
        assert_eq!(
            normalize_page_path("https://example.com/account/12345/orders"),
            "/account/{redacted}/orders",
            "should redact numeric identifiers"
        );
        assert_eq!(
            normalize_page_path("/reset/550e8400-e29b-41d4-a716-446655440000"),
            "/reset/{redacted}",
            "should redact UUID path segments"
        );
        assert_eq!(
            normalize_page_path("/reset/3xY9AbCDef0123456789Z"),
            "/reset/{redacted}",
            "should redact high-entropy token path segments"
        );
        assert_eq!(
            normalize_page_path("/users/user%40example.com/profile"),
            "/users/{redacted}/profile",
            "should redact percent-encoded email-like path segments"
        );
        assert_eq!(
            normalize_page_path("/users/john-smith/profile"),
            "/users/{redacted}/profile",
            "should redact handle-like segments after sensitive route parents"
        );
    }

    #[test]
    fn preserves_ordinary_slug_path_segments() {
        assert_eq!(
            normalize_page_path("blog/how-to-build-fast-websites"),
            "/blog/how-to-build-fast-websites",
            "should keep ordinary route slugs and add a leading slash"
        );
    }

    #[test]
    fn builds_context_from_geo_and_consent() {
        let consent = ConsentContext {
            gdpr_applies: true,
            ..ConsentContext::default()
        };
        let ctx = build_observation_context(
            AuctionSource::AuctionApi,
            "example.com",
            Some("https://example.com/p?x=1"),
            Some(&geo()),
            Some(&consent),
            1,
            1,
        );
        assert_eq!(
            ctx.source,
            AuctionSource::AuctionApi,
            "should carry the source"
        );
        assert_eq!(
            ctx.publisher_domain, "example.com",
            "should carry the domain"
        );
        assert_eq!(ctx.page_path, "/p", "should carry the normalized path");
        assert_eq!(ctx.country, "US", "should carry country from geo");
        assert_eq!(
            ctx.region.as_deref(),
            Some("CA"),
            "should carry region from geo"
        );
        assert!(ctx.gdpr_applies, "should carry gdpr_applies from consent");
        assert!(
            !ctx.consent_present,
            "a default consent is empty so consent_present is false"
        );
        assert!(!ctx.auction_id.is_nil(), "should mint a fresh telemetry id");
    }

    #[test]
    fn defaults_country_and_consent_when_absent() {
        let ctx = build_observation_context(
            AuctionSource::AuctionApi,
            "example.com",
            None,
            None,
            None,
            2,
            2,
        );
        assert_eq!(ctx.country, "", "no geo means empty country");
        assert!(ctx.region.is_none(), "no geo means no region");
        assert_eq!(ctx.page_path, "/", "no page url normalizes to /");
        assert!(!ctx.gdpr_applies, "no consent means gdpr_applies false");
        assert!(
            !ctx.consent_present,
            "no consent means consent_present false"
        );
    }
}
