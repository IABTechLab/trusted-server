use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use edgezero_core::blob_envelope::BlobEnvelope;
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::config_payload::{CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID};
use trusted_server_core::settings::Settings;
use url::Url;

const GENERATED_AT: &str = "2026-06-23T00:00:00Z";
const GENERATED_STORES_MARKER: &str = "        # GENERATED_TRUSTED_SERVER_CONFIG_STORES";

type DynError = Box<dyn Error + Send + Sync + 'static>;

#[derive(Debug, PartialEq)]
struct Args {
    template: PathBuf,
    app_config: PathBuf,
    output: PathBuf,
    origin_url: Option<String>,
    provider_endpoint_origin: Option<String>,
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

    let envelope_json = build_app_config_envelope(
        &app_config,
        args.origin_url.as_deref(),
        args.provider_endpoint_origin.as_deref(),
    )?;
    let generated_config = inject_generated_config_stores(&template, &envelope_json)?;

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
    let mut provider_endpoint_origin = None;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--template" => template = Some(next_path_arg(&mut iter, "--template")?),
            "--app-config" => app_config = Some(next_path_arg(&mut iter, "--app-config")?),
            "--output" => output = Some(next_path_arg(&mut iter, "--output")?),
            "--origin-url" => origin_url = Some(next_string_arg(&mut iter, "--origin-url")?),
            "--provider-endpoint-origin" => {
                provider_endpoint_origin =
                    Some(next_string_arg(&mut iter, "--provider-endpoint-origin")?);
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
        provider_endpoint_origin,
    })
}

/// Points every auction provider at `origin`, keeping each endpoint's path and query.
///
/// `origin` must be a bare origin: a scheme and authority with no credentials, path, query,
/// or fragment. Settings validation then decides whether the rewritten endpoints are allowed,
/// which admits plain HTTP only for the loopback literals.
fn rewrite_provider_endpoints(settings: &mut Settings, origin: &str) -> Result<(), DynError> {
    let origin = Url::parse(origin)
        .map_err(|error| error_box(format!("invalid provider endpoint origin: {error}")))?;
    if !origin.username().is_empty()
        || origin.password().is_some()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return Err(error_box(
            "provider endpoint origin must be a scheme and authority only",
        ));
    }
    for provider in settings.auction.providers.values_mut() {
        let endpoint = Url::parse(&provider.endpoint)
            .map_err(|error| error_box(format!("invalid auction provider endpoint: {error}")))?;
        let mut rewritten = origin.clone();
        rewritten.set_path(endpoint.path());
        rewritten.set_query(endpoint.query());
        provider.endpoint = rewritten.into();
    }
    Ok(())
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
    "usage: generate-viceroy-config --template <path> --app-config <path> --output <path> [--origin-url <url>] [--provider-endpoint-origin <url>]".to_string()
}

