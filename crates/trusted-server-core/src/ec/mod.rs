//! Edge Cookie (EC) identity subsystem.
//!
//! This module owns the EC lifecycle:
//!
//! 1. **Read** — [`EcContext::read_from_request`] extracts any existing EC ID
//!    from headers/cookies, captures the client IP, and builds the consent
//!    context. This is called pre-routing on every request.
//!
//! 2. **Generate** — [`EcContext::generate_if_needed`] creates a new EC ID
//!    when none exists and consent allows it. This is called only in organic
//!    handlers (publisher proxy, integration proxy) — never in read-only
//!    endpoints like `/identify`.
//!
//! # Module structure
//!
//! - [`generation`] — HMAC-based ID generation, IP normalization, format helpers
//! - [`consent`] — EC-specific consent gating wrapper
//! - [`cookies`] — `Set-Cookie` header creation and expiration helpers
//! - [`kv`] — KV Store identity graph operations (CAS, tombstones, debounce)
//! - [`kv_types`] — Schema types for KV identity graph entries
//! - [`partner`] — Partner registry (`PartnerRecord`, `PartnerStore`)
//! - [`admin`] — Admin endpoints for partner management
//! - [`sync_pixel`] — Pixel sync write endpoint (`GET /sync`)
//! - [`identify`] — Browser identity read endpoint (`GET /identify`)

pub mod admin;
pub mod consent;
pub mod cookies;
pub mod finalize;
pub mod generation;
pub mod identify;
pub mod kv;
pub mod kv_types;
pub mod partner;
pub mod sync_pixel;

use cookie::CookieJar;
use error_stack::Report;
use fastly::Request;

use crate::compat;
use crate::consent::{self as consent_mod, ConsentContext, ConsentPipelineInput};
use crate::constants::{COOKIE_TS_EC, HEADER_X_TS_EC};
use crate::cookies::handle_request_cookies;
use crate::ec::cookies::ec_id_has_only_allowed_chars;
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::settings::Settings;

use self::kv::KvIdentityGraph;
use self::kv_types::KvEntry;

pub use generation::{ec_hash, generate_ec_id, is_valid_ec_id};

/// Parsed EC identity from an incoming request.
///
/// Separates the header-derived and cookie-derived EC values so callers
/// can apply different policies (e.g. revocation targets the cookie value).
struct RequestEc {
    /// EC ID from the `X-ts-ec` header, if present.
    header_ec: Option<String>,
    /// EC ID from the `ts-ec` cookie, if present.
    cookie_ec: Option<String>,
    /// The parsed cookie jar (retained for consent pipeline input).
    jar: Option<CookieJar>,
}

/// Parses EC identity from request headers and cookies in a single pass.
///
/// # Errors
///
/// - [`TrustedServerError::InvalidHeaderValue`] if cookie parsing fails
fn parse_ec_from_request(req: &Request) -> Result<RequestEc, Report<TrustedServerError>> {
    let http_req = compat::from_fastly_headers_ref(req);
    let header_ec = http_req
        .headers()
        .get(HEADER_X_TS_EC)
        .and_then(|h| h.to_str().ok())
        .and_then(|value| request_ec_id_if_allowed(value, "x-ts-ec header"));

    let jar = handle_request_cookies(&http_req)?;
    let cookie_ec = jar
        .as_ref()
        .and_then(|j| j.get(COOKIE_TS_EC))
        .and_then(|cookie| request_ec_id_if_allowed(cookie.value(), "ts-ec cookie"));

    Ok(RequestEc {
        header_ec,
        cookie_ec,
        jar,
    })
}

fn request_ec_id_if_allowed(value: &str, source: &str) -> Option<String> {
    if ec_id_has_only_allowed_chars(value) {
        return Some(value.to_owned());
    }

    log::warn!("Rejected EC ID from {source} with disallowed characters");
    None
}

