//! Admin endpoints for partner management.
//!
//! Provides `POST /admin/partners/register` for registering and updating
//! partner configurations. Authentication is handled by the `[[handlers]]`
//! basic-auth layer before this code runs.

use error_stack::{Report, ResultExt};
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use url::Host;

use crate::error::TrustedServerError;

use super::partner::{
    hash_api_key, validate_partner_id, validate_pull_sync_config, PartnerRecord, PartnerStore,
};

/// Request body for `POST /admin/partners/register`.
///
/// Accepts `api_key` as plaintext — it is hashed before storage and
/// never persisted in cleartext.
#[derive(Debug, Deserialize)]
pub struct RegisterPartnerRequest {
    pub id: String,
    pub name: String,
    pub allowed_return_domains: Vec<String>,
    /// Raw API key — will be SHA-256 hashed before storage.
    pub api_key: String,
    #[serde(default)]
    pub bidstream_enabled: bool,
    pub source_domain: String,
    #[serde(default = "default_openrtb_atype")]
    pub openrtb_atype: u8,
    #[serde(default = "default_sync_rate_limit")]
    pub sync_rate_limit: u32,
    #[serde(default = "default_batch_rate_limit")]
    pub batch_rate_limit: u32,
    #[serde(default)]
    pub pull_sync_enabled: bool,
    #[serde(default)]
    pub pull_sync_url: Option<String>,
    #[serde(default)]
    pub pull_sync_allowed_domains: Vec<String>,
    #[serde(default = "default_pull_sync_ttl_sec")]
    pub pull_sync_ttl_sec: u64,
    #[serde(default = "default_pull_sync_rate_limit")]
    pub pull_sync_rate_limit: u32,
    #[serde(default)]
    pub ts_pull_token: Option<String>,
}

fn default_openrtb_atype() -> u8 {
    3
}
fn default_sync_rate_limit() -> u32 {
    100
}
fn default_batch_rate_limit() -> u32 {
    60
}
fn default_pull_sync_ttl_sec() -> u64 {
    86400
}
fn default_pull_sync_rate_limit() -> u32 {
    10
}

fn bad_request(message: impl Into<String>) -> Report<TrustedServerError> {
    Report::new(TrustedServerError::BadRequest {
        message: message.into(),
    })
}

fn normalize_required_text(
    value: &str,
    field_name: &str,
) -> Result<String, Report<TrustedServerError>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(bad_request(format!("{field_name} is required")));
    }
    Ok(trimmed.to_owned())
}

fn normalize_hostname(value: &str, field_name: &str) -> Result<String, Report<TrustedServerError>> {
    let trimmed = value.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        return Err(bad_request(format!("{field_name} is required")));
    }

    let normalized = trimmed.to_ascii_lowercase();
    Host::parse(&normalized)
        .map_err(|_| bad_request(format!("{field_name} must be a valid hostname")))?;

    Ok(normalized)
}

fn normalize_hostname_list(
    values: Vec<String>,
    field_name: &str,
) -> Result<Vec<String>, Report<TrustedServerError>> {
    let mut normalized_values = Vec::with_capacity(values.len());
    let mut seen = HashSet::with_capacity(values.len());

    for value in values {
        let trimmed = value.trim().trim_end_matches('.');
        if trimmed.is_empty() {
            return Err(bad_request(format!(
                "{field_name} entries must not be empty"
            )));
        }

        let normalized = trimmed.to_ascii_lowercase();
        Host::parse(&normalized).map_err(|_| {
            bad_request(format!("{field_name} contains invalid hostname '{value}'"))
        })?;

        if seen.insert(normalized.clone()) {
            normalized_values.push(normalized);
        }
    }

    Ok(normalized_values)
}

