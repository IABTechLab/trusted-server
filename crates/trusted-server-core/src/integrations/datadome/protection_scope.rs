use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use error_stack::Report;
use regex::Regex;
use serde::Deserialize;

use crate::error::TrustedServerError;
use crate::platform::{RuntimeServices, StoreName};

use super::{DataDomeConfig, DATADOME_INTEGRATION_ID};

/// Configured source for dynamic IP CIDR bypass lists.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectionIpCidrSourceConfig {
    /// Config Store containing a comma, whitespace, or JSON-array encoded CIDR list.
    #[serde(default = "default_ip_cidr_source_store")]
    pub config_store: String,
    /// Config Store key containing the CIDR list.
    pub key: String,
}

/// Configured request-scope exclusion rule for `DataDome` protection.
#[derive(Debug, Clone, Deserialize)]
// Do not add `deny_unknown_fields` here: serde rejects valid flattened
// internally tagged matcher fields when both are combined. The matcher enum
// still denies unknown fields for rule payload validation.
pub struct ProtectionExclusionRuleConfig {
    /// Operator-friendly identifier included in logs.
    #[serde(alias = "name")]
    pub id: String,
    /// Enables the rule. Defaults to true when a rule is present.
    #[serde(default = "default_enabled_rule")]
    pub enabled: bool,
    /// Optional methods this rule applies to. Empty means every method.
    #[serde(default, deserialize_with = "crate::settings::vec_from_seq_or_map")]
    pub methods: Vec<String>,
    /// Matcher-specific rule configuration.
    #[serde(flatten)]
    pub matcher: ProtectionMatcherConfig,
}

/// Matchers supported by `DataDome` protection-scope exclusion rules.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProtectionMatcherConfig {
    /// Match exact request paths.
    PathExact {
        #[serde(deserialize_with = "crate::settings::vec_from_seq_or_map")]
        paths: Vec<String>,
    },
    /// Match request path prefixes.
    PathPrefix {
        #[serde(deserialize_with = "crate::settings::vec_from_seq_or_map")]
        prefixes: Vec<String>,
    },
    /// Match request paths with one or more regexes.
    PathRegex {
        #[serde(deserialize_with = "crate::settings::vec_from_seq_or_map")]
        patterns: Vec<String>,
    },
    /// Match when any query parameter has a non-empty value.
    QueryParamNonEmpty {
        #[serde(deserialize_with = "crate::settings::vec_from_seq_or_map")]
        names: Vec<String>,
    },
    /// Match client autonomous system numbers.
    Asn {
        #[serde(deserialize_with = "crate::settings::vec_from_seq_or_map")]
        values: Vec<u32>,
    },
    /// Match client IP CIDRs configured inline.
    IpCidr {
        #[serde(deserialize_with = "crate::settings::vec_from_seq_or_map")]
        cidrs: Vec<String>,
    },
    /// Match client IP CIDRs loaded from Config Store.
    IpCidrSource { config_store: String, key: String },
}

/// Facts used by `DataDome` protection-scope matchers.
pub(super) struct ProtectionRequestFacts<'a> {
    pub(super) method: &'a str,
    pub(super) path: &'a str,
    pub(super) query: Option<&'a str>,
    pub(super) client_ip: Option<IpAddr>,
    pub(super) asn: Option<u32>,
}

/// Result of evaluating whether `DataDome` protection should run.
pub(super) enum ProtectionScopeDecision {
    Protect,
    Skip {
        rule_id: String,
        reason: &'static str,
    },
}

/// Compiled `DataDome` protection-scope rules.
pub(super) struct ProtectionScope {
    excluded_methods: MethodSet,
    excluded_asns: HashSet<u32>,
    excluded_ip_cidrs: Vec<IpCidr>,
    excluded_ip_cidr_sources: Vec<ProtectionIpCidrSource>,
    exclusion_rules: Vec<ProtectionExclusionRule>,
    ip_list_cache_ttl: Duration,
}

