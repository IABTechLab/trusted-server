//! GPT runtime diagnostics integration.
//!
//! This integration only makes the browser diagnostics module available and
//! injects its early tab-activation bootstrap. GPT observation and presentation
//! remain entirely client-side and do not alter ad serving behavior.

use std::sync::Arc;

use error_stack::Report;
use serde::Deserialize;
use validator::Validate;

use crate::error::TrustedServerError;
use crate::settings::{IntegrationConfig, Settings};

use super::{IntegrationHeadInjector, IntegrationHtmlContext, IntegrationRegistration};

const GPT_DIAGNOSTICS_INTEGRATION_ID: &str = "gpt_diagnostics";
const GPT_DIAGNOSTICS_BOOTSTRAP_JS: &str = include_str!("gpt_diagnostics_bootstrap.js");

/// Configuration for the GPT runtime diagnostics integration.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct GptDiagnosticsConfig {
    /// Whether the GPT diagnostics browser module is available.
    #[serde(default)]
    pub enabled: bool,
}

impl IntegrationConfig for GptDiagnosticsConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

struct GptDiagnosticsIntegration;

/// Register GPT diagnostics when explicitly enabled.
///
/// # Errors
///
/// Returns an error when the integration configuration cannot be parsed or
/// fails validation.
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(_config) =
        settings.integration_config::<GptDiagnosticsConfig>(GPT_DIAGNOSTICS_INTEGRATION_ID)?
    else {
        return Ok(None);
    };

    let integration = Arc::new(GptDiagnosticsIntegration);
    Ok(Some(
        IntegrationRegistration::builder(GPT_DIAGNOSTICS_INTEGRATION_ID)
            .with_head_injector(integration)
            .build(),
    ))
}

impl IntegrationHeadInjector for GptDiagnosticsIntegration {
    fn integration_id(&self) -> &'static str {
        GPT_DIAGNOSTICS_INTEGRATION_ID
    }

    fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
        vec![format!("<script>{GPT_DIAGNOSTICS_BOOTSTRAP_JS}</script>")]
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::integrations::{IntegrationDocumentState, IntegrationRegistry};
    use crate::test_support::tests::create_test_settings;

    #[test]
    fn register_returns_none_without_config() {
        let settings = create_test_settings();

        let registration = register(&settings).expect("should evaluate diagnostics config");

        assert!(
            registration.is_none(),
            "should not register diagnostics without explicit config"
        );
    }

    #[test]
    fn register_returns_none_when_disabled() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(GPT_DIAGNOSTICS_INTEGRATION_ID, &json!({ "enabled": false }))
            .expect("should insert diagnostics config");

        let registration = register(&settings).expect("should parse diagnostics config");

        assert!(
            registration.is_none(),
            "should not register disabled diagnostics"
        );
    }

    #[test]
    fn register_adds_only_immediate_js_and_head_bootstrap() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(GPT_DIAGNOSTICS_INTEGRATION_ID, &json!({ "enabled": true }))
            .expect("should insert diagnostics config");

        let registration = register(&settings)
            .expect("should parse diagnostics config")
            .expect("should register enabled diagnostics");

        assert_eq!(registration.integration_id, GPT_DIAGNOSTICS_INTEGRATION_ID);
        assert!(
            !registration.js_deferred,
            "should load diagnostics immediately"
        );
        assert!(!registration.js_disabled, "should include diagnostics JS");
        assert!(
            registration.proxies.is_empty(),
            "should register no proxy routes"
        );
        assert!(
            registration.attribute_rewriters.is_empty(),
            "should register no attribute rewriters"
        );
        assert!(
            registration.script_rewriters.is_empty(),
            "should register no script rewriters"
        );
        assert!(
            registration.html_post_processors.is_empty(),
            "should register no HTML post-processors"
        );
        assert!(
            registration.request_filters.is_empty(),
            "should register no request filters"
        );
        assert_eq!(
            registration.head_injectors.len(),
            1,
            "should register one activation bootstrap"
        );
    }

    #[test]
    fn enabled_registry_includes_diagnostics_in_immediate_modules() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(GPT_DIAGNOSTICS_INTEGRATION_ID, &json!({ "enabled": true }))
            .expect("should insert diagnostics config");

        let registry = IntegrationRegistry::new(&settings).expect("should build registry");

        assert!(
            registry
                .js_module_ids_immediate()
                .contains(&GPT_DIAGNOSTICS_INTEGRATION_ID),
            "should include diagnostics in the immediate JS bundle"
        );
        assert!(
            !registry
                .js_module_ids_deferred()
                .contains(&GPT_DIAGNOSTICS_INTEGRATION_ID),
            "should not defer diagnostics listener installation"
        );
    }

    #[test]
    fn head_bootstrap_only_exposes_private_activation_logic() {
        let integration = GptDiagnosticsIntegration;
        let document_state = IntegrationDocumentState::default();
        let context = IntegrationHtmlContext {
            request_host: "edge.example.com",
            request_scheme: "https",
            origin_host: "origin.example.com",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&context);

        assert_eq!(inserts.len(), 1, "should emit one bootstrap script");
        assert!(
            inserts[0].contains("ts_console"),
            "should read the activation query parameter"
        );
        assert!(
            inserts[0].contains("sessionStorage"),
            "should persist activation within the tab"
        );
        assert!(
            inserts[0].contains("history.replaceState"),
            "should remove recognized activation directives"
        );
        assert!(
            inserts[0].contains("__tsjs_gpt_diagnostics_active"),
            "should expose only the private document activation flag"
        );
        assert!(
            !inserts[0].contains("gptDiagnostics ="),
            "bootstrap should not expose the public diagnostics API"
        );
    }

    #[test]
    fn config_rejects_unknown_fields() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                GPT_DIAGNOSTICS_INTEGRATION_ID,
                &json!({ "enabled": true, "typo": true }),
            )
            .expect("should insert diagnostics config");

        let error = settings
            .integration_config::<GptDiagnosticsConfig>(GPT_DIAGNOSTICS_INTEGRATION_ID)
            .expect_err("should reject unknown diagnostics config fields");
        let error_text = format!("{error:?}");

        assert!(
            error_text.contains("typo") || error_text.contains("unknown field"),
            "error should mention the unknown field: {error:?}"
        );
    }
}
