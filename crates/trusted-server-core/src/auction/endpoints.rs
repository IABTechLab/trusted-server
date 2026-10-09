//! HTTP endpoint handlers for auction requests.

use std::collections::HashMap;

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{Request, Response, StatusCode, header};
use serde_json::Value as JsonValue;

use crate::auction::formats::AdRequest;
use crate::auction::orchestrator::OrchestrationResult;
use crate::consent::{consent_allows_server_side_auction, gate_eids_by_consent};
use crate::ec::EcContext;
use crate::ec::EcKvSnapshot;
use crate::ec::eids::{resolve_partner_ids, to_eids};
use crate::ec::kv::KvIdentityGraph;
use crate::ec::kv_types::MAX_UID_LENGTH;
use crate::ec::partner::normalize_partner_source_domain;
use crate::ec::prebid_eids::collect_prebid_eid_updates_from_eids;
use crate::ec::registry::PartnerRegistry;
use crate::error::TrustedServerError;
use crate::openrtb::{Eid, Uid};
use crate::platform::RuntimeServices;
use crate::settings::Settings;

use super::AuctionOrchestrator;
use super::formats::{
    convert_to_openrtb_response, convert_to_openrtb_response_with_report,
    convert_tsjs_to_auction_request,
};
use super::telemetry::{
    AuctionObservationContext, AuctionSource, AuctionTerminalOutcome, build_auction_events,
    emit_auction_events_best_effort_lazy,
};
use super::types::AuctionContext;

const MAX_CLIENT_EID_SOURCES: usize = 64;
const MAX_CLIENT_UIDS_PER_SOURCE: usize = 32;
const MAX_CLIENT_EID_SOURCE_BYTES: usize = 255;

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

    // Keep normalized body updates on the adapter-owned context, not the
    // response, so handled provider errors cannot lose persistence work.
    let client_eids = if ec_id.is_some() {
        parse_client_auction_eids(body.eids.as_ref())
    } else {
        None
    };
    if let Some(registry) = registry
        && let Some(eids) = &client_eids
        && consent_allows_server_side_auction(&consent_context)
    {
        ec_context.stage_browser_eid_updates(collect_prebid_eid_updates_from_eids(eids, registry));
        ec_context.set_eid_sync_source(crate::ec::EidSyncSource::Auction);
    }

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
        return convert_to_openrtb_response(
            &empty_result,
            settings,
            &auction_request,
            ec_context.ec_allowed(),
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
        return convert_to_openrtb_response(
            &empty_result,
            settings,
            &auction_request,
            ec_context.ec_allowed(),
        );
    }

    // Resolve partner EIDs from the KV identity graph when the user has a valid
    // EC and both KV and partner stores are available. Gate the read on a
    // present registry: without one, `resolve_auction_eids` yields no
    // server-side EIDs, so the snapshot would be an unused billable KV read.
    let auction_kv_snapshot = match (kv, ec_id.as_deref(), registry) {
        (Some(graph), Some(ec_id), Some(_)) => graph.load_snapshot(ec_id),
        _ => EcKvSnapshot::NotRead,
    };
    // Hand the loaded row to the request context so response finalization —
    // which runs on an EC context the adapter owns, after this handler returns
    // — ingests body/sharedId updates from this read instead of paying
    // for a second lookup.
    if !matches!(auction_kv_snapshot, EcKvSnapshot::NotRead) {
        ec_context.set_kv_snapshot(auction_kv_snapshot.clone());
    }

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

    // Merge current-request client EIDs with KV-resolved EIDs, then apply
    // consent gating before attaching them to the auction request.
    let merged_eids =
        resolve_and_merge_auction_eids(client_eids, &auction_kv_snapshot, registry, ec_context);
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

/// Resolves partner EIDs from the KV identity graph for bidstream decoration.
///
/// Returns `None` when any prerequisite is missing (no KV store, no partner
/// store, no EC, consent denied). On KV or partner-resolution errors, logs a
/// warning and returns empty EIDs so the auction can proceed in degraded mode.
pub(crate) fn resolve_auction_eids(
    snapshot: &EcKvSnapshot,
    registry: Option<&PartnerRegistry>,
    ec_context: &EcContext,
    now: Option<u64>,
) -> Option<Vec<Eid>> {
    let registry = registry?;

    if !ec_context.ec_allowed() {
        return None;
    }

    let ec_id = ec_context.ec_value()?;

    let Some(entry) = snapshot.entry_for(ec_id) else {
        return Some(Vec::new());
    };

    if !entry.consent.ok {
        return Some(Vec::new());
    }

    let resolved = resolve_partner_ids(registry, entry, now);
    Some(to_eids(&resolved))
}

