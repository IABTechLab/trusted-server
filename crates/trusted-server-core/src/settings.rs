#[cfg(test)]
use config::{Config, Environment, File, FileFormat};
use error_stack::{Report, ResultExt};
use glob::{MatchOptions, Pattern};
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::Value as JsonValue;
use sha2::{Digest as _, Sha256};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::Duration;
use subtle::ConstantTimeEq as _;
use url::Url;
use validator::{Validate, ValidationError, ValidationErrors, ValidationErrorsKind};

use crate::auction_config_types::AuctionConfig;
use crate::cache_policy::{CachePolicy, CacheVisibility};
use crate::consent_config::ConsentConfig;
use crate::constants::INTERNAL_HEADERS;
use crate::creative_opportunities::CreativeOpportunitiesConfig;
use crate::ec::module::{
    EcModuleSelection, HMAC_MODULE_KEY, HOST_SIGNALS_MODULE_KEY, check_named_module_configuration,
};
use crate::error::TrustedServerError;
use crate::host_header::validate_host_header_override_value;
use crate::middleware::{MiddlewarePhase, PhaseEntries};
use crate::platform::PlatformImageOptimizerRegion;
use crate::provider_table::{ProviderChoice, ProviderList, SectionModules};
use crate::redacted::Redacted;

#[cfg(test)]
pub const ENVIRONMENT_VARIABLE_PREFIX: &str = "TRUSTED_SERVER";
#[cfg(test)]
pub const ENVIRONMENT_VARIABLE_SEPARATOR: &str = "__";
/// Build inputs that share the settings prefix but are not settings.
///
/// The core build script exports these with `cargo:rustc-env`, and Cargo sets
/// them on every `cargo test` run of this crate.
#[cfg(test)]
const BUILD_ENVIRONMENT_VARIABLES: [&str; 1] = ["TRUSTED_SERVER__GIT_VERSION"];

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct Publisher {
    #[validate(custom(function = validate_publisher_domain))]
    pub domain: String,
    /// Domain for non-EC cookies. EC cookies use a separate computed domain
    /// (see [`ec_cookie_domain`](Self::ec_cookie_domain)).
    #[validate(custom(function = validate_cookie_domain))]
    pub cookie_domain: String,
    #[serde(serialize_with = "crate::redacted::sensitive")]
    #[validate(custom(function = validate_no_trailing_slash))]
    pub origin_url: String,
    /// Optional outbound Host header to send while connecting to `origin_url`.
    #[serde(default, serialize_with = "crate::redacted::sensitive")]
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
    /// Fastly origin bodies are preserved as streams on the publisher path, so
    /// this setting also caps the streaming pipeline twice over: cumulative
    /// raw (still compressed) bytes pulled from origin, and cumulative decoded
    /// bytes emitted by the decompressor — the latter so a decompression bomb
    /// cannot push an unbounded decoded volume through the rewrite pipeline.
    /// On the streaming path headers are already committed when either cap
    /// trips, so the response is truncated mid-body (with the error logged)
    /// rather than replaced with a 5xx.
    ///
    /// Buffered adapters keep using it as the post-rewrite output buffer cap.
    /// There it additionally bounds how much decoded gzip output may sit in the
    /// heap at once, so a bomb is rejected mid-decode instead of after its full
    /// expansion; that bound is per-step, never cumulative, so a gzip-encoded
    /// body is judged by the same post-rewrite total as an identity, deflate or
    /// brotli one.
    ///
    /// Must be at least 1: a zero-byte cap fails every non-empty buffered
    /// publisher response at request time, so it is rejected at config
    /// validation instead.
    #[serde(default = "default_max_buffered_body_bytes")]
    #[validate(range(min = 1, message = "must be at least 1 byte"))]
    pub max_buffered_body_bytes: usize,
    /// Optional signing of publisher origin reads, taking an asset route's `auth` table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_auth: Option<AssetOriginAuth>,
    /// Optional file name added to a request path that ends in `/` on the way
    /// to the origin, for an origin such as object storage that does not answer
    /// a folder with its index file. The reader's address is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(custom(function = validate_directory_index))]
    pub directory_index: Option<String>,
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
            origin_auth: None,
            directory_index: None,
        }
    }
}

impl Publisher {
    /// Known placeholder values that must not be used in production.
    pub const PROXY_SECRET_PLACEHOLDERS: &[&str] = &[
        "change-me-proxy-secret",
        "proxy-secret",
        "replace-with-random-proxy-secret",
    ];

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

    /// Reserved example publisher values copied verbatim from the config
    /// template. They deserialize fine but must be replaced before deploying.
    const PLACEHOLDER_DOMAINS: &[&str] = &["example.com"];
    const PLACEHOLDER_COOKIE_DOMAINS: &[&str] = &[".example.com"];
    /// Reserved example origin hosts. Matched against the parsed URL host so a
    /// spelling that resolves to the same host (an explicit `:443`, a trailing
    /// slash, a different scheme) cannot slip past the placeholder check.
    const PLACEHOLDER_ORIGIN_HOSTS: &[&str] = &["origin.example.com"];

    /// Returns `true` if `domain` is the unedited template placeholder
    /// (case-insensitive).
    #[must_use]
    pub fn is_placeholder_domain(domain: &str) -> bool {
        Self::PLACEHOLDER_DOMAINS
            .iter()
            .any(|p| p.eq_ignore_ascii_case(domain.trim()))
    }

    /// Returns `true` if `cookie_domain` is the unedited template placeholder
    /// (case-insensitive).
    #[must_use]
    pub fn is_placeholder_cookie_domain(cookie_domain: &str) -> bool {
        Self::PLACEHOLDER_COOKIE_DOMAINS
            .iter()
            .any(|p| p.eq_ignore_ascii_case(cookie_domain.trim()))
    }

    /// Returns `true` if `origin_url` resolves to an unedited template
    /// placeholder host (case-insensitive).
    ///
    /// The comparison is on the parsed URL host, not the raw string, so
    /// equivalent spellings of the reserved host - an explicit default port, a
    /// trailing slash, or a different scheme - are all rejected.
    #[must_use]
    pub fn is_placeholder_origin_url(origin_url: &str) -> bool {
        Url::parse(origin_url.trim())
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .is_some_and(|host| {
                Self::PLACEHOLDER_ORIGIN_HOSTS
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case(&host))
            })
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
    ///     origin_auth: None,
    ///     directory_index: None,
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

    /// Returns `true` when [`Self::origin_auth`] says the reader's query is
    /// left off origin requests, as an asset route's `origin_query` does.
    #[must_use]
    pub fn origin_query_is_stripped(&self) -> bool {
        self.origin_auth
            .as_ref()
            .and_then(AssetOriginAuth::origin_query_policy)
            == Some(OriginQueryPolicy::Strip)
    }

    fn normalize(&mut self) {
        if let Some(auth) = &mut self.origin_auth {
            auth.normalize();
        }
    }

    /// Refuses an `origin_auth` the signer cannot use, naming the setting.
    ///
    /// As for an asset route, a signed origin must use `https`.
    fn prepare_runtime(&self) -> Result<(), Report<TrustedServerError>> {
        let Some(auth) = &self.origin_auth else {
            return Ok(());
        };
        let uses_https = Url::parse(&self.origin_url).is_ok_and(|url| url.scheme() == "https");
        if !uses_https {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "{PUBLISHER_ORIGIN_AUTH_SETTING} requires publisher.origin_url to use https"
                ),
            }));
        }
        auth.prepare_runtime(PUBLISHER_ORIGIN_AUTH_SETTING)
    }
}

/// The setting named in every refusal of `[publisher]`'s `origin_auth`.
const PUBLISHER_ORIGIN_AUTH_SETTING: &str = "publisher.origin_auth";

/// The setting named in every refusal of an asset route's `auth`.
const ASSET_ROUTE_AUTH_SETTING: &str = "proxy.asset_routes auth";

/// Which integrations run, and the settings each one is given.
///
/// The sections of module types core does not read itself, such as `[cmp]` or
/// `[tag]`, each named for the type of module it selects.
///
/// Every top-level table that is not one of Trusted Server's own settings is
/// read as one of these. Whether its type is one a module on offer has is
/// only known where the registry is built, so it is checked there.
#[derive(Clone, Default, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TypeSections(BTreeMap<String, SectionModules>);

impl std::fmt::Debug for TypeSections {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_map().entries(self.0.iter()).finish()
    }
}

impl<'de> Deserialize<'de> for TypeSections {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let entries = serde_json::Map::<String, JsonValue>::deserialize(deserializer)?;
        let mut sections = BTreeMap::new();
        for (name, value) in entries {
            let JsonValue::Object(table) = value else {
                return Err(serde::de::Error::custom(format!(
                    "unknown field `{name}`, which is neither a setting Trusted Server reads nor \
                     the section of a module type, which is a table"
                )));
            };
            if name.contains('.') || !crate::module_name::is_valid(&name) {
                return Err(serde::de::Error::custom(format!(
                    "unknown field `{name}`. The section of a module type is named for \
                     the type, in lower case letters, digits, `_` or `-`"
                )));
            }
            let section = SectionModules::from_entries(table).map_err(|message| {
                serde::de::Error::custom(format!(
                    "[{name}] is not a section Trusted Server reads itself, so it is read as \
                     the section of a module type, and it {message}"
                ))
            })?;
            sections.insert(name, section);
        }
        Ok(Self(sections))
    }
}

impl TypeSections {
    /// Whether no type section is present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Each type section, by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &SectionModules)> {
        self.0
            .iter()
            .map(|(name, section)| (name.as_str(), section))
    }

    /// The section of `type_name`, when present.
    #[must_use]
    pub fn section(&self, type_name: &str) -> Option<&SectionModules> {
        self.0.get(type_name)
    }

    /// The section of `type_name`, created empty when absent.
    pub fn section_mut(&mut self, type_name: &str) -> &mut SectionModules {
        self.0.entry(type_name.to_owned()).or_default()
    }

    /// Refuses a section that selects nothing, and checks each selection.
    ///
    /// # Errors
    ///
    /// Naming the first section at fault.
    pub fn validate(&self) -> Result<(), Report<TrustedServerError>> {
        for (name, section) in &self.0 {
            if section.selected().is_empty() {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "[{name}] selects no module. A module type's section names what runs \
                         with `module` or `modules`, so select one or remove the section"
                    ),
                }));
            }
            section
                .validate(name)
                .map_err(|message| Report::new(TrustedServerError::Configuration { message }))?;
        }
        Ok(())
    }
}

/// The section that holds what one vendor's modules share.
pub const MIDDLEWARE_TYPE: &str = "middleware";

/// The `[middleware]` section, one `[middleware.<name>]` table for each
/// middleware that has settings of its own, written with the middleware's
/// full name, so `[middleware.example.cleanup]` is the `example.cleanup`
/// middleware's table. A table holds values and arrays. A table within one
/// is the table of the longer name it spells and never a setting, so a
/// middleware never sees another's settings. Core reads none of a table. A
/// middleware reads its own through [`Settings::middleware_config`].
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct MiddlewareTables(BTreeMap<String, JsonValue>);

impl std::fmt::Debug for MiddlewareTables {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MiddlewareTables")
            .field("names", &self.names())
            .finish()
    }
}

impl MiddlewareTables {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Every name a table stands at, the ones on the way to a deeper table
    /// included, so `[middleware.example.cleanup]` gives `example` and
    /// `example.cleanup`.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        fn walk(prefix: &str, table: &serde_json::Map<String, JsonValue>, names: &mut Vec<String>) {
            for (key, child) in table {
                if let JsonValue::Object(inner) = child {
                    let name = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    names.push(name.clone());
                    walk(&name, inner, names);
                }
            }
        }
        let mut names = Vec::new();
        for (key, value) in &self.0 {
            if let JsonValue::Object(table) = value {
                names.push(key.clone());
                walk(key, table, &mut names);
            }
        }
        names
    }

    /// The table at `name`, with the tables within it left out, because each
    /// of those is a longer name's table.
    #[must_use]
    pub fn table(&self, name: &str) -> Option<serde_json::Map<String, JsonValue>> {
        let mut parts = name.split('.');
        let mut node = self.0.get(parts.next()?)?;
        for part in parts {
            node = node.as_object()?.get(part)?;
        }
        let table = node.as_object()?;
        Some(
            table
                .iter()
                .filter(|(_, value)| !value.is_object())
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        )
    }

    /// Reads and validates `[middleware.<name>]` as `T`, or `None` when no
    /// table stands at `name`.
    ///
    /// # Errors
    ///
    /// When the table carries a key `T` does not know or a value that fails
    /// validation.
    pub fn get_typed<T>(&self, name: &str) -> Result<Option<T>, Report<TrustedServerError>>
    where
        T: DeserializeOwned + Validate,
    {
        let Some(table) = self.table(name) else {
            return Ok(None);
        };
        let config: T = serde_json::from_value(JsonValue::Object(table)).map_err(|error| {
            Report::new(TrustedServerError::Configuration {
                message: format!("[{MIDDLEWARE_TYPE}.{name}] could not be read: {error}"),
            })
        })?;
        config.validate().map_err(|error| {
            Report::new(TrustedServerError::Configuration {
                message: format!("[{MIDDLEWARE_TYPE}.{name}] failed validation: {error}"),
            })
        })?;
        Ok(Some(config))
    }

    /// Refuses a table at a name no middleware could have, and a value where
    /// only tables stand.
    ///
    /// # Errors
    ///
    /// With the key or the name that is wrong.
    pub fn validate_names(&self) -> Result<(), Report<TrustedServerError>> {
        if let Some((key, _)) = self.0.iter().find(|(_, value)| !value.is_object()) {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[{MIDDLEWARE_TYPE}] has a value `{key}` where only tables stand, one \
                     for each middleware, as [{MIDDLEWARE_TYPE}.<name>]"
                ),
            }));
        }
        if let Some(name) = self
            .names()
            .into_iter()
            .find(|name| !crate::module_name::is_valid(name))
        {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[{MIDDLEWARE_TYPE}] has a table named `{name}`, which is not a \
                     middleware's name. A name is parts joined by `.`, each in lower case \
                     letters, digits, `_` or `-`"
                ),
            }));
        }
        Ok(())
    }
}

pub const SHARED_TYPE: &str = "shared";

/// The `[shared]` section, one `[shared.<vendor>]` table per vendor, holding
/// the settings every module of that vendor reads, such as an endpoint and a
/// license key, read through [`Settings::shared_config`].
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct SharedTables(BTreeMap<String, serde_json::Map<String, JsonValue>>);

impl std::fmt::Debug for SharedTables {
    // The tables hold secrets, so only the vendors are shown.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SharedTables")
            .field("vendors", &self.vendors().collect::<Vec<_>>())
            .finish()
    }
}

impl SharedTables {
    /// Whether no vendor has a table.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The vendors with a table, in name order.
    pub fn vendors(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    /// Whether `[shared.<vendor>]` is present.
    #[must_use]
    pub fn contains(&self, vendor: &str) -> bool {
        self.0.contains_key(vendor)
    }

    /// `[shared.<vendor>]` as written, for a check that reads a setting by its
    /// path, such as that a secret setting names a key.
    #[must_use]
    pub(crate) fn table(&self, vendor: &str) -> Option<&serde_json::Map<String, JsonValue>> {
        self.0.get(vendor)
    }

    /// Reads and validates `[shared.<vendor>]` as `T`, or `None` when the table
    /// is absent.
    ///
    /// # Errors
    ///
    /// When the table cannot be read as `T` or fails its validation, naming
    /// the table and the setting at fault but never the setting's value.
    pub fn get_typed<T>(&self, vendor: &str) -> Result<Option<T>, Report<TrustedServerError>>
    where
        T: DeserializeOwned + Validate,
    {
        let Some(table) = self.0.get(vendor) else {
            return Ok(None);
        };
        read_table(&format!("{SHARED_TYPE}.{vendor}"), table.clone()).map(Some)
    }

    /// Refuses a table whose name is not a vendor's name, being lower case
    /// letters, digits, `_` or `-`.
    ///
    /// # Errors
    ///
    /// Naming the first table at fault.
    pub fn validate_names(&self) -> Result<(), Report<TrustedServerError>> {
        match self
            .vendors()
            .find(|vendor| vendor.contains('.') || !crate::module_name::is_valid(vendor))
        {
            Some(vendor) => Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[{SHARED_TYPE}] has a table named `{vendor}`, which is not a vendor's \
                     name. A vendor's shared table is named in lower case letters, \
                     digits, `_` or `-`"
                ),
            })),
            None => Ok(()),
        }
    }
}

/// Reads and validates a table as `T`, where `place` is the table's name as
/// written, such as `shared.example`.
///
/// A message names the setting at fault and the rule it broke, and never
/// quotes the setting's value, because at startup a table holds each of its
/// secrets resolved from the secret store.
fn read_table<T>(
    place: &str,
    table: serde_json::Map<String, JsonValue>,
) -> Result<T, Report<TrustedServerError>>
where
    T: DeserializeOwned + Validate,
{
    let config: T =
        serde_path_to_error::deserialize(JsonValue::Object(table)).map_err(|error| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[{place}] could not be read: {}",
                    read_error_summary(&error)
                ),
            })
        })?;
    config.validate().map_err(|errors| {
        Report::new(TrustedServerError::Configuration {
            message: format!(
                "[{place}] failed validation: {}",
                validation_error_summary(&errors)
            ),
        })
    })?;
    Ok(config)
}

/// Why a table could not be read, by the path of the setting at fault.
///
/// `serde` quotes the value it could not read, so its message is kept only
/// when it names keys alone: a key missing, unknown or written twice.
fn read_error_summary(error: &serde_path_to_error::Error<serde_json::Error>) -> String {
    let message = error.inner().to_string();
    let names_keys_only = ["missing field `", "unknown field `", "duplicate field `"]
        .iter()
        .any(|start| message.starts_with(start));
    let path = error.path().to_string();
    match (path.as_str(), names_keys_only) {
        (".", true) => message,
        (".", false) => "a setting holds a value of the wrong type or form".to_owned(),
        (path, true) => format!("{path}: {message}"),
        (path, false) => format!("`{path}` holds a value of the wrong type or form"),
    }
}

/// The settings type a module reads from its table, beneath the section that
/// selects it.
///
/// The type states which settings the module takes and how they are
/// validated, and nothing else. Whether the module runs is not its business,
/// because the section's selection names what runs.
pub trait IntegrationConfig: DeserializeOwned + Validate {}

/// A partner (SSP, DSP, identity vendor) configured in `[[ec.partners]]`.
///
/// Partners are defined statically in `trusted-server.toml` rather than
/// registered via API. At startup, each configured `api_token` is hashed
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
    /// `OpenRTB` `atype` value, including vendor-specific values such as PAIR's `571187`.
    #[serde(
        default = "EcPartner::default_openrtb_atype",
        deserialize_with = "from_value_or_str"
    )]
    #[validate(range(min = 0, message = "must be a non-negative OpenRTB agent type"))]
    pub openrtb_atype: i32,
    /// Whether this partner's UIDs appear in auction `user.eids`.
    #[serde(default, deserialize_with = "from_value_or_str")]
    pub bidstream_enabled: bool,
    /// Plaintext API token used by inbound batch sync and identify requests.
    ///
    /// When present, the token is hashed at startup for auth lookups. Omitting
    /// it disables inbound partner API authentication for this partner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_token: Option<Redacted<String>>,
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
    /// Known partner secret placeholders (`api_token` and `ts_pull_token`) that
    /// must not be used in deployments.
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
    pub const fn default_openrtb_atype() -> i32 {
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
///
/// Every key in the section other than the fields below is one module's
/// `[ec.<name>]` settings table, held in
/// [`module_blocks`](Self::module_blocks), so the field names below are
/// reserved and cannot name a module.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct Ec {
    /// The name of the Edge Cookie identity module to activate.
    ///
    /// Set it in the `[ec]` TOML section, for example `"hmac"`. The name is
    /// the module's implementation unless its `[ec.<name>]` block names a
    /// different one, in which case the name is a label of the operator's
    /// choosing (see [`module_blocks`](Self::module_blocks)). Deployment
    /// tooling can merge a `TRUSTED_SERVER__EC__MODULE` environment value
    /// into the published configuration before it is loaded, so the same
    /// compiled WebAssembly can switch modules at deployment. The running
    /// server reads its settings from the platform config store, not the
    /// environment. When absent, no Edge Cookie is generated and Trusted
    /// Server runs statelessly, and the explicit `"none"` spells the same
    /// choice. A selection whose implementation needs settings it has no block
    /// for is rejected at startup by
    /// [`validate_module_selection`](Self::validate_module_selection).
    ///
    /// Typed as [`EcModuleSelection`], which reads and writes the same
    /// string, so every check that asks which module is selected matches on
    /// one vocabulary rather than comparing string literals.
    #[serde(default)]
    pub module: Option<EcModuleSelection>,

    /// Deprecated location of the HMAC passphrase, read so a configuration
    /// written for the previous release still starts.
    ///
    /// [`migrate_legacy_ec_layout`](Self::migrate_legacy_ec_layout) maps it
    /// to `module = "hmac"` with the passphrase in the `[ec.hmac]` block and
    /// logs a deprecation warning, so a fleet can move configuration and
    /// binaries independently. A configuration carrying both the old and the
    /// new form is rejected rather than guessed at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passphrase: Option<Redacted<String>>,

    /// Extra exact origins allowed to POST the client resolve endpoint.
    ///
    /// The endpoint always accepts `https://{publisher.domain}` and nothing
    /// else by default. A publisher whose pages are served from another origin,
    /// `www` being the common case, lists those origins here. Each entry is a
    /// serialized origin (RFC 6454 §6.1), being `http://` or `https://` and a
    /// host with an optional port and nothing after it, and
    /// [`validate_resolve_allowed_origins`](Self::validate_resolve_allowed_origins)
    /// refuses any other entry when the settings load. Each is compared with
    /// the request's `Origin` by the same-origin test of RFC 6454 §5, so the
    /// scheme, the host and the port all have to match, with a missing port
    /// meaning the scheme's default. A suffix or subdomain match is never
    /// performed, because control of a DNS namespace does not make every host
    /// under it a trusted identity-setting origin.
    #[serde(default)]
    pub resolve_allowed_origins: Vec<String>,

    /// Fastly KV store name for the EC identity graph.
    #[serde(default, serialize_with = "crate::redacted::sensitive")]
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
    pub partners: Vec<EcPartner>,

    /// The settings tables of the Edge Cookie identity modules, keyed by the
    /// name each is written under.
    ///
    /// A module has a block only when it has settings of its own, so
    /// `[ec] module = "hmac"` needs an `[ec.hmac]` block for its required
    /// passphrase while a module with no settings needs none. The
    /// [`module`](Self::module) selector names the active module and
    /// [`validate_module_selection`](Self::validate_module_selection)
    /// rejects a block it does not name, so at most one block survives
    /// startup.
    #[serde(flatten)]
    pub module_blocks: EcModuleBlocks,
}

impl Ec {
    /// Known placeholder values that must not be used in production.
    pub const PASSPHRASE_PLACEHOLDERS: &[&str] = &[
        "secret-key",
        "secret_key",
        "trusted-server",
        "trusted-server-placeholder-secret",
        "replace-with-random-ec-passphrase",
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

    /// Refuses a [`resolve_allowed_origins`](Self::resolve_allowed_origins)
    /// entry that is not a serialized origin.
    ///
    /// The resolve endpoint reads each entry with
    /// [`parse_serialized_origin`](crate::ec::resolve::parse_serialized_origin),
    /// and an entry that parse rejects matches no request, so every resolve
    /// from the origin it was meant to allow would answer `403`. Checking with
    /// the same parse here means an entry that loads is one a request can
    /// match.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] naming the first entry
    /// that is not a serialized origin and what is wrong with it.
    pub fn validate_resolve_allowed_origins(&self) -> Result<(), Report<TrustedServerError>> {
        for entry in &self.resolve_allowed_origins {
            if let Err(reason) = crate::ec::resolve::parse_serialized_origin(entry) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "[ec] resolve_allowed_origins entry `{entry}` {reason}. \
                         Each entry is a bare origin, being `http://` or `https://` and a \
                         host with an optional port, such as `https://www.example.com`"
                    ),
                }));
            }
        }
        Ok(())
    }

    /// Validates the module selection against the configured blocks.
    ///
    /// When [`module`](Self::module) is set, the selection has to resolve
    /// to an implementation this deployment can configure, and every block in
    /// [`module_blocks`](Self::module_blocks) has to be the one the
    /// selector names, so a deployment that selects a module (in TOML or via
    /// the environment override) but has not configured it fails fast at
    /// startup rather than silently running stateless. When no module is
    /// selected, Trusted Server runs statelessly and this check passes.
    ///
    /// Whether the named implementation exists at all is settled by
    /// [`build_module`](crate::ec::module::build_module), which is the
    /// one place that knows both the implementations built into core and the
    /// one this deployment's adapter injects.
    ///
    /// Whether this build compiles an implementation in at all is answered by
    /// `check_named_module_configuration` in [`crate::ec::module`], beside
    /// the resolution it belongs to, rather than here, because the settings
    /// cannot know which implementations are compiled out of this build. Only
    /// the resolution knows that.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] when a module name or
    /// implementation is not a module name, when a block is configured with no
    /// selector or alongside `"none"`, when the selector names a key `[ec]`
    /// reads as its own setting, when the selected implementation is not
    /// compiled into this build, when a block the selector does not name is
    /// configured, or when the selected module resolves to an implementation
    /// that needs settings and has no block.
    pub fn validate_module_selection(&self) -> Result<(), Report<TrustedServerError>> {
        for (name, block) in self.module_blocks.iter() {
            Self::validate_module_name(name)?;
            if let Some(implementation) = &block.implementation {
                Self::validate_module_name(implementation)?;
            }
        }

        let Some(selection) = self.module.as_ref() else {
            if !self.module_blocks.is_empty() {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: "[ec.<name>] module blocks are configured but no [ec] module \
                              is selected. Set [ec] module = \"<name>\" to activate one, or \
                              remove the blocks to run statelessly"
                        .to_owned(),
                }));
            }
            return Ok(());
        };

        // `"none"` is explicit statelessness, the same meaning as omitting the
        // selector, spelled out. It is subject to the same rule that no
        // module blocks may be left configured.
        let EcModuleSelection::Named(name) = selection else {
            if !self.module_blocks.is_empty() {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: "[ec] module = \"none\" selects stateless operation, but \
                              [ec.<name>] module blocks are configured. Remove the blocks, \
                              or select the module they configure"
                        .to_owned(),
                }));
            }
            return Ok(());
        };

        let name = name.as_str();
        Self::validate_module_name(name)?;
        if EC_SECTION_KEYS.contains(&name) || name == REMOVED_EC_PROVIDERS_TABLE {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[ec] module = \"{name}\" names a key the `[ec]` section reads as its \
                     own setting, so no module can be configured under it. Give the \
                     module a name of its own"
                ),
            }));
        }

        // Whether this build compiles the implementation in at all is the
        // resolution's question, not the settings', so it is asked there.
        let implementation = self.module_blocks.implementation(name);
        check_named_module_configuration(implementation)?;

        // A module has a block only when it has settings, and core knows
        // which of its own implementations need them. Both modules that
        // derive an identifier at the edge take a passphrase, so both need one.
        // The demonstration module is built from nothing and needs none. A
        // module an adapter injects has the contents of its block read by
        // that adapter when it builds the module, so core cannot say whether
        // it needs one.
        if (implementation == HMAC_MODULE_KEY || implementation == HOST_SIGNALS_MODULE_KEY)
            && !self.module_blocks.contains_key(name)
        {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "Edge Cookie module `{name}` is selected but has no `[ec.{name}]` configuration"
                ),
            }));
        }

        // Every configured block must be the selected one. An unreferenced
        // block is almost always a mistake (a mistyped selector or a stale
        // block), and accepting it silently invites configuration drift.
        let unreferenced: Vec<&str> = self
            .module_blocks
            .keys()
            .map(String::as_str)
            .filter(|configured| *configured != name)
            .collect();
        if unreferenced.is_empty() {
            Ok(())
        } else {
            Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[ec.{}] is configured but `{name}` is selected. Remove the unselected \
                     block, or correct the selector",
                    unreferenced.join("], [ec.")
                ),
            }))
        }
    }

    /// Validates one module name or implementation id.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] when `name` is not a
    /// module name, see [`crate::module_name::is_valid`].
    fn validate_module_name(name: &str) -> Result<(), Report<TrustedServerError>> {
        if crate::module_name::is_valid(name) {
            return Ok(());
        }
        Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "Edge Cookie module name `{name}` is not a module name, which is parts \
                 joined by `.`, each of lower case letters, digits, `_` or `-`"
            ),
        }))
    }

    /// Migrates the deprecated `[ec] passphrase` form to the module layout.
    ///
    /// A configuration still carrying the old key keeps working for one
    /// release cycle, mapping to `module = "hmac"` with the passphrase in
    /// the `[ec.hmac]` block, and a deprecation warning names the new
    /// location. A configuration carrying both forms is rejected so a
    /// half-edited file fails loudly instead of one form silently winning.
    ///
    /// The deprecated key is held to the same passphrase rules as the new
    /// `[ec.hmac]` block. Validation runs before this migration and the
    /// deprecated field carries no check of its own, so without the check here
    /// a short or empty passphrase in the old location would start a
    /// deployment that the new location rejects.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] when both the deprecated
    /// key and any part of the module configuration are present, or when the
    /// deprecated passphrase fails [`Self::validate_passphrase`].
    pub fn migrate_legacy_ec_layout(&mut self) -> Result<(), Report<TrustedServerError>> {
        let Some(passphrase) = self.passphrase.take() else {
            return Ok(());
        };
        if self.module.is_some() || !self.module_blocks.is_empty() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: "[ec] passphrase (deprecated) and the [ec] module configuration \
                          are both present. Keep exactly one form, moving the passphrase to \
                          [ec.hmac] and deleting the old key"
                    .to_owned(),
            }));
        }
        Self::validate_passphrase(&passphrase).map_err(|err| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[ec] passphrase (deprecated) is invalid ({err}). Use a random secret \
                     of at least {} bytes, placed in [ec.hmac]",
                    Self::MIN_PASSPHRASE_LENGTH,
                ),
            })
        })?;
        log::warn!(
            "[ec] passphrase is deprecated. Move it to [ec.hmac] passphrase and set \
             [ec] module = \"hmac\""
        );
        self.module = Some(EcModuleSelection::from(HMAC_MODULE_KEY));
        self.module_blocks.insert(
            HMAC_MODULE_KEY.to_owned(),
            EcModuleBlock::from(HmacModuleConfig { passphrase }),
        );
        Ok(())
    }
}

