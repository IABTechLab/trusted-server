use std::collections::BTreeSet;
use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use edgezero_core::blob_envelope::BlobEnvelope;
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::config_payload::{CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID};
use trusted_server_core::platform::{BackendNamingPolicy, PlatformBackendSpec};
use url::{Host, Url};

const GENERATED_AT: &str = "2026-06-23T00:00:00Z";
const GENERATED_STORES_MARKER: &str = "        # GENERATED_TRUSTED_SERVER_CONFIG_STORES";

type DynError = Box<dyn Error + Send + Sync + 'static>;

#[derive(Debug, PartialEq)]
struct Args {
    template: PathBuf,
    app_config: PathBuf,
    output: PathBuf,
    origin_url: Option<String>,
    bidder_origin_url: Option<String>,
}

fn main() -> Result<(), DynError> {
    run(&parse_args(env::args().skip(1))?)
}

fn run(args: &Args) -> Result<(), DynError> {
    let template = fs::read_to_string(&args.template).map_err(|error| {
        error_box(format!(
            "failed to read Viceroy template `{}`: {error}",
            args.template.display()
        ))
    })?;
    let app_config = fs::read_to_string(&args.app_config).map_err(|error| {
        error_box(format!(
            "failed to read Trusted Server app config `{}`: {error}",
            args.app_config.display()
        ))
    })?;

    let envelope_json = build_app_config_envelope(&app_config, args.origin_url.as_deref())?;
    let mut generated_config = inject_generated_config_stores(&template, &envelope_json)?;
    if let Some(origin) = &args.bidder_origin_url {
        generated_config =
            inject_controlled_bidder_backends(&generated_config, &app_config, origin)?;
    }

    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            error_box(format!(
                "failed to create output directory `{}`: {error}",
                parent.display()
            ))
        })?;
    }
    fs::write(&args.output, generated_config).map_err(|error| {
        error_box(format!(
            "failed to write generated Viceroy config `{}`: {error}",
            args.output.display()
        ))
    })?;

    Ok(())
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, DynError> {
    let mut template = None;
    let mut app_config = None;
    let mut output = None;
    let mut origin_url = None;
    let mut bidder_origin_url = None;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--template" => template = Some(next_path_arg(&mut iter, "--template")?),
            "--app-config" => app_config = Some(next_path_arg(&mut iter, "--app-config")?),
            "--output" => output = Some(next_path_arg(&mut iter, "--output")?),
            "--origin-url" => origin_url = Some(next_string_arg(&mut iter, "--origin-url")?),
            "--bidder-origin-url" => {
                bidder_origin_url = Some(next_string_arg(&mut iter, "--bidder-origin-url")?);
            }
            "--help" | "-h" => return Err(error_box(usage())),
            other => {
                return Err(error_box(format!(
                    "unknown argument `{other}`\n\n{}",
                    usage()
                )));
            }
        }
    }

    Ok(Args {
        template: template
            .ok_or_else(|| error_box(format!("missing --template\n\n{}", usage())))?,
        app_config: app_config
            .ok_or_else(|| error_box(format!("missing --app-config\n\n{}", usage())))?,
        output: output.ok_or_else(|| error_box(format!("missing --output\n\n{}", usage())))?,
        origin_url,
        bidder_origin_url,
    })
}