/// Merges browser and KV EIDs without letting browser input revive an unusable UID.
///
/// Uses one checked time for both filters and only the active full EC ID's row.
/// A different browser UID remains usable for this request, not a KV replacement.
pub(crate) fn resolve_and_merge_auction_eids(
    mut client_eids: Option<Vec<Eid>>,
    snapshot: &EcKvSnapshot,
    registry: Option<&PartnerRegistry>,
    ec_context: &EcContext,
) -> Option<Vec<Eid>> {
    if !ec_context.ec_allowed() {
        return None;
    }
    let ec_id = ec_context.ec_value()?;
    let now = crate::ec::checked_current_timestamp();
    if let (Some(eids), Some(entry)) = (&mut client_eids, snapshot.entry_for(ec_id)) {
        for eid in eids.iter_mut() {
            if let Ok(source) = normalize_partner_source_domain(eid.source.trim())
                && let Some(record) = entry.ids.get(&source)
                && !record.is_usable(now)
            {
                eid.uids.retain(|uid| uid.id != record.uid);
            }
        }
        eids.retain(|eid| !eid.uids.is_empty());
    }
    let resolved = resolve_auction_eids(snapshot, registry, ec_context, now);
    merge_auction_eids(client_eids, resolved)
}

fn parse_client_auction_eids(raw: Option<&JsonValue>) -> Option<Vec<Eid>> {
    let Some(JsonValue::Array(entries)) = raw else {
        return None;
    };

    let mut eids = Vec::new();

    for entry in entries {
        if eids.len() >= MAX_CLIENT_EID_SOURCES {
            log::debug!(
                "Auction EIDs: reached max client EID source count ({MAX_CLIENT_EID_SOURCES})"
            );
            break;
        }
        let JsonValue::Object(entry) = entry else {
            log::debug!("Auction EIDs: dropping malformed client EID entry");
            continue;
        };

        let Some(source) = entry
            .get("source")
            .and_then(JsonValue::as_str)
            .filter(|source| !source.trim().is_empty())
            .filter(|source| source.len() <= MAX_CLIENT_EID_SOURCE_BYTES)
            .map(str::to_owned)
        else {
            continue;
        };

        let Some(JsonValue::Array(raw_uids)) = entry.get("uids") else {
            continue;
        };

        let uids: Vec<_> = raw_uids
            .iter()
            .filter_map(parse_client_auction_uid)
            .take(MAX_CLIENT_UIDS_PER_SOURCE)
            .collect();
        if uids.is_empty() {
            continue;
        }

        eids.push(Eid {
            source,
            uids,
            inserter: entry
                .get("inserter")
                .and_then(JsonValue::as_str)
                .map(str::to_owned),
            matcher: entry
                .get("matcher")
                .and_then(JsonValue::as_str)
                .map(str::to_owned),
            mm: entry
                .get("mm")
                .and_then(JsonValue::as_i64)
                .and_then(|mm| i32::try_from(mm).ok()),
        });
    }

    if eids.is_empty() { None } else { Some(eids) }
}

fn parse_client_auction_uid(raw: &JsonValue) -> Option<Uid> {
    let JsonValue::Object(uid) = raw else {
        return None;
    };

    let id = uid
        .get("id")
        .and_then(JsonValue::as_str)
        .filter(|id| !id.trim().is_empty())
        .filter(|id| id.len() <= MAX_UID_LENGTH)?
        .to_owned();

    let atype = uid
        .get("atype")
        .and_then(JsonValue::as_u64)
        .and_then(|atype| i32::try_from(atype).ok());

    let ext = match uid.get("ext") {
        Some(JsonValue::Object(_)) => uid.get("ext").cloned(),
        _ => None,
    };

    Some(Uid { id, atype, ext })
}

