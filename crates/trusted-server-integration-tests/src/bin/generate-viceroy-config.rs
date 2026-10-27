use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use edgezero_core::blob_envelope::BlobEnvelope;
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::config_payload::{CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID};
use trusted_server_core::settings::Settings;
use trusted_server_integration_tests::{fastly_fixture_blocks, native_fixtures};

const GENERATED_AT: &str = "2026-06-23T00:00:00Z";
const GENERATED_STORES_MARKER: &str = "        # GENERATED_TRUSTED_SERVER_CONFIG_STORES";

type DynError = Box<dyn Error + Send + Sync + 'static>;

#[derive(Debug, PartialEq)]
struct Args {
    template: PathBuf,
    app_config: PathBuf,
    output: PathBuf,
    origin_url: Option<String>,
    adapter: String,
    bindings_only: bool,
}

fn main() -> Result<(), DynError> {
    run(&parse_args(env::args().skip(1))?)
}

fn run(args: &Args) -> Result<(), DynError> {
    let app_config = fs::read_to_string(&args.app_config).map_err(|error| {
        error_box(format!(
            "failed to read Trusted Server app config `{}`: {error}",
            args.app_config.display()
        ))
    })?;

    let settings = fixture_settings(&app_config, args.origin_url.as_deref())?;
    if args.bindings_only {
        let entries = native_fixtures(&settings, &args.adapter)
            .map_err(error_box)?
            .into_iter()
            .map(|fixture| {
                serde_json::json!({
                    "descriptor_id": fixture.descriptor_id,
                    "native": fixture.native,
                    "value": fixture.value,
                })
            })
            .collect::<Vec<_>>();
        fs::write(&args.output, serde_json::to_vec_pretty(&entries)?)?;
        return Ok(());
    }
    let template = fs::read_to_string(&args.template)?;
    let template = inject_native_fixtures(&template, &settings, &args.adapter)?;
    let generated_config = if template.contains(GENERATED_STORES_MARKER) {
        let envelope_json = build_app_config_envelope(&app_config, args.origin_url.as_deref())?;
        inject_generated_config_stores(&template, &envelope_json)?
    } else {
        template
    };

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
    let mut adapter = "fastly".to_owned();
    let mut bindings_only = false;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--template" => template = Some(next_path_arg(&mut iter, "--template")?),
            "--app-config" => app_config = Some(next_path_arg(&mut iter, "--app-config")?),
            "--output" => output = Some(next_path_arg(&mut iter, "--output")?),
            "--origin-url" => origin_url = Some(next_string_arg(&mut iter, "--origin-url")?),
            "--adapter" => adapter = next_string_arg(&mut iter, "--adapter")?,
            "--bindings-only" => bindings_only = true,
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
        template: if bindings_only {
            template.unwrap_or_default()
        } else {
            template.ok_or_else(|| error_box(format!("missing --template\n\n{}", usage())))?
        },
        app_config: app_config
            .ok_or_else(|| error_box(format!("missing --app-config\n\n{}", usage())))?,
        output: output.ok_or_else(|| error_box(format!("missing --output\n\n{}", usage())))?,
        origin_url,
        adapter,
        bindings_only,
    })
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
    "usage: generate-viceroy-config --template <path> --app-config <path> --output <path> [--origin-url <url>] [--adapter fastly|axum|cloudflare|spin] [--bindings-only, no template required]".to_string()
}

