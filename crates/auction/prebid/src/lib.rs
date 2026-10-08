//! Prebid.js on the page, the module `auction.prebid`.
//!
//! Selected in `[auction]`, it writes the configuration Prebid.js reads into
//! `<head>`, serves the publisher's Prebid bundle from a first-party route and
//! removes the publisher's own Prebid script from the page. The bidders it
//! tells the browser to leave to the server are the auction plan's routes.

#![cfg_attr(
    test,
    allow(
        clippy::print_stdout,
        clippy::print_stderr,
        clippy::panic,
        clippy::dbg_macro,
        clippy::unwrap_used,
        reason = "tests use direct diagnostics and panic-on-failure helpers"
    )
)]

use std::collections::HashSet;
use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use base64::{
    Engine as _,
    engine::general_purpose::{
        STANDARD as BASE64_STANDARD, STANDARD_NO_PAD as BASE64_STANDARD_NO_PAD,
    },
};
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::header::HeaderValue;
use http::{Method, StatusCode, header};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use url::{Url, Url as ParsedUrl};
use validator::{Validate, ValidationError};

use trusted_server_core::auction::plan::AuctionPlan;
use trusted_server_core::cache_policy::{CacheControlPolicy, EdgeCacheHeader};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
    IntegrationEndpoint, IntegrationHeadInjector, IntegrationHtmlContext, IntegrationProxy,
    IntegrationRegistration,
};
use trusted_server_core::platform::RuntimeServices;
use trusted_server_core::proxy::{ProxyRequestConfig, is_host_allowed, proxy_request};
use trusted_server_core::settings::{IntegrationConfig, Settings};

pub(crate) const PREBID_INTEGRATION_ID: &str = "prebid";

/// The name this module is selected by, in `[auction]`.
pub const MODULE: &str = "auction.prebid";

/// The builder a deployment hands to an adapter. Prebid is selected in
/// `[auction]`, registered from the auction plan and checked against it.
#[must_use]
pub fn builder() -> trusted_server_core::integrations::IntegrationBuilder {
    trusted_server_core::integrations::IntegrationBuilder::new(
        PREBID_INTEGRATION_ID,
        env!("CARGO_PKG_NAME"),
        registered_from_the_plan,
        validate,
    )
    .with_module_name(MODULE)
    .with_plan_registration(register_for_plan)
    .with_plan_validator(validate_against_plan)
}

/// Prebid's registration needs the auction plan, so [`register_for_plan`]
/// makes it and the settings alone add nothing.
fn registered_from_the_plan(
    _settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    Ok(None)
}

/// Validates the browser configuration for deployment and reports whether
/// `[auction]` selects Prebid.
fn validate(settings: &Settings) -> Result<bool, Report<TrustedServerError>> {
    let Some(config) = settings.module_config::<PrebidIntegrationConfig>(MODULE)? else {
        return Ok(false);
    };
    validate_browser_config_for_startup(&config, &settings.proxy.allowed_domains)?;
    Ok(true)
}

/// Refuses a bidder the browser configuration keeps client-side that the
/// plan also runs server-side.
fn validate_against_plan(
    settings: &Settings,
    plan: &AuctionPlan,
) -> Result<(), Report<TrustedServerError>> {
    let Some(config) = settings.module_config::<PrebidIntegrationConfig>(MODULE)? else {
        return Ok(());
    };
    validate_browser_bidder_ownership(&config, plan)
}
const PREBID_BUNDLE_ROUTE: &str = "/integrations/prebid/bundle.js";
const PREBID_BUNDLE_CONTENT_TYPE: &str = "application/javascript; charset=utf-8";
const PREBID_BUNDLE_IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const PREBID_BUNDLE_REVALIDATION_CACHE_CONTROL: &str =
    "public, max-age=300, s-maxage=300, stale-while-revalidate=60, stale-if-error=86400";
const PREBID_BUNDLE_ERROR_CACHE_CONTROL: &str = "no-store";
const PREBID_BUNDLE_ERROR_CONTENT_TYPE: &str = "text/plain; charset=utf-8";
const PREBID_BUNDLE_NOSNIFF_HEADER: &str = "x-content-type-options";
const PREBID_BUNDLE_NOSNIFF_VALUE: &str = "nosniff";

/// Rejects a Prebid User ID identifier that Prebid.js could not address.
///
/// Applies only the constraints Prebid itself imposes on a `userSync.userIds`
/// entry name and on a storage key: a non-empty ASCII token with no
/// surrounding whitespace. Anything narrower would encode one vendor's rules
/// into core.
fn validate_prebid_user_id_token(value: &str) -> Result<(), ValidationError> {
    let is_valid = !value.is_empty()
        && value.trim() == value
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if is_valid {
        return Ok(());
    }

    let mut error = ValidationError::new("invalid_prebid_user_id_token");
    error.message = Some(
        "must be a non-empty ASCII token of letters, digits, `_`, `-`, or `.` without surrounding whitespace"
            .into(),
    );
    Err(error)
}

/// Browser storage mechanism for an operator-managed Prebid User ID module.
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PrebidUserIdStorageType {
    /// Store the module's value in a browser cookie.
    #[default]
    Cookie,
    /// Store the module's value in browser local storage.
    Html5,
}

/// Browser storage settings forwarded verbatim to a Prebid User ID module.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct PrebidManagedUserIdStorage {
    /// Browser storage mechanism.
    #[serde(default, rename = "type")]
    pub storage_type: PrebidUserIdStorageType,
    /// Cookie or local-storage key the module reads and writes.
    #[validate(custom(function = "validate_prebid_user_id_token"))]
    pub name: String,
    /// Number of days the browser retains the stored value.
    ///
    /// Omitted leaves Prebid's own default in place. Core applies no upper
    /// bound: the ceiling is a property of the selected module, not of Trusted
    /// Server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(range(min = 1))]
    pub expires: Option<u16>,
    /// Number of seconds before the module may refresh the stored value.
    ///
    /// Omitted leaves Prebid's own default in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(range(min = 1))]
    pub refresh_in_seconds: Option<u32>,
}

/// Rejects a managed User ID list whose names address one module twice.
///
/// Prebid matches `userSync.userIds` entry names to submodules
/// case-insensitively and takes the first matching entry, so two entries whose
/// names differ only by case give one submodule two conflicting configurations
/// with no defined winner.
fn validate_unique_managed_user_id_names(
    entries: &[PrebidManagedUserIdConfig],
) -> Result<(), ValidationError> {
    let mut seen = HashSet::with_capacity(entries.len());
    let Some(duplicate) = entries
        .iter()
        .find(|entry| !seen.insert(entry.name.to_ascii_lowercase()))
    else {
        return Ok(());
    };

    let mut error = ValidationError::new("duplicate_managed_user_id_name");
    // Name the matching rule: for a collision that differs only by case, the
    // printed name alone does not look repeated in the operator's config.
    error.message = Some(
        format!(
            "managed Prebid User ID module `{}` is configured more than once (names are matched case-insensitively)",
            duplicate.name
        )
        .into(),
    );
    Err(error)
}

/// Operator-owned Prebid User ID module entry that Trusted Server manages.
///
/// Core treats every entry as opaque: it validates only what Prebid.js needs to
/// address the module, then forwards the entry to the browser unchanged. Which
/// identity vendor an entry selects is an operator configuration choice, not a
/// property of core.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct PrebidManagedUserIdConfig {
    /// Prebid `userSync.userIds` entry name, for example `sharedId`.
    #[validate(custom(function = "validate_prebid_user_id_token"))]
    pub name: String,
    /// Module-specific parameters, forwarded to Prebid without inspection.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub params: serde_json::Map<String, Json>,
    /// Optional browser storage settings for the module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[validate(nested)]
    pub storage: Option<PrebidManagedUserIdStorage>,
}

/// The configuration the tests build an integration from without an auction
/// plan: the browser settings, and the bidder list a plan would supply.
#[cfg(test)]
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct LegacyPrebidConfig {
    /// Prebid Server account ID, injected into the client-side bundle via
    /// `window.__tsjs_prebid.accountId` so publishers don't need to configure
    /// it in JavaScript.
    #[serde(default)]
    pub account_id: Option<String>,
    /// Prebid User ID modules that Trusted Server installs and keeps installed.
    ///
    /// Each entry is forwarded to Prebid.js verbatim; publisher-configured
    /// entries with other names are preserved. Names must be unique.
    #[serde(default)]
    #[validate(nested, custom(function = "validate_unique_managed_user_id_names"))]
    pub managed_user_ids: Vec<PrebidManagedUserIdConfig>,
    #[serde(default = "default_timeout_ms")]
    #[validate(range(min = 1, max = 60000))]
    pub timeout_ms: u32,
    #[serde(
        default = "default_bidders",
        deserialize_with = "trusted_server_core::settings::vec_from_seq_or_map"
    )]
    pub bidders: Vec<String>,
    #[serde(default)]
    pub debug: bool,
    /// Patterns to match Prebid script URLs for serving empty JS.
    /// Supports suffix matching (e.g., "/prebid.min.js" matches any path ending with that)
    /// and wildcard patterns (e.g., "/static/prebid/*" matches paths under that prefix).
    #[serde(
        default = "default_script_patterns",
        deserialize_with = "trusted_server_core::settings::vec_from_seq_or_map"
    )]
    pub script_patterns: Vec<String>,
    /// Absolute HTTPS URL of the generated external Prebid bundle.
    #[serde(default)]
    #[validate(custom(function = "validate_external_bundle_url"))]
    pub external_bundle_url: Option<String>,
    /// Optional hex SHA-256 of the exact external bundle bytes.
    #[serde(default)]
    #[validate(regex(
        path = *EXTERNAL_BUNDLE_SHA256_PATTERN,
        message = "external_bundle_sha256 must be a 64-character hex SHA-256"
    ))]
    pub external_bundle_sha256: Option<String>,
    /// Optional browser Subresource Integrity value for the first-party script.
    #[serde(default)]
    #[validate(custom(function = "validate_external_bundle_sri"))]
    pub external_bundle_sri: Option<String>,
    /// Bidders that should run client-side in the browser via native Prebid.js
    /// adapters instead of being routed through the server-side auction.
    ///
    /// These bidders are **not** absorbed into the `trustedServer` adapter and
    /// remain as standalone bids in each ad unit.  The corresponding Prebid.js
    /// adapter modules must be statically imported in the JS bundle so they are
    /// available at runtime.
    ///
    /// This list is independent of [`bidders`](Self::bidders) — the operator
    /// manages both lists explicitly.
    #[serde(
        default,
        deserialize_with = "trusted_server_core::settings::vec_from_seq_or_map"
    )]
    pub client_side_bidders: Vec<String>,
    /// GAM ad-unit-path suffixes excluded from Trusted Server refresh auctions.
    ///
    /// Matching is exact and case-sensitive. Excluded slots still refresh through
    /// GAM, but are not included in synthetic Prebid refresh ad units.
    #[serde(
        default,
        deserialize_with = "trusted_server_core::settings::vec_from_seq_or_map"
    )]
    #[validate(custom(function = "validate_excluded_gam_ad_unit_path_suffixes"))]
    pub excluded_gam_ad_unit_path_suffixes: Vec<String>,
}

#[cfg(test)]
impl IntegrationConfig for LegacyPrebidConfig {}

/// CLI build inputs retained in app config but ignored safely by the runtime.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrebidBundleBuildConfig {
    /// Typed Prebid.js module selections consumed by `ts prebid client`.
    #[serde(default)]
    pub modules: PrebidBundleModulesConfig,
}

/// Exact Prebid.js module stems selected by `ts prebid client`.
///
/// The CLI validates these values. The runtime only parses them so app config
/// carrying build inputs remains loadable.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrebidBundleModulesConfig {
    /// Bidder adapter module stems.
    #[serde(default)]
    pub bidder: Vec<String>,
    /// User ID module stems; omission selects the generator's curated preset.
    #[serde(default)]
    pub user_id: Option<Vec<String>>,
    /// Analytics adapter module stems; omission selects no analytics adapters.
    #[serde(default)]
    pub analytics: Option<Vec<String>>,
}

