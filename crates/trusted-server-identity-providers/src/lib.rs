//! Identity provider implementations for Trusted Server.
//!
//! Each provider kind is a module implementing
//! [`trusted_server_core::ec::provider::ProviderKind`]. [`build`] maps a
//! configured `kind` to its implementation; [`build_coordinator`] and
//! [`build_after_response_coordinator`] build the coordinators for enabled
//! providers collected before and after the response.
//!
//! Dependencies point one way: this crate depends on core, and core never
//! depends on it.

mod common;

pub use common::LockrSettings;
pub mod connectid;
pub mod id5;
pub mod liveintent;
pub mod liveramp;
pub mod uid2;

use std::sync::Arc;
use std::time::Duration;

use error_stack::Report;
use trusted_server_core::ec::coordinator::{ProviderCoordinator, RegisteredProvider};
use trusted_server_core::ec::provider::{
    IdentityProvider, ProviderConfigError, ProviderKind as _, build_kind,
};
use trusted_server_core::ec::registry::PartnerRegistry;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::settings::{
    CollectTiming, Ec, IdentityProviderConfig, ProviderSecrets, Settings,
};

/// Whether `kind` has a registered implementation.
///
#[must_use]
pub fn is_registered_kind(kind: &str) -> bool {
    matches!(
        kind,
        id5::Id5::KIND
            | liveintent::LiveIntent::KIND
            | liveramp::LiveRamp::KIND
            | connectid::ConnectId::KIND
            | uid2::Uid2::KIND
    )
}

/// Whether `kind` declares the Lockr capability, so its settings accept a
/// `[ec.identity_providers.settings.lockr]` table.
///
/// The settings of every other kind reject that table as an unknown field
/// when the provider is built.
#[must_use]
pub fn reads_lockr(kind: &str) -> bool {
    matches!(
        kind,
        id5::Id5::KIND | liveramp::LiveRamp::KIND | uid2::Uid2::KIND
    )
}

/// Builds the partner registry, marking the sources claimed by enabled
/// providers as provider-owned.
///
/// # Errors
///
/// Returns the same errors as [`PartnerRegistry::from_ec_with_kinds`],
/// including for an enabled provider whose kind is not registered.
pub fn partner_registry(ec: &Ec) -> Result<PartnerRegistry, Report<TrustedServerError>> {
    PartnerRegistry::from_ec_with_kinds(ec, is_registered_kind)
}

/// Builds the provider for a configured `kind`.
///
/// # Errors
///
/// Returns [`ProviderConfigError::UnknownKind`] for an unregistered kind, or
/// any error from [`trusted_server_core::ec::provider::build_kind`].
pub fn build(
    config: &IdentityProviderConfig,
    secrets: &ProviderSecrets,
) -> Result<Box<dyn IdentityProvider>, Report<ProviderConfigError>> {
    match config.kind() {
        id5::Id5::KIND => build_kind::<id5::Id5>(config, secrets),
        liveintent::LiveIntent::KIND => build_kind::<liveintent::LiveIntent>(config, secrets),
        liveramp::LiveRamp::KIND => build_kind::<liveramp::LiveRamp>(config, secrets),
        connectid::ConnectId::KIND => build_kind::<connectid::ConnectId>(config, secrets),
        uid2::Uid2::KIND => build_kind::<uid2::Uid2>(config, secrets),
        kind => Err(Report::new(ProviderConfigError::UnknownKind)
            .attach(format!("kind `{kind}` is not registered"))),
    }
}

/// Builds the coordinator for enabled providers collected before the
/// response (`collect = "page"`, the default).
///
/// Returns `None` when no such provider is enabled.
///
/// # Errors
///
/// Returns [`ProviderConfigError`] when a provider cannot be built or the
/// enabled providers, taken together, fail the coordinator's configuration
/// checks.
pub fn build_coordinator(
    settings: &Settings,
) -> Result<Option<ProviderCoordinator>, Report<ProviderConfigError>> {
    let partners = partner_registry(&settings.ec)
        .map_err(|report| report.change_context(ProviderConfigError::Invalid))?;
    build_coordinator_with(settings, Arc::new(partners))
}

/// Builds the page-phase coordinator against an already built partner
/// registry.
///
/// # Errors
///
/// Returns [`ProviderConfigError`] when a provider cannot be built or fails
/// the coordinator's configuration checks.
pub fn build_coordinator_with(
    settings: &Settings,
    partners: Arc<PartnerRegistry>,
) -> Result<Option<ProviderCoordinator>, Report<ProviderConfigError>> {
    build_phase_coordinator(
        settings,
        partners,
        CollectTiming::Page,
        Duration::from_millis(settings.ec.identity_budget_ms),
    )
}

/// Builds the coordinator for enabled providers collected after the response
/// has been sent (`collect = "after_response"`), against an already built
/// partner registry.
///
/// Its budget is `identity_after_response_budget_ms`. Returns `None` when no
/// such provider is enabled.
///
/// # Errors
///
/// Returns the same errors as [`build_coordinator_with`].
pub fn build_after_response_coordinator(
    settings: &Settings,
    partners: Arc<PartnerRegistry>,
) -> Result<Option<ProviderCoordinator>, Report<ProviderConfigError>> {
    build_phase_coordinator(
        settings,
        partners,
        CollectTiming::AfterResponse,
        Duration::from_millis(settings.ec.identity_after_response_budget_ms),
    )
}

