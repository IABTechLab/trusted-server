//! Configuration management commands.
//!
//! Configuration is loaded from TOML files and merged with environment variables
//! prefixed with `TRUSTED_SERVER__`. For example, `TRUSTED_SERVER__PUBLISHER__DOMAIN`
//! will override `publisher.domain` in the TOML file.

use std::fs;
use std::path::PathBuf;

use trusted_server_common::config_store::{compute_settings_hash, SETTINGS_HASH_KEY, SETTINGS_KEY};
use trusted_server_common::settings::Settings;
use validator::Validate;

use crate::error::CliError;
use crate::platform::create_client;
use crate::Platform;

/// Load and merge configuration from TOML file with environment variables.
///
/// Environment variables prefixed with `TRUSTED_SERVER__` will override TOML values.
/// For example: `TRUSTED_SERVER__PUBLISHER__DOMAIN=example.com`
pub(crate) fn load_and_merge_config(
    file: &PathBuf,
    verbose: bool,
) -> Result<(Settings, String), CliError> {
    let content = fs::read_to_string(file)?;

    if verbose {
        println!("Loading config from: {}", file.display());
        println!("Environment variables with TRUSTED_SERVER__ prefix will be merged");
    }

    // Parse TOML and merge with environment variables
    let settings = Settings::from_toml(&content)
        .map_err(|e| CliError::Config(format!("Failed to parse and merge config: {:?}", e)))?;

    settings
        .validate()
        .map_err(|e| CliError::Config(format!("Settings validation failed: {e}")))?;

    let merged_toml = settings
        .to_canonical_toml()
        .map_err(|e| CliError::Config(format!("Failed to serialize merged config: {e:?}")))?;

    Ok((settings, merged_toml))
}

/// Push configuration to edge platform Config Store.
///
/// The configuration is first merged with environment variables, then pushed.
pub fn push(
    platform: Platform,
    file: PathBuf,
    store_id: String,
    dry_run: bool,
    verbose: bool,
) -> Result<(), CliError> {
    // Load, validate, and merge with env vars
    let (settings, merged_toml) = load_and_merge_config(&file, verbose)?;

    // Compute hash of the merged config
    let hash = compute_settings_hash(&merged_toml);

    if verbose {
        println!("Config file: {}", file.display());
        println!("Publisher domain: {}", settings.publisher.domain);
        println!("Config hash: {}", hash);
        println!("Platform: {}", platform);
    }

    if dry_run {
        println!("\n[Dry Run] Would upload the following:");
        println!("  Key '{}': {} bytes", SETTINGS_KEY, merged_toml.len());
        println!("  Key '{}': {}", SETTINGS_HASH_KEY, hash);
        if verbose {
            println!("\nMerged configuration preview:");
            println!("---");
            // Show first 50 lines
            for line in merged_toml.lines().take(50) {
                println!("{}", line);
            }
            if merged_toml.lines().count() > 50 {
                println!("... (truncated)");
            }
            println!("---");
        }
        return Ok(());
    }

    // Create platform client and push
    let client = create_client(&platform, store_id)?;

    if verbose {
        println!("\nUploading settings...");
    }

    client.put(SETTINGS_KEY, &merged_toml)?;
    client.put(SETTINGS_HASH_KEY, &hash)?;

    println!(
        "Successfully pushed configuration to {} Config Store",
        platform
    );
    println!("  Settings hash: {}", hash);

    Ok(())
}

/// Validate configuration file.
///
/// Validates TOML syntax, required fields, and merges with environment variables.
pub fn validate(file: PathBuf, verbose: bool) -> Result<(), CliError> {
    // Load, validate, and merge with env vars
    let (settings, merged_toml) = load_and_merge_config(&file, verbose)?;

    // Compute hash of merged config
    let hash = compute_settings_hash(&merged_toml);

    println!("Configuration is valid");
    println!("  File: {}", file.display());
    println!("  Hash: {}", hash);
    println!("  Publisher domain: {}", settings.publisher.domain);

    if verbose {
        // Parse the merged TOML to show sections
        let value: toml::Value = toml::from_str(&merged_toml)?;
        if let Some(table) = value.as_table() {
            println!("\nSections found:");
            for key in table.keys() {
                println!("  - [{}]", key);
            }
        }

        // Show active integrations
        println!("\nIntegrations:");
        if let Some(integrations) = value.get("integrations").and_then(|v| v.as_table()) {
            for (name, config) in integrations {
                let enabled = config
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                println!(
                    "  - {}: {}",
                    name,
                    if enabled { "enabled" } else { "disabled" }
                );
            }
        }
    }

    Ok(())
}

