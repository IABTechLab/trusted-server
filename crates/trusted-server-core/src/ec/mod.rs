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
pub(crate) mod client_eids;
pub mod consent;
pub mod cookies;
pub mod device;
pub mod eids;
pub mod finalize;
pub mod generation;
pub mod identify;
pub mod identity;
pub mod kv;
pub mod kv_backend;
pub mod kv_types;
pub mod partner;
pub mod prebid_eids;
pub mod pull_sync;
pub mod rate_limiter;
pub mod registry;

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

use std::collections::BTreeMap;
use std::sync::Arc;

use cookie::CookieJar;
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{Request, header};
use url::Url;

use crate::consent::{self as consent_mod, ConsentContext, ConsentPipelineInput};
use crate::constants::{COOKIE_TS_EC, COOKIE_TS_EC_REVOKED, COOKIE_TS_HEM};
use crate::cookies::{CONSENT_COOKIE_NAMES, TS_IDENTITY_COOKIE_NAMES, handle_request_cookies};
use crate::ec::cookies::ec_id_has_only_allowed_chars;
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::http_util::{is_document_navigation, is_prefetch_request};
use crate::platform::RuntimeServices;
use crate::settings::Settings;
use device::DeviceSignals;

use self::finalize::BrowserSeed;
use self::identity::input::HashedEmail;
use self::identity::measurement::{IdentityMeasurement, MeasurementInput, RouteKind};
use self::identity::request::{RequestIdentity, WriterDecision};
use self::identity::runtime::IdentityRuntime;
use self::kv::{CreateIfAbsentOutcome, KvIdentityGraph, apply_partner_id_updates};
use self::kv_types::KvEntry;
use self::rate_limiter::RateLimiter;

/// Bounded request classifications that may stage browser EID cookies.
///
/// Adapters classify publisher navigations and `POST /auction` only after
/// pre-route filters allow dispatch. The shared page-bids handler classifies an
/// admitted SPA navigation, while EC finalization classifies new identities.
/// Challenged or blocked requests remain unclassified and cannot stage EIDs.
///
/// Only [`Self::NewEc`] still reports an `EID sync measurement` line. A
/// returning visitor's request reports the `identity request measurement`
/// line instead, which carries no source label.
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

/// Create attempts a new EC makes, each with a fresh ID after a collision.
const MAX_CREATE_ATTEMPTS: usize = 5;

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

