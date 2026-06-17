#[cfg(test)]
use config::{Config, Environment, File, FileFormat};
use error_stack::{Report, ResultExt};
use regex::Regex;
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::str::FromStr;
use std::sync::OnceLock;
use url::Url;
use validator::{Validate, ValidationError};

use crate::auction_config_types::AuctionConfig;
use crate::consent_config::ConsentConfig;
use crate::error::TrustedServerError;
use crate::host_header::validate_host_header_override_value;
use crate::platform::PlatformImageOptimizerRegion;
use crate::redacted::Redacted;

#[cfg(test)]
pub const ENVIRONMENT_VARIABLE_PREFIX: &str = "TRUSTED_SERVER";
#[cfg(test)]
pub const ENVIRONMENT_VARIABLE_SEPARATOR: &str = "__";

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Publisher {
    #[validate(custom(function = validate_publisher_domain))]
    pub domain: String,
    /// Domain for non-EC cookies. EC cookies use a separate computed domain
    /// (see [`ec_cookie_domain`](Self::ec_cookie_domain)).
    #[validate(custom(function = validate_cookie_domain))]
    pub cookie_domain: String,
    #[validate(custom(function = validate_no_trailing_slash))]
    pub origin_url: String,
    /// Optional outbound Host header to send while connecting to `origin_url`.
    #[serde(default)]
    #[validate(custom(function = validate_host_header_override))]
    pub origin_host_header_override: Option<String>,
    /// Secret used to encrypt/decrypt proxied URLs in `/first-party/proxy`.
    /// Keep this secret stable to allow existing links to decode.
    #[validate(custom(function = validate_redacted_not_empty))]
    pub proxy_secret: Redacted<String>,
    /// Maximum number of bytes buffered when a publisher origin response is
    /// post-processed in full (HTML rewriting/injection) instead of streamed.
    /// This caps the *decoded, post-rewrite* output buffer and applies to any
    /// such buffered response on **both** the legacy and `EdgeZero` paths;
    /// exceeding it fails the response rather than allocating past the cap.
    /// Defaults to 16 MiB — a conservative cap that prevents Wasm-heap OOM.
    ///
    /// On Fastly the *effective* ceiling for a publisher page is lower: the
    /// platform HTTP client rejects any origin response whose raw (still
    /// compressed) body exceeds 10 MiB before this buffer is ever filled, so
    /// raising this value only helps highly compressible pages whose decoded
    /// size exceeds the 16 MiB default while their compressed origin body stays
    /// under 10 MiB. Raising it above ~10 MiB does not lift the platform cap for
    /// uncompressed pages. That platform limit is removed once true streaming
    /// lands (tracked for PR 15, issue #495), after which this setting becomes
    /// the sole ceiling.
    ///
    /// Must be at least 1: a zero-byte cap fails every non-empty buffered
    /// publisher response at request time, so it is rejected at config
    /// validation instead.
    #[serde(default = "default_max_buffered_body_bytes")]
    #[validate(range(min = 1, message = "must be at least 1 byte"))]
    pub max_buffered_body_bytes: usize,
}

fn default_max_buffered_body_bytes() -> usize {
    16 * 1024 * 1024
}

impl Default for Publisher {
    /// Hand-written so `max_buffered_body_bytes` matches the serde default
    /// ([`default_max_buffered_body_bytes`]) instead of `usize`'s `0`. A derived
    /// `Default` would set a zero-byte cap, which fails buffered post-processing
    /// immediately when `Publisher::default()` / `Settings::default()` are used
    /// programmatically (tests, helpers) rather than deserialized from TOML.
    fn default() -> Self {
        Self {
            domain: String::default(),
            cookie_domain: String::default(),
            origin_url: String::default(),
            origin_host_header_override: None,
            proxy_secret: Redacted::default(),
            max_buffered_body_bytes: default_max_buffered_body_bytes(),
        }
    }
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
    ///     origin_host_header_override: None,
    ///     proxy_secret: Redacted::new("proxy-secret".to_string()),
    ///     max_buffered_body_bytes: 16 * 1024 * 1024,
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

    /// Returns the outbound Host header for proxied publisher-origin requests.
    #[must_use]
    pub fn origin_host_header(&self) -> String {
        self.origin_host_header_override
            .clone()
            .unwrap_or_else(|| self.origin_host())
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
#[serde(deny_unknown_fields)]
pub struct EcPartner {
    /// Human-readable partner name.
    pub name: String,
    /// `OpenRTB` `source.domain` for EID entries (e.g. `"liveramp.com"`).
    ///
    /// This normalized domain is also the canonical EC KV `ids` map key.
    #[validate(custom(function = EcPartner::validate_source_domain))]
    pub source_domain: String,
    /// `OpenRTB` `atype` value (typically 3).
    #[serde(
        default = "EcPartner::default_openrtb_atype",
        deserialize_with = "from_value_or_str"
    )]
    pub openrtb_atype: u8,
    /// Whether this partner's UIDs appear in auction `user.eids`.
    #[serde(default, deserialize_with = "from_value_or_str")]
    pub bidstream_enabled: bool,
    /// Plaintext API token. Hashed at startup for auth lookups.
    /// Used by batch sync (inbound) and identify (inbound).
    pub api_token: Redacted<String>,
    /// Max batch sync API requests per partner per minute.
    #[serde(
        default = "EcPartner::default_batch_rate_limit",
        deserialize_with = "from_value_or_str"
    )]
    pub batch_rate_limit: u32,
    /// Whether server-to-server pull sync is enabled for this partner.
    #[serde(default, deserialize_with = "from_value_or_str")]
    pub pull_sync_enabled: bool,
    /// URL to call for pull sync. Required when `pull_sync_enabled`.
    #[serde(default)]
    pub pull_sync_url: Option<String>,
    /// Allowlist of domains TS may call for this partner's pull sync.
    #[serde(default, deserialize_with = "vec_from_seq_or_map")]
    pub pull_sync_allowed_domains: Vec<String>,
    /// Legacy pull-sync refresh interval retained for config compatibility.
    ///
    /// EC identity entries no longer store per-partner sync timestamps, so
    /// this value is not used by the current fill-missing-only pull sync
    /// behavior.
    #[serde(
        default = "EcPartner::default_pull_sync_ttl_sec",
        deserialize_with = "from_value_or_str"
    )]
    pub pull_sync_ttl_sec: u64,
    /// Max pull sync calls per EC hash per partner per hour.
    #[serde(
        default = "EcPartner::default_pull_sync_rate_limit",
        deserialize_with = "from_value_or_str"
    )]
    pub pull_sync_rate_limit: u32,
    /// Outbound bearer token for pull sync requests.
    #[serde(default)]
    pub ts_pull_token: Option<Redacted<String>>,
}

impl EcPartner {
    /// Known partner API token placeholders that must not be used in deployments.
    pub const API_TOKEN_PLACEHOLDERS: &[&str] = &[
        "partner-api-token-32-bytes-minimum",
        "replace-with-partner-api-token-32-bytes-minimum",
        "sharedid-internal-token-32-bytes",
        "inttest-api-key-1-32-bytes-minimum",
        "inttest2-api-key-2-32-bytes-minimum",
    ];

    /// Returns `true` if `api_token` matches a known placeholder value
    /// (case-insensitive).
    #[must_use]
    pub fn is_placeholder_api_token(api_token: &str) -> bool {
        let token = api_token.trim();
        Self::API_TOKEN_PLACEHOLDERS
            .iter()
            .any(|placeholder| placeholder.eq_ignore_ascii_case(token))
    }

    /// Validates a partner source domain for use as the canonical key.
    ///
    /// # Errors
    ///
    /// Returns a validation error when `source_domain` is not a plain hostname.
    pub fn validate_source_domain(source_domain: &str) -> Result<(), ValidationError> {
        let trimmed = source_domain.trim();
        if trimmed.is_empty()
            || trimmed != source_domain
            || trimmed.len() > 255
            || !trimmed.is_ascii()
            || trimmed.contains("://")
            || trimmed.contains('/')
            || trimmed.contains(':')
        {
            return Err(ValidationError::new("invalid_source_domain"));
        }

        let normalized = trimmed.trim_end_matches('.').to_ascii_lowercase();
        if normalized.is_empty() || normalized.len() > 255 {
            return Err(ValidationError::new("invalid_source_domain"));
        }

        for label in normalized.split('.') {
            if label.is_empty() || label.len() > 63 {
                return Err(ValidationError::new("invalid_source_domain"));
            }
            let bytes = label.as_bytes();
            let Some(first) = bytes.first().copied() else {
                return Err(ValidationError::new("invalid_source_domain"));
            };
            let Some(last) = bytes.last().copied() else {
                return Err(ValidationError::new("invalid_source_domain"));
            };
            if !first.is_ascii_alphanumeric() || !last.is_ascii_alphanumeric() {
                return Err(ValidationError::new("invalid_source_domain"));
            }
            if !bytes
                .iter()
                .copied()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            {
                return Err(ValidationError::new("invalid_source_domain"));
            }
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
#[serde(deny_unknown_fields)]
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

    /// Legacy cluster re-check interval retained for config compatibility.
    ///
    /// EC identity entries no longer store cluster-check timestamps, so this
    /// value is not used. `/_ts/api/v1/identify` computes cluster size only
    /// when an entry does not already have a stored `cluster_size`.
    #[serde(default = "Ec::default_cluster_recheck_secs")]
    pub cluster_recheck_secs: u64,

    /// Partners (SSPs, DSPs, identity vendors) for EC identity sync.
    #[serde(default, deserialize_with = "vec_from_seq_or_map")]
    #[validate(nested)]
    pub partners: Vec<EcPartner>,
}

impl Ec {
    /// Known placeholder values that must not be used in production.
    pub const PASSPHRASE_PLACEHOLDERS: &[&str] = &[
        "secret-key",
        "secret_key",
        "trusted-server",
        "trusted-server-placeholder-secret",
    ];

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

    /// Minimum passphrase length for HMAC-SHA256 key strength.
    ///
    /// The EC passphrase is long-lived keying material for visitor ID
    /// derivation. Operators should use a high-entropy random passphrase per
    /// the EC setup and key-rotation documentation.
    const MIN_PASSPHRASE_LENGTH: usize = 32;

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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
    /// Known handler password placeholders that must not be used in deployments.
    pub const PASSWORD_PLACEHOLDERS: &[&str] = &[
        "replace-with-admin-password-32-bytes",
        "replace-with-admin-password",
        "change-me-admin-password",
    ];

    /// Returns `true` if `password` matches a known placeholder value
    /// (case-insensitive).
    #[must_use]
    pub fn is_placeholder_password(password: &str) -> bool {
        let password = password.trim();
        Self::PASSWORD_PLACEHOLDERS
            .iter()
            .any(|placeholder| placeholder.eq_ignore_ascii_case(password))
    }

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
#[serde(deny_unknown_fields)]
pub struct RequestSigning {
    #[serde(default = "default_request_signing_enabled")]
    pub enabled: bool,
    pub config_store_id: String,
    pub secret_store_id: String,
}

fn default_request_signing_enabled() -> bool {
    false
}

fn default_s3_secret_store() -> String {
    "s3-auth".to_string()
}

fn default_s3_access_key_id() -> String {
    "access_key_id".to_string()
}

fn default_s3_secret_access_key() -> String {
    "secret_access_key".to_string()
}

fn default_asset_image_optimizer_enabled() -> bool {
    true
}

fn default_profile_param() -> String {
    "profile".to_string()
}

fn default_aspect_ratio_param() -> String {
    "ar".to_string()
}

fn default_debug_param() -> String {
    "_io_debug".to_string()
}

fn default_default_profile() -> String {
    "default".to_string()
}

fn default_crop_offset_x_param() -> String {
    "x".to_string()
}

fn default_crop_offset_y_param() -> String {
    "y".to_string()
}

fn default_crop_offset_buckets() -> Vec<u32> {
    vec![10, 30, 50, 70, 90]
}

fn default_crop_offset_value() -> u32 {
    50
}

/// Query-string handling policy for upstream origin requests.
///
/// Plain asset routes default to [`Self::Preserve`]. Image-optimized asset
/// routes default to [`Self::Strip`] because transformation query parameters are
/// not usually part of the origin object identity.
#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginQueryPolicy {
    /// Preserve the incoming query string on the origin request.
    Preserve,
    /// Strip the incoming query string before sending to origin.
    Strip,
}

/// Authentication configuration for an asset origin.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum AssetOriginAuth {
    /// Sign asset origin requests with AWS Signature Version 4 for `S3`.
    #[serde(rename = "s3_sigv4", alias = "s3_sig_v4")]
    S3SigV4(S3SigV4AuthConfig),
}

impl AssetOriginAuth {
    fn normalize(&mut self) {
        match self {
            Self::S3SigV4(config) => config.normalize(),
        }
    }

