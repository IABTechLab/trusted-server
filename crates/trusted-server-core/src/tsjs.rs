use std::collections::HashSet;
use std::fmt;
use std::sync::OnceLock;

use error_stack::Report;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use trusted_server_js::{
    TsjsModulePhase, all_integration_metadata, concatenated_hash, release_id, single_module_hash,
};
use validator::Validate;

use crate::auction::types::{
    BidRenderSourceV1, BrowserAuctionProjectionV1, MAX_BROWSER_AUCTION_PROJECTION_BYTES,
    SlotAuctionDecisionV1,
};
use crate::error::TrustedServerError;
use crate::settings::{IntegrationConfig, Settings};

/// Canonical empty initial auction projection used when no auction ran or a
/// projection fails closed.
pub(crate) const EMPTY_AUCTION_PROJECTION_JSON_V1: &str = r#"{"version":1,"auction":{"version":1,"auctionId":"initial","results":[]},"slots":[],"bids":[]}"#;

pub(crate) const INTEGRATION_CONFIG_IDS_V1: [&str; 11] = [
    "aps",
    "datadome",
    "didomi",
    "google_tag_manager",
    "gpt",
    "lockr",
    "osano",
    "permutive",
    "prebid",
    "sourcepoint",
    "testlight",
];
pub(crate) const INTEGRATION_CONFIG_MAX_DEPTH_V1: usize = 16;
pub(crate) const INTEGRATION_CONFIG_MAX_VALUES_V1: usize = 4_096;
pub(crate) const INTEGRATION_CONFIG_MAX_STRING_BYTES_V1: usize = 4_096;
pub(crate) const INTEGRATION_CONFIG_MAX_ENTRY_BYTES_V1: usize = 65_536;
const INTEGRATION_CONFIG_MAX_CARRIER_BYTES_V1: usize = 524_288;
const SERVER_BOOT_TRANSPORT_MAX_BYTES_V1: usize = 10 * 1024 * 1024;

#[derive(Debug)]
enum EcmascriptJsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Self>),
    Object(Vec<(String, Self)>),
}

struct EcmascriptJsonVisitor;

impl<'de> Visitor<'de> for EcmascriptJsonVisitor {
    type Value = EcmascriptJsonValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a finite JSON value")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(EcmascriptJsonValue::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(EcmascriptJsonValue::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(EcmascriptJsonValue::Number(value as f64))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(EcmascriptJsonValue::Number(value as f64))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if !value.is_finite() {
            return Err(E::custom("JSON number must be finite"));
        }
        Ok(EcmascriptJsonValue::Number(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(EcmascriptJsonValue::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(EcmascriptJsonValue::String(value))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0));
        while let Some(value) = sequence.next_element()? {
            values.push(value);
        }
        Ok(EcmascriptJsonValue::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Vec::with_capacity(map.size_hint().unwrap_or(0));
        let mut keys = HashSet::new();
        while let Some((key, value)) = map.next_entry::<String, EcmascriptJsonValue>()? {
            if !keys.insert(key.clone()) {
                return Err(serde::de::Error::custom("duplicate JSON object key"));
            }
            values.push((key, value));
        }
        Ok(EcmascriptJsonValue::Object(values))
    }
}

impl<'de> Deserialize<'de> for EcmascriptJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(EcmascriptJsonVisitor)
    }
}

fn ecmascript_number_string(value: f64) -> Option<String> {
    if !value.is_finite() {
        return None;
    }
    if value == 0.0 {
        return Some("0".to_owned());
    }
    let negative = value.is_sign_negative();
    let raw = serde_json::to_string(&value.abs()).ok()?;
    let (mantissa, exponent) = raw
        .split_once(['e', 'E'])
        .map_or((raw.as_str(), 0), |(mantissa, exponent)| {
            (mantissa, exponent.parse::<i32>().ok().unwrap_or(i32::MAX))
        });
    if exponent == i32::MAX {
        return None;
    }
    let point = mantissa.find('.').unwrap_or(mantissa.len());
    let mut digits = mantissa
        .bytes()
        .filter(|byte| *byte != b'.')
        .map(char::from)
        .collect::<String>();
    let mut decimal_position = i32::try_from(point).ok()?.checked_add(exponent)?;
    while digits.len() > 1 && digits.starts_with('0') {
        digits.remove(0);
        decimal_position = decimal_position.checked_sub(1)?;
    }
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    let digit_count = i32::try_from(digits.len()).ok()?;
    let mut output = String::new();
    if negative {
        output.push('-');
    }
    if digit_count <= decimal_position && decimal_position <= 21 {
        output.push_str(&digits);
        output.extend(std::iter::repeat_n(
            '0',
            usize::try_from(decimal_position - digit_count).ok()?,
        ));
    } else if decimal_position > 0 && decimal_position <= 21 {
        let split = usize::try_from(decimal_position).ok()?;
        output.push_str(&digits[..split]);
        output.push('.');
        output.push_str(&digits[split..]);
    } else if decimal_position > -6 && decimal_position <= 0 {
        output.push_str("0.");
        output.extend(std::iter::repeat_n(
            '0',
            usize::try_from(-decimal_position).ok()?,
        ));
        output.push_str(&digits);
    } else {
        output.push_str(&digits[..1]);
        if digits.len() > 1 {
            output.push('.');
            output.push_str(&digits[1..]);
        }
        output.push('e');
        let scientific_exponent = decimal_position.checked_sub(1)?;
        if scientific_exponent >= 0 {
            output.push('+');
        }
        output.push_str(&scientific_exponent.to_string());
    }
    Some(output)
}

fn ecmascript_array_index(key: &str) -> Option<u32> {
    if key.is_empty() || (key.len() > 1 && key.starts_with('0')) {
        return None;
    }
    let value = key.parse::<u32>().ok()?;
    (value < u32::MAX && value.to_string() == key).then_some(value)
}

fn write_ecmascript_json(value: &EcmascriptJsonValue, output: &mut String) -> Option<()> {
    match value {
        EcmascriptJsonValue::Null => output.push_str("null"),
        EcmascriptJsonValue::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        EcmascriptJsonValue::Number(value) => output.push_str(&ecmascript_number_string(*value)?),
        EcmascriptJsonValue::String(value) => {
            output.push_str(&serde_json::to_string(value).ok()?);
        }
        EcmascriptJsonValue::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                write_ecmascript_json(value, output)?;
            }
            output.push(']');
        }
        EcmascriptJsonValue::Object(values) => {
            let mut indexed = values
                .iter()
                .filter_map(|(key, value)| {
                    ecmascript_array_index(key).map(|index| (index, key, value))
                })
                .collect::<Vec<_>>();
            indexed.sort_unstable_by_key(|(index, _, _)| *index);
            output.push('{');
            let mut first = true;
            for (_, key, value) in indexed {
                if !first {
                    output.push(',');
                }
                first = false;
                output.push_str(&serde_json::to_string(key).ok()?);
                output.push(':');
                write_ecmascript_json(value, output)?;
            }
            for (key, value) in values {
                if ecmascript_array_index(key).is_some() {
                    continue;
                }
                if !first {
                    output.push(',');
                }
                first = false;
                output.push_str(&serde_json::to_string(key).ok()?);
                output.push(':');
                write_ecmascript_json(value, output)?;
            }
            output.push('}');
        }
    }
    Some(())
}

fn ecmascript_json_stringify_v1(json: &str) -> Option<String> {
    let value = serde_json::from_str::<EcmascriptJsonValue>(json).ok()?;
    let mut output = String::with_capacity(json.len());
    write_ecmascript_json(&value, &mut output)?;
    Some(output)
}

/// One admitted browser-safe product configuration.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct IntegrationConfigEntryV1 {
    id: &'static str,
    config: serde_json::Value,
}

/// The sole generic product configuration carrier in `BootV1`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct IntegrationConfigsV1 {
    version: u8,
    entries: Vec<IntegrationConfigEntryV1>,
}

impl IntegrationConfigsV1 {
    /// Admit canonical, explicitly projected browser configuration entries.
    pub(crate) fn new(
        entries: Vec<(&'static str, serde_json::Value)>,
    ) -> Result<Self, Report<TrustedServerError>> {
        if entries.len() > INTEGRATION_CONFIG_IDS_V1.len() {
            return Err(integration_config_error("more than 11 product entries"));
        }

        let mut previous_index = None;
        let mut value_count = 0;
        let mut admitted = Vec::with_capacity(entries.len());
        for (id, config) in entries {
            let index = INTEGRATION_CONFIG_IDS_V1
                .iter()
                .position(|candidate| *candidate == id)
                .ok_or_else(|| integration_config_error("unknown product id"))?;
            if previous_index.is_some_and(|previous| index <= previous) {
                return Err(integration_config_error(
                    "product entries are duplicated or out of canonical order",
                ));
            }
            if !config.is_object() {
                return Err(integration_config_error(
                    "product config must be a non-null object",
                ));
            }
            validate_integration_config_value_v1(&config, 0, &mut value_count)?;
            let entry = IntegrationConfigEntryV1 { id, config };
            admitted.push(entry);
            previous_index = Some(index);
        }

        let carrier = Self {
            version: 1,
            entries: admitted,
        };
        carrier.canonical_json_v1()?;
        Ok(carrier)
    }

    fn canonical_json_v1(&self) -> Result<String, Report<TrustedServerError>> {
        for entry in &self.entries {
            let serialized = serde_json::to_string(entry)
                .map_err(|_| integration_config_error("product entry serialization failed"))?;
            let canonical = ecmascript_json_stringify_v1(&serialized).ok_or_else(|| {
                integration_config_error("product entry ECMAScript serialization failed")
            })?;
            if canonical.len() > INTEGRATION_CONFIG_MAX_ENTRY_BYTES_V1 {
                return Err(integration_config_error(
                    "product entry exceeds 65,536 UTF-8 JSON bytes",
                ));
            }
        }
        let serialized = serde_json::to_string(self)
            .map_err(|_| integration_config_error("carrier serialization failed"))?;
        let canonical = ecmascript_json_stringify_v1(&serialized)
            .ok_or_else(|| integration_config_error("carrier ECMAScript serialization failed"))?;
        if canonical.len() > INTEGRATION_CONFIG_MAX_CARRIER_BYTES_V1 {
            return Err(integration_config_error(
                "carrier exceeds 524,288 UTF-8 JSON bytes",
            ));
        }
        Ok(canonical)
    }

    /// Construct the exact empty carrier used by documents with no product modules.
    #[must_use]
    pub(crate) const fn empty() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }

    /// Require exact equality between selected catalog products and carrier entries.
    pub(crate) fn validate_manifest(
        &self,
        module_ids: &[&str],
    ) -> Result<(), Report<TrustedServerError>> {
        let selected = INTEGRATION_CONFIG_IDS_V1
            .iter()
            .copied()
            .filter(|product_id| {
                module_ids.iter().any(|module_id| {
                    integration_config_product_for_module_v1(module_id) == Some(*product_id)
                })
            })
            .collect::<Vec<_>>();
        let configured = self
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        if configured != selected {
            return Err(integration_config_error(
                "manifest membership disagrees with product config entries",
            ));
        }
        Ok(())
    }

    /// Select the canonical product subset required by one generated manifest.
    pub(crate) fn select_for_manifest(
        &self,
        module_ids: &[&str],
    ) -> Result<Self, Report<TrustedServerError>> {
        let entries = self
            .entries
            .iter()
            .filter(|entry| {
                module_ids.iter().any(|module_id| {
                    integration_config_product_for_module_v1(module_id) == Some(entry.id)
                })
            })
            .map(|entry| (entry.id, entry.config.clone()))
            .collect::<Vec<_>>();
        let selected = Self::new(entries)?;
        selected.validate_manifest(module_ids)?;
        Ok(selected)
    }

    fn gpt_gam_attribution_enabled(&self) -> bool {
        self.entries
            .iter()
            .find(|entry| entry.id == "gpt")
            .and_then(|entry| entry.config.get("gamAttributionEnabled"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    }
}

impl Default for IntegrationConfigsV1 {
    fn default() -> Self {
        Self::empty()
    }
}

#[must_use]
pub(crate) fn is_integration_config_product_v1(id: &str) -> bool {
    INTEGRATION_CONFIG_IDS_V1.contains(&id)
}

#[must_use]
pub(crate) fn integration_config_order_v1(id: &str) -> Option<usize> {
    INTEGRATION_CONFIG_IDS_V1
        .iter()
        .position(|candidate| *candidate == id)
}

fn validate_integration_config_value_v1(
    value: &serde_json::Value,
    depth: usize,
    value_count: &mut usize,
) -> Result<(), Report<TrustedServerError>> {
    if depth > INTEGRATION_CONFIG_MAX_DEPTH_V1 {
        return Err(integration_config_error("config depth exceeds 16"));
    }
    *value_count += 1;
    if *value_count > INTEGRATION_CONFIG_MAX_VALUES_V1 {
        return Err(integration_config_error(
            "carrier exceeds 4,096 JSON values",
        ));
    }
    match value {
        serde_json::Value::String(string) => {
            if string.len() > INTEGRATION_CONFIG_MAX_STRING_BYTES_V1 {
                return Err(integration_config_error(
                    "config string exceeds 4,096 UTF-8 bytes",
                ));
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                validate_integration_config_value_v1(value, depth + 1, value_count)?;
            }
        }
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                if key.len() > INTEGRATION_CONFIG_MAX_STRING_BYTES_V1 {
                    return Err(integration_config_error(
                        "config key exceeds 4,096 UTF-8 bytes",
                    ));
                }
                validate_integration_config_value_v1(value, depth + 1, value_count)?;
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
    Ok(())
}

fn integration_config_product_for_module_v1(module_id: &str) -> Option<&'static str> {
    match module_id {
        "aps" => Some("aps"),
        "datadome" => Some("datadome"),
        "didomi" => Some("didomi"),
        "google_tag_manager" => Some("google_tag_manager"),
        "gpt" | "gpt_later" => Some("gpt"),
        "lockr" => Some("lockr"),
        "osano_consent" | "osano_lifecycle" => Some("osano"),
        "permutive_context" | "permutive_lifecycle" => Some("permutive"),
        "prebid" | "prebid_later" => Some("prebid"),
        "sourcepoint_consent" | "sourcepoint_lifecycle" => Some("sourcepoint"),
        "testlight" => Some("testlight"),
        _ => None,
    }
}

fn integration_config_error(message: &str) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: format!("TSJS integration config: {message}"),
    })
}