/// The hashed email from the `ts-hem` cookie and the cookies identity
/// providers may read: every first-party cookie except Trusted Server's own
/// and the consent cookies. A malformed `Cookie` header gives neither.
fn provider_cookie_signals(
    req: &Request<EdgeBody>,
) -> (Option<HashedEmail>, BTreeMap<String, String>) {
    let Some(jar) = handle_request_cookies(req).ok().flatten() else {
        return (None, BTreeMap::new());
    };
    let hashed_email = jar
        .get(COOKIE_TS_HEM)
        .and_then(|cookie| HashedEmail::from_cookie_hex(cookie.value()));
    let cookies = jar
        .iter()
        .filter(|cookie| {
            !TS_IDENTITY_COOKIE_NAMES
                .iter()
                .chain(CONSENT_COOKIE_NAMES)
                .any(|reserved| reserved.eq_ignore_ascii_case(cookie.name()))
        })
        .map(|cookie| (cookie.name().to_owned(), cookie.value().to_owned()))
        .collect();
    (hashed_email, cookies)
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
    /// The normalized client IP, captured early before the request body
    /// is consumed. `None` when the platform cannot determine client IP.
    client_ip: Option<String>,
    /// Geo information captured pre-routing for downstream KV writes.
    geo_info: Option<GeoInfo>,
    /// Device signals derived from TLS/H2/UA in the adapter layer.
    /// Set via [`EcContext::set_device_signals`] before
    /// [`EcContext::generate_if_needed`] is called.
    device_signals: Option<DeviceSignals>,
    /// Request-scoped persisted identity-graph state for the active EC ID.
    kv_snapshot: EcKvSnapshot,
    /// Whether this request may rotate an orphaned EC identity.
    recovery_eligible: bool,
    /// Allowed returning-user EID persistence source, assigned only after request filters pass.
    eid_sync_source: Option<EidSyncSource>,
    /// Identity state this request decides and collects.
    identity: RequestIdentity,
    /// Withdrawn EC IDs whose before-send tombstone returned
    /// [`EcKvSnapshot::Failed`], for the after-send retry.
    failed_withdrawals: Vec<String>,
    /// The `ts-ec-revoked` marker the request carried: the consent
    /// fingerprint a global revoke saw (spec 8).
    revoke_marker: Option<String>,
    /// The row of an EC minted by [`EcContext::mint_deferred`], created after
    /// the hold dispatch.
    deferred_create: Option<Box<KvEntry>>,
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
        });

        log::info!(
            "EC context: present={}, cookie_present={}, consent_allowed={}, jurisdiction={}",
            ec_was_present,
            parsed.cookie_ec.is_some(),
            consent::ec_consent_granted(&consent),
            consent.jurisdiction,
        );

        Ok(Self {
            ec_value,
            cookie_ec_value: parsed.cookie_ec,
            ec_was_present,
            ec_generated: false,
            consent,
            client_ip,
            geo_info: geo_info.cloned(),
            device_signals: None,
            kv_snapshot: EcKvSnapshot::NotRead,
            recovery_eligible: false,
            eid_sync_source: None,
            identity: {
                let mut identity = RequestIdentity::default();
                identity.set_cookie_header_bytes(
                    req.headers()
                        .get_all(header::COOKIE)
                        .iter()
                        .map(|value| value.as_bytes().len())
                        .sum(),
                );
                identity
            },
            failed_withdrawals: Vec::new(),
            revoke_marker: parsed
                .jar
                .as_ref()
                .and_then(|jar| jar.get(COOKIE_TS_EC_REVOKED))
                .map(|cookie| cookie.value().to_owned()),
            deferred_create: None,
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
    /// The new row carries `seed`, the request's browser ingest, so a new
    /// visitor's create is the one write (spec 5.2 phase 1). Pass
    /// [`BrowserSeed::empty`] when there is nothing to seed.
    ///
    /// # Errors
    ///
    /// Returns an error if the client IP is unavailable and generation is
    /// needed, or if HMAC generation fails.
    pub fn generate_if_needed(
        &mut self,
        settings: &Settings,
        kv: Option<&KvIdentityGraph>,
        seed: &BrowserSeed,
    ) -> Result<(), Report<TrustedServerError>> {
        let Some(entry) = self.new_entry_if_needed(settings, seed) else {
            return Ok(());
        };
        self.create_entry(settings, kv, entry, MAX_CREATE_ATTEMPTS)
    }

    /// Mints an EC ID as [`generate_if_needed`](Self::generate_if_needed)
    /// would, but keeps its row for
    /// [`complete_deferred_create`](Self::complete_deferred_create), so a new
    /// visitor's hold-mode providers can join the create (spec 4.7).
    ///
    /// No row exists until the create, so nothing may disclose the ID before
    /// it: the hold dispatch calls no provider that reads it.
    ///
    /// # Errors
    ///
    /// Returns an error if the client IP is unavailable or HMAC generation
    /// fails.
    pub fn mint_deferred(
        &mut self,
        settings: &Settings,
        seed: &BrowserSeed,
    ) -> Result<(), Report<TrustedServerError>> {
        let Some(entry) = self.new_entry_if_needed(settings, seed) else {
            return Ok(());
        };
        let ec_id = generation::generate_ec_id(settings, self.require_client_ip()?)?;
        log::info!(
            "Minted new EC ID with a deferred create: {}",
            log_id(&ec_id)
        );
        self.ec_value = Some(ec_id);
        self.ec_generated = true;
        self.deferred_create = Some(Box::new(entry));
        Ok(())
    }

    /// Creates the row [`mint_deferred`](Self::mint_deferred) kept, with
    /// whatever the hold dispatch added. Does nothing without one.
    ///
    /// # Errors
    ///
    /// Returns an error if the create fails or no unique ID is found.
    pub fn complete_deferred_create(
        &mut self,
        settings: &Settings,
        kv: Option<&KvIdentityGraph>,
    ) -> Result<(), Report<TrustedServerError>> {
        let Some(entry) = self.deferred_create.take() else {
            return Ok(());
        };
        // One attempt: the auction may already hold the minted ID, so a
        // collision fails the create instead of minting another.
        self.create_entry(settings, kv, *entry, 1)
    }

    /// Whether a minted EC still waits for its create.
    #[must_use]
    pub fn has_deferred_create(&self) -> bool {
        self.deferred_create.is_some()
    }

    /// The row a minted EC will be created with.
    #[must_use]
    pub(crate) fn deferred_entry(&self) -> Option<&KvEntry> {
        self.deferred_create.as_deref()
    }

    /// Replaces the row a minted EC will be created with.
    pub(crate) fn set_deferred_entry(&mut self, entry: KvEntry) {
        if self.deferred_create.is_some() {
            self.deferred_create = Some(Box::new(entry));
        }
    }

    /// The new row for a request without an EC whose consent allows one, or
    /// `None` when it needs none.
    fn new_entry_if_needed(&self, settings: &Settings, seed: &BrowserSeed) -> Option<KvEntry> {
        if self.ec_value.is_some() {
            return None;
        }

        if !consent::ec_consent_granted(&self.consent) {
            log::info!(
                "EC generation skipped: consent not granted (jurisdiction={})",
                self.consent.jurisdiction,
            );
            return None;
        }

        if self.revoke_blocks_minting() {
            log::info!("EC generation skipped: a global revoke holds until consent changes");
            return None;
        }

        let mut entry = KvEntry::new(
            &self.consent,
            self.geo_info.as_ref(),
            current_timestamp(),
            &settings.publisher.domain,
        );
        entry.device = self
            .device_signals
            .as_ref()
            .map(DeviceSignals::to_kv_device);
        apply_partner_id_updates(&mut entry, seed.updates());
        Some(entry)
    }

    fn require_client_ip(&self) -> Result<&str, Report<TrustedServerError>> {
        self.client_ip.as_deref().ok_or_else(|| {
            Report::new(TrustedServerError::EdgeCookie {
                message: "Client IP required for EC generation but unavailable".to_owned(),
            })
        })
    }

    /// Creates `entry` under the minted EC ID, or a fresh one, retrying an ID
    /// collision with a fresh ID.
    fn create_entry(
        &mut self,
        settings: &Settings,
        kv: Option<&KvIdentityGraph>,
        entry: KvEntry,
        max_attempts: usize,
    ) -> Result<(), Report<TrustedServerError>> {
        let client_ip = self.require_client_ip()?.to_owned();
        let mut minted = self.ec_value.take();
        self.ec_generated = false;

        for attempt in 0..max_attempts {
            let ec_id = match minted.take() {
                Some(ec_id) => ec_id,
                None => {
                    let ec_id = generation::generate_ec_id(settings, &client_ip)?;
                    log::info!("Generated new EC ID: {}", log_id(&ec_id));
                    ec_id
                }
            };

            if let Some(graph) = kv {
                self.identity.record_create_attempt();
                match graph.create_if_absent(&ec_id, &entry) {
                    Ok(CreateIfAbsentOutcome::Written) => {
                        self.kv_snapshot = EcKvSnapshot::Present {
                            ec_id: ec_id.clone(),
                            entry: Box::new(entry),
                            generation: None,
                        };
                    }
                    Ok(CreateIfAbsentOutcome::AlreadyExists) => {
                        log::warn!(
                            "Generated EC ID collision on attempt {}/{max_attempts}",
                            attempt + 1
                        );
                        continue;
                    }
                    Err(err) => {
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
            return Ok(());
        }

        Err(Report::new(TrustedServerError::EdgeCookie {
            message: format!("Failed to allocate a unique EC ID after {max_attempts} attempts"),
        }))
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

    /// Builds the `identity request measurement` for this request after
    /// phase 4 (spec 14, step 1).
    ///
    /// The route kind is `new_ec` for a request that generated an EC,
    /// `writer` for the one browser-driven writer, and `stager` otherwise.
    #[must_use]
    pub fn identity_measurement(&self, input: &MeasurementInput<'_>) -> IdentityMeasurement {
        let route = if self.ec_generated {
            RouteKind::NewEc
        } else if self.identity.writer() {
            RouteKind::Writer
        } else {
            RouteKind::Stager
        };
        IdentityMeasurement {
            cookie_header_bytes: self.identity.cookie_header_bytes(),
            route,
            // A snapshot that came from the new-EC create read nothing, and
            // each create attempt is one write made before routing.
            kv_reads: input
                .kv_reads
                .saturating_add(u32::from(
                    input.phase_one_read && self.identity.create_attempts() == 0,
                ))
                .saturating_add(self.identity.capture_kv_reads()),
            // The capture's pre-send reads and writes go through the
            // request-state graph, which the adapter's counters do not see.
            kv_writes: input
                .kv_writes
                .saturating_add(self.identity.create_attempts())
                .saturating_add(self.identity.capture_kv_writes()),
            flush: input.flush.copied(),
            removal_lost: self.identity.removal_lost(),
            removal_deferred: self.identity.removal_deferred(),
            withdrawal_after_send: input.withdrawal_after_send,
            withdrawal_after_send_failed: input.withdrawal_after_send_failed,
            capture_unpersisted: self.identity.capture_unpersisted().clone(),
        }
    }

    /// Asks finalize to run the consent-withdrawal tombstone path for this
    /// request, as a `revoke_scope = "global"` capture does from any client
    /// (spec 8).
    pub(crate) fn request_global_revoke(&mut self) {
        self.identity.request_global_revoke();
    }

    /// Whether the request carries a global-revoke marker for its own
    /// consent, which stops EC minting (spec 8).
    #[must_use]
    pub(crate) fn revoke_blocks_minting(&self) -> bool {
        self.revoke_marker.as_deref() == Some(cookies::revoke_marker_value(&self.consent).as_str())
    }

    /// Whether the request carries a global-revoke marker at all.
    #[must_use]
    pub(crate) fn has_revoke_marker(&self) -> bool {
        self.revoke_marker.is_some()
    }

    /// Sets the request's global-revoke marker, as if it carried the cookie.
    #[cfg(test)]
    pub(crate) fn set_revoke_marker_for_test(&mut self, marker: &str) {
        self.revoke_marker = Some(marker.to_owned());
    }

    /// Whether a capture asked for the consent-withdrawal tombstone path.
    /// The adapter gives finalize the identity graph when it did.
    #[must_use]
    pub fn global_revoke_requested(&self) -> bool {
        self.identity.global_revoke_requested()
    }

    /// The identity state this request decides and collects.
    #[must_use]
    pub(crate) fn identity(&self) -> &RequestIdentity {
        &self.identity
    }

    /// The identity state this request decides and collects, to fill.
    pub(crate) fn identity_mut(&mut self) -> &mut RequestIdentity {
        &mut self.identity
    }

    /// Gives the request the identity runtime built for the settings load.
    ///
    /// The adapter calls this where it builds the request's EC state.
    pub fn set_identity_runtime(&mut self, runtime: Arc<IdentityRuntime>) {
        self.identity.set_runtime(runtime);
    }

    /// Turns the `ts-eids-s` mirror on for this request, keeping its provider
    /// catalog, or with no provider when it has no runtime yet.
    #[cfg(test)]
    pub(crate) fn enable_mirror_for_test(&mut self) {
        let runtime = match self.identity_runtime() {
            Some(runtime) => runtime.with_mirror_enabled(),
            None => IdentityRuntime::from_settings_with_providers(
                &crate::test_support::tests::create_test_settings(),
                Vec::new(),
                std::collections::BTreeMap::new(),
            )
            .expect("should build an empty runtime")
            .with_mirror_enabled(),
        };
        self.set_identity_runtime(Arc::new(runtime));
    }

    /// Gives the request the limiter that counts provider calls for the burst
    /// cap and the circuit breaker before the response is sent.
    pub fn set_identity_rate_limiter(&mut self, limiter: Arc<dyn RateLimiter + Send + Sync>) {
        self.identity.set_rate_limiter(limiter);
    }

    /// The identity runtime, when the adapter set one.
    #[must_use]
    pub(crate) fn identity_runtime(&self) -> Option<&IdentityRuntime> {
        self.identity.runtime().map(Arc::as_ref)
    }

    /// Records whether the request passed the real-browser check, for a route
    /// that is never the writer.
    pub fn set_real_browser(&mut self, is_real_browser: bool) {
        self.identity.set_real_browser(is_real_browser);
    }

    /// Decides whether this request is the one browser-driven identity writer.
    ///
    /// Only an organic real-browser top-level document navigation that is not
    /// a prefetch writes (spec 5.1). Also captures the user agent, `DNT`, the
    /// page URL without query or fragment, the hashed email from the `ts-hem`
    /// cookie, and the first-party cookies identity providers may declare.
    ///
    /// The page URL is read from the request URI, so it needs an absolute
    /// URI. The Fastly adapter gives one (`edgezero-adapter-fastly` builds the
    /// request from `get_url_str()`); a request with an origin-form URI has no
    /// page URL, and a provider that wants the `PageUrl` input is then not
    /// due.
    pub fn decide_writer(
        &mut self,
        req: &Request<EdgeBody>,
        organic_route: bool,
        is_real_browser: bool,
    ) {
        let writer = organic_route
            && is_document_navigation(req)
            && !is_prefetch_request(req)
            && is_real_browser;
        let user_agent = req
            .headers()
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let dnt = req
            .headers()
            .get("dnt")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.trim() == "1");
        let page_url = Url::parse(&req.uri().to_string()).ok().map(|mut url| {
            url.set_query(None);
            url.set_fragment(None);
            url
        });
        let (hashed_email, cookies) = provider_cookie_signals(req);
        self.identity.set_writer_decision(WriterDecision {
            writer,
            is_real_browser,
            user_agent,
            dnt,
            page_url,
            hashed_email,
            cookies,
        });
    }

    /// Decides whether a `/_ts/page-bids` call is the SPA writer (spec 5.1):
    /// a real browser, `Sec-Fetch-Site: same-origin` and an interval above
    /// zero. The flush then checks the interval against the row's last
    /// writer flush. Reads the real-browser flag set earlier with
    /// [`set_real_browser`](Self::set_real_browser).
    pub fn decide_spa_writer(&mut self, req: &Request<EdgeBody>, interval_sec: u64) {
        let same_origin = req
            .headers()
            .get("sec-fetch-site")
            .and_then(|value| value.to_str().ok())
            == Some("same-origin");
        if !(same_origin && interval_sec > 0 && self.identity.is_real_browser()) {
            return;
        }
        let (hashed_email, cookies) = provider_cookie_signals(req);
        self.identity.set_writer_decision(WriterDecision {
            writer: true,
            is_real_browser: true,
            user_agent: req
                .headers()
                .get(header::USER_AGENT)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            dnt: req
                .headers()
                .get("dnt")
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.trim() == "1"),
            page_url: None,
            hashed_email,
            cookies,
        });
        self.identity.set_spa_writer(true);
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

    /// Returns the withdrawn EC IDs whose tombstone has failed and not yet
    /// succeeded.
    ///
    /// Before the after-send retry these are the before-send failures. After
    /// it, only the IDs that failed a second time remain, which is what the
    /// failure counters count.
    #[must_use]
    pub(crate) fn failed_withdrawals(&self) -> &[String] {
        &self.failed_withdrawals
    }

    /// Whether any withdrawn EC ID's tombstone is still unwritten.
    #[must_use]
    pub fn has_failed_withdrawals(&self) -> bool {
        !self.failed_withdrawals.is_empty()
    }

    /// Whether a capture removal write failed before send and awaits its
    /// one retry after send.
    #[must_use]
    pub fn has_pending_removals(&self) -> bool {
        self.identity.pending_removals().is_some()
    }

    /// Whether a capture kept positive writes for the pending key, which
    /// [`identity::flush::defer_capture_writes`] appends after send.
    #[must_use]
    pub fn has_capture_writes(&self) -> bool {
        !self.identity.capture_writes().is_empty()
    }

    /// Forgets a withdrawn EC ID whose retried tombstone was written.
    pub(crate) fn clear_failed_withdrawal(&mut self, ec_id: &str) {
        self.failed_withdrawals.retain(|id| id != ec_id);
    }

    /// Records a withdrawn EC ID whose before-send tombstone failed.
    pub(crate) fn record_failed_withdrawal(&mut self, ec_id: &str) {
        if !self.failed_withdrawals.iter().any(|id| id == ec_id) {
            self.failed_withdrawals.push(ec_id.to_owned());
        }
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

    /// Replaces an orphaned active ID after its new backing row is persisted.
    pub(crate) fn replace_with_generated(&mut self, ec_id: String, snapshot: EcKvSnapshot) {
        self.ec_value = Some(ec_id);
        self.ec_generated = true;
        self.kv_snapshot = snapshot;
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
    /// present on the request. This always returns the cookie value.
    #[must_use]
    pub fn existing_cookie_ec_id(&self) -> Option<&str> {
        self.cookie_ec_value.as_deref()
    }

    /// Returns `true` when the request carried a cookie EC and the selected
    /// active EC differs from that cookie value.
    #[must_use]
    pub fn cookie_differs_from_active_ec(&self) -> bool {
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
            kv_snapshot: EcKvSnapshot::NotRead,
            recovery_eligible: false,
            eid_sync_source: None,
            identity: RequestIdentity::default(),
            failed_withdrawals: Vec::new(),
            revoke_marker: None,
            deferred_create: None,
        }
    }

    /// Creates a test-only [`EcContext`] for a real browser's writer
    /// navigation, with explicit client IP.
    #[cfg(test)]
    #[must_use]
    pub fn new_writer_for_test_with_ip(
        ec_value: Option<String>,
        consent: ConsentContext,
        client_ip: Option<String>,
    ) -> Self {
        let mut context = Self::new_for_test_with_ip(ec_value, consent, client_ip);
        context.identity.set_writer_decision(WriterDecision {
            writer: true,
            is_real_browser: true,
            user_agent: None,
            dnt: false,
            page_url: None,
            hashed_email: None,
            cookies: BTreeMap::new(),
        });
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
            client_ip,
            geo_info: None,
            device_signals: None,
            kv_snapshot: EcKvSnapshot::NotRead,
            recovery_eligible: false,
            eid_sync_source: None,
            identity: RequestIdentity::default(),
            failed_withdrawals: Vec::new(),
            revoke_marker: None,
            deferred_create: None,
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
            kv_snapshot: EcKvSnapshot::NotRead,
            recovery_eligible: false,
            eid_sync_source: None,
            identity: RequestIdentity::default(),
            failed_withdrawals: Vec::new(),
            revoke_marker: None,
            deferred_create: None,
        }
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

/// Returns the current time as a [`std::time::SystemTime`] without calling
/// `std::time::SystemTime::now()`, which panics on `wasm32-unknown-unknown`.
///
/// Use this for APIs that only accept the std type, such as `httpdate` or
/// chrono's `DateTime::from`. It reads the wasm-safe [`web_time::SystemTime`]
/// clock and adds the elapsed time to [`std::time::UNIX_EPOCH`], which is pure
/// arithmetic. On native and `wasm32-wasip1`, [`web_time`] delegates to the
/// std clock, so this equals `std::time::SystemTime::now()` there.
pub(crate) fn std_system_time_now() -> std::time::SystemTime {
    std::time::UNIX_EPOCH
        + web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::{ConsentContext, ConsentSource};
    use crate::ec::kv_backend::test_support::InMemoryEcKv;
    use crate::ec::kv_backend::{
        EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome,
    };
    use crate::platform::test_support::noop_services;
    use crate::test_support::tests::create_test_settings;

    #[test]
    fn decide_spa_writer_needs_a_same_origin_browser_and_an_interval() {
        let page_bids = |site: Option<&str>| {
            let mut builder = Request::builder()
                .uri("https://www.example.com/_ts/page-bids?path=/a")
                .header("x-tsjs-page-bids", "1");
            if let Some(site) = site {
                builder = builder.header("sec-fetch-site", site);
            }
            builder
                .body(EdgeBody::empty())
                .expect("should build the request")
        };
        let decide = |request: &Request<EdgeBody>, browser: bool, interval: u64| {
            let mut context = EcContext::default();
            context.set_real_browser(browser);
            context.decide_spa_writer(request, interval);
            (context.identity().writer(), context.identity().spa_writer())
        };

        assert_eq!(
            decide(&page_bids(Some("same-origin")), true, 300),
            (true, true),
            "should make a same-origin browser page-bids call the SPA writer"
        );
        for (name, request, browser, interval) in [
            ("no Sec-Fetch-Site", page_bids(None), true, 300),
            ("same-site", page_bids(Some("same-site")), true, 300),
            ("not a browser", page_bids(Some("same-origin")), false, 300),
            ("interval off", page_bids(Some("same-origin")), true, 0),
        ] {
            assert_eq!(
                decide(&request, browser, interval),
                (false, false),
                "should not be the writer: {name}"
            );
        }
    }

    #[test]
    fn decide_writer_needs_every_condition() {
        let navigation = || {
            Request::builder()
                .uri("https://www.example.com/a?b=c#d")
                .header("sec-fetch-dest", "document")
                .header(header::USER_AGENT, "ExampleBrowser/1.0")
                .header("dnt", "1")
                .body(EdgeBody::empty())
                .expect("should build the request")
        };
        let mut context = EcContext::default();
        context.decide_writer(&navigation(), true, true);
        assert!(
            context.identity().writer(),
            "should make an organic real-browser document navigation the writer"
        );
        assert!(
            context.identity().is_real_browser(),
            "should record the real browser"
        );
        assert_eq!(
            context.identity().page_url().map(Url::as_str),
            Some("https://www.example.com/a"),
            "should drop the query and fragment"
        );
        assert_eq!(
            context.identity().user_agent(),
            Some("ExampleBrowser/1.0"),
            "should capture the user agent"
        );
        assert!(context.identity().dnt(), "should capture DNT");

        for (name, organic, browser, request) in [
            ("named route", false, true, navigation()),
            ("bot", true, false, navigation()),
            (
                "prefetch",
                true,
                true,
                Request::builder()
                    .header("sec-fetch-dest", "document")
                    .header("sec-purpose", "prefetch")
                    .body(EdgeBody::empty())
                    .expect("should build the request"),
            ),
        ] {
            let mut context = EcContext::default();
            context.decide_writer(&request, organic, browser);
            assert!(
                !context.identity().writer(),
                "should not be a writer: {name}"
            );
            assert_eq!(
                context.identity().is_real_browser(),
                browser,
                "should keep the real-browser flag: {name}"
            );
        }
    }

    #[test]
    fn decide_writer_has_no_page_url_for_an_origin_form_uri() {
        let request = Request::builder()
            .uri("/a?b=c")
            .header("sec-fetch-dest", "document")
            .body(EdgeBody::empty())
            .expect("should build the request");
        let mut context = EcContext::default();

        context.decide_writer(&request, true, true);

        assert!(
            context.identity().writer(),
            "control: the request is still the writer"
        );
        assert_eq!(
            context.identity().page_url(),
            None,
            "should have no page URL without an absolute URI"
        );
    }

    /// [`EcKvStore`] wrapper whose first `collisions` `Add` writes report a
    /// precondition failure, forcing generation to retry with a fresh suffix.
    struct AddCollidingEcKv {
        inner: InMemoryEcKv,
        collisions_remaining: std::sync::Mutex<u32>,
    }

    impl AddCollidingEcKv {
        fn new(collisions: u32) -> Self {
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
            EcContext::new_for_test_with_ip(None, granting_consent(), Some("192.0.2.5".to_owned()));
        let graph = KvIdentityGraph::new(AddCollidingEcKv::new(2));

        ec.generate_if_needed(&settings, Some(&graph), &BrowserSeed::empty())
            .expect("should generate after bounded collisions");

        assert!(ec.ec_value().is_some(), "should allocate a fresh EC ID");
        assert!(ec.ec_generated(), "should mark the EC as generated");
        assert!(
            matches!(ec.kv_snapshot(), EcKvSnapshot::Present { .. }),
            "generation should seed a present snapshot"
        );
    }

    #[test]
    fn a_revoke_marker_for_this_consent_stops_minting_until_consent_changes() {
        let settings = create_test_settings();
        let mint_under = |consent: ConsentContext, marker: &str| {
            let mut ec =
                EcContext::new_for_test_with_ip(None, consent, Some("192.0.2.5".to_owned()));
            ec.set_revoke_marker_for_test(marker);
            let graph = KvIdentityGraph::in_memory("ec-store");
            ec.generate_if_needed(&settings, Some(&graph), &BrowserSeed::empty())
                .expect("should not error");
            ec.ec_generated()
        };
        let same = cookies::revoke_marker_value(&granting_consent());
        let mut with_gpc = granting_consent();
        with_gpc.gpc = true;
        let earlier = cookies::revoke_marker_value(&with_gpc);

        assert_ne!(earlier, same, "precondition: GPC changes the marker value");
        assert!(
            !mint_under(granting_consent(), &same),
            "should not mint while the consent the revoke saw is unchanged"
        );
        assert!(
            mint_under(granting_consent(), &earlier),
            "should mint once the consent differs from the one the revoke saw"
        );
    }

    #[test]
    fn read_from_request_reads_the_revoke_marker() {
        let settings = create_test_settings();
        let req = create_test_request(&[("cookie", "ts-ec-revoked=10009")]);

        let ec = EcContext::read_from_request(&settings, &req, &noop_services())
            .expect("should read EC context");

        assert!(ec.has_revoke_marker(), "should read the marker cookie");
        let plain =
            EcContext::read_from_request(&settings, &create_test_request(&[]), &noop_services())
                .expect("should read EC context");
        assert!(!plain.has_revoke_marker(), "control: no cookie, no marker");
    }

    #[test]
    fn mint_deferred_writes_nothing_until_the_create() {
        let settings = create_test_settings();
        let graph = KvIdentityGraph::in_memory("ec-store");
        let mut ec =
            EcContext::new_for_test_with_ip(None, granting_consent(), Some("192.0.2.5".to_owned()));

        ec.mint_deferred(&settings, &BrowserSeed::empty())
            .expect("should mint");

        let ec_id = ec.ec_value().expect("should hold the minted ID").to_owned();
        assert!(ec.has_deferred_create(), "should keep the row for later");
        assert!(
            graph.get(&ec_id).expect("should read").is_none(),
            "should write nothing yet"
        );

        ec.complete_deferred_create(&settings, Some(&graph))
            .expect("should create");

        assert_eq!(
            ec.ec_value(),
            Some(ec_id.as_str()),
            "should keep the minted ID"
        );
        assert!(
            graph.get(&ec_id).expect("should read").is_some(),
            "should create the row"
        );
        assert!(
            matches!(ec.kv_snapshot(), EcKvSnapshot::Present { .. }),
            "should hold the created row"
        );
    }

    #[test]
    fn deferred_create_fails_on_a_collision_instead_of_reminting() {
        let settings = create_test_settings();
        let graph = KvIdentityGraph::new(AddCollidingEcKv::new(1));
        let mut ec =
            EcContext::new_for_test_with_ip(None, granting_consent(), Some("192.0.2.5".to_owned()));
        ec.mint_deferred(&settings, &BrowserSeed::empty())
            .expect("should mint");

        let result = ec.complete_deferred_create(&settings, Some(&graph));

        assert!(result.is_err(), "should fail rather than mint another ID");
        assert!(ec.ec_value().is_none(), "should keep no EC");
        assert!(!ec.ec_generated(), "should not count as generated");
    }

    #[test]
    fn generate_if_needed_errors_after_collision_exhaustion() {
        let settings = create_test_settings();
        let mut ec =
            EcContext::new_for_test_with_ip(None, granting_consent(), Some("192.0.2.6".to_owned()));
        // Collide on every attempt so the bounded retry is exhausted.
        let graph = KvIdentityGraph::new(AddCollidingEcKv::new(u32::MAX));

        let result = ec.generate_if_needed(&settings, Some(&graph), &BrowserSeed::empty());

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
    fn provider_cookie_signals_read_the_hem_and_hide_identity_and_consent_cookies() {
        let hem = "ab".repeat(32);
        let cookie = format!(
            "ts-hem={hem}; ts-ec=ec-value; ts-eids=eids; euconsent-v2=tc; __gpp=gpp; \
             us_privacy=1YNN; _lc2_fpi=fpi-example; other=value"
        );
        let req = create_test_request(&[("cookie", &cookie)]);

        let (hashed_email, cookies) = provider_cookie_signals(&req);

        assert_eq!(
            hashed_email.map(|email| email.to_hex()),
            Some(hem),
            "should read the ts-hem digest"
        );
        let names: Vec<&str> = cookies.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            ["_lc2_fpi", "other"],
            "should offer only first-party cookies that are not TS identity or consent cookies"
        );
        assert_eq!(cookies["_lc2_fpi"], "fpi-example", "should keep the value");
    }

    #[test]
    fn provider_cookie_signals_hide_reserved_cookies_in_any_case() {
        let req = create_test_request(&[("cookie", "TS-EC=x; Ts-Hem=y; EUCONSENT-V2=z; keep=1")]);

        let (_, cookies) = provider_cookie_signals(&req);

        let names: Vec<&str> = cookies.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            ["keep"],
            "should match reserved names case-insensitively"
        );
    }

    #[test]
    fn provider_cookie_signals_ignore_a_hem_that_is_not_a_digest() {
        for value in ["", "person@example.com", &"zz".repeat(32), &"ab".repeat(31)] {
            let cookie = format!("ts-hem={value}");
            let req = create_test_request(&[("cookie", &cookie)]);

            let (hashed_email, cookies) = provider_cookie_signals(&req);

            assert!(
                hashed_email.is_none(),
                "should not read `{value}` as a digest"
            );
            assert!(cookies.is_empty(), "should never offer ts-hem as a cookie");
        }
    }

    #[test]
    fn provider_cookie_signals_are_empty_without_a_cookie_header() {
        let req = create_test_request(&[]);

        let (hashed_email, cookies) = provider_cookie_signals(&req);

        assert!(hashed_email.is_none(), "should have no HEM");
        assert!(cookies.is_empty(), "should have no cookies");
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
        ec.generate_if_needed(&settings, None, &BrowserSeed::empty())
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

    #[test]
    fn std_system_time_now_tracks_std_clock() {
        let before = std::time::SystemTime::now();

        let now = std_system_time_now();

        let after = std::time::SystemTime::now();
        assert!(
            before <= now && now <= after,
            "should fall between std clock reads taken before and after"
        );
    }
}
