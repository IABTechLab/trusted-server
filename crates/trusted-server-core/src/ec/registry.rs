//! In-memory partner registry built from `[[ec.partners]]` configuration.
//!
//! Replaces the previous KV-backed partner store with a startup-validated,
//! in-memory registry. `HashMap` indexes provide O(1)
//! lookup by source domain and API key hash.

use std::collections::{HashMap, HashSet};

use error_stack::{Report, ResultExt as _};

use crate::error::TrustedServerError;
use crate::redacted::Redacted;
use crate::settings::{Ec, EcPartner};

use super::partner::{hash_api_key, normalize_partner_source_domain};

/// Provider kind under which pull-sync partners run as identity providers.
pub const PULL_SYNC_KIND: &str = "pull-sync";

/// Minimum length for inbound partner Bearer API tokens.
pub const MIN_API_TOKEN_LENGTH: usize = 32;

/// Runtime-ready partner configuration with precomputed API key hash.
#[derive(Debug, Clone)]
pub struct PartnerConfig {
    /// Human-readable partner name.
    pub name: String,
    /// Canonical `OpenRTB` EID source domain and EC KV `ids` key.
    pub source_domain: String,
    /// `OpenRTB` `atype` value.
    pub openrtb_atype: i32,
    /// Whether this partner's UIDs appear in auction `user.eids`.
    pub bidstream_enabled: bool,
    /// SHA-256 hex of the partner's API token, when inbound API access is enabled.
    pub api_key_hash: Option<String>,
    /// Max batch sync API requests per partner per minute.
    pub batch_rate_limit: u32,
    /// Whether server-to-server pull sync is enabled.
    pub pull_sync_enabled: bool,
    /// URL to call for pull sync.
    pub pull_sync_url: Option<String>,
    /// Allowlist of domains TS may call for this partner's pull sync.
    pub pull_sync_allowed_domains: Vec<String>,
    /// Legacy seconds between pull sync refreshes retained for config compatibility.
    ///
    /// Pull sync currently fills missing partner UIDs only, so this value is
    /// not used to refresh existing UIDs.
    pub pull_sync_ttl_sec: u64,
    /// Max pull sync calls per EC hash per partner per hour.
    pub pull_sync_rate_limit: u32,
    /// Outbound bearer token for pull sync requests.
    pub ts_pull_token: Option<Redacted<String>>,
    /// Whether an enabled identity provider claims this source. Browser
    /// ingest skips it and batch sync rejects it.
    pub provider_owned: bool,
}

/// In-memory partner registry with O(1) lookups by source domain and API key hash.
///
/// Built once at startup from `[[ec.partners]]` in `trusted-server.toml`.
/// All validation (source-domain format, duplicate detection, API token
/// uniqueness, pull sync consistency) happens during construction.
#[derive(Debug, Clone)]
pub struct PartnerRegistry {
    by_source_domain: HashMap<String, PartnerConfig>,
    by_api_key_hash: HashMap<String, String>,
    /// Normalized source domains in `[[ec.partners]]` order.
    order: Vec<String>,
}

