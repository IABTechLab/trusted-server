//! Trait definition for auction providers, and the provider core runs for
//! each demand source the plan selects.

use core::any::Any;
use std::collections::HashSet;

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt as _};
use http::{Method, Request};
use serde_json::json;

use crate::error::TrustedServerError;
use crate::platform::{
    PlatformHttpRequest, PlatformPendingRequest, PlatformResponse, RuntimeServices,
};
use crate::request_signing::{RequestSigner, SigningParams};

use super::demand::{CompiledDemand, DemandResponse, OutboundRequest, RequestFinalization};
use super::plan::{NotificationPolicy, ProviderPlan};
use super::routing::{ProviderAuctionInput, RoutedAuction};
use super::types::{AuctionContext, AuctionRequest, AuctionResponse, Bid};

/// Suppress notification URLs using exact returned-seat identity.
pub fn apply_notification_policy(bids: &mut [Bid], policy: &NotificationPolicy) {
    for bid in bids {
        let suppress = policy.suppress_all
            || bid
                .returned_seat
                .as_ref()
                .is_some_and(|seat| policy.suppress_seats.contains(seat));
        if suppress {
            bid.nurl = None;
            bid.burl = None;
        }
    }
}

/// Count bidder parameter objects a demand source did not consume.
#[must_use]
pub(crate) fn unused_bidder_params_count(
    demand: &dyn CompiledDemand,
    input: &ProviderAuctionInput,
) -> u32 {
    if demand.consumes_bidder_params() {
        return 0;
    }
    ignored_bidder_params_count(input)
}

/// Count routed bidder params for a demand source known to ignore them.
///
/// Public so an implementation that takes no bidder parameters reports the
/// ones it was routed the way core's own provider does.
#[must_use]
pub fn ignored_bidder_params_count(input: &ProviderAuctionInput) -> u32 {
    saturating_bidder_param_counts(input.slots().iter().map(|slot| slot.bidder_params().len()))
}

fn saturating_bidder_param_counts(counts: impl IntoIterator<Item = usize>) -> u32 {
    counts.into_iter().fold(0_u32, |count, slot_count| {
        count.saturating_add(u32::try_from(slot_count).unwrap_or(u32::MAX))
    })
}

fn attach_provider_routing_metadata(
    response: &mut AuctionResponse,
    demand: &dyn CompiledDemand,
    input: &ProviderAuctionInput,
) {
    response.metadata.insert(
        "routing".to_string(),
        json!({"unused_bidder_params_count": unused_bidder_params_count(demand, input)}),
    );
}

/// Provider-local state carried from request dispatch to response parsing.
pub type ProviderParseState = Box<dyn Any + Send + Sync>;

/// Result of asking a provider to start a bid request.
pub enum ProviderRequestOutcome {
    /// An upstream request is in flight and must be awaited by the orchestrator.
    Pending {
        /// Platform-specific pending request handle.
        request: PlatformPendingRequest,
        /// Optional provider-local state consumed when the response is parsed.
        parse_state: Option<ProviderParseState>,
    },
    /// A complete provider response that required no upstream request.
    Immediate(AuctionResponse),
}

impl ProviderRequestOutcome {
    /// Wrap an ordinary pending provider request without parse state.
    #[must_use]
    pub fn pending(request: PlatformPendingRequest) -> Self {
        Self::Pending {
            request,
            parse_state: None,
        }
    }

    /// Wrap a pending provider request with provider-local parse state.
    #[must_use]
    pub fn pending_with_state(
        request: PlatformPendingRequest,
        parse_state: ProviderParseState,
    ) -> Self {
        Self::Pending {
            request,
            parse_state: Some(parse_state),
        }
    }
}

/// Trait implemented by all auction providers (Prebid, APS, GAM, etc.).
#[async_trait(?Send)]
pub trait AuctionProvider: Send + Sync {
    /// Borrow this provider instance's unique validated identifier.
    ///
    /// Legacy providers may return a string literal; config-first providers
    /// return their owned operator-defined [`super::plan::ProviderId`].
    fn provider_name(&self) -> &str;

