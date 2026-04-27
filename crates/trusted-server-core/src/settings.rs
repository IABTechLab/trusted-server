use config::{Config, Environment, File, FileFormat};
use error_stack::{Report, ResultExt};
use regex::Regex;
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::OnceLock;
use url::Url;
use validator::{Validate, ValidationError};

use crate::auction_config_types::AuctionConfig;
use crate::consent_config::ConsentConfig;
use crate::error::TrustedServerError;
use crate::redacted::Redacted;

pub const ENVIRONMENT_VARIABLE_PREFIX: &str = "TRUSTED_SERVER";
pub const ENVIRONMENT_VARIABLE_SEPARATOR: &str = "__";

#[derive(Debug, Default, Clone, Deserialize, Serialize, Validate)]
pub struct Publisher {
    #[validate(custom(function = validate_publisher_domain))]
    pub domain: String,
    /// Domain for non-EC cookies. EC cookies use a separate computed domain
    /// (see [`ec_cookie_domain`](Self::ec_cookie_domain)).
    #[validate(custom(function = validate_cookie_domain))]
    pub cookie_domain: String,
    #[validate(custom(function = validate_no_trailing_slash))]
    pub origin_url: String,
    /// Secret used to encrypt/decrypt proxied URLs in `/first-party/proxy`.
    /// Keep this secret stable to allow existing links to decode.
    #[validate(custom(function = validate_redacted_not_empty))]
    pub proxy_secret: Redacted<String>,
}

impl Publisher {
    /// Known placeholder values that must not be used in production.
    pub const PROXY_SECRET_PLACEHOLDERS: &[&str] = &["change-me-proxy-secret", "proxy-secret"];

    /// Returns the EC cookie domain, computed as `.{domain}`.
    ///
    /// Per spec §5.2, EC cookies derive their domain from
    /// `publisher.domain` — **not** from `publisher.cookie_domain`.
    /// This ensures the EC cookie is always scoped to the publisher's
    /// apex domain regardless of how `cookie_domain` is configured.
    #[must_use]
    pub fn ec_cookie_domain(&self) -> String {
        format!(".{}", self.domain)
    }

    /// Returns `true` if `proxy_secret` matches a known placeholder value
    /// (case-insensitive).
    #[must_use]
    pub fn is_placeholder_proxy_secret(proxy_secret: &str) -> bool {
        Self::PROXY_SECRET_PLACEHOLDERS
            .iter()
            .any(|p| p.eq_ignore_ascii_case(proxy_secret))
    }

    /// Extracts the host (including port if present) from the `origin_url`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use trusted_server_core::settings::Publisher;
    /// # use trusted_server_core::redacted::Redacted;
    /// let publisher = Publisher {
    ///     domain: "example.com".to_string(),
    ///     cookie_domain: ".example.com".to_string(),
    ///     origin_url: "https://origin.example.com:8080".to_string(),
    ///     proxy_secret: Redacted::new("proxy-secret".to_string()),
    /// };
    /// assert_eq!(publisher.origin_host(), "origin.example.com:8080");
    /// ```
    #[allow(dead_code)]
    #[must_use]
    pub fn origin_host(&self) -> String {
        Url::parse(&self.origin_url)
            .ok()
            .and_then(|url| {
                url.host_str().map(|host| match url.port() {
                    Some(port) => format!("{}:{}", host, port),
                    None => host.to_string(),
                })
            })
            .unwrap_or_else(|| self.origin_url.clone())
    }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct IntegrationSettings {
    #[serde(flatten)]
    entries: HashMap<String, JsonValue>,
}

pub trait IntegrationConfig: DeserializeOwned + Validate {
    fn is_enabled(&self) -> bool;
}

impl IntegrationSettings {
    /// Inserts a configuration value for an integration.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration cannot be serialized to JSON.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn insert_config<T>(
        &mut self,
        integration_id: impl Into<String>,
        value: &T,
    ) -> Result<(), Report<TrustedServerError>>
    where
        T: Serialize,
    {
        let json =
            serde_json::to_value(value).change_context(TrustedServerError::Configuration {
                message: "Failed to serialize integration configuration".to_string(),
            })?;
        self.entries.insert(integration_id.into(), json);
        Ok(())
    }

    fn normalize_env_value(value: JsonValue) -> JsonValue {
        match value {
            JsonValue::Object(map) => JsonValue::Object(
                map.into_iter()
                    .map(|(key, val)| (key, Self::normalize_env_value(val)))
                    .collect(),
            ),
            JsonValue::Array(items) => {
                JsonValue::Array(items.into_iter().map(Self::normalize_env_value).collect())
            }
            JsonValue::String(raw) => {
                if let Ok(parsed) = serde_json::from_str::<JsonValue>(&raw) {
                    parsed
                } else {
                    JsonValue::String(raw)
                }
            }
            other => other,
        }
    }

    /// Normalizes all entries in place, converting JSON-encoded strings from
    /// environment variables into their proper typed representations.
    /// Called eagerly after deserialization so that TOML serialization in
    /// build.rs preserves correct types.
    pub fn normalize(&mut self) {
        for value in self.entries.values_mut() {
            *value = Self::normalize_env_value(value.clone());
        }
    }

    fn is_explicitly_disabled(raw: &JsonValue) -> bool {
        raw.as_object()
            .and_then(|map| map.get("enabled"))
            .and_then(JsonValue::as_bool)
            == Some(false)
    }

    /// Retrieves and validates a typed configuration for an integration.
    ///
    /// # Errors
    ///
    /// Returns an error if the configuration cannot be parsed from JSON or fails validation.
    pub fn get_typed<T>(
        &self,
        integration_id: &str,
    ) -> Result<Option<T>, Report<TrustedServerError>>
    where
        T: IntegrationConfig,
    {
        let raw = match self.entries.get(integration_id) {
            Some(value) => value,
            None => return Ok(None),
        };

        if Self::is_explicitly_disabled(raw) {
            return Ok(None);
        }

        let config: T = serde_json::from_value(raw.clone()).change_context(
            TrustedServerError::Configuration {
                message: format!(
                    "Integration '{integration_id}' configuration could not be parsed"
                ),
            },
        )?;

        config.validate().map_err(|err| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "Integration '{integration_id}' configuration failed validation: {err}"
                ),
            })
        })?;

        if !config.is_enabled() {
            return Ok(None);
        }

        Ok(Some(config))
    }
}

impl Deref for IntegrationSettings {
    type Target = HashMap<String, JsonValue>;

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl DerefMut for IntegrationSettings {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.entries
    }
}

/// A partner (SSP, DSP, identity vendor) configured in `[[ec.partners]]`.
///
/// Partners are defined statically in `trusted-server.toml` rather than
/// registered via API. At startup, each partner's `api_token` is hashed
/// (SHA-256) for O(1) auth lookups; the plaintext is never stored at runtime.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct EcPartner {
    /// Unique partner identifier. Must match `^[a-z0-9_-]{1,32}$` and
    /// not collide with reserved IDs (`ec`, `ts`, `eids`, etc.).
    #[validate(custom(function = EcPartner::validate_id))]
    pub id: String,
    /// Human-readable partner name.
    pub name: String,
    /// `OpenRTB` `source.domain` for EID entries (e.g. `"liveramp.com"`).
    pub source_domain: String,
    /// `OpenRTB` `atype` value (typically 3).
    #[serde(default = "EcPartner::default_openrtb_atype")]
    pub openrtb_atype: u8,
    /// Whether this partner's UIDs appear in auction `user.eids`.
    #[serde(default)]
    pub bidstream_enabled: bool,
    /// Plaintext API token. Hashed at startup for auth lookups.
    /// Used by batch sync (inbound) and identify (inbound).
    pub api_token: Redacted<String>,
    /// Max batch sync API requests per partner per minute.
    #[serde(default = "EcPartner::default_batch_rate_limit")]
    pub batch_rate_limit: u32,
    /// Whether server-to-server pull sync is enabled for this partner.
    #[serde(default)]
    pub pull_sync_enabled: bool,
    /// URL to call for pull sync. Required when `pull_sync_enabled`.
    #[serde(default)]
    pub pull_sync_url: Option<String>,
    /// Allowlist of domains TS may call for this partner's pull sync.
    #[serde(default)]
    pub pull_sync_allowed_domains: Vec<String>,
    /// Seconds between pull sync refreshes.
    #[serde(default = "EcPartner::default_pull_sync_ttl_sec")]
    pub pull_sync_ttl_sec: u64,
    /// Max pull sync calls per EC hash per partner per hour.
    #[serde(default = "EcPartner::default_pull_sync_rate_limit")]
    pub pull_sync_rate_limit: u32,
    /// Outbound bearer token for pull sync requests.
    #[serde(default)]
    pub ts_pull_token: Option<Redacted<String>>,
}