fn build_phase_coordinator(
    settings: &Settings,
    partners: Arc<PartnerRegistry>,
    timing: CollectTiming,
    budget: Duration,
) -> Result<Option<ProviderCoordinator>, Report<ProviderConfigError>> {
    // Source ownership across both phases is checked when settings load
    // (`config::validate_identity_providers`), so only this phase's providers
    // are built and checked here.
    let registered = settings
        .ec
        .identity_providers
        .iter()
        .filter(|config| config.enabled && config.collect == timing)
        .map(|config| {
            build(config, &config.secrets)
                .map(|provider| RegisteredProvider::new(config.clone(), provider))
        })
        .collect::<Result<Vec<_>, Report<ProviderConfigError>>>()?;
    if registered.is_empty() {
        return Ok(None);
    }
    ProviderCoordinator::new(registered, partners, budget).map(Some)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

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
    fn only_lockr_capable_kinds_accept_lockr_settings() {
        for (kind, endpoint, sources, settings) in [
            (
                "id5",
                Some("https://id5.example.com/g/v2"),
                json!(["id5.example.com"]),
                json!({ "partner_id": 1234 }),
            ),
            (
                "liveintent",
                Some("https://li.example.com/idex"),
                json!(["li.example.com"]),
                json!({ "attributes": { "nonId": "li.example.com" } }),
            ),
            ("liveramp", None, json!(["liveramp.example.com"]), json!({})),
            (
                "connectid",
                None,
                json!(["connectid.example.com"]),
                json!({}),
            ),
            ("uid2", None, json!(["uid2.example.com"]), json!({})),
        ] {
            let mut settings = settings;
            settings["lockr"] = json!({ "enabled": true });
            let config: IdentityProviderConfig = serde_json::from_value(json!({
                "id": kind,
                "kind": kind,
                "endpoint": endpoint,
                "allowed_domains": ["id5.example.com", "li.example.com"],
                "sources": sources,
                "settings": settings,
            }))
            .expect("should parse provider config");

            let built = build(&config, &ProviderSecrets::default());

            assert_eq!(
                built.is_ok(),
                reads_lockr(kind),
                "should accept lockr settings only for capable kind {kind}"
            );
        }
    }

    #[test]
    fn enabled_lockr_rejects_an_empty_cookie_list() {
        let config: IdentityProviderConfig = serde_json::from_value(json!({
            "id": "uid2",
            "kind": "uid2",
            "sources": ["uid2.example.com"],
            "settings": { "lockr": { "enabled": true, "cookie_names": [] } },
        }))
        .expect("should parse provider config");

        let error = build(&config, &ProviderSecrets::default())
            .err()
            .expect("should reject an empty lockr cookie list");

        assert!(
            matches!(error.current_context(), ProviderConfigError::Invalid),
            "should report invalid config"
        );
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
                "id = \"p\"\nkind = \"missing\"\nendpoint = \"https://ids.example.com/\"\nsources = [\"ids.example.com\"]",
            )
            .expect("should parse"),
        ];

        let Err(error) = build_coordinator(&settings) else {
            panic!("should fail instead of skipping an enabled provider of an unknown kind");
        };

        let message = format!("{error:?}");
        assert!(
            message.contains("`p`") && message.contains("missing"),
            "should name the provider and its unregistered kind: {message}"
        );
    }

    #[test]
    fn rejects_an_unknown_collect_timing() {
        toml::from_str::<IdentityProviderConfig>(
            "id = \"example-provider\"\nkind = \"example-kind\"\ncollect = \"later\"",
        )
        .expect_err("should reject an unknown `collect` value");
    }

    #[test]
    fn parses_an_optional_hourly_rate_limit() {
        let limited: IdentityProviderConfig = toml::from_str(
            "id = \"example-provider\"\nkind = \"example-kind\"\nendpoint = \"https://ids.example.com/v1\"\nrate_limit_per_hour = 2",
        )
        .expect("should parse a rate limit");
        let unset: IdentityProviderConfig =
            toml::from_str("id = \"example-provider\"\nkind = \"example-kind\"\nendpoint = \"https://ids.example.com/v1\"")
                .expect("should parse without a rate limit");

        assert_eq!(
            limited.rate_limit_per_hour.map(std::num::NonZeroU32::get),
            Some(2),
            "should keep the hourly limit"
        );
        assert!(
            unset.rate_limit_per_hour.is_none(),
            "should leave the limit unset by default"
        );
        toml::from_str::<IdentityProviderConfig>(
            "id = \"example-provider\"\nkind = \"example-kind\"\nendpoint = \"https://ids.example.com/v1\"\nrate_limit_per_hour = 0",
        )
        .expect_err("should reject a zero limit");
    }
}