    /// Submit a bid request to this provider.
    ///
    /// Implementations normally return a pending upstream request, but may return
    /// an immediate response for a legitimate outcome that requires no HTTP call.
    /// The orchestrator handles waiting for and parsing pending responses.
    ///
    /// # Errors
    ///
    /// Returns an error if the request cannot be created or if the provider endpoint
    /// cannot be reached (though usually network errors happen while the returned
    /// [`PlatformPendingRequest`] is polled).
    async fn request_bids(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>>;

    /// Parse the response from the provider into an `AuctionResponse`.
    ///
    /// Called by the orchestrator after the [`PlatformPendingRequest`] completes.
    /// Declared async so implementations can safely drain streaming response bodies
    /// without panicking on the `Body::Stream` variant.
    ///
    /// # Errors
    ///
    /// Returns an error if the response cannot be parsed into a valid `AuctionResponse`.
    async fn parse_response(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>>;

    /// Parse the response with access to the original auction request and context.
    ///
    /// Providers that need request-local metadata while transforming responses
    /// can override this method. `request` is the [`AuctionRequest`] the
    /// orchestrator dispatched, so request-scoped data (e.g. slot ID mappings)
    /// can be derived here instead of stored on the shared provider instance.
    /// The default preserves the existing response-only provider contract.
    ///
    /// # Errors
    ///
    /// Returns an error if the response cannot be parsed into a valid [`AuctionResponse`].
    async fn parse_response_with_context(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let _ = (request, context);
        self.parse_response(response, response_time_ms).await
    }

    /// Parse a response with access to provider-local dispatch state.
    ///
    /// The default ignores `parse_state` and preserves the context-aware parser
    /// contract. Providers should downcast only state they created themselves.
    ///
    /// # Errors
    ///
    /// Returns an error if the response cannot be parsed into a valid [`AuctionResponse`].
    async fn parse_response_with_context_and_state(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
        parse_state: Option<&(dyn Any + Send + Sync)>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let _ = parse_state;
        self.parse_response_with_context(response, response_time_ms, request, context)
            .await
    }

    /// Check if this provider supports a specific media type.
    fn supports_media_type(&self, media_type: &super::types::MediaType) -> bool {
        // By default, support banner ads
        matches!(media_type, super::types::MediaType::Banner)
    }

    /// Get the configured timeout for this provider in milliseconds.
    fn timeout_ms(&self) -> u32;

    /// Check if this provider is enabled.
    fn is_enabled(&self) -> bool {
        true
    }

    /// Return the backend name used by this provider for request routing.
    ///
    /// `timeout_ms` is the effective timeout that will be used when the backend
    /// is registered in [`request_bids`](Self::request_bids).  It must be
    /// forwarded to [`crate::platform::PlatformBackend::predict_name`] through
    /// `services` so the predicted name matches the actual platform backend
    /// registration.
    fn backend_name(&self, _services: &RuntimeServices, _timeout_ms: u32) -> Option<String> {
        None
    }
}

/// The provider core runs for one demand source the plan selects.
///
/// It sends the request the source builds from its routed input, and hands
/// the answer back to the source to read. Every instance owns its validated
/// provider identity and carries only its own typed response state across
/// transport.
pub(crate) struct RoutedDemandProvider {
    plan: ProviderPlan,
}

/// State created by and returned to one [`RoutedDemandProvider`].
///
/// `captured` holds whatever the demand source kept from its own request,
/// and only that source reads it back.
pub(crate) struct RoutedDemandParseState {
    provider_id: String,
    input: ProviderAuctionInput,
    captured: Option<Box<dyn Any + Send + Sync>>,
}

impl RoutedDemandProvider {
    pub(crate) fn new(plan: ProviderPlan) -> Self {
        Self { plan }
    }

    pub(crate) fn provider_name(&self) -> &str {
        self.plan.id.as_str()
    }

    pub(crate) fn timeout_ms(&self) -> u32 {
        self.plan.timeout_ms
    }

    #[cfg(any(test, feature = "test-utils"))]
    pub(crate) fn parse_state_for_test(&self, input: ProviderAuctionInput) -> ProviderParseState {
        Box::new(RoutedDemandParseState {
            provider_id: self.provider_name().to_string(),
            input,
            captured: None,
        })
    }

    /// Build, register, and start exactly one routed provider request.
    ///
    /// The public [`AuctionProvider::request_bids`] seam cannot carry a
    /// [`ProviderAuctionInput`], the complete [`RoutedAuction`], or an
    /// auction-local [`RequestSigner`] without shared mutable provider state.
    /// The plan-backed split dispatcher therefore supplies that explicit
    /// execution context here, while this method reuses
    /// [`ProviderRequestOutcome`] and [`ProviderParseState`] for the existing
    /// request/parse token boundary.
    #[allow(
        clippy::too_many_arguments,
        reason = "the provider keeps routed inputs, both budgets, signer, services, and collision state explicit"
    )]
    pub(crate) async fn request_bids_routed(
        &self,
        input: &ProviderAuctionInput,
        routed: &RoutedAuction,
        logical_budget_ms: u32,
        transport_timeout_ms: u32,
        signer: Option<&RequestSigner>,
        services: &RuntimeServices,
        reserved_backend_names: &mut HashSet<String>,
    ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
        let signing_params = SigningParams::new(
            input.common_request().id.clone(),
            input.common_request().publisher.domain.clone(),
            "https".to_string(),
        );
        let demand = self.plan.demand.as_ref();
        let Some(outbound) = demand.build_request(
            input,
            routed.transport(),
            logical_budget_ms,
            &RequestFinalization {
                signer,
                signing_params,
            },
        )?
        else {
            return Ok(ProviderRequestOutcome::Immediate(
                AuctionResponse::no_bid(self.provider_name(), 0)
                    .with_metadata("routing", json!({"skipped_no_usable_demand": true})),
            ));
        };

        let spec = self
            .plan
            .backend_spec_with_transport_timeout(transport_timeout_ms);
        let predicted_name =
            services
                .backend()
                .predict_name(&spec)
                .change_context(TrustedServerError::Auction {
                    message: format!(
                        "Provider {} backend prediction failed",
                        self.provider_name()
                    ),
                })?;
        let backend_name =
            services
                .backend()
                .ensure(&spec)
                .change_context(TrustedServerError::Auction {
                    message: format!(
                        "Provider {} backend registration failed",
                        self.provider_name()
                    ),
                })?;
        if backend_name != predicted_name {
            return Err(Report::new(TrustedServerError::Auction {
                message: format!(
                    "Provider {} backend registration did not match prediction",
                    self.provider_name()
                ),
            }));
        }
        if !reserved_backend_names.insert(backend_name.clone()) {
            return Err(Report::new(TrustedServerError::Auction {
                message: format!(
                    "Provider {} resolved an actual backend name already owned by another provider",
                    self.provider_name()
                ),
            }));
        }

        let OutboundRequest {
            body,
            headers,
            slot_ids,
            captured,
        } = outbound;
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(self.plan.endpoint.as_str());
        if let Some(target) = builder.headers_mut() {
            *target = headers;
        }
        let outbound =
            builder
                .body(EdgeBody::from(body))
                .change_context(TrustedServerError::Auction {
                    message: format!(
                        "Provider {} request construction failed",
                        self.provider_name()
                    ),
                })?;
        let pending = services
            .http_client()
            .send_async(PlatformHttpRequest::new(outbound, backend_name.clone()))
            .await
            .change_context(TrustedServerError::Auction {
                message: format!("Provider {} request launch failed", self.provider_name()),
            })?;
        if pending.backend_name() != Some(backend_name.as_str()) {
            return Err(Report::new(TrustedServerError::Auction {
                message: format!(
                    "Provider {} pending request backend did not match registered backend",
                    self.provider_name()
                ),
            }));
        }
        Ok(ProviderRequestOutcome::pending_with_state(
            pending,
            Box::new(RoutedDemandParseState {
                provider_id: self.provider_name().to_string(),
                input: input.filtered_slots(|slot| slot_ids.contains(slot.slot().id.as_str())),
                captured,
            }),
        ))
    }

