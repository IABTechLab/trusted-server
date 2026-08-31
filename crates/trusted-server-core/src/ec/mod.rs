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
//! - [`consent`] — EC-specific consent gating wrapper
//! - [`cookies`] — `Set-Cookie` header creation and expiration helpers
//! - [`kv`] — KV Store identity graph operations (CAS, tombstones, debounce)
//! - [`kv_backend`] — Platform-neutral KV primitives implemented by adapters
//! - [`kv_types`] — Schema types for KV identity graph entries
//! - [`device`]: Device signal derivation (UA, JA4, H2 SETTINGS)
//! - [`partner`] — Partner validation helpers (ID format, pull sync config)
//! - [`registry`] — In-memory partner registry built from config
//! - [`rate_limiter`] — Rate limiting abstraction (implemented by adapters)
//! - [`identify`] — Identity read endpoint (`GET /_ts/api/v1/identify`)
//! - [`eids`] — Shared EID resolution and formatting helpers
//! - [`batch_sync`] — S2S batch sync endpoint (`POST /_ts/api/v1/batch-sync`)
//! - [`pull_sync`] — Background pull-sync dispatcher for organic routes

mod auth;

pub mod admin;
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
use crate::evidence::BorrowedRequestInfo;
use crate::geo::GeoInfo;
use crate::platform::RuntimeServices;
use crate::settings::Settings;
use device::DeviceSignals;
use provider::{EdgeCookieProvider, GeneratedEdgeCookie, IdentityInput};

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
    /// Whether Edge Cookie creation is allowed for this request. Resolved once
    /// at construction: closed when no provider is selected, otherwise from the
    /// request's consent context. Read via [`ec_allowed`](Self::ec_allowed).
    ec_allowed: bool,
    /// The normalized client IP, captured early before the request body
    /// is consumed. `None` when the platform cannot determine client IP.
    client_ip: Option<String>,
    /// Geo information captured pre-routing for downstream KV writes.
    geo_info: Option<GeoInfo>,
    /// Device signals derived from TLS/H2/UA in the adapter layer.
    /// Set via [`EcContext::set_device_signals`] before
    /// [`EcContext::generate_if_needed`] is called.
    device_signals: Option<DeviceSignals>,
    /// The selected Edge Cookie provider (built-in or injected), built once at
    /// construction. Core asks it whether an identifier is well formed
    /// ([`accepts_id`](crate::ec::provider::EdgeCookieProvider::accepts_id)) so
    /// an opaque vendor identifier round-trips through read-back and withdrawal
    /// instead of being dropped by the built-in shape check. `None` when no
    /// provider is configured.
    selected_provider: Option<Arc<dyn crate::ec::provider::EdgeCookieProvider>>,
    /// A snapshot of the request evidence a provider reads at generation time:
    /// the request headers (so a provider can read cookies and client hints), and
    /// the URL path and query string (so it can read request parameters).
    /// Captured once at construction, and only when a provider is configured and
    /// the request carries no usable identifier, so a no-provider deployment and
    /// a returning visitor both clone nothing. A provider reads these through
    /// [`RequestInfo`](crate::evidence::RequestInfo) at generate time.
    request_headers: http::HeaderMap,
    request_path: String,
    request_query: String,
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

        // Take the selected provider once. It is used here to decide whether
        // the incoming cookie value is a usable identifier, and again by
        // generation, which reuses this one rather than asking for another.
        // Resolving needs no request data, so an adapter that resolved the
        // selection while it built application state hands the same instance
        // back here and nothing is built a second time on this request.
        let selected_provider: Option<Arc<dyn crate::ec::provider::EdgeCookieProvider>> =
            provider::request_provider(&settings.ec, services)?;

        // Read back an existing identifier only when the selected provider
        // accepts its shape, so an opaque vendor identifier (for example a signed
        // envelope) round-trips instead of being silently dropped by the built-in
        // shape check. With no provider configured, Trusted Server is stateless:
        // an existing identifier is treated as absent so it is never used or
        // egressed, while the raw cookie value stays available to withdrawal
        // handling below.
        let ec_value = parsed.cookie_ec.clone().filter(|v| {
            selected_provider
                .as_ref()
                .is_some_and(|selected| provider::provider_owns_id(selected.as_ref(), v))
        });
        let ec_was_present = ec_value.is_some();

        if let Some(ref id) = ec_value {
            log::trace!("Existing EC ID found: {}", log_id(id));
        }

        // Snapshot the request evidence a provider reads at generation time (the
        // headers, so it can read cookies and client hints, and the URL path and
        // query, so it can read request parameters). Capture only when a provider
        // is configured and no identifier already exists, so a no-provider
        // deployment and a returning visitor clone nothing. Generation runs after
        // the request body may be consumed, so the snapshot is owned.
        let (request_headers, request_path, request_query) =
            if selected_provider.is_some() && ec_value.is_none() {
                (
                    req.headers().clone(),
                    req.uri().path().to_owned(),
                    req.uri().query().unwrap_or_default().to_owned(),
                )
            } else {
                (http::HeaderMap::new(), String::new(), String::new())
            };

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

        // Gate Edge Cookie creation and use on the request's consent context
        // (jurisdiction and consent signals). With no provider selected nothing
        // may create or use an identifier, so the gate is closed rather than
        // open by default. Downstream consumers read the stored result via
        // [`EcContext::ec_allowed`] rather than re-deriving it.
        let ec_allowed = selected_provider
            .as_ref()
            .is_some_and(|_| consent::ec_consent_granted(&consent));

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
            client_ip,
            geo_info: geo_info.cloned(),
            device_signals: None,
            selected_provider,
            request_headers,
            request_path,
            request_query,
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
    /// Forwards every error from `generate_with_provider`: the selected provider
    /// failing to derive an identifier (which includes a provider that needs the
    /// client IP being run on a host that cannot supply one), the provider
    /// producing an identifier outside the cookie-safe alphabet or over the
    /// length cap, the provider asking for a response header inside core's
    /// reserved surface, or persisting the identifier to the KV identity graph
    /// failing.
    pub fn generate_if_needed(
        &mut self,
        settings: &Settings,
        kv: Option<&KvIdentityGraph>,
    ) -> Result<(), Report<TrustedServerError>> {
        if self.ec_value.is_some() {
            return Ok(());
        }

        // A deployment with no provider selected is stateless: nothing to
        // generate, and not an error. Reuse the provider built at read time
        // rather than building it again.
        let Some(ec_provider) = self.selected_provider.clone() else {
            log::trace!("EC generation skipped: no Edge Cookie provider configured");
            return Ok(());
        };

        if !self.ec_allowed {
            log::info!(
                "EC generation skipped: EC creation not permitted (jurisdiction={})",
                self.consent.jurisdiction,
            );
            return Ok(());
        }

        // Whether the client IP is needed is the selected provider's decision,
        // not core's. A provider that derives identity from headers, cookies,
        // query parameters, or the client reads no IP and must still run on a
        // host that cannot supply one. The IP is passed as the documented
        // unavailable value, the empty string (see
        // [`RequestInfo::client_ip`](crate::evidence::RequestInfo::client_ip)),
        // and a provider that needs it refuses there, returning the error to
        // the caller. The publisher proxy and integration proxy log it and
        // serve the response without an Edge Cookie.
        self.generate_with_provider(ec_provider.as_ref(), settings, kv)
    }

    /// Derives and commits an EC identifier using a specific provider.
    ///
    /// Split out of [`generate_if_needed`](Self::generate_if_needed) so the
    /// provider is supplied explicitly, resolved once at read time and threaded
    /// here rather than rebuilt. The request evidence captured at read time
    /// (client IP, headers, and the URL path and query) is passed borrowed
    /// through [`RequestInfo`](crate::evidence::RequestInfo), so a provider can
    /// read cookies and request parameters at generate time; the built-in HMAC
    /// provider reads only the client IP. The skip guards (existing EC, consent
    /// gate) stay in [`generate_if_needed`](Self::generate_if_needed).
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::EdgeCookie`] when the provider fails to
    /// derive an identifier (which for [`HmacProvider`] includes an
    /// unavailable client IP), the provider
    /// asks for a response header inside core's reserved surface (see
    /// [`reserved_response_effect`](crate::ec::provider::reserved_response_effect)),
    /// or persisting a generated identifier to the KV identity graph fails.
    fn generate_with_provider(
        &mut self,
        ec_provider: &dyn EdgeCookieProvider,
        settings: &Settings,
        kv: Option<&KvIdentityGraph>,
    ) -> Result<(), Report<TrustedServerError>> {
        let input = IdentityInput {
            consent: Some(&self.consent),
        };
        // Pass the request evidence captured at read time, borrowed: the client
        // IP, the request headers (so a provider reads cookies and client hints),
        // and the URL path and query (so it reads request parameters). A built-in
        // provider reads only the client IP; a vendor provider reads what it
        // needs through [`RequestInfo`].
        let request_info = BorrowedRequestInfo::new(
            self.client_ip.as_deref().unwrap_or_default(),
            Some(&self.request_headers),
        )
        .with_request_target(&self.request_path, &self.request_query);
        let generated: GeneratedEdgeCookie = ec_provider.generate(&request_info, &input)?;
        // Check every response header the provider asked for against core's
        // reserved surface before any of them are kept. A provider may set its
        // own cookies and headers, but not a managed `ts-` cookie, a header in
        // the `x-ts-` namespace, or a framing or hop-by-hop header. Rejection
        // fails the request, matching the identifier-bounds rejection below:
        // without it a provider could write `ts-ec` itself and bypass the
        // identifier validation and identity-graph row this function enforces.
        // Checked before the identifier is read, because a provider can return
        // headers with no identifier at all.
        for (name, value) in &generated.response_headers {
            if let Some(effect) = provider::reserved_response_effect(name, value) {
                return Err(Report::new(TrustedServerError::EdgeCookie {
                    message: format!(
                        "Provider `{}` returned a response header `{name}` that {effect}",
                        ec_provider.id(),
                    ),
                }));
            }
        }
        // Capture any response headers the provider asked for, even when it
        // produced no identifier (for example while it still needs more client
        // evidence). EC finalization applies them to the response.
        self.response_headers = generated.response_headers;
        let generated_id = generated
            .id
            .map(|value| crate::ec::provider::apply_provider_code(ec_provider, &value));
        let Some(ec_id) = generated_id else {
            log::info!(
                "EC generation produced no identifier (provider={}); proceeding without an EC",
                ec_provider.id(),
            );
            return Ok(());
        };
        // Enforce the global identifier bounds at creation. The cookie-safe
        // alphabet and the length cap apply to every provider, so no
        // implementation can emit a value the cookie layer or the identity
        // graph cannot carry. Rejection is loud and total; the identifier is
        // never rewritten.
        if !ec_id_has_only_allowed_chars(&ec_id) {
            return Err(Report::new(TrustedServerError::EdgeCookie {
                message: format!(
                    "Provider `{}` produced an identifier that is empty, over {} bytes, or \
                     outside the cookie-safe alphabet",
                    ec_provider.id(),
                    cookies::MAX_EC_ID_LEN,
                ),
            }));
        }
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

            // Key the identity graph by the provider's canonical form of the
            // identifier, so equivalent representations of one identity share
            // one row. The built-in normalization lowercases only the HMAC
            // hash segment; an opaque vendor provider overrides it to the
            // identity function.
            let kv_key = crate::ec::provider::provider_kv_key(ec_provider, ec_value);
            if let Err(err) = graph.create_or_revive(&kv_key, &entry) {
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

    /// The providers whose identifiers this request's paths accept.
    ///
    /// Today that is the selected provider alone (see
    /// [`AcceptedProviders`](provider::AcceptedProviders) for the
    /// `legacy_providers` seam).
    #[must_use]
    pub(crate) fn accepted_providers(&self) -> provider::AcceptedProviders<'_> {
        provider::AcceptedProviders::active(self.selected_provider.as_deref())
    }

    /// Returns whether `value` is a well-formed identifier for the selected
    /// provider.
    ///
    /// Lets core validate a cookie or active identifier (for example before
    /// withdrawing it) through the provider that issued it, rather than assuming
    /// the built-in shape. The global cookie bounds are checked first, then the
    /// provider-specific part is dispatched by the identifier's code. Falls back
    /// to the built-in shape when no provider is configured.
    #[must_use]
    pub(crate) fn accepts_id(&self, value: &str) -> bool {
        self.accepted_providers().accepts(value)
    }

    /// The identity-graph key for `value` under the providers this deployment
    /// reads.
    ///
    /// The canonical route from an identifier to a row key. The organic
    /// generate, identify and finalize paths turn an identifier into a row key
    /// through this (or through [`ec_kv_key`](Self::ec_kv_key), which wraps it),
    /// so a provider whose canonical form differs from the cookie value still
    /// finds the row it created. The owning provider is picked by the
    /// identifier's `{code}~` prefix and supplies the canonical form of its own
    /// value part, matching what
    /// [`generate_if_needed`](Self::generate_if_needed) wrote at creation.
    ///
    /// Known gap: pull sync (`ec::pull_sync`) and the admin lookup
    /// (`ec::admin`) still key rows by the raw active identifier rather than
    /// this canonical form, so for a provider whose canonical form differs from
    /// the cookie value they can read or write under the wrong key. The three
    /// organic paths were routed through the canonical form (commit
    /// `343ac3e`); these two were left keying raw and are tracked as a known
    /// issue for a later change.
    ///
    /// `None` when no provider this deployment reads owns `value`, in which
    /// case there is no row to read or write.
    #[must_use]
    pub(crate) fn kv_key_for(&self, value: &str) -> Option<String> {
        self.accepted_providers().canonical_kv_key(value)
    }

    /// The identity-graph key for this request's active identifier.
    #[must_use]
    pub(crate) fn ec_kv_key(&self) -> Option<String> {
        self.ec_value().and_then(|value| self.kv_key_for(value))
    }

    /// The identity-graph key for the `ts-ec` cookie the request carried.
    ///
    /// Withdrawal tombstones the cookie's row as well as the active one,
    /// because a stateless deployment leaves [`ec_kv_key`](Self::ec_kv_key)
    /// empty while a live row still exists, and the cookie is the only way
    /// back to it.
    ///
    /// This does not reach across a provider switch. An identifier created
    /// under a retired provider's `{code}~` prefix is owned by no provider
    /// this deployment reads, so [`kv_key_for`](Self::kv_key_for) yields
    /// `None` and its row is never tombstoned. Core cannot derive that key,
    /// because the canonical form is the owning provider's own normalization.
    /// The browser cookie is still expired, since that path keys off the raw
    /// cookie rather than off ownership. See the provider-switching section
    /// of the pluggable providers design spec for what an operator has to do
    /// about it.
    #[must_use]
    pub(crate) fn cookie_ec_kv_key(&self) -> Option<String> {
        self.existing_cookie_ec_id()
            .and_then(|value| self.kv_key_for(value))
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
    /// context. Mutations do not re-derive [`ec_allowed`](Self::ec_allowed),
    /// which is resolved once at construction.
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

    /// Returns the pre-routing geo data, if available.
    #[must_use]
    pub fn geo_info(&self) -> Option<&GeoInfo> {
        self.geo_info.as_ref()
    }

    /// Returns whether Edge Cookie creation is allowed for this request.
    ///
    /// Resolved once at construction: closed when no provider is selected,
    /// otherwise from the request's consent context (see
    /// [`consent::ec_consent_granted`]).
    #[must_use]
    pub fn ec_allowed(&self) -> bool {
        self.ec_allowed
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

    /// Returns the stable EC hash prefix from the active EC value.
    #[must_use]
    pub fn ec_hash(&self) -> Option<&str> {
        self.ec_value.as_deref().map(generation::ec_hash)
    }

    /// Attaches a selected provider to a test-only [`EcContext`].
    ///
    /// The production constructor builds the provider from settings and
    /// injected services. A test that only needs the provider's identifier
    /// semantics (which identifiers it owns, and their canonical key form)
    /// takes this shortcut instead.
    #[cfg(test)]
    #[must_use]
    pub fn with_provider_for_test(
        mut self,
        provider: Arc<dyn crate::ec::provider::EdgeCookieProvider>,
    ) -> Self {
        self.selected_provider = Some(provider);
        self
    }

    /// Creates a test-only `EcContext` whose creation gate is derived from the
    /// consent context, matching the production construction path.
    ///
    /// Use [`new_for_test_gated`](Self::new_for_test_gated) when a test needs
    /// an explicit gate.
    #[cfg(test)]
    #[must_use]
    pub fn new_for_test(ec_value: Option<String>, consent: ConsentContext) -> Self {
        let ec_allowed = consent::ec_consent_granted(&consent);
        Self::new_for_test_gated(ec_value, consent, ec_allowed)
    }

    /// Creates a test-only `EcContext` with an explicit creation gate.
    ///
    /// `ec_allowed` stands in for the gating decision the production path
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
            client_ip: None,
            geo_info: None,
            device_signals: None,
            selected_provider: None,
            request_headers: http::HeaderMap::new(),
            request_path: String::new(),
            request_query: String::new(),
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
        let ec_allowed = consent::ec_consent_granted(&consent);
        Self {
            ec_was_present: ec_value.is_some(),
            cookie_ec_value: ec_value.clone(),
            ec_value,
            ec_generated: false,
            consent,
            ec_allowed,
            client_ip,
            geo_info: None,
            device_signals: None,
            selected_provider: None,
            request_headers: http::HeaderMap::new(),
            request_path: String::new(),
            request_query: String::new(),
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
            client_ip: None,
            geo_info: None,
            device_signals: None,
            selected_provider: None,
            request_headers: http::HeaderMap::new(),
            request_path: String::new(),
            request_query: String::new(),
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
    use crate::ec::provider::{EcProviderSelection, ProviderCode};
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

    /// A provider that records the `Cookie` header from the request info passed
    /// to `generate`, so a test can prove request cookies reach a provider (a
    /// client that stores values in cookies relies on this).
    #[derive(Debug)]
    struct CookieCapturingProvider {
        seen_cookie: std::sync::Mutex<Option<String>>,
    }

    impl EdgeCookieProvider for CookieCapturingProvider {
        fn id(&self) -> &'static str {
            "cookie-capturing"
        }

        fn code(&self) -> ProviderCode {
            crate::provider_code!("t0cc")
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
        // values in cookies relies on this). The organic generate path passes a
        // snapshot of the request headers through generate_with_provider, so a
        // provider reads request cookies through it there too; this test
        // supplies its own headers directly.
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

    /// A provider whose identifiers are opaque and deliberately not the
    /// built-in HMAC shape (no dot, mixed case), modeling a vendor identifier
    /// such as a signed envelope. It accepts any of its own non-empty
    /// identifiers.
    #[derive(Debug)]
    struct OpaqueIdProvider;

    impl EdgeCookieProvider for OpaqueIdProvider {
        fn id(&self) -> &'static str {
            "opaque"
        }

        fn code(&self) -> ProviderCode {
            crate::provider_code!("t0op")
        }

        fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(GeneratedEdgeCookie::default())
        }

        fn accepts_id(&self, value: &str) -> bool {
            !value.is_empty()
        }
    }

    /// A geo that resolves to the non-regulated jurisdiction (US, no region),
    /// so the consent gate is open and generation runs in provider tests.
    fn non_regulated_geo() -> GeoInfo {
        GeoInfo {
            city: String::new(),
            country: "US".to_owned(),
            continent: "NorthAmerica".to_owned(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: None,
            asn: None,
        }
    }

    #[test]
    fn read_from_request_reuses_the_provider_the_composition_root_resolved() {
        // Reading EC state runs on every request, and it used to resolve
        // `[ec] provider` itself even though the composition root had just
        // resolved the same settings, so the provider was built twice per
        // request. An adapter now threads the resolved provider through
        // `RuntimeServices`, and the context has to take that instance.
        let mut settings = create_test_settings();
        settings.ec.provider = Some(EcProviderSelection::from("opaque"));

        let ec_config = settings.ec.clone();
        let resolved = crate::ec::provider::build_shared_provider(
            &ec_config,
            Some(Arc::new(OpaqueIdProvider)),
        )
        .expect("the composition root should resolve the selection")
        .expect("the selection should yield a provider");

        let services = crate::platform::test_support::noop_services_with_resolved_ec_provider(
            Arc::clone(&resolved),
        );
        let req = create_test_request(&[]);
        let ec = EcContext::read_from_request(&settings, &req, &services)
            .expect("should read EC context");

        let used = ec
            .selected_provider
            .as_ref()
            .expect("the context should hold the selected provider");
        assert!(
            Arc::ptr_eq(used, &resolved),
            "reading EC state should reuse the provider resolved at startup rather \
             than building a second one for this request"
        );
    }

    #[test]
    fn read_from_request_round_trips_an_opaque_provider_identifier() {
        use crate::platform::test_support::noop_services_with_ec_provider;

        // A vendor identifier that is deliberately not the built-in HMAC shape
        // (no dot, mixed case), the exact value the built-in check would drop.
        const OPAQUE_ID: &str = "AbC123opaqueEnvelopeValueXYZ";
        const CODED_ID: &str = "t0op~AbC123opaqueEnvelopeValueXYZ";

        let mut settings = create_test_settings();
        settings.ec.provider = Some(EcProviderSelection::from("opaque"));
        let cookie = format!("ts-ec={CODED_ID}");
        let req = create_test_request(&[("cookie", &cookie)]);

        // With the opaque provider injected, its `accepts_id` governs read-back,
        // so the identifier survives verbatim.
        let services = noop_services_with_ec_provider(Arc::new(OpaqueIdProvider));
        let ec = EcContext::read_from_request(&settings, &req, &services)
            .expect("should read EC context");
        assert_eq!(
            ec.ec_value(),
            Some(CODED_ID),
            "an opaque provider identifier should round-trip through read-back verbatim"
        );
        let _ = OPAQUE_ID;

        // Control: with the provider selected but not injected by the adapter,
        // the request fails loudly instead of silently running stateless with
        // the identifier dropped.
        let err = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect_err("a selected but uninjected provider should fail the request");
        assert!(
            err.to_string().contains("opaque"),
            "the error should name the selected provider, got: {err}"
        );

        // Control: with no provider selected at all, the identifier is treated
        // as absent, so a stateless deployment never uses or egresses it.
        let mut stateless = create_test_settings();
        stateless.ec.provider = None;
        stateless.ec.providers.hmac = None;
        let ec_without = EcContext::read_from_request(&stateless, &req, &noop_services())
            .expect("should read EC context");
        assert_eq!(
            ec_without.ec_value(),
            None,
            "with no provider selected, an existing identifier is treated as absent"
        );
        assert!(
            !ec_without.ec_allowed(),
            "with no provider selected, the gate stays closed"
        );
    }

    /// A provider that records the request query parameter `id` and the `Cookie`
    /// header it is given at generate time, proving request evidence (parameters
    /// and cookies) reaches a provider through the organic generate path.
    #[derive(Debug, Default)]
    struct EvidenceCapturingProvider {
        seen: std::sync::Mutex<Option<(String, String)>>,
    }

    impl EdgeCookieProvider for EvidenceCapturingProvider {
        fn id(&self) -> &'static str {
            "evidence"
        }

        fn code(&self) -> ProviderCode {
            crate::provider_code!("t0ev")
        }

        fn generate(
            &self,
            request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            let query_id = request_info.query_param("id").unwrap_or_default();
            let cookie = request_info.header("cookie").unwrap_or_default().to_owned();
            *self.seen.lock().expect("should lock seen evidence") = Some((query_id, cookie));
            Ok(GeneratedEdgeCookie {
                id: Some("evidence-ec".to_owned()),
                response_headers: Vec::new(),
            })
        }

        fn accepts_id(&self, value: &str) -> bool {
            !value.is_empty()
        }
    }

    #[test]
    fn generate_passes_request_parameters_and_cookies_to_the_provider() {
        use crate::platform::test_support::noop_services_with_ec_provider;

        let provider = Arc::new(EvidenceCapturingProvider::default());
        let mut settings = create_test_settings();
        settings.ec.provider = Some(EcProviderSelection::from("evidence"));

        // A request carrying a query parameter and a (non-EC) cookie, with no
        // existing `ts-ec` cookie so the generate path runs.
        let req = Request::builder()
            .method("GET")
            .uri("http://example.com/page?id=abc123&debug=1")
            .header("cookie", "client-id=xyz789")
            .body(EdgeBody::empty())
            .expect("should build request");

        let services = noop_services_with_ec_provider(provider.clone());
        let geo = non_regulated_geo();
        let mut ec = EcContext::read_from_request_with_geo(&settings, &req, &services, Some(&geo))
            .expect("should read EC context");
        ec.generate_if_needed(&settings, None)
            .expect("should run generation");

        let seen = provider
            .seen
            .lock()
            .expect("should lock seen evidence")
            .clone();
        assert_eq!(
            seen,
            Some(("abc123".to_owned(), "client-id=xyz789".to_owned())),
            "the provider should read the request query parameter and cookies at generate time"
        );
        assert_eq!(
            ec.ec_value(),
            Some("t0ev~evidence-ec"),
            "the identifier the provider created should be committed under its code"
        );
    }

    /// A provider that creates an opaque, mixed-case, non-HMAC identifier at
    /// the edge, so a test can prove such an identifier persists to the KV identity
    /// graph under its own value as the key.
    #[derive(Debug)]
    struct ServerOpaqueProvider;

    impl EdgeCookieProvider for ServerOpaqueProvider {
        fn id(&self) -> &'static str {
            "server-opaque"
        }

        fn code(&self) -> ProviderCode {
            crate::provider_code!("t0so")
        }

        fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(GeneratedEdgeCookie {
                id: Some("Opaque_EC_Value_MixedCase_123".to_owned()),
                response_headers: Vec::new(),
            })
        }

        fn accepts_id(&self, value: &str) -> bool {
            !value.is_empty()
        }

        fn normalize_id_for_kv(&self, value: &str) -> String {
            value.to_owned()
        }
    }

    #[test]
    fn generate_persists_an_opaque_identifier_to_kv_under_its_own_key() {
        use crate::platform::test_support::noop_services_with_ec_provider;

        const OPAQUE: &str = "t0so~Opaque_EC_Value_MixedCase_123";

        let mut settings = create_test_settings();
        settings.ec.provider = Some(EcProviderSelection::from("server-opaque"));
        let services = noop_services_with_ec_provider(Arc::new(ServerOpaqueProvider));
        let graph = KvIdentityGraph::in_memory("test-ec-store");

        // No existing cookie, so the edge creates one and persists it.
        let req = create_test_request(&[]);
        let geo = non_regulated_geo();
        let mut ec = EcContext::read_from_request_with_geo(&settings, &req, &services, Some(&geo))
            .expect("should read EC context");
        ec.generate_if_needed(&settings, Some(&graph))
            .expect("should generate and persist");

        assert_eq!(
            ec.ec_value(),
            Some(OPAQUE),
            "the opaque identifier should be created"
        );

        // The entry is stored under the full identifier verbatim.
        assert!(
            graph.get(OPAQUE).expect("kv get should succeed").is_some(),
            "the entry should exist under the opaque identifier key"
        );

        // A lowercased key must miss, proving the key preserves case rather than
        // being lowercased like the built-in HMAC form (the clash this guards).
        assert!(
            graph
                .get(&OPAQUE.to_lowercase())
                .expect("kv get should succeed")
                .is_none(),
            "the KV key must be case-sensitive and verbatim, not lowercased"
        );
    }

    /// A provider that needs no client IP, recording the value it was given so
    /// a test can prove a host that cannot determine one passes the documented
    /// unavailable value rather than failing the request.
    #[derive(Debug, Default)]
    struct NoClientIpProvider {
        seen_client_ip: std::sync::Mutex<Option<String>>,
    }

    impl EdgeCookieProvider for NoClientIpProvider {
        fn id(&self) -> &'static str {
            "no-client-ip"
        }

        fn code(&self) -> ProviderCode {
            crate::provider_code!("t0ni")
        }

        fn generate(
            &self,
            request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            *self
                .seen_client_ip
                .lock()
                .expect("should lock the seen client IP") =
                Some(request_info.client_ip().to_owned());
            Ok(GeneratedEdgeCookie {
                id: Some("no-ip-ec".to_owned()),
                response_headers: Vec::new(),
            })
        }

        fn accepts_id(&self, value: &str) -> bool {
            !value.is_empty()
        }
    }

    #[test]
    fn a_provider_that_reads_no_client_ip_mints_when_the_host_has_none() {
        use crate::platform::test_support::noop_services_with_ec_provider_without_client_ip;

        // The requirement for a client IP belongs to the provider that uses
        // one, not to core. A provider deriving identity from the request
        // query and cookies runs on a host that cannot determine a client IP,
        // and receives the documented unavailable value, the empty string.
        let provider = Arc::new(EvidenceCapturingProvider::default());
        let mut settings = create_test_settings();
        settings.ec.provider = Some(EcProviderSelection::from("evidence"));
        let req = Request::builder()
            .method("GET")
            .uri("http://example.com/page?id=abc123")
            .header("cookie", "client-id=xyz789")
            .body(EdgeBody::empty())
            .expect("should build request");

        let services = noop_services_with_ec_provider_without_client_ip(provider.clone());
        let geo = non_regulated_geo();
        let mut ec = EcContext::read_from_request_with_geo(&settings, &req, &services, Some(&geo))
            .expect("should read EC context");
        assert_eq!(
            ec.client_ip(),
            None,
            "the host should supply no client IP in this test"
        );

        ec.generate_if_needed(&settings, None)
            .expect("a provider that reads no client IP should still create an identifier");
        assert_eq!(
            ec.ec_value(),
            Some("t0ev~evidence-ec"),
            "the identifier should be committed with no client IP available"
        );
    }

    #[test]
    fn the_hmac_provider_refuses_when_the_host_has_no_client_ip() {
        // The other half: the built-in provider's only input is the client IP,
        // so with none it fails rather than hashing the empty string into an
        // identifier every visitor on that host would share. Identity cannot be
        // established, so generate_if_needed returns the error, which the
        // publisher and integration proxies log before serving the response
        // without an Edge Cookie.
        let settings = create_test_settings();
        let req = create_test_request(&[]);
        let geo = non_regulated_geo();
        let mut ec =
            EcContext::read_from_request_with_geo(&settings, &req, &noop_services(), Some(&geo))
                .expect("should read EC context");
        assert_eq!(
            ec.client_ip(),
            None,
            "the host should supply no client IP in this test"
        );

        let err = ec
            .generate_if_needed(&settings, None)
            .expect_err("the HMAC provider should refuse without a client IP");
        assert!(
            err.to_string().contains("client IP"),
            "the error should name the missing client IP, got: {err}"
        );
        assert_eq!(
            ec.ec_value(),
            None,
            "no identifier should be committed when the provider refuses"
        );
    }

    /// A provider that creates an identifier outside the cookie-safe alphabet,
    /// to prove core rejects it at creation rather than rewriting it.
    #[derive(Debug)]
    struct IllegalIdProvider;

    impl EdgeCookieProvider for IllegalIdProvider {
        fn id(&self) -> &'static str {
            "illegal"
        }

        fn code(&self) -> ProviderCode {
            crate::provider_code!("t0il")
        }

        fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(GeneratedEdgeCookie {
                id: Some("bad;value with spaces".to_owned()),
                response_headers: Vec::new(),
            })
        }

        fn accepts_id(&self, _value: &str) -> bool {
            true
        }
    }

    #[test]
    fn generate_rejects_an_identifier_outside_the_cookie_safe_alphabet() {
        use crate::platform::test_support::noop_services_with_ec_provider;

        let mut settings = create_test_settings();
        settings.ec.provider = Some(EcProviderSelection::from("illegal"));
        let services = noop_services_with_ec_provider(Arc::new(IllegalIdProvider));
        let req = create_test_request(&[]);
        let geo = non_regulated_geo();
        let mut ec = EcContext::read_from_request_with_geo(&settings, &req, &services, Some(&geo))
            .expect("should read EC context");

        let err = ec
            .generate_if_needed(&settings, None)
            .expect_err("an identifier outside the alphabet should be rejected at creation");
        assert!(
            err.to_string().contains("illegal"),
            "the error should name the provider, got: {err}"
        );
        assert_eq!(
            ec.ec_value(),
            None,
            "no identifier should be committed after a creation rejection"
        );
    }

    /// A provider that returns a caller-chosen response header and no
    /// identifier, so a test can drive one provider response effect at a time
    /// through the organic generate path.
    #[derive(Debug)]
    struct HeaderSettingProvider {
        name: &'static str,
        value: &'static str,
        mint: bool,
    }

    impl EdgeCookieProvider for HeaderSettingProvider {
        fn id(&self) -> &'static str {
            "header-setting"
        }

        fn code(&self) -> ProviderCode {
            crate::provider_code!("t0hs")
        }

        fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(GeneratedEdgeCookie {
                id: self.mint.then(|| "provider-value".to_owned()),
                response_headers: vec![(
                    http::HeaderName::from_bytes(self.name.as_bytes())
                        .expect("should parse header name"),
                    http::HeaderValue::from_static(self.value),
                )],
            })
        }

        fn accepts_id(&self, value: &str) -> bool {
            !value.is_empty()
        }

        fn normalize_id_for_kv(&self, value: &str) -> String {
            value.to_owned()
        }
    }

    fn generate_with_header_setting_provider(
        provider: HeaderSettingProvider,
        graph: Option<&KvIdentityGraph>,
    ) -> (Settings, Result<EcContext, Report<TrustedServerError>>) {
        use crate::platform::test_support::noop_services_with_ec_provider;

        let mut settings = create_test_settings();
        settings.ec.provider = Some(EcProviderSelection::from("header-setting"));
        let services = noop_services_with_ec_provider(Arc::new(provider));
        let req = create_test_request(&[]);
        let geo = non_regulated_geo();
        let mut ec = EcContext::read_from_request_with_geo(&settings, &req, &services, Some(&geo))
            .expect("should read EC context");
        let outcome = ec.generate_if_needed(&settings, graph).map(|()| ec);
        (settings, outcome)
    }

    #[test]
    fn generate_rejects_a_provider_effect_inside_the_reserved_response_surface() {
        // A provider that sets the managed `ts-ec` cookie would bypass core's
        // identifier validation and its identity-graph row entirely, so the
        // request fails rather than the effect being quietly dropped. The
        // provider creates no identifier here, which is exactly the case the
        // cookie write would otherwise slip through.
        let (_settings, outcome) = generate_with_header_setting_provider(
            HeaderSettingProvider {
                name: "set-cookie",
                value: "ts-ec=forged-value; Path=/",
                mint: false,
            },
            None,
        );

        let err = outcome.expect_err("a managed cookie effect should fail the request");
        assert!(
            err.to_string().contains("header-setting"),
            "the error should name the provider, got: {err}"
        );

        // The same for the reserved header namespace and for message framing.
        for (name, value) in [("x-ts-ec", "forged"), ("transfer-encoding", "chunked")] {
            let (_settings, outcome) = generate_with_header_setting_provider(
                HeaderSettingProvider {
                    name,
                    value,
                    mint: false,
                },
                None,
            );
            assert!(
                outcome.is_err(),
                "`{name}` is reserved and should fail the request"
            );
        }
    }

    #[test]
    fn generate_applies_a_provider_owned_cookie_to_the_response() {
        // The other half of the rule: a provider's own cookie is not core's, so
        // it survives generation and reaches the browser response unchanged,
        // alongside the managed `ts-ec` cookie core writes itself.
        let graph = KvIdentityGraph::in_memory("test-ec-store");
        let (settings, outcome) = generate_with_header_setting_provider(
            HeaderSettingProvider {
                name: "set-cookie",
                value: "acme-evidence=abc123; Path=/; Secure",
                mint: true,
            },
            Some(&graph),
        );
        let ec = outcome.expect("a provider-owned cookie should not fail the request");
        assert_eq!(
            ec.ec_value(),
            Some("t0hs~provider-value"),
            "the identifier should still be committed"
        );

        let mut response = http::Response::builder()
            .status(200)
            .body(EdgeBody::empty())
            .expect("should build test response");
        finalize::ec_finalize_response(
            &settings,
            &ec,
            Some(&graph),
            &registry::PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        let cookies: Vec<&str> = response
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect();
        assert!(
            cookies
                .iter()
                .any(|cookie| cookie.starts_with("acme-evidence=abc123")),
            "the provider's own cookie should reach the response, got: {cookies:?}"
        );
        assert!(
            cookies.iter().any(|cookie| cookie.starts_with("ts-ec=")),
            "core's own managed cookie should still be written, got: {cookies:?}"
        );
    }

    /// A provider whose identifier normalizes to a distinct canonical form, to
    /// prove the identity graph is keyed by the canonical form.
    ///
    /// Shared with the identify and finalization tests, which need a provider
    /// whose canonical key is not the value the browser carries.
    #[derive(Debug)]
    pub(crate) struct CanonicalizingProvider;

    impl EdgeCookieProvider for CanonicalizingProvider {
        fn id(&self) -> &'static str {
            "canonical"
        }

        fn code(&self) -> ProviderCode {
            crate::provider_code!("t0ca")
        }

        fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(GeneratedEdgeCookie {
                id: Some("MiXeD.CaseId".to_owned()),
                response_headers: Vec::new(),
            })
        }

        fn accepts_id(&self, _value: &str) -> bool {
            true
        }

        fn normalize_id_for_kv(&self, value: &str) -> String {
            value.to_ascii_lowercase()
        }
    }

    #[test]
    fn generate_keys_the_identity_graph_by_the_normalized_identifier() {
        use crate::platform::test_support::noop_services_with_ec_provider;

        let mut settings = create_test_settings();
        settings.ec.provider = Some(EcProviderSelection::from("canonical"));
        let services = noop_services_with_ec_provider(Arc::new(CanonicalizingProvider));
        let graph = KvIdentityGraph::in_memory("test-ec-store");
        let req = create_test_request(&[]);
        let geo = non_regulated_geo();
        let mut ec = EcContext::read_from_request_with_geo(&settings, &req, &services, Some(&geo))
            .expect("should read EC context");
        ec.generate_if_needed(&settings, Some(&graph))
            .expect("should generate and persist");

        assert_eq!(
            ec.ec_value(),
            Some("t0ca~MiXeD.CaseId"),
            "the cookie value keeps the provider's exact identifier under its code"
        );
        assert!(
            graph
                .get("t0ca~mixed.caseid")
                .expect("should read the graph")
                .is_some(),
            "the graph row should be keyed by the code plus the canonical form"
        );
        // Pin the read-side derivation to the key generation actually wrote.
        // Identify, the withdrawal tombstones, and EID ingestion all read the
        // row through `ec_kv_key`, so the two must never drift apart.
        assert_eq!(
            ec.ec_kv_key().as_deref(),
            Some("t0ca~mixed.caseid"),
            "the read-side key should be the key generation wrote"
        );
    }

    #[test]
    fn hmac_mints_a_coded_identifier_and_dual_reads_the_legacy_bare_form() {
        let settings = create_test_settings();
        let req = create_test_request(&[]);
        let geo = non_regulated_geo();
        let services = crate::platform::test_support::noop_services_with_client_ip(
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 7)),
        );
        let mut ec = EcContext::read_from_request_with_geo(&settings, &req, &services, Some(&geo))
            .expect("should read EC context");
        ec.generate_if_needed(&settings, None)
            .expect("should generate");
        let created = ec.ec_value().expect("should create an identifier");
        assert!(
            created.starts_with("hmac~"),
            "a fresh HMAC identifier should carry the hmac code, got {created}"
        );

        // A deployed pre-envelope cookie (bare form) still reads back, so the
        // migration does not orphan existing identities.
        let legacy = format!("{}.ABC123", "a".repeat(64));
        let cookie = format!("ts-ec={legacy}");
        let req = create_test_request(&[("cookie", &cookie)]);
        let ec = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");
        assert_eq!(
            ec.ec_value(),
            Some(legacy.as_str()),
            "the legacy bare form should dual-read under the hmac provider"
        );
    }

    #[test]
    fn a_foreign_provider_code_is_treated_as_absent() {
        // An identifier carrying another provider's code must never be adopted
        // by the selected provider, so switching providers cannot silently mix
        // identity populations.
        let settings = create_test_settings();
        let foreign = format!("zz00~{}.ABC123", "a".repeat(64));
        let cookie = format!("ts-ec={foreign}");
        let req = create_test_request(&[("cookie", &cookie)]);
        let ec = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");
        assert_eq!(
            ec.ec_value(),
            None,
            "an identifier with a foreign provider code is not this provider's"
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