pub(crate) fn merge_auction_eids(
    client_eids: Option<Vec<Eid>>,
    resolved_eids: Option<Vec<Eid>>,
) -> Option<Vec<Eid>> {
    let mut merged = Vec::new();

    for eid in resolved_eids
        .into_iter()
        .flatten()
        .chain(client_eids.into_iter().flatten())
    {
        if eid.source.is_empty() {
            continue;
        }

        let source_index = match merged
            .iter()
            .position(|existing: &Eid| existing.source == eid.source)
        {
            Some(index) => index,
            None => {
                merged.push(Eid {
                    source: eid.source.clone(),
                    uids: Vec::new(),
                    ..Eid::default()
                });
                merged.len() - 1
            }
        };

        // Browser provenance wins over missing KV provenance. Empty matcher
        // and zero-valued mm are both meaningful supplied values.
        if eid.inserter.is_some() {
            merged[source_index].inserter = eid.inserter;
        }
        if eid.matcher.is_some() {
            merged[source_index].matcher = eid.matcher;
        }
        if eid.mm.is_some() {
            merged[source_index].mm = eid.mm;
        }

        for uid in eid.uids {
            if uid.id.trim().is_empty() || uid.id.len() > MAX_UID_LENGTH {
                continue;
            }

            if let Some(existing_uid) = merged[source_index]
                .uids
                .iter_mut()
                .find(|existing| existing.id == uid.id)
            {
                if existing_uid.atype.is_none() {
                    existing_uid.atype = uid.atype;
                }
                if existing_uid.ext.is_none() {
                    existing_uid.ext = uid.ext;
                }
            } else {
                merged[source_index].uids.push(uid);
            }
        }
    }

    merged.retain(|eid| !eid.uids.is_empty());

    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
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
    use crate::constants::COOKIE_TS_EIDS;
    use crate::ec::kv_backend::test_support::InMemoryEcKv;
    use crate::ec::kv_backend::{
        EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome,
    };
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
    use serde_json::json;
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
            identity_owner: None,
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
            pull_sync_ttl_sec: crate::settings::EcPartner::default_pull_sync_ttl_sec(),
            pull_sync_rate_limit: crate::settings::EcPartner::default_pull_sync_rate_limit(),
            ts_pull_token: None,
        }
    }

    struct BrowserRecordingKv {
        inner: InMemoryEcKv,
        lookups: Arc<std::sync::atomic::AtomicUsize>,
        writes: Arc<Mutex<Vec<EcKvWriteMode>>>,
    }

    impl EcKvStore for BrowserRecordingKv {
        fn store_name(&self) -> &str {
            self.inner.store_name()
        }
        fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
            self.lookups
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            self.writes
                .lock()
                .expect("should lock write modes")
                .push(write.mode);
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

    #[tokio::test]
    async fn auction_endpoint_snapshot_is_reused_by_response_finalization() {
        // Body updates survive the provider error on the adapter-owned context.
        // Finalization combines sharedId and reuses the one auction lookup.
        let settings = create_test_settings();
        let mut orchestrator = AuctionOrchestrator::new(AuctionConfig {
            enabled: true,
            providers: AuctionConfig::legacy_provider_map(&["eid_capturing_provider"]),
            timeout_ms: 2000,
            mediator: None,
            ..Default::default()
        });
        orchestrator.register_provider(Arc::new(EidCapturingProvider {
            requests: Arc::new(Mutex::new(Vec::new())),
        }));
        let registry = PartnerRegistry::from_config(&[
            counting_test_partner("sharedid.org"),
            counting_test_partner("id5-sync.com"),
        ])
        .expect("should build partner registry");

        let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let writes = Arc::new(Mutex::new(Vec::new()));
        let graph = KvIdentityGraph::new(BrowserRecordingKv {
            inner: InMemoryEcKv::new("counting-store"),
            lookups: Arc::clone(&lookups),
            writes: Arc::clone(&writes),
        });
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mut live = crate::ec::kv_types::KvEntry::tombstone(1000);
        live.consent.ok = true;
        graph.create(&ec_id, &live).expect("should seed live row");
        lookups.store(0, std::sync::atomic::Ordering::Relaxed);
        writes.lock().expect("should lock write modes").clear();

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
                    ],
                    "eids": [
                        {"source":"id5-sync.com", "inserter":"forged", "matcher":"", "mm":0,
                         "uids":[{"id":"body-id5", "ext":{"writer":"forged"}}]},
                        {"source":"unknown.example", "uids":[{"id":"body-only"}]}
                    ]
                }))
                .expect("should serialize body"),
            ))
            .expect("should build auction request");

        // The capturing provider deliberately fails its launch; identity
        // resolution — the subject of this test — completes before dispatch.
        let result = handle_auction(
            &settings,
            &orchestrator,
            Some(&graph),
            Some(&registry),
            &mut ec_context,
            &noop_services(),
            req,
        )
        .await;
        assert!(
            result.is_err(),
            "provider launch failure should return a handled error"
        );

        assert_eq!(
            lookups.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "the endpoint should read the identity row exactly once"
        );
        assert!(
            ec_context.kv_snapshot().entry_for(&ec_id).is_some(),
            "the endpoint must hand its snapshot to the request context"
        );

        let mut response = http::Response::new(EdgeBody::empty());
        crate::ec::finalize::ec_finalize_response(
            &settings,
            &mut ec_context,
            Some(&graph),
            &registry,
            Some("shared-cookie-id"),
            &mut response,
        );

        assert_eq!(
            lookups.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "finalization must reuse the endpoint snapshot instead of reading again"
        );
        let recorded = writes.lock().expect("should lock write modes");
        assert_eq!(
            recorded.len(),
            1,
            "body and sharedId must make only one write"
        );
        assert!(matches!(recorded[0], EcKvWriteMode::IfGenerationMatch(_)));
        drop(recorded);
        let (stored, _) = graph
            .get(&ec_id)
            .expect("should read store")
            .expect("row should exist");
        assert_eq!(
            stored.ids.get("sharedid.org").map(|id| id.uid.as_str()),
            Some("shared-cookie-id"),
            "the sharedId update must still be ingested from the shared snapshot"
        );
        assert_eq!(
            stored.ids.get("id5-sync.com").map(|id| id.uid.as_str()),
            Some("body-id5")
        );
        assert!(!stored.ids.contains_key("unknown.example"));
        let serialized = serde_json::to_value(&stored).expect("should serialize persisted row");
        assert!(serialized["ids"]["id5-sync.com"].get("inserter").is_none());
        assert!(!serialized.to_string().contains("forged"));
    }

    #[tokio::test]
    async fn body_persistence_on_no_bid_never_creates_or_recovers_a_root() {
        for case in [
            "live",
            "no-token",
            "denied",
            "missing",
            "failed",
            "tombstone",
            "cookie-only",
        ] {
            let settings = create_test_settings();
            let orchestrator = AuctionOrchestrator::new(AuctionConfig {
                enabled: false,
                ..Default::default()
            });
            let registry = PartnerRegistry::from_config(&[counting_test_partner("id5-sync.com")])
                .expect("should build registry");
            let ec_id = format!("{}.ABC123", "a".repeat(64));
            let lookups = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let writes = Arc::new(Mutex::new(Vec::new()));
            let graph = KvIdentityGraph::new(BrowserRecordingKv {
                inner: InMemoryEcKv::new("browser-test"),
                lookups: Arc::clone(&lookups),
                writes: Arc::clone(&writes),
            });
            if !matches!(case, "missing" | "failed") {
                let mut entry = crate::ec::kv_types::KvEntry::tombstone(1_000);
                entry.consent.ok = case != "tombstone";
                graph.create(&ec_id, &entry).expect("should seed row");
            }
            let mut context = make_ec_context(
                Jurisdiction::NonRegulated,
                (case != "no-token").then_some(ec_id.as_str()),
            );
            if case == "denied" {
                context.consent_mut().jurisdiction = Jurisdiction::Unknown;
            }
            if case == "failed" {
                context.set_kv_snapshot(EcKvSnapshot::Failed {
                    ec_id: ec_id.clone(),
                });
            } else {
                context.set_kv_snapshot(graph.load_snapshot(&ec_id));
            }
            lookups.store(0, std::sync::atomic::Ordering::Relaxed);
            writes.lock().expect("should lock writes").clear();
            let payload = json!({"adUnits":[{"code":"slot", "mediaTypes":{"banner":{"sizes":[[300,250]]}}}],
            "eids": if case == "cookie-only" { JsonValue::Null } else {
                json!([{"source":"id5-sync.com", "uids":[{"id":"body-id"}]}])
            }});
            let cookie = BASE64.encode(
                serde_json::to_vec(&json!([
                    {"source":"id5-sync.com", "uids":[{"id":"retired-cookie-id"}]}
                ]))
                .expect("should encode legacy cookie"),
            );
            let request = Request::builder()
                .method("POST")
                .uri("https://publisher.example/auction")
                .header("cookie", format!("ts-eids={cookie}"))
                .body(EdgeBody::from(
                    serde_json::to_vec(&payload).expect("should serialize body"),
                ))
                .expect("should build request");
            let mut response = handle_auction(
                &settings,
                &orchestrator,
                Some(&graph),
                Some(&registry),
                &mut context,
                &noop_services(),
                request,
            )
            .await
            .expect("should return no bid");
            crate::ec::finalize::ec_finalize_response(
                &settings,
                &mut context,
                Some(&graph),
                &registry,
                None,
                &mut response,
            );
            assert_eq!(
                writes.lock().expect("should lock writes").len(),
                usize::from(case == "live"),
                "{case}"
            );
            // A Missing point-read snapshot is revalidated once by the
            // existing browser CAS path. Present generations and Failed
            // snapshots are reused; a miss never authorizes root creation.
            assert_eq!(
                lookups.load(std::sync::atomic::Ordering::Relaxed),
                usize::from(case == "missing"),
                "{case}: reuse usable snapshot, with one bounded miss revalidation"
            );
            assert!(
                !context.ec_generated(),
                "{case}: auction must not create or recover roots"
            );
            let stored = graph.get(&ec_id).expect("should inspect row");
            if case == "live" {
                assert_eq!(
                    stored.expect("live root remains").0.ids["id5-sync.com"].uid,
                    "body-id"
                );
            } else if let Some((entry, _)) = stored {
                assert!(entry.ids.is_empty(), "{case}: no browser enrichment");
            } else {
                assert!(matches!(case, "missing" | "failed"));
            }
            assert!(response.headers().get("x-ts-eids").is_none());
            assert!(response.headers().get("x-ts-eids-truncated").is_none());
        }
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

    /// Records bidder-facing requests, then fails without a transport handle.
    struct EidCapturingProvider {
        requests: Arc<Mutex<Vec<AuctionRequest>>>,
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
            self.requests
                .lock()
                .expect("should lock captured requests")
                .push(request.clone());
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
        let requests = Arc::new(Mutex::new(Vec::new()));
        orchestrator.register_provider(Arc::new(EidCapturingProvider {
            requests: Arc::clone(&requests),
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

        let captured = requests.lock().expect("should lock captured requests");
        let request = captured
            .last()
            .expect("should capture the bidder-facing request");
        assert!(
            request.user.eids.is_none(),
            "should carry no EIDs when EC identity is denied"
        );
    }

    #[tokio::test]
    async fn auction_body_and_cookie_cannot_resubmit_expired_identity() {
        for (client_uids, from_cookie, expected_uids) in [
            (vec!["expired-uid"], false, vec![]),
            (vec!["expired-uid"], true, vec![]),
            (vec!["different-uid"], false, vec!["different-uid"]),
            (
                vec!["expired-uid", "different-uid"],
                false,
                vec!["different-uid"],
            ),
        ] {
            let settings = create_test_settings();
            let mut orchestrator = AuctionOrchestrator::new(AuctionConfig {
                enabled: true,
                providers: AuctionConfig::legacy_provider_map(&["eid_capturing_provider"]),
                ..Default::default()
            });
            let requests = Arc::new(Mutex::new(Vec::new()));
            orchestrator.register_provider(Arc::new(EidCapturingProvider {
                requests: Arc::clone(&requests),
            }));
            let registry =
                PartnerRegistry::from_config(&[counting_test_partner("ids.example.com")])
                    .expect("should build registry");
            let graph = KvIdentityGraph::in_memory("auction-store");
            let ec_id = format!("{}.ABC123", "a".repeat(64));
            let mut entry =
                crate::ec::kv_types::KvEntry::minimal("ids.example.com", "expired-uid", 1_000);
            entry
                .ids
                .get_mut("ids.example.com")
                .expect("should contain record")
                .expires_at = Some(1);
            graph.create(&ec_id, &entry).expect("should seed identity");
            let (_, generation_before) = graph
                .get(&ec_id)
                .expect("should read identity")
                .expect("should find identity");
            let mut ec_context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));
            let eids = json!([{
                "source": "IDS.EXAMPLE.COM.",
                "uids": client_uids.iter().map(|uid| json!({"id": uid})).collect::<Vec<_>>()
            }]);
            let mut body = json!({ "adUnits": [{
                "code": "example-slot",
                "mediaTypes": { "banner": { "sizes": [[300, 250]] } }
            }] });
            let mut builder = Request::builder()
                .method("POST")
                .uri("https://publisher.example.com/auction");
            if from_cookie {
                let cookie =
                    BASE64.encode(serde_json::to_vec(&eids).expect("should encode cookie EIDs"));
                builder = builder.header("cookie", format!("{COOKIE_TS_EIDS}={cookie}"));
            } else {
                body["eids"] = eids;
            }
            let request = builder
                .body(EdgeBody::from(
                    serde_json::to_vec(&body).expect("should serialize body"),
                ))
                .expect("should build auction request");

            let _ = handle_auction(
                &settings,
                &orchestrator,
                Some(&graph),
                Some(&registry),
                &mut ec_context,
                &noop_services(),
                request,
            )
            .await;

            let captured = requests.lock().expect("should lock captured requests");
            let sent = captured
                .last()
                .expect("should capture the bidder-facing request");
            let actual_uids: Vec<&str> = sent
                .user
                .eids
                .iter()
                .flatten()
                .flat_map(|eid| &eid.uids)
                .map(|uid| uid.id.as_str())
                .collect();
            assert_eq!(
                actual_uids, expected_uids,
                "should remove the matching expired UID but preserve different current-request IDs"
            );
            let (retained, generation_after) = graph
                .get(&ec_id)
                .expect("should read retained identity")
                .expect("should retain identity");
            assert_eq!(
                retained, entry,
                "should not replace or delete retained expired records"
            );
            assert_eq!(
                generation_before, generation_after,
                "should not write on auction resolution"
            );
        }
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
            Some(1_000),
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

        let result = resolve_auction_eids(&EcKvSnapshot::NotRead, None, &ec_context, Some(1_000));
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
            Some(1_000),
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
            Some(1_000),
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
        let result = resolve_auction_eids(&snapshot, Some(&registry), &ec_context, Some(1_000));
        let eids = result.expect("should return Some on KV error (degraded mode)");
        assert!(
            eids.is_empty(),
            "should return empty vec on KV error (degraded mode)"
        );
    }

    #[test]
    fn expiry_matching_uses_normalized_sources_and_full_ec_identity() {
        let ec_id = format!("{}.ABC123", "a".repeat(64));
        let mut entry =
            crate::ec::kv_types::KvEntry::minimal("ids.example.com", "expired-uid", 1_000);
        entry
            .ids
            .get_mut("ids.example.com")
            .expect("should contain record")
            .expires_at = Some(1);
        let snapshot = EcKvSnapshot::Present {
            ec_id: ec_id.clone(),
            entry: Box::new(entry),
            generation: Some(1),
        };
        let context = make_ec_context(Jurisdiction::NonRegulated, Some(&ec_id));
        for source in ["ids.example.com", "IDS.EXAMPLE.COM.", " ids.example.com "] {
            let client = parse_client_auction_eids(Some(
                &json!([{"source": source, "uids": [{"id": "expired-uid"}]}]),
            ));
            assert!(
                resolve_and_merge_auction_eids(client, &snapshot, None, &context).is_none(),
                "should not bypass expiry with a source alias"
            );
        }
        let other_ec_id = format!("{}.XYZ789", "a".repeat(64));
        let other_context = make_ec_context(Jurisdiction::NonRegulated, Some(&other_ec_id));
        let client = parse_client_auction_eids(Some(
            &json!([{"source": "ids.example.com", "uids": [{"id": "expired-uid"}]}]),
        ));
        let resolved = resolve_and_merge_auction_eids(client, &snapshot, None, &other_context)
            .expect("should preserve unrelated current-request input");
        assert_eq!(
            resolved[0].uids[0].id, "expired-uid",
            "should not apply another full EC ID's lifecycle metadata"
        );
    }

    #[test]
    fn body_provenance_survives_parse_merge_and_serialization() {
        let raw = json!([{"source":"id5-sync.com", "inserter":"Lockr-For-Publishers",
            "matcher":"", "mm":0, "uids":[{"id":"uid", "ext":{"linkType":1}}]}]);
        let client = parse_client_auction_eids(Some(&raw));
        let resolved = Some(vec![Eid {
            source: "id5-sync.com".to_owned(),
            uids: vec![Uid {
                id: "uid".to_owned(),
                atype: Some(1),
                ext: None,
            }],
            ..Eid::default()
        }]);
        let merged = merge_auction_eids(client, resolved).expect("should merge body and KV");
        let output = serde_json::to_value(&merged).expect("should serialize");
        assert_eq!(output[0]["inserter"], "Lockr-For-Publishers");
        assert_eq!(output[0]["matcher"], "");
        assert_eq!(output[0]["mm"], 0);
        assert_eq!(output[0]["uids"][0]["atype"], 1);
        assert_eq!(output[0]["uids"][0]["ext"]["linkType"], 1);
        assert!(parse_client_auction_eids(None).is_none());
    }

    #[test]
    fn parse_client_auction_eids_ignores_malformed_entries() {
        let raw = json!([
            {
                "source": "id5-sync.com",
                "uids": [{ "id": "ID5_abc", "atype": 1 }]
            },
            {
                "source": "broken.example",
                "uids": "not-an-array"
            },
            {
                "source": "sharedid.org",
                "uids": [{ "id": "shared_123" }, { "id": "" }]
            }
        ]);

        let parsed = parse_client_auction_eids(Some(&raw)).expect("should parse valid EIDs");

        assert_eq!(parsed.len(), 2, "should keep only valid EID entries");
        assert_eq!(parsed[0].source, "id5-sync.com");
        assert_eq!(parsed[0].uids.len(), 1, "should keep valid UID");
        assert_eq!(parsed[1].source, "sharedid.org");
        assert_eq!(parsed[1].uids.len(), 1, "should drop empty UID values");
    }

    #[test]
    fn parse_client_auction_eids_caps_sources_and_uids() {
        let entries: Vec<_> = (0..(MAX_CLIENT_EID_SOURCES + 5))
            .map(|source_index| {
                let uids: Vec<_> = (0..(MAX_CLIENT_UIDS_PER_SOURCE + 5))
                    .map(|uid_index| json!({ "id": format!("uid-{source_index}-{uid_index}") }))
                    .collect();
                json!({
                    "source": format!("source-{source_index}.example.com"),
                    "uids": uids,
                })
            })
            .collect();
        let raw = JsonValue::Array(entries);

        let parsed = parse_client_auction_eids(Some(&raw)).expect("should parse capped EIDs");

        assert_eq!(
            parsed.len(),
            MAX_CLIENT_EID_SOURCES,
            "should cap client EID sources"
        );
        assert!(
            parsed
                .iter()
                .all(|eid| eid.uids.len() == MAX_CLIENT_UIDS_PER_SOURCE),
            "should cap UIDs per source"
        );
    }

    #[test]
    fn parse_client_auction_eids_drops_whitespace_and_oversized_uids() {
        let raw = json!([
            {
                "source": "id5-sync.com",
                "uids": [
                    { "id": "   " },
                    { "id": "x".repeat(MAX_UID_LENGTH + 1) },
                    { "id": "valid" }
                ]
            }
        ]);

        let parsed = parse_client_auction_eids(Some(&raw)).expect("should parse valid UID");

        assert_eq!(parsed.len(), 1, "should retain source with valid UID");
        assert_eq!(parsed[0].uids.len(), 1, "should drop invalid UIDs");
        assert_eq!(parsed[0].uids[0].id, "valid", "should keep valid UID");
    }

    #[test]
    fn parse_client_auction_eids_preserves_pair_atype() {
        let raw = json!([
            {
                "source": "google.com",
                "uids": [{ "id": "pair-id", "atype": 571187 }]
            }
        ]);

        let parsed = parse_client_auction_eids(Some(&raw)).expect("should parse PAIR EID");

        assert_eq!(
            parsed[0].uids[0].atype,
            Some(571187),
            "should preserve PAIR's vendor-specific atype"
        );
    }

    #[test]
    fn parse_client_auction_eids_preserves_uid_ext_and_sanitizes_invalid_atype() {
        let raw = json!([
            {
                "source": "adserver.org",
                "uids": [
                    {
                        "id": "uid-with-ext",
                        "atype": 1,
                        "ext": { "provider": "liveintent.com", "rtiPartner": "TDID" }
                    },
                    {
                        "id": "uid-bad-atype",
                        "atype": 2_147_483_648_u64,
                        "ext": { "keep": true }
                    },
                    {
                        "id": "uid-float-atype",
                        "atype": 1.5
                    }
                ]
            }
        ]);

        let parsed = parse_client_auction_eids(Some(&raw)).expect("should parse valid EIDs");

        assert_eq!(parsed.len(), 1, "should keep valid source");
        assert_eq!(parsed[0].uids.len(), 3, "should keep valid UIDs");
        assert_eq!(
            parsed[0].uids[0].atype,
            Some(1),
            "should preserve valid atype"
        );
        assert_eq!(
            parsed[0].uids[0].ext,
            Some(json!({ "provider": "liveintent.com", "rtiPartner": "TDID" })),
            "should preserve uid ext"
        );
        assert_eq!(
            parsed[0].uids[1].atype, None,
            "should drop out-of-range atype without dropping uid"
        );
        assert_eq!(
            parsed[0].uids[1].ext,
            Some(json!({ "keep": true })),
            "should preserve ext when atype is invalid"
        );
        assert_eq!(
            parsed[0].uids[2].atype, None,
            "should drop non-integer atype without dropping uid"
        );
    }

    #[test]
    fn merge_auction_eids_deduplicates_client_and_resolved_ids() {
        let client_eids = Some(vec![Eid {
            inserter: None,
            matcher: None,
            mm: None,
            source: "id5-sync.com".to_string(),
            uids: vec![Uid {
                id: "ID5_abc".to_string(),
                atype: Some(1),
                ext: None,
            }],
        }]);
        let resolved_eids = Some(vec![
            Eid {
                inserter: None,
                matcher: None,
                mm: None,
                source: "id5-sync.com".to_string(),
                uids: vec![Uid {
                    id: "ID5_abc".to_string(),
                    atype: Some(1),
                    ext: None,
                }],
            },
            Eid {
                inserter: None,
                matcher: None,
                mm: None,
                source: "liveramp.com".to_string(),
                uids: vec![Uid {
                    id: "LR_xyz".to_string(),
                    atype: Some(3),
                    ext: None,
                }],
            },
        ]);

        let merged = merge_auction_eids(client_eids, resolved_eids).expect("should merge EIDs");

        assert_eq!(merged.len(), 2, "should retain distinct EID sources");
        assert_eq!(merged[0].source, "id5-sync.com");
        assert_eq!(merged[0].uids.len(), 1, "should deduplicate matching UIDs");
        assert_eq!(merged[1].source, "liveramp.com");
        assert_eq!(merged[1].uids[0].id, "LR_xyz");
    }

    #[test]
    fn merge_auction_eids_preserves_multiple_uids_per_source() {
        let client_eids = Some(vec![Eid {
            inserter: None,
            matcher: None,
            mm: None,
            source: "sharedid.org".to_string(),
            uids: vec![Uid {
                id: "shared_client".to_string(),
                atype: None,
                ext: None,
            }],
        }]);
        let resolved_eids = Some(vec![Eid {
            inserter: None,
            matcher: None,
            mm: None,
            source: "sharedid.org".to_string(),
            uids: vec![Uid {
                id: "shared_server".to_string(),
                atype: Some(3),
                ext: None,
            }],
        }]);

        let merged = merge_auction_eids(client_eids, resolved_eids).expect("should merge EIDs");

        assert_eq!(merged.len(), 1, "should merge same-source entries");
        assert_eq!(merged[0].uids.len(), 2, "should preserve distinct UIDs");
        assert_eq!(merged[0].uids[0].id, "shared_server");
        assert_eq!(merged[0].uids[1].id, "shared_client");
    }

    #[test]
    fn merge_auction_eids_prefers_server_resolved_metadata_on_conflict() {
        let client_eids = Some(vec![Eid {
            inserter: None,
            matcher: None,
            mm: None,
            source: "adserver.org".to_string(),
            uids: vec![Uid {
                id: "shared_uid".to_string(),
                atype: Some(1),
                ext: Some(json!({ "provider": "client" })),
            }],
        }]);
        let resolved_eids = Some(vec![Eid {
            inserter: None,
            matcher: None,
            mm: None,
            source: "adserver.org".to_string(),
            uids: vec![Uid {
                id: "shared_uid".to_string(),
                atype: Some(3),
                ext: Some(json!({ "provider": "server" })),
            }],
        }]);

        let merged = merge_auction_eids(client_eids, resolved_eids).expect("should merge EIDs");

        assert_eq!(merged.len(), 1, "should merge duplicate source");
        assert_eq!(merged[0].uids.len(), 1, "should deduplicate duplicate uid");
        assert_eq!(
            merged[0].uids[0].atype,
            Some(3),
            "should prefer resolved atype"
        );
        assert_eq!(
            merged[0].uids[0].ext,
            Some(json!({ "provider": "server" })),
            "should prefer resolved ext"
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
}
