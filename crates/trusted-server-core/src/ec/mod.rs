//! Edge Cookie (EC) identity subsystem.
//!
//! This module owns the EC lifecycle:
//!
//! 1. **Read** — [`EcContext::read_from_request`] extracts any existing EC ID
//!    from cookies, captures the client IP, and builds the consent
//!    context. This is called pre-routing on every request.
//!
//! 2. **Generate** — [`EcContext::generate_if_needed`] creates a new EC ID
//!    when none exists and consent allows it. This is called only in organic
//!    handlers (publisher proxy, integration proxy) — never in read-only
//!    endpoints like `/_ts/api/v1/identify`.
//!
//! # Module structure
//!
//! - auth (private) — shared Bearer-token authentication helpers
//! - [`generation`] — HMAC-based ID generation, IP normalization, format helpers
//! - [`consent`]: EC-specific permission gating, with consent as one input
//! - [`cookies`] — `Set-Cookie` header creation and expiration helpers
//! - [`kv`] — KV Store identity graph operations (CAS, tombstones, debounce)
//! - [`kv_backend`] — Platform-neutral KV primitives implemented by adapters
//! - [`kv_types`] — Schema types for KV identity graph entries
//! - [`device`] — Device signal derivation (UA, JA4, H2 fingerprinting)
//! - [`partner`] — Partner validation helpers (ID format, pull sync config)
//! - [`registry`] — In-memory partner registry built from config
//! - [`rate_limiter`] — Rate limiting abstraction (implemented by adapters)
//! - [`identify`] — Identity read endpoint (`GET /_ts/api/v1/identify`)
//! - [`eids`] — Shared EID resolution and formatting helpers
//! - [`batch_sync`] — S2S batch sync endpoint (`POST /_ts/api/v1/batch-sync`)
//! - [`pull_sync`] — Background pull-sync dispatcher for organic routes

mod auth;

pub mod batch_sync;
pub mod consent;
pub mod cookies;
pub mod device;
pub mod eids;
pub mod finalize;
pub mod generation;
pub mod identify;
pub mod kv;
pub mod kv_backend;
pub mod kv_types;
pub mod partner;
pub mod prebid_eids;
pub mod provider;
pub mod pull_sync;
pub mod rate_limiter;
pub mod registry;
pub mod resolve;

/// Truncates an EC ID for safe inclusion in log messages.
///
/// Returns the first 8 characters followed by `…` to aid debugging without
/// writing the full user identifier to logs (satisfies the `CodeQL`
/// "cleartext logging of sensitive information" rule).
#[must_use]
pub fn log_id(ec_id: &str) -> String {
    let prefix = ec_id.get(..8).unwrap_or(ec_id);
    format!("{prefix}\u{2026}")
}

use std::sync::Arc;

use cookie::CookieJar;
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::Request;

use crate::consent::{self as consent_mod, ConsentContext, ConsentPipelineInput};
use crate::constants::COOKIE_TS_EC;
use crate::cookies::handle_request_cookies;
use crate::ec::cookies::ec_id_has_only_allowed_chars;
use crate::error::TrustedServerError;
use crate::evidence::{BorrowedRequestInfo, HostSignals};
use crate::geo::GeoInfo;
use crate::permissions::PermissionState;
use crate::platform::RuntimeServices;
use crate::settings::Settings;
use device::DeviceSignals;
use provider::{EdgeCookieProvider, GeneratedEdgeCookie, IdentityInput, build_provider};

use self::kv::KvIdentityGraph;
use self::kv_types::KvEntry;

pub use generation::{
    ec_hash, generate_ec_id, is_valid_ec_hash, is_valid_ec_id, normalize_ec_id_for_kv,
};

/// Parsed EC identity from an incoming request.
struct RequestEc {
    /// EC ID from the `ts-ec` cookie, if present.
    cookie_ec: Option<String>,
    /// The parsed cookie jar (retained for consent pipeline input).
    jar: Option<CookieJar>,
}

