//! Fastly management API transport for store write operations.
//!
//! Provides [`FastlyManagementApiClient`], which wraps the Fastly REST
//! management API for write operations on config and secret stores.
//! Used by [`super::platform::FastlyPlatformConfigStore`] and
//! [`super::platform::FastlyPlatformSecretStore`] to back store write methods.
//!
//! # Credentials
//!
//! The Fastly API token is read from the `api-keys` secret store under the
//! `api_key` entry. The token must have config-store write and secret-store
//! write permissions only — no service-level admin or purge permissions.
//!
//! # Security
//!
//! Credential values are never logged. Log messages include store IDs and
//! operation names only.

use std::io::Read;

use base64::{engine::general_purpose, Engine as _};
use error_stack::{Report, ResultExt};
use fastly::http::{Method, StatusCode};
use fastly::{Request, Response};
use trusted_server_core::platform::{PlatformError, PlatformSecretStore, StoreName};

use crate::platform::FastlyPlatformSecretStore;

const FASTLY_API_HOST: &str = "https://api.fastly.com";
const API_KEYS_STORE: &str = "api-keys";
const API_KEY_ENTRY: &str = "api_key";
const ERROR_BODY_LIMIT: usize = 200;

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

fn check_response(
    response: &mut Response,
    error_kind: fn() -> PlatformError,
    operation: &str,
    entity_description: &str,
    store_id: &str,
) -> Result<(), Report<PlatformError>> {
    if response.get_status().is_success() {
        return Ok(());
    }

    let mut body = String::new();
    response
        .get_body_mut()
        .read_to_string(&mut body)
        .change_context(error_kind())?;

    Err(Report::new(error_kind()).attach(format!(
        "{} failed with HTTP {} - {} for {} in store '{}'",
        operation,
        response.get_status(),
        truncate_error_body(&body),
        entity_description,
        store_id
    )))
}

/// HTTP client for Fastly management API write operations.
///
/// Backs the `put`/`delete` methods of
/// [`super::platform::FastlyPlatformConfigStore`] and the `create`/`delete`
/// methods of [`super::platform::FastlyPlatformSecretStore`].
pub(crate) struct FastlyManagementApiClient {
    api_key: String,
    base_url: &'static str,
    backend_name: String,
}

impl FastlyManagementApiClient {
    /// Initialize the client by reading the API token from the `api-keys` secret store.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Backend`] if the management API backend cannot
    /// be registered, or [`PlatformError::SecretStore`] if the API key cannot
    /// be read.
    pub(crate) fn new() -> Result<Self, Report<PlatformError>> {
        use trusted_server_core::backend::BackendConfig;

        let backend_name = BackendConfig::from_url(FASTLY_API_HOST, true)
            .change_context(PlatformError::Backend)
            .attach("failed to register Fastly management API backend")?;

        let api_key = FastlyPlatformSecretStore
            .get_string(&StoreName::from(API_KEYS_STORE), API_KEY_ENTRY)
            .change_context(PlatformError::SecretStore)
            .attach("failed to read Fastly API key from secret store")?;

        log::debug!("FastlyManagementApiClient: initialized for management API operations");

        Ok(Self {
            api_key,
            base_url: FASTLY_API_HOST,
            backend_name,
        })
    }

    fn make_request(
        &self,
        method: Method,
        path: &str,
        body: Option<String>,
        content_type: &str,
        error_kind: fn() -> PlatformError,
    ) -> Result<Response, Report<PlatformError>> {
        let url = format!("{}{}", self.base_url, path);

        let mut request = Request::new(method, &url);

        request = request
            .with_header("Fastly-Key", &self.api_key)
            .with_header("Accept", "application/json");

        if let Some(body_content) = body {
            request = request
                .with_header("Content-Type", content_type)
                .with_body(body_content);
        }

        request.send(&self.backend_name).map_err(|e| {
            Report::new(error_kind()).attach(format!("management API request failed: {}", e))
        })
    }

    /// Update or create a config store item.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or returns a non-OK status.
    pub(crate) fn update_config_item(
        &self,
        store_id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), Report<PlatformError>> {
        let path = build_config_item_path(store_id, key);
        let payload = build_config_item_payload(value);

        let mut response = self.make_request(
            Method::PUT,
            &path,
            Some(payload),
            "application/x-www-form-urlencoded",
            || PlatformError::ConfigStore,
        )?;

        let entity_description = format!("key '{}'", key);
        check_response(
            &mut response,
            || PlatformError::ConfigStore,
            "config item update",
            &entity_description,
            store_id,
        )?;

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
    /// Returns an error if the API request fails or returns an unexpected non-2xx status.
    pub(crate) fn delete_config_item(
        &self,
        store_id: &str,
        key: &str,
    ) -> Result<(), Report<PlatformError>> {
        let path = build_config_item_path(store_id, key);

        let mut response =
            self.make_request(Method::DELETE, &path, None, "application/json", || {
                PlatformError::ConfigStore
            })?;

        if response.get_status() == StatusCode::NOT_FOUND {
            return Ok(());
        }

        let entity_description = format!("key '{}'", key);
        check_response(
            &mut response,
            || PlatformError::ConfigStore,
            "config item delete",
            &entity_description,
            store_id,
        )?;

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
    /// Returns an error if the API request fails or returns a non-OK status.
    pub(crate) fn create_secret(
        &self,
        store_id: &str,
        secret_name: &str,
        secret_value: &str,
    ) -> Result<(), Report<PlatformError>> {
        let path = build_secret_collection_path(store_id);
        let payload = build_secret_payload(secret_name, secret_value);

        let mut response = self.make_request(
            Method::POST,
            &path,
            Some(payload),
            "application/json",
            || PlatformError::SecretStore,
        )?;

        let entity_description = format!("name '{}'", secret_name);
        check_response(
            &mut response,
            || PlatformError::SecretStore,
            "secret upsert",
            &entity_description,
            store_id,
        )?;

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
    /// Returns an error if the API request fails or returns an unexpected non-2xx status.
    pub(crate) fn delete_secret(
        &self,
        store_id: &str,
        secret_name: &str,
    ) -> Result<(), Report<PlatformError>> {
        let path = build_secret_path(store_id, secret_name);

        let mut response =
            self.make_request(Method::DELETE, &path, None, "application/json", || {
                PlatformError::SecretStore
            })?;

        if response.get_status() == StatusCode::NOT_FOUND {
            return Ok(());
        }

        let entity_description = format!("name '{}'", secret_name);
        check_response(
            &mut response,
            || PlatformError::SecretStore,
            "secret delete",
            &entity_description,
            store_id,
        )?;

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
    use super::*;

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

    #[test]
    fn create_secret_uses_secret_store_error_for_transport_failures() {
        let client = FastlyManagementApiClient {
            api_key: "test-api-key".to_string(),
            base_url: FASTLY_API_HOST,
            backend_name: "missing-management-backend".to_string(),
        };

        let err = client
            .create_secret("store-id", "secret-name", "secret-value")
            .expect_err("should fail when the management API backend is unavailable");

        assert!(
            matches!(err.current_context(), &PlatformError::SecretStore),
            "should classify secret transport failures as secret-store errors"
        );
    }
}
