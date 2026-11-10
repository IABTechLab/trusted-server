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
pub mod criteo;
pub mod id5;
pub mod liveintent;
pub mod liveramp;
pub mod pull_sync;
pub mod sharedid;
pub mod uid2;
pub mod unifiedid;

use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use error_stack::Report;
use trusted_server_core::config::with_placeholder_partner_tokens;
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
            | sharedid::SharedId::KIND
            | criteo::Criteo::KIND
            | unifiedid::UnifiedId::KIND
            | PullSync::KIND
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

/// Whether a provider of `kind` excludes `source` where GDPR applies.
fn excludes_where_gdpr_applies(kind: &str, source: &str) -> bool {
    match kind {
        uid2::Uid2::KIND => uid2::excludes_where_gdpr_applies(source),
        liveintent::LiveIntent::KIND => liveintent::excludes_where_gdpr_applies(source),
        _ => false,
    }
}

/// Builds the partner registry, marking the sources claimed by enabled
/// providers (including pull-sync partners) as provider-owned.
///
/// The sources any configured provider, enabled or disabled, excludes where
/// GDPR applies are marked too, so the UIDs already stored for them are
/// withheld from egress for a GDPR visitor.
///
/// # Errors
///
/// Returns the same errors as [`PartnerRegistry::from_ec_with_kinds`],
/// including for an enabled provider whose kind is not registered.
pub fn partner_registry(ec: &Ec) -> Result<PartnerRegistry, Report<TrustedServerError>> {
    let mut registry = PartnerRegistry::from_ec_with_kinds(ec, is_registered_kind)?;
    for provider in &ec.identity_providers {
        for source in &provider.sources {
            if excludes_where_gdpr_applies(provider.kind(), source) {
                registry.exclude_where_gdpr_applies(source);
            }
        }
    }
    Ok(registry)
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
        sharedid::SharedId::KIND => build_kind::<sharedid::SharedId>(config, secrets),
        criteo::Criteo::KIND => build_kind::<criteo::Criteo>(config, secrets),
        unifiedid::UnifiedId::KIND => build_kind::<unifiedid::UnifiedId>(config, secrets),
        PullSync::KIND => build_kind::<PullSync>(config, secrets),
        kind => Err(Report::new(ProviderConfigError::UnknownKind)
            .attach(format!("kind `{kind}` is not registered"))),
    }
}

/// One `pull-sync` provider per partner with pull sync enabled, built from the
/// partner's existing fields with no config change.
///
/// Pull sync runs after the response has been sent, so a slow partner never
/// delays the page and a new EC whose row does not exist yet is never pulled.
/// Its hourly rate limit is the partner's `pull_sync_rate_limit`, which
/// partner validation keeps nonzero.
pub fn pull_sync_providers(ec: &Ec) -> impl Iterator<Item = IdentityProviderConfig> + '_ {
    ec.partners
        .iter()
        .filter(|partner| partner.runs_pull_sync())
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
/// Returns [`ProviderConfigError`] when a provider cannot be built or fails
/// the coordinator's configuration checks.
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

