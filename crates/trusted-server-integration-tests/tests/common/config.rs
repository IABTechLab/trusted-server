use edgezero_core::blob_envelope::BlobEnvelope;
use error_stack::Report;
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::config_payload::{CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID};

use crate::common::runtime::{TestError, TestResult};

const GENERATED_AT: &str = "2026-06-23T00:00:00Z";
const APP_CONFIG: &str = include_str!("../../fixtures/configs/trusted-server.integration.toml");

pub fn integration_app_config_envelope(
    origin_port: u16,
    public_origin: &str,
) -> TestResult<String> {
    app_config_envelope(origin_port, Some(public_origin), |_| Ok(()))
}

/// Builds the integration app-config envelope after `customize` edits the
/// parsed fixture.
fn app_config_envelope(
    origin_port: u16,
    public_origin: Option<&str>,
    customize: impl FnOnce(&mut toml::Table) -> TestResult<()>,
) -> TestResult<String> {
    let origin_url = format!("http://127.0.0.1:{origin_port}");
    let mut table: toml::Table = toml::from_str(APP_CONFIG).map_err(|error| {
        Report::new(TestError::ConfigGeneration).attach(format!(
            "invalid Trusted Server integration config: {error}"
        ))
    })?;
    customize(&mut table)?;
    let app_config: TrustedServerAppConfig =
        toml::Value::Table(table).try_into().map_err(|error| {
            Report::new(TestError::ConfigGeneration).attach(format!(
                "invalid customized Trusted Server integration config: {error}"
            ))
        })?;
    let mut settings = app_config.into_settings();
    settings.publisher.origin_url = origin_url;
    settings.publisher.public_origin = public_origin.map(str::to_owned);
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
    let envelope = app_config_envelope(origin_port, None, |_| Ok(()))?;
    cloudflare_binding_json(&serde_json::json!({ CONFIG_BLOB_KEY: envelope }))
}

/// Builds a Cloudflare config binding with server-side auctions enabled.
///
/// The fixture's providers are removed so the enabled auction completes
/// through the orchestrator and telemetry without any outbound provider call.
pub fn cloudflare_auction_config_json(origin_port: u16) -> TestResult<String> {
    let envelope = app_config_envelope(origin_port, None, |table| {
        let auction = table_mut(table, "auction")?;
        auction.insert("enabled".to_string(), toml::Value::Boolean(true));
        auction.remove("providers");
        auction.remove("bidders");
        Ok(())
    })?;
    cloudflare_binding_json(&serde_json::json!({ CONFIG_BLOB_KEY: envelope }))
}

/// Retargets an existing fixture without changing its policies or secret references.
fn envelope_with_public_origin(envelope_json: &str, public_origin: &str) -> TestResult<String> {
    let envelope: BlobEnvelope = serde_json::from_str(envelope_json)
        .map_err(|error| Report::new(TestError::ConfigGeneration).attach(error.to_string()))?;
    envelope
        .verify()
        .map_err(|error| Report::new(TestError::ConfigGeneration).attach(error.to_string()))?;
    let app_config: TrustedServerAppConfig = serde_json::from_value(envelope.data)
        .map_err(|error| Report::new(TestError::ConfigGeneration).attach(error.to_string()))?;
    let mut settings = app_config.into_settings();
    settings.publisher.public_origin = Some(public_origin.to_owned());
    let app_config = TrustedServerAppConfig::new(settings)
        .map_err(|error| Report::new(TestError::ConfigGeneration).attach(format!("{error:?}")))?;
    let data = serde_json::to_value(app_config)
        .map_err(|error| Report::new(TestError::ConfigGeneration).attach(error.to_string()))?;
    serde_json::to_string(&BlobEnvelope::new(data, envelope.generated_at))
        .map_err(|error| Report::new(TestError::ConfigGeneration).attach(error.to_string()))
}

