//! A Fastly push under a key other than the logical store ID, which `ts`
//! writes itself because `EdgeZero` writes no such key.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use error_stack::Report;
use tempfile::TempDir;
use toml_edit::DocumentMut;
use trusted_server_core::config_payload::DEFAULT_CONFIG_STORE_ID;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::platform::{
    PlatformConfigStore, PlatformError, PlatformSecretStore, StoreId, StoreName,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::settings_data::get_settings_from_config_store_with;

const HOST: &str = "a.publisher.example";

/// A project holding the repository's manifests, with only the Fastly
/// adapter declared, and the example app config.
fn project() -> TempDir {
    let directory = tempfile::tempdir().expect("should create a temporary project");
    let mut manifest: toml::Value = toml::from_str(include_str!("../../../edgezero.toml"))
        .expect("should parse the repository manifest");
    manifest["adapters"]
        .as_table_mut()
        .expect("should declare adapters")
        .retain(|name, _| name == "fastly");
    fs::write(
        directory.path().join("edgezero.toml"),
        toml::to_string(&manifest).expect("should serialize the Fastly test manifest"),
    )
    .expect("should write the test manifest");
    fs::write(
        directory.path().join("fastly.toml"),
        include_str!("../../../fastly.toml"),
    )
    .expect("should copy the local Fastly manifest");
    fs::write(
        directory.path().join("trusted-server.toml"),
        include_str!("../../../trusted-server.example.toml"),
    )
    .expect("should copy the example app config");
    directory
}

/// Makes the project's app config long enough to be stored as chunks.
fn lengthen_app_config(project: &TempDir) {
    let path = project.path().join("trusted-server.toml");
    let mut document: DocumentMut = fs::read_to_string(&path)
        .expect("should read the app config")
        .parse()
        .expect("should parse the app config");
    let mut domains = toml_edit::Array::new();
    for index in 0..400 {
        domains.push(format!("*.assets-{index}.publisher.example"));
    }
    document["proxy"]["allowed_domains"] = toml_edit::value(domains);
    fs::write(&path, document.to_string()).expect("should write the app config");
}

fn fastly_toml(project: &TempDir) -> PathBuf {
    project.path().join("fastly.toml")
}

/// Runs `ts config push --adapter fastly` in `project` with `args`.
fn push(project: &TempDir, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ts"))
        .args([
            "config",
            "push",
            "--adapter",
            "fastly",
            "--no-diff",
            "--yes",
        ])
        .args(args)
        .current_dir(project.path())
        // Do not inherit operator configuration or credentials from the test runner.
        .env_clear()
        .env("RUST_LOG", "info")
        .env("TRUSTED_SERVER__PUBLISHER__DOMAIN", HOST)
        .env(
            "TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN",
            format!(".{HOST}"),
        )
        .env(
            "TRUSTED_SERVER__PUBLISHER__ORIGIN_URL",
            "https://origin.publisher.example",
        )
        .output()
        .expect("should run a config push")
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "the push should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The local server's logical config store, in the order its entries sit in
/// the file.
fn stored_entries(project: &TempDir) -> Vec<(String, String)> {
    let document: DocumentMut = fs::read_to_string(fastly_toml(project))
        .expect("should read the local Fastly manifest")
        .parse()
        .expect("should parse the local Fastly manifest");
    document["local_server"]["config_stores"][DEFAULT_CONFIG_STORE_ID]["contents"]
        .as_table()
        .expect("should hold the store's contents")
        .iter()
        .map(|(key, value)| {
            let value = value.as_str().expect("should store each entry as a string");
            (key.to_owned(), value.to_owned())
        })
        .collect()
}

struct MemoryConfigStore(BTreeMap<String, String>);

impl PlatformConfigStore for MemoryConfigStore {
    fn get(&self, _store_name: &StoreName, key: &str) -> Result<String, Report<PlatformError>> {
        self.0.get(key).cloned().ok_or_else(|| {
            Report::new(PlatformError::ConfigStore).attach(format!("missing key `{key}`"))
        })
    }

    fn put(
        &self,
        _store_id: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Ok(())
    }

    fn delete(&self, _store_id: &StoreId, _key: &str) -> Result<(), Report<PlatformError>> {
        Ok(())
    }
}

/// Answers every secret reference with one value, long enough for each.
struct FixedSecretStore;

impl PlatformSecretStore for FixedSecretStore {
    fn get_bytes(
        &self,
        _store_name: &StoreName,
        _key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        Ok(b"read-back-secret-value-of-at-least-32-bytes".to_vec())
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

/// Reads the blob at `key` the way the runtime does, chunks included.
fn read_back(
    entries: &[(String, String)],
    key: &str,
) -> Result<Settings, Report<TrustedServerError>> {
    get_settings_from_config_store_with(
        &MemoryConfigStore(entries.iter().cloned().collect()),
        &FixedSecretStore,
        &StoreName::from(DEFAULT_CONFIG_STORE_ID),
        key,
        &StoreName::from("trusted_server_secrets"),
        &trusted_server_cli::linked_builders(),
    )
}

#[test]
fn a_local_push_of_a_host_key_writes_the_local_store() {
    let project = project();

    let output = push(&project, &["--local", "--key", HOST]);

    assert_success(&output);
    let entries = stored_entries(&project);
    let settings = read_back(&entries, HOST).expect("should read the host's blob back");
    assert_eq!(
        settings.publisher.domain, HOST,
        "should store the pushed app config"
    );
    assert!(
        entries
            .iter()
            .all(|(key, _)| key != DEFAULT_CONFIG_STORE_ID),
        "a host's push should not write the logical store ID"
    );

    let logical = push(&project, &["--local", "--key", DEFAULT_CONFIG_STORE_ID]);

    assert_success(&logical);
    assert!(
        String::from_utf8_lossy(&logical.stdout)
            .contains(&format!("pushed-key={DEFAULT_CONFIG_STORE_ID}")),
        "a push of the logical store ID should still go through EdgeZero: {}",
        String::from_utf8_lossy(&logical.stdout)
    );
}

#[test]
fn a_long_document_is_written_as_chunks_then_a_pointer() {
    let project = project();
    lengthen_app_config(&project);
    let before = fs::read_to_string(fastly_toml(&project)).expect("should read the manifest");

    let preview = push(&project, &["--local", "--key", HOST, "--dry-run"]);

    assert_success(&preview);
    assert!(
        String::from_utf8_lossy(&preview.stdout).contains("would write"),
        "a dry run should list the keys it would write"
    );
    assert_eq!(
        fs::read_to_string(fastly_toml(&project)).expect("should read the manifest"),
        before,
        "a dry run should write nothing"
    );

    assert_success(&push(&project, &["--local", "--key", HOST]));

    let mut entries = stored_entries(&project);
    let chunk_prefix = format!("{HOST}.__edgezero_chunks.");
    let chunk_count = entries
        .iter()
        .filter(|(key, _)| key.starts_with(&chunk_prefix))
        .count();
    assert!(chunk_count > 1, "a long blob should be split into chunks");
    let pointer_at = entries
        .iter()
        .position(|(key, _)| key == HOST)
        .expect("should write the pointer");
    assert!(
        entries[..pointer_at]
            .iter()
            .filter(|(key, _)| key.starts_with(&chunk_prefix))
            .count()
            == chunk_count,
        "every chunk should be written before the pointer"
    );
    let settings = read_back(&entries, HOST).expect("should read the chunked blob back whole");
    assert_eq!(
        settings.publisher.domain, HOST,
        "should reassemble the pushed app config"
    );
    assert_eq!(
        settings.proxy.allowed_domains.len(),
        400,
        "should reassemble every part of the long blob"
    );

    let pointer = &mut entries[pointer_at].1;
    let mut changed: serde_json::Value =
        serde_json::from_str(pointer).expect("should write the pointer as JSON");
    changed["chunks"][0]["sha256"] = serde_json::Value::from("0".repeat(64));
    *pointer = changed.to_string();
    let refused =
        read_back(&entries, HOST).expect_err("a chunk whose hash differs should be refused");
    assert!(
        format!("{refused:?}").contains("sha mismatch"),
        "should refuse the chunk for its hash: {refused:?}"
    );
}

#[test]
fn a_key_whose_chunk_keys_are_too_long_is_refused_before_writing() {
    let project = project();
    lengthen_app_config(&project);
    let before = fs::read_to_string(fastly_toml(&project)).expect("should read the manifest");
    // Short enough for the pointer, too long once a chunk key adds its
    // infix, the envelope's hash and an index.
    let key = "k".repeat(200);

    let output = push(&project, &["--local", "--key", &key]);

    assert!(!output.status.success(), "the push should be refused");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("255-character limit"),
        "should say why: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(fastly_toml(&project)).expect("should read the manifest"),
        before,
        "a refused push should write nothing"
    );
}

#[test]
fn a_remote_dry_run_names_the_live_store_and_writes_nothing() {
    let project = project();
    let before = fs::read_to_string(fastly_toml(&project)).expect("should read the manifest");

    // The runner's environment is cleared, so no `fastly` CLI can be found
    // and a dry run that asked it anything would fail.
    let output = push(&project, &["--key", HOST, "--dry-run"]);

    assert_success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&format!("would write `{HOST}` ("))
            && stdout.contains(&format!(
                "the Fastly config store `{DEFAULT_CONFIG_STORE_ID}`"
            )),
        "should list the key for the store the selector names: {stdout}"
    );
    assert_eq!(
        fs::read_to_string(fastly_toml(&project)).expect("should read the manifest"),
        before,
        "a push without --local should not write the local store"
    );
}