#[derive(Debug, Clone)]
struct MethodSet {
    methods: HashSet<String>,
}

#[derive(Debug, Clone)]
struct ProtectionIpCidrSource {
    config_store: String,
    key: String,
}

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
struct ProtectionIpCidrSourceCacheKey {
    config_store: String,
    key: String,
}

#[derive(Debug, Clone)]
struct CachedIpCidrSource {
    cidrs: Vec<IpCidr>,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
struct ProtectionExclusionRule {
    id: String,
    methods: Option<MethodSet>,
    matcher: ProtectionMatcher,
}

#[derive(Debug, Clone)]
enum ProtectionMatcher {
    PathExact(HashSet<String>),
    PathPrefix(Vec<String>),
    PathRegex(Vec<Regex>),
    QueryParamNonEmpty(HashSet<String>),
    Asn(HashSet<u32>),
    IpCidr(Vec<IpCidr>),
    IpCidrSource(ProtectionIpCidrSource),
}

#[derive(Debug, Clone)]
enum IpCidr {
    V4 { network: u32, prefix: u8 },
    V6 { network: u128, prefix: u8 },
}

static IP_CIDR_SOURCE_CACHE: LazyLock<
    Mutex<HashMap<ProtectionIpCidrSourceCacheKey, CachedIpCidrSource>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn default_ip_cidr_source_store() -> String {
    "datadome_ip_bypass".to_string()
}

fn default_enabled_rule() -> bool {
    true
}

fn datadome_error(message: impl Into<String>) -> TrustedServerError {
    TrustedServerError::Integration {
        integration: DATADOME_INTEGRATION_ID.to_string(),
        message: message.into(),
    }
}

impl ProtectionScope {
    pub(super) fn compile(config: &DataDomeConfig) -> Result<Self, Report<TrustedServerError>> {
        let excluded_methods = MethodSet::new(&config.protection_excluded_methods)?;
        let excluded_ip_cidrs = compile_ip_cidrs(
            &config.protection_excluded_ip_cidrs,
            "protection_excluded_ip_cidrs",
        )?;
        let excluded_ip_cidr_sources = config
            .protection_excluded_ip_cidr_sources
            .iter()
            .map(ProtectionIpCidrSource::from_config)
            .collect::<Result<Vec<_>, _>>()?;
        let exclusion_rules = config
            .protection_exclusion_rules
            .iter()
            .filter(|rule| rule.enabled)
            .map(ProtectionExclusionRule::compile)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            excluded_methods,
            excluded_asns: config.protection_excluded_asns.iter().copied().collect(),
            excluded_ip_cidrs,
            excluded_ip_cidr_sources,
            exclusion_rules,
            ip_list_cache_ttl: Duration::from_secs(config.protection_ip_list_cache_ttl_seconds),
        })
    }

    pub(super) fn evaluate(
        &self,
        facts: &ProtectionRequestFacts<'_>,
        services: &RuntimeServices,
    ) -> ProtectionScopeDecision {
        if self.excluded_methods.matches(facts.method) {
            return ProtectionScopeDecision::Skip {
                rule_id: "excluded-methods".to_string(),
                reason: "method",
            };
        }

        if let Some(client_ip) = facts.client_ip {
            if cidrs_match(&self.excluded_ip_cidrs, client_ip) {
                return ProtectionScopeDecision::Skip {
                    rule_id: "excluded-ip-cidrs".to_string(),
                    reason: "client_ip",
                };
            }

            for source in &self.excluded_ip_cidr_sources {
                if source.matches(client_ip, services, self.ip_list_cache_ttl) {
                    return ProtectionScopeDecision::Skip {
                        rule_id: source.rule_id(),
                        reason: "client_ip_source",
                    };
                }
            }
        }

        if facts
            .asn
            .is_some_and(|asn| self.excluded_asns.contains(&asn))
        {
            return ProtectionScopeDecision::Skip {
                rule_id: "excluded-asns".to_string(),
                reason: "asn",
            };
        }

        for rule in &self.exclusion_rules {
            if rule.matches(facts, services, self.ip_list_cache_ttl) {
                return ProtectionScopeDecision::Skip {
                    rule_id: rule.id.clone(),
                    reason: rule.matcher.reason(),
                };
            }
        }

        ProtectionScopeDecision::Protect
    }
}