pub fn cloudflare_config_with_public_origin(
    config_json: &str,
    public_origin: &str,
) -> TestResult<String> {
    let mut binding: serde_json::Value = serde_json::from_str(config_json)
        .map_err(|error| Report::new(TestError::ConfigGeneration).attach(error.to_string()))?;
    let envelope = binding[CONFIG_BLOB_KEY].as_str().ok_or_else(|| {
        Report::new(TestError::ConfigGeneration).attach("missing Cloudflare app-config blob")
    })?;
    binding[CONFIG_BLOB_KEY] =
        serde_json::Value::String(envelope_with_public_origin(envelope, public_origin)?);
    cloudflare_binding_json(&binding)
}

pub fn viceroy_config_with_public_origin(config: &str, public_origin: &str) -> TestResult<String> {
    let mut config: toml::Value = toml::from_str(config)
        .map_err(|error| Report::new(TestError::ConfigGeneration).attach(error.to_string()))?;
    let contents = config
        .get_mut("local_server")
        .and_then(|value| value.get_mut("config_stores"))
        .and_then(|value| value.get_mut(DEFAULT_CONFIG_STORE_ID))
        .and_then(|value| value.get_mut("contents"))
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| {
            Report::new(TestError::ConfigGeneration).attach("missing Viceroy app-config store")
        })?;
    let envelope = contents
        .get(CONFIG_BLOB_KEY)
        .and_then(toml::Value::as_str)
        .ok_or_else(|| {
            Report::new(TestError::ConfigGeneration).attach("missing Viceroy app-config blob")
        })?;
    contents[CONFIG_BLOB_KEY] =
        toml::Value::String(envelope_with_public_origin(envelope, public_origin)?);
    toml::to_string(&config)
        .map_err(|error| Report::new(TestError::ConfigGeneration).attach(error.to_string()))
}

fn table_mut<'a>(table: &'a mut toml::Table, key: &str) -> TestResult<&'a mut toml::Table> {
    table
        .get_mut(key)
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| {
            Report::new(TestError::ConfigGeneration).attach(format!(
                "integration config should contain a `[{key}]` table"
            ))
        })
}

