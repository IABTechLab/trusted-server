// Build script includes source modules (`error`, `auction_config_types`, etc.)
// for compile-time config validation. Not all items from those modules are used
// in the build context, so `dead_code` is expected.
#![allow(clippy::unwrap_used, clippy::panic, dead_code)]

#[path = "src/error.rs"]
mod error;

#[path = "src/auction_config_types.rs"]
mod auction_config_types;

#[path = "src/consent_config.rs"]
mod consent_config;

#[path = "src/settings.rs"]
mod settings;

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

    // Merge base TOML with environment variable overrides and write output
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
