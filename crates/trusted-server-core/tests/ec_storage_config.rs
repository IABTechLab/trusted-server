//! Provider input remains pure publication data, not runtime or credential lookup.

use edgezero_core::blob_envelope::BlobEnvelope;
use serde_json::json;
use trusted_server_core::config::TrustedServerAppConfig;
use trusted_server_core::config_payload::VerifiedConfigBlob;
use trusted_server_core::ec::storage::{EcRuntime, EcStorageSelection};
use trusted_server_core::settings::Settings;

fn source(storage: &str) -> String {
    format!(
        r#"
        [[handlers]]
        path = "^/_ts/admin"
        username = "fixture"
        password = "fixture-password"
        [publisher]
        domain = "publisher.example.com"
        cookie_domain = ".publisher.example.com"
        origin_url = "https://origin.example.com"
        proxy_secret = "unit-test-proxy-secret"
        [ec]
        passphrase = "test-secret-key-32-bytes-minimum"
        {storage}
    "#
    )
}

#[test]
fn every_runtime_resolves_only_its_supported_explicit_provider() {
    let rows = [
        ("", ["none", "none", "none", "none"]),
        (
            r#"ec_store = "exact-fixture-binding""#,
            ["fastly", "none", "none", "none"],
        ),
        (
            "[ec.storage]\nstore = \"exact-fixture-binding\"",
            ["fastly", "error", "error", "error"],
        ),
        (
            "[ec.storage]\nprovider = \"fastly\"\nstore = \"exact-fixture-binding\"",
            ["fastly", "error", "error", "error"],
        ),
        (
            "[ec.storage]\nprovider = \"dynamodb\"\nregion = \"us-east-1\"\ntable = \"example-identity\"\nnamespace = \" exact \"",
            ["error", "dynamodb", "error", "error"],
        ),
        (
            "[ec.storage]\nprovider = \"celld\"\nendpoint = \"http://127.0.0.1:7780\"\nstore = \"identity\"\n[ec.storage.access]\nmode = \"trusted-network\"",
            ["error", "celld", "error", "error"],
        ),
    ];
    for (input, expected) in rows {
        let settings =
            Settings::from_toml(&source(input)).expect("should parse shape independent of runtime");
        for (runtime, expected) in [
            EcRuntime::Fastly,
            EcRuntime::Native,
            EcRuntime::Cloudflare,
            EcRuntime::Spin,
        ]
        .into_iter()
        .zip(expected)
        {
            let actual = match settings.ec.resolve_storage(runtime) {
                Ok(None) => "none",
                Ok(Some(EcStorageSelection::Fastly(store))) => {
                    assert_eq!(store, "exact-fixture-binding");
                    "fastly"
                }
                Ok(Some(EcStorageSelection::DynamoDb(config))) => {
                    assert_eq!(config.namespace, " exact ");
                    "dynamodb"
                }
                Ok(Some(EcStorageSelection::Celld(_))) => "celld",
                Err(_) => "error",
            };
            assert_eq!(
                actual, expected,
                "should resolve runtime {runtime:?} for {input}"
            );
        }
        let value = serde_json::to_value(&settings).expect("should serialize input");
        let published: TrustedServerAppConfig = serde_json::from_value(value.clone())
            .expect("should preserve runtime-independent CLI input");
        let envelope = BlobEnvelope::new(
            serde_json::to_value(published).expect("should publish typed input"),
            "2026-01-01T00:00:00Z".to_owned(),
        );
        assert_eq!(
            envelope.data, value,
            "should not inject defaults into published envelope"
        );
        let encoded = serde_json::to_string(&envelope).expect("should encode existing envelope");
        VerifiedConfigBlob::parse(&encoded)
            .expect("should reload and verify unchanged envelope format");
        let roundtrip =
            Settings::from_json_value(value.clone()).expect("should reload published shape");
        assert_eq!(
            serde_json::to_value(roundtrip).expect("should serialize roundtrip"),
            value
        );
        if !input.contains("provider") {
            assert!(
                value["ec"]["storage"].get("provider").is_none(),
                "should not publish an inferred provider"
            );
        }
    }
}

#[test]
fn ec_storage_published_shape_accepts_explicit_celld_without_credentials() {
    let settings = Settings::from_toml(&source(
        r#"
        [ec.storage]
        provider = "celld"
        endpoint = "https://stores.example.com:7780"
        store = "identity"
        [ec.storage.access]
        mode = "authenticated"
        token_env = "FIXTURE_STORE_TOKEN"
    "#,
    ))
    .expect("should accept pure celld config without reading a token");
    let value = serde_json::to_value(&settings).expect("should serialize publication input");
    assert_eq!(value["ec"]["storage"]["provider"], "celld");
    assert_eq!(
        value["ec"]["storage"]["access"],
        json!({"mode":"authenticated","token_env":"FIXTURE_STORE_TOKEN"})
    );
}

#[test]
fn absent_ec_storage_stays_absent_in_json() {
    let settings = Settings::from_toml(&source("")).expect("should preserve unconfigured settings");
    let value = serde_json::to_value(settings).expect("should serialize settings");
    assert!(
        value["ec"].get("storage").is_none(),
        "should not inject provider defaults or null storage"
    );
}

#[test]
fn ambiguous_incomplete_unknown_and_inappropriate_fields_are_rejected() {
    for input in [
        "ec_store = \"same\"\n[ec.storage]\nstore = \"same\"",
        "[ec.storage]",
        "[ec.storage]\nstore = \"\"",
        "[ec.storage]\nprovider = \"unknown\"\nstore = \"identity\"",
        "[ec.storage]\nprovider = \"fastly\"\nstore = \"identity\"\nregion = \"us-east-1\"",
        "[ec.storage]\nprovider = \"dynamodb\"\ntable = \"example-identity\"\nnamespace = \"fixture\"",
        "[ec.storage]\nprovider = \"dynamodb\"\nregion = \"us-east-1\"\ntable = \"example-identity\"\nnamespace = \"\"",
        "[ec.storage]\nprovider = \"celld\"\nendpoint = \"https://stores.example.com\"\nstore = \"identity\"",
        "[ec.storage]\nprovider = \"celld\"\nendpoint = \"http://stores.example.com\"\nstore = \"identity\"\n[ec.storage.access]\nmode = \"authenticated\"\ntoken_env = \"FIXTURE_STORE_TOKEN\"",
    ] {
        let error = Settings::from_toml(&source(input))
            .expect_err("should reject invalid provider input without a runtime");
        assert!(
            format!("{error:?}").contains("ec.storage"),
            "should identify EC storage configuration"
        );
    }
}

#[test]
fn typed_local_endpoint_preserves_emulator_restrictions_and_redacts_errors() {
    for endpoint in [
        "http://example.com:8000",
        "https://127.0.0.1:8000",
        "http://user:secret@127.0.0.1:8000",
        "http://127.0.0.1:8000/private",
        "http://127.0.0.1:8000/?secret=value",
        "http://127.0.0.1:8000/#secret",
        "http://127.0.0.1",
    ] {
        let input = format!(
            "[ec.storage]\nprovider = \"dynamodb\"\nregion = \"us-east-1\"\ntable = \"example-identity\"\nnamespace = \"fixture\"\nlocal_endpoint = {endpoint:?}"
        );
        let error = Settings::from_toml(&source(&input))
            .expect_err("should reject a nonlocal or ambiguous endpoint");
        let rendered = format!("{error:?}");
        assert!(rendered.contains("ec.storage.local_endpoint"));
        assert!(
            !rendered.contains(endpoint),
            "should not echo endpoint values"
        );
    }
}
