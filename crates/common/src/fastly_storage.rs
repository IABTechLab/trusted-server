use std::io::Read;

use fastly::{ConfigStore, Request, Response, SecretStore};
use http::StatusCode;

use crate::backend::ensure_backend_from_url;
use crate::error::TrustedServerError;

const FASTLY_API_HOST: &str = "https://api.fastly.com";

pub struct FastlyConfigStore {
    store_name: String,
}

impl FastlyConfigStore {
    pub fn new(store_name: impl Into<String>) -> Self {
        Self {
            store_name: store_name.into(),
        }
    }

    /// Retrieves a configuration value from the store.
    ///
    /// # Errors
    ///
    /// Returns an error if the key is not found in the config store.
    pub fn get(&self, key: &str) -> Result<String, TrustedServerError> {
        // TODO use try_open and return the error
        let store = ConfigStore::open(&self.store_name);
        store
            .get(key)
            .ok_or_else(|| TrustedServerError::Configuration {
                message: format!(
                    "Key '{}' not found in config store '{}'",
                    key, self.store_name
                ),
            })
    }
}

pub struct FastlySecretStore {
    store_name: String,
}

impl FastlySecretStore {
    pub fn new(store_name: impl Into<String>) -> Self {
        Self {
            store_name: store_name.into(),
        }
    }

    /// Retrieves a secret value from the store.
    ///
    /// # Errors
    ///
    /// Returns an error if the secret store cannot be opened, the key is not found,
    /// or the secret plaintext cannot be retrieved.
    pub fn get(&self, key: &str) -> Result<Vec<u8>, TrustedServerError> {
        let store =
            SecretStore::open(&self.store_name).map_err(|_| TrustedServerError::Configuration {
                message: format!("Failed to open SecretStore '{}'", self.store_name),
            })?;

        let secret = store
            .get(key)
            .ok_or_else(|| TrustedServerError::Configuration {
                message: format!(
                    "Secret '{}' not found in secret store '{}'",
                    key, self.store_name
                ),
            })?;

        secret
            .try_plaintext()
            .map_err(|_| TrustedServerError::Configuration {
                message: "Failed to get secret plaintext".into(),
            })
            .map(|bytes| bytes.into_iter().collect())
    }

