//! HTTP endpoint handlers for auction requests.

use std::collections::{HashMap, HashSet};

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{Request, Response, StatusCode, header};
use serde_json::value::RawValue;

use crate::auction::formats::AdRequest;
use crate::auction::orchestrator::OrchestrationResult;
use crate::consent::{
    allows_eid_persistence, consent_allows_server_side_auction, gate_eids_by_consent,
};
use crate::constants::COOKIE_TS_EIDS;
use crate::cookies::extract_cookie_value;
use crate::ec::EcContext;
use crate::ec::EcKvSnapshot;
use crate::ec::checked_current_timestamp;
use crate::ec::client_eids::parse_client_eids;
use crate::ec::current_timestamp;
use crate::ec::eids::{ServerSet, server_set};
use crate::ec::identity::IdentityRuntime;
use crate::ec::identity::UnixTime;
use crate::ec::identity::catalog::ProviderCatalog;
use crate::ec::identity::coordinator::HoldResults;
use crate::ec::identity::denials::{Denials, denials, filter_browser_candidates};
use crate::ec::identity::mirror::{MirrorPayload, MirrorRead, MirrorSource, mirror_payload};
use crate::ec::identity::policy::PolicySignals;
use crate::ec::identity::request::opted_out;
use crate::ec::identity::servable::ServeContext;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::partner::normalize_partner_source_domain;
use crate::ec::prebid_eids::parse_prebid_eids_cookie;
use crate::ec::registry::PartnerRegistry;
use crate::error::TrustedServerError;
use crate::openrtb::Eid;
use crate::platform::RuntimeServices;
use crate::settings::Settings;

use super::AuctionOrchestrator;
use super::formats::{
    convert_to_openrtb_response_with_mirror, convert_to_openrtb_response_with_report,
    convert_tsjs_to_auction_request,
};
use super::telemetry::{
    AuctionObservationContext, AuctionSource, AuctionTerminalOutcome, build_auction_events,
    emit_auction_events_best_effort_lazy,
};
use super::types::AuctionContext;

/// Maximum accepted JSON body size for `/auction`. Picked to comfortably fit
/// the largest realistic Prebid-derived auction request (hundreds of ad units
/// with EID arrays) while preventing an authenticated client from consuming
/// arbitrary WASM linear memory.
const MAX_AUCTION_BODY_SIZE: usize = 256 * 1024;

/// Handle auction request from `POST /auction`.
///
/// Accepts a JSON body matching [`AdRequest`][`super::formats::AdRequest`].
/// The minimum valid request is:
///
/// ```json
/// {
///   "adUnits": [{
///     "code": "atf_sidebar_ad",
///     "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
///   }]
/// }
/// ```
///
/// ## Bidder params: inline vs. stored-request
///
/// Each ad unit's `bids` array is **optional**. When absent or empty the PBS
/// integration falls back to a stored-request keyed by the unit's `code`
/// field (`imp.ext.prebid.storedrequest = { id: "<code>" }`). A PBS stored
/// request must therefore exist for every slot code that omits inline params.
///
/// When `bids` is supplied, each entry's `bidder`/`params` pair is forwarded
/// directly as `imp.ext.prebid.bidder.<bidder>`.
///
/// APS `OpenRTB` demand is never forwarded through Prebid Server. An ad unit
/// whose only bidder is `aps` intentionally does not use PBS stored-request
/// fallback; configure a non-APS PBS bidder for stored-request demand instead.
///
/// ## Context passthrough (`config`)
///
/// The optional `config` object is filtered through
/// [`auction.allowed_context_keys`][`crate::settings::AuctionConfig::allowed_context_keys`].
/// Only keys listed there reach the auction providers (e.g. `"permutive_segments"`).
/// All other keys are silently dropped. Values must be either strings or arrays of
/// strings.
///
/// ## Response
///
/// Returns an `OpenRTB 2.x` response. Creative HTML is inlined in each bid's
/// `adm` field after mandatory server-side sanitization. First-party resource
/// and click URL rewriting plus creative TSJS injection are enabled by default;
/// setting [`auction.rewrite_creatives`][`crate::auction_config_types::AuctionConfig::rewrite_creatives`]
/// to `false` skips only that rewrite pass.
///
/// ## Scroll, refresh, and SPA navigation
///
/// This endpoint is intended for **initial page render** and **programmatic
/// callers** (e.g. slim-Prebid, native apps, server-to-server integrations).
/// It is **not** the intended path for scroll or GPT refresh events.
///
/// **SPA navigation** is handled by `GET /_ts/page-bids`: the client-side SPA
/// hook (`installSpaAuctionHook`) intercepts `pushState`/`replaceState`/`popstate`
/// events and calls that endpoint to fetch fresh slots and bids for each new
/// route, then invokes `window.tsjs.adInit()` with the updated data.
///
/// **Scroll and GPT refresh** are owned by slim-Prebid in Phase 1: it runs
/// post-`window.load`, listens for GPT refresh events, and runs client-side
/// auctions independently of this endpoint.
///
/// A slot-template-aware refresh API (`POST /auction/refresh`) is deferred to a
/// future phase and not designed here.
///
/// # Errors
///
/// Returns an error if:
/// - The request body cannot be parsed
/// - The auction request conversion fails (e.g., invalid ad units)
/// - The auction execution fails
/// - The response cannot be serialized
pub async fn handle_auction(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    kv: Option<&KvIdentityGraph>,
    registry: Option<&PartnerRegistry>,
    ec_context: &mut EcContext,
    services: &RuntimeServices,
    req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    // Reject oversized bodies before core buffers/parses them. The Content-Length
    // pre-check stops well-behaved clients early; the post-read check defends
    // against clients that lie about (or omit) the header.
    let content_length_exceeded = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|length| length > MAX_AUCTION_BODY_SIZE);
    if content_length_exceeded {
        return Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(EdgeBody::from(format!(
                "Request body exceeds {MAX_AUCTION_BODY_SIZE} byte limit"
            )))
            .change_context(TrustedServerError::Auction {
                message: "Auction request body exceeded maximum size".to_string(),
            });
    }

    let (parts, body) = req.into_parts();
    let body_bytes = body.into_bytes().unwrap_or_default();
    if body_bytes.len() > MAX_AUCTION_BODY_SIZE {
        return Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header(header::CONTENT_TYPE, "text/plain")
            .body(EdgeBody::from(format!(
                "Request body exceeds {MAX_AUCTION_BODY_SIZE} byte limit"
            )))
            .change_context(TrustedServerError::Auction {
                message: "Auction request body exceeded maximum size".to_string(),
            });
    }
    let body: AdRequest =
        serde_json::from_slice(&body_bytes).change_context(TrustedServerError::BadRequest {
            message: "Failed to parse auction request body".to_string(),
        })?;

    log::info!(
        "Auction request received for {} ad units",
        body.ad_units.len()
    );

    let http_req = Request::from_parts(parts, EdgeBody::empty());

    // Story 5 middleware contract: auction is a read-only EC route.
    // It must not generate EC IDs; it only consumes pre-routed context.
    // Only forward the EC ID to auction partners when consent allows it.
    // Owned so the identity-graph snapshot can be stored back on `ec_context`
    // below without holding a borrow of it across the mutation.
    let ec_id = if ec_context.ec_allowed() {
        ec_context.ec_value().map(str::to_owned)
    } else {
        None
    };
    let consent_context = ec_context.consent().clone();

    if !orchestrator.is_enabled() {
        log::info!("/auction: auction is disabled; returning no-bid response");
        let auction_request = convert_tsjs_to_auction_request(
            &body,
            settings,
            services,
            &http_req,
            consent_context,
            ec_id.as_deref(),
            None,
        )?;
        let observation = AuctionObservationContext::from_auction_request(
            AuctionSource::AuctionApi,
            &auction_request,
            ec_context,
        );
        let elapsed_ms = observation.elapsed_ms();
        emit_auction_events_best_effort_lazy(services, || {
            build_auction_events(
                observation,
                AuctionTerminalOutcome::Skipped {
                    reason: "auction_disabled",
                    elapsed_ms,
                },
            )
        })
        .await;

        let empty_result = OrchestrationResult {
            provider_responses: Vec::new(),
            mediator_response: None,
            winning_bids: HashMap::new(),
            total_time_ms: 0,
            metadata: HashMap::new(),
        };
        return convert_to_openrtb_response_with_mirror(
            &empty_result,
            settings,
            &auction_request,
            ec_context.ec_allowed(),
            mirror_member(
                &EcKvSnapshot::NotRead,
                None,
                ec_context,
                PolicySignals::from_headers(http_req.headers()),
            ),
        );
    }

    // Server-side auction consent gate. The publisher-navigation and
    // `/_ts/page-bids` paths fail closed for GDPR/unknown jurisdictions that
    // lack effective TCF Purpose 1. `/auction` is the programmatic entry point
    // for the same server-side auction, so it must gate identically: returning
    // a no-bid response here prevents outbound PBS/APS calls and the forwarding
    // of request-derived signals (UA/IP/geo, and cookies under some Prebid
    // consent-forwarding modes) for traffic that must not run an auction.
    if !consent_allows_server_side_auction(&consent_context) {
        log::info!(
            "/auction: server-side auction consent gate denied; returning no-bid response without contacting providers"
        );
        // Build the request shape locally (no outbound calls, no geo lookup, no
        // EID resolution) so the no-bid OpenRTB response echoes the request id.
        let auction_request = convert_tsjs_to_auction_request(
            &body,
            settings,
            services,
            &http_req,
            consent_context,
            ec_id.as_deref(),
            None,
        )?;
        let observation = AuctionObservationContext::from_auction_request(
            AuctionSource::AuctionApi,
            &auction_request,
            ec_context,
        );
        emit_auction_events_best_effort_lazy(services, || {
            build_auction_events(
                observation,
                AuctionTerminalOutcome::Skipped {
                    reason: "consent_denied",
                    elapsed_ms: 0,
                },
            )
        })
        .await;

        let empty_result = OrchestrationResult {
            provider_responses: Vec::new(),
            mediator_response: None,
            winning_bids: HashMap::new(),
            total_time_ms: 0,
            metadata: HashMap::new(),
        };
        // A denial clears the mirror so a withdrawal reaches the cookie
        // (spec 5.6).
        return convert_to_openrtb_response_with_mirror(
            &empty_result,
            settings,
            &auction_request,
            ec_context.ec_allowed(),
            Some(MirrorPayload::Clear.to_json()),
        );
    }

    // Parse client-provided EIDs from the current request body. When the
    // current request does not include them, fall back to the persisted
    // `ts-eids` cookie so later requests can still forward the browser's
    // full OpenRTB-style EID structure.
    //
    // Gate this on the same identity-consent condition as the EC ID
    // (`ec_id.is_some()`, which is already filtered by `ec_context.ec_allowed()`).
    // Otherwise a US/GPC or US-Privacy opt-out context — where EC identity use is
    // denied but a non-personalized auction may still run — could forward
    // persistent client EIDs from the body/cookie, since `gate_eids_by_consent`
    // only strips on TCF/GDPR signals. This matches the publisher and
    // `/_ts/page-bids` paths, which also resolve client EIDs only when
    // `ec_id.is_some()`.
    let client_eids = if ec_id.is_some() {
        resolve_client_auction_eids(
            body.eids.as_deref(),
            extract_cookie_value(&http_req, COOKIE_TS_EIDS).as_deref(),
        )
    } else {
        None
    };
    // The browser's EIDs feed the merge below and nothing else: `/auction` is
    // a stager, so they never reach KV (spec 5.3, G10).

    // Resolve partner EIDs from the KV identity graph when the user has a valid
    // EC and both KV and partner stores are available. Gate the read on a
    // present registry: without one, `resolve_auction_eids` yields no
    // server-side EIDs, so the snapshot would be an unused billable KV read.
    let auction_kv_snapshot = match (kv, ec_id.as_deref(), registry) {
        (Some(graph), Some(ec_id), Some(_)) => graph.load_snapshot(ec_id),
        _ => EcKvSnapshot::NotRead,
    };
    // Hand the loaded row to the request context so response finalization,
    // which runs on an EC context the adapter owns after this handler
    // returns, reuses this read instead of paying for a second lookup.
    if !matches!(auction_kv_snapshot, EcKvSnapshot::NotRead) {
        ec_context.set_kv_snapshot(auction_kv_snapshot.clone());
    }
    let signals = PolicySignals::from_headers(http_req.headers());
    let identity = auction_identity(&auction_kv_snapshot, registry, ec_context, signals);
    let mirror = identity.mirror_member(ec_context);

    // Look up geo for device info.
    let geo = services
        .geo()
        .lookup(services.client_info().client_ip)
        .unwrap_or_else(|e| {
            log::warn!("geo lookup failed: {e}");
            None
        });

    // Convert tsjs request format to auction request
    let mut auction_request = convert_tsjs_to_auction_request(
        &body,
        settings,
        services,
        &http_req,
        consent_context,
        ec_id.as_deref(),
        geo,
    )?;

    // Forward the browser entries that pass the provider denials, append the
    // server set for sources the browser did not send, then apply consent
    // gating.
    let merged_eids = identity.merge(client_eids.as_deref());
    let had_eids = merged_eids.as_ref().is_some_and(|v| !v.is_empty());
    auction_request.user.eids =
        gate_eids_by_consent(merged_eids, auction_request.user.consent.as_ref());
    if had_eids && auction_request.user.eids.is_none() {
        log::warn!("Auction EIDs stripped by TCF consent gating");
    }

    // Create auction context
    let context = AuctionContext {
        settings,
        request: &http_req,
        timeout_ms: settings.auction.timeout_ms,
        transport_timeout_ms: settings.auction.timeout_ms,
        provider_responses: None,
        services,
    };

    let observation = AuctionObservationContext::from_auction_request(
        AuctionSource::AuctionApi,
        &auction_request,
        ec_context,
    );

    // Run the auction
    let result = match orchestrator.run_auction(&auction_request, &context).await {
        Ok(result) => result,
        Err(err) => {
            let elapsed_ms = observation.elapsed_ms();
            emit_auction_events_best_effort_lazy(services, || {
                build_auction_events(
                    observation,
                    AuctionTerminalOutcome::ExecutionFailed {
                        request: Some(&auction_request),
                        provider_responses: &[],
                        reason: "execution_failed",
                        elapsed_ms,
                    },
                )
            })
            .await;
            return Err(err.change_context(TrustedServerError::Auction {
                message: "Auction orchestration failed".to_string(),
            }));
        }
    };

    let conversion = match convert_to_openrtb_response_with_report(
        &result,
        settings,
        &auction_request,
        ec_context.ec_allowed(),
        mirror,
    ) {
        Ok(conversion) => conversion,
        Err(error) => {
            let elapsed_ms = observation.elapsed_ms();
            emit_auction_events_best_effort_lazy(services, || {
                build_auction_events(
                    observation,
                    AuctionTerminalOutcome::ExecutionFailed {
                        request: Some(&auction_request),
                        provider_responses: &result.provider_responses,
                        reason: "response_conversion_failed",
                        elapsed_ms,
                    },
                )
            })
            .await;
            return Err(error);
        }
    };

    emit_auction_events_best_effort_lazy(services, || {
        build_auction_events(
            observation,
            AuctionTerminalOutcome::Completed {
                request: &auction_request,
                result: &result,
                delivered_winner_slots: Some(&conversion.delivery.delivered_winner_slots),
            },
        )
    })
    .await;

    log::info!(
        "Auction completed: {} providers, {} delivered winning bids, {} dropped winners, {}ms total",
        result.provider_responses.len(),
        conversion.delivery.delivered_winner_slots.len(),
        conversion.delivery.dropped_winner_count,
        result.total_time_ms
    );

    Ok(conversion.response)
}