impl Validate for Ec {
    /// Validates the partner entries and the settings of every block that
    /// configures a module built into core.
    ///
    /// Written out rather than derived because each block's errors are keyed
    /// by the name the operator wrote the block under, so a passphrase
    /// `[ec.primary]` rejects is reported at `ec.primary.passphrase`. A
    /// derived nested validation would key them under this struct's own field
    /// name, which is a path no configuration has.
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::new();
        errors.merge_self("partners", self.partners.validate());
        let blocks = self
            .module_blocks
            .hmac_blocks()
            .map(|(name, config)| (name, config.validate()))
            .chain(
                self.module_blocks
                    .host_signals_blocks()
                    .map(|(name, config)| (name, config.validate())),
            );
        for (name, result) in blocks {
            if let Err(block_errors) = result {
                errors.errors_mut().insert(
                    Cow::Owned(name.to_owned()),
                    ValidationErrorsKind::Struct(Box::new(block_errors)),
                );
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// The keys the `[ec]` section reads as its own settings.
///
/// Every other key in the section is one module's `[ec.<name>]` settings
/// table, so these names are reserved and cannot name a module. They are
/// written out because serde reads the [`Ec`] fields by name and has no way to
/// report the list back for an error message.
const EC_SECTION_KEYS: &[&str] = &[
    "module",
    "passphrase",
    "resolve_allowed_origins",
    "ec_store",
    "pull_sync_concurrency",
    "cluster_trust_threshold",
    "cluster_recheck_secs",
    "partners",
];

/// The removed table that used to hold every module's settings.
const REMOVED_EC_PROVIDERS_TABLE: &str = "providers";

/// The key in a module block that names the implementation it configures.
///
/// Anything reading a module block out of a serialized configuration rather
/// than out of [`EcModuleBlock`] reads the same key from here, so the two
/// cannot drift apart.
pub(crate) const MODULE_IMPLEMENTATION_KEY: &str = "implementation";

/// The `[ec.<name>]` settings tables of the Edge Cookie identity modules,
/// keyed by the name each is written under.
///
/// A module that has settings is configured in its own table, for example:
///
/// ```toml
/// [ec]
/// module = "hmac"
///
/// [ec.hmac]
/// passphrase = "replace-with-32-plus-byte-random-secret"
/// ```
///
/// A table may name the implementation it configures, which makes its own name
/// a label of the operator's choosing, so the same configuration can also be
/// written as:
///
/// ```toml
/// [ec]
/// module = "primary"
///
/// [ec.primary]
/// implementation = "hmac"
/// passphrase = "replace-with-32-plus-byte-random-secret"
/// ```
///
/// The active module is chosen by the [`Ec::module`] selector, and the one
/// table present must be the one it names (see
/// [`Ec::validate_module_selection`]).
#[derive(Debug, Default, Clone, Serialize)]
#[serde(transparent)]
pub struct EcModuleBlocks(BTreeMap<String, EcModuleBlock>);

impl EcModuleBlocks {
    /// The implementation the module `name` resolves to.
    ///
    /// A block may name the implementation it configures. Without one, and
    /// when the module has no block at all, the module's name is its
    /// implementation.
    #[must_use]
    pub fn implementation<'a>(&'a self, name: &'a str) -> &'a str {
        self.0
            .get(name)
            .and_then(|block| block.implementation.as_deref())
            .unwrap_or(name)
    }

    /// Every configured block that sets the built-in HMAC module up, with
    /// the name it is written under.
    ///
    /// The name is the label the operator chose, so a caller reporting one of
    /// these settings names the path the operator wrote rather than the
    /// implementation's own.
    pub fn hmac_blocks(&self) -> impl Iterator<Item = (&str, &HmacModuleConfig)> {
        self.0
            .iter()
            .filter_map(|(name, block)| block.hmac_settings().map(|config| (name.as_str(), config)))
    }

    /// Every configured block that sets the built-in host-signal module up,
    /// with the name it is written under.
    ///
    /// The same contract as [`hmac_blocks`](Self::hmac_blocks), for the other
    /// module core builds itself.
    pub fn host_signals_blocks(&self) -> impl Iterator<Item = (&str, &HostSignalsModuleConfig)> {
        self.0.iter().filter_map(|(name, block)| {
            block
                .host_signals_settings()
                .map(|config| (name.as_str(), config))
        })
    }
}

impl Deref for EcModuleBlocks {
    type Target = BTreeMap<String, EcModuleBlock>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for EcModuleBlocks {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'de> Deserialize<'de> for EcModuleBlocks {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BlocksVisitor;

        impl<'de> serde::de::Visitor<'de> for BlocksVisitor {
            type Value = EcModuleBlocks;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("the `[ec.<name>]` module settings tables")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut blocks = BTreeMap::new();
                while let Some(name) = map.next_key::<String>()? {
                    let table = map.next_value::<JsonValue>()?;
                    blocks.insert(name.clone(), read_module_block::<A::Error>(&name, table)?);
                }
                Ok(EcModuleBlocks(blocks))
            }
        }

        deserializer.deserialize_map(BlocksVisitor)
    }
}

/// Reads one key left over from the `[ec]` section as the module block it
/// names.
///
/// This is where the `[ec]` section gets the unknown-key check that the fixed
/// fields lose by holding the module blocks alongside them. A key that is
/// not a table cannot be a module, so it is reported as the mistyped setting
/// it almost certainly is.
fn read_module_block<E>(name: &str, table: JsonValue) -> Result<EcModuleBlock, E>
where
    E: serde::de::Error,
{
    if name == REMOVED_EC_PROVIDERS_TABLE {
        return Err(E::custom(
            "[ec.providers] is no longer read. Each module's settings moved to a table of \
             its own under [ec], so a block written as [ec.providers.hmac] is now [ec.hmac]",
        ));
    }
    let JsonValue::Object(mut table) = table else {
        let known = EC_SECTION_KEYS
            .iter()
            .map(|key| format!("`{key}`"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(E::custom(format!(
            "unknown field `{name}`, expected one of {known}, or an [ec.<name>] module \
             settings table"
        )));
    };
    let implementation = match table.remove(MODULE_IMPLEMENTATION_KEY) {
        None => None,
        Some(JsonValue::String(implementation)) => Some(implementation),
        Some(_) => {
            return Err(E::custom(format!(
                "`implementation` in [ec.{name}] must be a string naming the module \
                 implementation the block configures"
            )));
        }
    };

    // The implementation decides how the rest of the table is read. Core reads
    // its own modules' settings here, so a mistyped key fails where the
    // configuration is read rather than at the request that needed it, and
    // keeps the settings of a module an adapter injects as the raw values
    // that adapter deserializes for itself.
    let invalid = |err| E::custom(format!("[ec.{name}] is invalid ({err})"));
    let settings = match implementation.as_deref().unwrap_or(name) {
        HMAC_MODULE_KEY => EcModuleSettings::Hmac(
            serde_json::from_value(JsonValue::Object(table)).map_err(invalid)?,
        ),
        HOST_SIGNALS_MODULE_KEY => EcModuleSettings::HostSignals(
            serde_json::from_value(JsonValue::Object(table)).map_err(invalid)?,
        ),
        _ => EcModuleSettings::Injected(table),
    };
    Ok(EcModuleBlock {
        implementation,
        settings,
    })
}

/// One module's `[ec.<name>]` settings table.
#[derive(Debug, Clone, Serialize)]
pub struct EcModuleBlock {
    /// The implementation this block configures, written as
    /// `implementation = "<id>"`, when the block's own name is not it.
    ///
    /// Setting it makes the block name a label of the operator's choosing, so
    /// one implementation can be configured under any name. Everything that
    /// resolves the selected module reads the implementation rather than the
    /// label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implementation: Option<String>,

    /// Everything the block holds other than `implementation`.
    #[serde(flatten)]
    pub settings: EcModuleSettings,
}

impl EcModuleBlock {
    /// The built-in HMAC module's settings, when this block configures it.
    #[must_use]
    pub fn hmac_settings(&self) -> Option<&HmacModuleConfig> {
        match &self.settings {
            EcModuleSettings::Hmac(config) => Some(config),
            EcModuleSettings::HostSignals(_) | EcModuleSettings::Injected(_) => None,
        }
    }

    /// The built-in host-signal module's settings, when this block
    /// configures it.
    #[must_use]
    pub fn host_signals_settings(&self) -> Option<&HostSignalsModuleConfig> {
        match &self.settings {
            EcModuleSettings::HostSignals(config) => Some(config),
            EcModuleSettings::Hmac(_) | EcModuleSettings::Injected(_) => None,
        }
    }
}

impl From<HmacModuleConfig> for EcModuleBlock {
    /// Builds the `[ec.hmac]` block, the built-in HMAC module configured
    /// under its own name. A block under a label names its implementation
    /// instead.
    fn from(config: HmacModuleConfig) -> Self {
        Self {
            implementation: None,
            settings: EcModuleSettings::Hmac(config),
        }
    }
}

impl From<HostSignalsModuleConfig> for EcModuleBlock {
    /// Builds the `[ec.host_signals]` block, the built-in host-signal module
    /// configured under its own name. A block under a label names its
    /// implementation instead.
    fn from(config: HostSignalsModuleConfig) -> Self {
        Self {
            implementation: None,
            settings: EcModuleSettings::HostSignals(config),
        }
    }
}

/// The settings one module block holds, read according to the
/// implementation the block configures.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum EcModuleSettings {
    /// The built-in HMAC module's settings.
    Hmac(HmacModuleConfig),

    /// The built-in host-signal module's settings.
    HostSignals(HostSignalsModuleConfig),

    /// The settings of a module an adapter injects, kept as the raw values
    /// the block held. The adapter that builds the module deserializes them
    /// into the vendor crate's own config type, so core never names a vendor
    /// and a new module adds nothing here.
    Injected(serde_json::Map<String, JsonValue>),
}

/// Configuration for the built-in HMAC Edge Cookie module.
///
/// Mapped from the `[ec.hmac]` TOML block, or from a block under a label whose
/// `implementation` is `hmac`. Unknown keys are rejected, so a mistyped
/// setting fails at startup instead of being accepted silently and leaving the
/// intended setting at its default.
#[derive(Debug, Default, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct HmacModuleConfig {
    /// Publisher passphrase used as the HMAC key for EC generation.
    #[validate(custom(function = Ec::validate_passphrase))]
    pub passphrase: Redacted<String>,
}

/// Configuration for the built-in host-signal Edge Cookie module.
///
/// Mapped from the `[ec.host_signals]` TOML block, or from a block under a
/// label whose `implementation` is `host_signals`. Unknown keys are rejected,
/// so a mistyped setting fails at startup instead of being accepted silently
/// and leaving the intended setting at its default.
#[derive(Debug, Default, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct HostSignalsModuleConfig {
    /// Passphrase used as the HMAC key over the host signals and client IP.
    #[validate(custom(function = Ec::validate_passphrase))]
    pub passphrase: Redacted<String>,
}

/// Device-detection configuration.
///
/// Mapped from the `[device]` TOML section. Selects which device-detection
/// module classifies a request into device signals, mirroring the Edge
/// Cookie module selection in [`Ec`].
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct DeviceConfig {
    /// The key of the device-detection module to activate.
    ///
    /// Defaults to the built-in `builtin` module when absent, which classifies
    /// from the User-Agent alone, so device classification itself makes no
    /// host-specific call. The opt-in `fastly` module strengthens the
    /// browser/bot gate with the host's TLS and HTTP/2 signals, which the Fastly
    /// entry point reads on every request regardless of this selector. Override
    /// it with the
    /// `TRUSTED_SERVER__device__module` environment variable so the same
    /// compiled WebAssembly can switch modules at deployment. An unknown key is
    /// rejected at startup by
    /// [`validate_module_selection`](Self::validate_module_selection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

impl DeviceConfig {
    /// Returns the active device-detection module key, defaulting to the
    /// built-in heuristic.
    #[must_use]
    pub fn module_key(&self) -> &str {
        self.module.as_deref().unwrap_or("builtin")
    }

    /// Validates the selected device-detection module.
    ///
    /// `builtin` and `fastly` are resolved by the core and the Fastly adapter.
    /// Any other key names an integration module that declares a device
    /// module, and the registry rejects a key no module supplies, so this
    /// cannot be a closed list without shutting modules out of device detection
    /// entirely.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] never, today. The error
    /// type is kept because the caller treats module validation uniformly
    /// across the three capabilities.
    pub fn validate_module_selection(&self) -> Result<(), Report<TrustedServerError>> {
        Ok(())
    }
}

/// Which permission signal modules run, and in what order.
///
/// Mapped from the `[permission-signal]` TOML section, where `modules`
/// selects. `[ec]`, `[geo]` and `[device]` each name one module with
/// `module`, whereas signals compose, because a request can carry a TCF string
/// and a Global Privacy Control header at once and both have something to say.
/// So here `modules` names a list, and the order is the policy, because the
/// last module with an opinion decides.
///
/// A module that gains settings will take them in a
/// `[permission-signal.<name>]` block named for it. None of the modules that
/// ship has settings, so `modules` is the only key accepted, and any other key
/// is refused as an unknown field rather than silently ignored.
///
/// See `crates/trusted-server-core/src/permission_signal/README.md`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Validate)]
pub struct PermissionSignalConfig {
    /// The modules to run, in order, named by their crate folders below
    /// `crates/permission-signal`, for example `gpc`, `gpp`, `us-privacy`,
    /// `tcf` and `mtm` for the five that ship.
    ///
    /// Absent means every module the adapter offers, in the order it offers
    /// them. A publisher who does not want to act on one removes it from the
    /// list, and there is no separate switch, because a module that is not
    /// listed does not run. An empty list runs none of them, leaving every
    /// permission at its country and region baseline.
    ///
    /// Which names are valid is only known where the module crates are
    /// linked, so the check that each name matches an available module and
    /// none is repeated happens at the adapter's composition root, through
    /// [`build_permission_signal_modules`], and refuses startup rather than
    /// silently ignoring a typo.
    ///
    /// [`build_permission_signal_modules`]:
    ///     crate::permission_signal::build_permission_signal_modules
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modules: Option<Vec<String>>,
}

/// Read by hand rather than derived, so that `sources` and `module`, the keys
/// `modules` replaced, are refused with a message saying what to write instead. A derived
/// struct could only refuse it by name by declaring it as a field, and would
/// then list it among the keys it expects whenever it refused any other.
impl<'de> Deserialize<'de> for PermissionSignalConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut section = serde_json::Map::<String, JsonValue>::deserialize(deserializer)?;
        if section.contains_key("sources") {
            return Err(serde::de::Error::custom(
                "[permission-signal] sources is no longer accepted. Name the modules \
                 to run, in order, in [permission-signal] modules instead",
            ));
        }
        if section.contains_key("module") {
            return Err(serde::de::Error::custom(
                "[permission-signal] module is `modules` here, a list, because signals \
                 compose. Name the modules to run, in order, in [permission-signal] \
                 modules instead",
            ));
        }
        if let Some(key) = section.keys().find(|key| key.as_str() != "modules") {
            return Err(serde::de::Error::custom(format!(
                "unknown field `{key}` in [permission-signal], expected `modules`. No \
                 permission signal module takes settings yet, so a \
                 [permission-signal.<name>] block is not accepted"
            )));
        }
        // Read as an option, so an explicit JSON null is the same as leaving
        // the key out.
        let modules = match section.remove("modules") {
            Some(value) => serde_json::from_value(value).map_err(serde::de::Error::custom)?,
            None => None,
        };
        Ok(Self { modules })
    }
}

/// Which prefix module gives the start of every name the browser holds and
/// every key in a shared store.
///
/// Mapped from the `[prefix]` TOML section. See [`crate::prefix`].
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct PrefixConfig {
    /// The prefix module to run, named as any module is, by its crate folder.
    ///
    /// Core's own module runs when this is absent or names `ts`, giving `ts`
    /// for the names the browser holds and `ts.<publisher.domain>` for shared
    /// store keys. A name no module supplies is refused when the registry is
    /// built, in
    /// [`IntegrationRegistry::with_plan_and_registrations`](crate::integrations::IntegrationRegistry::with_plan_and_registrations).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

/// The permission policy file a domain runs, mapped from the `[permissions]`
/// table.
///
/// `policy` names the file by its stem. `sha256` is the SHA-256 of the file,
/// as [`policy_sha256`](crate::permissions::policy_sha256) forms it, and
/// `text` is the file itself, for the adapters that read the policy from the
/// blob.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionsConfig {
    /// The policy file's stem, so `standard.yaml` is `standard`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    /// The SHA-256 of the policy file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// The policy file itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl PermissionsConfig {
    /// Whether the table sets nothing, so it is left out of a blob.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Geo / IP intelligence configuration.
///
/// Mapped from the `[geo]` TOML section. Selects which module resolves a
/// client IP into [`GeoInfo`](crate::platform::GeoInfo), mirroring the Edge
/// Cookie module selection in [`Ec`].
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct GeoConfig {
    /// The key of the geo module to activate.
    ///
    /// No module is the default: Trusted Server resolves no geolocation and
    /// makes no host geo call, so a default deployment is not tied to any host
    /// geo service, and the permission baseline comes from the top of the
    /// `rules` tree in `permissions.yaml`. `module = "none"` spells
    /// the same choice explicitly. The host platform's own geo lookup is
    /// opt-in via `module = "platform"`. Override it with the
    /// `TRUSTED_SERVER__geo__module` environment variable so the same compiled
    /// WebAssembly can switch modules at deployment. An unknown key is rejected
    /// at startup by
    /// [`validate_module_selection`](Self::validate_module_selection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,

    /// Acknowledges that, with no geo module, every request is treated as
    /// coming from the place at the top of the `permissions.yaml` `rules` tree.
    ///
    /// With geolocation off, a visitor from any other jurisdiction silently
    /// receives that top node's permission rules. A deployment that
    /// runs an Edge Cookie module without a geo module must set this to
    /// `true`, checked at startup by
    /// [`validate_jurisdiction_acknowledgment`](Self::validate_jurisdiction_acknowledgment),
    /// so serving a single jurisdiction is an explicit operator decision rather
    /// than an accident of the default configuration.
    #[serde(default)]
    pub assume_single_jurisdiction: bool,
}

impl GeoConfig {
    /// Validates that the selected geo module is available in this build.
    ///
    /// No selector is valid and is the default, running without geolocation,
    /// the same way the Edge Cookie module runs statelessly when none is
    /// selected. The explicit `"none"` spells the same choice.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] when the selected module
    /// key is not one this build provides.
    pub fn validate_module_selection(&self) -> Result<(), Report<TrustedServerError>> {
        // Unset, `none` and `platform` are resolved by the core. Any other key
        // names an integration module that declares a geo module, and the
        // registry rejects one that no module supplies, so this check cannot be
        // a closed list without shutting modules out of geo entirely.
        Ok(())
    }

    /// Validates that running jurisdiction consumers without geolocation is
    /// explicitly acknowledged.
    ///
    /// With no geo module, every request resolves to the permission baseline
    /// at the top of the `permissions.yaml` `rules` tree, so a visitor from any
    /// other jurisdiction silently receives that node's rules. That is
    /// acceptable only as an explicit operator
    /// decision. When an Edge Cookie module is configured (the permission
    /// model gates it by jurisdiction) and no geo module is selected,
    /// [`assume_single_jurisdiction`](Self::assume_single_jurisdiction) must be
    /// `true`.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] when an Edge Cookie
    /// module is configured, no geo module is selected, and
    /// `assume_single_jurisdiction` is not set.
    pub fn validate_jurisdiction_acknowledgment(
        &self,
        ec: &Ec,
    ) -> Result<(), Report<TrustedServerError>> {
        // Location is resolved by the host lookup (`platform`) or by any
        // integration module that declares a geo module. Only an unset
        // selector and the explicit `none` resolve nothing, so only those
        // two leave every request on the declared jurisdiction.
        let geo_disabled = matches!(self.module.as_deref(), None | Some("none"));
        // The selector is a typed enum, so statelessness is the absent
        // selector or the explicit `none`, matched rather than compared as a
        // string.
        let ec_active = !matches!(ec.module, None | Some(EcModuleSelection::None));
        if geo_disabled && ec_active && !self.assume_single_jurisdiction {
            return Err(Report::new(TrustedServerError::Configuration {
                message: "[ec] module is configured but no [geo] module is selected, so \
                          every request would be treated as the top of the permissions.yaml \
                          rules tree. Set [geo] assume_single_jurisdiction = true to \
                          acknowledge single-jurisdiction operation, or select a geo module"
                    .to_owned(),
            }));
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

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestSigning {
    #[serde(default = "default_request_signing_enabled")]
    pub enabled: bool,
    /// The removed `config_store_id`. The field never holds a value, because
    /// reading one always fails.
    #[serde(default, skip_serializing)]
    #[allow(
        dead_code,
        reason = "the field exists so that reading the removed key fails with directions"
    )]
    config_store_id: RemovedSigningStoreId,
    /// The removed `secret_store_id`, refused as `config_store_id` is.
    #[serde(default, skip_serializing)]
    #[allow(
        dead_code,
        reason = "the field exists so that reading the removed key fails with directions"
    )]
    secret_store_id: RemovedSigningStoreId,
}

impl RequestSigning {
    /// Request signing switched on or off.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Self::default()
        }
    }
}

fn default_request_signing_enabled() -> bool {
    false
}

fn default_s3_access_key_id() -> Redacted<String> {
    Redacted::new("access_key_id".to_string())
}

fn default_s3_secret_access_key() -> Redacted<String> {
    Redacted::new("secret_access_key".to_string())
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

/// Authentication configuration for an asset origin or the publisher origin.
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

    /// Refuses a table the signer cannot use, naming `setting` in the message.
    fn prepare_runtime(&self, setting: &str) -> Result<(), Report<TrustedServerError>> {
        match self {
            Self::S3SigV4(config) => config.prepare_runtime(setting),
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
/// the `SigV4` canonical request. Credential fields hold secret-store key names
/// in app config and resolved values at runtime.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct S3SigV4AuthConfig {
    /// `AWS` region used in the credential scope.
    pub region: String,
    /// Deprecated per-route store selector accepted for migration only.
    #[serde(default, skip_serializing)]
    pub secret_store: Option<String>,
    /// Secret reference containing the `AWS` access key ID.
    #[serde(default = "default_s3_access_key_id")]
    pub access_key_id: Redacted<String>,
    /// Secret reference containing the `AWS` secret access key.
    #[serde(default = "default_s3_secret_access_key")]
    pub secret_access_key: Redacted<String>,
    /// Optional secret reference containing an `AWS` session token.
    #[serde(default)]
    pub session_token: Option<Redacted<String>>,
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
        if self.secret_store.take().is_some() {
            log::warn!(
                "S3 secret_store is deprecated and ignored; static credentials resolve through the default app-config secret store"
            );
        }
        self.access_key_id = Redacted::new(self.access_key_id.expose().trim().to_string());
        self.secret_access_key = Redacted::new(self.secret_access_key.expose().trim().to_string());
        self.session_token = self.session_token.take().and_then(|value| {
            let value = value.expose().trim().to_string();
            (!value.is_empty()).then(|| Redacted::new(value))
        });
    }

    fn prepare_runtime(&self, setting: &str) -> Result<(), Report<TrustedServerError>> {
        if self.region.is_empty() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!("{setting} s3_sigv4 region must not be empty"),
            }));
        }
        if !s3_region_is_valid(&self.region) {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "{setting} s3_sigv4 region must contain only lowercase letters, digits, and '-'"
                ),
            }));
        }
        if self.access_key_id.expose().is_empty() || self.secret_access_key.expose().is_empty() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "{setting} s3_sigv4 credentials must not be empty after secret resolution"
                ),
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
    /// Trim image configuration keys and values, rejecting keys that collide after trimming.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if retained profile-set or profile keys
    /// collide after trimming.
    fn normalize(&mut self) -> Result<(), Report<TrustedServerError>> {
        validate_trimmed_image_optimizer_keys(
            self.profile_sets.keys(),
            "image_optimizer.profile_sets",
        )?;
        let mut profile_sets: Vec<_> = self.profile_sets.iter().collect();
        profile_sets.sort_unstable_by_key(|(name, _)| *name);
        for (name, profile_set) in profile_sets {
            if !name.trim().is_empty() {
                validate_trimmed_image_optimizer_keys(
                    profile_set.profiles.keys(),
                    &format!("image_optimizer.profile_sets.{name:?}.profiles"),
                )?;
            }
        }

        self.profile_sets = self
            .profile_sets
            .drain()
            .map(|(key, mut profile_set)| {
                profile_set.normalize();
                (key.trim().to_string(), profile_set)
            })
            .filter(|(key, _)| !key.is_empty())
            .collect();
        Ok(())
    }

    /// Eagerly validate configured image profile sets.
    pub(crate) fn prepare_runtime(&self) -> Result<(), Report<TrustedServerError>> {
        for (name, profile_set) in &self.profile_sets {
            profile_set.prepare_runtime(name)?;
        }
        Ok(())
    }
}

/// Check retained image configuration keys before normalization discards originals.
///
/// # Errors
///
/// Returns a configuration error naming both original keys when they trim to
/// the same non-empty name.
fn validate_trimmed_image_optimizer_keys<'a>(
    keys: impl Iterator<Item = &'a String>,
    path: &str,
) -> Result<(), Report<TrustedServerError>> {
    let mut names: Vec<_> = keys
        .map(|key| (key.trim(), key.as_str()))
        .filter(|(trimmed, _)| !trimmed.is_empty())
        .collect();
    names.sort_unstable();

    for pair in names.windows(2) {
        let (trimmed, first) = pair[0];
        let (next_trimmed, second) = pair[1];
        if trimmed == next_trimmed {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "{path} keys {first:?} and {second:?} collide after trimming to {trimmed:?}"
                ),
            }));
        }
    }
    Ok(())
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
        "auto" | "avif" | "gif" | "jpeg" | "jpg" | "jxl" | "jpegxl" | "mp4" | "png" | "webp" => {
            Ok(())
        }
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
    #[serde(serialize_with = "crate::redacted::sensitive")]
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
            auth.prepare_runtime(ASSET_ROUTE_AUTH_SETTING)?;
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
pub struct Proxy {
    /// Enable TLS certificate verification when proxying to HTTPS origins.
    /// Defaults to true for secure production use.
    /// Set to false for local development with self-signed certificates.
    #[serde(default = "default_certificate_check")]
    pub certificate_check: bool,
    /// Permitted signing, initial fetch, and redirect target domains for the
    /// first-party proxy.
    ///
    /// Supports exact hostname match (`"example.com"`) and subdomain wildcard
    /// prefix (`"*.example.com"`, which also matches the apex `example.com`).
    /// Matching is case-insensitive.
    ///
    /// When empty (the default), proxy hosts are not restricted. Configure this
    /// in production to constrain signed and fetched first-party proxy targets.
    /// When `auction.prebid.external_bundle_url` is configured, this list
    /// must include its host and any HTTPS redirect targets.
    #[serde(default, deserialize_with = "vec_from_seq_or_map")]
    pub allowed_domains: Vec<String>,
    /// Path-prefix-based asset proxy routes evaluated before publisher fallback.
    #[serde(default, deserialize_with = "vec_from_seq_or_map")]
    pub asset_routes: Vec<ProxyAssetRoute>,
    /// The modules this section selects, with each one's settings in the
    /// table at its name.
    #[serde(flatten)]
    pub modules: SectionModules,
}

fn default_certificate_check() -> bool {
    true
}

impl Default for Proxy {
    fn default() -> Self {
        Self {
            certificate_check: default_certificate_check(),
            allowed_domains: Vec::new(),
            asset_routes: Vec::new(),
            modules: SectionModules::default(),
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
                "proxy.allowed_domains is empty: all signing, initial fetch, and redirect hosts are permitted (open mode)"
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

/// The analytics module's name in full, which is also the table holding its
/// settings and which every message about them names.
const ANALYTICS_MODULE: &str = "analytics.tinybird";

/// Direct Tinybird Events API telemetry configuration, the settings of the
/// module `[analytics]` selects.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TinybirdSettings {
    /// Regional Tinybird API host, without scheme or path.
    #[serde(default)]
    pub api_host: String,
    /// Deprecated feature-specific store selector accepted for migration only.
    #[serde(default, skip_serializing)]
    pub secret_store: Option<String>,
    /// Auction Events API datasource name.
    #[serde(default = "default_tinybird_auction_dataset")]
    pub auction_dataset: String,
    /// Secret reference containing the auction datasource APPEND token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auction_token_secret: Option<Redacted<String>>,
    /// Reserved for future access-log telemetry.
    ///
    /// `true` is rejected until an access-log emitter is wired, so operators
    /// cannot enable a setting that silently emits nothing.
    #[serde(default)]
    pub access_enabled: bool,
    /// Future access-log Events API datasource name.
    #[serde(default = "default_tinybird_access_dataset")]
    pub access_dataset: String,
    /// Deprecated placeholder for the unwired access-log APPEND token.
    #[serde(default, skip_serializing)]
    pub access_token_secret: Option<Redacted<String>>,
    /// Future fraction of requests to emit for optional access telemetry.
    #[serde(default)]
    pub access_sample_rate: f64,
    /// Defensive maximum NDJSON body size for one Events API request.
    #[serde(default = "default_tinybird_max_body_bytes")]
    pub max_body_bytes: usize,
}

fn default_tinybird_auction_dataset() -> String {
    "auction_events_raw".to_owned()
}

fn default_tinybird_access_dataset() -> String {
    "access_logs_raw".to_owned()
}

fn default_tinybird_max_body_bytes() -> usize {
    1024 * 1024
}

impl Default for TinybirdSettings {
    fn default() -> Self {
        Self {
            api_host: String::new(),
            secret_store: None,
            auction_dataset: default_tinybird_auction_dataset(),
            auction_token_secret: None,
            access_enabled: false,
            access_dataset: default_tinybird_access_dataset(),
            access_token_secret: None,
            access_sample_rate: 0.0,
            max_body_bytes: default_tinybird_max_body_bytes(),
        }
    }
}

impl TinybirdSettings {
    fn normalize(&mut self) {
        self.api_host = self.api_host.trim().to_ascii_lowercase();
        if self.secret_store.take().is_some() {
            log::warn!(
                "{ANALYTICS_MODULE}.secret_store is deprecated and ignored; static credentials resolve through the default app-config secret store"
            );
        }
        self.auction_dataset = self.auction_dataset.trim().to_owned();
        self.auction_token_secret = self.auction_token_secret.take().and_then(|value| {
            let value = value.expose().trim().to_owned();
            (!value.is_empty()).then(|| Redacted::new(value))
        });
        self.access_dataset = self.access_dataset.trim().to_owned();
        self.access_token_secret = None;
    }

    fn prepare_runtime(&mut self) -> Result<(), Report<TrustedServerError>> {
        self.normalize();
        if !(0.0..=1.0).contains(&self.access_sample_rate) {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "{ANALYTICS_MODULE}.access_sample_rate must be between 0.0 and 1.0"
                ),
            }));
        }
        if self.max_body_bytes < 1024 {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!("{ANALYTICS_MODULE}.max_body_bytes must be at least 1024"),
            }));
        }
        if self.access_enabled {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "{ANALYTICS_MODULE}.access_enabled is reserved for future access-log telemetry; no emitter is currently wired"
                ),
            }));
        }
        validate_tinybird_api_host(&self.api_host)?;
        validate_tinybird_dataset(
            &self.auction_dataset,
            &format!("{ANALYTICS_MODULE}.auction_dataset"),
        )?;
        let token = self.auction_token_secret.as_ref().ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "{ANALYTICS_MODULE}.auction_token_secret is required when [analytics] selects the module"
                ),
            })
        })?;
        validate_tinybird_secret(
            token.expose(),
            &format!("{ANALYTICS_MODULE}.auction_token_secret"),
        )
    }
}

fn validate_tinybird_api_host(host: &str) -> Result<(), Report<TrustedServerError>> {
    if host.is_empty()
        || host.contains('/')
        || host.contains(':')
        || host.chars().any(char::is_control)
        || host.starts_with("http://")
        || host.starts_with("https://")
    {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "{ANALYTICS_MODULE}.api_host must be a regional host without scheme, port, or path"
            ),
        }));
    }
    validate_host_header_override_value(host).map_err(|reason| {
        Report::new(TrustedServerError::Configuration {
            message: format!("{ANALYTICS_MODULE}.api_host {reason}"),
        })
    })
}

fn validate_tinybird_dataset(value: &str, setting: &str) -> Result<(), Report<TrustedServerError>> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!("{setting} must be a non-empty datasource identifier"),
        }));
    }
    Ok(())
}

fn validate_tinybird_secret(value: &str, setting: &str) -> Result<(), Report<TrustedServerError>> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!("{setting} must be non-empty after secret resolution"),
        }));
    }
    Ok(())
}

/// The `[analytics]` section, selecting the module that sends auction telemetry.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnalyticsConfig {
    /// The module that runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// The analytics module's settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tinybird: Option<TinybirdSettings>,
}

/// Whether `written`, selected in `[analytics]`, names the analytics module,
/// in full or by its name within the section.
fn names_analytics_module(written: &str) -> bool {
    written == ANALYTICS_MODULE
        || written == crate::module_name::short_form("analytics", ANALYTICS_MODULE)
}

/// Whether the document `data` selects the analytics module in `[analytics]`,
/// read before the settings are, to tell which secret references are live.
pub(crate) fn document_selects_analytics_module(data: &JsonValue) -> bool {
    data.pointer("/analytics/module")
        .and_then(JsonValue::as_str)
        .is_some_and(names_analytics_module)
}

impl AnalyticsConfig {
    /// Whether `module` selects the analytics module.
    fn selects_analytics_module(&self) -> bool {
        self.module.as_deref().is_some_and(names_analytics_module)
    }

    /// The analytics module's settings, when `module` selects it.
    #[must_use]
    pub fn analytics_module(&self) -> Option<&TinybirdSettings> {
        if self.selects_analytics_module() {
            self.tinybird.as_ref()
        } else {
            None
        }
    }

    fn normalize(&mut self) {
        if let Some(settings) = &mut self.tinybird {
            settings.normalize();
        }
    }

    /// Refuses a section that selects nothing or a module this build does
    /// not have, and checks the selected module's settings.
    fn prepare_runtime(&mut self) -> Result<(), Report<TrustedServerError>> {
        let short = crate::module_name::short_form("analytics", ANALYTICS_MODULE);
        match self.module.as_deref() {
            None => {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: "[analytics] selects no module. A module type's section names \
                              what runs with `module` or `modules`, so select one or remove \
                              the section"
                        .to_owned(),
                }));
            }
            Some(written) if !self.selects_analytics_module() => {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "[analytics] selects `{written}`, which no module this deployment \
                         supplies. The analytics modules it supplies are [{short}]"
                    ),
                }));
            }
            Some(_) => {}
        }
        match &mut self.tinybird {
            Some(settings) => settings.prepare_runtime(),
            // Checking the defaults reports the first setting the module
            // needs and the table does not have.
            None => TinybirdSettings::default().prepare_runtime(),
        }
    }
}

/// Cache behavior configuration.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct CacheSettings {
    /// Ordered static/rehosted asset rules. The first enabled matching rule wins.
    #[serde(default)]
    pub asset_rules: Vec<CacheAssetRule>,
    /// How long the copies of each robots.txt contributor's answer last,
    /// `[cache.robots-txt]`.
    #[serde(
        rename = "robots-txt",
        default,
        skip_serializing_if = "LoadingCacheConfig::is_default"
    )]
    pub robots_txt: LoadingCacheConfig,
    /// The caches modules declare and load through, each in its own
    /// `[cache.<name>]` table with the same settings as `[cache.robots-txt]`.
    /// A name no linked module declares is refused when the deployment's
    /// caches are resolved, see `integrations::loading_caches_from`.
    #[serde(flatten, default)]
    pub declared: BTreeMap<String, LoadingCacheConfig>,
}

/// The most seconds a loading cache lifetime setting accepts, a year.
pub const MAX_LOADING_CACHE_SECONDS: u32 = 365 * 24 * 60 * 60;

/// The name of the cache that holds each robots.txt module's answer, whose
/// settings are `[cache.robots-txt]`.
pub const ROBOTS_TXT_CACHE: &str = "robots-txt";

/// How long one loading cache's copies last, each field defaulting to the
/// cache's own default when absent.
///
/// See [`crate::platform::PlatformLoadingCache`]. The memory copy is the brief
/// one a process keeps in front of the shared store, and the shared store's
/// copy is the one every instance reads.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LoadingCacheConfig {
    /// The module whose loading cache holds this cache's copies, as its short
    /// name or in full, for example `51degrees` or `loading-cache.51degrees`.
    /// Absent, the one loading cache module the build links, and with none
    /// linked, or several, no copy is kept and every call loads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// How long the brief copy in process memory is used, in seconds. Zero
    /// keeps no memory copy, and a missing value is still loaded once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_seconds: Option<u32>,
    /// How long the copy in the shared store is used at most, in seconds. An
    /// answer with a shorter lifetime of its own ends sooner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_seconds: Option<u32>,
    /// How long the copy in the shared store lasts unused, in seconds, after
    /// which it leaves the store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_idle_seconds: Option<u32>,
}

impl LoadingCacheConfig {
    /// Whether nothing is set, so every lifetime is the cache's default.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The lifetimes these settings give, each from `defaults` where unset.
    #[must_use]
    pub fn lifetimes(
        &self,
        defaults: crate::platform::CacheLifetimes,
    ) -> crate::platform::CacheLifetimes {
        let seconds = |value: Option<u32>, default: Duration| {
            value.map_or(default, |value| Duration::from_secs(u64::from(value)))
        };
        crate::platform::CacheLifetimes {
            memory: seconds(self.memory_seconds, defaults.memory),
            store: seconds(self.store_seconds, defaults.store),
            store_idle: seconds(self.store_idle_seconds, defaults.store_idle),
        }
    }

    /// Refuses a lifetime out of range, naming the setting by `table`.
    ///
    /// # Errors
    ///
    /// When the memory lifetime is over [`MAX_LOADING_CACHE_SECONDS`], or the
    /// store lifetime or idle time is zero or over it.
    pub fn check(&self, table: &str) -> Result<(), Report<TrustedServerError>> {
        check_loading_cache_seconds(table, "memory_seconds", self.memory_seconds, 0)?;
        check_loading_cache_seconds(table, "store_seconds", self.store_seconds, 1)?;
        check_loading_cache_seconds(table, "store_idle_seconds", self.store_idle_seconds, 1)
    }
}

fn check_loading_cache_seconds(
    table: &str,
    field: &str,
    value: Option<u32>,
    least: u32,
) -> Result<(), Report<TrustedServerError>> {
    match value {
        Some(value) if value < least || value > MAX_LOADING_CACHE_SECONDS => {
            Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "[{table}] {field} is {value}, and must be from {least} to \
                     {MAX_LOADING_CACHE_SECONDS}"
                ),
            }))
        }
        _ => Ok(()),
    }
}

impl CacheSettings {
    /// Each cache a loading cache may hold, by its name, with the settings
    /// its `[cache.<name>]` table carries: core's own first, then the ones
    /// modules declare, in name order.
    #[must_use]
    pub fn loading_caches(&self) -> Vec<(&str, &LoadingCacheConfig)> {
        std::iter::once((ROBOTS_TXT_CACHE, &self.robots_txt))
            .chain(
                self.declared
                    .iter()
                    .map(|(name, config)| (name.as_str(), config)),
            )
            .collect()
    }

    fn normalize(&mut self) {
        for rule in &mut self.asset_rules {
            rule.normalize();
        }
    }

    /// Eagerly validate runtime-only cache settings artifacts.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if a loading cache lifetime is out of
    /// range, if any rule ID is duplicate, or if an enabled rule has an
    /// invalid policy/matcher or cannot compile its regex/glob.
    pub fn prepare_runtime(&self) -> Result<(), Report<TrustedServerError>> {
        self.robots_txt.check("cache.robots-txt")?;
        for (name, config) in &self.declared {
            config.check(&format!("cache.{name}"))?;
        }
        let mut seen_ids = HashSet::new();
        for rule in &self.asset_rules {
            if rule.id.is_empty() {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: "cache.asset_rules id must not be empty".to_string(),
                }));
            }
            if !seen_ids.insert(rule.id.clone()) {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!("cache.asset_rules contains duplicate id `{}`", rule.id),
                }));
            }
        }
        for rule in &self.asset_rules {
            rule.prepare_runtime()?;
        }
        Ok(())
    }

    /// Resolve the first enabled asset cache rule that matches `path`.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if a lazily prepared matcher unexpectedly
    /// fails to compile.
    pub fn asset_policy_for_path(
        &self,
        path: &str,
    ) -> Result<Option<CachePolicy>, Report<TrustedServerError>> {
        for rule in &self.asset_rules {
            if rule.matches_path(path)? {
                return Ok(Some(rule.cache_policy()));
            }
        }
        Ok(None)
    }
}

/// A configurable cache rule for publisher-origin or rehosted static assets.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CacheAssetRule {
    /// Stable operator-facing identifier for logs/tests/config errors.
    pub id: String,
    /// Whether this rule participates in matching.
    #[serde(default)]
    pub enabled: bool,
    /// Built-in framework/static preset matcher.
    #[serde(default)]
    pub preset: Option<CacheAssetPreset>,
    /// Raw path prefix matcher.
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// Single glob matcher retained for concise configs.
    #[serde(default)]
    pub path_glob: Option<String>,
    /// Multiple glob matchers.
    #[serde(default)]
    pub path_globs: Vec<String>,
    /// Regex matcher applied to the request path.
    #[serde(default)]
    pub path_regex: Option<String>,
    /// File extensions matched against the request path, case-insensitively.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Bundler fingerprint style required in the filename before matching.
    #[serde(default)]
    pub fingerprint_style: Option<CacheAssetFingerprintStyle>,
    /// Browser-facing cache visibility.
    #[serde(default)]
    pub visibility: CachePolicyVisibility,
    /// Browser cache TTL rendered as `max-age`.
    #[serde(default)]
    pub browser_ttl_seconds: Option<u64>,
    /// Shared edge cache TTL rendered as runtime-specific edge control.
    #[serde(default)]
    pub edge_ttl_seconds: Option<u64>,
    /// Optional stale-while-revalidate duration.
    #[serde(default)]
    pub stale_while_revalidate_seconds: Option<u64>,
    /// Optional stale-if-error duration.
    #[serde(default)]
    pub stale_if_error_seconds: Option<u64>,
    /// Whether browser caches may treat the response as immutable.
    #[serde(default)]
    pub immutable: bool,
    #[serde(skip)]
    compiled_regex: OnceLock<Result<Regex, String>>,
    #[serde(skip)]
    compiled_globs: OnceLock<Result<Vec<Pattern>, String>>,
}