    /// Retrieves a secret value from the store and decodes it as a UTF-8 string.
    ///
    /// # Errors
    ///
    /// Returns an error if the secret cannot be retrieved or is not valid UTF-8.
    pub fn get_string(&self, key: &str) -> Result<String, TrustedServerError> {
        let bytes = self.get(key)?;
        String::from_utf8(bytes).map_err(|e| TrustedServerError::Configuration {
            message: format!("Failed to decode secret as UTF-8: {}", e),
        })
    }
}

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
    /// Returns an error if the secret store cannot be opened or the API key cannot be retrieved.
    pub fn new() -> Result<Self, TrustedServerError> {
        Self::from_secret_store("api-keys", "api_key")
    }

    /// Creates a new Fastly API client from a specified secret store.
    ///
    /// # Errors
    ///
    /// Returns an error if the API backend cannot be ensured or the API key cannot be retrieved.
    pub fn from_secret_store(store_name: &str, key_name: &str) -> Result<Self, TrustedServerError> {
        let backend_name = ensure_backend_from_url(FASTLY_API_HOST).map_err(|e| {
            TrustedServerError::Configuration {
                message: format!("Failed to ensure API backend: {}", e),
            }
        })?;

        let secret_store = FastlySecretStore::new(store_name);
        let api_key = secret_store.get(key_name)?;

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
    ) -> Result<Response, TrustedServerError> {
        let url = format!("{}{}", self.base_url, path);

        let api_key_str = String::from_utf8_lossy(&self.api_key).to_string();

        let mut request = match method {
            "GET" => Request::get(&url),
            "POST" => Request::post(&url),
            "PUT" => Request::put(&url),
            "DELETE" => Request::delete(&url),
            _ => {
                return Err(TrustedServerError::Configuration {
                    message: format!("Unsupported HTTP method: {}", method),
                })
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

        request
            .send(&self.backend_name)
            .map_err(|e| TrustedServerError::Configuration {
                message: format!("Failed to send API request: {}", e),
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
    ) -> Result<(), TrustedServerError> {
        let path = format!("/resources/stores/config/{}/item/{}", store_id, key);
        let payload = format!("item_value={}", value);

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
            .map_err(|e| TrustedServerError::Configuration {
                message: format!("Failed to read API response: {}", e),
            })?;

        if response.get_status() == StatusCode::OK {
            Ok(())
        } else {
            Err(TrustedServerError::Configuration {
                message: format!(
                    "Failed to update config item: HTTP {} - {}",
                    response.get_status(),
                    buf
                ),
            })
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
    ) -> Result<(), TrustedServerError> {
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
            .map_err(|e| TrustedServerError::Configuration {
                message: format!("Failed to read API response: {}", e),
            })?;

        if response.get_status() == StatusCode::OK {
            Ok(())
        } else {
            Err(TrustedServerError::Configuration {
                message: format!(
                    "Failed to create secret: HTTP {} - {}",
                    response.get_status(),
                    buf
                ),
            })
        }
    }

    /// Deletes a configuration item from a Fastly config store.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or returns a non-OK/NO_CONTENT status.
    pub fn delete_config_item(&self, store_id: &str, key: &str) -> Result<(), TrustedServerError> {
        let path = format!("/resources/stores/config/{}/item/{}", store_id, key);

        let mut response = self.make_request("DELETE", &path, None, "application/json")?;

        let mut buf = String::new();
        response
            .get_body_mut()
            .read_to_string(&mut buf)
            .map_err(|e| TrustedServerError::Configuration {
                message: format!("Failed to read API response: {}", e),
            })?;

        if response.get_status() == StatusCode::OK
            || response.get_status() == StatusCode::NO_CONTENT
        {
            Ok(())
        } else {
            Err(TrustedServerError::Configuration {
                message: format!(
                    "Failed to delete config item: HTTP {} - {}",
                    response.get_status(),
                    buf
                ),
            })
        }
    }

    /// Deletes a secret from a Fastly secret store.
    ///
    /// # Errors
    ///
    /// Returns an error if the API request fails or returns a non-OK/NO_CONTENT status.
    pub fn delete_secret(
        &self,
        store_id: &str,
        secret_name: &str,
    ) -> Result<(), TrustedServerError> {
        let path = format!(
            "/resources/stores/secret/{}/secrets/{}",
            store_id, secret_name
        );

        let mut response = self.make_request("DELETE", &path, None, "application/json")?;

        let mut buf = String::new();
        response
            .get_body_mut()
            .read_to_string(&mut buf)
            .map_err(|e| TrustedServerError::Configuration {
                message: format!("Failed to read API response: {}", e),
            })?;

        if response.get_status() == StatusCode::OK
            || response.get_status() == StatusCode::NO_CONTENT
        {
            Ok(())
        } else {
            Err(TrustedServerError::Configuration {
                message: format!(
                    "Failed to delete secret: HTTP {} - {}",
                    response.get_status(),
                    buf
                ),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_store_new() {
        let store = FastlyConfigStore::new("test_store");
        assert_eq!(store.store_name, "test_store");
    }

    #[test]
    fn test_secret_store_new() {
        let store = FastlySecretStore::new("test_secrets");
        assert_eq!(store.store_name, "test_secrets");
    }

    #[test]
    fn test_config_store_get() {
        let store = FastlyConfigStore::new("jwks_store");
        let result = store.get("current-kid");
        match result {
            Ok(kid) => println!("Current KID: {}", kid),
            Err(e) => println!("Expected error in test environment: {}", e),
        }
    }

    #[test]
    fn test_secret_store_get() {
        let store = FastlySecretStore::new("signing_keys");
        let config_store = FastlyConfigStore::new("jwks_store");

        match config_store.get("current-kid") {
            Ok(kid) => match store.get(&kid) {
                Ok(bytes) => {
                    println!("Successfully loaded secret, {} bytes", bytes.len());
                    assert!(!bytes.is_empty());
                }
                Err(e) => println!("Error loading secret: {}", e),
            },
            Err(e) => println!("Error getting current kid: {}", e),
        }
    }

    #[test]
    fn test_api_client_creation() {
        let result = FastlyApiClient::new();
        match result {
            Ok(_client) => println!("Successfully created API client"),
            Err(e) => println!("Expected error in test environment: {}", e),
        }
    }

    #[test]
    fn test_update_config_item() {
        let result = FastlyApiClient::new();
        if let Ok(client) = result {
            let result =
                client.update_config_item("5WNlRjznCUAGTU0QeYU8x2", "test-key", "test-value");
            match result {
                Ok(()) => println!("Successfully updated config item"),
                Err(e) => println!("Failed to update config item: {}", e),
            }
        }
    }

    #[test]
    fn test_create_secret() {
        let result = FastlyApiClient::new();
        if let Ok(client) = result {
            let result = client.create_secret(
                "Ltf3CkSGV0Yn2PIC2lDcZx",
                "test-secret-new",
                "SGVsbG8sIHdvcmxkIQ==",
            );
            match result {
                Ok(()) => println!("Successfully created secret"),
                Err(e) => println!("Failed to create secret: {}", e),
            }
        }
    }
}