impl EcPartner {
    const RESERVED_IDS: &[&str] = &[
        "ec",
        "eids",
        "ec-consent",
        "eids-truncated",
        "synthetic",
        "ts",
        "version",
        "env",
    ];

    /// Validates a partner ID for safe use in dynamic headers and cookies.
    ///
    /// # Errors
    ///
    /// Returns a validation error when `id` does not match the configured
    /// lowercase identifier policy or collides with a reserved name.
    pub fn validate_id(id: &str) -> Result<(), ValidationError> {
        if id.is_empty() || id.len() > 32 {
            return Err(ValidationError::new("invalid_partner_id_length"));
        }
        if Self::RESERVED_IDS.contains(&id) {
            return Err(ValidationError::new("reserved_partner_id"));
        }
        if !id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        }) {
            return Err(ValidationError::new("invalid_partner_id"));
        }
        Ok(())
    }

    #[must_use]
    pub const fn default_openrtb_atype() -> u8 {
        3
    }

    #[must_use]
    pub const fn default_batch_rate_limit() -> u32 {
        60
    }

    #[must_use]
    pub const fn default_pull_sync_ttl_sec() -> u64 {
        86400
    }

    #[must_use]
    pub const fn default_pull_sync_rate_limit() -> u32 {
        10
    }
}

/// Edge Cookie (EC) configuration.
///
/// Mapped from the `[ec]` TOML section. Controls EC identity generation,
/// KV store names, and partner registry.
#[derive(Debug, Default, Clone, Deserialize, Serialize, Validate)]
pub struct Ec {
    /// Publisher passphrase used as HMAC key for EC generation.
    #[validate(custom(function = Ec::validate_passphrase))]
    pub passphrase: Redacted<String>,

    /// Fastly KV store name for the EC identity graph.
    #[serde(default)]
    pub ec_store: Option<String>,

    /// Maximum number of concurrent pull-sync requests.
    #[serde(default = "Ec::default_pull_sync_concurrency")]
    pub pull_sync_concurrency: usize,

    /// Entries with `cluster_size` at or below this value are treated as
    /// individual users for identity resolution. B2B publishers should
    /// raise this to 50+ since readers are frequently on office networks.
    #[serde(default = "Ec::default_cluster_trust_threshold")]
    pub cluster_trust_threshold: u32,

    /// Seconds before re-evaluating an entry's `cluster_size`.
    /// Re-check occurs only in the `/_ts/api/v1/identify` endpoint.
    #[serde(default = "Ec::default_cluster_recheck_secs")]
    pub cluster_recheck_secs: u64,

    /// Partners (SSPs, DSPs, identity vendors) for EC identity sync.
    #[serde(default)]
    #[validate(nested)]
    pub partners: Vec<EcPartner>,
}

impl Ec {
    /// Known placeholder values that must not be used in production.
    pub const PASSPHRASE_PLACEHOLDERS: &[&str] = &["secret-key", "secret_key", "trusted-server"];

    /// Default maximum concurrent pull-sync requests.
    #[must_use]
    pub const fn default_pull_sync_concurrency() -> usize {
        3
    }

    /// Default cluster trust threshold.
    #[must_use]
    pub const fn default_cluster_trust_threshold() -> u32 {
        10
    }

    /// Default cluster re-check interval (1 hour).
    #[must_use]
    pub const fn default_cluster_recheck_secs() -> u64 {
        3600
    }

    /// Returns `true` if `passphrase` matches a known placeholder value
    /// (case-insensitive).
    #[must_use]
    pub fn is_placeholder_passphrase(passphrase: &str) -> bool {
        Self::PASSPHRASE_PLACEHOLDERS
            .iter()
            .any(|p| p.eq_ignore_ascii_case(passphrase))
    }

    /// Minimum passphrase length for HMAC key strength.
    ///
    /// This lower bound is only meant to reject obviously bad values; operators
    /// are still expected to use a high-entropy random passphrase per the EC
    /// setup and key-rotation documentation.
    const MIN_PASSPHRASE_LENGTH: usize = 8;

    /// Validates that the passphrase is not empty and meets minimum length.
    ///
    /// # Errors
    ///
    /// Returns a validation error if the passphrase is empty or shorter
    /// than [`Self::MIN_PASSPHRASE_LENGTH`] characters.
    pub fn validate_passphrase(passphrase: &Redacted<String>) -> Result<(), ValidationError> {
        if passphrase.expose().is_empty() {
            return Err(ValidationError::new("empty_passphrase"));
        }
        if passphrase.expose().len() < Self::MIN_PASSPHRASE_LENGTH {
            return Err(ValidationError::new("short_passphrase"));
        }
        Ok(())
    }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Validate)]
pub struct Rewrite {
    /// List of domains to exclude from rewriting. Supports wildcards (e.g., "*.example.com").
    /// URLs from these domains will not be proxied through first-party endpoints.
    #[serde(default)]
    pub exclude_domains: Vec<String>,
}

impl Rewrite {
    /// Checks if a URL should be excluded from rewriting based on domain matching
    #[allow(dead_code)]
    #[must_use]
    pub fn is_excluded(&self, url: &str) -> bool {
        // Parse URL to extract host
        let Ok(parsed) = url::Url::parse(url) else {
            return false;
        };

        let host = parsed.host_str().unwrap_or("");

        // Check exact domain matches (with wildcard support)
        for domain in &self.exclude_domains {
            if let Some(suffix) = domain.strip_prefix("*.") {
                // Wildcard: *.example.com matches both example.com and sub.example.com
                if host == suffix || host.ends_with(&format!(".{}", suffix)) {
                    return true;
                }
            } else if host == domain {
                return true;
            }
        }

        false
    }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Validate)]
pub struct Handler {
    #[validate(length(min = 1), custom(function = validate_path))]
    pub path: String,
    #[validate(custom(function = validate_redacted_not_empty))]
    pub username: Redacted<String>,
    #[validate(custom(function = validate_redacted_not_empty))]
    pub password: Redacted<String>,
    #[serde(skip, default)]
    #[validate(skip)]
    regex: OnceLock<Result<Regex, String>>,
}

impl Handler {
    fn compiled_regex(&self) -> Result<&Regex, Report<TrustedServerError>> {
        match self
            .regex
            .get_or_init(|| Regex::new(&self.path).map_err(|err| err.to_string()))
        {
            Ok(regex) => Ok(regex),
            Err(message) => Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "Handler path regex `{}` failed to compile: {message}",
                    self.path
                ),
            })),
        }
    }

    /// Eagerly compile the handler regex to fail fast during startup.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if the handler path regex does not compile.
    pub fn prepare_runtime(&self) -> Result<(), Report<TrustedServerError>> {
        self.compiled_regex().map(|_| ())
    }

    /// Determine whether this handler applies to the request path.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if the handler path regex does not compile.
    pub fn matches_path(&self, path: &str) -> Result<bool, Report<TrustedServerError>> {
        self.compiled_regex().map(|regex| regex.is_match(path))
    }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct RequestSigning {
    #[serde(default = "default_request_signing_enabled")]
    pub enabled: bool,
    pub config_store_id: String,
    pub secret_store_id: String,
}