    /// Parse a response using state created by this exact provider instance.
    ///
    /// The source reads its own response and reports an unusable one as an
    /// error response. The provider then applies the common notification
    /// policy and routing diagnostics, so every demand source reports them
    /// the same way.
    pub(crate) async fn parse_response_with_state(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
        parse_state: Option<&(dyn Any + Send + Sync)>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let parse_state = parse_state
            .and_then(|state| state.downcast_ref::<RoutedDemandParseState>())
            .ok_or_else(|| {
                Report::new(TrustedServerError::Auction {
                    message: format!(
                        "Provider {} received missing or invalid response state",
                        self.provider_name()
                    ),
                })
            })?;
        if parse_state.provider_id != self.provider_name() {
            return Err(Report::new(TrustedServerError::Auction {
                message: format!(
                    "Provider {} received response state owned by provider {}",
                    self.provider_name(),
                    parse_state.provider_id
                ),
            }));
        }

        let demand = self.plan.demand.as_ref();
        let mut parsed = demand
            .parse_response(
                DemandResponse {
                    provider_id: self.provider_name(),
                    endpoint: self.plan.endpoint.as_str(),
                    input: &parse_state.input,
                    response_time_ms,
                    captured: parse_state.captured.as_deref(),
                },
                response,
            )
            .await?;
        apply_notification_policy(&mut parsed.bids, &self.plan.notifications);
        attach_provider_routing_metadata(&mut parsed, demand, &parse_state.input);
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashSet};
    use std::str::FromStr as _;
    use std::sync::Arc;

