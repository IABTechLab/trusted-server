//! Jurisdiction detection: which privacy regime a request falls under.
//!
//! The regime comes from the request's location, resolved through the same
//! place tree in `permissions.yaml` that decides the permission baseline, so
//! one file states the policy for both. The detected jurisdiction never causes
//! consent to be synthesized (see proposal Key Decision #3), but it is not
//! only observability either: the server-side auction gate
//! (`consent_allows_server_side_auction`) fails closed on a GDPR or unknown
//! jurisdiction, and a US state jurisdiction is what lets a GPC header be
//! turned into a US Privacy string.

use core::fmt;

use crate::geo::GeoInfo;
use crate::permissions::PermissionMaps;

/// The privacy jurisdiction applicable to a request.
///
/// Resolved from the request's place through the `rules` tree in
/// `permissions.yaml`, where each node may name the jurisdiction that applies
/// there and a node without one inherits from the node above it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Jurisdiction {
    /// GDPR applies (the EU, EEA and UK regime).
    Gdpr,
    /// A US state with an active comprehensive privacy law.
    UsState(String),
    /// The place is known but no matching regulation was found.
    NonRegulated,
    /// No jurisdiction could be determined, for example after a failed geo
    /// lookup.
    #[default]
    Unknown,
}

impl Jurisdiction {
    /// Parses a `jurisdiction:` value written in the `permissions.yaml`
    /// `rules` tree.
    ///
    /// The vocabulary covers exactly the states this type can represent, so a
    /// policy owner cannot write a jurisdiction the consent code has no way of
    /// applying:
    ///
    /// - `gdpr` for [`Jurisdiction::Gdpr`], the EU, EEA and UK regime.
    /// - `us-state` for [`Jurisdiction::UsState`]. It carries no code, because
    ///   the node that names it is itself a region, so `region` supplies the
    ///   state (upper-cased). A node with no region of its own, meaning the top
    ///   of the tree or a country, cannot name it, and `None` is returned.
    /// - `non-regulated` for [`Jurisdiction::NonRegulated`], a place with no
    ///   matching regulation.
    /// - `unknown` for [`Jurisdiction::Unknown`], declining to name one.
    ///
    /// `region` is the ISO 3166-2 subdivision code of the node carrying the
    /// value, or `None` for the top of the tree and for a country.
    ///
    /// Returns `None` for anything else, which the permission policy parser
    /// reports as a configuration error rather than silently defaulting.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::consent::jurisdiction::Jurisdiction;
    ///
    /// assert_eq!(Jurisdiction::from_policy_name("gdpr", None), Some(Jurisdiction::Gdpr));
    /// assert_eq!(
    ///     Jurisdiction::from_policy_name("us-state", Some("ca")),
    ///     Some(Jurisdiction::UsState("CA".to_owned()))
    /// );
    /// assert_eq!(Jurisdiction::from_policy_name("us-state", None), None);
    /// assert_eq!(Jurisdiction::from_policy_name("nonsense", None), None);
    /// ```
    #[must_use]
    pub fn from_policy_name(value: &str, region: Option<&str>) -> Option<Self> {
        match value {
            "gdpr" => Some(Self::Gdpr),
            "us-state" => region.map(|code| Self::UsState(code.to_uppercase())),
            "non-regulated" => Some(Self::NonRegulated),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
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

/// Detects the privacy jurisdiction for a request from its location.
///
/// Walks the `permissions.yaml` place tree the same way the permission
/// baseline is resolved: the request's region when it is listed, otherwise its
/// country, otherwise the top of the tree. A node that names no jurisdiction of
/// its own inherits the one above it, which is resolved when the file is
/// parsed.
///
/// The tree also settles the `DE` collision on its own, because ISO 3166-1
/// `DE` is Germany and sits at the country level, while ISO 3166-2 `DE` is
/// Delaware and sits under `US`.
///
/// With no location this returns the top node's jurisdiction, the policy's
/// declared answer for a visitor whose place is not resolved. A caller that
/// must not apply that declaration, such as one holding a failed geo lookup,
/// resolves [`Jurisdiction::Unknown`] itself rather than calling this.
#[must_use]
pub fn detect_jurisdiction(geo: Option<&GeoInfo>) -> Jurisdiction {
    let maps = PermissionMaps::standard();
    match geo {
        Some(geo) => maps.jurisdiction_for(Some(&geo.country), geo.region.as_deref()),
        None => maps.default_jurisdiction(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{Jurisdiction, detect_jurisdiction};
    use crate::geo::GeoInfo;

    fn make_geo(country: &str, region: Option<&str>) -> GeoInfo {
        GeoInfo {
            city: "Test City".to_owned(),
            country: country.to_owned(),
            continent: "EU".to_owned(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: region.map(str::to_owned),
            asn: None,
        }
    }

    #[test]
    fn gdpr_detected_for_eu_country() {
        let geo = make_geo("DE", None);
        assert_eq!(
            detect_jurisdiction(Some(&geo)),
            Jurisdiction::Gdpr,
            "Germany should trigger GDPR"
        );
    }

    #[test]
    fn gdpr_detected_for_eea_country() {
        let geo = make_geo("NO", None);
        assert_eq!(
            detect_jurisdiction(Some(&geo)),
            Jurisdiction::Gdpr,
            "Norway (EEA) should trigger GDPR"
        );
    }

    #[test]
    fn gdpr_detected_for_uk() {
        let geo = make_geo("GB", None);
        assert_eq!(
            detect_jurisdiction(Some(&geo)),
            Jurisdiction::Gdpr,
            "the UK should trigger GDPR"
        );
    }

    #[test]
    fn us_state_detected_for_california() {
        let geo = make_geo("US", Some("CA"));
        assert_eq!(
            detect_jurisdiction(Some(&geo)),
            Jurisdiction::UsState("CA".to_owned()),
            "California should trigger US state privacy"
        );
    }

    #[test]
    fn delaware_is_a_us_state_and_germany_is_not() {
        // ISO 3166-1 `DE` is Germany and ISO 3166-2 `DE` is Delaware. The tree
        // keeps them apart by where they sit, so no ordering rule is needed.
        let delaware = make_geo("US", Some("DE"));
        assert_eq!(
            detect_jurisdiction(Some(&delaware)),
            Jurisdiction::UsState("DE".to_owned()),
            "US/DE should be Delaware"
        );
        let germany = make_geo("DE", None);
        assert_eq!(
            detect_jurisdiction(Some(&germany)),
            Jurisdiction::Gdpr,
            "DE at the country level should be Germany"
        );
    }

    #[test]
    fn us_non_privacy_state_inherits_the_country_node() {
        let geo = make_geo("US", Some("WY"));
        assert_eq!(
            detect_jurisdiction(Some(&geo)),
            Jurisdiction::NonRegulated,
            "Wyoming is not listed, so it inherits the US node"
        );
    }

    #[test]
    fn us_no_region_is_non_regulated() {
        let geo = make_geo("US", None);
        assert_eq!(
            detect_jurisdiction(Some(&geo)),
            Jurisdiction::NonRegulated,
            "the US without a region should be non-regulated"
        );
    }

    #[test]
    fn an_unlisted_country_inherits_the_top_of_the_tree() {
        // Nothing is written for Japan, so it inherits the top node, which the
        // shipped policy sets to GDPR. That is the same node an unresolved
        // place gets, so an unlisted country is treated no more loosely than a
        // visitor with no place at all.
        let geo = make_geo("JP", None);
        assert_eq!(
            detect_jurisdiction(Some(&geo)),
            Jurisdiction::Gdpr,
            "an unlisted country should inherit the top of the tree"
        );
    }

    #[test]
    fn no_geo_uses_the_top_of_the_tree() {
        assert_eq!(
            detect_jurisdiction(None),
            Jurisdiction::Gdpr,
            "with no place the policy's declared top node applies"
        );
    }

    #[test]
    fn case_insensitive_place_matching() {
        let geo = make_geo("de", None);
        assert_eq!(
            detect_jurisdiction(Some(&geo)),
            Jurisdiction::Gdpr,
            "a lowercase country code should still match"
        );
        let state = make_geo("us", Some("ca"));
        assert_eq!(
            detect_jurisdiction(Some(&state)),
            Jurisdiction::UsState("CA".to_owned()),
            "a lowercase region code should still match and upper-case the state"
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
