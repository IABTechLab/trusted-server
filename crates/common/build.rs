#[path = "src/error.rs"]
mod error;

#[path = "src/settings.rs"]
mod settings;

use serde_json::Value;
use std::collections::HashSet;

fn main() {
    // Watch the settings.rs file for changes
    println!("cargo:rerun-if-changed=../../trusted-server.toml");

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