impl MethodSet {
    fn new(methods: &[String]) -> Result<Self, Report<TrustedServerError>> {
        let mut normalized = HashSet::new();
        for method in methods {
            let method = normalize_method(method)?;
            normalized.insert(method);
        }
        Ok(Self {
            methods: normalized,
        })
    }

    fn optional(methods: &[String]) -> Result<Option<Self>, Report<TrustedServerError>> {
        if methods.is_empty() {
            return Ok(None);
        }
        Self::new(methods).map(Some)
    }

    fn matches(&self, method: &str) -> bool {
        self.methods.contains(&method.to_ascii_uppercase())
    }
}

impl ProtectionIpCidrSource {
    fn from_config(
        config: &ProtectionIpCidrSourceConfig,
    ) -> Result<Self, Report<TrustedServerError>> {
        let config_store = config.config_store.trim().to_string();
        let key = config.key.trim().to_string();
        if config_store.is_empty() || key.is_empty() {
            return Err(Report::new(datadome_error(
                "protection_excluded_ip_cidr_sources config_store and key must not be empty",
            )));
        }
        Ok(Self { config_store, key })
    }

    fn from_matcher_fields(
        config_store: &str,
        key: &str,
    ) -> Result<Self, Report<TrustedServerError>> {
        let config = ProtectionIpCidrSourceConfig {
            config_store: config_store.to_string(),
            key: key.to_string(),
        };
        Self::from_config(&config)
    }

    fn rule_id(&self) -> String {
        format!("ip-cidr-source:{}:{}", self.config_store, self.key)
    }

    fn matches(&self, client_ip: IpAddr, services: &RuntimeServices, cache_ttl: Duration) -> bool {
        match self.load_cidrs(services, cache_ttl) {
            Ok(cidrs) => cidrs_match(&cidrs, client_ip),
            Err(err) => {
                log::warn!(
                    "[datadome] Failed to load IP CIDR bypass source {}:{}: {err:?}",
                    self.config_store,
                    self.key
                );
                false
            }
        }
    }

    fn load_cidrs(
        &self,
        services: &RuntimeServices,
        cache_ttl: Duration,
    ) -> Result<Vec<IpCidr>, Report<TrustedServerError>> {
        let cache_key = ProtectionIpCidrSourceCacheKey {
            config_store: self.config_store.clone(),
            key: self.key.clone(),
        };
        if let Some(cached) = IP_CIDR_SOURCE_CACHE
            .lock()
            .expect("should lock DataDome IP CIDR source cache")
            .get(&cache_key)
            .filter(|cached| cached.expires_at > Instant::now())
            .cloned()
        {
            return Ok(cached.cidrs);
        }

        let store_name = StoreName::from(self.config_store.as_str());
        let raw = services
            .config_store()
            .get(&store_name, &self.key)
            .map_err(|err| {
                err.change_context(datadome_error(
                    "Failed to read DataDome IP CIDR bypass list from Config Store",
                ))
            })?;
        let cidr_strings = parse_cidr_list_value(&raw).map_err(|message| {
            Report::new(datadome_error(format!(
                "Invalid DataDome IP CIDR bypass list {}:{}: {message}",
                self.config_store, self.key
            )))
        })?;
        let cidrs = compile_ip_cidrs(&cidr_strings, "Config Store IP CIDR bypass list")?;

        IP_CIDR_SOURCE_CACHE
            .lock()
            .expect("should lock DataDome IP CIDR source cache")
            .insert(
                cache_key,
                CachedIpCidrSource {
                    cidrs: cidrs.clone(),
                    expires_at: Instant::now() + cache_ttl,
                },
            );

        Ok(cidrs)
    }
}

