//! EC identity lifecycle for publisher navigations on the Axum dev server.
//!
//! Mirrors the Fastly adapter: the `ts-ec` cookie is read and minted, enabled
//! identity providers run on permitted navigations, the page-phase results
//! go into the single page-load write in EC finalization, and after-response
//! providers run once the response has been produced, with their own
//! follow-up compare-and-swap write.
//!
//! Differences from Fastly, all specific to the local dev server:
//!
//! - The identity graph is the process-local [`crate::ec_kv::LocalEcKvStore`].
//! - The dev server sees no TLS or HTTP/2 fingerprint, so the bot gate
//!   accepts any request whose `User-Agent` names a known platform.
//! - After-response lookups run on a blocking tokio task spawned when the
//!   handler returns, rather than after the response bytes are sent.
//! - Partner pull sync is not run.

use std::net::IpAddr;
use std::sync::Arc;

use edgezero_core::http::{Request, Response, header};
use trusted_server_core::constants::{COOKIE_SHAREDID, COOKIE_TS_EIDS};
use trusted_server_core::cookies::extract_cookie_value;
use trusted_server_core::ec::after_response::{
    after_response_lookups_allowed, commit_after_response_lookups,
};
use trusted_server_core::ec::consent::ec_consent_withdrawn;
use trusted_server_core::ec::coordinator::transport::PlatformLookupTransport;
use trusted_server_core::ec::coordinator::{
    IdentityCoordinator as _, LookupContext, PendingLookups, ProviderCoordinator,
};
use trusted_server_core::ec::device::DeviceSignals;
use trusted_server_core::ec::eid_types::UnixSeconds;
use trusted_server_core::ec::finalize::ec_finalize_response;
use trusted_server_core::ec::kv::KvIdentityGraph;
use trusted_server_core::ec::page_load::NavigationPermit;
use trusted_server_core::ec::provider::{ClientSignals, RequestCookies};
use trusted_server_core::ec::{EcContext, EcKvSnapshot, current_timestamp};
use trusted_server_core::platform::{PlatformPendingRequest, RuntimeServices};

use crate::app::AppState;

/// Per-request EC state for a publisher navigation.
pub(crate) struct EcRequestState {
    pub(crate) ec_context: EcContext,
    /// Graph for reads and the auction; `None` for a non-browser.
    pub(crate) kv_graph: Option<KvIdentityGraph>,
    /// Graph for the finalize write; kept for a consent withdrawal even from
    /// a non-browser so tombstones stay authoritative.
    finalize_kv_graph: Option<KvIdentityGraph>,
    eids_cookie: Option<String>,
    sharedid_cookie: Option<String>,
    pub(crate) is_real_browser: bool,
}

/// Identity lookups dispatched for a navigation, collected before finalize.
pub(crate) struct PageLookups {
    coordinator: Arc<ProviderCoordinator>,
    pending: PendingLookups<PlatformPendingRequest>,
}

/// Request signals captured for after-response lookups.
pub(crate) struct AfterResponseLookups {
    ec_id: String,
    client_ip: Option<IpAddr>,
    user_agent: Option<String>,
    cookie_header: String,
}

/// Joins every `Cookie` header on the request into one `; `-separated value.
fn joined_cookie_header(req: &Request) -> String {
    req.headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect::<Vec<_>>()
        .join("; ")
}

fn user_agent(req: &Request) -> Option<&str> {
    req.headers()
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
}

/// Device signals for a request; the dev server only has the `User-Agent`.
pub(crate) fn device_signals_for(req: &Request) -> DeviceSignals {
    DeviceSignals::derive(user_agent(req).unwrap_or(""), None, None)
}

/// Dev-server bot gate.
///
/// Fastly also requires a TLS fingerprint, which a plain-HTTP local server
/// never has, so only the `User-Agent` platform is checked here.
pub(crate) fn looks_like_browser(signals: &DeviceSignals) -> bool {
    signals.platform_class.is_some()
}