/// Serialize the production size-admitted first-display bootstrap transport.
///
/// # Errors
///
/// Returns an error when the projection, catalog inventory, boot bits, or selected
/// first-display mask is invalid or not admitted by the generated release catalog.
pub fn tsjs_bootstrap_fragment_v1(
    config: TsjsBootScriptConfigV1<'_>,
    publisher_origin: &str,
) -> Result<String, Report<TrustedServerError>> {
    let selected = selected_metadata(config.module_ids)?;
    config
        .integration_configs
        .validate_manifest(config.module_ids)?;
    let contains = |id: &str| selected.iter().any(|metadata| metadata.id == id);
    let creative_required =
        config.creative.enabled && (config.creative.click_guard || config.creative.render_guard);
    if contains("creative") != creative_required
        || (!config.creative.enabled
            && (config.creative.click_guard || config.creative.render_guard))
    {
        return Err(boot_manifest_error(
            "creative boot bits disagree with manifest membership",
        ));
    }
    if contains("gpt_diagnostics") != config.gpt_diagnostics_active
        || contains("diagnostics_presentation")
            != (config.render_trace_overlay || config.gpt_diagnostics_active)
    {
        return Err(boot_manifest_error(
            "diagnostics membership disagrees with boot bits",
        ));
    }

    let projection = crate::auction::formats::coordinated_cutover_v1::canonicalize_browser_auction_projection_json_v1(
        config.auction_projection_json,
        publisher_origin,
    )
    .map_err(|_| boot_manifest_error("auction projection violates the version-1 contract"))?;
    let projection = ecmascript_json_stringify_v1(&projection)
        .ok_or_else(|| boot_manifest_error("auction projection ECMAScript serialization failed"))?;
    if projection.len() > MAX_BROWSER_AUCTION_PROJECTION_BYTES {
        return Err(boot_manifest_error(
            "auction projection exceeds 8 MiB after ECMAScript serialization",
        ));
    }
    let projection_value = serde_json::from_str::<BrowserAuctionProjectionV1>(&projection)
        .map_err(|_| boot_manifest_error("canonical auction projection is unavailable"))?;
    let projection_digest = hex::encode(Sha256::digest(projection.as_bytes()));
    let enabled_integrations = selected
        .iter()
        .filter_map(|metadata| match metadata.id {
            "render_runtime"
            | "diagnostics_presentation"
            | "gpt_later"
            | "osano_lifecycle"
            | "permutive_lifecycle"
            | "prebid_later"
            | "sourcepoint_lifecycle" => None,
            "osano_consent" => Some("osano"),
            "permutive_context" => Some("permutive"),
            "sourcepoint_consent" => Some("sourcepoint"),
            id => Some(id),
        })
        .collect::<Vec<_>>();
    let first_display = select_first_display_slices_v1(
        &projection_value,
        FirstDisplaySelectionConfigV1 {
            enabled_integrations: &enabled_integrations,
            creative: config.creative,
            gam_attribution_enabled: config.integration_configs.gpt_gam_attribution_enabled(),
        },
    );
    let agent = match first_display.as_ref() {
        Some(selection) => Some(
            TsjsStaticArtifactV1::new_first_display(selection.mask(), selection.slices())
                .ok_or_else(|| {
                    boot_manifest_error("first-display artifact composition is unavailable")
                })?,
        ),
        None => None,
    };
    let takeover_ids = selected
        .iter()
        .filter_map(|metadata| {
            (metadata.phase == Some(TsjsModulePhase::Takeover)).then_some(metadata.id)
        })
        .collect::<Vec<_>>();
    let runtime_src = tsjs_script_src(&takeover_ids);
    let slice_ids = first_display.as_ref().map_or_else(
        || Ok(Vec::new()),
        |selection| {
            selection
                .slices()
                .iter()
                .map(|id| {
                    serde_json::to_string(id).map_err(|_| {
                        boot_manifest_error("first-display slice serialization failed")
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        },
    )?;
    let integrations = selected
        .iter()
        .map(|metadata| {
            let id = serde_json::to_string(metadata.id)
                .map_err(|_| boot_manifest_error("integration id serialization failed"))?;
            match metadata.phase {
                Some(TsjsModulePhase::Takeover) => {
                    Ok(format!(r#"{{"id":{id},"phase":"takeover"}}"#))
                }
                Some(TsjsModulePhase::Deferred) => {
                    let trigger = metadata.trigger.ok_or_else(|| {
                        boot_manifest_error("deferred catalog trigger is unavailable")
                    })?;
                    let src = tsjs_single_module_script_src(metadata.id).ok_or_else(|| {
                        boot_manifest_error("deferred module source is unavailable")
                    })?;
                    Ok(format!(
                        r#"{{"id":{id},"phase":"deferred","trigger":{},"src":{}}}"#,
                        serde_json::to_string(trigger).map_err(|_| {
                            boot_manifest_error("deferred trigger serialization failed")
                        })?,
                        serde_json::to_string(&src).map_err(|_| {
                            boot_manifest_error("deferred source serialization failed")
                        })?
                    ))
                }
                _ => Err(boot_manifest_error(
                    "catalog integration phase is unavailable",
                )),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let first_display_manifest = agent.as_ref().map_or_else(
        || "null".to_owned(),
        |artifact| {
            format!(
                r#"{{"src":"{}","slices":[{}]}}"#,
                artifact.src(),
                slice_ids.join(",")
            )
        },
    );
    let manifest = format!(
        r#"{{"version":1,"releaseId":"{}","firstDisplay":{},"runtimeSrc":"{}","integrations":[{}]}}"#,
        release_id(),
        first_display_manifest,
        runtime_src,
        integrations.join(",")
    );
    let integration_configs_json = config.integration_configs.canonical_json_v1()?;
    let integration_config_digest =
        hex::encode(Sha256::digest(integration_configs_json.as_bytes()));
    let integrity = format!(
        r#"{{"version":1,"projectionDigest":"{}","integrationConfigDigest":"{}"}}"#,
        projection_digest, integration_config_digest
    );
    let outline = first_display.as_ref().map_or_else(
        || "null".to_owned(),
        |_| {
            format!(
                r#"{{"version":1,"releaseId":"{}","generation":1,"projectionDigest":"{}","integrationConfigDigest":"{}","slices":[{}],"slotCount":{},"outcomeCount":{},"capabilities":[],"objectKinds":[{}]}}"#,
                release_id(),
                projection_digest,
                integration_config_digest,
                slice_ids.join(","),
                projection_value.slots.len(),
                projection_value.auction.results.len(),
                if projection_value.bids.is_empty() {
                    ""
                } else {
                    r#""gpt_slot","dom_artifact""#
                },
            )
        },
    );
    let transport = format!(
        r#"{{"version":1,"boot":{{"abi":1,"releaseId":"{}","manifest":{},"auctionProjection":{},"integrations":{},"creative":{{"version":1,"enabled":{},"clickGuard":{},"renderGuard":{}}},"diagnostics":{{"version":1,"renderTraceOverlay":{},"gpt":{{"active":{}}}}}}},"integrity":{},"outline":{}}}"#,
        release_id(),
        manifest,
        projection,
        integration_configs_json,
        config.creative.enabled,
        config.creative.click_guard,
        config.creative.render_guard,
        config.render_trace_overlay,
        config.gpt_diagnostics_active,
        integrity,
        outline,
    );
    if transport.len() > SERVER_BOOT_TRANSPORT_MAX_BYTES_V1 {
        return Err(boot_manifest_error(
            "server boot transport exceeds 10 MiB UTF-8",
        ));
    }
    let transport_literal = serde_json::to_string(&transport)
        .map_err(|_| boot_manifest_error("server boot transport serialization failed"))?;
    let transport_literal = escape_json_for_inline_script(&transport_literal);
    let bootstrap = trusted_server_js::bootstrap_bundle();
    if bootstrap.to_ascii_lowercase().contains("</script") {
        return Err(boot_manifest_error(
            "generated bootstrap contains an inline-script terminator",
        ));
    }
    let controller = format!(
        "<script>const __TSJS_SERVER_BOOT_TRANSPORT_V1__={transport_literal};{bootstrap}</script>"
    );
    let selected_src = agent
        .as_ref()
        .map_or(runtime_src.as_str(), |artifact| artifact.src());
    Ok(format!(
        r#"{controller}<script src="{selected_src}" id="trustedserver-js"></script>"#
    ))
}

/// Serialize a production boot fragment with deterministic browser-safe fixture config.
///
/// This is available only to workspace integration-test tools. It exercises the
/// production manifest and immutable boot serializer without exposing the internal
/// [`IntegrationConfigsV1`] representation to downstream crates.
///
/// # Errors
///
/// Returns an error when the selected module inventory, projection, creative bits,
/// diagnostics bits, or generated fixture configuration violates the production ABI.
#[cfg(feature = "test-utils")]
pub fn tsjs_bootstrap_fixture_fragment_v1(
    module_ids: &[&str],
    auction_projection_json: &str,
    creative: CreativeBootConfigV1,
    render_trace_overlay: bool,
    gpt_diagnostics_active: bool,
    publisher_origin: &str,
) -> Result<String, Report<TrustedServerError>> {
    let entries = INTEGRATION_CONFIG_IDS_V1
        .iter()
        .copied()
        .filter(|product_id| {
            module_ids.iter().any(|module_id| {
                integration_config_product_for_module_v1(module_id) == Some(*product_id)
            })
        })
        .map(|product_id| {
            let config = match product_id {
                "didomi" => serde_json::json!({ "proxyPath": "/integrations/didomi/" }),
                "gpt" => serde_json::json!({
                    "gamAttributionEnabled": false,
                    "pageBidsEnabled": true,
                }),
                "prebid" => serde_json::json!({
                    "accountId": "fixture",
                    "timeout": 1_000,
                    "debug": false,
                    "bidders": [],
                }),
                "sourcepoint" => serde_json::json!({ "rewriteSdk": false }),
                _ => serde_json::json!({}),
            };
            (product_id, config)
        })
        .collect::<Vec<_>>();
    let integration_configs = IntegrationConfigsV1::new(entries)?;
    tsjs_bootstrap_fragment_v1(
        TsjsBootScriptConfigV1 {
            module_ids,
            integration_configs: &integration_configs,
            auction_projection_json,
            creative,
            render_trace_overlay,
            gpt_diagnostics_active,
        },
        publisher_origin,
    )
}

fn selected_metadata(
    module_ids: &[&str],
) -> Result<Vec<trusted_server_js::TsjsArtifactMetadata>, Report<TrustedServerError>> {
    if module_ids.len() > trusted_server_js::MAX_MANIFEST_MODULES {
        return Err(boot_manifest_error("more than 20 integration modules"));
    }
    let requested = module_ids.iter().copied().collect::<HashSet<_>>();
    if requested.len() != module_ids.len()
        || requested
            .iter()
            .any(|id| *id == "core" || !valid_integration_id(id))
    {
        return Err(boot_manifest_error("invalid integration inventory"));
    }

    let metadata = all_integration_metadata();
    if requested
        .iter()
        .any(|id| !metadata.iter().any(|entry| entry.id == *id))
    {
        return Err(boot_manifest_error("invalid integration inventory"));
    }
    if metadata
        .iter()
        .filter(|entry| entry.include == Some("always"))
        .any(|entry| !requested.contains(entry.id))
    {
        return Err(boot_manifest_error(
            "integration inventory omits an always-on catalog member",
        ));
    }
    Ok(metadata
        .into_iter()
        .filter(|entry| requested.contains(entry.id))
        .collect())
}

/// Exact immutable creative boot bits emitted for one document generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CreativeBootConfigV1 {
    /// Whether the creative integration is a required manifest member.
    pub enabled: bool,
    /// Whether automatic click interception activates after kernel commit.
    pub click_guard: bool,
    /// Whether automatic render interception activates after kernel commit.
    pub render_guard: bool,
}

/// Trusted server-owned inputs for selecting the closed provisional artifact.
#[derive(Clone, Copy, Debug)]
pub struct FirstDisplaySelectionConfigV1<'a> {
    /// Enabled browser integration products; never publisher supplied.
    pub enabled_integrations: &'a [&'a str],
    /// Exact server-resolved parser-time creative guard policy.
    pub creative: CreativeBootConfigV1,
    /// Whether GPT owns the document-local parser-time GAM attribution obligation.
    pub gam_attribution_enabled: bool,
}

/// Exact ordered first-display slice mask selected for one immutable projection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirstDisplaySelectionV1 {
    mask: u16,
    slices: Vec<&'static str>,
}

impl FirstDisplaySelectionV1 {
    /// Return the four-hex-digit transport mask value.
    #[must_use]
    pub const fn mask(&self) -> u16 {
        self.mask
    }

    /// Return selected slice ids in the fixed build order.
    #[must_use]
    pub fn slices(&self) -> &[&'static str] {
        &self.slices
    }
}

const FIRST_DISPLAY_KNOWN_INTEGRATIONS: &[&str] = &[
    "aps",
    "creative",
    "datadome",
    "didomi",
    "google_tag_manager",
    "gpt",
    "gpt_diagnostics",
    "lockr",
    "osano",
    "permutive",
    "prebid",
    "sourcepoint",
    "testlight",
];

/// Select the agent only for a complete, closed GPT-mediated initial projection.
///
/// An absent selection means the page must boot the persistent runtime directly.
/// PBS Cache, direct/programmatic projections, unknown obligations, incomplete
/// joins, and non-GPT configurations fail closed here.
#[must_use]
pub fn select_first_display_slices_v1(
    projection: &BrowserAuctionProjectionV1,
    config: FirstDisplaySelectionConfigV1<'_>,
) -> Option<FirstDisplaySelectionV1> {
    if projection.version != 1
        || projection.auction.version != 1
        || projection.auction.results.is_empty()
        || projection.auction.results.len() > crate::auction::types::MAX_BROWSER_AUCTION_RESULTS
        || projection.slots.len() != projection.auction.results.len()
    {
        return None;
    }

    let mut enabled = HashSet::new();
    for integration in config.enabled_integrations {
        if !FIRST_DISPLAY_KNOWN_INTEGRATIONS.contains(integration) || !enabled.insert(*integration)
        {
            return None;
        }
    }
    if !enabled.contains("gpt") {
        return None;
    }

    let mut winner_count = 0_usize;
    let mut aps_participates = false;
    let mut seen_slots = HashSet::new();
    for (index, decision) in projection.auction.results.iter().enumerate() {
        let slot = projection.slots.get(index)?;
        if slot.slot != decision.slot() || !seen_slots.insert(slot.slot.as_str()) {
            return None;
        }
        if let SlotAuctionDecisionV1::Winner { candidate_id, slot } = decision {
            let mut matching = projection
                .bids
                .iter()
                .filter(|bid| bid.candidate_id == *candidate_id && bid.slot == *slot);
            let bid = matching.next()?;
            if matching.next().is_some() {
                return None;
            }
            match &bid.render_source {
                BidRenderSourceV1::Aps(_) => {
                    if !bid.renderer_reservation_id.as_deref().is_some_and(
                        crate::auction::formats::coordinated_cutover_v1::valid_renderer_reservation_id,
                    ) {
                        return None;
                    }
                    if !enabled.contains("aps") {
                        return None;
                    }
                    aps_participates = true;
                }
                BidRenderSourceV1::Adm(_) => {
                    if !bid.renderer_reservation_id.as_deref().is_some_and(
                        crate::auction::formats::coordinated_cutover_v1::valid_renderer_reservation_id,
                    ) {
                        return None;
                    }
                }
                BidRenderSourceV1::PbsCache(_) => return None,
            }
            winner_count += 1;
        }
    }
    if winner_count != projection.bids.len() {
        return None;
    }

    let creative_guard = enabled.contains("creative")
        && config.creative.enabled
        && (config.creative.click_guard || config.creative.render_guard);
    let gpt_participates = winner_count > 0 || config.gam_attribution_enabled;
    let render_owner_participates = winner_count > 0;
    let prebid_participates =
        enabled.contains("prebid") && projection.bids.iter().any(|bid| bid.provider == "prebid");
    let mut mask = 0_u16;
    let mut slices = Vec::new();
    for (index, metadata) in trusted_server_js::all_first_display_metadata()
        .into_iter()
        .enumerate()
    {
        let selected = match metadata.include {
            Some("eligible_batch") => true,
            Some("render_owner_participates") => render_owner_participates,
            Some("aps_participates") => aps_participates,
            Some("creative_guard") => creative_guard,
            Some("gpt_initial") => gpt_participates,
            Some("prebid_participates") => prebid_participates,
            Some(predicate) => predicate
                .strip_prefix("integration:")
                .is_some_and(|integration| enabled.contains(integration)),
            None => false,
        };
        if selected {
            mask |= 1_u16 << index;
            slices.push(metadata.id);
        }
    }
    trusted_server_js::first_display_mask_is_permitted(mask)
        .then_some(FirstDisplaySelectionV1 { mask, slices })
}

impl Default for CreativeBootConfigV1 {
    fn default() -> Self {
        Self {
            enabled: true,
            click_guard: true,
            render_guard: false,
        }
    }
}

#[derive(Debug, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
struct CreativeBrowserConfigV1 {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default = "default_true")]
    click_guard: bool,
    #[serde(default)]
    render_guard: bool,
}

impl IntegrationConfig for CreativeBrowserConfigV1 {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

const fn default_true() -> bool {
    true
}

/// Resolve the exact server-owned creative browser boot policy.
pub(crate) fn creative_boot_config_v1(
    settings: &Settings,
) -> Result<CreativeBootConfigV1, Report<TrustedServerError>> {
    if !settings.integrations.contains_key("creative") {
        return Ok(CreativeBootConfigV1::default());
    }
    let Some(config) = settings.integration_config::<CreativeBrowserConfigV1>("creative")? else {
        return Ok(CreativeBootConfigV1 {
            enabled: false,
            click_guard: false,
            render_guard: false,
        });
    };
    Ok(CreativeBootConfigV1 {
        enabled: true,
        click_guard: config.click_guard,
        render_guard: config.render_guard,
    })
}

/// Inputs for the one hard-cutover browser boot transport.
#[derive(Clone, Copy, Debug)]
pub struct TsjsBootScriptConfigV1<'a> {
    /// Enabled integration bundles in their actual injection order.
    pub module_ids: &'a [&'a str],
    /// Exact browser-safe product configuration subset for this manifest.
    pub(crate) integration_configs: &'a IntegrationConfigsV1,
    /// Canonical exact [`BrowserAuctionProjectionV1`](crate::auction::types::BrowserAuctionProjectionV1)
    /// JSON produced by the auction projection boundary.
    pub auction_projection_json: &'a str,
    /// Exact creative integration boot configuration.
    pub creative: CreativeBootConfigV1,
    /// Whether the local render-trace overlay is active for this document.
    pub render_trace_overlay: bool,
    /// Whether request/session-scoped GPT diagnostics is active.
    pub gpt_diagnostics_active: bool,
}

fn escape_json_for_inline_script(json: &str) -> String {
    let mut escaped = String::with_capacity(json.len());
    for character in json.chars() {
        match character {
            '&' => escaped.push_str("\\u0026"),
            '<' => escaped.push_str("\\u003c"),
            '>' => escaped.push_str("\\u003e"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn valid_integration_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_' || *byte == b'-'
        })
}

fn boot_manifest_error(message: &str) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::Configuration {
        message: format!("TSJS boot manifest: {message}"),
    })
}

/// `/static` URL for the tsjs bundle with cache-busting hash based on
/// the concatenated content of the given module set.
#[must_use]
pub fn tsjs_script_src(module_ids: &[&str]) -> String {
    let hash = concatenated_hash(module_ids);
    format!("/static/tsjs=tsjs-unified.min.js?v={hash}")
}

/// `<script>` tag for injecting the tsjs bundle.
#[must_use]
pub fn tsjs_script_tag(module_ids: &[&str]) -> String {
    tsjs_script_tag_from_src(&tsjs_script_src(module_ids))
}

/// `<script>` tag for a precomputed exact TSJS artifact URL.
#[must_use]
pub(crate) fn tsjs_script_tag_from_src(src: &str) -> String {
    format!(r#"<script src="{src}" id="trustedserver-js"></script>"#)
}

/// Exact in-memory bytes and content identity for a unified TSJS artifact.
#[derive(Clone, Debug)]
pub(crate) struct TsjsStaticArtifactV1 {
    body: bytes::Bytes,
    hash: String,
    src: String,
}

impl TsjsStaticArtifactV1 {
    /// Build one exact content-addressed artifact from its canonical modules.
    #[must_use]
    pub(crate) fn new(module_ids: &[&str]) -> Self {
        let body = bytes::Bytes::from(trusted_server_js::concatenate_modules(module_ids));
        let hash = hex::encode(Sha256::digest(&body));
        let src = format!("/static/tsjs=tsjs-unified.min.js?v={hash}");
        Self { body, hash, src }
    }

    /// Build one exact first-display composition for a validated mask.
    #[must_use]
    pub(crate) fn new_first_display(mask: u16, selected_ids: &[&str]) -> Option<Self> {
        if selected_ids.first() != Some(&"first_display") {
            return None;
        }
        let body = bytes::Bytes::from(trusted_server_js::concatenate_first_display_slices(
            selected_ids.get(1..).unwrap_or_default(),
        )?);
        let hash = hex::encode(Sha256::digest(&body));
        let src = format!("/static/tsjs=tsjs-first-display.min.js?m={mask:04x}&v={hash}");
        Some(Self { body, hash, src })
    }

    /// Return immutable response bytes; cloning [`bytes::Bytes`] shares storage.
    #[must_use]
    pub(crate) const fn body(&self) -> &bytes::Bytes {
        &self.body
    }

    /// Return the SHA-256 identity of the exact response bytes.
    #[must_use]
    pub(crate) fn hash(&self) -> &str {
        &self.hash
    }

    /// Return the exact content-addressed static URL.
    #[must_use]
    pub(crate) fn src(&self) -> &str {
        &self.src
    }
}

const CREATIVE_TSJS_MODULE_IDS: &[&str] = &["render_runtime", "creative"];
const EMPTY_CREATIVE_AUCTION_PROJECTION_JSON: &str = r#"{"version":1,"auction":{"version":1,"auctionId":"initial","results":[]},"slots":[],"bids":[]}"#;

/// Build the complete hard-cutover bootstrap for a rewritten creative document.
///
/// Rewritten creatives are independent documents, so they need their own exact
/// boot transport and runtime artifact. The inventory is intentionally limited
/// to the always-on render runtime and creative integration; publisher-page
/// integrations and deferred presentation code do not belong in creative HTML.
///
/// # Errors
///
/// Returns an error when the creative settings or exact boot manifest are invalid.
pub(crate) fn creative_tsjs_bootstrap_v1(
    settings: &Settings,
) -> Result<Option<String>, Report<TrustedServerError>> {
    let creative = creative_boot_config_v1(settings)?;
    if !creative.enabled || (!creative.click_guard && !creative.render_guard) {
        return Ok(None);
    }
    let fragment = tsjs_bootstrap_fragment_v1(
        TsjsBootScriptConfigV1 {
            module_ids: CREATIVE_TSJS_MODULE_IDS,
            integration_configs: &IntegrationConfigsV1::empty(),
            auction_projection_json: EMPTY_CREATIVE_AUCTION_PROJECTION_JSON,
            creative,
            render_trace_overlay: false,
            gpt_diagnostics_active: false,
        },
        "https://creative.invalid",
    )?;
    Ok(Some(fragment))
}

/// Return the exact takeover module inventory admitted for rewritten creatives.
#[must_use]
pub(crate) const fn creative_tsjs_module_ids() -> &'static [&'static str] {
    CREATIVE_TSJS_MODULE_IDS
}

/// Return the process-wide exact artifact used by rewritten creative documents.
#[must_use]
pub(crate) fn creative_tsjs_static_artifact_v1() -> &'static TsjsStaticArtifactV1 {
    static ARTIFACT: OnceLock<TsjsStaticArtifactV1> = OnceLock::new();
    ARTIFACT.get_or_init(|| TsjsStaticArtifactV1::new(CREATIVE_TSJS_MODULE_IDS))
}