/// Builds every enabled provider of both collection phases, as the adapters
/// do on each navigation, so a provider-specific setting error is reported
/// before deploy.
///
/// At runtime such an error disables every provider for the request, so
/// deploy-time config validation (`ts config push`, `validate` and `diff`)
/// calls this. Partner tokens may still be secret key names at that point;
/// they are replaced with placeholders for the check.
///
/// # Errors
///
/// Returns [`ProviderConfigError`] for the first provider that cannot be
/// built or fails the coordinator's configuration checks.
pub fn validate_providers(settings: &Settings) -> Result<(), Report<ProviderConfigError>> {
    let ec = with_placeholder_partner_tokens(&settings.ec);
    let partners = Arc::new(
        partner_registry(&ec)
            .map_err(|report| report.change_context(ProviderConfigError::Invalid))?,
    );
    build_phase_coordinator(
        settings,
        Arc::clone(&partners),
        CollectTiming::Page,
        Duration::from_millis(settings.ec.identity_budget_ms),
    )?;
    build_phase_coordinator(
        settings,
        partners,
        CollectTiming::AfterResponse,
        Duration::from_millis(settings.ec.identity_after_response_budget_ms),
    )?;
    Ok(())
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
        .filter(|config| config.enabled)
        .cloned();
    let registered = configured
        .chain(pull_sync_providers(&settings.ec))
        .filter(|config| config.collect == timing)
        .map(|config| {
            build(&config, &config.secrets)
                .map(|provider| RegisteredProvider::new(config, provider))
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
    use serde_json::json;

    use super::*;
    use std::num::NonZeroU32;

    use trusted_server_core::ec::provider::validate_enabled_configs;
    use trusted_server_core::settings::IdentityProviderConfig;

    fn partners_for(settings: &Settings) -> Arc<PartnerRegistry> {
        Arc::new(partner_registry(&settings.ec).expect("should build partner registry"))
    }

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
                Some("https://id5.example.com/gs/v2"),
                json!(["id5.example.com"]),
                json!({ "partner_id": 1234 }),
            ),
            (
                "liveintent",
                Some("https://li.example.com/idex"),
                json!(["liveintent.com"]),
                json!({ "resolve": ["nonId"] }),
            ),
            ("liveramp", None, json!(["liveramp.example.com"]), json!({})),
            (
                "connectid",
                None,
                json!(["connectid.example.com"]),
                json!({}),
            ),
            ("uid2", None, json!(["uid2.example.com"]), json!({})),
            ("sharedid", None, json!(["sharedid.example.com"]), json!({})),
            ("criteo", None, json!(["criteo.example.com"]), json!({})),
            (
                "unifiedid",
                None,
                json!(["unifiedid.example.com"]),
                json!({}),
            ),
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

    fn settings_with_cookie_providers(sharedid_collect: &str, criteo_collect: &str) -> Settings {
        let mut settings = create_test_settings();
        settings.ec.partners = ["pubcid.org", "criteo.com"]
            .iter()
            .map(|source| {
                toml::from_str(&format!(
                    "name = \"{source}\"\nsource_domain = \"{source}\"\nbidstream_enabled = true"
                ))
                .expect("should parse partner")
            })
            .collect();
        settings.ec.identity_providers = vec![
            toml::from_str(&format!(
                "id = \"sharedid\"\nkind = \"sharedid\"\nsources = [\"pubcid.org\"]\ncollect = \"{sharedid_collect}\""
            ))
            .expect("should parse sharedid"),
            toml::from_str(&format!(
                "id = \"criteo\"\nkind = \"criteo\"\nsources = [\"criteo.com\"]\ncollect = \"{criteo_collect}\""
            ))
            .expect("should parse criteo"),
        ];
        settings
    }

    fn liveramp_provider(collect: &str, settings: &str) -> IdentityProviderConfig {
        toml::from_str(&format!(
            "id = \"liveramp\"\nkind = \"liveramp\"\nsources = [\"liveramp.example.com\"]\n\
             endpoint = \"https://envelope.example.com/api/identity/v2/envelope\"\n\
             allowed_domains = [\"envelope.example.com\"]\ncollect = \"{collect}\"\n\
             [settings]\n{settings}"
        ))
        .expect("should parse liveramp")
    }

    fn settings_with_liveramp(collect: &str, settings: &str) -> Settings {
        let mut configured = settings_with_cookie_providers("page", "page");
        configured.ec.partners.push(
            toml::from_str(
                "name = \"liveramp\"\nsource_domain = \"liveramp.example.com\"\nbidstream_enabled = true",
            )
            .expect("should parse partner"),
        );
        configured
            .ec
            .identity_providers
            .push(liveramp_provider(collect, settings));
        configured
    }

    #[test]
    fn validate_providers_accepts_buildable_providers_in_both_phases() {
        let page = settings_with_liveramp(
            "page",
            "placement_id = 14\ncustom_id_namespace = \"ana-example\"",
        );
        let after = settings_with_liveramp(
            "after_response",
            "placement_id = 14\ncustom_id_namespace = \"ana-example\"",
        );

        validate_providers(&page).expect("should accept a buildable page provider");
        validate_providers(&after).expect("should accept a buildable after-response provider");
    }

    #[test]
    fn validate_providers_rejects_a_provider_that_cannot_be_built() {
        for collect in ["page", "after_response"] {
            let settings = settings_with_liveramp(collect, "placement_id = 14");

            assert!(
                validate_providers(&settings).is_err(),
                "should reject a {collect} liveramp endpoint without a namespace"
            );
        }
    }

    #[test]
    fn validate_providers_accepts_partner_tokens_that_are_still_key_names() {
        let mut settings = settings_with_cookie_providers("page", "page");
        settings.ec.partners[0] = toml::from_str(
            "name = \"pubcid.org\"\nsource_domain = \"pubcid.org\"\nbidstream_enabled = true\napi_token = \"partner_token_key\"",
        )
        .expect("should parse partner with a token key name");

        validate_providers(&settings).expect("should not resolve partner token key names");
    }

    #[test]
    fn parses_an_optional_hourly_rate_limit() {
        let limited: IdentityProviderConfig = toml::from_str(
            "id = \"sharedid\"\nkind = \"sharedid\"\nsources = [\"pubcid.org\"]\nrate_limit_per_hour = 2",
        )
        .expect("should parse a rate limit");
        let zero = toml::from_str::<IdentityProviderConfig>(
            "id = \"sharedid\"\nkind = \"sharedid\"\nsources = [\"pubcid.org\"]\nrate_limit_per_hour = 0",
        );

        assert_eq!(
            limited.rate_limit_per_hour.map(NonZeroU32::get),
            Some(2),
            "should keep the hourly limit"
        );
        assert!(zero.is_err(), "should reject a zero limit");
        let settings = settings_with_cookie_providers("page", "page");
        assert!(
            settings.ec.identity_providers[0]
                .rate_limit_per_hour
                .is_none(),
            "should leave the limit unset by default"
        );
    }

    #[test]
    fn providers_default_to_page_collection() {
        let mut settings = settings_with_cookie_providers("page", "page");
        settings.ec.identity_providers = vec![
            toml::from_str("id = \"sharedid\"\nkind = \"sharedid\"\nsources = [\"pubcid.org\"]")
                .expect("should parse sharedid"),
        ];

        let page = build_coordinator(&settings).expect("should build page coordinator");
        let after = build_after_response_coordinator(&settings, partners_for(&settings))
            .expect("should build after-response coordinator");

        assert!(
            page.is_some(),
            "should collect a provider without `collect` on the page"
        );
        assert!(
            after.is_none(),
            "should build no after-response coordinator"
        );
    }

    #[test]
    fn splits_providers_by_collect_timing() {
        let settings = settings_with_cookie_providers("page", "after_response");

        let page = build_coordinator(&settings)
            .expect("should build page coordinator")
            .expect("should have a page provider");
        let after = build_after_response_coordinator(&settings, partners_for(&settings))
            .expect("should build after-response coordinator")
            .expect("should have an after-response provider");

        assert!(!page.is_empty(), "should hold the page provider");
        assert!(!after.is_empty(), "should hold the after-response provider");
    }

    #[test]
    fn all_after_response_builds_no_page_coordinator() {
        let settings = settings_with_cookie_providers("after_response", "after_response");

        let page = build_coordinator(&settings).expect("should build page coordinator");
        let after = build_after_response_coordinator(&settings, partners_for(&settings))
            .expect("should build after-response coordinator");

        assert!(page.is_none(), "should build no page coordinator");
        assert!(
            after.is_some(),
            "should build the after-response coordinator"
        );
    }

    #[test]
    fn rejects_a_source_claimed_in_both_phases() {
        let mut settings = settings_with_cookie_providers("page", "after_response");
        settings.ec.identity_providers[1] = toml::from_str(
            "id = \"sharedid-late\"\nkind = \"sharedid\"\nsources = [\"pubcid.org\"]\ncollect = \"after_response\"",
        )
        .expect("should parse second sharedid");

        let partners = partner_registry(&settings.ec).expect("should build partner registry");

        let result = validate_enabled_configs(&settings.ec.identity_providers, &partners);

        assert!(
            result.is_err(),
            "should reject a source claimed by providers in different phases"
        );
    }

    #[test]
    fn partner_registry_marks_sources_excluded_where_gdpr_applies() {
        // Arrange: a disabled `uid2` provider still stored UIDs, and
        // `liveintent` resolves UID2 next to its own source.
        let mut settings = settings_with_cookie_providers("page", "page");
        for source in ["uidapi.com", "liveintent.example.com"] {
            settings.ec.partners.push(
                toml::from_str(&format!(
                    "name = \"{source}\"\nsource_domain = \"{source}\"\nbidstream_enabled = true"
                ))
                .expect("should parse partner"),
            );
        }
        settings.ec.identity_providers.push(
            toml::from_str(
                "id = \"uid2\"\nkind = \"uid2\"\nenabled = false\nsources = [\"uidapi.com\"]",
            )
            .expect("should parse uid2"),
        );
        settings.ec.identity_providers.push(
            toml::from_str(
                "id = \"liveintent\"\nkind = \"liveintent\"\nenabled = false\n\
                 sources = [\"liveintent.example.com\", \"uidapi.com\"]",
            )
            .expect("should parse liveintent"),
        );

        // Act
        let registry = partner_registry(&settings.ec).expect("should build partner registry");

        // Assert
        let excluded = |source: &str| {
            registry
                .get(source)
                .is_some_and(|partner| partner.excluded_where_gdpr_applies)
        };
        assert!(excluded("uidapi.com"), "should mark the UID2 source");
        assert!(
            !excluded("liveintent.example.com"),
            "should not mark LiveIntent's own source"
        );
        assert!(!excluded("pubcid.org"), "should not mark other sources");
    }

    #[test]
    fn partner_registry_marks_a_liveintent_uid2_source_spelled_differently() {
        // Arrange: `liveintent` names the UID2 source in upper case with a
        // trailing dot, which building the provider accepts.
        let mut settings = settings_with_cookie_providers("page", "page");
        settings.ec.partners.push(
            toml::from_str(
                "name = \"uidapi.com\"\nsource_domain = \"uidapi.com\"\nbidstream_enabled = true",
            )
            .expect("should parse partner"),
        );
        settings.ec.identity_providers.push(
            toml::from_str(
                "id = \"liveintent\"\nkind = \"liveintent\"\nenabled = false\n\
                 sources = [\"UIDAPI.COM.\"]",
            )
            .expect("should parse liveintent"),
        );

        // Act
        let registry = partner_registry(&settings.ec).expect("should build partner registry");

        // Assert
        assert!(
            registry
                .get("uidapi.com")
                .is_some_and(|partner| partner.excluded_where_gdpr_applies),
            "should mark the UID2 source despite its spelling"
        );
    }

    #[test]
    fn rejects_an_unknown_collect_timing() {
        toml::from_str::<IdentityProviderConfig>(
            "id = \"sharedid\"\nkind = \"sharedid\"\ncollect = \"later\"",
        )
        .expect_err("should reject an unknown `collect` value");
    }
}
