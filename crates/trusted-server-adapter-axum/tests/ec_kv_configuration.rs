//! Exercise early record declarations in isolated processes without provider I/O.

use std::process::Command;

use edgezero_core::app::{App, Hooks as _};
use edgezero_core::router::RouterService;
use trusted_server_adapter_axum::app::TrustedServerApp;

#[test]
fn ec_kv_configuration_is_process_isolated() {
    for case in [
        "absent",
        "false",
        "empty",
        "uppercase",
        "numeric",
        "selected",
    ] {
        let output =
            Command::new(std::env::current_exe().expect("should locate config test executable"))
                .args(["--exact", "ec_kv_configuration_subprocess", "--nocapture"])
                .env_clear()
                .env("TS_EC_CONFIGURATION_CASE", case)
                // These old connector settings must not open another provider.
                .env("TS_EC_DYNAMODB_TABLE", "unused-fixture")
                .env("TS_EC_DYNAMODB_LOCAL_ENDPOINT", "invalid-unused-endpoint")
                .env("TS_EC_DYNAMODB_REGION", "unused")
                .env("AWS_EC2_METADATA_DISABLED", "true")
                .envs(match case {
                    "absent" => Vec::new(),
                    "false" => vec![("TS_EC_DYNAMODB_ENABLED", "false")],
                    "empty" => vec![("TS_EC_DYNAMODB_ENABLED", "")],
                    "uppercase" => vec![("TS_EC_DYNAMODB_ENABLED", "TRUE")],
                    "numeric" => vec![("TS_EC_DYNAMODB_ENABLED", "1")],
                    "selected" => vec![("TS_EC_DYNAMODB_ENABLED", "true")],
                    _ => unreachable!("should select a known fixture"),
                })
                .output()
                .expect("should run isolated declaration test");
        assert!(
            output.status.success(),
            "configuration case {case} failed: {}",
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
            result.expect("should retain ordinary disabled construction");
            assert!(app.record_store_requirements().is_empty());
            assert!(TrustedServerApp::stores().kv.is_none());
        }
        "selected" if cfg!(feature = "aws-dynamodb-ec") => {
            result.expect("should declare records before preparation without opening a provider");
            let layout = app
                .record_store_requirements()
                .get("trusted_server_kv")
                .expect("should require the declared logical KV store");
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
            let error = result.expect_err("should reject invalid or unsupported record selection");
            assert!(
                !format!("{error:?}").contains("invalid-unused-endpoint"),
                "should not echo connector inputs"
            );
        }
    }
}
