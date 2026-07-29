//! Regression coverage for typed app-config environment overlays.

use std::fs;
use std::process::{Command, Output};

use tempfile::TempDir;
use toml_edit::{DocumentMut, value};

const LEGACY_CONFIG: &str = include_str!(
    "../../trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml"
);
const MANIFEST: &str = r#"
[app]
name = "trusted-server"

[adapters.axum.adapter]
crate = "crates/trusted-server-adapter-axum"

[adapters.axum.commands]
build = "echo"
deploy = "echo"
serve = "echo"

[stores.config]
ids = ["trusted_server_config"]

[stores.secrets]
ids = ["trusted_server_secrets"]
"#;
const REWRITE_ENV: &str = "TRUSTED_SERVER__AUCTION__REWRITE_CREATIVES";

struct MigratedProject {
    directory: TempDir,
    config_path: std::path::PathBuf,
    manifest_path: std::path::PathBuf,
}

fn migrated_legacy_project() -> MigratedProject {
    let directory = tempfile::tempdir().expect("should create temporary config directory");
    let config_path = directory.path().join("trusted-server.toml");
    let manifest_path = directory.path().join("edgezero.toml");
    let mut document = LEGACY_CONFIG
        .parse::<DocumentMut>()
        .expect("should parse legacy integration config");
    document["auction"]["rewrite_creatives"] = value(true);
    fs::write(&config_path, document.to_string()).expect("should write migrated config");
    fs::write(&manifest_path, MANIFEST).expect("should write test manifest");
    MigratedProject {
        directory,
        config_path,
        manifest_path,
    }
}

fn validate_with_overlay(project: &MigratedProject, raw_value: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_ts"))
        .args(["config", "validate", "--manifest"])
        .arg(&project.manifest_path)
        .arg("--app-config")
        .arg(&project.config_path)
        .current_dir(project.directory.path())
        .env(REWRITE_ENV, raw_value)
        .output()
        .expect("should run ts config validate")
}

#[test]
fn migrated_legacy_config_applies_rewrite_creatives_environment_override() {
    let project = migrated_legacy_project();
    let output = Command::new(env!("CARGO_BIN_EXE_ts"))
        .args(["config", "push", "--adapter", "axum", "--manifest"])
        .arg(&project.manifest_path)
        .arg("--app-config")
        .arg(&project.config_path)
        .args(["--yes", "--no-diff"])
        .current_dir(project.directory.path())
        .env(REWRITE_ENV, "false")
        .output()
        .expect("should run ts config push");

    assert!(
        output.status.success(),
        "valid boolean overlay should push successfully: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let local_store_path = project
        .directory
        .path()
        .join(".edgezero/local-config-trusted_server_config.json");
    let local_store: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(local_store_path).expect("should read pushed local config"),
    )
    .expect("should parse local config store");
    let envelope_json = local_store
        .as_object()
        .and_then(|entries| entries.values().next())
        .and_then(serde_json::Value::as_str)
        .expect("should contain a blob envelope");
    let envelope: serde_json::Value =
        serde_json::from_str(envelope_json).expect("should parse blob envelope");

    // The config file carries `rewrite_creatives = true`, which serializes into
    // the blob. The `false` override matches the default, which is omitted from
    // serialized blobs — so the override having applied shows up as the field
    // being absent.
    assert!(
        envelope["data"]["auction"]
            .get("rewrite_creatives")
            .is_none(),
        "pushed config should apply the environment override over the file's explicit true"
    );
}

#[test]
fn migrated_legacy_config_default_rewrite_creatives_has_no_local_diff() {
    let project = migrated_legacy_project();
    // Rewriting is opt-in, so the default value is `false`. Write the default
    // into the config so the pushed blob omits the field entirely.
    let mut document = fs::read_to_string(&project.config_path)
        .expect("should read migrated config")
        .parse::<DocumentMut>()
        .expect("should parse migrated config");
    document["auction"]["rewrite_creatives"] = value(false);
    fs::write(&project.config_path, document.to_string())
        .expect("should write default rewrite config");
    let push = Command::new(env!("CARGO_BIN_EXE_ts"))
        .args(["config", "push", "--adapter", "axum", "--manifest"])
        .arg(&project.manifest_path)
        .arg("--app-config")
        .arg(&project.config_path)
        .args(["--yes", "--no-diff"])
        .current_dir(project.directory.path())
        .output()
        .expect("should run ts config push");

    assert!(
        push.status.success(),
        "default rewrite setting should push successfully: {}",
        String::from_utf8_lossy(&push.stderr)
    );

    let local_store_path = project
        .directory
        .path()
        .join(".edgezero/local-config-trusted_server_config.json");
    let local_store: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(local_store_path).expect("should read pushed local config"),
    )
    .expect("should parse local config store");
    let envelope_json = local_store
        .as_object()
        .and_then(|entries| entries.values().next())
        .and_then(serde_json::Value::as_str)
        .expect("should contain a blob envelope");
    let envelope: serde_json::Value =
        serde_json::from_str(envelope_json).expect("should parse blob envelope");

    assert!(
        envelope["data"]["auction"]
            .get("rewrite_creatives")
            .is_none(),
        "should omit the default rewrite setting from the pushed blob"
    );

    let diff = Command::new(env!("CARGO_BIN_EXE_ts"))
        .args([
            "config",
            "diff",
            "--adapter",
            "axum",
            "--local",
            "--manifest",
        ])
        .arg(&project.manifest_path)
        .arg("--app-config")
        .arg(&project.config_path)
        .current_dir(project.directory.path())
        .output()
        .expect("should run ts config diff");

    assert!(
        diff.status.success(),
        "default rewrite setting should have no local diff: {}",
        String::from_utf8_lossy(&diff.stderr)
    );
    assert!(
        String::from_utf8_lossy(&diff.stderr).contains("# no changes"),
        "diff should report no changes: {}",
        String::from_utf8_lossy(&diff.stderr)
    );
}

#[test]
fn migrated_legacy_config_rejects_invalid_rewrite_creatives_environment_override() {
    let project = migrated_legacy_project();
    let output = validate_with_overlay(&project, "not-a-boolean");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "invalid boolean overlay should fail validation"
    );
    assert!(
        stderr.contains(REWRITE_ENV) && stderr.contains("boolean"),
        "error should identify the invalid boolean overlay: {stderr}"
    );
}
