//! Shared provider and pull-sync request orchestration.

use std::net::IpAddr;
use std::sync::Arc;

use edgezero_core::http::{Request, header};
use edgezero_core::outbound::HttpClient;

use crate::consent::ConsentContext;
use crate::ec::after_response::{after_response_lookups_allowed, commit_after_response_lookups};
use crate::ec::coordinator::transport::{InFlightLookups, OutboundLookupTransport};
use crate::ec::coordinator::{
    IdentityCoordinator as _, LookupContext, PendingLookups, ProviderCoordinator,
};
use crate::ec::eid_types::UnixSeconds;
use crate::ec::kv::KvIdentityGraph;
use crate::ec::kv_types::KvEntry;
use crate::ec::provider::{ClientSignals, RequestCookies};
use crate::ec::{EcContext, EcKvSnapshot, current_timestamp};
use crate::platform::RuntimeServices;

use super::IdentityApp;

/// Identity lookups dispatched for this navigation, collected before finalize.
pub struct IdentityLookups {
    /// Coordinator owning these provider lookups.
    coordinator: Arc<ProviderCoordinator>,
    /// Request-scoped outbound transport used to collect the batch.
    transport: OutboundLookupTransport,
    /// Dispatched batch awaiting collection.
    pending: PendingLookups<InFlightLookups>,
}

/// What a navigation's after-response identity lookups need once the response
/// has been sent.
///
/// The router owns the request, so the request signals the providers read are
/// copied here. The lookups are dispatched only after the response, so they
/// never delay the page.
#[derive(Clone)]
pub struct AfterResponseLookups {
    /// Coordinator owning these provider lookups.
    pub coordinator: Arc<ProviderCoordinator>,
    /// Identity this work was prepared for.
    pub ec_id: String,
    /// Trusted raw client address captured from platform services.
    pub client_ip: Option<IpAddr>,
    /// Request user agent supplied to providers.
    pub user_agent: Option<String>,
    /// Request `Referer` supplied to providers.
    pub referer: Option<String>,
    /// Request `Accept-Language` supplied to providers.
    pub accept_language: Option<String>,
    /// Joined request cookies, including provider opt-outs.
    pub cookie_header: String,
}

/// Request signals lent to identity providers, copied out of the request so
/// lookups can run while the router hands the request to the origin fetch.
struct ProviderRequestSignals {
    client_ip: Option<IpAddr>,
    user_agent: Option<String>,
    referer: Option<String>,
    accept_language: Option<String>,
    cookie_header: String,
}

impl ProviderRequestSignals {
    fn read(req: &Request, services: &RuntimeServices) -> Self {
        Self {
            // The trusted raw address: `EcContext::client_ip` is the
            // normalized EC-hash input, which drops IPv6 separators.
            client_ip: services.client_info().client_ip,
            user_agent: header_string(req, &header::USER_AGENT),
            referer: header_string(req, &header::REFERER),
            accept_language: header_string(req, &header::ACCEPT_LANGUAGE),
            cookie_header: joined_cookie_header(req),
        }
    }

    fn client(&self) -> ClientSignals<'_> {
        ClientSignals {
            ip: self.client_ip,
            user_agent: self.user_agent.as_deref(),
            referer: self.referer.as_deref(),
            accept_language: self.accept_language.as_deref(),
        }
    }
}