/// Compare local config with deployed config.
pub fn diff(
    platform: Platform,
    store_id: String,
    file: PathBuf,
    verbose: bool,
) -> Result<(), CliError> {
    // Load and merge local config
    let (_settings, local_merged) = load_and_merge_config(&file, verbose)?;
    let local_hash = compute_settings_hash(&local_merged);

    if verbose {
        println!("Local file: {}", file.display());
        println!("Local hash (after env merge): {}", local_hash);
    }

    // Create platform client and fetch remote
    let client = create_client(&platform, store_id)?;

    let remote_hash = client.get(SETTINGS_HASH_KEY)?;
    let remote_content = client.get(SETTINGS_KEY)?;

    match (remote_hash, remote_content) {
        (Some(rh), Some(rc)) => {
            println!("Local hash:  {}", local_hash);
            println!("Remote hash: {}", rh);

            if local_hash == rh {
                println!("\nConfigurations are identical.");
            } else {
                println!("\nConfigurations differ!");

                if verbose {
                    // Show a simple diff
                    let local_lines: Vec<&str> = local_merged.lines().collect();
                    let remote_lines: Vec<&str> = rc.lines().collect();

                    println!("\n--- Remote");
                    println!("+++ Local (merged with env vars)");

                    for (i, (local, remote)) in
                        local_lines.iter().zip(remote_lines.iter()).enumerate()
                    {
                        if local != remote {
                            println!("@@ line {} @@", i + 1);
                            println!("-{}", remote);
                            println!("+{}", local);
                        }
                    }

                    // Handle different lengths
                    if local_lines.len() > remote_lines.len() {
                        println!("\n+++ Additional local lines:");
                        for line in local_lines.iter().skip(remote_lines.len()) {
                            println!("+{}", line);
                        }
                    } else if remote_lines.len() > local_lines.len() {
                        println!("\n--- Additional remote lines:");
                        for line in remote_lines.iter().skip(local_lines.len()) {
                            println!("-{}", line);
                        }
                    }
                }
            }
        }
        (None, Some(rc)) => {
            let computed_remote = compute_settings_hash(&rc);
            println!("Local hash:   {}", local_hash);
            println!("Remote hash (computed): {}", computed_remote);

            if local_hash == computed_remote {
                println!("\nConfigurations are identical.");
            } else {
                println!("\nConfigurations differ!");
            }

            if verbose {
                println!("\nRemote settings-hash missing; diffing content:");
                let local_lines: Vec<&str> = local_merged.lines().collect();
                let remote_lines: Vec<&str> = rc.lines().collect();

                println!("\n--- Remote");
                println!("+++ Local (merged with env vars)");

                for (i, (local, remote)) in local_lines.iter().zip(remote_lines.iter()).enumerate()
                {
                    if local != remote {
                        println!("@@ line {} @@", i + 1);
                        println!("-{}", remote);
                        println!("+{}", local);
                    }
                }
            }
        }
        (Some(rh), None) => {
            println!("Remote hash found but no settings content.");
            println!("Remote hash: {}", rh);
        }
        (None, None) => {
            println!("No remote configuration found.");
            println!("Local hash: {}", local_hash);
        }
    }

    Ok(())
}

/// Pull current config from Config Store.
pub fn pull(
    platform: Platform,
    store_id: String,
    output: PathBuf,
    verbose: bool,
) -> Result<(), CliError> {
    // Create platform client
    let client = create_client(&platform, store_id)?;

    if verbose {
        println!("Fetching configuration from {}...", platform);
    }

    let content = client
        .get(SETTINGS_KEY)?
        .ok_or_else(|| CliError::Platform("No settings found in config store".into()))?;

    let hash = client.get(SETTINGS_HASH_KEY)?;

    // Write to output file
    fs::write(&output, &content)?;

    println!("Configuration saved to: {}", output.display());

    if let Some(h) = hash {
        println!("Remote hash: {}", h);

        // Verify hash matches
        let computed = compute_settings_hash(&content);
        if computed == h {
            println!("Hash verification: OK");
        } else {
            println!("Warning: Hash mismatch!");
            println!("  Expected: {}", h);
            println!("  Computed: {}", computed);
        }
    }

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
    fn test_validate_valid_config() {
        let dir = TempDir::new().unwrap();
        let config_path = create_test_config(&dir);

        let result = validate(config_path, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_invalid_toml() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("invalid.toml");
        fs::write(&config_path, "invalid { toml").unwrap();

        let result = validate(config_path, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_missing_required_fields() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("incomplete.toml");
        fs::write(&config_path, "[publisher]\ndomain = \"test.com\"\n").unwrap();

        let result = validate(config_path, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_nonexistent_file() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("nonexistent.toml");

        let result = validate(config_path, false);
        assert!(result.is_err());
    }

    #[test]
    fn test_push_dry_run() {
        let dir = TempDir::new().unwrap();
        let config_path = create_test_config(&dir);

        // dry_run should succeed without network call
        let result = push(
            Platform::Fastly,
            config_path,
            "fake-store-id".to_string(),
            true, // dry_run
            false,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_push_dry_run_verbose() {
        let dir = TempDir::new().unwrap();
        let config_path = create_test_config(&dir);

        // dry_run with verbose should also succeed
        let result = push(
            Platform::Fastly,
            config_path,
            "fake-store-id".to_string(),
            true, // dry_run
            true, // verbose
        );
        assert!(result.is_ok());
    }
}
