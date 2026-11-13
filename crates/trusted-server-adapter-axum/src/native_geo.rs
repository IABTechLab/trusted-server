//! Explicit application-owned composition of the native Geo provider.
//!
//! EdgeZero does not load a Geo environment or store binding. This optional TS
//! boot profile selects its Rust options before store preparation and listening.
//! It contains a credential removal name, never the expected origin credential.

use std::fs;

use edgezero_adapter_axum::geo::{NativeGeoSelection, ProtectedCloudFrontAlb};
use error_stack::Report;
use serde::Deserialize;

/// Path to the separately delivered TS native Geo boot profile.
pub const PROFILE_FILE_ENV: &str = "TS_NATIVE_GEO_PROFILE_FILE";

/// Safe startup categories that never retain profile bytes or policy inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub enum NativeGeoProfileError {
    /// A selected profile file could not be read.
    #[display("native Geo profile could not be read")]
    Unavailable,
    /// The file is not the supported typed profile.
    #[display("native Geo profile is invalid")]
    InvalidProfile,
    /// `EdgeZero` rejected the protected source or credential removal policy.
    #[display("native Geo protected source policy is invalid")]
    InvalidPolicy,
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
}

/// Capture the application-owned selection once before starting the runner.
///
/// An absent profile keeps existing local hosting Off. A supplied unreadable or
/// invalid file is an error, never a silent fallback. The selected built-in
/// implementation is validated by final `AxumRunOptions::geo_capability()`.
///
/// # Errors
/// Returns a bounded category without the file path, input bytes, CIDRs or name.
pub fn load_selection() -> Result<NativeGeoSelection, Report<NativeGeoProfileError>> {
    let Some(path) = std::env::var_os(PROFILE_FILE_ENV) else {
        return Ok(NativeGeoSelection::Off);
    };
    let bytes =
        fs::read_to_string(path).map_err(|_| Report::new(NativeGeoProfileError::Unavailable))?;
    parse_selection(&bytes)
}

fn parse_selection(bytes: &str) -> Result<NativeGeoSelection, Report<NativeGeoProfileError>> {
    let profile: NativeGeoProfile =
        toml::from_str(bytes).map_err(|_| Report::new(NativeGeoProfileError::InvalidProfile))?;
    if profile.version != 1 {
        return Err(Report::new(NativeGeoProfileError::InvalidProfile));
    }
    match profile.geo {
        GeoSelection::Off {} => Ok(NativeGeoSelection::Off),
        GeoSelection::CloudFront {
            alb_peer_cidrs,
            origin_credential_header_name,
        } => ProtectedCloudFrontAlb::new(alb_peer_cidrs, &origin_credential_header_name)
            .map(|policy| NativeGeoSelection::CloudFront { policy })
            .map_err(|_| Report::new(NativeGeoProfileError::InvalidPolicy)),
    }
}

#[cfg(test)]
mod tests {
    use edgezero_adapter_axum::dev_server::AxumRunOptions;
    use edgezero_adapter_axum::geo::NativeGeoCapability;

    use super::*;

    const PROFILE: &str = r#"version = 1
[geo]
provider = "cloudfront"
alb_peer_cidrs = ["127.0.0.1/32"]
origin_credential_header_name = "x-origin-fixture-private-name"
"#;

    #[test]
    fn absent_profile_retains_off() {
        temp_env::with_var_unset(PROFILE_FILE_ENV, || {
            assert!(
                matches!(
                    load_selection().expect("should retain absent selection"),
                    NativeGeoSelection::Off
                ),
                "should keep Geo off when no profile is selected"
            );
        });
    }

    #[test]
    fn explicit_off_is_a_complete_selection() {
        assert!(
            matches!(
                parse_selection("version = 1\n[geo]\nprovider = 'off'")
                    .expect("should parse explicit off"),
                NativeGeoSelection::Off
            ),
            "should retain the explicit off selection"
        );
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
            let unavailable =
                load_selection().expect_err("should report a missing selected profile");
            assert_eq!(
                *unavailable.current_context(),
                NativeGeoProfileError::Unavailable,
                "should retain the typed missing-profile category"
            );
            assert_eq!(
                unavailable.to_string(),
                "native Geo profile could not be read",
                "should preserve the bounded missing-profile diagnostic"
            );
            assert!(
                !format!("{unavailable:?}")
                    .contains(path.to_str().expect("should have fixture path")),
                "should not retain the selected path in report frames"
            );
            fs::write(&path, "credential-sensitive malformed input")
                .expect("should write invalid profile");
            let invalid = load_selection().expect_err("should report a malformed selected profile");
            assert_eq!(
                *invalid.current_context(),
                NativeGeoProfileError::InvalidProfile,
                "should retain the typed malformed-profile category"
            );
            assert_eq!(
                invalid.to_string(),
                "native Geo profile is invalid",
                "should preserve the bounded malformed-profile diagnostic"
            );
            assert!(
                !format!("{invalid:?}").contains("credential-sensitive"),
                "should not retain parser input in report frames"
            );
        });
    }

    #[test]
    fn invalid_policy_diagnostics_do_not_disclose_inputs() {
        for (bytes, expected) in [
            (
                PROFILE.replace("127.0.0.1/32", "0.0.0.0/0"),
                NativeGeoProfileError::InvalidPolicy,
            ),
            (
                PROFILE.replace("x-origin-fixture-private-name", "authorization"),
                NativeGeoProfileError::InvalidPolicy,
            ),
            (
                PROFILE.replace("version = 1", "version = 2"),
                NativeGeoProfileError::InvalidProfile,
            ),
            (
                format!(
                    "{PROFILE}origin_credential_value = 'fixture-credential-never-supported'\n"
                ),
                NativeGeoProfileError::InvalidProfile,
            ),
        ] {
            let error =
                parse_selection(&bytes).expect_err("should reject unsupported native Geo profile");
            assert_eq!(
                *error.current_context(),
                expected,
                "should retain the typed redacted profile failure"
            );
            assert_eq!(
                error.to_string(),
                expected.to_string(),
                "should preserve the operator-facing failure category"
            );
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
        assert!(
            matches!(
                options
                    .geo_capability()
                    .expect("should validate selected implementation"),
                NativeGeoCapability::Selected { .. }
            ),
            "should validate the compiled-in selected provider"
        );
        #[cfg(not(feature = "geo-cloudfront"))]
        {
            assert!(
                options.geo_capability().is_err(),
                "should reject compiled-out Geo"
            );
            let off = options.with_geo(NativeGeoSelection::Off);
            assert_eq!(
                off.geo_capability().expect("should permit off"),
                NativeGeoCapability::Off,
                "should retain off capability when the provider is compiled out"
            );
        }
    }
}