fn build_app_config_envelope(
    app_config_toml: &str,
    origin_url: Option<&str>,
) -> Result<String, DynError> {
    let settings = fixture_settings(app_config_toml, origin_url)?;
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

fn fixture_settings(app_config_toml: &str, origin_url: Option<&str>) -> Result<Settings, DynError> {
    let app_config: TrustedServerAppConfig = toml::from_str(app_config_toml)
        .map_err(|error| error_box(format!("invalid Trusted Server app config: {error}")))?;
    let mut settings = app_config.into_settings();
    if let Some(origin_url) = origin_url {
        settings.publisher.origin_url = origin_url.to_string();
    }
    // Native-only fixtures may be compiled from the public starter template's
    // ordinary publisher placeholders. Deserialization still enforces the new
    // schema and endpoint syntax; native_fixtures enforces binding policy and
    // credential validators. Envelope generation separately validates the full
    // deployable configuration through TrustedServerAppConfig::new.
    Ok(settings)
}

fn replace_fixture_section(
    template: &str,
    section: &str,
    generated: &str,
) -> Result<String, DynError> {
    let begin = format!("# BEGIN GENERATED {section}");
    let end = format!("# END GENERATED {section}");
    let (before, rest) = template
        .split_once(&begin)
        .ok_or_else(|| error_box(format!("missing fixture marker {begin}")))?;
    let (_, after) = rest
        .split_once(&end)
        .ok_or_else(|| error_box(format!("missing fixture marker {end}")))?;
    Ok(format!("{before}{begin}\n{generated}\n{end}{after}"))
}

fn inject_native_fixtures(
    template: &str,
    settings: &Settings,
    adapter: &str,
) -> Result<String, DynError> {
    match adapter {
        "fastly" => replace_fixture_section(
            template,
            "APP SECRET FIXTURES",
            &fastly_fixture_blocks(settings).map_err(error_box)?,
        ),
        "cloudflare" => {
            let fixtures = native_fixtures(settings, adapter).map_err(error_box)?;
            let values = fixtures
                .into_iter()
                .map(|fixture| {
                    format!(
                        "{} = {}",
                        fixture.native.native_name,
                        toml::Value::String(fixture.value)
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            replace_fixture_section(template, "APP SECRET FIXTURES", &values)
        }
        "spin" => {
            let fixtures = native_fixtures(settings, adapter).map_err(error_box)?;
            let application = fixtures
                .iter()
                .map(|fixture| fixture.native.declarations[0].clone())
                .collect::<Vec<_>>()
                .join("\n\n");
            let component = fixtures
                .iter()
                .map(|fixture| {
                    fixture.native.declarations[1]
                        .strip_prefix("[component.trusted-server.variables]\n")
                        .ok_or_else(|| error_box("unexpected native component declaration"))
                })
                .collect::<Result<Vec<_>, _>>()?
                .join("\n");
            let template = replace_fixture_section(template, "APP SECRET VARIABLES", &application)?;
            replace_fixture_section(&template, "APP SECRET COMPONENT", &component)
        }
        _ => Err(error_box("unsupported fixture adapter")),
    }
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
        let settings = fixture_settings(APP_CONFIG, None)
            .expect("should deserialize the integration fixture settings");
        IntegrationSecretStore {
            values: native_fixtures(&settings, "fastly")
                .expect("should compile the integration native credentials")
                .into_iter()
                .map(|fixture| (fixture.native.key, fixture.value.into_bytes()))
                .collect(),
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
                adapter: "fastly".to_owned(),
                bindings_only: false
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
            settings.settings().publisher.origin_url,
            "http://127.0.0.1:9999",
            "should apply origin override before envelope creation"
        );
    }

    #[test]
    fn generated_native_fixtures_load_the_production_envelope() {
        let settings = fixture_settings(APP_CONFIG, None)
            .expect("should deserialize the integration fixture settings");
        let envelope = build_app_config_envelope(APP_CONFIG, None)
            .expect("should build the validated integration config envelope");
        let native = inject_native_fixtures(TEMPLATE, &settings, "fastly")
            .expect("should inject compiler-generated Fastly fixture blocks");
        let parsed: toml::Value =
            toml::from_str(&native).expect("should parse the generated Fastly fixture TOML");
        let values = parsed["local_server"]["secret_stores"]["trusted_server_secrets"]
            .as_array()
            .expect("should expose the generated logical secret-store entries")
            .iter()
            .map(|entry| {
                (
                    entry["key"]
                        .as_str()
                        .expect("should expose the generated native key")
                        .to_owned(),
                    entry["data"]
                        .as_str()
                        .expect("should expose the fictional raw credential value")
                        .as_bytes()
                        .to_vec(),
                )
            })
            .collect();
        let loaded = settings_from_config_blob(
            &envelope,
            &IntegrationSecretStore { values },
            &StoreName::from("trusted_server_secrets"),
        )
        .expect("should load the production envelope using generated native values");
        assert!(
            loaded
                .ec
                .partners
                .iter()
                .all(|partner| partner.api_token_enabled && partner.api_token.is_some())
        );
        assert_eq!(
            native,
            inject_native_fixtures(&native, &settings, "fastly")
                .expect("should leave generated fixture blocks unchanged on a second injection")
        );
    }

    #[test]
    fn spin_fixtures_declare_both_native_variable_levels_without_values() {
        let settings = fixture_settings(APP_CONFIG, None)
            .expect("should deserialize the integration fixture settings");
        let template = "# BEGIN GENERATED APP SECRET VARIABLES\n# END GENERATED APP SECRET VARIABLES\n[component.trusted-server.variables]\n# BEGIN GENERATED APP SECRET COMPONENT\n# END GENERATED APP SECRET COMPONENT\n";
        let generated = inject_native_fixtures(template, &settings, "spin")
            .expect("should generate both Spin declaration levels");
        let parsed: toml::Value = toml::from_str(&generated)
            .expect("should parse the generated Spin declarations as TOML");
        for fixture in native_fixtures(&settings, "spin")
            .expect("should compile the native Spin fixture identities")
        {
            let name = fixture.native.native_name;
            assert_eq!(parsed["variables"][&name]["secret"].as_bool(), Some(true));
            assert_eq!(parsed["variables"][&name]["required"].as_bool(), Some(true));
            assert_eq!(
                parsed["component"]["trusted-server"]["variables"][&name].as_str(),
                Some(format!("{{{{ {name} }}}}").as_str())
            );
            assert!(!generated.contains(&fixture.value));
        }
    }

    #[test]
    fn equal_handler_owners_generate_valid_cloudflare_and_spin_manifests() {
        let config =
            format!("{APP_CONFIG}\n[[handlers]]\npath = \"^/_ts/admin\"\nusername = \"admin\"\n");
        let settings = fixture_settings(&config, None)
            .expect("should parse integration configuration with two equal handler owners");
        let plan = trusted_server_core::secret_bindings::compile_bindings(&settings)
            .expect("should preserve both equal handler owners in the compiled plan");
        assert_eq!(plan.entries().len(), 6);

        let cloudflare = inject_native_fixtures(
            include_str!("../../../trusted-server-adapter-cloudflare/wrangler.ci.toml"),
            &settings,
            "cloudflare",
        )
        .expect("should emit each equal Cloudflare native binding once");
        let cloudflare: toml::Value = toml::from_str(&cloudflare)
            .expect("should parse the complete Cloudflare manifest with equal handlers");
        let fixtures = native_fixtures(&settings, "cloudflare")
            .expect("should compile five physical Cloudflare fixtures for six owner rows");
        assert_eq!(fixtures.len(), 5);
        let vars = cloudflare["vars"]
            .as_table()
            .expect("should retain the Cloudflare vars table");
        assert_eq!(
            vars.keys().filter(|name| name.starts_with("ts1_")).count(),
            5
        );
        for fixture in fixtures {
            assert_eq!(
                vars[&fixture.native.native_name].as_str(),
                Some(fixture.value.as_str())
            );
        }

        let spin = inject_native_fixtures(
            include_str!("../../../trusted-server-adapter-spin/spin.toml"),
            &settings,
            "spin",
        )
        .expect("should emit both Spin declaration levels once per physical request");
        let spin: toml::Value = toml::from_str(&spin)
            .expect("should parse the complete Spin manifest with equal handlers");
        let application = spin["variables"]
            .as_table()
            .expect("should retain the Spin application variables table");
        let component = spin["component"]["trusted-server"]["variables"]
            .as_table()
            .expect("should retain the Spin component variables table");
        assert_eq!(application.len(), 5);
        assert_eq!(component.len(), 5);
        for fixture in native_fixtures(&settings, "spin")
            .expect("should compile the coalesced Spin fixture identities")
        {
            let name = fixture.native.native_name;
            assert_eq!(application[&name]["secret"].as_bool(), Some(true));
            assert_eq!(application[&name]["required"].as_bool(), Some(true));
            assert_eq!(
                component[&name].as_str(),
                Some(format!("{{{{ {name} }}}}").as_str())
            );
        }
    }

    #[test]
    fn bindings_only_does_not_require_a_template() {
        let args = parse_args(
            [
                "--bindings-only",
                "--adapter",
                "axum",
                "--app-config",
                "fixture.toml",
                "--output",
                "bindings.json",
            ]
            .map(str::to_owned),
        )
        .expect("should parse bindings-only output without a template");
        assert!(args.bindings_only);
        assert_eq!(args.adapter, "axum");
    }

    #[test]
    fn public_starter_generates_native_fixtures_but_not_a_deployable_envelope() {
        const EXAMPLE: &str = include_str!("../../../../trusted-server.example.toml");
        let settings =
            fixture_settings(EXAMPLE, None).expect("should deserialize the public starter schema");
        for adapter in ["fastly", "axum", "cloudflare", "spin"] {
            let fixtures = native_fixtures(&settings, adapter)
                .expect("should compile binding policy despite ordinary starter placeholders");
            assert_eq!(
                fixtures.len(),
                3,
                "starter requires exactly three credential roles"
            );
        }
        let fastly =
            inject_native_fixtures(include_str!("../../../../fastly.toml"), &settings, "fastly")
                .expect("should render local Fastly fixtures from the public starter");
        toml::from_str::<toml::Value>(&fastly)
            .expect("should parse the generated local Fastly manifest");
        let spin = inject_native_fixtures(
            include_str!("../../../trusted-server-adapter-spin/spin.toml"),
            &settings,
            "spin",
        )
        .expect("should render local Spin declarations from the public starter");
        toml::from_str::<toml::Value>(&spin)
            .expect("should parse the generated local Spin manifest");
        let error = build_app_config_envelope(EXAMPLE, None)
            .expect_err("should reject starter placeholders when building a deployable envelope");
        let message = error.to_string();
        assert!(message.contains("Insecure default"));
        assert!(message.contains("publisher.domain"));
        assert!(message.contains("publisher.cookie_domain"));
        assert!(message.contains("publisher.origin_url"));
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