impl PartnerRegistry {
    /// Validates partner structure without inspecting secret values.
    ///
    /// This is the push-time half of partner validation. API-token length,
    /// placeholder, and collision checks remain in [`Self::from_config`],
    /// after secret references have been resolved.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] when non-secret partner
    /// structure is invalid.
    pub fn validate_config_for_deploy(
        partners: &[EcPartner],
    ) -> Result<(), Report<TrustedServerError>> {
        let mut source_domains = HashSet::with_capacity(partners.len());
        let mut api_token_key_references = HashMap::with_capacity(partners.len());

        for partner in partners {
            let normalized_source = normalize_partner_source_domain(&partner.source_domain)
                .map_err(|msg| {
                    Report::new(TrustedServerError::Configuration {
                        message: format!("ec.partners: {msg}"),
                    })
                })?;

            if !source_domains.insert(normalized_source.clone()) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!("ec.partners: duplicate source_domain '{normalized_source}'"),
                }));
            }

            if let Some(api_token) = &partner.api_token
                && let Some(previous_source) =
                    api_token_key_references.insert(api_token.expose(), normalized_source.clone())
            {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "ec.partners: API token key reference is shared by source_domain \
                         '{previous_source}' and '{normalized_source}'"
                    ),
                }));
            }

            validate_rate_limits_values(partner.batch_rate_limit, partner.pull_sync_rate_limit)
                .map_err(|error| {
                    Report::new(TrustedServerError::Configuration {
                        message: format!(
                            "ec.partners: invalid rate limits for '{normalized_source}': {error}"
                        ),
                    })
                })?;

            if partner.pull_sync_enabled {
                validate_pull_sync_fields(
                    partner.pull_sync_url.as_deref(),
                    &partner.pull_sync_allowed_domains,
                    partner
                        .ts_pull_token
                        .as_ref()
                        .map(|token| token.expose().as_str()),
                    false,
                )
                .change_context(TrustedServerError::Configuration {
                    message: format!(
                        "ec.partners: pull sync config invalid for '{normalized_source}'"
                    ),
                })?;
            }
        }

        Ok(())
    }

    /// Builds a registry from the config-defined partner list.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] if any partner has an
    /// invalid source domain, duplicate source domain, duplicate API token hash,
    /// or invalid pull sync configuration.
    pub fn from_config(partners: &[EcPartner]) -> Result<Self, Report<TrustedServerError>> {
        let mut by_source_domain = HashMap::with_capacity(partners.len());
        let mut by_api_key_hash = HashMap::with_capacity(partners.len());
        let mut order = Vec::with_capacity(partners.len());

        for partner in partners {
            let normalized_source = normalize_partner_source_domain(&partner.source_domain)
                .map_err(|msg| {
                    Report::new(TrustedServerError::Configuration {
                        message: format!("ec.partners: {msg}"),
                    })
                })?;

            if by_source_domain.contains_key(&normalized_source) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!("ec.partners: duplicate source_domain '{normalized_source}'"),
                }));
            }

            let api_key_hash = if let Some(api_token) = &partner.api_token {
                validate_api_token(&normalized_source, api_token.expose())?;

                let api_key_hash = hash_api_key(api_token.expose());
                if by_api_key_hash.contains_key(&api_key_hash) {
                    return Err(Report::new(TrustedServerError::Configuration {
                        message: format!(
                            "ec.partners: source_domain '{normalized_source}' has an API token that collides \
                             with another partner's token hash"
                        ),
                    }));
                }
                Some(api_key_hash)
            } else {
                None
            };

            let config = build_partner_config(partner, &normalized_source, api_key_hash.as_deref());

            validate_rate_limits(&config).change_context(TrustedServerError::Configuration {
                message: format!(
                    "ec.partners: invalid rate limits for '{}'",
                    config.source_domain
                ),
            })?;

            if config.pull_sync_enabled {
                validate_pull_sync(&config).change_context(TrustedServerError::Configuration {
                    message: format!(
                        "ec.partners: pull sync config invalid for '{}'",
                        config.source_domain
                    ),
                })?;
            }

            if let Some(api_key_hash) = api_key_hash {
                by_api_key_hash.insert(api_key_hash, normalized_source.clone());
            }
            order.push(normalized_source.clone());
            by_source_domain.insert(normalized_source, config);
        }

        Ok(Self {
            by_source_domain,
            by_api_key_hash,
            order,
        })
    }

    /// Builds the registry from `[ec]` settings without provider ownership.
    ///
    /// For adapters with no identity-provider implementations: no source is
    /// provider-owned, so browser-supplied IDs are ingested for every source.
    /// Kind registration is unknown here, so no unregistered-kind warning is
    /// logged; use [`Self::from_ec_with_kinds`] where kinds are known.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::from_config`].
    pub fn from_ec(ec: &Ec) -> Result<Self, Report<TrustedServerError>> {
        Self::from_config(&ec.partners)
    }

    /// Builds the registry from `[ec]` settings and marks every source claimed
    /// by an enabled identity provider whose kind is registered, and every
    /// pull-sync partner when the `pull-sync` kind is registered, as
    /// provider-owned.
    ///
    /// An enabled provider with an unregistered kind never runs, so its
    /// sources stay open to browser ingest and a warning is logged.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::from_config`].
    pub fn from_ec_with_kinds(
        ec: &Ec,
        is_registered_kind: impl Fn(&str) -> bool,
    ) -> Result<Self, Report<TrustedServerError>> {
        let mut registry = Self::from_config(&ec.partners)?;
        for provider in ec.identity_providers.iter().filter(|p| p.enabled) {
            if !is_registered_kind(provider.kind()) {
                log::warn!(
                    "Identity provider '{}' has unregistered kind '{}'; its sources stay browser-ingested",
                    provider.id,
                    provider.kind()
                );
                continue;
            }
            for source in &provider.sources {
                registry.mark_provider_owned(source);
            }
        }
        let pull_sync_registered = is_registered_kind(PULL_SYNC_KIND);
        for partner in ec.partners.iter().filter(|partner| {
            pull_sync_registered && partner.pull_sync_enabled && partner.pull_sync_url.is_some()
        }) {
            registry.mark_provider_owned(&partner.source_domain);
        }
        Ok(registry)
    }

    /// Returns an empty registry (no partners configured).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            by_source_domain: HashMap::new(),
            by_api_key_hash: HashMap::new(),
            order: Vec::new(),
        }
    }

    /// Looks up a partner by canonical source domain.
    #[must_use]
    pub fn get(&self, source_domain: &str) -> Option<&PartnerConfig> {
        let normalized = normalize_partner_source_domain(source_domain).ok()?;
        self.by_source_domain.get(&normalized)
    }

    /// Looks up a partner by the SHA-256 hex hash of their API token.
    #[must_use]
    pub fn find_by_api_key_hash(&self, hash: &str) -> Option<&PartnerConfig> {
        let source_domain = self.by_api_key_hash.get(hash)?;
        self.by_source_domain.get(source_domain)
    }

    /// Looks up a partner by their `source_domain`.
    #[must_use]
    pub fn find_by_source_domain(&self, domain: &str) -> Option<&PartnerConfig> {
        self.get(domain)
    }

    /// Marks `source` as owned by an enabled identity provider.
    ///
    /// Returns `false` when the source has no `[[ec.partners]]` entry.
    pub fn mark_provider_owned(&mut self, source: &str) -> bool {
        let Ok(normalized) = normalize_partner_source_domain(source) else {
            return false;
        };
        match self.by_source_domain.get_mut(&normalized) {
            Some(partner) => {
                partner.provider_owned = true;
                true
            }
            None => false,
        }
    }

    /// Whether an enabled identity provider owns `source`.
    #[must_use]
    pub fn is_provider_owned(&self, source: &str) -> bool {
        self.find_by_source_domain(source)
            .is_some_and(|partner| partner.provider_owned)
    }

    /// Returns all partners with `pull_sync_enabled = true`.
    #[must_use]
    pub fn pull_enabled_partners(&self) -> Vec<&PartnerConfig> {
        self.by_source_domain
            .values()
            .filter(|p| p.pull_sync_enabled)
            .collect()
    }

    /// Partners in `[[ec.partners]]` order.
    pub fn in_config_order(&self) -> impl Iterator<Item = &PartnerConfig> {
        self.order
            .iter()
            .filter_map(|source| self.by_source_domain.get(source))
    }

    /// Returns an iterator over all configured partners.
    ///
    /// Iteration order is unspecified; callers that need determinism should
    /// sort by source domain before consuming the results.
    pub fn all(&self) -> impl Iterator<Item = &PartnerConfig> {
        self.by_source_domain.values()
    }

    /// Returns the number of configured partners.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_source_domain.len()
    }

    /// Returns `true` if no partners are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_source_domain.is_empty()
    }
}

