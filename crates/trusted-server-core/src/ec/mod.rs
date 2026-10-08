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
//! - [`admin`]: operator endpoints that show the stored identity state
//!   (`GET /_ts/admin/ec`)
//! - [`generation`] — HMAC-based ID generation, IP normalization, format helpers
//! - [`consent`]: EC-specific permission gating, with consent as one input
//! - [`cookies`] — `Set-Cookie` header creation and expiration helpers
//! - [`finalize`]: the cookie and identity-graph writes made on the response
//!   after routing
//! - [`kv`] — KV Store identity graph operations (CAS, tombstones, debounce)
//! - [`kv_backend`] — Platform-neutral KV primitives implemented by adapters
//! - [`kv_types`] — Schema types for KV identity graph entries
//! - [`device`]: Device signal derivation (UA, JA4, H2 SETTINGS)
//! - [`partner`] — Partner validation helpers (ID format, pull sync config)
//! - [`module`]: Edge Cookie modules and their selection
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
pub mod module;
pub mod partner;
pub mod prebid_eids;
pub mod pull_sync;
pub(crate) mod pull_sync_marker;
pub mod rate_limiter;
pub mod registry;
pub mod resolve;

/// Characters of an identifier kept when redacting it for a log.
const LOG_ID_PREFIX_CHARS: usize = 8;

/// Truncates an EC ID for safe inclusion in log messages.
///
/// Returns the first [`LOG_ID_PREFIX_CHARS`] characters followed by `…` to aid
/// debugging without writing the full user identifier to logs (satisfies the
/// `CodeQL` "cleartext logging of sensitive information" rule).
#[must_use]
pub fn log_id(ec_id: &str) -> String {
    // Truncated by character, not by byte. A byte index that lands inside a
    // multi-byte character makes `get` return `None`, and falling back to the
    // whole value would print in full the identifier this exists to redact.
    let prefix: String = ec_id.chars().take(LOG_ID_PREFIX_CHARS).collect();
    format!("{prefix}\u{2026}")
}

use std::sync::Arc;

use cookie::CookieJar;
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::Request;

use crate::consent::jurisdiction::Jurisdiction;
use crate::consent::{self as consent_mod, ConsentContext, ConsentPipelineInput};
use crate::constants::{COOKIE_TS_EC, COOKIE_TS_EC_PULL_COMPLETE};
use crate::cookies::handle_request_cookies;
use crate::ec::cookies::ec_id_has_only_allowed_chars;
use crate::error::TrustedServerError;
use crate::evidence::BorrowedRequestInfo;
use crate::geo::GeoInfo;
use crate::permissions::{Permission, PermissionState};
use crate::platform::RuntimeServices;
use crate::settings::Settings;
use device::DeviceSignals;
use module::{EdgeCookieModule, GeneratedEdgeCookie, IdentityInput};

use self::kv::{CreateIfAbsentOutcome, KvIdentityGraph};
use self::kv_types::KvEntry;
use self::pull_sync_marker::{PullSyncMarkerState, validate_marker_state};

/// Bounded request classifications that may persist browser EID cookies.
///
/// Adapters classify publisher navigations and `POST /auction` only after
/// pre-route filters allow dispatch. The shared page-bids handler classifies an
/// admitted SPA navigation, while EC finalization classifies new identities.
/// Challenged or blocked requests remain unclassified and cannot persist EIDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub enum EidSyncSource {
    /// Publisher top-level document navigation.
    #[display("navigation")]
    Navigation,
    /// `POST /auction` request.
    #[display("auction")]
    Auction,
    /// Admitted `GET /_ts/page-bids` SPA navigation.
    #[display("page_bids")]
    PageBids,
    /// Request that generated a new EC identity during finalization.
    #[display("new_ec")]
    NewEc,
}

/// Request-scoped view of one EC identity-graph lookup.
///
/// The state distinguishes an authoritative miss from a store failure and
/// binds persisted entry data to the EC ID that was actually read or written.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum EcKvSnapshot {
    /// No identity-graph lookup has been attempted for this request.
    #[default]
    NotRead,
    /// The store authoritatively reported that this EC ID does not exist.
    Missing { ec_id: String },
    /// Persisted entry data, optionally with a generation usable for CAS.
    ///
    /// A generation never authorizes a write by itself. Callers must first
    /// enforce entry policy such as rejecting a withdrawal tombstone.
    Present {
        ec_id: String,
        entry: Box<KvEntry>,
        generation: Option<u64>,
    },
    /// The lookup failed, so absence is not authoritative.
    Failed { ec_id: String },
}

impl EcKvSnapshot {
    /// Returns whether this state was produced for `ec_id`.
    #[must_use]
    pub fn belongs_to(&self, ec_id: &str) -> bool {
        match self {
            Self::NotRead => false,
            Self::Missing { ec_id: snapshot_id }
            | Self::Present {
                ec_id: snapshot_id, ..
            }
            | Self::Failed { ec_id: snapshot_id } => snapshot_id == ec_id,
        }
    }

    /// Returns the persisted entry only when the snapshot belongs to `ec_id`.
    #[must_use]
    pub fn entry_for(&self, ec_id: &str) -> Option<&KvEntry> {
        match self {
            Self::Present {
                ec_id: snapshot_id,
                entry,
                ..
            } if snapshot_id == ec_id => Some(entry.as_ref()),
            _ => None,
        }
    }

    /// Returns a usable CAS generation only when the snapshot belongs to `ec_id`.
    #[must_use]
    pub fn generation_for(&self, ec_id: &str) -> Option<u64> {
        match self {
            Self::Present {
                ec_id: snapshot_id,
                generation,
                ..
            } if snapshot_id == ec_id => *generation,
            _ => None,
        }
    }
}

pub use generation::{
    ec_hash, generate_ec_id, is_valid_ec_hash, is_valid_ec_id, normalize_ec_id_for_kv,
};

/// Parsed EC identity from an incoming request.
struct RequestEc {
    /// EC ID from the `ts-ec` cookie, if present.
    cookie_ec: Option<String>,
    /// Pull-sync completeness marker, if present.
    pull_sync_marker: Option<String>,
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
    let pull_sync_marker = jar
        .as_ref()
        .and_then(|j| j.get(COOKIE_TS_EC_PULL_COMPLETE))
        .map(cookie::Cookie::value)
        .map(str::to_owned);

