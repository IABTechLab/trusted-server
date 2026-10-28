use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

use edgezero_core::blob_envelope::BlobEnvelope;
use sha2::{Digest as _, Sha256};
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::config_payload::{CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID};
use trusted_server_core::permissions::policy_sha256;

const GENERATED_AT: &str = "2026-06-23T00:00:00Z";
const GENERATED_STORES_MARKER: &str = "        # GENERATED_TRUSTED_SERVER_CONFIG_STORES";
/// The most characters Fastly, and Viceroy, accept in one config store value.
const CONFIG_ENTRY_LIMIT: usize = 8_000;
/// The most bytes in one chunk of a blob, as `EdgeZero`'s Fastly writer cuts
/// them.
const CHUNK_BYTES: usize = 7_000;
/// What a pointer to a chunked blob names itself, for the loader to know it.
const CHUNK_POINTER_KIND: &str = "fastly_config_chunks";

type DynError = Box<dyn Error + Send + Sync + 'static>;

#[derive(Debug, PartialEq)]
struct Args {
    template: PathBuf,
    app_config: PathBuf,
    permissions: PathBuf,
    output: PathBuf,
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

    let policy = PolicyFile::read(&args.permissions)?;

    let envelope_json =
        build_app_config_envelope(&app_config, &policy, args.origin_url.as_deref())?;
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
    let mut permissions = None;
    let mut output = None;
    let mut origin_url = None;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--template" => template = Some(next_path_arg(&mut iter, "--template")?),
            "--app-config" => app_config = Some(next_path_arg(&mut iter, "--app-config")?),
            "--permissions" => permissions = Some(next_path_arg(&mut iter, "--permissions")?),
            "--output" => output = Some(next_path_arg(&mut iter, "--output")?),
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
        permissions: permissions
            .ok_or_else(|| error_box(format!("missing --permissions\n\n{}", usage())))?,
        output: output.ok_or_else(|| error_box(format!("missing --output\n\n{}", usage())))?,
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

fn usage() -> String {
    "usage: generate-viceroy-config --template <path> --app-config <path> --permissions <path> --output <path> [--origin-url <url>]".to_string()
}

/// A permission policy file, named by its stem.
#[derive(Debug)]
struct PolicyFile {
    name: String,
    text: String,
}

impl PolicyFile {
    fn read(path: &std::path::Path) -> Result<Self, DynError> {
        let name = path
            .file_stem()
            .and_then(std::ffi::OsStr::to_str)
            .ok_or_else(|| error_box(format!("policy file `{}` has no name", path.display())))?
            .to_owned();
        let text = fs::read_to_string(path).map_err(|error| {
            error_box(format!(
                "failed to read permission policy file `{}`: {error}",
                path.display()
            ))
        })?;
        Ok(Self { name, text })
    }
}

/// Builds the blob `ts config push` would write, carrying the SHA-256 and the
/// text of the policy file the settings name.
fn build_app_config_envelope(
    app_config_toml: &str,
    policy: &PolicyFile,
    origin_url: Option<&str>,
) -> Result<String, DynError> {
    let app_config: TrustedServerAppConfig = toml::from_str(app_config_toml)
        .map_err(|error| error_box(format!("invalid Trusted Server app config: {error}")))?;
    let mut settings = app_config.into_settings();
    if settings.permissions.policy.as_deref() != Some(policy.name.as_str()) {
        return Err(error_box(format!(
            "the app config names the permission policy {:?}, not `{}`",
            settings.permissions.policy, policy.name
        )));
    }
    settings.permissions.sha256 = Some(policy_sha256(&policy.text));
    settings.permissions.text = Some(policy.text.clone());
    if let Some(origin_url) = origin_url {
        let mut origin = settings.origin_source().ok_or_else(|| {
            error_box("the app config should select the origin source".to_string())
        })?;
        origin.url = origin_url.to_string();
        settings.set_origin_source(&origin);
    }
    // The fixture selects modules the adapters link, so validation needs the
    // list as the adapters and the `ts` tool have it.
    trusted_server_core::config::register_deploy_integrations(
        trusted_server_adapter_axum::app::shipped_module_builders(),
    );
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

    let generated_stores = generated_config_store_blocks(&config_store_entries(envelope_json)?)?;
    Ok(template.replace(GENERATED_STORES_MARKER, &generated_stores))
}

/// The config store entries for the blob, as `ts config push --adapter
/// fastly` writes them. A blob that fits one entry is stored as it is.
/// A larger one is cut into chunks, and the blob's key holds a pointer to
/// them in the form the runtime loader reads.
fn config_store_entries(envelope_json: &str) -> Result<Vec<(String, String)>, DynError> {
    if envelope_json.len() <= CONFIG_ENTRY_LIMIT {
        return Ok(vec![(CONFIG_BLOB_KEY.to_owned(), envelope_json.to_owned())]);
    }
    let envelope_sha256 = sha256_hex(envelope_json);
    let mut entries = Vec::new();
    let mut chunks = Vec::new();
    let mut rest = envelope_json;
    while !rest.is_empty() {
        // Cut where a character ends, so every chunk is valid UTF-8.
        let mut end = rest.len().min(CHUNK_BYTES);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (chunk, remainder) = rest.split_at(end);
        let key = format!(
            "{CONFIG_BLOB_KEY}.__edgezero_chunks.{envelope_sha256}.{}",
            entries.len()
        );
        chunks.push(serde_json::json!({
            "key": key,
            "len": chunk.len(),
            "sha256": sha256_hex(chunk),
        }));
        entries.push((key, chunk.to_owned()));
        rest = remainder;
    }
    let pointer = serde_json::json!({
        "chunks": chunks,
        "edgezero_kind": CHUNK_POINTER_KIND,
        "envelope_len": envelope_json.len(),
        "envelope_sha256": envelope_sha256,
        "version": 1,
    })
    .to_string();
    if pointer.chars().count() > CONFIG_ENTRY_LIMIT {
        return Err(error_box(format!(
            "the chunk pointer is {} characters, more than one config store entry holds",
            pointer.chars().count()
        )));
    }
    entries.push((CONFIG_BLOB_KEY.to_owned(), pointer));
    Ok(entries)
}

/// The lower-case hexadecimal SHA-256 of `text`'s bytes.
fn sha256_hex(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// The app-config store block, with `entries` written as TOML strings, so a
/// value carries any character the blob does.
fn generated_config_store_blocks(entries: &[(String, String)]) -> Result<String, DynError> {
    let contents: toml::Table = entries
        .iter()
        .map(|(key, value)| (key.clone(), toml::Value::String(value.clone())))
        .collect();
    let rendered = toml::to_string(&contents).map_err(|error| {
        error_box(format!(
            "failed to write the app-config store entries: {error}"
        ))
    })?;
    Ok(format!(
        r#"        # Generated by generate-viceroy-config. Do not edit generated output.
        [local_server.config_stores.{DEFAULT_CONFIG_STORE_ID}]
            format = "inline-toml"
        [local_server.config_stores.{DEFAULT_CONFIG_STORE_ID}.contents]
{rendered}"#
    ))
}

fn error_box(message: impl Into<String>) -> DynError {
    std::io::Error::other(message.into()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use error_stack::Report;
    use std::collections::HashMap;
    use trusted_server_core::config_payload::settings_from_config_blob_with;
    use trusted_server_core::permissions::{SAMPLE_POLICY_NAME, SAMPLE_POLICY_TEXT};
    use trusted_server_core::platform::{
        PlatformConfigStore, PlatformError, PlatformSecretStore, PolicyOrigin, StoreId, StoreName,
    };
    use trusted_server_core::settings_data::get_settings_from_config_store_with;

    /// The repository's sample, which the fixture names.
    fn sample() -> PolicyFile {
        PolicyFile {
            name: SAMPLE_POLICY_NAME.to_owned(),
            text: SAMPLE_POLICY_TEXT.to_owned(),
        }
    }

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
            "--permissions".to_string(),
            "sample.yaml".to_string(),
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
            "--permissions".to_string(),
            "sample.yaml".to_string(),
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
                permissions: PathBuf::from("sample.yaml"),
                output: PathBuf::from("generated.toml"),
                origin_url: Some("http://127.0.0.1:9999".to_string())
            },
            "should parse expected args"
        );
    }

