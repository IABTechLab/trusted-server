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
//!    endpoints like `/_ts/api/v1/identify`.
//!
//! # Module structure
//!
//! - [`generation`] — HMAC-based ID generation, IP normalization, format helpers
//! - [`consent`] — EC-specific consent gating wrapper
//! - [`cookies`] — `Set-Cookie` header creation and expiration helpers
//! - [`kv`] — KV Store identity graph operations (CAS, tombstones, debounce)
//! - [`kv_types`] — Schema types for KV identity graph entries
//! - [`device`] — Device signal derivation (UA, JA4, H2 fingerprinting)
//! - [`partner`] — Partner validation helpers (ID format, pull sync config)
//! - [`registry`] — In-memory partner registry built from config
//! - [`rate_limiter`] — Rate limiting abstraction (Fastly Edge Rate Limiting)
//! - [`identify`] — Identity read endpoint (`GET /_ts/api/v1/identify`)
//! - [`eids`] — Shared EID resolution and formatting helpers
//! - [`batch_sync`] — S2S batch sync endpoint (`POST /_ts/api/v1/batch-sync`)
//! - [`pull_sync`] — Background pull-sync dispatcher for organic routes

pub mod batch_sync;
pub mod consent;
pub mod cookies;
pub mod device;
pub mod eids;
pub mod finalize;
pub mod generation;
pub mod identify;
pub mod kv;
pub mod kv_types;
pub mod partner;
pub mod prebid_eids;
pub mod pull_sync;
pub mod rate_limiter;
pub mod registry;

/// Truncates an EC ID for safe inclusion in log messages.
///
/// Returns the first 8 characters followed by `…` to aid debugging without
/// writing the full user identifier to logs (satisfies the `CodeQL`
/// "cleartext logging of sensitive information" rule).
#[must_use]
pub fn log_id(ec_id: &str) -> String {
    let prefix = ec_id.get(..8).unwrap_or(ec_id);
    format!("{prefix}…")
}

use cookie::CookieJar;
use error_stack::Report;
use fastly::Request;

use crate::consent::{self as consent_mod, ConsentContext, ConsentPipelineInput};
use crate::constants::{COOKIE_TS_EC, HEADER_X_TS_EC};
use crate::cookies::handle_request_cookies;
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::settings::Settings;
use device::DeviceSignals;

use self::kv::KvIdentityGraph;
use self::kv_types::KvEntry;

