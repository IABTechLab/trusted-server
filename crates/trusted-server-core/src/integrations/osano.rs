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
pub struct OsanoConfig {
    /// Whether the Osano browser consent mirror is enabled.
    #[serde(default)]
    pub enabled: bool,
}

impl IntegrationConfig for OsanoConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Register the Osano JS integration when enabled.
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

    use super::register;
    use crate::test_support::tests::create_test_settings;

    #[test]
    fn register_returns_none_when_disabled() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config("osano", &json!({ "enabled": false }))
            .expect("should insert osano config");

        let registration = register(&settings).expect("should parse disabled osano config");

        assert!(
            registration.is_none(),
            "disabled Osano integration should not register"
        );
    }

    #[test]
    fn register_returns_js_module_registration_when_enabled() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config("osano", &json!({ "enabled": true }))
            .expect("should insert osano config");

        let registration = register(&settings)
            .expect("should parse enabled osano config")
            .expect("enabled Osano integration should register");

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
}