/// Browser-only Prebid integration settings.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct PrebidIntegrationConfig {
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
    #[serde(default)]
    pub debug: bool,
    #[serde(
        default = "default_script_patterns",
        deserialize_with = "trusted_server_core::settings::vec_from_seq_or_map"
    )]
    pub script_patterns: Vec<String>,
    #[serde(default)]
    #[validate(custom(function = "validate_external_bundle_url"))]
    pub external_bundle_url: Option<String>,
    #[serde(default)]
    #[validate(regex(
        path = *EXTERNAL_BUNDLE_SHA256_PATTERN,
        message = "external_bundle_sha256 must be a 64-character hex SHA-256"
    ))]
    pub external_bundle_sha256: Option<String>,
    #[serde(default)]
    #[validate(custom(function = "validate_external_bundle_sri"))]
    pub external_bundle_sri: Option<String>,
    #[serde(
        default,
        deserialize_with = "trusted_server_core::settings::vec_from_seq_or_map"
    )]
    pub client_side_bidders: Vec<String>,
    #[serde(
        default,
        deserialize_with = "trusted_server_core::settings::vec_from_seq_or_map"
    )]
    #[validate(custom(function = "validate_excluded_gam_ad_unit_path_suffixes"))]
    pub excluded_gam_ad_unit_path_suffixes: Vec<String>,
    /// Prebid User ID modules that Trusted Server installs and keeps installed.
    ///
    /// Each entry is forwarded to Prebid.js verbatim; publisher-configured
    /// entries with other names are preserved. Names must be unique, and no two
    /// names may resolve to the same Prebid User ID submodule.
    #[serde(default)]
    #[validate(nested, custom(function = "validate_unique_managed_user_id_names"))]
    pub managed_user_ids: Vec<PrebidManagedUserIdConfig>,
    /// CLI-only external bundle build inputs; runtime registration ignores these fields.
    #[serde(default)]
    pub bundle: PrebidBundleBuildConfig,
}

impl Default for PrebidIntegrationConfig {
    fn default() -> Self {
        Self {
            account_id: None,
            timeout_ms: default_timeout_ms(),
            debug: false,
            script_patterns: default_script_patterns(),
            external_bundle_url: None,
            external_bundle_sha256: None,
            external_bundle_sri: None,
            client_side_bidders: Vec::new(),
            excluded_gam_ad_unit_path_suffixes: Vec::new(),
            managed_user_ids: Vec::new(),
            bundle: PrebidBundleBuildConfig::default(),
        }
    }
}

impl IntegrationConfig for PrebidIntegrationConfig {}

#[cfg(test)]
impl From<&LegacyPrebidConfig> for PrebidIntegrationConfig {
    fn from(config: &LegacyPrebidConfig) -> Self {
        Self {
            account_id: config.account_id.clone(),
            timeout_ms: config.timeout_ms,
            debug: config.debug,
            script_patterns: config.script_patterns.clone(),
            external_bundle_url: config.external_bundle_url.clone(),
            external_bundle_sha256: config.external_bundle_sha256.clone(),
            external_bundle_sri: config.external_bundle_sri.clone(),
            client_side_bidders: config.client_side_bidders.clone(),
            excluded_gam_ad_unit_path_suffixes: config.excluded_gam_ad_unit_path_suffixes.clone(),
            managed_user_ids: config.managed_user_ids.clone(),
            bundle: PrebidBundleBuildConfig::default(),
        }
    }
}

#[cfg(test)]
fn remove_aps_bidders(config: &mut LegacyPrebidConfig) {
    for (field, bidders) in [
        ("bidders", &mut config.bidders),
        ("client_side_bidders", &mut config.client_side_bidders),
    ] {
        let original_len = bidders.len();
        bidders.retain(|bidder| !bidder.eq_ignore_ascii_case("aps"));
        if bidders.len() != original_len {
            log::warn!(
                "prebid: ignoring APS in auction.prebid.{field}; configure APS as a [demand] source with implementation = \"auction.aps\""
            );
        }
    }
}

fn excluded_gam_ad_unit_path_suffix_validation_error(message: &'static str) -> ValidationError {
    let mut error = ValidationError::new("invalid_gam_ad_unit_path_suffix");
    error.message = Some(message.into());
    error
}

fn validate_excluded_gam_ad_unit_path_suffix(value: &str) -> Result<(), ValidationError> {
    if value.trim() != value {
        return Err(excluded_gam_ad_unit_path_suffix_validation_error(
            "excluded_gam_ad_unit_path_suffixes entries must not have surrounding whitespace",
        ));
    }

    if value.is_empty() {
        return Err(excluded_gam_ad_unit_path_suffix_validation_error(
            "excluded_gam_ad_unit_path_suffixes entries must not be empty",
        ));
    }

    if !value.starts_with('/') {
        return Err(excluded_gam_ad_unit_path_suffix_validation_error(
            "excluded_gam_ad_unit_path_suffixes entries must start with '/'",
        ));
    }

    if value == "/" {
        return Err(excluded_gam_ad_unit_path_suffix_validation_error(
            "excluded_gam_ad_unit_path_suffixes entries must identify a non-root path suffix",
        ));
    }

    Ok(())
}

fn validate_excluded_gam_ad_unit_path_suffixes(values: &[String]) -> Result<(), ValidationError> {
    for value in values {
        validate_excluded_gam_ad_unit_path_suffix(value)?;
    }

    Ok(())
}

#[cfg(test)]
fn canonicalize_excluded_gam_ad_unit_path_suffixes(config: &mut LegacyPrebidConfig) {
    let mut canonical = Vec::with_capacity(config.excluded_gam_ad_unit_path_suffixes.len());
    for suffix in std::mem::take(&mut config.excluded_gam_ad_unit_path_suffixes) {
        if !canonical.contains(&suffix) {
            canonical.push(suffix);
        }
    }
    config.excluded_gam_ad_unit_path_suffixes = canonical;
}

#[cfg(test)]
fn load_config(
    settings: &Settings,
) -> Result<Option<LegacyPrebidConfig>, Report<TrustedServerError>> {
    let Some(mut config) = settings.module_config::<LegacyPrebidConfig>(MODULE)? else {
        return Ok(None);
    };
    canonicalize_excluded_gam_ad_unit_path_suffixes(&mut config);
    remove_aps_bidders(&mut config);
    Ok(Some(config))
}

fn default_timeout_ms() -> u32 {
    1000
}

#[cfg(test)]
fn default_bidders() -> Vec<String> {
    vec!["mocktioneer".to_string()]
}

/// Default suffixes that identify Prebid scripts
const PREBID_SCRIPT_SUFFIXES: &[&str] = &[
    "/prebid.js",
    "/prebid.min.js",
    "/prebidjs.js",
    "/prebidjs.min.js",
];

fn default_script_patterns() -> Vec<String> {
    PREBID_SCRIPT_SUFFIXES
        .iter()
        .map(|&s| s.to_owned())
        .collect()
}

fn validate_external_bundle_url(value: &str) -> Result<(), ValidationError> {
    let url = Url::parse(value).map_err(|_| {
        let mut err = ValidationError::new("invalid_external_bundle_url");
        err.message = Some("external_bundle_url must be a valid absolute URL".into());
        err
    })?;

    if url.scheme() != "https" {
        let mut err = ValidationError::new("invalid_external_bundle_scheme");
        err.message = Some("external_bundle_url must use https".into());
        return Err(err);
    }

    if url.host_str().is_none() {
        let mut err = ValidationError::new("missing_external_bundle_host");
        err.message = Some("external_bundle_url must include a host".into());
        return Err(err);
    }

    Ok(())
}

/// Exact hex SHA-256: 64 hex digits. Used by the built-in `regex` validator on
/// [`PrebidIntegrationConfig::external_bundle_sha256`].
static EXTERNAL_BUNDLE_SHA256_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9a-fA-F]{64}$").expect("SHA-256 hex regex should compile"));

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum ExternalBundleSriAlgorithm {
    Sha256,
    Sha384,
    Sha512,
}

impl ExternalBundleSriAlgorithm {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "sha256" => Some(Self::Sha256),
            "sha384" => Some(Self::Sha384),
            "sha512" => Some(Self::Sha512),
            _ => None,
        }
    }

    fn expected_digest_len(self) -> usize {
        match self {
            Self::Sha256 => 32,
            Self::Sha384 => 48,
            Self::Sha512 => 64,
        }
    }
}

fn external_bundle_sri_validation_error(message: &'static str) -> ValidationError {
    let mut err = ValidationError::new("invalid_external_bundle_sri");
    err.message = Some(message.into());
    err
}

fn parse_external_bundle_sri(value: &str) -> Result<(), ValidationError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed != value {
        return Err(external_bundle_sri_validation_error(
            "external_bundle_sri must be non-empty with no surrounding whitespace",
        ));
    }

    for token in trimmed.split_ascii_whitespace() {
        let Some((algorithm_raw, digest_raw)) = token.split_once('-') else {
            return Err(external_bundle_sri_validation_error(
                "external_bundle_sri entries must use algorithm-digest format",
            ));
        };

        let Some(algorithm) = ExternalBundleSriAlgorithm::parse(algorithm_raw) else {
            return Err(external_bundle_sri_validation_error(
                "external_bundle_sri must use sha256, sha384, or sha512",
            ));
        };

        if digest_raw.is_empty() {
            return Err(external_bundle_sri_validation_error(
                "external_bundle_sri digest must be non-empty",
            ));
        }

        let digest = BASE64_STANDARD
            .decode(digest_raw)
            .or_else(|_| BASE64_STANDARD_NO_PAD.decode(digest_raw))
            .map_err(|_| {
                external_bundle_sri_validation_error("external_bundle_sri digest must be base64")
            })?;

        if digest.len() != algorithm.expected_digest_len() {
            return Err(external_bundle_sri_validation_error(
                "external_bundle_sri digest length does not match its algorithm",
            ));
        }
    }

    Ok(())
}

fn validate_external_bundle_sri(value: &str) -> Result<(), ValidationError> {
    parse_external_bundle_sri(value)
}

fn validate_external_bundle_url_allowed(
    external_bundle_url: Option<&str>,
    allowed_domains: &[String],
) -> Result<(), Report<TrustedServerError>> {
    let url = external_bundle_url.ok_or_else(|| {
        Report::new(TrustedServerError::Configuration {
            message: "auction.prebid.external_bundle_url is required when prebid runs".to_string(),
        })
    })?;

    let parsed = Url::parse(url).map_err(|_| {
        Report::new(TrustedServerError::Configuration {
            message: "auction.prebid.external_bundle_url must be a valid absolute URL".to_string(),
        })
    })?;

    if parsed.scheme() != "https" {
        return Err(Report::new(TrustedServerError::Configuration {
            message: "auction.prebid.external_bundle_url must use https".to_string(),
        }));
    }

    let host = parsed.host_str().ok_or_else(|| {
        Report::new(TrustedServerError::Configuration {
            message: "auction.prebid.external_bundle_url must include a host".to_string(),
        })
    })?;

    if allowed_domains.is_empty() {
        return Err(Report::new(TrustedServerError::Configuration {
            message:
                "proxy.allowed_domains must include the external Prebid bundle host when auction.prebid.external_bundle_url is configured"
                    .to_string(),
        }));
    }

    if !allowed_domains
        .iter()
        .any(|pattern| is_host_allowed(host, pattern))
    {
        return Err(Report::new(TrustedServerError::Configuration {
            message: format!(
                "auction.prebid.external_bundle_url host `{host}` is not permitted by proxy.allowed_domains"
            ),
        }));
    }

    Ok(())
}

pub(crate) fn validate_browser_config_for_startup(
    config: &PrebidIntegrationConfig,
    allowed_domains: &[String],
) -> Result<(), Report<TrustedServerError>> {
    validate_external_bundle_url_allowed(config.external_bundle_url.as_deref(), allowed_domains)
}

pub(crate) fn validate_browser_bidder_ownership(
    config: &PrebidIntegrationConfig,
    plan: &AuctionPlan,
) -> Result<(), Report<TrustedServerError>> {
    if !plan.enabled() {
        return Ok(());
    }

    let server_side = plan
        .browser_bidder_codes()
        .collect::<std::collections::BTreeSet<_>>();
    let conflicts = config
        .client_side_bidders
        .iter()
        .filter(|bidder| server_side.contains(bidder.as_str()))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if conflicts.is_empty() {
        return Ok(());
    }

    Err(Report::new(TrustedServerError::Configuration {
        message: format!(
            "Prebid bidders must have exactly one browser owner; configured as both client-side and server-side: {}",
            conflicts.into_iter().collect::<Vec<_>>().join(", ")
        ),
    }))
}

#[cfg(test)]
fn validate_external_bundle_config(
    config: &LegacyPrebidConfig,
    allowed_domains: &[String],
) -> Result<(), Report<TrustedServerError>> {
    validate_external_bundle_url_allowed(config.external_bundle_url.as_deref(), allowed_domains)
}

pub struct PrebidIntegration {
    config: PrebidIntegrationConfig,
    planned_head_inserts: Option<Vec<String>>,
    #[cfg(test)]
    legacy_config: Option<LegacyPrebidConfig>,
}

impl PrebidIntegration {
    #[cfg(test)]
    fn new(config: LegacyPrebidConfig) -> Arc<Self> {
        Arc::new(Self {
            config: PrebidIntegrationConfig::from(&config),
            planned_head_inserts: None,
            legacy_config: Some(config),
        })
    }

    fn for_browser_plan(config: &PrebidIntegrationConfig, plan: &AuctionPlan) -> Arc<Self> {
        let mut integration = Self {
            config: config.clone(),
            planned_head_inserts: None,
            #[cfg(test)]
            legacy_config: None,
        };
        integration.planned_head_inserts = Some(integration.head_inserts_for_plan(config, plan));
        Arc::new(integration)
    }