/// What the row a request read means for the `ts-eids-s` mirror (spec 6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MirrorState {
    /// No KV read for this request's EC: no mirror member.
    NotRead,
    /// EC use is denied, or the row is missing, a tombstone or withdrawn.
    Cleared,
    /// A live row: the mirror carries the servable server set.
    Live,
}

/// What one request's KV read gives the auction, resolved once (spec 7.1).
///
/// Servability is decided once, at one clock read, and both the bidstream
/// merge and the `ts-eids-s` mirror read the same [`ServerSet`].
pub(crate) struct AuctionIdentity {
    /// `None` when the request may not use KV IDs at all (no registry, no
    /// EC, or EC use denied).
    server: Option<ServerSet>,
    denials: Denials,
    mirror: MirrorState,
    /// The clock read servability used, when there was one.
    now: Option<u64>,
}

impl AuctionIdentity {
    /// The server set's EIDs, before browser precedence.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn server_eids(&self) -> Option<Vec<Eid>> {
        self.server.as_ref().map(ServerSet::eids)
    }

    /// Overlays the answers of the hold dispatch before this navigation's
    /// auction onto the server set. A request that may not use KV IDs stays
    /// without a set.
    pub(crate) fn add_hold_results(
        &mut self,
        results: HoldResults,
        registry: &PartnerRegistry,
        catalog: &ProviderCatalog,
    ) {
        if let Some(server) = self.server.as_mut() {
            server.with_hold_results(results, registry, catalog);
        }
    }

    /// The request's bidstream EIDs before consent gating: the browser's
    /// candidates after the denials, then the server set (spec 7.1).
    #[must_use]
    pub(crate) fn merge(&self, client_eids: Option<&[Eid]>) -> Option<Vec<Eid>> {
        let empty = ServerSet::default();
        let merged = merge_auction_eids(
            client_eids.unwrap_or_default(),
            self.server.as_ref().unwrap_or(&empty),
            &self.denials,
        );
        if merged.is_empty() {
            None
        } else {
            Some(merged)
        }
    }

    /// The mirror member of an `/auction` or `/_ts/page-bids` response
    /// (spec 6.2).
    ///
    /// The mirror carries the same server set as the bidstream, provenance
    /// defaults included, and never an ID consent would strip.
    ///
    /// `None` while `[ec.identity] mirror_enabled` is off or the request has
    /// no identity runtime: the response then carries no mirror member.
    #[must_use]
    pub(crate) fn mirror_member(&self, ec_context: &EcContext) -> Option<serde_json::Value> {
        if !mirror_enabled(ec_context) {
            return None;
        }
        let sources = match self.mirror {
            MirrorState::NotRead | MirrorState::Cleared => Vec::new(),
            MirrorState::Live if allows_eid_persistence(ec_context.consent()) => self
                .server
                .as_ref()
                .map(|server| mirror_sources(server, ec_context))
                .unwrap_or_default(),
            MirrorState::Live => Vec::new(),
        };
        let read = if self.mirror == MirrorState::NotRead {
            MirrorRead::NotRead
        } else if sources.is_empty() {
            MirrorRead::Cleared
        } else {
            MirrorRead::Servable(&sources)
        };
        let now = self.now.unwrap_or_else(current_timestamp);
        mirror_payload(read, now).map(|payload| payload.to_json())
    }
}

/// Whether the operator turned the `ts-eids-s` mirror on (spec 6.2, 15).
pub(crate) fn mirror_enabled(ec_context: &EcContext) -> bool {
    ec_context
        .identity_runtime()
        .is_some_and(|runtime| runtime.identity().mirror_enabled)
}

/// One [`MirrorSource`] per source of `server`, ranked by its writer's
/// `cookie_priority`.
fn mirror_sources(server: &ServerSet, ec_context: &EcContext) -> Vec<MirrorSource> {
    let identity = ec_context.identity_runtime().map(IdentityRuntime::identity);
    server
        .sources()
        .iter()
        .map(|source| {
            let priority = match (&source.writer, identity) {
                (Some(writer), Some(identity)) => identity
                    .providers
                    .get(writer.as_str())
                    .map_or(0, |settings| settings.cookie_priority),
                _ => 0,
            };
            MirrorSource {
                source: source.source_domain.clone(),
                provider: source.writer.clone(),
                priority,
                expires_at: source.expires_at,
                eids: source.eids.clone(),
            }
        })
        .collect()
}

fn mirror_state(
    snapshot: &EcKvSnapshot,
    registry: Option<&PartnerRegistry>,
    ec_context: &EcContext,
) -> MirrorState {
    if !ec_context.ec_allowed() {
        return MirrorState::Cleared;
    }
    let (Some(ec_id), Some(_)) = (ec_context.ec_value(), registry) else {
        return MirrorState::NotRead;
    };
    match snapshot {
        EcKvSnapshot::Missing { ec_id: read } if read == ec_id => MirrorState::Cleared,
        EcKvSnapshot::Present {
            ec_id: read, entry, ..
        } if read == ec_id => {
            if entry.consent.ok {
                MirrorState::Live
            } else {
                MirrorState::Cleared
            }
        }
        _ => MirrorState::NotRead,
    }
}

/// Resolves what the request's KV read gives the auction, once.
///
/// The server set is `None` when any prerequisite is missing (no partner
/// store, no EC, consent denied), and empty on a KV miss, a failed read, a
/// withdrawn row or a failed clock, so the auction proceeds in degraded
/// mode. A stored record is in it only when the shared servability check
/// passes under this request's consent and `signals`. The denials come from
/// the row read, whatever its consent state.
pub(crate) fn auction_identity(
    snapshot: &EcKvSnapshot,
    registry: Option<&PartnerRegistry>,
    ec_context: &EcContext,
    signals: PolicySignals,
) -> AuctionIdentity {
    let mut identity = AuctionIdentity {
        server: None,
        denials: Denials::default(),
        mirror: mirror_state(snapshot, registry, ec_context),
        now: None,
    };
    let Some(registry) = registry else {
        return identity;
    };
    if !ec_context.ec_allowed() {
        return identity;
    }
    let Some(ec_id) = ec_context.ec_value() else {
        return identity;
    };
    let entry = snapshot.entry_for(ec_id);
    // Without a runtime no provider is configured, so only unowned records
    // without a writer are servable, and no provider denies anything.
    let empty_catalog;
    let catalog = match ec_context.identity_runtime() {
        Some(runtime) => runtime.catalog(),
        None => {
            empty_catalog = ProviderCatalog::empty();
            &empty_catalog
        }
    };
    if let Some(entry) = entry {
        identity.denials = denials(entry, catalog);
    }
    // A zero time would make every expiry check pass (spec 4.4), so a failed
    // clock resolves no KV IDs.
    let Some(now) = checked_current_timestamp() else {
        identity.server = Some(ServerSet::default());
        return identity;
    };
    identity.now = Some(now);
    let opted_out = entry.map(opted_out).unwrap_or_default();
    let serve = ServeContext {
        now: UnixTime::from_secs(now),
        consent: ec_context.consent(),
        signals,
        catalog,
        opted_out: &opted_out,
    };
    identity.server = Some(server_set(entry, registry, &serve));
    identity
}

/// Resolves partner EIDs from the KV identity graph for bidstream decoration.
///
/// Returns `None` when any prerequisite is missing (no KV store, no partner
/// store, no EC, consent denied), and empty EIDs on a KV miss or error so
/// the auction can proceed in degraded mode. See [`auction_identity`].
#[cfg(test)]
pub(crate) fn resolve_auction_eids(
    snapshot: &EcKvSnapshot,
    registry: Option<&PartnerRegistry>,
    ec_context: &EcContext,
    signals: PolicySignals,
) -> Option<Vec<Eid>> {
    auction_identity(snapshot, registry, ec_context, signals).server_eids()
}

/// The mirror member of an `/auction` or `/_ts/page-bids` response, from the
/// row this request read (spec 6.2). See [`AuctionIdentity::mirror_member`].
pub(crate) fn mirror_member(
    snapshot: &EcKvSnapshot,
    registry: Option<&PartnerRegistry>,
    ec_context: &EcContext,
    signals: PolicySignals,
) -> Option<serde_json::Value> {
    auction_identity(snapshot, registry, ec_context, signals).mirror_member(ec_context)
}

/// Resolves the request's browser EIDs: the `/auction` body when it has a
/// valid entry, else the `ts-eids` cookie.
///
/// Both go through the shared client EID parser, so the same rules and caps
/// apply to each, and a malformed entry costs only itself.
pub(crate) fn resolve_client_auction_eids(
    raw: Option<&RawValue>,
    cookie_value: Option<&str>,
) -> Option<Vec<Eid>> {
    raw.map(parse_client_eids)
        .filter(|eids| !eids.is_empty())
        .or_else(|| parse_cookie_auction_eids(cookie_value))
}

fn parse_cookie_auction_eids(cookie_value: Option<&str>) -> Option<Vec<Eid>> {
    let cookie_value = cookie_value?;
    match parse_prebid_eids_cookie(cookie_value) {
        Ok(eids) if eids.is_empty() => None,
        Ok(eids) => Some(eids),
        Err(_) => {
            log::trace!("Auction EIDs: failed to parse ts-eids cookie; dropping");
            None
        }
    }
}