    fn prepare_runtime(&self) -> Result<(), Report<TrustedServerError>> {
        match self {
            Self::S3SigV4(config) => config.prepare_runtime(),
        }
    }

    /// Return the configured origin query policy, if any.
    #[must_use]
    pub fn origin_query_policy(&self) -> Option<OriginQueryPolicy> {
        match self {
            Self::S3SigV4(config) => config.origin_query,
        }
    }
}

/// AWS Signature Version 4 configuration for `S3` asset origins.
///
/// The route `origin_url` must use the same `S3` host that `AWS` validates in
/// the `SigV4` canonical request. Credentials are read from the named runtime
/// secret store and cached per process by configured secret names.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct S3SigV4AuthConfig {
    /// `AWS` region used in the credential scope.
    pub region: String,
    /// Runtime secret store containing `S3` credentials.
    #[serde(default = "default_s3_secret_store")]
    pub secret_store: String,
    /// Secret name containing the `AWS` access key ID.
    #[serde(default = "default_s3_access_key_id")]
    pub access_key_id: String,
    /// Secret name containing the `AWS` secret access key.
    #[serde(default = "default_s3_secret_access_key")]
    pub secret_access_key: String,
    /// Optional secret name containing an `AWS` session token.
    #[serde(default)]
    pub session_token: Option<String>,
    /// Query-string handling policy for the signed `S3` origin request.
    ///
    /// Set this to `strip` when request query parameters are transformation
    /// inputs rather than `S3` object identity. If omitted, image-optimized routes
    /// strip queries and plain routes preserve them.
    #[serde(default)]
    pub origin_query: Option<OriginQueryPolicy>,
}

fn s3_region_is_valid(region: &str) -> bool {
    region
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

impl S3SigV4AuthConfig {
    fn normalize(&mut self) {
        self.region = self.region.trim().to_string();
        self.secret_store = self.secret_store.trim().to_string();
        self.access_key_id = self.access_key_id.trim().to_string();
        self.secret_access_key = self.secret_access_key.trim().to_string();
        self.session_token = self
            .session_token
            .take()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
    }

    fn prepare_runtime(&self) -> Result<(), Report<TrustedServerError>> {
        if self.region.is_empty() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: "proxy.asset_routes auth s3_sigv4 region must not be empty".to_string(),
            }));
        }
        if !s3_region_is_valid(&self.region) {
            return Err(Report::new(TrustedServerError::Configuration {
                message:
                    "proxy.asset_routes auth s3_sigv4 region must contain only lowercase letters, digits, and '-'"
                        .to_string(),
            }));
        }
        if self.secret_store.is_empty()
            || self.access_key_id.is_empty()
            || self.secret_access_key.is_empty()
        {
            return Err(Report::new(TrustedServerError::Configuration {
                message: "proxy.asset_routes auth s3_sigv4 secret names must not be empty"
                    .to_string(),
            }));
        }
        Ok(())
    }
}

/// Route-level Image Optimizer configuration for asset proxying.
///
/// This block only selects the processing region and profile set. The actual
/// transformation table lives under top-level [`ImageOptimizerSettings`] so
/// multiple routes can share one closed set of profiles.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssetImageOptimizerConfig {
    /// Enables Image Optimizer for this route when the table is present.
    #[serde(
        default = "default_asset_image_optimizer_enabled",
        deserialize_with = "bool_from_bool_or_str"
    )]
    pub enabled: bool,
    /// Image Optimizer processing region.
    pub region: String,
    /// Name of the top-level profile set used to convert request query params.
    pub profile_set: String,
    /// Query-string handling policy for the origin request.
    ///
    /// `preserve` is rejected while Image Optimizer is enabled because Fastly `IO`
    /// can interpret arbitrary request query parameters as transformation
    /// inputs outside the configured profile table.
    #[serde(default)]
    pub origin_query: Option<OriginQueryPolicy>,
}

impl AssetImageOptimizerConfig {
    fn normalize(&mut self) {
        self.region = self.region.trim().to_string();
        self.profile_set = self.profile_set.trim().to_string();
    }

    fn prepare_runtime(&self) -> Result<(), Report<TrustedServerError>> {
        if !self.enabled {
            return Ok(());
        }
        if self.region.is_empty() || self.profile_set.is_empty() {
            return Err(Report::new(TrustedServerError::Configuration {
                message:
                    "proxy.asset_routes image_optimizer region and profile_set must not be empty"
                        .to_string(),
            }));
        }
        if PlatformImageOptimizerRegion::parse(&self.region).is_none() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "proxy.asset_routes image_optimizer region `{}` is not supported",
                    self.region
                ),
            }));
        }
        Ok(())
    }
}

/// Behavior when a requested image profile is missing or unknown.
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownProfilePolicy {
    /// Use the configured default profile.
    #[default]
    UseDefault,
    /// Reject the request.
    Reject,
}

/// Top-level reusable Image Optimizer configuration.
///
/// Profile sets are keyed by arbitrary deployment-local names. Keep customer or
/// site-specific profile tables in private configuration overlays when those
/// values should not be committed to the public repository.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageOptimizerSettings {
    /// Named profile sets referenced by asset routes.
    #[serde(default)]
    pub profile_sets: HashMap<String, ImageOptimizerProfileSet>,
}

impl ImageOptimizerSettings {
    fn normalize(&mut self) {
        self.profile_sets = self
            .profile_sets
            .drain()
            .map(|(key, mut profile_set)| {
                profile_set.normalize();
                (key.trim().to_string(), profile_set)
            })
            .filter(|(key, _)| !key.is_empty())
            .collect();
    }

    /// Eagerly validate configured image profile sets.
    pub(crate) fn prepare_runtime(&self) -> Result<(), Report<TrustedServerError>> {
        for (name, profile_set) in &self.profile_sets {
            profile_set.prepare_runtime(name)?;
        }
        Ok(())
    }
}

/// Named set of profile-table Image Optimizer mappings.
///
/// Each profile value is a URL-encoded parameter string using the strict
/// supported subset: `quality`, `resize-filter`, `format`, `width`, `height`,
/// and `crop`. Profile-specific parameters override [`Self::base_params`].
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageOptimizerProfileSet {
    /// Params applied to every profile before profile-specific params.
    #[serde(default)]
    pub base_params: String,
    /// Profile used when the query omits or does not recognize a profile.
    #[serde(default = "default_default_profile")]
    pub default_profile: String,
    /// Unknown profile handling policy.
    #[serde(default)]
    pub unknown_profile: UnknownProfilePolicy,
    /// Query parameter that carries the profile name.
    #[serde(default = "default_profile_param")]
    pub profile_param: String,
    /// Query parameter that carries an aspect ratio override.
    #[serde(default = "default_aspect_ratio_param")]
    pub aspect_ratio_param: String,
    /// Query parameter that disables `IO` for a request when set to `1`.
    #[serde(default = "default_debug_param")]
    pub debug_param: String,
    /// Profile name to IO param string mapping.
    ///
    /// Values use query-string syntax, for example `format=auto&width=828`.
    #[serde(default)]
    pub profiles: HashMap<String, String>,
    /// Optional aspect-ratio override rules.
    #[serde(default)]
    pub aspect_ratios: Option<ImageOptimizerAspectRatioConfig>,
    /// Optional crop offset bucketing rules.
    #[serde(default)]
    pub crop_offsets: Option<ImageOptimizerCropOffsetsConfig>,
}

impl ImageOptimizerProfileSet {
    fn normalize(&mut self) {
        self.base_params = self.base_params.trim().to_string();
        self.default_profile = self.default_profile.trim().to_string();
        self.profile_param = self.profile_param.trim().to_string();
        self.aspect_ratio_param = self.aspect_ratio_param.trim().to_string();
        self.debug_param = self.debug_param.trim().to_string();
        self.profiles = self
            .profiles
            .drain()
            .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
            .filter(|(key, _)| !key.is_empty())
            .collect();
        if let Some(config) = &mut self.aspect_ratios {
            config.normalize();
        }
        if let Some(config) = &mut self.crop_offsets {
            config.normalize();
        }
    }

    fn prepare_runtime(&self, name: &str) -> Result<(), Report<TrustedServerError>> {
        if self.default_profile.is_empty()
            || self.profile_param.is_empty()
            || self.aspect_ratio_param.is_empty()
            || self.debug_param.is_empty()
        {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "image_optimizer.profile_sets `{name}` parameter names and default_profile must not be empty"
                ),
            }));
        }
        if !self.profiles.contains_key(&self.default_profile) {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "image_optimizer.profile_sets `{name}` default_profile `{}` is not defined",
                    self.default_profile
                ),
            }));
        }
        validate_image_optimizer_profile_set(name, self)?;
        if let Some(config) = &self.aspect_ratios {
            config.prepare_runtime(name, &self.profiles)?;
        }
        if let Some(config) = &self.crop_offsets {
            config.prepare_runtime(name)?;
        }
        Ok(())
    }
}

/// Aspect-ratio override configuration for an Image Optimizer profile set.
///
/// When a request uses an allowed profile and an allowed ratio value, the
/// profile crop is replaced with an aspect-ratio crop derived from the request
/// query value.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageOptimizerAspectRatioConfig {
    /// Allowed aspect ratio query values such as `1-1` or `16-9`.
    #[serde(default, deserialize_with = "vec_from_seq_or_map")]
    pub allowed: Vec<String>,
    /// Profiles that accept aspect-ratio overrides.
    #[serde(default, deserialize_with = "vec_from_seq_or_map")]
    pub profiles: Vec<String>,
}

impl ImageOptimizerAspectRatioConfig {
    fn normalize(&mut self) {
        self.allowed = self
            .allowed
            .iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect();
        self.profiles = self
            .profiles
            .iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect();
    }

    fn prepare_runtime(
        &self,
        name: &str,
        configured_profiles: &HashMap<String, String>,
    ) -> Result<(), Report<TrustedServerError>> {
        for ratio in &self.allowed {
            if parse_aspect_ratio_value(ratio).is_none() {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "image_optimizer.profile_sets `{name}` aspect ratio `{ratio}` must look like `width-height`"
                    ),
                }));
            }
        }
        for profile in &self.profiles {
            if !configured_profiles.contains_key(profile) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "image_optimizer.profile_sets `{name}` aspect ratio profile `{profile}` is not defined"
                    ),
                }));
            }
        }
        Ok(())
    }
}

/// Behavior when a bare crop has no explicit x/y offsets.
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingCropOffsetMode {
    /// Append Fastly `IO` `smart` crop mode.
    #[default]
    Smart,
    /// Leave the crop as-is.
    None,
}

/// Crop offset normalization configuration.
///
/// Offset bucketing caps output variant cardinality. Request values outside
/// `0..=100` or values that fail to parse fall back to [`Self::default`].
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageOptimizerCropOffsetsConfig {
    /// Enable crop offset normalization.
    #[serde(
        default = "default_asset_image_optimizer_enabled",
        deserialize_with = "bool_from_bool_or_str"
    )]
    pub enabled: bool,
    /// Query parameter containing the x-axis offset.
    #[serde(default = "default_crop_offset_x_param")]
    pub x_param: String,
    /// Query parameter containing the y-axis offset.
    #[serde(default = "default_crop_offset_y_param")]
    pub y_param: String,
    /// Sorted offset buckets used to cap variant cardinality.
    #[serde(
        default = "default_crop_offset_buckets",
        deserialize_with = "vec_from_seq_or_map"
    )]
    pub buckets: Vec<u32>,
    /// Default offset used when input is missing or invalid.
    #[serde(default = "default_crop_offset_value")]
    pub default: u32,
    /// Behavior when neither x nor y is present.
    #[serde(default)]
    pub when_missing: MissingCropOffsetMode,
}