fn default_request_signing_enabled() -> bool {
    false
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Proxy {
    /// Enable TLS certificate verification when proxying to HTTPS origins.
    /// Defaults to true for secure production use.
    /// Set to false for local development with self-signed certificates.
    #[serde(default = "default_certificate_check")]
    pub certificate_check: bool,
    /// Permitted redirect target domains for the first-party proxy.
    ///
    /// Supports exact hostname match (`"example.com"`) and subdomain wildcard
    /// prefix (`"*.example.com"`, which also matches the apex `example.com`).
    /// Matching is case-insensitive.
    ///
    /// When empty (the default), redirect destinations are not restricted.
    /// Configure this in production to prevent SSRF via redirect chains
    /// initiated by signed first-party proxy URLs.
    #[serde(default, deserialize_with = "vec_from_seq_or_map")]
    pub allowed_domains: Vec<String>,
}

fn default_certificate_check() -> bool {
    true
}

impl Default for Proxy {
    fn default() -> Self {
        Self {
            certificate_check: default_certificate_check(),
            allowed_domains: Vec::new(),
        }
    }
}

impl Proxy {
    /// Normalizes `allowed_domains` in place.
    ///
    /// Each entry is trimmed of surrounding whitespace and lowercased.
    /// Empty entries (including those that were only whitespace) are removed.
    /// A bare `"*"` entry is removed with a warning: it is not a valid pattern
    /// (it never matches any real host) and is likely a mistake. Users who want
    /// open mode should omit `allowed_domains` entirely or leave it empty.
    fn normalize(&mut self) {
        self.allowed_domains = self
            .allowed_domains
            .iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();

        let before = self.allowed_domains.len();
        self.allowed_domains.retain(|s| s != "*");
        if self.allowed_domains.len() < before {
            log::warn!(
                "proxy.allowed_domains: bare \"*\" is not a valid pattern and has been removed; \
                 omit allowed_domains or leave it empty for open mode"
            );
        }

        if self.allowed_domains.is_empty() {
            log::info!(
                "proxy.allowed_domains is empty: all redirect destinations are permitted (open mode)"
            );
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Validate)]
pub struct Settings {
    #[validate(nested)]
    pub publisher: Publisher,
    #[serde(default)]
    #[validate(nested)]
    pub ec: Ec,
    #[serde(default)]
    pub integrations: IntegrationSettings,
    #[serde(default, deserialize_with = "vec_from_seq_or_map")]
    #[validate(nested)]
    pub handlers: Vec<Handler>,
    #[serde(default, deserialize_with = "map_from_obj_or_str")]
    pub response_headers: HashMap<String, String>,
    pub request_signing: Option<RequestSigning>,
    #[serde(default)]
    #[validate(nested)]
    pub rewrite: Rewrite,
    #[serde(default)]
    pub auction: AuctionConfig,
    #[serde(default)]
    pub consent: ConsentConfig,
    #[serde(default)]
    pub proxy: Proxy,
}

impl Settings {
    /// Creates a new [`Settings`] instance from a pre-built TOML string.
    ///
    /// Use this for the runtime path where the TOML has already been
    /// fully resolved (env vars baked in by build.rs).
    ///
    /// # Errors
    ///
    /// - [`TrustedServerError::Configuration`] if the TOML is invalid or missing required fields
    pub fn from_toml(toml_str: &str) -> Result<Self, Report<TrustedServerError>> {
        let mut settings: Self =
            toml::from_str(toml_str).change_context(TrustedServerError::Configuration {
                message: "Failed to deserialize TOML configuration".to_string(),
            })?;

        settings.proxy.normalize();
        settings.consent.validate();
        settings.prepare_runtime()?;

        settings.validate().map_err(|err| {
            Report::new(TrustedServerError::Configuration {
                message: format!("Configuration validation failed: {err}"),
            })
        })?;

        settings.validate_admin_coverage()?;

        Ok(settings)
    }

    /// Creates a new [`Settings`] instance from a TOML string, applying
    /// environment variable overrides using the `TRUSTED_SERVER__` prefix.
    ///
    /// Used by build.rs to merge the base config with env vars before
    /// baking the result into the binary.
    ///
    /// # Errors
    ///
    /// - [`TrustedServerError::Configuration`] if the TOML is invalid or missing required fields
    pub fn from_toml_and_env(toml_str: &str) -> Result<Self, Report<TrustedServerError>> {
        let environment = Environment::default()
            .prefix(ENVIRONMENT_VARIABLE_PREFIX)
            .separator(ENVIRONMENT_VARIABLE_SEPARATOR);

        let toml = File::from_str(toml_str, FileFormat::Toml);
        let config = Config::builder()
            .add_source(toml)
            .add_source(environment)
            .build()
            .change_context(TrustedServerError::Configuration {
                message: "Failed to build configuration".to_string(),
            })?;
        let mut settings: Self =
            config
                .try_deserialize()
                .change_context(TrustedServerError::Configuration {
                    message: "Failed to deserialize configuration".to_string(),
                })?;

        settings.integrations.normalize();
        settings.proxy.normalize();
        settings.consent.validate();

        settings.validate().map_err(|err| {
            Report::new(TrustedServerError::Configuration {
                message: format!("Build-time configuration validation failed: {err}"),
            })
        })?;

        settings.prepare_runtime()?;
        settings.validate_admin_coverage()?;

        Ok(settings)
    }

    /// Eagerly prepare runtime-only settings artifacts.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if any handler path regex does not compile.
    pub fn prepare_runtime(&self) -> Result<(), Report<TrustedServerError>> {
        for handler in &self.handlers {
            handler.prepare_runtime()?;
        }

        Ok(())
    }

    /// Rejects known placeholder secret values.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::InsecureDefault`] when one or more secret
    /// fields still contain a placeholder value.
    pub fn reject_placeholder_secrets(&self) -> Result<(), Report<TrustedServerError>> {
        let mut insecure_fields: Vec<&str> = Vec::new();

        if Ec::is_placeholder_passphrase(self.ec.passphrase.expose()) {
            insecure_fields.push("ec.passphrase");
        }
        if Publisher::is_placeholder_proxy_secret(self.publisher.proxy_secret.expose()) {
            insecure_fields.push("publisher.proxy_secret");
        }

        if insecure_fields.is_empty() {
            return Ok(());
        }

        Err(Report::new(TrustedServerError::InsecureDefault {
            field: insecure_fields.join(", "),
        }))
    }

    /// Resolve the first handler whose regex matches the request path.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if any handler regex does not compile.
    pub fn handler_for_path(
        &self,
        path: &str,
    ) -> Result<Option<&Handler>, Report<TrustedServerError>> {
        for handler in &self.handlers {
            if handler.matches_path(path)? {
                return Ok(Some(handler));
            }
        }

        Ok(None)
    }

    /// Known admin endpoint paths that must be covered by a handler.
    ///
    /// [`from_toml_and_env`](Self::from_toml_and_env) rejects configurations
    /// where any of these paths lack a matching handler, ensuring admin
    /// endpoints are always protected by authentication.
    /// Update [`ADMIN_ENDPOINTS`](Self::ADMIN_ENDPOINTS) when adding new
    /// admin routes to `crates/trusted-server-adapter-fastly/src/main.rs`.
    pub(crate) const ADMIN_ENDPOINTS: &[&str] =
        &["/_ts/admin/keys/rotate", "/_ts/admin/keys/deactivate"];

    /// Returns admin endpoint paths that no configured handler covers.
    ///
    /// Called by [`from_toml_and_env`](Self::from_toml_and_env) at build time
    /// to enforce that every admin endpoint has a handler. An empty return
    /// value means all admin endpoints are properly covered.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] if any handler has an invalid path regex.
    pub(crate) fn uncovered_admin_endpoints(
        &self,
    ) -> Result<Vec<&'static str>, Report<TrustedServerError>> {
        let mut uncovered = Vec::new();
        for &path in Self::ADMIN_ENDPOINTS {
            let mut covered = false;
            for h in &self.handlers {
                if h.matches_path(path)? {
                    covered = true;
                    break;
                }
            }
            if !covered {
                uncovered.push(path);
            }
        }
        Ok(uncovered)
    }

    /// Validates that every admin endpoint is covered by at least one handler.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] listing any uncovered
    /// admin endpoints.
    fn validate_admin_coverage(&self) -> Result<(), Report<TrustedServerError>> {
        let uncovered = self.uncovered_admin_endpoints()?;
        if uncovered.is_empty() {
            return Ok(());
        }
        Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "No handler covers admin endpoint(s): {}. \
                 Add a [[handlers]] entry with a path regex matching /_ts/admin/ \
                 to protect admin access.",
                uncovered.join(", ")
            ),
        }))
    }

    /// Retrieves the integration configuration of a specific type.
    ///
    /// # Errors
    ///
    /// Returns an error if the integration configuration exists but cannot be deserialized as the requested type.
    pub fn integration_config<T>(
        &self,
        integration_id: &str,
    ) -> Result<Option<T>, Report<TrustedServerError>>
    where
        T: IntegrationConfig,
    {
        self.integrations.get_typed(integration_id)
    }
}

