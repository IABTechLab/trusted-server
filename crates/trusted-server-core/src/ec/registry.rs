//! In-memory partner registry built from `[[ec.partners]]` configuration.
//!
//! Replaces the KV-backed [`PartnerStore`](super::partner::PartnerStore) with
//! a startup-validated, in-memory registry. Three `HashMap` indexes provide
//! O(1) lookup by partner ID, API key hash, and source domain.

use std::collections::HashMap;

use error_stack::{Report, ResultExt};

use crate::error::TrustedServerError;
use crate::redacted::Redacted;
use crate::settings::EcPartner;

use super::partner::{hash_api_key, validate_partner_id};

/// Runtime-ready partner configuration with precomputed API key hash.
#[derive(Debug, Clone)]
pub struct PartnerConfig {
    /// Unique partner identifier.
    pub id: String,
    /// Human-readable partner name.
    pub name: String,
    /// `OpenRTB` `source.domain` for EID entries.
    pub source_domain: String,
    /// `OpenRTB` `atype` value.
    pub openrtb_atype: u8,
    /// Whether this partner's UIDs appear in auction `user.eids`.
    pub bidstream_enabled: bool,
    /// SHA-256 hex of the partner's API token (precomputed at startup).
    pub api_key_hash: String,
    /// Max batch sync API requests per partner per minute.
    pub batch_rate_limit: u32,
    /// Whether server-to-server pull sync is enabled.
    pub pull_sync_enabled: bool,
    /// URL to call for pull sync.
    pub pull_sync_url: Option<String>,
    /// Allowlist of domains TS may call for this partner's pull sync.
    pub pull_sync_allowed_domains: Vec<String>,
    /// Seconds between pull sync refreshes.
    pub pull_sync_ttl_sec: u64,
    /// Max pull sync calls per EC hash per partner per hour.
    pub pull_sync_rate_limit: u32,
    /// Outbound bearer token for pull sync requests.
    pub ts_pull_token: Option<Redacted<String>>,
}

/// In-memory partner registry with O(1) lookups by ID, API key hash,
/// and source domain.
///
/// Built once at startup from `[[ec.partners]]` in `trusted-server.toml`.
/// All validation (ID format, duplicate detection, pull sync consistency)
/// happens during construction.
#[derive(Debug, Clone)]
pub struct PartnerRegistry {
    by_id: HashMap<String, PartnerConfig>,
    by_api_key_hash: HashMap<String, String>,
    by_source_domain: HashMap<String, String>,
}

impl PartnerRegistry {
    /// Builds a registry from the config-defined partner list.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] if any partner has an
    /// invalid ID, duplicate ID, duplicate API token hash, duplicate source
    /// domain, or invalid pull sync configuration.
    pub fn from_config(partners: &[EcPartner]) -> Result<Self, Report<TrustedServerError>> {
        let mut by_id = HashMap::with_capacity(partners.len());
        let mut by_api_key_hash = HashMap::with_capacity(partners.len());
        let mut by_source_domain = HashMap::with_capacity(partners.len());

        for partner in partners {
            validate_partner_id(&partner.id).map_err(|msg| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("ec.partners: {msg}"),
                })
            })?;

            if by_id.contains_key(&partner.id) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!("ec.partners: duplicate partner ID '{}'", partner.id),
                }));
            }

            let api_key_hash = hash_api_key(partner.api_token.expose());

            if by_api_key_hash.contains_key(&api_key_hash) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "ec.partners: partner '{}' has an API token that collides \
                         with another partner's token hash",
                        partner.id
                    ),
                }));
            }

            let normalized_source = partner.source_domain.to_ascii_lowercase();
            if by_source_domain.contains_key(&normalized_source) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "ec.partners: duplicate source_domain '{}' (partner '{}')",
                        partner.source_domain, partner.id
                    ),
                }));
            }

            let config = build_partner_config(partner, &api_key_hash)?;

            validate_rate_limits(&config).change_context(TrustedServerError::Configuration {
                message: format!("ec.partners: invalid rate limits for '{}'", partner.id),
            })?;

            if config.pull_sync_enabled {
                validate_pull_sync(&config).change_context(TrustedServerError::Configuration {
                    message: format!("ec.partners: pull sync config invalid for '{}'", partner.id),
                })?;
            }

            by_api_key_hash.insert(api_key_hash, partner.id.clone());
            by_source_domain.insert(normalized_source, partner.id.clone());
            by_id.insert(partner.id.clone(), config);
        }

        Ok(Self {
            by_id,
            by_api_key_hash,
            by_source_domain,
        })
    }

    /// Returns an empty registry (no partners configured).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            by_id: HashMap::new(),
            by_api_key_hash: HashMap::new(),
            by_source_domain: HashMap::new(),
        }
    }

    /// Looks up a partner by ID.
    #[must_use]
    pub fn get(&self, partner_id: &str) -> Option<&PartnerConfig> {
        self.by_id.get(partner_id)
    }

    /// Looks up a partner by the SHA-256 hex hash of their API token.
    #[must_use]
    pub fn find_by_api_key_hash(&self, hash: &str) -> Option<&PartnerConfig> {
        self.by_api_key_hash
            .get(hash)
            .and_then(|id| self.by_id.get(id))
    }

    /// Looks up a partner by their `source_domain` (case-insensitive).
    #[must_use]
    pub fn find_by_source_domain(&self, domain: &str) -> Option<&PartnerConfig> {
        self.by_source_domain
            .get(&domain.to_ascii_lowercase())
            .and_then(|id| self.by_id.get(id))
    }

    /// Returns all partners with `pull_sync_enabled = true`.
    #[must_use]
    pub fn pull_enabled_partners(&self) -> Vec<&PartnerConfig> {
        self.by_id
            .values()
            .filter(|p| p.pull_sync_enabled)
            .collect()
    }

    /// Returns an iterator over all configured partners.
    ///
    /// Iteration order is unspecified; callers that need determinism should
    /// sort by partner ID before consuming the results.
    pub fn all(&self) -> impl Iterator<Item = &PartnerConfig> {
        self.by_id.values()
    }

    /// Returns the number of configured partners.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Returns `true` if no partners are configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

