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

use error_stack::{Report, ResultExt};
use fastly::{Request, Response};
use trusted_server_core::platform::{PlatformError, PlatformSecretStore, StoreName};

use crate::platform::FastlyPlatformSecretStore;

const FASTLY_API_HOST: &str = "https://api.fastly.com";
const API_KEYS_STORE: &str = "api-keys";
const API_KEY_ENTRY: &str = "api_key";

pub(crate) fn build_config_item_payload(value: &str) -> String {
    format!("item_value={}", urlencoding::encode(value))
}

/// HTTP client for Fastly management API write operations.
///
/// Backs the `put`/`delete` methods of [`FastlyPlatformConfigStore`] and
/// the `create`/`delete` methods of [`FastlyPlatformSecretStore`].
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
        method: &str,
        path: &str,
        body: Option<String>,
        content_type: &str,
    ) -> Result<Response, Report<PlatformError>> {
        let url = format!("{}{}", self.base_url, path);

        let mut request = match method {
            "GET" => Request::get(&url),
            "POST" => Request::post(&url),
            "PUT" => Request::put(&url),
            "DELETE" => Request::delete(&url),
            _ => {
                return Err(Report::new(PlatformError::ConfigStore)
                    .attach(format!("unsupported HTTP method: {}", method)))
            }
        };

        request = request
            .with_header("Fastly-Key", &self.api_key)
            .with_header("Accept", "application/json");

        if let Some(body_content) = body {
            request = request
                .with_header("Content-Type", content_type)
                .with_body(body_content);
        }

        request.send(&self.backend_name).map_err(|e| {
            Report::new(PlatformError::ConfigStore)
                .attach(format!("management API request failed: {}", e))
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
        let path = format!("/resources/stores/config/{}/item/{}", store_id, key);
        let payload = build_config_item_payload(value);

        let mut response = self.make_request(
            "PUT",
            &path,
            Some(payload),
            "application/x-www-form-urlencoded",
        )?;

        let mut buf = String::new();
        response
            .get_body_mut()
            .read_to_string(&mut buf)
            .change_context(PlatformError::ConfigStore)?;

        if response.get_status().is_success() {
            log::debug!(
                "FastlyManagementApiClient: updated config key '{}' in store '{}'",
                key,
                store_id
            );
            Ok(())
        } else {
            Err(Report::new(PlatformError::ConfigStore).attach(format!(
                "config item update failed with HTTP {} - {} for key '{}' in store '{}'",
                response.get_status(),
                buf.trim(),
                key,
                store_id
            )))
        }
    }

    /// Delete a config store item.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or returns an unexpected status.
    pub(crate) fn delete_config_item(
        &self,
        store_id: &str,
        key: &str,
    ) -> Result<(), Report<PlatformError>> {
        let path = format!("/resources/stores/config/{}/item/{}", store_id, key);

        let mut response = self.make_request("DELETE", &path, None, "application/json")?;

        let mut buf = String::new();
        response
            .get_body_mut()
            .read_to_string(&mut buf)
            .change_context(PlatformError::ConfigStore)?;

        if response.get_status().is_success() {
            log::debug!(
                "FastlyManagementApiClient: deleted config key '{}' from store '{}'",
                key,
                store_id
            );
            Ok(())
        } else {
            Err(Report::new(PlatformError::ConfigStore).attach(format!(
                "config item delete failed with HTTP {} - {} for key '{}' in store '{}'",
                response.get_status(),
                buf.trim(),
                key,
                store_id
            )))
        }
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
        let path = format!("/resources/stores/secret/{}/secrets", store_id);
        let payload = serde_json::json!({
            "name": secret_name,
            "secret": secret_value
        });

        let mut response =
            self.make_request("POST", &path, Some(payload.to_string()), "application/json")?;

        let mut buf = String::new();
        response
            .get_body_mut()
            .read_to_string(&mut buf)
            .change_context(PlatformError::SecretStore)?;

        if response.get_status().is_success() {
            log::debug!(
                "FastlyManagementApiClient: created secret '{}' in store '{}'",
                secret_name,
                store_id
            );
            Ok(())
        } else {
            Err(Report::new(PlatformError::SecretStore).attach(format!(
                "secret create failed with HTTP {} - {} for name '{}' in store '{}'",
                response.get_status(),
                buf.trim(),
                secret_name,
                store_id
            )))
        }
    }

    /// Delete a secret store entry.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or returns an unexpected status.
    pub(crate) fn delete_secret(
        &self,
        store_id: &str,
        secret_name: &str,
    ) -> Result<(), Report<PlatformError>> {
        let path = format!(
            "/resources/stores/secret/{}/secrets/{}",
            store_id, secret_name
        );

        let mut response = self.make_request("DELETE", &path, None, "application/json")?;

        let mut buf = String::new();
        response
            .get_body_mut()
            .read_to_string(&mut buf)
            .change_context(PlatformError::SecretStore)?;

        if response.get_status().is_success() {
            log::debug!(
                "FastlyManagementApiClient: deleted secret '{}' from store '{}'",
                secret_name,
                store_id
            );
            Ok(())
        } else {
            Err(Report::new(PlatformError::SecretStore).attach(format!(
                "secret delete failed with HTTP {} - {} for name '{}' in store '{}'",
                response.get_status(),
                buf.trim(),
                secret_name,
                store_id
            )))
        }
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
}