    Ok(RequestEc {
        cookie_ec,
        pull_sync_marker,
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
    /// Whether the configured Edge Cookie module's required permissions are
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
    /// The selected Edge Cookie module (built-in or injected), built once at
    /// construction. Core asks it whether an identifier is well formed
    /// ([`accepts_id`](crate::ec::module::EdgeCookieModule::accepts_id)) so
    /// an opaque vendor identifier round-trips through read-back and withdrawal
    /// instead of being dropped by the built-in shape check. `None` when no
    /// module is configured.
    selected_module: Option<Arc<dyn crate::ec::module::EdgeCookieModule>>,
    /// A snapshot of the request evidence a module reads when it creates an
    /// identifier, at generation or in orphan recovery: the request headers (so
    /// a module can read cookies and client hints), and the URL path and query
    /// string (so it can read request parameters). Captured once at
    /// construction, when a module is configured and the request either
    /// carries no usable identifier or is a document navigation, the only
    /// request that can recover an orphaned one. So a no-module deployment
    /// and a returning visitor's subresource requests clone nothing. A module
    /// reads these through [`RequestInfo`](crate::evidence::RequestInfo).
    request_headers: http::HeaderMap,
    request_path: String,
    request_query: String,
    /// Response headers a module asked to set, captured when it creates an
    /// identifier (in [`EcContext::generate_if_needed`], or during orphan
    /// recovery in EC finalization) and applied to the response by EC
    /// finalization. Empty for modules that set no headers.
    response_headers: Vec<(http::HeaderName, http::HeaderValue)>,
    /// Request-scoped persisted identity-graph state for the active EC ID.
    kv_snapshot: EcKvSnapshot,
    /// Whether this request may rotate an orphaned EC identity.
    recovery_eligible: bool,
    /// Browser-carried proof of recent pull-partner completeness.
    pull_sync_marker: PullSyncMarkerState,
    /// Allowed returning-user EID persistence source, assigned only after request filters pass.
    eid_sync_source: Option<EidSyncSource>,
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
        Self::read_from_request_with_geo_status(
            settings,
            req,
            services,
            consent::GeoStatus::from(geo_info),
        )
    }

    /// Reads the EC context, resolving the location through the configured geo
    /// module first.
    ///
    /// This is the constructor adapters use: it runs the geo lookup itself so
    /// a failed lookup is distinguished from "no location resolved". No
    /// location falls back to the permission policy's top node, while a
    /// failure resolves every permission to the requires-signal floor (see
    /// [`consent::GeoStatus`]) and is logged at error level so an outage is
    /// visible.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError`] when the selected Edge Cookie module
    /// cannot be built, the same as
    /// [`read_from_request_with_geo`](Self::read_from_request_with_geo).
    pub fn read_from_request_resolving_geo(
        settings: &Settings,
        req: &Request<EdgeBody>,
        services: &RuntimeServices,
    ) -> Result<Self, Report<TrustedServerError>> {
        let lookup = services.geo().lookup(services.client_info().client_ip);
        let geo_info = match &lookup {
            Ok(info) => info.clone(),
            Err(error) => {
                log::error!(
                    "geo lookup failed; resolving permissions at the requires-signal floor: {error:?}"
                );
                None
            }
        };
        let status = match (&lookup, &geo_info) {
            (Err(_), _) => consent::GeoStatus::Failed,
            (Ok(_), Some(info)) => consent::GeoStatus::Located(info),
            (Ok(_), None) => consent::GeoStatus::NoLocation,
        };
        Self::read_from_request_with_geo_status(settings, req, services, status)
    }

    fn read_from_request_with_geo_status(
        settings: &Settings,
        req: &Request<EdgeBody>,
        services: &RuntimeServices,
        geo_status: consent::GeoStatus<'_>,
    ) -> Result<Self, Report<TrustedServerError>> {
        let geo_info = geo_status.info();
        let parsed = parse_ec_from_request(req)?;

        // Take the selected module once. It is used here to decide whether
        // the incoming cookie value is a usable identifier, to read the
        // module's required permissions, and again by generation, which
        // reuses this one rather than asking for another. `request_module`
        // hands back the instance the composition root resolved when there is
        // one, and otherwise resolves the selection from this request's own
        // services, the host signals among them, so a module built from
        // request evidence reads this request's evidence. A module that needs
        // a service the host did not supply fails to resolve, which stops the
        // request.
        let selected_module: Option<Arc<dyn crate::ec::module::EdgeCookieModule>> =
            module::request_module(&settings.ec, services)?;

        // Read back an existing identifier only when the selected module
        // accepts its shape, so an opaque vendor identifier (for example a signed
        // envelope) round-trips instead of being silently dropped by the built-in
        // shape check. With no module configured, Trusted Server is stateless:
        // an existing identifier is treated as absent so it is never used or
        // egressed, while the raw cookie value stays available to withdrawal
        // handling below.
        let ec_value = parsed.cookie_ec.clone().filter(|v| {
            selected_module
                .as_ref()
                .is_some_and(|selected| module::module_owns_id(selected.as_ref(), v))
        });
        let ec_was_present = ec_value.is_some();

        if let Some(ref id) = ec_value {
            log::trace!("Existing EC ID found: {}", log_id(id));
        }

        // Snapshot the request evidence a module reads when it creates an
        // identifier (the headers, so it can read cookies and client hints, and
        // the URL path and query, so it can read request parameters). Capture
        // only when a module is configured and either no identifier exists or
        // the request is a document navigation, which orphan recovery may need,
        // so a no-module deployment and a returning visitor's subresource
        // requests clone nothing. Creation runs after the request body may be
        // consumed, so the snapshot is owned.
        let (request_headers, request_path, request_query) = if selected_module.is_some()
            && (ec_value.is_none() || crate::http_util::is_navigation_request(req))
        {
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
        // Jurisdiction detection follows the permission model's fallback: with
        // no location resolved the policy's declared jurisdiction stands in, so
        // a deployment that declared one is not treated as unknown, while a
        // failed lookup stays unknown so the consent gates fail closed
        // alongside the requires-signal floor.
        let consent = consent_mod::build_consent_context(&ConsentPipelineInput {
            jar: parsed.jar.as_ref(),
            req,
            config: &settings.consent,
            geo: geo_info,
            default_jurisdiction: consent::default_jurisdiction(geo_status),
        });

        // Assemble the permission state once, here, through the permission
        // model, building the country/region baseline amended by what the
        // signal modules the adapter selected say about the request.
        // Downstream consumers read the stored result via
        // [`EcContext::permissions`] and [`EcContext::ec_allowed`] rather than
        // re-deriving it. The modules see the request as evidence, the same
        // abstraction the Edge Cookie and device modules read, so a scheme
        // core has never heard of can read its own signal from it.
        let evidence = crate::evidence::BorrowedRequestInfo::new(
            client_ip.as_deref().unwrap_or_default(),
            Some(req.headers()),
        )
        .with_request_target(req.uri().path(), req.uri().query().unwrap_or_default());
        let permissions = consent::assemble_permissions(
            &consent,
            &evidence,
            geo_status,
            services.permission_signal_modules(),
        );
        // A signal no module vouched for goes no further. The module for
        // its scheme has already decided what its absence means, so what is
        // forwarded is exactly what the permissions were built from.
        let mut consent = consent;
        consent.keep_only(permissions.signals());
        // With no module selected nothing may create or use an identifier, so
        // the gate is closed rather than open by default.
        let ec_allowed = selected_module
            .as_ref()
            .is_some_and(|selected| permissions.all_set(selected.required_permissions()));

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
            selected_module,
            request_headers,
            request_path,
            request_query,
            response_headers: Vec::new(),
            kv_snapshot: EcKvSnapshot::NotRead,
            recovery_eligible: false,
            pull_sync_marker: PullSyncMarkerState::from_cookie(parsed.pull_sync_marker),
            eid_sync_source: None,
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
    /// Forwards every error from `generate_with_module`: the selected module
    /// failing to derive an identifier (which includes a module that needs the
    /// client IP being run on a host that cannot supply one), the module
    /// producing an identifier outside the cookie-safe alphabet or over the
    /// length cap, the module asking for a response header inside core's
    /// reserved surface, persisting the identifier to the KV identity graph
    /// failing, or every attempt within the retry limit producing an identifier
    /// the graph already holds.
    pub fn generate_if_needed(
        &mut self,
        settings: &Settings,
        kv: Option<&KvIdentityGraph>,
    ) -> Result<(), Report<TrustedServerError>> {
        if self.ec_value.is_some() {
            return Ok(());
        }

        // A deployment with no module selected is stateless: nothing to
        // generate, and not an error. Reuse the module built at read time
        // rather than building it again.
        let Some(ec_module) = self.selected_module.clone() else {
            log::trace!("EC generation skipped: no Edge Cookie module configured");
            return Ok(());
        };

        if !self.ec_allowed {
            // The jurisdiction class is logged rather than the value. The
            // value carries a configured US state code, and the full
            // jurisdiction is already logged once when the EC context is
            // built, so naming the class here loses nothing.
            let jurisdiction = match &self.consent.jurisdiction {
                Jurisdiction::Gdpr => "gdpr",
                Jurisdiction::UsState(_) => "us-state",
                Jurisdiction::NonRegulated => "non-regulated",
                Jurisdiction::Unknown => "unknown",
            };
            log::info!(
                "EC generation skipped: required permissions not set (jurisdiction={jurisdiction})"
            );
            return Ok(());
        }

        // Whether the client IP is needed is the selected module's decision,
        // not core's. A module that derives identity from headers, cookies,
        // query parameters, or the client reads no IP and must still run on a
        // host that cannot supply one. The IP is passed as the documented
        // unavailable value, the empty string (see
        // [`RequestInfo::client_ip`](crate::evidence::RequestInfo::client_ip)),
        // and a module that needs it refuses there, returning the error to
        // the caller. The publisher proxy and integration proxy log it and
        // serve the response without an Edge Cookie.
        self.generate_with_module(ec_module.as_ref(), settings, kv)
    }

    /// Asks the selected module for one new identifier, without persisting
    /// it.
    ///
    /// Runs the checks every new identifier has to pass before it is kept. The
    /// module's response headers are checked against core's reserved surface
    /// and captured for EC finalization, even when the module produces no
    /// identifier, and the finished identifier, with its module code applied,
    /// is checked against the global bounds. Generation and orphan recovery both
    /// use this, so a rotated identifier comes from the same module
    /// and passes the same checks as a new one. The request evidence captured at
    /// read time (client IP, headers, and the URL path and query) is passed
    /// borrowed through [`RequestInfo`](crate::evidence::RequestInfo), and the
    /// built-in HMAC module reads only the client IP.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::EdgeCookie`] when the module fails to
    /// derive an identifier (which for [`HmacModule`] includes an
    /// unavailable client IP), asks for a response header inside core's
    /// reserved surface (see
    /// [`reserved_response_effect`](crate::ec::module::reserved_response_effect)),
    /// or produces an identifier that is empty, over the length cap, or outside
    /// the cookie-safe alphabet.
    pub(crate) fn candidate_id(
        &mut self,
        ec_module: &dyn EdgeCookieModule,
    ) -> Result<Option<String>, Report<TrustedServerError>> {
        let input = IdentityInput {
            permissions: Some(&self.permissions),
            consent: Some(&self.consent),
        };
        let request_info = BorrowedRequestInfo::new(
            self.client_ip.as_deref().unwrap_or_default(),
            Some(&self.request_headers),
        )
        .with_request_target(&self.request_path, &self.request_query);
        let generated: GeneratedEdgeCookie = ec_module.generate(&request_info, &input)?;
        // Check every response header the module asked for against core's
        // reserved surface before any of them are kept. A module may set its
        // own cookies and headers, but not a managed `ts-` cookie, a header in
        // the `x-ts-` namespace, or a framing or hop-by-hop header. Without the
        // check a module could write `ts-ec` itself and bypass the identifier
        // validation and identity-graph row generation enforces. A rejection
        // returns an error, as the identifier-bounds rejection below does.
        // Because the check runs before the headers are captured, nothing from
        // a rejected module response is kept, and the response is served
        // without a new Edge Cookie. Checked before the identifier is read,
        // because a module can return headers with no identifier at all.
        for (name, value) in &generated.response_headers {
            if let Some(effect) = module::reserved_response_effect(name, value) {
                return Err(Report::new(TrustedServerError::EdgeCookie {
                    message: format!(
                        "Module `{}` returned a response header `{name}` that {effect}",
                        ec_module.id(),
                    ),
                }));
            }
        }
        let generated_id = generated
            .id
            .map(|value| crate::ec::module::apply_module_code(ec_module, &value));
        let Some(ec_id) = generated_id else {
            // Keep the response headers the module asked for even though it
            // produced no identifier (for example while it still needs more
            // client evidence). EC finalization applies them to the response.
            self.response_headers = generated.response_headers;
            log::info!(
                "EC generation produced no identifier (module={}); proceeding without an EC",
                ec_module.id(),
            );
            return Ok(None);
        };
        // Enforce the global identifier bounds at creation. The cookie-safe
        // alphabet and the length cap apply to every module, so no
        // implementation can emit a value the cookie layer or the identity
        // graph cannot carry. Rejection is loud and total; the identifier is
        // never rewritten.
        if !ec_id_has_only_allowed_chars(&ec_id) {
            return Err(Report::new(TrustedServerError::EdgeCookie {
                message: format!(
                    "Module `{}` produced an identifier that is empty, over {} bytes, or \
                     outside the cookie-safe alphabet",
                    ec_module.id(),
                    cookies::MAX_EC_ID_LEN,
                ),
            }));
        }
        // Keep the module's response headers only now that its identifier
        // has passed the bounds check, so a module response whose identifier
        // is rejected keeps none of them. EC finalization applies them to the
        // response.
        self.response_headers = generated.response_headers;
        log::info!(
            "Generated new EC ID (module={}): {}",
            ec_module.id(),
            log_id(&ec_id),
        );
        Ok(Some(ec_id))
    }

    /// Derives and commits an EC identifier using a specific module.
    ///
    /// Split out of [`generate_if_needed`](Self::generate_if_needed) so the
    /// module is supplied explicitly, resolved once at read time and threaded
    /// here rather than rebuilt. Each attempt asks the module for a candidate
    /// through [`candidate_id`](Self::candidate_id) and creates its
    /// identity-graph row only when no row already holds that key, so a
    /// colliding identifier never overwrites another identity's row and the
    /// next attempt asks the module again. The row is keyed by the module's
    /// canonical form of the identifier, and the request snapshot is bound to
    /// that key. The skip guards (existing EC, permission gate) stay in
    /// [`generate_if_needed`](Self::generate_if_needed).
    ///
    /// The response headers a module response asks for stay on the context
    /// only when its identifier is committed or it produced no identifier at
    /// all. A colliding candidate's headers are dropped before the next attempt
    /// and nothing is kept when persisting fails, so EC finalization never
    /// applies a header from a candidate this request discarded.
    ///
    /// # Errors
    ///
    /// Forwards every error from [`candidate_id`](Self::candidate_id), and
    /// returns [`TrustedServerError::EdgeCookie`] when persisting a generated
    /// identifier to the KV identity graph fails or every attempt within the
    /// retry limit produces an identifier the graph already holds.
    fn generate_with_module(
        &mut self,
        ec_module: &dyn EdgeCookieModule,
        settings: &Settings,
        kv: Option<&KvIdentityGraph>,
    ) -> Result<(), Report<TrustedServerError>> {
        const MAX_CREATE_ATTEMPTS: usize = 5;
        for attempt in 0..MAX_CREATE_ATTEMPTS {
            let Some(ec_id) = self.candidate_id(ec_module)? else {
                return Ok(());
            };
            // Key the identity graph by the module's canonical form of the
            // identifier, so equivalent representations of one identity share
            // one row. The built-in normalization lowercases only the HMAC
            // hash segment; an opaque vendor module overrides it to the
            // identity function.
            let kv_key = crate::ec::module::module_kv_key(ec_module, &ec_id);
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

            if let Some(graph) = kv {
                match graph.create_if_absent(&kv_key, &entry) {
                    Ok(CreateIfAbsentOutcome::Written) => {
                        self.kv_snapshot = EcKvSnapshot::Present {
                            ec_id: kv_key,
                            entry: Box::new(entry),
                            generation: None,
                        };
                    }
                    Ok(CreateIfAbsentOutcome::AlreadyExists) => {
                        // The colliding candidate is discarded, and the
                        // response headers its module response asked for
                        // are discarded with it.
                        self.response_headers.clear();
                        log::warn!(
                            "Generated EC ID collision on attempt {}/{MAX_CREATE_ATTEMPTS}",
                            attempt + 1
                        );
                        continue;
                    }
                    Err(err) => {
                        // Nothing is committed, so none of the module's
                        // response headers are kept either.
                        self.response_headers.clear();
                        log::error!(
                            "Failed to create EC entry for id '{}' after generation: {err:?}",
                            log_id(&ec_id),
                        );
                        return Err(err.change_context(TrustedServerError::EdgeCookie {
                            message: "Failed to persist generated EC ID to KV identity graph"
                                .to_string(),
                        }));
                    }
                }
            }

            self.ec_value = Some(ec_id);
            self.ec_generated = true;
            self.pull_sync_marker.invalidate_for_replaced_ec();
            return Ok(());
        }

        Err(Report::new(TrustedServerError::EdgeCookie {
            message: format!(
                "Failed to allocate a unique EC ID after {MAX_CREATE_ATTEMPTS} attempts"
            ),
        }))
    }

    /// Returns the EC ID value, if present (either from request or generated).
    #[must_use]
    pub fn ec_value(&self) -> Option<&str> {
        self.ec_value.as_deref()
    }

    /// The modules whose identifiers this request's paths accept, being the
    /// selected module alone (see
    /// [`AcceptedModules`](module::AcceptedModules)).
    #[must_use]
    pub(crate) fn accepted_modules(&self) -> module::AcceptedModules<'_> {
        module::AcceptedModules::active(self.selected_module.as_deref())
    }

    /// The identity-graph key for `value` under the modules this deployment
    /// reads.
    ///
    /// The canonical route from a request's identifier to a row key, so a
    /// module whose canonical form differs from the cookie value still finds
    /// the row it created. The owning module is picked by the identifier's
    /// `{code}~` prefix and supplies the canonical form of its own value part,
    /// matching the key [`generate_if_needed`](Self::generate_if_needed) wrote
    /// at creation.
    ///
    /// Identify, EC finalization and pull sync read and write this
    /// identifier's row under this key. The publisher navigation, `/auction`
    /// and `/_ts/page-bids` paths load their request snapshot under this key,
    /// and EID resolution looks the entry up in that snapshot under this key.
    /// Each of these reaches the key through this function or through
    /// [`ec_kv_key`](Self::ec_kv_key), which wraps this function. Batch sync
    /// and the admin lookup have no EC context, so they call
    /// [`AcceptedModules::canonical_kv_key`](module::AcceptedModules::canonical_kv_key),
    /// the function this one wraps, directly.
    ///
    /// `None` when no module this deployment reads owns `value`, in which
    /// case there is no row to read or write.
    #[must_use]
    pub(crate) fn kv_key_for(&self, value: &str) -> Option<String> {
        self.accepted_modules().canonical_kv_key(value)
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
    /// This does not reach across a module switch. An identifier created
    /// under a retired module's `{code}~` prefix is owned by no module
    /// this deployment reads, so [`kv_key_for`](Self::kv_key_for) yields
    /// `None` and its row is never tombstoned. Core cannot derive that key,
    /// because the canonical form is the owning module's own normalization.
    /// So after a module switch a withdrawal expires only the browser
    /// cookie, because that path keys off the raw cookie rather than off
    /// ownership, and the retired module's row stays until its time to live
    /// runs out.
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
    /// context. Mutations do not re-derive [`ec_allowed`](Self::ec_allowed) or
    /// [`permissions`](Self::permissions), which are resolved once at
    /// construction.
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

    /// Returns the response headers a module asked to set during
    /// [`generate_if_needed`](Self::generate_if_needed). Empty unless a module
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

    /// Returns the request-scoped identity-graph snapshot.
    #[must_use]
    pub fn kv_snapshot(&self) -> &EcKvSnapshot {
        &self.kv_snapshot
    }

    /// Replaces the request-scoped identity-graph snapshot.
    pub fn set_kv_snapshot(&mut self, snapshot: EcKvSnapshot) {
        self.kv_snapshot = snapshot;
    }

    /// Marks a real-browser document navigation as eligible for orphan recovery.
    pub fn set_recovery_eligible(&mut self, eligible: bool) {
        self.recovery_eligible = eligible;
    }

    /// Allows returning-user EID cookie persistence for this request source.
    pub fn set_eid_sync_source(&mut self, source: EidSyncSource) {
        self.eid_sync_source = Some(source);
    }

    /// Returns the allowed returning-user EID persistence source.
    #[must_use]
    pub fn eid_sync_source(&self) -> Option<EidSyncSource> {
        self.eid_sync_source
    }

    /// Returns whether orphan recovery is allowed for this request.
    #[must_use]
    pub fn recovery_eligible(&self) -> bool {
        self.recovery_eligible
    }

    /// Validates a browser completeness marker against the active EC and partner set.
    pub(crate) fn validate_pull_sync_marker(
        &mut self,
        settings: &Settings,
        registry: &registry::PartnerRegistry,
    ) {
        validate_marker_state(
            &mut self.pull_sync_marker,
            settings,
            registry,
            self.ec_value.as_deref(),
        );
    }

    /// Returns the current pull-sync marker state.
    #[must_use]
    pub(crate) fn pull_sync_marker(&self) -> &PullSyncMarkerState {
        &self.pull_sync_marker
    }

    /// Returns mutable pull-sync marker state for response reconciliation.
    pub(crate) fn pull_sync_marker_mut(&mut self) -> &mut PullSyncMarkerState {
        &mut self.pull_sync_marker
    }

    /// Sets pull-sync marker state in focused unit tests.
    #[cfg(test)]
    pub(crate) fn set_pull_sync_marker_for_test(&mut self, state: PullSyncMarkerState) {
        self.pull_sync_marker = state;
    }

    /// Replaces an orphaned active ID after its new backing row is persisted.
    pub(crate) fn replace_with_generated(&mut self, ec_id: String, snapshot: EcKvSnapshot) {
        self.ec_value = Some(ec_id);
        self.ec_generated = true;
        self.kv_snapshot = snapshot;
        self.pull_sync_marker.invalidate_for_replaced_ec();
    }

    /// The Edge Cookie module this request resolved at read time, if any.
    ///
    /// Orphan recovery creates a replacement identifier through this module,
    /// so a rotated identifier comes from the same module as a new one.
    #[must_use]
    pub(crate) fn selected_module(&self) -> Option<Arc<dyn crate::ec::module::EdgeCookieModule>> {
        self.selected_module.clone()
    }

    /// Returns whether the configured Edge Cookie module's required
    /// permissions are set for this request.
    ///
    /// Resolved once at construction through the permission model (see
    /// [`consent::assemble_permissions`]).
    #[must_use]
    pub fn ec_allowed(&self) -> bool {
        self.ec_allowed
    }

    /// Whether the request carries an explicit signal withdrawing Edge Cookie
    /// storage, scoped to the jurisdiction's storage baseline.
    ///
    /// Answered by the signal modules at construction and recorded on the
    /// permission state, see [`PermissionState::storage_withdrawn`]. Of the
    /// schemes that ship, only a TCF record refusing storage withdraws, and
    /// only where the storage baseline is not `granted`. Suppression (the
    /// permission merely not set) is reported by
    /// [`ec_allowed`](Self::ec_allowed) being `false` instead.
    #[must_use]
    pub fn storage_withdrawn(&self) -> bool {
        self.permissions.storage_withdrawn()
    }

    /// Whether the Edge Cookie identifier may be shared beyond the edge for
    /// this request: into the bidstream as `user.id`, in a partner identify
    /// response, or in a partner sync call.
    ///
    /// Sharing rides on the same two permissions as bidstream EIDs (see
    /// [`crate::consent::gate_eids_by_permissions`]): storage (the identifier
    /// exists and is readable) and personalised-ad selection (it is shared to
    /// select ads). [`ec_allowed`](Self::ec_allowed) covers only the
    /// module's own requirements, so a storage-only grant keeps first-party
    /// use while withholding partner sharing.
    #[must_use]
    pub fn ec_sharing_allowed(&self) -> bool {
        self.ec_allowed()
            && self.permissions.is_set(Permission::StoreOnDevice)
            && self.permissions.is_set(Permission::SelectPersonalisedAds)
    }

    /// Returns the permissions resolved for this request.
    ///
    /// Assembled once at construction, the country/region baseline augmented by
    /// the session's signals. The core gates module execution on these, and a
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

    /// Returns the stable EC hash prefix from the active EC value.
    #[must_use]
    pub fn ec_hash(&self) -> Option<&str> {
        self.ec_value.as_deref().map(generation::ec_hash)
    }

    /// Attaches a selected module to a test-only [`EcContext`].
    ///
    /// The production constructor builds the module from settings and
    /// injected services. A test that only needs the module's identifier
    /// semantics (which identifiers it owns, and their canonical key form)
    /// takes this shortcut instead.
    #[cfg(test)]
    #[must_use]
    pub fn with_module_for_test(
        mut self,
        module: Arc<dyn crate::ec::module::EdgeCookieModule>,
    ) -> Self {
        self.selected_module = Some(module);
        self
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
        let permissions = if ec_allowed {
            PermissionState::new(
                [Permission::StoreOnDevice, Permission::SelectPersonalisedAds]
                    .into_iter()
                    .collect(),
            )
        } else {
            PermissionState::default()
        };
        Self {
            ec_was_present: ec_value.is_some(),
            cookie_ec_value: ec_value.clone(),
            ec_value,
            ec_generated: false,
            consent,
            ec_allowed,
            permissions,
            client_ip: None,
            geo_info: None,
            device_signals: None,
            selected_module: None,
            request_headers: http::HeaderMap::new(),
            request_path: String::new(),
            request_query: String::new(),
            response_headers: Vec::new(),
            kv_snapshot: EcKvSnapshot::NotRead,
            recovery_eligible: false,
            pull_sync_marker: PullSyncMarkerState::Absent,
            eid_sync_source: None,
        }
    }

    /// Creates a test-only `EcContext` whose request explicitly withdrew
    /// device storage, rather than merely not setting it.
    ///
    /// Withdrawal is destructive, expiring the browser cookie, where
    /// suppression is not, so a test of the destructive path has to say which
    /// of the two it means. [`new_for_test_gated`](Self::new_for_test_gated)
    /// with `false` gives suppression.
    #[cfg(test)]
    #[must_use]
    pub fn new_for_test_withdrawn(ec_value: Option<String>, consent: ConsentContext) -> Self {
        let mut context = Self::new_for_test_gated(ec_value, consent, false);
        context.permissions = PermissionState::default().with_storage_withdrawn(true);
        context
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
            selected_module: None,
            request_headers: http::HeaderMap::new(),
            request_path: String::new(),
            request_query: String::new(),
            response_headers: Vec::new(),
            kv_snapshot: EcKvSnapshot::NotRead,
            recovery_eligible: false,
            pull_sync_marker: PullSyncMarkerState::Absent,
            eid_sync_source: None,
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
            selected_module: None,
            request_headers: http::HeaderMap::new(),
            request_path: String::new(),
            request_query: String::new(),
            response_headers: Vec::new(),
            kv_snapshot: EcKvSnapshot::NotRead,
            recovery_eligible: false,
            pull_sync_marker: PullSyncMarkerState::Absent,
            eid_sync_source: None,
        }
    }

    /// The same context, recording that the request explicitly withdrew
    /// storage, as assembly records it when a signal module answers so.
    ///
    /// Core links no module, so a core test cannot derive the withdrawal
    /// from a consent record the way a deployment does through the TCF
    /// module. It states the answer instead and tests what finalization does
    /// with it. That a TCF refusal produces this answer is proved where the
    /// modules are linked, in the Axum adapter's `permission_signals` test.
    #[cfg(test)]
    #[must_use]
    pub fn with_storage_withdrawn_for_test(mut self, withdrawn: bool) -> Self {
        self.permissions = self.permissions.with_storage_withdrawn(withdrawn);
        self
    }
}

/// Returns the current Unix timestamp in seconds, falling back to zero on clock failure.
///
/// Uses [`web_time::SystemTime`], which maps to `std::time::SystemTime` on
/// native and `wasm32-wasip1` targets and to a JS-backed clock on
/// `wasm32-unknown-unknown` (Cloudflare Workers), where `std::time` is not
/// available.
pub(crate) fn current_timestamp() -> u64 {
    checked_current_timestamp().unwrap_or(0)
}

/// Returns the current Unix timestamp, or `None` when the clock precedes the epoch.
///
/// Use this instead of [`current_timestamp`] when a fallback could authorize
/// a time-bounded correctness decision.
pub(crate) fn checked_current_timestamp() -> Option<u64> {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|err| {
            log::error!("SystemTime::now() failed: {err}");
        })
        .ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};
    use crate::ec::kv_backend::test_support::InMemoryEcKv;
    use crate::ec::kv_backend::{
        EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome,
    };
    use crate::ec::module::{EcModuleSelection, ModuleCode};
    use crate::evidence::RequestInfo;
    use crate::platform::test_support::noop_services;
    use crate::test_support::tests::create_test_settings;

