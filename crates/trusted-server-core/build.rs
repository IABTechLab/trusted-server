// Build script includes source modules (`error`, `auction_config_types`, etc.)
// for compile-time config validation. Not all items from those modules are used
// in the build context, so `dead_code` is expected.
#![allow(clippy::unwrap_used, clippy::panic, dead_code)]

#[path = "src/error.rs"]
mod error;

#[path = "src/auction_config_types.rs"]
mod auction_config_types;

#[path = "src/redacted.rs"]
mod redacted;

#[path = "src/consent_config.rs"]
mod consent_config;

#[path = "src/price_bucket.rs"]
mod price_bucket;

// Build-script mirror of the creative opportunity config types. The runtime
// module also contains auction conversion helpers that depend on runtime-only
// auction/Fastly types, so build.rs keeps this narrow schema local.
mod creative_opportunities {
    use std::collections::HashMap;

    use serde::{Deserialize, Serialize};

    use crate::price_bucket::PriceGranularity;

    #[derive(Debug, Clone, Deserialize, Serialize)]
    pub struct CreativeOpportunitiesConfig {
        pub gam_network_id: String,
        #[serde(default)]
        pub auction_timeout_ms: Option<u32>,
        #[serde(default)]
        pub price_granularity: PriceGranularity,
    }

    #[derive(Debug, Clone, Default, Deserialize)]
    pub struct CreativeOpportunitiesFile {
        #[serde(rename = "slot", default)]
        pub slots: Vec<CreativeOpportunitySlot>,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct CreativeOpportunitySlot {
        pub id: String,
        pub gam_unit_path: Option<String>,
        pub div_id: Option<String>,
        pub page_patterns: Vec<String>,
        pub formats: Vec<CreativeOpportunityFormat>,
        pub floor_price: Option<f64>,
        #[serde(default)]
        pub targeting: HashMap<String, String>,
        #[serde(default)]
        pub providers: SlotProviders,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct CreativeOpportunityFormat {
        pub width: u32,
        pub height: u32,
        #[serde(default)]
        pub media_type: MediaType,
    }

    #[derive(Debug, Clone, Default, Deserialize)]
    pub struct SlotProviders {
        pub aps: Option<ApsSlotParams>,
    }

    #[derive(Debug, Clone, Deserialize)]
    pub struct ApsSlotParams {
        pub slot_id: String,
    }

    #[derive(Debug, Clone, Default, Deserialize)]
    #[serde(rename_all = "lowercase")]
    pub enum MediaType {
        #[default]
        Banner,
        Video,
        Native,
    }
}

#[path = "src/settings.rs"]
mod settings;

use std::fs;
use std::path::Path;

const TRUSTED_SERVER_INIT_CONFIG_PATH: &str = "../../trusted-server.toml";
const TRUSTED_SERVER_OUTPUT_CONFIG_PATH: &str = "../../target/trusted-server-out.toml";
const CREATIVE_OPPORTUNITIES_CONFIG_PATH: &str = "../../creative-opportunities.toml";

fn main() {
    // Always rerun build.rs: integration settings are stored in a flat
    // HashMap<String, JsonValue>, so we cannot enumerate all possible env
    // var keys ahead of time. Emitting rerun-if-changed for a nonexistent
    // file forces cargo to always rerun the build script.
    println!("cargo:rerun-if-changed=_always_rebuild_sentinel_");
    println!("cargo:rerun-if-changed={CREATIVE_OPPORTUNITIES_CONFIG_PATH}");

    validate_creative_opportunities_config();

    // Read init config
    let init_config_path = Path::new(TRUSTED_SERVER_INIT_CONFIG_PATH);
    let toml_content = fs::read_to_string(init_config_path)
        .unwrap_or_else(|_| panic!("Failed to read {init_config_path:?}"));

    // Merge base TOML with environment variable overrides and write output.
    // Panics if admin endpoints are not covered by a handler.
    let settings = settings::Settings::from_toml_and_env(&toml_content)
        .expect("Failed to parse settings at build time");

    let merged_toml =
        toml::to_string_pretty(&settings).expect("Failed to serialize settings to TOML");

    // Only write when content changes to avoid unnecessary recompilation.
    let dest_path = Path::new(TRUSTED_SERVER_OUTPUT_CONFIG_PATH);
    let current = fs::read_to_string(dest_path).unwrap_or_default();
    if current != merged_toml {
        fs::write(dest_path, merged_toml)
            .unwrap_or_else(|_| panic!("Failed to write {dest_path:?}"));
    }
}

fn validate_creative_opportunities_config() {
    let config_path = Path::new(CREATIVE_OPPORTUNITIES_CONFIG_PATH);
    let toml_content = fs::read_to_string(config_path)
        .unwrap_or_else(|_| panic!("Failed to read {config_path:?}"));

    let parsed = toml::from_str::<toml::Value>(&toml_content)
        .unwrap_or_else(|err| panic!("Failed to parse {config_path:?}: {err}"));
    let file = toml::from_str::<creative_opportunities::CreativeOpportunitiesFile>(&toml_content)
        .unwrap_or_else(|err| {
            panic!("Invalid creative opportunity schema in {config_path:?}: {err}")
        });

    let Some(slots) = parsed.get("slot") else {
        return;
    };

    let slots = slots
        .as_array()
        .unwrap_or_else(|| panic!("{config_path:?}: `slot` must be an array of tables"));

    let slot_id_regex =
        regex::Regex::new(r"^[A-Za-z0-9_-]+$").expect("should compile slot ID validation regex");

    for (slot, typed_slot) in slots.iter().zip(&file.slots) {
        let id = slot
            .get("id")
            .and_then(toml::Value::as_str)
            .unwrap_or_else(|| panic!("{config_path:?}: every [[slot]] must include string `id`"));

        if id != typed_slot.id {
            panic!("{config_path:?}: slot ID validation schema mismatch for `{id}`");
        }

        if !slot_id_regex.is_match(id) {
            panic!(
                "{config_path:?}: invalid slot id `{id}`; use only ASCII letters, digits, `_`, and `-`"
            );
        }
    }
}