fn inject_controlled_bidder_backends(
    runtime_config: &str,
    app_config: &str,
    origin: &str,
) -> Result<String, DynError> {
    let origin = Url::parse(origin)?;
    let local = match origin.host() {
        Some(Host::Domain("localhost")) => true,
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    if !local
        || origin.scheme() != "http"
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return Err(error_box(
            "bidder fixture requires a plain loopback HTTP origin",
        ));
    }
    let settings = toml::from_str::<TrustedServerAppConfig>(app_config)?.into_settings();
    let logical_budget = settings.auction.timeout_ms.max(
        settings
            .creative_opportunities
            .as_ref()
            .and_then(|config| config.auction_timeout_ms)
            .unwrap_or(settings.auction.timeout_ms),
    );
    if logical_budget > 60_000 {
        return Err(error_box(
            "controlled bidder fixture budget exceeds 60 seconds",
        ));
    }
    let plan = trusted_server_core::auction::compile_auction_plan(&settings)
        .map_err(|_| error_box("invalid controlled bidder auction plan"))?;
    let mut runtime: toml::Value = toml::from_str(runtime_config)?;
    let backends = runtime
        .get_mut("local_server")
        .and_then(|server| server.get_mut("backends"))
        .and_then(toml::Value::as_table_mut)
        .ok_or_else(|| error_box("runtime template requires a backend table"))?;
    for provider in plan.providers() {
        let endpoint = Url::parse(provider.endpoint.as_str())?;
        // Reuse production policies for every reachable timer/name, rather
        // than duplicating Fastly's quantization or backend-name algorithm.
        let timers = (1..=provider.timeout_ms.min(logical_budget))
            .map(|remaining| {
                BackendNamingPolicy::Fastly
                    .canonicalize_transport_timeout_ms(remaining, provider.timeout_ms)
            })
            .filter(|timer| *timer != 0)
            .collect::<BTreeSet<_>>();
        for timer in timers {
            let duration = Duration::from_millis(u64::from(timer));
            let spec = PlatformBackendSpec {
                scheme: endpoint.scheme().to_owned(),
                host: endpoint
                    .host_str()
                    .ok_or_else(|| error_box("missing bidder host"))?
                    .to_owned(),
                port: endpoint.port(),
                host_header_override: None,
                certificate_check: true,
                first_byte_timeout: duration,
                between_bytes_timeout: duration,
                discriminator: Some(provider.id.as_str().to_owned()),
            };
            let name = BackendNamingPolicy::Fastly
                .predict(&spec)
                .map_err(|_| error_box("invalid controlled bidder backend name"))?
                .name;
            let alias = toml::Value::Table(toml::Table::from_iter([(
                "url".to_owned(),
                toml::Value::String(origin.as_str().to_owned()),
            )]));
            if backends.insert(name, alias).is_some() {
                return Err(error_box(
                    "controlled bidder alias collides with template backend",
                ));
            }
        }
    }
    Ok(toml::to_string(&runtime)?)
}

fn next_path_arg(
    iter: &mut impl Iterator<Item = String>,
    flag: &'static str,
) -> Result<PathBuf, DynError> {
    next_string_arg(iter, flag).map(PathBuf::from)
}

fn next_string_arg(
    iter: &mut impl Iterator<Item = String>,
    flag: &'static str,
) -> Result<String, DynError> {
    iter.next()
        .ok_or_else(|| error_box(format!("{flag} requires a value")))
}

fn usage() -> String {
    "usage: generate-viceroy-config --template <path> --app-config <path> --output <path> [--origin-url <url>] [--bidder-origin-url <local-url>]".to_string()
}

fn build_app_config_envelope(
    app_config_toml: &str,
    origin_url: Option<&str>,
) -> Result<String, DynError> {
    let app_config: TrustedServerAppConfig = toml::from_str(app_config_toml)
        .map_err(|error| error_box(format!("invalid Trusted Server app config: {error}")))?;
    let mut settings = app_config.into_settings();
    if let Some(origin_url) = origin_url {
        settings.publisher.origin_url = origin_url.to_string();
    }
    let app_config = TrustedServerAppConfig::new(settings)
        .map_err(|report| error_box(format!("invalid Trusted Server app config: {report:?}")))?;

    let data = serde_json::to_value(&app_config).map_err(|error| {
        error_box(format!(
            "failed to serialize Trusted Server app config to JSON: {error}"
        ))
    })?;
    let envelope = BlobEnvelope::new(data, GENERATED_AT.to_string());
    serde_json::to_string(&envelope)
        .map_err(|error| error_box(format!("failed to serialize app-config envelope: {error}")))
}

fn inject_generated_config_stores(template: &str, envelope_json: &str) -> Result<String, DynError> {
    let marker_count = template.matches(GENERATED_STORES_MARKER).count();
    if marker_count != 1 {
        return Err(error_box(format!(
            "Viceroy template must contain exactly one `{GENERATED_STORES_MARKER}` marker, found {marker_count}"
        )));
    }

    let generated_stores = generated_config_store_blocks(envelope_json);
    Ok(template.replace(GENERATED_STORES_MARKER, &generated_stores))
}

fn generated_config_store_blocks(envelope_json: &str) -> String {
    format!(
        r#"        # Generated by generate-viceroy-config. Do not edit generated output.
        [local_server.config_stores.{DEFAULT_CONFIG_STORE_ID}]
            format = "inline-toml"
        [local_server.config_stores.{DEFAULT_CONFIG_STORE_ID}.contents]
            {CONFIG_BLOB_KEY} = '''{envelope_json}'''"#
    )
}