impl CacheAssetRule {
    fn normalize(&mut self) {
        self.id = self.id.trim().to_string();
        self.path_prefix = self
            .path_prefix
            .take()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.path_glob = self
            .path_glob
            .take()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.path_globs = self
            .path_globs
            .iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect();
        self.path_regex = self
            .path_regex
            .take()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.extensions = self
            .extensions
            .iter()
            .map(|value| value.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|value| !value.is_empty())
            .collect();
    }

    fn prepare_runtime(&self) -> Result<(), Report<TrustedServerError>> {
        if !self.enabled {
            return Ok(());
        }

        self.validate_matcher_shape()?;
        self.compiled_regex().map(|_| ())?;
        self.compiled_globs().map(|_| ())?;
        self.validate_policy_shape()?;
        Ok(())
    }

    fn validate_matcher_shape(&self) -> Result<(), Report<TrustedServerError>> {
        if self.path_glob.is_some() && !self.path_globs.is_empty() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "cache.asset_rules `{}` must use path_glob or path_globs, not both",
                    self.id
                ),
            }));
        }

        let matcher_count = usize::from(self.preset.is_some())
            + usize::from(self.path_prefix.is_some())
            + usize::from(self.path_glob.is_some() || !self.path_globs.is_empty())
            + usize::from(self.path_regex.is_some())
            + usize::from(!self.extensions.is_empty());

        if matcher_count != 1 {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "cache.asset_rules `{}` must configure exactly one matcher",
                    self.id
                ),
            }));
        }
        Ok(())
    }

    fn validate_policy_shape(&self) -> Result<(), Report<TrustedServerError>> {
        if self.visibility == CachePolicyVisibility::Private {
            if self.edge_ttl_seconds.is_some() {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "cache.asset_rules `{}` sets edge_ttl_seconds with private visibility; private rules must use browser_ttl_seconds",
                        self.id
                    ),
                }));
            }
            if self.browser_ttl_seconds.is_none() {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!(
                        "cache.asset_rules `{}` with private visibility must configure browser_ttl_seconds",
                        self.id
                    ),
                }));
            }
        } else if self.browser_ttl_seconds.is_none() && self.edge_ttl_seconds.is_none() {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "cache.asset_rules `{}` must configure browser_ttl_seconds or edge_ttl_seconds",
                    self.id
                ),
            }));
        }

        if !self.immutable {
            return Ok(());
        }

        if self
            .browser_ttl_seconds
            .is_none_or(|browser_ttl| browser_ttl == 0)
        {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "cache.asset_rules `{}` sets immutable without a positive browser_ttl_seconds",
                    self.id
                ),
            }));
        }

        let preset_is_content_addressed =
            matches!(self.preset, Some(CacheAssetPreset::NextJsStatic));
        if !preset_is_content_addressed {
            match self.fingerprint_style {
                None => {
                    return Err(Report::new(TrustedServerError::Configuration {
                        message: format!(
                            "cache.asset_rules `{}` sets immutable without fingerprint_style or a content-addressed preset",
                            self.id
                        ),
                    }));
                }
                Some(CacheAssetFingerprintStyle::ViteBase64Url) => {
                    return Err(Report::new(TrustedServerError::Configuration {
                        message: format!(
                            "cache.asset_rules `{}` cannot set immutable with vite-base64-url; use a content-addressed preset or an unambiguous fingerprint_style",
                            self.id
                        ),
                    }));
                }
                Some(_) => {}
            }
        }

        Ok(())
    }

    fn compiled_regex(&self) -> Result<Option<&Regex>, Report<TrustedServerError>> {
        let Some(pattern) = self.path_regex.as_deref() else {
            return Ok(None);
        };
        match self
            .compiled_regex
            .get_or_init(|| Regex::new(pattern).map_err(|err| err.to_string()))
        {
            Ok(regex) => Ok(Some(regex)),
            Err(message) => Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "cache.asset_rules `{}` path_regex `{pattern}` failed to compile: {message}",
                    self.id
                ),
            })),
        }
    }

    fn compiled_globs(&self) -> Result<Option<&[Pattern]>, Report<TrustedServerError>> {
        if self.path_glob.is_none() && self.path_globs.is_empty() {
            return Ok(None);
        }

        match self.compiled_globs.get_or_init(|| {
            let mut compiled = Vec::new();
            let source_patterns = self
                .path_glob
                .iter()
                .chain(self.path_globs.iter())
                .map(String::as_str);
            for pattern in source_patterns {
                compile_cache_asset_glob_patterns(pattern, &mut compiled)?;
            }
            Ok(compiled)
        }) {
            Ok(patterns) => Ok(Some(patterns.as_slice())),
            Err(message) => Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "cache.asset_rules `{}` glob matcher failed to compile: {message}",
                    self.id
                ),
            })),
        }
    }

    fn matches_path(&self, path: &str) -> Result<bool, Report<TrustedServerError>> {
        if !self.enabled || !self.matcher_matches_path(path)? {
            return Ok(false);
        }

        if let Some(style) = self.fingerprint_style
            && !filename_contains_fingerprint(path, style)
        {
            log::debug!(
                "cache asset rule `{}` rejects path `{path}` because the filename has no {style:?} fingerprint",
                self.id
            );
            return Ok(false);
        }

        Ok(true)
    }

    fn matcher_matches_path(&self, path: &str) -> Result<bool, Report<TrustedServerError>> {
        if let Some(preset) = self.preset {
            return Ok(preset.matches_path(path));
        }
        if let Some(prefix) = self.path_prefix.as_deref() {
            return Ok(path.starts_with(prefix));
        }
        if let Some(patterns) = self.compiled_globs()? {
            return Ok(patterns
                .iter()
                .any(|pattern| pattern.matches_with(path, CACHE_ASSET_GLOB_MATCH_OPTIONS)));
        }
        if let Some(regex) = self.compiled_regex()? {
            return Ok(regex.is_match(path));
        }
        if !self.extensions.is_empty() {
            return Ok(path_extension(path).is_some_and(|extension| {
                self.extensions
                    .iter()
                    .any(|candidate| candidate == &extension)
            }));
        }
        Ok(false)
    }

    fn cache_policy(&self) -> CachePolicy {
        CachePolicy {
            visibility: self.visibility.into(),
            browser_ttl: self.browser_ttl_seconds.map(Duration::from_secs),
            edge_ttl: self.edge_ttl_seconds.map(Duration::from_secs),
            stale_while_revalidate: self.stale_while_revalidate_seconds.map(Duration::from_secs),
            stale_if_error: self.stale_if_error_seconds.map(Duration::from_secs),
            immutable: self.immutable,
        }
    }
}

const CACHE_ASSET_GLOB_MATCH_OPTIONS: MatchOptions = MatchOptions {
    case_sensitive: true,
    require_literal_separator: true,
    require_literal_leading_dot: false,
};

fn compile_cache_asset_glob_patterns(
    pattern: &str,
    compiled: &mut Vec<Pattern>,
) -> Result<(), String> {
    let mut variants = vec![pattern.to_string()];
    let mut variant_index = 0;

    while variant_index < variants.len() {
        let variant = variants[variant_index].clone();
        let optional_segments = variant
            .match_indices("**/")
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for segment_start in optional_segments {
            let without_segment = format!(
                "{}{}",
                &variant[..segment_start],
                &variant[segment_start + "**/".len()..]
            );
            if !variants.contains(&without_segment) {
                variants.push(without_segment);
            }
        }
        variant_index += 1;
    }

    for variant in variants {
        compiled.push(Pattern::new(&variant).map_err(|err| err.to_string())?);
    }

    Ok(())
}

/// Built-in cache-rule presets that operators can enable explicitly.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum CacheAssetPreset {
    /// Next.js build output under `/_next/static/`.
    #[serde(rename = "nextjs-static")]
    NextJsStatic,
}

impl CacheAssetPreset {
    fn matches_path(self, path: &str) -> bool {
        match self {
            Self::NextJsStatic => path.starts_with("/_next/static/"),
        }
    }
}

/// Cache visibility parsed from operator configuration.
#[derive(Debug, Default, Clone, Copy, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum CachePolicyVisibility {
    /// Public browser/cache visibility.
    #[default]
    Public,
    /// Private browser visibility.
    Private,
}

impl From<CachePolicyVisibility> for CacheVisibility {
    fn from(value: CachePolicyVisibility) -> Self {
        match value {
            CachePolicyVisibility::Public => Self::Public,
            CachePolicyVisibility::Private => Self::Private,
        }
    }
}

fn path_extension(path: &str) -> Option<String> {
    let filename = path.rsplit('/').next()?;
    let (_, extension) = filename.rsplit_once('.')?;
    (!extension.is_empty()).then(|| extension.to_ascii_lowercase())
}

/// Operator-selected filename fingerprint convention for a cache rule.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum CacheAssetFingerprintStyle {
    /// A hexadecimal suffix, such as `app.0123abcd.js`.
    Hex,
    /// An eight-character uppercase Base32 suffix, such as `app-VRTVD5R5.js`.
    EsbuildBase32,
    /// An eight-character `Base64URL` suffix for non-immutable rules, such as `index-BsELY24f.js`.
    ViteBase64Url,
}

impl CacheAssetFingerprintStyle {
    fn matches_candidate(self, candidate: &str) -> bool {
        match self {
            Self::Hex => {
                candidate.len() >= 8
                    && candidate.chars().all(|ch| ch.is_ascii_hexdigit())
                    && candidate.chars().any(|ch| ch.is_ascii_alphabetic())
            }
            Self::EsbuildBase32 => {
                candidate.len() == 8
                    && candidate
                        .chars()
                        .all(|ch| ch.is_ascii_uppercase() || matches!(ch, '2'..='7'))
                    && candidate.chars().any(|ch| ch.is_ascii_alphabetic())
            }
            Self::ViteBase64Url => {
                candidate.len() == 8
                    && candidate
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
                    && candidate.chars().any(|ch| ch.is_ascii_uppercase())
                    && candidate.chars().any(|ch| {
                        ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_')
                    })
            }
        }
    }
}

fn filename_contains_fingerprint(path: &str, style: CacheAssetFingerprintStyle) -> bool {
    let filename = path.rsplit('/').next().unwrap_or(path);
    let Some((stem, extension)) = filename.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() || extension.is_empty() {
        return false;
    }

    stem.char_indices()
        .filter(|(_, ch)| matches!(ch, '.' | '-' | '_' | '~'))
        .any(|(separator_index, separator)| {
            let candidate_start = separator_index + separator.len_utf8();
            let prefix = &stem[..separator_index];
            let candidate = &stem[candidate_start..];
            !prefix.is_empty() && style.matches_candidate(candidate)
        })
}

/// Debug-only features. All flags default to `false` (off in production).
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DebugConfig {
    /// Expose the JA4/TLS probabilistic identifier debug endpoint at `GET /_ts/debug/ja4`.
    ///
    /// When `false` (the default), the endpoint returns 404. Enable only for
    /// intentional Fastly/browser TLS investigation. The endpoint reflects
    /// Fastly-observed TLS details that browser JS cannot normally read.
    #[serde(default)]
    pub ja4_endpoint_enabled: bool,

    /// Inject a `<!-- ts-debug: ... -->` HTML comment before `</body>` dumping
    /// per-provider auction diagnostics. The default validates response-level
    /// metadata, but bid fields and bounded creative previews remain visible;
    /// this is not a fully anonymized dump. Never enable in production.
    #[serde(default)]
    pub auction_html_comment: bool,

    /// Content and verbosity of the `auction_html_comment` dump. Ignored
    /// when `auction_html_comment` is false.
    ///
    /// The default table must stay omitted from serialized config blobs:
    /// [`DebugConfig`] denies unknown fields, so an older binary rejects a blob
    /// carrying this table during a mixed-version deployment or rollback. Any
    /// non-default table still serializes and requires restoring a compatible
    /// blob before rolling back.
    #[serde(
        default,
        skip_serializing_if = "is_default_auction_debug_comment_options"
    )]
    pub auction_html_comment_options: AuctionDebugCommentOptions,

    /// Enable the testing-only direct GAM-replace path and the verbose per-bid
    /// `debug_bid` blob in `window.tsjs.bids`.
    ///
    /// Note: the sanitized winning `adm` is now injected **unconditionally** for
    /// production inline rendering through the pbRender bridge (see
    /// [`crate::publisher::build_bid_map`]); this flag no longer gates `adm`.
    /// What it still gates is the client-side `debug_bid` signal that turns on
    /// the direct GAM-creative replacement (`injectAdmIntoSlot`), which bypasses
    /// GAM entirely — useful for validating the auction→creative pipeline while
    /// PBS Cache is unavailable. The `debug_bid` blob also carries the raw,
    /// un-sanitized creative for diagnostics, so never enable in production.
    #[serde(default)]
    pub inject_adm_for_testing: bool,

    /// Expose the reusable-sandbox counters endpoint at `GET /_ts/debug/sandbox`
    /// and attach the same counters to private, no-store workload responses.
    ///
    /// The counters are the guest-instance identifier, the request ordinal
    /// within that instance, the application build count, and the request
    /// correlation id. They carry no settings, secrets, or request content.
    /// Cacheable responses omit counters without changing their cache policy;
    /// probes of those routes cannot establish sandbox reuse.
    ///
    /// Independent of the adapter's `reusable-sandbox` Cargo feature by design:
    /// the feature decides whether a `Serve` loop exists, this flag decides
    /// whether counters are emitted. Keeping them separate is what lets the
    /// feature-off baseline be measured on the same channel as the reuse arms.
    ///
    /// Skipped from serialization while false: [`DebugConfig`] denies unknown
    /// fields, so a default blob must stay readable by a binary built before
    /// this field existed. A blob with it enabled requires restoring a
    /// compatible blob before rolling back, the same trade
    /// [`DebugConfig::auction_html_comment_options`] makes.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sandbox_metrics_enabled: bool,
}

/// Serde predicate for omitting `false` flags from serialized config blobs.
fn is_false(value: &bool) -> bool {
    !*value
}

/// Metadata keys safe to surface in the `ts-debug` auction comment.
///
/// Fail-closed superset: any key not listed here — notably `debug`, which
/// carries the resolved `OpenRTB` request (EC ID, `user.ext.eids`, the TC
/// consent string, `device.ip`, `device.geo`) plus per-bidder `httpcalls` —
/// is dropped in [`AuctionDebugCommentVerbosity::Redacted`] mode regardless
/// of what an operator lists in [`AuctionDebugCommentOptions::metadata_keys`].
/// `metadata_keys` is a subset selector against this const, never a way to
/// add new keys.
pub(crate) const AUCTION_DEBUG_METADATA_ALLOWLIST: &[&str] =
    &["error_type", "http_status", "message"];

/// Provider-controlled diagnostic keys exposed only by `Upstream` or `Full`.
///
/// Values remain untyped upstream JSON and may contain request or identity
/// data. Keeping this list separate prevents [`AuctionDebugCommentOptions::metadata_keys`]
/// from widening the default response-metadata boundary.
pub(crate) const AUCTION_DEBUG_UPSTREAM_METADATA_KEYS: &[&str] = &[
    "errors",
    "warnings",
    "responsetimemillis",
    "bidstatus",
    "upstream_message",
    "upstream_message_truncated",
];

fn default_true() -> bool {
    true
}

fn default_auction_debug_metadata_keys() -> Vec<String> {
    AUCTION_DEBUG_METADATA_ALLOWLIST
        .iter()
        .map(std::string::ToString::to_string)
        .collect()
}

// This predicate preserves rollback compatibility by omitting the default table.
fn is_default_auction_debug_comment_options(value: &AuctionDebugCommentOptions) -> bool {
    *value == AuctionDebugCommentOptions::default()
}

// The module selectors are new sections, so a serialized blob that carries
// them is rejected by a base-revision binary that has never heard of them.
// Omitting the default table keeps an unchanged `ts config push` readable
// across a rollout or a rollback.
fn is_default_device_config(value: &DeviceConfig) -> bool {
    *value == DeviceConfig::default()
}

fn is_default_geo_config(value: &GeoConfig) -> bool {
    *value == GeoConfig::default()
}

fn is_default_prefix_config(value: &PrefixConfig) -> bool {
    *value == PrefixConfig::default()
}

fn is_default_permission_signal_config(value: &PermissionSignalConfig) -> bool {
    *value == PermissionSignalConfig::default()
}

/// Message a configuration still carrying the removed `[integrations]` table
/// is rejected with.
const REMOVED_INTEGRATIONS_TABLE_MESSAGE: &str = "Configuration table `[integrations]` was removed. Each module is selected in the \
     section of its type, as [<type>] module = \"<name>\" or modules = [\"<name>\", ...], with its \
     settings in the table at its name, [<type>.<name>], as described in the CHANGELOG.md \
     breaking migration";

/// The removed `[integrations]` table.
///
/// Reading one always fails, with [`REMOVED_INTEGRATIONS_TABLE_MESSAGE`], so
/// the value is never held and the type carries no data.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RemovedIntegrationsTable;

impl<'de> Deserialize<'de> for RemovedIntegrationsTable {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // The value is read and discarded first so that a table, a string or
        // anything else all reach the same message. Reporting a type error
        // instead would send an operator looking for a type problem in a table
        // that has simply moved.
        serde::de::IgnoredAny::deserialize(deserializer)?;
        Err(serde::de::Error::custom(REMOVED_INTEGRATIONS_TABLE_MESSAGE))
    }
}

/// A table that is no longer read under its old name, refused with directions
/// to its new one when a configuration still carries it.
macro_rules! refused_table {
    ($(#[$doc:meta])* $name:ident => $message:expr) => {
        $(#[$doc])*
        #[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
        pub struct $name;

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                // The value is read and discarded first so that a table, a
                // string or anything else all reach the same message. A type
                // error instead would send an operator looking for a type
                // problem in a table that has simply moved.
                serde::de::IgnoredAny::deserialize(deserializer)?;
                Err(serde::de::Error::custom($message))
            }
        }
    };
}

refused_table! {
    /// The `[adserver]` table, renamed `[ad-server]`.
    RenamedAdServerTable => "Configuration table `[adserver]` is now `[ad-server]`. Select the \
        ad server with `[ad-server] module` and move its settings to `[ad-server.<name>]` \
        unchanged"
}

refused_table! {
    /// The `[integration]` table, which is no longer read.
    RemovedIntegrationTable => "Configuration table `[integration]` is no longer read. Each \
        module is selected in the section of its type, as [<type>] module = \"<name>\" or \
        modules = [\"<name>\", ...], with its settings in the table at its name, \
        [<type>.<name>]"
}

refused_table! {
    /// The `[[handlers]]` list, which gated paths behind HTTP Basic
    /// authentication.
    RemovedHandlers => "Configuration list `[[handlers]]` is not read, and no request is \
        asked for a password. Remove every `[[handlers]]` entry. Nothing is served beneath \
        /_ts/admin, so rotate signing keys with `ts keys`, purge the cache with \
        `ts cache purge`, and put a password on a path of the publisher's own at its origin \
        or in the hosting platform"
}

refused_table! {
    /// A store id under `[request_signing]`, which the service does not read.
    RemovedSigningStoreId => "`[request_signing]` does not take `config_store_id` or \
        `secret_store_id`. The service reads its signing keys from the stores linked as \
        `jwks_store` and `signing_keys` and writes neither. Remove both keys, and give the \
        ids to `ts keys rotate` and `ts keys deactivate` as `--config-store-id` and \
        `--secret-store-id`"
}

refused_table! {
    /// The `[tinybird]` table, now `[analytics.tinybird]`.
    MovedTinybirdTable => "Configuration table `[tinybird]` is now `[analytics.tinybird]`. \
        Select it with `[analytics] module = \"tinybird\"`, which replaces its `enabled` \
        line, and move its other settings there unchanged"
}

refused_table! {
    /// The `[permission_signal]` table, renamed `[permission-signal]`.
    RenamedPermissionSignalTable => "Configuration table `[permission_signal]` is now \
        `[permission-signal]`, named exactly as its folder crates/permission-signal. Move its \
        settings there unchanged"
}

/// Behavior of the `<!-- ts-debug: ... -->` auction dump. Only consulted when
/// [`DebugConfig::auction_html_comment`] is true.
///
/// `deny_unknown_fields` matches the convention used by sibling config
/// structs in this file, including the `DebugConfig` this struct nests
/// under: an operator typo (e.g. `metadata_key` instead of `metadata_keys`)
/// must fail config load loudly, not be silently ignored.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuctionDebugCommentOptions {
    /// Include the `provider_responses` section at all.
    #[serde(default = "default_true")]
    pub include_provider_responses: bool,

    /// Include `adserver_response` when an ad server ran.
    #[serde(default = "default_true")]
    pub include_adserver_response: bool,

    /// Include each provider's `bids` array (vs. status/metadata only).
    #[serde(default = "default_true")]
    pub include_bids: bool,

    /// Subset of [`AUCTION_DEBUG_METADATA_ALLOWLIST`] to surface in
    /// [`AuctionDebugCommentVerbosity::Redacted`] mode. This selector cannot
    /// unlock provider diagnostics, and entries outside the fixed allowlist are
    /// rejected at config load by
    /// [`validate_metadata_keys`](Self::validate_metadata_keys).
    ///
    /// [`AuctionDebugCommentVerbosity::Upstream`] builds on the redacted
    /// metadata, so this subset still gates those three keys there; the six
    /// upstream diagnostics are unlocked by `verbosity` alone. Ignored entirely
    /// when `verbosity` is [`AuctionDebugCommentVerbosity::Full`].
    #[serde(default = "default_auction_debug_metadata_keys")]
    pub metadata_keys: Vec<String>,

    /// `Redacted` (default): validated `metadata_keys` subset only, with
    /// creative previews truncated to `MAX_BID_CREATIVE_DUMP_BYTES`.
    /// `Upstream`: redacted fields plus six untyped provider diagnostics;
    /// creative previews remain truncated.
    /// `Full`: raw `response.metadata` verbatim, including the `debug`
    /// subtree (httpcalls/resolvedrequest) when present, and no creative
    /// truncation. The total dump byte cap and comment-terminator
    /// neutralization still apply unconditionally.
    ///
    /// NEVER enable `Upstream` or `Full` in production — identity-bearing
    /// request/response data may become visible via view-source.
    #[serde(default)]
    pub verbosity: AuctionDebugCommentVerbosity,

    /// JSON representation used for the outer auction dump.
    #[serde(default)]
    pub format: AuctionDebugCommentFormat,
}

impl Default for AuctionDebugCommentOptions {
    fn default() -> Self {
        Self {
            include_provider_responses: true,
            include_adserver_response: true,
            include_bids: true,
            metadata_keys: default_auction_debug_metadata_keys(),
            verbosity: AuctionDebugCommentVerbosity::Redacted,
            format: AuctionDebugCommentFormat::Compact,
        }
    }
}

impl AuctionDebugCommentOptions {
    pub(crate) fn normalize(&mut self) {
        self.metadata_keys = self
            .metadata_keys
            .drain(..)
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
            .collect();
    }

    /// Reject [`Self::metadata_keys`] entries outside
    /// [`AUCTION_DEBUG_METADATA_ALLOWLIST`].
    ///
    /// Render time intersects the configured list with the allowlist, so an
    /// entry outside it is dead config that silently renders `metadata: {}`.
    /// Fail the load loudly instead, matching the `deny_unknown_fields`
    /// contract on this struct. The render-time intersection stays as
    /// defense-in-depth for config paths that bypass this check.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Configuration`] naming every unknown key.
    pub(crate) fn validate_metadata_keys(&self) -> Result<(), Report<TrustedServerError>> {
        let unknown: Vec<&str> = self
            .metadata_keys
            .iter()
            .map(String::as_str)
            .filter(|key| !AUCTION_DEBUG_METADATA_ALLOWLIST.contains(key))
            .collect();

        if unknown.is_empty() {
            return Ok(());
        }

        Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "debug.auction_html_comment_options.metadata_keys contains unsupported keys [{}]; supported keys are [{}]",
                unknown.join(", "),
                AUCTION_DEBUG_METADATA_ALLOWLIST.join(", ")
            ),
        }))
    }
}

/// Verbosity of the `ts-debug` auction comment. See
/// [`AuctionDebugCommentOptions::verbosity`].
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuctionDebugCommentVerbosity {
    #[default]
    Redacted,
    Upstream,
    Full,
}

/// JSON representation used for the outer `ts-debug` auction dump.
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuctionDebugCommentFormat {
    #[default]
    Compact,
    Pretty,
}

/// Tester-cookie endpoint configuration.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct TesterCookieConfig {
    /// Enable tester-cookie endpoints that set and clear `ts-tester`.
    #[serde(default)]
    pub enabled: bool,
    /// Lifetime of the `ts-tester` cookie in seconds. Defaults to 30 days.
    ///
    /// Without an explicit lifetime the cookie is session-scoped, and Safari
    /// deletes session cookies when the browser quits, so testers silently
    /// fall back to the baseline arm on their next visit. Safari's ITP may
    /// still cap the effective lifetime (for example to 7 days when the
    /// serving host is a CNAME to a third-party edge), which remains far
    /// better than session scope.
    #[validate(range(
        min = 1,
        max = 31_536_000,
        message = "must be between 1 second and 1 year"
    ))]
    #[serde(default = "TesterCookieConfig::default_max_age_seconds")]
    pub max_age_seconds: u32,
}

impl TesterCookieConfig {
    /// Default tester-cookie lifetime: 30 days.
    #[must_use]
    pub const fn default_max_age_seconds() -> u32 {
        30 * 24 * 60 * 60
    }
}

// A manual impl rather than derive: derive(Default) would zero
// `max_age_seconds` whenever the whole `[tester_cookie]` section is absent,
// because `Settings.tester_cookie` uses `#[serde(default)]`.
impl Default for TesterCookieConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_age_seconds: Self::default_max_age_seconds(),
        }
    }
}

/// Authenticated forwarding configuration for a trusted client IP header.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
#[validate(schema(function = validate_trusted_client_ip))]
pub struct TrustedClientIpConfig {
    /// Header containing the client IP address supplied by the trusted edge.
    pub ip_header: String,
    /// Header containing the shared-secret authentication value.
    pub auth_header: String,
    /// Shared secret required before accepting the forwarded client IP address.
    #[validate(custom(function = validate_trusted_client_ip_shared_secret))]
    pub shared_secret: Redacted<String>,
}

impl TrustedClientIpConfig {
    /// Placeholder shared secrets shipped in the example configuration and docs.
    pub const SHARED_SECRET_PLACEHOLDERS: &[&str] = &["replace-with-a-random-shared-secret"];

    /// Minimum accepted `shared_secret` length.
    ///
    /// Matches `Ec::MIN_PASSPHRASE_LENGTH`. This secret is the only gate on
    /// forging the client address that geolocation, EC identity derivation, and
    /// bot protection consume, so it is held to the same strength as the EC
    /// passphrase.
    const MIN_SHARED_SECRET_LENGTH: usize = Ec::MIN_PASSPHRASE_LENGTH;

    /// Returns `true` if `shared_secret` matches a known placeholder value
    /// (case-insensitive).
    #[must_use]
    pub fn is_placeholder_shared_secret(shared_secret: &str) -> bool {
        Self::SHARED_SECRET_PLACEHOLDERS
            .iter()
            .any(|p| p.eq_ignore_ascii_case(shared_secret))
    }

    /// Returns whether `candidate` exactly matches the configured shared secret.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::redacted::Redacted;
    /// use trusted_server_core::settings::TrustedClientIpConfig;
    ///
    /// let config = TrustedClientIpConfig {
    ///     ip_header: "fastly-client-ip".to_owned(),
    ///     auth_header: "x-trusted-client-auth".to_owned(),
    ///     shared_secret: Redacted::new("fictional-shared-secret-0123456789".to_owned()),
    /// };
    ///
    /// assert!(config.authenticates("fictional-shared-secret-0123456789"));
    /// assert!(!config.authenticates("fictional-wrong-secret"));
    /// ```
    #[must_use]
    pub fn authenticates(&self, candidate: &str) -> bool {
        let configured_digest = Sha256::digest(self.shared_secret.expose().as_bytes());
        let candidate_digest = Sha256::digest(candidate.as_bytes());

        configured_digest.ct_eq(&candidate_digest).into()
    }
}

fn validate_trusted_client_ip(config: &TrustedClientIpConfig) -> Result<(), ValidationError> {
    let ip_header = http::HeaderName::from_bytes(config.ip_header.as_bytes())
        .map_err(|_| ValidationError::new("invalid_trusted_client_ip_header"))?;
    let auth_header = http::HeaderName::from_bytes(config.auth_header.as_bytes())
        .map_err(|_| ValidationError::new("invalid_trusted_client_ip_auth_header"))?;

    if ip_header == auth_header {
        return Err(ValidationError::new("identical_trusted_client_ip_headers"));
    }

    for header in [&ip_header, &auth_header] {
        if INTERNAL_HEADERS.contains(&header.as_str()) {
            return Err(ValidationError::new("reserved_trusted_client_ip_header"));
        }
    }

    if ip_header.as_str() != "fastly-client-ip" && !ip_header.as_str().starts_with("x-") {
        return Err(ValidationError::new("unsafe_trusted_client_ip_header"));
    }
    if !auth_header.as_str().starts_with("x-") {
        return Err(ValidationError::new("unsafe_trusted_client_ip_auth_header"));
    }

    Ok(())
}

fn validate_trusted_client_ip_shared_secret(
    shared_secret: &Redacted<String>,
) -> Result<(), ValidationError> {
    let shared_secret = shared_secret.expose();
    if shared_secret.len() < TrustedClientIpConfig::MIN_SHARED_SECRET_LENGTH {
        return Err(ValidationError::new(
            "short_trusted_client_ip_shared_secret",
        ));
    }
    if !shared_secret
        .bytes()
        .all(|byte| matches!(byte, b'!'..=b'~'))
    {
        return Err(ValidationError::new(
            "invalid_trusted_client_ip_shared_secret",
        ));
    }

    Ok(())
}

#[derive(Debug, Default, Clone, Deserialize, Serialize, Validate)]
pub struct Settings {
    #[validate(nested)]
    pub publisher: Publisher,
    #[serde(default)]
    #[validate(nested)]
    pub tester_cookie: TesterCookieConfig,
    /// Optional authenticated trusted client IP forwarding configuration.
    ///
    /// `None` must stay omitted from serialized config blobs: `Settings`
    /// schemas that predate this field reject unknown keys, so emitting
    /// `trusted_client_ip: null` would make an unchanged `ts config push`
    /// break older instances during rollout or rollback. A configured value
    /// remains serialized and requires restoring a compatible blob before
    /// rolling back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
    pub trusted_client_ip: Option<TrustedClientIpConfig>,
    /// Optional `robots.txt`, assembled by core from the contributors
    /// `[robots-txt] modules` selects.
    ///
    /// `None` leaves `/robots.txt` to the publisher's origin, and must stay
    /// omitted from serialized config blobs for the reason given on
    /// `trusted_client_ip`.
    #[serde(
        rename = "robots-txt",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    #[validate(nested)]
    pub robots_txt: Option<crate::robots_txt::RobotsTxtConfig>,
    #[serde(default)]
    #[validate(nested)]
    pub ec: Ec,
    /// The removed `[integrations]` table.
    ///
    /// The name is kept so a configuration written for the previous release is
    /// told where its blocks moved, rather than being handed a bare
    /// unknown-field error listing every table Trusted Server accepts. The
    /// field never holds a value, because reading one always fails.
    #[serde(default, skip_serializing)]
    #[allow(
        dead_code,
        reason = "the field exists so that reading the removed table fails with directions"
    )]
    integrations: RemovedIntegrationsTable,
    /// The `[integration]` table, kept so a configuration carrying it is
    /// told where modules are selected now.
    #[serde(default, skip_serializing)]
    #[allow(
        dead_code,
        reason = "the field exists so that reading the removed table fails with directions"
    )]
    integration: RemovedIntegrationTable,
    /// The sections of module types core does not read itself, such as
    /// `[cmp]` or `[tag]`, each named for the type of module it selects.
    #[serde(flatten)]
    pub sections: TypeSections,
    /// The `[[handlers]]` list, kept so a configuration carrying one is
    /// told that no path is gated by a password.
    #[serde(default, skip_serializing)]
    #[allow(
        dead_code,
        reason = "the field exists so that reading the removed list fails with directions"
    )]
    handlers: RemovedHandlers,
    #[serde(default, deserialize_with = "map_from_obj_or_str")]
    pub response_headers: HashMap<String, String>,
    pub request_signing: Option<RequestSigning>,
    #[serde(default)]
    #[validate(nested)]
    pub rewrite: Rewrite,
    #[serde(default)]
    #[validate(nested)]
    pub auction: AuctionConfig,
    /// The auction's demand sources. `[demand] modules` selects them and each
    /// `[demand.<name>]` table holds one source's settings.
    #[serde(default, skip_serializing_if = "ProviderList::is_unset")]
    pub demand: ProviderList,
    /// The ad server that picks the auction winner. `[ad-server] module`
    /// selects it and `[ad-server.<name>]` holds its settings.
    #[serde(
        rename = "ad-server",
        default,
        skip_serializing_if = "ProviderChoice::is_unset"
    )]
    pub adserver: ProviderChoice,
    /// The `[adserver]` table, kept so a configuration carrying it is told
    /// its new name.
    #[serde(rename = "adserver", default, skip_serializing)]
    #[allow(
        dead_code,
        reason = "the field exists so that reading the renamed table fails with directions"
    )]
    renamed_adserver: RenamedAdServerTable,
    #[serde(default)]
    pub consent: ConsentConfig,
    #[serde(default)]
    pub cache: CacheSettings,
    /// What one vendor's modules share, a table per vendor, `[shared.<vendor>]`.
    #[serde(default, skip_serializing_if = "SharedTables::is_empty")]
    pub shared: SharedTables,
    /// A middleware's own settings, a table per middleware, `[middleware.<name>]`.
    #[serde(default, skip_serializing_if = "MiddlewareTables::is_empty")]
    pub middleware: MiddlewareTables,
    #[serde(default)]
    pub proxy: Proxy,
    #[serde(default)]
    pub creative_opportunities: Option<CreativeOpportunitiesConfig>,
    #[serde(default)]
    pub image_optimizer: ImageOptimizerSettings,
    /// Analytics. `[analytics] module` selects the module that runs, with its
    /// settings in the table at its name. Omitted from a serialized
    /// configuration when absent, for the reason given on `trusted_client_ip`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analytics: Option<AnalyticsConfig>,
    /// The `[tinybird]` table, kept so a configuration carrying it is told
    /// where it moved.
    #[serde(rename = "tinybird", default, skip_serializing)]
    #[allow(
        dead_code,
        reason = "the field exists so that reading the moved table fails with directions"
    )]
    moved_tinybird: MovedTinybirdTable,
    #[serde(default)]
    pub debug: DebugConfig,
    #[serde(default, skip_serializing_if = "is_default_device_config")]
    #[validate(nested)]
    pub device: DeviceConfig,
    #[serde(default, skip_serializing_if = "is_default_geo_config")]
    #[validate(nested)]
    pub geo: GeoConfig,
    /// The prefix module, `[prefix] module`, see [`crate::prefix`].
    #[serde(default, skip_serializing_if = "is_default_prefix_config")]
    #[validate(nested)]
    pub prefix: PrefixConfig,
    #[serde(
        rename = "permission-signal",
        default,
        skip_serializing_if = "is_default_permission_signal_config"
    )]
    #[validate(nested)]
    pub permission_signal: PermissionSignalConfig,
    /// The permission policy file this domain runs.
    #[serde(default, skip_serializing_if = "PermissionsConfig::is_empty")]
    pub permissions: PermissionsConfig,
    /// The `[permission_signal]` table, kept so a configuration carrying it is
    /// told its new name.
    #[serde(rename = "permission_signal", default, skip_serializing)]
    #[allow(
        dead_code,
        reason = "the field exists so that reading the renamed table fails with directions"
    )]
    renamed_permission_signal: RenamedPermissionSignalTable,
    /// What the configuration page at `/_ts/config` shows.
    #[serde(
        default,
        skip_serializing_if = "crate::inspect::config::InspectConfig::is_default"
    )]
    pub inspect: crate::inspect::config::InspectConfig,
    /// The page changes run on a document fetched from the origin, written
    /// as `[[fetch]]` entries. See [`crate::middleware`].
    #[serde(default, skip_serializing_if = "PhaseEntries::is_empty")]
    pub fetch: PhaseEntries,
    /// The page changes run on each reader's copy of a document, written as
    /// `[[serve]]` entries.
    #[serde(default, skip_serializing_if = "PhaseEntries::is_empty")]
    pub serve: PhaseEntries,
    /// Where the loader wrote secrets, which the configuration view masks.
    /// Never read from a document and never written to one.
    #[serde(skip)]
    resolved_secrets: crate::secret_resolution::ResolvedSecrets,
    /// The policy the runtime loader read and checked for `permissions`.
    #[serde(skip)]
    permission_policy: Option<std::sync::Arc<crate::permissions::PermissionPolicy>>,
}

