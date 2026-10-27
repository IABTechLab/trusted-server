use edgezero_core::blob_envelope::BlobEnvelope;
use error_stack::Report;
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::config_payload::CONFIG_BLOB_KEY;

use crate::common::runtime::{TestError, TestResult};

const GENERATED_AT: &str = "2026-06-23T00:00:00Z";
const APP_CONFIG: &str = include_str!("../../fixtures/configs/trusted-server.integration.toml");

/// Compile fictional native credentials for the integration feature configuration.
///
/// # Errors
/// Returns a config-generation error if the fixture or native rendering is invalid.
///
/// # Examples
/// Use `integration_native_fixtures("spin")` to obtain the fixture's generated
/// Spin variable names and public test values.
pub fn integration_native_fixtures(
    adapter: &str,
) -> TestResult<Vec<trusted_server_integration_tests::NativeFixture>> {
    let config: TrustedServerAppConfig =
        toml::from_str(APP_CONFIG).map_err(|_| Report::new(TestError::ConfigGeneration))?;
    trusted_server_integration_tests::native_fixtures(&config.into_settings(), adapter)
        .map_err(|_| Report::new(TestError::ConfigGeneration))
}

pub fn integration_app_config_envelope(origin_port: u16) -> TestResult<String> {
    app_config_envelope(origin_port, |_| Ok(()))
}

/// Builds the integration app-config envelope after `customize` edits the
/// parsed fixture.
fn app_config_envelope(
    origin_port: u16,
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
    cloudflare_binding_json(&serde_json::json!({ CONFIG_BLOB_KEY: envelope }))
}

/// Builds a Cloudflare config binding with server-side auctions enabled.
///
/// The fixture's providers are removed so the enabled auction completes
/// through the orchestrator and telemetry without any outbound provider call.
pub fn cloudflare_auction_config_json(origin_port: u16) -> TestResult<String> {
    let envelope = app_config_envelope(origin_port, |table| {
        let auction = table_mut(table, "auction")?;
        auction.insert("enabled".to_string(), toml::Value::Boolean(true));
        auction.remove("providers");
        auction.remove("bidders");
        Ok(())
    })?;
    cloudflare_binding_json(&serde_json::json!({ CONFIG_BLOB_KEY: envelope }))
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
    const FASTLY_CONFIG: &str = include_str!("../../../../fastly.toml");
    const VICEROY_TEMPLATE: &str = include_str!("../../fixtures/configs/viceroy-template.toml");
    const LOGICAL_SECRET_STORE_ID: &str = "trusted_server_secrets";

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
            let config = if name == "Viceroy integration template" {
                let app: trusted_server_core::config::TrustedServerAppConfig =
                    toml::from_str(super::APP_CONFIG)
                        .expect("should deserialize the integration feature configuration");
                let blocks =
                    trusted_server_integration_tests::fastly_fixture_blocks(&app.into_settings())
                        .expect("should compile the Fastly native fixture blocks");
                let (before, rest) = config
                    .split_once("# BEGIN GENERATED APP SECRET FIXTURES")
                    .expect("should find the generated app-secret section start marker");
                let (_, after) = rest
                    .split_once("# END GENERATED APP SECRET FIXTURES")
                    .expect("should find the generated app-secret section end marker");
                format!("{before}{blocks}{after}")
            } else {
                config.to_owned()
            };
            let parsed: toml::Value =
                toml::from_str(&config).expect("should parse Fastly configuration");
            let local_server = &parsed["local_server"];

            assert!(
                local_server["secret_stores"]
                    .get(LOGICAL_SECRET_STORE_ID)
                    .is_some_and(toml::Value::is_array),
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
    fn local_fastly_config_defines_compiled_starter_credentials() {
        let parsed: toml::Value =
            toml::from_str(FASTLY_CONFIG).expect("should parse root fastly.toml");
        let entries = parsed["local_server"]["secret_stores"][LOGICAL_SECRET_STORE_ID]
            .as_array()
            .expect("fastly.toml should define trusted_server_secrets");

        let config: trusted_server_core::config::TrustedServerAppConfig =
            toml::from_str(include_str!("../../../../trusted-server.example.toml"))
                .expect("should deserialize the public example configuration");
        let fixtures =
            trusted_server_integration_tests::native_fixtures(&config.into_settings(), "fastly")
                .expect("should compile native fixtures for the public example");
        for fixture in fixtures {
            let key = fixture.native.key;
            assert!(
                entries
                    .iter()
                    .any(|entry| entry["key"].as_str() == Some(key.as_str())),
                "fastly.toml should define local value for starter secret `{key}`"
            );
        }
    }
}