impl ImageOptimizerCropOffsetsConfig {
    fn normalize(&mut self) {
        self.x_param = self.x_param.trim().to_string();
        self.y_param = self.y_param.trim().to_string();
        self.buckets.sort_unstable();
        self.buckets.dedup();
    }

    fn prepare_runtime(&self, name: &str) -> Result<(), Report<TrustedServerError>> {
        if self.x_param.is_empty() || self.y_param.is_empty() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "image_optimizer.profile_sets `{name}` crop offset param names must not be empty"
                ),
            }));
        }
        if self.buckets.is_empty()
            || self.buckets.iter().any(|bucket| *bucket > 100)
            || self.default > 100
        {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "image_optimizer.profile_sets `{name}` crop offset buckets/default must be in 0..=100"
                ),
            }));
        }
        Ok(())
    }
}

fn parse_aspect_ratio_value(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once('-')?;
    let width = width.parse::<u32>().ok()?;
    let height = height.parse::<u32>().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

fn validate_image_optimizer_profile_set(
    name: &str,
    profile_set: &ImageOptimizerProfileSet,
) -> Result<(), Report<TrustedServerError>> {
    validate_image_optimizer_param_string(name, "base_params", &profile_set.base_params)?;
    for (profile_name, params) in &profile_set.profiles {
        validate_image_optimizer_param_string(name, profile_name, params)?;
    }
    Ok(())
}

fn validate_image_optimizer_param_string(
    set_name: &str,
    profile_name: &str,
    params: &str,
) -> Result<(), Report<TrustedServerError>> {
    for (key, value) in url::form_urlencoded::parse(params.as_bytes()) {
        match key.as_ref() {
            "format" => validate_image_optimizer_format(set_name, profile_name, value.as_ref())?,
            "quality" => {
                validate_bounded_u32_param(
                    set_name,
                    profile_name,
                    "quality",
                    value.as_ref(),
                    0,
                    100,
                )?;
            }
            "resize-filter" => {
                validate_resize_filter(set_name, profile_name, value.as_ref())?;
            }
            "width" | "height" => {
                validate_positive_u32_param(set_name, profile_name, key.as_ref(), value.as_ref())?;
            }
            "crop" => validate_crop_param(set_name, profile_name, value.as_ref())?,
            unsupported => {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "image_optimizer.profile_sets `{set_name}` profile `{profile_name}` uses unsupported parameter `{unsupported}`"
                    ),
                }));
            }
        }
    }
    Ok(())
}

fn validate_image_optimizer_format(
    set_name: &str,
    profile_name: &str,
    value: &str,
) -> Result<(), Report<TrustedServerError>> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" | "avif" | "gif" | "jpeg" | "jpg" | "jxl" | "jpegxl" | "mp4" | "png"
        | "webp" => Ok(()),
        _ => Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "image_optimizer.profile_sets `{set_name}` profile `{profile_name}` has unsupported format `{value}`"
            ),
        })),
    }
}

fn validate_resize_filter(
    set_name: &str,
    profile_name: &str,
    value: &str,
) -> Result<(), Report<TrustedServerError>> {
    match value.trim().to_ascii_lowercase().as_str() {
        "nearest" | "bilinear" | "bicubic" | "lanczos2" | "lanczos3" => Ok(()),
        _ => Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "image_optimizer.profile_sets `{set_name}` profile `{profile_name}` has unsupported resize-filter `{value}`"
            ),
        })),
    }
}

fn validate_positive_u32_param(
    set_name: &str,
    profile_name: &str,
    param_name: &str,
    value: &str,
) -> Result<(), Report<TrustedServerError>> {
    let parsed = value.parse::<u32>().map_err(|err| {
        Report::new(TrustedServerError::Configuration {
            message: format!(
                "image_optimizer.profile_sets `{set_name}` profile `{profile_name}` parameter `{param_name}` must be an integer: {err}"
            ),
        })
    })?;
    if parsed == 0 {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "image_optimizer.profile_sets `{set_name}` profile `{profile_name}` parameter `{param_name}` must be greater than zero"
            ),
        }));
    }
    Ok(())
}

fn validate_bounded_u32_param(
    set_name: &str,
    profile_name: &str,
    param_name: &str,
    value: &str,
    min: u32,
    max: u32,
) -> Result<(), Report<TrustedServerError>> {
    let parsed = value.parse::<u32>().map_err(|err| {
        Report::new(TrustedServerError::Configuration {
            message: format!(
                "image_optimizer.profile_sets `{set_name}` profile `{profile_name}` parameter `{param_name}` must be an integer: {err}"
            ),
        })
    })?;
    if parsed < min || parsed > max {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "image_optimizer.profile_sets `{set_name}` profile `{profile_name}` parameter `{param_name}` must be in {min}..={max}"
            ),
        }));
    }
    Ok(())
}

fn validate_crop_param(
    set_name: &str,
    profile_name: &str,
    value: &str,
) -> Result<(), Report<TrustedServerError>> {
    let mut parts = value.split(',');
    let ratio = parts.next().unwrap_or_default();
    let Some((width, height)) = ratio.split_once(':') else {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "image_optimizer.profile_sets `{set_name}` profile `{profile_name}` crop `{value}` must look like `width:height`"
            ),
        }));
    };
    validate_positive_u32_param(set_name, profile_name, "crop width", width)?;
    validate_positive_u32_param(set_name, profile_name, "crop height", height)?;

    let mut has_smart = false;
    let mut has_offset_x = false;
    let mut has_offset_y = false;
    for suffix in parts {
        if suffix == "smart" {
            has_smart = true;
        } else if let Some(offset) = suffix.strip_prefix("offset-x") {
            validate_bounded_u32_param(set_name, profile_name, "crop offset-x", offset, 0, 100)?;
            has_offset_x = true;
        } else if let Some(offset) = suffix.strip_prefix("offset-y") {
            validate_bounded_u32_param(set_name, profile_name, "crop offset-y", offset, 0, 100)?;
            has_offset_y = true;
        } else {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "image_optimizer.profile_sets `{set_name}` profile `{profile_name}` crop has unsupported suffix `{suffix}`"
                ),
            }));
        }
    }

    if has_smart && (has_offset_x || has_offset_y) {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "image_optimizer.profile_sets `{set_name}` profile `{profile_name}` crop cannot combine smart with offsets"
            ),
        }));
    }
    if has_offset_x != has_offset_y {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "image_optimizer.profile_sets `{set_name}` profile `{profile_name}` crop offsets must include both offset-x and offset-y"
            ),
        }));
    }
    Ok(())
}

/// A path-prefix asset route that proxies matched first-party requests to an alternate origin.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyAssetRoute {
    /// Path prefix matched against the incoming request path. Must start with `/`.
    ///
    /// Matching uses string-prefix semantics, not path-segment semantics. Include
    /// a trailing `/` unless you intentionally want `/static` to match paths such
    /// as `/staticfile.js`.
    pub prefix: String,
    /// Absolute `http` or `https` origin used for upstream requests.
    ///
    /// Only the scheme, host, and port are used. Any path or query configured on
    /// this URL is rejected because the incoming request path/query, or the
    /// configured rewrite result, replaces them at runtime.
    pub origin_url: String,
    /// Optional regex matched against the incoming request path before proxying.
    pub path_pattern: Option<String>,
    /// Optional regex replacement used with [`Self::path_pattern`] to build the upstream path.
    ///
    /// Must be configured together with [`Self::path_pattern`] and must produce a
    /// path that starts with `/`.
    pub target_path: Option<String>,
    /// Optional origin authentication configuration.
    #[serde(default)]
    pub auth: Option<AssetOriginAuth>,
    /// Optional Image Optimizer configuration.
    #[serde(default)]
    pub image_optimizer: Option<AssetImageOptimizerConfig>,
    #[serde(skip, default)]
    compiled_pattern: OnceLock<Result<Regex, String>>,
}

