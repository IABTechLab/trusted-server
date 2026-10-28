//! The `[consent]` settings table, read by the permission signal modules.
//!
//! Core reads one key, `mode`, which says whether the modules decode the
//! signals they read or leave them as received. Every other key is the
//! business of the module for the scheme it concerns, which reads the table
//! into a type of its own through [`ConsentConfig::read`].

use error_stack::Report;
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::error::TrustedServerError;

/// A key the table once held and refuses now, so a document still carrying
/// it is told rather than silently kept.
const REMOVED_KEYS: &[&str] = &["consent_store"];

/// The `[consent]` table.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ConsentConfig {
    /// Whether the signal modules decode what they read.
    #[serde(default)]
    pub mode: ConsentMode,
    /// Every other key, as written, for the modules to read.
    #[serde(flatten)]
    settings: Map<String, Value>,
}

/// The table as written, before the removed keys are refused.
#[derive(Deserialize)]
struct WrittenConsentConfig {
    #[serde(default)]
    mode: ConsentMode,
    #[serde(flatten)]
    settings: Map<String, Value>,
}

impl<'de> Deserialize<'de> for ConsentConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let written = WrittenConsentConfig::deserialize(deserializer)?;
        if let Some(removed) = REMOVED_KEYS
            .iter()
            .find(|key| written.settings.contains_key(**key))
        {
            return Err(D::Error::custom(format!(
                "[consent] {removed} was removed and is no longer accepted. Remove the \
                 line, because consent is read from the request on every request and \
                 kept with the identity graph, never in a store of its own"
            )));
        }
        Ok(Self {
            mode: written.mode,
            settings: written.settings,
        })
    }
}

impl ConsentConfig {
    /// The table read as `T`, `mode` included.
    ///
    /// A module names the keys it reads in `T` and leaves the rest to the
    /// other modules, so `T` must accept keys it does not name.
    ///
    /// # Errors
    ///
    /// A key `T` names whose value is not of the type `T` expects.
    pub fn read<T: DeserializeOwned>(&self) -> Result<T, Report<TrustedServerError>> {
        let mut table = self.settings.clone();
        table.insert(
            "mode".to_owned(),
            serde_json::to_value(self.mode).unwrap_or_default(),
        );
        serde_json::from_value(Value::Object(table)).map_err(|error| {
            Report::new(TrustedServerError::Configuration {
                message: format!("[consent] could not be read: {error}"),
            })
        })
    }

    /// Whether the table carries `key`.
    #[must_use]
    pub fn has(&self, key: &str) -> bool {
        self.settings.contains_key(key)
    }
}

/// Whether the signal modules decode what they read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConsentMode {
    /// Decode each signal and act on what it says.
    #[default]
    Interpreter,
    /// Leave each signal as received and forward it without reading it.
    Proxy,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, Deserialize, PartialEq)]
    #[serde(default)]
    struct ExampleSettings {
        mode: ConsentMode,
        example_days: u32,
    }

    #[test]
    fn an_empty_table_decodes_and_reads_as_defaults() {
        let config: ConsentConfig =
            serde_json::from_str("{}").expect("should deserialize an empty table");
        assert_eq!(config.mode, ConsentMode::Interpreter);
        assert_eq!(
            config.read::<ExampleSettings>().expect("should read"),
            ExampleSettings::default()
        );
    }

    #[test]
    fn a_module_reads_its_keys_and_leaves_the_others() {
        let config: ConsentConfig = serde_json::from_value(serde_json::json!({
            "mode": "proxy",
            "example_days": 7,
            "another": { "key": true }
        }))
        .expect("should deserialize the table");
        assert_eq!(config.mode, ConsentMode::Proxy, "should parse proxy mode");
        assert!(config.has("another") && !config.has("missing"));
        assert_eq!(
            config.read::<ExampleSettings>().expect("should read"),
            ExampleSettings {
                mode: ConsentMode::Proxy,
                example_days: 7,
            }
        );
    }

    #[test]
    fn a_key_of_the_wrong_type_is_refused_naming_the_table() {
        let config: ConsentConfig =
            serde_json::from_value(serde_json::json!({ "example_days": "seven" }))
                .expect("should deserialize the table");
        let error = config
            .read::<ExampleSettings>()
            .expect_err("should refuse a string where a number is expected");
        assert!(format!("{error:?}").contains("[consent] could not be read"));
    }

    #[test]
    fn a_removed_key_is_refused_by_name() {
        let error = serde_json::from_value::<ConsentConfig>(serde_json::json!({
            "consent_store": "legacy-consent-store"
        }))
        .expect_err("should refuse the removed key");
        assert!(
            error.to_string().contains("consent_store"),
            "should name the removed key: {error}"
        );
    }

    #[test]
    fn the_table_serializes_as_written() {
        let config: ConsentConfig = serde_json::from_value(serde_json::json!({
            "mode": "proxy",
            "example_days": 7
        }))
        .expect("should deserialize the table");
        let written = serde_json::to_value(&config).expect("should serialize");
        assert_eq!(
            written,
            serde_json::json!({ "mode": "proxy", "example_days": 7 })
        );
    }
}