fn validate_publisher_domain(value: &str) -> Result<(), ValidationError> {
    if value.trim() != value || value.is_empty() || value.len() > 253 {
        return Err(ValidationError::new("invalid_publisher_domain"));
    }
    if value.starts_with('.') || value.ends_with('.') || value.contains(['/', ':']) {
        return Err(ValidationError::new("invalid_publisher_domain"));
    }

    for label in value.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(ValidationError::new("invalid_publisher_domain"));
        }
        let bytes = label.as_bytes();
        if bytes.first() == Some(&b'-') || bytes.last() == Some(&b'-') {
            return Err(ValidationError::new("invalid_publisher_domain"));
        }
        if !bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
        {
            return Err(ValidationError::new("invalid_publisher_domain"));
        }
    }

    Ok(())
}

fn validate_cookie_domain(value: &str) -> Result<(), ValidationError> {
    // `=` is excluded: it only has special meaning in the name=value pair,
    // not within the Domain attribute value.
    if value.contains([';', '\n', '\r']) {
        let mut err = ValidationError::new("cookie_metacharacters");
        err.message =
            Some("cookie_domain must not contain cookie metacharacters (;, \\n, \\r)".into());
        return Err(err);
    }
    Ok(())
}

fn validate_no_trailing_slash(value: &str) -> Result<(), ValidationError> {
    if value.ends_with('/') {
        let mut err = ValidationError::new("trailing_slash");
        err.add_param("value".into(), &value);
        err.message = Some("origin_url must not end with '/'".into());
        return Err(err);
    }
    Ok(())
}

fn validate_redacted_not_empty(value: &Redacted<String>) -> Result<(), ValidationError> {
    if value.expose().is_empty() {
        return Err(ValidationError::new("empty_value"));
    }
    Ok(())
}

fn validate_path(value: &str) -> Result<(), ValidationError> {
    Regex::new(value).map(|_| ()).map_err(|err| {
        let mut validation_error = ValidationError::new("invalid_regex");
        validation_error.add_param("value".into(), &value);
        validation_error.add_param("message".into(), &err.to_string());
        validation_error
    })
}
// Helper: allow Vec fields to deserialize from either a JSON array or a map of numeric indices.
// This lets env vars like TRUSTED_SERVER__INTEGRATIONS__PREBID__BIDDERS__0=smartadserver work, which the config env source
// represents as an object {"0": "value"} rather than a sequence. Also supports string inputs that are
// JSON arrays or comma-separated values.
/// Deserializes a `HashMap<String, String>` from either:
/// - A TOML table / JSON object (standard deserialization)
/// - A JSON string (e.g. from env var: `'{"Key": "value"}'`)
///
/// This allows setting map fields via environment variables while
/// preserving key casing and special characters like hyphens.
pub(crate) fn map_from_obj_or_str<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    let v = JsonValue::deserialize(deserializer)?;
    match v {
        JsonValue::Object(map) => map
            .into_iter()
            .map(|(k, v)| {
                let val = match v {
                    JsonValue::String(s) => s,
                    other => other.to_string(),
                };
                Ok((k, val))
            })
            .collect(),
        JsonValue::String(s) => {
            let txt = s.trim();
            if txt.starts_with('{') {
                serde_json::from_str::<HashMap<String, String>>(txt)
                    .map_err(serde::de::Error::custom)
            } else {
                Err(serde::de::Error::custom(
                    "expected JSON object string, e.g. '{\"Key\": \"value\"}'",
                ))
            }
        }
        JsonValue::Null => Ok(HashMap::new()),
        other => Err(serde::de::Error::custom(format!(
            "expected object or JSON string, got {other}",
        ))),
    }
}

