//! Real CLI coverage for read-only creative proxy policy diagnostics.

use std::fs;
use std::process::{Command, Output};

use serde_json::{Value, json};
use tempfile::TempDir;
use toml_edit::{Array, DocumentMut, table, value};

const CONFIG: &str = include_str!(
    "../../trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml"
);
const ENDPOINT: &str = r#"
[proxy.identity_endpoints.0]
id = "event"
origin = "https://measurement.example.com"
path_exact = "/events/impression"

[proxy.identity_endpoints.1]
id = "events"
origin = "https://measurement.example.com"
path_prefix = "/events/"
"#;

fn project(endpoints: &str, restricted: bool) -> TempDir {
    let directory = tempfile::tempdir().expect("should create config directory");
    fs::write(
        directory.path().join("edgezero.toml"),
        "[app]\nname = \"trusted-server\"\n",
    )
    .expect("should write manifest");
    let mut document = CONFIG.parse::<DocumentMut>().expect("should parse fixture");
    // The fixture has no `[proxy]` table, and the appended endpoint tables need
    // a standard table rather than the inline one indexing would create.
    document.entry("proxy").or_insert(table());
    document["proxy"]["allowed_domains"] = value(
        [if restricted {
            "other.example.com"
        } else {
            "measurement.example.com"
        }]
        .into_iter()
        .collect::<Array>(),
    );
    document["proxy"]["certificate_check"] = value(true);
    fs::write(
        directory.path().join("trusted-server.toml"),
        format!("{document}\n{endpoints}"),
    )
    .expect("should write app config");
    directory
}

fn explain(directory: &TempDir, url: &str, no_env: bool, json: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ts"));
    command
        .args(["config", "proxy", "explain", url])
        .current_dir(directory.path());
    if json {
        command.arg("--json");
    }
    if no_env {
        command.arg("--no-env");
    }
    command
        .env(
            "TRUSTED_SERVER__PROXY__IDENTITY_ENDPOINTS__0__PATH_EXACT",
            "/overlay",
        )
        .output()
        .expect("should run proxy explain")
}

fn decision(directory: &TempDir, url: &str, no_env: bool) -> Value {
    let output = explain(directory, url, no_env, true);
    assert!(
        output.status.success(),
        "should explain allowed and blocked targets: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("should emit JSON policy decision")
}

#[test]
fn default_deny_has_stable_complete_contract_without_url_secrets() {
    let directory = project("", false);
    let actual = decision(
        &directory,
        "https://measurement.example.com/pixel?ecid=secret-identity&consent=secret-consent#secret-fragment",
        true,
    );
    assert_eq!(
        actual,
        json!({
            "version": 1, "route": "creative", "fetch_allowed": true, "rule_id": null,
            "identity_eligibility": "denied", "headers": {
                "profile": "creative_resource", "forwarded": ["accept", "accept-language"],
                "user_agent": "TrustedServer/1.0", "accept_encoding": "platform_supported",
                "cors": "strip_upstream_grants"
            }, "upstream_cache": "platform_default", "downstream_cache": "origin",
            "redirects": {"policy": "strict_creative", "terminal_location": "reject"}
        }),
        "should describe default-deny resource behavior without echoing input secrets"
    );
}

#[test]
fn exact_and_prefix_rules_match_runtime_selectors_and_event_privacy() {
    let directory = project(ENDPOINT, false);
    for (path, expected) in [
        ("/events/impression", Some("event")),
        ("/events/other", Some("events")),
        ("/events-other/a", None),
        ("/events", None),
        ("/events/%69mpression", None),
    ] {
        let actual = decision(
            &directory,
            &format!("https://MEASUREMENT.example.com:443{path}"),
            true,
        );
        assert_eq!(
            actual["rule_id"],
            json!(expected),
            "should use runtime exact/prefix selector for {path}"
        );
        assert_eq!(
            actual["identity_eligibility"],
            if expected.is_some() {
                "conditional"
            } else {
                "denied"
            },
            "should require current request authorization"
        );
        assert_eq!(
            actual["upstream_cache"],
            if expected.is_some() {
                "bypass"
            } else {
                "platform_default"
            },
            "should bypass event cache even without a supplied identity"
        );
        assert_eq!(
            actual["downstream_cache"],
            if expected.is_some() {
                "private_no_store"
            } else {
                "origin"
            },
            "should retain event privacy"
        );
    }
}

#[test]
fn blocked_fetch_has_no_effective_identity_grant() {
    let directory = project(ENDPOINT, true);
    let actual = decision(
        &directory,
        "https://measurement.example.com/events/impression",
        true,
    );
    assert_eq!(
        actual["fetch_allowed"], false,
        "should retain fetch restriction"
    );
    assert_eq!(
        actual["rule_id"],
        Value::Null,
        "should suppress ineffective grant"
    );
    assert_eq!(
        actual["identity_eligibility"], "denied",
        "should deny blocked destination identity"
    );
}

#[test]
fn effective_endpoint_environment_overlay_and_no_env_use_seeded_leaf() {
    let directory = project(ENDPOINT, false);
    assert_eq!(
        decision(&directory, "https://measurement.example.com/overlay", false)["rule_id"],
        "event",
        "should apply seeded endpoint leaf override"
    );
    assert_eq!(
        decision(&directory, "https://measurement.example.com/overlay", true)["rule_id"],
        Value::Null,
        "should honor no-env"
    );
    assert_eq!(
        decision(
            &directory,
            "https://measurement.example.com/events/impression",
            true
        )["rule_id"],
        "event",
        "should preserve file endpoint"
    );
}

#[test]
fn malformed_urls_and_configuration_fail_without_echoing_url_secrets() {
    let directory = project("", false);
    for url in [
        "invalid?secret-query#secret-fragment",
        "ftp://measurement.example.com/path?secret-query",
        "https://?secret-query",
        "https://user:secret-password@measurement.example.com/path",
    ] {
        let output = explain(&directory, url, true, true);
        assert!(
            !output.status.success(),
            "should reject malformed or credentialed target"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(2),
            "should use CLI tool-error exit code"
        );
        assert!(
            stderr.contains("proxy target"),
            "should report target validation failure: {stderr}"
        );
        assert!(
            !stderr.contains("secret-"),
            "should not echo supplied URL secrets: {stderr}"
        );
    }
    let directory = project(
        &ENDPOINT.replace(
            "https://measurement.example.com",
            "http://measurement.example.com",
        ),
        false,
    );
    let output = explain(
        &directory,
        "https://measurement.example.com/events/impression",
        true,
        true,
    );
    assert!(
        !output.status.success(),
        "should reject invalid endpoint configuration"
    );
}

#[test]
fn text_output_omits_query_and_fragment_and_escapes_terminal_content() {
    let directory = project("", false);
    let output = explain(
        &directory,
        "https://measurement.example.com/\u{202e}path?secret-query#secret-fragment",
        true,
        false,
    );
    assert!(output.status.success(), "should explain text policy");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains('\u{202e}') && !stdout.contains("secret-"),
        "should render inert redacted output: {stdout}"
    );
    assert!(
        stdout.contains("default-deny"),
        "should state unmatched denial reason"
    );
}