    use edgezero_core::body::Body as EdgeBody;
    use http::Request;
    use serde_json::{Value, json};

    use super::{
        RoutedDemandProvider, apply_notification_policy, saturating_bidder_param_counts,
        unused_bidder_params_count,
    };
    use crate::auction::plan::{
        AuctionPlan, BidderId, BidderRouteConfig, NotificationPolicy, ProviderId,
    };
    use crate::auction::provider::ProviderRequestOutcome;
    use crate::auction::routing::route_auction;
    use crate::auction::test_support::{
        canonical_parity_auction_request, demand_fixture, demand_table, plan_config,
    };
    use crate::auction::types::{AuctionResponse, Bid, BidStatus};
    use crate::platform::PlatformHttpClient;
    use crate::platform::test_support::{
        StubBackend, StubHttpClient, build_services_with_backend_and_http_client,
    };

    #[test]
    fn unused_bidder_param_count_saturates_across_slots_and_large_values() {
        assert_eq!(saturating_bidder_param_counts([1, 2, 3]), 6);
        assert_eq!(
            saturating_bidder_param_counts([usize::try_from(u32::MAX).unwrap_or(usize::MAX), 1]),
            u32::MAX
        );
        assert_eq!(saturating_bidder_param_counts([usize::MAX]), u32::MAX);
    }

    #[test]
    fn unused_bidder_param_count_follows_the_implementation() {
        for (implementation, expected) in [("auction.fixture", 0), ("auction.plain-fixture", 1)] {
            let endpoint = "https://provider.example/bid";
            let provider_id =
                ProviderId::from_str("fictional_provider").expect("should parse provider ID");
            let mut config = plan_config(vec![(
                "fictional_provider",
                demand_table(implementation, endpoint),
            )]);
            config.bidders = BTreeMap::from([(
                BidderId::from_str("exampleBidder").expect("should parse bidder ID"),
                BidderRouteConfig {
                    module: provider_id,
                },
            )]);
            let plan = AuctionPlan::compile(config).expect("should compile the plan");
            let inbound = Request::new(EdgeBody::empty());
            let routed = route_auction(canonical_parity_auction_request(), &inbound, &plan, None);

            assert_eq!(
                unused_bidder_params_count(
                    plan.providers()[0].demand.as_ref(),
                    &routed.inputs()[0]
                ),
                expected,
                "{implementation} should report only bidder params it ignores"
            );
        }
    }

