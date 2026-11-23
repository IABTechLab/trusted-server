//! Jurisdiction recognition for consent policy and observability.
//!
//! Geography selects the applicable policy, never synthesizes consent.
//! Publisher configuration determines the regulated countries and US states.

use core::fmt;

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
/// Returns [`Jurisdiction::Unknown`] when no geo data is available.
#[must_use]
pub fn detect_jurisdiction(geo: Option<&GeoInfo>, config: &ConsentConfig) -> Jurisdiction {
    let Some(geo) = geo else {
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
        .any(|code| code.eq_ignore_ascii_case(&geo.country))
    {
        return Jurisdiction::Gdpr;
    }

    // For US users, check if the region is a state with a privacy law.
    if geo.country.eq_ignore_ascii_case("US")
        && let Some(region) = &geo.region
        && config
            .us_states
            .privacy_states
            .iter()
            .any(|state| state.eq_ignore_ascii_case(region))
    {
        return Jurisdiction::UsState(region.to_uppercase());
    }

    Jurisdiction::NonRegulated
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{Jurisdiction, detect_jurisdiction};
    use crate::consent::{ConsentContext, allows_ec_creation, consent_allows_server_side_auction};
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
    fn us_no_region_is_non_regulated() {
        let config = ConsentConfig::default();
        let geo = make_geo("US", None);
        assert_eq!(
            detect_jurisdiction(Some(&geo), &config),
            Jurisdiction::NonRegulated,
            "should retain the country-only US fallback without inferring a state"
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
    fn publisher_country_list_is_authoritative() {
        let mut config = ConsentConfig::default();
        config.gdpr.applies_in = vec!["xk".to_owned(), "T1".to_owned()];
        for country in ["XK", "t1"] {
            assert_eq!(
                detect_jurisdiction(Some(&make_geo(country, None)), &config),
                Jurisdiction::Gdpr,
                "should apply the publisher's GDPR list without an SDK country catalogue"
            );
        }
        for country in ["DE", "", "XX", "ZZ", "UK", "USA"] {
            assert_eq!(
                detect_jurisdiction(Some(&make_geo(country, None)), &config),
                Jurisdiction::NonRegulated,
                "should retain the baseline fallback for countries outside the publisher's list"
            );
        }
    }

    #[test]
    fn publisher_state_list_is_authoritative() {
        let mut config = ConsentConfig::default();
        config.us_states.privacy_states = vec!["zz".to_owned()];
        assert_eq!(
            detect_jurisdiction(Some(&make_geo("us", Some("ZZ"))), &config),
            Jurisdiction::UsState("ZZ".to_owned()),
            "should use the publisher's state list without an additional subdivision catalogue"
        );
        for region in ["CA", "01", "US-CA", "", " CA"] {
            assert_eq!(
                detect_jurisdiction(Some(&make_geo("US", Some(region))), &config),
                Jurisdiction::NonRegulated,
                "should retain the baseline fallback for regions outside the publisher's list"
            );
        }
    }

    #[test]
    fn geography_never_synthesizes_tcf_consent() {
        let config = ConsentConfig::default();
        for (geo, auction_allowed, ec_allowed) in [
            (make_geo("US", None), true, true),
            (make_geo("US", Some("ZZ")), true, true),
            (make_geo("GB", None), false, false),
            (make_geo("US", Some("CA")), true, false),
        ] {
            let mut context = ConsentContext {
                jurisdiction: detect_jurisdiction(Some(&geo), &config),
                ..ConsentContext::default()
            };
            assert_eq!(
                consent_allows_server_side_auction(&context),
                auction_allowed,
                "should retain baseline auction admission for {geo:?}"
            );
            assert_eq!(
                allows_ec_creation(&context),
                ec_allowed,
                "should retain baseline EC creation policy for {geo:?}"
            );
            assert!(
                context.tcf.is_none() && !context.gdpr_applies,
                "should not manufacture TCF consent or a TCF signal from geography"
            );
            context.gdpr_applies = true;
            assert!(
                !consent_allows_server_side_auction(&context),
                "should still require TCF storage consent when an EU TCF signal is present"
            );
        }
    }

    #[test]
    fn display_formatting() {
        assert_eq!(Jurisdiction::Gdpr.to_string(), "GDPR");
        assert_eq!(Jurisdiction::UsState("CA".to_owned()).to_string(), "US-CA");
        assert_eq!(Jurisdiction::NonRegulated.to_string(), "non-regulated");
        assert_eq!(Jurisdiction::Unknown.to_string(), "unknown");
    }
}