    /// [`EcKvStore`] wrapper whose first `collisions` `Add` writes report a
    /// precondition failure, forcing generation to retry with a fresh suffix.
    ///
    /// Shared with the finalization tests, whose orphan recovery creates its
    /// replacement through the same `Add` write.
    pub(crate) struct AddCollidingEcKv {
        inner: InMemoryEcKv,
        collisions_remaining: std::sync::Mutex<u32>,
    }

    impl AddCollidingEcKv {
        pub(crate) fn new(collisions: u32) -> Self {
            Self {
                inner: InMemoryEcKv::new("add-colliding-store"),
                collisions_remaining: std::sync::Mutex::new(collisions),
            }
        }
    }

    impl EcKvStore for AddCollidingEcKv {
        fn store_name(&self) -> &str {
            self.inner.store_name()
        }
        fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            self.inner.lookup(key)
        }
        fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
            self.inner.key_exists(key)
        }

        fn insert(
            &self,
            key: &str,
            write: EcKvWrite<'_>,
        ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
            if matches!(write.mode, EcKvWriteMode::Add) {
                let mut remaining = self
                    .collisions_remaining
                    .lock()
                    .expect("should lock collision counter");
                if *remaining > 0 {
                    *remaining -= 1;
                    return Ok(EcKvWriteOutcome::PreconditionFailed);
                }
            }
            self.inner.insert(key, write)
        }
        fn list_keys_with_prefix(
            &self,
            prefix: &str,
            limit: u32,
        ) -> Result<Vec<String>, Report<TrustedServerError>> {
            self.inner.list_keys_with_prefix(prefix, limit)
        }
        fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
            self.inner.delete(key)
        }
    }

    fn granting_consent() -> ConsentContext {
        ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            source: ConsentSource::Cookie,
            ..Default::default()
        }
    }

    #[test]
    fn generate_if_needed_retries_id_collision_then_persists() {
        let settings = create_test_settings();
        let mut ec =
            EcContext::new_for_test_with_ip(None, granting_consent(), Some("192.0.2.5".to_owned()))
                .with_module_for_test(hmac_module());
        let graph = KvIdentityGraph::new(AddCollidingEcKv::new(2));

        ec.generate_if_needed(&settings, Some(&graph))
            .expect("should generate after bounded collisions");

        assert!(ec.ec_value().is_some(), "should allocate a fresh EC ID");
        assert!(ec.ec_generated(), "should mark the EC as generated");
        assert!(
            matches!(ec.kv_snapshot(), EcKvSnapshot::Present { .. }),
            "generation should seed a present snapshot"
        );
    }

    #[test]
    fn generate_if_needed_errors_after_collision_exhaustion() {
        let settings = create_test_settings();
        let mut ec =
            EcContext::new_for_test_with_ip(None, granting_consent(), Some("192.0.2.6".to_owned()))
                .with_module_for_test(hmac_module());
        // Collide on every attempt so the bounded retry is exhausted.
        let graph = KvIdentityGraph::new(AddCollidingEcKv::new(u32::MAX));

        let result = ec.generate_if_needed(&settings, Some(&graph));

        assert!(result.is_err(), "should fail after exhausting attempts");
        assert!(
            ec.ec_value().is_none() && !ec.ec_generated(),
            "must not activate an EC ID it could not persist"
        );
    }

    #[test]
    fn default_ec_context_is_recovery_ineligible_and_unread() {
        let ec = EcContext::default();
        assert!(
            !ec.recovery_eligible(),
            "a default context must not authorize orphan recovery"
        );
        assert!(
            matches!(ec.kv_snapshot(), EcKvSnapshot::NotRead),
            "a default context must carry no identity-graph state"
        );
    }

    #[test]
    fn read_from_request_does_not_authorize_recovery_from_navigation_headers() {
        // Non-Fastly adapters build EC context through the shared read path and
        // never call `set_recovery_eligible`. Navigation headers alone must not
        // authorize orphan recovery or seed KV state.
        let settings = create_test_settings();
        let ec_id = valid_ec_id("b", "CkEc01");
        let cookie = format!("ts-ec={ec_id}");
        let req = create_test_request(&[("cookie", &cookie), ("sec-fetch-dest", "document")]);

        let ec = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");

        assert!(
            !ec.recovery_eligible(),
            "the shared read path must never authorize recovery from headers"
        );
        assert!(
            matches!(ec.kv_snapshot(), EcKvSnapshot::NotRead),
            "the shared read path must leave the snapshot unread"
        );
    }

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

    /// A module other than the built-in HMAC one, whose identifiers the HMAC
    /// grammar rejects (no dot, mixed case), modeling a vendor identifier such
    /// as a signed envelope. It accepts any of its own non-empty identifiers
    /// and keys the identity graph by the value unchanged.
    ///
    /// Shared with the admin lookup, batch sync and pull sync tests.
    #[derive(Debug)]
    pub(crate) struct OpaqueModule;

    impl EdgeCookieModule for OpaqueModule {
        fn id(&self) -> &'static str {
            "opaque"
        }

        fn code(&self) -> ModuleCode {
            crate::module_code!("t0op")
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

        fn normalize_id_for_kv(&self, value: &str) -> String {
            value.to_owned()
        }
    }

    /// A geo that resolves to the non-regulated jurisdiction (US, no region),
    /// so the permission gate is open and generation runs.
    ///
    /// A test that drives a built-in module needs this. The test default
    /// country is FR, whose baseline requires a signal before
    /// `StoreOnDevice` is set, so the built-in modules are gated off
    /// without one. A test double declaring no required permission runs
    /// either way and can read the request without a location.
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
    fn read_from_request_reuses_the_module_the_composition_root_resolved() {
        // The context reuses the module the composition root resolved and
        // threaded through `RuntimeServices`, so the request builds none.
        let mut settings = create_test_settings();
        settings.ec.module = Some(EcModuleSelection::from("opaque"));

        let ec_config = settings.ec.clone();
        let resolved = crate::ec::module::build_reusable_module(
            &ec_config,
            None,
            Some(Arc::new(OpaqueModule)),
        )
        .expect("the composition root should resolve the selection")
        .expect("the selection should yield a module");

        let services =
            crate::platform::test_support::noop_services_with_ec_module(Arc::clone(&resolved));
        let req = create_test_request(&[]);
        let ec = EcContext::read_from_request(&settings, &req, &services)
            .expect("should read EC context");

        let used = ec
            .selected_module
            .as_ref()
            .expect("the context should hold the selected module");
        assert!(
            Arc::ptr_eq(used, &resolved),
            "reading EC state should reuse the module resolved at startup rather \
             than building a second one for this request"
        );
    }

    #[test]
    fn read_from_request_round_trips_an_opaque_module_identifier() {
        use crate::platform::test_support::noop_services_with_ec_module;

        // A vendor identifier that is deliberately not the built-in HMAC shape
        // (no dot, mixed case), the exact value the built-in check would drop.
        const OPAQUE_ID: &str = "AbC123opaqueEnvelopeValueXYZ";
        const CODED_ID: &str = "t0op~AbC123opaqueEnvelopeValueXYZ";

        let mut settings = create_test_settings();
        settings.ec.module = Some(EcModuleSelection::from("opaque"));
        let cookie = format!("ts-ec={CODED_ID}");
        let req = create_test_request(&[("cookie", &cookie)]);

        // With the opaque module injected, its `accepts_id` governs read-back,
        // so the identifier survives verbatim.
        let services = noop_services_with_ec_module(Arc::new(OpaqueModule));
        let ec = EcContext::read_from_request(&settings, &req, &services)
            .expect("should read EC context");
        assert_eq!(
            ec.ec_value(),
            Some(CODED_ID),
            "an opaque module identifier should round-trip through read-back verbatim"
        );
        let _ = OPAQUE_ID;

        // Control: with the module selected but not injected by the adapter,
        // the request fails loudly instead of silently running stateless with
        // the identifier dropped.
        let err = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect_err("a selected but uninjected module should fail the request");
        assert!(
            err.to_string().contains("opaque"),
            "the error should name the selected module, got: {err}"
        );

        // Control: with no module selected at all, the identifier is treated
        // as absent, so a stateless deployment never uses or egresses it.
        let mut stateless = create_test_settings();
        stateless.ec.module = None;
        stateless.ec.module_blocks.clear();
        let ec_without = EcContext::read_from_request(&stateless, &req, &noop_services())
            .expect("should read EC context");
        assert_eq!(
            ec_without.ec_value(),
            None,
            "with no module selected, an existing identifier is treated as absent"
        );
        assert!(
            !ec_without.ec_allowed(),
            "with no module selected, the gate stays closed"
        );
    }

    /// A module that records the request query parameter `id` and the `Cookie`
    /// header it is given at generate time, proving request evidence (parameters
    /// and cookies) reaches a module through the organic generate path.
    #[derive(Debug, Default)]
    struct EvidenceCapturingModule {
        seen: std::sync::Mutex<Option<(String, String)>>,
    }

    impl EdgeCookieModule for EvidenceCapturingModule {
        fn id(&self) -> &'static str {
            "evidence"
        }

        fn code(&self) -> ModuleCode {
            crate::module_code!("t0ev")
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
    fn generate_passes_request_parameters_and_cookies_to_the_module() {
        use crate::platform::test_support::noop_services_with_ec_module;

        let module = Arc::new(EvidenceCapturingModule::default());
        let mut settings = create_test_settings();
        settings.ec.module = Some(EcModuleSelection::from("evidence"));

        // A request carrying a query parameter and a (non-EC) cookie, with no
        // existing `ts-ec` cookie so the generate path runs.
        let req = Request::builder()
            .method("GET")
            .uri("http://example.com/page?id=abc123&debug=1")
            .header("cookie", "client-id=xyz789")
            .body(EdgeBody::empty())
            .expect("should build request");

        let services = noop_services_with_ec_module(module.clone());
        let geo = non_regulated_geo();
        let mut ec = EcContext::read_from_request_with_geo(&settings, &req, &services, Some(&geo))
            .expect("should read EC context");
        ec.generate_if_needed(&settings, None)
            .expect("should run generation");

        let seen = module
            .seen
            .lock()
            .expect("should lock seen evidence")
            .clone();
        assert_eq!(
            seen,
            Some(("abc123".to_owned(), "client-id=xyz789".to_owned())),
            "the module should read the request query parameter and cookies at generate time"
        );
        assert_eq!(
            ec.ec_value(),
            Some("t0ev~evidence-ec"),
            "the identifier the module created should be committed under its code"
        );
    }

    #[test]
    fn recovery_on_a_navigation_passes_request_parameters_and_cookies_to_the_module() {
        use crate::platform::test_support::noop_services_with_ec_module;

        let module = Arc::new(EvidenceCapturingModule::default());
        let mut settings = create_test_settings();
        settings.ec.module = Some(EcModuleSelection::from("evidence"));

        // A returning visitor's document navigation, carrying an identifier
        // the module owns, which is the request orphan recovery runs on.
        let cookie = "ts-ec=t0ev~orphaned; client-id=xyz789";
        let req = Request::builder()
            .method("GET")
            .uri("http://example.com/page?id=abc123")
            .header("cookie", cookie)
            .header("sec-fetch-dest", "document")
            .body(EdgeBody::empty())
            .expect("should build request");

        let services = noop_services_with_ec_module(module.clone());
        let geo = non_regulated_geo();
        let mut ec = EcContext::read_from_request_with_geo(&settings, &req, &services, Some(&geo))
            .expect("should read EC context");
        assert!(ec.ec_was_present(), "the identifier should be read back");
        let selected = ec.selected_module().expect("a module is selected");
        ec.candidate_id(selected.as_ref())
            .expect("should create a replacement");

        let seen = module
            .seen
            .lock()
            .expect("should lock seen evidence")
            .clone();
        assert_eq!(
            seen,
            Some(("abc123".to_owned(), cookie.to_owned())),
            "recovery should give the module the request's parameters and cookies"
        );
    }

    /// A module that creates an opaque, mixed-case, non-HMAC identifier at
    /// the edge, so a test can prove such an identifier persists to the KV identity
    /// graph under its own value as the key.
    #[derive(Debug)]
    struct ServerOpaqueModule;

    impl EdgeCookieModule for ServerOpaqueModule {
        fn id(&self) -> &'static str {
            "server_opaque"
        }

        fn code(&self) -> ModuleCode {
            crate::module_code!("t0so")
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
        use crate::platform::test_support::noop_services_with_ec_module;

        const OPAQUE: &str = "t0so~Opaque_EC_Value_MixedCase_123";

        let mut settings = create_test_settings();
        settings.ec.module = Some(EcModuleSelection::from("server_opaque"));
        let services = noop_services_with_ec_module(Arc::new(ServerOpaqueModule));
        let graph = KvIdentityGraph::in_memory("test-ec-store");

        // No existing cookie, so the edge creates one and persists it.
        let req = create_test_request(&[]);
        let mut ec = EcContext::read_from_request(&settings, &req, &services)
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

    #[test]
    fn a_module_that_reads_no_client_ip_creates_when_the_host_has_none() {
        use crate::platform::test_support::noop_services_with_ec_module_without_client_ip;

        // The requirement for a client IP belongs to the module that uses
        // one, not to core. A module deriving identity from the request
        // query and cookies runs on a host that cannot determine a client IP,
        // and receives the documented unavailable value, the empty string.
        let module = Arc::new(EvidenceCapturingModule::default());
        let mut settings = create_test_settings();
        settings.ec.module = Some(EcModuleSelection::from("evidence"));
        let req = Request::builder()
            .method("GET")
            .uri("http://example.com/page?id=abc123")
            .header("cookie", "client-id=xyz789")
            .body(EdgeBody::empty())
            .expect("should build request");

        let services = noop_services_with_ec_module_without_client_ip(module.clone());
        let mut ec = EcContext::read_from_request(&settings, &req, &services)
            .expect("should read EC context");
        assert_eq!(
            ec.client_ip(),
            None,
            "the host should supply no client IP in this test"
        );

        ec.generate_if_needed(&settings, None)
            .expect("a module that reads no client IP should still create an identifier");
        assert_eq!(
            ec.ec_value(),
            Some("t0ev~evidence-ec"),
            "the identifier should be committed with no client IP available"
        );
    }

    #[test]
    fn the_hmac_module_refuses_when_the_host_has_no_client_ip() {
        // The other half: the built-in module's only input is the client IP,
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
            .expect_err("the HMAC module should refuse without a client IP");
        assert!(
            err.to_string().contains("client IP"),
            "the error should name the missing client IP, got: {err}"
        );
        assert_eq!(
            ec.ec_value(),
            None,
            "no identifier should be committed when the module refuses"
        );
    }

    /// A module that creates an identifier outside the cookie-safe alphabet,
    /// to prove core rejects it at creation rather than rewriting it.
    #[derive(Debug)]
    struct IllegalIdModule;

    impl EdgeCookieModule for IllegalIdModule {
        fn id(&self) -> &'static str {
            "illegal"
        }

        fn code(&self) -> ModuleCode {
            crate::module_code!("t0il")
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
        use crate::platform::test_support::noop_services_with_ec_module;

        let mut settings = create_test_settings();
        settings.ec.module = Some(EcModuleSelection::from("illegal"));
        let services = noop_services_with_ec_module(Arc::new(IllegalIdModule));
        let req = create_test_request(&[]);
        let mut ec = EcContext::read_from_request(&settings, &req, &services)
            .expect("should read EC context");

        let err = ec
            .generate_if_needed(&settings, None)
            .expect_err("an identifier outside the alphabet should be rejected at creation");
        assert!(
            err.to_string().contains("illegal"),
            "the error should name the module, got: {err}"
        );
        assert_eq!(
            ec.ec_value(),
            None,
            "no identifier should be committed after a creation rejection"
        );
    }

    /// A module that returns a caller-chosen response header, and `id` as its
    /// identifier when one is given, so a test can drive one module response
    /// effect at a time through the organic generate path.
    #[derive(Debug)]
    struct HeaderSettingModule {
        name: &'static str,
        value: &'static str,
        id: Option<&'static str>,
    }

    impl EdgeCookieModule for HeaderSettingModule {
        fn id(&self) -> &'static str {
            "header_setting"
        }

        fn code(&self) -> ModuleCode {
            crate::module_code!("t0hs")
        }

        fn generate(
            &self,
            _request_info: &dyn RequestInfo,
            _input: &IdentityInput<'_>,
        ) -> Result<GeneratedEdgeCookie, Report<TrustedServerError>> {
            Ok(GeneratedEdgeCookie {
                id: self.id.map(str::to_owned),
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

    /// Runs generation through `module` and hands back the context whether or
    /// not generation succeeded, so a test can finalize a response on a context
    /// whose generation returned an error.
    fn generate_with_header_setting_module(
        module: HeaderSettingModule,
        graph: Option<&KvIdentityGraph>,
    ) -> (Settings, EcContext, Result<(), Report<TrustedServerError>>) {
        use crate::platform::test_support::noop_services_with_ec_module;

        let mut settings = create_test_settings();
        settings.ec.module = Some(EcModuleSelection::from("header_setting"));
        let services = noop_services_with_ec_module(Arc::new(module));
        let req = create_test_request(&[]);
        let mut ec = EcContext::read_from_request(&settings, &req, &services)
            .expect("should read EC context");
        let outcome = ec.generate_if_needed(&settings, graph);
        (settings, ec, outcome)
    }

    #[test]
    fn a_rejected_module_effect_never_reaches_the_finalized_response() {
        // A module that sets the managed `ts-ec` cookie, a header in the
        // `x-ts-` namespace or a framing header would bypass core's identifier
        // validation and its identity-graph row, so generation returns an
        // error rather than quietly dropping the effect. The publisher and
        // integration proxies log the error and still serve the response, and
        // EC finalization applies the response headers this same context
        // holds, so a rejected header kept among them would reach the browser
        // anyway. Each header is tried with no identifier, the case a cookie
        // write would otherwise slip through, and with one, to show that
        // nothing from the rejected module response is kept.
        for (name, value, id) in [
            ("set-cookie", "ts-ec=forged-value; Path=/", None),
            ("x-ts-ec", "forged", None),
            ("transfer-encoding", "chunked", None),
            (
                "set-cookie",
                "ts-ec=forged-value; Path=/",
                Some("module-value"),
            ),
            ("x-ts-ec", "forged", Some("module-value")),
            ("transfer-encoding", "chunked", Some("module-value")),
        ] {
            let case = format!("`{name}` with identifier {id:?}");
            let graph = KvIdentityGraph::in_memory("test-ec-store");
            let (settings, mut ec, outcome) = generate_with_header_setting_module(
                HeaderSettingModule { name, value, id },
                Some(&graph),
            );
            let Err(err) = outcome else {
                panic!("{case}: the header is reserved, so generation should return an error");
            };
            assert!(
                err.to_string().contains("header_setting"),
                "{case}: the error should name the module, got: {err}"
            );
            assert_eq!(
                ec.ec_value(),
                None,
                "{case}: no identifier should be committed after the rejection"
            );

            let mut response = http::Response::builder()
                .status(200)
                .body(EdgeBody::empty())
                .expect("should build test response");
            finalize::ec_finalize_response(
                &settings,
                &mut ec,
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
                .filter_map(|cookie| cookie.to_str().ok())
                .collect();
            assert!(
                !cookies.iter().any(|cookie| cookie.contains("ts-ec=forged")),
                "{case}: the rejected effect should not set `ts-ec`, got: {cookies:?}"
            );
            assert!(
                response.headers().get("x-ts-ec").is_none(),
                "{case}: the rejected effect should not set `x-ts-ec`"
            );
            assert!(
                response
                    .headers()
                    .get(http::header::TRANSFER_ENCODING)
                    .is_none(),
                "{case}: the rejected effect should not set `transfer-encoding`"
            );
        }
    }

    #[test]
    fn generate_applies_a_module_owned_cookie_to_the_response() {
        // The other half of the rule: a module's own cookie is not core's, so
        // it survives generation and reaches the browser response unchanged,
        // alongside the managed `ts-ec` cookie core writes itself.
        let graph = KvIdentityGraph::in_memory("test-ec-store");
        let (settings, mut ec, outcome) = generate_with_header_setting_module(
            HeaderSettingModule {
                name: "set-cookie",
                value: "acme-evidence=abc123; Path=/; Secure",
                id: Some("module-value"),
            },
            Some(&graph),
        );
        outcome.expect("generation should accept a module-owned cookie");
        assert_eq!(
            ec.ec_value(),
            Some("t0hs~module-value"),
            "the identifier should still be committed"
        );

        let mut response = http::Response::builder()
            .status(200)
            .body(EdgeBody::empty())
            .expect("should build test response");
        finalize::ec_finalize_response(
            &settings,
            &mut ec,
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
            "the module's own cookie should reach the response, got: {cookies:?}"
        );
        assert!(
            cookies.iter().any(|cookie| cookie.starts_with("ts-ec=")),
            "core's own managed cookie should still be written, got: {cookies:?}"
        );
    }

    /// A cookie a module may set for itself, used to check which module
    /// responses keep their headers.
    const MODULE_EVIDENCE_COOKIE: &str = "acme-evidence=abc123; Path=/; Secure";

    /// Runs EC finalization on `ec` for an empty response and returns the
    /// `Set-Cookie` values the response carries.
    fn finalized_set_cookies(
        settings: &Settings,
        ec: &mut EcContext,
        graph: &KvIdentityGraph,
    ) -> Vec<String> {
        let mut response = http::Response::builder()
            .status(200)
            .body(EdgeBody::empty())
            .expect("should build test response");
        finalize::ec_finalize_response(
            settings,
            ec,
            Some(graph),
            &registry::PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );
        response
            .headers()
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|cookie| cookie.to_str().ok())
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn a_discarded_candidate_keeps_none_of_its_module_response_headers() {
        // The module's own cookie is permitted, but each of these candidates
        // is discarded, so generation returns an error and the cookie its
        // module response asked for must not reach the browser.
        for (case, graph, id) in [
            // The identifier is outside the cookie-safe alphabet.
            (
                "a rejected identifier",
                KvIdentityGraph::in_memory("test-ec-store"),
                "bad;value with spaces",
            ),
            // Every candidate collides with a row the graph already holds, so
            // each one is discarded until the retries run out.
            (
                "a colliding candidate",
                KvIdentityGraph::new(AddCollidingEcKv::new(u32::MAX)),
                "module-value",
            ),
            // The identity graph cannot store the candidate's row.
            (
                "an unpersisted candidate",
                KvIdentityGraph::new(crate::ec::kv_backend::test_support::FailingEcKv::new(
                    "failing-ec-store",
                )),
                "module-value",
            ),
        ] {
            let (settings, mut ec, outcome) = generate_with_header_setting_module(
                HeaderSettingModule {
                    name: "set-cookie",
                    value: MODULE_EVIDENCE_COOKIE,
                    id: Some(id),
                },
                Some(&graph),
            );

            assert!(
                outcome.is_err(),
                "{case}: generation should return an error"
            );
            let cookies = finalized_set_cookies(&settings, &mut ec, &graph);
            assert!(
                !cookies
                    .iter()
                    .any(|cookie| cookie.starts_with("acme-evidence=")),
                "{case}: the module response's cookie should not reach the response, got: \
                 {cookies:?}"
            );
        }
    }

    #[test]
    fn a_module_that_creates_nothing_still_has_its_headers_applied() {
        // A module may ask for more client evidence before it can create an
        // identifier, for example with `Accept-CH`, so its headers reach the
        // response even though no Edge Cookie is set.
        let graph = KvIdentityGraph::in_memory("test-ec-store");
        let (settings, mut ec, outcome) = generate_with_header_setting_module(
            HeaderSettingModule {
                name: "accept-ch",
                value: "Sec-CH-UA",
                id: None,
            },
            Some(&graph),
        );
        outcome.expect("a module that creates nothing should not fail the request");
        assert_eq!(ec.ec_value(), None, "the module created no identifier");

        let mut response = http::Response::builder()
            .status(200)
            .body(EdgeBody::empty())
            .expect("should build test response");
        finalize::ec_finalize_response(
            &settings,
            &mut ec,
            Some(&graph),
            &registry::PartnerRegistry::empty(),
            None,
            None,
            &mut response,
        );

        assert_eq!(
            response
                .headers()
                .get("accept-ch")
                .and_then(|value| value.to_str().ok()),
            Some("Sec-CH-UA"),
            "the module's header should reach the response"
        );
        assert!(
            !response
                .headers()
                .get_all(http::header::SET_COOKIE)
                .iter()
                .filter_map(|value| value.to_str().ok())
                .any(|cookie| cookie.starts_with("ts-ec=")),
            "no Edge Cookie should be set when the module created nothing"
        );
    }

    /// A module whose identifier normalizes to a distinct canonical form, to
    /// prove the identity graph is keyed by the canonical form.
    ///
    /// Shared with the identify, finalization, pull sync, admin lookup, auction
    /// and publisher tests, which need a module whose canonical key is not the
    /// value the browser carries.
    #[derive(Debug)]
    pub(crate) struct CanonicalizingModule;

    impl EdgeCookieModule for CanonicalizingModule {
        fn id(&self) -> &'static str {
            "canonical"
        }

        fn code(&self) -> ModuleCode {
            crate::module_code!("t0ca")
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

    /// The identifier [`CanonicalizingModule`] creates, as the browser
    /// carries it in the `ts-ec` cookie.
    pub(crate) const CANONICAL_COOKIE_VALUE: &str = "t0ca~MiXeD.CaseId";

    /// The identity-graph key generation writes that identifier's row under.
    /// Pinned to the creation path by
    /// `generate_keys_the_identity_graph_by_the_normalized_identifier`, which
    /// asserts both the key generation writes and the key
    /// [`EcContext::ec_kv_key`] derives.
    pub(crate) const CANONICAL_KV_KEY: &str = "t0ca~mixed.caseid";

    /// The built-in HMAC module, as an HMAC deployment selects it.
    ///
    /// Creating or rotating an identifier needs a selected module, so the
    /// generation and orphan-recovery tests attach this one. Shared with the
    /// finalization tests.
    pub(crate) fn hmac_module() -> Arc<dyn EdgeCookieModule> {
        Arc::new(crate::ec::module::HmacModule::new(
            crate::redacted::Redacted::new("test-secret-key-32-bytes-minimum".to_owned()),
        ))
    }

    #[test]
    fn generate_keys_the_identity_graph_by_the_normalized_identifier() {
        use crate::platform::test_support::noop_services_with_ec_module;

        let mut settings = create_test_settings();
        settings.ec.module = Some(EcModuleSelection::from("canonical"));
        let services = noop_services_with_ec_module(Arc::new(CanonicalizingModule));
        let graph = KvIdentityGraph::in_memory("test-ec-store");
        let req = create_test_request(&[]);
        let mut ec = EcContext::read_from_request(&settings, &req, &services)
            .expect("should read EC context");
        ec.generate_if_needed(&settings, Some(&graph))
            .expect("should generate and persist");

        assert_eq!(
            ec.ec_value(),
            Some(CANONICAL_COOKIE_VALUE),
            "the cookie value keeps the module's exact identifier under its code"
        );
        assert!(
            graph
                .get(CANONICAL_KV_KEY)
                .expect("should read the graph")
                .is_some(),
            "the graph row should be keyed by the code plus the canonical form"
        );
        // Pin the read-side derivation to the key generation actually wrote.
        // Identify, the withdrawal tombstones, and EID ingestion all read the
        // row through `ec_kv_key`, so the two must never drift apart.
        assert_eq!(
            ec.ec_kv_key().as_deref(),
            Some(CANONICAL_KV_KEY),
            "the read-side key should be the key generation wrote"
        );
    }

    #[test]
    fn hmac_creates_a_coded_identifier_and_reads_the_legacy_bare_form() {
        let settings = create_test_settings();
        // Place the request in a US opt-out state, whose baseline grants the
        // storage permission with no signal, so the creation runs.
        let geo = us_opt_out_geo();
        let req = create_test_request(&[]);
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

        // A bare identifier with no module code still reads back under the
        // hmac module.
        let legacy = format!("{}.ABC123", "a".repeat(64));
        let cookie = format!("ts-ec={legacy}");
        let req = create_test_request(&[("cookie", &cookie)]);
        let ec =
            EcContext::read_from_request_with_geo(&settings, &req, &noop_services(), Some(&geo))
                .expect("should read EC context");
        assert_eq!(
            ec.ec_value(),
            Some(legacy.as_str()),
            "the legacy bare form should dual-read under the hmac module"
        );
    }

    #[test]
    fn the_request_path_hashes_an_ipv6_client_ip_by_its_64_prefix() {
        // A device rotates the lower 64 bits of its IPv6 address, so the
        // context keeps only the /64 prefix and the HMAC module hashes that.
        // Two addresses in one /64 share an identity hash, and it is the hash
        // of the prefix.
        let settings = create_test_settings();
        let geo = non_regulated_geo();
        let req = create_test_request(&[]);
        let prefix = "20010db885a30000";
        let passphrase = crate::test_support::tests::hmac_passphrase(
            &settings.ec,
            crate::ec::module::HMAC_MODULE_KEY,
        );
        let expected = generation::generate_ec_id(passphrase, prefix)
            .expect("should generate from the prefix");
        for interface in [0x1234, 0xabcd] {
            let ip = std::net::IpAddr::V6(std::net::Ipv6Addr::new(
                0x2001, 0x0db8, 0x85a3, 0x0000, 0x8a2e, 0x0370, 0x7334, interface,
            ));
            let services = crate::platform::test_support::noop_services_with_client_ip(ip);
            let mut ec =
                EcContext::read_from_request_with_geo(&settings, &req, &services, Some(&geo))
                    .expect("should read EC context");
            assert_eq!(
                ec.client_ip(),
                Some(prefix),
                "{ip}: the context should keep the /64 prefix alone"
            );
            ec.generate_if_needed(&settings, None)
                .expect("should generate");
            let created = ec
                .ec_value()
                .and_then(|value| value.strip_prefix("hmac~"))
                .expect("should create an identifier under the hmac code");
            assert_eq!(
                generation::ec_hash(created),
                generation::ec_hash(&expected),
                "{ip}: the identity hash should be the hash of the /64 prefix"
            );
        }
    }

    #[test]
    fn a_foreign_module_code_is_treated_as_absent() {
        // An identifier carrying another module's code must never be adopted
        // by the selected module, so switching modules cannot silently mix
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
            "an identifier with a foreign module code is not this module's"
        );
    }

    /// A geo module whose lookup fails, the state the permission model's
    /// fail-closed rule exists for.
    ///
    /// No geo module shipped in this workspace can fail: the Fastly SDK's
    /// `geo_lookup` returns an `Option`, the Cloudflare module reads request
    /// headers, and the Axum and Spin modules resolve nothing at all. The
    /// `Result` on [`PlatformGeo::lookup`] is there for a module that does
    /// its own fallible lookup, so this stands in for one and proves the floor
    /// is reached through the seam rather than only from a hand-built status.
    #[derive(Debug)]
    struct FailingGeo;

    impl crate::platform::PlatformGeo for FailingGeo {
        fn lookup(
            &self,
            _client_ip: Option<std::net::IpAddr>,
        ) -> Result<Option<GeoInfo>, Report<crate::platform::PlatformError>> {
            Err(Report::new(crate::platform::PlatformError::Geo))
        }
    }

    /// A location in a US opt-out state, whose group grants every modeled
    /// purpose without a signal. Used where a test needs a granted baseline.
    fn us_opt_out_geo() -> GeoInfo {
        GeoInfo {
            city: String::new(),
            country: "US".to_owned(),
            continent: String::new(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: Some("CA".to_owned()),
            asn: None,
        }
    }

    #[test]
    fn a_geo_module_failure_resolves_permissions_at_the_requires_signal_floor() {
        use crate::permissions::Permission;
        use crate::platform::test_support::build_services_with_geo;

        // A located request in a granted-baseline state, so the assertion can
        // only pass by the failure reaching the floor rather than a tree node.
        let settings = create_test_settings();
        let req = create_test_request(&[]);
        let geo = us_opt_out_geo();

        let granted =
            EcContext::read_from_request_with_geo(&settings, &req, &noop_services(), Some(&geo))
                .expect("should read EC context for a located request");
        assert!(
            granted.permissions().is_set(Permission::StoreOnDevice),
            "the located baseline must grant storage, or this test proves nothing"
        );

        let services = build_services_with_geo(std::sync::Arc::new(FailingGeo));
        let failed = EcContext::read_from_request_resolving_geo(&settings, &req, &services)
            .expect("a failed lookup should resolve permissions, not fail the request");
        assert!(
            !failed.permissions().is_set(Permission::StoreOnDevice),
            "a geo module failure must resolve at the requires-signal floor"
        );
        assert_eq!(
            failed.consent().jurisdiction,
            crate::consent::jurisdiction::Jurisdiction::Unknown,
            "a failed lookup must not adopt the policy's declared jurisdiction"
        );
    }

    #[test]
    fn the_resolved_place_decides_the_consent_jurisdiction() {
        use crate::consent::jurisdiction::Jurisdiction;

        let settings = create_test_settings();
        let req = create_test_request(&[]);

        // No location at all: the policy's top node answers.
        let unplaced = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");
        assert_eq!(
            unplaced.consent().jurisdiction,
            Jurisdiction::Gdpr,
            "with no place the top node's jurisdiction should apply"
        );

        // A listed US state names itself as the state.
        let geo = us_opt_out_geo();
        let located =
            EcContext::read_from_request_with_geo(&settings, &req, &noop_services(), Some(&geo))
                .expect("should read EC context");
        assert_eq!(
            located.consent().jurisdiction,
            Jurisdiction::UsState("CA".to_owned()),
            "a listed US state should resolve its own state jurisdiction"
        );
    }

    #[test]
    fn sharing_requires_the_personalised_ads_permission_not_just_storage() {
        let mut ec =
            EcContext::new_for_test(Some(valid_ec_id("a", "ABC123")), ConsentContext::default());
        ec.permissions = PermissionState::new([Permission::StoreOnDevice].into_iter().collect());
        assert!(ec.ec_allowed(), "the module gate is open");
        assert!(
            !ec.ec_sharing_allowed(),
            "storage alone must not allow sharing beyond the edge"
        );
    }

    #[test]
    fn kv_snapshot_distinguishes_non_present_states() {
        assert!(EcKvSnapshot::NotRead.entry_for("ec-1").is_none());
        assert!(
            EcKvSnapshot::Missing {
                ec_id: "ec-1".to_owned()
            }
            .entry_for("ec-1")
            .is_none()
        );
        assert!(
            EcKvSnapshot::Failed {
                ec_id: "ec-1".to_owned()
            }
            .entry_for("ec-1")
            .is_none()
        );
    }

    #[test]
    fn kv_snapshot_present_state_is_bound_to_ec_id() {
        let consent = ConsentContext::default();
        let entry = KvEntry::new(&consent, None, 1_000, "example.com");
        let snapshot = EcKvSnapshot::Present {
            ec_id: "ec-1".to_owned(),
            entry: Box::new(entry.clone()),
            generation: Some(7),
        };

        assert_eq!(snapshot.entry_for("ec-1"), Some(&entry));
        assert_eq!(snapshot.generation_for("ec-1"), Some(7));
        assert!(snapshot.entry_for("ec-2").is_none());
        assert_eq!(snapshot.generation_for("ec-2"), None);
    }

    #[test]
    fn kv_snapshot_retains_persisted_entry_without_generation() {
        let consent = ConsentContext::default();
        let entry = KvEntry::new(&consent, None, 1_000, "example.com");
        let snapshot = EcKvSnapshot::Present {
            ec_id: "ec-1".to_owned(),
            entry: Box::new(entry.clone()),
            generation: None,
        };

        assert_eq!(snapshot.entry_for("ec-1"), Some(&entry));
        assert_eq!(snapshot.generation_for("ec-1"), None);
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
    fn log_id_never_emits_more_than_the_redacted_prefix() {
        // A byte index inside a multi-byte character used to make the
        // truncation fall back to the whole value, printing in full the
        // identifier this redacts.
        let boundary_splitting = "abcdefg\u{e9}-tail-that-must-not-be-logged";
        let redacted = log_id(boundary_splitting);

        assert!(
            !redacted.contains("must-not-be-logged"),
            "should not disclose the rest of the identifier: {redacted}"
        );
        assert_eq!(
            redacted.chars().count(),
            9,
            "should be eight characters plus the ellipsis: {redacted}"
        );

        // The ordinary case is unchanged.
        assert_eq!(log_id("0123456789abcdef.ABC123"), "01234567\u{2026}");
        // A value shorter than the prefix is emitted whole, which is all there is.
        assert_eq!(log_id("abc"), "abc\u{2026}");
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
