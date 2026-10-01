use edgezero_core::blob_envelope::BlobEnvelope;
use error_stack::Report;
use trusted_server_core::config::TrustedServerAppConfig;

use crate::common::runtime::{TestError, TestResult};

const GENERATED_AT: &str = "2026-06-23T00:00:00Z";
const APP_CONFIG: &str = include_str!("../../fixtures/configs/trusted-server.integration.toml");

pub fn integration_app_config_envelope(origin_port: u16) -> TestResult<String> {
    let origin_url = format!("http://127.0.0.1:{origin_port}");
    let app_config: TrustedServerAppConfig = toml::from_str(APP_CONFIG).map_err(|error| {
        Report::new(TestError::ConfigGeneration).attach(format!(
            "invalid Trusted Server integration config: {error}"
        ))
    })?;
    let mut settings = app_config.into_settings();
    settings.publisher.origin_url = origin_url;
    let app_config = TrustedServerAppConfig::new(settings).map_err(|report| {
        Report::new(TestError::ConfigGeneration)
            .attach(format!("invalid generated integration config: {report:?}"))
    })?;

    let data = serde_json::to_value(&app_config).map_err(|error| {
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

#[cfg(test)]
mod tests {
    const FASTLY_CONFIG: &str = include_str!("../../../../fastly.toml");
    const VICEROY_TEMPLATE: &str = include_str!("../../fixtures/configs/viceroy-template.toml");
    const VICEROY_SECRET_STORE_MAPPING_KEY: &str =
        "EDGEZERO__SERVICES__0000000000000000000000__STORES__SECRETS__TRUSTED_SERVER_SECRETS__NAME";

    #[test]
    fn local_fastly_config_defines_runtime_kv_stores() {
        let parsed: toml::Value =
            toml::from_str(FASTLY_CONFIG).expect("should parse root fastly.toml");
        let stores = &parsed["local_server"]["kv_stores"];

        assert!(
            stores["counter_store"].is_array(),
            "fastly.toml should define counter_store for batch-sync rate limiting"
        );
        let ec_entries = stores["ec_identity_store"]
            .as_array()
            .expect("fastly.toml should define ec_identity_store");
        assert!(
            ec_entries.iter().any(|entry| {
                entry["key"].as_str()
                    == Some(
                        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.test01",
                    )
            }),
            "fastly.toml should preserve the pre-seeded local EC test row"
        );
    }

    #[test]
    fn local_fastly_secret_store_mapping_is_service_scoped() {
        for (name, config) in [
            ("fastly.toml", FASTLY_CONFIG),
            ("Viceroy integration template", VICEROY_TEMPLATE),
        ] {
            let parsed: toml::Value =
                toml::from_str(config).expect("should parse Fastly configuration");
            let runtime_env =
                &parsed["local_server"]["config_stores"]["edgezero_runtime_env"]["contents"];

            assert_eq!(
                runtime_env[VICEROY_SECRET_STORE_MAPPING_KEY].as_str(),
                Some("ts_secrets"),
                "{name} should scope the secret-store mapping to Viceroy's service ID"
            );
            assert!(
                runtime_env
                    .get("EDGEZERO__STORES__SECRETS__TRUSTED_SERVER_SECRETS__NAME")
                    .is_none(),
                "{name} should not define the ignored unscoped secret-store mapping"
            );
        }
    }

    #[test]
    fn local_fastly_config_defines_starter_secret_references() {
        let parsed: toml::Value =
            toml::from_str(FASTLY_CONFIG).expect("should parse root fastly.toml");
        let entries = parsed["local_server"]["secret_stores"]["ts_secrets"]
            .as_array()
            .expect("fastly.toml should define ts_secrets");

        for key in [
            "publisher_proxy_secret",
            "ec_passphrase",
            "handler_password",
        ] {
            assert!(
                entries
                    .iter()
                    .any(|entry| entry["key"].as_str() == Some(key)),
                "fastly.toml should define local value for starter secret `{key}`"
            );
        }
    }
}
