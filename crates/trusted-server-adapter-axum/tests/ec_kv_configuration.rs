//! Exercise real environment selection in fresh processes, never mutate the
//! parent test runner's environment or consult ambient AWS configuration.

use std::process::Command;

use trusted_server_adapter_axum::ec_kv::prepare_from_env;

#[test]
fn ec_kv_configuration_is_process_isolated() {
    let mut cases = vec!["absent", "false", "empty", "uppercase", "numeric"];
    #[cfg(not(feature = "aws-dynamodb-ec"))]
    cases.push("missing-feature");
    #[cfg(feature = "aws-dynamodb-ec")]
    cases.extend([
        "missing-binding",
        "remote-endpoint",
        "https-endpoint",
        "endpoint-userinfo",
        "endpoint-path",
        "endpoint-query",
        "endpoint-fragment",
        "endpoint-no-port",
    ]);
    for case in cases {
        let output =
            Command::new(std::env::current_exe().expect("should locate config test executable"))
                .args(["--exact", "ec_kv_configuration_subprocess", "--nocapture"])
                .env_clear()
                .env("TS_EC_CONFIGURATION_CASE", case)
                .env("AWS_EC2_METADATA_DISABLED", "true")
                .envs(configuration_bindings(case))
                .output()
                .expect("should execute isolated provider configuration test");
        assert!(
            output.status.success(),
            "configuration case {case} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn configuration_bindings(case: &str) -> Vec<(&'static str, &'static str)> {
    let mut vars = vec![
        ("TS_EC_DYNAMODB_TABLE", "ec-configuration"),
        ("TS_EC_DYNAMODB_NAMESPACE", "configuration"),
        ("TS_EC_DYNAMODB_REGION", "us-east-1"),
        ("TS_EC_DYNAMODB_LOCAL_ENDPOINT", "invalid-unused-endpoint"),
    ];
    match case {
        "absent" => {}
        "false" => vars.push(("TS_EC_DYNAMODB_ENABLED", "false")),
        "empty" => vars.push(("TS_EC_DYNAMODB_ENABLED", "")),
        "uppercase" => vars.push(("TS_EC_DYNAMODB_ENABLED", "TRUE")),
        "numeric" => vars.push(("TS_EC_DYNAMODB_ENABLED", "1")),
        "missing-feature" | "missing-binding" => vars.push(("TS_EC_DYNAMODB_ENABLED", "true")),
        endpoint_case => {
            let endpoint = match endpoint_case {
                "remote-endpoint" => "http://example.com:8000",
                "https-endpoint" => "https://127.0.0.1:8000",
                "endpoint-userinfo" => "http://user:secret@127.0.0.1:8000",
                "endpoint-path" => "http://127.0.0.1:8000/private",
                "endpoint-query" => "http://127.0.0.1:8000/?secret=value",
                "endpoint-fragment" => "http://127.0.0.1:8000/#secret",
                "endpoint-no-port" => "http://127.0.0.1",
                _ => unreachable!("unknown subprocess fixture case"),
            };
            vars.retain(|(name, _)| *name != "TS_EC_DYNAMODB_LOCAL_ENDPOINT");
            vars.push(("TS_EC_DYNAMODB_ENABLED", "true"));
            vars.push(("TS_EC_DYNAMODB_LOCAL_ENDPOINT", endpoint));
        }
    }
    if case == "missing-binding" {
        vars.retain(|(name, _)| *name != "TS_EC_DYNAMODB_TABLE");
    }
    vars
}

#[test]
fn ec_kv_configuration_subprocess() {
    let Ok(case) = std::env::var("TS_EC_CONFIGURATION_CASE") else {
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("should build isolated configuration runtime");
    let result = runtime.block_on(prepare_from_env());
    if matches!(case.as_str(), "absent" | "false") {
        assert!(
            result
                .expect("unselected backend should remain off")
                .is_none()
        );
    } else {
        let error = result.expect_err("selected invalid backend should fail closed");
        let detail = format!("{error:?}");
        if case == "missing-feature" {
            assert!(detail.contains("aws-dynamodb-ec build feature is unavailable"));
        }
        assert!(
            !detail.contains("secret"),
            "provider configuration error should not echo input"
        );
    }
}
