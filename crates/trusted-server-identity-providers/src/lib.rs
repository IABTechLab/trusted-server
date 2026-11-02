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

pub mod pull_sync;

use std::num::{NonZeroU32, NonZeroUsize};
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

use crate::pull_sync::PullSync;

/// Whether `kind` has a registered implementation.
#[must_use]
pub fn is_registered_kind(kind: &str) -> bool {
    kind == PullSync::KIND
}

/// Builds the partner registry, marking only sources claimed by enabled
/// providers whose kind is registered (including pull-sync partners) as
/// provider-owned.
///
/// # Errors
///
/// Returns the same errors as [`PartnerRegistry::from_ec`].
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
        PullSync::KIND => build_kind::<PullSync>(config, secrets),
        kind => Err(Report::new(ProviderConfigError::UnknownKind)
            .attach(format!("kind `{kind}` is not registered"))),
    }
}

/// One `pull-sync` provider per partner with pull sync enabled, built from the
/// partner's existing fields with no config change.
///
/// Pull sync runs after the response has been sent, as it did before it
/// became a provider, so a slow partner never delays the page and a new EC
/// whose row does not exist yet is never pulled. Its hourly rate limit is
/// the partner's `pull_sync_rate_limit` (`None` when it is 0).
pub fn pull_sync_providers(ec: &Ec) -> impl Iterator<Item = IdentityProviderConfig> + '_ {
    ec.partners
        .iter()
        .filter(|partner| partner.pull_sync_enabled)
        .filter_map(|partner| {
            let endpoint = partner.pull_sync_url.clone()?;
            let mut config = IdentityProviderConfig::new(
                format!("pull-sync:{}", partner.source_domain),
                PullSync::KIND.to_owned(),
                endpoint,
                partner.pull_sync_allowed_domains.clone(),
                vec![partner.source_domain.clone()],
            );
            config.secrets.token = partner.ts_pull_token.clone();
            config.collect = CollectTiming::AfterResponse;
            config.rate_limit_per_hour = NonZeroU32::new(partner.pull_sync_rate_limit);
            Some(config)
        })
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
/// Enabled providers with an unregistered kind are skipped; building the
/// registry with [`partner_registry`] already warned about them.
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
    let configured = settings
        .ec
        .identity_providers
        .iter()
        .filter(|config| config.enabled && is_registered_kind(config.kind()))
        .cloned();
    let registered = configured
        .chain(pull_sync_providers(&settings.ec))
        .filter(|config| config.collect == timing)
        .map(|config| {
            build(&config, &config.secrets).map(|provider| {
                let rate_limit = config.rate_limit_per_hour;
                let registered = RegisteredProvider::new(config, provider);
                match rate_limit {
                    Some(per_hour) => registered.with_rate_limit(per_hour),
                    None => registered,
                }
            })
        })
        .collect::<Result<Vec<_>, Report<ProviderConfigError>>>()?;
    if registered.is_empty() {
        return Ok(None);
    }
    let pull_sync_concurrency = NonZeroUsize::new(settings.ec.pull_sync_concurrency);
    ProviderCoordinator::new(registered, partners, budget)
        .map(|coordinator| Some(coordinator.with_pull_sync_concurrency(pull_sync_concurrency)))
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
    fn pull_sync_partner_runs_after_the_response() {
        let mut settings = create_test_settings();
        settings.ec.partners = vec![
            toml::from_str(
                r#"
                name = "Pull partner"
                source_domain = "partner.example.com"
                pull_sync_enabled = true
                pull_sync_url = "https://sync.example.com/pull"
                pull_sync_allowed_domains = ["sync.example.com"]
                ts_pull_token = "example-pull-token-32-bytes-minimum"
                "#,
            )
            .expect("should parse partner"),
        ];

        let page = build_coordinator(&settings).expect("should build page providers");
        let partners = Arc::new(partner_registry(&settings.ec).expect("should build partners"));
        let after_response = build_after_response_coordinator(&settings, partners)
            .expect("should build pull-sync providers")
            .expect("should build an after-response coordinator");

        assert!(
            page.is_none(),
            "should not run pull sync before the response"
        );
        assert!(
            !after_response.is_empty(),
            "should run pull sync after the response"
        );
    }

    #[test]
    fn enabled_unknown_kind_is_skipped_and_claims_no_sources() {
        let mut settings = create_test_settings();
        settings.ec.identity_providers = vec![
            toml::from_str(
                "id = \"p\"\nkind = \"missing\"\nendpoint = \"https://ids.example.com/\"\nsources = [\"ids.example.com\"]",
            )
            .expect("should parse"),
        ];

        let coordinator =
            build_coordinator(&settings).expect("should skip unregistered provider kinds");
        let registry = partner_registry(&settings.ec).expect("should build registry");

        assert!(coordinator.is_none(), "should build no coordinator");
        assert!(
            !registry.is_provider_owned("ids.example.com"),
            "should leave unregistered provider sources to browser ingest"
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