    fn bid(seat: Option<&str>) -> Bid {
        Bid {
            slot_id: "slot".to_string(),
            price: Some(1.0),
            currency: "USD".to_string(),
            creative: Some("<div></div>".to_string()),
            adomain: None,
            bidder: seat.unwrap_or("unknown").to_string(),
            returned_seat: seat.map(str::to_string),
            width: 300,
            height: 250,
            nurl: Some("https://n.example".to_string()),
            burl: Some("https://b.example".to_string()),
            bid_id: None,
            ad_id: None,
            creative_id: None,
            renderer: None,
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn notification_suppression_uses_only_the_exact_returned_seat() {
        let mut exact = vec![
            bid(Some("exact")),
            bid(Some("Exact")),
            bid(None),
            bid(Some("unknown")),
        ];
        apply_notification_policy(
            &mut exact,
            &NotificationPolicy {
                suppress_all: false,
                suppress_seats: BTreeSet::from(["exact".to_string()]),
            },
        );
        assert!(exact[0].nurl.is_none(), "should suppress the exact seat");
        assert!(exact[1].nurl.is_some(), "matching should be case-sensitive");
        assert!(
            exact[2].nurl.is_some(),
            "a bid with no returned seat must not match a seat"
        );
        assert!(
            exact[3].nurl.is_some(),
            "the placeholder bidder name is not a returned seat"
        );

        let mut all = vec![bid(Some("exact")), bid(None)];
        apply_notification_policy(
            &mut all,
            &NotificationPolicy {
                suppress_all: true,
                suppress_seats: BTreeSet::new(),
            },
        );
        assert!(
            all.iter()
                .all(|bid| bid.nurl.is_none() && bid.burl.is_none()),
            "suppress_all should remove every notification"
        );
    }

    /// The provider sends exactly one request to the endpoint the plan
    /// settled, with the method, headers and body the source built, and a
    /// source's completion of a bare host is kept.
    #[test]
    fn a_completed_endpoint_is_sent_one_request_and_a_custom_path_is_kept() {
        futures::executor::block_on(async {
            for (configured_endpoint, expected_endpoint) in [
                (
                    "https://exchange.example",
                    format!("https://exchange.example{}", demand_fixture::AUCTION_PATH),
                ),
                (
                    "https://exchange.example/bid",
                    "https://exchange.example/bid".to_string(),
                ),
            ] {
                let mut table = demand_table(demand_fixture::MODULE, configured_endpoint);
                table.insert("timeout_ms".to_string(), json!(321));
                let mut config = plan_config(vec![("fictional_provider", table)]);
                config.timeout_ms = 321;
                config.bidders = BTreeMap::from([(
                    BidderId::from_str("exampleBidder").expect("should parse bidder ID"),
                    BidderRouteConfig {
                        module: ProviderId::from_str("fictional_provider")
                            .expect("should parse provider ID"),
                    },
                )]);
                let plan = AuctionPlan::compile(config).expect("should compile the endpoint");
                let inbound = Request::builder()
                    .uri("https://publisher.example/auction")
                    .body(EdgeBody::empty())
                    .expect("should build inbound request");
                let routed =
                    route_auction(canonical_parity_auction_request(), &inbound, &plan, None);
                let provider_plan = plan.providers()[0].clone();
                let provider = RoutedDemandProvider::new(provider_plan.clone());
                let client = Arc::new(StubHttpClient::new());
                client.push_response_with_headers(
                    302,
                    Vec::new(),
                    vec![("location", "https://redirect.example.test/bid")],
                );
                let services = build_services_with_backend_and_http_client(
                    Arc::new(StubBackend),
                    Arc::clone(&client) as Arc<dyn PlatformHttpClient>,
                );
                let mut reserved_backend_names = HashSet::new();

                let outcome = provider
                    .request_bids_routed(
                        &routed.inputs()[0],
                        &routed,
                        321,
                        321,
                        None,
                        &services,
                        &mut reserved_backend_names,
                    )
                    .await
                    .expect("should launch one request");
                let ProviderRequestOutcome::Pending {
                    request,
                    parse_state,
                } = outcome
                else {
                    panic!("should launch a pending request");
                };
                let selected = services
                    .http_client()
                    .select(vec![request])
                    .await
                    .expect("should select one response");
                let response = selected.ready.expect("should receive the response");
                let parsed = provider
                    .parse_response_with_state(response, 7, parse_state.as_deref())
                    .await
                    .expect("should read the response");

                assert_eq!(parsed.status, BidStatus::Error);
                assert_eq!(parsed.metadata["error_type"], "http_status");
                assert_eq!(parsed.metadata["http_status"], 302);
                assert_eq!(parsed.metadata["routing"]["unused_bidder_params_count"], 0);
                assert_eq!(
                    client.recorded_backend_names(),
                    vec!["stub-backend"],
                    "a redirect must not trigger a second send"
                );
                assert_eq!(client.recorded_request_methods(), vec!["POST"]);
                assert_eq!(client.recorded_request_uris(), vec![expected_endpoint]);
                let headers = &client.recorded_request_headers()[0];
                assert!(
                    headers
                        .iter()
                        .any(|(name, value)| name == "content-type" && value == "application/json"),
                    "the source's headers should be sent as built"
                );
                let spec = provider_plan.backend_spec();
                assert_eq!(spec.host, "exchange.example");
                assert_eq!(spec.discriminator.as_deref(), Some("fictional_provider"));
                let body: Value = serde_json::from_slice(&client.recorded_request_bodies()[0])
                    .expect("should parse the recorded body");
                assert_eq!(
                    body["auction"], "fictional-auction",
                    "the body should be the one the source built"
                );
            }
        });
    }

    /// A source that asks for nothing makes no request, and the response
    /// says so.
    #[test]
    fn a_source_with_nothing_to_ask_for_makes_no_request() {
        futures::executor::block_on(async {
            let mut config = plan_config(vec![(
                "fictional_provider",
                demand_table(demand_fixture::MODULE, "https://exchange.example/bid"),
            )]);
            config.bidders = BTreeMap::from([(
                BidderId::from_str("exampleBidder").expect("should parse bidder ID"),
                BidderRouteConfig {
                    module: ProviderId::from_str("fictional_provider")
                        .expect("should parse provider ID"),
                },
            )]);
            let plan = AuctionPlan::compile(config).expect("should compile the plan");
            let mut request = canonical_parity_auction_request();
            request.slots[0].bidders = std::collections::HashMap::from([(
                "trustedServer".to_string(),
                json!({"bidderParams":{"exampleBidder":{}}, "storedRequest": false}),
            )]);
            let inbound = Request::new(EdgeBody::empty());
            let routed = route_auction(request, &inbound, &plan, None);
            assert_eq!(routed.inputs().len(), 1, "the slot is routed to the source");
            let provider = RoutedDemandProvider::new(plan.providers()[0].clone());
            let client = Arc::new(StubHttpClient::new());
            let services = build_services_with_backend_and_http_client(
                Arc::new(StubBackend),
                Arc::clone(&client) as Arc<dyn PlatformHttpClient>,
            );

            let outcome = provider
                .request_bids_routed(
                    &routed.inputs()[0],
                    &routed,
                    321,
                    321,
                    None,
                    &services,
                    &mut HashSet::new(),
                )
                .await
                .expect("should answer without a request");
            let ProviderRequestOutcome::Immediate(response) = outcome else {
                panic!("should answer immediately");
            };
            assert_eq!(response.status, BidStatus::NoBid);
            assert_eq!(
                response.metadata["routing"]["skipped_no_usable_demand"],
                true
            );
            assert!(
                client.recorded_request_uris().is_empty(),
                "nothing should be sent"
            );
            let _: AuctionResponse = response;
        });
    }
}
