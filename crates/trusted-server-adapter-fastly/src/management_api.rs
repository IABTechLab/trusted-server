//! Fastly management API transport for store write operations.
//!
//! Provides [`FastlyManagementApiClient`], which wraps the Fastly REST
//! management API for write operations on config and secret stores.
//! Used by [`super::platform::FastlyPlatformConfigStore`] and
//! [`super::platform::FastlyPlatformSecretStore`] to back store write methods.
//!
//! Each call goes through the request's `EdgeZero` [`HttpClient`] as a
//! buffered request with the one second connect timeout, the
//! [`BUFFERED_TIMEOUT`] total timeout, a 1 MiB response cap, and a decoded
//! response body.
//!
//! # Credentials
//!
//! The Fastly API token is read from the `api-keys` secret store under the
//! `api_key` entry. The token must have config-store write and secret-store
//! write permissions only, with no service-level admin or purge permissions.
//!
//! # Security
//!
//! Credential values are never logged. Log messages include store IDs and
//! operation names only.

use base64::{Engine as _, engine::general_purpose};
use bytes::Bytes;
use edgezero_core::http::{Method, StatusCode, header};
use edgezero_core::outbound::ResponseDecoding;
use error_stack::{Report, ResultExt as _};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::outbound::{
    BUFFERED_TIMEOUT, OutboundRequestSource, buffered, outbound_error, outbound_request,
};
use trusted_server_core::platform::{
    HttpClient, PlatformError, PlatformSecretStore as _, StoreName,
};

use crate::platform::FastlyPlatformSecretStore;

const FASTLY_API_HOST: &str = "https://api.fastly.com";
const API_KEYS_STORE: &str = "api-keys";
const API_KEY_ENTRY: &str = "api_key";
const ERROR_BODY_LIMIT: usize = 200;
/// Cap on a management API response body.
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

