use core::str;

use config::{Config, Environment, File, FileFormat};
use error_stack::{Report, ResultExt};
use regex::Regex;
use serde::{
    de::{DeserializeOwned, IntoDeserializer},
    Deserialize, Deserializer, Serialize,
};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::OnceLock;
use url::Url;
use validator::{Validate, ValidationError};

use crate::error::TrustedServerError;

pub const ENVIRONMENT_VARIABLE_PREFIX: &str = "TRUSTED_SERVER";
pub const ENVIRONMENT_VARIABLE_SEPARATOR: &str = "__";

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
pub struct Publisher {
    pub domain: String,
    pub cookie_domain: String,
    pub origin_url: String,
    /// Secret used to encrypt/decrypt proxied URLs in `/first-party/proxy`.
    /// Keep this secret stable to allow existing links to decode.
    pub proxy_secret: String,
    #[serde(default)]
    #[validate(nested)]
    pub nextjs: NextJs,
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
    ///     proxy_secret: "proxy-secret".to_string(),
    ///     nextjs: Default::default(),
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
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
    #[serde(default = "default_bidders", deserialize_with = "vec_from_seq_or_map")]
    pub bidders: Vec<String>,
    #[serde(default = "default_auto_configure")]
    pub auto_configure: bool,
    #[serde(default)]
    pub debug: bool,
}

fn default_timeout_ms() -> u32 {
    1000
}

fn default_bidders() -> Vec<String> {
    vec!["mocktioneer".to_string()]
}

fn default_auto_configure() -> bool {
    true
}

#[derive(Debug, Deserialize, Serialize, Validate)]
pub struct NextJs {
    #[serde(default)]
    pub enabled: bool,
    #[serde(
        default = "default_nextjs_attributes",
        deserialize_with = "deserialize_nextjs_attributes"
    )]
    pub rewrite_attributes: Vec<String>,
}

fn default_nextjs_attributes() -> Vec<String> {
    vec!["href".to_string(), "link".to_string(), "url".to_string()]
}