/// Builds the per-request EC state, mirroring the Fastly pre-routing prelude.
pub(crate) fn build_ec_request_state(
    state: &AppState,
    services: &RuntimeServices,
    req: &Request,
) -> EcRequestState {
    let device_signals = device_signals_for(req);
    let is_real_browser = looks_like_browser(&device_signals);
    if !is_real_browser {
        log::info!("Bot gate: blocking EC operations for a request without a known UA platform");
    }

    let eids_cookie = extract_cookie_value(req, COOKIE_TS_EIDS);
    let sharedid_cookie = extract_cookie_value(req, COOKIE_SHAREDID);

    let geo_info = services
        .geo()
        .lookup(services.client_info().client_ip)
        .unwrap_or_else(|e| {
            log::warn!("geo lookup failed during EC setup: {e}");
            None
        });
    let (ec_context, setup_ok) = match EcContext::read_from_request_with_geo(
        state.settings(),
        req,
        services,
        geo_info.as_ref(),
    ) {
        Ok(mut context) => {
            context.set_device_signals(device_signals);
            (context, true)
        }
        Err(report) => {
            // A malformed consent signal leaves a fail-closed default context
            // with no identity-graph access.
            log::warn!("EC context read failed: {report:?}");
            (EcContext::default(), false)
        }
    };

    let kv_graph = setup_ok.then(|| state.identity_graph()).flatten();
    let finalize_kv_graph = if is_real_browser || ec_consent_withdrawn(ec_context.consent()) {
        kv_graph.clone()
    } else {
        None
    };
    let kv_graph = if is_real_browser { kv_graph } else { None };

    EcRequestState {
        ec_context,
        kv_graph,
        finalize_kv_graph,
        eids_cookie,
        sharedid_cookie,
        is_real_browser,
    }
}

/// Starts page-phase identity lookups for a permitted publisher navigation.
///
/// Returns `None` when no page-phase provider is enabled, the identity graph
/// is unavailable. On the navigation that mints the EC, only providers that
/// do not send the EC ID run, against the row this navigation creates.
pub(crate) async fn dispatch_page_lookups(
    state: &AppState,
    services: &RuntimeServices,
    req: &Request,
    ec: &mut EcRequestState,
) -> Option<PageLookups> {
    let coordinator = Arc::clone(state.page_identity()?);
    let graph = ec.kv_graph.as_ref()?;
    let ec_id = ec.ec_context.ec_value()?.to_owned();
    if matches!(ec.ec_context.kv_snapshot(), EcKvSnapshot::NotRead) {
        let snapshot = graph.load_snapshot(&ec_id);
        ec.ec_context.set_kv_snapshot(snapshot);
    }
    let transport = PlatformLookupTransport::new(services);
    let cookie_header = joined_cookie_header(req);
    let pending = coordinator
        .dispatch(
            &transport,
            &LookupContext {
                ec_id: &ec_id,
                snapshot: ec.ec_context.kv_snapshot(),
                minted_entry: ec.ec_context.pending_entry(),
                consent: ec.ec_context.consent(),
                client: ClientSignals {
                    ip: services.client_info().client_ip,
                    user_agent: user_agent(req),
                },
                publisher_domain: &state.settings().publisher.domain,
                cookies: RequestCookies::new(&cookie_header),
                rate_limiter: state.rate_limiter(),
                now: UnixSeconds::new(current_timestamp()),
            },
        )
        .await;
    Some(PageLookups {
        coordinator,
        pending,
    })
}

/// Captures request signals for after-response lookups.
///
/// Returns `None` when no after-response provider is enabled, the request
/// has no EC, or the identity graph is unavailable.
pub(crate) fn prepare_after_response_lookups(
    state: &AppState,
    services: &RuntimeServices,
    req: &Request,
    ec: &EcRequestState,
) -> Option<AfterResponseLookups> {
    state.after_response_identity()?;
    ec.kv_graph.as_ref()?;
    let ec_id = ec.ec_context.ec_value()?.to_owned();
    Some(AfterResponseLookups {
        ec_id,
        client_ip: services.client_info().client_ip,
        user_agent: user_agent(req).map(str::to_owned),
        cookie_header: joined_cookie_header(req),
    })
}