fn encode_path_segment(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

pub(crate) fn build_config_item_payload(value: &str) -> String {
    format!("item_value={}", urlencoding::encode(value))
}

pub(crate) fn build_config_item_path(store_id: &str, key: &str) -> String {
    format!(
        "/resources/stores/config/{}/item/{}",
        encode_path_segment(store_id),
        encode_path_segment(key)
    )
}

fn build_secret_collection_path(store_id: &str) -> String {
    format!(
        "/resources/stores/secret/{}/secrets",
        encode_path_segment(store_id)
    )
}

fn build_secret_path(store_id: &str, secret_name: &str) -> String {
    format!(
        "/resources/stores/secret/{}/secrets/{}",
        encode_path_segment(store_id),
        encode_path_segment(secret_name)
    )
}

fn build_secret_payload(secret_name: &str, secret_value: &str) -> String {
    serde_json::json!({
        "name": secret_name,
        "secret": general_purpose::STANDARD.encode(secret_value.as_bytes()),
        "method": "create_or_recreate",
    })
    .to_string()
}

fn truncate_error_body(body: &str) -> String {
    body.trim().chars().take(ERROR_BODY_LIMIT).collect()
}

/// A management API request body with the media type its endpoint takes.
enum ManagementBody {
    /// A URL-encoded form, which the config item update takes.
    Form(String),
    /// A JSON document, which the secret upsert takes.
    Json(String),
}

impl ManagementBody {
    fn into_parts(self) -> (&'static str, Bytes) {
        match self {
            Self::Form(body) => ("application/x-www-form-urlencoded", Bytes::from(body)),
            Self::Json(body) => ("application/json", Bytes::from(body)),
        }
    }
}

/// HTTP client for Fastly management API write operations.
///
/// Backs the `put`/`delete` methods of
/// [`super::platform::FastlyPlatformConfigStore`] and the `create`/`delete`
/// methods of [`super::platform::FastlyPlatformSecretStore`].
pub(crate) struct FastlyManagementApiClient {
    api_key: String,
    base_url: &'static str,
}

impl FastlyManagementApiClient {
    /// Initialize the client by reading the API token from the `api-keys` secret store.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::SecretStore`] if the API key cannot be read.
    pub(crate) fn new() -> Result<Self, Report<PlatformError>> {
        let api_key = FastlyPlatformSecretStore::read_only()
            .get_string(&StoreName::from(API_KEYS_STORE), API_KEY_ENTRY)
            .change_context(PlatformError::SecretStore)
            .attach("failed to read Fastly API key from secret store")?;

        log::debug!("FastlyManagementApiClient: initialized for management API operations");

        Ok(Self {
            api_key,
            base_url: FASTLY_API_HOST,
        })
    }

    /// Construct a client from explicit components for tests.
    #[cfg(test)]
    pub(crate) fn with_components(api_key: String, base_url: &'static str) -> Self {
        Self { api_key, base_url }
    }

    /// Sends one management API request through `client`.
    ///
    /// A DELETE that the API answers 404 succeeds, because the entry is already
    /// gone, so retries after a partial failure converge. The response body is
    /// read only to describe a failure.
    async fn make_request(
        &self,
        client: &HttpClient,
        method: Method,
        path: &str,
        body: Option<ManagementBody>,
    ) -> Result<(), Report<TrustedServerError>> {
        let failed = || TrustedServerError::Configuration {
            message: format!("Fastly management API {method} {path} failed"),
        };
        let mut request = outbound_request(method.clone(), &format!("{}{path}", self.base_url))
            .and_then(|request| request.header("Fastly-Key", &self.api_key))
            .and_then(|request| request.header(header::ACCEPT, "application/json"))
            .map_err(|error| outbound_error(error, failed(), OutboundRequestSource::Internal))?;
        if let Some(body) = body {
            let (content_type, bytes) = body.into_parts();
            request = request
                .header(header::CONTENT_TYPE, content_type)
                .map_err(|error| outbound_error(error, failed(), OutboundRequestSource::Internal))?
                .body(bytes);
        }

        let response = client
            .send(buffered(
                request,
                BUFFERED_TIMEOUT,
                MAX_RESPONSE_BYTES,
                ResponseDecoding::Decode,
            ))
            .await
            .map_err(|error| outbound_error(error, failed(), OutboundRequestSource::Internal))?;
        let status = response.status();
        if status.is_success() || (method == Method::DELETE && status == StatusCode::NOT_FOUND) {
            return Ok(());
        }
        let body = response
            .into_bytes_bounded(MAX_RESPONSE_BYTES)
            .await
            .map_err(|error| outbound_error(error, failed(), OutboundRequestSource::Internal))?;
        Err(Report::new(failed()).attach(format!(
            "management API answered HTTP {status} - {}",
            truncate_error_body(&String::from_utf8_lossy(&body))
        )))
    }

    /// Update or create a config store item.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::ConfigStore`] if the API request fails or
    /// returns a non-2xx status.
    pub(crate) async fn update_config_item(
        &self,
        client: &HttpClient,
        store_id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), Report<PlatformError>> {
        let path = build_config_item_path(store_id, key);
        let body = ManagementBody::Form(build_config_item_payload(value));

        self.make_request(client, Method::PUT, &path, Some(body))
            .await
            .map_err(|report| {
                report
                    .change_context(PlatformError::ConfigStore)
                    .attach(format!(
                        "config item update failed for key '{key}' in store '{store_id}'"
                    ))
            })?;

        log::debug!(
            "FastlyManagementApiClient: updated config key '{}' in store '{}'",
            key,
            store_id
        );
        Ok(())
    }

    /// Delete a config store item.
    ///
    /// Returns `Ok(())` if the item does not exist (404), so retries after
    /// partial failures converge without error.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::ConfigStore`] if the API request fails or
    /// returns an unexpected non-2xx status.
    pub(crate) async fn delete_config_item(
        &self,
        client: &HttpClient,
        store_id: &str,
        key: &str,
    ) -> Result<(), Report<PlatformError>> {
        let path = build_config_item_path(store_id, key);

        self.make_request(client, Method::DELETE, &path, None)
            .await
            .map_err(|report| {
                report
                    .change_context(PlatformError::ConfigStore)
                    .attach(format!(
                        "config item delete failed for key '{key}' in store '{store_id}'"
                    ))
            })?;

        log::debug!(
            "FastlyManagementApiClient: deleted config key '{}' from store '{}'",
            key,
            store_id
        );
        Ok(())
    }

    /// Create or overwrite a secret store entry.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::SecretStore`] if the API request fails or
    /// returns a non-2xx status.
    pub(crate) async fn create_secret(
        &self,
        client: &HttpClient,
        store_id: &str,
        secret_name: &str,
        secret_value: &str,
    ) -> Result<(), Report<PlatformError>> {
        let path = build_secret_collection_path(store_id);
        let body = ManagementBody::Json(build_secret_payload(secret_name, secret_value));

        self.make_request(client, Method::POST, &path, Some(body))
            .await
            .map_err(|report| {
                report
                    .change_context(PlatformError::SecretStore)
                    .attach(format!(
                        "secret upsert failed for name '{secret_name}' in store '{store_id}'"
                    ))
            })?;

        log::debug!(
            "FastlyManagementApiClient: upserted secret '{}' in store '{}'",
            secret_name,
            store_id
        );
        Ok(())
    }

    /// Delete a secret store entry.
    ///
    /// Returns `Ok(())` if the secret does not exist (404), so retries after
    /// partial failures converge without error.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::SecretStore`] if the API request fails or
    /// returns an unexpected non-2xx status.
    pub(crate) async fn delete_secret(
        &self,
        client: &HttpClient,
        store_id: &str,
        secret_name: &str,
    ) -> Result<(), Report<PlatformError>> {
        let path = build_secret_path(store_id, secret_name);

        self.make_request(client, Method::DELETE, &path, None)
            .await
            .map_err(|report| {
                report
                    .change_context(PlatformError::SecretStore)
                    .attach(format!(
                        "secret delete failed for name '{secret_name}' in store '{store_id}'"
                    ))
            })?;

        log::debug!(
            "FastlyManagementApiClient: deleted secret '{}' from store '{}'",
            secret_name,
            store_id
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use edgezero_core::error::EdgeError;
    use edgezero_core::key_value_store::NoopKvStore;
    use edgezero_core::outbound::{OutboundCachePolicy, ResponseMode};
    use futures::executor::block_on;
    use trusted_server_core::platform::{
        ClientInfo, PlatformConfigStore, PlatformSecretStore, RuntimeServices, StoreId,
    };
    use trusted_server_core::request_signing::{KeyRotationManager, KeyRotationResult};
    use trusted_server_core::test_support::scripted_client::{
        ExpectedTransport, RecordedRequest, ScriptedHttpClient, ScriptedReply, TestClock,
    };

    use super::*;
    use crate::platform::RequestGeo;

    const BASE_URL: &str = "https://management.example.com";
    const API_KEY: &str = "example-api-key";

    fn test_management_client() -> FastlyManagementApiClient {
        FastlyManagementApiClient::with_components(API_KEY.to_owned(), BASE_URL)
    }

    fn management_transport() -> ExpectedTransport {
        ExpectedTransport {
            timeout: BUFFERED_TIMEOUT,
            response_mode: ResponseMode::Buffered {
                max_bytes: 1024 * 1024,
            },
            response_decoding: ResponseDecoding::Decode,
            cache_policy: OutboundCachePolicy::PlatformDefault,
        }
    }

    #[test]
    fn build_config_item_payload_url_encodes_reserved_characters() {
        let payload = build_config_item_payload(r#"value with spaces + symbols &= {"kid":"a+b"}"#);

        assert_eq!(
            payload,
            "item_value=value%20with%20spaces%20%2B%20symbols%20%26%3D%20%7B%22kid%22%3A%22a%2Bb%22%7D",
            "should URL-encode config item values in form payloads"
        );
    }

    #[test]
    fn build_config_item_path_url_encodes_store_id_and_key() {
        let path = build_config_item_path("store/id", "current?kid+#1");

        assert_eq!(
            path, "/resources/stores/config/store%2Fid/item/current%3Fkid%2B%231",
            "should percent-encode reserved path characters"
        );
    }

    #[test]
    fn build_secret_payload_base64_encodes_raw_secret_value() {
        let payload = build_secret_payload("signing-key", "raw-secret-value");
        let json: serde_json::Value =
            serde_json::from_str(&payload).expect("should serialize secret payload as JSON");

        assert_eq!(json["name"], "signing-key");
        assert_eq!(
            json["secret"],
            base64::engine::general_purpose::STANDARD.encode("raw-secret-value"),
            "should base64-encode the secret payload for the Fastly API"
        );
        assert_eq!(
            json["method"], "create_or_recreate",
            "should request upsert semantics so re-rotation of the same kid succeeds"
        );
    }

    #[test]
    fn truncate_error_body_limits_length_after_trimming() {
        let body = format!("  {}  ", "a".repeat(250));

        let truncated = truncate_error_body(&body);

        assert_eq!(truncated.len(), 200, "should cap error bodies at 200 chars");
        assert_eq!(truncated, "a".repeat(200), "should trim before truncating");
    }

    /// Config and secret stores whose writes go through the management client
    /// over a scripted outbound client.
    ///
    /// Reads fail, as they do for a store with no entries, so a rotation starts
    /// with no current or active kid.
    struct ScriptedManagementStores {
        management: FastlyManagementApiClient,
        client: HttpClient,
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformConfigStore for ScriptedManagementStores {
        fn get(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<String, Report<PlatformError>> {
            Err(Report::new(PlatformError::ConfigStore))
        }

        async fn put(
            &self,
            store_id: &StoreId,
            key: &str,
            value: &str,
        ) -> Result<(), Report<PlatformError>> {
            self.management
                .update_config_item(&self.client, store_id.as_ref(), key, value)
                .await
        }

        async fn delete(&self, store_id: &StoreId, key: &str) -> Result<(), Report<PlatformError>> {
            self.management
                .delete_config_item(&self.client, store_id.as_ref(), key)
                .await
        }
    }

    #[async_trait::async_trait(?Send)]
    impl PlatformSecretStore for ScriptedManagementStores {
        fn get_bytes(
            &self,
            _store_name: &StoreName,
            _key: &str,
        ) -> Result<Vec<u8>, Report<PlatformError>> {
            Err(Report::new(PlatformError::SecretStore))
        }

        async fn create(
            &self,
            store_id: &StoreId,
            name: &str,
            value: &str,
        ) -> Result<(), Report<PlatformError>> {
            self.management
                .create_secret(&self.client, store_id.as_ref(), name, value)
                .await
        }

        async fn delete(
            &self,
            store_id: &StoreId,
            name: &str,
        ) -> Result<(), Report<PlatformError>> {
            self.management
                .delete_secret(&self.client, store_id.as_ref(), name)
                .await
        }
    }

    /// Runtime services whose config and secret store writes go to `upstream`.
    fn rotation_services(upstream: &ScriptedHttpClient, clock: &TestClock) -> RuntimeServices {
        let stores = Arc::new(ScriptedManagementStores {
            management: test_management_client(),
            client: upstream.http_client(),
        });
        RuntimeServices::builder()
            .config_store(Arc::clone(&stores) as Arc<dyn PlatformConfigStore>)
            .secret_store(stores)
            .kv_store(Arc::new(NoopKvStore))
            .outbound_client(upstream.http_client())
            .clock(clock.monotonic_clock())
            .geo(Arc::new(RequestGeo::default()))
            .client_info(ClientInfo::default())
            .build()
    }

    fn rotate_example_kid(
        services: &RuntimeServices,
    ) -> Result<KeyRotationResult, Report<TrustedServerError>> {
        block_on(
            KeyRotationManager::new("config-store-id", "secret-store-id")
                .rotate_key(services, Some("example-kid".to_owned())),
        )
    }

    fn secrets_uri() -> String {
        format!("{BASE_URL}/resources/stores/secret/secret-store-id/secrets")
    }

    fn item_uri(key: &str) -> String {
        format!("{BASE_URL}/resources/stores/config/config-store-id/item/{key}")
    }

    fn methods_and_uris(requests: &[RecordedRequest]) -> Vec<(Method, String)> {
        requests
            .iter()
            .map(|request| (request.method.clone(), request.uri.to_string()))
            .collect()
    }

    #[test]
    fn rotation_sends_expected_method_headers_and_body() {
        let clock = TestClock::new();
        let upstream = ScriptedHttpClient::new(&clock);
        upstream.respond_to(
            Method::POST,
            &secrets_uri(),
            ScriptedReply::status(StatusCode::OK),
        );
        for key in ["example-kid", "active-kids", "current-kid"] {
            upstream.respond_to(
                Method::PUT,
                &item_uri(key),
                ScriptedReply::status(StatusCode::OK),
            );
        }

        let rotation = rotate_example_kid(&rotation_services(&upstream, &clock))
            .expect("should rotate through the management API");

        assert_eq!(rotation.new_kid, "example-kid", "should use the given kid");
        assert!(
            rotation.previous_kid.is_none(),
            "should find no previous kid in an empty store"
        );
        assert_eq!(
            rotation.active_kids,
            vec!["example-kid".to_owned()],
            "should activate only the new kid"
        );
        let requests = upstream.requests();
        assert_eq!(
            methods_and_uris(&requests),
            vec![
                (Method::POST, secrets_uri()),
                (Method::PUT, item_uri("example-kid")),
                (Method::PUT, item_uri("active-kids")),
                (Method::PUT, item_uri("current-kid")),
            ],
            "should write the private key, the JWK, active-kids, then current-kid"
        );
        for request in &requests {
            request.assert_transport(management_transport());
        }

        let upsert_request = &requests[0];
        upsert_request.assert_headers(&[
            ("fastly-key", API_KEY),
            ("accept", "application/json"),
            ("content-type", "application/json"),
        ]);
        let upsert: serde_json::Value = serde_json::from_slice(&upsert_request.body)
            .expect("should send the secret upsert as JSON");
        assert_eq!(
            (&upsert["name"], &upsert["method"]),
            (
                &serde_json::json!("example-kid"),
                &serde_json::json!("create_or_recreate")
            ),
            "should upsert the secret under the new kid"
        );
        let stored_key = upsert["secret"]
            .as_str()
            .and_then(|secret| general_purpose::STANDARD.decode(secret).ok())
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .expect("should send the secret as base64 of a text value");
        let signing_key = general_purpose::STANDARD
            .decode(stored_key)
            .expect("should store the signing key as base64 text");
        assert_eq!(
            signing_key.len(),
            32,
            "should store a 32-byte Ed25519 signing key"
        );

        let jwk = serde_json::to_string(&rotation.jwk).expect("should serialize the rotated JWK");
        let item_bodies = [
            format!("item_value={}", urlencoding::encode(&jwk)),
            "item_value=example-kid".to_owned(),
            "item_value=example-kid".to_owned(),
        ];
        for (request, body) in requests[1..].iter().zip(item_bodies) {
            request.assert_headers(&[
                ("fastly-key", API_KEY),
                ("accept", "application/json"),
                ("content-type", "application/x-www-form-urlencoded"),
            ]);
            assert_eq!(
                request.body.as_ref(),
                body.as_bytes(),
                "should send {} {} its config item value as a URL-encoded form",
                request.method,
                request.uri
            );
        }
    }

    #[test]
    fn rotation_rolls_back_jwk_and_private_key_when_active_kids_write_fails() {
        let clock = TestClock::new();
        let upstream = ScriptedHttpClient::new(&clock);
        let secret_uri = format!("{}/example-kid", secrets_uri());
        upstream
            .respond_to(
                Method::POST,
                &secrets_uri(),
                ScriptedReply::status(StatusCode::OK),
            )
            .respond_to(
                Method::PUT,
                &item_uri("example-kid"),
                ScriptedReply::status(StatusCode::OK),
            )
            .respond_to(
                Method::PUT,
                &item_uri("active-kids"),
                ScriptedReply::status(StatusCode::INTERNAL_SERVER_ERROR),
            )
            .respond_to(
                Method::DELETE,
                &item_uri("example-kid"),
                ScriptedReply::status(StatusCode::OK),
            )
            .respond_to(
                Method::DELETE,
                &secret_uri,
                ScriptedReply::status(StatusCode::OK),
            );

        let error = rotate_example_kid(&rotation_services(&upstream, &clock))
            .expect_err("should fail when the active-kids write fails");

        assert!(
            matches!(
                error.current_context(),
                TrustedServerError::Configuration { message } if message == "failed to update active-kids"
            ),
            "should report the failed active-kids write"
        );
        let requests = upstream.requests();
        assert_eq!(
            methods_and_uris(&requests),
            vec![
                (Method::POST, secrets_uri()),
                (Method::PUT, item_uri("example-kid")),
                (Method::PUT, item_uri("active-kids")),
                (Method::DELETE, item_uri("example-kid")),
                (Method::DELETE, secret_uri),
            ],
            "should roll back the JWK, then the private key, and never flip current-kid"
        );
        for rollback in &requests[3..] {
            rollback.assert_transport(management_transport());
            rollback.assert_headers(&[("fastly-key", API_KEY), ("accept", "application/json")]);
            assert!(
                rollback.body.is_empty(),
                "should send a rollback DELETE with no body"
            );
        }
    }

    #[test]
    fn non_2xx_management_response_truncates_body_to_200_chars() {
        let clock = TestClock::new();
        let upstream = ScriptedHttpClient::new(&clock);
        let uri = format!("{BASE_URL}/resources/stores/config/config-store-id/item/current-kid");
        upstream.respond_to(
            Method::PUT,
            &uri,
            ScriptedReply::status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(format!("  {}example-tail  ", "x".repeat(200))),
        );

        let error = block_on(test_management_client().update_config_item(
            &upstream.http_client(),
            "config-store-id",
            "current-kid",
            "example-kid",
        ))
        .expect_err("should fail when the management API answers 500");

        assert!(
            matches!(error.current_context(), PlatformError::ConfigStore),
            "should classify the failed write as a config-store error"
        );
        let answer = error
            .frames()
            .filter_map(|frame| frame.downcast_ref::<String>())
            .find(|text| text.starts_with("management API answered"))
            .expect("should attach the management API answer");
        assert_eq!(
            answer,
            &format!(
                "management API answered HTTP 500 Internal Server Error - {}",
                "x".repeat(200)
            ),
            "should keep the trimmed body truncated to 200 chars"
        );
    }

    #[test]
    fn delete_treats_missing_entry_as_deleted() {
        let clock = TestClock::new();
        let upstream = ScriptedHttpClient::new(&clock);
        let item_uri =
            format!("{BASE_URL}/resources/stores/config/config-store-id/item/example-kid");
        let secret_uri =
            format!("{BASE_URL}/resources/stores/secret/secret-store-id/secrets/example-kid");
        upstream
            .respond_to(
                Method::DELETE,
                &item_uri,
                ScriptedReply::status(StatusCode::NOT_FOUND),
            )
            .respond_to(
                Method::DELETE,
                &secret_uri,
                ScriptedReply::status(StatusCode::NOT_FOUND),
            )
            .respond_to(
                Method::PUT,
                &item_uri,
                ScriptedReply::status(StatusCode::NOT_FOUND),
            );
        let client = upstream.http_client();
        let management = test_management_client();

        block_on(management.delete_config_item(&client, "config-store-id", "example-kid"))
            .expect("should treat a missing config item as deleted");
        block_on(management.delete_secret(&client, "secret-store-id", "example-kid"))
            .expect("should treat a missing secret as deleted");
        let error = block_on(management.update_config_item(
            &client,
            "config-store-id",
            "example-kid",
            "example-value",
        ))
        .expect_err("should fail a PUT that the management API answers 404");

        assert!(
            matches!(error.current_context(), PlatformError::ConfigStore),
            "should classify the failed write as a config-store error"
        );
        let requests = upstream.requests();
        assert_eq!(requests.len(), 3, "should send each management call once");
        for delete in &requests[..2] {
            assert_eq!(delete.method, Method::DELETE, "should send a DELETE");
            delete.assert_transport(management_transport());
            delete.assert_headers(&[("fastly-key", API_KEY), ("accept", "application/json")]);
            assert!(delete.body.is_empty(), "should send a DELETE with no body");
        }
    }

    #[test]
    fn create_secret_uses_secret_store_error_for_transport_failures() {
        let clock = TestClock::new();
        let upstream = ScriptedHttpClient::new(&clock);
        upstream.respond_to(
            Method::POST,
            &format!("{BASE_URL}/resources/stores/secret/store-id/secrets"),
            ScriptedReply::error(EdgeError::bad_gateway("example connection failure")),
        );

        let err = block_on(test_management_client().create_secret(
            &upstream.http_client(),
            "store-id",
            "secret-name",
            "secret-value",
        ))
        .expect_err("should fail when the management API is unreachable");

        assert!(
            matches!(err.current_context(), &PlatformError::SecretStore),
            "should classify secret transport failures as secret-store errors"
        );
        assert!(
            err.downcast_ref::<EdgeError>().is_some(),
            "should keep the transport error on the report"
        );
    }
}