    fn matches_script_url(&self, attr_value: &str) -> bool {
        let trimmed = attr_value.trim();
        let without_query = trimmed.split(['?', '#']).next().unwrap_or(trimmed);

        if self.matches_script_pattern(without_query) {
            return true;
        }

        if !without_query.starts_with('/')
            && !without_query.starts_with("//")
            && !without_query.contains("://")
        {
            let with_slash = format!("/{without_query}");
            if self.matches_script_pattern(&with_slash) {
                return true;
            }
        }

        let parsed = if without_query.starts_with("//") {
            ParsedUrl::parse(&format!("https:{without_query}"))
        } else {
            ParsedUrl::parse(without_query)
        };

        parsed
            .ok()
            .is_some_and(|url| self.matches_script_pattern(url.path()))
    }

    fn matches_script_pattern(&self, path: &str) -> bool {
        // Normalize path to lowercase for case-insensitive matching
        let path_lower = path.to_ascii_lowercase();

        // Check if path matches any configured pattern
        for pattern in &self.config.script_patterns {
            let pattern_lower = pattern.to_ascii_lowercase();

            // Check for wildcard patterns: /* or {*name}
            if pattern_lower.ends_with("/*") || pattern_lower.contains("{*") {
                // Extract prefix before the wildcard
                let prefix = if pattern_lower.ends_with("/*") {
                    &pattern_lower[..pattern_lower.len() - 1] // Remove trailing *
                } else {
                    // Find {* and extract prefix before it
                    pattern_lower.split("{*").next().unwrap_or("")
                };

                if path_lower.starts_with(prefix) {
                    // Check if it ends with a known Prebid script suffix
                    if PREBID_SCRIPT_SUFFIXES
                        .iter()
                        .any(|suffix| path_lower.ends_with(suffix))
                    {
                        return true;
                    }
                }
            } else {
                // Exact match or suffix match
                if path_lower.ends_with(&pattern_lower) {
                    return true;
                }
            }
        }
        false
    }

    fn handle_script_handler(
        &self,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let body = "// Script overridden by Trusted Server\n";

        let mut response = http::Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, PREBID_BUNDLE_CONTENT_TYPE)
            .body(EdgeBody::from(body))
            .change_context(TrustedServerError::Prebid {
                message: "Failed to build Prebid script handler response".to_string(),
            })?;
        CacheControlPolicy::NoStorePrivate
            .apply_to_headers(response.headers_mut(), EdgeCacheHeader::None);
        Ok(response)
    }

    fn external_bundle_script_tag(&self) -> String {
        external_bundle_script_tag(
            self.config.external_bundle_sha256.as_deref(),
            self.config.external_bundle_sri.as_deref(),
        )
    }

    /// Build the prepared browser injection from browser settings and validated routes.
    pub(crate) fn head_inserts_for_plan(
        &self,
        browser_config: &PrebidIntegrationConfig,
        plan: &AuctionPlan,
    ) -> Vec<String> {
        let payload = InjectedPrebidClientConfig {
            server_side_bidders: Some(if plan.enabled() {
                plan.browser_bidder_codes().collect()
            } else {
                Vec::new()
            }),
            ..InjectedPrebidClientConfig::from(browser_config)
        };
        let config_json = serialize_injected_prebid_config(&payload);

        vec![
            injected_prebid_config_script(&config_json),
            external_bundle_script_tag(
                browser_config.external_bundle_sha256.as_deref(),
                browser_config.external_bundle_sri.as_deref(),
            ),
        ]
    }

    fn is_managed_external(&self) -> bool {
        self.config.external_bundle_url.is_some()
    }

    fn external_bundle_request_cache_mode(
        &self,
        req: &http::Request<EdgeBody>,
    ) -> Result<Option<ExternalBundleCacheMode>, Report<TrustedServerError>> {
        let versions = req
            .uri()
            .query()
            .map(|query| {
                url::form_urlencoded::parse(query.as_bytes())
                    .filter(|(key, _)| key == "v")
                    .map(|(_, value)| value.into_owned())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if versions.len() > 1 {
            return Ok(None);
        }

        let requested_version = versions.first().map(String::as_str);
        match (
            self.config.external_bundle_sha256.as_deref(),
            requested_version,
        ) {
            (None, Some(_)) => Ok(None),
            (Some(expected), Some(actual)) if expected != actual => Ok(None),
            (Some(_), Some(_)) => Ok(Some(ExternalBundleCacheMode::Immutable)),
            _ => Ok(Some(ExternalBundleCacheMode::Revalidate)),
        }
    }

    fn apply_external_bundle_headers(
        &self,
        response: &mut http::Response<EdgeBody>,
        mode: ExternalBundleCacheMode,
    ) {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static(PREBID_BUNDLE_CONTENT_TYPE),
        );
        response.headers_mut().insert(
            header::HeaderName::from_static(PREBID_BUNDLE_NOSNIFF_HEADER),
            HeaderValue::from_static(PREBID_BUNDLE_NOSNIFF_VALUE),
        );

        match mode {
            ExternalBundleCacheMode::Immutable => {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static(PREBID_BUNDLE_IMMUTABLE_CACHE_CONTROL),
                );
                if let Some(sha256) = self.config.external_bundle_sha256.as_deref() {
                    response.headers_mut().insert(
                        header::ETAG,
                        HeaderValue::from_str(&format!("\"sha256:{sha256}\""))
                            .expect("should build etag header"),
                    );
                }
            }
            ExternalBundleCacheMode::Revalidate => {
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static(PREBID_BUNDLE_REVALIDATION_CACHE_CONTROL),
                );
                if let Some(sha256) = self.config.external_bundle_sha256.as_deref() {
                    response.headers_mut().insert(
                        header::ETAG,
                        HeaderValue::from_str(&format!("\"sha256:{sha256}\""))
                            .expect("should build etag header"),
                    );
                }
            }
        }
    }

    fn sanitize_external_bundle_response(
        &self,
        response: http::Response<EdgeBody>,
        mode: ExternalBundleCacheMode,
    ) -> http::Response<EdgeBody> {
        let status = response.status();
        let content_encoding = response.headers().get(header::CONTENT_ENCODING).cloned();
        let body = response.into_body();

        let mut sanitized = http::Response::builder()
            .status(status)
            .body(body)
            .expect("should build sanitized response");

        if let Some(content_encoding) = content_encoding {
            sanitized
                .headers_mut()
                .insert(header::CONTENT_ENCODING, content_encoding);
        }

        if status == StatusCode::OK {
            self.apply_external_bundle_headers(&mut sanitized, mode);
        } else {
            sanitized.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static(PREBID_BUNDLE_ERROR_CONTENT_TYPE),
            );
            sanitized.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static(PREBID_BUNDLE_ERROR_CACHE_CONTROL),
            );
            sanitized.headers_mut().insert(
                header::HeaderName::from_static(PREBID_BUNDLE_NOSNIFF_HEADER),
                HeaderValue::from_static(PREBID_BUNDLE_NOSNIFF_VALUE),
            );
        }

        sanitized
    }

    async fn handle_external_bundle(
        &self,
        settings: &Settings,
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let Some(cache_mode) = self.external_bundle_request_cache_mode(&req)? else {
            return Ok(http::Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(EdgeBody::from("Not Found"))
                .expect("should build not found response"));
        };

        let target_url = self.config.external_bundle_url.as_deref().ok_or_else(|| {
            Report::new(TrustedServerError::Configuration {
                message: "auction.prebid.external_bundle_url is required when prebid runs"
                    .to_string(),
            })
        })?;

        let proxy_config = ProxyRequestConfig::new(target_url)
            .without_ec_id()
            .without_forward_headers()
            .with_streaming()
            .with_allowed_domains(&settings.proxy.allowed_domains)
            .with_https_only();

        let response = proxy_request(settings, req, proxy_config, services).await?;
        Ok(self.sanitize_external_bundle_response(response, cache_mode))
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum ExternalBundleCacheMode {
    Immutable,
    Revalidate,
}

fn escape_html_attr(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn external_bundle_script_src(sha256: Option<&str>) -> String {
    match sha256 {
        Some(sha256) => format!("{PREBID_BUNDLE_ROUTE}?v={sha256}"),
        None => PREBID_BUNDLE_ROUTE.to_string(),
    }
}

fn external_bundle_script_tag(sha256: Option<&str>, sri: Option<&str>) -> String {
    let src = external_bundle_script_src(sha256);
    let integrity = sri
        .map(|value| format!(" integrity=\"{}\"", escape_html_attr(value)))
        .unwrap_or_default();

    format!("<script src=\"{src}\"{integrity} defer></script>")
}

#[cfg(test)]
fn build(
    settings: &Settings,
) -> Result<Option<Arc<PrebidIntegration>>, Report<TrustedServerError>> {
    let Some(config) = load_config(settings)? else {
        return Ok(None);
    };

    validate_external_bundle_config(&config, &settings.proxy.allowed_domains)?;

    // Warn about bidders that appear in both lists — this is likely a config
    // mistake. A bidder should be in either `bidders` (server-side) or
    // `client_side_bidders` (browser-side), not both.
    for bidder in &config.client_side_bidders {
        if config.bidders.iter().any(|b| b == bidder) {
            log::warn!(
                "prebid: bidder \"{}\" is in both bidders and client_side_bidders — \
                 it will run server-side AND be left for client-side, which is likely unintended",
                bidder
            );
        }
    }

    Ok(Some(PrebidIntegration::new(config)))
}

/// Register the Prebid integration when a section selects it.
///
/// # Errors
///
/// Returns an error when the Prebid integration runs with invalid
/// configuration.
pub fn register_for_plan(
    settings: &Settings,
    plan: &AuctionPlan,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(mut config) = settings.module_config::<PrebidIntegrationConfig>(MODULE)? else {
        return Ok(None);
    };
    let mut canonical = Vec::with_capacity(config.excluded_gam_ad_unit_path_suffixes.len());
    for suffix in std::mem::take(&mut config.excluded_gam_ad_unit_path_suffixes) {
        if !canonical.contains(&suffix) {
            canonical.push(suffix);
        }
    }
    config.excluded_gam_ad_unit_path_suffixes = canonical;
    validate_browser_config_for_startup(&config, &settings.proxy.allowed_domains)?;
    validate_browser_bidder_ownership(&config, plan)?;
    let integration = PrebidIntegration::for_browser_plan(&config, plan);
    Ok(Some(
        IntegrationRegistration::builder(PREBID_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration.clone())
            .with_head_injector(integration)
            .with_deferred_js()
            .build(),
    ))
}

#[cfg(test)]
#[allow(clippy::missing_errors_doc)]
pub fn register(
    settings: &Settings,
) -> Result<Option<IntegrationRegistration>, Report<TrustedServerError>> {
    let Some(integration) = build(settings)? else {
        return Ok(None);
    };

    Ok(Some(
        IntegrationRegistration::builder(PREBID_INTEGRATION_ID)
            .with_proxy(integration.clone())
            .with_attribute_rewriter(integration.clone())
            .with_head_injector(integration)
            .with_deferred_js()
            .build(),
    ))
}

#[async_trait(?Send)]
impl IntegrationProxy for PrebidIntegration {
    fn integration_name(&self) -> &'static str {
        PREBID_INTEGRATION_ID
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        let mut routes = vec![];

        routes.push(self.get("/bundle.js"));

        // Register routes for script removal patterns
        // Patterns can be exact paths (e.g., "/prebid.min.js") or use matchit wildcards
        // (e.g., "/static/prebid/{*rest}")
        for pattern in &self.config.script_patterns {
            // Intentional leak: runs once at startup and patterns are small.
            // `IntegrationEndpoint` requires `&'static str`.
            let static_path: &'static str = Box::leak(pattern.clone().into_boxed_str());
            routes.push(IntegrationEndpoint::get(static_path));
        }

        routes
    }

    async fn handle(
        &self,
        settings: &Settings,
        services: &RuntimeServices,
        req: http::Request<EdgeBody>,
    ) -> Result<http::Response<EdgeBody>, Report<TrustedServerError>> {
        let path = req.uri().path().to_string();
        let method = req.method().clone();

        match method {
            Method::GET if self.is_managed_external() && path == PREBID_BUNDLE_ROUTE => {
                self.handle_external_bundle(settings, services, req).await
            }
            // Serve empty JS for matching script patterns
            Method::GET if self.matches_script_pattern(&path) => self.handle_script_handler(),
            _ => http::Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(EdgeBody::from("Not Found"))
                .change_context(TrustedServerError::Prebid {
                    message: "Failed to build Prebid not found response".to_string(),
                }),
        }
    }
}

impl IntegrationAttributeRewriter for PrebidIntegration {
    fn integration_id(&self) -> &'static str {
        PREBID_INTEGRATION_ID
    }

    fn handles_attribute(&self, attribute: &str) -> bool {
        matches!(attribute, "src" | "href")
    }

    fn rewrite(
        &self,
        _attr_name: &str,
        attr_value: &str,
        _ctx: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteAction {
        if self.matches_script_url(attr_value) {
            AttributeRewriteAction::remove_element()
        } else {
            AttributeRewriteAction::keep()
        }
    }
}

fn serialize_injected_prebid_config(payload: &impl Serialize) -> String {
    // JSON appears in script raw-text, where every less-than sign must be escaped.
    serde_json::to_string(payload)
        .unwrap_or_else(|error| {
            log::warn!("Prebid: failed to serialize client config: {error}");
            "{}".to_string()
        })
        .replace('<', "\\u003c")
}

fn injected_prebid_config_script(config_json: &str) -> String {
    format!(
        r#"<script>window.pbjs=window.pbjs||{{}};window.pbjs.que=window.pbjs.que||[];window.pbjs.cmd=window.pbjs.cmd||[];window.__tsjs_prebid={config_json};</script>"#
    )
}

impl IntegrationHeadInjector for PrebidIntegration {
    fn integration_id(&self) -> &'static str {
        PREBID_INTEGRATION_ID
    }

    fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
        if let Some(inserts) = &self.planned_head_inserts {
            return inserts.clone();
        }
        let payload = InjectedPrebidClientConfig {
            bidders: Some({
                #[cfg(test)]
                {
                    self.legacy_config
                        .as_ref()
                        .map_or(&[][..], |config| config.bidders.as_slice())
                }
                #[cfg(not(test))]
                {
                    &[]
                }
            }),
            ..InjectedPrebidClientConfig::from(&self.config)
        };

        let config_json = serialize_injected_prebid_config(&payload);
        let mut inserts = vec![injected_prebid_config_script(&config_json)];

        inserts.push(self.external_bundle_script_tag());

        inserts
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InjectedManagedUserIdStorage<'a> {
    #[serde(rename = "type")]
    storage_type: PrebidUserIdStorageType,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_in_seconds: Option<u32>,
}

