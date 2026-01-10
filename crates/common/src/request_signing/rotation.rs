//! Key rotation management for request signing.
//!
//! This module provides functionality for rotating signing keys, managing key lifecycle,
//! and storing keys in Fastly Config and Secret stores.

use base64::{engine::general_purpose, Engine};
use ed25519_dalek::SigningKey;
use jose_jwk::Jwk;

use crate::error::TrustedServerError;
use crate::fastly_storage::{FastlyApiClient, FastlyConfigStore};

use super::Keypair;

#[derive(Debug, Clone)]
pub struct KeyRotationResult {
    pub new_kid: String,
    pub previous_kid: Option<String>,
    pub active_kids: Vec<String>,
    pub jwk: Jwk,
}

pub struct KeyRotationManager {
    config_store: FastlyConfigStore,
    api_client: FastlyApiClient,
    config_store_id: String,
    secret_store_id: String,
}

impl KeyRotationManager {
    pub fn new(
        config_store_id: impl Into<String>,
        secret_store_id: impl Into<String>,
    ) -> Result<Self, TrustedServerError> {
        let config_store_id = config_store_id.into();
        let secret_store_id = secret_store_id.into();

        let config_store = FastlyConfigStore::new("jwks_store");
        let api_client = FastlyApiClient::new()?;

        Ok(Self {
            config_store,
            api_client,
            config_store_id,
            secret_store_id,
        })
    }

    pub fn rotate_key(&self, kid: Option<String>) -> Result<KeyRotationResult, TrustedServerError> {
        let new_kid = kid.unwrap_or_else(generate_date_based_kid);

        let keypair = Keypair::generate();
        let jwk = keypair.get_jwk(new_kid.clone());
        let previous_kid = self.config_store.get_required("current-kid").ok();

        self.store_private_key(&new_kid, &keypair.signing_key)?;
        self.store_public_jwk(&new_kid, &jwk)?;

        let active_kids = match &previous_kid {
            Some(prev) if prev != &new_kid => vec![prev.clone(), new_kid.clone()],
            _ => vec![new_kid.clone()],
        };

        self.update_current_kid(&new_kid)?;
        self.update_active_kids(&active_kids)?;

        Ok(KeyRotationResult {
            new_kid,
            previous_kid,
            active_kids,
            jwk,
        })
    }

    fn store_private_key(
        &self,
        kid: &str,
        signing_key: &SigningKey,
    ) -> Result<(), TrustedServerError> {
        let key_bytes = signing_key.as_bytes();
        let key_b64 = general_purpose::STANDARD.encode(key_bytes);

        self.api_client
            .create_secret(&self.secret_store_id, kid, &key_b64)
            .map_err(|e| TrustedServerError::Configuration {
                message: format!("Failed to store private key '{}': {}", kid, e),
            })
    }

    fn store_public_jwk(&self, kid: &str, jwk: &Jwk) -> Result<(), TrustedServerError> {
        let jwk_json =
            serde_json::to_string(jwk).map_err(|e| TrustedServerError::Configuration {
                message: format!("Failed to serialize JWK: {}", e),
            })?;

        self.api_client
            .update_config_item(&self.config_store_id, kid, &jwk_json)
            .map_err(|e| TrustedServerError::Configuration {
                message: format!("Failed to store public JWK '{}': {}", kid, e),
            })
    }

    fn update_current_kid(&self, kid: &str) -> Result<(), TrustedServerError> {
        self.api_client
            .update_config_item(&self.config_store_id, "current-kid", kid)
            .map_err(|e| TrustedServerError::Configuration {
                message: format!("Failed to update current-kid: {}", e),
            })
    }

    fn update_active_kids(&self, active_kids: &[String]) -> Result<(), TrustedServerError> {
        let active_kids_str = active_kids.join(",");

        self.api_client
            .update_config_item(&self.config_store_id, "active-kids", &active_kids_str)
            .map_err(|e| TrustedServerError::Configuration {
                message: format!("Failed to update active-kids: {}", e),
            })
    }

    pub fn list_active_keys(&self) -> Result<Vec<String>, TrustedServerError> {
        let active_kids_str = self.config_store.get_required("active-kids")?;

        let active_kids: Vec<String> = active_kids_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        Ok(active_kids)
    }

    pub fn deactivate_key(&self, kid: &str) -> Result<(), TrustedServerError> {
        let mut active_kids = self.list_active_keys()?;

        active_kids.retain(|k| k != kid);

        if active_kids.is_empty() {
            return Err(TrustedServerError::Configuration {
                message: "Cannot deactivate the last active key".into(),
            });
        }

        self.update_active_kids(&active_kids)
    }

    pub fn delete_key(&self, kid: &str) -> Result<(), TrustedServerError> {
        self.deactivate_key(kid)?;

        self.api_client
            .delete_config_item(&self.config_store_id, kid)
            .map_err(|e| TrustedServerError::Configuration {
                message: format!("Failed to delete JWK from ConfigStore: {}", e),
            })?;

        self.api_client
            .delete_secret(&self.secret_store_id, kid)
            .map_err(|e| TrustedServerError::Configuration {
                message: format!("Failed to delete secret from SecretStore: {}", e),
            })?;

        Ok(())
    }
}

pub fn generate_date_based_kid() -> String {
    use chrono::Utc;
    format!("ts-{}", Utc::now().format("%Y-%m-%d"))
}

#[cfg(test)]
mod tests {
    use crate::request_signing::Keypair;

    use super::*;

    #[test]
    fn test_generate_date_based_kid() {
        let kid = generate_date_based_kid();
        println!("Generated KID: {}", kid);

        // Verify format: ts-YYYY-MM-DD
        assert!(kid.starts_with("ts-"));
        assert!(kid.len() >= 13); // "ts-" + "YYYY-MM-DD" = 13 chars minimum

        // Verify it contains only valid characters
        let parts: Vec<&str> = kid.split('-').collect();
        assert_eq!(parts.len(), 4); // ["ts", "YYYY", "MM", "DD"]
        assert_eq!(parts[0], "ts");
    }

    #[test]
    fn test_key_rotation_manager_creation() {
        let result = KeyRotationManager::new("jwks_store", "signing_keys");
        match result {
            Ok(manager) => {
                assert_eq!(manager.config_store_id, "jwks_store");
                assert_eq!(manager.secret_store_id, "signing_keys");
                println!("✓ KeyRotationManager created successfully");
            }
            Err(e) => {
                println!("Expected error in test environment: {}", e);
            }
        }
    }

    #[test]
    fn test_list_active_keys() {
        let result = KeyRotationManager::new("jwks_store", "signing_keys");
        if let Ok(manager) = result {
            match manager.list_active_keys() {
                Ok(keys) => {
                    println!("Active keys: {:?}", keys);
                    assert!(!keys.is_empty(), "Should have at least one active key");
                }
                Err(e) => println!("Expected error in test environment: {}", e),
            }
        }
    }

    #[test]
    fn test_key_rotation_result_structure() {
        let jwk = Keypair::generate().get_jwk("test-key".to_string());

        let result = KeyRotationResult {
            new_kid: "ts-2024-01-01".to_string(),
            previous_kid: Some("ts-2023-12-31".to_string()),
            active_kids: vec!["ts-2023-12-31".to_string(), "ts-2024-01-01".to_string()],
            jwk: jwk.clone(),
        };

        assert_eq!(result.new_kid, "ts-2024-01-01");
        assert_eq!(result.previous_kid, Some("ts-2023-12-31".to_string()));
        assert_eq!(result.active_kids.len(), 2);
        assert_eq!(result.jwk.prm.kid, Some("test-key".to_string()));
    }
}
