//! Asynchronous prepared signing: content versions are signed by the
//! publisher's native preparation worker, outside reader delivery, and
//! readers only look up what the delivery instance verified and published.
//!
//! In `execution = "async_prepared"` a reader never signs, prepares,
//! finalizes or uploads. It computes each item's identity from the exact
//! projection or bytes it delivers, looks up a published result under that
//! immutable identity, and on a miss delivers the current version unsigned
//! and hands the worker a bounded hint. A prior version's result is never
//! applied to new bytes, and the mode never falls back to signing in
//! delivery.
//!
//! [`protocol`] holds the identities and messages both sides agree on,
//! [`items`] the one extraction of a document's items both sides run, and
//! [`pipeline`] the prepared-hash steps the worker runs and the delivery
//! instance re-verifies through the authenticated API before it publishes
//! anything.

pub mod items;
pub mod pipeline;
pub mod protocol;

pub(crate) mod edge;

use serde::{Deserialize, Serialize};
use validator::{Validate, ValidationError};

use trusted_server_core::redacted::Redacted;

pub use crate::api::{EmbeddingPlan, PlanOperation};
pub use protocol::{VariantParam, VariantSchema};

/// How the module signs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Execution {
    /// Each reader's delivery signs what it has no cached result for. The
    /// existing mode, kept as a deliberate operator choice.
    #[default]
    Synchronous,
    /// The publisher's preparation worker signs; delivery only looks up
    /// published results and never signs.
    AsyncPrepared,
}

/// `[provenance.encypher.preparation]`, the asynchronous mode's settings.
#[derive(Debug, Clone, Deserialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct PreparationConfig {
    /// The preparation worker's origin, `https`, or `http` on a loopback
    /// host for a local deployment. Reader hints and public source reads go
    /// here, never with a reader's credentials.
    #[validate(custom(function = validate_service_url))]
    pub worker_url: String,
    /// The origin the delivery instance reads its own snapshots from: its
    /// public `https` address, or `http` on a loopback host. While
    /// `[asset-cache]` selects a module, exactly `https://<public_host>`, with
    /// at most a trailing `/` or the default port `:443`, so a snapshot reads
    /// the copy the asset cache holds for readers.
    #[validate(custom(function = validate_service_url))]
    pub loopback_url: String,
    /// The deployment's identifier, which every message carries.
    #[validate(custom(function = validate_deployment_id))]
    pub deployment_id: String,
    /// The host readers are served on and every prepared address names:
    /// the claimed domain or its `www.` name. Readers on another host find
    /// no published result.
    pub public_host: String,
    /// The secret-store entry of the reader-signal scope, resolved to the
    /// secret as settings load. It authorizes hints and nothing else.
    pub signal_secret: Redacted<String>,
    /// The secret-store entry of the snapshot and publication scope,
    /// resolved as settings load. It authorizes snapshots, result and marker
    /// publication and the worker's public source reads.
    pub publication_secret: Redacted<String>,
    /// Whether readers hint the worker about unprepared items. Needs a
    /// platform with a real timer or an after-response lifetime; refused at
    /// registration on one with neither. Publication events, warming and
    /// reconciliation prepare content either way.
    #[serde(default)]
    pub reader_trigger: bool,
    /// The longest a reader's hint may take on a platform with a timer, in
    /// milliseconds.
    #[serde(default = "default_admission_ms")]
    #[validate(range(min = 10, max = 200))]
    pub admission_ms: u64,
    /// Path prefixes snapshots may read and hints may name. Nothing else is
    /// ever read for preparation.
    #[validate(length(min = 1, max = 64), custom(function = validate_routes))]
    pub routes: Vec<String>,
    /// The bounded variant schema: query parameters and the values that
    /// name a variant. A reader query naming anything else is not hinted.
    #[serde(default)]
    #[validate(custom(function = validate_variants))]
    pub variants: VariantSchema,
}

fn default_admission_ms() -> u64 {
    50
}

/// Whether `value` is an `https` origin, or `http` on a loopback host, with
/// no path, query, fragment or user information.
fn service_url_ok(value: &str) -> bool {
    let Ok(parsed) = url::Url::parse(value) else {
        return false;
    };
    let loopback = matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    let scheme_ok = parsed.scheme() == "https" || (parsed.scheme() == "http" && loopback);
    scheme_ok
        && parsed.host_str().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && matches!(parsed.path(), "" | "/")
}

fn validate_service_url(value: &str) -> Result<(), ValidationError> {
    if service_url_ok(value) {
        Ok(())
    } else {
        Err(ValidationError::new("preparation_url"))
    }
}

fn validate_deployment_id(value: &str) -> Result<(), ValidationError> {
    let ok = !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if ok {
        Ok(())
    } else {
        Err(ValidationError::new("deployment_id"))
    }
}

fn validate_routes(routes: &[String]) -> Result<(), ValidationError> {
    let ok = routes.iter().all(|route| {
        route.starts_with('/')
            && route.len() <= protocol::MAX_TARGET_PATH_BYTES
            && route
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'?' | b'#'))
    });
    if ok {
        Ok(())
    } else {
        Err(ValidationError::new("preparation_routes"))
    }
}

fn validate_variants(variants: &VariantSchema) -> Result<(), ValidationError> {
    if variants.is_bounded() {
        Ok(())
    } else {
        Err(ValidationError::new("preparation_variants"))
    }
}

impl PreparationConfig {
    /// Whether `path` lies under a configured route.
    #[must_use]
    pub fn admits_path(&self, path: &str) -> bool {
        self.routes
            .iter()
            .any(|route| path.starts_with(route.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_urls_are_https_or_loopback_http_origins_only() {
        for good in [
            "https://prep.example.com",
            "https://prep.example.com/",
            "http://127.0.0.1:8181",
            "http://localhost:8181",
        ] {
            assert!(service_url_ok(good), "should admit {good}");
        }
        for bad in [
            "http://prep.example.com",
            "https://user:pw@prep.example.com",
            "https://prep.example.com/path",
            "https://prep.example.com/?q=1",
            "ftp://prep.example.com",
        ] {
            assert!(!service_url_ok(bad), "should refuse {bad}");
        }
    }
}
