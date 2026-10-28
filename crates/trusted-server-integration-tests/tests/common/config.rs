use edgezero_core::blob_envelope::BlobEnvelope;
use error_stack::Report;
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::permissions::{SAMPLE_POLICY_TEXT, policy_sha256};

use crate::common::runtime::{TestError, TestResult};

const GENERATED_AT: &str = "2026-06-23T00:00:00Z";
const APP_CONFIG: &str = include_str!("../../fixtures/configs/trusted-server.integration.toml");

pub fn integration_app_config_envelope(origin_port: u16) -> TestResult<String> {
    app_config_envelope(origin_port, |_| Ok(()))
}

/// Builds the integration app-config envelope for the Axum dev server.
///
/// The fixture selects the host's geo lookup, which Viceroy answers and the
/// Axum dev server, having none, refuses at startup. With `[geo] module`
/// unset and single-jurisdiction operation acknowledged, the dev server
/// locates no request, as its own lookup did.
pub fn axum_app_config_envelope(origin_port: u16) -> TestResult<String> {
    app_config_envelope(origin_port, |table| {
        let geo = table_mut(table, "geo")?;
        geo.remove("module");
        geo.insert(
            "assume_single_jurisdiction".to_string(),
            toml::Value::Boolean(true),
        );
        Ok(())
    })
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
    let mut origin = settings.origin_source().ok_or_else(|| {
        Report::new(TestError::ConfigGeneration)
            .attach("the integration config should select the origin source")
    })?;
    origin.url = origin_url;
    settings.set_origin_source(&origin);
    // The fixture selects modules the adapters link, so validation needs the
    // list as the adapters and the `ts` tool have it.
    trusted_server_core::config::register_deploy_integrations(
        trusted_server_adapter_axum::app::shipped_module_builders(),
    );
    // What `ts config push --permissions` writes for the sample the fixture
    // names. Axum reads the file from its folder and leaves the text unused.
    settings.permissions.sha256 = Some(policy_sha256(SAMPLE_POLICY_TEXT));
    settings.permissions.text = Some(SAMPLE_POLICY_TEXT.to_owned());
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

/// Builds the integration app-config envelope with server-side auctions
/// enabled.
///
/// The fixture's demand modules and bidder routes are removed so the enabled
/// auction completes through the orchestrator and telemetry without any
/// outbound call, and so Cloudflare, which runs one demand source at a time,
/// accepts it at startup.
pub fn auction_app_config_envelope(origin_port: u16) -> TestResult<String> {
    app_config_envelope(origin_port, |table| {
        let auction = table_mut(table, "auction")?;
        auction.insert("enabled".to_string(), toml::Value::Boolean(true));
        auction.remove("bidders");
        let mut demand = toml::Table::new();
        demand.insert("modules".to_string(), toml::Value::Array(Vec::new()));
        table.insert("demand".to_string(), toml::Value::Table(demand));
        Ok(())
    })
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

    /// The committed development environment files.
    const DEVELOPMENT_FILES: [(&str, &str); 2] = [
        (".env.dev", include_str!("../../../../.env.dev")),
        (".env.example", include_str!("../../../../.env.example")),
    ];

    /// Every store that an uncommented `*_STORE=<name>` line of a development
    /// file names is declared in `fastly.toml` and in the Viceroy template, so
    /// no name points at a store the local runtime does not have.
    #[test]
    fn every_store_a_development_file_names_is_declared() {
        for (config_name, config) in [
            ("fastly.toml", FASTLY_CONFIG),
            ("Viceroy integration template", VICEROY_TEMPLATE),
        ] {
            let parsed: toml::Value =
                toml::from_str(config).expect("should parse Fastly configuration");
            let local_server = &parsed["local_server"];
            let declares = |store: &str| {
                ["kv_stores", "config_stores", "secret_stores"]
                    .into_iter()
                    .any(|kind| {
                        local_server
                            .get(kind)
                            .and_then(|stores| stores.get(store))
                            .is_some()
                    })
            };

            for (file_name, contents) in DEVELOPMENT_FILES {
                for (variable, store) in contents
                    .lines()
                    .filter(|line| !line.trim_start().starts_with('#'))
                    .filter_map(|line| line.split_once('='))
                    .filter(|(variable, _)| variable.trim().ends_with("_STORE"))
                {
                    assert!(
                        declares(store.trim()),
                        "{file_name} sets {variable} to `{store}`, which {config_name} does not declare"
                    );
                }
            }
        }
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

        for key in ["publisher_proxy_secret", "ec_passphrase"] {
            assert!(
                entries
                    .iter()
                    .any(|entry| entry["key"].as_str() == Some(key)),
                "fastly.toml should define local value for starter secret `{key}`"
            );
        }
    }
}
