use core::str;

use config::{Config, Environment, File, FileFormat};
use error_stack::{Report, ResultExt};
use serde::{Deserialize, Serialize};

use crate::error::TrustedServerError;

pub const ENVIRONMENT_VARIABLE_PREFIX: &str = "TRUSTED_SERVER";
pub const ENVIRONMENT_VARIABLE_SEPARATOR: &str = "__";

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct AdServer {
    pub ad_partner_url: String,
    pub sync_url: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Publisher {
    pub domain: String,
    pub cookie_domain: String,
    pub origin_url: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Prebid {
    pub server_url: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[allow(unused)]
pub struct GamAdUnit {
    pub name: String,
    pub size: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[allow(unused)]
pub struct Gam {
    pub publisher_id: String,
    pub server_url: String,
    pub ad_units: Vec<GamAdUnit>,
}

#[allow(unused)]
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Synthetic {
    pub counter_store: String,
    pub opid_store: String,
    pub secret_key: String,
    pub template: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Settings {
    pub ad_server: AdServer,
    pub publisher: Publisher,
    pub prebid: Prebid,
    pub gam: Gam,
    pub synthetic: Synthetic,
}

#[allow(unused)]
impl Settings {
    /// Creates a new [`Settings`] instance from the embedded configuration file.
    ///
    /// Loads the configuration from the embedded `trusted-server.toml` file
    /// and applies any environment variable overrides.
    ///
    /// # Errors
    ///
    /// - [`TrustedServerError::InvalidUtf8`] if the embedded TOML file contains invalid UTF-8
    /// - [`TrustedServerError::Configuration`] if the configuration is invalid or missing required fields
    /// - [`TrustedServerError::InsecureSecretKey`] if the secret key is set to the default value
    pub fn new() -> Result<Self, Report<TrustedServerError>> {
        let toml_bytes = include_bytes!("../../../trusted-server.toml");
        let toml_str =
            str::from_utf8(toml_bytes).change_context(TrustedServerError::InvalidUtf8 {
                message: "embedded trusted-server.toml file".to_string(),
            })?;

        let settings = Self::from_toml(toml_str)?;

        // Validate that the secret key is not the default
        if settings.synthetic.secret_key == "secret-key" {
            return Err(Report::new(TrustedServerError::InsecureSecretKey));
        }

        Ok(settings)
    }

    /// Creates a new [`Settings`] instance from a TOML string.
    ///
    /// Parses the provided TOML configuration and applies any environment
    /// variable overrides using the `TRUSTED_SERVER__` prefix.
    ///
    /// # Errors
    ///
    /// - [`TrustedServerError::Configuration`] if the TOML is invalid or missing required fields
    pub fn from_toml(toml_str: &str) -> Result<Self, Report<TrustedServerError>> {
        let environment = Environment::default()
            .prefix(ENVIRONMENT_VARIABLE_PREFIX)
            .separator(ENVIRONMENT_VARIABLE_SEPARATOR);

        let toml = File::from_str(toml_str, FileFormat::Toml);
        let config = Config::builder()
            .add_source(toml)
            .add_source(environment)
            .build()
            .change_context(TrustedServerError::Configuration {
                message: "Failed to build configuration".to_string(),
            })?;
        // You can deserialize (and thus freeze) the entire configuration as
        config
            .try_deserialize()
            .change_context(TrustedServerError::Configuration {
                message: "Failed to deserialize configuration".to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    use crate::test_support::tests::crate_test_settings_str;

    #[test]
    fn test_settings_new() {
        // Test that Settings::new() loads successfully
        let settings = Settings::new();
        assert!(settings.is_ok(), "Settings should load from embedded TOML");

        let settings = settings.unwrap();
        // Verify basic structure is loaded
        assert!(!settings.ad_server.ad_partner_url.is_empty());
        assert!(!settings.ad_server.sync_url.is_empty());
        assert!(!settings.publisher.domain.is_empty());
        assert!(!settings.publisher.cookie_domain.is_empty());
        assert!(!settings.publisher.origin_url.is_empty());
        assert!(!settings.prebid.server_url.is_empty());
        assert!(!settings.synthetic.counter_store.is_empty());
        assert!(!settings.synthetic.opid_store.is_empty());
        assert!(!settings.synthetic.secret_key.is_empty());
        assert!(!settings.synthetic.template.is_empty());
    }

    #[test]
    fn test_settings_from_valid_toml() {
        let toml_str = crate_test_settings_str();
        let settings = Settings::from_toml(&toml_str);

        assert!(settings.is_ok());

        let settings = settings.expect("should parse valid TOML");
        assert_eq!(
            settings.ad_server.ad_partner_url,
            "https://test-adpartner.com"
        );
        assert_eq!(
            settings.ad_server.sync_url,
            "https://test-adpartner.com/synthetic_id={{synthetic_id}}"
        );
        assert_eq!(
            settings.prebid.server_url,
            "https://test-prebid.com/openrtb2/auction"
        );
        assert_eq!(settings.publisher.domain, "test-publisher.com");
        assert_eq!(settings.publisher.cookie_domain, ".test-publisher.com");
        assert_eq!(
            settings.publisher.origin_url,
            "https://origin.test-publisher.com"
        );
        assert_eq!(settings.synthetic.counter_store, "test-counter-store");
        assert_eq!(settings.synthetic.opid_store, "test-opid-store");
        assert_eq!(settings.synthetic.secret_key, "test-secret-key");
        assert!(settings.synthetic.template.contains("{{client_ip}}"));
    }

    #[test]
    fn test_settings_missing_required_fields() {
        let re = Regex::new(r"ad_partner_url = .*").unwrap();
        let toml_str = crate_test_settings_str();
        let toml_str = re.replace(&toml_str, "");

        let settings = Settings::from_toml(&toml_str);
        assert!(
            settings.is_err(),
            "Should fail when required fields are missing"
        );
    }

    #[test]
    fn test_settings_empty_toml() {
        let toml_str = "";
        let settings = Settings::from_toml(toml_str);

        assert!(settings.is_err(), "Should fail with empty TOML");
    }

    #[test]
    fn test_settings_invalid_toml_syntax() {
        let re = Regex::new(r"\]").unwrap();
        let toml_str = crate_test_settings_str();
        let toml_str = re.replace(&toml_str, "");

        let settings = Settings::from_toml(&toml_str);
        assert!(settings.is_err(), "Should fail with invalid TOML syntax");
    }

    #[test]
    fn test_settings_partial_config() {
        let re = Regex::new(r"\[ad_server\]").unwrap();
        let toml_str = crate_test_settings_str();
        let toml_str = re.replace(&toml_str, "");

        let settings = Settings::from_toml(&toml_str);
        assert!(settings.is_err(), "Should fail when sections are missing");
    }

    #[test]
    fn test_settings_extra_fields() {
        let toml_str = crate_test_settings_str() + "\nhello = 1";

        let settings = Settings::from_toml(&toml_str);
        assert!(settings.is_ok(), "Extra fields should be ignored");
    }

    #[test]
    fn test_set_env() {
        let re = Regex::new(r"ad_partner_url = .*").unwrap();
        let toml_str = crate_test_settings_str();
        let toml_str = re.replace(&toml_str, "");

        temp_env::with_var(
            format!(
                "{}{}AD_SERVER{}AD_PARTNER_URL",
                ENVIRONMENT_VARIABLE_PREFIX,
                ENVIRONMENT_VARIABLE_SEPARATOR,
                ENVIRONMENT_VARIABLE_SEPARATOR
            ),
            Some("https://change-ad.com/serve"),
            || {
                let settings = Settings::from_toml(&toml_str);

                assert!(settings.is_ok(), "Settings should load from embedded TOML");
                assert_eq!(
                    settings.unwrap().ad_server.ad_partner_url,
                    "https://change-ad.com/serve"
                );
            },
        );
    }

    #[test]
    fn test_override_env() {
        let toml_str = crate_test_settings_str();

        temp_env::with_var(
            format!(
                "{}{}AD_SERVER{}AD_PARTNER_URL",
                ENVIRONMENT_VARIABLE_PREFIX,
                ENVIRONMENT_VARIABLE_SEPARATOR,
                ENVIRONMENT_VARIABLE_SEPARATOR
            ),
            Some("https://change-ad.com/serve"),
            || {
                let settings = Settings::from_toml(&toml_str);

                assert!(settings.is_ok(), "Settings should load from embedded TOML");
                assert_eq!(
                    settings.unwrap().ad_server.ad_partner_url,
                    "https://change-ad.com/serve"
                );
            },
        );
    }
}