fn validate_api_token(
    source_domain: &str,
    api_token: &str,
) -> Result<(), Report<TrustedServerError>> {
    if api_token.trim().is_empty() {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "ec.partners: source_domain '{source_domain}' api_token must not be empty"
            ),
        }));
    }

    if api_token.len() < MIN_API_TOKEN_LENGTH {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "ec.partners: source_domain '{source_domain}' api_token must be at least {MIN_API_TOKEN_LENGTH} bytes"
            ),
        }));
    }

    Ok(())
}

fn build_partner_config(
    partner: &EcPartner,
    normalized_source: &str,
    api_key_hash: Option<&str>,
) -> PartnerConfig {
    PartnerConfig {
        name: partner.name.clone(),
        source_domain: normalized_source.to_owned(),
        openrtb_atype: partner.openrtb_atype,
        bidstream_enabled: partner.bidstream_enabled,
        api_key_hash: api_key_hash.map(ToOwned::to_owned),
        batch_rate_limit: partner.batch_rate_limit,
        pull_sync_enabled: partner.pull_sync_enabled,
        pull_sync_url: partner.pull_sync_url.clone(),
        pull_sync_allowed_domains: partner.pull_sync_allowed_domains.clone(),
        pull_sync_ttl_sec: partner.pull_sync_ttl_sec,
        pull_sync_rate_limit: partner.pull_sync_rate_limit,
        ts_pull_token: partner.ts_pull_token.clone(),
        provider_owned: false,
    }
}

