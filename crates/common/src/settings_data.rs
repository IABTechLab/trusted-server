use core::str;
use error_stack::{Report, ResultExt};
use validator::Validate;

use crate::error::TrustedServerError;
use crate::settings::Settings;

pub use crate::auction_config_types::AuctionConfig;

const SETTINGS_DATA: &[u8] = include_bytes!("../../../target/trusted-server-out.toml");

/// Creates a new [`Settings`] instance from the embedded configuration file.
///
/// Deserializes directly via `toml::from_str` instead of [`Settings::from_toml`],
/// which runs the full `config` crate pipeline (env var scanning, source merging).
///
/// This is safe because `build.rs` already calls `Settings::from_toml()` at compile
/// time — merging `trusted-server.toml` with all `TRUSTED_SERVER__*` env vars — and
/// writes the fully-resolved result to `target/trusted-server-out.toml`. The embedded
/// bytes are that resolved output, so re-scanning env vars at runtime is redundant.
/// See `build.rs::merge_toml()` and the `cargo:rerun-if-env-changed` directives.
///
/// # Errors
///
/// - [`TrustedServerError::InvalidUtf8`] if the embedded TOML file contains invalid UTF-8
/// - [`TrustedServerError::Configuration`] if the configuration is invalid or missing required fields
pub fn get_settings() -> Result<Settings, Report<TrustedServerError>> {
    let toml_bytes = SETTINGS_DATA;
    let toml_str = str::from_utf8(toml_bytes).change_context(TrustedServerError::InvalidUtf8 {
        message: "embedded trusted-server.toml file".to_string(),
    })?;

    let mut settings: Settings =
        toml::from_str(toml_str).change_context(TrustedServerError::Configuration {
            message: "Failed to deserialize embedded config".to_string(),
        })?;

    settings.publisher.normalize();

    // Validate the settings
    settings
        .validate()
        .change_context(TrustedServerError::Configuration {
            message: "Failed to validate configuration".to_string(),
        })?;

    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_settings() {
        // Test that Settings::new() loads successfully
        let settings = get_settings();
        assert!(settings.is_ok(), "Settings should load from embedded TOML");

        let settings = settings.expect("should load settings from embedded TOML");
        // Verify basic structure is loaded
        assert!(!settings.publisher.domain.is_empty());
        assert!(!settings.publisher.cookie_domain.is_empty());
        assert!(!settings.publisher.origin_url.is_empty());
        assert!(!settings.synthetic.counter_store.is_empty());
        assert!(!settings.synthetic.opid_store.is_empty());
        assert!(!settings.synthetic.secret_key.is_empty());
        assert!(!settings.synthetic.template.is_empty());
    }
}