/// Gets an existing EC ID from the request.
///
/// Attempts to retrieve an existing EC ID from:
/// 1. The `x-ts-ec` header
/// 2. The `ts-ec` cookie
///
/// Returns `None` if neither source contains an EC ID.
///
/// # Errors
///
/// - [`TrustedServerError::InvalidHeaderValue`] if cookie parsing fails
pub fn get_ec_id(req: &fastly::Request) -> Result<Option<String>, Report<TrustedServerError>> {
    let parsed = parse_ec_from_request(req)?;
    // Header takes precedence over cookie.
    let ec_id = parsed.header_ec.or(parsed.cookie_ec);
    if let Some(ref id) = ec_id {
        log::trace!("Existing EC ID found: {id}");
    }
    Ok(ec_id)
}

/// Captures the EC state for a single request lifecycle.
///
/// Created via [`read_from_request`](Self::read_from_request) during
/// pre-routing, then optionally mutated by
/// [`generate_if_needed`](Self::generate_if_needed) in organic handlers.
#[derive(Debug)]
pub struct EcContext {
    /// The EC ID value, if one exists (from request) or was generated.
    ec_value: Option<String>,
    /// The EC ID from the `ts-ec` cookie, if present on the incoming
    /// request. Stored separately from `ec_value` because the header may
    /// take precedence, but revocation still needs the cookie value.
    cookie_ec_value: Option<String>,
    /// Whether an EC ID was found on the incoming request (header or cookie).
    ec_was_present: bool,
    /// Whether a new EC ID was generated during this request.
    ec_generated: bool,
    /// The consent context for this request.
    consent: ConsentContext,
    /// The normalized client IP, captured early before the request body
    /// is consumed. `None` when the platform cannot determine client IP.
    client_ip: Option<String>,
    /// Geo information captured pre-routing for downstream KV writes.
    geo_info: Option<GeoInfo>,
}

impl EcContext {
    /// Reads EC state from an incoming request without generating a new ID.
    ///
    /// This is the first phase of the EC lifecycle. It:
    /// - Checks the `X-ts-ec` header and `ts-ec` cookie for an existing EC ID
    /// - Captures the client IP (normalized) for later generation
    /// - Builds the full [`ConsentContext`] from cookies, headers, and geo
    ///
    /// Call this pre-routing on **every** request.
    ///
    /// # Errors
    ///
    /// Returns an error if cookie parsing fails.
    pub fn read_from_request(
        settings: &Settings,
        req: &Request,
    ) -> Result<Self, Report<TrustedServerError>> {
        Self::read_from_request_with_geo(settings, req, None)
    }

    /// Reads EC state from an incoming request using pre-extracted geo data.
    ///
    /// Use this when geo has already been resolved in router prelude to avoid
    /// duplicate lookup work.
    ///
    /// # Errors
    ///
    /// Returns an error if cookie parsing fails.
    pub fn read_from_request_with_geo(
        settings: &Settings,
        req: &Request,
        geo_info: Option<&GeoInfo>,
    ) -> Result<Self, Report<TrustedServerError>> {
        let parsed = parse_ec_from_request(req)?;

        // Header takes precedence over cookie for the active EC value.
        // The cookie value is stored separately for revocation handling.
        let ec_value = parsed.header_ec.or_else(|| parsed.cookie_ec.clone());
        let ec_was_present = ec_value.is_some();

        if let Some(ref id) = ec_value {
            log::trace!("Existing EC ID found: {id}");
        }

        // Capture the client IP now — the request body may be consumed later.
        let client_ip = generation::extract_client_ip(req).ok();
        let http_req = compat::from_fastly_headers_ref(req);

        // Build consent context. Pass the EC ID (if any) so the consent
        // pipeline can use it for KV Store fallback/write operations.
        let consent = consent_mod::build_consent_context(&ConsentPipelineInput {
            jar: parsed.jar.as_ref(),
            req: &http_req,
            config: &settings.consent,
            geo: geo_info,
            ec_id: ec_value.as_deref(),
            kv_store: None,
        });

        Ok(Self {
            ec_value,
            cookie_ec_value: parsed.cookie_ec,
            ec_was_present,
            ec_generated: false,
            consent,
            client_ip,
            geo_info: geo_info.cloned(),
        })
    }