fn cloudflare_binding_json(binding: &serde_json::Value) -> TestResult<String> {
    serde_json::to_string(binding).map_err(|error| {
        Report::new(TestError::ConfigGeneration).attach(format!(
            "failed to serialize Cloudflare config binding: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FASTLY_CONFIG: &str = include_str!("../../../../fastly.toml");
    const VICEROY_TEMPLATE: &str = include_str!("../../fixtures/configs/viceroy-template.toml");
    const LOGICAL_SECRET_STORE_ID: &str = "trusted_server_secrets";

    #[test]
    fn integration_config_uses_each_runtime_origin_without_changing_upstream() {
        for port in [43121, 43122] {
            let public_origin = format!("http://127.0.0.1:{port}");
            let json = integration_app_config_envelope(9999, &public_origin)
                .expect("should build runtime-specific config");
            let envelope: BlobEnvelope =
                serde_json::from_str(&json).expect("should parse runtime-specific envelope");
            envelope
                .verify()
                .expect("should verify runtime-specific envelope");

            assert_eq!(
                envelope.data["publisher"]["public_origin"], public_origin,
                "should use the selected runtime listener"
            );
            assert_eq!(
                envelope.data["publisher"]["origin_url"], "http://127.0.0.1:9999",
                "should keep the upstream separate from the runtime listener"
            );
        }
    }

    #[test]
    fn cloudflare_runtime_origin_preserves_custom_auction_config() {
        let original = cloudflare_auction_config_json(9999).expect("should build auction config");
        let retargeted = cloudflare_config_with_public_origin(&original, "http://127.0.0.1:43123")
            .expect("should retarget auction config");
        let original: serde_json::Value =
            serde_json::from_str(&original).expect("should parse original binding");
        let retargeted: serde_json::Value =
            serde_json::from_str(&retargeted).expect("should parse retargeted binding");
        let mut before: BlobEnvelope = serde_json::from_str(
            original[CONFIG_BLOB_KEY]
                .as_str()
                .expect("should contain original blob"),
        )
        .expect("should parse original blob");
        let after: BlobEnvelope = serde_json::from_str(
            retargeted[CONFIG_BLOB_KEY]
                .as_str()
                .expect("should contain retargeted blob"),
        )
        .expect("should parse retargeted blob");
        after.verify().expect("should verify retargeted blob");
        before.data["publisher"]["public_origin"] = serde_json::json!("http://127.0.0.1:43123");
        assert_eq!(
            after.data, before.data,
            "should change only the runtime public origin"
        );
        assert_eq!(
            after.data["auction"]["enabled"], true,
            "should preserve enabled auctions"
        );
    }

    #[test]
    fn viceroy_runtime_origin_preserves_resources_and_config_policy() {
        let envelope = app_config_envelope(9999, None, |_| Ok(()))
            .expect("should build legacy config without a public origin");
        let stores = format!(
            "[local_server.config_stores.{DEFAULT_CONFIG_STORE_ID}]\nformat = \"inline-toml\"\n[local_server.config_stores.{DEFAULT_CONFIG_STORE_ID}.contents]\n{CONFIG_BLOB_KEY} = '''{envelope}'''"
        );
        let config = VICEROY_TEMPLATE.replace("# GENERATED_TRUSTED_SERVER_CONFIG_STORES", &stores);
        let original: toml::Value = toml::from_str(&config).expect("should parse original config");
        let retargeted = viceroy_config_with_public_origin(&config, "http://127.0.0.1:43124")
            .expect("should retarget Viceroy config");
        let mut retargeted: toml::Value =
            toml::from_str(&retargeted).expect("should parse retargeted Viceroy config");
        let contents =
            &mut retargeted["local_server"]["config_stores"][DEFAULT_CONFIG_STORE_ID]["contents"];
        let after: BlobEnvelope = serde_json::from_str(
            contents[CONFIG_BLOB_KEY]
                .as_str()
                .expect("should contain app-config blob"),
        )
        .expect("should parse retargeted blob");
        after.verify().expect("should verify retargeted blob");
        let mut before: BlobEnvelope =
            serde_json::from_str(&envelope).expect("should parse original blob");
        before.data["publisher"]["public_origin"] = serde_json::json!("http://127.0.0.1:43124");
        assert_eq!(
            after.data, before.data,
            "should preserve upstream and config policy"
        );
        contents[CONFIG_BLOB_KEY] = toml::Value::String(envelope);
        assert_eq!(
            retargeted, original,
            "should preserve all other Viceroy resources"
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

    #[test]
    fn local_fastly_secret_store_is_exposed_under_its_logical_id() {
        for (name, config) in [
            ("fastly.toml", FASTLY_CONFIG),
            ("Viceroy integration template", VICEROY_TEMPLATE),
        ] {
            let parsed: toml::Value =
                toml::from_str(config).expect("should parse Fastly configuration");
            let local_server = &parsed["local_server"];

            assert!(
                local_server["secret_stores"][LOGICAL_SECRET_STORE_ID].is_array(),
                "{name} should expose the secret store under its logical ID"
            );
            assert!(
                local_server["secret_stores"].get("ts_secrets").is_none(),
                "{name} should not define the legacy physical secret store"
            );
            assert!(
                local_server["config_stores"]
                    .get("edgezero_runtime_env")
                    .is_none(),
                "{name} should not define the legacy runtime selector store"
            );
        }
    }

    #[test]
    fn local_fastly_config_defines_starter_secret_references() {
        let parsed: toml::Value =
            toml::from_str(FASTLY_CONFIG).expect("should parse root fastly.toml");
        let entries = parsed["local_server"]["secret_stores"][LOGICAL_SECRET_STORE_ID]
            .as_array()
            .expect("fastly.toml should define trusted_server_secrets");

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