fn build_partner_config(
    partner: &EcPartner,
    api_key_hash: &str,
) -> Result<PartnerConfig, Report<TrustedServerError>> {
    Ok(PartnerConfig {
        id: partner.id.clone(),
        name: partner.name.clone(),
        source_domain: partner.source_domain.clone(),
        openrtb_atype: partner.openrtb_atype,
        bidstream_enabled: partner.bidstream_enabled,
        api_key_hash: api_key_hash.to_owned(),
        batch_rate_limit: partner.batch_rate_limit,
        pull_sync_enabled: partner.pull_sync_enabled,
        pull_sync_url: partner.pull_sync_url.clone(),
        pull_sync_allowed_domains: partner.pull_sync_allowed_domains.clone(),
        pull_sync_ttl_sec: partner.pull_sync_ttl_sec,
        pull_sync_rate_limit: partner.pull_sync_rate_limit,
        ts_pull_token: partner.ts_pull_token.clone(),
    })
}

fn validate_rate_limits(config: &PartnerConfig) -> Result<(), Report<TrustedServerError>> {
    if config.batch_rate_limit == 0 {
        return Err(Report::new(TrustedServerError::Configuration {
            message: "batch_rate_limit must be greater than 0".to_owned(),
        }));
    }

    if config.pull_sync_rate_limit == 0 {
        return Err(Report::new(TrustedServerError::Configuration {
            message: "pull_sync_rate_limit must be greater than 0".to_owned(),
        }));
    }

    Ok(())
}

