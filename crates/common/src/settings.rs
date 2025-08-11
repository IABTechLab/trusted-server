use core::str;

use config::{Config, Environment, File, FileFormat};
use error_stack::{Report, ResultExt};
use serde::{Deserialize, Serialize};
use url::Url;
use validator::{Validate, ValidationError};

use crate::error::TrustedServerError;

pub const ENVIRONMENT_VARIABLE_PREFIX: &str = "TRUSTED_SERVER";
pub const ENVIRONMENT_VARIABLE_SEPARATOR: &str = "__";

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
pub struct AdServer {
    pub ad_partner_url: String,
    pub sync_url: String,
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
pub struct Publisher {
    pub domain: String,
    pub cookie_domain: String,
    pub origin_backend: String,
    pub origin_url: String,
}

impl Publisher {
    /// Extracts the host (including port if present) from the origin_url.
    ///
    /// # Examples
    ///
    /// ```
    /// # use trusted_server_common::settings::Publisher;
    /// let publisher = Publisher {
    ///     domain: "example.com".to_string(),
    ///     cookie_domain: ".example.com".to_string(),
    ///     origin_url: "https://origin.example.com:8080".to_string(),
    /// };
    /// assert_eq!(publisher.origin_host(), "origin.example.com:8080");
    /// ```
    #[allow(dead_code)]
    pub fn origin_host(&self) -> String {
        Url::parse(&self.origin_url)
            .ok()
            .and_then(|url| {
                url.host_str().map(|host| match url.port() {
                    Some(port) => format!("{}:{}", host, port),
                    None => host.to_string(),
                })
            })
            .unwrap_or_else(|| self.origin_url.clone())
    }
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
pub struct Prebid {
    pub server_url: String,
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
pub struct GamAdUnit {
    pub name: String,
    pub size: String,
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
pub struct Gam {
    pub publisher_id: String,
    pub server_url: String,
    pub ad_units: Vec<GamAdUnit>,
}


#[allow(unused)]
#[derive(Debug, Default, Deserialize, Serialize, Validate)]
pub struct Synthetic {
    pub counter_store: String,
    pub opid_store: String,
    #[validate(length(min = 1), custom(function = Synthetic::validate_secret_key))]
    pub secret_key: String,
    #[validate(length(min = 1))]
    pub template: String,
}

impl Synthetic {
    pub fn validate_secret_key(secret_key: &String) -> Result<(), ValidationError> {
        match (secret_key).as_str() {
            "secret_key" => Err(ValidationError::new("Secret key is not valid")),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct PartnerConfig {
    pub enabled: bool,
    pub name: String,
    pub domains_to_proxy: Vec<String>,
    pub proxy_domain: String,
    pub backend_name: String,
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
pub struct Partners {
    pub gam: Option<PartnerConfig>,
    pub equativ: Option<PartnerConfig>,
    pub prebid: Option<PartnerConfig>,
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
pub struct Experimental {
    pub enable_edge_pub: bool,
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
pub struct Settings {
    #[validate(nested)]
    pub ad_server: AdServer,
    #[validate(nested)]
    pub publisher: Publisher,
    #[validate(nested)]
    pub prebid: Prebid,
    #[validate(nested)]
    pub gam: Gam,
    #[validate(nested)]
    pub synthetic: Synthetic,
    #[validate(nested)]
    pub partners: Option<Partners>,
    #[validate(nested)]
    pub experimental: Option<Experimental>,
}

#[allow(unused)]
impl Settings {
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

        assert_eq!(settings.gam.publisher_id, "21796327522");
        assert_eq!(
            settings.gam.server_url,
            "https://securepubads.g.doubleclick.net/gampad/ads"
        );
        assert_eq!(settings.gam.ad_units.len(), 2);
        assert_eq!(settings.gam.ad_units[0].name, "test_unit_1");
        assert_eq!(settings.gam.ad_units[0].size, "320x50");
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

    #[test]
    fn test_publisher_origin_host() {
        // Test with full URL including port
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_backend: "publisher_origin".to_string(),
            origin_url: "https://origin.example.com:8080".to_string(),
        };
        assert_eq!(publisher.origin_host(), "origin.example.com:8080");

        // Test with URL without port (default HTTPS port)
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_backend: "publisher_origin".to_string(),
            origin_url: "https://origin.example.com".to_string(),
        };
        assert_eq!(publisher.origin_host(), "origin.example.com");

        // Test with HTTP URL with explicit port
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_backend: "publisher_origin".to_string(),
            origin_url: "http://localhost:9090".to_string(),
        };
        assert_eq!(publisher.origin_host(), "localhost:9090");

        // Test with URL without protocol (fallback to original)
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_backend: "publisher_origin".to_string(),
            origin_url: "localhost:9090".to_string(),
        };
        assert_eq!(publisher.origin_host(), "localhost:9090");

        // Test with IPv4 address
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_backend: "publisher_origin".to_string(),
            origin_url: "http://192.168.1.1:8080".to_string(),
        };
        assert_eq!(publisher.origin_host(), "192.168.1.1:8080");

        // Test with IPv6 address
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_backend: "publisher_origin".to_string(),
            origin_url: "http://[::1]:8080".to_string(),
        };
        assert_eq!(publisher.origin_host(), "[::1]:8080");
    }
}
