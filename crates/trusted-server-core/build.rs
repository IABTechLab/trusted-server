// Build script includes source modules (`error`, `auction_config_types`, etc.)
// for compile-time config validation. Not all items from those modules are used
// in the build context, so `dead_code` is expected.
#![allow(clippy::unwrap_used, clippy::panic, dead_code)]

// `glob` is a real build-dependency (see Cargo.toml `[build-dependencies]`), so
// `creative_slot_build_check::pattern_compiles` resolves `glob::Pattern::new`
// against the actual glob crate. It must NOT be stubbed here: a stub that always
// returned `Ok` would let an invalid env-injected pattern such as
// `page_patterns = ["["]` pass the build-time check and embed into the config,
// only to be dropped by the real glob crate at runtime settings load.

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

#[path = "src/host_header.rs"]
mod host_header;

#[path = "src/platform/image_optimizer.rs"]
mod platform_image_optimizer;

mod platform {
    pub use crate::platform_image_optimizer::PlatformImageOptimizerRegion;
}

// CreativeOpportunitiesConfig for build.rs deserialization only
mod creative_opportunities {
    use serde::{Deserialize, Serialize};

    /// Stub slot type — only used so settings.rs compiles in the build context.
    #[derive(Debug, Clone, Deserialize, Serialize)]
    pub struct CreativeOpportunitySlot {
        pub id: String,
    }

    #[derive(Debug, Clone, Deserialize, Serialize)]
    pub struct CreativeOpportunitiesConfig {
        pub gam_network_id: String,
        #[serde(default)]
        pub auction_timeout_ms: Option<u32>,
        #[serde(default = "default_price_granularity")]
        pub price_granularity: String,
        /// Deserialized as raw JSON values so build.rs can validate slot IDs
        /// without pulling in the full runtime type. Uses `vec_from_seq_or_map`
        /// so env var JSON blobs (strings) deserialize correctly.
        #[serde(
            default,
            rename = "slot",
            deserialize_with = "crate::settings::vec_from_seq_or_map"
        )]
        pub slot_raw: Vec<serde_json::Value>,
        /// Typed slot vec — always empty in the build context; exists only so
        /// settings.rs (included via #[path]) compiles against the stub.
        #[serde(skip)]
        pub slot: Vec<CreativeOpportunitySlot>,
    }

    impl CreativeOpportunitiesConfig {
        /// No-op stub — pattern compilation only runs at runtime.
        pub fn compile_slots(&mut self) {}

        /// No-op stub — full slot-shape validation runs at runtime against
        /// the real creative opportunity types.
        pub fn validate_runtime(&self) -> Result<(), String> {
            Ok(())
        }
    }

    /// Stub — the typed `slot` vec is always empty in the build context (see
    /// `#[serde(skip)]` above), so `Settings::prepare_runtime` never reaches
    /// this. Build-time slot-id validation happens in `main()` against
    /// `slot_raw` instead.
    pub fn validate_slot_id(_id: &str) -> Result<(), String> {
        Ok(())
    }

    fn default_price_granularity() -> String {
        "dense".to_string()
    }
}

#[path = "src/settings.rs"]
mod settings;

#[path = "src/creative_slot_build_check.rs"]
mod creative_slot_build_check;

use creative_slot_build_check::{validate_creative_slot, validate_price_granularity};
use std::fs;
use std::path::Path;

const TRUSTED_SERVER_INIT_CONFIG_PATH: &str = "../../trusted-server.toml";
const TRUSTED_SERVER_OUTPUT_CONFIG_PATH: &str = "../../target/trusted-server-out.toml";

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

    // Merge base TOML with environment variable overrides.
    // Panics if admin endpoints are not covered by a handler.
    let settings = settings::Settings::from_toml_and_env(&toml_content)
        .expect("Failed to parse settings at build time");

    // Validate [creative_opportunities.slot] entries from the *merged* config
    // (base trusted-server.toml plus any TRUSTED_SERVER__CREATIVE_OPPORTUNITIES__SLOT
    // env overrides) before it is serialized and embedded. This mirrors the
    // runtime validator (CreativeOpportunitySlot::validate_runtime) — the build
    // context uses a stub whose validate_runtime is a no-op, so without this an
    // invalid slot would pass CI and surface as a request-time configuration
    // error / service outage. The validator is shared with the crate (see
    // `creative_slot_build_check`) so it stays under test. Running it before the
    // write also means a rejected config is never persisted to the embedded file.
    if let Some(co) = &settings.creative_opportunities {
        // price_granularity is a String stub in the build context, so validate it
        // against the real PriceGranularity enum before embedding — an invalid
        // value would otherwise fail runtime settings load on every request.
        if let Err(err) = validate_price_granularity(&co.price_granularity) {
            panic!("trusted-server.toml [creative_opportunities]: {err}");
        }
        for slot in &co.slot_raw {
            if let Err(err) = validate_creative_slot(slot, &co.gam_network_id) {
                panic!("trusted-server.toml [creative_opportunities.slot]: {err}");
            }
        }
        if !co.slot_raw.is_empty() {
            println!(
                "cargo:warning=creative_opportunities: {} slot(s) validated",
                co.slot_raw.len()
            );
        }
    }

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