impl ProxyAssetRoute {
    /// Create an asset route with the required prefix and origin URL.
    #[must_use]
    pub fn new(prefix: impl Into<String>, origin_url: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            origin_url: origin_url.into(),
            ..Self::default()
        }
    }

    fn normalize(&mut self) {
        self.prefix = self.prefix.trim().to_string();
        self.origin_url = self.origin_url.trim().to_string();
        self.path_pattern = self
            .path_pattern
            .take()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.target_path = self
            .target_path
            .take()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(auth) = &mut self.auth {
            auth.normalize();
        }
        if let Some(image_optimizer) = &mut self.image_optimizer {
            image_optimizer.normalize();
        }
    }

    fn compiled_path_pattern(&self) -> Result<Option<&Regex>, Report<TrustedServerError>> {
        let Some(pattern) = self.path_pattern.as_deref() else {
            return Ok(None);
        };

        match self
            .compiled_pattern
            .get_or_init(|| Regex::new(pattern).map_err(|err| err.to_string()))
        {
            Ok(regex) => Ok(Some(regex)),
            Err(message) => Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "proxy.asset_routes path_pattern `{pattern}` failed to compile: {message}"
                ),
            })),
        }
    }

    /// Rewrite a matched request path to the configured upstream target path.
    ///
    /// # Errors
    ///
    /// Returns a proxy/configuration error if the rewrite is incomplete, does not
    /// match the request path, or produces a path that does not start with `/`.
    pub fn target_path_for(&self, path: &str) -> Result<String, Report<TrustedServerError>> {
        match (&self.path_pattern, &self.target_path) {
            (None, None) => Ok(path.to_string()),
            (Some(_), Some(target_path)) => {
                let Some(regex) = self.compiled_path_pattern()? else {
                    return Err(Report::new(TrustedServerError::Configuration {
                        message: format!(
                            "proxy.asset_routes prefix `{}` must configure path_pattern and target_path together",
                            self.prefix
                        ),
                    }));
                };

                if !regex.is_match(path) {
                    return Err(Report::new(TrustedServerError::Proxy {
                        message: format!(
                            "asset path `{path}` matched prefix `{}` but did not match path_pattern",
                            self.prefix
                        ),
                    }));
                }

                let rewritten = regex.replace(path, target_path.as_str()).into_owned();
                if !rewritten.starts_with('/') {
                    return Err(Report::new(TrustedServerError::Configuration {
                        message: format!(
                            "proxy.asset_routes prefix `{}` rewrote `{path}` to `{rewritten}`, which must start with '/'",
                            self.prefix
                        ),
                    }));
                }

                Ok(rewritten)
            }
            _ => Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "proxy.asset_routes prefix `{}` must configure path_pattern and target_path together",
                    self.prefix
                ),
            })),
        }
    }

    /// Eagerly validate runtime-only asset-route configuration.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if the asset-route prefix, origin URL, or
    /// path rewrite settings are invalid.
    pub fn prepare_runtime(&self) -> Result<(), Report<TrustedServerError>> {
        validate_asset_route_prefix(&self.prefix).map_err(|err| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "proxy.asset_routes prefix `{}` is invalid: {err}",
                    self.prefix
                ),
            })
        })?;

        validate_proxy_origin_url(&self.origin_url).map_err(|err| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "proxy.asset_routes origin_url `{}` is invalid: {err}",
                    self.origin_url
                ),
            })
        })?;

        if matches!(&self.auth, Some(AssetOriginAuth::S3SigV4(_))) {
            let parsed_origin = Url::parse(&self.origin_url).map_err(|err| {
                Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "proxy.asset_routes origin_url `{}` is invalid: {err}",
                        self.origin_url
                    ),
                })
            })?;
            if parsed_origin.scheme() != "https" {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "proxy.asset_routes origin_url `{}` must use https when auth type is s3_sigv4",
                        self.origin_url
                    ),
                }));
            }
        }

        match (&self.path_pattern, &self.target_path) {
            (None, None) | (Some(_), Some(_)) => {}
            _ => {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "proxy.asset_routes prefix `{}` must configure path_pattern and target_path together",
                        self.prefix
                    ),
                }));
            }
        }

        if let Some(auth) = &self.auth {
            auth.prepare_runtime()?;
        }
        if let Some(image_optimizer) = &self.image_optimizer {
            image_optimizer.prepare_runtime()?;
        }
        if self.image_optimizer_enabled()
            && self.origin_query_policy() == OriginQueryPolicy::Preserve
        {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "proxy.asset_routes prefix `{}` cannot preserve origin query while image_optimizer is enabled; profile-table IO requires origin_query = \"strip\"",
                    self.prefix
                ),
            }));
        }

        self.compiled_path_pattern().map(|_| ())
    }

    /// Return true when this route has enabled Image Optimizer configuration.
    #[must_use]
    pub fn image_optimizer_enabled(&self) -> bool {
        self.image_optimizer
            .as_ref()
            .is_some_and(|config| config.enabled)
    }

    /// Return the effective origin query policy for this asset route.
    ///
    /// Precedence is auth-level `origin_query`, then enabled Image Optimizer
    /// `origin_query`, then the route default. The default is `strip` for
    /// enabled Image Optimizer routes and `preserve` otherwise.
    #[must_use]
    pub fn origin_query_policy(&self) -> OriginQueryPolicy {
        if let Some(policy) = self
            .auth
            .as_ref()
            .and_then(AssetOriginAuth::origin_query_policy)
        {
            return policy;
        }
        if let Some(policy) = self
            .image_optimizer
            .as_ref()
            .filter(|config| config.enabled)
            .and_then(|config| config.origin_query)
        {
            return policy;
        }
        if self.image_optimizer_enabled() {
            OriginQueryPolicy::Strip
        } else {
            OriginQueryPolicy::Preserve
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
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
    /// Path-prefix-based asset proxy routes evaluated before publisher fallback.
    #[serde(default, deserialize_with = "vec_from_seq_or_map")]
    pub asset_routes: Vec<ProxyAssetRoute>,
}

fn default_certificate_check() -> bool {
    true
}

fn is_admin_placeholder_password(password: &str) -> bool {
    Handler::is_placeholder_password(password)
        || matches!(
            password.trim().to_ascii_lowercase().as_str(),
            "changeme" | "password" | "admin"
        )
}

impl Default for Proxy {
    fn default() -> Self {
        Self {
            certificate_check: default_certificate_check(),
            allowed_domains: Vec::new(),
            asset_routes: Vec::new(),
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
            log::debug!(
                "proxy.allowed_domains is empty: all redirect destinations are permitted (open mode)"
            );
        }

        for route in &mut self.asset_routes {
            route.normalize();
        }

        let mut seen_prefixes = HashSet::new();
        for route in &self.asset_routes {
            if !route.prefix.is_empty() && !seen_prefixes.insert(route.prefix.clone()) {
                log::warn!(
                    "proxy.asset_routes contains duplicate prefix `{}`; the first configured route will be used",
                    route.prefix
                );
            }

            if !route.prefix.is_empty() && route.prefix != "/" && !route.prefix.ends_with('/') {
                log::warn!(
                    "proxy.asset_routes prefix `{}` does not end with `/`; matching uses raw string-prefix semantics, so this also matches paths such as `{}example`",
                    route.prefix,
                    route.prefix
                );
            }
        }
    }

    /// Eagerly validate runtime-only proxy settings artifacts.
    ///
    /// Asset-route validation lives here so regex compilation and origin URL
    /// semantic checks fail fast alongside other runtime-prepared settings.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if any configured asset route is invalid.
    pub fn prepare_runtime(&self) -> Result<(), Report<TrustedServerError>> {
        for route in &self.asset_routes {
            route.prepare_runtime()?;
        }

        Ok(())
    }

    /// Resolve the longest matching asset route for the given request path.
    #[must_use]
    pub fn asset_route_for_path(&self, path: &str) -> Option<&ProxyAssetRoute> {
        let mut best_match: Option<&ProxyAssetRoute> = None;

        for route in &self.asset_routes {
            if !path.starts_with(&route.prefix) {
                continue;
            }

            match best_match {
                Some(current) if current.prefix.len() >= route.prefix.len() => {}
                _ => best_match = Some(route),
            }
        }

        best_match
    }
}

/// Debug-only features. All flags default to `false` (off in production).
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DebugConfig {
    /// Expose the JA4/TLS fingerprint debug endpoint at `GET /_ts/debug/ja4`.
    ///
    /// When `false` (the default), the endpoint returns 404. Enable only for
    /// intentional Fastly/browser TLS investigation — the endpoint reflects
    /// Fastly-observed TLS details that browser JS cannot normally read.
    #[serde(default)]
    pub ja4_endpoint_enabled: bool,
}

/// Tester-cookie endpoint configuration.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct TesterCookieConfig {
    /// Enable `GET /_ts/set-tester`, which sets `ts-tester=true`.
    #[serde(default)]
    pub enabled: bool,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[validate(nested)]
    pub publisher: Publisher,
    #[serde(default)]
    pub tester_cookie: TesterCookieConfig,
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
    #[serde(default)]
    pub image_optimizer: ImageOptimizerSettings,
    #[serde(default)]
    pub debug: DebugConfig,
}

impl Settings {
    /// Creates a new [`Settings`] instance from a TOML string.
    ///
    /// # Errors
    ///
    /// - [`TrustedServerError::Configuration`] if the TOML is invalid or missing required fields
    pub fn from_toml(toml_str: &str) -> Result<Self, Report<TrustedServerError>> {
        let settings: Self =
            toml::from_str(toml_str).change_context(TrustedServerError::Configuration {
                message: "Failed to deserialize TOML configuration".to_string(),
            })?;

        Self::finalize_deserialized(settings, "Configuration")
    }

    /// Creates a new [`Settings`] instance from a JSON value.
    ///
    /// Runtime config-store loading uses this after reconstructing the flattened
    /// `app_config` entries into the same typed settings shape.
    ///
    /// # Errors
    ///
    /// - [`TrustedServerError::Configuration`] if the JSON value is invalid or missing required fields
    pub fn from_json_value(value: JsonValue) -> Result<Self, Report<TrustedServerError>> {
        let settings: Self =
            serde_json::from_value(value).change_context(TrustedServerError::Configuration {
                message: "Failed to deserialize JSON configuration".to_string(),
            })?;

        Self::finalize_deserialized(settings, "Configuration")
    }

    /// Creates a new [`Settings`] instance from a TOML string with legacy
    /// test-only `TRUSTED_SERVER__` environment variable overrides.
    ///
    /// Production loading does not support app-config environment overlays; this
    /// helper remains available to existing tests that exercise legacy parsing
    /// behavior.
    ///
    /// # Errors
    ///
    /// - [`TrustedServerError::Configuration`] if the TOML is invalid or missing required fields
    #[cfg(test)]
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
        let settings: Self =
            config
                .try_deserialize()
                .change_context(TrustedServerError::Configuration {
                    message: "Failed to deserialize configuration".to_string(),
                })?;

        Self::finalize_deserialized(settings, "Build-time configuration")
    }

    fn finalize_deserialized(
        mut settings: Self,
        validation_label: &str,
    ) -> Result<Self, Report<TrustedServerError>> {
        settings.integrations.normalize();
        settings.proxy.normalize();
        settings.image_optimizer.normalize();
        settings.consent.validate();

        settings.prepare_runtime()?;

        settings.validate().map_err(|err| {
            Report::new(TrustedServerError::Configuration {
                message: format!("{validation_label} validation failed: {err}"),
            })
        })?;

        settings.validate_admin_coverage()?;
        settings.validate_admin_handler_passwords()?;

        Ok(settings)
    }

    /// Eagerly prepare runtime-only settings artifacts.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if any handler path regex does not compile.
    pub fn prepare_runtime(&self) -> Result<(), Report<TrustedServerError>> {
        self.image_optimizer.prepare_runtime()?;
        self.proxy.prepare_runtime()?;
        self.validate_asset_image_optimizer_profile_sets()?;

        for handler in &self.handlers {
            handler.prepare_runtime()?;
        }

        for (name, value) in &self.response_headers {
            http::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("Invalid response header name: {name}"),
                })
            })?;
            http::header::HeaderValue::from_str(value).map_err(|_| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("Invalid response header value for {name}"),
                })
            })?;
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
        let mut insecure_fields: Vec<String> = Vec::new();

        if Ec::is_placeholder_passphrase(self.ec.passphrase.expose()) {
            insecure_fields.push("ec.passphrase".to_owned());
        }
        if Publisher::is_placeholder_proxy_secret(self.publisher.proxy_secret.expose()) {
            insecure_fields.push("publisher.proxy_secret".to_owned());
        }
        for partner in &self.ec.partners {
            if EcPartner::is_placeholder_api_token(partner.api_token.expose()) {
                insecure_fields.push(format!("ec.partners[{}].api_token", partner.source_domain));
            }
        }
        for handler in &self.handlers {
            if Handler::is_placeholder_password(handler.password.expose()) {
                insecure_fields.push(format!("handlers[{}].password", handler.path));
            }
        }

        if insecure_fields.is_empty() {
            return Ok(());
        }

        Err(Report::new(TrustedServerError::InsecureDefault {
            field: insecure_fields.join(", "),
        }))
    }

    fn validate_asset_image_optimizer_profile_sets(
        &self,
    ) -> Result<(), Report<TrustedServerError>> {
        for route in &self.proxy.asset_routes {
            let Some(config) = &route.image_optimizer else {
                continue;
            };
            if !config.enabled {
                continue;
            }
            if !self
                .image_optimizer
                .profile_sets
                .contains_key(&config.profile_set)
            {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "proxy.asset_routes prefix `{}` references unknown image_optimizer profile_set `{}`",
                        route.prefix, config.profile_set
                    ),
                }));
            }
        }
        Ok(())
    }

    /// Resolve the longest matching asset route for the request path.
    #[must_use]
    pub fn asset_route_for_path(&self, path: &str) -> Option<&ProxyAssetRoute> {
        self.proxy.asset_route_for_path(path)
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
    /// [`from_toml`](Self::from_toml) rejects configurations
    /// where any of these paths lack a matching handler, ensuring admin
    /// endpoints are always protected by authentication.
    /// Update [`ADMIN_ENDPOINTS`](Self::ADMIN_ENDPOINTS) when adding new
    /// admin routes to `crates/trusted-server-adapter-fastly/src/main.rs`.
    pub(crate) const ADMIN_ENDPOINTS: &[&str] =
        &["/_ts/admin/keys/rotate", "/_ts/admin/keys/deactivate"];

    /// Returns admin endpoint paths that no configured handler covers.
    ///
    /// Called during settings finalization to enforce that every admin endpoint
    /// has a handler. An empty return
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

    fn validate_admin_handler_passwords(&self) -> Result<(), Report<TrustedServerError>> {
        for handler in &self.handlers {
            let covers_admin = Self::ADMIN_ENDPOINTS
                .iter()
                .try_fold(false, |covered, path| {
                    handler.matches_path(path).map(|matches| covered || matches)
                })?;

            if covers_admin && is_admin_placeholder_password(handler.password.expose()) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "Admin handler `{}` uses a placeholder password; configure a strong secret",
                        handler.path
                    ),
                }));
            }
        }

        Ok(())
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
        err.message = Some("origin_url must not include a trailing slash".into());
        return Err(err);
    }
    Ok(())
}

