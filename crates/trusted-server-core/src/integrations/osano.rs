//! Osano integration for client-side consent mirroring.
//!
//! The Rust side of this integration intentionally only provides explicit
//! enablement for the `tsjs-osano` browser module. Osano consent extraction runs
//! in JavaScript because the relevant CMP APIs (`__uspapi`, `__gpp`, and
//! `__tcfapi`) are browser-only.

use error_stack::Report;
use serde::Deserialize;
use validator::Validate;

use crate::error::TrustedServerError;
use crate::settings::{IntegrationConfig, Settings};

use super::IntegrationRegistration;

const OSANO_INTEGRATION_ID: &str = "osano";

/// Configuration for the Osano consent mirror integration.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct OsanoConfig {}

impl IntegrationConfig for OsanoConfig {}

/// Validates the Osano configuration for deployment and reports whether
/// `[integration] provider` names the integration.
///
/// # Errors
///
/// Returns an error when the Osano configuration cannot be parsed or fails
/// validation.
pub(crate) fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    settings
        .integration_config::<OsanoConfig>(OSANO_INTEGRATION_ID)
        .map(|config| config.is_some())
}

/// Register the Osano JS integration when `[integration] provider` names it.
///
/// # Errors
///
/// Returns an error when the Osano integration configuration cannot be parsed or
/// fails validation.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(_config) = settings.integration_config::<OsanoConfig>(OSANO_INTEGRATION_ID)? else {
        return Ok(None);
    };

    Ok(Some(
        IntegrationRegistration::builder(OSANO_INTEGRATION_ID).build(),
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{OsanoConfig, register};
    use crate::test_support::tests::create_test_settings;

    #[test]
    fn register_returns_none_when_the_provider_list_does_not_name_it() {
        let settings = create_test_settings();

        let registration = register(&settings).expect("should read an unnamed integration");

        assert!(
            registration.is_none(),
            "an Osano integration nothing names should not register"
        );
    }

    #[test]
    fn register_returns_js_module_registration_when_enabled() {
        let mut settings = create_test_settings();
        settings.integration.select("osano");

        let registration = register(&settings)
            .expect("should parse the osano config")
            .expect("a named Osano integration should register");

        assert_eq!(registration.integration_id, "osano");
        assert!(
            registration.proxies.is_empty(),
            "Osano v1 should not register Rust proxy routes"
        );
        assert!(
            registration.head_injectors.is_empty(),
            "Osano v1 should not inject HTML from Rust"
        );
    }

    #[test]
    fn config_rejects_unknown_fields() {
        let mut settings = create_test_settings();
        settings
            .integration
            .insert_config("osano", &json!({"typo": true }))
            .expect("should insert osano config");

        let err = settings
            .integration_config::<OsanoConfig>("osano")
            .expect_err("should reject unknown Osano config fields");
        let error_text = format!("{err:?}");

        assert!(
            error_text.contains("typo") || error_text.contains("unknown field"),
            "error should mention the unknown field: {err:?}"
        );
    }
}
