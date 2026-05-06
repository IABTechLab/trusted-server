// Build script includes source modules (`error`, `auction_config_types`, etc.)
// for compile-time config validation. Not all items from those modules are used
// in the build context, so `dead_code` is expected.
#![allow(clippy::unwrap_used, clippy::panic, dead_code)]

// Stub out dependencies for build.rs context
mod glob {
    pub struct Pattern;
    impl Pattern {
        pub fn new(_: &str) -> Result<Self, String> {
            Ok(Pattern)
        }
        pub fn matches(&self, _: &str) -> bool {
            false
        }
    }
}

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

// CreativeOpportunitiesConfig for build.rs deserialization only
mod creative_opportunities {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Deserialize, Serialize)]
    pub struct CreativeOpportunitiesConfig {
        pub gam_network_id: String,
        #[serde(default)]
        pub auction_timeout_ms: Option<u32>,
        #[serde(default = "default_price_granularity")]
        pub price_granularity: String,
    }

    fn default_price_granularity() -> String {
        "dense".to_string()
    }
}

#[path = "src/settings.rs"]
mod settings;

use std::fs;
use std::path::Path;

const TRUSTED_SERVER_INIT_CONFIG_PATH: &str = "../../trusted-server.toml";
const TRUSTED_SERVER_OUTPUT_CONFIG_PATH: &str = "../../target/trusted-server-out.toml";
const CREATIVE_OPPORTUNITIES_PATH: &str = "../../creative-opportunities.toml";

fn main() {
    // Always rerun build.rs: integration settings are stored in a flat
    // HashMap<String, JsonValue>, so we cannot enumerate all possible env
    // var keys ahead of time. Emitting rerun-if-changed for a nonexistent
    // file forces cargo to always rerun the build script.
    println!("cargo:rerun-if-changed=_always_rebuild_sentinel_");

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

    // Validate creative-opportunities.toml slot IDs at build time
    println!("cargo:rerun-if-changed={}", CREATIVE_OPPORTUNITIES_PATH);

    let co_path = Path::new(CREATIVE_OPPORTUNITIES_PATH);
    if co_path.exists() {
        let co_content =
            fs::read_to_string(co_path).expect("should read creative-opportunities.toml");
        let co_value: toml::Value =
            toml::from_str(&co_content).expect("creative-opportunities.toml: invalid TOML");
        let slot_id_re = regex::Regex::new(r"^[A-Za-z0-9_\-]+$").expect("should compile regex");
        if let Some(slots) = co_value.get("slot").and_then(|v| v.as_array()) {
            for slot in slots {
                let id = slot
                    .get("id")
                    .and_then(|v| v.as_str())
                    .expect("creative-opportunities.toml: slot missing 'id' field");
                if !slot_id_re.is_match(id) {
                    panic!(
                        "creative-opportunities.toml: slot id '{}' is invalid; \
                         only [A-Za-z0-9_-] allowed",
                        id
                    );
                }
            }
            println!(
                "cargo:warning=creative-opportunities.toml: {} slot(s) validated",
                slots.len()
            );
        }
    }
}
