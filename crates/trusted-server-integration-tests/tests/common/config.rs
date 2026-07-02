use edgezero_core::blob_envelope::BlobEnvelope;
use error_stack::Report;
use trusted_server_core::config::validate_settings_for_deploy;
use trusted_server_core::settings::Settings;

use crate::common::runtime::{TestError, TestResult};

const GENERATED_AT: &str = "2026-06-23T00:00:00Z";
const APP_CONFIG: &str = include_str!("../../fixtures/configs/trusted-server.integration.toml");

pub fn integration_app_config_envelope(origin_port: u16) -> TestResult<String> {
    let origin_url = format!("http://127.0.0.1:{origin_port}");
    let mut settings = Settings::from_toml(APP_CONFIG).map_err(|report| {
        Report::new(TestError::ConfigGeneration).attach(format!(
            "invalid Trusted Server integration config: {report:?}"
        ))
    })?;
    settings.publisher.origin_url = origin_url;
    validate_settings_for_deploy(&settings).map_err(|report| {
        Report::new(TestError::ConfigGeneration)
            .attach(format!("invalid generated integration config: {report:?}"))
    })?;

    let data = serde_json::to_value(&settings).map_err(|error| {
        Report::new(TestError::ConfigGeneration)
            .attach(format!("failed to serialize integration settings: {error}"))
    })?;
    let envelope = BlobEnvelope::new(data, GENERATED_AT.to_string());
    serde_json::to_string(&envelope).map_err(|error| {
        Report::new(TestError::ConfigGeneration).attach(format!(
            "failed to serialize integration app-config envelope: {error}"
        ))
    })
}

pub fn cloudflare_config_json(origin_port: u16) -> TestResult<String> {
    let envelope = integration_app_config_envelope(origin_port)?;
    serde_json::to_string(&serde_json::json!({ "app_config": envelope })).map_err(|error| {
        Report::new(TestError::ConfigGeneration).attach(format!(
            "failed to serialize Cloudflare config binding: {error}"
        ))
    })
}
