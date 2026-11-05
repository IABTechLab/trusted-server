//! Explicit application-owned composition of the native Geo provider.
//!
//! EdgeZero does not load a Geo environment or store binding. This optional TS
//! boot profile selects its Rust options before store preparation and listening.
//! Local databases use checked reader IPs and the existing trusted proxy policy.
//! Protected assertions contain a credential removal name, never its value.

use std::fmt;
use std::fs;
use std::path::PathBuf;

use edgezero_adapter_axum::geo::{
    MaxMindGeoConfig, NativeGeoSelection, ProtectedCloudFrontAlb, ReaderIpExpectation,
};
use serde::Deserialize;

/// Path to the separately delivered TS native Geo boot profile.
pub const PROFILE_FILE_ENV: &str = "TS_NATIVE_GEO_PROFILE_FILE";

/// Safe startup categories that never retain profile bytes or policy inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeGeoProfileError {
    /// A selected profile file could not be read.
    Unavailable,
    /// The file is not the supported typed profile.
    InvalidProfile,
    /// `EdgeZero` rejected the protected source or credential removal policy.
    InvalidPolicy,
}

impl fmt::Display for NativeGeoProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "native Geo profile could not be read",
            Self::InvalidProfile => "native Geo profile is invalid",
            Self::InvalidPolicy => "native Geo protected source policy is invalid",
        })
    }
}

impl core::error::Error for NativeGeoProfileError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeGeoProfile {
    version: u32,
    geo: GeoSelection,
}

