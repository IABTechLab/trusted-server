//! Fastly management API client (legacy).
//!
//! This module holds [`FastlyApiClient`], which wraps the Fastly management
//! REST API for write operations on config and secret stores.
//! New code should use [`crate::platform::PlatformConfigStore`] and
//! [`crate::platform::PlatformSecretStore`] write methods instead.
//! This type will be removed once all call sites have migrated.

use std::io::Read;

use error_stack::{Report, ResultExt};
use fastly::{Request, Response};
use http::StatusCode;

use crate::backend::BackendConfig;
use crate::error::TrustedServerError;
use crate::storage::secret_store::FastlySecretStore;

const FASTLY_API_HOST: &str = "https://api.fastly.com";

fn build_config_item_payload(value: &str) -> String {
    format!("item_value={}", urlencoding::encode(value))
}

/// HTTP client for the Fastly management API.
///
/// Used to perform write operations on config and secret stores via the
/// Fastly REST API. Reads are performed directly through the edge-side SDK.
///
/// # Migration note
///
/// This type predates the `platform` abstraction. New code should use
/// [`crate::platform::PlatformConfigStore`] and
/// [`crate::platform::PlatformSecretStore`] write methods instead.
pub struct FastlyApiClient {
    api_key: Vec<u8>,
    base_url: &'static str,
    backend_name: String,
}

impl FastlyApiClient {
    /// Creates a new Fastly API client using the default secret store.
    ///
    /// # Errors
    ///
    /// Returns an error if the secret store cannot be opened or the API key
    /// cannot be retrieved.
    pub fn new() -> Result<Self, Report<TrustedServerError>> {
        Self::from_secret_store("api-keys", "api_key")
    }

    /// Creates a new Fastly API client reading credentials from a specified
    /// secret store entry.
    ///
    /// # Errors
    ///
    /// Returns an error if the API backend cannot be ensured or the API key
    /// cannot be retrieved.
    pub fn from_secret_store(
        store_name: &str,
        key_name: &str,
    ) -> Result<Self, Report<TrustedServerError>> {
        let backend_name = BackendConfig::from_url("https://api.fastly.com", true)?;
        let api_key = FastlySecretStore::new(store_name).get(key_name)?;

        log::debug!("FastlyApiClient initialized with backend: {}", backend_name);

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
    ) -> Result<Response, Report<TrustedServerError>> {
        let url = format!("{}{}", self.base_url, path);
        let api_key_str = String::from_utf8_lossy(&self.api_key).to_string();

        let mut request = match method {
            "GET" => Request::get(&url),
            "POST" => Request::post(&url),
            "PUT" => Request::put(&url),
            "DELETE" => Request::delete(&url),
            _ => {
                return Err(Report::new(TrustedServerError::Configuration {
                    message: format!("unsupported HTTP method: {}", method),
                }))
            }
        };

        request = request
            .with_header("Fastly-Key", api_key_str)
            .with_header("Accept", "application/json");

        if let Some(body_content) = body {
            request = request
                .with_header("Content-Type", content_type)
                .with_body(body_content);
        }

        request.send(&self.backend_name).map_err(|e| {
            Report::new(TrustedServerError::Configuration {
                message: format!("failed to send API request: {}", e),
            })
        })
    }

    /// Updates a configuration item in a Fastly config store.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or returns a non-OK status.
    pub fn update_config_item(
        &self,
        store_id: &str,
        key: &str,
        value: &str,
    ) -> Result<(), Report<TrustedServerError>> {
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
            .change_context(TrustedServerError::Configuration {
                message: "failed to read config store API response".into(),
            })?;

        if response.get_status() == StatusCode::OK {
            Ok(())
        } else {
            Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "failed to update config item: HTTP {} - {}",
                    response.get_status(),
                    buf
                ),
            }))
        }
    }

    /// Creates a secret in a Fastly secret store.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or returns a non-OK status.
    pub fn create_secret(
        &self,
        store_id: &str,
        secret_name: &str,
        secret_value: &str,
    ) -> Result<(), Report<TrustedServerError>> {
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
            .change_context(TrustedServerError::Configuration {
                message: "failed to read secret store API response".into(),
            })?;

        if response.get_status() == StatusCode::OK {
            Ok(())
        } else {
            Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "failed to create secret: HTTP {} - {}",
                    response.get_status(),
                    buf
                ),
            }))
        }
    }

    /// Deletes a configuration item from a Fastly config store.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or returns a non-OK or
    /// non-NO_CONTENT status.
    pub fn delete_config_item(
        &self,
        store_id: &str,
        key: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        let path = format!("/resources/stores/config/{}/item/{}", store_id, key);

        let mut response = self.make_request("DELETE", &path, None, "application/json")?;

        let mut buf = String::new();
        response
            .get_body_mut()
            .read_to_string(&mut buf)
            .change_context(TrustedServerError::Configuration {
                message: "failed to read config store delete API response".into(),
            })?;

        if response.get_status() == StatusCode::OK
            || response.get_status() == StatusCode::NO_CONTENT
        {
            Ok(())
        } else {
            Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "failed to delete config item: HTTP {} - {}",
                    response.get_status(),
                    buf
                ),
            }))
        }
    }

    /// Deletes a secret from a Fastly secret store.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or returns a non-OK or
    /// non-NO_CONTENT status.
    pub fn delete_secret(
        &self,
        store_id: &str,
        secret_name: &str,
    ) -> Result<(), Report<TrustedServerError>> {
        let path = format!(
            "/resources/stores/secret/{}/secrets/{}",
            store_id, secret_name
        );

        let mut response = self.make_request("DELETE", &path, None, "application/json")?;

        let mut buf = String::new();
        response
            .get_body_mut()
            .read_to_string(&mut buf)
            .change_context(TrustedServerError::Configuration {
                message: "failed to read secret store delete API response".into(),
            })?;

        if response.get_status() == StatusCode::OK
            || response.get_status() == StatusCode::NO_CONTENT
        {
            Ok(())
        } else {
            Err(Report::new(TrustedServerError::Configuration {
                message: format!(
                    "failed to delete secret: HTTP {} - {}",
                    response.get_status(),
                    buf
                ),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_item_payload_url_encodes_reserved_characters() {
        let payload = build_config_item_payload(r#"value with spaces + symbols &= {"kid":"a+b"}"#);

        assert_eq!(
            payload,
            "item_value=value%20with%20spaces%20%2B%20symbols%20%26%3D%20%7B%22kid%22%3A%22a%2Bb%22%7D",
            "should URL-encode config item values in form payloads"
        );
    }
}