/// The first `name` header on the request, when it is visible ASCII.
fn header_string(req: &Request, name: &header::HeaderName) -> Option<String> {
    req.headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// What a navigation's page-phase identity lookups read, copied out of the
/// request and its EC context by [`IdentityApp::prepare_identity_lookups`].
///
/// The lookups, and the hashed-email status call before them (spec §8.7),
/// can then run beside the origin fetch, which takes the request and borrows
/// the EC context mutably, so this owns its inputs.
///
/// # Performance
///
/// Clones the consent context and the stored and pending rows once per
/// navigation that dispatches providers.
pub struct PageIdentityDispatch {
    coordinator: Arc<ProviderCoordinator>,
    ec_id: String,
    snapshot: EcKvSnapshot,
    minted_entry: Option<KvEntry>,
    consent: ConsentContext,
    signals: ProviderRequestSignals,
}

impl PageIdentityDispatch {
    /// Starts the lookups, after the status call when a provider needs it,
    /// counting them against `identity`'s rate limiter.
    pub async fn start(
        self,
        identity: &IdentityApp,
        services: &RuntimeServices,
    ) -> IdentityLookups {
        // The request's own client: the batch it starts is bound to the serving
        // callback, so it stays in flight until the handler collects it
        // before finalization.
        let transport = OutboundLookupTransport::for_services(services);
        let now = current_timestamp();
        let pending = self
            .coordinator
            .dispatch(
                &transport,
                &LookupContext {
                    ec_id: &self.ec_id,
                    snapshot: &self.snapshot,
                    minted_entry: self.minted_entry.as_ref(),
                    consent: &self.consent,
                    client: self.signals.client(),
                    publisher_domain: &identity.settings.publisher.domain,
                    cookies: RequestCookies::new(&self.signals.cookie_header),
                    rate_limiter: identity.rate_limiter(),
                    now: UnixSeconds::new(now),
                },
            )
            .await;
        IdentityLookups {
            coordinator: self.coordinator,
            transport,
            pending,
        }
    }
}

/// Joins every `Cookie` header on the request into one `; `-separated value.
///
/// HTTP/2 and HTTP/3 clients may split cookies across several headers.
pub fn joined_cookie_header(req: &Request) -> String {
    req.headers()
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect::<Vec<_>>()
        .join("; ")
}

impl IdentityApp {
    /// Captures the request signals for after-response identity lookups on a
    /// permitted publisher navigation.
    ///
    /// Returns `None` when no after-response provider (including a pull-sync
    /// partner) is enabled, the client failed the browser check, the request has
    /// no EC, or the identity graph is unavailable.
    pub fn prepare_after_response_lookups(
        &self,
        services: &RuntimeServices,
        req: &Request,
        context: &EcContext,
        graph: Option<&KvIdentityGraph>,
        allowed: bool,
    ) -> Option<AfterResponseLookups> {
        let coordinator = Arc::clone(self.after_response_identity()?);
        // Bot gate: pull sync and other after-response lookups disclose the EC ID
        // to partners, so they run only for recognized browsers.
        if !allowed {
            return None;
        }
        graph?;
        let ec_id = context.ec_value()?.to_owned();
        let signals = ProviderRequestSignals::read(req, services);
        Some(AfterResponseLookups {
            coordinator,
            ec_id,
            client_ip: signals.client_ip,
            user_agent: signals.user_agent,
            referer: signals.referer,
            accept_language: signals.accept_language,
            cookie_header: signals.cookie_header,
        })
    }

    /// Copies the inputs of a permitted publisher navigation's page lookups,
    /// reading the stored row first when the request has not read it.
    ///
    /// Returns `None` when no provider is enabled, the request has no EC, or the
    /// identity graph is unavailable. For an EC minted on this request only
    /// providers that do not send the EC ID are dispatched, against the row this
    /// navigation will create. [`PageIdentityDispatch::start`] starts them.
    pub async fn prepare_identity_lookups(
        &self,
        services: &RuntimeServices,
        req: &Request,
        context: &mut EcContext,
        graph: Option<&KvIdentityGraph>,
    ) -> Option<PageIdentityDispatch> {
        let coordinator = Arc::clone(self.page_identity()?);
        let graph = graph?;
        let ec_id = context.ec_value()?.to_owned();
        if matches!(context.kv_snapshot(), EcKvSnapshot::NotRead) {
            let snapshot = graph.load_snapshot(&ec_id).await;
            context.set_kv_snapshot(snapshot);
        }
        Some(PageIdentityDispatch {
            coordinator,
            ec_id,
            snapshot: context.kv_snapshot().clone(),
            minted_entry: context.pending_entry().cloned(),
            consent: context.consent().clone(),
            signals: ProviderRequestSignals::read(req, services),
        })
    }

    /// Starts identity-provider lookups for a permitted publisher navigation.
    ///
    /// Prepares them with [`Self::prepare_identity_lookups`] and starts them at
    /// once, so a hashed-email status call a provider needs is awaited here.
    /// Returns `None` in the same cases.
    pub async fn dispatch_identity_lookups(
        &self,
        services: &RuntimeServices,
        req: &Request,
        context: &mut EcContext,
        graph: Option<&KvIdentityGraph>,
    ) -> Option<IdentityLookups> {
        let dispatch = self
            .prepare_identity_lookups(services, req, context, graph)
            .await?;
        Some(dispatch.start(self, services).await)
    }

    /// Records on the EC context the sources whose identity providers' own
    /// cookies on this request record an opt-out.
    ///
    /// Runs on every route before any identity egress, so the publisher auction,
    /// page-bids, `/auction` and identify withhold those sources on the request
    /// the opt-out cookie arrives on. Only cookies are read: no lookup is made.
    pub fn apply_cookie_opt_outs(&self, req: &Request, context: &mut EcContext) {
        let coordinators = [self.page_identity(), self.after_response_identity()];
        if coordinators.iter().all(Option::is_none) {
            return;
        }
        let cookie_header = joined_cookie_header(req);
        let cookies = RequestCookies::new(&cookie_header)
            .at(UnixSeconds::new(current_timestamp()))
            .where_gdpr_applies(context.consent().gdpr_applies);
        let sources: Vec<String> = coordinators
            .into_iter()
            .flatten()
            .flat_map(|coordinator| coordinator.cookie_opted_out_sources(cookies))
            .map(|source| source.as_str().to_owned())
            .collect();
        if !sources.is_empty() {
            log::debug!("Identity egress: withholding sources opted out in cookies: {sources:?}");
        }
        context.set_cookie_opted_out_sources(sources);
    }

    /// Collects dispatched page lookups into this request's identity context.
    pub async fn collect_identity_lookups(
        &self,
        lookups: IdentityLookups,
        context: &mut EcContext,
    ) {
        let results = lookups
            .coordinator
            .collect(&lookups.transport, lookups.pending)
            .await;
        context.set_lookup_results(results);
    }
    /// Runs after-response identity lookups and applies their follow-up write.
    ///
    /// Runs once the response has been delivered, so provider latency never
    /// delays the page. Skipped unless finalize confirmed the navigation (it
    /// issues the follow-up permit) and the EC is still the one the lookups were
    /// prepared for. Lookups go out on the post-delivery `client`.
    pub async fn run_after_response_lookups(
        &self,
        context: &mut EcContext,
        lookups: Option<AfterResponseLookups>,
        client: &HttpClient,
    ) {
        let Some(lookups) = lookups else {
            return;
        };
        let Some(permit) = context.take_after_response_permit() else {
            log::debug!(
                "After-response lookups skipped: no permit (consent, redirect or failed page-load write)"
            );
            return;
        };
        if context.ec_value() != Some(lookups.ec_id.as_str()) {
            log::debug!("After-response lookups skipped: EC changed during the request");
            return;
        }
        // The request snapshot holds the row as finalize left it. A source held
        // there is still held in the live row unless consent was withdrawn, which
        // blocks the lookup anyway, so the live read is skipped when nothing is due.
        // Only pull sync can be skipped: configured providers always run, so their
        // cookie opt-outs and regional exclusions are processed.
        let now = UnixSeconds::new(current_timestamp());
        let request_entry = context.kv_snapshot().entry_for(&lookups.ec_id);
        if !lookups.coordinator.any_due(request_entry, now) {
            log::debug!("After-response lookups skipped: nothing due");
            return;
        }
        let Some(graph) = self.identity_graph() else {
            return;
        };
        let snapshot = graph.load_snapshot(&lookups.ec_id).await;
        if !after_response_lookups_allowed(&snapshot, &lookups.ec_id, context.ec_generated()) {
            log::debug!("After-response lookups skipped: new EC or no live row");
            return;
        }
        let transport = OutboundLookupTransport::new(client);
        let pending = lookups
            .coordinator
            .dispatch(
                &transport,
                &LookupContext {
                    ec_id: &lookups.ec_id,
                    snapshot: &snapshot,
                    minted_entry: None,
                    consent: context.consent(),
                    client: ClientSignals {
                        ip: lookups.client_ip,
                        user_agent: lookups.user_agent.as_deref(),
                        referer: lookups.referer.as_deref(),
                        accept_language: lookups.accept_language.as_deref(),
                    },
                    publisher_domain: &self.settings.publisher.domain,
                    cookies: RequestCookies::new(&lookups.cookie_header),
                    rate_limiter: self.rate_limiter(),
                    now,
                },
            )
            .await;
        if pending.is_empty() {
            return;
        }
        let results = lookups.coordinator.collect(&transport, pending).await;
        let outcome = commit_after_response_lookups(
            &graph,
            permit,
            self.partners(),
            &lookups.ec_id,
            &results,
        )
        .await;
        log::info!("After-response lookups: write outcome={outcome}");
    }
}

#[cfg(test)]
mod tests {
    use edgezero_core::Body;
    use edgezero_core::http::{HeaderValue, header, request_builder};

    #[test]
    fn provider_signals_preserve_split_cookie_headers() {
        let mut request = request_builder()
            .uri("/article")
            .body(Body::empty())
            .expect("should build the request");
        request
            .headers_mut()
            .append(header::COOKIE, HeaderValue::from_static("a=1"));
        request
            .headers_mut()
            .append(header::COOKIE, HeaderValue::from_static("vendor_id=c-1"));
        assert_eq!(
            super::joined_cookie_header(&request),
            "a=1; vendor_id=c-1",
            "should preserve provider cookies from every header"
        );
    }
}