/// Response body for `POST /admin/partners/register`.
///
/// Echoes key fields without exposing sensitive data (`api_key_hash`,
/// `ts_pull_token`).
#[derive(Debug, Serialize)]
pub struct RegisterPartnerResponse {
    pub id: String,
    pub name: String,
    pub pull_sync_enabled: bool,
    pub bidstream_enabled: bool,
    pub created: bool,
}

/// Handles `POST /admin/partners/register`.
///
/// Registers a new partner or updates an existing one. Authentication is
/// handled upstream by the `[[handlers]]` basic-auth layer.
///
/// # Errors
///
/// Returns `Report<TrustedServerError>` on validation failure (400),
/// KV store failure (503), or JSON parse failure (400).
pub fn handle_register_partner(
    partner_store: &PartnerStore,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    // Parse request body.
    let body_bytes = req.take_body_bytes();
    let request: RegisterPartnerRequest =
        serde_json::from_slice(&body_bytes).change_context(TrustedServerError::BadRequest {
            message: "Invalid JSON in request body".to_owned(),
        })?;

    let RegisterPartnerRequest {
        id,
        name,
        allowed_return_domains,
        api_key,
        bidstream_enabled,
        source_domain,
        openrtb_atype,
        sync_rate_limit,
        batch_rate_limit,
        pull_sync_enabled,
        pull_sync_url,
        pull_sync_allowed_domains,
        pull_sync_ttl_sec,
        pull_sync_rate_limit,
        ts_pull_token,
    } = request;

    // Validate partner ID.
    validate_partner_id(&id).map_err(bad_request)?;

    // Validate and normalize required fields.
    let name = normalize_required_text(&name, "name")?;
    if api_key.trim().is_empty() {
        return Err(bad_request("api_key is required"));
    }
    let source_domain = normalize_hostname(&source_domain, "source_domain")?;

    if allowed_return_domains.is_empty() {
        return Err(bad_request(
            "allowed_return_domains must have at least one entry",
        ));
    }
    let allowed_return_domains =
        normalize_hostname_list(allowed_return_domains, "allowed_return_domains")?;
    let pull_sync_allowed_domains =
        normalize_hostname_list(pull_sync_allowed_domains, "pull_sync_allowed_domains")?;

    // Build the PartnerRecord with hashed API key.
    let record = PartnerRecord {
        id,
        name,
        allowed_return_domains,
        api_key_hash: hash_api_key(&api_key),
        bidstream_enabled,
        source_domain,
        openrtb_atype,
        sync_rate_limit,
        batch_rate_limit,
        pull_sync_enabled,
        pull_sync_url,
        pull_sync_allowed_domains,
        pull_sync_ttl_sec,
        pull_sync_rate_limit,
        ts_pull_token,
    };

    // Validate pull sync configuration.
    validate_pull_sync_config(&record).map_err(bad_request)?;

    // Persist to KV store.
    let created = partner_store.upsert(&record)?;

    let status = if created {
        log::info!("Registered new partner '{}'", record.id);
        fastly::http::StatusCode::CREATED
    } else {
        log::info!("Updated existing partner '{}'", record.id);
        fastly::http::StatusCode::OK
    };

    let response_body = RegisterPartnerResponse {
        id: record.id,
        name: record.name,
        pull_sync_enabled: record.pull_sync_enabled,
        bidstream_enabled: record.bidstream_enabled,
        created,
    };

    let body = serde_json::to_string(&response_body).change_context(TrustedServerError::Ec {
        message: "Failed to serialize registration response".to_owned(),
    })?;

    Ok(Response::from_status(status)
        .with_content_type(fastly::mime::APPLICATION_JSON)
        .with_body(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_deserializes_with_defaults() {
        let json = r#"{
            "id": "ssp_x",
            "name": "SSP Example",
            "allowed_return_domains": ["sync.example-ssp.com"],
            "api_key": "raw-secret-key",
            "source_domain": "example-ssp.com"
        }"#;

        let req: RegisterPartnerRequest =
            serde_json::from_str(json).expect("should deserialize with defaults");

        assert_eq!(req.id, "ssp_x");
        assert_eq!(req.openrtb_atype, 3, "should default to 3");
        assert_eq!(req.sync_rate_limit, 100, "should default to 100");
        assert_eq!(req.batch_rate_limit, 60, "should default to 60");
        assert_eq!(req.pull_sync_ttl_sec, 86400, "should default to 86400");
        assert_eq!(req.pull_sync_rate_limit, 10, "should default to 10");
        assert!(!req.bidstream_enabled, "should default to false");
        assert!(!req.pull_sync_enabled, "should default to false");
        assert!(req.pull_sync_url.is_none());
        assert!(req.ts_pull_token.is_none());
    }

    #[test]
    fn response_does_not_contain_sensitive_fields() {
        let response = RegisterPartnerResponse {
            id: "ssp_x".to_owned(),
            name: "SSP Example".to_owned(),
            pull_sync_enabled: false,
            bidstream_enabled: true,
            created: true,
        };

        let json = serde_json::to_string(&response).expect("should serialize");
        assert!(!json.contains("api_key"), "should not contain api_key");
        assert!(
            !json.contains("api_key_hash"),
            "should not contain api_key_hash"
        );
        assert!(
            !json.contains("ts_pull_token"),
            "should not contain ts_pull_token"
        );
    }

    #[test]
    fn request_deserializes_full_payload() {
        let json = r#"{
            "id": "ssp_x",
            "name": "SSP Example",
            "allowed_return_domains": ["sync.example-ssp.com"],
            "api_key": "raw-secret-key",
            "bidstream_enabled": true,
            "source_domain": "example-ssp.com",
            "openrtb_atype": 3,
            "sync_rate_limit": 200,
            "batch_rate_limit": 120,
            "pull_sync_enabled": true,
            "pull_sync_url": "https://sync.example-ssp.com/pull",
            "pull_sync_allowed_domains": ["sync.example-ssp.com"],
            "pull_sync_ttl_sec": 43200,
            "pull_sync_rate_limit": 5,
            "ts_pull_token": "bearer-token-123"
        }"#;

        let req: RegisterPartnerRequest =
            serde_json::from_str(json).expect("should deserialize full payload");

        assert_eq!(req.sync_rate_limit, 200);
        assert_eq!(req.batch_rate_limit, 120);
        assert!(req.pull_sync_enabled);
        assert_eq!(
            req.pull_sync_url.as_deref(),
            Some("https://sync.example-ssp.com/pull")
        );
    }

    #[test]
    fn normalize_required_text_rejects_whitespace_only() {
        let err = normalize_required_text("   ", "name")
            .expect_err("should reject whitespace-only required field");
        assert!(
            err.to_string().contains("name is required"),
            "should mention required field"
        );
    }

    #[test]
    fn normalize_hostname_normalizes_case_and_trailing_dot() {
        let normalized = normalize_hostname("  Sync.Example.COM.  ", "source_domain")
            .expect("should parse host");
        assert_eq!(normalized, "sync.example.com");
    }

    #[test]
    fn normalize_hostname_list_rejects_empty_entry() {
        let err = normalize_hostname_list(
            vec!["sync.example.com".to_owned(), "   ".to_owned()],
            "allowed_return_domains",
        )
        .expect_err("should reject empty domain entries");
        assert!(
            err.to_string()
                .contains("allowed_return_domains entries must not be empty"),
            "should surface empty-entry error"
        );
    }

    #[test]
    fn normalize_hostname_list_deduplicates_normalized_values() {
        let normalized = normalize_hostname_list(
            vec![
                "Sync.Example.com".to_owned(),
                "sync.example.com.".to_owned(),
                "cdn.example.com".to_owned(),
            ],
            "allowed_return_domains",
        )
        .expect("should normalize hostnames");
        assert_eq!(
            normalized,
            vec!["sync.example.com".to_owned(), "cdn.example.com".to_owned()]
        );
    }
}