/// `/static` URL for the unified bundle when exact module IDs are unavailable.
///
/// This intentionally omits `?v=` because the serving path can only mark a URL
/// immutable when the hash matches the exact enabled module set. Use
/// [`tsjs_script_src`] with exact module IDs when [`IntegrationRegistry`] is
/// available.
///
/// [`IntegrationRegistry`]: crate::integrations::IntegrationRegistry
#[must_use]
pub fn tsjs_unified_script_src() -> String {
    "/static/tsjs=tsjs-unified.min.js".to_string()
}

/// `<script>` tag for the unified bundle when exact module IDs are unavailable.
///
/// See [`tsjs_unified_script_src`] for details.
#[must_use]
pub fn tsjs_unified_script_tag() -> String {
    format!(
        "<script src=\"{}\" id=\"trustedserver-js\"></script>",
        tsjs_unified_script_src()
    )
}

/// `/static` URL for one module with its own cache-busting hash.
#[must_use]
pub fn tsjs_single_module_script_src(module_id: &str) -> Option<String> {
    let metadata = trusted_server_js::integration_metadata(module_id)?;
    if metadata.phase != Some(TsjsModulePhase::Deferred) {
        return None;
    }
    let hash = single_module_hash(module_id)?;
    Some(format!("/static/tsjs=tsjs-{module_id}.min.js?v={hash}"))
}

