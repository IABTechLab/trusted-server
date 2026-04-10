//! Admin endpoints for partner management.
//!
//! Provides `POST /_ts/admin/v1/partners/register` for registering and updating
//! partner configurations. Authentication is handled by the `[[handlers]]`
//! basic-auth layer before this code runs.

use error_stack::{Report, ResultExt};
use fastly::{Request, Response};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use url::Host;

use crate::error::TrustedServerError;

use super::partner::{
    hash_api_key, validate_fp_signal_config, validate_partner_id, validate_pull_sync_config,
    PartnerRecord, PartnerStore,
};

/// Request body for `POST /_ts/admin/v1/partners/register`.
///
/// Accepts `api_key` as plaintext — it is hashed before storage and
/// never persisted in cleartext.
#[derive(Deserialize)]
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
    #[serde(default)]
    pub fp_signal_cookie_names: Vec<String>,
    #[serde(default)]
    pub fp_signal_json_path: Option<String>,
    #[serde(default = "default_fp_signal_ttl_sec")]
    pub fp_signal_ttl_sec: u64,
}

impl std::fmt::Debug for RegisterPartnerRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisterPartnerRequest")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("api_key", &"[REDACTED]")
            .field("bidstream_enabled", &self.bidstream_enabled)
            .field("source_domain", &self.source_domain)
            .field("pull_sync_enabled", &self.pull_sync_enabled)
            .field(
                "ts_pull_token",
                &self.ts_pull_token.as_ref().map(|_| "[REDACTED]"),
            )
            .finish_non_exhaustive()
    }
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
fn default_fp_signal_ttl_sec() -> u64 {
    86400
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

/// Response body for `POST /_ts/admin/v1/partners/register`.
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

/// Handles `POST /_ts/admin/v1/partners/register`.
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
    // Parse request body with size limit to prevent memory abuse.
    const MAX_BODY_SIZE: usize = 64 * 1024; // 64 KiB
    let body_bytes = req.take_body_bytes();
    if body_bytes.len() > MAX_BODY_SIZE {
        return Err(Report::new(TrustedServerError::BadRequest {
            message: format!(
                "Request body too large ({} bytes, max {MAX_BODY_SIZE})",
                body_bytes.len()
            ),
        }));
    }
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
        fp_signal_cookie_names,
        fp_signal_json_path,
        fp_signal_ttl_sec,
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
        fp_signal_cookie_names,
        fp_signal_json_path,
        fp_signal_ttl_sec,
    };

    // Validate pull sync configuration.
    validate_pull_sync_config(&record).map_err(bad_request)?;

    // Validate FP signal configuration.
    validate_fp_signal_config(&record).map_err(bad_request)?;

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

    let body =
        serde_json::to_string(&response_body).change_context(TrustedServerError::EdgeCookie {
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
        assert!(
            req.fp_signal_cookie_names.is_empty(),
            "should default to empty"
        );
        assert!(req.fp_signal_json_path.is_none(), "should default to None");
        assert_eq!(req.fp_signal_ttl_sec, 86400, "should default to 86400");
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
            "ts_pull_token": "bearer-token-123",
            "fp_signal_cookie_names": ["uid2_token", "__euid_uid2"],
            "fp_signal_json_path": "advertising_token",
            "fp_signal_ttl_sec": 43200
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
        assert_eq!(
            req.fp_signal_cookie_names,
            vec!["uid2_token".to_owned(), "__euid_uid2".to_owned()]
        );
        assert_eq!(
            req.fp_signal_json_path.as_deref(),
            Some("advertising_token")
        );
        assert_eq!(req.fp_signal_ttl_sec, 43200);
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