impl Settings {
    /// Where the loader wrote secrets, for the configuration view.
    #[must_use]
    pub(crate) fn resolved_secrets(&self) -> &crate::secret_resolution::ResolvedSecrets {
        &self.resolved_secrets
    }

    /// Records where the loader wrote secrets.
    pub(crate) fn set_resolved_secrets(
        &mut self,
        secrets: crate::secret_resolution::ResolvedSecrets,
    ) {
        self.resolved_secrets = secrets;
    }

    /// The permission policy this domain loaded.
    ///
    /// The runtime loader attaches it, and refuses a domain without one, so
    /// only settings built some other way, as in a test, have none.
    #[must_use]
    pub fn permission_policy(
        &self,
    ) -> Option<&std::sync::Arc<crate::permissions::PermissionPolicy>> {
        self.permission_policy.as_ref()
    }

    /// The permission policy this domain loaded, which an adapter hands to
    /// every request.
    ///
    /// # Errors
    ///
    /// [`TrustedServerError::Configuration`] when the settings carry no
    /// policy, because the runtime loader did not load them.
    pub fn required_permission_policy(
        &self,
    ) -> Result<std::sync::Arc<crate::permissions::PermissionPolicy>, Report<TrustedServerError>>
    {
        self.permission_policy.clone().ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: "the settings carry no permission policy, which the runtime loader \
                          attaches when it loads them"
                    .to_owned(),
            })
        })
    }

    /// Keeps the policy the runtime loader read for these settings.
    pub(crate) fn attach_permission_policy(
        &mut self,
        policy: std::sync::Arc<crate::permissions::PermissionPolicy>,
    ) {
        self.permission_policy = Some(policy);
    }

    /// The same settings carrying `policy`, as the runtime loader would
    /// attach it, for tests.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub fn with_permission_policy(
        mut self,
        policy: std::sync::Arc<crate::permissions::PermissionPolicy>,
    ) -> Self {
        self.permission_policy = Some(policy);
        self
    }

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
    /// Runtime config-store loading uses this after verifying the `app_config`
    /// blob envelope and extracting the same typed settings shape.
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
    /// Runtime loading does not use this legacy helper; `EdgeZero` CLI app-config
    /// overlays are applied before deserializing [`crate::config::TrustedServerAppConfig`].
    /// This helper remains available to existing tests that exercise legacy
    /// parsing behavior. Build inputs such as `TRUSTED_SERVER__GIT_VERSION`
    /// are not treated as overrides.
    ///
    /// # Errors
    ///
    /// - [`TrustedServerError::Configuration`] if the TOML is invalid or missing required fields
    #[cfg(test)]
    pub fn from_toml_and_env(toml_str: &str) -> Result<Self, Report<TrustedServerError>> {
        let overrides = std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .filter(|(key, _)| {
                !BUILD_ENVIRONMENT_VARIABLES
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(key))
            })
            .collect();
        let environment = Environment::default()
            .prefix(ENVIRONMENT_VARIABLE_PREFIX)
            .separator(ENVIRONMENT_VARIABLE_SEPARATOR)
            .source(Some(overrides));

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

    /// The analytics module's settings, when `[analytics]` selects it.
    #[must_use]
    pub fn analytics_module(&self) -> Option<&TinybirdSettings> {
        self.analytics
            .as_ref()
            .and_then(AnalyticsConfig::analytics_module)
    }

    /// Normalize settings after deserialization.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if image optimizer keys collide after trimming.
    pub(crate) fn normalize_deserialized(&mut self) -> Result<(), Report<TrustedServerError>> {
        self.cache.normalize();
        self.publisher.normalize();
        self.proxy.normalize();
        self.image_optimizer.normalize()?;
        self.debug.auction_html_comment_options.normalize();
        if let Some(analytics) = &mut self.analytics {
            analytics.normalize();
        }
        self.consent.validate();
        Ok(())
    }

    pub(crate) fn finalize_deserialized(
        mut settings: Self,
        validation_label: &str,
    ) -> Result<Self, Report<TrustedServerError>> {
        settings.normalize_deserialized()?;
        settings.prepare_runtime()?;

        settings.validate().map_err(|err| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "{validation_label} validation failed: {}",
                    validation_error_summary(&err)
                ),
            })
        })?;

        settings.ec.migrate_legacy_ec_layout()?;
        settings.ec.validate_module_selection()?;
        settings.ec.validate_resolve_allowed_origins()?;
        settings.validate_module_sections()?;
        settings.validate_phase_entries()?;
        settings.device.validate_module_selection()?;
        settings.geo.validate_module_selection()?;
        settings
            .geo
            .validate_jurisdiction_acknowledgment(&settings.ec)?;

        Ok(settings)
    }

    /// Eagerly prepare runtime-only settings artifacts.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if any cached runtime artifact cannot be
    /// prepared, if any handler path regex does not compile, if a creative
    /// opportunity slot is invalid, if `publisher.origin_auth` cannot sign, or
    /// if [`AuctionDebugCommentOptions::metadata_keys`] names an unsupported
    /// key.
    pub fn prepare_runtime(&mut self) -> Result<(), Report<TrustedServerError>> {
        self.image_optimizer.prepare_runtime()?;
        self.cache.prepare_runtime()?;
        self.publisher.prepare_runtime()?;
        self.proxy.prepare_runtime()?;
        if let Some(analytics) = &mut self.analytics {
            analytics.prepare_runtime()?;
        }
        self.debug
            .auction_html_comment_options
            .validate_metadata_keys()?;
        self.validate_asset_image_optimizer_profile_sets()?;
        self.shared.validate_names()?;
        self.middleware.validate_names()?;
        if let Some(robots_txt) = &self.robots_txt {
            robots_txt.prepare_runtime()?;
            self.require_robots_txt_placed()?;
        }

        if let Some(co) = &mut self.creative_opportunities {
            co.compile_slots();
            // Parse `gam_unit_path` templates once here (mirrors the compiled
            // glob cache) so request-time rendering is substitution-only.
            co.compile_unit_templates().map_err(|err| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("Invalid creative opportunity gam_unit_path template: {err}"),
                })
            })?;
            // Slots flow into injected HTML/JS, provider payloads, and GPT
            // calls. Env/private config can bypass static review, so validate
            // the full runtime shape on every load path.
            co.validate_runtime().map_err(|err| {
                Report::new(TrustedServerError::Configuration {
                    message: format!("Invalid creative opportunity slot config: {err}"),
                })
            })?;
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

    /// Refuses a `[robots-txt]` section unless a `[[fetch]]` entry for
    /// `text/plain` with `path = "/robots.txt"` names the `robots_txt`
    /// middleware, and refuses any entry naming it on another path or media
    /// type. Only a `[[fetch]]` entry makes the file where the origin has
    /// none, and an entry on another path would replace that file with the
    /// robots.txt rules.
    fn require_robots_txt_placed(&self) -> Result<(), Report<TrustedServerError>> {
        use crate::robots_txt::ROBOTS_TXT_PATH;
        use crate::robots_txt::middleware::{MEDIA_TYPE, MODULE};

        let names = |entry: &crate::middleware::PhaseEntry| {
            entry.middleware.iter().any(|name| name == MODULE)
        };
        let on_the_file = |entry: &crate::middleware::PhaseEntry| {
            entry.media_type == MEDIA_TYPE && entry.path.as_deref() == Some(ROBOTS_TXT_PATH)
        };
        for (phase, entries) in [("fetch", &self.fetch), ("serve", &self.serve)] {
            for (index, entry) in entries.entries().iter().enumerate() {
                if names(entry) && !on_the_file(entry) {
                    return Err(Report::new(TrustedServerError::Settings {
                        message: format!(
                            "[[{phase}]] entry {} names `{MODULE}` for `{}` on {}, where it \
                             would replace each file it covers with robots.txt. Name it only \
                             in an entry with media_type = `{MEDIA_TYPE}` and path = \
                             `{ROBOTS_TXT_PATH}`",
                            index + 1,
                            entry.media_type,
                            entry.path.as_deref().map_or_else(
                                || "every path".to_owned(),
                                |path| format!("`{path}`")
                            ),
                        ),
                    }));
                }
            }
        }
        if self
            .fetch
            .entries()
            .iter()
            .any(|entry| names(entry) && on_the_file(entry))
        {
            return Ok(());
        }
        Err(Report::new(TrustedServerError::Settings {
            message: format!(
                "[robots-txt] is set, but no [[fetch]] entry with media_type = `{MEDIA_TYPE}` and \
                 path = `{ROBOTS_TXT_PATH}` names `{MODULE}`, so the file would never be made \
                 where the origin has none. Add that entry"
            ),
        }))
    }

    /// Returns compiled creative opportunity slots when template delivery is enabled.
    #[must_use]
    pub fn creative_opportunity_slots(
        &self,
    ) -> &[crate::creative_opportunities::CreativeOpportunitySlot] {
        self.creative_opportunities
            .as_ref()
            .filter(|co| co.enabled)
            .map(|co| co.slot.as_slice())
            .unwrap_or(&[])
    }

    /// Rejects known placeholder secret values.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::InsecureDefault`] when one or more secret
    /// fields still contain a placeholder value.
    pub fn reject_placeholder_secrets(&self) -> Result<(), Report<TrustedServerError>> {
        let mut insecure_fields: Vec<String> = Vec::new();

        for (name, hmac) in self.ec.module_blocks.hmac_blocks() {
            if Ec::is_placeholder_passphrase(hmac.passphrase.expose()) {
                insecure_fields.push(format!("ec.{name}.passphrase"));
            }
        }
        for (name, host_signals) in self.ec.module_blocks.host_signals_blocks() {
            if Ec::is_placeholder_passphrase(host_signals.passphrase.expose()) {
                insecure_fields.push(format!("ec.{name}.passphrase"));
            }
        }
        if Publisher::is_placeholder_proxy_secret(self.publisher.proxy_secret.expose()) {
            insecure_fields.push("publisher.proxy_secret".to_owned());
        }
        if let Some(trusted_client_ip) = &self.trusted_client_ip
            && TrustedClientIpConfig::is_placeholder_shared_secret(
                trusted_client_ip.shared_secret.expose(),
            )
        {
            insecure_fields.push("trusted_client_ip.shared_secret".to_owned());
        }
        for partner in &self.ec.partners {
            if partner
                .api_token
                .as_ref()
                .is_some_and(|token| EcPartner::is_placeholder_api_token(token.expose()))
            {
                insecure_fields.push(format!("ec.partners[{}].api_token", partner.source_domain));
            }
            if partner
                .ts_pull_token
                .as_ref()
                .is_some_and(|token| EcPartner::is_placeholder_api_token(token.expose()))
            {
                insecure_fields.push(format!(
                    "ec.partners[{}].ts_pull_token",
                    partner.source_domain
                ));
            }
        }
        if Publisher::is_placeholder_domain(&self.publisher.domain) {
            insecure_fields.push("publisher.domain".to_owned());
        }
        if Publisher::is_placeholder_cookie_domain(&self.publisher.cookie_domain) {
            insecure_fields.push("publisher.cookie_domain".to_owned());
        }
        if Publisher::is_placeholder_origin_url(&self.publisher.origin_url) {
            insecure_fields.push("publisher.origin_url".to_owned());
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

    /// Resolve the first matching configured asset cache policy for the request path.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if matcher preparation unexpectedly fails.
    pub fn asset_cache_policy_for_path(
        &self,
        path: &str,
    ) -> Result<Option<CachePolicy>, Report<TrustedServerError>> {
        self.cache.asset_policy_for_path(path)
    }

    /// Resolve the longest matching asset route for the request path.
    #[must_use]
    pub fn asset_route_for_path(&self, path: &str) -> Option<&ProxyAssetRoute> {
        self.proxy.asset_route_for_path(path)
    }

    /// Every section that selects modules, with its name: `[proxy]`,
    /// `[auction]` and each module type's own section.
    pub fn module_sections(&self) -> impl Iterator<Item = (&str, &SectionModules)> {
        [
            ("proxy", &self.proxy.modules),
            ("auction", &self.auction.modules),
        ]
        .into_iter()
        .chain(self.sections.iter())
    }

    /// The section that selects the module `name`, and the name as written
    /// there, whether in full or with the section's type folder left off.
    #[must_use]
    pub fn module_selection(&self, name: &str) -> Option<(&str, &str)> {
        self.module_sections().find_map(|(section, modules)| {
            modules
                .selected()
                .iter()
                .find(|written| crate::module_name::resolve(section, written, &[name]).is_some())
                .map(|written| (section, written.as_str()))
        })
    }

    /// Whether a section selects the module `name`.
    #[must_use]
    pub fn selects_module(&self, name: &str) -> bool {
        self.module_selection(name).is_some()
    }

    /// The table `section` holds at the name `written`, as it was written,
    /// or an empty one when the section holds none.
    #[must_use]
    pub fn section_table(
        &self,
        section: &str,
        written: &str,
    ) -> serde_json::Map<String, JsonValue> {
        self.module_sections()
            .find(|(candidate, _)| *candidate == section)
            .and_then(|(_, modules)| modules.settings_of(written))
            .cloned()
            .unwrap_or_default()
    }

    /// Reads and validates a selected module's settings, from the table at the
    /// name it is written under beneath the section that selects it, or
    /// returns `None` when no section selects it.
    ///
    /// A selected module with no table reads an empty one, so a module that
    /// takes no settings runs on its selection alone and one that requires a
    /// setting reports the setting it is missing. The parse and validation
    /// messages name the setting at fault in the outermost message, because a
    /// report renders only that in `ts config validate`.
    ///
    /// # Errors
    ///
    /// When the table cannot be read as `T` or fails its validation, naming the
    /// table and the setting at fault but never the setting's value.
    pub fn module_config<T>(&self, name: &str) -> Result<Option<T>, Report<TrustedServerError>>
    where
        T: IntegrationConfig,
    {
        let Some((section, written)) = self.module_selection(name) else {
            return Ok(None);
        };
        let table = self
            .module_sections()
            .find(|(candidate, _)| *candidate == section)
            .and_then(|(_, modules)| modules.settings_of(written))
            .cloned()
            .unwrap_or_default();
        read_table(&format!("{section}.{written}"), table).map(Some)
    }

    /// Reads `[shared.<vendor>]`, the settings every module of `vendor`
    /// shares, or `None` when the table is absent.
    ///
    /// # Errors
    ///
    /// When the table cannot be read as `T` or fails its validation, naming
    /// the table.
    pub fn shared_config<T>(&self, vendor: &str) -> Result<Option<T>, Report<TrustedServerError>>
    where
        T: DeserializeOwned + Validate,
    {
        self.shared.get_typed(vendor)
    }

    /// Reads `[middleware.<name>]`, a middleware's own settings, as `T`, or
    /// `None` when no table stands at `name`. A module's one middleware may
    /// read the module's own table instead, as the stock modules do.
    ///
    /// # Errors
    ///
    /// When the table carries a key `T` does not know or a value that fails
    /// validation.
    pub fn middleware_config<T>(&self, name: &str) -> Result<Option<T>, Report<TrustedServerError>>
    where
        T: DeserializeOwned + Validate,
    {
        self.middleware.get_typed(name)
    }

    /// The modules `section` selects, to change, with the section created
    /// empty when it is a module type's and absent.
    fn section_modules_mut(&mut self, section: &str) -> &mut SectionModules {
        match section {
            "proxy" => &mut self.proxy.modules,
            "auction" => &mut self.auction.modules,
            _ => self.sections.section_mut(section),
        }
    }

    /// Selects the module `name` in `section`, written with the section's
    /// type folder left off, with no table of its own.
    pub fn select_module(&mut self, section: &str, name: &str) {
        let written = crate::module_name::short_form(section, name).to_owned();
        self.section_modules_mut(section).select(&written);
    }

    /// Stops selecting the module `name` in `section` and drops its table.
    pub fn remove_module(&mut self, section: &str, name: &str) {
        let written = crate::module_name::short_form(section, name).to_owned();
        self.section_modules_mut(section).remove(&written);
    }

    /// Selects the module `name` in `section`, with `settings` as its table,
    /// which is what writing both in a document does.
    ///
    /// The section is the name's type folder, except for the sections that
    /// Trusted Server reads itself and that also select modules, where the
    /// name is written in full.
    ///
    /// # Errors
    ///
    /// Returns an error if `settings` cannot be serialized to a table.
    pub fn insert_module_config<T>(
        &mut self,
        section: &str,
        name: &str,
        settings: &T,
    ) -> Result<(), Report<TrustedServerError>>
    where
        T: Serialize,
    {
        let JsonValue::Object(table) =
            serde_json::to_value(settings).change_context(TrustedServerError::Configuration {
                message: "Failed to serialize module configuration".to_string(),
            })?
        else {
            return Err(Report::new(TrustedServerError::Configuration {
                message: format!("the settings of `{name}` are not a table"),
            }));
        };
        let written = crate::module_name::short_form(section, name).to_owned();
        self.section_modules_mut(section).insert(&written, table);
        Ok(())
    }

    /// The entries of `phase`, in the order written.
    #[must_use]
    pub fn phase_entries(&self, phase: MiddlewarePhase) -> &PhaseEntries {
        match phase {
            MiddlewarePhase::Fetch => &self.fetch,
            MiddlewarePhase::Serve => &self.serve,
        }
    }

    /// Checks the shape of each phase's entries, see
    /// [`PhaseEntries::validate`]. Whether a name is one a running module
    /// supplies is only knowable where the registry is built, so it is
    /// checked there.
    ///
    /// # Errors
    ///
    /// Naming the entry at fault.
    pub fn validate_phase_entries(&self) -> Result<(), Report<TrustedServerError>> {
        for phase in MiddlewarePhase::ALL {
            self.phase_entries(phase)
                .validate(phase)
                .map_err(|message| Report::new(TrustedServerError::Configuration { message }))?;
        }
        Ok(())
    }

    /// Checks every section's selection and tables, and that no module is
    /// selected in two sections.
    ///
    /// # Errors
    ///
    /// Naming the section at fault.
    pub fn validate_module_sections(&self) -> Result<(), Report<TrustedServerError>> {
        self.sections.validate()?;
        for (section, modules) in [
            ("proxy", &self.proxy.modules),
            ("auction", &self.auction.modules),
        ] {
            modules
                .validate(section)
                .map_err(|message| Report::new(TrustedServerError::Configuration { message }))?;
        }
        let mut seen: Vec<(&str, String)> = Vec::new();
        for (section, modules) in self.module_sections() {
            for written in modules.selected() {
                let full = if written.contains('.') {
                    written.clone()
                } else {
                    format!("{section}.{written}")
                };
                if let Some((earlier, _)) = seen.iter().find(|(_, name)| *name == full) {
                    return Err(Report::new(TrustedServerError::Configuration {
                        message: format!(
                            "`{written}` is selected in both [{earlier}] and [{section}]. A \
                             module is selected in one section"
                        ),
                    }));
                }
                seen.push((section, full));
            }
        }
        Ok(())
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

/// Longest `directory_index` accepted, the usual limit on one file name.
const MAX_DIRECTORY_INDEX_BYTES: usize = 255;

/// Accepts a single file name that can be added to a path as written.
///
/// The name is added to the origin request without encoding, so it is limited
/// to the characters a path segment carries unescaped. A `/` would reach into
/// another folder, `%` would let an escape stand for one, and `.` or `..`
/// would name a folder rather than a file.
fn validate_directory_index(value: &str) -> Result<(), ValidationError> {
    let is_segment_byte = |byte: u8| {
        byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'.'
                    | b'_'
                    | b'~'
                    | b'!'
                    | b'$'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'*'
                    | b'+'
                    | b','
                    | b';'
                    | b'='
                    | b':'
                    | b'@'
            )
    };
    let is_file_name = !value.is_empty()
        && value.len() <= MAX_DIRECTORY_INDEX_BYTES
        && value != "."
        && value != ".."
        && value.bytes().all(is_segment_byte);
    if !is_file_name {
        let mut err = ValidationError::new("invalid_directory_index");
        err.add_param("value".into(), &value);
        err.message = Some(
            "directory_index must be one file name such as `index.html`, without `/`, `%`, \
             spaces, a query or a fragment"
                .into(),
        );
        return Err(err);
    }
    Ok(())
}

fn validation_error_summary(errors: &validator::ValidationErrors) -> String {
    fn walk(errors: &validator::ValidationErrors, prefix: &str, messages: &mut Vec<String>) {
        let mut fields = errors
            .errors()
            .keys()
            .map(AsRef::as_ref)
            .collect::<Vec<_>>();
        fields.sort_unstable();

        for field in fields {
            let path = if prefix.is_empty() {
                field.to_owned()
            } else {
                format!("{prefix}.{field}")
            };
            let Some(kind) = errors.errors().get(field) else {
                continue;
            };
            match kind {
                validator::ValidationErrorsKind::Field(validations) => {
                    // A validator's message, where it gives one, says why the
                    // value was refused, which the code alone does not.
                    for validation in validations {
                        messages.push(match &validation.message {
                            Some(message) => format!("{path}: {} ({message})", validation.code),
                            None => format!("{path}: {}", validation.code),
                        });
                    }
                }
                validator::ValidationErrorsKind::Struct(inner) => {
                    walk(inner, &path, messages);
                }
                validator::ValidationErrorsKind::List(items) => {
                    for (index, inner) in items {
                        walk(inner, &format!("{path}[{index}]"), messages);
                    }
                }
            }
        }
    }

    let mut messages = Vec::new();
    walk(errors, "", &mut messages);
    messages.join(", ")
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
// This lets env vars such as
// TRUSTED_SERVER__AUCTION__PREBID__CLIENT_SIDE_BIDDERS__0=example-browser work;
// the config env source represents the value as an object rather than a sequence.
// String inputs may also be JSON arrays or comma-separated values.
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

/// Reads a list setting written as a sequence, as a map keyed by position,
/// or as a string holding a JSON array or comma-separated values.
///
/// The map and string forms are what an environment overlay produces, so a
/// list reads the same from a file and from the environment. Public so a
/// module crate's own list settings read the same way.
///
/// # Errors
///
/// When the value is none of those forms, a map key is not a position, or an
/// item cannot be read as `T`.
pub fn vec_from_seq_or_map<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
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
    use std::collections::BTreeSet;

    use crate::ec::resolve::NotAnOrigin;

    /// `DebugConfig` denies unknown fields, so a binary built before
    /// `sandbox_metrics_enabled` existed must still accept a default blob.
    /// That only holds while the flag is skipped during serialization.
    #[test]
    fn default_debug_config_omits_sandbox_metrics_for_rollback() {
        let serialized =
            serde_json::to_value(DebugConfig::default()).expect("should serialize debug config");

        assert!(
            serialized.get("sandbox_metrics_enabled").is_none(),
            "a default blob must not carry the field, or an older binary rejects it: {serialized}"
        );
    }

    #[test]
    fn enabled_sandbox_metrics_serializes_and_round_trips() {
        let config = DebugConfig {
            sandbox_metrics_enabled: true,
            ..DebugConfig::default()
        };

        let serialized = serde_json::to_value(&config).expect("should serialize debug config");
        assert_eq!(
            serialized.get("sandbox_metrics_enabled"),
            Some(&json!(true)),
            "an enabled flag must be written so the setting survives a round trip"
        );

        let restored: DebugConfig =
            serde_json::from_value(serialized).expect("should deserialize debug config");
        assert!(
            restored.sandbox_metrics_enabled,
            "the flag should survive a round trip"
        );
    }

    #[test]
    fn debug_config_accepts_a_blob_without_the_sandbox_field() {
        let restored: DebugConfig = serde_json::from_value(json!({"ja4_endpoint_enabled": true}))
            .expect("should deserialize a blob written before the field existed");

        assert!(
            restored.ja4_endpoint_enabled,
            "existing fields should still load"
        );
        assert!(
            !restored.sandbox_metrics_enabled,
            "an absent flag should default to off"
        );
    }

    use crate::integrations::IntegrationRegistry;
    use crate::redacted::Redacted;
    use crate::test_support::tests::{
        crate_test_settings_str, crate_test_settings_str_with_ec_section, create_test_settings,
        hmac_passphrase, select_hmac_module,
    };

    fn trusted_client_ip_toml(ip_header: &str, auth_header: &str, shared_secret: &str) -> String {
        format!(
            "{}\n[trusted_client_ip]\nip_header = \"{ip_header}\"\nauth_header = \"{auth_header}\"\nshared_secret = \"{shared_secret}\"\n",
            crate_test_settings_str()
        )
    }

    #[test]
    fn trusted_client_ip_is_absent_by_default() {
        let settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should parse settings without trusted client IP configuration");

        assert!(
            settings.trusted_client_ip.is_none(),
            "should leave trusted client IP configuration disabled by default"
        );
    }

    fn settings_str_with_robots_txt(section: &str) -> String {
        format!(
            "{}\n[robots-txt]\n{section}\n\n[[fetch]]\nmedia_type = \"text/plain\"\npath = \"/robots.txt\"\nmiddleware = [\"robots_txt\"]\n",
            crate_test_settings_str()
        )
    }

    #[test]
    fn a_robots_txt_section_with_no_entry_placing_the_middleware_is_refused() {
        let toml = format!(
            "{}\n[robots-txt]\nmodules = \"refuse_all\"\n",
            crate_test_settings_str()
        );

        let error = Settings::from_toml(&toml).expect_err("the file would never be made");

        let message = format!("{error:?}");
        assert!(
            message.contains(
                "no [[fetch]] entry with media_type = `text/plain` and path = `/robots.txt` names \
                 `robots_txt`"
            ),
            "{message}"
        );
    }

    /// The `[[fetch]]` or `[[serve]]` entry placing the middleware on
    /// `/robots.txt`.
    const ROBOTS_TXT_ENTRY: &str =
        "media_type = \"text/plain\"\npath = \"/robots.txt\"\nmiddleware = [\"robots_txt\"]\n";

    /// Settings with `[robots-txt]` refusing every crawler and `entries`.
    fn settings_str_placing_robots_txt(entries: &str) -> String {
        format!(
            "{}\n[robots-txt]\nmodules = \"refuse_all\"\n\n{entries}\n",
            crate_test_settings_str()
        )
    }

    #[test]
    fn a_fetch_entry_on_the_files_path_places_robots_txt() {
        let toml = settings_str_placing_robots_txt(&format!(
            "[[fetch]]\n{ROBOTS_TXT_ENTRY}\n[[serve]]\n{ROBOTS_TXT_ENTRY}"
        ));

        Settings::from_toml(&toml).expect("an entry on the file's path places the middleware");
    }

    /// An entry with no path would replace every text file, `ads.txt`
    /// included, with the robots.txt rules.
    #[test]
    fn an_entry_naming_robots_txt_on_another_path_is_refused() {
        let toml = settings_str_placing_robots_txt(&format!(
            "[[fetch]]\n{ROBOTS_TXT_ENTRY}\n[[fetch]]\nmedia_type = \"text/plain\"\nmiddleware = [\"robots_txt\"]\n"
        ));

        let error = Settings::from_toml(&toml).expect_err("every text file would be robots.txt");

        let message = format!("{error:?}");
        assert!(
            message.contains("[[fetch]] entry 2 names `robots_txt` for `text/plain` on every path"),
            "{message}"
        );
    }

    /// Only a `[[fetch]]` entry makes the file where the origin has none, so
    /// a `[[serve]]` placement alone would leave an origin's `404` standing,
    /// which a crawler reads as permission to crawl everything.
    #[test]
    fn robots_txt_placed_only_in_serve_is_refused() {
        let toml = settings_str_placing_robots_txt(&format!("[[serve]]\n{ROBOTS_TXT_ENTRY}"));

        let error = Settings::from_toml(&toml).expect_err("the file would never be made");

        assert!(
            format!("{error:?}").contains("no [[fetch]] entry with media_type = `text/plain`"),
            "{error:?}"
        );
    }

    #[test]
    fn the_robots_txt_section_is_read_by_its_hyphenated_name() {
        let toml = settings_str_with_robots_txt(
            "modules = [\"refuse_all\"]\nalways_allow = [\"/ads.txt\"]",
        );

        let settings = Settings::from_toml(&toml).expect("should read the section");

        let robots_txt = settings
            .robots_txt
            .as_ref()
            .expect("should carry the section");
        assert_eq!(robots_txt.modules, vec!["refuse_all"]);
        assert_eq!(robots_txt.always_allow, vec!["/ads.txt"]);
        assert!(
            settings
                .module_sections()
                .all(|(name, _)| name != "robots-txt"),
            "should not also be read as the section of a module type"
        );
    }

    #[test]
    fn a_robots_txt_section_naming_no_module_is_refused_when_the_settings_load() {
        let toml = settings_str_with_robots_txt("sitemap = \"https://example.com/sitemap.xml\"");

        let error = Settings::from_toml(&toml).expect_err("there is no default");

        assert!(
            format!("{error:?}").contains("[robots-txt] names no module"),
            "should say a module must be named: {error:?}"
        );
    }

    #[test]
    fn a_robots_txt_path_that_could_start_a_second_line_is_refused() {
        let toml = settings_str_with_robots_txt(
            "modules = \"refuse_all\"\nalways_allow = [\"/ads.txt\\nDisallow:\"]",
        );

        assert!(
            Settings::from_toml(&toml).is_err(),
            "should refuse a path that holds white space"
        );
    }

    #[test]
    fn settings_without_a_robots_txt_section_are_written_without_one() {
        let settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should load the test settings fixture");

        let value = serde_json::to_value(&settings).expect("should serialize the settings");

        assert!(
            value.get("robots-txt").is_none(),
            "a build that predates the section refuses a key it does not know"
        );
    }

    #[test]
    fn a_robots_txt_section_survives_the_stored_form() {
        let toml = settings_str_with_robots_txt(
            "modules = [\"allow_all\"]\nsitemap = \"https://example.com/sitemap.xml\"",
        );
        let settings = Settings::from_toml(&toml).expect("should read the section");

        let stored = serde_json::to_value(&settings).expect("should serialize the settings");
        let reloaded = Settings::from_json_value(stored).expect("should read the section back");

        let robots_txt = reloaded
            .robots_txt
            .as_ref()
            .expect("should still carry the section");
        assert_eq!(robots_txt.modules, vec!["allow_all"]);
        assert_eq!(
            robots_txt.sitemap.as_deref(),
            Some("https://example.com/sitemap.xml")
        );
    }

    /// Mirrors the `Settings` schema of the revision that predates
    /// `trusted_client_ip`: every key that revision knew, and
    /// `deny_unknown_fields` so an extra key fails deserialization exactly as an
    /// older binary would reject a pushed config blob.
    // The fields exist to model the accepted key set, never to be read.
    #[allow(dead_code)]
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct BaseRevisionSettings {
        #[serde(default)]
        publisher: serde::de::IgnoredAny,
        #[serde(default)]
        tester_cookie: serde::de::IgnoredAny,
        #[serde(default)]
        ec: serde::de::IgnoredAny,
        #[serde(default)]
        integrations: serde::de::IgnoredAny,
        #[serde(default)]
        handlers: serde::de::IgnoredAny,
        #[serde(default)]
        response_headers: serde::de::IgnoredAny,
        #[serde(default)]
        request_signing: serde::de::IgnoredAny,
        #[serde(default)]
        rewrite: serde::de::IgnoredAny,
        #[serde(default)]
        auction: serde::de::IgnoredAny,
        #[serde(default)]
        consent: serde::de::IgnoredAny,
        #[serde(default)]
        cache: serde::de::IgnoredAny,
        #[serde(default)]
        proxy: serde::de::IgnoredAny,
        #[serde(default)]
        creative_opportunities: serde::de::IgnoredAny,
        #[serde(default)]
        image_optimizer: serde::de::IgnoredAny,
        #[serde(default)]
        tinybird: serde::de::IgnoredAny,
        #[serde(default)]
        debug: serde::de::IgnoredAny,
    }

    #[test]
    fn trusted_client_ip_is_omitted_from_serialized_config_when_unset() {
        // `ts config push` serializes `Settings` verbatim. Emitting the key —
        // even as `null` — makes a `deny_unknown_fields` binary from the base
        // revision reject the blob during rollout or rollback.
        let settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should parse settings without trusted client IP configuration");

        let value = serde_json::to_value(&settings).expect("should serialize settings");

        assert!(
            value.get("trusted_client_ip").is_none(),
            "unset trusted_client_ip should not be serialized, got {value}"
        );
    }

    #[test]
    fn serialized_default_config_stays_readable_by_the_base_revision_schema() {
        let mut settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should parse settings without trusted client IP configuration");

        // The guarantee covers a config that configures none of the sections
        // added since the base revision. A configured section is serialized and,
        // like a configured `trusted_client_ip`, needs a compatible blob restored
        // before rolling back to a binary that predates it. The shared test
        // config sets `[geo]` because the permission model requires a default
        // country, so both selector tables are reset to unset here.
        settings.geo = GeoConfig::default();
        settings.device = DeviceConfig::default();
        settings.sections = TypeSections::default();
        settings.auction.modules.clear();

        let value = serde_json::to_value(&settings).expect("should serialize settings");

        serde_json::from_value::<BaseRevisionSettings>(value)
            .expect("base revision schema should accept a config blob with no trusted client IP");
    }

    #[test]
    fn geo_selector_is_omitted_from_serialized_config_when_unset() {
        // `ts config push` serializes `Settings` verbatim, so a selector nobody
        // set must not appear in the blob. A `deny_unknown_fields` binary that
        // predates the selector rejects the key during rollout or rollback.
        //
        // The shared fixture writes a `[geo]` table (it acknowledges running
        // with no geo module), so the assertion is about the selector key
        // rather than the table, which is what the blob's compatibility
        // actually turns on.
        let settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should parse settings without a geo selector");

        let value = serde_json::to_value(&settings).expect("should serialize settings");

        let geo = value
            .get("geo")
            .expect("the geo table is written because the fixture acknowledges no geo module");
        assert!(
            geo.get("module").is_none(),
            "an unset geo selector should not be serialized, got {geo}"
        );
        assert!(
            value
                .get("device")
                .is_none_or(|device| device.get("module").is_none()),
            "an unset device selector should not be serialized, got {value}"
        );
    }

    #[test]
    fn a_selected_geo_module_stays_in_the_serialized_config() {
        // The shared test settings already carry a `[geo]` table, so the
        // selector is set inside that table rather than in a second one, which
        // TOML rejects as a duplicate key.
        let settings = Settings::from_toml(&crate_test_settings_str().replace(
            "[geo]",
            "[geo]
module = \"none\"",
        ))
        .expect("should parse settings with a geo selector");

        let value = serde_json::to_value(&settings).expect("should serialize settings");

        assert_eq!(
            value
                .pointer("/geo/module")
                .and_then(serde_json::Value::as_str),
            Some("none"),
            "a selected geo module should survive serialization"
        );
    }

    #[test]
    fn trusted_client_ip_parses_and_redacts_shared_secret_in_debug_output() {
        let settings = Settings::from_toml(&trusted_client_ip_toml(
            "fastly-client-ip",
            "x-trusted-client-auth",
            "fictional-shared-secret-0123456789",
        ))
        .expect("should parse valid trusted client IP configuration");
        let config = settings
            .trusted_client_ip
            .expect("should retain trusted client IP configuration");

        assert_eq!(config.ip_header, "fastly-client-ip");
        assert_eq!(config.auth_header, "x-trusted-client-auth");
        let debug = format!("{config:?}");
        assert!(
            debug.contains("[REDACTED]"),
            "should redact trusted client IP shared secret in debug output"
        );
        assert!(
            !debug.contains("fictional-shared-secret-0123456789"),
            "should not expose trusted client IP shared secret in debug output"
        );
    }

    // One distinctive canary per `Redacted<String>` field reachable from
    // `Settings`'s derived `Debug` impl. This is a regression guard over the
    // field list below, not a completeness guarantee: a new secret field
    // added without the `Redacted` wrapper has no canary here and will pass
    // this test while leaking. Adding the canary is a manual step.
    //
    // Integration configs are deliberately out of scope. They reach
    // `Settings` as opaque JSON under `IntegrationSettings`, whose
    // hand-written `Debug` impl prints only integration IDs, never values.
    //
    // Do not use `..Struct::default()` anywhere in this function. A default
    // spread would let a new secret field be added to `EcPartner`,
    // `TinybirdSettings`, or any other struct built here without forcing
    // anyone to consider it. The compile break is the prompt; the canary
    // list below is still maintained by hand. List every field explicitly.
    #[test]
    fn settings_debug_output_redacts_every_secret_field() {
        const CANARY_PROXY_SECRET: &str = "CANARY-PROXY-SECRET-0123456789";
        const CANARY_EC_PASSPHRASE: &str = "CANARY-EC-PASSPHRASE-0123456789";
        const CANARY_EC_PARTNER_API_TOKEN: &str = "CANARY-EC-PARTNER-API-TOKEN-0123456789";
        const CANARY_EC_PARTNER_TS_PULL_TOKEN: &str = "CANARY-EC-PARTNER-TS-PULL-TOKEN-0123456789";
        const CANARY_TRUSTED_CLIENT_IP_SHARED_SECRET: &str =
            "CANARY-TRUSTED-CLIENT-IP-SHARED-SECRET-0123456789";
        const CANARY_S3_ACCESS_KEY_ID: &str = "CANARY-S3-ACCESS-KEY-ID-0123456789";
        const CANARY_S3_SECRET_ACCESS_KEY: &str = "CANARY-S3-SECRET-ACCESS-KEY-0123456789";
        const CANARY_S3_SESSION_TOKEN: &str = "CANARY-S3-SESSION-TOKEN-0123456789";
        const CANARY_TINYBIRD_AUCTION_TOKEN: &str = "CANARY-TINYBIRD-AUCTION-TOKEN-0123456789";
        const CANARY_TINYBIRD_ACCESS_TOKEN: &str = "CANARY-TINYBIRD-ACCESS-TOKEN-0123456789";
        const CANARY_MODULE_KEY: &str = "CANARY-MODULE-TABLE-KEY-0123456789";

        let mut settings = create_test_settings();

        settings.publisher.proxy_secret = Redacted::new(CANARY_PROXY_SECRET.to_string());
        select_hmac_module(&mut settings.ec, HMAC_MODULE_KEY, CANARY_EC_PASSPHRASE);

        settings.ec.partners = vec![EcPartner {
            name: "canary-partner".to_string(),
            source_domain: "canary-partner.example".to_string(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: false,
            api_token: Some(Redacted::new(CANARY_EC_PARTNER_API_TOKEN.to_string())),
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: Vec::new(),
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: Some(Redacted::new(CANARY_EC_PARTNER_TS_PULL_TOKEN.to_string())),
        }];

        settings.trusted_client_ip = Some(TrustedClientIpConfig {
            ip_header: "fastly-client-ip".to_string(),
            auth_header: "x-trusted-client-auth".to_string(),
            shared_secret: Redacted::new(CANARY_TRUSTED_CLIENT_IP_SHARED_SECRET.to_string()),
        });

        let mut asset_route = ProxyAssetRoute::new("/s3-assets/", "https://s3.canary.example");
        asset_route.auth = Some(AssetOriginAuth::S3SigV4(S3SigV4AuthConfig {
            region: "us-east-1".to_string(),
            secret_store: None,
            access_key_id: Redacted::new(CANARY_S3_ACCESS_KEY_ID.to_string()),
            secret_access_key: Redacted::new(CANARY_S3_SECRET_ACCESS_KEY.to_string()),
            session_token: Some(Redacted::new(CANARY_S3_SESSION_TOKEN.to_string())),
            origin_query: None,
        }));
        settings.proxy.asset_routes = vec![asset_route];

        settings.analytics = Some(AnalyticsConfig {
            module: Some(ANALYTICS_MODULE.to_string()),
            tinybird: Some(TinybirdSettings {
                auction_token_secret: Some(Redacted::new(
                    CANARY_TINYBIRD_AUCTION_TOKEN.to_string(),
                )),
                access_token_secret: Some(Redacted::new(CANARY_TINYBIRD_ACCESS_TOKEN.to_string())),
                api_host: String::new(),
                secret_store: None,
                auction_dataset: String::new(),
                access_enabled: false,
                access_dataset: String::new(),
                access_sample_rate: 0.0f64,
                max_body_bytes: 0,
            }),
        });

        // A module's table is opaque JSON, and the section's hand-written
        // `Debug` impl is the only thing keeping a secret a module's table
        // holds out of this output, so pin it here.
        settings
            .insert_module_config(
                "testing",
                "testing.example",
                &json!({
                    "key_name": CANARY_MODULE_KEY,
                }),
            )
            .expect("should insert a module's table");

        let debug = format!("{settings:?}");

        assert!(
            debug.contains("[REDACTED]"),
            "should redact secret fields in Settings debug output"
        );
        assert!(
            debug.contains("canary-partner.example"),
            "should leave a non-secret value visible in debug output"
        );

        let canaries = [
            ("publisher.proxy_secret", CANARY_PROXY_SECRET),
            ("ec.hmac.passphrase", CANARY_EC_PASSPHRASE),
            ("ec.partners[].api_token", CANARY_EC_PARTNER_API_TOKEN),
            (
                "ec.partners[].ts_pull_token",
                CANARY_EC_PARTNER_TS_PULL_TOKEN,
            ),
            (
                "trusted_client_ip.shared_secret",
                CANARY_TRUSTED_CLIENT_IP_SHARED_SECRET,
            ),
            (
                "proxy.asset_routes[].auth.access_key_id",
                CANARY_S3_ACCESS_KEY_ID,
            ),
            (
                "proxy.asset_routes[].auth.secret_access_key",
                CANARY_S3_SECRET_ACCESS_KEY,
            ),
            (
                "proxy.asset_routes[].auth.session_token",
                CANARY_S3_SESSION_TOKEN,
            ),
            (
                "analytics.tinybird.auction_token_secret",
                CANARY_TINYBIRD_AUCTION_TOKEN,
            ),
            (
                "analytics.tinybird.access_token_secret",
                CANARY_TINYBIRD_ACCESS_TOKEN,
            ),
            ("testing.example.key_name", CANARY_MODULE_KEY),
        ];

        for (field, canary) in canaries {
            assert!(
                !debug.contains(canary),
                "should redact {field} in Settings debug output"
            );
        }
    }

    #[test]
    fn trusted_client_ip_accepts_x_prefixed_ip_header() {
        let settings = Settings::from_toml(&trusted_client_ip_toml(
            "x-trusted-client-ip",
            "x-trusted-client-auth",
            "fictional-shared-secret-0123456789",
        ))
        .expect("should accept an x-prefixed trusted client IP header");
        let config = settings
            .trusted_client_ip
            .expect("should retain trusted client IP configuration");

        assert_eq!(
            config.ip_header, "x-trusted-client-ip",
            "should retain the x-prefixed trusted client IP header"
        );
    }

    #[test]
    fn trusted_client_ip_authentication_requires_an_exact_match() {
        let settings = Settings::from_toml(&trusted_client_ip_toml(
            "fastly-client-ip",
            "x-trusted-client-auth",
            "fictional-shared-secret-0123456789",
        ))
        .expect("should parse valid trusted client IP configuration");
        let config = settings
            .trusted_client_ip
            .expect("should retain trusted client IP configuration");

        assert!(
            config.authenticates("fictional-shared-secret-0123456789"),
            "should authenticate an exact shared secret match"
        );
        assert!(
            !config.authenticates("fictional-wrong-secret"),
            "should reject a different shared secret"
        );
        assert!(
            !config.authenticates(" fictional-shared-secret-0123456789"),
            "should reject a leading-whitespace shared secret"
        );
        assert!(
            !config.authenticates("fictional-shared-secret-0123456789 "),
            "should reject a trailing-whitespace shared secret"
        );
    }

    #[test]
    fn trusted_client_ip_rejects_identical_header_names() {
        for (ip_header, auth_header) in [
            ("x-trusted-client", "x-trusted-client"),
            ("X-Trusted-Client", "x-trusted-client"),
        ] {
            let error = Settings::from_toml(&trusted_client_ip_toml(
                ip_header,
                auth_header,
                "fictional-shared-secret-0123456789",
            ))
            .expect_err("should reject identical trusted client IP header names");

            assert!(
                format!("{error:?}").contains("identical_trusted_client_ip_headers"),
                "should identify duplicate trusted client IP header names"
            );
        }
    }

    #[test]
    fn trusted_client_ip_rejects_unsafe_header_names() {
        for (ip_header, auth_header, expected_code) in [
            (
                "host",
                "x-trusted-client-auth",
                "unsafe_trusted_client_ip_header",
            ),
            (
                "fastly-client-ip",
                "authorization",
                "unsafe_trusted_client_ip_auth_header",
            ),
        ] {
            let error = Settings::from_toml(&trusted_client_ip_toml(
                ip_header,
                auth_header,
                "fictional-shared-secret-0123456789",
            ))
            .expect_err("should reject unsafe trusted client IP header names");
            let message = format!("{error:?}");

            assert!(
                message.contains(expected_code),
                "should identify unsafe trusted client IP header names"
            );
            assert!(
                !message.contains("fictional-shared-secret-0123456789"),
                "should not include the shared secret in validation errors"
            );
        }
    }

    #[test]
    fn trusted_client_ip_rejects_reserved_internal_headers() {
        for (ip_header, auth_header) in [
            ("x-ts-tls-protocol", "x-trusted-client-auth"),
            ("x-ts-tls-cipher", "x-trusted-client-auth"),
            ("fastly-client-ip", "x-ts-tls-protocol"),
            ("fastly-client-ip", "x-ts-tls-cipher"),
            ("x-forwarded-for", "x-trusted-client-auth"),
            ("x-geo-info-available", "x-trusted-client-auth"),
            ("fastly-client-ip", "x-ts-ec"),
        ] {
            let error = Settings::from_toml(&trusted_client_ip_toml(
                ip_header,
                auth_header,
                "fictional-shared-secret-0123456789",
            ))
            .expect_err("should reject reserved internal headers");

            assert!(
                format!("{error:?}").contains("reserved_trusted_client_ip_header"),
                "should identify reserved internal headers"
            );
        }
    }

    #[test]
    fn trusted_client_ip_rejects_empty_secret_malformed_names_and_incomplete_sections() {
        let empty_secret = Settings::from_toml(&trusted_client_ip_toml(
            "fastly-client-ip",
            "x-trusted-client-auth",
            "",
        ));
        assert!(
            empty_secret.is_err(),
            "should reject an empty trusted client IP shared secret"
        );

        for (ip_header, auth_header, expected_code) in [
            (
                "invalid header",
                "x-trusted-client-auth",
                "invalid_trusted_client_ip_header",
            ),
            (
                "fastly-client-ip",
                "invalid header",
                "invalid_trusted_client_ip_auth_header",
            ),
        ] {
            let error = Settings::from_toml(&trusted_client_ip_toml(
                ip_header,
                auth_header,
                "fictional-shared-secret-0123456789",
            ))
            .expect_err("should reject malformed trusted client IP header names");
            assert!(
                format!("{error:?}").contains(expected_code),
                "should identify malformed trusted client IP header names"
            );
        }

        for section in [
            "[trusted_client_ip]\nauth_header = \"x-trusted-client-auth\"\nshared_secret = \"fictional-shared-secret-0123456789\"",
            "[trusted_client_ip]\nip_header = \"fastly-client-ip\"\nshared_secret = \"fictional-shared-secret-0123456789\"",
            "[trusted_client_ip]\nip_header = \"fastly-client-ip\"\nauth_header = \"x-trusted-client-auth\"",
            "[trusted_client_ip]\nip_header = \"fastly-client-ip\"\nauth_header = \"x-trusted-client-auth\"\nshared_secret = \"fictional-shared-secret-0123456789\"\nunknown_field = true",
        ] {
            let result =
                Settings::from_toml(&format!("{}\n{section}\n", crate_test_settings_str()));
            assert!(
                result.is_err(),
                "should reject incomplete or unknown trusted client IP configuration"
            );
        }
    }

    #[test]
    fn trusted_client_ip_rejects_control_byte_auth_header_without_exposing_secret() {
        let mut settings = serde_json::to_value(
            Settings::from_toml(&crate_test_settings_str())
                .expect("should parse base settings for JSON validation"),
        )
        .expect("should serialize base settings for JSON validation");
        settings["trusted_client_ip"] = json!({
            "ip_header": "fastly-client-ip",
            "auth_header": "x-trusted\u{0000}client-auth",
            "shared_secret": "fictional-control-byte-secret-0123",
        });

        let error = Settings::from_json_value(settings)
            .expect_err("should reject a control byte in the trusted client IP auth header");
        let message = format!("{error:?}");

        assert!(
            message.contains("invalid_trusted_client_ip_auth_header"),
            "should identify the malformed trusted client IP auth header"
        );
        assert!(
            !message.contains("fictional-control-byte-secret-0123"),
            "should not expose the trusted client IP shared secret in validation errors"
        );
    }

    #[test]
    fn trusted_client_ip_rejects_a_31_byte_shared_secret_without_exposing_it() {
        let shared_secret = "1234567890123456789012345678901";
        let error = Settings::from_toml(&trusted_client_ip_toml(
            "fastly-client-ip",
            "x-trusted-client-auth",
            shared_secret,
        ))
        .expect_err("should reject a shared secret below the minimum length");
        let message = format!("{error:?}");

        assert!(
            message.contains("short_trusted_client_ip_shared_secret"),
            "should identify the undersized trusted client IP shared secret"
        );
        assert!(
            !message.contains(shared_secret),
            "should not expose the undersized trusted client IP shared secret"
        );
    }

    #[test]
    fn trusted_client_ip_accepts_an_exactly_32_byte_ascii_graphic_shared_secret() {
        let shared_secret = "0123456789abcdef0123456789ABCDEF";
        let settings = Settings::from_toml(&trusted_client_ip_toml(
            "fastly-client-ip",
            "x-trusted-client-auth",
            shared_secret,
        ))
        .expect("should accept an exactly 32-byte ASCII graphic shared secret");
        let config = settings
            .trusted_client_ip
            .expect("should retain trusted client IP configuration");

        assert_eq!(
            config.shared_secret.expose(),
            shared_secret,
            "should retain the accepted shared secret"
        );
    }

    #[test]
    fn trusted_client_ip_rejects_a_non_ascii_shared_secret_without_exposing_it() {
        let shared_secret = "ascii-graphic-secret-0123456789é";
        let error = Settings::from_toml(&trusted_client_ip_toml(
            "fastly-client-ip",
            "x-trusted-client-auth",
            shared_secret,
        ))
        .expect_err("should reject a non-ASCII shared secret that exceeds 32 bytes");
        let message = format!("{error:?}");

        assert!(
            message.contains("invalid_trusted_client_ip_shared_secret"),
            "should identify the non-header-safe trusted client IP shared secret"
        );
        assert!(
            !message.contains(shared_secret),
            "should not expose the non-ASCII trusted client IP shared secret"
        );
    }

    #[test]
    fn trusted_client_ip_rejects_a_shared_secret_with_an_embedded_space_without_exposing_it() {
        let shared_secret = "valid-shared-secret-with space-012345";
        let error = Settings::from_toml(&trusted_client_ip_toml(
            "fastly-client-ip",
            "x-trusted-client-auth",
            shared_secret,
        ))
        .expect_err("should reject a shared secret containing an ASCII space");
        let message = format!("{error:?}");

        assert!(
            message.contains("invalid_trusted_client_ip_shared_secret"),
            "should identify the non-header-safe trusted client IP shared secret"
        );
        assert!(
            !message.contains(shared_secret),
            "should not expose the shared secret containing an ASCII space"
        );
    }

    #[test]
    fn trusted_client_ip_rejects_a_shared_secret_with_an_embedded_tab_without_exposing_it() {
        let shared_secret = "valid-shared-secret-with\t-tab-012345";
        let mut settings = serde_json::to_value(
            Settings::from_toml(&crate_test_settings_str())
                .expect("should parse base settings for JSON validation"),
        )
        .expect("should serialize base settings for JSON validation");
        settings["trusted_client_ip"] = json!({
            "ip_header": "fastly-client-ip",
            "auth_header": "x-trusted-client-auth",
            "shared_secret": shared_secret,
        });

        let error = Settings::from_json_value(settings)
            .expect_err("should reject a shared secret containing a horizontal tab");
        let message = format!("{error:?}");

        assert!(
            message.contains("invalid_trusted_client_ip_shared_secret"),
            "should identify the non-header-safe trusted client IP shared secret"
        );
        assert!(
            !message.contains(shared_secret),
            "should not expose the shared secret containing a horizontal tab"
        );
    }

    #[test]
    fn trusted_client_ip_rejects_a_shared_secret_with_del_without_exposing_it() {
        let shared_secret = "valid-shared-secret-with\u{007f}-del-012345";
        let mut settings = serde_json::to_value(
            Settings::from_toml(&crate_test_settings_str())
                .expect("should parse base settings for JSON validation"),
        )
        .expect("should serialize base settings for JSON validation");
        settings["trusted_client_ip"] = json!({
            "ip_header": "fastly-client-ip",
            "auth_header": "x-trusted-client-auth",
            "shared_secret": shared_secret,
        });

        let error = Settings::from_json_value(settings)
            .expect_err("should reject a shared secret containing DEL");
        let message = format!("{error:?}");

        assert!(
            message.contains("invalid_trusted_client_ip_shared_secret"),
            "should identify the non-header-safe trusted client IP shared secret"
        );
        assert!(
            !message.contains(shared_secret),
            "should not expose the shared secret containing DEL"
        );
    }

    #[test]
    fn trusted_client_ip_rejects_a_shared_secret_with_a_control_byte_without_exposing_it() {
        let shared_secret = "valid-shared-secret-with\u{0001}-control-012345";
        let mut settings = serde_json::to_value(
            Settings::from_toml(&crate_test_settings_str())
                .expect("should parse base settings for JSON validation"),
        )
        .expect("should serialize base settings for JSON validation");
        settings["trusted_client_ip"] = json!({
            "ip_header": "fastly-client-ip",
            "auth_header": "x-trusted-client-auth",
            "shared_secret": shared_secret,
        });

        let error = Settings::from_json_value(settings)
            .expect_err("should reject a shared secret containing a control byte");
        let message = format!("{error:?}");

        assert!(
            message.contains("invalid_trusted_client_ip_shared_secret"),
            "should identify the non-header-safe trusted client IP shared secret"
        );
        assert!(
            !message.contains(shared_secret),
            "should not expose the shared secret containing a control byte"
        );
    }

    #[test]
    fn trusted_client_ip_rejects_placeholder_shared_secrets() {
        for placeholder in TrustedClientIpConfig::SHARED_SECRET_PLACEHOLDERS {
            assert!(
                TrustedClientIpConfig::is_placeholder_shared_secret(placeholder),
                "should detect placeholder shared secret '{placeholder}'"
            );
            assert!(
                TrustedClientIpConfig::is_placeholder_shared_secret(&placeholder.to_uppercase()),
                "should detect placeholder shared secret case-insensitively"
            );

            let settings = Settings::from_toml(&trusted_client_ip_toml(
                "fastly-client-ip",
                "x-trusted-client-auth",
                placeholder,
            ))
            .expect("should parse a placeholder trusted client IP shared secret");
            let error = settings
                .reject_placeholder_secrets()
                .expect_err("should reject a placeholder trusted client IP shared secret");

            assert!(
                format!("{error:?}").contains("trusted_client_ip.shared_secret"),
                "should name the placeholder trusted client IP shared secret field"
            );
        }
    }

    /// The shared fixture with one `[[handlers]]` entry ahead of everything
    /// else, as a configuration written for a build that asked for passwords
    /// carries it.
    fn settings_str_with_a_handler() -> String {
        format!(
            "[[handlers]]\npath = \"^/_ts/admin\"\nusername = \"admin\"\n\
             password = \"admin-pass\"\n{}",
            crate_test_settings_str()
        )
    }

    #[test]
    fn toml_settings_refuse_a_handlers_list_with_directions() {
        let error = Settings::from_toml(&settings_str_with_a_handler())
            .expect_err("should refuse a configuration that still gates a path");

        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("`[[handlers]]` is not read"),
            "should say the list is not read: {rendered}"
        );
        assert!(
            rendered.contains("ts keys") && rendered.contains("ts cache purge"),
            "should say where administration went: {rendered}"
        );
    }

    #[test]
    fn json_settings_refuse_a_handlers_list_with_directions() {
        let settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should load the test settings fixture");
        let mut value = serde_json::to_value(settings)
            .expect("should serialize the test settings fixture to JSON");
        value["handlers"] = json!([
            { "path": "^/secure", "username": "user", "password": "handler_password" },
        ]);

        let error = Settings::from_json_value(value)
            .expect_err("should refuse a stored document that still gates a path");

        assert!(
            format!("{error:?}").contains("`[[handlers]]` is not read"),
            "should say the list is not read: {error:?}"
        );
    }

    #[test]
    fn an_empty_handlers_list_is_refused_too() {
        let toml = format!("handlers = []\n{}", crate_test_settings_str());

        let error = Settings::from_toml(&toml).expect_err("should refuse the key itself");

        assert!(
            format!("{error:?}").contains("`[[handlers]]` is not read"),
            "should say the list is not read: {error:?}"
        );
    }

    #[test]
    fn settings_are_written_without_a_handlers_list() {
        let settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should load the test settings fixture");

        let value = serde_json::to_value(&settings).expect("should serialize the settings");

        assert!(
            value.get("handlers").is_none(),
            "should write nothing a later read would refuse"
        );
    }

    #[test]
    fn toml_settings_refuse_a_signing_store_id_with_directions() {
        for key in ["config_store_id", "secret_store_id"] {
            let fixture = crate_test_settings_str();
            assert!(
                fixture.contains("[request_signing]\n"),
                "the fixture should have the section the key is added to"
            );
            let toml = fixture.replace(
                "[request_signing]\n",
                &format!("[request_signing]\n{key} = \"01GEXAMPLE\"\n"),
            );

            let error = Settings::from_toml(&toml).expect_err("should refuse the removed key");

            let rendered = format!("{error:?}");
            assert!(
                rendered.contains("does not take `config_store_id` or `secret_store_id`"),
                "should say the key is not read, for {key}: {rendered}"
            );
            assert!(
                rendered.contains("ts keys rotate"),
                "should say where the ids go, for {key}: {rendered}"
            );
        }
    }

    #[test]
    fn json_settings_refuse_a_signing_store_id_with_directions() {
        let settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should load the test settings fixture");
        let mut value = serde_json::to_value(settings)
            .expect("should serialize the test settings fixture to JSON");
        value["request_signing"]["secret_store_id"] = json!("01GEXAMPLE");

        let error = Settings::from_json_value(value).expect_err("should refuse the removed key");

        assert!(
            format!("{error:?}").contains("--secret-store-id"),
            "should say where the id goes: {error:?}"
        );
    }

    #[test]
    fn request_signing_is_written_as_enabled_alone() {
        let mut settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should load the test settings fixture");
        settings.request_signing = Some(RequestSigning::new(true));

        let value = serde_json::to_value(&settings).expect("should serialize the settings");

        assert_eq!(
            value["request_signing"],
            json!({ "enabled": true }),
            "should write nothing a later read would refuse"
        );
        let read = Settings::from_json_value(value).expect("should read what was written");
        assert!(
            read.request_signing.is_some_and(|signing| signing.enabled),
            "should keep request signing on across the round trip"
        );
    }

    #[test]
    fn json_settings_rejects_legacy_auction_provider_list_with_migration_guidance() {
        let settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should load the test settings fixture");
        let mut value = serde_json::to_value(settings)
            .expect("should serialize the test settings fixture to JSON");
        value["auction"]["providers"] = json!(["example"]);

        let error = Settings::from_json_value(value)
            .expect_err("should reject the removed auction provider list schema");
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("auction.providers"),
            "error should identify the removed field, got {rendered}"
        );
        assert!(
            rendered.contains("[demand] modules"),
            "error should name where the setting moved to, got {rendered}"
        );
    }

    #[test]
    fn toml_settings_reject_legacy_auction_provider_list_with_migration_guidance() {
        let toml = format!(
            "{}\n",
            crate_test_settings_str()
                .replace("[auction]\n", "[auction]\nproviders = [\"example\"]\n")
        );

        let error = Settings::from_toml(&toml)
            .expect_err("should reject the removed auction provider list schema");
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("auction.providers"),
            "error should identify the removed field, got {rendered}"
        );
        assert!(
            rendered.contains("[demand] modules"),
            "error should name where the setting moved to, got {rendered}"
        );
    }

    #[test]
    fn auction_debug_comment_options_default_matches_serde_defaults() {
        let opts = AuctionDebugCommentOptions::default();
        assert!(opts.include_provider_responses, "should default to true");
        assert!(opts.include_adserver_response, "should default to true");
        assert!(opts.include_bids, "should default to true");
        assert_eq!(
            opts.metadata_keys,
            vec![
                "error_type".to_string(),
                "http_status".to_string(),
                "message".to_string(),
            ],
            "should default to only schema-validated response metadata"
        );
        assert_eq!(
            opts.verbosity,
            AuctionDebugCommentVerbosity::Redacted,
            "should default to Redacted"
        );
        assert_eq!(
            opts.format,
            AuctionDebugCommentFormat::Compact,
            "should default to compact output"
        );
    }

    #[test]
    fn auction_debug_comment_options_normalize_trims_and_drops_empty_keys() {
        let mut opts = AuctionDebugCommentOptions {
            metadata_keys: vec![
                " http_status ".to_string(),
                "".to_string(),
                "debug".to_string(),
            ],
            ..AuctionDebugCommentOptions::default()
        };
        opts.normalize();
        assert_eq!(
            opts.metadata_keys,
            vec!["http_status".to_string(), "debug".to_string()]
        );
    }

    #[test]
    fn auction_debug_comment_options_deserializes_upstream_verbosity() {
        let options: AuctionDebugCommentOptions = toml::from_str(r#"verbosity = "upstream""#)
            .expect("should deserialize upstream verbosity");
        assert_eq!(options.verbosity, AuctionDebugCommentVerbosity::Upstream);
    }

    #[test]
    fn auction_debug_comment_options_deserializes_pretty_format() {
        let options: AuctionDebugCommentOptions =
            toml::from_str(r#"format = "pretty""#).expect("should deserialize pretty format");
        assert_eq!(options.format, AuctionDebugCommentFormat::Pretty);
    }

    #[test]
    fn auction_debug_comment_options_bad_format_fails_config_load() {
        let result: Result<AuctionDebugCommentOptions, _> =
            toml::from_str(r#"format = "expanded""#);
        assert!(
            result.is_err(),
            "unrecognized format must fail to deserialize, not silently fall back"
        );
    }

    #[test]
    fn bad_verbosity_string_fails_config_load() {
        // Deserialize AuctionDebugCommentOptions directly, not a full Settings —
        // Settings has required fields with no #[serde(default)] (e.g.
        // `publisher`), so a full-Settings fixture missing them would fail with
        // "missing field `publisher`" regardless of whether `verbosity` itself
        // deserialized correctly, testing the wrong thing.
        let result: Result<AuctionDebugCommentOptions, _> =
            toml::from_str(r#"verbosity = "everything""#);
        assert!(
            result.is_err(),
            "unrecognized verbosity must fail to deserialize, not silently fall back"
        );
    }

    #[test]
    fn auction_debug_comment_options_unknown_metadata_key_fails_config_load() {
        let toml = format!(
            "{}\n[debug]\nauction_html_comment = true\n\n[debug.auction_html_comment_options]\nmetadata_keys = [\"http_staus\", \"errors\"]\n",
            crate_test_settings_str()
        );
        let error = Settings::from_toml(&toml)
            .expect_err("should reject metadata keys outside the fixed allowlist");
        let rendered = format!("{error:?}");
        assert!(
            rendered.contains("http_staus") && rendered.contains("errors"),
            "error should name every unsupported key, got {rendered}"
        );
    }

    #[test]
    fn auction_debug_comment_options_allowlisted_metadata_keys_load() {
        let toml = format!(
            "{}\n[debug]\nauction_html_comment = true\n\n[debug.auction_html_comment_options]\nmetadata_keys = [\" message \"]\n",
            crate_test_settings_str()
        );
        let settings = Settings::from_toml(&toml).expect("should accept an allowlisted key");
        assert_eq!(
            settings.debug.auction_html_comment_options.metadata_keys,
            vec!["message".to_string()],
            "normalize should trim before validation runs"
        );
    }

    #[test]
    fn auction_debug_comment_options_unknown_field_fails_config_load() {
        let result: Result<AuctionDebugCommentOptions, _> =
            toml::from_str(r#"metadata_key = ["message"]"#);
        assert!(
            result.is_err(),
            "a misspelled field must fail config load, not be silently ignored"
        );
    }

    #[test]
    fn default_auction_debug_comment_options_stay_out_of_serialized_config() {
        // Rollback contract: `DebugConfig` denies unknown fields, so the
        // previous binary rejects a config blob carrying a table it does not
        // know. Defaults must therefore serialize to nothing.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct LegacyDebugConfig {
            #[serde(default)]
            ja4_endpoint_enabled: bool,
            #[serde(default)]
            auction_html_comment: bool,
            #[serde(default)]
            inject_adm_for_testing: bool,
        }

        let value = serde_json::to_value(DebugConfig::default())
            .expect("should serialize the default debug config");
        assert!(
            value.get("auction_html_comment_options").is_none(),
            "default options table should not be serialized, got {value}"
        );

        let legacy: LegacyDebugConfig = serde_json::from_value(value)
            .expect("legacy schema should accept the default debug payload");
        assert!(!legacy.ja4_endpoint_enabled);
        assert!(!legacy.auction_html_comment);
        assert!(!legacy.inject_adm_for_testing);

        let configured = DebugConfig {
            auction_html_comment: true,
            auction_html_comment_options: AuctionDebugCommentOptions {
                include_bids: false,
                ..AuctionDebugCommentOptions::default()
            },
            ..DebugConfig::default()
        };
        let value =
            serde_json::to_value(&configured).expect("should serialize a configured debug config");
        assert!(
            value.get("auction_html_comment_options").is_some(),
            "non-default options must still serialize, got {value}"
        );
    }

    /// `[analytics]` selecting the analytics module, with `table` as the
    /// lines of its settings table.
    fn settings_str_with_analytics_module(table: &str) -> String {
        format!(
            "{}\n[analytics]\nmodule = \"tinybird\"\n\n[analytics.tinybird]\n{table}\n",
            crate_test_settings_str()
        )
    }

    /// The lines of a telemetry table that passes every check.
    const ANALYTICS_MODULE_LINES: &str =
        "api_host = \"api.us-east.aws.tinybird.co\"\nauction_token_secret = \"test-auction-token\"";

    #[test]
    fn settings_without_an_analytics_section_select_no_module() {
        let settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should parse settings without an analytics section");

        assert!(settings.analytics.is_none(), "should hold no section");
        assert!(
            settings.analytics_module().is_none(),
            "should select no module"
        );
        let value = serde_json::to_value(&settings).expect("should serialize the settings");
        assert!(
            value.get("analytics").is_none() && value.get("tinybird").is_none(),
            "should write neither the section nor the table it replaced: {value}"
        );
    }

    #[test]
    fn the_analytics_module_is_selected_by_its_short_or_full_name() {
        for name in ["tinybird", "analytics.tinybird"] {
            let toml = format!(
                "{}\n[analytics]\nmodule = \"{name}\"\n\n[analytics.tinybird]\n{ANALYTICS_MODULE_LINES}\n",
                crate_test_settings_str()
            );

            let settings = Settings::from_toml(&toml).expect("should accept the selection");

            let telemetry = settings
                .analytics_module()
                .expect("should select the analytics module");
            assert_eq!(telemetry.api_host, "api.us-east.aws.tinybird.co");
            assert_eq!(telemetry.auction_dataset, "auction_events_raw");
        }
    }

    #[test]
    fn a_selected_analytics_module_survives_the_stored_form() {
        let settings =
            Settings::from_toml(&settings_str_with_analytics_module(ANALYTICS_MODULE_LINES))
                .expect("should accept the selection");

        let stored = serde_json::to_value(&settings).expect("should serialize the settings");

        assert_eq!(stored["analytics"]["module"], "tinybird");
        assert_eq!(
            stored["analytics"]["tinybird"]["api_host"],
            "api.us-east.aws.tinybird.co"
        );
        assert!(
            stored.get("tinybird").is_none(),
            "should not write the table that moved: {stored}"
        );
    }

    #[test]
    fn the_analytics_module_needs_a_regional_host() {
        let toml =
            settings_str_with_analytics_module("api_host = \"https://api.example.com/path\"");

        let err = Settings::from_toml(&toml).expect_err("should reject invalid api host");

        assert!(
            format!("{err:?}").contains("analytics.tinybird.api_host"),
            "should name the setting by its table: {err:?}"
        );
    }

    #[test]
    fn the_analytics_module_needs_its_token() {
        let toml = settings_str_with_analytics_module("api_host = \"api.us-east.aws.tinybird.co\"");

        let err = Settings::from_toml(&toml).expect_err("should require the token");

        assert!(
            format!("{err:?}").contains(
                "analytics.tinybird.auction_token_secret is required when [analytics] selects \
                 the module"
            ),
            "should say what is missing and why: {err:?}"
        );
    }

    #[test]
    fn a_selected_analytics_module_with_no_table_is_told_what_it_needs() {
        let toml = format!(
            "{}\n[analytics]\nmodule = \"tinybird\"\n",
            crate_test_settings_str()
        );

        let err = Settings::from_toml(&toml).expect_err("should need the table's settings");

        assert!(
            format!("{err:?}").contains("analytics.tinybird.api_host"),
            "should name the first setting the module needs: {err:?}"
        );
    }

    #[test]
    fn access_telemetry_is_rejected_until_an_emitter_is_wired() {
        let toml = settings_str_with_analytics_module(&format!(
            "{ANALYTICS_MODULE_LINES}\naccess_enabled = true"
        ));

        let err = Settings::from_toml(&toml)
            .expect_err("should reject access telemetry before an emitter exists");

        assert!(
            format!("{err:?}").contains("analytics.tinybird.access_enabled"),
            "should name the unsupported setting: {err:?}"
        );
    }

    #[test]
    fn an_analytics_section_selecting_nothing_is_refused() {
        let toml = format!(
            "{}\n[analytics.tinybird]\n{ANALYTICS_MODULE_LINES}\n",
            crate_test_settings_str()
        );

        let err = Settings::from_toml(&toml).expect_err("nothing would read the table");

        assert!(
            format!("{err:?}").contains("[analytics] selects no module"),
            "should say the section selects nothing: {err:?}"
        );
    }

    #[test]
    fn an_analytics_module_this_build_does_not_have_is_refused() {
        let toml = format!(
            "{}\n[analytics]\nmodule = \"another\"\n",
            crate_test_settings_str()
        );

        let err = Settings::from_toml(&toml).expect_err("no such module");
        let message = format!("{err:?}");

        assert!(
            message.contains(
                "[analytics] selects `another`, which no module this deployment supplies"
            ),
            "should name what was written: {message}"
        );
        assert!(
            message.contains("The analytics modules it supplies are [tinybird]"),
            "should list what can be selected: {message}"
        );
    }

    /// The token is a secret reference, and an empty or absent one is refused
    /// as the settings load, before any deploy step sees the document.
    #[test]
    fn an_analytics_token_reference_that_is_empty_or_absent_is_refused_when_the_settings_load() {
        for (case, table) in [
            (
                "an empty token",
                "api_host = \"api.us-east.aws.tinybird.co\"\nauction_token_secret = \"\"",
            ),
            ("no token", "api_host = \"api.us-east.aws.tinybird.co\""),
        ] {
            let err = Settings::from_toml(&settings_str_with_analytics_module(table))
                .err()
                .unwrap_or_else(|| panic!("{case}: the settings should not load"));

            assert!(
                format!("{err:?}").contains("auction_token_secret"),
                "{case}: should name the token: {err:?}"
            );
        }
    }

    #[test]
    fn the_analytics_module_table_refuses_a_key_it_does_not_know() {
        let toml = settings_str_with_analytics_module(&format!(
            "{ANALYTICS_MODULE_LINES}\nenabled = true"
        ));

        let err = Settings::from_toml(&toml).expect_err("the enabled line is gone");

        assert!(
            format!("{err:?}").contains("enabled"),
            "should name the key it does not know: {err:?}"
        );
    }

    #[test]
    fn a_table_the_section_does_not_select_gives_no_analytics_module() {
        let analytics = AnalyticsConfig {
            module: None,
            tinybird: Some(TinybirdSettings::default()),
        };

        assert!(
            analytics.analytics_module().is_none(),
            "a table nothing selects should give no settings"
        );
    }

    #[test]
    fn the_tinybird_table_is_refused_with_directions() {
        for table in [
            "enabled = true\napi_host = \"api.us-east.aws.tinybird.co\"",
            "enabled = false",
        ] {
            let toml = format!("{}\n[tinybird]\n{table}\n", crate_test_settings_str());

            let err = Settings::from_toml(&toml).expect_err("the table has moved");

            assert!(
                format!("{err:?}")
                    .contains("Configuration table `[tinybird]` is now `[analytics.tinybird]`"),
                "should say where the table went: {err:?}"
            );
        }
    }

    #[test]
    fn settings_rejects_removed_consent_store_toml() {
        let toml = format!(
            "{}\n[consent]\nconsent_store = \"legacy-consent-store\"\n",
            crate_test_settings_str()
        );

        let err = Settings::from_toml(&toml)
            .expect_err("should reject the removed consent_store TOML field");

        assert!(
            format!("{err:?}").contains("consent_store"),
            "should identify the removed field: {err:?}"
        );
    }

    #[test]
    fn settings_rejects_removed_consent_store_json() {
        let settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should parse baseline settings");
        let mut value = serde_json::to_value(settings).expect("should serialize baseline settings");
        value["consent"]["consent_store"] = json!("legacy-consent-store");

        let err = Settings::from_json_value(value)
            .expect_err("should reject the removed consent_store JSON field");

        assert!(
            format!("{err:?}").contains("consent_store"),
            "should identify the removed field: {err:?}"
        );
    }

    #[test]
    fn test_settings_from_valid_toml() {
        let toml_str = crate_test_settings_str();
        let settings = Settings::from_toml(&toml_str);

        assert!(settings.is_ok());

        let settings = settings.expect("should parse valid TOML");
        assert!(
            settings
                .module_config::<OneRequiredSetting>(ENDPOINT_MODULE)
                .expect("a query for a module no section selects should succeed")
                .is_none(),
            "a module no section selects should not run"
        );
        assert!(
            settings.auction.modules.selected().is_empty(),
            "the fixture should select no auction module"
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
            settings.ec.module.as_ref(),
            Some(&EcModuleSelection::from(HMAC_MODULE_KEY)),
            "test settings should select the hmac EC module"
        );
        assert_eq!(
            hmac_passphrase(&settings.ec, HMAC_MODULE_KEY),
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
        assert_eq!(
            settings.tester_cookie.max_age_seconds,
            TesterCookieConfig::default_max_age_seconds(),
            "omitted max_age_seconds should fall back to the 30-day default"
        );
    }

    #[test]
    fn tester_cookie_max_age_parses_from_toml() {
        let toml_str = format!(
            r#"{}

            [tester_cookie]
            enabled = true
            max_age_seconds = 3600
        "#,
            crate_test_settings_str()
        );

        let settings =
            Settings::from_toml(&toml_str).expect("should parse tester-cookie max age config");

        assert_eq!(
            settings.tester_cookie.max_age_seconds, 3_600,
            "tester-cookie config should use the configured max age"
        );
    }

    #[test]
    fn tester_cookie_absent_section_defaults_max_age() {
        let settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should parse settings without a tester-cookie section");

        assert_eq!(
            settings.tester_cookie.max_age_seconds,
            TesterCookieConfig::default_max_age_seconds(),
            "absent tester-cookie section should default max_age_seconds to 30 days"
        );
    }

    #[test]
    fn tester_cookie_zero_max_age_fails_validation() {
        let toml_str = format!(
            r#"{}

            [tester_cookie]
            enabled = true
            max_age_seconds = 0
        "#,
            crate_test_settings_str()
        );

        let error =
            Settings::from_toml(&toml_str).expect_err("should reject a zero tester-cookie max age");

        assert!(
            error.to_string().contains("validation failed"),
            "zero max_age_seconds should fail settings validation, got: {error}"
        );
    }

    #[test]
    fn loading_cache_lifetimes_are_read_with_defaults_for_what_is_unset() {
        let toml_str = format!(
            r#"{}

            [cache.robots-txt]
            store_seconds = 600
            store_idle_seconds = 3600
        "#,
            crate_test_settings_str()
        );
        let settings = Settings::from_toml(&toml_str).expect("should parse the lifetimes");
        let defaults = crate::platform::CacheLifetimes {
            memory: Duration::from_secs(5),
            store: Duration::from_secs(60),
            store_idle: Duration::from_secs(70),
        };

        assert_eq!(
            settings.cache.robots_txt.lifetimes(defaults),
            crate::platform::CacheLifetimes {
                memory: Duration::from_secs(5),
                store: Duration::from_secs(600),
                store_idle: Duration::from_secs(3600),
            }
        );
    }

    #[test]
    fn a_loading_cache_lifetime_out_of_range_is_refused_naming_it() {
        for (field, value, least) in [
            ("store_seconds", "0", 1),
            ("store_idle_seconds", "0", 1),
            ("memory_seconds", "31536001", 0),
        ] {
            let toml_str = format!(
                "{}

[cache.robots-txt]
{field} = {value}
",
                crate_test_settings_str()
            );
            let error = Settings::from_toml(&toml_str)
                .expect_err("a lifetime out of range should not load");
            let message = format!("{error:?}");
            assert!(
                message.contains(&format!(
                    "[cache.robots-txt] {field} is {value}, and must be from {least} to 31536000"
                )),
                "{field}: {message}"
            );
        }
    }

    #[test]
    fn unset_loading_cache_lifetimes_are_not_written_back() {
        let settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse the settings");
        let written = serde_json::to_value(&settings.cache).expect("should serialize");

        assert_eq!(written, serde_json::json!({ "asset_rules": [] }));
    }

    #[test]
    fn a_cache_a_module_declares_is_read_from_its_own_table() {
        let toml_str = format!(
            "{}\n[cache.answers]\nmodule = \"51degrees\"\nstore_seconds = 3600\n",
            crate_test_settings_str()
        );
        let settings = Settings::from_toml(&toml_str).expect("should parse a declared cache");

        let caches = settings.cache.loading_caches();

        assert_eq!(caches.len(), 2);
        assert_eq!(caches[1].0, "answers");
        assert_eq!(caches[1].1.module.as_deref(), Some("51degrees"));
        assert_eq!(caches[1].1.store_seconds, Some(3600));
        let written = serde_json::to_value(&settings.cache).expect("should serialize");
        assert_eq!(written["answers"]["module"], "51degrees");
    }

    #[test]
    fn a_cache_assets_table_is_read_as_a_cache_a_module_declares() {
        let toml_str = format!(
            "{}\n[cache.assets]\nstore_seconds = 3600\n",
            crate_test_settings_str()
        );
        let settings = Settings::from_toml(&toml_str).expect("should parse the table");

        assert_eq!(
            settings.cache.loading_caches(),
            vec![
                (super::ROBOTS_TXT_CACHE, &settings.cache.robots_txt),
                ("assets", &settings.cache.declared["assets"]),
            ],
            "core has no `[cache.assets]` of its own, so a module would declare it"
        );
    }

    #[test]
    fn a_loading_cache_module_is_read_and_written_back() {
        let toml_str = format!(
            r#"{}

            [cache.robots-txt]
            module = "51degrees"
        "#,
            crate_test_settings_str()
        );
        let settings = Settings::from_toml(&toml_str).expect("should parse the selection");

        assert_eq!(
            settings.cache.robots_txt.module.as_deref(),
            Some("51degrees")
        );
        assert!(!settings.cache.robots_txt.is_default());
        assert_eq!(
            settings.cache.loading_caches(),
            vec![(super::ROBOTS_TXT_CACHE, &settings.cache.robots_txt)],
            "should list the robots.txt cache by its name"
        );
        let written = serde_json::to_value(&settings.cache).expect("should serialize");
        assert_eq!(
            written,
            serde_json::json!({ "asset_rules": [], "robots-txt": { "module": "51degrees" } })
        );
    }

    #[test]
    fn cache_asset_rule_nextjs_preset_is_operator_controlled() {
        let toml_str = format!(
            r#"{}

            [[cache.asset_rules]]
            id = "nextjs-static"
            enabled = true
            preset = "nextjs-static"
            visibility = "public"
            browser_ttl_seconds = 31536000
            edge_ttl_seconds = 31536000
            immutable = true
        "#,
            crate_test_settings_str()
        );
        let settings = Settings::from_toml(&toml_str).expect("should parse cache asset rule");

        let policy = settings
            .asset_cache_policy_for_path("/_next/static/chunks/app.js")
            .expect("should evaluate cache rules")
            .expect("should match enabled Next.js preset");
        assert_eq!(
            policy,
            CachePolicy::public_immutable(Duration::from_secs(31_536_000)),
            "enabled preset should produce immutable static policy"
        );

        let disabled_toml = toml_str.replace("enabled = true", "enabled = false");
        let disabled_settings =
            Settings::from_toml(&disabled_toml).expect("should parse disabled cache asset rule");
        assert!(
            disabled_settings
                .asset_cache_policy_for_path("/_next/static/chunks/app.js")
                .expect("should evaluate disabled cache rules")
                .is_none(),
            "disabled preset must not mark framework paths immutable"
        );
    }

    #[test]
    fn cache_asset_rule_requires_selected_fingerprint_style() {
        let expected_policy = CachePolicy::public_immutable(Duration::from_secs(31_536_000));
        for (style, matching_path, non_matching_path) in [
            ("hex", "/assets/app.0123abcd.js", "/assets/app-VRTVD5R5.js"),
            (
                "esbuild-base32",
                "/assets/app-VRTVD5R5.js",
                "/assets/index-BsELY24f.js",
            ),
        ] {
            let toml_str = format!(
                r#"{}

                [[cache.asset_rules]]
                id = "publisher-assets"
                enabled = true
                path_globs = ["/assets/**/*.js"]
                fingerprint_style = "{style}"
                visibility = "public"
                browser_ttl_seconds = 31536000
                edge_ttl_seconds = 31536000
                immutable = true
            "#,
                crate_test_settings_str()
            );
            let settings = Settings::from_toml(&toml_str).expect("should parse cache asset rule");

            assert_eq!(
                settings
                    .asset_cache_policy_for_path(matching_path)
                    .expect("should evaluate cache rules"),
                Some(expected_policy),
                "{style} should match its configured fingerprint convention"
            );
            assert!(
                settings
                    .asset_cache_policy_for_path(non_matching_path)
                    .expect("should evaluate cache rules")
                    .is_none(),
                "{style} should not fall through to another fingerprint convention"
            );
        }
    }

    #[test]
    fn immutable_vite_style_cannot_cache_human_named_assets() {
        let rule = format!(
            r#"{}

            [[cache.asset_rules]]
            id = "vite-assets"
            enabled = true
            path_globs = ["/assets/**/*.js", "/assets/**/*.jpg", "/assets/**/*.png", "/assets/**/*.svg"]
            fingerprint_style = "vite-base64-url"
            visibility = "public"
            browser_ttl_seconds = 31536000
            edge_ttl_seconds = 31536000
            immutable = true
        "#,
            crate_test_settings_str()
        );

        for path in [
            "/assets/hero-Portrait.jpg",
            "/assets/logo-DarkMode.svg",
            "/assets/banner-Summer24.png",
        ] {
            let error = Settings::from_toml(&rule)
                .expect_err("should reject immutable Vite-style cache rule");
            assert!(
                format!("{error:?}").contains("cannot set immutable with vite-base64-url"),
                "{path} must not receive an immutable policy through a Vite-style rule"
            );
        }
    }

    #[test]
    fn non_immutable_vite_style_remains_available_for_cache_matching() {
        let toml = format!(
            r#"{}

            [[cache.asset_rules]]
            id = "vite-assets"
            enabled = true
            path_glob = "/assets/*.js"
            fingerprint_style = "vite-base64-url"
            browser_ttl_seconds = 300
        "#,
            crate_test_settings_str()
        );
        let settings =
            Settings::from_toml(&toml).expect("should allow Vite-style matching without immutable");

        assert!(
            settings
                .asset_cache_policy_for_path("/assets/index-BsELY24f.js")
                .expect("should evaluate Vite-style cache rule")
                .is_some(),
            "non-immutable Vite-style rule should still match a Vite output filename"
        );
    }

    #[test]
    fn module_selection_allows_no_module_for_stateless_operation() {
        let ec = Ec::default();
        assert!(ec.module.is_none(), "default Ec selects no module");
        ec.validate_module_selection()
            .expect("should allow no module selected and run statelessly");
    }

    #[test]
    fn selecting_an_implementation_that_needs_settings_without_its_block_is_rejected() {
        // The built-in HMAC module has a required passphrase, so selecting
        // it with no `[ec.hmac]` block is a deployment that would run
        // stateless under a selector saying otherwise.
        let toml_str = crate_test_settings_str_with_ec_section("[ec]\nmodule = \"hmac\"\n");

        let err = Settings::from_toml(&toml_str)
            .expect_err("an implementation with no settings block should fail at startup");
        assert!(
            format!("{err:?}").contains("`hmac` is selected but has no `[ec.hmac]` configuration"),
            "should name the missing block: {err:?}"
        );
    }

    #[test]
    fn selecting_a_module_with_no_settings_needs_no_block() {
        // A block exists only when a module has settings, and only the
        // adapter that injects a module knows whether it has any, so a
        // selector naming one core does not supply is left to
        // `build_module`, which is where the injected modules are known.
        let toml_str = crate_test_settings_str_with_ec_section("[ec]\nmodule = \"acme\"\n");

        let settings =
            Settings::from_toml(&toml_str).expect("a module with no settings should need no block");
        assert!(
            settings.ec.module_blocks.is_empty(),
            "the selection should stand on its own with no block configured"
        );
    }

    /// The fixture settings with `line` added to `[ec]`.
    fn settings_toml_with_ec_line(line: &str) -> String {
        crate_test_settings_str_with_ec_section(&format!(
            "[ec]\nmodule = \"hmac\"\n{line}\n\n[ec.hmac]\n\
             passphrase = \"test-secret-key-32-bytes-minimum\"\n"
        ))
    }

    /// The `resolve_allowed_origins` line listing `entries`, each written as a
    /// TOML basic string.
    fn resolve_allowed_origins_line(entries: &[&str]) -> String {
        let written: Vec<String> = entries
            .iter()
            .map(|entry| serde_json::to_string(entry).expect("should write a string"))
            .collect();
        format!("resolve_allowed_origins = [{}]", written.join(", "))
    }

    #[test]
    fn resolve_allowed_origins_load_only_as_bare_origins() {
        let accepted: [(&str, Option<&[&str]>); 7] = [
            ("an absent list", None),
            ("an empty list", Some(&[])),
            ("https with no port", Some(&["https://www.example.com"])),
            ("https with :443", Some(&["https://www.example.com:443"])),
            ("http with :80", Some(&["http://www.example.com:80"])),
            (
                "a non-default port",
                Some(&["https://www.example.com:8443"]),
            ),
            (
                "the generator's https://{domain} shape",
                Some(&["https://www.example.com", "https://m.example.com"]),
            ),
        ];
        for (label, entries) in accepted {
            let line = entries
                .map(resolve_allowed_origins_line)
                .unwrap_or_default();

            let settings = Settings::from_toml(&settings_toml_with_ec_line(&line))
                .unwrap_or_else(|err| panic!("should load {label}: {err:?}"));

            assert_eq!(
                settings.ec.resolve_allowed_origins,
                entries.unwrap_or_default(),
                "should keep the entries of {label} as written"
            );
        }

        let refused = [
            ("https://www.example.com/", NotAnOrigin::Path),
            ("https://www.example.com/path", NotAnOrigin::Path),
            ("https://www.example.com\\", NotAnOrigin::Path),
            ("https://www.example.com?a=1", NotAnOrigin::Query),
            ("https://www.example.com#top", NotAnOrigin::Fragment),
            ("https://user@www.example.com", NotAnOrigin::Userinfo),
            ("https://user:pass@www.example.com", NotAnOrigin::Userinfo),
            ("www.example.com", NotAnOrigin::Scheme),
            ("www.example.com:443", NotAnOrigin::Scheme),
            ("ftp://www.example.com", NotAnOrigin::Scheme),
            ("wss://www.example.com", NotAnOrigin::Scheme),
            ("null", NotAnOrigin::Scheme),
            ("", NotAnOrigin::Empty),
            ("https://", NotAnOrigin::Host),
            ("https://www.example.com ", NotAnOrigin::Host),
        ];
        for (entry, reason) in refused {
            let line = resolve_allowed_origins_line(&["https://www.example.com", entry]);

            let Err(err) = Settings::from_toml(&settings_toml_with_ec_line(&line)) else {
                panic!("should refuse `{entry}` at load");
            };

            assert!(
                format!("{err:?}")
                    .contains(&format!("resolve_allowed_origins entry `{entry}` {reason}")),
                "should name `{entry}` and say it {reason}: {err:?}"
            );
        }
    }

    #[test]
    fn a_resolve_allowed_origin_with_a_trailing_slash_is_refused_at_load() {
        let line = resolve_allowed_origins_line(&["https://www.example.com/"]);

        let err = Settings::from_toml(&settings_toml_with_ec_line(&line))
            .expect_err("should refuse an entry that would never match a request's Origin");

        assert!(
            format!("{err:?}").contains("`https://www.example.com/`"),
            "should name the entry: {err:?}"
        );
    }

    #[test]
    fn cache_asset_rule_globs_respect_path_separators() {
        let toml_str = format!(
            r#"{}

            [[cache.asset_rules]]
            id = "direct-assets"
            enabled = true
            path_glob = "/assets/*.js"
            browser_ttl_seconds = 300
        "#,
            crate_test_settings_str()
        );
        let settings = Settings::from_toml(&toml_str).expect("should parse cache asset rule");

        assert!(
            settings
                .asset_cache_policy_for_path("/assets/app.js")
                .expect("should evaluate direct asset rule")
                .is_some(),
            "single-star glob should match a direct child"
        );
        for path in ["/assets/vendor/app.js", "/assets/app.JS"] {
            assert!(
                settings
                    .asset_cache_policy_for_path(path)
                    .expect("should evaluate direct asset rule")
                    .is_none(),
                "single-star glob should not match {path}"
            );
        }

        let recursive_toml = toml_str.replace("/assets/*.js", "/assets/**/*.js");
        let recursive_settings =
            Settings::from_toml(&recursive_toml).expect("should parse recursive cache asset rule");
        for path in ["/assets/app.js", "/assets/vendor/app.js"] {
            assert!(
                recursive_settings
                    .asset_cache_policy_for_path(path)
                    .expect("should evaluate recursive asset rule")
                    .is_some(),
                "double-star glob should match {path}"
            );
        }
    }

    #[test]
    fn cache_asset_rule_globs_expand_each_optional_recursive_segment() {
        let toml = format!(
            r#"{}

            [[cache.asset_rules]]
            id = "nested-assets"
            enabled = true
            path_glob = "/a/**/b/**/c.js"
            browser_ttl_seconds = 300
        "#,
            crate_test_settings_str()
        );
        let settings = Settings::from_toml(&toml).expect("should parse recursive cache rule");

        for path in ["/a/x/b/y/c.js", "/a/b/y/c.js", "/a/x/b/c.js", "/a/b/c.js"] {
            assert!(
                settings
                    .asset_cache_policy_for_path(path)
                    .expect("should evaluate recursive cache rule")
                    .is_some(),
                "recursive pattern should match {path}"
            );
        }
    }

    #[test]
    fn disabled_cache_asset_rules_defer_matcher_and_policy_validation() {
        let toml_str = format!(
            r#"{}

            [[cache.asset_rules]]
            id = "disabled-invalid-regex"
            enabled = false
            path_regex = "["

            [[cache.asset_rules]]
            id = "disabled-placeholder"
            enabled = false

            [[cache.asset_rules]]
            id = "disabled-unsafe-immutable"
            enabled = false
            path_prefix = "/assets/"
            immutable = true
        "#,
            crate_test_settings_str()
        );

        let settings =
            Settings::from_toml(&toml_str).expect("should defer disabled rule validation");
        assert!(
            settings
                .asset_cache_policy_for_path("/assets/app-DA15JTLU.js")
                .expect("should evaluate disabled cache rules")
                .is_none(),
            "disabled rules should never match"
        );
    }

    #[test]
    fn module_blocks_without_a_selector_are_rejected() {
        // A half-migrated configuration that carries an [ec.hmac] block but
        // never selects it would silently run stateless, so it is rejected at
        // startup instead.
        let toml_str = crate_test_settings_str().replace("module = \"hmac\"\n", "");

        let err = Settings::from_toml(&toml_str)
            .expect_err("a module block with no selector should fail at startup");
        assert!(
            matches!(
                err.current_context(),
                TrustedServerError::Configuration { .. }
            ),
            "should be a configuration error, got: {:?}",
            err.current_context()
        );
    }

    #[test]
    fn cache_asset_rule_policy_validation_rejects_unsafe_config() {
        let missing_ttl = format!(
            r#"{}

            [[cache.asset_rules]]
            id = "missing-ttl"
            enabled = true
            path_prefix = "/assets/"
        "#,
            crate_test_settings_str()
        );
        let missing_ttl_err =
            Settings::from_toml(&missing_ttl).expect_err("should reject rule without a TTL");
        assert!(
            format!("{missing_ttl_err:?}").contains("browser_ttl_seconds or edge_ttl_seconds"),
            "should explain missing TTL: {missing_ttl_err:?}"
        );

        let immutable_without_fingerprint_style = format!(
            r#"{}

            [[cache.asset_rules]]
            id = "unsafe-immutable"
            enabled = true
            path_prefix = "/assets/"
            browser_ttl_seconds = 31536000
            immutable = true
        "#,
            crate_test_settings_str()
        );
        let fingerprint_style_err = Settings::from_toml(&immutable_without_fingerprint_style)
            .expect_err("should reject immutable rule without a fingerprint style");
        assert!(
            format!("{fingerprint_style_err:?}").contains("fingerprint_style"),
            "should explain immutable fingerprint-style requirement: {fingerprint_style_err:?}"
        );

        let immutable_without_browser_ttl = format!(
            r#"{}

            [[cache.asset_rules]]
            id = "immutable-without-browser-ttl"
            enabled = true
            path_prefix = "/assets/"
            fingerprint_style = "hex"
            browser_ttl_seconds = 0
            edge_ttl_seconds = 31536000
            immutable = true
        "#,
            crate_test_settings_str()
        );
        let browser_ttl_err = Settings::from_toml(&immutable_without_browser_ttl)
            .expect_err("should reject immutable rule without positive browser TTL");
        assert!(
            format!("{browser_ttl_err:?}").contains("positive browser_ttl_seconds"),
            "should explain immutable browser TTL requirement: {browser_ttl_err:?}"
        );

        let private_edge_only = format!(
            r#"{}

            [[cache.asset_rules]]
            id = "private-edge-only"
            enabled = true
            path_prefix = "/assets/"
            visibility = "private"
            edge_ttl_seconds = 300
        "#,
            crate_test_settings_str()
        );
        let private_edge_only_err = Settings::from_toml(&private_edge_only)
            .expect_err("should reject private rule with only an edge TTL");
        assert!(
            format!("{private_edge_only_err:?}").contains("edge_ttl_seconds"),
            "should explain that private rules cannot use an edge TTL: {private_edge_only_err:?}"
        );

        let private_dual_ttl = private_edge_only.replace(
            "id = \"private-edge-only\"",
            "id = \"private-dual-ttl\"\n            browser_ttl_seconds = 300",
        );
        let private_dual_ttl_err = Settings::from_toml(&private_dual_ttl)
            .expect_err("should reject private rule with browser and edge TTLs");
        assert!(
            format!("{private_dual_ttl_err:?}").contains("edge_ttl_seconds"),
            "should reject edge TTL even when a private rule has a browser TTL: {private_dual_ttl_err:?}"
        );

        let private_browser_ttl = private_edge_only.replace(
            "id = \"private-edge-only\"\n            enabled = true\n            path_prefix = \"/assets/\"\n            visibility = \"private\"\n            edge_ttl_seconds = 300",
            "id = \"private-browser-ttl\"\n            enabled = true\n            path_prefix = \"/assets/\"\n            visibility = \"private\"\n            browser_ttl_seconds = 300",
        );
        let private_settings = Settings::from_toml(&private_browser_ttl)
            .expect("should accept a private rule with a browser TTL");
        let private_policy = private_settings
            .asset_cache_policy_for_path("/assets/app.js")
            .expect("should evaluate private cache rule")
            .expect("should match private cache rule");
        assert_eq!(
            private_policy
                .cache_control_value(crate::cache_policy::EdgeCacheHeader::SurrogateControl),
            "private, max-age=300",
            "private rules should render their browser TTL"
        );
        assert_eq!(
            private_policy
                .edge_header_value(crate::cache_policy::EdgeCacheHeader::SurrogateControl),
            None,
            "private rules should not render an edge cache TTL"
        );
    }

    #[test]
    fn legacy_passphrase_migrates_to_the_hmac_module() {
        let mut ec = Ec {
            passphrase: Some(Redacted::new("test-secret-key-32-bytes-minimum".to_owned())),
            ..Ec::default()
        };
        ec.migrate_legacy_ec_layout()
            .expect("should migrate the deprecated form");
        assert_eq!(
            ec.module.as_ref(),
            Some(&EcModuleSelection::from(HMAC_MODULE_KEY)),
            "the deprecated passphrase should select the hmac module"
        );
        assert_eq!(
            hmac_passphrase(&ec, HMAC_MODULE_KEY),
            "test-secret-key-32-bytes-minimum",
            "the passphrase should move into the hmac block"
        );
        assert!(
            ec.passphrase.is_none(),
            "the deprecated field should be consumed by the migration"
        );
    }

    /// The crate test configuration with its `[ec]` section rewritten to the
    /// deprecated single-passphrase form.
    fn legacy_ec_settings_str(passphrase: &str) -> String {
        let legacy = crate_test_settings_str_with_ec_section(&format!(
            "[ec]\npassphrase = \"{passphrase}\"\n"
        ));
        assert!(
            !legacy.contains("[ec.hmac]"),
            "the legacy configuration should carry no module block"
        );
        legacy
    }

    #[test]
    fn a_legacy_passphrase_is_held_to_the_passphrase_rules() {
        // Validation runs before the migration and the deprecated field
        // carries no check of its own, so the migration itself has to apply
        // the passphrase rules. Without that, a value the new `[ec.hmac]`
        // block rejects would still start a deployment from the old location.
        let short = Settings::from_toml(&legacy_ec_settings_str("short"))
            .expect_err("a short legacy passphrase should be rejected");
        assert!(
            format!("{short:?}").contains("passphrase (deprecated) is invalid"),
            "should name the deprecated passphrase as the fault: {short:?}"
        );

        let empty = Settings::from_toml(&legacy_ec_settings_str(""))
            .expect_err("an empty legacy passphrase should be rejected");
        assert!(
            format!("{empty:?}").contains("passphrase (deprecated) is invalid"),
            "should name the deprecated passphrase as the fault: {empty:?}"
        );

        let settings =
            Settings::from_toml(&legacy_ec_settings_str("test-secret-key-32-bytes-minimum"))
                .expect("a legacy passphrase of adequate length should still start");
        assert_eq!(
            settings.ec.module.as_ref(),
            Some(&EcModuleSelection::from(HMAC_MODULE_KEY)),
            "an adequate legacy passphrase should still select the hmac module"
        );
        assert_eq!(
            hmac_passphrase(&settings.ec, HMAC_MODULE_KEY),
            "test-secret-key-32-bytes-minimum",
            "an adequate legacy passphrase should still move into the hmac block"
        );
    }

    #[test]
    fn cache_asset_rule_validation_rejects_invalid_config() {
        let duplicate_ids = format!(
            r#"{}

            [[cache.asset_rules]]
            id = "duplicate"
            enabled = true
            path_prefix = "/assets/"

            [[cache.asset_rules]]
            id = "duplicate"
            enabled = true
            path_prefix = "/static/"
        "#,
            crate_test_settings_str()
        );
        let duplicate_err =
            Settings::from_toml(&duplicate_ids).expect_err("should reject duplicate rule ids");
        assert!(
            format!("{duplicate_err:?}").contains("duplicate id"),
            "should explain duplicate rule id: {duplicate_err:?}"
        );

        let invalid_regex = format!(
            r#"{}

            [[cache.asset_rules]]
            id = "bad-regex"
            enabled = true
            path_regex = "["
        "#,
            crate_test_settings_str()
        );
        let regex_err =
            Settings::from_toml(&invalid_regex).expect_err("should reject invalid regex");
        assert!(
            format!("{regex_err:?}").contains("path_regex"),
            "should explain invalid regex: {regex_err:?}"
        );

        let invalid_shape = format!(
            r#"{}

            [[cache.asset_rules]]
            id = "too-many-matchers"
            enabled = true
            path_prefix = "/assets/"
            extensions = ["js"]
        "#,
            crate_test_settings_str()
        );
        let shape_err =
            Settings::from_toml(&invalid_shape).expect_err("should reject invalid matcher shape");
        assert!(
            format!("{shape_err:?}").contains("exactly one matcher"),
            "should explain invalid matcher shape: {shape_err:?}"
        );

        let missing_matcher = format!(
            r#"{}

            [[cache.asset_rules]]
            id = "missing-matcher"
            enabled = true
            browser_ttl_seconds = 60
        "#,
            crate_test_settings_str()
        );
        let missing_matcher_err =
            Settings::from_toml(&missing_matcher).expect_err("should reject missing matcher");
        assert!(
            format!("{missing_matcher_err:?}").contains("exactly one matcher"),
            "should explain missing matcher: {missing_matcher_err:?}"
        );
    }

    #[test]
    fn legacy_passphrase_alongside_module_config_is_rejected() {
        let mut ec = Ec {
            passphrase: Some(Redacted::new("test-secret-key-32-bytes-minimum".to_owned())),
            module: Some(EcModuleSelection::from(HMAC_MODULE_KEY)),
            ..Ec::default()
        };
        let err = ec
            .migrate_legacy_ec_layout()
            .expect_err("both forms present should be rejected");
        assert!(
            matches!(
                err.current_context(),
                TrustedServerError::Configuration { .. }
            ),
            "should be a configuration error, got: {:?}",
            err.current_context()
        );
    }

    #[test]
    fn an_unknown_key_in_the_hmac_module_block_is_rejected() {
        // A mistyped key dropped silently would leave the setting the
        // operator meant to change at its default.
        let toml_str = crate_test_settings_str().replace(
            "passphrase = \"test-secret-key-32-bytes-minimum\"",
            "passphrase = \"test-secret-key-32-bytes-minimum\"\n            typo_key = \"x\"",
        );
        assert!(
            toml_str.contains("typo_key"),
            "the test configuration should carry the unknown key"
        );

        let err = Settings::from_toml(&toml_str)
            .expect_err("an unknown key in [ec.hmac] should be rejected");
        assert!(
            format!("{err:?}").contains("typo_key"),
            "should name the unknown key: {err:?}"
        );
    }

    #[test]
    fn module_none_is_explicit_stateless() {
        let ec = Ec {
            module: Some(EcModuleSelection::None),
            ..Ec::default()
        };
        ec.validate_module_selection()
            .expect("explicit none with no blocks should be valid");
    }

    #[test]
    fn module_none_with_configured_blocks_is_rejected() {
        let mut ec = Ec::default();
        select_hmac_module(&mut ec, HMAC_MODULE_KEY, "test-secret-key-32-bytes-minimum");
        ec.module = Some(EcModuleSelection::None);
        assert!(
            ec.validate_module_selection().is_err(),
            "none alongside configured blocks should be rejected"
        );
    }

    #[test]
    fn device_module_defaults_to_builtin_and_rejects_unknown() {
        let config = DeviceConfig::default();
        assert_eq!(
            config.module_key(),
            "builtin",
            "no selector should default to the built-in module"
        );
        config
            .validate_module_selection()
            .expect("should validate the built-in default");

        let fastly = DeviceConfig {
            module: Some("fastly".to_owned()),
        };
        fastly
            .validate_module_selection()
            .expect("should validate the fastly opt-in");

        // As with geo, a key core does not know is not a settings error,
        // because a module's name is a legitimate value here too.
        let module_key = DeviceConfig {
            module: Some("acme".to_owned()),
        };
        module_key
            .validate_module_selection()
            .expect("a module id should be accepted by settings validation");

        // And as with geo, the rejection happens at registry build, so a
        // mistyped selector cannot fall back to the built-in module in
        // silence.
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.device.module = Some("acme".to_owned());
        let error = match crate::integrations::IntegrationRegistry::new(&settings) {
            Ok(_) => {
                panic!("a device module no module supplies should be rejected at registry build")
            }
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(
            message.contains("acme") && message.contains("[device] module"),
            "the error should name the selector and the module, got: {message}"
        );
    }

    #[test]
    fn an_unselected_module_block_is_rejected() {
        // A vendor selector with the vendor block present, plus a stray hmac
        // block, is almost always a stale or mistyped configuration.
        let toml_str = crate_test_settings_str().replace(
            "module = \"hmac\"",
            "module = \"acme\"\n\n            [ec.acme]\n            api_key = \"example\"",
        );
        let err = Settings::from_toml(&toml_str)
            .expect_err("a configured but unselected block should fail at startup");
        assert!(
            matches!(
                err.current_context(),
                TrustedServerError::Configuration { .. }
            ),
            "should be a configuration error, got: {:?}",
            err.current_context()
        );
    }

    #[test]
    fn a_host_signals_block_is_read_as_the_built_in_modules_settings() {
        // The block is the built-in module's own settings, rather than kept as
        // the raw values of a module an adapter injects.
        let settings = Settings::from_toml(&crate_test_settings_str_with_ec_section(&format!(
            "[ec]
module = \"{HOST_SIGNALS_MODULE_KEY}\"

\
             [ec.{HOST_SIGNALS_MODULE_KEY}]
\
             passphrase = \"test-secret-key-32-bytes-minimum\"
"
        )))
        .expect("a host_signals selection with its block should load");
        assert!(
            settings
                .ec
                .module_blocks
                .get(HOST_SIGNALS_MODULE_KEY)
                .and_then(EcModuleBlock::host_signals_settings)
                .is_some(),
            "the block should be read as the host-signal module's settings"
        );
    }
    #[test]
    fn a_labeled_block_the_selector_does_not_name_is_rejected() {
        // A block under a label is still a module block, so it is held to
        // the rule every block is held to, which is that the selector names
        // it.
        let toml_str = crate_test_settings_str_with_ec_section(
            r#"[ec]
module = "hmac"

[ec.hmac]
passphrase = "test-secret-key-32-bytes-minimum"

[ec.primary]
implementation = "hmac"
passphrase = "another-test-secret-key-32-bytes"
"#,
        );

        let err = Settings::from_toml(&toml_str)
            .expect_err("a labeled block the selector does not name should fail at startup");
        let message = format!("{err:?}");
        assert!(
            message.contains("[ec.primary] is configured but `hmac` is selected"),
            "should name the unselected labeled block, got: {message}"
        );
    }

    #[test]
    fn the_removed_providers_table_is_rejected_with_its_new_location() {
        let toml_str = crate_test_settings_str_with_ec_section(
            "[ec]\nmodule = \"hmac\"\n\n[ec.providers.hmac]\npassphrase = \"test-secret-key-32-bytes-minimum\"\n",
        );

        let err = Settings::from_toml(&toml_str)
            .expect_err("the removed [ec.providers] table should be rejected");
        let message = format!("{err:?}");
        assert!(
            message.contains("[ec.providers] is no longer read") && message.contains("[ec.hmac]"),
            "should send the operator to the new location, got: {message}"
        );
    }

    #[test]
    fn a_key_under_ec_that_is_not_a_table_is_an_unknown_field() {
        // Holding the module blocks alongside the fixed keys costs the
        // section serde's own unknown-key check, so a mistyped setting has to
        // be caught where the blocks are read.
        let toml_str = crate_test_settings_str_with_ec_section(
            "[ec]\nmodule = \"hmac\"\nec_stor = \"ec_identity_store\"\n\n[ec.hmac]\npassphrase = \"test-secret-key-32-bytes-minimum\"\n",
        );

        let err =
            Settings::from_toml(&toml_str).expect_err("a mistyped [ec] key should be rejected");
        let message = format!("{err:?}");
        assert!(
            message.contains("unknown field `ec_stor`") && message.contains("`ec_store`"),
            "should name the mistyped key and the keys it could have been, got: {message}"
        );
    }

    #[test]
    fn geo_module_accepts_default_platform_and_none_and_rejects_unknown() {
        let config = GeoConfig::default();
        assert!(
            config.module.is_none(),
            "geo should default to no selector, which selects no geo module"
        );
        config
            .validate_module_selection()
            .expect("should validate the default of running without geolocation");

        let platform = GeoConfig {
            module: Some("platform".to_owned()),
            assume_single_jurisdiction: false,
        };
        platform
            .validate_module_selection()
            .expect("should validate the explicit platform selection");

        let none = GeoConfig {
            module: Some("none".to_owned()),
            assume_single_jurisdiction: false,
        };
        none.validate_module_selection()
            .expect("should validate the explicit opt-out of geolocation");

        // A key core does not know is not a settings error, because a
        // module's name is a legitimate value and a closed list here would
        // shut every module out of geo. Settings accepts it and the registry
        // decides.
        let module_key = GeoConfig {
            module: Some("acme".to_owned()),
            assume_single_jurisdiction: false,
        };
        module_key
            .validate_module_selection()
            .expect("a module id should be accepted by settings validation");

        // The rejection moved to registry build, where it is known whether any
        // module supplies the name. With no module supplying `acme`, building
        // the registry fails and the error names the selector and the module.
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.geo.module = Some("acme".to_owned());
        // `IntegrationRegistry` is not `Debug`, so the error is taken by match
        // rather than `expect_err`.
        let error = match crate::integrations::IntegrationRegistry::new(&settings) {
            Ok(_) => {
                panic!("a geo module no module supplies should be rejected at registry build")
            }
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(
            message.contains("acme") && message.contains("[geo] module"),
            "the error should name the selector and the module, got: {message}"
        );
    }

    #[test]
    fn the_reserved_ec_keys_are_the_ones_the_section_reads() {
        // The list is written out for the unknown-field message and the
        // reserved-name check, so it has to stay level with the struct.
        let ec = Ec {
            passphrase: Some(Redacted::new("test-secret-key-32-bytes-minimum".to_owned())),
            ..Ec::default()
        };
        let value = serde_json::to_value(&ec).expect("should serialize the [ec] section");
        let written = value
            .as_object()
            .expect("the section should serialize as a table")
            .keys()
            .map(String::as_str)
            .collect::<HashSet<_>>();

        assert_eq!(
            written,
            EC_SECTION_KEYS.iter().copied().collect::<HashSet<_>>(),
            "EC_SECTION_KEYS should name every key the [ec] section reads as its own"
        );
    }

    #[test]
    fn unknown_keys_in_module_sections_are_rejected() {
        // A mistyped key must fail at startup rather than silently selecting
        // a default behind the operator's back.
        for (section, bad_key) in [
            ("[geo]", "providr = \"platform\""),
            ("[device]", "providr = \"builtin\""),
        ] {
            let toml_str = format!(
                "{}\n\n            {section}\n            {bad_key}\n",
                crate_test_settings_str()
            );
            assert!(
                Settings::from_toml(&toml_str).is_err(),
                "an unknown key in {section} should be rejected"
            );
        }

        let toml_str = crate_test_settings_str()
            .replace("[ec.hmac]", "[ec.hmac]\n            unexpected = \"value\"");
        assert!(
            Settings::from_toml(&toml_str).is_err(),
            "an unknown key in [ec.hmac] should be rejected"
        );
    }

    #[test]
    fn a_reserved_key_cannot_name_a_module() {
        let toml_str = crate_test_settings_str_with_ec_section("[ec]\nmodule = \"ec_store\"\n");

        let err =
            Settings::from_toml(&toml_str).expect_err("a reserved key should not name a module");
        assert!(
            format!("{err:?}").contains("names a key the `[ec]` section reads as its own setting"),
            "should say why the name cannot be used: {err:?}"
        );
    }

    #[test]
    fn module_names_and_implementations_follow_the_module_name_rule() {
        for ec_section in [
            "[ec]\nmodule = \"Primary\"\n",
            "[ec]\nmodule = \"primary\"\n\n[ec.primary]\nimplementation = \"Hmac\"\n",
            "[ec]\nmodule = \"primary\"\n\n[ec.Primary]\nimplementation = \"hmac\"\npassphrase = \"test-secret-key-32-bytes-minimum\"\n",
        ] {
            let err = Settings::from_toml(&crate_test_settings_str_with_ec_section(ec_section))
                .expect_err("a name that is not a module name should be rejected");
            assert!(
                format!("{err:?}").contains("is not a module name"),
                "should hold `{ec_section}` to the module name rule: {err:?}"
            );
        }
    }

    #[test]
    fn an_hmac_block_without_its_passphrase_names_the_block() {
        // The implementation the block names decides how its settings are
        // read, so the block the operator wrote is what the error names.
        let toml_str = crate_test_settings_str_with_ec_section(
            "[ec]\nmodule = \"primary\"\n\n[ec.primary]\nimplementation = \"hmac\"\n",
        );

        let err = Settings::from_toml(&toml_str)
            .expect_err("an hmac block without its passphrase should be rejected");
        let message = format!("{err:?}");
        assert!(
            message.contains("[ec.primary] is invalid") && message.contains("passphrase"),
            "should name the block and the setting it lacks, got: {message}"
        );
    }

    #[test]
    fn a_labeled_block_round_trips_through_serialization() {
        // The pushed configuration is the serialized settings, so a label and
        // the implementation it names have to survive being written back.
        let toml_str = crate_test_settings_str_with_ec_section(
            "[ec]\nmodule = \"primary\"\n\n[ec.primary]\nimplementation = \"hmac\"\npassphrase = \"test-secret-key-32-bytes-minimum\"\n",
        );
        let settings = Settings::from_toml(&toml_str).expect("should parse a labeled module block");

        let value = serde_json::to_value(&settings).expect("should serialize settings");
        assert_eq!(
            value["ec"]["primary"]["implementation"], "hmac",
            "the label should keep the implementation it names"
        );

        let reparsed =
            Settings::from_json_value(value).expect("should reparse the serialized settings");
        assert_eq!(
            reparsed.ec.module_blocks.implementation("primary"),
            HMAC_MODULE_KEY,
            "the implementation should survive the round trip"
        );
        assert_eq!(
            hmac_passphrase(&reparsed.ec, "primary"),
            "test-secret-key-32-bytes-minimum"
        );

        // A block under the implementation's own name is written back as it
        // was, without gaining a key the operator never wrote.
        let written = serde_json::to_value(create_test_settings())
            .expect("should serialize the test settings");
        assert!(
            written["ec"]["hmac"].get("implementation").is_none(),
            "an unlabeled block should not gain an implementation key: {}",
            written["ec"]["hmac"]
        );
    }

    #[test]
    fn an_injected_modules_settings_are_kept_as_written() {
        // Core never names a vendor, so the block of a module an adapter
        // injects is kept as the values it held for that adapter to read.
        let toml_str = crate_test_settings_str_with_ec_section(
            "[ec]\nmodule = \"acme\"\n\n[ec.acme]\nendpoint = \"https://ec.acme.example.com\"\n",
        );
        let settings = Settings::from_toml(&toml_str).expect("should parse a vendor module block");

        let block = settings
            .ec
            .module_blocks
            .get("acme")
            .expect("should configure the acme block");
        assert_eq!(
            settings.ec.module_blocks.implementation("acme"),
            "acme",
            "a block that names no implementation is its own name's"
        );
        let EcModuleSettings::Injected(injected) = &block.settings else {
            panic!("a vendor block should keep its settings as the raw values it held");
        };
        assert_eq!(injected["endpoint"], "https://ec.acme.example.com");

        // They survive being written back into the pushed configuration, so
        // the adapter reads what the operator wrote.
        let written = serde_json::to_value(&settings).expect("should serialize settings");
        assert_eq!(
            written["ec"]["acme"]["endpoint"],
            "https://ec.acme.example.com"
        );
    }

    #[test]
    fn ec_without_geo_requires_the_single_jurisdiction_acknowledgment() {
        // The base test settings acknowledge single-jurisdiction operation.
        // Removing the acknowledgment while an EC module is configured and
        // no geo module is selected must fail at startup.
        let toml_str = crate_test_settings_str().replace("assume_single_jurisdiction = true\n", "");
        let err = Settings::from_toml(&toml_str)
            .expect_err("an EC module with no geo module needs the acknowledgment");
        assert!(
            matches!(
                err.current_context(),
                TrustedServerError::Configuration { .. }
            ),
            "should be a configuration error, got: {:?}",
            err.current_context()
        );

        // Selecting a geo module removes the requirement.
        let toml_str = crate_test_settings_str()
            .replace("assume_single_jurisdiction = true\n", "")
            .replace("[geo]", "[geo]\n            module = \"platform\"");
        Settings::from_toml(&toml_str)
            .expect("a geo module resolves jurisdictions, so no acknowledgment is needed");

        // With no EC module there is no jurisdiction consumer to protect.
        let toml_str = crate_test_settings_str()
            .replace("assume_single_jurisdiction = true\n", "")
            .replace("module = \"hmac\"", "")
            .replace(
                "[ec.hmac]\n            passphrase = \"test-secret-key-32-bytes-minimum\"",
                "",
            );
        Settings::from_toml(&toml_str)
            .expect("stateless operation needs no jurisdiction acknowledgment");
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
    fn validate_accepts_vendor_specific_ec_partner_atype() {
        let toml_str = format!(
            r#"{}
            [[ec.partners]]
            name = "PAIR Partner"
            source_domain = "google.com"
            openrtb_atype = 571187
            api_token = "test-vendor-token-32-bytes-minimum"
            "#,
            crate_test_settings_str(),
        );

        let settings = Settings::from_toml(&toml_str)
            .expect("should accept vendor-specific OpenRTB agent type");

        assert_eq!(
            settings.ec.partners[0].openrtb_atype, 571187,
            "should preserve PAIR's vendor-specific atype"
        );
    }

    #[test]
    fn validate_rejects_negative_ec_partner_atype() {
        let toml_str = format!(
            r#"{}
            [[ec.partners]]
            name = "Invalid Partner"
            source_domain = "partner.example.com"
            openrtb_atype = -1
            api_token = "test-vendor-token-32-bytes-minimum"
            "#,
            crate_test_settings_str(),
        );

        let result = Settings::from_toml(&toml_str);

        assert!(result.is_err(), "should reject negative OpenRTB agent type");
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
    fn ec_partner_api_token_can_be_omitted() {
        let partner: EcPartner = toml::from_str(
            r#"
name = "Example Partner"
source_domain = "partner.example.com"
"#,
        )
        .expect("should deserialize partner without API token");

        assert!(partner.api_token.is_none(), "should omit API token");
        let serialized = serde_json::to_value(partner).expect("should serialize partner");
        assert!(
            serialized.get("api_token").is_none(),
            "should not serialize an omitted API token"
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
    fn is_placeholder_domain_rejects_known_placeholders_case_insensitively() {
        for placeholder in Publisher::PLACEHOLDER_DOMAINS {
            assert!(
                Publisher::is_placeholder_domain(placeholder),
                "should detect placeholder domain '{placeholder}'"
            );
        }
        assert!(
            Publisher::is_placeholder_domain(" Example.COM "),
            "should detect trimmed, mixed-case placeholder domain"
        );
    }

    #[test]
    fn is_placeholder_domain_accepts_non_placeholder() {
        assert!(
            !Publisher::is_placeholder_domain("publisher.test"),
            "should accept a real publisher domain"
        );
    }

    #[test]
    fn is_placeholder_cookie_domain_rejects_known_placeholders_case_insensitively() {
        for placeholder in Publisher::PLACEHOLDER_COOKIE_DOMAINS {
            assert!(
                Publisher::is_placeholder_cookie_domain(placeholder),
                "should detect placeholder cookie_domain '{placeholder}'"
            );
        }
        assert!(
            Publisher::is_placeholder_cookie_domain(" .Example.COM "),
            "should detect trimmed, mixed-case placeholder cookie_domain"
        );
    }

    #[test]
    fn is_placeholder_cookie_domain_accepts_non_placeholder() {
        assert!(
            !Publisher::is_placeholder_cookie_domain(".publisher.test"),
            "should accept a real cookie domain"
        );
    }

    #[test]
    fn is_placeholder_origin_url_rejects_equivalent_spellings_of_reserved_host() {
        for reserved in [
            "https://origin.example.com",
            "https://origin.example.com/",
            "https://origin.example.com:443",
            "http://origin.example.com",
            "https://Origin.Example.com",
            " https://origin.example.com ",
        ] {
            assert!(
                Publisher::is_placeholder_origin_url(reserved),
                "should reject origin_url resolving to the reserved host: '{reserved}'"
            );
        }
    }

    #[test]
    fn is_placeholder_origin_url_accepts_non_placeholder() {
        assert!(
            !Publisher::is_placeholder_origin_url("https://origin.publisher.test"),
            "should accept a real origin url"
        );
        assert!(
            !Publisher::is_placeholder_origin_url("https://cdn.example.com"),
            "should accept a different host under the same example domain"
        );
    }

    fn test_partner_with_pull_token(ts_pull_token: &str) -> EcPartner {
        test_partner_with_tokens(None, ts_pull_token)
    }

    fn test_partner_with_tokens(api_token: Option<&str>, ts_pull_token: &str) -> EcPartner {
        EcPartner {
            name: "Test Partner".to_owned(),
            source_domain: "partner.example.com".to_owned(),
            openrtb_atype: EcPartner::default_openrtb_atype(),
            bidstream_enabled: false,
            api_token: api_token.map(|token| Redacted::new(token.to_owned())),
            batch_rate_limit: EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: true,
            pull_sync_url: Some("https://partner.example.com/sync".to_owned()),
            pull_sync_allowed_domains: vec!["partner.example.com".to_owned()],
            pull_sync_ttl_sec: EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: Some(Redacted::new(ts_pull_token.to_owned())),
        }
    }

    #[test]
    fn reject_placeholder_secrets_includes_partner_pull_tokens() {
        let mut settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");
        settings.publisher.proxy_secret = Redacted::new("unit-test-proxy-secret".to_owned());
        settings.ec.partners = vec![test_partner_with_pull_token(
            "partner-api-token-32-bytes-minimum",
        )];

        let err = settings
            .reject_placeholder_secrets()
            .expect_err("should reject placeholder partner pull token");
        assert!(
            format!("{err:?}").contains("ec.partners[partner.example.com].ts_pull_token"),
            "error should mention the partner pull token field"
        );
    }

    #[test]
    fn reject_placeholder_secrets_allows_realistic_partner_pull_token() {
        let mut settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");
        settings.publisher.proxy_secret = Redacted::new("unit-test-proxy-secret".to_owned());
        settings.ec.partners = vec![test_partner_with_pull_token(
            "unit-test-realistic-pull-sync-token-32-bytes-min",
        )];

        settings
            .reject_placeholder_secrets()
            .expect("should accept a realistic partner pull token");
    }

    #[test]
    fn reject_placeholder_secrets_reports_both_partner_tokens() {
        let mut settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");
        settings.publisher.proxy_secret = Redacted::new("unit-test-proxy-secret".to_owned());
        settings.ec.partners = vec![test_partner_with_tokens(
            Some("partner-api-token-32-bytes-minimum"),
            "replace-with-partner-api-token-32-bytes-minimum",
        )];

        let err = settings
            .reject_placeholder_secrets()
            .expect_err("should reject placeholder partner tokens");
        let message = format!("{err:?}");
        assert!(
            message.contains("ec.partners[partner.example.com].api_token"),
            "error should mention the partner API token field"
        );
        assert!(
            message.contains("ec.partners[partner.example.com].ts_pull_token"),
            "error should also mention the partner pull token field on the same partner"
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
                (partner_0_openrtb_atype_key, Some("571187")),
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
                assert_eq!(settings.ec.partners[0].openrtb_atype, 571187);
                assert!(settings.ec.partners[0].bidstream_enabled);
                assert_eq!(
                    settings.ec.partners[0]
                        .api_token
                        .as_ref()
                        .map(Redacted::expose)
                        .map(String::as_str),
                    Some("env-token-0")
                );
                assert_eq!(settings.ec.partners[1].name, "Env Partner 1");
                assert_eq!(
                    settings.ec.partners[1].source_domain,
                    "envpartner1.example.com"
                );
                assert_eq!(settings.ec.partners[1].openrtb_atype, 3);
                assert!(!settings.ec.partners[1].bidstream_enabled);
                assert_eq!(
                    settings.ec.partners[1]
                        .api_token
                        .as_ref()
                        .map(Redacted::expose)
                        .map(String::as_str),
                    Some("env-token-1")
                );
            },
        );
    }

    /// An environment variable can give a handler to a configuration that
    /// carries none, and that is refused as the list itself is.
    #[test]
    fn a_handler_given_by_an_environment_variable_is_refused_too() {
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
                temp_env::with_var(path_key, Some("^/secure"), || {
                    let error = Settings::from_toml_and_env(&toml_str)
                        .expect_err("should refuse a handler an environment variable gives");

                    assert!(
                        format!("{error:?}").contains("`[[handlers]]` is not read"),
                        "should say the list is not read: {error:?}"
                    );
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
    fn from_toml_and_env_loads_while_the_git_version_variable_is_set() {
        temp_env::with_var(
            format!(
                "{}{}GIT_VERSION",
                ENVIRONMENT_VARIABLE_PREFIX, ENVIRONMENT_VARIABLE_SEPARATOR
            ),
            Some("v1.0.0"),
            || {
                Settings::from_toml_and_env(&crate_test_settings_str())
                    .expect("should load settings while the build's git version variable is set");
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
    fn legacy_env_loader_ignores_build_time_git_version() {
        let toml_str = crate_test_settings_str();

        for key in [
            "TRUSTED_SERVER__GIT_VERSION",
            "trusted_server__git_version",
            "Trusted_Server__Git_Version",
        ] {
            for value in ["v0.0.0-example", ""] {
                temp_env::with_var(key, Some(value), || {
                    Settings::from_toml_and_env(&toml_str).expect(
                        "should ignore the build-time git version input regardless of case",
                    );
                });
            }
        }
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
            origin_auth: None,
            directory_index: None,
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
            origin_auth: None,
            directory_index: None,
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
            origin_auth: None,
            directory_index: None,
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
            origin_auth: None,
            directory_index: None,
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
            origin_auth: None,
            directory_index: None,
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
            origin_auth: None,
            directory_index: None,
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
            origin_auth: None,
            directory_index: None,
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
            origin_auth: None,
            directory_index: None,
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
            [publisher]
            domain = "example.com"
            cookie_domain = ".example.com"
            origin_url = "https://origin.example.com"
            proxy_secret = "unit-test-proxy-secret"

            [ec]
            module = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"

            [geo]
            assume_single_jurisdiction = true
            "#,
        )
        .expect("should parse settings without max_buffered_body_bytes");
        assert_eq!(
            from_toml.publisher.max_buffered_body_bytes,
            Publisher::default().max_buffered_body_bytes,
            "TOML default and Publisher::default() must stay aligned"
        );
    }

    /// The fixture settings with `lines` added to `[publisher]` and its
    /// `origin_url` replaced by `origin_url`.
    fn publisher_settings_with(origin_url: &str, lines: &str) -> String {
        crate_test_settings_str()
            .replacen(
                "origin_url = \"https://origin.test-publisher.com\"",
                &format!("origin_url = \"{origin_url}\""),
                1,
            )
            .replacen(
                "proxy_secret = \"unit-test-proxy-secret\"",
                &format!("proxy_secret = \"unit-test-proxy-secret\"\n{lines}"),
                1,
            )
    }

    const STORAGE_ORIGIN_URL: &str = "https://storage.example.com/bucket/site";

    #[test]
    fn publisher_object_storage_settings_are_off_by_default() {
        let settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");

        assert!(
            settings.publisher.origin_auth.is_none(),
            "origin_auth should default to unset"
        );
        assert!(
            settings.publisher.directory_index.is_none(),
            "directory_index should default to unset"
        );
        let serialized =
            serde_json::to_value(&settings.publisher).expect("should serialize publisher");
        assert!(
            serialized.get("origin_auth").is_none() && serialized.get("directory_index").is_none(),
            "unset settings should serialize as a document written before they existed: \
             {serialized}"
        );
    }

    #[test]
    fn publisher_origin_auth_reads_the_asset_route_auth_table() {
        let settings = Settings::from_toml(&publisher_settings_with(
            STORAGE_ORIGIN_URL,
            r#"origin_auth = { type = "s3_sigv4", region = "test-region-1", access_key_id = "origin_access_key_id", secret_access_key = "origin_secret_access_key", session_token = "origin_session_token", origin_query = "strip" }
            directory_index = "index.html""#,
        ))
        .expect("should parse a publisher with origin_auth and directory_index");

        let Some(AssetOriginAuth::S3SigV4(auth)) = settings.publisher.origin_auth.as_ref() else {
            panic!("should read origin_auth as the asset route auth table");
        };
        assert_eq!(auth.region, "test-region-1");
        assert_eq!(auth.access_key_id.expose(), "origin_access_key_id");
        assert_eq!(auth.secret_access_key.expose(), "origin_secret_access_key");
        assert_eq!(
            auth.session_token
                .as_ref()
                .map(Redacted::expose)
                .map(String::as_str),
            Some("origin_session_token")
        );
        assert!(
            settings.publisher.origin_query_is_stripped(),
            "origin_query = \"strip\" should leave the query off origin requests"
        );
        assert_eq!(
            settings.publisher.directory_index.as_deref(),
            Some("index.html")
        );

        let defaulted = Settings::from_toml(&publisher_settings_with(
            STORAGE_ORIGIN_URL,
            r#"origin_auth = { type = "s3_sigv4", region = "test-region-1" }"#,
        ))
        .expect("should parse origin_auth naming only its region");
        let Some(AssetOriginAuth::S3SigV4(auth)) = defaulted.publisher.origin_auth.as_ref() else {
            panic!("should read origin_auth with defaulted key references");
        };
        assert_eq!(
            (
                auth.access_key_id.expose().as_str(),
                auth.secret_access_key.expose().as_str()
            ),
            ("access_key_id", "secret_access_key"),
            "omitted key references should default as an asset route's do"
        );
        assert!(
            !defaulted.publisher.origin_query_is_stripped(),
            "the query should be kept unless origin_query strips it"
        );
    }

    #[test]
    fn publisher_directory_index_accepts_a_file_name() {
        for name in [
            "index.html",
            "default.htm",
            "index",
            "index_v2.html",
            "home~1.html",
        ] {
            let lines = format!("directory_index = {}", toml::Value::String(name.to_owned()));
            let settings = Settings::from_toml(&publisher_settings_with(
                "https://origin.test-publisher.com",
                &lines,
            ))
            .unwrap_or_else(|err| panic!("should accept directory_index `{name}`: {err:?}"));
            assert_eq!(settings.publisher.directory_index.as_deref(), Some(name));
        }
    }

    #[test]
    fn publisher_directory_index_refuses_anything_but_a_file_name() {
        let too_long = "a".repeat(super::MAX_DIRECTORY_INDEX_BYTES + 1);
        for name in [
            "",
            ".",
            "..",
            "pages/index.html",
            "/index.html",
            "index.html/",
            "index\\html",
            "index.html?lang=en",
            "index.html#top",
            "index page.html",
            " index.html",
            "%2e%2e",
            "index%2Ehtml",
            "\u{e9}t\u{e9}.html",
            too_long.as_str(),
        ] {
            let lines = format!("directory_index = {}", toml::Value::String(name.to_owned()));
            let err = Settings::from_toml(&publisher_settings_with(
                "https://origin.test-publisher.com",
                &lines,
            ))
            .expect_err("should refuse a directory_index that is not one file name");
            assert!(
                format!("{err:?}").contains("publisher.directory_index"),
                "the refusal of `{name}` should name the setting: {err:?}"
            );
        }
    }

    #[test]
    fn publisher_origin_auth_refuses_a_table_it_cannot_sign_with() {
        for (origin_url, table, expected) in [
            (
                STORAGE_ORIGIN_URL,
                r#"{ type = "s3_sigv4", region = "" }"#,
                "publisher.origin_auth s3_sigv4 region must not be empty",
            ),
            (
                STORAGE_ORIGIN_URL,
                r#"{ type = "s3_sigv4", region = "Test_Region" }"#,
                "publisher.origin_auth s3_sigv4 region must contain only lowercase letters",
            ),
            (
                STORAGE_ORIGIN_URL,
                r#"{ type = "s3_sigv4", region = "test-region-1", access_key_id = " " }"#,
                "publisher.origin_auth s3_sigv4 credentials must not be empty",
            ),
            (
                STORAGE_ORIGIN_URL,
                r#"{ type = "s3_sigv4", region = "test-region-1", secret_access_key = "" }"#,
                "publisher.origin_auth s3_sigv4 credentials must not be empty",
            ),
            (
                "http://storage.example.com/bucket/site",
                r#"{ type = "s3_sigv4", region = "test-region-1" }"#,
                "publisher.origin_auth requires publisher.origin_url to use https",
            ),
            (
                STORAGE_ORIGIN_URL,
                r#"{ type = "unknown_scheme", region = "test-region-1" }"#,
                "origin_auth",
            ),
            (
                STORAGE_ORIGIN_URL,
                r#"{ type = "s3_sigv4", region = "test-region-1", bucket = "site" }"#,
                "origin_auth",
            ),
            (STORAGE_ORIGIN_URL, r#""s3_sigv4""#, "origin_auth"),
        ] {
            let err = Settings::from_toml(&publisher_settings_with(
                origin_url,
                &format!("origin_auth = {table}"),
            ))
            .expect_err("should refuse an origin_auth the signer cannot use");
            assert!(
                format!("{err:?}").contains(expected),
                "the refusal of `{table}` should say `{expected}`: {err:?}"
            );
        }
    }

    #[test]
    fn asset_route_auth_refusals_still_name_the_asset_route() {
        let mut route = ProxyAssetRoute::new("/assets/", "https://storage.example.com");
        route.auth = Some(AssetOriginAuth::S3SigV4(S3SigV4AuthConfig {
            region: String::new(),
            secret_store: None,
            access_key_id: Redacted::new("access_key_id".to_owned()),
            secret_access_key: Redacted::new("secret_access_key".to_owned()),
            session_token: None,
            origin_query: None,
        }));

        let err = route
            .prepare_runtime()
            .expect_err("should refuse an asset route auth with no region");
        assert!(
            format!("{err:?}")
                .contains("proxy.asset_routes auth s3_sigv4 region must not be empty"),
            "the asset route refusal should keep its wording: {err:?}"
        );
    }

    #[test]
    fn publisher_origin_auth_credentials_stay_out_of_debug_output() {
        let canaries = [
            "canary-origin-access",
            "canary-origin-secret",
            "canary-origin-session",
        ];
        let publisher = Publisher {
            origin_auth: Some(AssetOriginAuth::S3SigV4(S3SigV4AuthConfig {
                region: "test-region-1".to_owned(),
                secret_store: None,
                access_key_id: Redacted::new(canaries[0].to_owned()),
                secret_access_key: Redacted::new(canaries[1].to_owned()),
                session_token: Some(Redacted::new(canaries[2].to_owned())),
                origin_query: None,
            })),
            ..Publisher::default()
        };

        let debug = format!("{publisher:?}");

        assert!(
            debug.contains("test-region-1"),
            "should leave the region visible in debug output: {debug}"
        );
        for canary in canaries {
            assert!(
                !debug.contains(canary),
                "should keep `{canary}` out of debug output"
            );
        }
    }

    #[test]
    fn rejects_zero_max_buffered_body_bytes() {
        // A zero-byte cap fails every non-empty buffered publisher response at
        // request time, so it must be rejected at config validation instead of
        // silently breaking traffic.
        let result = Settings::from_toml(
            r#"
            [publisher]
            domain = "example.com"
            cookie_domain = ".example.com"
            origin_url = "https://origin.example.com"
            proxy_secret = "unit-test-proxy-secret"
            max_buffered_body_bytes = 0

            [geo]
            assume_single_jurisdiction = true

            [ec]
            module = "hmac"

            [ec.hmac]
            passphrase = "test-secret-key-32-bytes-minimum"
            "#,
        );
        let error = result.expect_err("should reject a zero buffered-body cap");
        assert!(
            error.to_string().contains("max_buffered_body_bytes"),
            "the rejection should be for the zero cap, not another validation, got: {error}"
        );
    }

    /// The settings of an example module that takes none.
    #[derive(Debug, Deserialize, Validate)]
    #[serde(deny_unknown_fields)]
    struct NoSettings {}

    impl IntegrationConfig for NoSettings {}

    /// The settings of an example module that requires one.
    #[derive(Debug, Deserialize, Validate)]
    #[serde(deny_unknown_fields)]
    struct OneRequiredSetting {
        #[validate(url)]
        endpoint: String,
    }

    impl IntegrationConfig for OneRequiredSetting {}

    /// The example modules' names, in a section of their own type.
    const EXAMPLE_SECTION: &str = "example";
    const PLAIN_MODULE: &str = "example.plain";
    const ENDPOINT_MODULE: &str = "example.endpoint";

    /// A module no section selects has no configuration, whatever else the
    /// settings hold.
    #[test]
    fn a_module_that_is_not_selected_has_no_configuration() {
        let settings = create_test_settings();

        assert!(
            !settings.selects_module(ENDPOINT_MODULE),
            "the shared fixture should not select the example module"
        );
        assert!(
            settings
                .module_config::<OneRequiredSetting>(ENDPOINT_MODULE)
                .expect("reading an unselected module should succeed")
                .is_none(),
            "a module that is not selected should have no configuration"
        );
    }

    /// A selected module with no table of its own is read from an empty one,
    /// so one that takes no settings runs on its selection alone.
    #[test]
    fn a_selected_module_with_no_table_is_read_from_an_empty_one() {
        let mut settings = create_test_settings();
        settings.select_module(EXAMPLE_SECTION, PLAIN_MODULE);

        assert!(
            settings
                .module_config::<NoSettings>(PLAIN_MODULE)
                .expect("a module that takes no settings should read from an empty table")
                .is_some(),
            "selecting the module should be the whole configuration"
        );
    }

    /// The same empty table makes a module that requires a setting report the
    /// setting it is missing, rather than starting without it.
    #[test]
    fn a_selected_module_without_a_required_setting_names_it() {
        let mut settings = create_test_settings();
        settings.select_module(EXAMPLE_SECTION, ENDPOINT_MODULE);

        let error = settings
            .module_config::<OneRequiredSetting>(ENDPOINT_MODULE)
            .expect_err("should reject a selected module with no endpoint");

        let rendered = error.to_string();
        assert!(
            rendered.contains("[example.endpoint]") && rendered.contains("endpoint"),
            "should name the module's table and the missing setting: {rendered}"
        );
    }

    /// An example module's settings with a secret in them, as its table holds
    /// them at startup, once the secret is resolved from the store.
    #[derive(Debug, Deserialize, Validate)]
    #[serde(deny_unknown_fields)]
    struct SecretSetting {
        #[validate(url)]
        endpoint: String,
        #[validate(range(max = 10))]
        retries: Option<u32>,
    }

    impl IntegrationConfig for SecretSetting {}

    #[test]
    fn a_module_table_that_is_refused_names_the_setting_and_never_its_value() {
        const SECRET: &str = "resolved-secret-value-4f7a";
        for (table, setting) in [
            // Fails a rule, whose parameters carry the value checked.
            (serde_json::json!({ "endpoint": SECRET }), "endpoint"),
            // Cannot be read, where `serde` quotes the value it found.
            (
                serde_json::json!({ "endpoint": "https://example.com", "retries": SECRET }),
                "retries",
            ),
        ] {
            let mut settings = create_test_settings();
            settings
                .insert_module_config(EXAMPLE_SECTION, ENDPOINT_MODULE, &table)
                .expect("should insert the module's table");

            let error = settings
                .module_config::<SecretSetting>(ENDPOINT_MODULE)
                .expect_err("the table breaks the module's rules");

            let shown = format!("{error}\n{error:?}");
            assert!(
                !shown.contains(SECRET),
                "printed a setting's value: {shown}"
            );
            // The module's name holds `endpoint` too, so the setting is looked
            // for after the table's name.
            let rendered = error.to_string();
            let reason = rendered
                .split_once("[example.endpoint]")
                .map_or("", |(_, reason)| reason);
            assert!(
                reason.contains(setting),
                "should name the table and the setting: {rendered}"
            );
        }
    }

    /// A table written for a module its section does not select is refused,
    /// rather than sitting in the configuration doing nothing.
    #[test]
    fn a_table_for_a_module_that_is_not_selected_is_refused() {
        let toml = format!(
            "{}\n[testing]\nmodule = \"example\"\n\n[testing.another]\n",
            crate_test_settings_str()
        );

        let error = Settings::from_toml(&toml).expect_err("should reject a table nothing selects");
        let rendered = format!("{error:?}");

        assert!(
            rendered.contains("[testing.another] is configured")
                && rendered.contains("does not select"),
            "should name the table and the selection it is missing from: {rendered}"
        );
    }

    /// A module type's section that selects nothing is refused, which is also
    /// what a misspelt section of Trusted Server's own meets.
    #[test]
    fn a_section_that_selects_no_module_is_refused() {
        let toml = format!(
            "{}\n[framework.nextjs]\nrewrite_attributes = [\"href\"]\n",
            crate_test_settings_str()
        );

        let error =
            Settings::from_toml(&toml).expect_err("should reject a section selecting nothing");

        assert!(
            format!("{error:?}").contains("[framework] selects no module"),
            "should name the section: {error:?}"
        );
    }

    /// A key left in a module's table that its settings do not have, such as
    /// the removed `enabled`, is refused when the module reads its table, so a
    /// configuration cannot read as switched off while the module runs.
    #[test]
    fn an_enabled_key_left_in_a_table_is_refused() {
        let toml = format!(
            "{}\n[example]\nmodules = [\"endpoint\"]\n\n[example.endpoint]\nendpoint = \"https://endpoint.example\"\nenabled = false\n",
            crate_test_settings_str()
        );
        let settings = Settings::from_toml(&toml).expect("core reads no module's table itself");

        let error = settings
            .module_config::<OneRequiredSetting>(ENDPOINT_MODULE)
            .expect_err("should reject a leftover enabled key");
        let rendered = format!("{error:?}");

        assert!(
            rendered.contains("[example.endpoint]") && rendered.contains("enabled"),
            "should name the table and the key: {rendered}"
        );
    }

    /// Naming one module twice is a mistake rather than a way of running it
    /// twice, so it is refused.
    #[test]
    fn naming_a_module_twice_is_refused() {
        let toml = format!(
            "{}\n[example]\nmodules = [\"endpoint\", \"endpoint\"]\n",
            crate_test_settings_str()
        );

        let error = Settings::from_toml(&toml).expect_err("should reject a repeated name");

        assert!(
            format!("{error:?}").contains("more than once"),
            "should report the repeated name: {error:?}"
        );
    }

    /// The tables removed from the configuration are refused with the move
    /// spelled out, rather than with a bare unknown-field error.
    #[test]
    fn the_removed_integration_tables_are_refused_with_directions() {
        for (table, expected) in [
            ("[integrations.example]", "CHANGELOG.md"),
            (
                "[integration]\nmodules = [\"example\"]",
                "is no longer read",
            ),
        ] {
            let toml = format!("{}\n{table}\n", crate_test_settings_str());

            let error = Settings::from_toml(&toml).expect_err("should reject the removed table");
            let rendered = format!("{error:?}");

            assert!(
                rendered.contains("the section of its type") && rendered.contains(expected),
                "should say where modules are selected now: {rendered}"
            );
        }
    }

    /// The same refusal reaches a configuration blob, which is the shape the
    /// runtime loads rather than TOML.
    #[test]
    fn json_settings_refuse_the_removed_integration_tables() {
        for table in ["integrations", "integration"] {
            let mut value = serde_json::to_value(create_test_settings())
                .expect("should serialize the test settings fixture to JSON");
            value
                .as_object_mut()
                .expect("settings should serialize as an object")
                .insert(table.to_owned(), json!({ "example": {} }));

            let error = Settings::from_json_value(value)
                .expect_err("should reject the removed table in JSON");

            assert!(
                format!("{error:?}").contains("the section of its type"),
                "should say where modules are selected now: {error:?}"
            );
        }
    }

    #[test]
    fn invalid_settings_for_a_selected_module_fail_registry_startup() {
        fn build(
            settings: &Settings,
        ) -> Result<Option<crate::integrations::IntegrationRegistration>, Report<TrustedServerError>>
        {
            Ok(settings
                .module_config::<OneRequiredSetting>(ENDPOINT_MODULE)?
                .map(|_| crate::integrations::IntegrationRegistration::builder("endpoint").build()))
        }

        fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
            settings
                .module_config::<OneRequiredSetting>(ENDPOINT_MODULE)
                .map(|config| config.is_some())
        }

        let mut settings = create_test_settings();
        settings
            .insert_module_config(
                EXAMPLE_SECTION,
                ENDPOINT_MODULE,
                &json!({
                    "endpoint": "not a url",
                }),
            )
            .expect("should insert the example module's table");
        let extra = [crate::integrations::IntegrationBuilder::new(
            "endpoint",
            "settings-tests",
            build,
            validate,
        )
        .with_module_name(ENDPOINT_MODULE)];

        let err = match IntegrationRegistry::with_registrations(&settings, &extra) {
            Ok(_) => panic!("a selected module with invalid settings should fail startup"),
            Err(err) => err,
        };
        assert!(
            format!("{err:?}").contains("[example.endpoint]"),
            "should identify the invalid module table: {err:?}"
        );
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
    fn test_auction_creative_processing_defaults_when_omitted() {
        let toml_str =
            crate_test_settings_str().replace("[auction]\n", "[auction]\nenabled = true\n");

        let settings = Settings::from_toml(&toml_str).expect("should parse valid TOML");

        assert!(
            settings.auction.rewrite_creatives,
            "creative rewriting stays enabled when the setting is omitted"
        );
        assert!(
            !settings.auction.sanitize_creatives,
            "creative sanitization is opt-in when the setting is omitted"
        );
    }

    #[test]
    fn test_auction_rewrite_creatives_accepts_explicit_false() {
        let toml_str = crate_test_settings_str().replace(
            "[auction]\n",
            "[auction]\nenabled = true\nrewrite_creatives = false\n",
        );

        let settings = Settings::from_toml(&toml_str).expect("should parse valid TOML");

        assert!(
            !settings.auction.rewrite_creatives,
            "should disable creative rewriting when explicitly configured"
        );
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
        let toml_str = crate_test_settings_str().replace("[auction]\n", "[auction]\nenabled = true\nallowed_context_keys = [\"permutive_segments\", \"lockr_ids\"]\n");
        let settings = Settings::from_toml(&toml_str).expect("should parse valid TOML");
        assert_eq!(
            settings.auction.allowed_context_keys,
            BTreeSet::from(["permutive_segments".to_string(), "lockr_ids".to_string()])
        );
    }

    #[test]
    fn test_auction_empty_allowed_context_keys_blocks_all() {
        let toml_str = crate_test_settings_str().replace(
            "[auction]\n",
            "[auction]\nenabled = true\nallowed_context_keys = []\n",
        );
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
            modules: SectionModules::default(),
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
            modules: SectionModules::default(),
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
            modules: SectionModules::default(),
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
            modules: SectionModules::default(),
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
            modules: SectionModules::default(),
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
            modules: SectionModules::default(),
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
            modules: SectionModules::default(),
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
                assert_eq!(config.secret_store, None);
                assert_eq!(config.access_key_id.expose(), "access_key_id");
                assert_eq!(config.secret_access_key.expose(), "secret_access_key");
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

    fn assert_image_optimizer_key_collision(toml: &str, expected_fragments: &[&str]) {
        let raw: Settings = toml::from_str(toml).expect("should deserialize unnormalized settings");
        let value = serde_json::to_value(raw).expect("should serialize unnormalized settings");
        let mut messages = Vec::new();

        for result in [Settings::from_toml(toml), Settings::from_json_value(value)] {
            let error = result.expect_err("should reject colliding image optimizer keys");
            assert!(
                matches!(
                    error.current_context(),
                    TrustedServerError::Configuration { .. }
                ),
                "should return a configuration error"
            );
            let message = error.to_string();
            for expected in expected_fragments {
                assert!(
                    message.contains(expected),
                    "should identify the collision with {expected:?}: {message}"
                );
            }
            messages.push(message);
        }
        assert_eq!(messages[0], messages[1], "should report a stable collision");
    }

    #[test]
    fn image_optimizer_rejects_profile_keys_that_collide_after_trimming() {
        for (first, second, second_params) in [
            ("medium", " medium", "width=200"),
            ("medium ", "\tmedium", "width=200"),
            (" medium ", "medium", "width=100"),
        ] {
            let toml = format!(
                "{}\n[image_optimizer.profile_sets.default_images]\n\
                 default_profile = \"medium\"\n\
                 [image_optimizer.profile_sets.default_images.profiles]\n\
                 {first:?} = \"width=100\"\n{second:?} = {second_params:?}\n",
                crate_test_settings_str()
            );
            assert_image_optimizer_key_collision(
                &toml,
                &[
                    "image_optimizer.profile_sets",
                    "default_images",
                    "profiles",
                    "collide after trimming",
                    &format!("{first:?}"),
                    &format!("{second:?}"),
                    "\"medium\"",
                ],
            );
        }
    }

    #[test]
    fn image_optimizer_rejects_profile_set_keys_that_collide_after_trimming() {
        for (first, second, second_params) in [
            ("default_images", " default_images", "width=200"),
            ("default_images ", "\tdefault_images", "width=200"),
            (" default_images ", "default_images", "width=100"),
        ] {
            let toml = format!(
                "{}\n[image_optimizer.profile_sets.{first:?}.profiles]\n\
                 default = \"width=100\"\n\
                 [image_optimizer.profile_sets.{second:?}.profiles]\n\
                 default = {second_params:?}\n",
                crate_test_settings_str()
            );
            assert_image_optimizer_key_collision(
                &toml,
                &[
                    "image_optimizer.profile_sets",
                    "collide after trimming",
                    &format!("{first:?}"),
                    &format!("{second:?}"),
                    "\"default_images\"",
                ],
            );
        }
    }

    #[test]
    fn image_optimizer_reports_first_profile_key_collision_in_stable_set_order() {
        let toml = crate_test_settings_str()
            + r#"
            [image_optimizer.profile_sets." alpha_images ".profiles]
            medium = "width=100"
            " medium" = "width=200"

            [image_optimizer.profile_sets.zeta_images.profiles]
            medium = "width=300"
            " medium" = "width=400"
            "#;

        for _ in 0..32 {
            assert_image_optimizer_key_collision(
                &toml,
                &[
                    r#"image_optimizer.profile_sets." alpha_images ".profiles keys " medium" and "medium" collide after trimming to "medium""#,
                ],
            );
        }
    }

    #[test]
    fn image_optimizer_normalization_preserves_non_colliding_configuration() {
        let toml = crate_test_settings_str()
            + r#"
            [image_optimizer.profile_sets." default_images "]
            base_params = " quality=70 "
            default_profile = " medium "
            profile_param = " profile "
            aspect_ratio_param = " ratio "
            debug_param = " debug "

            [image_optimizer.profile_sets." default_images ".profiles]
            " medium " = " width=100 "
            large = "width=200"
            "" = "ignored"
            " " = "also ignored"

            [image_optimizer.profile_sets." default_images ".aspect_ratios]
            allowed = [" 1-1 ", ""]
            profiles = [" medium ", ""]

            [image_optimizer.profile_sets." default_images ".crop_offsets]
            x_param = " x "
            y_param = " y "
            buckets = [90, 10, 10]

            [image_optimizer.profile_sets.other_images.profiles]
            medium = "width=300"
            default = "width=400"

            [image_optimizer.profile_sets."".profiles]
            default = "ignored"
            " default " = "also ignored"

            [image_optimizer.profile_sets." "]

            [[proxy.asset_routes]]
            prefix = "/images/"
            origin_url = "https://assets.example.com"

            [proxy.asset_routes.image_optimizer]
            region = " us_east "
            profile_set = " default_images "
            "#;
        let raw: Settings =
            toml::from_str(&toml).expect("should deserialize unnormalized settings");
        let value = serde_json::to_value(raw).expect("should serialize unnormalized settings");

        for result in [Settings::from_toml(&toml), Settings::from_json_value(value)] {
            let settings = result.expect("should accept non-colliding keys");
            assert_eq!(
                settings.image_optimizer.profile_sets.len(),
                2,
                "should discard empty profile-set names"
            );
            let profile_set = &settings.image_optimizer.profile_sets["default_images"];
            assert_eq!(
                profile_set.base_params, "quality=70",
                "should trim base parameters"
            );
            assert_eq!(
                profile_set.default_profile, "medium",
                "should trim the default profile reference"
            );
            assert_eq!(
                profile_set.profile_param, "profile",
                "should trim the profile query parameter name"
            );
            assert_eq!(
                profile_set.aspect_ratio_param, "ratio",
                "should trim the aspect ratio query parameter name"
            );
            assert_eq!(
                profile_set.debug_param, "debug",
                "should trim the debug query parameter name"
            );
            assert_eq!(
                profile_set.profiles.len(),
                2,
                "should discard empty profile names"
            );
            assert_eq!(
                profile_set.profiles["medium"], "width=100",
                "should trim profile keys and values"
            );
            assert_eq!(
                profile_set.profiles["large"], "width=200",
                "should preserve unpadded profile keys and values"
            );
            assert_eq!(
                settings.image_optimizer.profile_sets["other_images"].profiles["medium"],
                "width=300",
                "should allow the same profile name in distinct sets"
            );
            let ratios = profile_set
                .aspect_ratios
                .as_ref()
                .expect("should retain ratios");
            assert_eq!(
                ratios.allowed,
                ["1-1"],
                "should trim allowed aspect ratios and discard empty entries"
            );
            assert_eq!(
                ratios.profiles,
                ["medium"],
                "should trim aspect ratio profile references and discard empty entries"
            );
            let offsets = profile_set
                .crop_offsets
                .as_ref()
                .expect("should retain offsets");
            assert_eq!(
                offsets.x_param, "x",
                "should trim the horizontal crop offset parameter name"
            );
            assert_eq!(
                offsets.y_param, "y",
                "should trim the vertical crop offset parameter name"
            );
            assert_eq!(
                offsets.buckets,
                [10, 90],
                "should sort and deduplicate crop offset buckets"
            );
            let route = settings.proxy.asset_routes[0]
                .image_optimizer
                .as_ref()
                .expect("should retain the route's image optimizer");
            assert_eq!(
                route.region, "us_east",
                "should trim the image optimizer region"
            );
            assert_eq!(
                route.profile_set, "default_images",
                "should trim the route's profile-set reference"
            );
        }
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
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}PREFIX"
                ),
                Some("/.image/"),
            ),
            (
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}ORIGIN_URL"
                ),
                Some("https://bucket.s3.us-west-2.amazonaws.com"),
            ),
            (
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}AUTH{separator}TYPE"
                ),
                Some("s3_sigv4"),
            ),
            (
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}AUTH{separator}REGION"
                ),
                Some("us-west-2"),
            ),
            (
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}AUTH{separator}ORIGIN_QUERY"
                ),
                Some("strip"),
            ),
            (
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}IMAGE_OPTIMIZER{separator}ENABLED"
                ),
                Some("true"),
            ),
            (
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}IMAGE_OPTIMIZER{separator}REGION"
                ),
                Some("us_west"),
            ),
            (
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}PROXY{separator}ASSET_ROUTES{separator}0{separator}IMAGE_OPTIMIZER{separator}PROFILE_SET"
                ),
                Some("default_images"),
            ),
            (
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}BASE_PARAMS"
                ),
                Some("quality=70&resize-filter=bicubic"),
            ),
            (
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}DEFAULT_PROFILE"
                ),
                Some("w828"),
            ),
            (
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}PROFILES{separator}W828"
                ),
                Some("format=auto&width=828"),
            ),
            (
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}PROFILES{separator}W1536"
                ),
                Some("format=auto&width=1536"),
            ),
            (
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}ASPECT_RATIOS{separator}ALLOWED"
                ),
                Some("[\"1-1\",\"16-9\"]"),
            ),
            (
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}ASPECT_RATIOS{separator}PROFILES"
                ),
                Some("[\"w828\",\"w1536\"]"),
            ),
            (
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}CROP_OFFSETS{separator}ENABLED"
                ),
                Some("true"),
            ),
            (
                format!(
                    "{ENVIRONMENT_VARIABLE_PREFIX}{separator}IMAGE_OPTIMIZER{separator}PROFILE_SETS{separator}DEFAULT_IMAGES{separator}CROP_OFFSETS{separator}BUCKETS"
                ),
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
            modules: SectionModules::default(),
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
            modules: SectionModules::default(),
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

    #[test]
    fn settings_parses_creative_opportunities_section() {
        let toml = r#"
[publisher]
domain = "example.com"
cookie_domain = ".example.com"
origin_url = "https://origin.example.com"
proxy_secret = "secret"

[geo]
assume_single_jurisdiction = true

[ec]
module = "hmac"

[ec.hmac]
passphrase = "test-secret-key-32-bytes-minimum"

[creative_opportunities]
gam_network_id = "21765378893"
auction_timeout_ms = 500
section_root = "home"

[[creative_opportunities.slot]]
id = "atf"
gam_unit_path = "/{network_id}/example/{section}"
page_patterns = ["/"]
formats = [{ width = 300, height = 250 }]
"#;
        let settings = Settings::from_toml(toml).expect("should parse");
        let co = settings
            .creative_opportunities
            .expect("should have creative_opportunities");
        assert!(
            co.enabled,
            "creative-opportunity templates should default to enabled"
        );
        assert_eq!(co.gam_network_id, "21765378893");
        assert_eq!(co.auction_timeout_ms, Some(500));
        assert_eq!(
            co.section_segment,
            Some(0),
            "startup finalization should materialize the dynamic-template compatibility marker"
        );
    }

    #[test]
    fn settings_disables_creative_opportunity_slots_when_configured_off() {
        let toml = format!(
            "{}\n[creative_opportunities]\nenabled = false\ngam_network_id = \"21765378893\"\n\n[[creative_opportunities.slot]]\nid = \"atf\"\npage_patterns = [\"/\"]\nformats = [{{ width = 300, height = 250 }}]\n",
            crate_test_settings_str()
        );
        let settings = Settings::from_toml(&toml).expect("should parse disabled templates");
        assert!(
            settings.creative_opportunity_slots().is_empty(),
            "disabled template delivery should expose no runtime slots"
        );
    }

    #[test]
    fn legacy_settings_loader_applies_creative_opportunity_enabled_environment_override() {
        let toml = format!(
            "{}\n[creative_opportunities]\nenabled = true\ngam_network_id = \"21765378893\"\n",
            crate_test_settings_str()
        );
        let env_key = format!(
            "{}{}CREATIVE_OPPORTUNITIES{}ENABLED",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

        temp_env::with_var(env_key, Some("false"), || {
            let settings = Settings::from_toml_and_env(&toml)
                .expect("should parse template enabled environment override");
            assert!(
                !settings
                    .creative_opportunities
                    .expect("should have creative opportunities")
                    .enabled,
                "legacy settings loader should disable template delivery"
            );
        });
    }

    #[test]
    fn settings_rejects_invalid_creative_opportunity_slot_id() {
        let toml = r#"
[publisher]
domain = "example.com"
cookie_domain = ".example.com"
origin_url = "https://origin.example.com"
proxy_secret = "secret"

[geo]
assume_single_jurisdiction = true

[ec]
module = "hmac"

[ec.hmac]
passphrase = "test-secret-key-32-bytes-minimum"

[creative_opportunities]
gam_network_id = "21765378893"

[[creative_opportunities.slot]]
id = "xss<script>"
page_patterns = ["/"]
formats = [{ width = 300, height = 250 }]
"#;
        let err = Settings::from_toml(toml).expect_err("should reject invalid slot id");
        assert!(
            format!("{err:?}").contains("Invalid creative opportunity slot config"),
            "error should mention the invalid slot id, got: {err:?}"
        );
    }

    #[test]
    fn settings_rejects_env_injected_invalid_creative_opportunity_slot_id() {
        // A TRUSTED_SERVER__CREATIVE_OPPORTUNITIES__SLOT override must go through
        // the same runtime slot validation as a TOML-defined slot, so an invalid
        // id injected via env is rejected by from_toml_and_env (the build-time
        // path uses the same validation against the merged config).
        let toml = r#"
[publisher]
domain = "example.com"
cookie_domain = ".example.com"
origin_url = "https://origin.example.com"
proxy_secret = "secret"

[geo]
assume_single_jurisdiction = true

[ec]
module = "hmac"

[ec.hmac]
passphrase = "test-secret-key-32-bytes-minimum"

[creative_opportunities]
gam_network_id = "21765378893"
"#;
        let slot_key = format!(
            "{}{}CREATIVE_OPPORTUNITIES{}SLOT",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        temp_env::with_var(
            slot_key,
            Some(
                r#"[{"id":"bad id","page_patterns":["/"],"formats":[{"width":300,"height":250}]}]"#,
            ),
            || {
                let err = Settings::from_toml_and_env(toml)
                    .expect_err("should reject env-injected invalid slot id");
                assert!(
                    format!("{err:?}").contains("Invalid creative opportunity slot config"),
                    "error should mention the invalid slot id, got: {err:?}"
                );
            },
        );
    }

    fn creative_opportunity_settings_toml(slot_body: &str) -> String {
        format!(
            r#"
[publisher]
domain = "example.com"
cookie_domain = ".example.com"
origin_url = "https://origin.example.com"
proxy_secret = "secret"

[geo]
assume_single_jurisdiction = true

[ec]
module = "hmac"

[ec.hmac]
passphrase = "test-secret-key-32-bytes-minimum"

[creative_opportunities]
gam_network_id = "21765378893"

[[creative_opportunities.slot]]
{slot_body}
"#
        )
    }

    fn assert_creative_opportunity_slot_config_rejected(slot_body: &str, expected: &str) {
        let toml = creative_opportunity_settings_toml(slot_body);
        let err = Settings::from_toml(&toml)
            .expect_err("should reject malformed creative opportunity slot");
        assert!(
            format!("{err:?}").contains(expected),
            "error should contain {expected:?}, got: {err:?}"
        );
    }

    #[test]
    fn settings_rejects_creative_opportunity_slot_without_page_patterns() {
        assert_creative_opportunity_slot_config_rejected(
            r#"
id = "atf"
page_patterns = []
formats = [{ width = 300, height = 250 }]
"#,
            "must include at least one page pattern",
        );
    }

    #[test]
    fn settings_rejects_creative_opportunity_slot_without_valid_page_patterns() {
        assert_creative_opportunity_slot_config_rejected(
            r#"
id = "atf"
page_patterns = ["["]
formats = [{ width = 300, height = 250 }]
"#,
            "must include at least one valid page pattern",
        );
    }

    #[test]
    fn settings_rejects_creative_opportunity_slot_without_formats() {
        assert_creative_opportunity_slot_config_rejected(
            r#"
id = "atf"
page_patterns = ["/"]
formats = []
"#,
            "must include at least one format",
        );
    }

    #[test]
    fn settings_rejects_creative_opportunity_slot_with_zero_dimensions() {
        assert_creative_opportunity_slot_config_rejected(
            r#"
id = "atf"
page_patterns = ["/"]
formats = [{ width = 0, height = 250 }]
"#,
            "must have positive width and height",
        );
    }

    #[test]
    fn settings_rejects_creative_opportunity_slot_with_empty_gam_unit_path() {
        assert_creative_opportunity_slot_config_rejected(
            r#"
id = "atf"
gam_unit_path = ""
page_patterns = ["/"]
formats = [{ width = 300, height = 250 }]
"#,
            "gam_unit_path template must not be empty",
        );
    }

    #[test]
    fn settings_rejects_dynamic_gam_unit_path_over_byte_limit_using_configured_values() {
        let gam_unit_path = "{network_id}".repeat(10);
        let slot_body = format!(
            r#"
id = "atf"
gam_unit_path = "{gam_unit_path}"
page_patterns = ["/"]
formats = [{{ width = 300, height = 250 }}]
"#
        );

        assert_creative_opportunity_slot_config_rejected(
            &slot_body,
            "must render to at most 100 UTF-8 bytes",
        );
    }

    /// An unset selector must not serialize as `"module": null`.
    ///
    /// The section as a whole is skipped while every field is default, so this
    /// serializes the struct directly. Once a later change makes another field
    /// required, the section is always emitted and a null selector would then
    /// reach a config blob, where a binary that predates the field rejects it.
    #[test]
    fn an_unset_module_selector_is_omitted_from_the_serialized_section() {
        let geo = GeoConfig::default();
        let json = serde_json::to_string(&geo).expect("should serialize the geo section");
        assert!(
            !json.contains("module"),
            "an unset geo selector should be omitted rather than serialized as null, got {json}"
        );

        let device = DeviceConfig::default();
        let json = serde_json::to_string(&device).expect("should serialize the device section");
        assert!(
            !json.contains("module"),
            "an unset device selector should be omitted rather than serialized as null, got {json}"
        );
    }
}

#[cfg(test)]
mod permission_signal_config_tests {
    use super::*;
    use serde_json::json;

    use crate::config::TrustedServerAppConfig;
    use crate::test_support::tests::crate_test_settings_str;

    // Which names are valid is only known where the scheme crates are linked,
    // so the checks that a name matches an available module, and that none
    // is repeated, live with the seam in `permission_signal::select`. What is
    // tested here is the shape of the section itself.

    /// The test fixture's configuration with `section` written as its
    /// `[permission-signal]` section.
    fn settings_toml_with(section: &str) -> String {
        format!(
            "{}\n[permission-signal]\n{section}\n",
            crate_test_settings_str()
        )
    }

    #[test]
    fn no_section_is_allowed_and_means_every_module() {
        let config = PermissionSignalConfig::default();
        assert!(
            config.modules.is_none(),
            "absent rather than empty, because the two mean opposite things"
        );
    }

    #[test]
    fn the_section_round_trips_through_toml() {
        let parsed: PermissionSignalConfig =
            toml::from_str(r#"modules = ["gpc", "tcf"]"#).expect("should parse the section");
        assert_eq!(
            parsed.modules.as_deref(),
            Some(["gpc".to_owned(), "tcf".to_owned()].as_slice()),
            "the order written is the order read, because the order is the policy"
        );
    }

    #[test]
    fn the_section_round_trips_through_a_config_blob() {
        // The section is written by derive and read by hand, so what a push
        // writes into a blob must be what a deployment reads back from it.
        let written = PermissionSignalConfig {
            modules: Some(vec!["tcf".to_owned(), "gpc".to_owned()]),
        };
        let blob = serde_json::to_value(&written).expect("should write the section");
        let read: PermissionSignalConfig =
            serde_json::from_value(blob).expect("should read back what was written");
        assert_eq!(read, written, "the list and its order survive the blob");

        let null: PermissionSignalConfig = serde_json::from_value(json!({ "modules": null }))
            .expect("should read an explicit null");
        assert_eq!(
            null,
            PermissionSignalConfig::default(),
            "an explicit null is the same as leaving the key out"
        );
    }

    #[test]
    fn an_empty_list_is_kept_apart_from_no_list() {
        let parsed: PermissionSignalConfig =
            toml::from_str("modules = []").expect("should parse an empty list");
        assert_eq!(
            parsed.modules.as_deref(),
            Some(&[][..]),
            "a publisher acting on no signal at all writes an empty list, and it must \
             not read back as having written nothing"
        );
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let error = toml::from_str::<PermissionSignalConfig>(r#"module_list = ["gpc"]"#)
            .expect_err("should refuse a misspelled key rather than silently ignore it");
        assert!(
            error
                .to_string()
                .contains("unknown field `module_list` in [permission-signal], expected `modules`"),
            "the refusal names the key it did not recognize and the one it accepts: {error}"
        );
    }

    #[test]
    fn the_singular_key_is_refused_naming_modules() {
        let error = toml::from_str::<PermissionSignalConfig>(r#"module = ["gpc"]"#)
            .expect_err("should refuse the singular key the other sections use");
        assert!(
            error
                .to_string()
                .contains("[permission-signal] module is `modules` here"),
            "the refusal says which key to write instead: {error}"
        );
    }

    #[test]
    fn the_removed_sources_key_is_refused_naming_modules() {
        for written in [
            r#"sources = ["gpc", "tcf"]"#,
            "modules = [\"gpc\", \"tcf\"]\nsources = [\"gpc\", \"tcf\"]",
        ] {
            let error = toml::from_str::<PermissionSignalConfig>(written)
                .expect_err("should refuse the removed key, alone or beside its replacement");
            assert!(
                error.to_string().contains(
                    "[permission-signal] sources is no longer accepted. Name the modules \
                     to run, in order, in [permission-signal] modules instead"
                ),
                "the refusal says which key to write instead: {error}"
            );
        }
    }

    #[test]
    fn every_way_settings_are_read_refuses_the_removed_sources_key() {
        let written = settings_toml_with(r#"sources = ["gpc", "tcf"]"#);

        let error = Settings::from_toml(&written).expect_err("should refuse the removed key");
        assert!(
            format!("{error:?}").contains("[permission-signal] modules"),
            "reading a TOML file names the key that replaced it: {error:?}"
        );

        // `ts config push` parses the file into a TOML value before reading the
        // settings from it.
        let value: toml::Value = toml::from_str(&written).expect("should parse as TOML");
        let error = value
            .try_into::<TrustedServerAppConfig>()
            .expect_err("should refuse the removed key before a push");
        assert!(
            error.to_string().contains("[permission-signal] modules"),
            "a push names the key that replaced it: {error}"
        );

        // A deployment reads its settings from a JSON config blob.
        let settings = Settings::from_toml(&crate_test_settings_str())
            .expect("should load the test settings fixture");
        let mut blob = serde_json::to_value(settings).expect("should serialize the fixture");
        blob["permission-signal"] = json!({ "sources": ["gpc", "tcf"] });
        let error =
            Settings::from_json_value(blob).expect_err("should refuse the removed key at startup");
        assert!(
            format!("{error:?}").contains("[permission-signal] modules"),
            "startup names the key that replaced it: {error:?}"
        );
    }

    #[test]
    fn the_old_table_name_is_refused_with_its_new_name() {
        let written = format!(
            "{}\n[permission_signal]\nmodules = [\"gpc\"]\n",
            crate_test_settings_str()
        );
        let error = Settings::from_toml(&written).expect_err("should refuse the old table name");
        let message = format!("{error:?}");
        assert!(
            message.contains("[permission_signal]") && message.contains("[permission-signal]"),
            "the refusal names the old table and the new one: {message}"
        );
    }

    #[test]
    fn the_old_ad_server_table_name_is_refused_with_its_new_name() {
        let written = format!(
            "{}\n[adserver]\nmodule = \"adserver_mock\"\n",
            crate_test_settings_str()
        );
        let error = Settings::from_toml(&written).expect_err("should refuse the old table name");
        let message = format!("{error:?}");
        assert!(
            message.contains("[adserver]") && message.contains("[ad-server] module"),
            "the refusal names the old table and the new selector: {message}"
        );
    }

    #[test]
    fn a_block_of_module_settings_is_refused_as_an_unknown_field() {
        // No module takes settings yet, so a block for one is refused rather
        // than read and then ignored.
        let written =
            settings_toml_with("modules = [\"gpc\"]\n\n[permission-signal.gpc]\nenabled = true");
        let error =
            Settings::from_toml(&written).expect_err("should refuse settings no module takes");
        let message = format!("{error:?}");
        assert!(
            message.contains("unknown field `gpc` in [permission-signal]")
                && message.contains("[permission-signal.<name>] block is not accepted"),
            "the refusal names the block and says why it is refused: {message}"
        );
    }

    mod shared_tables {
        use super::*;

        #[derive(Debug, Deserialize, Validate)]
        #[serde(deny_unknown_fields)]
        struct ExampleShared {
            #[validate(url)]
            endpoint: String,
        }

        fn with(toml: &str) -> Result<Settings, Report<TrustedServerError>> {
            Settings::from_toml(&format!("{}\n{toml}", crate_test_settings_str()))
        }

        #[test]
        fn a_vendor_reads_its_own_shared_table_and_nothing_else() {
            let settings = with(
                r#"
                [shared.example]
                endpoint = "https://api.example.com/v1"
                "#,
            )
            .expect("should load");

            let read: ExampleShared = settings
                .shared_config("example")
                .expect("should read")
                .expect("the table is present");
            assert_eq!(read.endpoint, "https://api.example.com/v1");
            assert!(
                settings
                    .shared_config::<ExampleShared>("another")
                    .expect("an absent table reads as nothing")
                    .is_none()
            );
        }

        /// A vendor's settings with a secret in them, as the table holds them
        /// at startup, once the secret is resolved from the store.
        #[derive(Debug, Deserialize, Validate)]
        #[serde(deny_unknown_fields)]
        struct SecretShared {
            #[validate(length(max = 8))]
            license_key: String,
            #[validate(range(max = 10))]
            retries: Option<u32>,
        }

        const SECRET: &str = "resolved-secret-value-4f7a";

        #[test]
        fn a_shared_table_that_is_refused_names_the_setting_and_never_its_value() {
            for (table, setting) in [
                // Fails a rule, whose parameters carry the value checked.
                (format!("license_key = '{SECRET}'"), "license_key"),
                // Cannot be read, where `serde` quotes the value it found.
                (
                    format!("license_key = 'short'\nretries = '{SECRET}'"),
                    "retries",
                ),
            ] {
                let settings = with(&format!("[shared.example]\n{table}"))
                    .expect("core reads none of the table, so it loads");

                let refused = settings
                    .shared_config::<SecretShared>("example")
                    .expect_err("the table breaks the vendor's rules");

                let shown = format!("{refused}\n{refused:?}");
                assert!(
                    !shown.contains(SECRET),
                    "printed a setting's value: {shown}"
                );
                assert!(
                    shown.contains("[shared.example]") && shown.contains(setting),
                    "should name the table and the setting: {shown}"
                );
            }
        }

        #[test]
        fn a_shared_table_its_vendor_cannot_read_is_refused_naming_the_table() {
            let settings = with(
                r#"
                [shared.example]
                endpoint = "https://api.example.com/v1"
                unknown = true
                "#,
            )
            .expect("core reads none of the table, so it loads");

            let refused = format!(
                "{:?}",
                settings
                    .shared_config::<ExampleShared>("example")
                    .expect_err("the vendor does not know the key")
            );
            assert!(
                refused.contains("[shared.example] could not be read")
                    && refused.contains("unknown"),
                "{refused}"
            );
        }

        #[test]
        fn a_shared_table_failing_its_validation_is_refused_naming_the_table() {
            let settings = with(
                r#"
                [shared.example]
                endpoint = "not-a-url"
                "#,
            )
            .expect("core reads none of the table, so it loads");

            let refused = format!(
                "{:?}",
                settings
                    .shared_config::<ExampleShared>("example")
                    .expect_err("the endpoint is not a URL")
            );
            assert!(
                refused.contains("[shared.example] failed validation"),
                "{refused}"
            );
        }

        #[test]
        fn a_shared_table_not_named_as_a_vendor_is_refused() {
            for table in ["[shared.Example]", "[shared.\"an.example\"]"] {
                let refused = with(&format!("{table}\nendpoint = \"https://api.example.com\""))
                    .expect_err("a vendor's table is named by its folder");

                assert!(
                    format!("{refused:?}").contains("which is not a vendor's name"),
                    "{table}: {refused:?}"
                );
            }
        }

        /// The tables hold secrets, so a settings dump shows the vendors alone,
        /// and the serialized settings carry the tables whole.
        #[test]
        fn a_shared_table_is_shown_by_its_vendor_and_serialized_whole() {
            let settings = with(
                r#"
                [shared.example]
                endpoint = "https://api.example.com/v1"
                license_key = "a-secret-value"
                "#,
            )
            .expect("should load");

            let shown = format!("{:?}", settings.shared);
            assert!(shown.contains("example"), "{shown}");
            assert!(!shown.contains("a-secret-value"), "{shown}");

            let serialized = serde_json::to_value(&settings).expect("should serialize");
            assert_eq!(
                serialized.pointer("/shared/example/endpoint"),
                Some(&serde_json::json!("https://api.example.com/v1"))
            );
        }

        #[test]
        fn settings_without_a_shared_section_are_written_without_one() {
            let settings = with("").expect("should load");

            let serialized = serde_json::to_value(&settings).expect("should serialize");
            assert!(serialized.get("shared").is_none());
        }
    }
}

#[cfg(test)]
mod permissions_config_tests {
    use super::*;

    use crate::test_support::tests::crate_test_settings_str;

    /// The test fixture's configuration with `table` written as its
    /// `[permissions]` table.
    fn settings_toml_with(table: &str) -> String {
        format!("{}\n[permissions]\n{table}\n", crate_test_settings_str())
    }

    #[test]
    fn the_table_names_the_policy_file_by_its_stem() {
        let settings = Settings::from_toml(&settings_toml_with(r#"policy = "standard""#))
            .expect("should parse a table naming a policy");

        assert_eq!(
            settings.permissions.policy.as_deref(),
            Some("standard"),
            "should read the policy's name"
        );
        assert_eq!(settings.permissions.sha256, None);
        assert_eq!(settings.permissions.text, None);
    }

    #[test]
    fn an_unknown_key_in_the_table_is_refused() {
        let error = Settings::from_toml(&settings_toml_with(r#"file = "standard.yaml""#))
            .expect_err("should refuse a key the table does not define");

        let message = format!("{error:?}");
        assert!(
            message.contains("unknown field `file`"),
            "the refusal names the key: {message}"
        );
    }

    #[test]
    fn the_table_round_trips_through_a_blob() {
        let mut settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");
        settings.permissions = PermissionsConfig {
            policy: Some("standard".to_owned()),
            sha256: Some("80b7eafed0d1".to_owned()),
            text: Some("groups: {}\n".to_owned()),
        };

        let data = serde_json::to_value(&settings).expect("should serialize settings");
        let read: Settings = serde_json::from_value(data).expect("should read the blob back");

        assert_eq!(read.permissions, settings.permissions);
    }

    #[test]
    fn settings_that_name_no_policy_leave_the_table_out_of_a_blob() {
        let settings =
            Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings");

        let data = serde_json::to_value(&settings).expect("should serialize settings");

        assert!(
            data.get("permissions").is_none(),
            "an empty table should not be written: {data}"
        );
    }
}
