//! Builds an `AuctionObservationContext` from request, geo, and consent inputs.
//!
//! This is the only telemetry code that mints the telemetry id and normalizes
//! the page path. It performs no I/O.

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
    path.chars().take(512).collect()
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
