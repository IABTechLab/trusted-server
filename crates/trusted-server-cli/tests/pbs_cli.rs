//! Exercise the actual CLI and process adapter against a local fake AWS executable.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::{Value, json};
use tempfile::TempDir;

const TOKEN: &str = "11111111-2222-4333-8444-555555555555";

fn fixture() -> TempDir {
    let dir = tempfile::tempdir().expect("should create fixture directory");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/pbs");
    for name in ["deployment.yaml", "pbs.yaml", "east.yaml", "bindings.json"] {
        fs::copy(source.join(name), dir.path().join(name)).expect("should copy fixture");
    }
    let fake = dir.path().join("aws");
    fs::write(
        &fake,
        r#"#!/usr/bin/env python3
import json, os, pathlib, stat, sys
args = sys.argv[1:]
root = pathlib.Path(os.environ['PBS_FAKE_ROOT'])
with (root / 'calls').open('a') as log:
    log.write(json.dumps(args) + '\n')
assert '--profile' in args and args[args.index('--profile')+1] == 'pbs-sandbox'
if 'configure' in args:
    if os.environ.get('PBS_FAKE_HISTORY') == 'enabled':
        print('enabled')
    else:
        print('disabled')
    sys.exit(0)
assert args[args.index('--region')+1] == 'us-east-1'
assert os.environ.get('AWS_IGNORE_CONFIGURED_ENDPOINT_URLS') == 'true'
path = pathlib.Path(args[args.index('--cli-input-json')+1].removeprefix('file://'))
assert stat.S_IMODE(path.stat().st_mode) == 0o600
with (root / 'payload_paths').open('a') as paths:
    paths.write(str(path) + '\n')
request = json.loads(path.read_text())
arn = 'arn:aws:secretsmanager:us-east-1:123456789012:secret:pbs/example-AbCdEf'
if 'get-caller-identity' in args:
    print(json.dumps({'Account': os.environ.get('PBS_FAKE_ACCOUNT', '123456789012')}))
elif 'describe-secret' in args:
    print(json.dumps({'ARN': arn}))
elif 'put-secret-value' in args:
    if os.environ.get('PBS_FAKE_FAILURE') == 'yes':
        print(request['SecretString'], file=sys.stderr)
        sys.exit(1)
    (root / 'captured_request.json').write_text(json.dumps(request))
    print(json.dumps({'ARN': arn, 'VersionId': request['ClientRequestToken']}))
elif 'describe-instance-status' in args:
    print(json.dumps({'InstanceStatuses': []}))
else:
    sys.exit(2)
"#,
    )
    .expect("should write fake AWS executable");
    fs::set_permissions(fake, fs::Permissions::from_mode(0o700))
        .expect("should set executable mode");
    dir
}