    /// Generates a new EC ID if none exists and consent allows it.
    ///
    /// This is the second phase of the EC lifecycle. Call this only in
    /// organic handlers (publisher proxy, integration proxy, auction) —
    /// never in read-only endpoints.
    ///
    /// If an EC ID already exists (from the request), this is a no-op.
    /// If consent does not permit EC creation, this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if the client IP is unavailable and generation is
    /// needed, or if HMAC generation fails.
    pub fn generate_if_needed(
        &mut self,
        settings: &Settings,
        kv: Option<&KvIdentityGraph>,
    ) -> Result<(), Report<TrustedServerError>> {
        if self.ec_value.is_some() {
            return Ok(());
        }

        if !consent::ec_consent_granted(&self.consent) {
            log::debug!(
                "EC generation skipped: consent not granted (jurisdiction={})",
                self.consent.jurisdiction,
            );
            return Ok(());
        }

        let client_ip = self.client_ip.as_deref().ok_or_else(|| {
            Report::new(TrustedServerError::Ec {
                message: "Client IP required for EC generation but unavailable".to_string(),
            })
        })?;

        let ec_id = generation::generate_ec_id(settings, client_ip)?;
        log::trace!("Generated new EC ID: {ec_id}");
        self.ec_value = Some(ec_id);
        self.ec_generated = true;

        if let (Some(graph), Some(ec_value)) = (kv, self.ec_value.as_deref()) {
            let now = current_timestamp();
            let entry = KvEntry::new(&self.consent, self.geo_info.as_ref(), now);
            let ec_hash = generation::ec_hash(ec_value);

            if let Err(err) = graph.create_or_revive(ec_hash, &entry) {
                log::error!(
                    "Failed to create or revive EC entry for hash '{}' after generation: {err:?}",
                    ec_hash,
                );
            }
        }

        Ok(())
    }

    /// Returns the EC ID value, if present (either from request or generated).
    #[must_use]
    pub fn ec_value(&self) -> Option<&str> {
        self.ec_value.as_deref()
    }

    /// Returns whether the `ts-ec` cookie was present on the incoming request.
    #[must_use]
    pub fn cookie_was_present(&self) -> bool {
        self.cookie_ec_value.is_some()
    }

    /// Returns whether an EC ID was found on the incoming request
    /// (from header or cookie).
    #[must_use]
    pub fn ec_was_present(&self) -> bool {
        self.ec_was_present
    }

    /// Returns whether a new EC ID was generated during this request.
    #[must_use]
    pub fn ec_generated(&self) -> bool {
        self.ec_generated
    }

    /// Returns a reference to the consent context for this request.
    #[must_use]
    pub fn consent(&self) -> &ConsentContext {
        &self.consent
    }

    /// Returns a mutable reference to the consent context.
    ///
    /// Used by `/sync` to apply query-param fallback consent for the current
    /// request only when pre-routing consent extraction produced an empty
    /// context.
    pub fn consent_mut(&mut self) -> &mut ConsentContext {
        &mut self.consent
    }

    /// Returns the normalized client IP, if available.
    #[must_use]
    pub fn client_ip(&self) -> Option<&str> {
        self.client_ip.as_deref()
    }

    /// Returns the pre-routing geo data, if available.
    #[must_use]
    pub fn geo_info(&self) -> Option<&GeoInfo> {
        self.geo_info.as_ref()
    }

    /// Returns whether EC creation is permitted by consent for this request.
    #[must_use]
    pub fn ec_allowed(&self) -> bool {
        consent::ec_consent_granted(&self.consent)
    }

    /// Returns the existing EC cookie value for revocation handling.
    ///
    /// When consent is withdrawn, this value is needed to identify the
    /// correct KV entry to tombstone. Returns `None` if no cookie was
    /// present on the request. This always returns the cookie value,
    /// even when the header took precedence for the active EC ID.
    #[must_use]
    pub fn existing_cookie_ec_id(&self) -> Option<&str> {
        self.cookie_ec_value.as_deref()
    }

