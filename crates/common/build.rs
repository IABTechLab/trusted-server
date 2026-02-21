#![allow(clippy::unwrap_used, clippy::panic)]

#[path = "src/error.rs"]
mod error;

#[path = "src/auction_config_types.rs"]
mod auction_config_types;

#[path = "src/settings.rs"]
mod settings;

use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

const TRUSTED_SERVER_INIT_CONFIG_PATH: &str = "../../trusted-server.toml";
const TRUSTED_SERVER_OUTPUT_CONFIG_PATH: &str = "../../target/trusted-server-out.toml";

fn main() {
    merge_toml();
    rerun_if_changed();
}

fn rerun_if_changed() {
    // Watch the root trusted-server.toml file for changes
    println!("cargo:rerun-if-changed={}", TRUSTED_SERVER_INIT_CONFIG_PATH);

    // Create a default Settings instance and convert to JSON to discover all fields
    let default_settings = settings::Settings::default();
    let settings_json = serde_json::to_value(&default_settings).unwrap();

    let mut env_vars = HashSet::new();
    collect_env_vars(&settings_json, &mut env_vars, &[]);

    // Print rerun-if-env-changed for each variable
    let mut sorted_vars: Vec<_> = env_vars.into_iter().collect();
    sorted_vars.sort();

    for var in sorted_vars {
        println!("cargo:rerun-if-env-changed={}", var);
    }
}

fn merge_toml() {
    use config::{Config, Environment, File, FileFormat};

    // Get the OUT_DIR where we'll copy the config file
    let dest_path = Path::new(TRUSTED_SERVER_OUTPUT_CONFIG_PATH);

    // Read init config
    let init_config_path = Path::new(TRUSTED_SERVER_INIT_CONFIG_PATH);
    let toml_content = fs::read_to_string(init_config_path)
        .unwrap_or_else(|_| panic!("Failed to read {:?}", init_config_path));

    // Merge TOML + TRUSTED_SERVER__* env vars at build time using the config crate.
    // At runtime, from_toml() skips this — the embedded TOML is already resolved.
    let environment = Environment::default()
        .prefix(settings::ENVIRONMENT_VARIABLE_PREFIX)
        .separator(settings::ENVIRONMENT_VARIABLE_SEPARATOR);
    let config = Config::builder()
        .add_source(File::from_str(&toml_content, FileFormat::Toml))
        .add_source(environment)
        .build()
        .expect("Failed to build configuration at build time");
    let settings: settings::Settings = config
        .try_deserialize()
        .expect("Failed to deserialize configuration at build time");

    // Write the merged settings to the output directory as TOML
    let merged_toml =
        toml::to_string_pretty(&settings).expect("Failed to serialize settings to TOML");

    fs::write(dest_path, merged_toml).unwrap_or_else(|_| panic!("Failed to write {:?}", dest_path));
}

fn collect_env_vars(value: &Value, env_vars: &mut HashSet<String>, path: &[String]) {
    if let Value::Object(map) = value {
        for (key, val) in map {
            let mut new_path = path.to_owned();
            new_path.push(key.to_uppercase());

            match val {
                Value::String(_) | Value::Number(_) | Value::Bool(_) => {
                    // Leaf node - create environment variable
                    let env_var = format!(
                        "{}{}{}",
                        settings::ENVIRONMENT_VARIABLE_PREFIX,
                        settings::ENVIRONMENT_VARIABLE_SEPARATOR,
                        new_path.join(settings::ENVIRONMENT_VARIABLE_SEPARATOR)
                    );
                    env_vars.insert(env_var);
                }
                Value::Object(_) => {
                    // Recurse into nested objects
                    collect_env_vars(val, env_vars, &new_path);
                }
                _ => {}
            }
        }
    }
}