#[derive(Deserialize)]
#[serde(tag = "provider", deny_unknown_fields)]
enum GeoSelection {
    #[serde(rename = "off")]
    Off {},
    #[serde(rename = "cloudfront")]
    CloudFront {
        alb_peer_cidrs: Vec<String>,
        origin_credential_header_name: String,
    },
    #[serde(rename = "maxmind")]
    MaxMind {
        database_path: PathBuf,
        reader_ip_expectation: ReaderExpectation,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReaderExpectation {
    PeerAllowed,
    ForwardedRequired,
}

/// Capture the application-owned selection once before starting the runner.
///
/// An absent profile keeps existing local hosting Off. A supplied unreadable or
/// invalid file is an error, never a silent fallback. The selected built-in
/// implementation is validated by final `AxumRunOptions::geo_capability()`.
///
/// # Errors
/// Returns a bounded category without the file path, input bytes, CIDRs or name.
pub fn load_selection() -> Result<NativeGeoSelection, NativeGeoProfileError> {
    let Some(path) = std::env::var_os(PROFILE_FILE_ENV) else {
        return Ok(NativeGeoSelection::Off);
    };
    let bytes = fs::read_to_string(path).map_err(|_| NativeGeoProfileError::Unavailable)?;
    parse_selection(&bytes)
}

fn parse_selection(bytes: &str) -> Result<NativeGeoSelection, NativeGeoProfileError> {
    let profile: NativeGeoProfile =
        toml::from_str(bytes).map_err(|_| NativeGeoProfileError::InvalidProfile)?;
    if profile.version != 1 {
        return Err(NativeGeoProfileError::InvalidProfile);
    }
    match profile.geo {
        GeoSelection::Off {} => Ok(NativeGeoSelection::Off),
        GeoSelection::CloudFront {
            alb_peer_cidrs,
            origin_credential_header_name,
        } => ProtectedCloudFrontAlb::new(alb_peer_cidrs, &origin_credential_header_name)
            .map(|policy| NativeGeoSelection::CloudFront { policy })
            .map_err(|_| NativeGeoProfileError::InvalidPolicy),
        GeoSelection::MaxMind {
            database_path,
            reader_ip_expectation,
        } => Ok(NativeGeoSelection::MaxMind {
            config: MaxMindGeoConfig::new(
                database_path,
                match reader_ip_expectation {
                    ReaderExpectation::PeerAllowed => ReaderIpExpectation::PeerAllowed,
                    ReaderExpectation::ForwardedRequired => ReaderIpExpectation::ForwardedRequired,
                },
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use edgezero_adapter_axum::dev_server::AxumRunOptions;
    use edgezero_adapter_axum::geo::NativeGeoCapability;

    use super::*;

    const PROFILE: &str = r#"version = 1
[geo]
provider = "cloudfront"
alb_peer_cidrs = ["127.0.0.1/32"]
origin_credential_header_name = "x-origin-fixture-private-name"
"#;

    const MAXMIND_PROFILE: &str = r#"version = 1
[geo]
provider = "maxmind"
database_path = "/geo/GeoLite2-City.mmdb"
reader_ip_expectation = "forwarded_required"
"#;

    #[test]
    fn maxmind_profile_selects_local_database_and_explicit_reader_expectation() {
        for (name, expected) in [
            ("forwarded_required", ReaderIpExpectation::ForwardedRequired),
            ("peer_allowed", ReaderIpExpectation::PeerAllowed),
        ] {
            let selection = parse_selection(&MAXMIND_PROFILE.replace("forwarded_required", name))
                .expect("should parse local database selection");
            let NativeGeoSelection::MaxMind { config } = selection else {
                unreachable!("should select MaxMind");
            };
            assert_eq!(config.path, Path::new("/geo/GeoLite2-City.mmdb"));
            assert_eq!(config.expectation, expected);
        }
    }

    #[test]
    fn maxmind_profile_rejects_missing_or_unknown_expectation_and_foreign_policy_fields() {
        for bytes in [
            MAXMIND_PROFILE.replace("reader_ip_expectation = \"forwarded_required\"", ""),
            MAXMIND_PROFILE.replace("forwarded_required", "automatic"),
            MAXMIND_PROFILE.replace("database_path = \"/geo/GeoLite2-City.mmdb\"", ""),
            format!("{MAXMIND_PROFILE}alb_peer_cidrs = ['127.0.0.1/32']\n"),
            format!("{MAXMIND_PROFILE}origin_credential_header_name = 'x-origin-test'\n"),
        ] {
            assert!(matches!(
                parse_selection(&bytes),
                Err(NativeGeoProfileError::InvalidProfile)
            ));
        }
    }

    #[test]
    fn maxmind_capability_validates_compiled_provider_without_opening_database() {
        let selection = parse_selection(MAXMIND_PROFILE)
            .expect("should parse selection without opening a database");
        let options = AxumRunOptions::new("127.0.0.1:8787".parse().expect("should parse address"))
            .expect("should construct options")
            .with_geo(selection);
        #[cfg(all(feature = "geo-maxmind", unix))]
        assert!(matches!(
            options
                .geo_capability()
                .expect("should validate available provider"),
            NativeGeoCapability::Selected {
                input: edgezero_adapter_axum::geo::NativeGeoInputContract::ReaderIp {
                    expectation: ReaderIpExpectation::ForwardedRequired
                },
                ..
            }
        ));
        #[cfg(not(all(feature = "geo-maxmind", unix)))]
        assert!(
            options.geo_capability().is_err(),
            "should reject unavailable MaxMind"
        );
    }

    #[test]
    fn absent_profile_retains_off() {
        temp_env::with_var_unset(PROFILE_FILE_ENV, || {
            assert!(matches!(
                load_selection().expect("should retain absent selection"),
                NativeGeoSelection::Off
            ));
        });
    }

    #[test]
    fn explicit_off_is_a_complete_selection() {
        assert!(matches!(
            parse_selection("version = 1\n[geo]\nprovider = 'off'")
                .expect("should parse explicit off"),
            NativeGeoSelection::Off
        ));
        assert!(
            parse_selection("version = 1\n[geo]\nprovider = 'off'\nalb_peer_cidrs = []").is_err(),
            "should reject source policy fields on an off selection"
        );
    }

    #[test]
    fn invalid_selected_file_does_not_fall_back() {
        let directory = tempfile::tempdir().expect("should create profile directory");
        let path = directory.path().join("missing-profile.toml");
        temp_env::with_var(PROFILE_FILE_ENV, Some(path.as_os_str()), || {
            assert!(matches!(
                load_selection(),
                Err(NativeGeoProfileError::Unavailable)
            ));
            fs::write(&path, "credential-sensitive malformed input")
                .expect("should write invalid profile");
            assert!(matches!(
                load_selection(),
                Err(NativeGeoProfileError::InvalidProfile)
            ));
        });
    }

    #[test]
    fn invalid_policy_diagnostics_do_not_disclose_inputs() {
        for bytes in [
            PROFILE.replace("127.0.0.1/32", "0.0.0.0/0"),
            PROFILE.replace("x-origin-fixture-private-name", "authorization"),
            PROFILE.replace("version = 1", "version = 2"),
            format!("{PROFILE}origin_credential_value = 'fixture-credential-never-supported'\n"),
        ] {
            let error =
                parse_selection(&bytes).expect_err("should reject unsupported native Geo profile");
            let diagnostic = format!("{error:?}: {error}");
            for sensitive in [
                "127.0.0.1",
                "0.0.0.0",
                "x-origin-fixture-private-name",
                "authorization",
                "fixture-credential-never-supported",
            ] {
                assert!(
                    !diagnostic.contains(sensitive),
                    "should redact profile inputs"
                );
            }
        }
    }

    #[test]
    fn selected_implementation_is_explicitly_validated() {
        let selection = parse_selection(PROFILE).expect("should compose protected source policy");
        let options = AxumRunOptions::new("127.0.0.1:8787".parse().expect("should parse address"))
            .expect("should construct options")
            .with_geo(selection);
        #[cfg(feature = "geo-cloudfront")]
        assert!(matches!(
            options
                .geo_capability()
                .expect("should validate selected implementation"),
            NativeGeoCapability::Selected { .. }
        ));
        #[cfg(not(feature = "geo-cloudfront"))]
        {
            assert!(
                options.geo_capability().is_err(),
                "should reject compiled-out Geo"
            );
            let off = options.with_geo(NativeGeoSelection::Off);
            assert_eq!(
                off.geo_capability().expect("should permit off"),
                NativeGeoCapability::Off
            );
        }
    }
}