pub use generation::{
    ec_hash, generate_ec_id, is_valid_ec_hash, is_valid_ec_id, normalize_ec_id_for_kv,
};

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
    let header_ec = req
        .get_header(HEADER_X_TS_EC)
        .and_then(|h| h.to_str().ok())
        .map(str::to_owned);

    let jar = handle_request_cookies(req)?;
    let cookie_ec = jar
        .as_ref()
        .and_then(|j| j.get(COOKIE_TS_EC))
        .map(|cookie| cookie.value().to_owned());

    Ok(RequestEc {
        header_ec,
        cookie_ec,
        jar,
    })
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
    // Header takes precedence over cookie; malformed values are discarded.
    let ec_id = parsed
        .header_ec
        .filter(|v| is_valid_ec_id(v))
        .or_else(|| parsed.cookie_ec.filter(|v| is_valid_ec_id(v)));
    if let Some(ref id) = ec_id {
        log::trace!("Existing EC ID found: {}", log_id(id));
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
    /// Device signals derived from TLS/H2/UA in the adapter layer.
    /// Set via [`EcContext::set_device_signals`] before
    /// [`EcContext::generate_if_needed`] is called.
    device_signals: Option<DeviceSignals>,
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
        #[allow(deprecated)]
        let geo_info = GeoInfo::from_request(req);
        Self::read_from_request_with_geo(settings, req, geo_info.as_ref())
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
        // Malformed values are discarded per §4.2: "If the header is
        // present but malformed, it is discarded and the cookie value
        // is used instead."
        let ec_value = parsed
            .header_ec
            .filter(|v| is_valid_ec_id(v))
            .or_else(|| parsed.cookie_ec.clone().filter(|v| is_valid_ec_id(v)));
        let ec_was_present = ec_value.is_some();

        if let Some(ref id) = ec_value {
            log::trace!("Existing EC ID found: {}", log_id(id));
        }

        // Capture the client IP now — the request body may be consumed later.
        let client_ip = generation::extract_client_ip(req).ok();

        // Build consent context. Pass the EC ID (if any) so the consent
        // pipeline can use it for KV Store fallback/write operations.
        let consent = consent_mod::build_consent_context(&ConsentPipelineInput {
            jar: parsed.jar.as_ref(),
            req,
            config: &settings.consent,
            geo: geo_info,
            ec_id: ec_value.as_deref(),
            kv_store: None, // EC module manages its own KV identity graph
        });

        Ok(Self {
            ec_value,
            cookie_ec_value: parsed.cookie_ec,
            ec_was_present,
            ec_generated: false,
            consent,
            client_ip,
            geo_info: geo_info.cloned(),
            device_signals: None,
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
            Report::new(TrustedServerError::EdgeCookie {
                message: "Client IP required for EC generation but unavailable".to_string(),
            })
        })?;

        let ec_id = generation::generate_ec_id(settings, client_ip)?;
        log::trace!("Generated new EC ID: {}", log_id(&ec_id));
        self.ec_value = Some(ec_id);
        self.ec_generated = true;

        if let (Some(graph), Some(ec_value)) = (kv, self.ec_value.as_deref()) {
            let now = current_timestamp();
            let mut entry = KvEntry::new(
                &self.consent,
                self.geo_info.as_ref(),
                now,
                &settings.publisher.domain,
            );
            entry.device = self
                .device_signals
                .as_ref()
                .map(DeviceSignals::to_kv_device);

            if let Err(err) = graph.create_or_revive(ec_value, &entry) {
                log::error!(
                    "Failed to create or revive EC entry for id '{}' after generation: {err:?}",
                    log_id(ec_value),
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
    /// Used by `/_ts/api/v1/sync` to apply query-param fallback consent for the current
    /// request only when pre-routing consent extraction produced an empty
    /// context.
    pub fn consent_mut(&mut self) -> &mut ConsentContext {
        &mut self.consent
    }

    /// Sets the device signals derived from the adapter layer.
    ///
    /// Must be called before [`generate_if_needed`] so that new entries
    /// include the [`KvDevice`] record. The adapter derives these from
    /// `req.get_tls_ja4()`, `req.get_client_h2_fingerprint()`, and UA.
    ///
    /// [`KvDevice`]: super::kv_types::KvDevice
    /// [`generate_if_needed`]: Self::generate_if_needed
    pub fn set_device_signals(&mut self, signals: DeviceSignals) {
        self.device_signals = Some(signals);
    }

    /// Returns the device signals, if set.
    #[must_use]
    pub fn device_signals(&self) -> Option<&DeviceSignals> {
        self.device_signals.as_ref()
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

    /// Creates a test-only `EcContext` with explicit field values.
    #[cfg(test)]
    #[must_use]
    pub fn new_for_test(ec_value: Option<String>, consent: ConsentContext) -> Self {
        Self {
            ec_was_present: ec_value.is_some(),
            cookie_ec_value: ec_value.clone(),
            ec_value,
            ec_generated: false,
            consent,
            client_ip: None,
            geo_info: None,
            device_signals: None,
        }
    }

    /// Creates a test-only [`EcContext`] with explicit client IP.
    #[cfg(test)]
    #[must_use]
    pub fn new_for_test_with_ip(
        ec_value: Option<String>,
        consent: ConsentContext,
        client_ip: Option<String>,
    ) -> Self {
        Self {
            ec_was_present: ec_value.is_some(),
            cookie_ec_value: ec_value.clone(),
            ec_value,
            ec_generated: false,
            consent,
            client_ip,
            geo_info: None,
            device_signals: None,
        }
    }

    /// Creates a test-only [`EcContext`] with independent cookie and active EC
    /// values. Use this to test cookie-mismatch and withdrawal scenarios.
    #[cfg(test)]
    #[must_use]
    pub fn new_for_test_with_cookie(
        ec_value: Option<String>,
        cookie_ec_value: Option<String>,
        ec_was_present: bool,
        ec_generated: bool,
        consent: ConsentContext,
    ) -> Self {
        Self {
            ec_value,
            cookie_ec_value,
            ec_was_present,
            ec_generated,
            consent,
            client_ip: None,
            geo_info: None,
            device_signals: None,
        }
    }
}

/// Returns the current Unix timestamp in seconds.
///
/// Uses `std::time::SystemTime` which is supported on `wasm32-wasip1`.
pub(crate) fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_else(|err| {
            log::error!("SystemTime::now() failed, falling back to epoch 0: {err}");
            0
        })
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

    /// Creates a valid EC ID for testing: `{64hex}.{6alnum}`.
    fn valid_ec_id(prefix_char: &str, suffix: &str) -> String {
        format!("{}.{suffix}", prefix_char.repeat(64))
    }

    #[test]
    fn read_from_request_with_header_ec() {
        let settings = create_test_settings();
        let ec_id = valid_ec_id("a", "HdrEc1");
        let req = create_test_request(&[("x-ts-ec", &ec_id)]);

        let ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");

        assert_eq!(ec.ec_value(), Some(ec_id.as_str()));
        assert!(ec.ec_was_present(), "should detect EC from header");
        assert!(!ec.cookie_was_present(), "should not detect cookie");
        assert!(!ec.ec_generated(), "should not mark as generated");
    }

    #[test]
    fn read_from_request_with_cookie_ec() {
        let settings = create_test_settings();
        let ec_id = valid_ec_id("b", "CkEc01");
        let cookie = format!("ts-ec={ec_id}");
        let req = create_test_request(&[("cookie", &cookie)]);

        let ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");

        assert_eq!(ec.ec_value(), Some(ec_id.as_str()));
        assert!(ec.ec_was_present(), "should detect EC from cookie");
        assert!(ec.cookie_was_present(), "should detect cookie");
        assert!(!ec.ec_generated(), "should not mark as generated");
    }

    #[test]
    fn read_from_request_header_takes_precedence_over_cookie() {
        let settings = create_test_settings();
        let header_id = valid_ec_id("a", "Hdr001");
        let cookie_id = valid_ec_id("b", "Ck0001");
        let cookie = format!("ts-ec={cookie_id}");
        let req = create_test_request(&[("x-ts-ec", &header_id), ("cookie", &cookie)]);

        let ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");

        assert_eq!(
            ec.ec_value(),
            Some(header_id.as_str()),
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
    fn read_from_request_discards_malformed_header_falls_back_to_cookie() {
        let settings = create_test_settings();
        let cookie_id = valid_ec_id("c", "FbCk01");
        let cookie = format!("ts-ec={cookie_id}");
        let req = create_test_request(&[("x-ts-ec", "malformed-header"), ("cookie", &cookie)]);

        let ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");

        assert_eq!(
            ec.ec_value(),
            Some(cookie_id.as_str()),
            "should fall back to cookie when header is malformed"
        );
        assert!(ec.cookie_was_present(), "should detect cookie");
    }

    #[test]
    fn read_from_request_discards_malformed_header_and_cookie() {
        let settings = create_test_settings();
        let req = create_test_request(&[("x-ts-ec", "bad-header"), ("cookie", "ts-ec=bad-cookie")]);

        let ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");

        assert!(
            ec.ec_value().is_none(),
            "should discard both malformed header and cookie"
        );
        assert!(
            !ec.ec_was_present(),
            "ec_was_present should be false when no valid EC found"
        );
        assert!(
            ec.cookie_was_present(),
            "cookie_was_present should still be true for withdrawal path"
        );
    }

    #[test]
    fn generate_if_needed_skips_when_ec_exists() {
        let settings = create_test_settings();
        let ec_id = valid_ec_id("d", "Exist1");
        let req = create_test_request(&[("x-ts-ec", &ec_id)]);

        let mut ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");
        ec.generate_if_needed(&settings, None)
            .expect("should not error when EC already exists");

        assert_eq!(
            ec.ec_value(),
            Some(ec_id.as_str()),
            "should keep existing EC"
        );
        assert!(!ec.ec_generated(), "should not mark as generated");
    }

    #[test]
    fn existing_cookie_ec_id_returns_cookie_value() {
        let settings = create_test_settings();

        // With cookie present (valid format)
        let cookie_ec = valid_ec_id("e", "CkVal1");
        let cookie = format!("ts-ec={cookie_ec}");
        let req = create_test_request(&[("cookie", &cookie)]);
        let ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");
        assert_eq!(
            ec.existing_cookie_ec_id(),
            Some(cookie_ec.as_str()),
            "should return cookie EC ID"
        );

        // With only header (no cookie)
        let header_ec = valid_ec_id("f", "HdrVl1");
        let req = create_test_request(&[("x-ts-ec", &header_ec)]);
        let ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");
        assert!(
            ec.existing_cookie_ec_id().is_none(),
            "should return None when EC came from header only"
        );

        // With both header and cookie — should return cookie value
        let header_ec2 = valid_ec_id("a", "Hdr002");
        let cookie_ec2 = valid_ec_id("b", "Ck0002");
        let cookie2 = format!("ts-ec={cookie_ec2}");
        let req = create_test_request(&[("x-ts-ec", &header_ec2), ("cookie", &cookie2)]);
        let ec = EcContext::read_from_request(&settings, &req).expect("should read EC context");
        assert_eq!(
            ec.ec_value(),
            Some(header_ec2.as_str()),
            "should use header as active EC"
        );
        assert_eq!(
            ec.existing_cookie_ec_id(),
            Some(cookie_ec2.as_str()),
            "should return cookie value for revocation even when header takes precedence"
        );
    }
}
