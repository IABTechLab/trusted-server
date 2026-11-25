//! Early record declarations and obsolete connector selection are process-isolated.

use std::process::Command;

use edgezero_core::app::{App, Hooks as _};
use edgezero_core::router::RouterService;
use trusted_server_adapter_axum::app::TrustedServerApp;

#[test]
fn ec_kv_configuration_is_process_isolated() {
    for case in ["absent", "false", "true", "empty", "uppercase", "numeric"] {
        let mut command =
            Command::new(std::env::current_exe().expect("should locate configuration test"));
        command
            .args(["--exact", "ec_kv_configuration_subprocess", "--nocapture"])
            .env_clear()
            .env("TS_EC_CONFIGURATION_CASE", case)
            .env("TS_EC_DYNAMODB_LOCAL_ENDPOINT", "private-unused-value")
            .env("AWS_EC2_METADATA_DISABLED", "true");
        if case != "absent" {
            command.env(
                "TRUSTED_SERVER__EC_RECORDS_ENABLED",
                match case {
                    "empty" => "",
                    "uppercase" => "TRUE",
                    "numeric" => "1",
                    value => value,
                },
            );
        }
        let output = command
            .output()
            .expect("should run isolated declaration test");
        assert!(
            output.status.success(),
            "should pass declaration case {case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn ec_kv_configuration_subprocess() {
    let Ok(case) = std::env::var("TS_EC_CONFIGURATION_CASE") else {
        return;
    };
    let mut app = App::new(RouterService::builder().build());
    let result = TrustedServerApp::configure(&mut app);
    match case.as_str() {
        "absent" | "false" => {
            result.expect("should retain disabled ordinary construction");
            assert!(app.record_store_requirements().is_empty());
            assert!(TrustedServerApp::stores().kv.is_none());
        }
        "true" => {
            result.expect("should declare records before provider preparation");
            let layout = app
                .record_store_requirements()
                .get("trusted_server_kv")
                .expect("should require the logical KV store");
            assert_eq!(layout.id(), "trusted-server-ec");
            assert_eq!(
                TrustedServerApp::stores()
                    .kv
                    .expect("should declare KV metadata")
                    .ids,
                &["trusted_server_kv"]
            );
        }
        _ => {
            let error = result.expect_err("should reject malformed declaration");
            assert!(!format!("{error:?}").contains("private-unused-value"));
        }
    }
}

#[test]
fn legacy_ec_selection_is_rejected_in_isolated_processes() {
    for value in ["true", "", "TRUE", "1"] {
        let output = Command::new(std::env::current_exe().expect("should locate migration test"))
            .args(["--exact", "legacy_ec_selection_subprocess", "--nocapture"])
            .env_clear()
            .env("TS_EC_LEGACY_CASE", "selected")
            .env("TRUSTED_SERVER__EC_DYNAMODB_ENABLED", value)
            .env("TS_EC_DYNAMODB_LOCAL_ENDPOINT", "private-unused-value")
            .output()
            .expect("should run migration test");
        assert!(
            output.status.success(),
            "should reject obsolete selection without transport I/O"
        );
    }
}

#[test]
fn legacy_ec_selection_subprocess() {
    if std::env::var("TS_EC_LEGACY_CASE").is_err() {
        return;
    }
    let mut app = App::new(RouterService::builder().build());
    let error = TrustedServerApp::configure(&mut app)
        .expect_err("should reject obsolete provider selection");
    let detail = format!("{error:?}");
    assert!(
        detail.contains("TRUSTED_SERVER__EC_RECORDS_ENABLED"),
        "should name the early record declaration"
    );
    assert!(
        !detail.contains("private-unused-value"),
        "should not echo discarded connector values"
    );
}
