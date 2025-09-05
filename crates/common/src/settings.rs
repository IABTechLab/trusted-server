use core::str;

use config::{Config, Environment, File, FileFormat};
use error_stack::{Report, ResultExt};
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize};
use serde_json::Value as JsonValue;
use url::Url;

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

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Prebid {
    pub server_url: String,
    #[serde(default = "default_account_id")]
    pub account_id: String,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
    #[serde(default = "default_bidders", deserialize_with = "vec_from_seq_or_map")]
    pub bidders: Vec<String>,
    #[serde(default = "default_auto_configure")]
    pub auto_configure: bool,
    #[serde(default)]
    pub debug: bool,
}

fn default_account_id() -> String {
    "1001".to_string()
}

fn default_timeout_ms() -> u32 {
    1000
}

fn default_bidders() -> Vec<String> {
    vec![
        "kargo".to_string(),
        "rubicon".to_string(),
        "appnexus".to_string(),
        "openx".to_string(),
    ]
}

fn default_auto_configure() -> bool {
    true
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
pub struct PartnerConfig {
    pub enabled: bool,
    pub name: String,
    pub domains_to_proxy: Vec<String>,
    pub proxy_domain: String,
    pub backend_name: String,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Partners {
    pub gam: Option<PartnerConfig>,
    pub equativ: Option<PartnerConfig>,
    pub prebid: Option<PartnerConfig>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Experimental {
    pub enable_edge_pub: bool,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Settings {
    pub ad_server: AdServer,
    pub publisher: Publisher,
    pub prebid: Prebid,
    pub gam: Gam,
    pub synthetic: Synthetic,
    pub partners: Option<Partners>,
    pub experimental: Option<Experimental>,
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

// Helper: allow Vec fields to deserialize from either a JSON array or a map of numeric indices.
// This lets env vars like TRUSTED_SERVER__PREBID__BIDDERS__0=smartadserver work, which the config env source
// represents as an object {"0": "value"} rather than a sequence. Also supports string inputs that are
// JSON arrays or comma-separated values.
fn vec_from_seq_or_map<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let v = JsonValue::deserialize(deserializer)?;
    match v {
        JsonValue::Array(arr) => arr
            .into_iter()
            .map(|item| serde_json::from_value(item).map_err(serde::de::Error::custom))
            .collect(),
        JsonValue::Object(map) => {
            let mut items: Vec<(usize, T)> = Vec::with_capacity(map.len());
            for (k, val) in map.into_iter() {
                let idx = k.parse::<usize>().map_err(|_| {
                    serde::de::Error::custom(format!("Invalid index '{}' in map for Vec field", k))
                })?;
                let parsed: T = serde_json::from_value(val).map_err(serde::de::Error::custom)?;
                items.push((idx, parsed));
            }
            items.sort_by_key(|(idx, _)| *idx);
            Ok(items.into_iter().map(|(_, v)| v).collect())
        }
        JsonValue::String(s) => {
            let txt = s.trim();
            if txt.starts_with('[') {
                serde_json::from_str::<Vec<T>>(txt).map_err(serde::de::Error::custom)
            } else {
                let parts = if txt.contains(',') {
                    txt.split(',')
                        .map(|p| p.trim())
                        .filter(|p| !p.is_empty())
                        .collect::<Vec<_>>()
                } else {
                    vec![txt]
                };
                let mut out: Vec<T> = Vec::with_capacity(parts.len());
                for p in parts {
                    let json = format!("\"{}\"", p.replace('"', "\\\""));
                    let parsed: T =
                        serde_json::from_str(&json).map_err(serde::de::Error::custom)?;
                    out.push(parsed);
                }
                Ok(out)
            }
        }
        other => Err(serde::de::Error::custom(format!(
            "expected array, map of indices, or parseable string, got {}",
            other
        ))),
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

        assert!(!settings.gam.publisher_id.is_empty());
        assert!(!settings.gam.server_url.is_empty());
        assert!(!settings.gam.ad_units.is_empty());
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
    fn test_prebid_bidders_override_with_json_env() {
        let toml_str = crate_test_settings_str();
        let env_key = format!(
            "{}{}PREBID{}BIDDERS",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

        // Ensure no external override interferes
        let origin_key = format!(
            "{}{}PUBLISHER{}ORIGIN_URL",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        temp_env::with_var(
            origin_key,
            Some("https://origin.test-publisher.com"),
            || {
                temp_env::with_var(env_key, Some("[\"smartadserver\",\"rubicon\"]"), || {
                    let res = Settings::from_toml(&toml_str);
                    if res.is_err() {
                        eprintln!("JSON override error: {:?}", res.as_ref().err());
                    }
                    let settings = res.expect("Settings should parse with JSON env override");
                    assert_eq!(
                        settings.prebid.bidders,
                        vec!["smartadserver".to_string(), "rubicon".to_string()]
                    );
                });
            },
        );
    }

    #[test]
    fn test_prebid_bidders_override_with_indexed_env() {
        let toml_str = crate_test_settings_str();

        let env_key0 = format!(
            "{}{}PREBID{}BIDDERS{}0",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let env_key1 = format!(
            "{}{}PREBID{}BIDDERS{}1",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

        // Also ensure origin_url env is a plain string (avoid any external env interference)
        let origin_key = format!(
            "{}{}PUBLISHER{}ORIGIN_URL",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        temp_env::with_var(
            origin_key,
            Some("https://origin.test-publisher.com"),
            || {
                temp_env::with_var(env_key0, Some("smartadserver"), || {
                    temp_env::with_var(env_key1, Some("openx"), || {
                        let res = Settings::from_toml(&toml_str);
                        if res.is_err() {
                            eprintln!("Indexed override error: {:?}", res.as_ref().err());
                        }
                        let settings =
                            res.expect("Settings should parse with indexed env override");
                        assert_eq!(
                            settings.prebid.bidders,
                            vec!["smartadserver".to_string(), "openx".to_string()]
                        );
                    });
                });
            },
        );
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
