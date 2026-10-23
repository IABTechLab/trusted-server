//! Jurisdiction recognition for consent policy and observability.
//!
//! Geography selects the applicable policy, never synthesizes consent. Partial
//! or unrecognized assertions remain unknown when policy requires subdivision
//! recognition.

use core::fmt;

use edgezero_core::geo::CountryCode;

use crate::consent_config::ConsentConfig;
use crate::geo::GeoInfo;

/// The privacy jurisdiction applicable to a request.
///
/// Derived from the user's geolocation and the publisher's configured
/// country/state lists. Used by consent policy, never for consent synthesis.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Jurisdiction {
    /// GDPR applies (EU/EEA/UK per `consent.gdpr.applies_in`).
    Gdpr,
    /// A US state with an active comprehensive privacy law.
    UsState(String),
    /// Geolocation is known but no matching regulation was found.
    NonRegulated,
    /// No geolocation data available — jurisdiction cannot be determined.
    #[default]
    Unknown,
}

impl fmt::Display for Jurisdiction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gdpr => write!(f, "GDPR"),
            Self::UsState(state) => write!(f, "US-{state}"),
            Self::NonRegulated => write!(f, "non-regulated"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Detects the privacy jurisdiction for a request based on geolocation.
///
/// Checks the user's country against `config.gdpr.applies_in`, and for US
/// users checks the region against `config.us_states.privacy_states`.
///
/// Returns [`Jurisdiction::Unknown`] for missing/unrecognized country data or
/// US geography without a recognized subdivision.
#[must_use]
pub fn detect_jurisdiction(geo: Option<&GeoInfo>, config: &ConsentConfig) -> Jurisdiction {
    let Some(geo) = geo else {
        return Jurisdiction::Unknown;
    };
    let Ok(country) = CountryCode::new(&geo.country) else {
        return Jurisdiction::Unknown;
    };

    // Check GDPR countries first (EU/EEA/UK). This ordering also resolves
    // the `DE` code collision: ISO 3166-1 `DE` is Germany (GDPR), while
    // US-Delaware uses ISO 3166-2 `US-DE`. The US state check below only
    // triggers when `country == "US"`, so there is no actual ambiguity.
    if config
        .gdpr
        .applies_in
        .iter()
        .any(|code| code.eq_ignore_ascii_case(country.as_str()))
    {
        return Jurisdiction::Gdpr;
    }

    if country.as_str() == "US" {
        let Some(region) = geo.region.as_deref() else {
            return Jurisdiction::Unknown;
        };
        let region = region.to_ascii_uppercase();
        // Recognition is not the configurable list of states with privacy laws.
        // CloudFront supplies syntax-checked assertions, not a state catalogue.
        if !US_SUBDIVISIONS.contains(&region.as_str()) {
            return Jurisdiction::Unknown;
        }
        if config
            .us_states
            .privacy_states
            .iter()
            .any(|state| state.eq_ignore_ascii_case(&region))
        {
            return Jurisdiction::UsState(region);
        }
    }

    Jurisdiction::NonRegulated
}

// Assigned US state, district and outlying-area subdivision components. This
// recognition list is separate from the publisher's privacy-policy settings.
const US_SUBDIVISIONS: &[&str] = &[
    "AK", "AL", "AR", "AS", "AZ", "CA", "CO", "CT", "DC", "DE", "FL", "GA", "GU", "HI", "IA", "ID",
    "IL", "IN", "KS", "KY", "LA", "MA", "MD", "ME", "MI", "MN", "MO", "MP", "MS", "MT", "NC", "ND",
    "NE", "NH", "NJ", "NM", "NV", "NY", "OH", "OK", "OR", "PA", "PR", "RI", "SC", "SD", "TN", "TX",
    "UM", "UT", "VA", "VI", "VT", "WA", "WI", "WV", "WY",
];

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{Jurisdiction, detect_jurisdiction};
    use crate::consent_config::ConsentConfig;
    use crate::geo::GeoInfo;

    fn make_geo(country: &str, region: Option<&str>) -> GeoInfo {
        GeoInfo::country_region(country, region.map(str::to_owned))
    }

    #[test]
    fn gdpr_detected_for_eu_country() {
        let config = ConsentConfig::default();
        let geo = make_geo("DE", None);
        assert_eq!(
            detect_jurisdiction(Some(&geo), &config),
            Jurisdiction::Gdpr,
            "Germany should trigger GDPR"
        );
    }

    #[test]
    fn gdpr_detected_for_eea_country() {
        let config = ConsentConfig::default();
        let geo = make_geo("NO", None);
        assert_eq!(
            detect_jurisdiction(Some(&geo), &config),
            Jurisdiction::Gdpr,
            "Norway (EEA) should trigger GDPR"
        );
    }

    #[test]
    fn gdpr_detected_for_uk() {
        let config = ConsentConfig::default();
        let geo = make_geo("GB", None);
        assert_eq!(
            detect_jurisdiction(Some(&geo), &config),
            Jurisdiction::Gdpr,
            "UK should trigger GDPR"
        );
    }

    #[test]
    fn us_state_detected_for_california() {
        let config = ConsentConfig::default();
        let geo = make_geo("US", Some("CA"));
        assert_eq!(
            detect_jurisdiction(Some(&geo), &config),
            Jurisdiction::UsState("CA".to_owned()),
            "California should trigger US state privacy"
        );
    }

    #[test]
    fn us_non_privacy_state_is_non_regulated() {
        let config = ConsentConfig::default();
        let geo = make_geo("US", Some("WY"));
        assert_eq!(
            detect_jurisdiction(Some(&geo), &config),
            Jurisdiction::NonRegulated,
            "Wyoming should be non-regulated"
        );
    }

    #[test]
    fn us_no_region_remains_unknown() {
        let config = ConsentConfig::default();
        let geo = make_geo("US", None);
        assert_eq!(
            detect_jurisdiction(Some(&geo), &config),
            Jurisdiction::Unknown,
            "country-only US geography must not imply a non-regulated state"
        );
    }

    #[test]
    fn non_gdpr_non_us_is_non_regulated() {
        let config = ConsentConfig::default();
        let geo = make_geo("JP", None);
        assert_eq!(
            detect_jurisdiction(Some(&geo), &config),
            Jurisdiction::NonRegulated,
            "Japan should be non-regulated"
        );
    }

    #[test]
    fn no_geo_returns_unknown() {
        let config = ConsentConfig::default();
        assert_eq!(
            detect_jurisdiction(None, &config),
            Jurisdiction::Unknown,
            "missing geo should return unknown"
        );
    }

    #[test]
    fn case_insensitive_country_matching() {
        let config = ConsentConfig::default();
        let geo = make_geo("de", None);
        assert_eq!(
            detect_jurisdiction(Some(&geo), &config),
            Jurisdiction::Gdpr,
            "lowercase country code should still match"
        );
    }

    #[test]
    fn unrecognized_subdivisions_remain_unknown_even_if_configured() {
        let mut config = ConsentConfig::default();
        config.us_states.privacy_states.push("ZZ".to_owned());
        for region in ["ZZ", "01", "US-CA", "", " CA"] {
            let geo = make_geo("US", Some(region));
            assert_eq!(
                detect_jurisdiction(Some(&geo), &config),
                Jurisdiction::Unknown
            );
        }
        for country in ["", "XX", "ZZ", "UK", "USA"] {
            assert_eq!(
                detect_jurisdiction(Some(&make_geo(country, None)), &config),
                Jurisdiction::Unknown
            );
        }
    }

    #[test]
    fn partial_geo_never_synthesizes_tcf_or_auction_permission() {
        use crate::consent::{ConsentContext, consent_allows_server_side_auction};
        let config = ConsentConfig::default();
        for geo in [
            make_geo("US", None),
            make_geo("US", Some("ZZ")),
            make_geo("GB", None),
        ] {
            let context = ConsentContext {
                jurisdiction: detect_jurisdiction(Some(&geo), &config),
                ..ConsentContext::default()
            };
            assert!(!consent_allows_server_side_auction(&context));
            assert!(context.tcf.is_none());
        }
        let context = ConsentContext {
            jurisdiction: detect_jurisdiction(Some(&make_geo("US", Some("CA"))), &config),
            ..ConsentContext::default()
        };
        assert!(
            consent_allows_server_side_auction(&context),
            "should retain existing non-GDPR auction admission policy"
        );
        assert!(
            context.tcf.is_none(),
            "should not manufacture identity consent"
        );
    }

    #[test]
    fn display_formatting() {
        assert_eq!(Jurisdiction::Gdpr.to_string(), "GDPR");
        assert_eq!(Jurisdiction::UsState("CA".to_owned()).to_string(), "US-CA");
        assert_eq!(Jurisdiction::NonRegulated.to_string(), "non-regulated");
        assert_eq!(Jurisdiction::Unknown.to_string(), "unknown");
    }
}