impl ProtectionExclusionRule {
    fn compile(config: &ProtectionExclusionRuleConfig) -> Result<Self, Report<TrustedServerError>> {
        let id = config.id.trim().to_string();
        if id.is_empty() {
            return Err(Report::new(datadome_error(
                "protection_exclusion_rules id must not be empty",
            )));
        }

        Ok(Self {
            id,
            methods: MethodSet::optional(&config.methods)?,
            matcher: ProtectionMatcher::compile(&config.matcher)?,
        })
    }

    fn matches(
        &self,
        facts: &ProtectionRequestFacts<'_>,
        services: &RuntimeServices,
        cache_ttl: Duration,
    ) -> bool {
        if let Some(methods) = &self.methods {
            if !methods.matches(facts.method) {
                return false;
            }
        }

        self.matcher.matches(facts, services, cache_ttl)
    }
}

impl ProtectionMatcher {
    fn compile(config: &ProtectionMatcherConfig) -> Result<Self, Report<TrustedServerError>> {
        match config {
            ProtectionMatcherConfig::PathExact { paths } => {
                ensure_non_empty(paths, "path_exact paths")?;
                Ok(Self::PathExact(
                    paths.iter().map(|path| path.trim().to_string()).collect(),
                ))
            }
            ProtectionMatcherConfig::PathPrefix { prefixes } => {
                ensure_non_empty(prefixes, "path_prefix prefixes")?;
                Ok(Self::PathPrefix(
                    prefixes
                        .iter()
                        .map(|prefix| prefix.trim().to_string())
                        .collect(),
                ))
            }
            ProtectionMatcherConfig::PathRegex { patterns } => {
                ensure_non_empty(patterns, "path_regex patterns")?;
                let regexes = patterns
                    .iter()
                    .map(|pattern| {
                        Regex::new(pattern).map_err(|err| {
                            Report::new(datadome_error(format!(
                                "Invalid protection_exclusion_rules path_regex pattern: {err}"
                            )))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Self::PathRegex(regexes))
            }
            ProtectionMatcherConfig::QueryParamNonEmpty { names } => {
                ensure_non_empty(names, "query_param_non_empty names")?;
                Ok(Self::QueryParamNonEmpty(
                    names.iter().map(|name| name.trim().to_string()).collect(),
                ))
            }
            ProtectionMatcherConfig::Asn { values } => {
                if values.is_empty() {
                    return Err(Report::new(datadome_error("asn values must not be empty")));
                }
                Ok(Self::Asn(values.iter().copied().collect()))
            }
            ProtectionMatcherConfig::IpCidr { cidrs } => {
                ensure_non_empty(cidrs, "ip_cidr cidrs")?;
                Ok(Self::IpCidr(compile_ip_cidrs(cidrs, "ip_cidr cidrs")?))
            }
            ProtectionMatcherConfig::IpCidrSource { config_store, key } => Ok(Self::IpCidrSource(
                ProtectionIpCidrSource::from_matcher_fields(config_store, key)?,
            )),
        }
    }

    fn matches(
        &self,
        facts: &ProtectionRequestFacts<'_>,
        services: &RuntimeServices,
        cache_ttl: Duration,
    ) -> bool {
        match self {
            ProtectionMatcher::PathExact(paths) => paths.contains(facts.path),
            ProtectionMatcher::PathPrefix(prefixes) => {
                prefixes.iter().any(|prefix| facts.path.starts_with(prefix))
            }
            ProtectionMatcher::PathRegex(regexes) => {
                regexes.iter().any(|regex| regex.is_match(facts.path))
            }
            ProtectionMatcher::QueryParamNonEmpty(names) => {
                query_param_non_empty(facts.query, names)
            }
            ProtectionMatcher::Asn(values) => facts.asn.is_some_and(|asn| values.contains(&asn)),
            ProtectionMatcher::IpCidr(cidrs) => facts
                .client_ip
                .is_some_and(|client_ip| cidrs_match(cidrs, client_ip)),
            ProtectionMatcher::IpCidrSource(source) => facts
                .client_ip
                .is_some_and(|client_ip| source.matches(client_ip, services, cache_ttl)),
        }
    }

    fn reason(&self) -> &'static str {
        match self {
            ProtectionMatcher::PathExact(_) => "path_exact",
            ProtectionMatcher::PathPrefix(_) => "path_prefix",
            ProtectionMatcher::PathRegex(_) => "path_regex",
            ProtectionMatcher::QueryParamNonEmpty(_) => "query_param_non_empty",
            ProtectionMatcher::Asn(_) => "asn",
            ProtectionMatcher::IpCidr(_) => "ip_cidr",
            ProtectionMatcher::IpCidrSource(_) => "ip_cidr_source",
        }
    }
}

impl FromStr for IpCidr {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err("CIDR must not be empty".to_string());
        }

        let (addr, prefix) = match raw.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (raw, None),
        };
        let ip = addr
            .parse::<IpAddr>()
            .map_err(|err| format!("invalid IP address `{addr}`: {err}"))?;

