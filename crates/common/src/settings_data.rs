use core::str;
use error_stack::{Report, ResultExt};
use validator::Validate;

use crate::error::TrustedServerError;
use crate::settings::Settings;

const SETTINGS_DATA: &[u8] = include_bytes!("../../../target/trusted-server-out.toml");

/// Creates a new [`Settings`] instance from the embedded configuration file.
// /
// / Loads the configuration from the embedded `trusted-server.toml` file
// / and applies any environment variable overrides.
// /
// / # Errors
// /
// / - [`TrustedServerError::InvalidUtf8`] if the embedded TOML file contains invalid UTF-8
// / - [`TrustedServerError::Configuration`] if the configuration is invalid or missing required fields
// / - [`TrustedServerError::InsecureSecretKey`] if the secret key is set to the default value
pub fn get_settings() -> Result<Settings, Report<TrustedServerError>> {
    let toml_bytes = SETTINGS_DATA;
    let toml_str = str::from_utf8(toml_bytes).change_context(TrustedServerError::InvalidUtf8 {
        message: "embedded trusted-server.toml file".to_string(),
    })?;

    let settings = Settings::from_toml(toml_str)?;

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

        let settings = settings.unwrap();
        // Verify basic structure is loaded
        assert!(!settings.publisher.domain.is_empty());
        assert!(!settings.publisher.cookie_domain.is_empty());
        assert!(!settings.publisher.origin_url.is_empty());
        assert!(!settings.prebid.server_url.is_empty());
        assert!(!settings.synthetic.counter_store.is_empty());
        assert!(!settings.synthetic.opid_store.is_empty());
        assert!(!settings.synthetic.secret_key.is_empty());
        assert!(!settings.synthetic.template.is_empty());
    }
}
