use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use edgezero_core::blob_envelope::BlobEnvelope;
use trusted_server_core::{config::validate_settings_for_deploy, settings::Settings};

const GENERATED_AT: &str = "2026-06-23T00:00:00Z";
const GENERATED_STORES_MARKER: &str = "        # GENERATED_TRUSTED_SERVER_CONFIG_STORES";

type DynError = Box<dyn Error + Send + Sync + 'static>;

#[derive(Debug, PartialEq)]
struct Args {
    template: PathBuf,
    app_config: PathBuf,
    output: PathBuf,
    edgezero_enabled: bool,
    origin_url: Option<String>,
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
    let generated_config =
        inject_generated_config_stores(&template, &envelope_json, args.edgezero_enabled)?;

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
    let mut edgezero_enabled = None;
    let mut origin_url = None;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--template" => template = Some(next_path_arg(&mut iter, "--template")?),
            "--app-config" => app_config = Some(next_path_arg(&mut iter, "--app-config")?),
            "--output" => output = Some(next_path_arg(&mut iter, "--output")?),
            "--edgezero-enabled" => {
                let value = next_string_arg(&mut iter, "--edgezero-enabled")?;
                edgezero_enabled = Some(parse_bool(&value).ok_or_else(|| {
                    error_box(format!(
                        "--edgezero-enabled must be `true` or `false`, got `{value}`"
                    ))
                })?);
            }
            "--origin-url" => origin_url = Some(next_string_arg(&mut iter, "--origin-url")?),
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
        edgezero_enabled: edgezero_enabled
            .ok_or_else(|| error_box(format!("missing --edgezero-enabled\n\n{}", usage())))?,
        origin_url,
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

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn usage() -> String {
    "usage: generate-viceroy-config --template <path> --app-config <path> --output <path> --edgezero-enabled <true|false> [--origin-url <url>]".to_string()
}

fn build_app_config_envelope(
    app_config_toml: &str,
    origin_url: Option<&str>,
) -> Result<String, DynError> {
    let mut settings = Settings::from_toml(app_config_toml)
        .map_err(|report| error_box(format!("invalid Trusted Server app config: {report:?}")))?;
    if let Some(origin_url) = origin_url {
        settings.publisher.origin_url = origin_url.to_string();
    }
    validate_settings_for_deploy(&settings)
        .map_err(|report| error_box(format!("invalid Trusted Server app config: {report:?}")))?;

    let data = serde_json::to_value(&settings).map_err(|error| {
        error_box(format!(
            "failed to serialize Trusted Server app config to JSON: {error}"
        ))
    })?;
    let envelope = BlobEnvelope::new(data, GENERATED_AT.to_string());
    serde_json::to_string(&envelope)
        .map_err(|error| error_box(format!("failed to serialize app-config envelope: {error}")))
}

fn inject_generated_config_stores(
    template: &str,
    envelope_json: &str,
    edgezero_enabled: bool,
) -> Result<String, DynError> {
    let marker_count = template.matches(GENERATED_STORES_MARKER).count();
    if marker_count != 1 {
        return Err(error_box(format!(
            "Viceroy template must contain exactly one `{GENERATED_STORES_MARKER}` marker, found {marker_count}"
        )));
    }

    let generated_stores = generated_config_store_blocks(envelope_json, edgezero_enabled);
    Ok(template.replace(GENERATED_STORES_MARKER, &generated_stores))
}

fn generated_config_store_blocks(envelope_json: &str, edgezero_enabled: bool) -> String {
    let edgezero_enabled_value = if edgezero_enabled { "true" } else { "false" };
    format!(
        r#"        # Generated by generate-viceroy-config. Do not edit generated output.
        [local_server.config_stores.app_config]
            format = "inline-toml"
        [local_server.config_stores.app_config.contents]
            app_config = '''{envelope_json}'''

        # Preserves the Fastly rollout flag location used by production.
        [local_server.config_stores.trusted_server_config]
            format = "inline-toml"
        [local_server.config_stores.trusted_server_config.contents]
            edgezero_enabled = "{edgezero_enabled_value}""#
    )
}

fn error_box(message: impl Into<String>) -> DynError {
    std::io::Error::other(message.into()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusted_server_core::config_payload::settings_from_config_blob;

    const TEMPLATE: &str = include_str!("../../fixtures/configs/viceroy-template.toml");
    const APP_CONFIG: &str = include_str!("../../fixtures/configs/trusted-server.integration.toml");

    #[test]
    fn parse_args_accepts_required_flags_and_origin_override() {
        let args = parse_args([
            "--template".to_string(),
            "template.toml".to_string(),
            "--app-config".to_string(),
            "trusted-server.toml".to_string(),
            "--output".to_string(),
            "generated.toml".to_string(),
            "--edgezero-enabled".to_string(),
            "true".to_string(),
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
                edgezero_enabled: true,
                origin_url: Some("http://127.0.0.1:9999".to_string())
            },
            "should parse expected args"
        );
    }

    #[test]
    fn generated_config_contains_blob_and_rollout_flag() {
        let envelope = build_app_config_envelope(APP_CONFIG, None).expect("should build envelope");
        let generated = inject_generated_config_stores(TEMPLATE, &envelope, true)
            .expect("should inject generated stores");

        assert!(
            generated.contains("[local_server.config_stores.app_config]"),
            "should include app config store"
        );
        assert!(
            generated.contains("edgezero_enabled = \"true\""),
            "should include enabled rollout flag"
        );
        assert!(
            generated.contains("[local_server.config_stores.jwks_store]"),
            "should preserve following template content"
        );
    }

    #[test]
    fn generated_config_can_disable_edgezero() {
        let envelope = build_app_config_envelope(APP_CONFIG, None).expect("should build envelope");
        let generated = inject_generated_config_stores(TEMPLATE, &envelope, false)
            .expect("should inject generated stores");

        assert!(
            generated.contains("edgezero_enabled = \"false\""),
            "should include disabled rollout flag"
        );
    }

    #[test]
    fn generated_config_is_valid_toml() {
        let envelope = build_app_config_envelope(APP_CONFIG, None).expect("should build envelope");
        let generated = inject_generated_config_stores(TEMPLATE, &envelope, true)
            .expect("should inject generated stores");
        let parsed: toml::Value = toml::from_str(&generated).expect("should parse as TOML");

        assert_eq!(
            parsed["local_server"]["config_stores"]["trusted_server_config"]["contents"]
                ["edgezero_enabled"]
                .as_str(),
            Some("true"),
            "should expose rollout flag as string config-store value"
        );
    }

    #[test]
    fn generated_blob_verifies_and_applies_origin_override() {
        let envelope = build_app_config_envelope(APP_CONFIG, Some("http://127.0.0.1:9999"))
            .expect("should build envelope");
        let settings = settings_from_config_blob(&envelope).expect("should verify blob");

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
    fn missing_marker_fails() {
        let result = inject_generated_config_stores("[local_server]", "{}", false);

        assert!(result.is_err(), "should reject templates without marker");
    }
}