fn command(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ts"));
    command
        .current_dir(dir)
        .env("PBS_FAKE_ROOT", dir)
        .env(
            "PATH",
            format!(
                "{}:{}",
                dir.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("AWS_ACCESS_KEY_ID")
        .env_remove("AWS_SECRET_ACCESS_KEY")
        .env_remove("AWS_SESSION_TOKEN")
        .env("AWS_EC2_METADATA_DISABLED", "true");
    command
}

fn secret_command(dir: &Path) -> Command {
    let mut command = command(dir);
    command.args([
        "prebid",
        "server",
        "secrets",
        "set",
        "examplebidder",
        "--deployment",
        "deployment.yaml",
        "--region",
        "us-east-1",
        "--file",
        "secret.json",
        "--yes",
        "--request-token",
        TOKEN,
        "--json",
    ]);
    command
}

fn assert_no_secret(output: &Output) {
    assert!(!String::from_utf8_lossy(&output.stdout).contains("DUMMY_SECRET"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("DUMMY_SECRET"));
}

fn assert_payload_cleanup(dir: &Path) {
    let paths =
        fs::read_to_string(dir.join("payload_paths")).expect("should capture payload paths");
    for path in paths.lines() {
        assert!(
            !Path::new(path).exists(),
            "should remove tool-created request payloads"
        );
    }
}

#[test]
fn local_commands_never_execute_aws_and_preserve_the_source() {
    let dir = fixture();
    let toml = "[integrations.prebid]\nenabled=false\nbidders=['examplebidder']\naccount_id='DUMMY_SECRET'\n";
    fs::write(dir.path().join("trusted-server.toml"), toml).expect("should write TOML");
    let output = command(dir.path())
        .args(["prebid", "server", "inspect", "--json"])
        .output()
        .expect("should run CLI");
    assert!(output.status.success());
    assert_no_secret(&output);
    let report: Value = serde_json::from_slice(&output.stdout).expect("should emit JSON");
    assert_eq!(report["enabled_explicit"], false);
    assert_eq!(
        fs::read_to_string(dir.path().join("trusted-server.toml")).expect("should read TOML"),
        toml
    );
    let output = command(dir.path())
        .args([
            "prebid",
            "server",
            "check",
            "--deployment",
            "deployment.yaml",
            "--json",
        ])
        .output()
        .expect("should run checks");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !dir.path().join("calls").exists(),
        "local commands must not invoke AWS"
    );
}

#[test]
fn secret_payload_is_private_not_in_argv_and_deleted_after_use() {
    let dir = fixture();
    let input = json!({"api_key": "DUMMY_SECRET-$\"\nvalue"}).to_string();
    fs::write(dir.path().join("secret.json"), &input).expect("should write dummy credential");
    let output = secret_command(dir.path())
        .output()
        .expect("should execute CLI");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_no_secret(&output);
    let calls =
        fs::read_to_string(dir.path().join("calls")).expect("should read captured arguments");
    assert!(
        !calls.contains("DUMMY_SECRET"),
        "secret values must not appear in arguments"
    );
    let request: Value = serde_json::from_str(
        &fs::read_to_string(dir.path().join("captured_request.json"))
            .expect("should read dummy request"),
    )
    .expect("should parse request");
    assert_eq!(request["ClientRequestToken"], TOKEN);
    let secret: Value = serde_json::from_str(
        request["SecretString"]
            .as_str()
            .expect("should have string payload"),
    )
    .expect("should parse payload");
    assert_eq!(
        secret,
        serde_json::from_str::<Value>(&input).expect("should parse fixture")
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("secret.json")).expect("should preserve input"),
        input
    );
    assert_payload_cleanup(dir.path());
}

#[test]
fn aws_errors_never_forward_provider_stderr_and_cleanup_payloads() {
    let dir = fixture();
    fs::write(
        dir.path().join("secret.json"),
        "{\"api_key\":\"DUMMY_SECRET\"}",
    )
    .expect("should write fixture");
    let output = secret_command(dir.path())
        .env("PBS_FAKE_FAILURE", "yes")
        .output()
        .expect("should run CLI");
    assert_eq!(output.status.code(), Some(2));
    assert_no_secret(&output);
    assert_payload_cleanup(dir.path());
}

#[test]
fn enabled_cli_history_and_wrong_accounts_block_writes() {
    for (setting, value) in [
        ("PBS_FAKE_HISTORY", "enabled"),
        ("PBS_FAKE_ACCOUNT", "999999999999"),
    ] {
        let dir = fixture();
        fs::write(
            dir.path().join("secret.json"),
            "{\"api_key\":\"DUMMY_SECRET\"}",
        )
        .expect("should write fixture");
        let output = secret_command(dir.path())
            .env(setting, value)
            .output()
            .expect("should run CLI");
        assert_eq!(output.status.code(), Some(2));
        assert_no_secret(&output);
        let calls = fs::read_to_string(dir.path().join("calls")).expect("should read calls");
        assert!(!calls.contains("put-secret-value"));
        assert!(!dir.path().join("captured_request.json").exists());
        assert_payload_cleanup(dir.path());
    }
}

#[test]
fn incomplete_status_returns_json_and_nonzero_exit() {
    let dir = fixture();
    let output = command(dir.path())
        .args([
            "prebid",
            "server",
            "status",
            "--deployment",
            "deployment.yaml",
            "--json",
        ])
        .output()
        .expect("should run status");
    assert_eq!(output.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&output.stdout).expect("should emit partial JSON");
    assert_eq!(report["complete"], false);
    assert_eq!(
        report["regions"][0]["instances"][0]["pbs_health"],
        "unknown"
    );
}
