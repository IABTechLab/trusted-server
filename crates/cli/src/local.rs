//! Local development support for Fastly Compute.
//!
//! This module provides functionality to generate config store JSON files
//! for local development with `fastly compute serve`.

use std::fs;
use std::path::PathBuf;

use trusted_server_common::config_store::compute_settings_hash;
use trusted_server_common::settings::Settings;
use validator::Validate;

use crate::error::CliError;

/// Default output path for the config store JSON file.
pub const DEFAULT_OUTPUT_PATH: &str = "target/trusted-server-config.json";

/// Generate a JSON file for Fastly local config store.
///
/// This creates a JSON file that can be referenced in fastly.toml using the
/// `file` option. The fastly.toml should already be configured to read from
/// `target/trusted-server-config.json`.
pub fn generate_config_store_json(
    file: PathBuf,
    output: PathBuf,
    verbose: bool,
) -> Result<(), CliError> {
    let content = fs::read_to_string(&file)?;

    if verbose {
        println!("Loading config from: {}", file.display());
        println!("Environment variables with TRUSTED_SERVER__ prefix will be merged");
    }

    // Parse and validate with env var merging
    let settings = Settings::from_toml(&content)
        .map_err(|e| CliError::Config(format!("Failed to parse and merge config: {:?}", e)))?;

    settings
        .validate()
        .map_err(|e| CliError::Config(format!("Settings validation failed: {e}")))?;

    let merged_toml = settings
        .to_canonical_toml()
        .map_err(|e| CliError::Config(format!("Failed to serialize config: {e:?}")))?;

    // Compute hash
    let hash = compute_settings_hash(&merged_toml);

    // Create JSON structure for Fastly config store
    let config_store = serde_json::json!({
        "settings": merged_toml,
        "settings-hash": hash
    });

    let json_output = serde_json::to_string_pretty(&config_store)
        .map_err(|e| CliError::Config(format!("Failed to serialize JSON: {}", e)))?;

    // Ensure parent directory exists
    if let Some(parent) = output.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }

    fs::write(&output, &json_output)?;

    println!("Config store JSON written to: {}", output.display());
    println!("Settings hash: {}", hash);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_config(dir: &TempDir) -> PathBuf {
        let config_path = dir.path().join("test-config.toml");
        let mut file = fs::File::create(&config_path).unwrap();
        write!(
            file,
            r#"
[publisher]
domain = "test.com"
cookie_domain = ".test.com"
origin_url = "https://origin.test.com"
proxy_secret = "test-secret-key-that-is-long-enough"

[synthetic]
counter_store = "counter"
opid_store = "opid"
secret_key = "test-synthetic-secret-key"
template = "{{{{ client_ip }}}}"

[[handlers]]
path = "^/admin"
username = "admin"
password = "password"
"#
        )
        .unwrap();
        config_path
    }

    #[test]
    fn test_generate_config_store_json_creates_valid_json() {
        let dir = TempDir::new().unwrap();
        let config_path = create_test_config(&dir);
        let output_path = dir.path().join("output.json");

        let result = generate_config_store_json(config_path, output_path.clone(), false);
        assert!(result.is_ok());

        // Verify file exists and is valid JSON
        let content = fs::read_to_string(&output_path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert!(json.get("settings").is_some());
        assert!(json.get("settings-hash").is_some());
        assert!(json["settings-hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:"));
    }

    #[test]
    fn test_generate_config_store_json_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let config_path = create_test_config(&dir);
        let output_path = dir.path().join("nested").join("deep").join("output.json");

        let result = generate_config_store_json(config_path, output_path.clone(), false);
        assert!(result.is_ok());
        assert!(output_path.exists());
    }

    #[test]
    fn test_generate_config_store_json_with_invalid_config() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("invalid.toml");
        fs::write(&config_path, "invalid { toml").unwrap();
        let output_path = dir.path().join("output.json");

        let result = generate_config_store_json(config_path, output_path, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_generate_config_store_json_with_nonexistent_file() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("nonexistent.toml");
        let output_path = dir.path().join("output.json");

        let result = generate_config_store_json(config_path, output_path, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_default_output_path() {
        assert_eq!(DEFAULT_OUTPUT_PATH, "target/trusted-server-config.json");
    }
}
