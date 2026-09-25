//! Exercise the CLI's manifest resolution against the compiled runtime defaults.

use std::collections::BTreeMap;
use std::fs;
use std::process::{Command, Output};

use tempfile::TempDir;
use trusted_server_core::config_payload::{CONFIG_BLOB_KEY, DEFAULT_CONFIG_STORE_ID};

fn project() -> TempDir {
    let directory = tempfile::tempdir().expect("should create a temporary project");
    let mut manifest: toml::Value = toml::from_str(include_str!("../../../edgezero.toml"))
        .expect("should parse the repository manifest");
    // Keep the real store declarations without loading unrelated adapter files.
    manifest["adapters"]
        .as_table_mut()
        .expect("should declare adapters")
        .retain(|name, _| name == "axum");
    fs::write(
        directory.path().join("edgezero.toml"),
        toml::to_string(&manifest).expect("should serialize the Axum test manifest"),
    )
    .expect("should write the test manifest");
    fs::write(
        directory.path().join("trusted-server.toml"),
        include_str!("../../../trusted-server.example.toml"),
    )
    .expect("should copy the example app config");
    directory
}

fn push(project: &TempDir, args: &[&str], overrides: &[(&str, &str)]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_ts"))
        .args([
            "config",
            "push",
            "--adapter",
            "axum",
            "--local",
            "--no-diff",
        ])
        .args(args)
        .current_dir(project.path())
        // Do not inherit operator configuration or credentials from the test runner.
        .env_clear()
        .env("RUST_LOG", "info")
        .env("TRUSTED_SERVER__PUBLISHER__DOMAIN", "publisher.example.com")
        .env(
            "TRUSTED_SERVER__PUBLISHER__COOKIE_DOMAIN",
            ".publisher.example.com",
        )
        .env(
            "TRUSTED_SERVER__PUBLISHER__ORIGIN_URL",
            "https://upstream.example.com",
        )
        .envs(overrides.iter().copied())
        .output()
        .expect("should run a local config push");
    assert!(
        output.status.success(),
        "local push should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn stored_entries(project: &TempDir) -> BTreeMap<String, String> {
    // Axum's local file is keyed by logical ID even with a physical-name override.
    let path = project.path().join(format!(
        ".edgezero/local-config-{DEFAULT_CONFIG_STORE_ID}.json"
    ));
    let raw = fs::read_to_string(path).expect("should write the manifest-default local store");
    serde_json::from_str(&raw).expect("should parse the stored config entries")
}

#[test]
fn config_push_default_store_and_key_match_the_compiled_runtime() {
    let project = project();
    push(&project, &["--yes"], &[]);

    let entries = stored_entries(&project);
    assert_eq!(entries.len(), 1, "should write only the default blob key");
    let envelope: serde_json::Value = serde_json::from_str(
        entries
            .get(CONFIG_BLOB_KEY)
            .expect("should write at the compiled runtime's default key"),
    )
    .expect("should write a JSON blob envelope");
    assert_eq!(
        envelope["data"]["publisher"]["domain"],
        "publisher.example.com"
    );
}

#[test]
fn config_push_resolves_the_physical_name_and_explicit_key() {
    let project = project();
    let name_var = format!(
        "EDGEZERO__STORES__CONFIG__{}__NAME",
        DEFAULT_CONFIG_STORE_ID.to_uppercase()
    );
    let key_var = format!(
        "EDGEZERO__STORES__CONFIG__{}__KEY",
        DEFAULT_CONFIG_STORE_ID.to_uppercase()
    );
    let overrides = [
        (name_var.as_str(), "example_config"),
        (key_var.as_str(), "active_config"),
    ];
    let preview = push(
        &project,
        &["--dry-run", "--key", "active_config"],
        &overrides,
    );
    let stdout = String::from_utf8_lossy(&preview.stdout);
    assert!(
        stdout.contains(&format!(
            "store `{DEFAULT_CONFIG_STORE_ID}` (platform name `example_config`)"
        )),
        "preview should resolve the logical and physical store names: {stdout}"
    );
    assert!(
        !project.path().join(".edgezero").exists(),
        "preview should not write local config"
    );

    push(&project, &["--yes", "--key", "active_config"], &overrides);
    let entries = stored_entries(&project);
    assert_eq!(entries.len(), 1);
    assert!(
        entries.contains_key("active_config"),
        "explicit push key should match the runtime override"
    );
}

#[test]
fn config_push_does_not_use_the_runtime_key_override_without_the_key_flag() {
    let project = project();
    let key_var = format!(
        "EDGEZERO__STORES__CONFIG__{}__KEY",
        DEFAULT_CONFIG_STORE_ID.to_uppercase()
    );
    push(&project, &["--yes"], &[(&key_var, "active_config")]);

    let entries = stored_entries(&project);
    assert_eq!(entries.len(), 1);
    assert!(
        entries.contains_key(CONFIG_BLOB_KEY),
        "runtime-only key override should not move the CLI's write destination"
    );
}