fn validate_rate_limits(config: &PartnerConfig) -> Result<(), Report<TrustedServerError>> {
    validate_rate_limits_values(config.batch_rate_limit, config.pull_sync_rate_limit).map_err(
        |message| {
            Report::new(TrustedServerError::Configuration {
                message: message.to_owned(),
            })
        },
    )
}

fn validate_rate_limits_values(
    batch_rate_limit: u32,
    pull_sync_rate_limit: u32,
) -> Result<(), &'static str> {
    if batch_rate_limit == 0 {
        return Err("batch_rate_limit must be greater than 0");
    }

    if pull_sync_rate_limit == 0 {
        return Err("pull_sync_rate_limit must be greater than 0");
    }

    Ok(())
}

fn validate_pull_sync(config: &PartnerConfig) -> Result<(), Report<TrustedServerError>> {
    validate_pull_sync_fields(
        config.pull_sync_url.as_deref(),
        &config.pull_sync_allowed_domains,
        config
            .ts_pull_token
            .as_ref()
            .map(|token| token.expose().as_str()),
        true,
    )
}

fn validate_pull_sync_fields(
    url: Option<&str>,
    allowed_domains: &[String],
    token_value: Option<&str>,
    require_nonempty_token: bool,
) -> Result<(), Report<TrustedServerError>> {
    let url_str = url.unwrap_or("");
    if url_str.is_empty() {
        return Err(Report::new(TrustedServerError::Configuration {
            message: "pull_sync_url is required when pull_sync_enabled is true".to_owned(),
        }));
    }

    if token_value.is_none() {
        return Err(Report::new(TrustedServerError::Configuration {
            message: "ts_pull_token is required when pull_sync_enabled is true".to_owned(),
        }));
    }

    let parsed = url::Url::parse(url_str).map_err(|e| {
        Report::new(TrustedServerError::Configuration {
            message: format!("pull_sync_url is not a valid URL: {e}"),
        })
    })?;

    if parsed.scheme() != "https" {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "pull_sync_url must use HTTPS, got scheme '{}'",
                parsed.scheme()
            ),
        }));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: "pull_sync_url has no hostname".to_owned(),
            })
        })?
        .trim_end_matches('.')
        .to_ascii_lowercase();

    let domain_match = allowed_domains.iter().any(|d| {
        let normalized = d.trim_end_matches('.').to_ascii_lowercase();
        host == normalized
    });

    if !domain_match {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!("pull_sync_url hostname '{host}' not in pull_sync_allowed_domains"),
        }));
    }

    if require_nonempty_token && token_value.is_some_and(|value| value.trim().is_empty()) {
        return Err(Report::new(TrustedServerError::Configuration {
            message: "ts_pull_token must not be empty when pull sync is enabled".to_owned(),
        }));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redacted::Redacted;

    fn valid_api_token(label: &str) -> String {
        format!("{label}-api-token-32-bytes-minimum")
    }

    fn make_partner(source_domain: &str, api_token: &str) -> EcPartner {
        EcPartner {
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: false,
            api_token: Some(Redacted::new(api_token.to_owned())),
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        }
    }

    #[test]
    fn from_ec_marks_enabled_provider_sources_only() {
        let mut ec = crate::test_support::tests::create_test_settings().ec;
        let mut pull = make_partner("pull.example.com", &valid_api_token("pull-partner"));
        pull.pull_sync_enabled = true;
        pull.pull_sync_url = Some("https://sync.example.com/pull".to_owned());
        pull.pull_sync_allowed_domains = vec!["sync.example.com".to_owned()];
        pull.ts_pull_token = Some(Redacted::new("example-pull-token".to_owned()));
        ec.partners = vec![
            make_partner("ids.example.com", &valid_api_token("ids-partner")),
            make_partner("other.example.com", &valid_api_token("other-partner")),
            pull,
        ];
        ec.identity_providers = vec![
            toml::from_str(
                "id = \"a\"\nkind = \"k\"\nendpoint = \"https://ids.example.com/\"\nsources = [\"ids.example.com\"]",
            )
            .expect("should parse enabled provider"),
            toml::from_str(
                "id = \"b\"\nkind = \"k\"\nenabled = false\nendpoint = \"https://ids.example.com/\"\nsources = [\"other.example.com\"]",
            )
            .expect("should parse disabled provider"),
        ];

        let registry =
            PartnerRegistry::from_ec_with_kinds(&ec, |kind| kind == "k" || kind == PULL_SYNC_KIND)
                .expect("should build registry");

        assert!(
            registry.is_provider_owned("ids.example.com"),
            "should mark enabled provider source"
        );
        assert!(
            !registry.is_provider_owned("other.example.com"),
            "should not mark disabled provider source"
        );
        assert!(
            registry.is_provider_owned("pull.example.com"),
            "should mark pull-sync partner source"
        );
    }

    #[test]
    fn from_ec_skips_providers_with_unregistered_kinds() {
        let mut ec = crate::test_support::tests::create_test_settings().ec;
        ec.partners = vec![make_partner(
            "ids.example.com",
            &valid_api_token("ids-partner"),
        )];
        ec.identity_providers = vec![
            toml::from_str(
                "id = \"a\"\nkind = \"missing\"\nendpoint = \"https://ids.example.com/\"\nsources = [\"ids.example.com\"]",
            )
            .expect("should parse enabled provider"),
        ];

        let with_kinds = PartnerRegistry::from_ec_with_kinds(&ec, |kind| kind == "k")
            .expect("should build registry");
        let plain = PartnerRegistry::from_ec(&ec).expect("should build registry");

        assert!(
            !with_kinds.is_provider_owned("ids.example.com"),
            "should not claim sources for an unregistered kind"
        );
        assert!(
            !plain.is_provider_owned("ids.example.com"),
            "should not claim sources without provider implementations"
        );
    }

    #[test]
    fn in_config_order_preserves_partner_order() {
        let partners = vec![
            make_partner("c.example.com", &valid_api_token("token-c")),
            make_partner("a.example.com", &valid_api_token("token-a")),
            make_partner("b.example.com", &valid_api_token("token-b")),
        ];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");

        let order: Vec<&str> = registry
            .in_config_order()
            .map(|partner| partner.source_domain.as_str())
            .collect();

        assert_eq!(
            order,
            ["c.example.com", "a.example.com", "b.example.com"],
            "should keep config order"
        );
    }

    #[test]
    fn empty_config_builds_empty_registry() {
        let registry = PartnerRegistry::from_config(&[]).expect("should build empty registry");
        assert!(registry.is_empty(), "should have no partners");
    }

    #[test]
    fn lookup_by_source_domain_returns_configured_partner() {
        let partners = vec![make_partner("ssp.example.com", &valid_api_token("token-a"))];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");

        let found = registry.get("ssp.example.com");
        assert!(found.is_some(), "should find partner by source domain");
        assert_eq!(
            found.expect("should exist").source_domain,
            "ssp.example.com",
            "should match source domain"
        );
    }

    #[test]
    fn lookup_by_api_key_hash_returns_partner() {
        let partners = vec![make_partner(
            "ssp.example.com",
            &valid_api_token("my-secret"),
        )];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");

        let hash = hash_api_key(&valid_api_token("my-secret"));
        let found = registry.find_by_api_key_hash(&hash);
        assert!(found.is_some(), "should find partner by API key hash");
        assert_eq!(
            found.expect("should exist").source_domain,
            "ssp.example.com",
            "should match source domain"
        );
    }

    #[test]
    fn partner_without_api_token_is_only_indexed_by_source_domain() {
        let mut partner = make_partner("ssp.example.com", &valid_api_token("unused"));
        partner.api_token = None;
        let registry =
            PartnerRegistry::from_config(&[partner]).expect("should build registry without token");

        let found = registry
            .find_by_source_domain("ssp.example.com")
            .expect("should find partner by source domain");
        assert!(
            found.api_key_hash.is_none(),
            "should not assign an API key hash"
        );
        assert!(
            registry
                .find_by_api_key_hash(&hash_api_key(&valid_api_token("unused")))
                .is_none(),
            "should not authenticate omitted API token"
        );
    }

    #[test]
    fn lookup_by_source_domain_normalizes_input() {
        let partners = vec![make_partner(
            "SSP.Example.Com.",
            &valid_api_token("token-a"),
        )];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");

        let found = registry.find_by_source_domain("ssp.example.com");
        assert!(
            found.is_some(),
            "should find partner by normalized source domain"
        );
        assert_eq!(
            found.expect("should exist").source_domain,
            "ssp.example.com",
            "should store normalized source domain"
        );
    }

    #[test]
    fn duplicate_source_domain_is_rejected_after_normalization() {
        let partners = vec![
            make_partner("same.com", &valid_api_token("token-a")),
            make_partner("SAME.com.", &valid_api_token("token-b")),
        ];
        let result = PartnerRegistry::from_config(&partners);
        assert!(result.is_err(), "should reject duplicate source domain");
    }

    #[test]
    fn deploy_validation_rejects_duplicate_api_token_key_references() {
        let shared_key = "partner_api_token";
        let partners = vec![
            make_partner("first.example.com", shared_key),
            make_partner("second.example.com", shared_key),
        ];

        let error = PartnerRegistry::validate_config_for_deploy(&partners)
            .expect_err("should reject duplicate API token key references");
        let message = error.to_string();

        assert!(message.contains("first.example.com"));
        assert!(message.contains("second.example.com"));
    }

    #[test]
    fn deploy_validation_allows_distinct_api_tokens_and_shared_pull_token_references() {
        let mut first = make_partner("first.example.com", "first_partner_api_token");
        first.pull_sync_enabled = true;
        first.pull_sync_url = Some("https://first.example.com/sync".to_owned());
        first.pull_sync_allowed_domains = vec!["first.example.com".to_owned()];
        first.ts_pull_token = Some(Redacted::new("shared_pull_token".to_owned()));

        let mut second = make_partner("second.example.com", "second_partner_api_token");
        second.pull_sync_enabled = true;
        second.pull_sync_url = Some("https://second.example.com/sync".to_owned());
        second.pull_sync_allowed_domains = vec!["second.example.com".to_owned()];
        second.ts_pull_token = Some(Redacted::new("shared_pull_token".to_owned()));

        PartnerRegistry::validate_config_for_deploy(&[first, second])
            .expect("should allow distinct API token and shared pull-token key references");
    }

    #[test]
    fn invalid_source_domain_is_rejected() {
        let partners = vec![make_partner(
            "https://ssp.example.com",
            &valid_api_token("token-a"),
        )];
        let result = PartnerRegistry::from_config(&partners);
        assert!(result.is_err(), "should reject invalid source domain");
    }

    #[test]
    fn pull_enabled_partners_filters_correctly() {
        let mut pull_partner = make_partner("pull.example.com", &valid_api_token("token-p"));
        pull_partner.api_token = None;
        pull_partner.pull_sync_enabled = true;
        pull_partner.pull_sync_url = Some("https://pull.example.com/sync".to_owned());
        pull_partner.pull_sync_allowed_domains = vec!["pull.example.com".to_owned()];
        pull_partner.ts_pull_token = Some(Redacted::new("outbound-token".to_owned()));

        let partners = vec![
            make_partner("nopull.example.com", &valid_api_token("token-np")),
            pull_partner,
        ];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");

        let pull_enabled = registry.pull_enabled_partners();
        assert_eq!(
            pull_enabled.len(),
            1,
            "should have exactly one pull-enabled partner"
        );
        assert_eq!(
            pull_enabled[0].source_domain, "pull.example.com",
            "should be the correct partner"
        );
        assert!(
            pull_enabled[0].api_key_hash.is_none(),
            "should allow pull sync without an inbound API token"
        );
        assert_eq!(
            pull_enabled[0]
                .ts_pull_token
                .as_ref()
                .expect("should keep pull token")
                .expose(),
            "outbound-token",
            "should preserve the token without unwrapping it in the registry"
        );
    }

    #[test]
    fn partner_debug_output_redacts_pull_token() {
        let mut partner = make_partner("pull.example.com", &valid_api_token("token-p"));
        partner.pull_sync_enabled = true;
        partner.pull_sync_url = Some("https://pull.example.com/sync".to_owned());
        partner.pull_sync_allowed_domains = vec!["pull.example.com".to_owned()];
        partner.ts_pull_token = Some(Redacted::new("outbound-token".to_owned()));

        let registry = PartnerRegistry::from_config(&[partner]).expect("should build registry");
        let configured = registry
            .get("pull.example.com")
            .expect("should find configured partner");

        let debug_output = format!("{configured:?}");
        assert!(
            !debug_output.contains("outbound-token"),
            "should not expose the pull token in debug output"
        );
        assert!(
            debug_output.contains("[REDACTED]"),
            "should render the pull token through Redacted debug output"
        );
    }

    #[test]
    fn empty_api_token_is_rejected() {
        let partner = make_partner("ssp.example.com", "   ");

        let result = PartnerRegistry::from_config(&[partner]);
        assert!(result.is_err(), "should reject empty api_token");
    }

    #[test]
    fn short_api_token_is_rejected() {
        let partner = make_partner("ssp.example.com", "short-token");

        let result = PartnerRegistry::from_config(&[partner]);
        assert!(result.is_err(), "should reject short api_token");
    }

    #[test]
    fn api_token_at_minimum_length_is_accepted() {
        let token = "x".repeat(MIN_API_TOKEN_LENGTH);
        let partners = vec![make_partner("ssp.example.com", &token)];

        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        assert!(
            registry.get("ssp.example.com").is_some(),
            "should accept minimum-length token"
        );
    }

    #[test]
    fn zero_batch_rate_limit_is_rejected() {
        let mut partner = make_partner("ssp.example.com", &valid_api_token("token-a"));
        partner.batch_rate_limit = 0;

        let result = PartnerRegistry::from_config(&[partner]);
        assert!(result.is_err(), "should reject zero batch_rate_limit");
    }

    #[test]
    fn zero_pull_sync_rate_limit_is_rejected() {
        let mut partner = make_partner("pull.example.com", &valid_api_token("token-p"));
        partner.pull_sync_enabled = true;
        partner.pull_sync_url = Some("https://pull.example.com/sync".to_owned());
        partner.pull_sync_allowed_domains = vec!["pull.example.com".to_owned()];
        partner.pull_sync_rate_limit = 0;
        partner.ts_pull_token = Some(Redacted::new("outbound-token".to_owned()));

        let result = PartnerRegistry::from_config(&[partner]);
        assert!(result.is_err(), "should reject zero pull_sync_rate_limit");
    }
}
