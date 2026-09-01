use edgezero_core::blob_envelope::BlobEnvelope;
use error_stack::Report;
use trusted_server_core::config::TrustedServerAppConfig;

use crate::common::runtime::{TestError, TestResult};

const GENERATED_AT: &str = "2026-06-23T00:00:00Z";
const APP_CONFIG: &str = include_str!("../../fixtures/configs/trusted-server.integration.toml");

fn app_config_envelope(origin_port: u16, aps_proxy_fixture: bool) -> TestResult<String> {
    let origin_url = format!("http://127.0.0.1:{origin_port}");
    let app_config: TrustedServerAppConfig = toml::from_str(APP_CONFIG).map_err(|error| {
        Report::new(TestError::ConfigGeneration).attach(format!(
            "invalid Trusted Server integration config: {error}"
        ))
    })?;
    let mut settings = app_config.into_settings();
    settings.publisher.origin_url = origin_url;
    if aps_proxy_fixture {
        settings.auction.enabled = true;
        settings
            .auction
            .providers
            .retain(|_, provider| provider.profile == "aps");
        settings.auction.bidders.clear();
        settings.auction.mediator = None;
    }
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

pub fn integration_app_config_envelope(origin_port: u16) -> TestResult<String> {
    app_config_envelope(origin_port, false)
}

#[cfg(feature = "aps-runner-proxy")]
pub fn aps_runner_proxy_app_config_envelope(origin_port: u16) -> TestResult<String> {
    app_config_envelope(origin_port, true)
}

pub fn cloudflare_config_json(origin_port: u16) -> TestResult<String> {
    let envelope = integration_app_config_envelope(origin_port)?;
    serde_json::to_string(&serde_json::json!({ "app_config": envelope })).map_err(|error| {
        Report::new(TestError::ConfigGeneration).attach(format!(
            "failed to serialize Cloudflare config binding: {error}"
        ))
    })
}

#[cfg(feature = "aps-runner-proxy")]
pub fn cloudflare_aps_runner_proxy_config_json(origin_port: u16) -> TestResult<String> {
    let envelope = aps_runner_proxy_app_config_envelope(origin_port)?;
    serde_json::to_string(&serde_json::json!({ "app_config": envelope })).map_err(|error| {
        Report::new(TestError::ConfigGeneration).attach(format!(
            "failed to serialize Cloudflare APS proxy config binding: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "aps-runner-proxy")]
    use super::{aps_runner_proxy_app_config_envelope, integration_app_config_envelope};
    const FASTLY_CONFIG: &str = include_str!("../../../../fastly.toml");

    #[cfg(feature = "aps-runner-proxy")]
    #[test]
    fn aps_proxy_envelope_enables_only_its_auction_fixture() {
        let regular: serde_json::Value = serde_json::from_str(
            &integration_app_config_envelope(8888).expect("should build regular fixture envelope"),
        )
        .expect("should parse regular fixture envelope");
        let aps_proxy: serde_json::Value = serde_json::from_str(
            &aps_runner_proxy_app_config_envelope(8888)
                .expect("should build APS proxy fixture envelope"),
        )
        .expect("should parse APS proxy fixture envelope");

        assert_eq!(regular["data"]["auction"]["enabled"], false);
        assert_eq!(aps_proxy["data"]["auction"]["enabled"], true);
        assert_eq!(
            aps_proxy["data"]["auction"]["providers"]
                .as_object()
                .expect("proxy providers should be an object")
                .keys()
                .collect::<Vec<_>>(),
            ["aps-main"]
        );
        assert_eq!(
            aps_proxy["data"]["auction"]["bidders"],
            serde_json::json!({})
        );
        assert_eq!(
            regular["data"]["auction"]["providers"]["aps-main"],
            aps_proxy["data"]["auction"]["providers"]["aps-main"],
            "the proxy fixture should preserve the canonical APS provider configuration"
        );
    }

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
}