/// Parses EC identity from request cookies in a single pass.
///
/// # Errors
///
/// - [`TrustedServerError::InvalidHeaderValue`] if cookie parsing fails
fn parse_ec_from_request(req: &Request<EdgeBody>) -> Result<RequestEc, Report<TrustedServerError>> {
    let jar = handle_request_cookies(req)?;
    let cookie_ec = jar
        .as_ref()
        .and_then(|j| j.get(COOKIE_TS_EC))
        .map(cookie::Cookie::value)
        .and_then(|value| request_ec_id_if_allowed(value, "ts-ec cookie"));

    Ok(RequestEc { cookie_ec, jar })
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
/// Attempts to retrieve an existing EC ID from the `ts-ec` cookie.
///
/// Returns `None` if the cookie does not contain a valid EC ID.
///
/// # Errors
///
/// - [`TrustedServerError::InvalidHeaderValue`] if cookie parsing fails
pub fn get_ec_id(req: &Request<EdgeBody>) -> Result<Option<String>, Report<TrustedServerError>> {
    let parsed = parse_ec_from_request(req)?;
    let ec_id = parsed.cookie_ec.filter(|v| is_valid_ec_id(v));
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
#[derive(Debug, Default, Clone)]
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
    /// Whether the configured Edge Cookie provider's required permissions are
    /// set for this request. Resolved once at construction through the
    /// permission model and read via [`ec_allowed`](Self::ec_allowed).
    ec_allowed: bool,
    /// The permissions resolved for this request: the country/region baseline
    /// augmented by the session's signals. Assembled once at construction and
    /// read via [`permissions`](Self::permissions).
    permissions: PermissionState,
    /// The normalized client IP, captured early before the request body
    /// is consumed. `None` when the platform cannot determine client IP.
    client_ip: Option<String>,
    /// Geo information captured pre-routing for downstream KV writes.
    geo_info: Option<GeoInfo>,
    /// Device signals derived from TLS/H2/UA in the adapter layer.
    /// Set via [`EcContext::set_device_signals`] before
    /// [`EcContext::generate_if_needed`] is called.
    device_signals: Option<DeviceSignals>,
    /// The host-signal service for this request, when the host supplies one
    /// (the Fastly adapter registers the TLS/HTTP-2 fingerprints). `None` on a
    /// host that exposes none. Injected into a provider that needs it when the
    /// provider is built.
    host_signals: Option<Arc<dyn HostSignals>>,
    /// Response headers a provider asked to set, captured during
    /// [`EcContext::generate_if_needed`] and applied to the response by EC
    /// finalization. Empty for providers that set no headers.
    response_headers: Vec<(http::HeaderName, http::HeaderValue)>,
}

impl EcContext {
    /// Reads EC state from an incoming request without generating a new ID.
    ///
    /// This is the first phase of the EC lifecycle. It:
    /// - Checks the `ts-ec` cookie for an existing EC ID
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
        req: &Request<EdgeBody>,
        services: &RuntimeServices,
    ) -> Result<Self, Report<TrustedServerError>> {
        Self::read_from_request_with_geo(settings, req, services, None)
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
        req: &Request<EdgeBody>,
        services: &RuntimeServices,
        geo_info: Option<&GeoInfo>,
    ) -> Result<Self, Report<TrustedServerError>> {
        let parsed = parse_ec_from_request(req)?;

        let ec_value = parsed.cookie_ec.clone().filter(|v| is_valid_ec_id(v));
        let ec_was_present = ec_value.is_some();

        if let Some(ref id) = ec_value {
            log::trace!("Existing EC ID found: {}", log_id(id));
        }

        // Capture the client IP from platform services (normalized).
        let client_ip = services
            .client_info()
            .client_ip
            .map(generation::normalize_ip);

        // Build consent context from request-local cookies, headers, and geo.
        let consent = consent_mod::build_consent_context(&ConsentPipelineInput {
            jar: parsed.jar.as_ref(),
            req,
            config: &settings.consent,
            geo: geo_info,
            ec_id: None,
            kv_store: None,
        });

        // Assemble the permission state once, here, through the permission
        // model, building the country/region baseline augmented by the session's
        // signals. Downstream consumers read the stored result via
        // [`EcContext::permissions`] and [`EcContext::ec_allowed`] rather than
        // re-deriving it.
        let permissions = consent::assemble_permissions(settings, &consent, geo_info);
        // Build the selected provider once, injecting the request info and any
        // host signals the host supplies, to read its required permissions. A
        // provider that needs a service the host did not supply fails to build
        // here, which stops the request.
        let host_signals = services.host_signals();
        // The provider is built here only to read its required permissions, which
        // need no request data, so nothing is cloned from the request.
        let ec_allowed = build_provider(&settings.ec, host_signals.clone())?
            .is_none_or(|provider| permissions.all_set(provider.required_permissions()));

        log::info!(
            "EC context: present={}, cookie_present={}, ec_allowed={}, jurisdiction={}",
            ec_was_present,
            parsed.cookie_ec.is_some(),
            ec_allowed,
            consent.jurisdiction,
        );

        Ok(Self {
            ec_value,
            cookie_ec_value: parsed.cookie_ec,
            ec_was_present,
            ec_generated: false,
            consent,
            ec_allowed,
            permissions,
            client_ip,
            geo_info: geo_info.cloned(),
            device_signals: None,
            host_signals,
            response_headers: Vec::new(),
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

        if !self.ec_allowed {
            log::info!(
                "EC generation skipped: required permissions not set (jurisdiction={})",
                self.consent.jurisdiction,
            );
            return Ok(());
        }

        // EC generation needs the client IP; fail early if it is unavailable. The
        // provider reads it borrowed at generate time (see
        // [`generate_with_provider`]), so nothing is cloned here.
        if self.client_ip.is_none() {
            return Err(Report::new(TrustedServerError::EdgeCookie {
                message: "Client IP required for EC generation but unavailable".to_owned(),
            }));
        }
        let Some(ec_provider) = build_provider(&settings.ec, self.host_signals.clone())? else {
            log::info!("EC generation skipped: no Edge Cookie provider configured");
            return Ok(());
        };

        self.generate_with_provider(ec_provider.as_ref(), settings, kv)
    }

    /// Derives and commits an EC identifier using a specific provider.
    ///
    /// Split out of [`generate_if_needed`](Self::generate_if_needed) so the
    /// provider is supplied explicitly: the configured path builds it from
    /// settings, and tests pass one in to observe the [`IdentityInput`] a
    /// provider receives. This path passes no header snapshot (the built-ins
    /// read only the client IP); a provider that needs request headers reads
    /// them through [`RequestInfo`](crate::evidence::RequestInfo) where the
    /// caller supplies them. The skip guards (existing EC, permission gate)
    /// stay in [`generate_if_needed`](Self::generate_if_needed).
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::EdgeCookie`] when the client IP is
    /// unavailable, the provider fails to derive an identifier, or persisting a
    /// generated identifier to the KV identity graph fails.
    fn generate_with_provider(
        &mut self,
        ec_provider: &dyn EdgeCookieProvider,
        settings: &Settings,
        kv: Option<&KvIdentityGraph>,
    ) -> Result<(), Report<TrustedServerError>> {
        let input = IdentityInput {
            permissions: Some(&self.permissions),
            consent: Some(&self.consent),
        };
        // The provider reads only the client IP on this path; pass it borrowed
        // with no header snapshot, so no request data is cloned.
        let request_info =
            BorrowedRequestInfo::new(self.client_ip.as_deref().unwrap_or_default(), None);
        let generated: GeneratedEdgeCookie = ec_provider.generate(&request_info, &input)?;
        // Capture any response headers the provider asked for, even when it
        // produced no identifier (for example while it still needs more client
        // evidence). EC finalization applies them to the response.
        self.response_headers = generated.response_headers;
        let Some(ec_id) = generated.id else {
            log::info!(
                "EC generation produced no identifier (provider={}); proceeding without an EC",
                ec_provider.id(),
            );
            return Ok(());
        };
        log::info!(
            "Generated new EC ID (provider={}): {}",
            ec_provider.id(),
            log_id(&ec_id),
        );
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
                self.ec_value = None;
                self.ec_generated = false;
                return Err(err.change_context(TrustedServerError::EdgeCookie {
                    message: "Failed to persist generated EC ID to KV identity graph".to_string(),
                }));
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

    /// Returns whether an EC ID was found in the `ts-ec` cookie on the
    /// incoming request.
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
    /// Allows handlers to apply query-param fallback consent for the current
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

    /// Returns the response headers a provider asked to set during
    /// [`generate_if_needed`](Self::generate_if_needed). Empty unless a provider
    /// produced any.
    #[must_use]
    pub fn response_headers(&self) -> &[(http::HeaderName, http::HeaderValue)] {
        &self.response_headers
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

    /// Returns the host-computed client fingerprints captured for this request,
    /// when the host supplies them.
    ///
    /// The resolve path rebuilds the provider with the same injected services
    /// as the organic path, so it reads the host signals captured here rather
    /// than re-deriving them.
    pub(crate) fn host_signals(&self) -> Option<Arc<dyn HostSignals>> {
        self.host_signals.clone()
    }

    /// Returns the pre-routing geo data, if available.
    #[must_use]
    pub fn geo_info(&self) -> Option<&GeoInfo> {
        self.geo_info.as_ref()
    }

    /// Returns whether the configured Edge Cookie provider's required
    /// permissions are set for this request.
    ///
    /// Resolved once at construction through the permission model (see
    /// [`consent::ec_permission_granted`]).
    #[must_use]
    pub fn ec_allowed(&self) -> bool {
        self.ec_allowed
    }

    /// Returns the permissions resolved for this request.
    ///
    /// Assembled once at construction, the country/region baseline augmented by
    /// the session's signals. The core gates provider execution on these, and a
    /// consumer may read them for its own logic.
    #[must_use]
    pub fn permissions(&self) -> &PermissionState {
        &self.permissions
    }

    /// Returns the existing EC cookie value for revocation handling.
    ///
    /// When consent is withdrawn, this value is needed to identify the
    /// correct KV entry to tombstone. Returns `None` if no cookie was
    /// present on the request. This always returns the cookie value.
    #[must_use]
    pub fn existing_cookie_ec_id(&self) -> Option<&str> {
        self.cookie_ec_value.as_deref()
    }

    /// Returns `true` when the request carried a cookie EC and the selected
    /// active EC denotes a different identity than the cookie value.
    ///
    /// The equality test is delegated to the [`EdgeCookieProvider`], because EC
    /// identifiers are not assumed comparable by natural string equality: two
    /// values may be different wrappers of the same payload, which only the
    /// provider knows how to compare.
    #[must_use]
    pub fn cookie_differs_from_active_ec(&self, provider: &dyn EdgeCookieProvider) -> bool {
        matches!(
            (self.cookie_ec_value.as_deref(), self.ec_value.as_deref()),
            (Some(cookie), Some(active)) if !provider.keys_equal(cookie, active)
        )
    }

    /// Returns the stable EC hash prefix from the active EC value.
    #[must_use]
    pub fn ec_hash(&self) -> Option<&str> {
        self.ec_value.as_deref().map(generation::ec_hash)
    }

    /// Creates a test-only `EcContext` with the permission gate open.
    ///
    /// Use [`new_for_test_gated`](Self::new_for_test_gated) when a test needs
    /// the gate closed.
    #[cfg(test)]
    #[must_use]
    pub fn new_for_test(ec_value: Option<String>, consent: ConsentContext) -> Self {
        Self::new_for_test_gated(ec_value, consent, true)
    }

    /// Creates a test-only `EcContext` with an explicit permission gate.
    ///
    /// `ec_allowed` stands in for the permission decision the production path
    /// resolves at construction, so a test can exercise the gate-open and
    /// gate-closed branches directly.
    #[cfg(test)]
    #[must_use]
    pub fn new_for_test_gated(
        ec_value: Option<String>,
        consent: ConsentContext,
        ec_allowed: bool,
    ) -> Self {
        Self {
            ec_was_present: ec_value.is_some(),
            cookie_ec_value: ec_value.clone(),
            ec_value,
            ec_generated: false,
            consent,
            ec_allowed,
            permissions: PermissionState::default(),
            client_ip: None,
            geo_info: None,
            device_signals: None,
            host_signals: None,
            response_headers: Vec::new(),
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
            ec_allowed: true,
            permissions: PermissionState::default(),
            client_ip,
            geo_info: None,
            device_signals: None,
            host_signals: None,
            response_headers: Vec::new(),
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
        ec_allowed: bool,
    ) -> Self {
        Self {
            ec_value,
            cookie_ec_value,
            ec_was_present,
            ec_generated,
            consent,
            ec_allowed,
            permissions: PermissionState::default(),
            client_ip: None,
            geo_info: None,
            device_signals: None,
            host_signals: None,
            response_headers: Vec::new(),
        }
    }
}

/// Returns the current Unix timestamp in seconds.
///
/// Uses [`web_time::SystemTime`], which maps to `std::time::SystemTime` on
/// native and `wasm32-wasip1` targets and to a JS-backed clock on
/// `wasm32-unknown-unknown` (Cloudflare Workers), where `std::time` is not
/// available.
pub(crate) fn current_timestamp() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_else(|err| {
            log::error!("SystemTime::now() failed, falling back to epoch 0: {err}");
            0
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::{OwnedRequestInfo, RequestInfo};
    use crate::platform::test_support::noop_services;
    use crate::test_support::tests::create_test_settings;

    fn create_test_request(headers: &[(&str, &str)]) -> Request<EdgeBody> {
        let mut builder = Request::builder().method("GET").uri("http://example.com");
        for &(key, value) in headers {
            builder = builder.header(key, value);
        }
        builder
            .body(EdgeBody::empty())
            .expect("should build test request")
    }

    /// Creates a valid EC ID for testing: `{64hex}.{6alnum}`.
    fn valid_ec_id(prefix_char: &str, suffix: &str) -> String {
        format!("{}.{suffix}", prefix_char.repeat(64))
    }

    /// A test provider that compares identifiers by the payload after a `:`,
    /// modeling an envelope whose wrapper can differ for the same identity.
    struct WrapperInsensitiveProvider;

    impl EdgeCookieProvider for WrapperInsensitiveProvider {
        fn id(&self) -> &'static str {
            "wrapper-insensitive"
        }

        fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(GeneratedEdgeCookie::default())
        }

        fn keys_equal(&self, left: &str, right: &str) -> bool {
            fn payload(value: &str) -> &str {
                value.split_once(':').map_or(value, |(_, payload)| payload)
            }
            payload(left) == payload(right)
        }
    }

    /// A test provider that does not override `keys_equal`, so it uses the
    /// default natural string equality.
    struct NaturalProvider;

    impl EdgeCookieProvider for NaturalProvider {
        fn id(&self) -> &'static str {
            "natural"
        }

        fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(GeneratedEdgeCookie::default())
        }
    }

    #[test]
    fn cookie_differs_from_active_ec_delegates_to_the_provider() {
        // The cookie and the active EC are different wrappers of one payload.
        let context = EcContext::new_for_test_with_cookie(
            Some("wrapper-active:shared-payload".to_owned()),
            Some("wrapper-cookie:shared-payload".to_owned()),
            true,
            false,
            ConsentContext::default(),
            true,
        );

        assert!(
            !context.cookie_differs_from_active_ec(&WrapperInsensitiveProvider),
            "a payload-aware provider should treat different wrappers as the same identity"
        );
        assert!(
            context.cookie_differs_from_active_ec(&NaturalProvider),
            "natural equality should treat different wrappers as different"
        );
    }

    /// A provider that records the `Cookie` header from the request info passed
    /// to `generate`, so a test can prove request cookies reach a provider (a
    /// client that stores values in cookies relies on this).
    struct CookieCapturingProvider {
        seen_cookie: std::sync::Mutex<Option<String>>,
    }

    impl EdgeCookieProvider for CookieCapturingProvider {
        fn id(&self) -> &'static str {
            "cookie-capturing"
        }

        fn generate(
            &self,
            request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            let cookie = request_info.header("cookie").map(ToOwned::to_owned);
            *self.seen_cookie.lock().expect("should lock seen cookie") = cookie;
            Ok(GeneratedEdgeCookie::default())
        }
    }

    #[test]
    fn a_provider_reads_request_cookies_from_the_request_info() {
        // RequestInfo contract: a provider given request info that carries
        // headers can read request cookies through it (a client that stores
        // values in cookies relies on this). The organic generate path passes
        // no header snapshot; a caller that has headers supplies them.
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "cookie",
            "client-id=abc123; ts-ec=xyz"
                .parse()
                .expect("should build a valid cookie header"),
        );
        let request_info = OwnedRequestInfo::new("203.0.113.7".to_owned(), headers);
        let provider = CookieCapturingProvider {
            seen_cookie: std::sync::Mutex::new(None),
        };

        provider
            .generate(&request_info, &IdentityInput::default())
            .expect("generation should succeed");

        assert_eq!(
            provider
                .seen_cookie
                .lock()
                .expect("should lock seen cookie")
                .as_deref(),
            Some("client-id=abc123; ts-ec=xyz"),
            "the provider should read the request cookies from the request info"
        );
    }

    #[test]
    fn read_from_request_ignores_header_ec() {
        let settings = create_test_settings();
        let ec_id = valid_ec_id("a", "HdrEc1");
        let req = create_test_request(&[("x-ts-ec", &ec_id)]);

        let ec = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");

        assert!(ec.ec_value().is_none(), "should ignore EC from header");
        assert!(!ec.ec_was_present(), "should not detect EC from header");
        assert!(!ec.cookie_was_present(), "should not detect cookie");
        assert!(!ec.ec_generated(), "should not mark as generated");
    }

    #[test]
    fn read_from_request_with_cookie_ec() {
        let settings = create_test_settings();
        let ec_id = valid_ec_id("b", "CkEc01");
        let cookie = format!("ts-ec={ec_id}");
        let req = create_test_request(&[("cookie", &cookie)]);

        let ec = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");

        assert_eq!(ec.ec_value(), Some(ec_id.as_str()));
        assert!(ec.ec_was_present(), "should detect EC from cookie");
        assert!(ec.cookie_was_present(), "should detect cookie");
        assert!(!ec.ec_generated(), "should not mark as generated");
    }

    #[test]
    fn read_from_request_cookie_is_authoritative_when_header_present() {
        let settings = create_test_settings();
        let header_id = valid_ec_id("a", "Hdr001");
        let cookie_id = valid_ec_id("b", "Ck0001");
        let cookie = format!("ts-ec={cookie_id}");
        let req = create_test_request(&[("x-ts-ec", &header_id), ("cookie", &cookie)]);

        let ec = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");

        assert_eq!(
            ec.ec_value(),
            Some(cookie_id.as_str()),
            "should use cookie instead of header"
        );
        assert!(ec.cookie_was_present(), "should still detect cookie");
    }

    #[test]
    fn read_from_request_no_ec() {
        let settings = create_test_settings();
        let req = create_test_request(&[]);

        let ec = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");

        assert!(ec.ec_value().is_none(), "should have no EC value");
        assert!(!ec.ec_was_present(), "should not detect EC");
        assert!(!ec.cookie_was_present(), "should not detect cookie");
    }

    #[test]
    fn read_from_request_uses_cookie_when_malformed_header_present() {
        let settings = create_test_settings();
        let cookie_id = valid_ec_id("c", "FbCk01");
        let cookie = format!("ts-ec={cookie_id}");
        let req = create_test_request(&[("x-ts-ec", "malformed-header"), ("cookie", &cookie)]);

        let ec = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");

        assert_eq!(
            ec.ec_value(),
            Some(cookie_id.as_str()),
            "should use cookie when header is malformed"
        );
        assert!(ec.cookie_was_present(), "should detect cookie");
    }

    #[test]
    fn read_from_request_discards_malformed_header_and_cookie() {
        let settings = create_test_settings();
        let req = create_test_request(&[("x-ts-ec", "bad-header"), ("cookie", "ts-ec=bad-cookie")]);

        let ec = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");

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
        let cookie = format!("ts-ec={ec_id}");
        let req = create_test_request(&[("cookie", &cookie)]);

        let mut ec = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");
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
        let ec = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");
        assert_eq!(
            ec.existing_cookie_ec_id(),
            Some(cookie_ec.as_str()),
            "should return cookie EC ID"
        );

        // With only header (no cookie)
        let header_ec = valid_ec_id("f", "HdrVl1");
        let req = create_test_request(&[("x-ts-ec", &header_ec)]);
        let ec = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");
        assert!(
            ec.existing_cookie_ec_id().is_none(),
            "should return None when only header is present"
        );

        // With both header and cookie — should return cookie value
        let header_ec2 = valid_ec_id("a", "Hdr002");
        let cookie_ec2 = valid_ec_id("b", "Ck0002");
        let cookie2 = format!("ts-ec={cookie_ec2}");
        let req = create_test_request(&[("x-ts-ec", &header_ec2), ("cookie", &cookie2)]);
        let ec = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");
        assert_eq!(
            ec.ec_value(),
            Some(cookie_ec2.as_str()),
            "should use cookie as active EC"
        );
        assert_eq!(
            ec.existing_cookie_ec_id(),
            Some(cookie_ec2.as_str()),
            "should return cookie value for revocation even when header is present"
        );
    }
}
