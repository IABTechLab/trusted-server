//! Identity provider implementations for Trusted Server.
//!
//! Each provider kind is a module implementing
//! [`trusted_server_core::ec::provider::ProviderKind`]. [`build`] maps a
//! configured `kind` to its implementation; [`build_coordinator`] builds the
//! coordinator for every enabled provider.
//!
//! Dependencies point one way: this crate depends on core, and core never
//! depends on it.

use std::sync::Arc;
use std::time::Duration;

use error_stack::Report;
use trusted_server_core::ec::coordinator::{ProviderCoordinator, RegisteredProvider};
use trusted_server_core::ec::provider::{IdentityProvider, ProviderConfigError};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::settings::{IdentityProviderConfig, ProviderSecrets, Settings};

/// Builds the provider for a configured `kind`.
///
/// No kinds are registered yet, so every kind is rejected.
///
/// # Errors
///
/// Returns [`ProviderConfigError::UnknownKind`] for an unregistered kind, or
/// any error from [`trusted_server_core::ec::provider::build_kind`].
pub fn build(
    config: &IdentityProviderConfig,
    _secrets: &ProviderSecrets,
) -> Result<Box<dyn IdentityProvider>, Report<ProviderConfigError>> {
    let kind = config.kind();
    Err(Report::new(ProviderConfigError::UnknownKind)
        .attach(format!("kind `{kind}` is not registered")))
}

/// Builds the coordinator for every enabled provider in `settings`.
///
/// Returns `None` when no provider is enabled.
///
/// # Errors
///
/// Returns [`ProviderConfigError`] when a provider cannot be built or fails
/// the coordinator's configuration checks.
pub fn build_coordinator(
    settings: &Settings,
) -> Result<Option<ProviderCoordinator>, Report<ProviderConfigError>> {
    let enabled: Vec<&IdentityProviderConfig> = settings
        .ec
        .identity_providers
        .iter()
        .filter(|config| config.enabled)
        .collect();
    if enabled.is_empty() {
        return Ok(None);
    }
    let registered = enabled
        .into_iter()
        .map(|config| {
            build(config, &config.secrets)
                .map(|provider| RegisteredProvider::new(config.clone(), provider))
        })
        .collect::<Result<Vec<_>, Report<ProviderConfigError>>>()?;
    let partners = PartnerRegistry::from_ec(&settings.ec)
        .map_err(|report| report.change_context(ProviderConfigError::Invalid))?;
    let budget = Duration::from_millis(settings.ec.identity_budget_ms);
    ProviderCoordinator::new(registered, Arc::new(partners), budget).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_settings() -> Settings {
        Settings::from_toml(
            r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "example-admin-password"

            [publisher]
            domain = "www.example.com"
            cookie_domain = ".example.com"
            origin_url = "https://origin.example.com"
            proxy_secret = "example-proxy-secret"

            [ec]
            passphrase = "example-passphrase-32-bytes-minimum"
            "#,
        )
        .expect("should parse test settings")
    }

    #[test]
    fn unknown_kind_is_rejected() {
        let config: IdentityProviderConfig = toml::from_str(
            "id = \"p\"\nkind = \"missing\"\nendpoint = \"https://ids.example.com/\"\nallowed_domains = [\"ids.example.com\"]",
        )
        .expect("should parse");

        let error = build(&config, &ProviderSecrets::default())
            .err()
            .expect("should reject unknown kind");

        assert!(
            matches!(error.current_context(), ProviderConfigError::UnknownKind),
            "should report unknown kind"
        );
    }

    #[test]
    fn no_enabled_provider_builds_no_coordinator() {
        let mut settings = create_test_settings();
        settings.ec.identity_providers = vec![
            toml::from_str(
                "id = \"p\"\nkind = \"missing\"\nenabled = false\nendpoint = \"https://ids.example.com/\"",
            )
            .expect("should parse"),
        ];

        let coordinator = build_coordinator(&settings).expect("should skip disabled providers");

        assert!(coordinator.is_none(), "should build no coordinator");
    }

    #[test]
    fn enabled_unknown_kind_fails_the_build() {
        let mut settings = create_test_settings();
        settings.ec.identity_providers = vec![
            toml::from_str(
                "id = \"p\"\nkind = \"missing\"\nendpoint = \"https://ids.example.com/\"",
            )
            .expect("should parse"),
        ];

        let error = build_coordinator(&settings)
            .err()
            .expect("should reject unknown kind");

        assert!(
            matches!(error.current_context(), ProviderConfigError::UnknownKind),
            "should report unknown kind"
        );
    }
}
