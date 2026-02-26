//! Jurisdiction detection for consent observability.
//!
//! Determines the applicable privacy regime based on geolocation data and
//! publisher configuration. Used for **logging and monitoring only** — the
//! detected jurisdiction never causes consent to be synthesized (see proposal
//! Key Decision #3).

use core::fmt;

use crate::consent_config::ConsentConfig;
use crate::geo::GeoInfo;

/// The privacy jurisdiction applicable to a request.
///
/// Derived from the user's geolocation and the publisher's configured
/// country/state lists. Used for observability — not for consent synthesis.
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
    let geo = match geo {
        Some(g) => g,
        None => return Jurisdiction::Unknown,
    };

    // Check GDPR countries first (EU/EEA/UK).
    if config
        .gdpr
        .applies_in
        .iter()
        .any(|code| code.eq_ignore_ascii_case(&geo.country))
    {
        return Jurisdiction::Gdpr;
    }

    // For US users, check if the region is a state with a privacy law.
    if geo.country.eq_ignore_ascii_case("US") {
        if let Some(region) = &geo.region {
            if config
                .us_states
                .privacy_states
                .iter()
                .any(|state| state.eq_ignore_ascii_case(region))
            {
                return Jurisdiction::UsState(region.to_uppercase());
            }
        }
    }

    Jurisdiction::NonRegulated
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{detect_jurisdiction, Jurisdiction};
    use crate::consent_config::ConsentConfig;
    use crate::geo::GeoInfo;

    fn make_geo(country: &str, region: Option<&str>) -> GeoInfo {
        GeoInfo {
            city: "Test".to_owned(),
            country: country.to_owned(),
            continent: "Test".to_owned(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: region.map(str::to_owned),
        }
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
            "US without region should be non-regulated"
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
    fn display_formatting() {
        assert_eq!(Jurisdiction::Gdpr.to_string(), "GDPR");
        assert_eq!(Jurisdiction::UsState("CA".to_owned()).to_string(), "US-CA");
        assert_eq!(Jurisdiction::NonRegulated.to_string(), "non-regulated");
        assert_eq!(Jurisdiction::Unknown.to_string(), "unknown");
    }
}