/// Collects page-phase lookups and runs EC finalization on `response`.
///
/// Finalization performs at most one page-load write to the identity graph.
pub(crate) async fn finalize_navigation(
    state: &AppState,
    services: &RuntimeServices,
    ec: &mut EcRequestState,
    page_lookups: Option<PageLookups>,
    response: &mut Response,
) {
    // A redirect skips the write, so do not wait for lookups it would discard.
    if let Some(PageLookups {
        coordinator,
        pending,
    }) = page_lookups
    {
        if NavigationPermit::is_redirect(response.status()) {
            log::debug!("Identity lookups dropped: redirect response skips the write");
        } else {
            let transport = PlatformLookupTransport::new(services);
            let results = coordinator.collect(&transport, pending).await;
            ec.ec_context.set_lookup_results(results);
        }
    }
    ec_finalize_response(
        state.settings(),
        &mut ec.ec_context,
        ec.finalize_kv_graph.as_ref(),
        state.partner_registry(),
        ec.eids_cookie.as_deref(),
        ec.sharedid_cookie.as_deref(),
        response,
    );
}

/// Spawns after-response lookups once the navigation response is ready.
///
/// Runs only when finalization issued the follow-up permit. The lookups run
/// on a blocking tokio task so they never delay the response; returns `None`
/// when nothing was spawned.
pub(crate) fn spawn_after_response_lookups(
    state: Arc<AppState>,
    services: RuntimeServices,
    ec: &mut EcRequestState,
    lookups: Option<AfterResponseLookups>,
) -> Option<tokio::task::JoinHandle<()>> {
    let lookups = lookups?;
    let Some(permit) = ec.ec_context.take_after_response_permit() else {
        log::debug!(
            "After-response lookups skipped: no permit (consent, redirect or failed page-load write)"
        );
        return None;
    };
    if ec.ec_context.ec_value() != Some(lookups.ec_id.as_str()) {
        log::debug!("After-response lookups skipped: EC changed during the request");
        return None;
    }
    let ec_generated = ec.ec_context.ec_generated();
    let consent = ec.ec_context.consent().clone();
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        log::warn!("After-response lookups skipped: no tokio runtime");
        return None;
    };
    Some(tokio::task::spawn_blocking(move || {
        runtime.block_on(async move {
            let Some(coordinator) = state.after_response_identity() else {
                return;
            };
            let Some(graph) = state.identity_graph() else {
                return;
            };
            let snapshot = graph.load_snapshot(&lookups.ec_id);
            if !after_response_lookups_allowed(&snapshot, &lookups.ec_id, ec_generated) {
                log::debug!("After-response lookups skipped: new EC or no live row");
                return;
            }
            let transport = PlatformLookupTransport::new(&services);
            let pending = coordinator
                .dispatch(
                    &transport,
                    &LookupContext {
                        ec_id: &lookups.ec_id,
                        snapshot: &snapshot,
                        minted_entry: None,
                        consent: &consent,
                        client: ClientSignals {
                            ip: lookups.client_ip,
                            user_agent: lookups.user_agent.as_deref(),
                        },
                        publisher_domain: &state.settings().publisher.domain,
                        cookies: RequestCookies::new(&lookups.cookie_header),
                        rate_limiter: state.rate_limiter(),
                        now: UnixSeconds::new(current_timestamp()),
                    },
                )
                .await;
            if pending.is_empty() {
                return;
            }
            let results = coordinator.collect(&transport, pending).await;
            let outcome = commit_after_response_lookups(
                &graph,
                permit,
                state.partner_registry(),
                &lookups.ec_id,
                &results,
            );
            log::info!("After-response lookups: write outcome={outcome}");
        });
    }))
}