/// `/static` URL for a single deferred module with its own cache-busting hash.
#[must_use]
pub fn tsjs_deferred_script_src(module_id: &str) -> Option<String> {
    tsjs_single_module_script_src(module_id)
}

/// `<script defer>` tag for a single deferred module.
#[must_use]
pub fn tsjs_deferred_script_tag(module_id: &str) -> Option<String> {
    tsjs_deferred_script_src(module_id).map(|src| format!("<script src=\"{src}\" defer></script>"))
}

/// Generate all deferred `<script defer>` tags for the given module IDs.
///
/// Returns an empty string when no deferred modules are present.
#[must_use]
pub fn tsjs_deferred_script_tags(module_ids: &[&str]) -> String {
    module_ids
        .iter()
        .filter_map(|id| tsjs_deferred_script_tag(id))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tests::bootstrap_transport;

    const VALID_BROWSER_AUCTION_PROJECTION_JSON: &str = r#"{"version":1,"auction":{"version":1,"auctionId":"initial","results":[]},"slots":[],"bids":[]}"#;
    const PERFORMANCE_ORIGIN: &str = "https://performance.example";

    #[test]
    fn ecmascript_json_corpus_matches_the_browser_stringify_contract() {
        let corpus: serde_json::Value = serde_json::from_str(include_str!(
            "../../trusted-server-js/lib/test/fixtures/contracts/ecmascript-json-stringify-v1.json"
        ))
        .expect("shared ECMAScript JSON corpus should parse");
        for case in corpus
            .as_array()
            .expect("shared ECMAScript JSON corpus should be an array")
        {
            let name = case["name"].as_str().expect("case should have a name");
            let input = case["input"].as_str().expect("case should have input");
            let expected = case["expected"]
                .as_str()
                .expect("case should have expected bytes");
            assert_eq!(
                ecmascript_json_stringify_v1(input).as_deref(),
                Some(expected),
                "shared case should match: {name}"
            );
        }
    }

    #[test]
    fn integration_configs_serialize_once_in_canonical_product_order() {
        let configs = IntegrationConfigsV1::new(vec![
            ("aps", serde_json::json!({})),
            ("datadome", serde_json::json!({})),
            ("didomi", serde_json::json!({ "proxyPath": "/consent/" })),
            ("google_tag_manager", serde_json::json!({})),
            (
                "gpt",
                serde_json::json!({
                    "gamAttributionEnabled": true,
                    "pageBidsEnabled": true,
                }),
            ),
            ("lockr", serde_json::json!({})),
            ("osano", serde_json::json!({})),
            ("permutive", serde_json::json!({})),
            ("prebid", serde_json::json!({ "accountId": "publisher" })),
            ("sourcepoint", serde_json::json!({ "rewriteSdk": true })),
            ("testlight", serde_json::json!({})),
        ])
        .expect("canonical browser-safe integration configs should be admitted");

        assert_eq!(
            serde_json::to_string(&configs).expect("carrier should serialize"),
            r#"{"version":1,"entries":[{"id":"aps","config":{}},{"id":"datadome","config":{}},{"id":"didomi","config":{"proxyPath":"/consent/"}},{"id":"google_tag_manager","config":{}},{"id":"gpt","config":{"gamAttributionEnabled":true,"pageBidsEnabled":true}},{"id":"lockr","config":{}},{"id":"osano","config":{}},{"id":"permutive","config":{}},{"id":"prebid","config":{"accountId":"publisher"}},{"id":"sourcepoint","config":{"rewriteSdk":true}},{"id":"testlight","config":{}}]}"#,
        );
    }

    #[test]
    fn integration_configs_reject_noncanonical_duplicate_unknown_or_non_object_entries() {
        for entries in [
            vec![
                ("datadome", serde_json::json!({})),
                ("aps", serde_json::json!({})),
            ],
            vec![
                ("aps", serde_json::json!({})),
                ("aps", serde_json::json!({})),
            ],
            vec![("unknown", serde_json::json!({}))],
            vec![("aps", serde_json::json!(null))],
        ] {
            assert!(
                IntegrationConfigsV1::new(entries).is_err(),
                "invalid integration carrier entries must fail closed"
            );
        }
    }

    #[test]
    fn integration_configs_enforce_recursive_json_and_serialized_size_caps() {
        let long_string = "a".repeat(INTEGRATION_CONFIG_MAX_STRING_BYTES_V1 + 1);
        assert!(
            IntegrationConfigsV1::new(vec![("aps", serde_json::json!({ "value": long_string }),)])
                .is_err(),
            "strings above the UTF-8 cap must be rejected"
        );

        let long_key = "k".repeat(INTEGRATION_CONFIG_MAX_STRING_BYTES_V1 + 1);
        assert!(
            IntegrationConfigsV1::new(vec![(
                "aps",
                serde_json::Value::Object(serde_json::Map::from_iter([(
                    long_key,
                    serde_json::Value::Null,
                )])),
            )])
            .is_err(),
            "keys above the UTF-8 cap must be rejected"
        );

        let mut too_deep = serde_json::Value::Null;
        for _ in 0..=INTEGRATION_CONFIG_MAX_DEPTH_V1 {
            too_deep = serde_json::json!([too_deep]);
        }
        assert!(
            IntegrationConfigsV1::new(vec![("aps", serde_json::json!({ "value": too_deep }),)])
                .is_err(),
            "values above the recursive depth cap must be rejected"
        );

        let too_many_values = vec![serde_json::Value::Null; INTEGRATION_CONFIG_MAX_VALUES_V1];
        assert!(
            IntegrationConfigsV1::new(vec![(
                "aps",
                serde_json::json!({ "values": too_many_values }),
            )])
            .is_err(),
            "the config object and array also count toward the value cap"
        );

        let oversized_entry = "x".repeat(INTEGRATION_CONFIG_MAX_ENTRY_BYTES_V1);
        assert!(
            IntegrationConfigsV1::new(vec![(
                "aps",
                serde_json::json!({ "value": oversized_entry }),
            )])
            .is_err(),
            "canonical entry JSON above the byte cap must be rejected"
        );

        let chunk = "z".repeat(INTEGRATION_CONFIG_MAX_STRING_BYTES_V1);
        let carrier_entries = INTEGRATION_CONFIG_IDS_V1
            .iter()
            .take(9)
            .map(|id| {
                (
                    *id,
                    serde_json::json!({ "chunks": vec![chunk.clone(); 15] }),
                )
            })
            .collect::<Vec<_>>();
        assert!(
            IntegrationConfigsV1::new(carrier_entries).is_err(),
            "the complete carrier byte cap must be enforced independently"
        );
    }

    #[test]
    fn integration_configs_must_match_manifest_product_predicates_exactly() {
        let aps = IntegrationConfigsV1::new(vec![("aps", serde_json::json!({}))])
            .expect("APS should admit its required empty browser projection");
        let empty = IntegrationConfigsV1::empty();

        assert!(
            aps.validate_manifest(&["render_runtime", "aps"]).is_ok(),
            "APS manifest membership must match its one product config"
        );
        assert!(
            aps.validate_manifest(&["render_runtime"]).is_err(),
            "an unselected product config must be rejected"
        );
        assert!(
            empty.validate_manifest(&["render_runtime", "aps"]).is_err(),
            "a selected product without config must be rejected"
        );

        let prebid = IntegrationConfigsV1::new(vec![("prebid", serde_json::json!({}))])
            .expect("Prebid config should be admitted");
        assert!(
            prebid
                .validate_manifest(&["render_runtime", "prebid", "prebid_later"])
                .is_ok(),
            "takeover and deferred modules must share one product config"
        );
    }

    #[test]
    fn production_bootstrap_embeds_the_carrier_and_rejects_manifest_mismatch() {
        let aps = IntegrationConfigsV1::new(vec![("aps", serde_json::json!({}))])
            .expect("APS browser projection should be admitted");
        let script = tsjs_bootstrap_fragment_v1(
            TsjsBootScriptConfigV1 {
                module_ids: &["render_runtime", "aps"],
                integration_configs: &aps,
                auction_projection_json: VALID_BROWSER_AUCTION_PROJECTION_JSON,
                creative: CreativeBootConfigV1 {
                    enabled: false,
                    click_guard: false,
                    render_guard: false,
                },
                render_trace_overlay: false,
                gpt_diagnostics_active: false,
            },
            PERFORMANCE_ORIGIN,
        )
        .expect("matching carrier and manifest should serialize");
        let transport = bootstrap_transport(&script);
        assert_eq!(
            transport
                .as_object()
                .map(|record| record.keys().cloned().collect::<Vec<_>>()),
            Some(vec![
                "boot".to_owned(),
                "integrity".to_owned(),
                "outline".to_owned(),
                "version".to_owned(),
            ]),
            "production transport must have one exact four-key root"
        );
        assert!(transport["integrity"]["projectionDigest"].is_string());
        assert!(transport["integrity"]["integrationConfigDigest"].is_string());
        assert!(transport["outline"].is_null());
        assert!(!script.contains("__TSJS_SERVER_BOOT_INPUT_V1__"));
        assert!(!script.contains(r#""target""#));
        assert!(
            transport["boot"]["integrations"]
                == serde_json::json!({"version":1,"entries":[{"id":"aps","config":{}}]}),
            "boot must contain the sole typed integration carrier: {script}"
        );

        assert!(
            tsjs_bootstrap_fragment_v1(
                TsjsBootScriptConfigV1 {
                    module_ids: &["render_runtime", "aps"],
                    integration_configs: &IntegrationConfigsV1::empty(),
                    auction_projection_json: VALID_BROWSER_AUCTION_PROJECTION_JSON,
                    creative: CreativeBootConfigV1 {
                        enabled: false,
                        click_guard: false,
                        render_guard: false,
                    },
                    render_trace_overlay: false,
                    gpt_diagnostics_active: false,
                },
                PERFORMANCE_ORIGIN,
            )
            .is_err(),
            "HTML emission must reject manifest/config mismatch"
        );
    }

    #[test]
    fn production_bootstrap_centrally_escapes_integration_config_markup() {
        let configs = IntegrationConfigsV1::new(vec![(
            "prebid",
            serde_json::json!({
                "accountId": "</ScRiPt><script>&",
                "timeout": 1_000,
                "debug": false,
                "bidders": [],
            }),
        )])
        .expect("Prebid browser projection should be admitted");
        let script = tsjs_bootstrap_fragment_v1(
            TsjsBootScriptConfigV1 {
                module_ids: &["render_runtime", "prebid"],
                integration_configs: &configs,
                auction_projection_json: VALID_BROWSER_AUCTION_PROJECTION_JSON,
                creative: CreativeBootConfigV1 {
                    enabled: false,
                    click_guard: false,
                    render_guard: false,
                },
                render_trace_overlay: false,
                gpt_diagnostics_active: false,
            },
            PERFORMANCE_ORIGIN,
        )
        .expect("valid integration config should serialize inside boot");

        assert!(!script.contains("</ScRiPt><script>&"));
        assert!(script.contains(r#"\u003c/ScRiPt\u003e\u003cscript\u003e\u0026"#));
    }

    fn projection_with_adm(adm: &str) -> String {
        use crate::auction::types::{
            AdmRenderSourceV1, AuctionDecisionSetV1, BidRenderSourceV1, BrowserAuctionBidV1,
            BrowserAuctionProjectionV1, BrowserAuctionSlotV1, SlotAuctionDecisionV1,
        };

        serde_json::to_string(&BrowserAuctionProjectionV1 {
            version: 1,
            auction: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "initial".to_string(),
                results: vec![SlotAuctionDecisionV1::Winner {
                    slot: "slot-1".to_string(),
                    candidate_id: "AAAAAAAAAAAA".to_string(),
                }],
            },
            slots: vec![BrowserAuctionSlotV1 {
                slot: "slot-1".to_string(),
                gam_unit_path: "/123/slot-1".to_string(),
                div_id: "slot-1".to_string(),
                formats: vec![[300, 250]],
                targeting: Default::default(),
            }],
            bids: vec![BrowserAuctionBidV1 {
                candidate_id: "AAAAAAAAAAAA".to_string(),
                slot: "slot-1".to_string(),
                provider: "test".to_string(),
                upstream_bid_id: "bid-1".to_string(),
                cpm: 1.0,
                currency: "USD".to_string(),
                targeting: Default::default(),
                renderer_reservation_id: Some("r1_aaaaaaaaaaaaaaaaaaaaaa".to_string()),
                render_source: BidRenderSourceV1::Adm(AdmRenderSourceV1 {
                    version: 1,
                    adm: adm.to_string(),
                    width: 300,
                    height: 250,
                }),
            }],
        })
        .expect("should serialize a canonical ADM projection")
    }

    #[test]
    fn first_display_outline_binds_the_canonical_integration_config_digest() {
        let configs = IntegrationConfigsV1::new(vec![(
            "gpt",
            serde_json::json!({
                "gamAttributionEnabled": false,
                "pageBidsEnabled": false,
            }),
        )])
        .expect("GPT browser config should be admitted");
        let canonical = serde_json::to_string(&configs).expect("carrier should serialize");
        let expected_digest = hex::encode(Sha256::digest(canonical.as_bytes()));
        let script = tsjs_bootstrap_fragment_v1(
            TsjsBootScriptConfigV1 {
                module_ids: &["render_runtime", "gpt"],
                integration_configs: &configs,
                auction_projection_json: &projection_with_adm("<main>ad</main>"),
                creative: CreativeBootConfigV1 {
                    enabled: false,
                    click_guard: false,
                    render_guard: false,
                },
                render_trace_overlay: false,
                gpt_diagnostics_active: false,
            },
            "https://publisher.example",
        )
        .expect("first-display bootstrap should serialize");

        assert!(
            bootstrap_transport(&script)["integrity"]["integrationConfigDigest"].as_str()
                == Some(expected_digest.as_str()),
            "outline must bind the exact canonical carrier: {script}"
        );
        assert_eq!(
            bootstrap_transport(&script)["outline"]["integrationConfigDigest"],
            expected_digest,
            "outline must copy the always-present integrity value"
        );
    }

    #[test]
    fn production_bootstrap_selects_gpt_initial_for_attribution_without_a_winner() {
        let configs = IntegrationConfigsV1::new(vec![(
            "gpt",
            serde_json::json!({
                "gamAttributionEnabled": true,
                "pageBidsEnabled": false,
            }),
        )])
        .expect("GPT browser config should be admitted");
        let projection = serde_json::to_string(&first_display_projection(None))
            .expect("no-bid projection should serialize");

        let script = tsjs_bootstrap_fragment_v1(
            TsjsBootScriptConfigV1 {
                module_ids: &["render_runtime", "gpt"],
                integration_configs: &configs,
                auction_projection_json: &projection,
                creative: CreativeBootConfigV1 {
                    enabled: false,
                    click_guard: false,
                    render_guard: false,
                },
                render_trace_overlay: false,
                gpt_diagnostics_active: false,
            },
            "https://publisher.example",
        )
        .expect("attribution-only first-display bootstrap should serialize");

        assert!(
            bootstrap_transport(&script)["boot"]["manifest"]["firstDisplay"]["slices"]
                == serde_json::json!(["first_display", "gpt_initial"]),
            "typed GAM attribution must select the GPT parser-time owner: {script}"
        );
        assert!(
            script.contains("m=0081"),
            "attribution-only first display must use the admitted GPT mask: {script}"
        );
    }

    fn hash_query_value(src: &str) -> &str {
        src.split_once("?v=")
            .map(|(_, hash)| hash)
            .expect("should contain cache-busting hash query")
    }

    fn assert_sha256_hex_hash(value: &str) {
        assert_eq!(value.len(), 64, "should be a SHA-256 hex digest");
        assert!(
            value.chars().all(|ch| ch.is_ascii_hexdigit()),
            "should contain only ASCII hex digits"
        );
    }

    #[test]
    fn release_id_is_shared_by_generated_metadata_and_every_bundle() {
        let release = release_id();

        assert_eq!(release.len(), 64, "should be one SHA-256 release id");
        assert!(
            release
                .chars()
                .all(|character| character.is_ascii_digit() || ('a'..='f').contains(&character)),
            "should use lowercase hexadecimal"
        );
        for id in trusted_server_js::all_module_ids() {
            let bundle = trusted_server_js::module_bundle(id).expect("should include known module");
            assert_eq!(
                bundle.matches(release).count(),
                1,
                "module {id} should carry the shared release id exactly once"
            );
        }
    }

    #[test]
    fn production_bootstrap_keeps_deferred_modules_out_of_html_script_tags() {
        let controller = tsjs_bootstrap_fragment_v1(
            TsjsBootScriptConfigV1 {
                module_ids: &[
                    "render_runtime",
                    "gpt",
                    "diagnostics_presentation",
                    "gpt_later",
                ],
                integration_configs: &IntegrationConfigsV1::new(vec![(
                    "gpt",
                    serde_json::json!({
                        "gamAttributionEnabled": false,
                        "pageBidsEnabled": false,
                    }),
                )])
                .expect("GPT browser config should be admitted"),
                auction_projection_json: VALID_BROWSER_AUCTION_PROJECTION_JSON,
                creative: CreativeBootConfigV1 {
                    enabled: false,
                    click_guard: false,
                    render_guard: false,
                },
                render_trace_overlay: true,
                gpt_diagnostics_active: false,
            },
            PERFORMANCE_ORIGIN,
        )
        .expect("should serialize the production bootstrap");

        assert_eq!(
            bootstrap_transport(&controller)["boot"]["manifest"]["runtimeSrc"],
            format!(
                "/static/tsjs=tsjs-unified.min.js?v={}",
                concatenated_hash(&["render_runtime", "gpt"])
            ),
            "should carry the exact runtime artifact source in BootManifestV1"
        );
        assert!(
            controller.ends_with(&format!(
                r#"<script src="/static/tsjs=tsjs-unified.min.js?v={}" id="trustedserver-js"></script>"#,
                concatenated_hash(&["render_runtime", "gpt"])
            )),
            "should emit exactly one runtime script tag"
        );
        assert!(
            controller.matches("<script").count() == 2
                && !controller
                    .contains(r#"<script src="/static/tsjs=tsjs-diagnostics_presentation"#)
                && !controller.contains(r#"<script src="/static/tsjs=tsjs-gpt_later"#),
            "deferred modules must be present only in the manifest, not parser-time tags"
        );
    }

    #[test]
    fn production_bootstrap_rejects_diagnostics_mismatch_and_escapes_projection_markup() {
        let mismatched = tsjs_bootstrap_fragment_v1(
            TsjsBootScriptConfigV1 {
                module_ids: &["render_runtime", "creative"],
                integration_configs: &IntegrationConfigsV1::empty(),
                auction_projection_json: r#"{"version":1,"auction":{"version":1,"auctionId":"initial","results":[]},"slots":[],"bids":[]}"#,
                creative: CreativeBootConfigV1 {
                    enabled: true,
                    click_guard: true,
                    render_guard: false,
                },
                render_trace_overlay: false,
                gpt_diagnostics_active: true,
            },
            "https://publisher.example",
        );
        assert!(
            mismatched.is_err(),
            "should reject an active diagnostics bit without its module"
        );

        let projection = projection_with_adm("</ScRiPt><script>&");
        let script = tsjs_bootstrap_fragment_v1(
            TsjsBootScriptConfigV1 {
                module_ids: &["render_runtime", "creative"],
                integration_configs: &IntegrationConfigsV1::empty(),
                auction_projection_json: &projection,
                creative: CreativeBootConfigV1 {
                    enabled: true,
                    click_guard: true,
                    render_guard: false,
                },
                render_trace_overlay: false,
                gpt_diagnostics_active: false,
            },
            "https://publisher.example",
        )
        .expect("should escape valid projection JSON");
        assert!(!script.contains("</ScRiPt><script>&"));
        assert!(script.contains(r#"\u003c/ScRiPt\u003e\u003cscript\u003e\u0026"#));
    }

    #[test]
    fn production_boot_rejects_a_noncanonical_browser_projection() {
        let script = tsjs_bootstrap_fragment_v1(
            TsjsBootScriptConfigV1 {
                module_ids: &["render_runtime"],
                integration_configs: &IntegrationConfigsV1::empty(),
                auction_projection_json: r#"{"version":1,"auction":{"version":1,"auctionId":"initial","results":[]},"slots":[],"bids":[],"unexpected":true}"#,
                creative: CreativeBootConfigV1 {
                    enabled: false,
                    click_guard: false,
                    render_guard: false,
                },
                render_trace_overlay: false,
                gpt_diagnostics_active: false,
            },
            "https://publisher.example",
        );

        assert!(
            script.is_err(),
            "production boot must enforce the same canonical projection contract as fixtures"
        );
    }

    #[test]
    fn inline_script_json_escaping_is_byte_exact_for_every_guarded_character() {
        assert_eq!(
            escape_json_for_inline_script("plain &<>\u{2028}\u{2029} tail"),
            r"plain \u0026\u003c\u003e\u2028\u2029 tail",
            "should preserve plain UTF-8 while escaping every script-significant character"
        );
    }

    #[test]
    fn tsjs_script_src_formats_unified_bundle_url_with_hash() {
        let src = tsjs_script_src(&["creative"]);

        assert!(
            src.starts_with("/static/tsjs=tsjs-unified.min.js?v="),
            "should use unified static bundle path"
        );
        assert_sha256_hex_hash(hash_query_value(&src));
    }

    #[test]
    fn tsjs_script_src_empty_module_list_matches_core_only_bundle() {
        let empty_src = tsjs_script_src(&[]);

        assert!(
            empty_src.starts_with("/static/tsjs=tsjs-unified.min.js?v="),
            "should use unified static bundle path"
        );
        assert_sha256_hex_hash(hash_query_value(&empty_src));
        assert_eq!(
            empty_src,
            tsjs_script_src(&["core"]),
            "should include core exactly once for an empty module list"
        );
    }

    #[test]
    fn tsjs_script_src_hash_changes_with_module_set() {
        let creative_src = tsjs_script_src(&["creative"]);
        let creative_datadome_src = tsjs_script_src(&["creative", "datadome"]);

        assert_ne!(
            creative_src, creative_datadome_src,
            "should include requested modules in cache-busting hash"
        );
    }

    #[test]
    fn tsjs_script_src_hash_depends_on_module_order() {
        assert_ne!(
            tsjs_script_src(&["creative", "datadome"]),
            tsjs_script_src(&["datadome", "creative"]),
            "should include module order in cache-busting hash"
        );
    }

    #[test]
    fn tsjs_script_src_deduplicates_core_module() {
        assert_eq!(
            tsjs_script_src(&["core", "datadome"]),
            tsjs_script_src(&["datadome"]),
            "should not hash core twice when requested explicitly"
        );
    }

    #[test]
    fn tsjs_script_src_is_stable_for_identical_module_ids() {
        let module_ids = ["core", "lockr", "permutive_context"];
        let src = tsjs_script_src(&module_ids);

        assert_sha256_hex_hash(hash_query_value(&src));
        assert_eq!(
            src,
            tsjs_script_src(&module_ids),
            "should produce a stable URL for identical module IDs"
        );
    }

    #[test]
    fn tsjs_script_tag_wraps_source_in_single_trustedserver_tag() {
        let module_ids = ["creative"];
        let src = tsjs_script_src(&module_ids);

        assert_eq!(
            tsjs_script_tag(&module_ids),
            format!("<script src=\"{src}\" id=\"trustedserver-js\"></script>"),
            "should generate exactly one trusted server script tag"
        );
    }

    #[test]
    fn creative_bootstrap_uses_only_the_required_takeover_modules() {
        let settings = crate::test_support::tests::create_test_settings();
        let bootstrap = creative_tsjs_bootstrap_v1(&settings)
            .expect("should build the creative bootstrap")
            .expect("the default creative integration should be enabled");
        let expected_src = tsjs_script_src(&["render_runtime", "creative"]);

        assert!(bootstrap.contains("__TSJS_SERVER_BOOT_TRANSPORT_V1__"));
        assert!(!bootstrap.contains("__TSJS_SERVER_BOOT_INPUT_V1__"));
        let transport = bootstrap_transport(&bootstrap);
        assert_eq!(
            transport["boot"]["manifest"]["integrations"],
            serde_json::json!([
                {"id":"render_runtime","phase":"takeover"},
                {"id":"creative","phase":"takeover"}
            ]),
            "{bootstrap}"
        );
        assert!(
            bootstrap.contains(&format!(r#"src="{expected_src}""#)),
            "{bootstrap}"
        );
        assert_eq!(bootstrap.matches("id=\"trustedserver-js\"").count(), 1);
        assert!(
            transport["boot"]["manifest"]["integrations"]
                .as_array()
                .is_some_and(|entries| entries.iter().all(|entry| entry["id"] != "gpt"))
        );
        assert!(
            transport["boot"]["manifest"]["integrations"]
                .as_array()
                .is_some_and(|entries| entries.iter().all(|entry| entry["phase"] != "deferred"))
        );
    }

    #[test]
    fn creative_bootstrap_is_absent_when_both_guards_are_disabled() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings
            .integrations
            .insert_config(
                "creative",
                &serde_json::json!({
                    "enabled": true,
                    "click_guard": false,
                    "render_guard": false,
                }),
            )
            .expect("should configure creative browser policy");

        assert_eq!(
            creative_tsjs_bootstrap_v1(&settings)
                .expect("should resolve the creative bootstrap policy"),
            None,
            "zero browser work must not inject a runtime into creative HTML"
        );
    }

    #[test]
    fn production_bootstrap_accepts_enabled_creative_with_no_guards_and_no_module() {
        assert!(
            tsjs_bootstrap_fragment_v1(
                TsjsBootScriptConfigV1 {
                    module_ids: &["render_runtime"],
                    integration_configs: &IntegrationConfigsV1::empty(),
                    auction_projection_json: EMPTY_CREATIVE_AUCTION_PROJECTION_JSON,
                    creative: CreativeBootConfigV1 {
                        enabled: true,
                        click_guard: false,
                        render_guard: false,
                    },
                    render_trace_overlay: false,
                    gpt_diagnostics_active: false,
                },
                "https://publisher.example"
            )
            .is_ok(),
            "creative membership is required only when a guard has browser work"
        );
    }

    #[test]
    fn tsjs_unified_helpers_use_unversioned_fallback_without_registry() {
        let src = tsjs_unified_script_src();

        assert_eq!(
            src, "/static/tsjs=tsjs-unified.min.js",
            "registry-free unified helper should not emit an unverifiable hash"
        );
        assert_eq!(
            tsjs_unified_script_tag(),
            format!(r#"<script src="{src}" id="trustedserver-js"></script>"#),
            "should wrap the registry-free unified source"
        );
    }

    #[test]
    fn tsjs_single_module_script_src_formats_known_module_url_with_hash() {
        let src =
            tsjs_single_module_script_src("prebid_later").expect("should name a deferred module");

        assert!(
            src.starts_with("/static/tsjs=tsjs-prebid_later.min.js?v="),
            "should use per-module static bundle path"
        );
        assert_sha256_hex_hash(hash_query_value(&src));
        assert_eq!(tsjs_single_module_script_src("creative"), None);
    }

    #[test]
    fn tsjs_deferred_script_src_hashes_only_catalogued_deferred_modules() {
        let prebid_src =
            tsjs_deferred_script_src("prebid_later").expect("should name Prebid later slice");
        assert!(
            prebid_src.starts_with("/static/tsjs=tsjs-prebid_later.min.js?v="),
            "Prebid later slice should use the deferred TSJS route"
        );
        assert_sha256_hex_hash(hash_query_value(&prebid_src));
        assert_eq!(tsjs_deferred_script_src("unknown-module"), None);
        assert_eq!(tsjs_deferred_script_src("prebid"), None);
    }

    #[test]
    fn tsjs_deferred_script_tag_marks_script_defer() {
        let src = tsjs_deferred_script_src("prebid_later").expect("should name Prebid later slice");

        assert_eq!(
            tsjs_deferred_script_tag("prebid_later"),
            Some(format!("<script src=\"{src}\" defer></script>")),
            "should generate a deferred script tag"
        );
    }

    #[test]
    fn tsjs_deferred_script_tags_returns_empty_for_empty_input() {
        assert_eq!(
            tsjs_deferred_script_tags(&[]),
            "",
            "should not emit tags when no deferred modules exist"
        );
    }

    #[test]
    fn tsjs_deferred_script_tags_preserves_input_order() {
        assert_eq!(
            tsjs_deferred_script_tags(&["prebid_later", "sourcepoint_lifecycle"]),
            format!(
                "{}{}",
                tsjs_deferred_script_tag("prebid_later").expect("should build Prebid tag"),
                tsjs_deferred_script_tag("sourcepoint_lifecycle")
                    .expect("should build Sourcepoint tag")
            ),
            "should preserve caller-provided deferred module order"
        );
    }

    #[test]
    fn tsjs_unified_script_src_and_tag_omit_unverifiable_cache_busting_hash() {
        let src = tsjs_unified_script_src();

        assert_eq!(
            src, "/static/tsjs=tsjs-unified.min.js",
            "should use the unified script URL without an unverifiable hash"
        );
        assert_eq!(
            tsjs_unified_script_tag(),
            format!(r#"<script src="{src}" id="trustedserver-js"></script>"#),
            "should wrap the unified source in a trusted server script tag"
        );
    }

    #[test]
    fn tsjs_script_src_differs_for_different_module_sets() {
        assert_ne!(
            tsjs_script_src(&["lockr"]),
            tsjs_script_src(&["lockr", "permutive_context"]),
            "should bust the cache when the module set content changes"
        );
    }

    #[test]
    fn tsjs_deferred_script_src_rejects_unknown_module() {
        assert_eq!(tsjs_deferred_script_src("does-not-exist"), None);
    }

    fn first_display_projection(source: Option<BidRenderSourceV1>) -> BrowserAuctionProjectionV1 {
        use crate::auction::types::{
            AuctionDecisionSetV1, BrowserAuctionBidV1, BrowserAuctionSlotV1, SlotAuctionDecisionV1,
        };

        let slot = BrowserAuctionSlotV1 {
            slot: "slot-1".to_string(),
            gam_unit_path: "/123/home".to_string(),
            div_id: "div-1".to_string(),
            formats: vec![[300, 250]],
            targeting: std::collections::BTreeMap::new(),
        };
        let (results, bids) = source.map_or_else(
            || {
                (
                    vec![SlotAuctionDecisionV1::NoBid {
                        slot: "slot-1".to_string(),
                    }],
                    vec![],
                )
            },
            |render_source| {
                (
                    vec![SlotAuctionDecisionV1::Winner {
                        slot: "slot-1".to_string(),
                        candidate_id: "candidate-1".to_string(),
                    }],
                    vec![BrowserAuctionBidV1 {
                        candidate_id: "candidate-1".to_string(),
                        slot: "slot-1".to_string(),
                        provider: "prebid".to_string(),
                        upstream_bid_id: "bid-1".to_string(),
                        cpm: 1.0,
                        currency: "USD".to_string(),
                        targeting: std::collections::BTreeMap::new(),
                        renderer_reservation_id: match render_source {
                            BidRenderSourceV1::Aps(_) | BidRenderSourceV1::Adm(_) => {
                                Some("r1_aaaaaaaaaaaaaaaaaaaaaa".to_string())
                            }
                            BidRenderSourceV1::PbsCache(_) => None,
                        },
                        render_source,
                    }],
                )
            },
        );
        BrowserAuctionProjectionV1 {
            version: 1,
            auction: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "initial".to_string(),
                results,
            },
            slots: vec![slot],
            bids,
        }
    }

    fn adm_source() -> BidRenderSourceV1 {
        use crate::auction::types::AdmRenderSourceV1;
        BidRenderSourceV1::Adm(AdmRenderSourceV1 {
            version: 1,
            adm: "<div>creative</div>".to_string(),
            width: 300,
            height: 250,
        })
    }

    fn aps_source() -> BidRenderSourceV1 {
        use crate::auction::types::{ApsRendererV1, ApsTagType};
        BidRenderSourceV1::Aps(ApsRendererV1 {
            version: 1,
            account_id: "account-1".to_string(),
            bid_id: "bid-1".to_string(),
            creative_id: None,
            tag_type: ApsTagType::Iframe,
            creative_url: "https://creative.example/render".to_string(),
            aax_response: "envelope".to_string(),
            width: 300,
            height: 250,
        })
    }

    fn cache_source() -> BidRenderSourceV1 {
        use crate::auction::types::BaselinePbsCacheSourceV1;
        BidRenderSourceV1::PbsCache(BaselinePbsCacheSourceV1 {
            version: 1,
            cache_id: "cache-1".to_string(),
            cache_host: "cache.example".to_string(),
            cache_path: "/cache".to_string(),
            width: 300,
            height: 250,
        })
    }

    #[test]
    fn first_display_selection_admits_only_the_closed_projected_batch() {
        let no_bid = first_display_projection(None);
        let adm = first_display_projection(Some(adm_source()));
        let enabled = ["gpt"];

        let no_bid_selected = select_first_display_slices_v1(
            &no_bid,
            FirstDisplaySelectionConfigV1 {
                enabled_integrations: &enabled,
                creative: CreativeBootConfigV1::default(),
                gam_attribution_enabled: false,
            },
        )
        .expect("closed no-bid batch should select the base agent");
        assert_eq!(no_bid_selected.mask(), 0x0001);
        assert_eq!(no_bid_selected.slices(), &["first_display"]);

        let adm_selected = select_first_display_slices_v1(
            &adm,
            FirstDisplaySelectionConfigV1 {
                enabled_integrations: &enabled,
                creative: CreativeBootConfigV1::default(),
                gam_attribution_enabled: false,
            },
        )
        .expect("closed GPT ADM batch should select GPT initial ownership");
        assert_eq!(adm_selected.mask(), 0x0083);
        assert_eq!(
            adm_selected.slices(),
            &["first_display", "render_owner_initial", "gpt_initial"]
        );

        for invalid in [None, Some("not-a-reservation".to_string())] {
            let mut malformed = adm.clone();
            malformed.bids[0].renderer_reservation_id = invalid;
            assert!(
                select_first_display_slices_v1(
                    &malformed,
                    FirstDisplaySelectionConfigV1 {
                        enabled_integrations: &enabled,
                        creative: CreativeBootConfigV1::default(),
                        gam_attribution_enabled: false,
                    },
                )
                .is_none(),
                "a rendering mask must require one exact reservation capability"
            );
        }

        assert!(
            select_first_display_slices_v1(
                &first_display_projection(Some(cache_source())),
                FirstDisplaySelectionConfigV1 {
                    enabled_integrations: &enabled,
                    creative: CreativeBootConfigV1::default(),
                    gam_attribution_enabled: false,
                },
            )
            .is_none(),
            "PBS Cache must stay on the direct persistent GPT path"
        );
        let mut direct = no_bid.clone();
        direct.slots.clear();
        assert!(
            select_first_display_slices_v1(
                &direct,
                FirstDisplaySelectionConfigV1 {
                    enabled_integrations: &enabled,
                    creative: CreativeBootConfigV1::default(),
                    gam_attribution_enabled: false,
                },
            )
            .is_none(),
            "direct/programmatic projection must not select the agent"
        );
    }

    #[test]
    fn first_display_selection_derives_exact_permitted_slices_from_trusted_configuration() {
        let creative_selected = select_first_display_slices_v1(
            &first_display_projection(Some(adm_source())),
            FirstDisplaySelectionConfigV1 {
                enabled_integrations: &["creative", "gpt"],
                creative: CreativeBootConfigV1 {
                    enabled: true,
                    click_guard: false,
                    render_guard: true,
                },
                gam_attribution_enabled: false,
            },
        )
        .expect("bounded creative/GPT configuration should select the agent");

        assert_eq!(
            creative_selected.slices(),
            &[
                "first_display",
                "render_owner_initial",
                "creative_initial",
                "gpt_initial",
            ]
        );
        assert_eq!(creative_selected.mask(), 0x008b);

        let aps_selected = select_first_display_slices_v1(
            &first_display_projection(Some(aps_source())),
            FirstDisplaySelectionConfigV1 {
                enabled_integrations: &["aps", "gpt"],
                creative: CreativeBootConfigV1::default(),
                gam_attribution_enabled: false,
            },
        )
        .expect("bounded APS/GPT configuration should select the agent");
        assert_eq!(
            aps_selected.slices(),
            &[
                "first_display",
                "render_owner_initial",
                "aps_initial",
                "gpt_initial",
            ]
        );
        assert_eq!(aps_selected.mask(), 0x0087);

        let aps_creative_selected = select_first_display_slices_v1(
            &first_display_projection(Some(aps_source())),
            FirstDisplaySelectionConfigV1 {
                enabled_integrations: &["aps", "creative", "gpt"],
                creative: CreativeBootConfigV1 {
                    enabled: true,
                    click_guard: true,
                    render_guard: false,
                },
                gam_attribution_enabled: false,
            },
        )
        .expect("bounded APS/creative/GPT configuration should select the agent");
        assert_eq!(aps_creative_selected.mask(), 0x008f);

        let mut prebid_projection = first_display_projection(Some(adm_source()));
        prebid_projection.bids[0].provider = "prebid".to_owned();
        let prebid_selected = select_first_display_slices_v1(
            &prebid_projection,
            FirstDisplaySelectionConfigV1 {
                enabled_integrations: &["gpt", "prebid"],
                creative: CreativeBootConfigV1::default(),
                gam_attribution_enabled: false,
            },
        )
        .expect("bounded Prebid/GPT configuration should select the agent");
        assert_eq!(
            prebid_selected.slices(),
            &[
                "first_display",
                "render_owner_initial",
                "gpt_initial",
                "prebid_initial",
            ]
        );
        assert_eq!(prebid_selected.mask(), 0x1083);
    }

    #[test]
    fn first_display_selection_obeys_generated_size_admission() {
        let enabled = [
            "aps",
            "creative",
            "datadome",
            "didomi",
            "google_tag_manager",
            "gpt",
            "lockr",
            "osano",
            "permutive",
            "prebid",
            "sourcepoint",
            "testlight",
        ];
        let mut projection = first_display_projection(Some(adm_source()));
        projection.bids[0].provider = "prebid".to_owned();
        assert!(
            select_first_display_slices_v1(
                &projection,
                FirstDisplaySelectionConfigV1 {
                    enabled_integrations: &enabled,
                    creative: CreativeBootConfigV1 {
                        enabled: true,
                        click_guard: false,
                        render_guard: true,
                    },
                    gam_attribution_enabled: false,
                },
            )
            .is_none(),
            "an oversized optional composition must fall through to persistent boot"
        );
        let selected = select_first_display_slices_v1(
            &projection,
            FirstDisplaySelectionConfigV1 {
                enabled_integrations: &["creative", "gpt", "prebid"],
                creative: CreativeBootConfigV1 {
                    enabled: true,
                    click_guard: false,
                    render_guard: true,
                },
                gam_attribution_enabled: false,
            },
        )
        .expect("the bounded owner/creative/GPT/Prebid composition should fit");
        assert!(trusted_server_js::first_display_mask_is_permitted(
            selected.mask()
        ));

        let unknown = ["gpt", "publisher_supplied"];
        assert!(
            select_first_display_slices_v1(
                &first_display_projection(None),
                FirstDisplaySelectionConfigV1 {
                    enabled_integrations: &unknown,
                    creative: CreativeBootConfigV1::default(),
                    gam_attribution_enabled: false,
                },
            )
            .is_none(),
            "an unknown obligation must force direct persistent boot"
        );
    }
}
