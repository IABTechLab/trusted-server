use edgezero_core::blob_envelope::BlobEnvelope;
use error_stack::Report;
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::config_payload::CONFIG_BLOB_KEY;

use crate::common::runtime::{TestError, TestResult};

const GENERATED_AT: &str = "2026-06-23T00:00:00Z";
const APP_CONFIG: &str = include_str!("../../fixtures/configs/trusted-server.integration.toml");

/// Recover feature settings from the exact customized runtime envelope.
///
/// # Errors
/// Returns a fixture-generation error for invalid envelopes or feature config.
pub fn fixture_settings(envelope: &str) -> TestResult<trusted_server_core::settings::Settings> {
    let envelope: BlobEnvelope =
        serde_json::from_str(envelope).map_err(|_| Report::new(TestError::ConfigGeneration))?;
    envelope
        .verify()
        .map_err(|_| Report::new(TestError::ConfigGeneration))?;
    let config: TrustedServerAppConfig = serde_json::from_value(envelope.into_data())
        .map_err(|_| Report::new(TestError::ConfigGeneration))?;
    Ok(config.into_settings())
}

/// Render credentials for this exact envelope, including customized handlers.
///
/// # Errors
/// Returns a fixture-generation error when compilation or rendering fails.
pub fn app_config_native_fixtures(
    envelope: &str,
    adapter: &str,
) -> TestResult<Vec<trusted_server_integration_tests::NativeFixture>> {
    trusted_server_integration_tests::native_fixtures(&fixture_settings(envelope)?, adapter)
        .map_err(|_| Report::new(TestError::ConfigGeneration))
}

pub fn integration_app_config_envelope(origin_port: u16) -> TestResult<String> {
    app_config_envelope(origin_port, false, |_| Ok(()))
}

/// Generate an isolated runtime fixture with an explicit trace feature setting.
///
/// # Errors
///
/// Returns a fixture-generation error when configuration validation fails.
pub fn integration_app_config_envelope_with_trace(
    origin_port: u16,
    trace: bool,
) -> TestResult<String> {
    app_config_envelope(origin_port, trace, |_| Ok(()))
}

/// Builds the integration app-config envelope after `customize` edits the
/// parsed fixture.
fn app_config_envelope(
    origin_port: u16,
    trace: bool,
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
    if trace {
        settings
            .integrations
            .insert_config(
                "gpt_diagnostics",
                &serde_json::json!({"enabled":true,"trace_page_enabled":true}),
            )
            .map_err(|report| {
                Report::new(TestError::ConfigGeneration)
                    .attach(format!("invalid trace fixture config: {report:?}"))
            })?;
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

pub fn cloudflare_config_json(origin_port: u16) -> TestResult<String> {
    let envelope = integration_app_config_envelope(origin_port)?;
    cloudflare_binding_json(&serde_json::json!({ CONFIG_BLOB_KEY: envelope }))
}

/// Builds a Cloudflare config binding with server-side auctions enabled.
///
/// The fixture's providers are removed so the enabled auction completes
/// through the orchestrator and telemetry without any outbound provider call.
pub fn cloudflare_auction_config_json(origin_port: u16) -> TestResult<String> {
    let envelope = app_config_envelope(origin_port, false, |table| {
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
    use super::*;

    #[test]
    fn customized_handlers_get_their_own_native_credentials_on_every_adapter() {
        let baseline = integration_app_config_envelope(8888).expect("should generate baseline");
        let customized = app_config_envelope(8888, true, |table| {
            table
                .get_mut("handlers")
                .and_then(toml::Value::as_array_mut)
                .expect("should retain handler array")
                .insert(
                    0,
                    toml::Value::Table(toml::Table::from_iter([
                        (
                            "path".to_owned(),
                            toml::Value::String("^/_ts/trace".to_owned()),
                        ),
                        (
                            "username".to_owned(),
                            toml::Value::String("example-user".to_owned()),
                        ),
                    ])),
                );
            Ok(())
        })
        .expect("should generate customized handler features");
        for adapter in ["axum", "fastly", "cloudflare", "spin"] {
            let baseline =
                app_config_native_fixtures(&baseline, adapter).expect("should render baseline");
            let customized = app_config_native_fixtures(&customized, adapter)
                .expect("should render exact customized config");
            assert_eq!(customized.len(), baseline.len() + 1);
            let added = customized
                .iter()
                .filter(|entry| {
                    !baseline
                        .iter()
                        .any(|old| old.native.native_name == entry.native.native_name)
                })
                .collect::<Vec<_>>();
            assert_eq!(added.len(), 1);
            assert_eq!(added[0].descriptor_id, "handler.password");
            assert_eq!(added[0].value, "integration-admin-password-32-bytes-ok");
        }
    }

    #[test]
    fn trace_browser_fixture_keeps_auctions_and_shared_assembly_disabled() {
        let envelope: serde_json::Value = serde_json::from_str(
            &integration_app_config_envelope_with_trace(8888, true)
                .expect("should generate normal-browser trace fixture"),
        )
        .expect("should parse trace browser envelope");
        assert_eq!(
            envelope["data"]["auction"]["enabled"], false,
            "should keep the normal browser workflow independent of bidder transport"
        );
        assert!(
            envelope["data"]
                .get("creative_opportunities")
                .is_none_or(serde_json::Value::is_null),
            "should keep shared assembly and configured server slots out of this fixture"
        );
        assert_eq!(
            envelope["data"]["integrations"]["gpt_diagnostics"]["trace_page_enabled"], true,
            "should activate trace only through the explicit isolated configuration"
        );
    }

    #[test]
    fn trace_runtime_boundary_config_enables_only_explicit_fixture() {
        let baseline: serde_json::Value = serde_json::from_str(
            &integration_app_config_envelope(8888).expect("should generate baseline envelope"),
        )
        .expect("should parse baseline envelope");
        assert_ne!(
            baseline["data"]["integrations"]["gpt_diagnostics"]["trace_page_enabled"], true,
            "should keep the trace feature disabled in the ordinary baseline"
        );
        let enabled: serde_json::Value = serde_json::from_str(
            &integration_app_config_envelope_with_trace(8888, true)
                .expect("should generate trace fixture envelope"),
        )
        .expect("should parse trace envelope");
        assert_eq!(
            enabled["data"]["integrations"]["gpt_diagnostics"]["trace_page_enabled"], true,
            "should explicitly enable isolated trace fixture"
        );
    }
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
