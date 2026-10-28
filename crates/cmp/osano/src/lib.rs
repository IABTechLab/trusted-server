//! Osano integration for client-side consent mirroring.
//!
//! The Rust side of this integration intentionally only provides explicit
//! enablement for the `tsjs-osano` browser module. Osano consent extraction runs
//! in JavaScript because the relevant CMP APIs (`__uspapi`, `__gpp`, and
//! `__tcfapi`) are browser-only.

#![cfg_attr(
    test,
    allow(
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::panic,
        clippy::dbg_macro,
        clippy::unwrap_used,
        reason = "tests use direct diagnostics and panic-on-failure helpers"
    )
)]

use error_stack::Report;
use serde::Deserialize;
use validator::Validate;

use trusted_server_core::error::TrustedServerError;
use trusted_server_core::integrations::{IntegrationBuilder, IntegrationRegistration};
use trusted_server_core::settings::{IntegrationConfig, Settings};

const OSANO_INTEGRATION_ID: &str = "osano";

/// The name this module is selected by, in `[cmp]`.
pub const MODULE: &str = "cmp.osano";

/// The builder a deployment hands to an adapter, which the registry runs when
/// a section selects [`MODULE`].
#[must_use]
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::new(
        OSANO_INTEGRATION_ID,
        env!("CARGO_PKG_NAME"),
        register,
        validate,
    )
    .with_module_name(MODULE)
}

/// Configuration for the Osano consent mirror integration.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct OsanoConfig {}

impl IntegrationConfig for OsanoConfig {}

/// Validates the Osano configuration for deployment and reports whether
/// a section selects the integration's module.
///
/// # Errors
///
/// Returns an error when the Osano configuration cannot be parsed or fails
/// validation.
pub(crate) fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    settings
        .module_config::<OsanoConfig>(MODULE)
        .map(|config| config.is_some())
}

/// Register the Osano JS integration when a section selects it.
///
/// # Errors
///
/// Returns an error when the Osano integration configuration cannot be parsed or
/// fails validation.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(_config) = settings.module_config::<OsanoConfig>(MODULE)? else {
        return Ok(None);
    };

    Ok(Some(
        IntegrationRegistration::builder(OSANO_INTEGRATION_ID).build(),
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::json;
    use trusted_server_core::auction::compile_auction_plan;
    use trusted_server_core::config::validate_settings_for_deploy_with;
    use trusted_server_core::integrations::IntegrationRegistry;
    use trusted_server_core::test_support::tests::create_test_settings;

    use super::{MODULE, OsanoConfig, builder, register};

    #[test]
    fn register_returns_none_when_the_integration_list_does_not_name_it() {
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
        settings.select_module("cmp", "cmp.osano");

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
            .insert_module_config("cmp", "cmp.osano", &json!({"typo": true }))
            .expect("should insert osano config");

        let err = settings
            .module_config::<OsanoConfig>(super::MODULE)
            .expect_err("should reject unknown Osano config fields");
        let error_text = format!("{err:?}");

        assert!(
            error_text.contains("typo") || error_text.contains("unknown field"),
            "error should mention the unknown field: {err:?}"
        );
    }

    #[test]
    fn deploy_validation_rejects_an_unknown_setting() {
        let mut settings = create_test_settings();
        settings
            .insert_module_config("cmp", MODULE, &json!({"typo": true }))
            .expect("should insert Osano config");

        let err = validate_settings_for_deploy_with(&settings, &[builder()])
            .expect_err("should reject invalid Osano config during deploy validation");
        let error_text = format!("{err:?}");

        assert!(
            error_text.contains("osano") || error_text.contains("typo"),
            "error should mention Osano or the invalid field: {err:?}"
        );
    }

    #[test]
    fn a_selected_mirror_is_served_and_listed() {
        let mut settings = create_test_settings();
        settings.select_module("cmp", MODULE);
        let plan = Arc::new(compile_auction_plan(&settings).expect("should compile auction plan"));

        let registry =
            IntegrationRegistry::with_plan_and_registrations(&settings, plan, &[builder()])
                .expect("should create registry");

        assert!(
            registry.js_module_ids_immediate().contains(&"osano"),
            "should include Osano's browser module when it is named"
        );
        assert!(
            registry
                .registered_integrations()
                .iter()
                .any(|integration| integration.id == "osano"),
            "should list the registration, which has a browser module and no hooks"
        );
    }

    #[test]
    fn module_constant_is_the_crate_folder() {
        assert_eq!(
            super::MODULE,
            trusted_server_core::module_name!(),
            "should be named by the folder this crate lives in"
        );
    }
}