/// Merges the request's browser EIDs with the server set (spec 7.1).
///
/// The browser's candidates first pass the provider `denials`. Those left
/// are forwarded as parsed, in wire order, never altered beyond a denied
/// UID: they are what Prebid sent the `trustedServer` adapter, and a source
/// may repeat with different EID-level fields. A server source's whole entry
/// set is appended after them, and the two are never mixed:
///
/// - For a source whose owner wrote a valid value, the owner's set replaces
///   every browser entry of the source, however often the browser repeated
///   it.
/// - For any other source, the server's set goes out only when no browser
///   entry carries the source, so the browser's current value wins over a
///   browser-seeded record and over any unowned record.
///
/// Sources are compared as the partner registry normalizes them (lowercase,
/// trailing dot removed). Entries without UIDs are dropped.
pub(crate) fn merge_auction_eids(
    client_eids: &[Eid],
    server: &ServerSet,
    denials: &Denials,
) -> Vec<Eid> {
    let owned_sources: HashSet<String> = server
        .sources()
        .iter()
        .filter(|source| source.owned && source.eids.iter().any(|eid| !eid.uids.is_empty()))
        .filter_map(|source| normalize_partner_source_domain(&source.source_domain).ok())
        .collect();
    let is_owned = |eid: &Eid| {
        normalize_partner_source_domain(&eid.source)
            .is_ok_and(|source| owned_sources.contains(&source))
    };
    let mut merged: Vec<Eid> = filter_browser_candidates(client_eids, denials)
        .into_iter()
        .filter(|eid| !eid.uids.is_empty() && !is_owned(eid))
        .collect();
    let browser_sources: HashSet<String> = merged
        .iter()
        .filter_map(|eid| normalize_partner_source_domain(&eid.source).ok())
        .collect();

    for source in server.sources() {
        for eid in &source.eids {
            if eid.uids.is_empty() {
                continue;
            }
            let carried_by_browser = normalize_partner_source_domain(&eid.source)
                .is_ok_and(|source| browser_sources.contains(&source));
            if source.owned || !carried_by_browser {
                merged.push(eid.clone());
            }
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::config::AuctionConfig;
    use crate::auction::provider::{AuctionProvider, ProviderRequestOutcome};
    use crate::auction::telemetry::{AuctionEventBatch, AuctionTelemetrySink};
    use crate::auction::types::{AuctionRequest, AuctionResponse};
    use crate::consent::jurisdiction::Jurisdiction;
    use crate::consent::types::ConsentContext;
    use crate::ec::identity::revision::uid_hash;
    use crate::ec::identity::test_support::{
        ec_partner, example_entry_set, full_record, pid, provider_from, source,
    };
    use crate::ec::kv_types::{KvEid, KvEntry, KvPartnerId, KvProviderState, KvUid};
    use crate::error::IntoHttpResponse as _;
    use crate::openrtb::Uid;
    use crate::platform::test_support::{
        NoopBackend, NoopConfigStore, NoopGeo, NoopHttpClient, NoopSecretStore, StubHttpClient,
        noop_services,
    };
    use crate::platform::{ClientInfo, PlatformHttpClient, PlatformHttpRequest, PlatformResponse};
    use crate::test_support::tests::{crate_test_settings_str, create_test_settings};
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use serde_json::Value as JsonValue;
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingTelemetrySink {
        batches: Mutex<Vec<AuctionEventBatch>>,
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionTelemetrySink for RecordingTelemetrySink {
        async fn emit_auction_events(
            &self,
            _services: &RuntimeServices,
            batch: AuctionEventBatch,
        ) -> Result<(), Report<TrustedServerError>> {
            self.batches
                .lock()
                .expect("should lock telemetry batches")
                .push(batch);
            Ok(())
        }
    }

    fn services_with_telemetry(sink: Arc<RecordingTelemetrySink>) -> RuntimeServices {
        let telemetry_sink: Arc<dyn AuctionTelemetrySink> = sink;
        RuntimeServices::builder()
            .config_store(Arc::new(NoopConfigStore))
            .secret_store(Arc::new(NoopSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(Arc::new(NoopBackend))
            .http_client(Arc::new(NoopHttpClient))
            .geo(Arc::new(NoopGeo))
            .auction_telemetry_sink(telemetry_sink)
            .client_info(ClientInfo::default())
            .build()
    }

    fn make_ec_context(jurisdiction: Jurisdiction, ec_value: Option<&str>) -> EcContext {
        EcContext::new_for_test(
            ec_value.map(str::to_owned),
            ConsentContext {
                jurisdiction,
                ..ConsentContext::default()
            },
        )
    }

    fn counting_test_partner(source_domain: &str) -> crate::settings::EcPartner {
        crate::settings::EcPartner {
            name: format!("Partner {source_domain}"),
            source_domain: source_domain.to_owned(),
            openrtb_atype: crate::settings::EcPartner::default_openrtb_atype(),
            bidstream_enabled: true,
            api_token: Some(crate::redacted::Redacted::new(format!(
                "token-{source_domain}-32-bytes-minimum-value"
            ))),
            batch_rate_limit: crate::settings::EcPartner::default_batch_rate_limit(),
            pull_sync_enabled: false,
            pull_sync_url: None,
            pull_sync_allowed_domains: vec![],
            removed_pull_sync_ttl_sec: None,
            removed_pull_sync_rate_limit: None,
            owner: None,
            ts_pull_token: None,
        }
    }

    #[tokio::test]
    async fn auction_endpoint_snapshot_is_reused_by_response_finalization() {
        // `/auction` loads the identity-graph row to resolve server-side EIDs.
        // Finalization runs afterwards on the same EC context and ingests
        // `ts-eids`/`sharedId` updates. Both must be served by a single billable
        // read: before the snapshot was shared, finalization saw `NotRead` and
        // paid for a second lookup.
        let settings = create_test_settings();
        let mut orchestrator = AuctionOrchestrator::new(AuctionConfig {
            enabled: true,
            providers: AuctionConfig::legacy_provider_map(&["eid_capturing_provider"]),
            timeout_ms: 2000,
            mediator: None,
            ..Default::default()
        });
        orchestrator.register_provider(Arc::new(EidCapturingProvider {
            had_eids: Arc::new(std::sync::Mutex::new(None)),
        }));
        let registry = PartnerRegistry::from_config(&[counting_test_partner("sharedid.org")])
            .expect("should build partner registry");

        let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let graph = KvIdentityGraph::counting("counting-store", Arc::clone(&lookups));
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mut live = crate::ec::kv_types::KvEntry::tombstone(1000);
        live.consent.ok = true;
        graph.create(&ec_id, &live).expect("should seed live row");
        lookups.store(0, std::sync::atomic::Ordering::Relaxed);

        let mut ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));
        let req = Request::builder()
            .method("POST")
            .uri("https://test-publisher.com/auction")
            .body(EdgeBody::from(
                serde_json::to_vec(&json!({
                    "adUnits": [
                        {
                            "code": "div-gpt-ad-1",
                            "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
                        }
                    ]
                }))
                .expect("should serialize body"),
            ))
            .expect("should build auction request");

        // The capturing provider deliberately fails its launch; identity
        // resolution — the subject of this test — completes before dispatch.
        let _ = handle_auction(
            &settings,
            &orchestrator,
            Some(&graph),
            Some(&registry),
            &mut ec_context,
            &noop_services(),
            req,
        )
        .await;

        assert_eq!(
            lookups.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the endpoint should read the identity row exactly once"
        );
        assert!(
            ec_context.kv_snapshot().entry_for(&ec_id).is_some(),
            "the endpoint must hand its snapshot to the request context"
        );

        ec_context.set_eid_sync_source(crate::ec::EidSyncSource::Auction);
        let mut response = http::Response::new(EdgeBody::empty());
        crate::ec::finalize::ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            None,
            Some("shared-cookie-id"),
            &mut response,
        );

        assert_eq!(
            lookups.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "finalization must reuse the endpoint snapshot instead of reading again"
        );
        assert!(
            stored_ids(&graph, &ec_id).is_empty(),
            "/auction is a stager and must not write the sharedId"
        );
        assert!(
            ec_context.identity().mutation().is_empty(),
            "/auction is a stager and must not queue the sharedId"
        );
    }

    const COOKIE_ONLY_SOURCE: &str = "cookie-only.example.com";

    fn ingestion_registry(extra_source: &str) -> PartnerRegistry {
        PartnerRegistry::from_config(&[
            counting_test_partner(COOKIE_ONLY_SOURCE),
            counting_test_partner(extra_source),
        ])
        .expect("should build partner registry")
    }

    fn cookie_only_ts_eids() -> String {
        BASE64.encode(
            serde_json::to_vec(&json!([
                { "source": COOKIE_ONLY_SOURCE, "uids": [{ "id": "cookie-uid" }] }
            ]))
            .expect("should serialize cookie"),
        )
    }

    fn seeded_graph(ec_id: &str) -> KvIdentityGraph {
        let graph = KvIdentityGraph::in_memory("test_store");
        let mut live = crate::ec::kv_types::KvEntry::tombstone(1000);
        live.consent.ok = true;
        graph.create(ec_id, &live).expect("should seed live row");
        graph
    }

    fn stored_ids(graph: &KvIdentityGraph, ec_id: &str) -> Vec<(String, String)> {
        let (stored, _) = graph
            .get(ec_id)
            .expect("should read store")
            .expect("row should exist");
        stored
            .ids
            .iter()
            .map(|(source, id)| {
                (
                    source.clone(),
                    id.first_uid().unwrap_or_default().to_owned(),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn auction_ingests_nothing_from_its_body_or_the_cookie() {
        let settings = create_test_settings();
        let mut orchestrator = AuctionOrchestrator::new(AuctionConfig {
            enabled: true,
            providers: AuctionConfig::legacy_provider_map(&["eid_capturing_provider"]),
            timeout_ms: 2000,
            mediator: None,
            ..Default::default()
        });
        orchestrator.register_provider(Arc::new(EidCapturingProvider {
            had_eids: Arc::new(std::sync::Mutex::new(None)),
        }));
        let registry = ingestion_registry("body.example.com");
        let ec_id = format!("{}.ingbdy", "a".repeat(64));
        let graph = seeded_graph(&ec_id);
        let mut ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));
        ec_context.set_eid_sync_source(crate::ec::EidSyncSource::Auction);
        let cookie = cookie_only_ts_eids();
        let req = Request::builder()
            .method("POST")
            .uri("https://test-publisher.com/auction")
            .header(header::COOKIE, format!("{COOKIE_TS_EIDS}={cookie}"))
            .body(EdgeBody::from(
                serde_json::to_vec(&json!({
                    "adUnits": [{ "code": "div-gpt-ad-1", "mediaTypes": { "banner": { "sizes": [[300, 250]] } } }],
                    "eids": [{ "source": "body.example.com", "uids": [{ "id": "body-uid" }] }]
                }))
                .expect("should serialize body"),
            ))
            .expect("should build auction request");

        let _ = handle_auction(
            &settings,
            &orchestrator,
            Some(&graph),
            Some(&registry),
            &mut ec_context,
            &noop_services(),
            req,
        )
        .await;
        let mut response = http::Response::new(EdgeBody::empty());
        crate::ec::finalize::ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            Some(&cookie),
            None,
            &mut response,
        );

        assert!(
            stored_ids(&graph, &ec_id).is_empty(),
            "KV should learn nothing from an /auction"
        );
        assert!(
            ec_context.identity().mutation().is_empty(),
            "/auction should queue nothing"
        );
    }

    #[tokio::test]
    async fn disabled_auction_ingests_nothing() {
        let settings = create_test_settings();
        let orchestrator = AuctionOrchestrator::new(AuctionConfig {
            enabled: false,
            ..Default::default()
        });
        let registry = ingestion_registry("sharedid.org");
        let ec_id = format!("{}.ingdis", "a".repeat(64));
        let graph = seeded_graph(&ec_id);
        let mut ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));
        ec_context.set_eid_sync_source(crate::ec::EidSyncSource::Auction);
        let cookie = cookie_only_ts_eids();
        let req = Request::builder()
            .method("POST")
            .uri("https://test-publisher.com/auction")
            .header(header::COOKIE, format!("{COOKIE_TS_EIDS}={cookie}"))
            .body(EdgeBody::from(ad_units_only_body()))
            .expect("should build auction request");

        let _ = handle_auction(
            &settings,
            &orchestrator,
            Some(&graph),
            Some(&registry),
            &mut ec_context,
            &noop_services(),
            req,
        )
        .await;
        let mut response = http::Response::new(EdgeBody::empty());
        crate::ec::finalize::ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            Some(&cookie),
            Some("shared-cookie-id"),
            &mut response,
        );

        assert!(
            stored_ids(&graph, &ec_id).is_empty(),
            "a disabled /auction should ingest nothing"
        );
        assert!(
            ec_context.identity().mutation().is_empty(),
            "a disabled /auction should queue nothing"
        );
    }

    #[test]
    fn writer_navigation_queues_the_cookie_only_source() {
        // Control for the two tests above: the registry does match the
        // cookie-only source when the writer navigation reads the cookie.
        let settings = create_test_settings();
        let registry = ingestion_registry("body.example.com");
        let ec_id = format!("{}.ingnav", "a".repeat(64));
        let graph = seeded_graph(&ec_id);
        let mut ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));
        ec_context.set_eid_sync_source(crate::ec::EidSyncSource::Navigation);
        ec_context.identity_mut().set_writer_decision(
            crate::ec::identity::request::WriterDecision {
                writer: true,
                is_real_browser: true,
                user_agent: None,
                dnt: false,
                page_url: None,
            },
        );
        let cookie = cookie_only_ts_eids();
        let mut response = http::Response::new(EdgeBody::empty());

        crate::ec::finalize::ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            Some(&cookie),
            None,
            &mut response,
        );

        assert!(
            stored_ids(&graph, &ec_id).is_empty(),
            "a navigation should leave the write to phase 4"
        );
        let queued = ec_context.identity().mutation().positive_changes();
        assert!(
            matches!(
                queued,
                [crate::ec::identity::mutation::PartnerIdChange::FillMissing { source, record }]
                    if source.as_str() == COOKIE_ONLY_SOURCE
                        && record.first_uid() == Some("cookie-uid")
            ),
            "a navigation should queue the cookie's registered source"
        );
    }

    #[tokio::test]
    async fn auction_body_eids_never_reach_kv() {
        // The `/auction` body reaches bidders but is not an ingest input
        // (spec G10): finalization neither writes nor queues it.
        let settings = create_test_settings();
        let mut orchestrator = AuctionOrchestrator::new(AuctionConfig {
            enabled: true,
            providers: AuctionConfig::legacy_provider_map(&["eid_capturing_provider"]),
            timeout_ms: 2000,
            mediator: None,
            ..Default::default()
        });
        orchestrator.register_provider(Arc::new(EidCapturingProvider {
            had_eids: Arc::new(std::sync::Mutex::new(None)),
        }));
        let registry = PartnerRegistry::from_config(&[counting_test_partner("id5-sync.com")])
            .expect("should build partner registry");

        let graph = KvIdentityGraph::in_memory("test_store");
        let ec_id = format!("{}.eidbdy", "a".repeat(64));
        let mut live = crate::ec::kv_types::KvEntry::tombstone(1000);
        live.consent.ok = true;
        graph.create(&ec_id, &live).expect("should seed live row");

        let mut ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));
        ec_context.set_eid_sync_source(crate::ec::EidSyncSource::Auction);
        let req = Request::builder()
            .method("POST")
            .uri("https://test-publisher.com/auction")
            .body(EdgeBody::from(
                serde_json::to_vec(&json!({
                    "adUnits": [
                        {
                            "code": "div-gpt-ad-1",
                            "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
                        }
                    ],
                    "eids": [
                        {"source": "id5-sync.com", "uids": [{"id": "ID5_from_body", "atype": 1}]}
                    ]
                }))
                .expect("should serialize body"),
            ))
            .expect("should build auction request");

        // The capturing provider deliberately fails its launch; identity
        // resolution — the subject of this test — completes before dispatch.
        let _ = handle_auction(
            &settings,
            &orchestrator,
            Some(&graph),
            Some(&registry),
            &mut ec_context,
            &noop_services(),
            req,
        )
        .await;

        let mut response = http::Response::new(EdgeBody::empty());
        crate::ec::finalize::ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            None, // No `ts-eids` cookie on this request at all.
            None,
            &mut response,
        );

        assert!(
            stored_ids(&graph, &ec_id).is_empty(),
            "the body's EID must not be ingested into KV"
        );
        assert!(
            ec_context.identity().mutation().is_empty(),
            "the body's EID must not be queued"
        );
    }

    /// Provider that fails the test if it is ever contacted. Used to prove the
    /// `/auction` consent gate short-circuits before any outbound bid request.
    struct PanicOnBidProvider;

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for PanicOnBidProvider {
        fn provider_name(&self) -> &str {
            "panic_provider"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            _context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            panic!("provider must not be contacted when the consent gate denies the auction");
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            _response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            panic!("provider must not parse a response when the auction is gated off");
        }

        fn timeout_ms(&self) -> u32 {
            100
        }

        fn backend_name(&self, _services: &RuntimeServices, _timeout_ms: u32) -> Option<String> {
            Some("panic-backend".to_string())
        }
    }

    /// Provider used to prove that direct `/auction` remains available when
    /// publisher server-side ad templates are disabled.
    struct TemplateSwitchProbeProvider {
        calls: Arc<Mutex<usize>>,
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for TemplateSwitchProbeProvider {
        fn provider_name(&self) -> &'static str {
            "template-switch-probe"
        }

        async fn request_bids(
            &self,
            _request: &AuctionRequest,
            context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            *self.calls.lock().expect("should lock provider call count") += 1;
            let request = Request::builder()
                .method("POST")
                .uri("https://bidder.example/auction")
                .body(EdgeBody::empty())
                .expect("should build probe provider request");
            context
                .services
                .http_client()
                .send_async(PlatformHttpRequest::new(
                    request,
                    "template-switch-probe-backend",
                ))
                .await
                .change_context(TrustedServerError::Auction {
                    message: "probe provider launch failed".to_string(),
                })
                .map(ProviderRequestOutcome::pending)
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            _response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            Ok(AuctionResponse::success(
                self.provider_name(),
                Vec::new(),
                0,
            ))
        }

        fn timeout_ms(&self) -> u32 {
            100
        }

        fn backend_name(&self, _services: &RuntimeServices, _timeout_ms: u32) -> Option<String> {
            Some("template-switch-probe-backend".to_string())
        }
    }

    #[tokio::test]
    async fn direct_auction_remains_available_when_templates_are_disabled() {
        let settings_toml = format!(
            "{}\n[auction]\nenabled = true\n\n[auction.providers.template-switch-probe]\nprotocol = \"openrtb-2.6\"\nendpoint = \"https://bidder.example/auction\"\nrouting = \"all_eligible\"\n\n[creative_opportunities]\nenabled = false\ngam_network_id = \"12345\"\n",
            crate_test_settings_str()
        );
        let settings = Settings::from_toml(&settings_toml)
            .expect("should parse settings with disabled templates");
        let calls = Arc::new(Mutex::new(0));
        let mut orchestrator = AuctionOrchestrator::new(settings.auction.clone());
        orchestrator.register_provider(Arc::new(TemplateSwitchProbeProvider {
            calls: Arc::clone(&calls),
        }));

        let stub = Arc::new(StubHttpClient::new());
        stub.push_response(200, b"probe response".to_vec());
        let services = RuntimeServices::builder()
            .config_store(Arc::new(NoopConfigStore))
            .secret_store(Arc::new(NoopSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .backend(Arc::new(NoopBackend))
            .http_client(Arc::clone(&stub) as Arc<dyn PlatformHttpClient>)
            .geo(Arc::new(NoopGeo))
            .client_info(ClientInfo::default())
            .build();
        let mut ec_context = make_ec_context(Jurisdiction::NonRegulated, None);
        let body = json!({
            "adUnits": [{
                "code": "div-gpt-ad-1",
                "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
            }]
        });
        let req = Request::builder()
            .method("POST")
            .uri("https://test-publisher.com/auction")
            .body(EdgeBody::from(
                serde_json::to_vec(&body).expect("should serialize body"),
            ))
            .expect("should build auction request");

        let response = handle_auction(
            &settings,
            &orchestrator,
            None,
            None,
            &mut ec_context,
            &services,
            req,
        )
        .await
        .expect("direct auction should remain available");

        assert_eq!(
            *calls.lock().expect("should lock provider call count"),
            1,
            "disabling publisher templates must not disable direct /auction"
        );
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn disabled_auction_endpoint_emits_skipped_telemetry_without_provider_work() {
        let settings = create_test_settings();
        let config = AuctionConfig {
            enabled: false,
            providers: AuctionConfig::legacy_provider_map(&["panic_provider"]),
            timeout_ms: 2000,
            mediator: None,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(PanicOnBidProvider));
        let telemetry_sink = Arc::new(RecordingTelemetrySink::default());
        let services = services_with_telemetry(Arc::clone(&telemetry_sink));
        let mut ec_context = make_ec_context(Jurisdiction::NonRegulated, None);
        let body = json!({
            "adUnits": [{
                "code": "div-gpt-ad-1",
                "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
            }]
        });
        let request = Request::builder()
            .method("POST")
            .uri("https://test-publisher.example/auction")
            .body(EdgeBody::from(
                serde_json::to_vec(&body).expect("should serialize disabled-auction body"),
            ))
            .expect("should build disabled-auction request");

        let response = handle_auction(
            &settings,
            &orchestrator,
            None,
            None,
            &mut ec_context,
            &services,
            request,
        )
        .await
        .expect("disabled auction should return a no-bid response");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "disabled auction should return a 200 no-bid response"
        );
        let batches = telemetry_sink
            .batches
            .lock()
            .expect("should lock telemetry batches");
        assert_eq!(batches.len(), 1, "should emit one telemetry batch");
        let rows = batches[0].rows();
        assert_eq!(rows.len(), 1, "should emit one skipped summary row");
        assert_eq!(rows[0].event_kind, "summary", "should emit a summary row");
        assert_eq!(rows[0].terminal_status.as_deref(), Some("skipped"));
        assert_eq!(
            rows[0].terminal_reason.as_deref(),
            Some("auction_disabled"),
            "should identify the disabled auction policy"
        );
    }

    #[tokio::test]
    async fn all_planned_launch_failures_return_bad_gateway_and_execution_failed_telemetry() {
        let settings_toml = format!(
            "{}\n[auction]\nenabled = true\n\n[auction.providers.launch-fail]\nprotocol = \"openrtb-2.6\"\nprofile = \"standard\"\nendpoint = \"https://bidder.example/auction\"\nrouting = \"all_eligible\"\n",
            crate_test_settings_str()
        );
        let settings =
            Settings::from_toml(&settings_toml).expect("should parse launch-failure settings");
        let plan = Arc::new(
            crate::auction::compile_auction_plan(&settings)
                .expect("should compile launch-failure plan"),
        );
        let orchestrator = AuctionOrchestrator::from_plan(plan, None);
        let telemetry_sink = Arc::new(RecordingTelemetrySink::default());
        let services = services_with_telemetry(Arc::clone(&telemetry_sink));
        let mut ec_context = make_ec_context(Jurisdiction::NonRegulated, None);
        let body = json!({
            "adUnits": [{
                "code": "div-gpt-ad-1",
                "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
            }]
        });
        let request = Request::builder()
            .method("POST")
            .uri("https://test-publisher.example/auction")
            .body(EdgeBody::from(
                serde_json::to_vec(&body).expect("should serialize launch-failure body"),
            ))
            .expect("should build launch-failure request");

        let error = handle_auction(
            &settings,
            &orchestrator,
            None,
            None,
            &mut ec_context,
            &services,
            request,
        )
        .await
        .expect_err("all planned launch failures should fail the auction endpoint");

        assert_eq!(
            error.current_context().status_code(),
            StatusCode::BAD_GATEWAY
        );
        let batches = telemetry_sink
            .batches
            .lock()
            .expect("should lock telemetry batches");
        assert_eq!(batches.len(), 1, "should emit one telemetry batch");
        let rows = batches[0].rows();
        assert_eq!(rows.len(), 1, "should emit one execution-failure summary");
        assert_eq!(rows[0].event_kind, "summary");
        assert_eq!(rows[0].terminal_status.as_deref(), Some("execution_failed"));
        assert_eq!(rows[0].terminal_reason.as_deref(), Some("execution_failed"));
    }

    #[tokio::test]
    async fn auction_endpoint_consent_gate_returns_no_bid_without_contacting_providers() {
        // GDPR/unknown jurisdiction lacking effective TCF Purpose 1 must not run
        // a server-side auction. The /auction endpoint must short-circuit to a
        // no-bid response before dispatching to any provider — matching the
        // publisher-navigation and /_ts/page-bids paths.
        let settings = create_test_settings();
        let config = AuctionConfig {
            enabled: true,
            providers: AuctionConfig::legacy_provider_map(&["panic_provider"]),
            timeout_ms: 2000,
            mediator: None,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        orchestrator.register_provider(Arc::new(PanicOnBidProvider));
        let telemetry_sink = Arc::new(RecordingTelemetrySink::default());
        let services = services_with_telemetry(Arc::clone(&telemetry_sink));
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mut ec_context = make_ec_context(Jurisdiction::Unknown, Some(&ec_id));

        let body = json!({
            "adUnits": [
                {
                    "code": "div-gpt-ad-1",
                    "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
                }
            ]
        });
        let req = Request::builder()
            .method("POST")
            .uri("https://test-publisher.com/auction")
            .body(EdgeBody::from(
                serde_json::to_vec(&body).expect("should serialize body"),
            ))
            .expect("should build auction request");

        let response = handle_auction(
            &settings,
            &orchestrator,
            None,
            None,
            &mut ec_context,
            &services,
            req,
        )
        .await
        .expect("gated auction should still return a valid response");

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "gated auction should return a 200 no-bid response"
        );
        let body_bytes = response.into_body().into_bytes().unwrap_or_default();
        let parsed: JsonValue =
            serde_json::from_slice(&body_bytes).expect("response body should be valid JSON");
        let seatbid_empty = match parsed.get("seatbid").and_then(JsonValue::as_array) {
            Some(seatbid) => seatbid.is_empty(),
            None => true,
        };
        assert!(
            seatbid_empty,
            "gated auction must return no bids, got: {parsed}"
        );

        let batches = telemetry_sink
            .batches
            .lock()
            .expect("should lock telemetry batches");
        assert_eq!(batches.len(), 1, "should emit one telemetry batch");
        let rows = batches[0].rows();
        assert_eq!(rows.len(), 1, "skipped auction should emit one summary row");
        assert_eq!(rows[0].event_kind, "summary");
        assert_eq!(rows[0].terminal_status.as_deref(), Some("skipped"));
        assert_eq!(rows[0].terminal_reason.as_deref(), Some("consent_denied"));
        let ndjson = batches[0]
            .to_ndjson(16 * 1024)
            .expect("should serialize telemetry");
        assert!(
            !ndjson.contains(&ec_id),
            "telemetry must not serialize EC identifiers"
        );
    }

    /// Provider that records whether the auction request it received carried
    /// EIDs, then fails its launch so no real transport handle is needed.
    struct EidCapturingProvider {
        had_eids: Arc<std::sync::Mutex<Option<bool>>>,
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for EidCapturingProvider {
        fn provider_name(&self) -> &str {
            "eid_capturing_provider"
        }

        async fn request_bids(
            &self,
            request: &AuctionRequest,
            _context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            *self.had_eids.lock().expect("should lock captured eids") =
                Some(request.user.eids.is_some());
            Err(Report::new(TrustedServerError::Auction {
                message: "capture only".to_string(),
            }))
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            _response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            panic!("parse_response must not run when the launch fails");
        }

        fn timeout_ms(&self) -> u32 {
            100
        }

        fn backend_name(&self, _services: &RuntimeServices, _timeout_ms: u32) -> Option<String> {
            Some("capture-backend".to_string())
        }
    }

    const AUCTION_BODY_FIXTURE: &str =
        include_str!("../../../trusted-server-js/lib/test/fixtures/auction-body-eids.txt");
    const TS_EIDS_COOKIE_FIXTURE: &str =
        include_str!("../../../trusted-server-js/lib/test/fixtures/ts-eids-cookie.txt");

    /// Provider that records the full EID set its auction request carried,
    /// then fails its launch so no real transport handle is needed.
    struct EidRecordingProvider {
        eids: Arc<Mutex<Option<JsonValue>>>,
    }

    #[async_trait::async_trait(?Send)]
    impl AuctionProvider for EidRecordingProvider {
        fn provider_name(&self) -> &str {
            "eid_recording_provider"
        }

        async fn request_bids(
            &self,
            request: &AuctionRequest,
            _context: &AuctionContext<'_>,
        ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
            *self.eids.lock().expect("should lock recorded eids") = Some(
                serde_json::to_value(&request.user.eids).expect("should serialize recorded eids"),
            );
            Err(Report::new(TrustedServerError::Auction {
                message: "record only".to_string(),
            }))
        }

        async fn parse_response(
            &self,
            _response: PlatformResponse,
            _response_time_ms: u64,
        ) -> Result<AuctionResponse, Report<TrustedServerError>> {
            panic!("parse_response must not run when the launch fails");
        }

        fn timeout_ms(&self) -> u32 {
            100
        }

        fn backend_name(&self, _services: &RuntimeServices, _timeout_ms: u32) -> Option<String> {
            Some("record-backend".to_string())
        }
    }

    /// Runs `/auction` with an allowed EC and returns the EIDs the provider
    /// received: `None` when no provider was called (the request was
    /// rejected), `Some(Value::Null)` when it carried no EIDs.
    async fn recorded_auction_eids(
        body: Vec<u8>,
        ts_eids_cookie: Option<&str>,
    ) -> Option<JsonValue> {
        let settings = create_test_settings();
        let mut orchestrator = AuctionOrchestrator::new(AuctionConfig {
            enabled: true,
            providers: AuctionConfig::legacy_provider_map(&["eid_recording_provider"]),
            timeout_ms: 2000,
            mediator: None,
            ..Default::default()
        });
        let recorded = Arc::new(Mutex::new(None));
        orchestrator.register_provider(Arc::new(EidRecordingProvider {
            eids: Arc::clone(&recorded),
        }));
        let ec_id = format!("{}.fixtur", "a".repeat(64));
        let mut ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));
        let mut builder = Request::builder()
            .method("POST")
            .uri("https://test-publisher.com/auction");
        if let Some(cookie) = ts_eids_cookie {
            builder = builder.header(header::COOKIE, format!("{COOKIE_TS_EIDS}={cookie}"));
        }
        let req = builder
            .body(EdgeBody::from(body))
            .expect("should build auction request");

        // The recording provider fails its launch; the assertion is on what
        // it received, not on the auction result.
        let _ = handle_auction(
            &settings,
            &orchestrator,
            None,
            None,
            &mut ec_context,
            &noop_services(),
            req,
        )
        .await;

        recorded.lock().expect("should lock recorded eids").clone()
    }

    /// The two parsable entries of the fixture input, as providers receive
    /// them. The cookie copy has no EID-level `ext` on the last entry.
    fn fixture_good_entries(with_eid_ext: bool) -> JsonValue {
        let mut last = json!({
            "source": "last.example.com",
            "uids": [{ "id": "last-uid", "atype": 3 }]
        });
        if with_eid_ext {
            last["ext"] = json!({ "segments": ["s1"] });
        }
        json!([
            {
                "source": "first.example.com",
                "inserter": "inserter-€.example.com",
                "matcher": "first.example.com",
                "mm": 3,
                "uids": [{ "id": "first-uid", "atype": 1, "ext": { "label": "日本" } }]
            },
            last
        ])
    }

    #[tokio::test]
    async fn tsjs_auction_body_fixture_drops_only_the_unparsable_entry() {
        let recorded =
            recorded_auction_eids(AUCTION_BODY_FIXTURE.trim_end().as_bytes().to_vec(), None).await;

        assert_eq!(
            recorded,
            Some(fixture_good_entries(true)),
            "the request should be accepted and carry the two good entries unchanged"
        );
    }

    #[tokio::test]
    async fn tsjs_cookie_fixture_drops_only_the_unparsable_entry() {
        let recorded = recorded_auction_eids(
            ad_units_only_body(),
            Some(TS_EIDS_COOKIE_FIXTURE.trim_end()),
        )
        .await;

        assert_eq!(
            recorded,
            Some(fixture_good_entries(false)),
            "the cookie's good entries should reach the provider without the bad one"
        );
    }

    #[tokio::test]
    async fn auction_body_eid_level_fields_reach_the_auction_request() {
        let eids = json!([{
            "source": "adserver.org",
            "inserter": "adserver.org",
            "matcher": "adserver.org",
            "mm": 4,
            "uids": [{ "id": "fictional-tdid", "atype": 1, "ext": { "rtiPartner": "TDID" } }]
        }]);
        let body = serde_json::to_vec(&json!({
            "adUnits": [{ "code": "div-gpt-1", "mediaTypes": { "banner": { "sizes": [[300, 250]] } } }],
            "eids": eids
        }))
        .expect("should serialize body");

        assert_eq!(
            recorded_auction_eids(body, None).await,
            Some(eids),
            "EID-level fields from the body should reach the provider unchanged"
        );
    }

    fn ad_units_only_body() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "adUnits": [{ "code": "div-gpt-1", "mediaTypes": { "banner": { "sizes": [[300, 250]] } } }]
        }))
        .expect("should serialize body")
    }

    #[tokio::test]
    async fn auction_uses_the_cookie_when_no_body_entry_is_valid() {
        let cookie_entries =
            json!([{ "source": "cookie.example.com", "uids": [{ "id": "cookie-uid" }] }]);
        let cookie =
            BASE64.encode(serde_json::to_vec(&cookie_entries).expect("should serialize cookie"));
        let body = serde_json::to_vec(&json!({
            "adUnits": [{ "code": "div-gpt-1", "mediaTypes": { "banner": { "sizes": [[300, 250]] } } }],
            "eids": [{ "source": "", "uids": [{ "id": "body-uid" }] }]
        }))
        .expect("should serialize body");

        assert_eq!(
            recorded_auction_eids(body, Some(&cookie)).await,
            Some(cookie_entries),
            "a body whose entries are all invalid should count as absent"
        );
    }

    #[tokio::test]
    async fn auction_strips_client_eids_when_ec_identity_denied() {
        // US-state opt-out via GPC: the server-side auction consent gate still
        // allows a non-personalized auction, but EC identity use is denied
        // (`ec_allowed()` is false) and `gate_eids_by_consent` does not strip
        // because no TCF signal is present and GDPR does not apply. Client EIDs
        // supplied in the request body/cookie must NOT be forwarded — the
        // outgoing auction request must have `user.eids == None`.
        let settings = create_test_settings();
        let config = AuctionConfig {
            enabled: true,
            providers: AuctionConfig::legacy_provider_map(&["eid_capturing_provider"]),
            timeout_ms: 2000,
            mediator: None,
            ..Default::default()
        };
        let mut orchestrator = AuctionOrchestrator::new(config);
        let had_eids = Arc::new(std::sync::Mutex::new(None));
        orchestrator.register_provider(Arc::new(EidCapturingProvider {
            had_eids: Arc::clone(&had_eids),
        }));
        let services = noop_services();

        // US-state jurisdiction with an explicit GPC opt-out: auction allowed,
        // EC identity denied.
        let mut ec_context = EcContext::new_for_test(
            None,
            ConsentContext {
                jurisdiction: Jurisdiction::UsState("CA".to_owned()),
                gpc: true,
                ..ConsentContext::default()
            },
        );

        // Persistent EIDs supplied in both the request body and the ts-eids cookie.
        let cookie_payload = json!([
            {
                "source": "sharedid.org",
                "uids": [{ "id": "cookie_uid", "atype": 3 }]
            }
        ]);
        let encoded_cookie = BASE64
            .encode(serde_json::to_vec(&cookie_payload).expect("should serialize cookie payload"));
        let body = json!({
            "adUnits": [
                {
                    "code": "div-gpt-ad-1",
                    "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
                }
            ],
            "eids": [
                {
                    "source": "id5-sync.com",
                    "uids": [{ "id": "body_uid", "atype": 1 }]
                }
            ]
        });
        let req = Request::builder()
            .method("POST")
            .uri("https://test-publisher.com/auction")
            .header("cookie", format!("{COOKIE_TS_EIDS}={encoded_cookie}"))
            .body(EdgeBody::from(
                serde_json::to_vec(&body).expect("should serialize body"),
            ))
            .expect("should build auction request");

        // The capturing provider fails its launch, so the auction errors overall;
        // the assertion is on the EIDs observed by the provider, not the result.
        let _ = handle_auction(
            &settings,
            &orchestrator,
            None,
            None,
            &mut ec_context,
            &services,
            req,
        )
        .await;

        assert_eq!(
            *had_eids.lock().expect("should lock captured eids"),
            Some(false),
            "outgoing auction request must carry no EIDs when EC identity is denied"
        );
    }

    #[test]
    fn resolve_auction_eids_returns_empty_without_snapshot() {
        let registry = PartnerRegistry::empty();
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));

        let result = resolve_auction_eids(
            &EcKvSnapshot::NotRead,
            Some(&registry),
            &ec_context,
            PolicySignals::default(),
        );
        assert!(
            result.is_some_and(|eids| eids.is_empty()),
            "should degrade to empty EIDs without a snapshot"
        );
    }

    #[test]
    fn resolve_auction_eids_returns_none_without_registry() {
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));

        let result = resolve_auction_eids(
            &EcKvSnapshot::NotRead,
            None,
            &ec_context,
            PolicySignals::default(),
        );
        assert!(
            result.is_none(),
            "should return None when registry is missing"
        );
    }

    #[test]
    fn resolve_auction_eids_returns_none_when_consent_denied() {
        let registry = PartnerRegistry::empty();
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::Unknown, Some(&ec_id));

        let result = resolve_auction_eids(
            &EcKvSnapshot::NotRead,
            Some(&registry),
            &ec_context,
            PolicySignals::default(),
        );
        assert!(
            result.is_none(),
            "should return None when consent is denied"
        );
    }

    #[test]
    fn resolve_auction_eids_returns_none_when_no_ec() {
        let registry = PartnerRegistry::empty();
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, None);

        let result = resolve_auction_eids(
            &EcKvSnapshot::NotRead,
            Some(&registry),
            &ec_context,
            PolicySignals::default(),
        );
        assert!(
            result.is_none(),
            "should return None when no EC value is present"
        );
    }

    #[test]
    fn resolve_auction_eids_returns_empty_on_kv_miss() {
        let registry = PartnerRegistry::empty();
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));

        let snapshot = EcKvSnapshot::Failed {
            ec_id: ec_id.clone(),
        };
        let result = resolve_auction_eids(
            &snapshot,
            Some(&registry),
            &ec_context,
            PolicySignals::default(),
        );
        let eids = result.expect("should return Some on KV error (degraded mode)");
        assert!(
            eids.is_empty(),
            "should return empty vec on KV error (degraded mode)"
        );
    }

    #[test]
    fn generic_pull_record_with_writer_is_served_after_flush() {
        use crate::ec::identity::IdentityRuntime;
        use crate::ec::identity::test_support::{ec_partner, record_for_test};
        use crate::test_support::tests::create_test_settings;

        let mut partner = ec_partner(
            "ssp.example.com",
            "https://sync.ssp.example.com/pull",
            "example-token-value-0123456789abcdef",
        );
        partner.api_token = Some(crate::redacted::Redacted::new(
            "token-ssp-example-com-32-bytes-minimum".to_owned(),
        ));
        let registry = PartnerRegistry::from_config(std::slice::from_ref(&partner))
            .expect("should build registry");
        let mut settings = create_test_settings();
        settings.ec.partners = vec![partner];
        let with_provider = std::sync::Arc::new(
            IdentityRuntime::from_settings(
                &settings,
                &crate::integrations::IntegrationRegistry::default(),
            )
            .expect("should build the runtime"),
        );
        settings.ec.partners = Vec::new();
        let without_provider = std::sync::Arc::new(
            IdentityRuntime::from_settings(
                &settings,
                &crate::integrations::IntegrationRegistry::default(),
            )
            .expect("should build the runtime"),
        );

        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mut entry = crate::ec::kv_types::KvEntry::minimal("other.example.com", "other", 1);
        entry.ids.insert(
            "ssp.example.com".to_owned(),
            record_for_test("pulled-uid", Some("pull-ssp-example-com"), 1),
        );
        let snapshot = EcKvSnapshot::Present {
            ec_id: ec_id.clone(),
            entry: Box::new(entry),
            generation: None,
        };
        let resolve = |runtime: Option<std::sync::Arc<IdentityRuntime>>| {
            let mut ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));
            if let Some(runtime) = runtime {
                ec_context.set_identity_runtime(runtime);
            }
            resolve_auction_eids(
                &snapshot,
                Some(&registry),
                &ec_context,
                PolicySignals::default(),
            )
            .expect("should resolve EIDs")
        };

        let served = resolve(Some(with_provider));
        let dropped = resolve(Some(without_provider));
        let no_runtime = resolve(None);

        assert!(
            served.iter().any(|eid| eid.source == "ssp.example.com"),
            "should send the pulled UID when the runtime's catalog holds its writer"
        );
        assert!(
            dropped.iter().all(|eid| eid.source != "ssp.example.com"),
            "control: should drop the record when the catalog lacks its writer"
        );
        assert!(
            no_runtime.iter().all(|eid| eid.source != "ssp.example.com"),
            "control: should drop the record without a runtime"
        );
    }

    #[test]
    fn resolve_auction_eids_without_providers_matches_pre_identity_output() {
        let partners = vec![
            counting_test_partner("a.example.com"),
            counting_test_partner("b.example.com"),
        ];
        let registry = PartnerRegistry::from_config(&partners).expect("should build registry");
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));
        let mut entry = crate::ec::kv_types::KvEntry::minimal("a.example.com", "ua", 1);
        entry.ids.insert(
            "b.example.com".to_owned(),
            crate::ec::kv_types::KvPartnerId::new("ub"),
        );
        let snapshot = EcKvSnapshot::Present {
            ec_id: ec_id.clone(),
            entry: Box::new(entry),
            generation: None,
        };

        let eids = resolve_auction_eids(
            &snapshot,
            Some(&registry),
            &ec_context,
            PolicySignals::default(),
        )
        .expect("should resolve EIDs");

        let atype = crate::settings::EcPartner::default_openrtb_atype();
        let expected: Vec<Eid> = [("a.example.com", "ua"), ("b.example.com", "ub")]
            .into_iter()
            .map(|(source, uid)| {
                Eid::new(
                    source.to_owned(),
                    vec![crate::openrtb::Uid {
                        id: uid.to_owned(),
                        atype: Some(atype),
                        ext: None,
                    }],
                )
            })
            .collect();
        assert_eq!(
            eids, expected,
            "should send exactly the EIDs sent before identity providers existed"
        );
    }

    #[test]
    fn resolve_client_auction_eids_falls_back_to_ts_eids_cookie() {
        let cookie_payload = json!([
            {
                "source": "sharedid.org",
                "uids": [
                    { "id": "shared_cookie", "atype": 3 },
                    { "id": "shared_cookie_2", "ext": { "provider": "example" } }
                ]
            }
        ]);
        let encoded = BASE64
            .encode(serde_json::to_vec(&cookie_payload).expect("should serialize cookie payload"));

        let resolved = resolve_client_auction_eids(None, Some(&encoded))
            .expect("should fall back to structured ts-eids cookie");

        assert_eq!(resolved.len(), 1, "should preserve cookie source entry");
        assert_eq!(resolved[0].source, "sharedid.org");
        assert_eq!(
            resolved[0].uids.len(),
            2,
            "should preserve multiple cookie UIDs"
        );
        assert_eq!(resolved[0].uids[0].id, "shared_cookie");
        assert_eq!(
            resolved[0].uids[1].ext,
            Some(json!({ "provider": "example" })),
            "should preserve UID ext from cookie fallback"
        );
    }

    #[test]
    fn resolve_client_auction_eids_prefers_request_body_over_cookie() {
        let raw = serde_json::value::to_raw_value(&json!([
            {
                "source": "id5-sync.com",
                "uids": [{ "id": "body_uid", "atype": 1 }]
            }
        ]))
        .expect("should capture raw EIDs");
        let cookie_payload = json!([
            {
                "source": "sharedid.org",
                "uids": [{ "id": "cookie_uid", "atype": 3 }]
            }
        ]);
        let encoded = BASE64
            .encode(serde_json::to_vec(&cookie_payload).expect("should serialize cookie payload"));

        let resolved = resolve_client_auction_eids(Some(raw.as_ref()), Some(&encoded))
            .expect("should prefer request body EIDs");

        assert_eq!(
            serde_json::to_value(&resolved).expect("should serialize resolved EIDs"),
            json!([{ "source": "id5-sync.com", "uids": [{ "id": "body_uid", "atype": 1 }] }]),
            "should use the request body when it has valid entries"
        );
    }

    const SOURCE_S: &str = "ids.example.com";
    const SOURCE_T: &str = "other.example.com";

    fn kv_eid(source: &str, id: &str, atype: i32) -> Eid {
        Eid::new(
            source.to_owned(),
            vec![Uid {
                id: id.to_owned(),
                atype: Some(atype),
                ext: None,
            }],
        )
    }

    /// A browser entry, every field spelled out (spec 7.1 rule 9).
    fn browser_eid(
        source: &str,
        id: &str,
        atype: Option<i32>,
        inserter: Option<&str>,
        mm: Option<i32>,
    ) -> Eid {
        Eid {
            source: source.to_owned(),
            uids: vec![Uid {
                id: id.to_owned(),
                atype,
                ext: None,
            }],
            inserter: inserter.map(str::to_owned),
            matcher: None,
            mm,
            ext: None,
        }
    }

    fn merged_json(client: Option<Vec<Eid>>, kv: Option<Vec<Eid>>) -> JsonValue {
        let merged = merge_auction_eids(
            &client.unwrap_or_default(),
            &ServerSet::from_eids(kv.unwrap_or_default()),
            &Denials::default(),
        );
        let merged = (!merged.is_empty()).then_some(merged);
        serde_json::to_value(merged).expect("should serialize merged EIDs")
    }

    const MERGE_EC_ID_PREFIX: &str = "merge";

    fn merge_ec_id() -> String {
        format!("{}.{MERGE_EC_ID_PREFIX}", "a".repeat(64))
    }

    fn merge_registry(sources: &[&str]) -> PartnerRegistry {
        let partners: Vec<_> = sources
            .iter()
            .map(|source| counting_test_partner(source))
            .collect();
        PartnerRegistry::from_config(&partners).expect("should build registry")
    }

    fn merge_entry(records: Vec<(&str, KvPartnerId)>) -> KvEntry {
        let mut entry = KvEntry::minimal("unused.example.com", "unused", 1);
        entry.ids.clear();
        for (source, record) in records {
            entry.ids.insert(source.to_owned(), record);
        }
        entry
    }

    /// The request's [`AuctionIdentity`] for `entry`, with no providers.
    fn merge_identity(entry: KvEntry, registry: &PartnerRegistry) -> AuctionIdentity {
        let ec_id = merge_ec_id();
        let ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));
        let snapshot = EcKvSnapshot::Present {
            ec_id,
            entry: Box::new(entry),
            generation: None,
        };
        auction_identity(
            &snapshot,
            Some(registry),
            &ec_context,
            PolicySignals::default(),
        )
    }

    fn default_atype() -> i32 {
        crate::settings::EcPartner::default_openrtb_atype()
    }

    fn plain_eid(source: &str, id: &str, atype: Option<i32>) -> Eid {
        Eid {
            source: source.to_owned(),
            uids: vec![Uid {
                id: id.to_owned(),
                atype,
                ext: None,
            }],
            inserter: None,
            matcher: None,
            mm: None,
            ext: None,
        }
    }

    #[test]
    fn merge_browser_value_wins_for_unowned_source() {
        let registry = merge_registry(&[SOURCE_S]);
        let identity = merge_identity(
            merge_entry(vec![(SOURCE_S, KvPartnerId::new("server"))]),
            &registry,
        );

        let merged = identity.merge(Some(&[plain_eid(SOURCE_S, "browser", Some(1))]));

        assert_eq!(
            merged,
            Some(vec![plain_eid(SOURCE_S, "browser", Some(1))]),
            "the browser's value should win for an unowned source"
        );
    }

    #[test]
    fn merge_server_value_goes_out_when_the_browser_did_not_send_the_source() {
        let registry = merge_registry(&[SOURCE_S, SOURCE_T]);
        let identity = merge_identity(
            merge_entry(vec![(SOURCE_T, KvPartnerId::new("server"))]),
            &registry,
        );

        let merged = identity.merge(Some(&[plain_eid(SOURCE_S, "browser", Some(1))]));

        assert_eq!(
            merged,
            Some(vec![
                plain_eid(SOURCE_S, "browser", Some(1)),
                plain_eid(SOURCE_T, "server", Some(default_atype())),
            ]),
            "the server value should follow the browser entries"
        );
    }

    #[test]
    fn merge_expired_entry_is_not_sent() {
        let registry = merge_registry(&[SOURCE_S, SOURCE_T]);
        let expired = KvPartnerId {
            exp: Some(1),
            ..KvPartnerId::new("expired")
        };
        let identity = merge_identity(
            merge_entry(vec![
                (SOURCE_S, expired),
                (SOURCE_T, KvPartnerId::new("live")),
            ]),
            &registry,
        );

        assert_eq!(
            identity.merge(None),
            Some(vec![plain_eid(SOURCE_T, "live", Some(default_atype()))]),
            "an expired record should not be sent"
        );
    }

    #[test]
    fn merge_stored_atype_and_ext_are_emitted() {
        let registry = merge_registry(&[SOURCE_S]);
        let identity = merge_identity(
            merge_entry(vec![(SOURCE_S, full_record(None, example_entry_set()))]),
            &registry,
        );

        assert_eq!(
            identity.merge(None),
            Some(vec![
                Eid {
                    source: SOURCE_S.to_owned(),
                    uids: vec![
                        Uid {
                            id: "example-a".to_owned(),
                            atype: Some(1),
                            ext: None,
                        },
                        Uid {
                            id: "example-b".to_owned(),
                            atype: Some(3),
                            ext: Some(json!({ "stype": "ppuid" })),
                        },
                    ],
                    inserter: Some("example.com".to_owned()),
                    matcher: None,
                    mm: Some(3),
                    ext: None,
                },
                Eid {
                    source: SOURCE_S.to_owned(),
                    uids: vec![Uid {
                        id: "example-c".to_owned(),
                        atype: Some(1),
                        ext: None,
                    }],
                    inserter: None,
                    matcher: Some("partner.example.com".to_owned()),
                    mm: None,
                    ext: None,
                },
            ]),
            "the stored atype, UID ext and EID-level fields should go out"
        );
    }

    #[test]
    fn merge_entry_set_goes_out_one_eid_per_entry_in_stored_order_after_browser_entries() {
        let registry = merge_registry(&[SOURCE_S, SOURCE_T]);
        let identity = merge_identity(
            merge_entry(vec![(SOURCE_S, full_record(None, example_entry_set()))]),
            &registry,
        );

        let merged = identity
            .merge(Some(&[plain_eid(SOURCE_T, "browser", None)]))
            .expect("should merge EIDs");

        let shape: Vec<(String, Vec<String>)> = merged
            .iter()
            .map(|eid| {
                (
                    eid.source.clone(),
                    eid.uids.iter().map(|uid| uid.id.clone()).collect(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (SOURCE_T.to_owned(), vec!["browser".to_owned()]),
                (
                    SOURCE_S.to_owned(),
                    vec!["example-a".to_owned(), "example-b".to_owned()]
                ),
                (SOURCE_S.to_owned(), vec!["example-c".to_owned()]),
            ],
            "the stored set should go out one EID per entry, in order, after the browser"
        );
    }

    fn decode_eids_header(value: &str) -> Vec<Eid> {
        serde_json::from_slice(&BASE64.decode(value).expect("should be base64"))
            .expect("should be EID JSON")
    }

    #[test]
    fn merge_x_ts_eids_matches_what_was_sent() {
        let registry = merge_registry(&[SOURCE_S, SOURCE_T]);
        let identity = merge_identity(
            merge_entry(vec![(SOURCE_S, full_record(None, example_entry_set()))]),
            &registry,
        );
        let merged = identity
            .merge(Some(&[plain_eid(SOURCE_T, "browser", Some(1))]))
            .expect("should merge EIDs");

        let (header, truncated) =
            crate::ec::eids::encode_eids_header(&merged).expect("should encode the header");

        assert!(!truncated, "should fit without truncation");
        assert_eq!(
            decode_eids_header(&header),
            merged,
            "x-ts-eids should carry exactly what was sent"
        );
    }

    #[test]
    fn merge_header_still_truncates_per_entry() {
        let registry = merge_registry(&[SOURCE_S]);
        let entries: Vec<KvEid> = (0..40)
            .map(|index| KvEid {
                uids: vec![KvUid {
                    id: format!("{index:03}-{}", "x".repeat(100)),
                    atype: Some(1),
                    ext: None,
                }],
                ..KvEid::default()
            })
            .collect();
        let identity = merge_identity(
            merge_entry(vec![(SOURCE_S, full_record(None, entries))]),
            &registry,
        );
        let merged = identity.merge(None).expect("should merge EIDs");

        let (header, truncated) =
            crate::ec::eids::encode_eids_header(&merged).expect("should encode the header");

        assert_eq!(merged.len(), 40, "user.eids should hold the whole set");
        assert!(truncated, "x-ts-eids-truncated should be set");
        let carried = decode_eids_header(&header);
        assert!(
            !carried.is_empty() && carried.len() < merged.len(),
            "x-ts-eids should hold the entries that fit"
        );
        assert_eq!(
            carried,
            merged[..carried.len()].to_vec(),
            "the header should hold a prefix of whole entries"
        );
    }

    #[test]
    fn merge_denials_apply_through_the_request_identity() {
        let partner = ec_partner(
            "ssp.example.com",
            "https://sync.ssp.example.com/pull",
            "example-token-value-0123456789abcdef",
        );
        let mut settings = create_test_settings();
        settings.ec.partners = vec![partner];
        let runtime = std::sync::Arc::new(
            IdentityRuntime::from_settings(
                &settings,
                &crate::integrations::IntegrationRegistry::default(),
            )
            .expect("should build the runtime"),
        );
        let registry = merge_registry(&["ssp.example.com"]);
        let ec_id = merge_ec_id();
        let mut ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));
        ec_context.set_identity_runtime(runtime);
        let mut entry = merge_entry(Vec::new());
        entry.ps.insert(
            "pull-ssp-example-com".to_owned(),
            KvProviderState {
                iv: vec![uid_hash("invalidated")],
                ..KvProviderState::default()
            },
        );
        let snapshot = EcKvSnapshot::Present {
            ec_id,
            entry: Box::new(entry),
            generation: None,
        };
        let identity = auction_identity(
            &snapshot,
            Some(&registry),
            &ec_context,
            PolicySignals::default(),
        );

        let merged = identity.merge(Some(&[
            plain_eid("ssp.example.com", "invalidated", Some(1)),
            plain_eid(SOURCE_T, "kept", Some(1)),
        ]));

        assert_eq!(
            merged,
            Some(vec![plain_eid(SOURCE_T, "kept", Some(1))]),
            "a browser UID the claiming provider invalidated should not be sent"
        );
    }

    /// The request identity for `entry` when `provider` owns each of `owned`.
    fn owned_identity(
        entry: KvEntry,
        registry: &PartnerRegistry,
        provider: Arc<dyn crate::ec::identity::provider::IdentityProvider>,
        owned: &[&str],
        consent: ConsentContext,
    ) -> (AuctionIdentity, EcContext) {
        let owners = owned
            .iter()
            .map(|name| (source(name), provider.descriptor().id().clone()))
            .collect();
        let runtime = Arc::new(
            IdentityRuntime::from_settings_with_providers(
                &create_test_settings(),
                vec![provider],
                owners,
            )
            .expect("should build the runtime"),
        );
        let ec_id = merge_ec_id();
        let mut ec_context = EcContext::new_for_test(Some(ec_id.clone()), consent);
        ec_context.set_identity_runtime(runtime);
        ec_context.enable_mirror_for_test();
        let snapshot = EcKvSnapshot::Present {
            ec_id,
            entry: Box::new(entry),
            generation: None,
        };
        let identity = auction_identity(
            &snapshot,
            Some(registry),
            &ec_context,
            PolicySignals::default(),
        );
        (identity, ec_context)
    }

    fn non_regulated_consent() -> ConsentContext {
        ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            ..ConsentContext::default()
        }
    }

    fn owner_provider(expiring: bool) -> Arc<dyn crate::ec::identity::provider::IdentityProvider> {
        let mut builder =
            crate::ec::identity::descriptor::ProviderDescriptor::builder(pid("owner"))
                .with_source(source(SOURCE_S))
                .with_resolves(
                    crate::ec::identity::descriptor::Resolves::new()
                        .with_endpoint("https://api.example.com/fetch"),
                )
                .with_consent_policy(crate::ec::identity::test_support::resolver_policy());
        if expiring {
            builder = builder.with_expiring_ids();
        }
        provider_from(builder.build().expect("should build the descriptor"))
    }

    fn owner_record(uid: &str) -> KvPartnerId {
        KvPartnerId {
            w: Some("owner".to_owned()),
            ..KvPartnerId::new(uid)
        }
    }

    #[test]
    fn owner_valid_value_beats_browser_value() {
        let registry = merge_registry(&[SOURCE_S, SOURCE_T]);
        let (identity, _) = owned_identity(
            merge_entry(vec![(SOURCE_S, owner_record("owner-uid"))]),
            &registry,
            owner_provider(false),
            &[SOURCE_S],
            non_regulated_consent(),
        );

        let merged = identity.merge(Some(&[
            plain_eid(SOURCE_S, "browser", Some(1)),
            plain_eid(SOURCE_T, "other", Some(1)),
        ]));

        assert_eq!(
            merged,
            Some(vec![
                plain_eid(SOURCE_T, "other", Some(1)),
                plain_eid(SOURCE_S, "owner-uid", Some(default_atype())),
            ]),
            "the owner's value should replace the browser's, after the browser entries"
        );
    }

    #[test]
    fn owner_valid_set_replaces_every_repeated_browser_entry() {
        let registry = merge_registry(&[SOURCE_S, SOURCE_T]);
        let (identity, _) = owned_identity(
            merge_entry(vec![(
                SOURCE_S,
                full_record(Some("owner"), example_entry_set()),
            )]),
            &registry,
            owner_provider(false),
            &[SOURCE_S],
            non_regulated_consent(),
        );

        let merged = identity.merge(Some(&[
            plain_eid(SOURCE_S, "browser-one", Some(1)),
            plain_eid(SOURCE_T, "other", Some(1)),
            Eid {
                inserter: Some("inserter.example.com".to_owned()),
                ..plain_eid(SOURCE_S, "browser-two", Some(3))
            },
        ]));

        assert_eq!(
            merged,
            Some(vec![
                plain_eid(SOURCE_T, "other", Some(1)),
                Eid {
                    source: SOURCE_S.to_owned(),
                    uids: vec![
                        Uid {
                            id: "example-a".to_owned(),
                            atype: Some(1),
                            ext: None,
                        },
                        Uid {
                            id: "example-b".to_owned(),
                            atype: Some(3),
                            ext: Some(json!({ "stype": "ppuid" })),
                        },
                    ],
                    inserter: Some("example.com".to_owned()),
                    matcher: None,
                    mm: Some(3),
                    ext: None,
                },
                Eid {
                    source: SOURCE_S.to_owned(),
                    uids: vec![Uid {
                        id: "example-c".to_owned(),
                        atype: Some(1),
                        ext: None,
                    }],
                    inserter: None,
                    matcher: Some("partner.example.com".to_owned()),
                    mm: None,
                    ext: None,
                },
            ]),
            "exactly the owner's two entries should go out, in stored order, and no browser entry of the source"
        );
    }

    #[test]
    fn browser_seeded_value_never_beats_browser_current_value() {
        let registry = merge_registry(&[SOURCE_S]);
        let (identity, _) = owned_identity(
            merge_entry(vec![(SOURCE_S, KvPartnerId::new("seeded"))]),
            &registry,
            owner_provider(false),
            &[SOURCE_S],
            non_regulated_consent(),
        );

        let sent = identity.merge(Some(&[plain_eid(SOURCE_S, "current", Some(1))]));
        let silent = identity.merge(None);

        assert_eq!(
            sent,
            Some(vec![plain_eid(SOURCE_S, "current", Some(1))]),
            "the browser's current value should beat a browser-seeded record"
        );
        assert_eq!(
            silent,
            Some(vec![plain_eid(SOURCE_S, "seeded", Some(default_atype()))]),
            "control: a seeded value of a source that does not expire should go out when the browser is silent"
        );
    }

    #[test]
    fn expiring_owned_source_never_serves_browser_seeded_value() {
        let registry = merge_registry(&[SOURCE_S]);
        let (expiring, _) = owned_identity(
            merge_entry(vec![(SOURCE_S, KvPartnerId::new("seeded"))]),
            &registry,
            owner_provider(true),
            &[SOURCE_S],
            non_regulated_consent(),
        );
        let (owned_expiring, _) = owned_identity(
            merge_entry(vec![(
                SOURCE_S,
                KvPartnerId {
                    exp: Some(current_timestamp() + 1_000),
                    ..owner_record("owner-uid")
                },
            )]),
            &registry,
            owner_provider(true),
            &[SOURCE_S],
            non_regulated_consent(),
        );

        assert_eq!(
            expiring.merge(None),
            None,
            "should not serve a browser-seeded value of an expiring owned source"
        );
        assert_eq!(
            owned_expiring.merge(None),
            Some(vec![plain_eid(
                SOURCE_S,
                "owner-uid",
                Some(default_atype())
            )]),
            "control: the owner's unexpired value should go out"
        );
    }

    #[test]
    fn unowned_source_keeps_the_browser_first_rule() {
        let registry = merge_registry(&[SOURCE_S, SOURCE_T]);
        let (identity, _) = owned_identity(
            merge_entry(vec![
                (SOURCE_S, owner_record("owner-uid")),
                (SOURCE_T, owner_record("claimed-uid")),
            ]),
            &registry,
            owner_provider(false),
            &[SOURCE_S],
            non_regulated_consent(),
        );

        let merged = identity.merge(Some(&[plain_eid(SOURCE_T, "browser", Some(1))]));

        assert_eq!(
            merged,
            Some(vec![
                plain_eid(SOURCE_T, "browser", Some(1)),
                plain_eid(SOURCE_S, "owner-uid", Some(default_atype())),
            ]),
            "the browser should still win for a source with no owner"
        );
    }

    #[test]
    fn targeted_opt_out_blocks_server_ids_flush_and_mirror() {
        let registry = merge_registry(&[SOURCE_S]);
        let consent_with = |targeted: bool| ConsentContext {
            jurisdiction: Jurisdiction::NonRegulated,
            gpp: Some(crate::consent::types::GppConsent {
                version: 1,
                section_ids: vec![7],
                eu_tcf: None,
                us_sale_opt_out: Some(false),
                us_sharing_opt_out: None,
                us_targeted_advertising_opt_out: Some(targeted),
                us_gpc: None,
                us_known_child: None,
                us_privacy_section: None,
                us_section_decode_failed: false,
            }),
            ..ConsentContext::default()
        };
        let provider = owner_provider(false);
        let entry = || merge_entry(vec![(SOURCE_S, owner_record("owner-uid"))]);
        let (blocked, blocked_context) = owned_identity(
            entry(),
            &registry,
            provider.clone(),
            &[SOURCE_S],
            consent_with(true),
        );
        let (allowed, allowed_context) = owned_identity(
            entry(),
            &registry,
            provider.clone(),
            &[SOURCE_S],
            consent_with(false),
        );

        assert_eq!(
            blocked.merge(None),
            None,
            "a targeted-advertising opt-out should leave no server-side ID in user.eids"
        );
        assert_eq!(
            blocked.mirror_member(&blocked_context),
            Some(json!({ "clear": true })),
            "the ts-eids-s mirror should carry none of the provider's entries"
        );
        assert!(
            allowed.merge(None).is_some()
                && allowed
                    .mirror_member(&allowed_context)
                    .is_some_and(|member| member.get("value").is_some()),
            "control: without the opt-out the ID should go out and be mirrored"
        );

        let catalog = ProviderCatalog::new(vec![provider.clone()], BTreeMap::new());
        let settings = crate::settings::ProviderSettings::default();
        let identity_settings = crate::settings::IdentitySettings::default();
        let check = |consent: &ConsentContext| {
            crate::ec::identity::coordinator::eligibility(
                &crate::ec::identity::coordinator::EligibilityInput {
                    provider: provider.as_ref(),
                    settings: &settings,
                    identity: &identity_settings,
                    catalog: &catalog,
                    consent,
                },
                &crate::ec::identity::coordinator::EligibilityView::empty(),
                &crate::ec::identity::test_support::facts_for_writer(),
                crate::ec::identity::coordinator::Phase::AfterSend,
            )
        };
        assert!(
            matches!(
                check(&consent_with(true)),
                crate::ec::identity::coordinator::Eligibility::Skip(
                    crate::ec::identity::coordinator::SkipReason::ConsentPolicy
                )
            ),
            "the provider should not be dispatched, so nothing of it is flushed"
        );
        assert!(
            matches!(
                check(&consent_with(false)),
                crate::ec::identity::coordinator::Eligibility::Eligible { .. }
            ),
            "control: without the opt-out the provider should be eligible"
        );
    }

    #[test]
    fn golden_merge_one_uid_row_goes_out_as_on_main() {
        let registry = merge_registry(&["a.example.com", "b.example.com"]);
        let identity = merge_identity(
            merge_entry(vec![
                ("b.example.com", KvPartnerId::new("ub")),
                ("a.example.com", KvPartnerId::new("ua")),
            ]),
            &registry,
        );

        // Written from `main`'s `merge_auction_eids(None, Some(to_eids(..)))`.
        let expected = vec![
            Eid {
                source: "a.example.com".to_owned(),
                uids: vec![Uid {
                    id: "ua".to_owned(),
                    atype: Some(3),
                    ext: None,
                }],
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
            },
            Eid {
                source: "b.example.com".to_owned(),
                uids: vec![Uid {
                    id: "ub".to_owned(),
                    atype: Some(3),
                    ext: None,
                }],
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
            },
        ];
        assert_eq!(
            identity.merge(None),
            Some(expected),
            "a one-UID row should go out exactly as on main"
        );
    }

    #[test]
    fn golden_merge_browser_entries_are_forwarded_unchanged() {
        let client = vec![
            Eid {
                source: SOURCE_S.to_owned(),
                uids: vec![
                    Uid {
                        id: "s1".to_owned(),
                        atype: Some(1),
                        ext: Some(json!({ "stype": "ppuid" })),
                    },
                    Uid {
                        id: "s2".to_owned(),
                        atype: None,
                        ext: None,
                    },
                ],
                inserter: Some("inserter-1.example.com".to_owned()),
                matcher: Some("matcher.example.com".to_owned()),
                mm: Some(1),
                ext: Some(json!({ "segments": ["a"] })),
            },
            Eid {
                source: SOURCE_T.to_owned(),
                uids: Vec::new(),
                inserter: None,
                matcher: None,
                mm: None,
                ext: None,
            },
            Eid {
                source: SOURCE_S.to_owned(),
                uids: vec![Uid {
                    id: "s1".to_owned(),
                    atype: Some(3),
                    ext: None,
                }],
                inserter: Some("inserter-2.example.com".to_owned()),
                matcher: None,
                mm: Some(2),
                ext: None,
            },
        ];

        let merged = merge_auction_eids(&client, &ServerSet::default(), &Denials::default());

        assert_eq!(
            merged,
            vec![client[0].clone(), client[2].clone()],
            "browser entries should go out as today, minus entries without UIDs"
        );
    }

    #[test]
    fn merge_case_1_forwards_a_browser_entry_with_its_fields() {
        let merged = merged_json(
            Some(vec![browser_eid(
                SOURCE_S,
                "u1",
                None,
                Some("inserter-1.example.com"),
                Some(1),
            )]),
            None,
        );

        assert_eq!(
            merged,
            json!([{ "source": SOURCE_S, "inserter": "inserter-1.example.com", "mm": 1, "uids": [{ "id": "u1" }] }]),
            "a browser entry should go out with its EID-level fields"
        );
    }

    #[test]
    fn merge_case_2_keeps_the_browser_atype_for_a_shared_uid() {
        let merged = merged_json(
            Some(vec![browser_eid(
                SOURCE_S,
                "u1",
                Some(1),
                Some("inserter-1.example.com"),
                Some(1),
            )]),
            Some(vec![kv_eid(SOURCE_S, "u1", 3)]),
        );

        assert_eq!(
            merged,
            json!([{ "source": SOURCE_S, "inserter": "inserter-1.example.com", "mm": 1, "uids": [{ "id": "u1", "atype": 1 }] }]),
            "KV's operator-configured atype should not override the browser's"
        );
    }

    #[test]
    fn merge_case_3_drops_the_stale_kv_uid_for_a_browser_source() {
        let merged = merged_json(
            Some(vec![browser_eid(
                SOURCE_S,
                "u1",
                None,
                Some("inserter-1.example.com"),
                Some(1),
            )]),
            Some(vec![kv_eid(SOURCE_S, "u0", 3)]),
        );

        assert_eq!(
            merged,
            json!([{ "source": SOURCE_S, "inserter": "inserter-1.example.com", "mm": 1, "uids": [{ "id": "u1" }] }]),
            "KV's value for a source the browser sends should not be forwarded"
        );
    }

    #[test]
    fn merge_case_4_appends_kv_sources_the_browser_did_not_send() {
        let merged = merged_json(
            Some(vec![browser_eid(SOURCE_S, "u1", None, None, None)]),
            Some(vec![kv_eid(SOURCE_T, "t1", 3)]),
        );

        assert_eq!(
            merged,
            json!([
                { "source": SOURCE_S, "uids": [{ "id": "u1" }] },
                { "source": SOURCE_T, "uids": [{ "id": "t1", "atype": 3 }] }
            ]),
            "browser entries should come first and KV-only sources after"
        );
    }

    #[test]
    fn merge_case_5_keeps_same_source_entries_with_different_fields_apart() {
        let merged = merged_json(
            Some(vec![
                browser_eid(
                    SOURCE_S,
                    "u1",
                    None,
                    Some("inserter-1.example.com"),
                    Some(1),
                ),
                browser_eid(
                    SOURCE_S,
                    "u1",
                    None,
                    Some("inserter-2.example.com"),
                    Some(2),
                ),
            ]),
            None,
        );

        assert_eq!(
            merged,
            json!([
                { "source": SOURCE_S, "inserter": "inserter-1.example.com", "mm": 1, "uids": [{ "id": "u1" }] },
                { "source": SOURCE_S, "inserter": "inserter-2.example.com", "mm": 2, "uids": [{ "id": "u1" }] }
            ]),
            "two browser entries for one source should stay two entries"
        );
    }

    #[test]
    fn merge_case_6_matches_sources_as_the_registry_normalizes_them() {
        let merged = merged_json(
            Some(vec![browser_eid(
                "IDS.Example.com.",
                "u1",
                None,
                None,
                None,
            )]),
            Some(vec![kv_eid(SOURCE_S, "u0", 3)]),
        );

        assert_eq!(
            merged,
            json!([{ "source": "IDS.Example.com.", "uids": [{ "id": "u1" }] }]),
            "a browser source differing only in case or a trailing dot should suppress KV's"
        );
    }

    #[test]
    fn merge_case_7_forwards_kv_entries_when_the_browser_sent_none() {
        let merged = merged_json(
            None,
            Some(vec![kv_eid(SOURCE_S, "u0", 3), kv_eid(SOURCE_T, "t1", 3)]),
        );

        assert_eq!(
            merged,
            json!([
                { "source": SOURCE_S, "uids": [{ "id": "u0", "atype": 3 }] },
                { "source": SOURCE_T, "uids": [{ "id": "t1", "atype": 3 }] }
            ]),
            "KV entries should be forwarded unchanged without browser EIDs"
        );
    }

    #[test]
    fn merge_drops_entries_without_uids() {
        let empty_browser = Eid::new(SOURCE_S.to_owned(), Vec::new());
        let empty_kv = Eid::new(SOURCE_T.to_owned(), Vec::new());

        assert_eq!(
            merged_json(Some(vec![empty_browser]), Some(vec![empty_kv])),
            JsonValue::Null,
            "entries without UIDs should be dropped, leaving no EIDs"
        );
    }

    #[test]
    fn merge_auction_eids_deduplicates_client_and_resolved_ids() {
        let merged = merged_json(
            Some(vec![kv_eid("id5-sync.com", "ID5_abc", 1)]),
            Some(vec![
                kv_eid("id5-sync.com", "ID5_abc", 1),
                kv_eid("liveramp.com", "LR_xyz", 3),
            ]),
        );

        assert_eq!(
            merged,
            json!([
                { "source": "id5-sync.com", "uids": [{ "id": "ID5_abc", "atype": 1 }] },
                { "source": "liveramp.com", "uids": [{ "id": "LR_xyz", "atype": 3 }] }
            ]),
            "the browser's id5 entry should go out once, followed by KV's liveramp"
        );
    }

    #[test]
    fn merge_auction_eids_does_not_union_kv_uids_into_a_browser_source() {
        let merged = merged_json(
            Some(vec![browser_eid(
                "sharedid.org",
                "shared_client",
                None,
                None,
                None,
            )]),
            Some(vec![kv_eid("sharedid.org", "shared_server", 3)]),
        );

        assert_eq!(
            merged,
            json!([{ "source": "sharedid.org", "uids": [{ "id": "shared_client" }] }]),
            "KV's UID for a source the browser sends should not be added to it"
        );
    }

    #[test]
    fn merge_auction_eids_keeps_browser_metadata_on_conflict() {
        let client = Eid::new(
            "adserver.org".to_owned(),
            vec![Uid {
                id: "shared_uid".to_owned(),
                atype: Some(1),
                ext: Some(json!({ "provider": "client" })),
            }],
        );
        let resolved = Eid::new(
            "adserver.org".to_owned(),
            vec![Uid {
                id: "shared_uid".to_owned(),
                atype: Some(3),
                ext: Some(json!({ "provider": "server" })),
            }],
        );

        assert_eq!(
            merged_json(Some(vec![client]), Some(vec![resolved])),
            json!([{
                "source": "adserver.org",
                "uids": [{ "id": "shared_uid", "atype": 1, "ext": { "provider": "client" } }]
            }]),
            "the browser's atype and ext should win over KV's"
        );
    }

    #[test]
    fn auction_rejects_oversized_body() {
        futures::executor::block_on(async {
            use edgezero_core::body::Body as EdgeBody;
            use http::{Method, Request as HttpRequest, StatusCode};

            use crate::auction::build_orchestrator;
            use crate::consent::ConsentContext;
            use crate::ec::EcContext;
            use crate::platform::test_support::noop_services;
            use crate::test_support::tests::create_test_settings;

            let settings = create_test_settings();
            let orchestrator = build_orchestrator(&settings).expect("should build orchestrator");
            let services = noop_services();
            let mut ec_context = EcContext::new_for_test(None, ConsentContext::default());
            let oversized = vec![b'x'; MAX_AUCTION_BODY_SIZE + 1];
            let req = HttpRequest::builder()
                .method(Method::POST)
                .uri("https://test.com/auction")
                .body(EdgeBody::from(oversized))
                .expect("should build request");
            let response = handle_auction(
                &settings,
                &orchestrator,
                None,
                None,
                &mut ec_context,
                &services,
                req,
            )
            .await
            .expect("should return 413 response for oversized body");
            assert_eq!(
                response.status(),
                StatusCode::PAYLOAD_TOO_LARGE,
                "should return 413 for auction body over limit"
            );
        });
    }

    #[test]
    fn auction_rejects_streaming_body_instead_of_treating_as_empty() {
        futures::executor::block_on(async {
            use bytes::Bytes;
            use edgezero_core::body::Body as EdgeBody;
            use http::{Method, Request as HttpRequest};

            use crate::auction::build_orchestrator;
            use crate::consent::ConsentContext;
            use crate::ec::EcContext;
            use crate::error::TrustedServerError;
            use crate::platform::test_support::noop_services;
            use crate::test_support::tests::create_test_settings;

            let settings = create_test_settings();
            let orchestrator = build_orchestrator(&settings).expect("should build orchestrator");
            let services = noop_services();
            let mut ec_context = EcContext::new_for_test(None, ConsentContext::default());
            let stream = futures::stream::iter([Bytes::from_static(br#"{}"#)]);
            let req = HttpRequest::builder()
                .method(Method::POST)
                .uri("https://test.com/auction")
                .body(EdgeBody::stream(stream))
                .expect("should build request");

            let result = handle_auction(
                &settings,
                &orchestrator,
                None,
                None,
                &mut ec_context,
                &services,
                req,
            )
            .await;

            let err = match result {
                Ok(_) => panic!("streaming body should be rejected"),
                Err(err) => err,
            };
            assert!(
                matches!(err.current_context(), TrustedServerError::BadRequest { .. }),
                "streaming request body should fail as bad request"
            );
        });
    }

    mod mirror {
        use super::*;
        use crate::ec::kv_types::{KvEntry, KvPartnerId};
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;

        const MIRROR_SOURCE_A: &str = "a.example.com";
        const MIRROR_SOURCE_B: &str = "b.example.com";

        fn mirror_registry() -> PartnerRegistry {
            let mut disabled = counting_test_partner("off.example.com");
            disabled.bidstream_enabled = false;
            PartnerRegistry::from_config(&[
                counting_test_partner(MIRROR_SOURCE_A),
                counting_test_partner(MIRROR_SOURCE_B),
                disabled,
            ])
            .expect("should build partner registry")
        }

        fn row_with(records: &[(&str, &str, Option<u64>)]) -> KvEntry {
            let mut entry = KvEntry::minimal("unregistered.example.com", "x", 1);
            entry.ids.clear();
            for (source, uid, exp) in records {
                entry.ids.insert(
                    (*source).to_owned(),
                    KvPartnerId {
                        exp: *exp,
                        ..KvPartnerId::browser(*uid, Some(1))
                    },
                );
            }
            entry
        }

        fn ec_id_for(tag: &str) -> String {
            format!("{}.{tag}", "a".repeat(64))
        }

        /// Records the EIDs its request carried, then answers no-bid so the
        /// auction completes and the response can be inspected.
        struct NoBidRecordingProvider {
            eids: Arc<Mutex<Option<JsonValue>>>,
        }

        #[async_trait::async_trait(?Send)]
        impl AuctionProvider for NoBidRecordingProvider {
            fn provider_name(&self) -> &str {
                "eid_recording_provider"
            }

            async fn request_bids(
                &self,
                request: &AuctionRequest,
                _context: &AuctionContext<'_>,
            ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
                *self.eids.lock().expect("should lock recorded eids") = Some(
                    serde_json::to_value(&request.user.eids)
                        .expect("should serialize recorded eids"),
                );
                Ok(ProviderRequestOutcome::Immediate(AuctionResponse::no_bid(
                    self.provider_name(),
                    0,
                )))
            }

            async fn parse_response(
                &self,
                _response: PlatformResponse,
                _response_time_ms: u64,
            ) -> Result<AuctionResponse, Report<TrustedServerError>> {
                panic!("parse_response must not run for an immediate answer");
            }

            fn timeout_ms(&self) -> u32 {
                100
            }

            fn backend_name(
                &self,
                _services: &RuntimeServices,
                _timeout_ms: u32,
            ) -> Option<String> {
                Some("record-backend".to_string())
            }
        }

        fn recording_orchestrator(recorded: &Arc<Mutex<Option<JsonValue>>>) -> AuctionOrchestrator {
            let mut orchestrator = AuctionOrchestrator::new(AuctionConfig {
                enabled: true,
                providers: AuctionConfig::legacy_provider_map(&["eid_recording_provider"]),
                timeout_ms: 2000,
                mediator: None,
                ..Default::default()
            });
            orchestrator.register_provider(Arc::new(NoBidRecordingProvider {
                eids: Arc::clone(recorded),
            }));
            orchestrator
        }

        async fn run_auction(
            ec_context: &mut EcContext,
            graph: Option<&KvIdentityGraph>,
            registry: Option<&PartnerRegistry>,
            body: Vec<u8>,
        ) -> JsonValue {
            let recorded = Arc::new(Mutex::new(None));
            let orchestrator = recording_orchestrator(&recorded);
            let req = Request::builder()
                .method("POST")
                .uri("https://test-publisher.com/auction")
                .body(EdgeBody::from(body))
                .expect("should build auction request");
            let response = handle_auction(
                &create_test_settings(),
                &orchestrator,
                graph,
                registry,
                ec_context,
                &noop_services(),
                req,
            )
            .await
            .expect("should answer the auction");
            let bytes = response
                .into_body()
                .into_bytes()
                .expect("should buffer the body");
            serde_json::from_slice(&bytes).expect("should parse the response")
        }

        fn mirror_of(response: &JsonValue) -> Option<&JsonValue> {
            response.get("ext").and_then(|ext| ext.get("ts_eids_s"))
        }

        fn decoded(mirror: &JsonValue) -> JsonValue {
            let value = mirror["value"].as_str().expect("should hold a value");
            let bytes = URL_SAFE_NO_PAD
                .decode(value)
                .expect("should be unpadded base64url");
            serde_json::from_slice(&bytes).expect("should be JSON")
        }

        fn live_context(ec_id: &str, graph: &KvIdentityGraph) -> EcContext {
            let _ = graph;
            let mut context = make_ec_context(Jurisdiction::NonRegulated, Some(ec_id));
            context.enable_mirror_for_test();
            context
        }

        #[tokio::test]
        async fn auction_body_eids_are_not_written_to_kv() {
            let (graph, store) = KvIdentityGraph::write_counting("test_store");
            let ec_id = ec_id_for("nowrit");
            graph
                .create(&ec_id, &row_with(&[]))
                .expect("should seed the live row");
            store.reset_counts();
            let registry = mirror_registry();
            let recorded = Arc::new(Mutex::new(None));
            let orchestrator = recording_orchestrator(&recorded);
            let mut ec_context = live_context(&ec_id, &graph);
            let req = Request::builder()
                .method("POST")
                .uri("https://test-publisher.com/auction")
                .body(EdgeBody::from(
                    serde_json::to_vec(&json!({
                        "adUnits": [{ "code": "div-gpt-1", "mediaTypes": { "banner": { "sizes": [[300, 250]] } } }],
                        "eids": [{ "source": MIRROR_SOURCE_A, "uids": [{ "id": "body-uid", "atype": 1 }] }]
                    }))
                    .expect("should serialize body"),
                ))
                .expect("should build auction request");

            let _ = handle_auction(
                &create_test_settings(),
                &orchestrator,
                Some(&graph),
                Some(&registry),
                &mut ec_context,
                &noop_services(),
                req,
            )
            .await;
            let mut response = http::Response::new(EdgeBody::empty());
            crate::ec::finalize::ec_finalize_response(
                &create_test_settings(),
                &mut ec_context,
                Some(&graph),
                &registry,
                None,
                None,
                &mut response,
            );

            let writes = store.writes();
            assert_eq!(
                (writes.add, writes.if_generation_match, writes.overwrite),
                (0, 0, 0),
                "should write nothing to KV"
            );
            assert_eq!(
                recorded.lock().expect("should lock recorded eids").clone(),
                Some(
                    json!([{ "source": MIRROR_SOURCE_A, "uids": [{ "id": "body-uid", "atype": 1 }] }])
                ),
                "should still send the body's UID to the bidder"
            );
        }

        #[tokio::test]
        async fn auction_omits_the_mirror_while_it_is_disabled() {
            let ec_id = ec_id_for("mirror-off");
            let graph = KvIdentityGraph::in_memory("test_store");
            graph
                .create(&ec_id, &row_with(&[(MIRROR_SOURCE_A, "uid-a", None)]))
                .expect("should seed the live row");
            let registry = mirror_registry();
            let mut ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));

            let response = run_auction(
                &mut ec_context,
                Some(&graph),
                Some(&registry),
                ad_units_only_body(),
            )
            .await;

            assert_eq!(
                mirror_of(&response),
                None,
                "should return no mirror member while [ec.identity] mirror_enabled is off"
            );
        }

        #[tokio::test]
        async fn auction_returns_the_mirror_of_its_servable_set() {
            let ec_id = ec_id_for("mirror");
            let graph = KvIdentityGraph::in_memory("test_store");
            graph
                .create(&ec_id, &row_with(&[(MIRROR_SOURCE_A, "uid-a", None)]))
                .expect("should seed the live row");
            let registry = mirror_registry();
            let mut ec_context = live_context(&ec_id, &graph);

            let response = run_auction(
                &mut ec_context,
                Some(&graph),
                Some(&registry),
                ad_units_only_body(),
            )
            .await;

            let mirror = mirror_of(&response).expect("should carry the mirror");
            let atype = crate::settings::EcPartner::default_openrtb_atype();
            assert_eq!(
                decoded(mirror),
                json!({ "v": 1, "eids": [{ "source": MIRROR_SOURCE_A, "uids": [{ "id": "uid-a", "atype": atype }] }] }),
                "should hold exactly the servable record's EID"
            );
            assert_eq!(
                mirror["max_age"], 86_400,
                "should live a day without an exp"
            );
        }

        #[tokio::test]
        async fn auction_mirror_max_age_follows_the_earliest_exp() {
            let now = crate::ec::current_timestamp();
            let ec_id = ec_id_for("maxage");
            let graph = KvIdentityGraph::in_memory("test_store");
            graph
                .create(
                    &ec_id,
                    &row_with(&[
                        (MIRROR_SOURCE_A, "uid-a", Some(now + 600)),
                        (MIRROR_SOURCE_B, "uid-b", Some(now + 999_999)),
                    ]),
                )
                .expect("should seed the live row");
            let registry = mirror_registry();
            let mut ec_context = live_context(&ec_id, &graph);

            let response = run_auction(
                &mut ec_context,
                Some(&graph),
                Some(&registry),
                ad_units_only_body(),
            )
            .await;

            let max_age = mirror_of(&response).expect("should carry the mirror")["max_age"]
                .as_u64()
                .expect("should hold a number");
            assert!(
                (595..=600).contains(&max_age),
                "should take the earliest exp, got {max_age}"
            );
        }

        #[tokio::test]
        async fn auction_mirror_leaves_out_bidstream_disabled_sources() {
            let ec_id = ec_id_for("bidoff");
            let graph = KvIdentityGraph::in_memory("test_store");
            graph
                .create(
                    &ec_id,
                    &row_with(&[
                        (MIRROR_SOURCE_A, "uid-a", None),
                        ("off.example.com", "uid-off", None),
                    ]),
                )
                .expect("should seed the live row");
            let registry = mirror_registry();
            let mut ec_context = live_context(&ec_id, &graph);

            let response = run_auction(
                &mut ec_context,
                Some(&graph),
                Some(&registry),
                ad_units_only_body(),
            )
            .await;

            let sources: Vec<String> =
                decoded(mirror_of(&response).expect("should carry the mirror"))["eids"]
                    .as_array()
                    .expect("should hold EIDs")
                    .iter()
                    .map(|eid| eid["source"].as_str().unwrap_or_default().to_owned())
                    .collect();
            assert_eq!(
                sources,
                vec![MIRROR_SOURCE_A.to_owned()],
                "should leave out the source bidstream disabled"
            );
        }

        #[tokio::test]
        async fn auction_mirror_leaves_out_ids_consent_filters_out() {
            // TCF Purpose 1 granted, Purpose 4 refused: the auction runs, but
            // `gate_eids_by_consent` strips every EID the bidder would get.
            let ec_id = ec_id_for("gated");
            let graph = KvIdentityGraph::in_memory("test_store");
            graph
                .create(&ec_id, &row_with(&[(MIRROR_SOURCE_A, "uid-a", None)]))
                .expect("should seed the live row");
            let registry = mirror_registry();
            let mut ec_context =
                EcContext::new_for_test(Some(ec_id.clone()), gdpr_consent_with_purposes(&[1], 4));
            ec_context.enable_mirror_for_test();

            let response = run_auction(
                &mut ec_context,
                Some(&graph),
                Some(&registry),
                ad_units_only_body(),
            )
            .await;

            assert_eq!(
                mirror_of(&response),
                Some(&json!({ "clear": true })),
                "should not mirror an ID consent filtered out"
            );
        }

        fn gdpr_consent_with_purposes(granted: &[usize], _refused: usize) -> ConsentContext {
            let mut consent =
                crate::ec::identity::test_support::test_consent_gdpr_without_vendor(9_999);
            let tcf = consent.tcf.as_mut().expect("should hold a TCF string");
            tcf.purpose_consents = (1..=24).map(|purpose| granted.contains(&purpose)).collect();
            consent
        }

        #[tokio::test]
        async fn auction_returns_clear_on_consent_denial_tombstone_and_missing_row() {
            let registry = mirror_registry();
            let live = row_with(&[(MIRROR_SOURCE_A, "uid-a", None)]);
            let mut withdrawn = live.clone();
            withdrawn.consent.ok = false;
            let cases: [(&str, Option<KvEntry>, bool, JsonValue); 5] = [
                ("live row", Some(live.clone()), false, json!("value")),
                (
                    "tombstone",
                    Some(KvEntry::tombstone(1_000)),
                    false,
                    json!({ "clear": true }),
                ),
                (
                    "consent.ok false",
                    Some(withdrawn),
                    false,
                    json!({ "clear": true }),
                ),
                ("missing row", None, false, json!({ "clear": true })),
                ("consent denial", Some(live), true, json!({ "clear": true })),
            ];

            for (label, row, deny_consent, expected) in cases {
                let ec_id = ec_id_for("clears");
                let graph = KvIdentityGraph::in_memory("test_store");
                if let Some(row) = &row {
                    graph.create(&ec_id, row).expect("should seed the row");
                }
                let mut ec_context = if deny_consent {
                    // No TCF Purpose 1: the auction consent gate answers early.
                    EcContext::new_for_test(Some(ec_id.clone()), gdpr_consent_with_purposes(&[], 4))
                } else {
                    live_context(&ec_id, &graph)
                };

                let response = run_auction(
                    &mut ec_context,
                    Some(&graph),
                    Some(&registry),
                    ad_units_only_body(),
                )
                .await;

                let mirror = mirror_of(&response)
                    .unwrap_or_else(|| panic!("{label}: should carry a mirror"));
                if expected == json!("value") {
                    assert!(
                        mirror.get("value").is_some(),
                        "{label}: should carry a value"
                    );
                } else {
                    assert_eq!(mirror, &expected, "{label}: should clear");
                }
            }
        }

        #[tokio::test]
        async fn auction_returns_clear_when_the_servable_set_is_empty() {
            let ec_id = ec_id_for("empty");
            let graph = KvIdentityGraph::in_memory("test_store");
            graph
                .create(&ec_id, &row_with(&[("off.example.com", "uid-off", None)]))
                .expect("should seed the live row");
            let registry = mirror_registry();
            let mut ec_context = live_context(&ec_id, &graph);

            let response = run_auction(
                &mut ec_context,
                Some(&graph),
                Some(&registry),
                ad_units_only_body(),
            )
            .await;

            assert_eq!(
                mirror_of(&response),
                Some(&json!({ "clear": true })),
                "should clear an empty servable set"
            );
        }

        #[tokio::test]
        async fn auction_without_a_kv_read_omits_the_mirror() {
            let ec_id = ec_id_for("noread");
            let registry = mirror_registry();
            let mut ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));
            ec_context.enable_mirror_for_test();

            let response =
                run_auction(&mut ec_context, None, Some(&registry), ad_units_only_body()).await;

            assert!(
                response["ext"].get("ts_eids_s").is_none(),
                "should leave the mirror cookie as it is"
            );
        }
    }
}
