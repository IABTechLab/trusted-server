//! Discovery endpoint for trusted-server.
//!
//! This module provides a standardized discovery mechanism similar to the IAB's
//! Data Subject Rights framework. The `.well-known/trusted-server.json` endpoint
//! allows partners to discover JWKS keys for signature verification.

use serde::Serialize;

/// Main discovery document returned by `.well-known/trusted-server.json`
#[derive(Debug, Serialize)]
pub struct TrustedServerDiscovery {
    /// Version of the discovery document format
    pub version: String,

    /// JSON Web Key Set containing public keys for signature verification
    pub jwks: serde_json::Value,
}

impl TrustedServerDiscovery {
    /// Creates a new discovery document with the given JWKS
    #[must_use]
    pub fn new(jwks_value: serde_json::Value) -> Self {
        Self {
            version: "1.0".to_string(),
            jwks: jwks_value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_discovery_document_structure() {
        let jwks = json!({
            "keys": [
                {
                    "kty": "OKP",
                    "crv": "Ed25519",
                    "x": "test_key",
                    "kid": "test-kid"
                }
            ]
        });

        let discovery = TrustedServerDiscovery::new(jwks);

        assert_eq!(discovery.version, "1.0");
        assert!(discovery.jwks.is_object());
    }

    #[test]
    fn test_discovery_document_serialization() {
        let jwks = json!({
            "keys": []
        });

        let discovery = TrustedServerDiscovery::new(jwks);
        let serialized =
            serde_json::to_string(&discovery).expect("should serialize discovery document");

        // Verify it's valid JSON
        let parsed: serde_json::Value =
            serde_json::from_str(&serialized).expect("should parse serialized JSON");

        assert_eq!(parsed["version"], "1.0");
        assert!(parsed.get("jwks").is_some());
        assert!(parsed.get("endpoints").is_none());
    }

    #[test]
    fn test_discovery_includes_jwks() {
        let jwks = json!({
            "keys": [
                {
                    "kty": "OKP",
                    "kid": "test-key"
                }
            ]
        });

        let discovery = TrustedServerDiscovery::new(jwks);
        let serialized =
            serde_json::to_string(&discovery).expect("should serialize discovery document");
        let parsed: serde_json::Value =
            serde_json::from_str(&serialized).expect("should parse serialized JSON");

        assert!(parsed["jwks"]["keys"].is_array());
        assert_eq!(parsed["jwks"]["keys"][0]["kid"], "test-key");
    }
}