fn error_box(message: impl Into<String>) -> DynError {
    std::io::Error::other(message.into()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use error_stack::Report;
    use std::collections::HashMap;
    use tempfile::tempdir;
    use trusted_server_core::config_payload::settings_from_config_blob;
    use trusted_server_core::platform::{PlatformError, PlatformSecretStore, StoreId, StoreName};

    const TEMPLATE: &str = include_str!("../../fixtures/configs/viceroy-template.toml");
    const APP_CONFIG: &str = include_str!("../../fixtures/configs/trusted-server.integration.toml");

    struct IntegrationSecretStore {
        values: HashMap<String, Vec<u8>>,
    }

    impl PlatformSecretStore for IntegrationSecretStore {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            self.values
                .get(key)
                .cloned()
                .ok_or_else(|| Report::new(PlatformError::SecretStore))
        }

        fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Ok(())
        }

        fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
            Ok(())
        }
    }

    fn integration_secret_store() -> IntegrationSecretStore {
        IntegrationSecretStore {
            values: HashMap::from([
                (
                    "integration_admin_password".to_owned(),
                    b"integration-admin-password-32-bytes-ok".to_vec(),
                ),
                (
                    "integration_proxy_secret".to_owned(),
                    b"integration-test-proxy-secret-32-bytes-ok".to_vec(),
                ),
                (
                    "integration_ec_passphrase".to_owned(),
                    b"integration-test-ec-secret-padded-32".to_vec(),
                ),
                (
                    "integration_partner_token_alpha".to_owned(),
                    b"integration-test-token-alpha-32-bytes-ok".to_vec(),
                ),
                (
                    "integration_partner_token_bravo".to_owned(),
                    b"integration-test-token-bravo-32-bytes-ok".to_vec(),
                ),
            ]),
        }
    }

    #[test]
    fn parse_args_does_not_require_removed_rollout_switch() {
        let result = parse_args([
            "--template".to_string(),
            "template.toml".to_string(),
            "--app-config".to_string(),
            "trusted-server.toml".to_string(),
            "--output".to_string(),
            "generated.toml".to_string(),
        ]);

        assert!(
            result.is_ok(),
            "post-cutover config generation should not require --edgezero-enabled"
        );
    }

    #[test]
    fn parse_args_accepts_explicit_controlled_bidder_origin() {
        assert!(
            parse_args([
                "--template".to_string(),
                "template.toml".to_string(),
                "--app-config".to_string(),
                "trusted-server.toml".to_string(),
                "--output".to_string(),
                "generated.toml".to_string(),
                "--bidder-origin-url".to_string(),
                "http://127.0.0.1:8888".to_string(),
            ])
            .is_ok(),
            "should accept an explicit local bidder fixture without changing defaults"
        );
    }

    fn run_bidder_fixture(origin: &str) -> Result<toml::Value, DynError> {
        let directory = tempdir().expect("should create isolated generator fixture");
        let template = directory.path().join("template.toml");
        let app_config = directory.path().join("app.toml");
        let output = directory.path().join("output.toml");
        fs::write(&template, TEMPLATE).expect("should write runtime template");
        fs::write(
            &app_config,
            include_str!("../../fixtures/configs/trusted-server.trace.toml"),
        )
        .expect("should write controlled trace app config");
        run(&Args {
            template,
            app_config,
            output: output.clone(),
            origin_url: None,
            bidder_origin_url: Some(origin.to_string()),
        })?;
        Ok(toml::from_str(&fs::read_to_string(output)?)?)
    }

    #[test]
    fn controlled_bidder_uses_backend_aliases_and_preserves_https_app_config() {
        let generated = run_bidder_fixture("http://127.0.0.1:8888")
            .expect("should generate explicit local bidder aliases");
        let aliases = generated["local_server"]["backends"]
            .as_table()
            .expect("should retain backend table");
        assert!(
            !aliases.is_empty(),
            "should route the real Rust bidder HTTP client to the controlled local origin"
        );
        for alias in aliases.values() {
            assert_eq!(
                alias["url"].as_str(),
                Some("http://127.0.0.1:8888/"),
                "should keep every runtime-only alias local"
            );
        }
        let envelope: serde_json::Value = serde_json::from_str(
            generated["local_server"]["config_stores"][DEFAULT_CONFIG_STORE_ID]["contents"]
                [CONFIG_BLOB_KEY]
                .as_str()
                .expect("should retain validated app envelope"),
        )
        .expect("should read unchanged app config envelope");
        assert_eq!(
            envelope["data"]["auction"]["providers"]["example"]["endpoint"],
            "https://bidder.example.com/api/trace-bidder",
            "should preserve production HTTPS and certificate admission"
        );
    }

    #[test]
    fn controlled_bidder_rejects_non_loopback_or_ambiguous_origins() {
        for origin in [
            "http://bidder.example.com",
            "http://127.0.0.1:8888/path",
            "http://127.0.0.1:8888/?query=value",
            "http://user:password@127.0.0.1:8888",
        ] {
            assert!(
                run_bidder_fixture(origin).is_err(),
                "should reject a non-local or non-origin bidder fixture URL"
            );
        }
    }

    #[test]
    fn controlled_bidder_omits_unreachable_large_provider_timer_aliases() {
        let app_config = include_str!("../../fixtures/configs/trusted-server.trace.toml").replace(
            "[auction.providers.example]",
            "[auction.providers.example]\ntimeout_ms = 100000",
        );
        let generated =
            inject_controlled_bidder_backends(TEMPLATE, &app_config, "http://127.0.0.1:8888")
                .expect("should bound the configured provider by reachable logical budgets");
        let generated: toml::Value =
            toml::from_str(&generated).expect("should generate valid bounded runtime aliases");
        assert_eq!(
            generated["local_server"]["backends"]
                .as_table()
                .expect("should retain aliases")
                .len(),
            8,
            "should generate only eight reachable timers for the 1000 ms fixture budget"
        );
    }

    #[test]
    fn controlled_bidder_rejects_excessive_fixture_logical_budget() {
        let app_config = include_str!("../../fixtures/configs/trusted-server.trace.toml")
            .replace("auction_timeout_ms = 1000", "auction_timeout_ms = 100000");
        assert!(
            inject_controlled_bidder_backends(TEMPLATE, &app_config, "http://127.0.0.1:8888")
                .is_err(),
            "should reject fixture budgets beyond the bounded 60 second timer enumeration"
        );
    }

    #[test]
    fn parse_args_accepts_required_flags_and_origin_override() {
        let args = parse_args([
            "--template".to_string(),
            "template.toml".to_string(),
            "--app-config".to_string(),
            "trusted-server.toml".to_string(),
            "--output".to_string(),
            "generated.toml".to_string(),
            "--origin-url".to_string(),
            "http://127.0.0.1:9999".to_string(),
        ])
        .expect("should parse args");

        assert_eq!(
            args,
            Args {
                template: PathBuf::from("template.toml"),
                app_config: PathBuf::from("trusted-server.toml"),
                output: PathBuf::from("generated.toml"),
                origin_url: Some("http://127.0.0.1:9999".to_string()),
                bidder_origin_url: None,
            },
            "should parse expected args"
        );
    }

    #[test]
    fn generated_config_contains_blob_without_removed_rollout_flags() {
        let envelope = build_app_config_envelope(APP_CONFIG, None).expect("should build envelope");
        let generated = inject_generated_config_stores(TEMPLATE, &envelope)
            .expect("should inject generated stores");

        assert!(
            generated.contains(&format!(
                "[local_server.config_stores.{DEFAULT_CONFIG_STORE_ID}]"
            )),
            "should include manifest-default app config store"
        );
        assert!(
            !generated.contains("edgezero_enabled"),
            "should omit the removed edgezero_enabled flag"
        );
        assert!(
            !generated.contains("edgezero_rollout_pct"),
            "should omit the removed edgezero_rollout_pct flag"
        );
        assert!(
            generated.contains("[local_server.config_stores.jwks_store]"),
            "should preserve following template content"
        );
    }

    #[test]
    fn generated_config_is_valid_toml() {
        let envelope = build_app_config_envelope(APP_CONFIG, None).expect("should build envelope");
        let generated = inject_generated_config_stores(TEMPLATE, &envelope)
            .expect("should inject generated stores");
        let parsed: toml::Value = toml::from_str(&generated).expect("should parse as TOML");

        assert_eq!(
            parsed["local_server"]["config_stores"][DEFAULT_CONFIG_STORE_ID]["contents"]
                [CONFIG_BLOB_KEY]
                .as_str(),
            Some(envelope.as_str()),
            "manifest-default config store should contain the app-config blob"
        );
    }

    #[test]
    fn generated_blob_verifies_and_applies_origin_override() {
        let envelope = build_app_config_envelope(APP_CONFIG, Some("http://127.0.0.1:9999"))
            .expect("should build envelope");
        let settings = settings_from_config_blob(
            &envelope,
            &integration_secret_store(),
            &StoreName::from("trusted_server_secrets"),
        )
        .expect("should verify blob");

        assert_eq!(
            settings.publisher.origin_url, "http://127.0.0.1:9999",
            "should apply origin override before envelope creation"
        );
    }

    #[test]
    fn invalid_app_config_fails() {
        let result = build_app_config_envelope("not valid toml", None);

        assert!(result.is_err(), "should reject invalid app config");
    }

    #[test]
    fn invalid_non_secret_app_config_fails_before_envelope_generation() {
        let invalid = APP_CONFIG.replace("domain = \"localhost\"", "domain = \"invalid/domain\"");

        let err = build_app_config_envelope(&invalid, None)
            .expect_err("should reject invalid non-secret config before creating an envelope");

        assert!(
            err.to_string().contains("invalid_publisher_domain"),
            "error should identify the structural validation failure: {err}"
        );
    }

    #[test]
    fn missing_marker_fails() {
        let result = inject_generated_config_stores("[local_server]", "{}");

        assert!(result.is_err(), "should reject templates without marker");
    }
}
