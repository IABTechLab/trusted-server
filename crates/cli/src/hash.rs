//! Hash computation for configuration files.

use std::fs;
use std::path::Path;

use trusted_server_common::config_store::compute_settings_hash;

use crate::config::load_and_merge_config;
use crate::error::CliError;
use crate::HashFormat;

/// Compute SHA-256 hash of a configuration file.
///
/// Line endings are normalized to LF for consistent hashing across platforms.
pub fn compute_file_hash(path: &Path) -> Result<String, CliError> {
    let content = fs::read_to_string(path)?;
    Ok(compute_settings_hash(&content))
}

/// Compute and display the hash of a configuration file.
pub fn compute_and_display(
    path: std::path::PathBuf,
    format: HashFormat,
    raw: bool,
    verbose: bool,
) -> Result<(), CliError> {
    let hash = if raw {
        compute_file_hash(&path)?
    } else {
        let (_settings, merged_toml) = load_and_merge_config(&path, verbose)?;
        compute_settings_hash(&merged_toml)
    };

    match format {
        HashFormat::Text => {
            println!("{}", hash);
        }
        HashFormat::Json => {
            let output = serde_json::json!({
                "file": path.display().to_string(),
                "hash": hash,
                "algorithm": "sha256"
            });
            println!("{}", serde_json::to_string_pretty(&output).unwrap());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compute_hash(content: &str) -> String {
        compute_settings_hash(content)
    }

    #[test]
    fn test_compute_hash() {
        let content = "[publisher]\ndomain = \"example.com\"\n";
        let hash = compute_hash(content);
        assert!(hash.starts_with("sha256:"));
        assert_eq!(hash.len(), 7 + 64); // "sha256:" + 64 hex chars
    }

    #[test]
    fn test_hash_normalization() {
        let lf_content = "line1\nline2\n";
        let crlf_content = "line1\r\nline2\r\n";

        let lf_hash = compute_hash(lf_content);
        let crlf_hash = compute_hash(crlf_content);

        assert_eq!(lf_hash, crlf_hash);
    }
}
