//! Compile-time `OpenRTB` profile registry and typed profile plans.

use std::collections::BTreeMap;

use error_stack::Report;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::consent_config::ConsentForwardingMode;
use crate::error::TrustedServerError;
use crate::integrations::aps::compile_profile_config as compile_aps_profile_config;
use crate::integrations::prebid::{
    BidParamOverrideEngine, BidParamOverrideRule, compile_profile_override_rules,
};

const STANDARD_PROFILE_ID: &str = "standard";
const PREBID_PROFILE_ID: &str = "prebid-server";
const APS_PROFILE_ID: &str = "aps";
const STATIC_EXTENSION_MAX_BYTES: usize = 16 * 1024;
const STATIC_EXTENSION_MAX_DEPTH: usize = 8;
const STATIC_EXTENSION_MAX_KEYS: usize = 256;

/// A registered profile's provider-timeout default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileTimeoutDefault {
    /// Inherit the configured auction timeout.
    Auction,
    /// Use this fixed profile timeout.
    Fixed(u32),
}

/// Compile-time profile registration.
#[derive(Clone, Copy)]
pub struct OpenRtbProfileRegistration {
    /// Stable profile identifier used by configuration.
    pub id: &'static str,
    /// Profile timeout used when a provider has no explicit override.
    pub default_timeout: ProfileTimeoutDefault,
    compile: fn(&Value) -> Result<CompiledOpenRtbProfile, Report<TrustedServerError>>,
}

impl core::fmt::Debug for OpenRtbProfileRegistration {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("OpenRtbProfileRegistration")
            .field("id", &self.id)
            .field("default_timeout", &self.default_timeout)
            .finish_non_exhaustive()
    }
}

impl OpenRtbProfileRegistration {
    pub(crate) fn compile(
        self,
        config: &Value,
    ) -> Result<CompiledOpenRtbProfile, Report<TrustedServerError>> {
        (self.compile)(config)
    }
}

/// Immutable, typed profile behavior selected during plan compilation.
#[derive(Debug, Clone)]
pub enum CompiledOpenRtbProfile {
    /// Generic `OpenRTB` 2.6 profile.
    Standard(StandardProfilePlan),
    /// Prebid Server compatibility profile.
    PrebidServer(PrebidProfilePlan),
    /// APS `OpenRTB` compatibility profile.
    Aps(ApsProfilePlan),
}

impl CompiledOpenRtbProfile {
    /// Return the stable profile identifier.
    #[must_use]
    pub fn id(&self) -> &'static str {
        match self {
            Self::Standard(_) => STANDARD_PROFILE_ID,
            Self::PrebidServer(_) => PREBID_PROFILE_ID,
            Self::Aps(_) => APS_PROFILE_ID,
        }
    }

    /// Return whether this plan uses the Prebid Server profile.
    #[must_use]
    pub(crate) fn is_prebid_server(&self) -> bool {
        matches!(self, Self::PrebidServer(_))
    }
}

/// Validated static extension object.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StaticExtension(Map<String, Value>);

impl StaticExtension {
    /// Borrow the validated extension object.
    #[must_use]
    pub fn as_object(&self) -> &Map<String, Value> {
        &self.0
    }
}

/// Compiled generic `OpenRTB` profile configuration.
#[derive(Debug, Clone, Default)]
pub struct StandardProfilePlan {
    /// Static request-level extension fields.
    pub request_ext: StaticExtension,
    /// Static impression-level extension fields.
    pub imp_ext: StaticExtension,
}

/// Compiled Prebid profile configuration.
#[derive(Debug, Clone)]
pub struct PrebidProfilePlan {
    /// Include Prebid HTTP exchange diagnostics.
    pub debug: bool,
    /// Set `OpenRTB` test mode.
    pub test_mode: bool,
    /// Optional query fragment appended to the page URL under legacy rules.
    pub debug_query_params: Option<String>,
    /// Compiled override matching and merge index.
    pub(crate) override_engine: BidParamOverrideEngine,
    /// Consent transport policy.
    pub consent_forwarding: ConsentForwardingMode,
}

/// Compiled APS profile configuration.
#[derive(Debug, Clone)]
pub struct ApsProfilePlan {
    /// APS account identifier.
    pub account_id: String,
    /// Include APS request/response diagnostics.
    pub debug: bool,
    /// Permit APS script creatives.
    pub allow_script_creatives: bool,
    /// Optional authorized inventory domain.
    pub inventory_domain: Option<String>,
    /// Optional canonical inventory page origin.
    pub inventory_page_origin: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct StandardProfileConfig {
    #[serde(default)]
    request_ext: Option<Value>,
    #[serde(default)]
    imp_ext: Option<Value>,
}

/// Typed operator configuration compiled into a [`PrebidProfilePlan`].
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrebidProfileConfig {
    #[serde(default)]
    debug: bool,
    #[serde(default)]
    test_mode: bool,
    #[serde(default)]
    debug_query_params: Option<String>,
    #[serde(default)]
    bid_param_zone_overrides: BTreeMap<String, BTreeMap<String, Map<String, Value>>>,
    #[serde(default)]
    bid_param_overrides: BTreeMap<String, Map<String, Value>>,
    #[serde(default)]
    bid_param_override_rules: Vec<BidParamOverrideRule>,
    #[serde(default)]
    consent_forwarding: ConsentForwardingMode,
}

const PROFILE_REGISTRATIONS: [OpenRtbProfileRegistration; 3] = [
    OpenRtbProfileRegistration {
        id: STANDARD_PROFILE_ID,
        default_timeout: ProfileTimeoutDefault::Auction,
        compile: compile_standard,
    },
    OpenRtbProfileRegistration {
        id: PREBID_PROFILE_ID,
        default_timeout: ProfileTimeoutDefault::Fixed(1000),
        compile: compile_prebid,
    },
    OpenRtbProfileRegistration {
        id: APS_PROFILE_ID,
        default_timeout: ProfileTimeoutDefault::Fixed(800),
        compile: compile_aps,
    },
];

/// Return the compile-time profile registry.
#[must_use]
pub fn profile_registrations() -> &'static [OpenRtbProfileRegistration] {
    &PROFILE_REGISTRATIONS
}