fn validate_pull_sync(config: &PartnerConfig) -> Result<(), Report<TrustedServerError>> {
    let url_str = config.pull_sync_url.as_deref().unwrap_or("");
    if url_str.is_empty() {
        return Err(Report::new(TrustedServerError::Configuration {
            message: "pull_sync_url is required when pull_sync_enabled is true".to_owned(),
        }));
    }

    if config
        .ts_pull_token
        .as_ref()
        .map(|token| token.expose().trim().is_empty())
        .unwrap_or(true)
    {
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

    let domain_match = config.pull_sync_allowed_domains.iter().any(|d| {
        let normalized = d.trim_end_matches('.').to_ascii_lowercase();
        host == normalized
    });

    if !domain_match {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!("pull_sync_url hostname '{host}' not in pull_sync_allowed_domains"),
        }));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redacted::Redacted;

    fn make_partner(id: &str, source_domain: &str, api_token: &str) -> EcPartner {
        EcPartner {
            id: id.to_owned(),
            name: format!("Partner {id}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: false,
            api_token: Redacted::new(api_token.to_owned()),
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
    fn empty_config_builds_empty_registry() {
        let registry = PartnerRegistry::from_config(&[]).expect("should build empty registry");
        assert!(registry.is_empty(), "should have no partners");
    }

    #[test]
    fn lookup_by_id_returns_configured_partner() {
        let partners = vec![make_partner("ssp_x", "ssp.example.com", "token-a")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");

        let found = registry.get("ssp_x");
        assert!(found.is_some(), "should find partner by ID");
        assert_eq!(
            found.expect("should exist").source_domain,
            "ssp.example.com",
            "should match source domain"
        );
    }

    #[test]
    fn lookup_by_api_key_hash_returns_partner() {
        let partners = vec![make_partner("ssp_x", "ssp.example.com", "my-secret")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");

        let hash = hash_api_key("my-secret");
        let found = registry.find_by_api_key_hash(&hash);
        assert!(found.is_some(), "should find partner by API key hash");
        assert_eq!(
            found.expect("should exist").id,
            "ssp_x",
            "should match partner ID"
        );
    }

    #[test]
    fn lookup_by_source_domain_is_case_insensitive() {
        let partners = vec![make_partner("ssp_x", "SSP.Example.Com", "token-a")];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");

        let found = registry.find_by_source_domain("ssp.example.com");
        assert!(
            found.is_some(),
            "should find partner by lowercase source domain"
        );
    }

    #[test]
    fn duplicate_partner_id_is_rejected() {
        let partners = vec![
            make_partner("ssp_x", "a.com", "token-a"),
            make_partner("ssp_x", "b.com", "token-b"),
        ];
        let result = PartnerRegistry::from_config(&partners);
        assert!(result.is_err(), "should reject duplicate partner ID");
    }

    #[test]
    fn duplicate_source_domain_is_rejected() {
        let partners = vec![
            make_partner("ssp_a", "same.com", "token-a"),
            make_partner("ssp_b", "same.com", "token-b"),
        ];
        let result = PartnerRegistry::from_config(&partners);
        assert!(result.is_err(), "should reject duplicate source domain");
    }

    #[test]
    fn reserved_partner_id_is_rejected() {
        let partners = vec![make_partner("ec", "ec.example.com", "token-a")];
        let result = PartnerRegistry::from_config(&partners);
        assert!(result.is_err(), "should reject reserved partner ID 'ec'");
    }

    #[test]
    fn pull_enabled_partners_filters_correctly() {
        let mut pull_partner = make_partner("puller", "pull.example.com", "token-p");
        pull_partner.pull_sync_enabled = true;
        pull_partner.pull_sync_url = Some("https://pull.example.com/sync".to_owned());
        pull_partner.pull_sync_allowed_domains = vec!["pull.example.com".to_owned()];
        pull_partner.ts_pull_token = Some(Redacted::new("outbound-token".to_owned()));

        let partners = vec![
            make_partner("no_pull", "nopull.example.com", "token-np"),
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
            pull_enabled[0].id, "puller",
            "should be the correct partner"
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
        let mut partner = make_partner("puller", "pull.example.com", "token-p");
        partner.pull_sync_enabled = true;
        partner.pull_sync_url = Some("https://pull.example.com/sync".to_owned());
        partner.pull_sync_allowed_domains = vec!["pull.example.com".to_owned()];
        partner.ts_pull_token = Some(Redacted::new("outbound-token".to_owned()));

        let registry = PartnerRegistry::from_config(&[partner]).expect("should build registry");
        let configured = registry
            .get("puller")
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
    fn zero_batch_rate_limit_is_rejected() {
        let mut partner = make_partner("ssp_x", "ssp.example.com", "token-a");
        partner.batch_rate_limit = 0;

        let result = PartnerRegistry::from_config(&[partner]);
        assert!(result.is_err(), "should reject zero batch_rate_limit");
    }

    #[test]
    fn zero_pull_sync_rate_limit_is_rejected() {
        let mut partner = make_partner("puller", "pull.example.com", "token-p");
        partner.pull_sync_enabled = true;
        partner.pull_sync_url = Some("https://pull.example.com/sync".to_owned());
        partner.pull_sync_allowed_domains = vec!["pull.example.com".to_owned()];
        partner.pull_sync_rate_limit = 0;
        partner.ts_pull_token = Some(Redacted::new("outbound-token".to_owned()));

        let result = PartnerRegistry::from_config(&[partner]);
        assert!(result.is_err(), "should reject zero pull_sync_rate_limit");
    }
}
