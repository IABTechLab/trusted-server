//! The app-config type that `ts config push`, `validate` and `diff` check.
//!
//! [`TrustedServerAppConfig`] validation lives in core, which cannot build
//! identity providers because the provider crate depends on core. At runtime
//! a provider that fails to build disables every provider for the request, so
//! this wrapper adds [`validate_providers`] to the config checks: a
//! provider-specific setting error then fails the command before deploy.

use std::borrow::Cow;

use edgezero_core::app_config::{AppConfigMeta, SecretField};
use error_stack::{AttachmentKind, FrameKind, Report};
use serde::{Deserialize, Serialize};
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::ec::provider::ProviderConfigError;
use trusted_server_identity_providers::validate_providers;
use validator::{Validate, ValidationError, ValidationErrors};

/// Field that identity-provider build errors are reported under.
const PROVIDER_VALIDATION_FIELD: &str = "identity_providers";

/// [`TrustedServerAppConfig`] whose validation also builds every enabled
/// identity provider.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct CheckedAppConfig(TrustedServerAppConfig);

impl Validate for CheckedAppConfig {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = self.0.validate().err().unwrap_or_default();
        if let Err(report) = validate_providers(self.0.settings()) {
            let mut error = ValidationError::new("trusted_server_identity_providers");
            error.message = Some(Cow::Owned(provider_error_message(&report)));
            errors.add(PROVIDER_VALIDATION_FIELD, error);
        }
        if errors.errors().is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl AppConfigMeta for CheckedAppConfig {
    fn secret_fields() -> Vec<SecretField> {
        TrustedServerAppConfig::secret_fields()
    }
}

/// Joins the report's context with every printable attachment and underlying
/// error, which name the failing provider and setting (for example the
/// settings parser's `unknown field` error).
fn provider_error_message(report: &Report<ProviderConfigError>) -> String {
    let mut details: Vec<String> = Vec::new();
    for frame in report.frames() {
        let detail = match frame.kind() {
            FrameKind::Attachment(AttachmentKind::Printable(printable)) => printable.to_string(),
            FrameKind::Context(context) if !frame.is::<ProviderConfigError>() => {
                context.to_string()
            }
            _ => continue,
        };
        if !details.contains(&detail) {
            details.push(detail);
        }
    }
    if details.is_empty() {
        report.current_context().to_string()
    } else {
        format!("{}: {}", report.current_context(), details.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE_CONFIG: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../trusted-server.example.toml"
    ));

    fn with_liveramp(settings: &str) -> CheckedAppConfig {
        let config = format!(
            "{EXAMPLE_CONFIG}\n\
             [[ec.partners]]\n\
             name = \"liveramp\"\n\
             source_domain = \"liveramp.example.com\"\n\
             bidstream_enabled = true\n\
             \n\
             [[ec.identity_providers]]\n\
             id = \"liveramp\"\n\
             kind = \"liveramp\"\n\
             endpoint = \"https://envelope.example.com/api/identity/v2/envelope\"\n\
             allowed_domains = [\"envelope.example.com\"]\n\
             sources = [\"liveramp.example.com\"]\n\
             [ec.identity_providers.settings]\n\
             {settings}\n"
        );
        toml::from_str(&config).expect("should parse app config")
    }

    /// The identity-provider validation message, when there is one.
    ///
    /// The example config fails unrelated core checks (placeholder publisher
    /// values, unresolved secrets), so tests look only at this field.
    fn provider_error(config: &CheckedAppConfig) -> Option<String> {
        let errors = config.validate().err()?;
        errors
            .field_errors()
            .get(PROVIDER_VALIDATION_FIELD)
            .and_then(|errors| errors.first())
            .and_then(|error| error.message.as_deref())
            .map(str::to_owned)
    }

    #[test]
    fn reports_no_provider_error_for_the_example_config() {
        let config: CheckedAppConfig =
            toml::from_str(EXAMPLE_CONFIG).expect("should parse the example config");

        assert_eq!(
            provider_error(&config),
            None,
            "should build the example config's providers"
        );
    }

    #[test]
    fn reports_no_provider_error_for_a_provider_that_builds() {
        let config = with_liveramp("placement_id = 14\ncustom_id_namespace = \"ana-example\"");

        assert_eq!(
            provider_error(&config),
            None,
            "should accept a complete liveramp lookup"
        );
    }

    #[test]
    fn rejects_a_provider_that_cannot_be_built_and_keeps_core_errors() {
        let config = with_liveramp("placement_id = 14");

        let message = provider_error(&config).expect("should report the provider error");
        let errors = config.validate().expect_err("should fail validation");

        assert!(
            message.contains("custom_id_namespace"),
            "should name the missing setting, got: {message}"
        );
        assert!(
            errors.errors().contains_key("trusted_server"),
            "should keep the core validation errors"
        );
    }

    #[test]
    fn names_the_unknown_setting_in_a_provider_settings_typo() {
        let config = with_liveramp(
            "placement_id = 14\ncustom_id_namespace = \"ana-example\"\ncookie_nmes = [\"_lr_env\"]",
        );

        let message = provider_error(&config).expect("should report the provider error");

        assert!(
            message.contains("cookie_nmes"),
            "should name the unknown setting, got: {message}"
        );
    }
}