    #[test]
    fn generated_config_contains_blob_without_removed_rollout_flags() {
        let envelope =
            build_app_config_envelope(APP_CONFIG, &sample(), None).expect("should build envelope");
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

    /// The app-config store's entries in the config `generated` holds.
    fn generated_entries(generated: &str) -> HashMap<String, String> {
        let parsed: toml::Value = toml::from_str(generated).expect("should parse as TOML");
        parsed["local_server"]["config_stores"][DEFAULT_CONFIG_STORE_ID]["contents"]
            .as_table()
            .expect("should carry the app-config store's contents")
            .iter()
            .map(|(key, value)| {
                let value = value.as_str().expect("every entry should be a string");
                (key.clone(), value.to_owned())
            })
            .collect()
    }

    /// The entries a config store holds, as Viceroy serves them.
    struct GeneratedConfigStore(HashMap<String, String>);

    impl GeneratedConfigStore {
        fn entry(&self, key: &str) -> Result<String, Report<PlatformError>> {
            self.0
                .get(key)
                .cloned()
                .ok_or_else(|| Report::new(PlatformError::ConfigStore))
        }
    }

    impl PlatformConfigStore for GeneratedConfigStore {
        /// The entry at `key`, its chunks joined in order when it is a chunk
        /// pointer, as the Fastly config store returns a long entry whole.
        fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
            let value = self.entry(key)?;
            let pointer: serde_json::Value =
                serde_json::from_str(&value).unwrap_or(serde_json::Value::Null);
            if pointer["edgezero_kind"] != CHUNK_POINTER_KIND {
                return Ok(value);
            }
            pointer["chunks"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|chunk| self.entry(chunk["key"].as_str().unwrap_or_default()))
                .collect()
        }

        fn put(
            &self,
            _store_id: &StoreId,
            _key: &str,
            _value: &str,
        ) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::ConfigStore))
        }

        fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
            Err(Report::new(PlatformError::ConfigStore))
        }
    }

    #[test]
    fn every_generated_entry_fits_a_config_store_value() {
        let envelope =
            build_app_config_envelope(APP_CONFIG, &sample(), None).expect("should build envelope");
        let generated = inject_generated_config_stores(TEMPLATE, &envelope)
            .expect("should inject generated stores");
        let entries = generated_entries(&generated);

        assert!(
            envelope.chars().count() > CONFIG_ENTRY_LIMIT,
            "a blob carrying the sample should be too long for one entry, or this proves nothing"
        );
        assert!(entries.len() > 2, "the blob should take several chunks");
        for (key, value) in &entries {
            assert!(
                value.chars().count() <= CONFIG_ENTRY_LIMIT,
                "`{key}` holds {} characters",
                value.chars().count()
            );
        }
    }

    #[test]
    fn the_runtime_loader_reads_the_generated_entries() {
        let envelope =
            build_app_config_envelope(APP_CONFIG, &sample(), None).expect("should build envelope");
        let generated = inject_generated_config_stores(TEMPLATE, &envelope)
            .expect("should inject generated stores");
        let store = GeneratedConfigStore(generated_entries(&generated));

        let settings = get_settings_from_config_store_with(
            &store,
            &integration_secret_store(),
            &StoreName::from(DEFAULT_CONFIG_STORE_ID),
            CONFIG_BLOB_KEY,
            &StoreName::from("trusted_server_secrets"),
            PolicyOrigin::Blob,
            &trusted_server_adapter_axum::app::shipped_module_builders(),
        )
        .expect("should load the settings through the chunk pointer");

        assert_eq!(
            settings
                .permission_policy()
                .map(|policy| policy.name().to_owned()),
            Some(SAMPLE_POLICY_NAME.to_owned()),
            "the loaded settings should carry the sample policy"
        );
    }

    #[test]
    fn a_blob_that_fits_one_entry_is_stored_as_it_is() {
        let entries = config_store_entries("{}").expect("should build the entries");

        assert_eq!(entries, vec![(CONFIG_BLOB_KEY.to_owned(), "{}".to_owned())]);
    }

    #[test]
    fn generated_blob_verifies_and_applies_origin_override() {
        let envelope =
            build_app_config_envelope(APP_CONFIG, &sample(), Some("http://127.0.0.1:9999"))
                .expect("should build envelope");
        let settings = settings_from_config_blob_with(
            &envelope,
            &integration_secret_store(),
            &StoreName::from("trusted_server_secrets"),
            PolicyOrigin::Blob,
            &trusted_server_adapter_axum::app::shipped_module_builders(),
        )
        .expect("should verify blob");

        assert_eq!(
            settings
                .permission_policy()
                .map(|policy| policy.name().to_owned()),
            Some(SAMPLE_POLICY_NAME.to_owned()),
            "the blob should carry the policy the fixture names"
        );

        assert_eq!(
            settings
                .origin_source()
                .expect("should select the origin source")
                .url,
            "http://127.0.0.1:9999",
            "should apply origin override before envelope creation"
        );
    }

    #[test]
    fn invalid_app_config_fails() {
        let result = build_app_config_envelope("not valid toml", &sample(), None);

        assert!(result.is_err(), "should reject invalid app config");
    }

    #[test]
    fn invalid_non_secret_app_config_fails_before_envelope_generation() {
        let invalid = APP_CONFIG.replace("domain = \"localhost\"", "domain = \"invalid/domain\"");

        let err = build_app_config_envelope(&invalid, &sample(), None)
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
