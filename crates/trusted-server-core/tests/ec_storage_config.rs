//! Record references remain pure publication data, independent of the CLI host.

use edgezero_core::blob_envelope::BlobEnvelope;
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
fn every_runtime_resolves_only_supported_record_references() {
    for (input, expected) in [
        ("", ["none", "none", "none", "none"]),
        (
            r#"ec_store = "exact-fixture-binding""#,
            ["fastly", "error", "error", "error"],
        ),
        (
            "[ec.storage]\nstore='exact-fixture-binding'",
            ["fastly", "error", "error", "error"],
        ),
        (
            "[ec.storage]\nprovider='fastly'\nstore='exact-fixture-binding'",
            ["fastly", "error", "error", "error"],
        ),
        (
            "[ec.storage]\nprovider='records'\nstore='exact-fixture-binding'",
            ["records", "records", "error", "error"],
        ),
    ] {
        let settings =
            Settings::from_toml(&source(input)).expect("should parse without provider I/O");
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
                Ok(Some(EcStorageSelection::Records(store))) => {
                    assert_eq!(store, "exact-fixture-binding");
                    "records"
                }
                Err(_) => "error",
            };
            assert_eq!(
                actual, expected,
                "should resolve actual runtime {runtime:?}"
            );
        }
        let value = serde_json::to_value(&settings).expect("should serialize pure input");
        let published: TrustedServerAppConfig =
            serde_json::from_value(value.clone()).expect("should preserve CLI publication input");
        let envelope = BlobEnvelope::new(
            serde_json::to_value(published).expect("should publish typed input"),
            "2026-01-01T00:00:00Z".to_owned(),
        );
        assert_eq!(
            envelope.data, value,
            "should not infer a provider during publication"
        );
        VerifiedConfigBlob::parse(
            &serde_json::to_string(&envelope).expect("should encode envelope"),
        )
        .expect("should reload unchanged envelope format");
        let roundtrip =
            Settings::from_json_value(value.clone()).expect("should reload published reference");
        assert_eq!(
            serde_json::to_value(roundtrip).expect("should serialize roundtrip"),
            value
        );
        if !input.contains("provider") {
            assert!(
                value["ec"]["storage"].get("provider").is_none(),
                "should retain omitted-provider shorthand"
            );
        }
    }
}

#[test]
fn absent_storage_stays_absent() {
    let value = serde_json::to_value(
        Settings::from_toml(&source("")).expect("should parse unconfigured identity"),
    )
    .expect("should serialize settings");
    assert!(
        value["ec"].get("storage").is_none(),
        "should not inject null or provider defaults"
    );
}

#[test]
fn connector_configuration_is_not_accepted_or_echoed_as_identity_data() {
    for input in [
        "ec_store='same'\n[ec.storage]\nprovider='records'\nstore='same'",
        "[ec.storage]",
        "[ec.storage]\nprovider='records'\nstore=''",
        "[ec.storage]\nprovider='unknown'\nstore='identity'",
        "[ec.storage]\nprovider='records'\nstore='identity'\nendpoint='https://private.example.com/secret-value'",
        "[ec.storage]\nprovider='records'\nstore='identity'\nregion='secret-value'",
        "[ec.storage]\nprovider='records'\nstore='identity'\ntoken='secret-value'",
        "[ec.storage]\nprovider='dynamodb'\nregion='us-east-1'\ntable='example-identity'\nnamespace='fixture'",
        "[ec.storage]\nprovider='celld'\nendpoint='https://private.example.com/secret-value'\nstore='identity'",
    ] {
        let error = Settings::from_toml(&source(input))
            .expect_err("should reject misplaced connector configuration");
        let detail = format!("{error:?}");
        assert!(
            detail.contains("ec.storage"),
            "should identify the configuration field"
        );
        assert!(
            !detail.contains("secret-value"),
            "should not echo private connector values"
        );
    }
}