impl<'a> From<&'a PrebidManagedUserIdStorage> for InjectedManagedUserIdStorage<'a> {
    fn from(storage: &'a PrebidManagedUserIdStorage) -> Self {
        Self {
            storage_type: storage.storage_type,
            name: &storage.name,
            expires: storage.expires,
            refresh_in_seconds: storage.refresh_in_seconds,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InjectedManagedUserId<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    params: &'a serde_json::Map<String, Json>,
    #[serde(skip_serializing_if = "Option::is_none")]
    storage: Option<InjectedManagedUserIdStorage<'a>>,
}

impl<'a> From<&'a PrebidManagedUserIdConfig> for InjectedManagedUserId<'a> {
    fn from(config: &'a PrebidManagedUserIdConfig) -> Self {
        Self {
            name: &config.name,
            params: &config.params,
            storage: config
                .storage
                .as_ref()
                .map(InjectedManagedUserIdStorage::from),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InjectedPrebidClientConfig<'a> {
    account_id: &'a str,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    managed_user_ids: Vec<InjectedManagedUserId<'a>>,
    timeout: u32,
    debug: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    bidders: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    server_side_bidders: Option<Vec<&'a str>>,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    client_side_bidders: &'a [String],
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    excluded_gam_ad_unit_path_suffixes: &'a [String],
}

impl<'a> From<&'a PrebidIntegrationConfig> for InjectedPrebidClientConfig<'a> {
    fn from(config: &'a PrebidIntegrationConfig) -> Self {
        Self {
            account_id: config.account_id.as_deref().unwrap_or_default(),
            managed_user_ids: config
                .managed_user_ids
                .iter()
                .map(InjectedManagedUserId::from)
                .collect(),
            timeout: config.timeout_ms,
            debug: config.debug,
            bidders: None,
            server_side_bidders: None,
            client_side_bidders: &config.client_side_bidders,
            excluded_gam_ad_unit_path_suffixes: &config.excluded_gam_ad_unit_path_suffixes,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    use trusted_server_core::auction::plan::{BidderId, BidderRouteConfig, ProviderId};

    use trusted_server_core::html_processor::{HtmlProcessorConfig, create_html_processor};
    use trusted_server_core::integrations::{
        AttributeRewriteAction, IntegrationDocumentState, IntegrationRegistry,
    };
    use trusted_server_core::platform::test_support::{
        StubHttpClient, build_services_with_http_client,
    };

    use base64::engine::general_purpose::STANDARD as TEST_BASE64_STANDARD;
    use trusted_server_core::settings::Settings;
    use trusted_server_core::streaming_processor::{
        Compression, PipelineConfig, StreamingPipeline,
    };
    use trusted_server_core::test_support::tests::create_test_settings;

    use http::Method;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::io::Cursor;
    use std::str::FromStr as _;

    #[test]
    fn external_bundle_sha256_validation_matches_hex_pattern() {
        use validator::Validate as _;

        let config = |sha: &str| -> PrebidIntegrationConfig {
            serde_json::from_value(serde_json::json!({
                "external_bundle_sha256": sha,
            }))
            .expect("should deserialize prebid config")
        };

        // Exactly 64 hex digits (either case) passes.
        config(&"a".repeat(64))
            .validate()
            .expect("64-char lowercase hex sha256 should pass");
        config("ABCDEF0123456789abcdef0123456789ABCDEF0123456789abcdef0123456789")
            .validate()
            .expect("mixed-case 64-char hex sha256 should pass");

        // Wrong length or non-hex characters are rejected.
        for bad in [
            "a".repeat(63),
            "a".repeat(65),
            "g".repeat(64),
            String::new(),
        ] {
            config(&bad)
                .validate()
                .expect_err(&format!("invalid sha256 {bad:?} should be rejected"));
        }
    }

    /// The shared test settings with Prebid selected and its bundle served
    /// from an external URL.
    fn make_settings() -> Settings {
        let mut settings = create_test_settings();
        settings
            .insert_module_config(
                "auction",
                MODULE,
                &json!({
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "bundle": { "modules": { "bidder": ["exampleBidderBidAdapter"] } }
                }),
            )
            .expect("should select Prebid");
        settings
    }

    fn validate_for_deploy(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
        trusted_server_core::config::validate_settings_for_deploy_with(settings, &[builder()])
    }

    fn validate_for_runtime(settings: &Settings) -> Result<(), Report<TrustedServerError>> {
        trusted_server_core::config::validate_settings_for_runtime_with(settings, &[builder()])
    }

    /// The setting that named the Prebid Server endpoint belongs to a
    /// `[demand]` source, so Prebid's table refuses it.
    #[test]
    fn removed_integration_fields_are_rejected() {
        let mut settings = create_test_settings();
        settings
            .insert_module_config("auction", MODULE, &json!({ "server_url": "removed-value" }))
            .expect("should insert the removed Prebid field");
        let error = settings
            .module_config::<PrebidIntegrationConfig>(MODULE)
            .expect_err("should reject the removed Prebid field");
        assert!(
            format!("{error:?}").contains("server_url"),
            "should identify the removed field: {error:?}"
        );
    }

    #[test]
    fn deploy_validation_rejects_external_prebid_bundle_without_proxy_allowed_domains() {
        let mut settings = make_settings();
        settings.proxy.allowed_domains.clear();

        let err = validate_for_deploy(&settings)
            .expect_err("should reject external Prebid bundle without proxy allowlist");

        assert!(
            err.to_string().contains("proxy.allowed_domains"),
            "error should mention proxy.allowed_domains: {err:?}"
        );
    }

    /// A selected Prebid table that names bundle modules has to name where the
    /// bundle is served from as well, and deploy validation says so. Selection
    /// is what makes Prebid run, so the check applies to a selected table.
    #[test]
    fn deploy_validation_requires_external_bundle_url_for_selected_prebid() {
        let mut settings = make_settings();
        settings
            .insert_module_config(
                "auction",
                MODULE,
                &json!({
                    "bundle": {
                        "modules": { "bidder": ["exampleBidderBidAdapter"] }
                    }
                }),
            )
            .expect("should insert the Prebid config");

        let error = validate_for_deploy(&settings)
            .expect_err("should require enabled Prebid external bundle URL");
        assert!(error.to_string().contains("external_bundle_url"));
    }

    /// Deploy and runtime validation both reach Prebid's table. It is planted
    /// with a setting its config type does not have, and the rejection must
    /// name the table, so a failure elsewhere in validation cannot pass for
    /// it.
    #[test]
    fn validation_reaches_the_prebid_table() {
        let mut settings = make_settings();
        settings
            .insert_module_config("auction", MODULE, &json!({ "no_such_setting": true }))
            .expect("should insert the planted table");
        let expected = "[auction.prebid]";

        let Err(deploy_error) = validate_for_deploy(&settings) else {
            panic!("deploy validation should reject the planted table");
        };
        assert!(
            format!("{deploy_error:?}").contains(expected),
            "deploy validation should reject the table by name: {deploy_error:?}"
        );

        let Err(runtime_error) = validate_for_runtime(&settings) else {
            panic!("runtime validation should reject the planted table");
        };
        assert!(
            format!("{runtime_error:?}").contains(expected),
            "runtime validation should reject the table by name: {runtime_error:?}"
        );
    }

    /// Settings in which `exampleBidder` runs server-side through a demand
    /// source and is also listed as a client-side bidder.
    fn settings_with_browser_bidder_overlap(auction_enabled: bool) -> Settings {
        let mut settings = make_settings();
        settings.proxy.allowed_domains = vec!["*.example".to_string()];
        settings.auction.enabled = auction_enabled;
        settings.demand = trusted_server_core::auction::test_support::demand_named(&["pbs"]);
        settings.auction.bidders.insert(
            "exampleBidder"
                .parse()
                .expect("should parse server-side bidder"),
            BidderRouteConfig {
                module: "pbs".parse().expect("should parse provider"),
            },
        );
        let mut prebid = settings
            .module_config::<PrebidIntegrationConfig>(MODULE)
            .expect("should parse Prebid config")
            .expect("should have enabled Prebid config");
        prebid.client_side_bidders = vec!["exampleBidder".to_string()];
        settings
            .insert_module_config("auction", MODULE, &prebid)
            .expect("should replace Prebid config");
        settings
    }

    #[test]
    fn runtime_validation_rejects_enabled_browser_bidder_ownership_conflict() {
        let settings = settings_with_browser_bidder_overlap(true);

        let error = validate_for_runtime(&settings)
            .expect_err("should reject enabled browser bidder ownership conflict");

        assert!(error.to_string().contains("exampleBidder"));
        assert!(
            error
                .to_string()
                .contains("both client-side and server-side")
        );
    }

    #[test]
    fn runtime_validation_accepts_disabled_browser_bidder_ownership_overlap() {
        let settings = settings_with_browser_bidder_overlap(false);

        validate_for_runtime(&settings)
            .expect("runtime should accept disabled browser bidder ownership overlap");
    }

    #[test]
    fn js_module_ids_defer_prebid_shim_when_external_bundle_is_configured() {
        let settings = make_settings();
        let plan = Arc::new(
            trusted_server_core::auction::compile_auction_plan(&settings)
                .expect("should compile auction plan"),
        );

        let registry =
            IntegrationRegistry::with_plan_and_registrations(&settings, plan, &[builder()])
                .expect("should create registry");

        assert!(
            registry.js_module_ids().contains(&"prebid"),
            "external bundle mode should include the prebid shim in embedded TSJS modules"
        );
        assert!(
            !registry.js_module_ids_immediate().contains(&"prebid"),
            "the prebid shim should not load in the immediate TSJS bundle"
        );
        assert!(
            registry.js_module_ids_deferred().contains(&"prebid"),
            "the prebid shim should load as a deferred TSJS module"
        );
        assert!(
            registry.has_route(&Method::GET, "/integrations/prebid/bundle.js"),
            "external bundle mode should register the first-party bundle route"
        );
    }

    #[test]
    fn injected_prebid_config_escapes_every_less_than_sign() {
        let config_json = serialize_injected_prebid_config(&json!({
            "accountId": "x<!--<script",
        }));
        let script = injected_prebid_config_script(&config_json);

        assert!(
            config_json.contains(r#"x\u003c!--\u003cscript"#),
            "should escape every less-than sign in JSON script data: {config_json}"
        );
        assert_eq!(
            script.matches('<').count(),
            2,
            "should leave less-than signs only in the outer script element: {script}"
        );
    }

    fn base_config() -> LegacyPrebidConfig {
        LegacyPrebidConfig {
            account_id: Some("test-account".to_string()),
            managed_user_ids: Vec::new(),
            timeout_ms: 1000,
            bidders: vec!["exampleBidder".to_string()],
            debug: false,
            script_patterns: default_script_patterns(),
            external_bundle_url: Some(
                "https://assets.example/prebid/trusted-prebid.js".to_string(),
            ),
            external_bundle_sha256: None,
            external_bundle_sri: None,
            client_side_bidders: Vec::new(),
            excluded_gam_ad_unit_path_suffixes: Vec::new(),
        }
    }

    fn valid_managed_user_id() -> PrebidManagedUserIdConfig {
        PrebidManagedUserIdConfig {
            name: "exampleId".to_string(),
            params: serde_json::Map::from_iter([("pid".to_string(), json!("999"))]),
            storage: Some(PrebidManagedUserIdStorage {
                storage_type: PrebidUserIdStorageType::Cookie,
                name: "example_env".to_string(),
                expires: Some(15),
                refresh_in_seconds: Some(1800),
            }),
        }
    }

    fn test_sri(algorithm: &str, digest: &[u8]) -> String {
        format!("{algorithm}-{}", TEST_BASE64_STANDARD.encode(digest))
    }

    fn test_request(url: impl AsRef<str>) -> http::Request<EdgeBody> {
        http::Request::builder()
            .method(http::Method::GET)
            .uri(url.as_ref())
            .body(EdgeBody::empty())
            .expect("should build request")
    }

    fn header_value_str(response: &http::Response<EdgeBody>, name: &str) -> Option<String> {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok().map(std::string::ToString::to_string))
    }

    fn response_header_is_present(response: &http::Response<EdgeBody>, name: &str) -> bool {
        response.headers().contains_key(name)
    }

    fn response_body_string(response: http::Response<EdgeBody>) -> String {
        String::from_utf8(
            response
                .into_body()
                .into_bytes()
                .unwrap_or_default()
                .to_vec(),
        )
        .expect("should parse response body as utf-8")
    }

    fn config_from_settings(
        settings: &Settings,
        registry: &IntegrationRegistry,
    ) -> HtmlProcessorConfig {
        HtmlProcessorConfig::from_settings(
            settings,
            registry,
            "origin.example.com",
            "test.example.com",
            "https",
        )
    }

    /// Shared TOML prefix for config-parsing tests (publisher + ec sections),
    /// naming the integration whose block each fixture appends.
    const TOML_BASE: &str = r#"
[auction]
modules = ["prebid"]

[[handlers]]
path = "^/_ts/admin"
username = "admin"
password = "admin-pass"

[publisher]
domain = "test-publisher.com"
cookie_domain = ".test-publisher.com"
origin_url = "https://origin.test-publisher.com"
proxy_secret = "test-secret"

[ec]
module = "hmac"

[ec.hmac]
passphrase = "test-secret-key-32-bytes-minimum"

[geo]
assume_single_jurisdiction = true
"#;

    fn parse_browser_prebid_toml_result(
        prebid_section: &str,
    ) -> Result<Option<PrebidIntegrationConfig>, Report<TrustedServerError>> {
        let toml_str = format!("{}{}", TOML_BASE, prebid_section);
        let settings = Settings::from_toml(&toml_str)?;
        settings.module_config::<PrebidIntegrationConfig>(MODULE)
    }

    #[test]
    fn browser_config_accepts_strict_nested_bundle_modules() {
        let config = parse_browser_prebid_toml_result(
            r#"
[auction.prebid]

[auction.prebid.bundle.modules]
bidder = ["rubiconBidAdapter"]
user_id = ["sharedIdSystem"]
analytics = ["atsAnalyticsAdapter"]
"#,
        )
        .expect("should parse nested Prebid bundle modules")
        .expect("should enable Prebid browser config");
        let bundle = serde_json::to_value(config.bundle).expect("should serialize bundle config");

        assert_eq!(
            bundle,
            json!({
                "modules": {
                    "bidder": ["rubiconBidAdapter"],
                    "user_id": ["sharedIdSystem"],
                    "analytics": ["atsAnalyticsAdapter"]
                }
            }),
            "should retain nested Prebid bundle module stems"
        );
    }

    #[test]
    fn browser_config_rejects_removed_and_unknown_bundle_fields() {
        // There is no `enabled` flag to vary, because an integration runs when
        // a section selects it, so each field is checked once.
        for (section, field, value) in [
            ("bundle", "adapters", "[\"rubicon\"]"),
            ("bundle", "user_id_modules", "[\"sharedIdSystem\"]"),
            ("bundle", "analytics_adapters", "[\"atsAnalyticsAdapter\"]"),
            ("bundle.modules", "unsupported_kind", "[]"),
        ] {
            let error = parse_browser_prebid_toml_result(&format!(
                r#"
[auction.prebid]

[auction.prebid.{section}]
{field} = {value}
"#
            ))
            .expect_err("should reject a removed or unknown Prebid bundle field");

            let error = format!("{error:?}");
            assert!(
                error.contains(field),
                "should identify rejected field {field:?}: {error}"
            );
        }
    }

    /// Parse a TOML string containing only the `[auction.prebid]` section
    /// (plus any sub-tables) into a [`LegacyPrebidConfig`].
    fn parse_prebid_toml(prebid_section: &str) -> LegacyPrebidConfig {
        let toml_str = format!("{}{}", TOML_BASE, prebid_section);
        let settings = Settings::from_toml(&toml_str).expect("should parse TOML");
        settings
            .module_config::<LegacyPrebidConfig>(MODULE)
            .expect("should get config")
            .expect("should be enabled")
    }

    fn parse_prebid_toml_result(
        prebid_section: &str,
    ) -> Result<LegacyPrebidConfig, Report<TrustedServerError>> {
        let toml_str = format!("{}{}", TOML_BASE, prebid_section);
        let settings = Settings::from_toml(&toml_str)?;
        settings
            .module_config::<LegacyPrebidConfig>(MODULE)?
            .ok_or_else(|| {
                Report::new(TrustedServerError::Configuration {
                    message: "prebid integration config should be present".to_string(),
                })
            })
    }

    #[test]
    fn excluded_gam_ad_unit_path_suffixes_default_to_empty() {
        let config = parse_prebid_toml(
            r#"
[auction.prebid]
server_url = "https://prebid.example/openrtb2/auction"
"#,
        );

        assert!(
            config.excluded_gam_ad_unit_path_suffixes.is_empty(),
            "should default to no refresh-auction exclusions"
        );
    }

    #[test]
    fn planned_registration_canonicalizes_excluded_gam_ad_unit_path_suffixes() {
        let mut settings = make_settings();
        settings
            .insert_module_config(
                "auction",
                "auction.prebid",
                &json!({
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "excluded_gam_ad_unit_path_suffixes": [
                        "/trackingonly",
                        "/measurement-only",
                        "/trackingonly"
                    ]
                }),
            )
            .expect("should replace Prebid test configuration");
        let plan = Arc::new(
            trusted_server_core::auction::compile_auction_plan(&settings)
                .expect("should compile auction plan"),
        );
        let registry =
            IntegrationRegistry::with_plan_and_registrations(&settings, plan, &[builder()])
                .expect("should build integration registry");
        let document_state = IntegrationDocumentState::default();
        let context = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = registry.head_inserts(&context);
        let config_insert = inserts
            .iter()
            .find(|insert| insert.contains("window.__tsjs_prebid"))
            .expect("should inject planned Prebid config");

        assert!(
            config_insert.contains(
                r#""excludedGamAdUnitPathSuffixes":["/trackingonly","/measurement-only"]"#
            ),
            "should inject the canonical suffix list: {config_insert}"
        );
    }
    #[test]
    fn managed_user_ids_parse_with_opaque_params() {
        let config = parse_prebid_toml(
            r#"
[auction.prebid]
server_url = "https://prebid.example/openrtb2/auction"

[[auction.prebid.managed_user_ids]]
name = "exampleId"
params = { pid = "999", notUse3P = false, nested = { depth = 2 } }

[auction.prebid.managed_user_ids.storage]
type = "html5"
name = "example_env"
expires = 30
refresh_in_seconds = 3600
"#,
        );

        let [entry] = config.managed_user_ids.as_slice() else {
            panic!("should parse exactly one managed User ID entry");
        };
        assert_eq!(entry.name, "exampleId", "should preserve the module name");
        assert_eq!(
            Json::Object(entry.params.clone()),
            json!({"pid": "999", "notUse3P": false, "nested": {"depth": 2}}),
            "should carry module parameters through without inspecting them"
        );

        let storage = entry.storage.as_ref().expect("should parse storage");
        assert_eq!(
            storage.storage_type,
            PrebidUserIdStorageType::Html5,
            "should preserve the configured storage mechanism"
        );
        assert_eq!(storage.name, "example_env", "should preserve storage key");
        assert_eq!(storage.expires, Some(30), "should preserve expiry");
        assert_eq!(
            storage.refresh_in_seconds,
            Some(3600),
            "should preserve refresh interval"
        );
    }

    #[test]
    fn managed_user_ids_leave_prebid_defaults_in_place_when_unset() {
        let config = parse_prebid_toml(
            r#"
[auction.prebid]
server_url = "https://prebid.example/openrtb2/auction"

[[auction.prebid.managed_user_ids]]
name = "exampleId"

[auction.prebid.managed_user_ids.storage]
name = "example_env"
"#,
        );

        let [entry] = config.managed_user_ids.as_slice() else {
            panic!("should parse exactly one managed User ID entry");
        };
        assert!(
            entry.params.is_empty(),
            "should treat parameters as optional"
        );

        let storage = entry.storage.as_ref().expect("should parse storage");
        assert_eq!(
            storage.storage_type,
            PrebidUserIdStorageType::Cookie,
            "should default to cookie storage"
        );
        assert_eq!(
            storage.expires, None,
            "should leave Prebid's own expiry default in place"
        );
        assert_eq!(
            storage.refresh_in_seconds, None,
            "should leave Prebid's own refresh default in place"
        );
    }

    #[test]
    fn managed_user_ids_allow_an_entry_without_storage() {
        let config = parse_prebid_toml(
            r#"
[auction.prebid]
server_url = "https://prebid.example/openrtb2/auction"

[[auction.prebid.managed_user_ids]]
name = "exampleId"
"#,
        );

        let [entry] = config.managed_user_ids.as_slice() else {
            panic!("should parse exactly one managed User ID entry");
        };
        assert!(
            entry.storage.is_none(),
            "should treat storage as optional for modules that need none"
        );
    }

    #[test]
    fn managed_user_ids_reject_invalid_values() {
        for (name, entry_section) in [
            ("missing name", "params = { pid = \"999\" }"),
            ("empty name", "name = \"\""),
            ("padded name", "name = \" exampleId \""),
            ("name with a space", "name = \"example id\""),
            (
                "unknown entry field",
                "name = \"exampleId\"\nunsupported = true",
            ),
            (
                "empty storage name",
                "name = \"exampleId\"\n\n[auction.prebid.managed_user_ids.storage]\nname = \"\"",
            ),
            (
                "missing storage name",
                "name = \"exampleId\"\n\n[auction.prebid.managed_user_ids.storage]\ntype = \"cookie\"",
            ),
            (
                "zero expiry",
                "name = \"exampleId\"\n\n[auction.prebid.managed_user_ids.storage]\nname = \"example_env\"\nexpires = 0",
            ),
            (
                "zero refresh",
                "name = \"exampleId\"\n\n[auction.prebid.managed_user_ids.storage]\nname = \"example_env\"\nrefresh_in_seconds = 0",
            ),
            (
                "unknown storage mechanism",
                "name = \"exampleId\"\n\n[auction.prebid.managed_user_ids.storage]\nname = \"example_env\"\ntype = \"session\"",
            ),
            (
                "unknown storage field",
                "name = \"exampleId\"\n\n[auction.prebid.managed_user_ids.storage]\nname = \"example_env\"\nunsupported = true",
            ),
        ] {
            let result = parse_prebid_toml_result(&format!(
                r#"
[auction.prebid]
server_url = "https://prebid.example/openrtb2/auction"

[[auction.prebid.managed_user_ids]]
{entry_section}
"#
            ));

            assert!(result.is_err(), "should reject {name}");
        }
    }

    #[test]
    fn managed_user_ids_reject_a_repeated_module_name() {
        let result = parse_prebid_toml_result(
            r#"
[auction.prebid]
server_url = "https://prebid.example/openrtb2/auction"

[[auction.prebid.managed_user_ids]]
name = "exampleId"
params = { pid = "1" }

[[auction.prebid.managed_user_ids]]
name = "exampleId"
params = { pid = "2" }
"#,
        );

        assert!(
            result.is_err(),
            "should reject the same module configured twice"
        );
    }

    #[test]
    fn managed_user_ids_reject_a_case_variant_module_name() {
        let result = parse_prebid_toml_result(
            r#"
[auction.prebid]
server_url = "https://prebid.example/openrtb2/auction"

[[auction.prebid.managed_user_ids]]
name = "exampleId"
params = { pid = "1" }

[[auction.prebid.managed_user_ids]]
name = "exampleid"
params = { pid = "2" }
"#,
        );

        assert!(
            result.is_err(),
            "should reject two names that address the same submodule under Prebid's case-insensitive match"
        );
    }

    #[test]
    fn managed_user_ids_accept_distinct_module_names() {
        let config = parse_prebid_toml(
            r#"
[auction.prebid]
server_url = "https://prebid.example/openrtb2/auction"

[[auction.prebid.managed_user_ids]]
name = "exampleId"

[[auction.prebid.managed_user_ids]]
name = "otherExampleId"
"#,
        );

        assert_eq!(
            config.managed_user_ids.len(),
            2,
            "should keep every distinctly named module"
        );
    }

    #[test]
    fn managed_user_ids_default_to_none_configured() {
        let config = parse_prebid_toml(
            r#"
[auction.prebid]
server_url = "https://prebid.example/openrtb2/auction"
"#,
        );

        assert!(
            config.managed_user_ids.is_empty(),
            "should manage no User ID modules by default"
        );
    }

    #[test]
    fn excluded_gam_ad_unit_path_suffixes_reject_invalid_values() {
        for (suffix, expected_message) in [
            ("", "must not be empty"),
            (" /trackingonly", "must not have surrounding whitespace"),
            ("trackingonly", "must start with '/'"),
            ("/", "must identify a non-root path suffix"),
        ] {
            let error = parse_prebid_toml_result(&format!(
                r#"
[auction.prebid]
server_url = "https://prebid.example/openrtb2/auction"
excluded_gam_ad_unit_path_suffixes = ["{suffix}"]
"#
            ))
            .expect_err("should reject an invalid refresh-auction exclusion suffix");

            assert!(
                error.to_string().contains(expected_message),
                "should report why suffix {suffix:?} is invalid: {error}"
            );
        }
    }

    #[test]
    fn attribute_rewriter_removes_prebid_scripts() {
        let integration = PrebidIntegration::new(base_config());
        let ctx = IntegrationAttributeContext {
            attribute_name: "src",
            element_name: "script",
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
        };

        let rewritten = integration.rewrite("src", "https://cdn.prebid.org/prebid.min.js", &ctx);
        assert!(matches!(rewritten, AttributeRewriteAction::RemoveElement));

        let untouched = integration.rewrite("src", "https://cdn.example.com/app.js", &ctx);
        assert!(matches!(untouched, AttributeRewriteAction::Keep));
    }

    #[test]
    fn attribute_rewriter_handles_query_strings_and_links() {
        let integration = PrebidIntegration::new(base_config());
        let ctx = IntegrationAttributeContext {
            attribute_name: "href",
            element_name: "a",
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
        };

        let rewritten =
            integration.rewrite("href", "https://cdn.prebid.org/prebid.js?v=1.2.3", &ctx);
        assert!(matches!(rewritten, AttributeRewriteAction::RemoveElement));
    }

    #[test]
    fn html_processor_keeps_prebid_scripts_when_no_patterns() {
        let html = r#"<html><head>
            <script src="https://cdn.prebid.org/prebid.min.js"></script>
            <link rel="preload" as="script" href="https://cdn.prebid.org/prebid.js" />
        </head><body></body></html>"#;

        let mut settings = make_settings();
        settings
            .insert_module_config(
                "auction",
                "auction.prebid",
                &json!({
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "timeout_ms": 1000,
                    "script_patterns": [],
                    "debug": false
                }),
            )
            .expect("should update prebid config");
        let registry = IntegrationRegistry::with_registrations(&settings, &[builder()])
            .expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        let result = pipeline.process(Cursor::new(html.as_bytes()), &mut output);
        assert!(result.is_ok());
        let processed = String::from_utf8_lossy(&output);
        assert!(
            processed.contains("tsjs-unified"),
            "Unified bundle should be injected"
        );
        assert!(
            processed.contains("prebid.min.js"),
            "Prebid script should remain when no script patterns configured"
        );
        assert!(
            processed.contains("cdn.prebid.org/prebid.js"),
            "Prebid preload should remain when no script patterns configured"
        );
    }

    #[test]
    fn html_processor_removes_prebid_scripts_when_patterns_match() {
        let html = r#"<html><head>
            <script src="https://cdn.prebid.org/prebid.min.js"></script>
            <link rel="preload" as="script" href="https://cdn.prebid.org/prebid.js" />
        </head><body></body></html>"#;

        let mut settings = make_settings();
        settings
            .insert_module_config(
                "auction",
                "auction.prebid",
                &json!({
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "timeout_ms": 1000,
                    "script_patterns": ["/prebid.js", "/prebid.min.js"],
                    "debug": false
                }),
            )
            .expect("should update prebid config");
        let registry = IntegrationRegistry::with_registrations(&settings, &[builder()])
            .expect("should create registry");
        let config = config_from_settings(&settings, &registry);
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        let result = pipeline.process(Cursor::new(html.as_bytes()), &mut output);
        assert!(result.is_ok());
        let processed = String::from_utf8_lossy(&output);
        assert!(
            processed.contains("tsjs-unified"),
            "Unified bundle should be injected"
        );
        assert!(
            !processed.contains("cdn.prebid.org/prebid.min.js"),
            "Publisher prebid script should be removed when auto-config is enabled"
        );
        assert!(
            !processed.contains("cdn.prebid.org/prebid.js"),
            "Prebid preload should be removed when auto-config is enabled"
        );
        // Both scripts are `defer`, so they execute in document order. The
        // bundle must run first: the shim disables the whole integration when
        // it finds no Prebid.js API on window.pbjs.
        let bundle_index = processed
            .find(PREBID_BUNDLE_ROUTE)
            .expect("should inject external prebid bundle route");
        let shim_index = processed
            .find("tsjs-prebid.min.js")
            .expect("should inject deferred tsjs prebid shim");
        assert!(
            bundle_index < shim_index,
            "external prebid bundle must execute before the deferred tsjs shim"
        );
    }

    #[test]
    fn matches_script_url_matches_common_variants() {
        let integration = PrebidIntegration::new(base_config());
        assert!(integration.matches_script_url("https://cdn.com/prebid.js"));
        assert!(integration.matches_script_url("https://cdn.com/prebid.min.js?version=1"));
        assert!(!integration.matches_script_url("https://cdn.com/app.js"));
    }

    #[test]
    fn script_patterns_config_parsing() {
        let config = parse_prebid_toml(
            r#"
[auction.prebid]
server_url = "https://prebid.example"
script_patterns = ["/prebid.js", "/custom/prebid.min.js"]
"#,
        );

        assert_eq!(config.script_patterns.len(), 2);
        assert!(config.script_patterns.contains(&"/prebid.js".to_string()));
        assert!(
            config
                .script_patterns
                .contains(&"/custom/prebid.min.js".to_string())
        );
    }

    #[test]
    fn script_patterns_defaults() {
        let config = parse_prebid_toml(
            r#"
[auction.prebid]
server_url = "https://prebid.example"
"#,
        );

        assert!(!config.script_patterns.is_empty());
        assert!(config.script_patterns.contains(&"/prebid.js".to_string()));
        assert!(
            config
                .script_patterns
                .contains(&"/prebid.min.js".to_string())
        );
    }

    #[test]
    fn external_bundle_config_parses_with_optional_hash_metadata() {
        let config = parse_prebid_toml(
            r#"
[auction.prebid]
server_url = "https://prebid.example"
external_bundle_url = "https://assets.example/prebid/trusted-prebid.js"
"#,
        );

        assert_eq!(
            config.external_bundle_url.as_deref(),
            Some("https://assets.example/prebid/trusted-prebid.js"),
            "should preserve configured external bundle URL"
        );
        assert!(
            config.external_bundle_sha256.is_none(),
            "SHA-256 should be optional"
        );
    }

    #[test]
    fn external_bundle_config_rejects_malformed_hash_metadata() {
        let err = parse_prebid_toml_result(
            r#"
[auction.prebid]
server_url = "https://prebid.example"
external_bundle_url = "https://assets.example/prebid/trusted-prebid.js"
external_bundle_sha256 = "not-a-sha"
"#,
        )
        .expect_err("should reject malformed SHA-256");

        assert!(
            err.to_string().contains("external_bundle_sha256"),
            "error should mention malformed SHA-256: {err:?}"
        );
    }

    #[test]
    fn external_bundle_config_rejects_non_https_bundle_url() {
        let err = parse_prebid_toml_result(
            r#"
[auction.prebid]
server_url = "https://prebid.example"
external_bundle_url = "http://assets.example/prebid/trusted-prebid.js"
"#,
        )
        .expect_err("should reject non-HTTPS external bundle URL");

        assert!(
            err.to_string().contains("external_bundle_url"),
            "error should mention external bundle URL: {err:?}"
        );
    }

    #[test]
    fn external_bundle_config_rejects_invalid_sri_base64() {
        let err = parse_prebid_toml_result(
            r#"
[auction.prebid]
server_url = "https://prebid.example"
external_bundle_url = "https://assets.example/prebid/trusted-prebid.js"
external_bundle_sri = "sha384-not-valid!!!"
"#,
        )
        .expect_err("should reject invalid SRI base64");

        assert!(
            err.to_string().contains("external_bundle_sri"),
            "error should mention external bundle SRI: {err:?}"
        );
    }

    #[test]
    fn external_bundle_config_rejects_sri_with_wrong_digest_length() {
        let err = parse_prebid_toml_result(
            r#"
[auction.prebid]
server_url = "https://prebid.example"
external_bundle_url = "https://assets.example/prebid/trusted-prebid.js"
external_bundle_sri = "sha384-AAAA"
"#,
        )
        .expect_err("should reject SRI with wrong digest length");

        assert!(
            err.to_string().contains("external_bundle_sri"),
            "error should mention external bundle SRI: {err:?}"
        );
    }

    #[test]
    fn external_bundle_registration_requires_bundle_url() {
        let mut settings = make_settings();
        // The settings name a bundle URL, and this asks what the registry
        // does without one.
        settings
            .insert_module_config("auction", "auction.prebid", &json!({}))
            .expect("should replace the prebid block");
        let plan = Arc::new(
            trusted_server_core::auction::compile_auction_plan(&settings)
                .expect("should compile auction plan"),
        );

        let error =
            match IntegrationRegistry::with_plan_and_registrations(&settings, plan, &[builder()]) {
                Ok(_) => panic!("should reject missing external bundle URL"),
                Err(error) => error,
            };

        assert!(
            error.to_string().contains("external_bundle_url"),
            "error should mention missing external bundle URL: {error:?}"
        );
    }

    #[test]
    fn external_bundle_registration_allows_sha256_without_sri() {
        let mut settings = make_settings();
        settings
            .insert_module_config(
                "auction",
                "auction.prebid",
                &json!({
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "external_bundle_sha256": "0".repeat(64)
                }),
            )
            .expect("should update prebid config");

        let registry = IntegrationRegistry::with_registrations(&settings, &[builder()])
            .expect("should create registry with valid SHA-256 and no SRI");

        assert!(
            registry.has_route(&Method::GET, PREBID_BUNDLE_ROUTE),
            "should register external bundle route"
        );
    }

    #[test]
    fn external_bundle_registration_allows_sha256_with_valid_sha384_sri() {
        let mut settings = make_settings();
        settings
            .insert_module_config(
                "auction",
                "auction.prebid",
                &json!({
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "external_bundle_sha256": "0".repeat(64),
                    "external_bundle_sri": test_sri("sha384", &[0; 48])
                }),
            )
            .expect("should update prebid config");

        let registry = IntegrationRegistry::with_registrations(&settings, &[builder()])
            .expect("should create registry with valid SHA-256 and SHA-384 SRI");

        assert!(
            registry.has_route(&Method::GET, PREBID_BUNDLE_ROUTE),
            "should register external bundle route"
        );
    }

    #[test]
    fn external_bundle_registration_uses_proxy_allowed_domains() {
        let mut settings = make_settings();
        settings.proxy.allowed_domains = vec!["allowed.example".to_string()];
        settings
            .insert_module_config(
                "auction",
                "auction.prebid",
                &json!({
                    "external_bundle_url": "https://blocked.example/prebid/trusted-prebid.js"
                }),
            )
            .expect("should update prebid config");

        let err = match IntegrationRegistry::with_registrations(&settings, &[builder()]) {
            Ok(_) => panic!("should reject bundle host outside proxy.allowed_domains"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("proxy.allowed_domains"),
            "error should mention proxy.allowed_domains: {err:?}"
        );
    }

    #[test]
    fn script_handler_returns_empty_js() {
        let integration = PrebidIntegration::new(base_config());

        let response = integration
            .handle_script_handler()
            .expect("should return response");

        assert_eq!(response.status(), StatusCode::OK);

        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .expect("should have content-type");
        assert_eq!(content_type, "application/javascript; charset=utf-8");

        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .expect("should have cache-control");
        assert_eq!(
            cache_control, "no-store, private",
            "neutralized stable shim must not be cached for a year"
        );
        assert!(
            response.headers().get("surrogate-control").is_none(),
            "neutralized shim must not emit edge-cache headers"
        );

        let body = String::from_utf8(
            response
                .into_body()
                .into_bytes()
                .unwrap_or_default()
                .to_vec(),
        )
        .expect("should parse script body as utf-8");
        assert!(body.contains("// Script overridden by Trusted Server"));
    }

    #[test]
    fn external_bundle_request_cache_mode_validates_version_query() {
        let sha256 = "a".repeat(64);
        let mut config = base_config();
        config.external_bundle_url =
            Some("https://assets.example/prebid/trusted-prebid.js".to_string());
        config.external_bundle_sha256 = Some(sha256.clone());
        let integration = PrebidIntegration::new(config);

        let versioned_req = test_request(format!(
            "https://pub.example{PREBID_BUNDLE_ROUTE}?v={sha256}"
        ));
        let missing_version_req = test_request(format!("https://pub.example{PREBID_BUNDLE_ROUTE}"));
        let mismatched_req = test_request(format!(
            "https://pub.example{PREBID_BUNDLE_ROUTE}?v={}",
            "b".repeat(64)
        ));

        assert_eq!(
            integration
                .external_bundle_request_cache_mode(&versioned_req)
                .expect("should parse versioned request"),
            Some(ExternalBundleCacheMode::Immutable),
            "matching v query should use immutable cache mode"
        );
        assert_eq!(
            integration
                .external_bundle_request_cache_mode(&missing_version_req)
                .expect("should parse unversioned request"),
            Some(ExternalBundleCacheMode::Revalidate),
            "missing v query should use revalidation cache mode"
        );
        assert_eq!(
            integration
                .external_bundle_request_cache_mode(&mismatched_req)
                .expect("should parse mismatched request"),
            None,
            "mismatched v query should 404"
        );
    }

    #[test]
    fn external_bundle_request_cache_mode_rejects_version_when_hash_is_absent() {
        let mut config = base_config();
        config.external_bundle_url =
            Some("https://assets.example/prebid/trusted-prebid.js".to_string());
        let integration = PrebidIntegration::new(config);

        let versioned_req = test_request(format!(
            "https://pub.example{PREBID_BUNDLE_ROUTE}?v={}",
            "a".repeat(64)
        ));
        let unversioned_req = test_request(format!("https://pub.example{PREBID_BUNDLE_ROUTE}"));

        assert_eq!(
            integration
                .external_bundle_request_cache_mode(&versioned_req)
                .expect("should parse versioned request"),
            None,
            "v query should 404 when SHA-256 is omitted"
        );
        assert_eq!(
            integration
                .external_bundle_request_cache_mode(&unversioned_req)
                .expect("should parse unversioned request"),
            Some(ExternalBundleCacheMode::Revalidate),
            "unversioned request should be served with revalidation cache mode"
        );
    }

    #[test]
    fn external_bundle_headers_use_cache_policy_for_mode() {
        let sha256 = "a".repeat(64);
        let mut config = base_config();
        config.external_bundle_url =
            Some("https://assets.example/prebid/trusted-prebid.js".to_string());
        config.external_bundle_sha256 = Some(sha256.clone());
        config.external_bundle_sri = Some(test_sri("sha384", &[0; 48]));
        let integration = PrebidIntegration::new(config);

        let mut immutable = http::Response::builder()
            .status(StatusCode::OK)
            .body(EdgeBody::empty())
            .expect("should build response");
        integration
            .apply_external_bundle_headers(&mut immutable, ExternalBundleCacheMode::Immutable);
        assert_eq!(
            header_value_str(&immutable, "content-type"),
            Some(PREBID_BUNDLE_CONTENT_TYPE.to_string()),
            "should normalize JS content type"
        );
        assert_eq!(
            header_value_str(&immutable, PREBID_BUNDLE_NOSNIFF_HEADER),
            Some(PREBID_BUNDLE_NOSNIFF_VALUE.to_string()),
            "should disable content sniffing"
        );
        assert_eq!(
            header_value_str(&immutable, "cache-control"),
            Some(PREBID_BUNDLE_IMMUTABLE_CACHE_CONTROL.to_string()),
            "versioned responses should be immutable"
        );
        assert_eq!(
            header_value_str(&immutable, "etag"),
            Some(format!("\"sha256:{sha256}\"")),
            "should emit configured hash ETag"
        );

        let mut revalidate = http::Response::builder()
            .status(StatusCode::OK)
            .body(EdgeBody::empty())
            .expect("should build response");
        integration
            .apply_external_bundle_headers(&mut revalidate, ExternalBundleCacheMode::Revalidate);
        assert_eq!(
            header_value_str(&revalidate, "cache-control"),
            Some(PREBID_BUNDLE_REVALIDATION_CACHE_CONTROL.to_string()),
            "unversioned responses should use short-lived revalidation"
        );
    }

    #[test]
    fn external_bundle_response_sanitization_uses_header_whitelist_for_ok_response() {
        let sha256 = "a".repeat(64);
        let mut config = base_config();
        config.external_bundle_url =
            Some("https://assets.example/prebid/trusted-prebid.js".to_string());
        config.external_bundle_sha256 = Some(sha256.clone());
        config.external_bundle_sri = Some(test_sri("sha384", &[0; 48]));
        let integration = PrebidIntegration::new(config);

        let mut upstream = http::Response::builder()
            .status(StatusCode::OK)
            .body(EdgeBody::from("console.log('ok');"))
            .expect("should build upstream response");
        upstream
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
        upstream.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=0"),
        );
        upstream.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_static("bad=1; Path=/"),
        );
        upstream
            .headers_mut()
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        upstream
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static("16"));
        upstream.headers_mut().insert(
            header::HeaderName::from_static("x-upstream"),
            HeaderValue::from_static("leak"),
        );

        let sanitized = integration
            .sanitize_external_bundle_response(upstream, ExternalBundleCacheMode::Immutable);

        assert_eq!(
            header_value_str(&sanitized, "content-type"),
            Some(PREBID_BUNDLE_CONTENT_TYPE.to_string()),
            "should normalize JS content type"
        );
        assert_eq!(
            header_value_str(&sanitized, PREBID_BUNDLE_NOSNIFF_HEADER),
            Some(PREBID_BUNDLE_NOSNIFF_VALUE.to_string()),
            "should disable content sniffing"
        );
        assert_eq!(
            header_value_str(&sanitized, "cache-control"),
            Some(PREBID_BUNDLE_IMMUTABLE_CACHE_CONTROL.to_string()),
            "should apply trusted cache policy"
        );
        assert_eq!(
            header_value_str(&sanitized, "etag"),
            Some(format!("\"sha256:{sha256}\"")),
            "should emit trusted ETag"
        );
        assert_eq!(
            header_value_str(&sanitized, "content-encoding"),
            Some("gzip".to_string()),
            "should preserve body encoding metadata"
        );
        assert!(
            !response_header_is_present(&sanitized, "content-length"),
            "should strip upstream content length so the platform can derive it from the body"
        );
        assert!(
            !response_header_is_present(&sanitized, "set-cookie"),
            "should strip upstream Set-Cookie"
        );
        assert!(
            !response_header_is_present(&sanitized, "x-upstream"),
            "should strip arbitrary upstream headers"
        );
        assert_eq!(
            response_body_string(sanitized),
            "console.log('ok');",
            "should preserve body bytes"
        );
    }

    #[test]
    fn external_bundle_response_sanitization_strips_headers_for_error_response() {
        let mut config = base_config();
        config.external_bundle_url =
            Some("https://assets.example/prebid/trusted-prebid.js".to_string());
        let integration = PrebidIntegration::new(config);

        let mut upstream = http::Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(EdgeBody::from("missing"))
            .expect("should build upstream response");
        upstream
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
        upstream.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000"),
        );
        upstream.headers_mut().insert(
            header::SET_COOKIE,
            HeaderValue::from_static("bad=1; Path=/"),
        );
        upstream.headers_mut().insert(
            header::HeaderName::from_static("x-upstream"),
            HeaderValue::from_static("leak"),
        );

        let sanitized = integration
            .sanitize_external_bundle_response(upstream, ExternalBundleCacheMode::Revalidate);

        assert_eq!(
            sanitized.status(),
            StatusCode::NOT_FOUND,
            "should preserve upstream status"
        );
        assert_eq!(
            header_value_str(&sanitized, "cache-control"),
            Some(PREBID_BUNDLE_ERROR_CACHE_CONTROL.to_string()),
            "should prevent caching upstream error responses"
        );
        assert_eq!(
            header_value_str(&sanitized, "content-type"),
            Some(PREBID_BUNDLE_ERROR_CONTENT_TYPE.to_string()),
            "should replace upstream content type on error responses"
        );
        assert_eq!(
            header_value_str(&sanitized, PREBID_BUNDLE_NOSNIFF_HEADER),
            Some(PREBID_BUNDLE_NOSNIFF_VALUE.to_string()),
            "should disable content sniffing on error responses"
        );
        assert!(
            !response_header_is_present(&sanitized, "set-cookie"),
            "should strip upstream Set-Cookie on error responses"
        );
        assert!(
            !response_header_is_present(&sanitized, "x-upstream"),
            "should strip arbitrary upstream headers on error responses"
        );
    }

    #[test]
    fn external_bundle_registration_requires_proxy_allowed_domains() {
        let mut settings = make_settings();
        settings.proxy.allowed_domains.clear();
        let plan = Arc::new(
            trusted_server_core::auction::compile_auction_plan(&settings)
                .expect("should compile auction plan"),
        );

        let error =
            match IntegrationRegistry::with_plan_and_registrations(&settings, plan, &[builder()]) {
                Ok(_) => panic!("should reject external bundle without proxy allowlist"),
                Err(error) => error,
            };

        assert!(
            error.to_string().contains("proxy.allowed_domains"),
            "error should mention proxy.allowed_domains: {error:?}"
        );
    }
    #[test]
    fn external_bundle_handler_fetches_and_sanitizes_with_platform_client() {
        futures::executor::block_on(async {
            let sha256 = "a".repeat(64);
            let mut config = base_config();
            config.external_bundle_sha256 = Some(sha256.clone());
            let integration = PrebidIntegration::new(config);
            let mut settings = make_settings();
            settings.proxy.allowed_domains = vec!["assets.example".to_string()];

            let stub = Arc::new(StubHttpClient::new());
            stub.push_response_with_headers(
                200,
                b"console.log('bundle');".to_vec(),
                vec![
                    (header::CONTENT_TYPE.as_str(), "text/html"),
                    (header::CACHE_CONTROL.as_str(), "private, max-age=0"),
                    (header::SET_COOKIE.as_str(), "bad=1; Path=/"),
                    ("x-upstream", "leak"),
                ],
            );
            let services = build_services_with_http_client(
                Arc::clone(&stub) as Arc<dyn trusted_server_core::platform::PlatformHttpClient>
            );
            let req = http::Request::builder()
                .method(Method::GET)
                .uri(format!(
                    "https://pub.example{PREBID_BUNDLE_ROUTE}?v={sha256}"
                ))
                .header(header::COOKIE, "ts-ec=should-not-forward")
                .header(header::ACCEPT, "*/*")
                .body(EdgeBody::empty())
                .expect("should build external bundle request");

            let response = integration
                .handle_external_bundle(&settings, &services, req)
                .await
                .expect("should proxy external bundle");

            assert_eq!(response.status(), StatusCode::OK, "should preserve status");
            assert_eq!(
                header_value_str(&response, header::CONTENT_TYPE.as_str()),
                Some(PREBID_BUNDLE_CONTENT_TYPE.to_string()),
                "should normalize JS content type"
            );
            assert_eq!(
                header_value_str(&response, header::CACHE_CONTROL.as_str()),
                Some(PREBID_BUNDLE_IMMUTABLE_CACHE_CONTROL.to_string()),
                "versioned bundle response should be immutable"
            );
            assert_eq!(
                header_value_str(&response, header::ETAG.as_str()),
                Some(format!("\"sha256:{sha256}\"")),
                "should emit configured hash ETag"
            );
            assert!(
                !response_header_is_present(&response, header::SET_COOKIE.as_str()),
                "should strip upstream Set-Cookie"
            );
            assert!(
                !response_header_is_present(&response, "x-upstream"),
                "should strip arbitrary upstream headers"
            );
            assert_eq!(
                response_body_string(response),
                "console.log('bundle');",
                "should preserve bundle bytes"
            );

            assert_eq!(
                stub.recorded_request_uris(),
                vec!["https://assets.example/prebid/trusted-prebid.js".to_string()],
                "should fetch the configured external bundle URL without adding EC query params"
            );
            let recorded_headers = stub.recorded_request_headers();
            assert_eq!(
                recorded_headers.len(),
                1,
                "should make one upstream request"
            );
            assert!(
                !recorded_headers[0]
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(header::COOKIE.as_str())),
                "external bundle fetch should not forward client cookies"
            );
            assert!(
                !recorded_headers[0]
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(header::ACCEPT.as_str())),
                "external bundle fetch should not forward client headers"
            );
        });
    }

    #[test]
    fn routes_include_script_patterns() {
        let integration = PrebidIntegration::new(base_config());

        let routes = integration.routes();

        // Should have routes for default script patterns
        assert!(!routes.is_empty());

        let has_prebid_js_route = routes
            .iter()
            .any(|r| r.path == "/prebid.js" && r.method == Method::GET);
        assert!(has_prebid_js_route, "should register /prebid.js route");

        let has_prebid_min_js_route = routes
            .iter()
            .any(|r| r.path == "/prebid.min.js" && r.method == Method::GET);
        assert!(
            has_prebid_min_js_route,
            "should register /prebid.min.js route"
        );
        assert!(
            routes
                .iter()
                .any(|r| r.path == PREBID_BUNDLE_ROUTE && r.method == Method::GET),
            "should register the bundle route"
        );
    }

    #[test]
    fn head_injector_emits_config_script() {
        let integration = PrebidIntegration::new(base_config());
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        assert_eq!(inserts.len(), 2, "should produce config and bundle inserts");

        let script = &inserts[0];
        assert!(
            script.starts_with("<script>") && script.ends_with("</script>"),
            "should be wrapped in script tags"
        );
        assert!(
            script.contains(r#""accountId":"test-account""#),
            "should include accountId from config: {}",
            script
        );
        assert!(
            script.contains(r#""timeout":1000"#),
            "should include timeout: {}",
            script
        );
        assert!(
            script.contains(r#""debug":false"#),
            "should include debug flag: {}",
            script
        );
        assert!(
            script.contains(r#""bidders":["exampleBidder"]"#),
            "should include bidders array: {}",
            script
        );
        assert!(
            !script.contains("excludedGamAdUnitPathSuffixes"),
            "should omit empty refresh-auction exclusions: {}",
            script
        );
    }

    #[test]
    fn planned_registration_injects_managed_user_ids() {
        let mut settings = make_settings();
        settings
            .insert_module_config(
                "auction",
                "auction.prebid",
                &json!({
                    "external_bundle_url": "https://assets.example/prebid/trusted-prebid.js",
                    "managed_user_ids": [{
                        "name": "exampleId",
                        "params": {"nested": {"value": "</script>"}},
                        "storage": {"name": "example_env", "refresh_in_seconds": 3600}
                    }, {"name": "anotherId"}]
                }),
            )
            .expect("should configure prebid");
        let plan = trusted_server_core::auction::compile_auction_plan(&settings)
            .expect("should compile auction plan");
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };
        for enabled in [true, false] {
            let registration = register_for_plan(&settings, &plan.clone().with_enabled(enabled))
                .expect("should register prebid")
                .expect("should enable prebid");
            let inserts = registration.head_injectors[0].head_inserts(&ctx);
            let script = &inserts[0];
            assert!(
                script.contains(r#""managedUserIds":[{"name":"exampleId""#),
                "should inject managed IDs: {script}"
            );
            assert!(
                script.contains(r#""refreshInSeconds":3600"#),
                "should use browser storage keys: {script}"
            );
            assert!(
                script.contains(r#"{"name":"anotherId"}"#),
                "should omit unset fields: {script}"
            );
            assert!(
                !script.contains("</script>\""),
                "should escape script breakout: {script}"
            );
        }
    }

    #[test]
    fn head_injector_includes_managed_user_ids() {
        let mut config = base_config();
        config.managed_user_ids = vec![PrebidManagedUserIdConfig {
            name: "exampleId".to_string(),
            params: serde_json::Map::from_iter([
                ("pid".to_string(), json!("999")),
                ("notUse3P".to_string(), json!(true)),
            ]),
            storage: Some(PrebidManagedUserIdStorage {
                storage_type: PrebidUserIdStorageType::Html5,
                name: "example_env".to_string(),
                expires: Some(30),
                refresh_in_seconds: Some(3600),
            }),
        }];
        let integration = PrebidIntegration::new(config);
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        let script = &inserts[0];

        assert!(
            script.contains(
                r#""managedUserIds":[{"name":"exampleId","params":{"notUse3P":true,"pid":"999"},"storage":{"type":"html5","name":"example_env","expires":30,"refreshInSeconds":3600}}]"#
            ),
            "should inject the managed User ID entry verbatim: {script}"
        );
    }

    #[test]
    fn head_injector_omits_optional_managed_user_id_fields_when_unset() {
        let mut config = base_config();
        config.managed_user_ids = vec![PrebidManagedUserIdConfig {
            name: "exampleId".to_string(),
            params: serde_json::Map::new(),
            storage: None,
        }];
        let integration = PrebidIntegration::new(config);
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        let script = &inserts[0];

        assert!(
            script.contains(r#""managedUserIds":[{"name":"exampleId"}]"#),
            "should omit empty parameters and absent storage: {script}"
        );
    }

    #[test]
    fn head_injector_omits_managed_user_ids_when_none_configured() {
        let integration = PrebidIntegration::new(base_config());
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        let script = &inserts[0];

        assert!(
            !script.contains("managedUserIds"),
            "should omit managed User IDs when none are configured: {script}"
        );
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn prepared_browser_injection_uses_only_plan_routes_and_browser_timeout_debug() {
        let integration = PrebidIntegration::new(base_config());
        let mut browser_config = PrebidIntegrationConfig::default();
        browser_config.account_id = Some("browser-account".to_string());
        browser_config.timeout_ms = 1750;
        browser_config.debug = false;
        let mut primary = trusted_server_core::auction::test_support::demand_table(
            "auction.prebid-server",
            "https://primary.example.test/openrtb",
        );
        primary.insert("timeout_ms".to_string(), json!(3000));
        primary.insert("debug".to_string(), json!(true));
        let mut secondary = trusted_server_core::auction::test_support::demand_table(
            "auction.prebid-server",
            "https://secondary.example.test/openrtb",
        );
        secondary.insert("timeout_ms".to_string(), json!(4000));
        secondary.insert("debug".to_string(), json!(true));
        let mut config = trusted_server_core::auction::test_support::plan_config_with(
            vec![("pbs_primary", primary), ("pbs_secondary", secondary)],
            &[trusted_server_auction_prebid_server::builder()],
        );
        config.timeout_ms = 2500;
        config.bidders = BTreeMap::from([
            (
                BidderId::from_str("secondaryRoute").expect("should parse bidder ID"),
                BidderRouteConfig {
                    module: ProviderId::from_str("pbs_secondary")
                        .expect("should parse provider ID"),
                },
            ),
            (
                BidderId::from_str("primaryRoute").expect("should parse bidder ID"),
                BidderRouteConfig {
                    module: ProviderId::from_str("pbs_primary").expect("should parse provider ID"),
                },
            ),
        ]);
        let plan = AuctionPlan::compile(config)
            .expect("should compile plan while browser integration is not part of compilation");

        let inserts = integration.head_inserts_for_plan(&browser_config, &plan);
        let script = &inserts[0];

        assert!(script.contains(r#""timeout":1750,"debug":false"#));
        assert!(
            script.contains(r#""serverSideBidders":["primaryRoute","secondaryRoute"]"#),
            "should inject deterministic browser route codes: {script}"
        );
        assert!(!script.contains("pbs_primary"));
        assert!(!script.contains("pbs_secondary"));
        assert!(!script.contains("3000"));
        assert!(!script.contains("4000"));

        let disabled_plan = plan.clone().with_enabled(false);
        let disabled_inserts = integration.head_inserts_for_plan(&browser_config, &disabled_plan);
        assert!(
            disabled_inserts[0].contains(r#""serverSideBidders":[]"#),
            "auction kill switch should suppress browser server-side bidders: {}",
            disabled_inserts[0]
        );
    }

    #[test]
    fn head_injector_escapes_script_breakout_in_managed_user_ids() {
        let mut config = base_config();
        config.managed_user_ids = vec![PrebidManagedUserIdConfig {
            params: serde_json::Map::from_iter([(
                "pid".to_string(),
                json!("1</script><script>alert(1)</script>"),
            )]),
            ..valid_managed_user_id()
        }];
        let integration = PrebidIntegration::new(config);
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        let script = &inserts[0];

        assert!(
            script.contains(r#""pid":"1\u003c/script>\u003cscript>alert(1)\u003c/script>""#),
            "should retain the escaped module parameter: {script}"
        );
        assert_eq!(
            script.matches("</script>").count(),
            1,
            "should contain only the legitimate outer closing script tag"
        );
    }

    #[test]
    fn browser_only_config_defaults_are_independent_of_the_server() {
        let config = PrebidIntegrationConfig::default();

        assert_eq!(config.timeout_ms, 1000);
        assert!(!config.debug);
    }

    #[test]
    fn head_injector_includes_excluded_gam_ad_unit_path_suffixes() {
        let mut config = base_config();
        config.excluded_gam_ad_unit_path_suffixes =
            vec!["/trackingonly".to_string(), "/measurement-only".to_string()];
        let integration = PrebidIntegration::new(config);
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        let script = &inserts[0];
        assert!(
            script.contains(
                r#""excludedGamAdUnitPathSuffixes":["/trackingonly","/measurement-only"]"#
            ),
            "should inject refresh-auction exclusion suffixes: {}",
            script
        );
    }

    #[test]
    fn head_injector_handles_missing_account_id() {
        let mut config = base_config();
        config.account_id = None;
        let integration = PrebidIntegration::new(config);
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        let script = &inserts[0];
        assert!(
            script.contains(r#""accountId":"""#),
            "should emit empty accountId when not configured: {}",
            script
        );
    }

    #[test]
    fn head_injector_emits_external_bundle_script_with_hash_and_integrity() {
        let sha256 = "a".repeat(64);
        let mut config = base_config();
        config.external_bundle_url =
            Some("https://assets.example/prebid/trusted-prebid.js".to_string());
        config.external_bundle_sha256 = Some(sha256.clone());
        config.external_bundle_sri = Some(test_sri("sha384", &[0; 48]));
        let integration = PrebidIntegration::new(config);
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);

        assert_eq!(inserts.len(), 2, "should emit config and bundle scripts");
        assert!(
            inserts[1].contains(&format!("src=\"{PREBID_BUNDLE_ROUTE}?v={sha256}\"")),
            "bundle script should use content-addressed first-party URL: {}",
            inserts[1]
        );
        assert!(
            inserts[1].contains("integrity=\"sha384-"),
            "bundle script should include configured SRI: {}",
            inserts[1]
        );
        assert!(
            !inserts[1].contains("crossorigin"),
            "same-origin bundle script should not include crossorigin: {}",
            inserts[1]
        );
    }

    #[test]
    fn head_injector_emits_external_bundle_script_without_hash_query_when_unhashed() {
        let mut config = base_config();
        config.external_bundle_url =
            Some("https://assets.example/prebid/trusted-prebid.js".to_string());
        let integration = PrebidIntegration::new(config);
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);

        assert_eq!(inserts.len(), 2, "should emit config and bundle scripts");
        assert!(
            inserts[1].contains(&format!("src=\"{PREBID_BUNDLE_ROUTE}\"")),
            "bundle script should use first-party route without hash query: {}",
            inserts[1]
        );
        assert!(
            !inserts[1].contains("?v="),
            "unhashed bundle script should not include version query: {}",
            inserts[1]
        );
    }

    #[test]
    fn head_injector_escapes_less_than_signs_in_values() {
        let mut config = base_config();
        config.account_id = Some("</script><script>alert(1)</script>".to_string());
        let integration = PrebidIntegration::new(config);
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        let script = &inserts[0];
        assert!(
            script.contains(r#""accountId":"\u003c/script>\u003cscript>alert(1)\u003c/script>""#),
            "should escape every less-than sign inside JSON values: {}",
            script
        );
    }

    #[test]
    fn head_injector_omits_client_side_bidders_when_empty() {
        let integration = PrebidIntegration::new(base_config());
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        let script = &inserts[0];
        assert!(
            !script.contains("clientSideBidders"),
            "should omit clientSideBidders when empty: {}",
            script
        );
    }

    #[test]
    fn head_injector_includes_client_side_bidders_when_configured() {
        let mut config = base_config();
        config.client_side_bidders = vec!["rubicon".to_string(), "magnite".to_string()];
        let integration = PrebidIntegration::new(config);
        let document_state = IntegrationDocumentState::default();
        let ctx = IntegrationHtmlContext {
            request_host: "pub.example",
            request_scheme: "https",
            origin_host: "origin.example",
            document_state: &document_state,
        };

        let inserts = integration.head_inserts(&ctx);
        let script = &inserts[0];
        assert!(
            script.contains(r#""clientSideBidders":["rubicon","magnite"]"#),
            "should include clientSideBidders array: {}",
            script
        );
    }

    #[test]
    fn routes_with_empty_script_patterns() {
        let mut config = base_config();
        config.script_patterns = vec![];
        let integration = PrebidIntegration::new(config);

        let routes = integration.routes();

        assert_eq!(
            routes.len(),
            1,
            "should keep bundle route when no script patterns configured"
        );
        assert!(
            routes
                .iter()
                .any(|route| route.path == PREBID_BUNDLE_ROUTE && route.method == Method::GET),
            "should register the bundle route"
        );
    }

    #[test]
    fn config_accepts_aps_in_prebid_bidder_lists_for_upgrade_compatibility() {
        for (field, bidder) in [
            ("bidders", "aps"),
            ("bidders", "APS"),
            ("client_side_bidders", "Aps"),
        ] {
            let result = parse_prebid_toml_result(&format!(
                r#"
[auction.prebid]
server_url = "https://prebid.example"
{field} = ["{bidder}"]
"#
            ));
            assert!(result.is_ok(), "should accept legacy APS in {field}");
        }
    }

    #[test]
    fn module_constant_is_the_crate_folder() {
        assert_eq!(
            super::MODULE,
            trusted_server_core::module_name!(),
            "should be named by the folder this crate lives in"
        );
    }
}