pub(crate) fn find_profile(id: &str) -> Option<OpenRtbProfileRegistration> {
    profile_registrations()
        .iter()
        .copied()
        .find(|registration| registration.id == id)
}

fn configuration_error(message: impl Into<String>) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: message.into(),
    })
}

fn deserialize_profile<T>(id: &str, value: &Value) -> Result<T, Report<TrustedServerError>>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(value.clone())
        .map_err(|error| configuration_error(format!("invalid `{id}` profile_config: {error}")))
}

fn compile_standard(value: &Value) -> Result<CompiledOpenRtbProfile, Report<TrustedServerError>> {
    let config: StandardProfileConfig = deserialize_profile(STANDARD_PROFILE_ID, value)?;
    Ok(CompiledOpenRtbProfile::Standard(StandardProfilePlan {
        request_ext: validate_static_extension("request_ext", config.request_ext)?,
        imp_ext: validate_static_extension("imp_ext", config.imp_ext)?,
    }))
}

fn compile_prebid(value: &Value) -> Result<CompiledOpenRtbProfile, Report<TrustedServerError>> {
    let config: PrebidProfileConfig = deserialize_profile(PREBID_PROFILE_ID, value)?;
    let override_engine = compile_profile_override_rules(
        &config.bid_param_zone_overrides,
        &config.bid_param_overrides,
        &config.bid_param_override_rules,
    )?;
    Ok(CompiledOpenRtbProfile::PrebidServer(PrebidProfilePlan {
        debug: config.debug,
        test_mode: config.test_mode,
        debug_query_params: config.debug_query_params,
        override_engine,
        consent_forwarding: config.consent_forwarding,
    }))
}

fn compile_aps(value: &Value) -> Result<CompiledOpenRtbProfile, Report<TrustedServerError>> {
    let config = compile_aps_profile_config(value.clone())?;
    Ok(CompiledOpenRtbProfile::Aps(ApsProfilePlan {
        account_id: config.account_id,
        debug: config.debug,
        allow_script_creatives: config.allow_script_creatives,
        inventory_domain: config.inventory_domain,
        inventory_page_origin: config.inventory_page_origin,
    }))
}

fn validate_static_extension(
    field: &str,
    value: Option<Value>,
) -> Result<StaticExtension, Report<TrustedServerError>> {
    let Some(value) = value else {
        return Ok(StaticExtension::default());
    };
    let object = value.as_object().ok_or_else(|| {
        configuration_error(format!("standard profile {field} must be an object"))
    })?;
    let size = serde_json::to_vec(&value)
        .map_err(|error| configuration_error(format!("cannot serialize {field}: {error}")))?
        .len();
    if size > STATIC_EXTENSION_MAX_BYTES {
        return Err(configuration_error(format!(
            "standard profile {field} exceeds {STATIC_EXTENSION_MAX_BYTES} bytes"
        )));
    }
    validate_extension_value(field, &value, 0)?;
    reject_reserved_fields(field, object)?;
    Ok(StaticExtension(object.clone()))
}

fn validate_extension_value(
    field: &str,
    value: &Value,
    container_depth: usize,
) -> Result<(), Report<TrustedServerError>> {
    match value {
        Value::Object(object) => {
            let container_depth = container_depth + 1;
            if container_depth > STATIC_EXTENSION_MAX_DEPTH {
                return Err(configuration_error(format!(
                    "standard profile {field} exceeds nesting depth {STATIC_EXTENSION_MAX_DEPTH}"
                )));
            }
            if object.len() > STATIC_EXTENSION_MAX_KEYS {
                return Err(configuration_error(format!(
                    "standard profile {field} object exceeds {STATIC_EXTENSION_MAX_KEYS} keys"
                )));
            }
            for nested in object.values() {
                validate_extension_value(field, nested, container_depth)?;
            }
        }
        Value::Array(array) => {
            let container_depth = container_depth + 1;
            if container_depth > STATIC_EXTENSION_MAX_DEPTH {
                return Err(configuration_error(format!(
                    "standard profile {field} exceeds nesting depth {STATIC_EXTENSION_MAX_DEPTH}"
                )));
            }
            for nested in array {
                validate_extension_value(field, nested, container_depth)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn reject_reserved_fields(
    field: &str,
    object: &Map<String, Value>,
) -> Result<(), Report<TrustedServerError>> {
    let reserved: &[&str] = match field {
        "request_ext" => &["trusted_server"],
        _ => &[],
    };
    if let Some(key) = reserved.iter().find(|key| object.contains_key(**key)) {
        return Err(configuration_error(format!(
            "standard profile {field} cannot claim reserved field `{key}`"
        )));
    }
    Ok(())
}
