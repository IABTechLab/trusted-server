//! Platform-specific API clients for config store operations.
//!
//! Currently only Fastly is supported. Cloudflare and Akamai support
//! is planned for future releases.

use crate::error::CliError;
use crate::Platform;

/// Platform client for interacting with edge platform APIs.
pub trait PlatformClient {
    /// Push a key-value pair to the config store.
    fn put(&self, key: &str, value: &str) -> Result<(), CliError>;

    /// Get a value from the config store.
    fn get(&self, key: &str) -> Result<Option<String>, CliError>;
}

/// Fastly Config Store client.
#[derive(Debug)]
pub struct FastlyClient {
    api_token: String,
    store_id: String,
}

impl FastlyClient {
    pub fn new(api_token: String, store_id: String) -> Self {
        Self {
            api_token,
            store_id,
        }
    }

    pub fn from_env(store_id: String) -> Result<Self, CliError> {
        let api_token = std::env::var("FASTLY_API_TOKEN").map_err(|_| {
            CliError::Config("FASTLY_API_TOKEN environment variable not set".into())
        })?;
        Ok(Self::new(api_token, store_id))
    }
}

impl PlatformClient for FastlyClient {
    fn put(&self, key: &str, value: &str) -> Result<(), CliError> {
        let url = format!(
            "https://api.fastly.com/resources/stores/config/{}/item/{}",
            self.store_id, key
        );
        let payload = format!("item_value={}", urlencoding::encode(value));

        let response = ureq::put(&url)
            .header("Fastly-Key", &self.api_token)
            .header("Accept", "application/json")
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(payload.as_bytes())
            .map_err(|e| CliError::Http(format!("Failed to send request: {}", e)))?;

        if response.status().is_success() {
            Ok(())
        } else {
            let status = response.status();
            let body = response.into_body().read_to_string().unwrap_or_default();
            Err(CliError::Platform(format!(
                "Failed to update config item: HTTP {} - {}",
                status, body
            )))
        }
    }

    fn get(&self, key: &str) -> Result<Option<String>, CliError> {
        let url = format!(
            "https://api.fastly.com/resources/stores/config/{}/item/{}",
            self.store_id, key
        );

        let response = match ureq::get(&url)
            .header("Fastly-Key", &self.api_token)
            .header("Accept", "application/json")
            .call()
        {
            Ok(resp) => resp,
            Err(ureq::Error::StatusCode(404)) => return Ok(None),
            Err(ureq::Error::StatusCode(code)) => {
                return Err(CliError::Platform(format!(
                    "Fastly returned HTTP {} for config store item",
                    code
                )));
            }
            Err(e) => {
                return Err(CliError::Http(format!("Failed to send request: {}", e)));
            }
        };

        let body = response
            .into_body()
            .read_to_string()
            .map_err(|e| CliError::Http(format!("Failed to read response: {}", e)))?;
        let json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| CliError::Platform(format!("Failed to parse response: {}", e)))?;
        Ok(json.get("value").and_then(|v| v.as_str()).map(String::from))
    }
}

/// Create a platform client based on the platform type.
pub fn create_client(
    platform: &Platform,
    store_id: String,
) -> Result<Box<dyn PlatformClient>, CliError> {
    match platform {
        Platform::Fastly => Ok(Box::new(FastlyClient::from_env(store_id)?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fastly_client_new() {
        let client = FastlyClient::new("test-token".to_string(), "test-store".to_string());
        assert_eq!(client.api_token, "test-token");
        assert_eq!(client.store_id, "test-store");
    }

    #[test]
    fn test_fastly_client_from_env_missing_token() {
        // Ensure env var is not set
        std::env::remove_var("FASTLY_API_TOKEN");

        let result = FastlyClient::from_env("test-store".to_string());
        assert!(result.is_err());
        match result.unwrap_err() {
            CliError::Config(msg) => {
                assert!(msg.contains("FASTLY_API_TOKEN"));
            }
            _ => panic!("Expected Config error"),
        }
    }

    #[test]
    fn test_create_client_fastly_missing_token() {
        std::env::remove_var("FASTLY_API_TOKEN");

        let result = create_client(&Platform::Fastly, "test-store".to_string());
        assert!(result.is_err());
    }
}