impl Default for NextJs {
    fn default() -> Self {
        Self {
            enabled: false,
            rewrite_attributes: default_nextjs_attributes(),
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct IntegrationSettings {
    #[serde(flatten)]
    entries: HashMap<String, JsonValue>,
}

pub trait IntegrationConfig: DeserializeOwned + Validate {
    fn is_enabled(&self) -> bool;
}

impl IntegrationSettings {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn insert_config<T>(
        &mut self,
        integration_id: impl Into<String>,
        value: &T,
    ) -> Result<(), Report<TrustedServerError>>
    where
        T: Serialize,
    {
        let json =
            serde_json::to_value(value).change_context(TrustedServerError::Configuration {
                message: "Failed to serialize integration configuration".to_string(),
            })?;
        self.entries.insert(integration_id.into(), json);
        Ok(())
    }

    fn normalize_env_value(value: JsonValue) -> JsonValue {
        match value {
            JsonValue::Object(map) => JsonValue::Object(
                map.into_iter()
                    .map(|(key, val)| (key, Self::normalize_env_value(val)))
                    .collect(),
            ),
            JsonValue::Array(items) => {
                JsonValue::Array(items.into_iter().map(Self::normalize_env_value).collect())
            }
            JsonValue::String(raw) => {
                if let Ok(parsed) = serde_json::from_str::<JsonValue>(&raw) {
                    parsed
                } else {
                    JsonValue::String(raw)
                }
            }
            other => other,
        }
    }

    pub fn get_typed<T>(
        &self,
        integration_id: &str,
    ) -> Result<Option<T>, Report<TrustedServerError>>
    where
        T: IntegrationConfig,
    {
        let raw = match self.entries.get(integration_id) {
            Some(value) => value,
            None => return Ok(None),
        };

        let normalized = Self::normalize_env_value(raw.clone());

        let config: T = serde_json::from_value(normalized).change_context(
            TrustedServerError::Configuration {
                message: format!(
                    "Integration '{integration_id}' configuration could not be parsed"
                ),
            },
        )?;

        config.validate().map_err(|err| {
            Report::new(TrustedServerError::Configuration {
                message: format!(
                    "Integration '{integration_id}' configuration failed validation: {err}"
                ),
            })
        })?;

        if !config.is_enabled() {
            return Ok(None);
        }

        Ok(Some(config))
    }
}

impl Deref for IntegrationSettings {
    type Target = HashMap<String, JsonValue>;

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl DerefMut for IntegrationSettings {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.entries
    }
}

fn deserialize_nextjs_attributes<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<JsonValue>::deserialize(deserializer)?;
    match value {
        Some(json) => vec_from_seq_or_map(json.into_deserializer())
            .map_err(<D::Error as serde::de::Error>::custom),
        None => Ok(default_nextjs_attributes()),
    }
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
    pub fn validate_secret_key(secret_key: &str) -> Result<(), ValidationError> {
        match secret_key {
            "secret_key" => Err(ValidationError::new("Secret key is not valid")),
            _ => Ok(()),
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
pub struct Rewrite {
    /// List of domains to exclude from rewriting. Supports wildcards (e.g., "*.example.com").
    /// URLs from these domains will not be proxied through first-party endpoints.
    #[serde(default)]
    pub exclude_domains: Vec<String>,
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
pub struct ScriptOverrides {
    /// List of URL paths that should return empty JavaScript responses.
    /// Supports exact path matching only.
    #[serde(default)]
    pub paths: Vec<String>,
}

impl Rewrite {
    /// Checks if a URL should be excluded from rewriting based on domain matching
    #[allow(dead_code)]
    pub fn is_excluded(&self, url: &str) -> bool {
        // Parse URL to extract host
        let Ok(parsed) = url::Url::parse(url) else {
            return false;
        };

        let host = parsed.host_str().unwrap_or("");

        // Check exact domain matches (with wildcard support)
        for domain in &self.exclude_domains {
            if let Some(suffix) = domain.strip_prefix("*.") {
                // Wildcard: *.example.com matches both example.com and sub.example.com
                if host == suffix || host.ends_with(&format!(".{}", suffix)) {
                    return true;
                }
            } else if host == domain {
                return true;
            }
        }

        false
    }
}

impl ScriptOverrides {
    /// Checks if a path should be overridden with an empty script
    #[allow(dead_code)]
    pub fn is_overridden(&self, path: &str) -> bool {
        self.paths.iter().any(|p| p == path)
    }
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
pub struct Handler {
    #[validate(length(min = 1), custom(function = validate_path))]
    pub path: String,
    #[validate(length(min = 1))]
    pub username: String,
    #[validate(length(min = 1))]
    pub password: String,
    #[serde(skip, default)]
    #[validate(skip)]
    regex: OnceLock<Regex>,
}

impl Handler {
    fn compiled_regex(&self) -> &Regex {
        self.regex.get_or_init(|| {
            Regex::new(&self.path).expect("configuration validation should ensure regex compiles")
        })
    }

    pub fn matches_path(&self, path: &str) -> bool {
        self.compiled_regex().is_match(path)
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct RequestSigning {
    #[serde(default = "default_request_signing_enabled")]
    pub enabled: bool,
    pub config_store_id: String,
    pub secret_store_id: String,
}

fn default_request_signing_enabled() -> bool {
    false
}

#[derive(Debug, Default, Deserialize, Serialize, Validate)]
pub struct Settings {
    #[validate(nested)]
    pub publisher: Publisher,
    #[validate(nested)]
    pub prebid: Prebid,
    #[serde(default)]
    #[validate(nested)]
    pub synthetic: Synthetic,
    #[serde(default)]
    pub integrations: IntegrationSettings,
    #[serde(default, deserialize_with = "vec_from_seq_or_map")]
    #[validate(nested)]
    pub handlers: Vec<Handler>,
    #[serde(default)]
    pub response_headers: HashMap<String, String>,
    pub request_signing: Option<RequestSigning>,
    #[serde(default)]
    #[validate(nested)]
    pub rewrite: Rewrite,
    #[serde(default)]
    #[validate(nested)]
    pub script_overrides: ScriptOverrides,
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

    #[must_use]
    pub fn handler_for_path(&self, path: &str) -> Option<&Handler> {
        self.handlers
            .iter()
            .find(|handler| handler.matches_path(path))
    }

    pub fn integration_config<T>(
        &self,
        integration_id: &str,
    ) -> Result<Option<T>, Report<TrustedServerError>>
    where
        T: IntegrationConfig,
    {
        self.integrations.get_typed(integration_id)
    }
}

fn validate_path(value: &str) -> Result<(), ValidationError> {
    Regex::new(value).map(|_| ()).map_err(|err| {
        let mut validation_error = ValidationError::new("invalid_regex");
        validation_error.add_param("value".into(), &value);
        validation_error.add_param("message".into(), &err.to_string());
        validation_error
    })
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

    use crate::test_support::tests::{crate_test_settings_str, create_test_settings};

    #[test]
    fn test_settings_new() {
        // Test that Settings::new() loads successfully
        let settings = Settings::new();
        assert!(settings.is_ok(), "Settings should load from embedded TOML");

        let settings = settings.unwrap();

        assert!(!settings.publisher.domain.is_empty());
        assert!(!settings.publisher.cookie_domain.is_empty());
        assert!(!settings.publisher.origin_url.is_empty());

        assert!(!settings.prebid.server_url.is_empty());
        assert!(
            !settings.publisher.nextjs.enabled,
            "Next.js URL rewriting should default to disabled"
        );
        assert_eq!(
            settings.publisher.nextjs.rewrite_attributes,
            vec!["href".to_string(), "link".to_string(), "url".to_string()],
            "Next.js rewrite attributes should default to href/link/url"
        );

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
            settings.prebid.server_url,
            "https://test-prebid.com/openrtb2/auction"
        );
        assert!(
            !settings.publisher.nextjs.enabled,
            "Next.js URL rewriting should default to disabled"
        );
        assert_eq!(
            settings.publisher.nextjs.rewrite_attributes,
            vec!["href".to_string(), "link".to_string(), "url".to_string()],
            "Next.js rewrite attributes should default to href/link/url"
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

        settings.validate().expect("Failed to validate settings");
    }

    #[test]
    fn test_settings_missing_required_fields() {
        let re = Regex::new(r"origin_url = .*").unwrap();
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
        let re = Regex::new(r"\[publisher\]").unwrap();
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
    fn test_handlers_override_with_env() {
        let toml_str = crate_test_settings_str();

        let origin_key = format!(
            "{}{}PUBLISHER{}ORIGIN_URL",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let path_key = format!(
            "{}{}HANDLERS{}0{}PATH",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let username_key = format!(
            "{}{}HANDLERS{}0{}USERNAME",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );
        let password_key = format!(
            "{}{}HANDLERS{}0{}PASSWORD",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

        temp_env::with_var(
            origin_key,
            Some("https://origin.test-publisher.com"),
            || {
                temp_env::with_var(path_key, Some("^/env-handler"), || {
                    temp_env::with_var(username_key, Some("env-user"), || {
                        temp_env::with_var(password_key, Some("env-pass"), || {
                            let settings = Settings::from_toml(&toml_str)
                                .expect("Settings should load from env");
                            assert_eq!(settings.handlers.len(), 1);
                            let handler = &settings.handlers[0];
                            assert_eq!(handler.path, "^/env-handler");
                            assert_eq!(handler.username, "env-user");
                            assert_eq!(handler.password, "env-pass");
                        });
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
        temp_env::with_var(
            format!(
                "{}{}PUBLISHER{}ORIGIN_URL",
                ENVIRONMENT_VARIABLE_PREFIX,
                ENVIRONMENT_VARIABLE_SEPARATOR,
                ENVIRONMENT_VARIABLE_SEPARATOR
            ),
            Some("https://change-publisher.com"),
            || {
                let settings = Settings::from_toml(&crate_test_settings_str());

                assert!(settings.is_ok(), "Settings should load from embedded TOML");
                assert_eq!(
                    settings.unwrap().publisher.origin_url,
                    "https://change-publisher.com"
                );
            },
        );
    }

    #[test]
    fn test_override_env() {
        let toml_str = crate_test_settings_str();

        temp_env::with_var(
            format!(
                "{}{}PUBLISHER{}ORIGIN_URL",
                ENVIRONMENT_VARIABLE_PREFIX,
                ENVIRONMENT_VARIABLE_SEPARATOR,
                ENVIRONMENT_VARIABLE_SEPARATOR
            ),
            Some("https://change-publisher.com"),
            || {
                let settings = Settings::from_toml(&toml_str);

                assert!(settings.is_ok(), "Settings should load from embedded TOML");
                assert_eq!(
                    settings.unwrap().publisher.origin_url,
                    "https://change-publisher.com"
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
            origin_url: "https://origin.example.com:8080".to_string(),
            proxy_secret: "test-secret".to_string(),
            nextjs: NextJs::default(),
        };
        assert_eq!(publisher.origin_host(), "origin.example.com:8080");

        // Test with URL without port (default HTTPS port)
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "https://origin.example.com".to_string(),
            proxy_secret: "test-secret".to_string(),
            nextjs: NextJs::default(),
        };
        assert_eq!(publisher.origin_host(), "origin.example.com");

        // Test with HTTP URL with explicit port
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "http://localhost:9090".to_string(),
            proxy_secret: "test-secret".to_string(),
            nextjs: NextJs::default(),
        };
        assert_eq!(publisher.origin_host(), "localhost:9090");

        // Test with URL without protocol (fallback to original)
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "localhost:9090".to_string(),
            proxy_secret: "test-secret".to_string(),
            nextjs: NextJs::default(),
        };
        assert_eq!(publisher.origin_host(), "localhost:9090");

        // Test with IPv4 address
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "http://192.168.1.1:8080".to_string(),
            proxy_secret: "test-secret".to_string(),
            nextjs: NextJs::default(),
        };
        assert_eq!(publisher.origin_host(), "192.168.1.1:8080");

        // Test with IPv6 address
        let publisher = Publisher {
            domain: "example.com".to_string(),
            cookie_domain: ".example.com".to_string(),
            origin_url: "http://[::1]:8080".to_string(),
            proxy_secret: "test-secret".to_string(),
            nextjs: NextJs::default(),
        };
        assert_eq!(publisher.origin_host(), "[::1]:8080");
    }

    #[test]
    fn test_integration_settings_from_env() {
        use crate::integrations::testlight::TestlightConfig;

        let toml_str = crate_test_settings_str();

        let origin_key = format!(
            "{}{}PUBLISHER{}ORIGIN_URL",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

        let integration_prefix = format!(
            "{}{}INTEGRATIONS{}TESTLIGHT{}",
            ENVIRONMENT_VARIABLE_PREFIX,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR,
            ENVIRONMENT_VARIABLE_SEPARATOR
        );

        let endpoint_key = format!("{}ENDPOINT", integration_prefix);
        let timeout_key = format!("{}TIMEOUT_MS", integration_prefix);
        let rewrite_key = format!("{}REWRITE_SCRIPTS", integration_prefix);
        let enabled_key = format!("{}ENABLED", integration_prefix);

        temp_env::with_var(
            origin_key,
            Some("https://origin.test-publisher.com"),
            || {
                temp_env::with_var(
                    endpoint_key,
                    Some("https://testlight-env.test/auction"),
                    || {
                        temp_env::with_var(timeout_key, Some("2500"), || {
                            temp_env::with_var(rewrite_key, Some("true"), || {
                                temp_env::with_var(enabled_key, Some("true"), || {
                                    let settings = Settings::from_toml(&toml_str)
                                        .expect("Settings should load");

                                    let config = settings
                                        .integration_config::<TestlightConfig>("testlight")
                                        .expect("integration parsing should succeed")
                                        .expect("integration should be enabled");

                                    assert_eq!(
                                        config.endpoint,
                                        "https://testlight-env.test/auction"
                                    );
                                    assert_eq!(config.timeout_ms, 2500);
                                    assert!(config.rewrite_scripts);
                                    assert!(config.enabled);
                                });
                            });
                        });
                    },
                );
            },
        );
    }

    #[test]
    fn test_disabled_integration_does_not_register() {
        use crate::integrations::testlight::TestlightConfig;
        use serde_json::json;

        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "testlight",
                &json!({
                    "enabled": false,
                    "endpoint": "https://testlight.test/auction",
                    "rewrite_scripts": true,
                }),
            )
            .expect("should insert integration config");

        let config = settings
            .integration_config::<TestlightConfig>("testlight")
            .expect("integration parsing should succeed");

        assert!(config.is_none(), "Disabled integrations should be skipped");
    }

    #[test]
    fn test_rewrite_is_excluded() {
        let rewrite = Rewrite {
            exclude_domains: vec!["cdn.example.com".to_string(), "*.example2.com".to_string()],
        };

        // Exact domain match
        assert!(rewrite.is_excluded("http://cdn.example.com/image.png"));

        // Wildcard match - base domain
        assert!(rewrite.is_excluded("https://example2.com/cdn.js"));
        // Wildcard match - subdomains
        assert!(rewrite.is_excluded("https://cdnjs.example2.com/lib.js"));
        assert!(rewrite.is_excluded("https://sub.domain.example2.com/asset.js"));

        // Should NOT match
        assert!(!rewrite.is_excluded("https://other.example.com/asset.js"));
        assert!(!rewrite.is_excluded("https://sub.cdn.example.com/asset.js"));
        assert!(!rewrite.is_excluded("https://example2.com.fake.com/asset.js"));
        assert!(!rewrite.is_excluded("https://notexample.com/asset.js"));

        // Invalid URLs should not crash and should return false
        assert!(!rewrite.is_excluded("not a url"));
        assert!(!rewrite.is_excluded(""));
    }

    #[test]
    fn test_script_overrides_is_overridden() {
        let overrides = ScriptOverrides {
            paths: vec![
                "/.static/prebid/1.0.8/prebid.min.js".to_string(),
                "/js/analytics.js".to_string(),
            ],
        };

        // Should match exact paths
        assert!(overrides.is_overridden("/.static/prebid/1.0.8/prebid.min.js"));
        assert!(overrides.is_overridden("/js/analytics.js"));

        // Should not match similar but different paths
        assert!(!overrides.is_overridden("/.static/prebid/1.0.9/prebid.min.js"));
        assert!(!overrides.is_overridden("/js/analytics.min.js"));
        assert!(!overrides.is_overridden("/other/path.js"));
        assert!(!overrides.is_overridden(""));
    }

    #[test]
    fn test_script_overrides_empty() {
        let overrides = ScriptOverrides { paths: vec![] };

        assert!(!overrides.is_overridden("/.static/prebid/1.0.8/prebid.min.js"));
        assert!(!overrides.is_overridden("/js/analytics.js"));
    }

    #[test]
    fn test_script_overrides_from_toml() {
        let toml_str = r#"
[publisher]
domain = "test-publisher.com"
cookie_domain = ".test-publisher.com"
origin_url = "https://origin.test-publisher.com"
proxy_secret = "test-secret"

[prebid]
server_url = "https://test-prebid.com/openrtb2/auction"

[synthetic]
counter_store = "test-counter-store"
opid_store = "test-opid-store"
secret_key = "test-secret-key"
template = "{{client_ip}}:{{user_agent}}"

[script_overrides]
paths = [
    "/.static/prebid/1.0.8/prebid.min.js",
    "/js/old-analytics.js"
]
"#;

        let settings = Settings::from_toml(toml_str).expect("should parse TOML");

        assert_eq!(settings.script_overrides.paths.len(), 2);
        assert!(settings
            .script_overrides
            .is_overridden("/.static/prebid/1.0.8/prebid.min.js"));
        assert!(settings
            .script_overrides
            .is_overridden("/js/old-analytics.js"));
        assert!(!settings
            .script_overrides
            .is_overridden("/js/other-script.js"));
    }

    #[test]
    fn test_script_overrides_default_empty() {
        let toml_str = crate_test_settings_str();
        let settings = Settings::from_toml(&toml_str).expect("should parse TOML");

        // When not specified, script_overrides should default to empty
        assert_eq!(settings.script_overrides.paths.len(), 0);
        assert!(!settings
            .script_overrides
            .is_overridden("/.static/prebid/1.0.8/prebid.min.js"));
    }
}