pub(crate) fn vec_from_seq_or_map<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let v = JsonValue::deserialize(deserializer)?;
    match v {
        JsonValue::Array(arr) => arr
            .into_iter()
            .map(|item| serde_json::from_value(item).map_err(serde::de::Error::custom))
            .collect(),
        JsonValue::Object(map) => {
            let mut items: Vec<(usize, T)> = Vec::with_capacity(map.len());
            for (k, val) in map.into_iter() {
                let idx = k.parse::<usize>().map_err(|_| {
                    serde::de::Error::custom(format!("Invalid index '{}' in map for Vec field", k))
                })?;
                let parsed: T = serde_json::from_value(val).map_err(serde::de::Error::custom)?;
                items.push((idx, parsed));
            }
            items.sort_by_key(|(idx, _)| *idx);
            Ok(items.into_iter().map(|(_, v)| v).collect())
        }
        JsonValue::String(s) => {
            let txt = s.trim();
            if txt.starts_with('[') && txt.ends_with(']') {
                if let Ok(vec) = serde_json::from_str::<Vec<T>>(txt) {
                    return Ok(vec);
                }
                // Not valid JSON array — strip brackets and split on commas
                let inner = txt[1..txt.len() - 1].trim();
                let parts: Vec<&str> = inner
                    .split(',')
                    .map(str::trim)
                    .filter(|p| !p.is_empty())
                    .collect();
                let mut out: Vec<T> = Vec::with_capacity(parts.len());
                for p in parts {
                    let json = format!("\"{}\"", p.replace('"', "\\\""));
                    let parsed: T =
                        serde_json::from_str(&json).map_err(serde::de::Error::custom)?;
                    out.push(parsed);
                }
                Ok(out)
            } else {
                let parts = if txt.contains(',') {
                    txt.split(',')
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .collect::<Vec<_>>()
                } else {
                    vec![txt]
                };
                let mut out: Vec<T> = Vec::with_capacity(parts.len());
                for p in parts {
                    let json = format!("\"{}\"", p.replace('"', "\\\""));
                    let parsed: T =
                        serde_json::from_str(&json).map_err(serde::de::Error::custom)?;
                    out.push(parsed);
                }
                Ok(out)
            }
        }
        other => Err(serde::de::Error::custom(format!(
            "expected array, map of indices, or parseable string, got {}",
            other
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use serde_json::json;
    use std::collections::HashSet;

    use crate::auction::build_orchestrator;
    use crate::integrations::{
        gpt::GptConfig, nextjs::NextJsIntegrationConfig, prebid::PrebidIntegrationConfig,
        testlight::TestlightConfig, IntegrationRegistry,
    };
    use crate::redacted::Redacted;
    use crate::test_support::tests::{crate_test_settings_str, create_test_settings};

    #[test]
    fn test_settings_from_valid_toml() {
        let toml_str = crate_test_settings_str();
        let settings = Settings::from_toml(&toml_str);

        assert!(settings.is_ok());

        let settings = settings.expect("should parse valid TOML");
        let prebid_cfg = settings
            .integration_config::<PrebidIntegrationConfig>("prebid")
            .expect("Prebid config query should succeed")
            .expect("Prebid config should load from test settings");
        assert_eq!(
            prebid_cfg.server_url,
            "https://test-prebid.com/openrtb2/auction"
        );
        assert!(
            settings
                .integration_config::<NextJsIntegrationConfig>("nextjs")
                .expect("Next.js config query should succeed")
                .is_none(),
            "Next.js integration should default to disabled"
        );
        let raw_nextjs = settings
            .integrations
            .get("nextjs")
            .expect("test settings should include nextjs block");
        assert_eq!(raw_nextjs["enabled"], json!(false));
        assert_eq!(
            raw_nextjs["rewrite_attributes"],
            json!(["href", "link", "url"]),
            "Next.js rewrite attributes should default to href/link/url"
        );
        assert_eq!(settings.publisher.domain, "test-publisher.com");
        assert_eq!(settings.publisher.cookie_domain, ".test-publisher.com");
        assert_eq!(
            settings.publisher.ec_cookie_domain(),
            ".test-publisher.com",
            "EC cookie domain should be computed as .{{domain}}"
        );
        assert_eq!(
            settings.publisher.origin_url,
            "https://origin.test-publisher.com"
        );
        assert_eq!(settings.ec.passphrase.expose(), "test-secret-key");

        settings.validate().expect("Failed to validate settings");
    }

    #[test]
    fn validate_rejects_trailing_slash_in_origin_url() {
        let toml_str = crate_test_settings_str().replace(
            r#"origin_url = "https://origin.test-publisher.com""#,
            r#"origin_url = "https://origin.test-publisher.com/""#,
        );

        let result = Settings::from_toml(&toml_str);
        assert!(
            result.is_err(),
            "origin_url ending with '/' should fail validation"
        );
    }

    #[test]
    fn validate_rejects_invalid_publisher_domains() {
        for domain in [
            "",
            ".example.com",
            "example.com.",
            "https://example.com",
            "bad_domain.com",
        ] {
            let toml_str = crate_test_settings_str().replace(
                r#"domain = "test-publisher.com""#,
                &format!(r#"domain = "{domain}""#),
            );

            let result = Settings::from_toml(&toml_str);
            assert!(result.is_err(), "should reject invalid domain {domain:?}");
        }
    }

    #[test]
    fn validate_accepts_localhost_publisher_domain() {
        let toml_str = crate_test_settings_str().replace(
            r#"domain = "test-publisher.com""#,
            r#"domain = "localhost""#,
        );

        let settings = Settings::from_toml(&toml_str).expect("should accept localhost domain");
        assert_eq!(settings.publisher.ec_cookie_domain(), ".localhost");
    }

    #[test]
    fn validate_rejects_invalid_ec_partner_ids() {
        for partner_id in [
            "Upper",
            "bad id",
            "ec",
            "",
            "abcdefghijklmnopqrstuvwxyzabcdefg",
        ] {
            let toml_str = format!(
                r#"{}
                [[ec.partners]]
                id = "{}"
                name = "Invalid Partner"
                source_domain = "invalid.example.com"
                api_token = "invalid-token"
                "#,
                crate_test_settings_str(),
                partner_id,
            );

            let result = Settings::from_toml(&toml_str);
            assert!(
                result.is_err(),
                "should reject invalid partner ID {partner_id:?}"
            );
        }
    }

    #[test]
    fn prepare_runtime_rejects_invalid_handler_regex() {
        let toml_str = crate_test_settings_str().replace(r#"path = "^/secure""#, r#"path = "(""#);

        let err = Settings::from_toml(&toml_str).expect_err("should reject invalid handler regex");
        assert!(
            err.to_string()
                .contains("Handler path regex `(` failed to compile"),
            "should describe the invalid handler regex"
        );
    }

    #[test]
    fn test_settings_missing_required_fields() {
        let re = Regex::new(r"origin_url = .*").expect("regex should compile");

        let toml_str = crate_test_settings_str();
        let toml_str = re.replace(&toml_str, "");

        let settings = Settings::from_toml(&toml_str);
        assert!(
            settings.is_err(),
            "Should fail when required fields are missing"
        );
    }

    #[test]
    fn is_placeholder_passphrase_rejects_all_known_placeholders() {
        for placeholder in Ec::PASSPHRASE_PLACEHOLDERS {
            assert!(
                Ec::is_placeholder_passphrase(placeholder),
                "should detect placeholder passphrase '{placeholder}'"
            );
        }
    }

    #[test]
    fn is_placeholder_passphrase_is_case_insensitive() {
        assert!(
            Ec::is_placeholder_passphrase("SECRET-KEY"),
            "should detect case-insensitive placeholder passphrase"
        );
        assert!(
            Ec::is_placeholder_passphrase("Trusted-Server"),
            "should detect mixed-case placeholder passphrase"
        );
    }

    #[test]
    fn is_placeholder_passphrase_accepts_non_placeholder() {
        assert!(
            !Ec::is_placeholder_passphrase("test-secret-key"),
            "should accept non-placeholder passphrase"
        );
    }

    #[test]
    fn is_placeholder_proxy_secret_rejects_all_known_placeholders() {
        for placeholder in Publisher::PROXY_SECRET_PLACEHOLDERS {
            assert!(
                Publisher::is_placeholder_proxy_secret(placeholder),
                "should detect placeholder proxy_secret '{placeholder}'"
            );
        }
    }

    #[test]
    fn is_placeholder_proxy_secret_is_case_insensitive() {
        assert!(
            Publisher::is_placeholder_proxy_secret("CHANGE-ME-PROXY-SECRET"),
            "should detect case-insensitive placeholder proxy_secret"
        );
    }

    #[test]
    fn is_placeholder_proxy_secret_accepts_non_placeholder() {
        assert!(
            !Publisher::is_placeholder_proxy_secret("unit-test-proxy-secret"),
            "should accept non-placeholder proxy_secret"
        );
    }

    #[test]
    fn test_settings_empty_toml() {
        let toml_str = "";
        let settings = Settings::from_toml(toml_str);

        assert!(settings.is_err(), "Should fail with empty TOML");
    }

    #[test]
    fn test_settings_invalid_toml_syntax() {
        let re = Regex::new(r"\]").expect("regex should compile");
        let toml_str = crate_test_settings_str();
        let toml_str = re.replace(&toml_str, "");

        let settings = Settings::from_toml(&toml_str);
        assert!(settings.is_err(), "Should fail with invalid TOML syntax");
    }

    #[test]
    fn test_settings_partial_config() {
        let re = Regex::new(r"\[publisher\]").expect("regex should compile");
        let toml_str = crate_test_settings_str();
        let toml_str = re.replace(&toml_str, "");

        let settings = Settings::from_toml(&toml_str);
        assert!(settings.is_err(), "Should fail when sections are missing");
    }

    #[test]
    fn test_prebid_bidders_override_with_json_env() {
        let toml_str = crate_test_settings_str();
        let env_key = format!(
            "{}{}INTEGRATIONS{}PREBID{}BIDDERS",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

        // Ensure no external override interferes
        let origin_key = format!(
            "{}{}PUBLISHER{}ORIGIN_URL",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        temp_env::with_var(
            origin_key,
            Some("https://origin.test-publisher.com"),
            || {
                temp_env::with_var(env_key, Some("[\"smartadserver\",\"rubicon\"]"), || {
                    let res = Settings::from_toml_and_env(&toml_str);
                    if res.is_err() {
                        eprintln!("JSON override error: {:?}", res.as_ref().err());
                    }
                    let settings = res.expect("Settings should parse with JSON env override");
                    let cfg = settings
                        .integration_config::<PrebidIntegrationConfig>("prebid")
                        .expect("Prebid config query should succeed")
                        .expect("Prebid config should exist with env override");
                    assert_eq!(
                        cfg.bidders,
                        vec!["smartadserver".to_string(), "rubicon".to_string()]
                    );
                });
            },
        );
    }

    #[test]
    fn test_prebid_bidders_override_with_indexed_env() {
        let toml_str = crate_test_settings_str();

        let env_key0 = format!(
            "{}{}INTEGRATIONS{}PREBID{}BIDDERS{}0",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let env_key1 = format!(
            "{}{}INTEGRATIONS{}PREBID{}BIDDERS{}1",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

        // Also ensure origin_url env is a plain string (avoid any external env interference)
        let origin_key = format!(
            "{}{}PUBLISHER{}ORIGIN_URL",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        temp_env::with_var(
            origin_key,
            Some("https://origin.test-publisher.com"),
            || {
                temp_env::with_var(env_key0, Some("smartadserver"), || {
                    temp_env::with_var(env_key1, Some("openx"), || {
                        let res = Settings::from_toml_and_env(&toml_str);
                        if res.is_err() {
                            eprintln!("Indexed override error: {:?}", res.as_ref().err());
                        }
                        let settings =
                            res.expect("Settings should parse with indexed env override");
                        let cfg = settings
                            .integration_config::<PrebidIntegrationConfig>("prebid")
                            .expect("Prebid config query should succeed")
                            .expect("Prebid config should exist with indexed env override");
                        assert_eq!(
                            cfg.bidders,
                            vec!["smartadserver".to_string(), "openx".to_string()]
                        );
                    });
                });
            },
        );
    }

    #[test]
    fn test_handlers_override_with_env() {
        let toml_str = crate_test_settings_str();

        let origin_key = format!(
            "{}{}PUBLISHER{}ORIGIN_URL",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        // Override handler 0 via env vars
        let path_key_0 = format!(
            "{}{}HANDLERS{}0{}PATH",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let username_key_0 = format!(
            "{}{}HANDLERS{}0{}USERNAME",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let password_key_0 = format!(
            "{}{}HANDLERS{}0{}PASSWORD",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        // Admin handler at index 1 (required for admin endpoint coverage)
        let path_key_1 = format!(
            "{}{}HANDLERS{}1{}PATH",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let username_key_1 = format!(
            "{}{}HANDLERS{}1{}USERNAME",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let password_key_1 = format!(
            "{}{}HANDLERS{}1{}PASSWORD",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

        temp_env::with_vars(
            [
                (origin_key, Some("https://origin.test-publisher.com")),
                (path_key_0, Some("^/env-handler")),
                (username_key_0, Some("env-user")),
                (password_key_0, Some("env-pass")),
                (path_key_1, Some("^/_ts/admin")),
                (username_key_1, Some("admin")),
                (password_key_1, Some("admin-pass")),
            ],
            || {
                let settings =
                    Settings::from_toml_and_env(&toml_str).expect("Settings should load from env");
                assert_eq!(settings.handlers.len(), 2);
                let handler = &settings.handlers[0];
                assert_eq!(handler.path, "^/env-handler");
                assert_eq!(handler.username.expose(), "env-user");
                assert_eq!(handler.password.expose(), "env-pass");
            },
        );
    }

    #[test]
    fn test_invalid_handler_override_fails_during_runtime_preparation() {
        let toml_str = crate_test_settings_str();

        let origin_key = format!(
            "{}{}PUBLISHER{}ORIGIN_URL",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let path_key = format!(
            "{}{}HANDLERS{}0{}PATH",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

        temp_env::with_var(
            origin_key,
            Some("https://origin.test-publisher.com"),
            || {
                temp_env::with_var(path_key, Some("("), || {
                    let _ = Settings::from_toml_and_env(&toml_str)
                        .expect_err("should reject invalid handler regex override");
                });
            },
        );
    }

    #[test]
    fn test_response_headers_override_with_json_env() {
        let toml_str = crate_test_settings_str();
        let env_key = format!(
            "{}{}RESPONSE_HEADERS",
            ENVIRONMENT_VARIABLE_PREFIX, ENVIRONMENT_VARIABLE_SEPARATOR,
        );

        temp_env::with_var(
            env_key,
            Some(r#"{"X-Robots-Tag": "noindex", "X-Custom-Header": "custom value"}"#),
            || {
                let settings = Settings::from_toml_and_env(&toml_str)
                    .expect("Settings should parse with JSON response_headers env");
                assert_eq!(settings.response_headers.len(), 2);
                assert_eq!(
                    settings.response_headers.get("X-Robots-Tag"),
                    Some(&"noindex".to_string())
                );
                assert_eq!(
                    settings.response_headers.get("X-Custom-Header"),
                    Some(&"custom value".to_string())
                );
            },
        );
    }

    #[test]
    fn test_settings_extra_fields() {
        let toml_str = crate_test_settings_str() + "\nhello = 1";

        let settings = Settings::from_toml(&toml_str);
        assert!(settings.is_ok(), "Extra fields should be ignored");
    }

    #[test]
    fn test_set_env() {
        temp_env::with_var(
            format!(
                "{}{}PUBLISHER{}ORIGIN_URL",
                ENVIRONMENT_VARIABLE_PREFIX,
                ENVIRONMENT_VARIABLE_SEPARATOR,
                ENVIRONMENT_VARIABLE_SEPARATOR
            ),
            Some("https://change-publisher.com"),
            || {
                let settings = Settings::from_toml_and_env(&crate_test_settings_str());

                assert!(settings.is_ok(), "Settings should load from embedded TOML");
                assert_eq!(
                    settings.expect("should load settings").publisher.origin_url,
                    "https://change-publisher.com"
                );
            },
        );
    }

    #[test]
    fn test_override_env() {
        let toml_str = crate_test_settings_str();

        temp_env::with_var(
            format!(
                "{}{}PUBLISHER{}ORIGIN_URL",
                ENVIRONMENT_VARIABLE_PREFIX,
                ENVIRONMENT_VARIABLE_SEPARATOR,
                ENVIRONMENT_VARIABLE_SEPARATOR
            ),
            Some("https://change-publisher.com"),
            || {
                let settings = Settings::from_toml_and_env(&toml_str);

                assert!(settings.is_ok(), "Settings should load from embedded TOML");
                assert_eq!(
                    settings.expect("should load settings").publisher.origin_url,
                    "https://change-publisher.com"
                );
            },
        );
    }

    #[test]
    fn test_publisher_origin_host() {
        // Test with full URL including port
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "https://origin.example.com:8080".to_string(),
            proxy_secret: Redacted::new("test-secret".to_string()),
        };
        assert_eq!(publisher.origin_host(), "origin.example.com:8080");

        // Test with URL without port (default HTTPS port)
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            proxy_secret: Redacted::new("test-secret".to_string()),
        };
        assert_eq!(publisher.origin_host(), "origin.example.com");

        // Test with HTTP URL with explicit port
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "http://localhost:9090".to_string(),
            proxy_secret: Redacted::new("test-secret".to_string()),
        };
        assert_eq!(publisher.origin_host(), "localhost:9090");

        // Test with URL without protocol (fallback to original)
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "localhost:9090".to_string(),
            proxy_secret: Redacted::new("test-secret".to_string()),
        };
        assert_eq!(publisher.origin_host(), "localhost:9090");

        // Test with IPv4 address
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "http://192.168.1.1:8080".to_string(),
            proxy_secret: Redacted::new("test-secret".to_string()),
        };
        assert_eq!(publisher.origin_host(), "192.168.1.1:8080");

        // Test with IPv6 address
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "http://[::1]:8080".to_string(),
            proxy_secret: Redacted::new("test-secret".to_string()),
        };
        assert_eq!(publisher.origin_host(), "[::1]:8080");
    }

    #[test]
    fn test_integration_settings_from_env() {
        use crate::integrations::testlight::TestlightConfig;

        let toml_str = crate_test_settings_str();

        let origin_key = format!(
            "{}{}PUBLISHER{}ORIGIN_URL",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

        let integration_prefix = format!(
            "{}{}INTEGRATIONS{}TESTLIGHT{}",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

        let endpoint_key = format!("{}ENDPOINT", integration_prefix);
        let timeout_key = format!("{}TIMEOUT_MS", integration_prefix);
        let rewrite_key = format!("{}REWRITE_SCRIPTS", integration_prefix);
        let enabled_key = format!("{}ENABLED", integration_prefix);

        temp_env::with_var(
            origin_key,
            Some("https://origin.test-publisher.com"),
            || {
                temp_env::with_var(
                    endpoint_key,
                    Some("https://testlight-env.test/auction"),
                    || {
                        temp_env::with_var(timeout_key, Some("2500"), || {
                            temp_env::with_var(rewrite_key, Some("true"), || {
                                temp_env::with_var(enabled_key, Some("true"), || {
                                    let settings = Settings::from_toml_and_env(&toml_str)
                                        .expect("Settings should load");

                                    let config = settings
                                        .integration_config::<TestlightConfig>("testlight")
                                        .expect("integration parsing should succeed")
                                        .expect("integration should be enabled");

                                    assert_eq!(
                                        config.endpoint,
                                        "https://testlight-env.test/auction"
                                    );
                                    assert_eq!(config.timeout_ms, 2500);
                                    assert!(config.rewrite_scripts);
                                    assert!(config.enabled);
                                });
                            });
                        });
                    },
                );
            },
        );
    }

    #[test]
    fn test_disabled_integration_does_not_register() {
        use crate::integrations::testlight::TestlightConfig;
        use serde_json::json;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "testlight",
                &json!({
                    "enabled": false,
                    "endpoint": "https://testlight.test/auction",
                    "rewrite_scripts": true,
                }),
            )
            .expect("should insert integration config");

        let config = settings
            .integration_config::<TestlightConfig>("testlight")
            .expect("integration parsing should succeed");

        assert!(config.is_none(), "Disabled integrations should be skipped");
    }

    #[test]
    fn disabled_invalid_integration_skips_validation() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "gpt",
                &json!({
                    "enabled": false,
                    "script_url": "not a url",
                }),
            )
            .expect("should insert GPT config");

        let config = settings
            .integration_config::<GptConfig>("gpt")
            .expect("disabled GPT config should be ignored");
        assert!(config.is_none(), "disabled GPT config should be skipped");
        IntegrationRegistry::new(&settings)
            .expect("disabled invalid integration config should not fail registry startup");
    }

    #[test]
    fn disabled_invalid_default_enabled_prebid_skips_validation() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "prebid",
                &json!({
                    "enabled": false,
                    "server_url": "not a url",
                }),
            )
            .expect("should insert prebid config");

        let config = settings
            .integration_config::<PrebidIntegrationConfig>("prebid")
            .expect("disabled prebid config should be ignored");
        assert!(config.is_none(), "disabled prebid config should be skipped");
        IntegrationRegistry::new(&settings)
            .expect("disabled default-enabled prebid config should not fail registry startup");
        build_orchestrator(&settings)
            .expect("disabled default-enabled prebid config should not fail orchestrator startup");
    }

    #[test]
    fn enabled_invalid_integration_fails_registry_startup() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "gpt",
                &json!({
                    "enabled": true,
                    "script_url": "not a url",
                }),
            )
            .expect("should insert GPT config");

        let err = match IntegrationRegistry::new(&settings) {
            Ok(_) => panic!("enabled invalid integration should fail registry startup"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("Integration 'gpt'"),
            "should identify the invalid integration config"
        );
    }

    #[test]
    fn disabled_invalid_provider_config_does_not_fail_orchestrator_startup() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "adserver_mock",
                &json!({
                    "enabled": false,
                    "endpoint": "not a url",
                }),
            )
            .expect("should insert adserver mock config");

        build_orchestrator(&settings).expect("disabled invalid provider config should be ignored");
    }

    #[test]
    fn enabled_invalid_provider_config_fails_orchestrator_startup() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "adserver_mock",
                &json!({
                    "enabled": true,
                    "endpoint": "not a url",
                }),
            )
            .expect("should insert adserver mock config");

        let err = match build_orchestrator(&settings) {
            Ok(_) => panic!("enabled invalid provider config should fail startup"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("Integration 'adserver_mock'"),
            "should identify the invalid provider config"
        );
    }

    #[test]
    fn empty_prebid_server_url_fails_orchestrator_startup() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "prebid",
                &json!({
                    "enabled": true,
                    "server_url": "",
                }),
            )
            .expect("should insert prebid config");

        let err = match build_orchestrator(&settings) {
            Ok(_) => panic!("empty prebid server_url should fail startup"),
            Err(err) => err,
        };
        assert!(
            err.to_string()
                .contains("Integration 'prebid' configuration failed validation"),
            "should surface a validation error for prebid.server_url"
        );
    }

    /// Tests the full build.rs round-trip: env vars are baked into Settings
    /// at build time via `from_toml_and_env`, serialized to TOML, then parsed
    /// back at runtime via `from_toml`. Verifies that env-sourced integration
    /// values (strings like "true") are normalized to proper types so the
    /// serialized TOML has correct types.
    #[test]
    fn test_env_var_roundtrip_normalizes_integration_types() {
        let toml_str = crate_test_settings_str();

        let integration_prefix = format!(
            "{}{}INTEGRATIONS{}TESTLIGHT{}",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
        );
        let enabled_key = format!("{}ENABLED", integration_prefix);
        let endpoint_key = format!("{}ENDPOINT", integration_prefix);

        temp_env::with_var(enabled_key, Some("true"), || {
            temp_env::with_var(
                endpoint_key,
                Some("https://testlight-env.test/auction"),
                || {
                    // Step 1: Parse with env vars (what build.rs does)
                    let settings =
                        Settings::from_toml_and_env(&toml_str).expect("Settings should parse");

                    // Verify normalization converted "true" to bool
                    let raw = settings.integrations.get("testlight").unwrap();
                    assert!(
                        raw.get("enabled").unwrap().is_boolean(),
                        "enabled should be normalized to bool, got: {:?}",
                        raw.get("enabled")
                    );

                    // Step 2: Serialize to TOML (what build.rs does)
                    let merged_toml =
                        toml::to_string_pretty(&settings).expect("Should serialize to TOML");

                    // Step 3: Parse back (what runtime does)
                    let runtime_settings =
                        Settings::from_toml(&merged_toml).expect("Runtime should parse");

                    let config = runtime_settings
                        .integration_config::<TestlightConfig>("testlight")
                        .expect("should get config")
                        .expect("should be enabled");

                    assert_eq!(config.endpoint, "https://testlight-env.test/auction");
                    assert!(config.enabled);
                },
            );
        });
    }

    /// Verifies that `from_toml` does NOT read environment variables.
    /// The runtime path should only use the pre-built TOML.
    #[test]
    fn test_from_toml_ignores_env_vars() {
        let toml_str = crate_test_settings_str();

        temp_env::with_var(
            format!(
                "{}{}PUBLISHER{}DOMAIN",
                ENVIRONMENT_VARIABLE_PREFIX,
                ENVIRONMENT_VARIABLE_SEPARATOR,
                ENVIRONMENT_VARIABLE_SEPARATOR,
            ),
            Some("env-override.com"),
            || {
                let settings = Settings::from_toml(&toml_str).expect("should parse");
                assert_eq!(
                    settings.publisher.domain, "test-publisher.com",
                    "from_toml should ignore env vars"
                );
            },
        );
    }

    #[test]
    fn test_rewrite_is_excluded() {
        let rewrite = Rewrite {
            exclude_domains: vec!["cdn.example.com".to_string(), "*.example2.com".to_string()],
        };

        // Exact domain match
        assert!(rewrite.is_excluded("http://cdn.example.com/image.png"));

        // Wildcard match - base domain
        assert!(rewrite.is_excluded("https://example2.com/cdn.js"));
        // Wildcard match - subdomains
        assert!(rewrite.is_excluded("https://cdnjs.example2.com/lib.js"));
        assert!(rewrite.is_excluded("https://sub.domain.example2.com/asset.js"));

        // Should NOT match
        assert!(!rewrite.is_excluded("https://other.example.com/asset.js"));
        assert!(!rewrite.is_excluded("https://sub.cdn.example.com/asset.js"));
        assert!(!rewrite.is_excluded("https://example2.com.fake.com/asset.js"));
        assert!(!rewrite.is_excluded("https://notexample.com/asset.js"));

        // Invalid URLs should not crash and should return false
        assert!(!rewrite.is_excluded("not a url"));
        assert!(!rewrite.is_excluded(""));
    }

    #[test]
    fn test_auction_allowed_context_keys_defaults_to_empty() {
        let settings = create_test_settings();
        assert!(
            settings.auction.allowed_context_keys.is_empty(),
            "Default allowed_context_keys should be empty (secure-by-default)"
        );
    }

    #[test]
    fn test_auction_allowed_context_keys_from_toml() {
        let toml_str = crate_test_settings_str()
            + r#"
            [auction]
            enabled = true
            providers = []
            allowed_context_keys = ["permutive_segments", "lockr_ids"]
            "#;
        let settings = Settings::from_toml(&toml_str).expect("should parse valid TOML");
        assert_eq!(
            settings.auction.allowed_context_keys,
            HashSet::from(["permutive_segments".to_string(), "lockr_ids".to_string()])
        );
    }

    #[test]
    fn test_auction_empty_allowed_context_keys_blocks_all() {
        let toml_str = crate_test_settings_str()
            + r#"
            [auction]
            enabled = true
            providers = []
            allowed_context_keys = []
            "#;
        let settings = Settings::from_toml(&toml_str).expect("should parse valid TOML");
        assert!(
            settings.auction.allowed_context_keys.is_empty(),
            "Empty allowed_context_keys should be respected (blocks all keys)"
        );
    }

    // --- Proxy::normalize ---

    #[test]
    fn proxy_normalize_trims_and_lowercases() {
        let mut proxy = Proxy {
            certificate_check: true,
            allowed_domains: vec![
                "  AD.EXAMPLE.COM  ".to_string(),
                "*.Example.Org".to_string(),
            ],
        };
        proxy.normalize();
        assert_eq!(
            proxy.allowed_domains,
            vec!["ad.example.com".to_string(), "*.example.org".to_string()],
            "should trim and lowercase each entry"
        );
    }

    #[test]
    fn proxy_normalize_drops_empty_and_whitespace_entries() {
        let mut proxy = Proxy {
            certificate_check: true,
            allowed_domains: vec![
                "example.com".to_string(),
                "   ".to_string(),
                "".to_string(),
                "cdn.example.com".to_string(),
            ],
        };
        proxy.normalize();
        assert_eq!(
            proxy.allowed_domains,
            vec!["example.com".to_string(), "cdn.example.com".to_string()],
            "should drop blank and whitespace-only entries"
        );
    }

    #[test]
    fn proxy_normalize_removes_bare_wildcard() {
        let mut proxy = Proxy {
            certificate_check: true,
            allowed_domains: vec!["*".to_string(), "tracker.com".to_string()],
        };
        proxy.normalize();
        assert_eq!(
            proxy.allowed_domains,
            vec!["tracker.com".to_string()],
            "should remove bare \"*\" (invalid pattern that blocks all traffic)"
        );
    }

    #[test]
    fn proxy_normalize_bare_wildcard_alone_yields_open_mode() {
        let mut proxy = Proxy {
            certificate_check: true,
            allowed_domains: vec!["*".to_string()],
        };
        proxy.normalize();
        assert!(
            proxy.allowed_domains.is_empty(),
            "bare \"*\" alone should normalize to empty list (open mode)"
        );
    }

    #[test]
    fn proxy_normalize_all_blank_yields_empty_list() {
        let mut proxy = Proxy {
            certificate_check: true,
            allowed_domains: vec!["  ".to_string(), "\t".to_string()],
        };
        proxy.normalize();
        assert!(
            proxy.allowed_domains.is_empty(),
            "all-blank list should normalize to empty (open mode)"
        );
    }

    #[test]
    fn proxy_normalize_applied_by_from_toml() {
        let toml_str = crate_test_settings_str()
            + r#"
            [proxy]
            allowed_domains = ["  AD.EXAMPLE.COM  ", "  ", "*.CDN.Example.Com"]
            "#;
        let settings = Settings::from_toml(&toml_str).expect("should parse TOML");
        assert_eq!(
            settings.proxy.allowed_domains,
            vec![
                "ad.example.com".to_string(),
                "*.cdn.example.com".to_string()
            ],
            "from_toml should normalize allowed_domains"
        );
    }

    #[test]
    fn proxy_normalize_applied_by_from_toml_and_env() {
        let toml_str = crate_test_settings_str()
            + r#"
            [proxy]
            allowed_domains = ["  AD.EXAMPLE.COM  ", "  ", "*.CDN.Example.Com"]
            "#;
        let origin_key = format!(
            "{}{}PUBLISHER{}ORIGIN_URL",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        temp_env::with_var(
            origin_key,
            Some("https://origin.test-publisher.com"),
            || {
                let settings =
                    Settings::from_toml_and_env(&toml_str).expect("should parse TOML with env");
                assert_eq!(
                    settings.proxy.allowed_domains,
                    vec![
                        "ad.example.com".to_string(),
                        "*.cdn.example.com".to_string()
                    ],
                    "from_toml_and_env should normalize allowed_domains"
                );
            },
        );
    }

    // --- admin endpoint coverage ---

    #[test]
    fn test_publisher_rejects_cookie_domain_with_metacharacters() {
        for bad_domain in [
            "evil.com;\nSet-Cookie: bad=1",
            "evil.com\r\nX-Injected: yes",
            "evil.com;path=/",
        ] {
            let mut settings = create_test_settings();
            settings.publisher.cookie_domain = bad_domain.to_string();
            assert!(
                settings.validate().is_err(),
                "should reject cookie_domain containing metacharacters: {bad_domain:?}"
            );
        }
    }

    #[test]
    fn test_publisher_accepts_valid_cookie_domain() {
        let mut settings = create_test_settings();
        settings.publisher.cookie_domain = ".example.com".to_string();
        assert!(
            settings.validate().is_ok(),
            "should accept a valid cookie_domain"
        );
    }

    /// Helper that returns a settings TOML string WITHOUT any admin handler,
    /// for tests that need to verify uncovered-admin-endpoint behaviour.
    fn settings_str_without_admin_handler() -> String {
        r#"
            [[handlers]]
            path = "^/secure"
            username = "user"
            password = "pass"

            [publisher]
            domain = "test-publisher.com"
            cookie_domain = ".test-publisher.com"
            origin_url = "https://origin.test-publisher.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key"

            [request_signing]
            config_store_id = "test-config-store-id"
            secret_store_id = "test-secret-store-id"
        "#
        .to_string()
    }

    #[test]
    fn uncovered_admin_endpoints_returns_all_when_no_handler_covers_admin() {
        // Deserialize directly to bypass from_toml's admin validation,
        // since this test exercises uncovered_admin_endpoints itself.
        let settings: Settings =
            toml::from_str(&settings_str_without_admin_handler()).expect("should deserialize TOML");
        let uncovered = settings
            .uncovered_admin_endpoints()
            .expect("should check admin coverage");
        assert_eq!(
            uncovered,
            vec!["/_ts/admin/keys/rotate", "/_ts/admin/keys/deactivate",],
            "should report all admin endpoints as uncovered"
        );
    }

    #[test]
    fn uncovered_admin_endpoints_returns_empty_when_handler_covers_admin() {
        let settings = create_test_settings();
        let uncovered = settings
            .uncovered_admin_endpoints()
            .expect("should check admin coverage");
        assert!(
            uncovered.is_empty(),
            "should report no uncovered admin endpoints when handler covers /_ts/admin"
        );
    }

    #[test]
    fn uncovered_admin_endpoints_detects_partial_coverage() {
        let toml_str = settings_str_without_admin_handler()
            + r#"
            [[handlers]]
            path = "^/_ts/admin/keys/rotate$"
            username = "admin"
            password = "secret"
            "#;
        // Deserialize directly to bypass from_toml's admin validation,
        // since this test exercises uncovered_admin_endpoints itself.
        let settings: Settings = toml::from_str(&toml_str).expect("should deserialize TOML");
        let uncovered = settings
            .uncovered_admin_endpoints()
            .expect("should check admin coverage");
        assert_eq!(
            uncovered,
            vec!["/_ts/admin/keys/deactivate"],
            "should detect endpoints not covered by the rotate-only handler"
        );
    }

    #[test]
    fn from_toml_and_env_rejects_config_without_admin_handler() {
        let origin_key = format!(
            "{}{}PUBLISHER{}ORIGIN_URL",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        temp_env::with_var(
            origin_key,
            Some("https://origin.test-publisher.com"),
            || {
                let result = Settings::from_toml_and_env(&settings_str_without_admin_handler());
                assert!(
                    result.is_err(),
                    "should reject configuration when admin endpoints are not covered"
                );
                let err = format!("{:?}", result.unwrap_err());
                assert!(
                    err.contains("No handler covers admin endpoint"),
                    "error should mention uncovered admin endpoints, got: {err}"
                );
            },
        );
    }

    #[test]
    fn from_toml_rejects_config_without_admin_handler() {
        let result = Settings::from_toml(&settings_str_without_admin_handler());
        assert!(
            result.is_err(),
            "should reject configuration when admin endpoints are not covered"
        );
        let err = format!("{:?}", result.expect_err("should be an error"));
        assert!(
            err.contains("No handler covers admin endpoint"),
            "error should mention uncovered admin endpoints, got: {err}"
        );
    }

    /// Verifies that [`Settings::ADMIN_ENDPOINTS`] stays in sync with the
    /// admin route table in `crates/trusted-server-adapter-fastly/src/main.rs`.
    ///
    /// If this test fails, a route was added or removed in the Fastly
    /// router without updating `ADMIN_ENDPOINTS` (or vice versa).
    #[test]
    fn admin_endpoints_match_fastly_router() {
        let router_source = include_str!("../../trusted-server-adapter-fastly/src/main.rs");

        for endpoint in Settings::ADMIN_ENDPOINTS {
            assert!(
                router_source.contains(endpoint),
                "ADMIN_ENDPOINTS lists \"{endpoint}\" but it was not found in \
                 crates/trusted-server-adapter-fastly/src/main.rs — remove it from ADMIN_ENDPOINTS or \
                 add the route back to the router"
            );
        }

        // Also verify we haven't missed any admin routes in the router.
        // Best-effort: only detects string-literal routes in standard match-arm
        // format. If you define admin routes differently (e.g. via constants or
        // non-standard formatting), add them to ADMIN_ENDPOINTS manually.
        let admin_routes_in_router: Vec<&str> = router_source
            .lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                // Match arms look like: (Method::POST, "/_ts/admin/...") => ...
                if trimmed.starts_with('(') && trimmed.contains("\"/_ts/admin/") {
                    let start = trimmed.find("\"/_ts/admin/")?;
                    let rest = &trimmed[start + 1..];
                    let end = rest.find('"')?;
                    Some(&rest[..end])
                } else {
                    None
                }
            })
            .collect();

        for route in &admin_routes_in_router {
            assert!(
                Settings::ADMIN_ENDPOINTS.contains(route),
                "Router has admin route \"{route}\" that is missing from \
                 Settings::ADMIN_ENDPOINTS — add it to ensure auth coverage"
            );
        }
    }
}