        match ip {
            IpAddr::V4(addr) => {
                let prefix = parse_prefix(prefix, 32)?;
                Ok(Self::V4 {
                    network: u32::from(addr) & v4_mask(prefix),
                    prefix,
                })
            }
            IpAddr::V6(addr) => {
                let prefix = parse_prefix(prefix, 128)?;
                Ok(Self::V6 {
                    network: u128::from(addr) & v6_mask(prefix),
                    prefix,
                })
            }
        }
    }
}

impl IpCidr {
    fn contains(&self, ip: IpAddr) -> bool {
        match (self, ip) {
            (IpCidr::V4 { network, prefix }, IpAddr::V4(addr)) => {
                (u32::from(addr) & v4_mask(*prefix)) == *network
            }
            (IpCidr::V6 { network, prefix }, IpAddr::V6(addr)) => {
                (u128::from(addr) & v6_mask(*prefix)) == *network
            }
            _ => false,
        }
    }
}

fn normalize_method(method: &str) -> Result<String, Report<TrustedServerError>> {
    let method = method.trim();
    if method.is_empty() {
        return Err(Report::new(datadome_error(
            "DataDome protection excluded methods must not contain empty values",
        )));
    }
    Ok(method.to_ascii_uppercase())
}

fn ensure_non_empty(values: &[String], name: &str) -> Result<(), Report<TrustedServerError>> {
    if values.iter().any(|value| value.trim().is_empty()) || values.is_empty() {
        return Err(Report::new(datadome_error(format!(
            "DataDome protection {name} must not contain empty values"
        ))));
    }
    Ok(())
}

fn compile_ip_cidrs(
    raw_cidrs: &[String],
    name: &str,
) -> Result<Vec<IpCidr>, Report<TrustedServerError>> {
    raw_cidrs
        .iter()
        .map(|raw| {
            raw.parse::<IpCidr>().map_err(|err| {
                Report::new(datadome_error(format!(
                    "Invalid DataDome protection {name} entry `{raw}`: {err}"
                )))
            })
        })
        .collect()
}

fn cidrs_match(cidrs: &[IpCidr], ip: IpAddr) -> bool {
    cidrs.iter().any(|cidr| cidr.contains(ip))
}

fn parse_prefix(prefix: Option<&str>, max: u8) -> Result<u8, String> {
    let Some(prefix) = prefix else {
        return Ok(max);
    };
    let prefix = prefix
        .parse::<u8>()
        .map_err(|err| format!("invalid prefix `{prefix}`: {err}"))?;
    if prefix > max {
        return Err(format!("prefix `{prefix}` exceeds maximum {max}"));
    }
    Ok(prefix)
}

fn v4_mask(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    }
}

fn v6_mask(prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    }
}

