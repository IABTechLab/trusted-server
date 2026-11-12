//! Verify the committed AWS examples through the native runtime config loader.

#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;

use error_stack::Report;
use trusted_server_core::config_payload::VerifiedConfigBlob;
use trusted_server_core::ec::storage::{EcRuntime, EcStorageSelection};
use trusted_server_core::platform::{PlatformError, PlatformSecretStore, StoreId, StoreName};

const ENVELOPES: [(&str, &str); 2] = [
    (
        "default",
        include_str!("../../../deploy/examples/native-aws-workload/example-envelope.json"),
    ),
    (
        "noindex",
        include_str!("../../../deploy/examples/native-aws-workload/example-noindex-envelope.json"),
    ),
];

struct FictionalSecrets;

#[async_trait::async_trait(?Send)]
impl PlatformSecretStore for FictionalSecrets {
    fn get_bytes(
        &self,
        store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        if store_name.as_ref() != "trusted_server_secrets" {
            return Err(Report::new(PlatformError::SecretStore));
        }
        let value = match key {
            "handler_password" => "fictional-reviewer-password",
            "publisher_proxy_secret" => "fictional-publisher-proxy-secret-32-bytes",
            "ec_passphrase" => "fictional-ec-passphrase-at-least-32-bytes",
            "browser_forwarder_secret" => "fictional-browser-forwarder-secret-32-bytes",
            _ => return Err(Report::new(PlatformError::SecretStore)),
        };
        Ok(value.as_bytes().to_vec())
    }

    async fn create(
        &self,
        _store_id: &StoreId,
        _name: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore))
    }

    async fn delete(&self, _store_id: &StoreId, _name: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore))
    }
}

#[test]
fn committed_aws_envelopes_pass_runtime_integrity_and_settings_validation() {
    let expected_keys = BTreeSet::from([
        "handler_password".to_owned(),
        "publisher_proxy_secret".to_owned(),
        "ec_passphrase".to_owned(),
        "browser_forwarder_secret".to_owned(),
    ]);
    for (name, envelope) in ENVELOPES {
        let blob = VerifiedConfigBlob::parse(envelope)
            .unwrap_or_else(|error| panic!("should verify {name} envelope: {error:?}"));
        assert_eq!(
            blob.required_secret_keys()
                .expect("should collect reference-only secret keys"),
            expected_keys,
            "should resolve only the four declared application secrets for {name}"
        );
        let settings = blob
            .into_settings(
                &FictionalSecrets,
                &StoreName::from("trusted_server_secrets"),
            )
            .unwrap_or_else(|error| panic!("should validate {name} settings: {error:?}"));
        assert_eq!(
            settings.publisher.domain, "publisher.example.com",
            "should preserve the operator-configurable publisher fixture"
        );
        assert_eq!(
            settings.publisher.proxy_secret.expose(),
            "fictional-publisher-proxy-secret-32-bytes",
            "should resolve the proxy reference without using a real provider"
        );
        let forwarder = settings
            .trusted_forwarder
            .as_ref()
            .expect("should authenticate the publisher-origin browser proxy");
        assert_eq!(forwarder.auth_header, "x-ts-forwarder-auth");
        assert!(
            forwarder.authenticates("fictional-browser-forwarder-secret-32-bytes"),
            "should resolve the browser token independently of other credentials"
        );
        assert!(!settings.auction.enabled, "should keep auctions disabled");
        let Some(EcStorageSelection::DynamoDb(storage)) = settings
            .ec
            .resolve_storage(EcRuntime::Native)
            .expect("should resolve native EC storage")
        else {
            panic!("should select DynamoDB for the {name} AWS envelope");
        };
        assert_eq!(
            storage.region, "eu-west-2",
            "should use the retained region"
        );
        assert_eq!(
            storage.table, "example-retained-ec",
            "should select the retained table"
        );
        assert_eq!(
            storage.namespace, "example-staging",
            "should preserve the stable retained namespace"
        );
        assert!(
            storage.local_endpoint.is_none(),
            "should not publish a local emulator endpoint"
        );
        assert_eq!(
            settings
                .response_headers
                .get("X-Robots-Tag")
                .map(String::as_str),
            (name == "noindex").then_some("noindex"),
            "should preserve the optional noindex configuration"
        );
    }
}

#[test]
fn rewriting_cli_float_tokens_invalidates_the_aws_envelopes() {
    for (name, envelope) in ENVELOPES {
        let rewritten =
            envelope.replacen("\"access_sample_rate\":0.0", "\"access_sample_rate\":0", 1);
        assert_ne!(
            rewritten, envelope,
            "should exercise the original float-to-integer regression for {name}"
        );
        let error = VerifiedConfigBlob::parse(&rewritten)
            .err()
            .expect("should reject numeric normalization after CLI publication");
        assert!(
            error.to_string().contains("integrity verification"),
            "should detect rewritten {name} config before startup: {error:?}"
        );
    }
}

#[test]
fn tampered_aws_envelopes_fail_runtime_integrity_verification() {
    for (name, envelope) in ENVELOPES {
        let mut tampered: serde_json::Value =
            serde_json::from_str(envelope).expect("should parse the committed envelope");
        tampered["data"]["publisher"]["domain"] =
            serde_json::json!("different.publisher.example.com");
        let tampered =
            serde_json::to_string(&tampered).expect("should serialize the tampered envelope");
        let error = VerifiedConfigBlob::parse(&tampered)
            .err()
            .expect("should reject a data change without a matching checksum");
        assert!(
            error.to_string().contains("integrity verification"),
            "should reject tampered {name} through the runtime integrity check: {error:?}"
        );
    }
}
