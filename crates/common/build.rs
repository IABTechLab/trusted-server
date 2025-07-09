#[path = "src/error.rs"]
mod error;

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
    collect_env_vars(&settings_json, &mut env_vars, vec![]);

    // Print rerun-if-env-changed for each variable
    let mut sorted_vars: Vec<_> = env_vars.into_iter().collect();
    sorted_vars.sort();

    for var in sorted_vars {
        println!("cargo:rerun-if-env-changed={}", var);
    }
}

fn merge_toml() {
    // Get the OUT_DIR where we'll copy the config file
    let dest_path = Path::new(TRUSTED_SERVER_OUTPUT_CONFIG_PATH);

    // Read init config
    let init_config_path = Path::new(TRUSTED_SERVER_INIT_CONFIG_PATH);
    let toml_content = fs::read_to_string(init_config_path)
        .expect(&format!("Failed to read {:?}", init_config_path));

    // For build time: use from_toml to parse with environment variables
    let settings = settings::Settings::from_toml(&toml_content)
        .expect("Failed to parse settings at build time");

    // Write the merged settings to the output directory as TOML
    let merged_toml =
        toml::to_string_pretty(&settings).expect("Failed to serialize settings to TOML");

    fs::write(&dest_path, merged_toml).expect(&format!("Failed to write {:?}", dest_path));
}

fn collect_env_vars(value: &Value, env_vars: &mut HashSet<String>, path: Vec<String>) {
    if let Value::Object(map) = value {
        for (key, val) in map {
            let mut new_path = path.clone();
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
                    collect_env_vars(val, env_vars, new_path);
                }
                _ => {}
            }
        }
    }
}