fn validate_host_header_override(value: &str) -> Result<(), ValidationError> {
    if let Err(reason) = validate_host_header_override_value(value) {
        let mut err = ValidationError::new("invalid_host_header_override");
        err.add_param("value".into(), &value);
        err.add_param("reason".into(), &reason);
        err.message = Some(
            "origin_host_header_override must be a valid host or host:port without scheme, path, query, or fragment"
                .into(),
        );
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

fn validate_asset_route_prefix(value: &str) -> Result<(), ValidationError> {
    if !value.starts_with('/') {
        let mut err = ValidationError::new("invalid_prefix");
        err.add_param("value".into(), &value);
        err.message = Some("asset-route prefix must start with '/'".into());
        return Err(err);
    }

    Ok(())
}

fn validate_proxy_origin_url(value: &str) -> Result<(), ValidationError> {
    validate_no_trailing_slash(value)?;

    let parsed = Url::parse(value).map_err(|parse_error| {
        let mut err = ValidationError::new("invalid_origin_url");
        err.add_param("value".into(), &value);
        err.add_param("message".into(), &parse_error.to_string());
        err.message = Some("origin_url must be an absolute http or https URL".into());
        err
    })?;

    if !matches!(parsed.scheme(), "http" | "https") {
        let mut err = ValidationError::new("invalid_origin_url_scheme");
        err.add_param("value".into(), &value);
        err.message = Some("origin_url must use http or https".into());
        return Err(err);
    }

    if parsed.host_str().is_none() {
        let mut err = ValidationError::new("missing_origin_host");
        err.add_param("value".into(), &value);
        err.message = Some("origin_url must include a host".into());
        return Err(err);
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        let mut err = ValidationError::new("origin_url_has_userinfo");
        err.add_param("value".into(), &value);
        err.message = Some("origin_url must not include username or password".into());
        return Err(err);
    }

    if parsed.fragment().is_some() {
        let mut err = ValidationError::new("origin_url_has_fragment");
        err.add_param("value".into(), &value);
        err.message = Some("origin_url must not include a fragment".into());
        return Err(err);
    }

    if !matches!(parsed.path(), "" | "/") {
        let mut err = ValidationError::new("origin_url_has_path");
        err.add_param("value".into(), &value);
        err.message =
            Some("origin_url must not include a path; only scheme/host/port are used".into());
        return Err(err);
    }

    if parsed.query().is_some() {
        let mut err = ValidationError::new("origin_url_has_query");
        err.add_param("value".into(), &value);
        err.message = Some("origin_url must not include a query string".into());
        return Err(err);
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
fn from_value_or_str<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned + FromStr,
    T::Err: std::fmt::Display,
{
    let value = JsonValue::deserialize(deserializer)?;
    match value {
        JsonValue::String(value) => T::from_str(&value).map_err(serde::de::Error::custom),
        other => serde_json::from_value(other).map_err(serde::de::Error::custom),
    }
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

pub(crate) fn bool_from_bool_or_str<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let value = JsonValue::deserialize(deserializer)?;
    match value {
        JsonValue::Bool(value) => Ok(value),
        JsonValue::String(value) => value
            .trim()
            .parse::<bool>()
            .map_err(serde::de::Error::custom),
        other => Err(serde::de::Error::custom(format!(
            "expected bool or parseable bool string, got {other}"
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
        datadome::{DataDomeConfig, ProtectionMatcherConfig},
        gpt::GptConfig,
        nextjs::NextJsIntegrationConfig,
        prebid::PrebidIntegrationConfig,
        testlight::TestlightConfig,
        IntegrationRegistry,
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
        assert!(
            !settings.tester_cookie.enabled,
            "tester-cookie route should default to disabled"
        );
        assert_eq!(
            settings.publisher.ec_cookie_domain(),
            ".test-publisher.com",
            "EC cookie domain should be computed as .{{domain}}"
        );
        assert_eq!(
            settings.publisher.origin_url,
            "https://origin.test-publisher.com"
        );
        assert_eq!(settings.publisher.origin_host_header_override, None);
        assert_eq!(
            settings.ec.passphrase.expose(),
            "test-secret-key-32-bytes-minimum"
        );

        settings.validate().expect("Failed to validate settings");
    }

    #[test]
    fn tester_cookie_enabled_parses_from_toml() {
        let toml_str = format!(
            r#"{}

            [tester_cookie]
            enabled = true
        "#,
            crate_test_settings_str()
        );

        let settings = Settings::from_toml(&toml_str).expect("should parse tester-cookie config");

        assert!(
            settings.tester_cookie.enabled,
            "tester-cookie config should enable the route"
        );
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
    fn validate_rejects_invalid_ec_partner_source_domains() {
        for source_domain in [
            "",
            " bad.example.com",
            "https://bad.example.com",
            "bad.example.com/path",
            "bad.example.com:443",
            "bad_domain.example.com",
        ] {
            let toml_str = format!(
                r#"{}
                [[ec.partners]]
                name = "Invalid Partner"
                source_domain = "{}"
                api_token = "invalid-token"
                "#,
                crate_test_settings_str(),
                source_domain,
            );

            let result = Settings::from_toml(&toml_str);
            assert!(
                result.is_err(),
                "should reject invalid source_domain {source_domain:?}"
            );
        }
    }

    #[test]
    fn validate_accepts_origin_host_header_override() {
        let toml_str = crate_test_settings_str().replace(
            r#"origin_url = "https://origin.test-publisher.com""#,
            r#"origin_url = "https://origin.test-publisher.com"
origin_host_header_override = "www.example.com:8443""#,
        );

        let settings = Settings::from_toml(&toml_str).expect("should accept host header override");
        assert_eq!(
            settings.publisher.origin_host_header(),
            "www.example.com:8443",
            "should use configured host header override"
        );
    }

    #[test]
    fn publisher_rejects_unknown_fields() {
        let toml_str = crate_test_settings_str().replace(
            r#"origin_url = "https://origin.test-publisher.com""#,
            r#"origin_url = "https://origin.test-publisher.com"
origin_host_header_overide = "www.example.com""#,
        );

        let err = Settings::from_toml(&toml_str)
            .expect_err("unknown publisher fields should fail configuration loading");
        assert!(
            format!("{err:?}").contains("origin_host_header_overide"),
            "error should identify the misspelled publisher field: {err:?}"
        );
    }

    #[test]
    fn validate_rejects_invalid_origin_host_header_overrides() {
        for override_value in [
            "",
            " www.example.com",
            "www.example.com ",
            "https://www.example.com",
            "www.example.com/path",
            "www.example.com?query=1",
            "www.example.com#fragment",
            "www.example.com\n",
            "www.example.com:",
            "www.example.com:99999",
            "example..com",
            ".",
            "-",
            "-example.com",
            "example-.com",
            "[::1",
        ] {
            let toml_str = crate_test_settings_str().replace(
                r#"origin_url = "https://origin.test-publisher.com""#,
                &format!(
                    "origin_url = \"https://origin.test-publisher.com\"\norigin_host_header_override = {override_value:?}"
                ),
            );

            let result = Settings::from_toml(&toml_str);
            assert!(
                result.is_err(),
                "origin_host_header_override {override_value:?} should fail validation"
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
            !Ec::is_placeholder_passphrase("test-secret-key-32-bytes-minimum"),
            "should accept non-placeholder passphrase"
        );
    }

    #[test]
    fn is_placeholder_api_token_rejects_all_known_placeholders() {
        for placeholder in EcPartner::API_TOKEN_PLACEHOLDERS {
            assert!(
                EcPartner::is_placeholder_api_token(placeholder),
                "should detect placeholder api_token '{placeholder}'"
            );
        }
    }

    #[test]
    fn is_placeholder_api_token_is_case_insensitive() {
        assert!(
            EcPartner::is_placeholder_api_token("SHAREDID-INTERNAL-TOKEN-32-BYTES"),
            "should detect case-insensitive placeholder api_token"
        );
    }

    #[test]
    fn is_placeholder_api_token_accepts_non_placeholder() {
        assert!(
            !EcPartner::is_placeholder_api_token("production-partner-token-32-bytes-min"),
            "should accept non-placeholder api_token"
        );
    }

    #[test]
    fn validate_passphrase_rejects_under_32_characters() {
        let passphrase = Redacted::new("a".repeat(31));

        let err = Ec::validate_passphrase(&passphrase).expect_err("should reject short passphrase");

        assert_eq!(
            err.code.as_ref(),
            "short_passphrase",
            "should report short passphrase validation error"
        );
    }

    #[test]
    fn validate_passphrase_accepts_32_characters() {
        let passphrase = Redacted::new("a".repeat(32));

        Ec::validate_passphrase(&passphrase).expect("should accept 32-character passphrase");
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
    fn is_placeholder_handler_password_rejects_known_template_value() {
        assert!(
            Handler::is_placeholder_password("replace-with-admin-password-32-bytes"),
            "init-template handler password should be rejected"
        );
    }

    #[test]
    fn reject_placeholder_secrets_includes_handler_passwords() {
        let mut settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");
        settings.publisher.proxy_secret = Redacted::new("unit-test-proxy-secret".to_owned());
        settings.ec.passphrase = Redacted::new("test-secret-key-32-bytes-minimum".to_owned());
        settings.handlers[0].password =
            Redacted::new("replace-with-admin-password-32-bytes".to_owned());

        let err = settings
            .reject_placeholder_secrets()
            .expect_err("should reject placeholder handler password");
        assert!(
            format!("{err:?}").contains("handlers"),
            "error should mention handler password field"
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
    fn test_prebid_bid_param_overrides_override_with_json_env() {
        let toml_str = crate_test_settings_str();
        let env_key = format!(
            "{}{}INTEGRATIONS{}PREBID{}BID_PARAM_OVERRIDES",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

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
                temp_env::with_var(
                    env_key,
                    Some(r#"{"criteo":{"networkId":99999,"pubid":"server-pub"}}"#),
                    || {
                        let settings = Settings::from_toml_and_env(&toml_str)
                            .expect("Settings should parse with bidder param override env");
                        let cfg = settings
                            .integration_config::<PrebidIntegrationConfig>("prebid")
                            .expect("Prebid config query should succeed")
                            .expect("Prebid config should exist with env override");
                        let cfg_json =
                            serde_json::to_value(&cfg).expect("should serialize config to JSON");

                        assert_eq!(
                            cfg_json["bid_param_overrides"]["criteo"]["networkId"],
                            json!(99999),
                            "should deserialize networkId override from env JSON"
                        );
                        assert_eq!(
                            cfg_json["bid_param_overrides"]["criteo"]["pubid"],
                            json!("server-pub"),
                            "should deserialize pubid override from env JSON"
                        );
                    },
                );
            },
        );
    }

    #[test]
    fn test_prebid_bid_param_override_rules_override_with_json_env() {
        let toml_str = crate_test_settings_str();
        let env_key = format!(
            "{}{}INTEGRATIONS{}PREBID{}BID_PARAM_OVERRIDE_RULES",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

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
                temp_env::with_var(
                    env_key,
                    Some(
                        r#"[{"when":{"bidder":"kargo","zone":"header"},"set":{"placementId":"server-header","keep":"yes"}}]"#,
                    ),
                    || {
                        let settings = Settings::from_toml_and_env(&toml_str)
                            .expect("Settings should parse canonical bidder param override rules");
                        let cfg = settings
                            .integration_config::<PrebidIntegrationConfig>("prebid")
                            .expect("Prebid config query should succeed")
                            .expect("Prebid config should exist with env override");
                        let cfg_json =
                            serde_json::to_value(&cfg).expect("should serialize config to JSON");

                        assert_eq!(
                            cfg_json["bid_param_override_rules"][0]["when"]["bidder"],
                            json!("kargo"),
                            "should deserialize bidder matcher from env JSON"
                        );
                        assert_eq!(
                            cfg_json["bid_param_override_rules"][0]["when"]["zone"],
                            json!("header"),
                            "should deserialize zone matcher from env JSON"
                        );
                        assert_eq!(
                            cfg_json["bid_param_override_rules"][0]["set"]["placementId"],
                            json!("server-header"),
                            "should deserialize set object from env JSON"
                        );
                    },
                );
            },
        );
    }

    #[test]
    fn test_datadome_protection_scope_overrides_with_json_env() {
        let toml_str = crate_test_settings_str();
        let separator = ENVIRONMENT_VARIABLE_SEPARATOR;
        let origin_key = format!(
            "{}{}PUBLISHER{}ORIGIN_URL",
            ENVIRONMENT_VARIABLE_PREFIX, separator, separator
        );
        let enabled_key = format!(
            "{}{}INTEGRATIONS{}DATADOME{}ENABLED",
            ENVIRONMENT_VARIABLE_PREFIX, separator, separator, separator
        );
        let enable_protection_key = format!(
            "{}{}INTEGRATIONS{}DATADOME{}ENABLE_PROTECTION",
            ENVIRONMENT_VARIABLE_PREFIX, separator, separator, separator
        );
        let excluded_methods_key = format!(
            "{}{}INTEGRATIONS{}DATADOME{}PROTECTION_EXCLUDED_METHODS",
            ENVIRONMENT_VARIABLE_PREFIX, separator, separator, separator
        );
        let cidr_sources_key = format!(
            "{}{}INTEGRATIONS{}DATADOME{}PROTECTION_EXCLUDED_IP_CIDR_SOURCES",
            ENVIRONMENT_VARIABLE_PREFIX, separator, separator, separator
        );
        let rules_key = format!(
            "{}{}INTEGRATIONS{}DATADOME{}PROTECTION_EXCLUSION_RULES",
            ENVIRONMENT_VARIABLE_PREFIX, separator, separator, separator
        );

        temp_env::with_vars(
            [
                (origin_key, Some("https://origin.test-publisher.com")),
                (enabled_key, Some("true")),
                (enable_protection_key, Some("true")),
                (excluded_methods_key, Some(r#"["OPTIONS","TRACE"]"#)),
                (
                    cidr_sources_key,
                    Some(r#"[{"config_store":"datadome-ip-bypass","key":"googlebot_ips"}]"#),
                ),
                (
                    rules_key,
                    Some(
                        r#"[{"id":"legacy-static-get-head","methods":["GET","HEAD"],"type":"path_regex","patterns":["(?i)\\.(css|js)$"]},{"id":"next-rsc","type":"query_param_non_empty","names":["_rsc"]}]"#,
                    ),
                ),
            ],
            || {
                let settings = Settings::from_toml_and_env(&toml_str)
                    .expect("Settings should parse DataDome JSON env overrides");
                let cfg = settings
                    .integration_config::<DataDomeConfig>("datadome")
                    .expect("DataDome config query should succeed")
                    .expect("DataDome config should exist with env override");

                assert!(cfg.enabled, "should parse enabled override as bool");
                assert!(
                    cfg.enable_protection,
                    "should parse enable_protection override as bool"
                );
                assert_eq!(
                    cfg.protection_excluded_methods,
                    vec!["OPTIONS".to_string(), "TRACE".to_string()],
                    "should parse method list from JSON env override"
                );
                assert_eq!(
                    cfg.protection_excluded_ip_cidr_sources[0].config_store, "datadome-ip-bypass",
                    "should parse CIDR source config_store from JSON env override"
                );
                assert_eq!(
                    cfg.protection_excluded_ip_cidr_sources[0].key, "googlebot_ips",
                    "should parse CIDR source key from JSON env override"
                );
                assert_eq!(
                    cfg.protection_exclusion_rules.len(),
                    2,
                    "should parse all structured rules from JSON env override"
                );
                assert!(matches!(
                    &cfg.protection_exclusion_rules[0].matcher,
                    ProtectionMatcherConfig::PathRegex { patterns }
                        if patterns == &vec!["(?i)\\.(css|js)$".to_string()]
                ));
                assert!(matches!(
                    &cfg.protection_exclusion_rules[1].matcher,
                    ProtectionMatcherConfig::QueryParamNonEmpty { names }
                        if names == &vec!["_rsc".to_string()]
                ));
            },
        );
    }

    #[test]
    fn test_datadome_protection_scope_overrides_with_indexed_env() {
        let toml_str = crate_test_settings_str();
        let separator = ENVIRONMENT_VARIABLE_SEPARATOR;
        let datadome_prefix = format!(
            "{}{}INTEGRATIONS{}DATADOME{}",
            ENVIRONMENT_VARIABLE_PREFIX, separator, separator, separator
        );
        let origin_key = format!(
            "{}{}PUBLISHER{}ORIGIN_URL",
            ENVIRONMENT_VARIABLE_PREFIX, separator, separator
        );

        temp_env::with_vars(
            [
                (origin_key, Some("https://origin.test-publisher.com")),
                (format!("{datadome_prefix}ENABLED"), Some("true")),
                (
                    format!("{datadome_prefix}ENABLE_PROTECTION"),
                    Some("true"),
                ),
                (
                    format!("{datadome_prefix}PROTECTION_EXCLUDED_METHODS{separator}0"),
                    Some("OPTIONS"),
                ),
                (
                    format!("{datadome_prefix}PROTECTION_EXCLUDED_METHODS{separator}1"),
                    Some("TRACE"),
                ),
                (
                    format!("{datadome_prefix}PROTECTION_EXCLUDED_ASNS{separator}0"),
                    Some("19750"),
                ),
                (
                    format!("{datadome_prefix}PROTECTION_EXCLUDED_IP_CIDRS{separator}0"),
                    Some("198.51.100.0/24"),
                ),
                (
                    format!(
                        "{datadome_prefix}PROTECTION_EXCLUDED_IP_CIDR_SOURCES{separator}0{separator}CONFIG_STORE"
                    ),
                    Some("datadome-ip-bypass"),
                ),
                (
                    format!(
                        "{datadome_prefix}PROTECTION_EXCLUDED_IP_CIDR_SOURCES{separator}0{separator}KEY"
                    ),
                    Some("googlebot_ips"),
                ),
                (
                    format!("{datadome_prefix}PROTECTION_EXCLUSION_RULES{separator}0{separator}ID"),
                    Some("legacy-static-get-head"),
                ),
                (
                    format!("{datadome_prefix}PROTECTION_EXCLUSION_RULES{separator}0{separator}METHODS{separator}0"),
                    Some("GET"),
                ),
                (
                    format!("{datadome_prefix}PROTECTION_EXCLUSION_RULES{separator}0{separator}METHODS{separator}1"),
                    Some("HEAD"),
                ),
                (
                    format!("{datadome_prefix}PROTECTION_EXCLUSION_RULES{separator}0{separator}TYPE"),
                    Some("path_regex"),
                ),
                (
                    format!("{datadome_prefix}PROTECTION_EXCLUSION_RULES{separator}0{separator}PATTERNS{separator}0"),
                    Some(r"(?i)\.(css|js)$"),
                ),
                (
                    format!("{datadome_prefix}PROTECTION_EXCLUSION_RULES{separator}1{separator}ID"),
                    Some("next-rsc"),
                ),
                (
                    format!("{datadome_prefix}PROTECTION_EXCLUSION_RULES{separator}1{separator}TYPE"),
                    Some("query_param_non_empty"),
                ),
                (
                    format!("{datadome_prefix}PROTECTION_EXCLUSION_RULES{separator}1{separator}NAMES{separator}0"),
                    Some("_rsc"),
                ),
            ],
            || {
                let settings = Settings::from_toml_and_env(&toml_str)
                    .expect("Settings should parse DataDome indexed env overrides");
                let cfg = settings
                    .integration_config::<DataDomeConfig>("datadome")
                    .expect("DataDome config query should succeed")
                    .expect("DataDome config should exist with indexed env override");

                assert_eq!(
                    cfg.protection_excluded_methods,
                    vec!["OPTIONS".to_string(), "TRACE".to_string()],
                    "should parse indexed method list"
                );
                assert_eq!(
                    cfg.protection_excluded_asns,
                    vec![19750],
                    "should parse indexed ASN list"
                );
                assert_eq!(
                    cfg.protection_excluded_ip_cidrs,
                    vec!["198.51.100.0/24".to_string()],
                    "should parse indexed IP CIDR list"
                );
                assert_eq!(
                    cfg.protection_excluded_ip_cidr_sources[0].key,
                    "googlebot_ips",
                    "should parse indexed CIDR source list"
                );
                assert!(matches!(
                    &cfg.protection_exclusion_rules[0].matcher,
                    ProtectionMatcherConfig::PathRegex { patterns }
                        if patterns == &vec!["(?i)\\.(css|js)$".to_string()]
                ));
                assert!(matches!(
                    &cfg.protection_exclusion_rules[1].matcher,
                    ProtectionMatcherConfig::QueryParamNonEmpty { names }
                        if names == &vec!["_rsc".to_string()]
                ));
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
    fn test_ec_partners_override_with_indexed_env() {
        let toml_str = crate_test_settings_str();

        let origin_key = format!(
            "{}{}PUBLISHER{}ORIGIN_URL",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let partner_0_name_key = format!(
            "{}{}EC{}PARTNERS{}0{}NAME",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let partner_0_source_domain_key = format!(
            "{}{}EC{}PARTNERS{}0{}SOURCE_DOMAIN",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let partner_0_openrtb_atype_key = format!(
            "{}{}EC{}PARTNERS{}0{}OPENRTB_ATYPE",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let partner_0_bidstream_enabled_key = format!(
            "{}{}EC{}PARTNERS{}0{}BIDSTREAM_ENABLED",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let partner_0_api_token_key = format!(
            "{}{}EC{}PARTNERS{}0{}API_TOKEN",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let partner_1_name_key = format!(
            "{}{}EC{}PARTNERS{}1{}NAME",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let partner_1_source_domain_key = format!(
            "{}{}EC{}PARTNERS{}1{}SOURCE_DOMAIN",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let partner_1_openrtb_atype_key = format!(
            "{}{}EC{}PARTNERS{}1{}OPENRTB_ATYPE",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let partner_1_bidstream_enabled_key = format!(
            "{}{}EC{}PARTNERS{}1{}BIDSTREAM_ENABLED",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let partner_1_api_token_key = format!(
            "{}{}EC{}PARTNERS{}1{}API_TOKEN",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

        temp_env::with_vars(
            [
                (origin_key, Some("https://origin.test-publisher.com")),
                (partner_0_name_key, Some("Env Partner 0")),
                (partner_0_source_domain_key, Some("envpartner0.example.com")),
                (partner_0_openrtb_atype_key, Some("1")),
                (partner_0_bidstream_enabled_key, Some("true")),
                (partner_0_api_token_key, Some("env-token-0")),
                (partner_1_name_key, Some("Env Partner 1")),
                (partner_1_source_domain_key, Some("envpartner1.example.com")),
                (partner_1_openrtb_atype_key, Some("3")),
                (partner_1_bidstream_enabled_key, Some("false")),
                (partner_1_api_token_key, Some("env-token-1")),
            ],
            || {
                let settings = Settings::from_toml_and_env(&toml_str)
                    .expect("Settings should load indexed EC partners from env");

                assert_eq!(settings.ec.partners.len(), 2);
                assert_eq!(settings.ec.partners[0].name, "Env Partner 0");
                assert_eq!(
                    settings.ec.partners[0].source_domain,
                    "envpartner0.example.com"
                );
                assert_eq!(settings.ec.partners[0].openrtb_atype, 1);
                assert!(settings.ec.partners[0].bidstream_enabled);
                assert_eq!(settings.ec.partners[0].api_token.expose(), "env-token-0");
                assert_eq!(settings.ec.partners[1].name, "Env Partner 1");
                assert_eq!(
                    settings.ec.partners[1].source_domain,
                    "envpartner1.example.com"
                );
                assert_eq!(settings.ec.partners[1].openrtb_atype, 3);
                assert!(!settings.ec.partners[1].bidstream_enabled);
                assert_eq!(settings.ec.partners[1].api_token.expose(), "env-token-1");
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
        assert!(
            settings.is_err(),
            "unknown top-level fields should be rejected"
        );
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
    fn test_origin_host_header_override_env() {
        let env_key = format!(
            "{}{}PUBLISHER{}ORIGIN_HOST_HEADER_OVERRIDE",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

        temp_env::with_var(env_key, Some("www.example.com"), || {
            let settings = Settings::from_toml_and_env(&crate_test_settings_str())
                .expect("should load settings with host header override env");

            assert_eq!(
                settings.publisher.origin_host_header_override.as_deref(),
                Some("www.example.com")
            );
            assert_eq!(settings.publisher.origin_host_header(), "www.example.com");
        });
    }

    #[test]
    fn test_origin_host_header_override_env_typo_fails_closed() {
        let env_key = format!(
            "{}{}PUBLISHER{}ORIGIN_HOST_HEADER_OVERIDE",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

        temp_env::with_var(env_key, Some("www.example.com"), || {
            let err = Settings::from_toml_and_env(&crate_test_settings_str())
                .expect_err("misspelled host override env var should fail configuration loading");
            assert!(
                format!("{err:?}").contains("origin_host_header_overide"),
                "error should identify the misspelled publisher env field: {err:?}"
            );
        });
    }

    #[test]
    fn test_publisher_origin_host() {
        // Test with full URL including port
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "https://origin.example.com:8080".to_string(),
            origin_host_header_override: None,
            proxy_secret: Redacted::new("test-secret".to_string()),
            max_buffered_body_bytes: 16 * 1024 * 1024,
        };
        assert_eq!(publisher.origin_host(), "origin.example.com:8080");

        // Test with URL without port (default HTTPS port)
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            origin_host_header_override: None,
            proxy_secret: Redacted::new("test-secret".to_string()),
            max_buffered_body_bytes: 16 * 1024 * 1024,
        };
        assert_eq!(publisher.origin_host(), "origin.example.com");

        // Test with HTTP URL with explicit port
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "http://localhost:9090".to_string(),
            origin_host_header_override: None,
            proxy_secret: Redacted::new("test-secret".to_string()),
            max_buffered_body_bytes: 16 * 1024 * 1024,
        };
        assert_eq!(publisher.origin_host(), "localhost:9090");

        // Test with URL without protocol (fallback to original)
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "localhost:9090".to_string(),
            origin_host_header_override: None,
            proxy_secret: Redacted::new("test-secret".to_string()),
            max_buffered_body_bytes: 16 * 1024 * 1024,
        };
        assert_eq!(publisher.origin_host(), "localhost:9090");

        // Test with IPv4 address
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "http://192.168.1.1:8080".to_string(),
            origin_host_header_override: None,
            proxy_secret: Redacted::new("test-secret".to_string()),
            max_buffered_body_bytes: 16 * 1024 * 1024,
        };
        assert_eq!(publisher.origin_host(), "192.168.1.1:8080");

        // Test with IPv6 address
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "http://[::1]:8080".to_string(),
            origin_host_header_override: None,
            proxy_secret: Redacted::new("test-secret".to_string()),
            max_buffered_body_bytes: 16 * 1024 * 1024,
        };
        assert_eq!(publisher.origin_host(), "[::1]:8080");
    }

    #[test]
    fn test_publisher_origin_host_header_defaults_to_origin_host() {
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "https://origin.example.com:8443".to_string(),
            origin_host_header_override: None,
            proxy_secret: Redacted::new("test-secret".to_string()),
            max_buffered_body_bytes: 16 * 1024 * 1024,
        };

        assert_eq!(publisher.origin_host_header(), "origin.example.com:8443");
    }

    #[test]
    fn test_publisher_origin_host_header_uses_override() {
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            origin_host_header_override: Some("www.example.com".to_string()),
            proxy_secret: Redacted::new("test-secret".to_string()),
            max_buffered_body_bytes: 16 * 1024 * 1024,
        };

        assert_eq!(publisher.origin_host_header(), "www.example.com");
    }

    #[test]
    fn publisher_default_max_buffered_body_bytes_matches_config_default() {
        // The manual `Default` impl must agree with the serde default applied
        // when the key is omitted from TOML, so programmatic `Publisher::default()`
        // does not silently produce a zero-byte buffer cap.
        assert_eq!(
            Publisher::default().max_buffered_body_bytes,
            super::default_max_buffered_body_bytes(),
            "Publisher::default() must use the same buffer cap as the TOML default"
        );

        let from_toml = Settings::from_toml(
            r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "example.com"
            cookie_domain = ".example.com"
            origin_url = "https://origin.example.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"
            "#,
        )
        .expect("should parse settings without max_buffered_body_bytes");
        assert_eq!(
            from_toml.publisher.max_buffered_body_bytes,
            Publisher::default().max_buffered_body_bytes,
            "TOML default and Publisher::default() must stay aligned"
        );
    }

    #[test]
    fn rejects_zero_max_buffered_body_bytes() {
        // A zero-byte cap fails every non-empty buffered publisher response at
        // request time, so it must be rejected at config validation instead of
        // silently breaking traffic.
        let result = Settings::from_toml(
            r#"
            [[handlers]]
            path = "^/_ts/admin"
            username = "admin"
            password = "admin-pass"

            [publisher]
            domain = "example.com"
            cookie_domain = ".example.com"
            origin_url = "https://origin.example.com"
            proxy_secret = "unit-test-proxy-secret"
            max_buffered_body_bytes = 0

            [ec]
            passphrase = "test-secret-key-32-bytes-minimum"
            "#,
        );
        assert!(
            result.is_err(),
            "publisher.max_buffered_body_bytes = 0 must fail config validation"
        );
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
            asset_routes: vec![],
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
            asset_routes: vec![],
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
            asset_routes: vec![],
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
            asset_routes: vec![],
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
            asset_routes: vec![],
        };
        proxy.normalize();
        assert!(
            proxy.allowed_domains.is_empty(),
            "all-blank list should normalize to empty (open mode)"
        );
    }

    #[test]
    fn proxy_normalize_trims_asset_routes() {
        let mut proxy = Proxy {
            certificate_check: true,
            allowed_domains: vec![],
            asset_routes: vec![ProxyAssetRoute {
                prefix: "  /.images/  ".to_string(),
                origin_url: "  https://assets.example.com  ".to_string(),
                ..Default::default()
            }],
        };
        proxy.normalize();
        assert_eq!(
            proxy.asset_routes[0].prefix, "/.images/",
            "should trim asset-route prefix"
        );
        assert_eq!(
            proxy.asset_routes[0].origin_url, "https://assets.example.com",
            "should trim asset-route origin_url"
        );
    }

    #[test]
    fn proxy_normalize_trims_asset_route_rewrite_fields() {
        let mut proxy = Proxy {
            certificate_check: true,
            allowed_domains: vec![],
            asset_routes: vec![ProxyAssetRoute {
                prefix: "/.images/".to_string(),
                origin_url: "https://assets.example.com".to_string(),
                path_pattern: Some("  ^/(.*)$  ".to_string()),
                target_path: Some("  /rewritten/$1  ".to_string()),
                ..Default::default()
            }],
        };
        proxy.normalize();

        assert_eq!(
            proxy.asset_routes[0].path_pattern.as_deref(),
            Some("^/(.*)$"),
            "should trim asset-route path_pattern"
        );
        assert_eq!(
            proxy.asset_routes[0].target_path.as_deref(),
            Some("/rewritten/$1"),
            "should trim asset-route target_path"
        );
    }

    #[test]
    fn proxy_asset_route_rewrite_fields_parse_from_toml() {
        let toml_str = crate_test_settings_str()
            + r#"
            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.image/"
            origin_url = "https://assets.example.com"
            path_pattern = "^/\\.image/(.*)/[^/]+\\.([^/.]+)$"
            target_path = "/image/upload/$1.$2"
            "#;
        let settings = Settings::from_toml(&toml_str).expect("should parse asset route rewrite");
        let route = settings
            .asset_route_for_path("/.image/options/id/example.jpg")
            .expect("should match configured asset route");

        assert_eq!(
            route.path_pattern.as_deref(),
            Some(r"^/\.image/(.*)/[^/]+\.([^/.]+)$"),
            "should preserve the configured rewrite pattern"
        );
        assert_eq!(
            route.target_path.as_deref(),
            Some("/image/upload/$1.$2"),
            "should preserve the configured replacement"
        );
    }

    #[test]
    fn proxy_asset_route_auth_and_image_optimizer_parse_from_toml() {
        let toml_str = crate_test_settings_str()
            + r#"
            [image_optimizer.profile_sets.default_images]
            base_params = "quality=70&resize-filter=bicubic"
            default_profile = "default"
            unknown_profile = "use_default"

            [image_optimizer.profile_sets.default_images.profiles]
            default = "width=1920"
            medium = "format=auto&width=828"

            [image_optimizer.profile_sets.default_images.aspect_ratios]
            allowed = ["1-1", "16-9"]
            profiles = ["medium"]

            [image_optimizer.profile_sets.default_images.crop_offsets]
            enabled = true
            buckets = [10, 30, 50, 70, 90]
            default = 50

            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.image/"
            origin_url = "https://bucket.s3.us-east-1.amazonaws.com"

            [proxy.asset_routes.auth]
            type = "s3_sigv4"
            region = "us-east-1"
            origin_query = "strip"

            [proxy.asset_routes.image_optimizer]
            enabled = true
            region = "us_east"
            profile_set = "default_images"
            "#;

        let settings = Settings::from_toml(&toml_str)
            .expect("should parse S3 auth and image optimizer asset route");
        let route = settings
            .asset_route_for_path("/.image/id/example.jpg")
            .expect("should match configured route");
        assert!(route.image_optimizer_enabled());
        assert_eq!(route.origin_query_policy(), OriginQueryPolicy::Strip);
        match route.auth.as_ref().expect("should configure route auth") {
            AssetOriginAuth::S3SigV4(config) => {
                assert_eq!(config.region, "us-east-1");
                assert_eq!(config.secret_store, "s3-auth");
                assert_eq!(config.access_key_id, "access_key_id");
                assert_eq!(config.secret_access_key, "secret_access_key");
            }
        }
    }

    #[test]
    fn proxy_asset_route_validation_rejects_s3_sigv4_http_origin_url() {
        let toml_str = crate_test_settings_str()
            + r#"
            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.images/"
            origin_url = "http://bucket.s3.us-east-1.amazonaws.com"

            [proxy.asset_routes.auth]
            type = "s3_sigv4"
            region = "us-east-1"
            "#;

        let err = Settings::from_toml(&toml_str)
            .expect_err("should reject cleartext S3 SigV4 origin URLs");

        assert!(
            format!("{err:?}").contains("must use https when auth type is s3_sigv4"),
            "should mention the S3 SigV4 HTTPS requirement: {err:?}"
        );
    }

    #[test]
    fn proxy_asset_route_validation_rejects_invalid_s3_regions() {
        for region in ["us east 1", "us/east/1", "US-EAST-1", "us-east-\\n1"] {
            let toml_str = crate_test_settings_str()
                + &format!(
                    r#"
            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.images/"
            origin_url = "https://bucket.s3.us-east-1.amazonaws.com"

            [proxy.asset_routes.auth]
            type = "s3_sigv4"
            region = "{region}"
            "#
                );

            let err = Settings::from_toml(&toml_str)
                .expect_err("should reject malformed S3 region values");

            assert!(
                format!("{err:?}").contains("region must contain only lowercase letters"),
                "should mention the S3 region character policy for {region:?}: {err:?}"
            );
        }
    }

    #[test]
    fn proxy_asset_route_validation_rejects_unknown_s3_auth_fields() {
        let toml_str = crate_test_settings_str()
            + r#"
            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.images/"
            origin_url = "https://bucket.s3.us-east-1.amazonaws.com"

            [proxy.asset_routes.auth]
            type = "s3_sigv4"
            region = "us-east-1"
            secret_access_key_name = "secret_access_key"
            "#;

        let err = Settings::from_toml(&toml_str)
            .expect_err("should reject unknown S3 auth config fields");

        assert!(
            format!("{err:?}").contains("secret_access_key_name"),
            "should mention the unknown S3 auth field: {err:?}"
        );
    }

    #[test]
    fn proxy_asset_route_validation_rejects_invalid_image_optimizer_regions() {
        let toml_str = crate_test_settings_str()
            + r#"
            [image_optimizer.profile_sets.default_images]
            base_params = "quality=70"
            default_profile = "default"

            [image_optimizer.profile_sets.default_images.profiles]
            default = "width=1920"

            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.image/"
            origin_url = "https://assets.example.com"

            [proxy.asset_routes.image_optimizer]
            enabled = true
            region = "us-east-2"
            profile_set = "default_images"
            "#;

        let err = Settings::from_toml(&toml_str)
            .expect_err("should reject unsupported Image Optimizer regions");

        assert!(
            format!("{err:?}").contains("image_optimizer region `us-east-2` is not supported"),
            "should mention the unsupported Image Optimizer region: {err:?}"
        );
    }

    #[test]
    fn image_optimizer_validation_rejects_unknown_aspect_ratio_profile() {
        let toml_str = crate_test_settings_str()
            + r#"
            [image_optimizer.profile_sets.default_images]
            default_profile = "default"

            [image_optimizer.profile_sets.default_images.profiles]
            default = "width=1920"

            [image_optimizer.profile_sets.default_images.aspect_ratios]
            allowed = ["1-1"]
            profiles = ["missing"]
            "#;

        let err = Settings::from_toml(&toml_str)
            .expect_err("should reject aspect-ratio profiles that are not defined");
        assert!(
            format!("{err:?}").contains("aspect ratio profile `missing` is not defined"),
            "should mention the unknown aspect-ratio profile: {err:?}"
        );
    }

    #[test]
    fn proxy_asset_route_image_optimizer_env_accepts_nested_bool_strings_and_arrays() {
        let toml_str = crate_test_settings_str();
        let separator = ENVIRONMENT_VARIABLE_SEPARATOR;
        let vars = [
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}PREFIX"),
                Some("/.image/"),
            ),
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}ORIGIN_URL"),
                Some("https://bucket.s3.us-west-2.amazonaws.com"),
            ),
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}AUTH{separator}TYPE"),
                Some("s3_sigv4"),
            ),
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}AUTH{separator}REGION"),
                Some("us-west-2"),
            ),
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}AUTH{separator}ORIGIN_QUERY"),
                Some("strip"),
            ),
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}IMAGE_OPTIMIZER{separator}ENABLED"),
                Some("true"),
            ),
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}IMAGE_OPTIMIZER{separator}REGION"),
                Some("us_west"),
            ),
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}IMAGE_OPTIMIZER{separator}PROFILE_SET"),
                Some("default_images"),
            ),
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}BASE_PARAMS"),
                Some("quality=70&resize-filter=bicubic"),
            ),
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}DEFAULT_PROFILE"),
                Some("w828"),
            ),
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}PROFILES{separator}W828"),
                Some("format=auto&width=828"),
            ),
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}PROFILES{separator}W1536"),
                Some("format=auto&width=1536"),
            ),
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}ASPECT_RATIOS{separator}ALLOWED"),
                Some("[\"1-1\",\"16-9\"]"),
            ),
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}ASPECT_RATIOS{separator}PROFILES"),
                Some("[\"w828\",\"w1536\"]"),
            ),
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}CROP_OFFSETS{separator}ENABLED"),
                Some("true"),
            ),
            (
                format!("{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}CROP_OFFSETS{separator}BUCKETS"),
                Some("[10,30,50,70,90]"),
            ),
        ];

        temp_env::with_vars(vars, || {
            let settings = Settings::from_toml_and_env(&toml_str)
                .expect("should parse image optimizer env overrides");
            let route = settings
                .asset_route_for_path("/.image/id/example.jpg")
                .expect("should match image optimizer asset route");
            assert!(route.image_optimizer_enabled());

            let image_optimizer = route
                .image_optimizer
                .as_ref()
                .expect("should configure image optimizer");
            assert!(image_optimizer.enabled);
            assert_eq!(image_optimizer.region, "us_west");
            assert_eq!(image_optimizer.profile_set, "default_images");

            let profile_set = settings
                .image_optimizer
                .profile_sets
                .get("default_images")
                .expect("should configure default image profiles");
            assert_eq!(profile_set.profiles["w828"], "format=auto&width=828");
            let aspect_ratios = profile_set
                .aspect_ratios
                .as_ref()
                .expect("should configure aspect ratios");
            assert_eq!(aspect_ratios.allowed, vec!["1-1", "16-9"]);
            assert_eq!(aspect_ratios.profiles, vec!["w828", "w1536"]);
            let crop_offsets = profile_set
                .crop_offsets
                .as_ref()
                .expect("should configure crop offsets");
            assert!(crop_offsets.enabled);
            assert_eq!(crop_offsets.buckets, vec![10, 30, 50, 70, 90]);
        });
    }

    #[test]
    fn proxy_asset_route_validation_rejects_image_optimizer_preserve_query() {
        let toml_str = crate_test_settings_str()
            + r#"
            [image_optimizer.profile_sets.default_images]
            base_params = "quality=70"

            [image_optimizer.profile_sets.default_images.profiles]
            default = "width=1920"

            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.image/"
            origin_url = "https://bucket.s3.us-east-1.amazonaws.com"

            [proxy.asset_routes.image_optimizer]
            enabled = true
            region = "us_east"
            profile_set = "default_images"
            origin_query = "preserve"
            "#;
        let err = Settings::from_toml(&toml_str)
            .expect_err("should reject preserving arbitrary client query with IO enabled");

        assert!(
            format!("{err:?}")
                .contains("cannot preserve origin query while image_optimizer is enabled"),
            "should mention the rejected IO origin query policy: {err:?}"
        );
    }

    #[test]
    fn proxy_asset_route_disabled_image_optimizer_does_not_override_origin_query_policy() {
        let route = ProxyAssetRoute {
            prefix: "/.image/".to_string(),
            origin_url: "https://assets.example.com".to_string(),
            image_optimizer: Some(AssetImageOptimizerConfig {
                enabled: false,
                region: "us_east".to_string(),
                profile_set: "default_images".to_string(),
                origin_query: Some(OriginQueryPolicy::Strip),
            }),
            ..Default::default()
        };

        assert_eq!(route.origin_query_policy(), OriginQueryPolicy::Preserve);
    }

    #[test]
    fn proxy_asset_route_validation_rejects_incomplete_rewrite() {
        let toml_str = crate_test_settings_str()
            + r#"
            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.image/"
            origin_url = "https://assets.example.com"
            path_pattern = "^/\\.image/(.*)$"
            "#;
        let err = Settings::from_toml(&toml_str)
            .expect_err("should reject incomplete asset route rewrite");

        assert!(
            format!("{err:?}").contains("must configure path_pattern and target_path together"),
            "should mention the incomplete rewrite configuration: {err:?}"
        );
    }

    #[test]
    fn proxy_asset_route_validation_rejects_invalid_path_pattern() {
        let toml_str = crate_test_settings_str()
            + r#"
            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.image/"
            origin_url = "https://assets.example.com"
            path_pattern = "["
            target_path = "/image/upload/$1"
            "#;
        let err = Settings::from_toml(&toml_str)
            .expect_err("should reject invalid asset route path_pattern");

        assert!(
            format!("{err:?}").contains("failed to compile"),
            "should mention the invalid regex: {err:?}"
        );
    }

    #[test]
    fn proxy_asset_route_for_path_prefers_longest_prefix() {
        let proxy = Proxy {
            certificate_check: true,
            allowed_domains: vec![],
            asset_routes: vec![
                ProxyAssetRoute {
                    prefix: "/.images/".to_string(),
                    origin_url: "https://a.example.com".to_string(),
                    ..Default::default()
                },
                ProxyAssetRoute {
                    prefix: "/.images/special/".to_string(),
                    origin_url: "https://b.example.com".to_string(),
                    ..Default::default()
                },
            ],
        };

        let route = proxy
            .asset_route_for_path("/.images/special/banner.png")
            .expect("should match a configured asset route");
        assert_eq!(
            route.origin_url, "https://b.example.com",
            "should prefer the most specific prefix"
        );
    }

    #[test]
    fn proxy_asset_route_for_path_keeps_first_duplicate_prefix() {
        let proxy = Proxy {
            certificate_check: true,
            allowed_domains: vec![],
            asset_routes: vec![
                ProxyAssetRoute {
                    prefix: "/.images/".to_string(),
                    origin_url: "https://first.example.com".to_string(),
                    ..Default::default()
                },
                ProxyAssetRoute {
                    prefix: "/.images/".to_string(),
                    origin_url: "https://second.example.com".to_string(),
                    ..Default::default()
                },
            ],
        };

        let route = proxy
            .asset_route_for_path("/.images/banner.png")
            .expect("should match duplicate prefixes deterministically");
        assert_eq!(
            route.origin_url, "https://first.example.com",
            "should keep the first configured duplicate prefix"
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
    fn proxy_asset_route_validation_rejects_prefix_without_leading_slash() {
        let toml_str = crate_test_settings_str()
            + r#"
            [proxy]

            [[proxy.asset_routes]]
            prefix = ".images/"
            origin_url = "https://assets.example.com"
            "#;
        let err =
            Settings::from_toml(&toml_str).expect_err("should reject invalid asset-route prefix");
        assert!(
            format!("{err:?}").contains("asset-route prefix must start with '/'"),
            "should mention the prefix validation failure: {err:?}"
        );
    }

    #[test]
    fn proxy_asset_route_validation_rejects_non_http_origin_url() {
        let toml_str = crate_test_settings_str()
            + r#"
            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.images/"
            origin_url = "ftp://assets.example.com"
            "#;
        let err = Settings::from_toml(&toml_str)
            .expect_err("should reject non-http asset-route origin_url");
        assert!(
            format!("{err:?}").contains("origin_url must use http or https"),
            "should mention the origin_url validation failure: {err:?}"
        );
    }

    #[test]
    fn proxy_asset_route_validation_rejects_origin_url_path() {
        let toml_str = crate_test_settings_str()
            + r#"
            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.images/"
            origin_url = "https://assets.example.com/api"
            "#;
        let err = Settings::from_toml(&toml_str)
            .expect_err("should reject asset-route origin_url with path");
        assert!(
            format!("{err:?}").contains("origin_url must not include a path"),
            "should mention the origin_url path validation failure: {err:?}"
        );
    }

    #[test]
    fn proxy_asset_route_validation_rejects_origin_url_query() {
        let toml_str = crate_test_settings_str()
            + r#"
            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.images/"
            origin_url = "https://assets.example.com?token=abc"
            "#;
        let err = Settings::from_toml(&toml_str)
            .expect_err("should reject asset-route origin_url with query");
        assert!(
            format!("{err:?}").contains("origin_url must not include a query string"),
            "should mention the origin_url query validation failure: {err:?}"
        );
    }

    #[test]
    fn proxy_asset_route_validation_rejects_origin_url_userinfo() {
        let toml_str = crate_test_settings_str()
            + r#"
            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.images/"
            origin_url = "https://user:pass@assets.example.com"
            "#;
        let err = Settings::from_toml(&toml_str)
            .expect_err("should reject asset-route origin_url with userinfo");
        assert!(
            format!("{err:?}").contains("origin_url must not include username or password"),
            "should mention the origin_url userinfo validation failure: {err:?}"
        );
    }

    #[test]
    fn proxy_asset_route_validation_rejects_origin_url_fragment() {
        let toml_str = crate_test_settings_str()
            + r#"
            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.images/"
            origin_url = "https://assets.example.com#fragment"
            "#;
        let err = Settings::from_toml(&toml_str)
            .expect_err("should reject asset-route origin_url with fragment");
        assert!(
            format!("{err:?}").contains("origin_url must not include a fragment"),
            "should mention the origin_url fragment validation failure: {err:?}"
        );
    }

    #[test]
    fn proxy_asset_route_validation_accepts_origin_url_host_and_port() {
        let toml_str = crate_test_settings_str()
            + r#"
            [proxy]

            [[proxy.asset_routes]]
            prefix = "/.images/"
            origin_url = "https://assets.example.com:8443"
            "#;
        let settings =
            Settings::from_toml(&toml_str).expect("should accept asset-route origin host and port");
        assert_eq!(
            settings.proxy.asset_routes[0].origin_url, "https://assets.example.com:8443",
            "should preserve valid origin URL with non-standard port"
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
            passphrase = "test-secret-key-32-bytes-minimum"

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
            vec!["/_ts/admin/keys/rotate", "/_ts/admin/keys/deactivate"],
            "should report every admin endpoint as uncovered"
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
            "should detect the admin endpoints not covered by the narrow handler"
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
    fn from_toml_rejects_admin_handler_placeholder_password() {
        let toml_str = crate_test_settings_str()
            .replace(r#"password = "admin-pass""#, r#"password = "changeme""#);

        let result = Settings::from_toml(&toml_str);
        assert!(
            result.is_err(),
            "should reject placeholder password on admin handler"
        );
        let err = format!("{:?}", result.expect_err("should reject placeholder"));
        assert!(
            err.contains("placeholder password"),
            "error should mention placeholder admin password, got: {err}"
        );
    }

    #[test]
    fn from_toml_accepts_non_placeholder_admin_password() {
        let settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should accept non-placeholder admin password");
        assert_eq!(settings.handlers.len(), 2, "should parse handlers");
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
