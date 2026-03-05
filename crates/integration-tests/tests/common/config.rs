use super::runtime::TestError;
use error_stack::{Result, ResultExt};
use std::io::Write;
use std::path::PathBuf;
use tempfile::NamedTempFile;

/// Builder for runtime configuration
pub struct RuntimeConfigBuilder {
    template: String,
    origin_url: Option<String>,
    integrations: Vec<String>,
    wasm_path: Option<PathBuf>,
}

impl RuntimeConfigBuilder {
    pub fn new(template: &str) -> Self {
        Self {
            template: template.to_string(),
            origin_url: None,
            integrations: Vec::new(),
            wasm_path: None,
        }
    }

    pub fn with_origin_url(mut self, url: String) -> Self {
        self.origin_url = Some(url);
        self
    }

    pub fn with_integrations(mut self, ids: Vec<&str>) -> Self {
        self.integrations = ids.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn with_wasm_path(mut self, path: PathBuf) -> Self {
        self.wasm_path = Some(path);
        self
    }

    pub fn build(self) -> Result<super::RuntimeConfig, TestError> {
        // Parse template as TOML
        let mut config: toml::Value = toml::from_str(&self.template)
            .change_context(TestError::ConfigParse)?;

        // Set origin_url if provided
        if let Some(origin_url) = self.origin_url {
            if let Some(publisher) = config.get_mut("publisher") {
                if let Some(publisher_table) = publisher.as_table_mut() {
                    publisher_table.insert(
                        "origin_url".to_string(),
                        toml::Value::String(origin_url),
                    );
                }
            }
        }

        // Enable integrations if provided
        if !self.integrations.is_empty() {
            if let Some(integrations_config) = config
                .get_mut("integrations")
                .and_then(|v| v.get_mut("config"))
                .and_then(|v| v.as_table_mut())
            {
                for integration_id in &self.integrations {
                    if let Some(integration_entry) = integrations_config.get_mut(integration_id) {
                        if let Some(table) = integration_entry.as_table_mut() {
                            table.insert("enabled".to_string(), toml::Value::Boolean(true));
                        }
                    }
                }
            }
        }

        // Write to temp file
        let mut file = NamedTempFile::new().change_context(TestError::ConfigWrite)?;
        let content =
            toml::to_string_pretty(&config).change_context(TestError::ConfigSerialize)?;
        file.write_all(content.as_bytes())
            .change_context(TestError::ConfigWrite)?;

        let config_path = file.into_temp_path().to_path_buf();
        let wasm_path = self
            .wasm_path
            .unwrap_or_else(super::runtime::wasm_binary_path);

        Ok(super::RuntimeConfig {
            config_path,
            wasm_path,
        })
    }
}