fn build_app_config_envelope(
    app_config_toml: &str,
    origin_url: Option<&str>,
    provider_endpoint_origin: Option<&str>,
) -> Result<String, DynError> {
    let app_config: TrustedServerAppConfig = toml::from_str(app_config_toml)
        .map_err(|error| error_box(format!("invalid Trusted Server app config: {error}")))?;
    let mut settings = app_config.into_settings();
    if let Some(origin_url) = origin_url {
        settings.publisher.origin_url = origin_url.to_string();
    }
    if let Some(origin) = provider_endpoint_origin {
        rewrite_provider_endpoints(&mut settings, origin)?;
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
    use trusted_server_core::config_payload::settings_from_config_blob;
    use trusted_server_core::platform::{PlatformError, PlatformSecretStore, StoreId, StoreName};

    const TEMPLATE: &str = include_str!("../../fixtures/configs/viceroy-template.toml");
    const APP_CONFIG: &str = include_str!("../../fixtures/configs/trusted-server.integration.toml");

    struct IntegrationSecretStore {
        values: HashMap<String, Vec<u8>>,
    }

    #[async_trait::async_trait(?Send)]
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

        async fn create(
            &self,
            _store_id: &StoreId,
            _name: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Ok(())
        }

        async fn delete(
            &self,
            _store_id: &StoreId,
            _name: &str,
        ) -> Result<(), Report<PlatformError>> {
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

    const TRACE_APP_CONFIG: &str = include_str!("../../fixtures/configs/trusted-server.trace.toml");

    #[test]
    fn parse_args_accepts_provider_endpoint_origin() {
        let args = parse_args([
            "--template".to_string(),
            "template.toml".to_string(),
            "--app-config".to_string(),
            "trusted-server.toml".to_string(),
            "--output".to_string(),
            "generated.toml".to_string(),
            "--provider-endpoint-origin".to_string(),
            "http://127.0.0.1:8888".to_string(),
        ])
        .expect("should accept a provider endpoint origin");

        assert_eq!(
            args.provider_endpoint_origin.as_deref(),
            Some("http://127.0.0.1:8888"),
            "should keep the provider endpoint origin"
        );
    }

    fn provider_endpoint(envelope: &str, provider: &str) -> String {
        let envelope: serde_json::Value =
            serde_json::from_str(envelope).expect("should parse the generated envelope");
        envelope["data"]["auction"]["providers"][provider]["endpoint"]
            .as_str()
            .expect("should keep the provider endpoint")
            .to_owned()
    }

    #[test]
    fn provider_endpoint_origin_replaces_scheme_and_authority_and_keeps_path_and_query() {
        let app_config = TRACE_APP_CONFIG.replace(
            "endpoint = \"https://bidder.example.com/api/trace-bidder\"",
            "endpoint = \"https://bidder.example.com/api/trace-bidder?route=example\"",
        );

        let envelope = build_app_config_envelope(&app_config, None, Some("http://127.0.0.1:8888"))
            .expect("should rewrite the provider endpoint onto the loopback origin");

        assert_eq!(
            provider_endpoint(&envelope, "example"),
            "http://127.0.0.1:8888/api/trace-bidder?route=example",
            "should replace the scheme and authority and keep the path and query"
        );
    }

    #[test]
    fn provider_endpoints_are_unchanged_without_provider_endpoint_origin() {
        let envelope = build_app_config_envelope(TRACE_APP_CONFIG, None, None)
            .expect("should build the trace envelope");

        assert_eq!(
            provider_endpoint(&envelope, "example"),
            "https://bidder.example.com/api/trace-bidder",
            "should keep the configured endpoint"
        );
    }

    #[test]
    fn provider_endpoint_origin_rejects_non_origin_and_non_loopback_urls() {
        for origin in [
            "http://127.0.0.1:8888/path",
            "http://127.0.0.1:8888/?query=value",
            "http://user:password@127.0.0.1:8888",
            "http://localhost:8888",
            "http://bidder.example.com",
        ] {
            assert!(
                build_app_config_envelope(TRACE_APP_CONFIG, None, Some(origin)).is_err(),
                "should reject `{origin}` as a provider endpoint origin"
            );
        }
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
                provider_endpoint_origin: None,
            },
            "should parse expected args"
        );
    }

    #[test]
    fn generated_config_contains_blob_without_removed_rollout_flags() {
        let envelope =
            build_app_config_envelope(APP_CONFIG, None, None).expect("should build envelope");
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
        let envelope =
            build_app_config_envelope(APP_CONFIG, None, None).expect("should build envelope");
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
        let envelope = build_app_config_envelope(APP_CONFIG, Some("http://127.0.0.1:9999"), None)
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
        let result = build_app_config_envelope("not valid toml", None, None);

        assert!(result.is_err(), "should reject invalid app config");
    }

    #[test]
    fn invalid_non_secret_app_config_fails_before_envelope_generation() {
        let invalid = APP_CONFIG.replace("domain = \"localhost\"", "domain = \"invalid/domain\"");

        let err = build_app_config_envelope(&invalid, None, None)
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