fn parse_cidr_list_value(value: &str) -> Result<Vec<String>, String> {
    if value.trim().starts_with('[') {
        return serde_json::from_str::<Vec<String>>(value)
            .map_err(|err| format!("CIDR JSON array is invalid: {err}"));
    }

    Ok(value
        .split(|ch: char| ch == ',' || ch.is_whitespace())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect())
}

fn query_param_non_empty(query: Option<&str>, names: &HashSet<String>) -> bool {
    let Some(query) = query else {
        return false;
    };

    url::form_urlencoded::parse(query.as_bytes())
        .any(|(key, value)| names.contains(key.as_ref()) && !value.is_empty())
}

#[cfg(test)]
pub(super) fn clear_ip_cidr_source_cache_for_tests() {
    IP_CIDR_SOURCE_CACHE
        .lock()
        .expect("should lock DataDome IP CIDR source cache")
        .clear();
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{Ipv4Addr, Ipv6Addr};

    use crate::platform::test_support::{
        build_services_with_config_and_secret, HashMapConfigStore, NoopSecretStore,
    };

    use super::*;

    fn facts<'a>(
        method: &'a str,
        path: &'a str,
        query: Option<&'a str>,
        client_ip: Option<IpAddr>,
        asn: Option<u32>,
    ) -> ProtectionRequestFacts<'a> {
        ProtectionRequestFacts {
            method,
            path,
            query,
            client_ip,
            asn,
        }
    }

    fn config_with_protection() -> DataDomeConfig {
        DataDomeConfig {
            enabled: true,
            enable_protection: true,
            ..DataDomeConfig::default()
        }
    }

    #[test]
    fn exclusion_rule_deserializes_documented_shape() {
        let rule: ProtectionExclusionRuleConfig = serde_json::from_value(serde_json::json!({
            "id": "legacy-static-get-head",
            "methods": ["GET", "HEAD"],
            "type": "path_regex",
            "patterns": ["(?i)\\.(css|js)$"]
        }))
        .expect("should deserialize documented rule shape");

        assert_eq!(rule.id, "legacy-static-get-head");
        assert_eq!(rule.methods, vec!["GET".to_string(), "HEAD".to_string()]);
        assert!(matches!(
            rule.matcher,
            ProtectionMatcherConfig::PathRegex { patterns } if patterns == vec!["(?i)\\.(css|js)$".to_string()]
        ));
    }

    #[test]
    fn cidr_matches_ipv4_and_ipv6_ranges() {
        let cidr = "192.0.2.0/24".parse::<IpCidr>().expect("should parse CIDR");
        assert!(cidr.contains(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10))));
        assert!(!cidr.contains(IpAddr::V4(Ipv4Addr::new(192, 0, 3, 10))));

        let cidr = "2001:db8::/32"
            .parse::<IpCidr>()
            .expect("should parse CIDR");
        assert!(cidr.contains(IpAddr::V6(
            "2001:db8::1"
                .parse::<Ipv6Addr>()
                .expect("should parse IPv6")
        )));
        assert!(!cidr.contains(IpAddr::V6(
            "2001:db9::1"
                .parse::<Ipv6Addr>()
                .expect("should parse IPv6")
        )));
    }

    #[test]
    fn scope_skips_configured_methods() {
        let mut config = config_with_protection();
        config.protection_excluded_methods = vec!["OPTIONS".to_string(), "FASTLYPURGE".to_string()];
        let scope = ProtectionScope::compile(&config).expect("should compile scope");
        let services = crate::platform::test_support::noop_services();

        let decision = scope.evaluate(&facts("FASTLYPURGE", "/page", None, None, None), &services);

        assert!(matches!(
            decision,
            ProtectionScopeDecision::Skip {
                reason: "method",
                ..
            }
        ));
    }

    #[test]
    fn scope_skips_configured_asns() {
        let mut config = config_with_protection();
        config.protection_excluded_asns = vec![19750, 209366];
        let scope = ProtectionScope::compile(&config).expect("should compile scope");
        let services = crate::platform::test_support::noop_services();

        let decision = scope.evaluate(&facts("GET", "/page", None, None, Some(19750)), &services);

        assert!(matches!(
            decision,
            ProtectionScopeDecision::Skip { reason: "asn", .. }
        ));
    }

    #[test]
    fn scope_skips_inline_ip_cidr_matches() {
        let mut config = config_with_protection();
        config.protection_excluded_ip_cidrs = vec!["198.51.100.0/24".to_string()];
        let scope = ProtectionScope::compile(&config).expect("should compile scope");
        let services = crate::platform::test_support::noop_services();

        let decision = scope.evaluate(
            &facts(
                "GET",
                "/page",
                None,
                Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 42))),
                None,
            ),
            &services,
        );

        assert!(matches!(
            decision,
            ProtectionScopeDecision::Skip {
                reason: "client_ip",
                ..
            }
        ));
    }

    #[test]
    fn scope_skips_config_store_ip_cidr_source_matches() {
        clear_ip_cidr_source_cache_for_tests();
        let mut config = config_with_protection();
        config.protection_excluded_ip_cidr_sources = vec![ProtectionIpCidrSourceConfig {
            config_store: "datadome_ip_bypass".to_string(),
            key: "googlebot_ips".to_string(),
        }];
        let scope = ProtectionScope::compile(&config).expect("should compile scope");
        let mut data = HashMap::new();
        data.insert("googlebot_ips".to_string(), "203.0.113.0/24".to_string());
        let services =
            build_services_with_config_and_secret(HashMapConfigStore::new(data), NoopSecretStore);

        let decision = scope.evaluate(
            &facts(
                "GET",
                "/page",
                None,
                Some(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10))),
                None,
            ),
            &services,
        );

        assert!(matches!(
            decision,
            ProtectionScopeDecision::Skip {
                reason: "client_ip_source",
                ..
            }
        ));
    }

    #[test]
    fn rule_path_regex_is_method_scoped() {
        let mut config = config_with_protection();
        config.protection_exclusion_rules = vec![ProtectionExclusionRuleConfig {
            id: "static-get-head".to_string(),
            enabled: true,
            methods: vec!["GET".to_string(), "HEAD".to_string()],
            matcher: ProtectionMatcherConfig::PathRegex {
                patterns: vec![r"(?i)\.(css|js|json)$".to_string()],
            },
        }];
        let scope = ProtectionScope::compile(&config).expect("should compile scope");
        let services = crate::platform::test_support::noop_services();

        assert!(matches!(
            scope.evaluate(&facts("GET", "/app.JSON", None, None, None), &services),
            ProtectionScopeDecision::Skip {
                reason: "path_regex",
                ..
            }
        ));
        assert!(matches!(
            scope.evaluate(&facts("POST", "/app.JSON", None, None, None), &services),
            ProtectionScopeDecision::Protect
        ));
    }

    #[test]
    fn rule_query_param_non_empty_matches_rsc() {
        let mut config = config_with_protection();
        config.protection_exclusion_rules = vec![ProtectionExclusionRuleConfig {
            id: "rsc".to_string(),
            enabled: true,
            methods: vec!["GET".to_string(), "HEAD".to_string()],
            matcher: ProtectionMatcherConfig::QueryParamNonEmpty {
                names: vec!["_rsc".to_string()],
            },
        }];
        let scope = ProtectionScope::compile(&config).expect("should compile scope");
        let services = crate::platform::test_support::noop_services();

        assert!(matches!(
            scope.evaluate(
                &facts("GET", "/page", Some("_rsc=abc&x=1"), None, None),
                &services
            ),
            ProtectionScopeDecision::Skip {
                reason: "query_param_non_empty",
                ..
            }
        ));
        assert!(matches!(
            scope.evaluate(
                &facts("GET", "/page", Some("_rsc=&x=1"), None, None),
                &services
            ),
            ProtectionScopeDecision::Protect
        ));
    }
}
