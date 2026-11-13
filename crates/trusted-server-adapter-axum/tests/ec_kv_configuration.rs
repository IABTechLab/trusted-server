//! The obsolete launch contract is rejected without reading AWS binding values.

use std::process::Command;

#[cfg(not(feature = "aws-dynamodb-ec"))]
use trusted_server_adapter_axum::ec_kv::prepare;
use trusted_server_adapter_axum::ec_kv::reject_legacy_configuration;
#[cfg(not(feature = "aws-dynamodb-ec"))]
use trusted_server_core::ec::storage::DynamoDbEcStorage;

#[test]
fn legacy_ec_selection_is_rejected_in_isolated_processes() {
    for case in ["absent", "false", "true", "empty", "uppercase", "numeric"] {
        let mut command =
            Command::new(std::env::current_exe().expect("should locate config test executable"));
        command
            .args(["--exact", "legacy_ec_selection_subprocess", "--nocapture"])
            .env_clear()
            .env("TS_EC_CONFIGURATION_CASE", case)
            .env("TS_EC_DYNAMODB_TABLE", "orphaned-unused-table")
            .env("TS_EC_DYNAMODB_NAMESPACE", "orphaned-unused-namespace")
            .env("TS_EC_DYNAMODB_LOCAL_ENDPOINT", "invalid-secret-endpoint");
        if case != "absent" {
            command.env(
                "TS_EC_DYNAMODB_ENABLED",
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
            .expect("should execute isolated migration check");
        assert!(
            output.status.success(),
            "should pass case {case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn legacy_ec_selection_subprocess() {
    let Ok(case) = std::env::var("TS_EC_CONFIGURATION_CASE") else {
        return;
    };
    let result = reject_legacy_configuration();
    if matches!(case.as_str(), "absent" | "false") {
        result.expect("should ignore inactive legacy selection and orphaned bindings");
    } else {
        let detail = format!(
            "{:?}",
            result.expect_err("should reject obsolete or malformed selection")
        );
        assert!(
            detail.contains("ec.storage"),
            "should explain publication migration"
        );
        assert!(
            !detail.contains("invalid-secret-endpoint"),
            "should not echo legacy input"
        );
    }
}

#[cfg(not(feature = "aws-dynamodb-ec"))]
#[tokio::test]
async fn typed_dynamodb_selection_fails_without_compiled_provider() {
    let error = prepare(&DynamoDbEcStorage {
        region: "us-east-1".to_owned(),
        table: "example-identity".to_owned(),
        namespace: "fixture".to_owned(),
        local_endpoint: None,
    })
    .await
    .expect_err("should reject selected provider before network access");
    assert!(format!("{error:?}").contains("aws-dynamodb-ec build feature is unavailable"));
}