    /// Returns true when both cookie and active EC are present and differ.
    #[must_use]
    pub fn has_cookie_mismatch(&self) -> bool {
        matches!(
            (self.cookie_ec_value.as_deref(), self.ec_value.as_deref()),
            (Some(cookie), Some(active)) if cookie != active
        )
    }

    /// Returns the stable EC hash prefix from the active EC value.
    #[must_use]
    pub fn ec_hash(&self) -> Option<&str> {
        self.ec_value.as_deref().map(generation::ec_hash)
    }
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tests::create_test_settings;
    use fastly::http::HeaderValue;

    fn create_test_request(headers: &[(&str, &str)]) -> Request {
        let mut req = Request::new("GET", "http://example.com");
        for &(key, value) in headers {
            req.set_header(
                key,
                HeaderValue::from_str(value).expect("should create valid header value"),
            );
        }
        req
    }

    #[test]
    fn read_from_request_with_header_ec() {
        let settings = create_test_settings();
        let req = create_test_request(&[("x-ts-ec", "header-ec-id")]);

        let ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");

        assert_eq!(ec.ec_value(), Some("header-ec-id"));
        assert!(ec.ec_was_present(), "should detect EC from header");
        assert!(!ec.cookie_was_present(), "should not detect cookie");
        assert!(!ec.ec_generated(), "should not mark as generated");
    }

    #[test]
    fn read_from_request_with_cookie_ec() {
        let settings = create_test_settings();
        let req = create_test_request(&[("cookie", "ts-ec=cookie-ec-id")]);

        let ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");

        assert_eq!(ec.ec_value(), Some("cookie-ec-id"));
        assert!(ec.ec_was_present(), "should detect EC from cookie");
        assert!(ec.cookie_was_present(), "should detect cookie");
        assert!(!ec.ec_generated(), "should not mark as generated");
    }

    #[test]
    fn read_from_request_header_takes_precedence_over_cookie() {
        let settings = create_test_settings();
        let req = create_test_request(&[("x-ts-ec", "header-id"), ("cookie", "ts-ec=cookie-id")]);

        let ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");

        assert_eq!(
            ec.ec_value(),
            Some("header-id"),
            "should prefer header over cookie"
        );
        assert!(ec.cookie_was_present(), "should still detect cookie");
    }

    #[test]
    fn read_from_request_no_ec() {
        let settings = create_test_settings();
        let req = create_test_request(&[]);

        let ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");

        assert!(ec.ec_value().is_none(), "should have no EC value");
        assert!(!ec.ec_was_present(), "should not detect EC");
        assert!(!ec.cookie_was_present(), "should not detect cookie");
    }

    #[test]
    fn generate_if_needed_skips_when_ec_exists() {
        let settings = create_test_settings();
        let req = create_test_request(&[("x-ts-ec", "existing-id")]);

        let mut ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");
        ec.generate_if_needed(&settings, None)
            .expect("should not error when EC already exists");

        assert_eq!(
            ec.ec_value(),
            Some("existing-id"),
            "should keep existing EC"
        );
        assert!(!ec.ec_generated(), "should not mark as generated");
    }

    #[test]
    fn existing_cookie_ec_id_returns_cookie_value() {
        let settings = create_test_settings();

        // With cookie present
        let req = create_test_request(&[("cookie", "ts-ec=cookie-value")]);
        let ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");
        assert_eq!(
            ec.existing_cookie_ec_id(),
            Some("cookie-value"),
            "should return cookie EC ID"
        );

        // With only header (no cookie)
        let req = create_test_request(&[("x-ts-ec", "header-value")]);
        let ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");
        assert!(
            ec.existing_cookie_ec_id().is_none(),
            "should return None when EC came from header only"
        );

        // With both header and cookie — should return cookie value
        let req = create_test_request(&[("x-ts-ec", "header-id"), ("cookie", "ts-ec=cookie-id")]);
        let ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");
        assert_eq!(
            ec.ec_value(),
            Some("header-id"),
            "should use header as active EC"
        );
        assert_eq!(
            ec.existing_cookie_ec_id(),
            Some("cookie-id"),
            "should return cookie value for revocation even when header takes precedence"
        );
    }
}
