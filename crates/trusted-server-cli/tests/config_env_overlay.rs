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
const SANITIZE_ENV: &str = "TRUSTED_SERVER__AUCTION__SANITIZE_CREATIVES";
const GAM_ATTRIBUTION_ENV: &str = "TRUSTED_SERVER__INTEGRATIONS__GPT__GAM_ATTRIBUTION_ENABLED";
const AD_TEMPLATES_ENABLED_ENV: &str = "TRUSTED_SERVER__CREATIVE_OPPORTUNITIES__ENABLED";
const PROVIDER_ENDPOINT_ENV: &str = "TRUSTED_SERVER__AUCTION__PROVIDERS__PBS-MAIN__ENDPOINT";
const BIDDER_PROVIDER_ENV: &str = "TRUSTED_SERVER__AUCTION__BIDDERS__EXAMPLE-BIDDER__PROVIDER";

struct MigratedProject {
    directory: TempDir,
    config_path: std::path::PathBuf,
    manifest_path: std::path::PathBuf,
}

fn migrated_project() -> MigratedProject {
    let directory = tempfile::tempdir().expect("should create temporary config directory");
    let config_path = directory.path().join("trusted-server.toml");
    let manifest_path = directory.path().join("edgezero.toml");
    let mut document = LEGACY_CONFIG
        .parse::<DocumentMut>()
        .expect("should parse legacy integration config");
    // EdgeZero environment overlays cannot create missing TOML leaves,
    // so a migrated config must carry every leaf whose environment override is
    // expected to take effect.
    document["auction"]["rewrite_creatives"] = value(true);
    document["auction"]["sanitize_creatives"] = value(false);
    document["creative_opportunities"]["enabled"] = value(true);
    document["creative_opportunities"]["gam_network_id"] = value("123456789");
    document["auction"]["providers"]["pbs-main"] = toml_edit::table();
    document["auction"]["providers"]["pbs-main"]["protocol"] = value("openrtb-2.6");
    document["auction"]["providers"]["pbs-main"]["profile"] = value("standard");
    document["auction"]["providers"]["pbs-main"]["endpoint"] =
        value("https://original.example/openrtb2/auction");
    document["auction"]["bidders"]["example-bidder"] = toml_edit::table();
    document["auction"]["bidders"]["example-bidder"]["provider"] = value("pbs-main");
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
fn map_config_applies_rewrite_creatives_environment_override() {
    let project = migrated_project();
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

    assert_eq!(
        envelope["data"]["auction"]["rewrite_creatives"],
        serde_json::Value::Bool(false),
        "pushed config should contain the environment override"
    );
}

#[test]
fn migrated_config_applies_boolean_environment_overrides() {
    let project = migrated_project();
    let output = Command::new(env!("CARGO_BIN_EXE_ts"))
        .args(["config", "push", "--adapter", "axum", "--manifest"])
        .arg(&project.manifest_path)
        .arg("--app-config")
        .arg(&project.config_path)
        .args(["--yes", "--no-diff"])
        .current_dir(project.directory.path())
        .env(GAM_ATTRIBUTION_ENV, "true")
        .env(AD_TEMPLATES_ENABLED_ENV, "false")
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

    assert_eq!(
        envelope["data"]["integrations"]["gpt"]["gam_attribution_enabled"],
        serde_json::Value::Bool(true),
        "pushed config should contain the GAM attribution environment override"
    );
    assert_eq!(
        envelope["data"]["creative_opportunities"]["enabled"],
        serde_json::Value::Bool(false),
        "pushed config should contain the creative opportunities environment override"
    );
}

#[test]
fn migrated_config_applies_sanitize_creatives_environment_override() {
    let project = migrated_project();
    let output = Command::new(env!("CARGO_BIN_EXE_ts"))
        .args(["config", "push", "--adapter", "axum", "--manifest"])
        .arg(&project.manifest_path)
        .arg("--app-config")
        .arg(&project.config_path)
        .args(["--yes", "--no-diff"])
        .current_dir(project.directory.path())
        .env(SANITIZE_ENV, "true")
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

    assert_eq!(
        envelope["data"]["auction"]["sanitize_creatives"],
        serde_json::Value::Bool(true),
        "pushed config should contain the sanitize environment override"
    );
}

#[test]
fn map_config_default_rewrite_creatives_has_no_local_diff() {
    let project = migrated_project();
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
fn map_config_rejects_invalid_rewrite_creatives_environment_override() {
    let project = migrated_project();
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

#[test]
fn map_shaped_provider_and_bidder_environment_overlays_apply() {
    let project = migrated_project();
    let output = Command::new(env!("CARGO_BIN_EXE_ts"))
        .args(["config", "push", "--adapter", "axum", "--manifest"])
        .arg(&project.manifest_path)
        .arg("--app-config")
        .arg(&project.config_path)
        .args(["--yes", "--no-diff"])
        .current_dir(project.directory.path())
        .env(
            PROVIDER_ENDPOINT_ENV,
            "https://overlay.example/openrtb2/auction",
        )
        .env(BIDDER_PROVIDER_ENV, "pbs-main")
        .output()
        .expect("should run ts config push with map overlays");

    assert!(
        output.status.success(),
        "map-shaped overlays should push successfully: {}",
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

    assert_eq!(
        envelope["data"]["auction"]["providers"]["pbs-main"]["endpoint"],
        "https://overlay.example/openrtb2/auction"
    );
    assert_eq!(
        envelope["data"]["auction"]["bidders"]["example-bidder"]["provider"],
        "pbs-main"
    );
}
