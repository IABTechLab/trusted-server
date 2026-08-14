//! Mock Ad Server Integration
//!
//! Provides a mock ad server mediator that calls mocktioneer's mediation endpoint.
//! This integration acts as a mediator in the auction flow, selecting winning bids
//! based on price (highest price wins).

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{Method, header};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;
use validator::Validate;

use crate::auction::context::{ContextQueryParams, build_url_with_context_params};
use crate::auction::provider::{AuctionProvider, ProviderRequestOutcome};
use crate::auction::types::{
    AuctionContext, AuctionRequest, AuctionResponse, Bid, BidStatus, MediaType,
};
use crate::error::TrustedServerError;
use crate::integrations::{
    UPSTREAM_RTB_MAX_RESPONSE_BYTES, collect_response_bounded,
    ensure_integration_backend_with_timeout, predict_integration_backend_name,
};
use crate::platform::{PlatformHttpRequest, PlatformResponse, RuntimeServices};
use crate::settings::{IntegrationConfig, Settings};

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for mock ad server integration.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct AdServerMockConfig {
    /// Whether this integration is enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Mediation endpoint URL
    #[validate(url)]
    pub endpoint: String,

    /// Timeout in milliseconds
    #[serde(default = "default_timeout_ms")]
    #[validate(range(min = 1, max = 60000))]
    pub timeout_ms: u32,

    /// Optional price floor (minimum acceptable CPM)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_floor: Option<f64>,

    /// Mapping from auction-request context keys to query-parameter names.
    /// Allows forwarding integration-supplied data (e.g. audience segments)
    /// to the mediation endpoint without hard-coding integration knowledge.
    ///
    /// ```toml
    /// [integrations.adserver_mock.context_query_params]
    /// permutive_segments = "permutive"
    /// ```
    #[serde(default)]
    pub context_query_params: ContextQueryParams,
}

fn default_enabled() -> bool {
    false
}

fn default_timeout_ms() -> u32 {
    500
}

impl Default for AdServerMockConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            endpoint: "http://localhost:6767/adserver/mediate".to_string(),
            timeout_ms: default_timeout_ms(),
            price_floor: None,
            context_query_params: BTreeMap::new(),
        }
    }
}

impl IntegrationConfig for AdServerMockConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

// ============================================================================
// Provider
// ============================================================================

/// Lookup index built from the original SSP bids, used while parsing the
/// mediation response to restore render/accounting fields that the mock
/// mediator endpoint does not echo back.
///
/// Keyed only by the server-minted opaque candidate identifier.
type BidIndex = HashMap<String, Bid>;

fn optional_selection_field_matches<T: Serialize>(
    selection: &serde_json::Map<String, Json>,
    field: &str,
    expected: &Option<T>,
) -> bool {
    let Some(actual) = selection.get(field) else {
        return true;
    };
    expected
        .as_ref()
        .and_then(|expected| serde_json::to_value(expected).ok())
        .is_some_and(|expected| expected == *actual)
}

/// Builds the SSP-bid lookup index from the orchestrator-provided
/// bidder responses on the auction context.
fn build_bid_index(bidder_responses: &[AuctionResponse]) -> Option<BidIndex> {
    let mut index = BidIndex::new();
    for response in bidder_responses {
        for bid in &response.bids {
            let Some(candidate_id) = bid.candidate_id.as_ref() else {
                log::warn!(
                    "adserver_mock: source bid from provider '{}' lacks a candidate id",
                    response.provider
                );
                return None;
            };
            if index.insert(candidate_id.clone(), bid.clone()).is_some() {
                log::warn!("adserver_mock: duplicate source candidate id");
                return None;
            }
        }
    }
    Some(index)
}

/// Mock ad server mediator provider.
pub struct AdServerMockProvider {
    config: AdServerMockConfig,
}

impl AdServerMockProvider {
    /// Create a new mock ad server provider.
    #[must_use]
    pub fn new(config: AdServerMockConfig) -> Self {
        Self { config }
    }

    /// Build the mediation endpoint URL, appending context values as query
    /// parameters according to the `context_query_params` config mapping.
    ///
    /// For example, with `context_query_params = { permutive_segments = "permutive" }`
    /// and segments `[10000001, 10000003]` in context, the URL becomes
    /// `https://…/adserver/mediate?permutive=10000001,10000003`.
    fn build_endpoint_url(&self, request: &AuctionRequest) -> String {
        build_url_with_context_params(
            &self.config.endpoint,
            &request.context,
            &self.config.context_query_params,
        )
    }

    /// Build mediation request from auction request and bidder responses.
    ///
    /// Only bids with decoded numeric prices are eligible for mediation.
    fn build_mediation_request(
        &self,
        request: &AuctionRequest,
        bidder_responses: &[AuctionResponse],
    ) -> Result<Json, Report<TrustedServerError>> {
        // Convert bidder responses to mediation format
        let bidder_responses_json: Vec<Json> = bidder_responses
            .iter()
            .filter(|r| r.status == BidStatus::Success)
            .map(|response| {
                let bids: Vec<Json> = response
                    .bids
                    .iter()
                    .filter_map(|bid| {
                        let Some(price) = bid.price else {
                            log::debug!(
                                "adserver_mock: omitting bid for slot '{}' without decoded price",
                                bid.slot_id
                            );
                            return None;
                        };
                        let Some(candidate_id) = bid.candidate_id.as_deref() else {
                            log::warn!(
                                "adserver_mock: omitting source bid for slot '{}' without a candidate id",
                                bid.slot_id
                            );
                            return None;
                        };
                        Some(json!({
                            "imp_id": bid.slot_id,
                            "price": price,
                            "adm": bid.creative,
                            "w": bid.width,
                            "h": bid.height,
                            "crid": bid.creative_id,
                            "adomain": bid.adomain,
                            "ext": {
                                "trusted_server": {
                                    "candidate_id": candidate_id
                                }
                            }
                        }))
                    })
                    .collect();

                json!({
                    "bidder": response.provider,
                    "bids": bids,
                })
            })
            .collect();

        // Build impressions from request slots
        let imps: Vec<Json> = request
            .slots
            .iter()
            .map(|slot| {
                let banner_format = slot
                    .formats
                    .iter()
                    .find(|f| f.media_type == MediaType::Banner);

                json!({
                    "id": slot.id,
                    "banner": banner_format.map(|f| json!({
                        "w": f.width,
                        "h": f.height,
                    })),
                })
            })
            .collect();

        // Build mediation config
        let config_json = if self.config.price_floor.is_some() {
            json!({
                "price_floor": self.config.price_floor,
            })
        } else {
            json!(null)
        };

        // Build consent summary from ConsentContext
        let consent_json = request.user.consent.as_ref().map(|ctx| {
            json!({
                "gdpr": if ctx.gdpr_applies { 1 } else { 0 },
                "consent": ctx.raw_tc_string,
                "us_privacy": ctx.raw_us_privacy,
                "gpp": ctx.raw_gpp_string,
                "gpp_sid": ctx.gpp_section_ids,
            })
        });

        // Build full mediation request
        Ok(json!({
            "id": request.id,
            "imp": imps,
            "ext": {
                "bidder_responses": bidder_responses_json,
                "config": config_json,
                "consent": consent_json,
            },
        }))
    }

    /// Parse `OpenRTB` response from mediation endpoint.
    /// Mediation returns decoded prices for all selected bids.
    ///
    /// `bid_index` is the SSP-bid lookup built from the auction context's
    /// bidder responses. The mediator may select only by echoing one exact
    /// server-minted candidate id; all render and accounting authority is
    /// restored from the indexed source candidate.
    fn parse_mediation_response(
        &self,
        json: &Json,
        response_time_ms: u64,
        bid_index: &BidIndex,
    ) -> AuctionResponse {
        let Some(seatbids) = json.get("seatbid").and_then(Json::as_array) else {
            return AuctionResponse::error("adserver_mock", response_time_ms)
                .with_metadata("mediation_error", json!("invalid_candidate_provenance"));
        };
        let mut all_bids = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut seen_slots = std::collections::HashSet::new();
        for seat in seatbids {
            let Some(seat_object) = seat.as_object() else {
                return AuctionResponse::error("adserver_mock", response_time_ms)
                    .with_metadata("mediation_error", json!("invalid_candidate_provenance"));
            };
            if !seat_object
                .keys()
                .all(|key| matches!(key.as_str(), "seat" | "bid"))
            {
                return AuctionResponse::error("adserver_mock", response_time_ms)
                    .with_metadata("mediation_error", json!("invalid_candidate_provenance"));
            }
            let Some(bids) = seat_object.get("bid").and_then(Json::as_array) else {
                return AuctionResponse::error("adserver_mock", response_time_ms)
                    .with_metadata("mediation_error", json!("invalid_candidate_provenance"));
            };
            for bid in bids {
                let Some(selection) = bid.as_object() else {
                    return AuctionResponse::error("adserver_mock", response_time_ms)
                        .with_metadata("mediation_error", json!("invalid_candidate_provenance"));
                };
                if !selection.keys().all(|key| {
                    matches!(
                        key.as_str(),
                        "id" | "impid"
                            | "price"
                            | "ext"
                            | "adm"
                            | "w"
                            | "h"
                            | "crid"
                            | "adomain"
                            | "nurl"
                            | "burl"
                    )
                }) {
                    return AuctionResponse::error("adserver_mock", response_time_ms)
                        .with_metadata("mediation_error", json!("invalid_candidate_provenance"));
                }
                let trusted = selection
                    .get("ext")
                    .and_then(Json::as_object)
                    .filter(|ext| ext.len() == 1)
                    .and_then(|ext| ext.get("trusted_server"))
                    .and_then(Json::as_object);
                let candidate_id = trusted
                    .filter(|trusted| trusted.len() == 1)
                    .and_then(|trusted| trusted.get("candidate_id"))
                    .and_then(Json::as_str);
                let Some(candidate_id) = candidate_id else {
                    return AuctionResponse::error("adserver_mock", response_time_ms)
                        .with_metadata("mediation_error", json!("invalid_candidate_provenance"));
                };
                let Some(original) = bid_index.get(candidate_id) else {
                    return AuctionResponse::error("adserver_mock", response_time_ms)
                        .with_metadata("mediation_error", json!("invalid_candidate_provenance"));
                };
                let seat_matches = seat_object
                    .get("seat")
                    .is_none_or(|seat| seat.as_str() == original.candidate_provider.as_deref());
                let currency_matches = json
                    .get("cur")
                    .is_none_or(|currency| currency.as_str() == Some(original.currency.as_str()));
                let authority_matches = selection
                    .get("w")
                    .is_none_or(|width| width.as_u64() == Some(u64::from(original.width)))
                    && selection
                        .get("h")
                        .is_none_or(|height| height.as_u64() == Some(u64::from(original.height)))
                    && optional_selection_field_matches(selection, "adm", &original.creative)
                    && optional_selection_field_matches(selection, "crid", &original.creative_id)
                    && optional_selection_field_matches(selection, "adomain", &original.adomain)
                    && optional_selection_field_matches(selection, "nurl", &original.nurl)
                    && optional_selection_field_matches(selection, "burl", &original.burl);
                if !seen.insert(candidate_id)
                    || !seen_slots.insert(original.slot_id.as_str())
                    || selection.get("impid").and_then(Json::as_str)
                        != Some(original.slot_id.as_str())
                    || selection
                        .get("price")
                        .and_then(Json::as_f64)
                        .is_none_or(|price| !price.is_finite() || price < 0.0)
                    || !seat_matches
                    || !currency_matches
                    || !authority_matches
                {
                    return AuctionResponse::error("adserver_mock", response_time_ms)
                        .with_metadata("mediation_error", json!("invalid_candidate_provenance"));
                }

                let mut resolved = original.clone();
                resolved.price = selection.get("price").and_then(Json::as_f64);
                all_bids.push(resolved);
            }
        }

        if all_bids.is_empty() {
            AuctionResponse::no_bid("adserver_mock", response_time_ms)
        } else {
            AuctionResponse::success("adserver_mock", all_bids, response_time_ms)
        }
    }

    /// Shared parse body for the context-aware and context-less trait methods.
    ///
    /// # Errors
    ///
    /// Returns an error when the mediation response body is not valid JSON.
    async fn parse_response_inner(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
        bid_index: &BidIndex,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let response = response.response;

        if !response.status().is_success() {
            log::warn!("AdServer Mock returned non-success: {}", response.status());
            return Ok(AuctionResponse::error("adserver_mock", response_time_ms));
        }

        // collect_response_bounded caps memory from misbehaving providers.
        let body_bytes = collect_response_bounded(
            response.into_body(),
            UPSTREAM_RTB_MAX_RESPONSE_BYTES,
            "adserver_mock",
        )
        .await
        .change_context(TrustedServerError::Auction {
            message: "Failed to read AdServer Mock response body".to_string(),
        })?;
        let response_json: Json =
            serde_json::from_slice(&body_bytes).change_context(TrustedServerError::Auction {
                message: "Failed to parse mediation response".to_string(),
            })?;

        log::trace!("AdServer Mock response: {:?}", response_json);

        let auction_response =
            self.parse_mediation_response(&response_json, response_time_ms, bid_index);

        log::info!(
            "AdServer Mock returned {} bids in {}ms",
            auction_response.bids.len(),
            response_time_ms
        );

        Ok(auction_response)
    }
}

#[async_trait(?Send)]
impl AuctionProvider for AdServerMockProvider {
    fn provider_name(&self) -> &str {
        "adserver_mock"
    }

    async fn request_bids(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
        // Get bidder responses from context (passed by orchestrator for mediation)
        let bidder_responses = context.provider_responses.unwrap_or(&[]);

        log::info!(
            "AdServer Mock: mediating {} slots with {} bidder responses",
            request.slots.len(),
            bidder_responses.len()
        );

        // Build mediation request
        let mediation_req = self
            .build_mediation_request(request, bidder_responses)
            .change_context(TrustedServerError::Auction {
                message: "Failed to build mediation request".to_string(),
            })?;

        log::trace!("AdServer Mock: mediation request: {:?}", mediation_req);

        // Build endpoint URL with context-driven query parameters
        let endpoint_url = self.build_endpoint_url(request);

        // Create HTTP POST request
        let mediation_body =
            serde_json::to_vec(&mediation_req).change_context(TrustedServerError::Auction {
                message: "Failed to serialize mediation request".to_string(),
            })?;
        let mut req = http::Request::builder()
            .method(Method::POST)
            .uri(&endpoint_url)
            .header(header::CONTENT_TYPE, "application/json")
            .body(EdgeBody::from(mediation_body))
            .change_context(TrustedServerError::Auction {
                message: "Failed to build mediation request".to_string(),
            })?;

        // Set Host header with port to ensure mocktioneer generates correct iframe URLs
        if let Ok(url) = url::Url::parse(&self.config.endpoint)
            && let Some(host) = url.host_str()
        {
            let host_with_port = if let Some(port) = url.port() {
                format!("{}:{}", host, port)
            } else {
                host.to_string()
            };
            match header::HeaderValue::from_str(&host_with_port) {
                Ok(value) => {
                    req.headers_mut().insert(header::HOST, value);
                }
                Err(e) => {
                    log::warn!(
                        "Failed to build Host header for '{}': {}",
                        host_with_port,
                        e
                    );
                }
            }
        }

        // Uses the auction-scoped canonical transport timeout rather than the
        // 15 s fixed timeout in ensure_integration_backend, which is for proxy
        // endpoints. The exact logical budget remains in context.timeout_ms.
        let backend_name = ensure_integration_backend_with_timeout(
            context.services,
            &self.config.endpoint,
            "adserver_mock",
            Duration::from_millis(u64::from(context.transport_timeout_ms)),
        )
        .change_context(TrustedServerError::Auction {
            message: format!(
                "Failed to resolve backend for mediation endpoint: {}",
                self.config.endpoint
            ),
        })?;

        let pending = context
            .services
            .http_client()
            .send_async(PlatformHttpRequest::new(req, backend_name))
            .await
            .change_context(TrustedServerError::Auction {
                message: "Failed to send mediation request".to_string(),
            })?;

        Ok(ProviderRequestOutcome::pending(pending))
    }

    async fn parse_response(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        // No auction context available — nurl/burl/ad_id restoration from the
        // original SSP bids is skipped. The orchestrator always calls
        // [`parse_response_with_context`], so this path only serves callers
        // outside the orchestration flow.
        log::debug!("adserver_mock: parsing without context — SSP bid metadata unavailable");
        self.parse_response_inner(response, response_time_ms, &BidIndex::new())
            .await
    }

    async fn parse_response_with_context(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
        _request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        // Rebuild the SSP-bid lookup from the orchestrator-provided bidder
        // responses so nurl/burl/ad_id survive mediation. Request-scoped data
        // travels on the context instead of provider-instance state.
        let Some(bid_index) = build_bid_index(context.provider_responses.unwrap_or(&[])) else {
            return Ok(AuctionResponse::error("adserver_mock", response_time_ms)
                .with_metadata("mediation_error", json!("invalid_candidate_provenance")));
        };
        self.parse_response_inner(response, response_time_ms, &bid_index)
            .await
    }

    fn supports_media_type(&self, media_type: &MediaType) -> bool {
        matches!(media_type, MediaType::Banner)
    }

    fn timeout_ms(&self) -> u32 {
        self.config.timeout_ms
    }

    fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    fn backend_name(
        &self,
        services: &RuntimeServices,
        transport_timeout_ms: u32,
    ) -> Option<String> {
        predict_integration_backend_name(
            services,
            &self.config.endpoint,
            "adserver_mock",
            Duration::from_millis(u64::from(transport_timeout_ms)),
        )
        .inspect_err(|e| {
            log::error!(
                "Failed to predict backend name for AdServer Mock endpoint '{}': {e:?}",
                self.config.endpoint
            );
        })
        .ok()
    }
}

// ============================================================================
// Auto-Registration
// ============================================================================

/// Auto-register ad server mock provider based on settings configuration.
///
/// # Errors
///
/// Returns an error when the ad server mock provider is enabled with invalid
/// configuration.
pub fn register_providers(
    settings: &Settings,
) -> Result<Vec<Arc<dyn AuctionProvider>>, Report<TrustedServerError>> {
    let mut providers: Vec<Arc<dyn AuctionProvider>> = Vec::new();

    match settings.integration_config::<AdServerMockConfig>("adserver_mock") {
        Ok(Some(config)) => {
            log::info!(
                "Registering AdServer Mock mediator (endpoint: {})",
                config.endpoint
            );
            providers.push(Arc::new(AdServerMockProvider::new(config)));
        }
        Ok(None) => {
            log::debug!("AdServer Mock config found but is disabled");
        }
        Err(e) => {
            return Err(e);
        }
    }

    Ok(providers)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::context::ContextValue;
    use crate::auction::types::*;

    fn create_test_auction_request() -> AuctionRequest {
        AuctionRequest {
            id: "test-auction-123".to_string(),
            slots: vec![AdSlot {
                id: "header-banner".to_string(),
                formats: vec![AdFormat {
                    media_type: MediaType::Banner,
                    width: 728,
                    height: 90,
                }],
                floor_price: Some(1.50),
                targeting: HashMap::new(),
                bidders: HashMap::new(),
            }],
            publisher: PublisherInfo {
                domain: "test.com".to_string(),
                page_url: Some("https://test.com/article".to_string()),
            },
            user: UserInfo {
                id: Some("user-123".to_string()),
                consent: None,
                eids: None,
            },
            device: Some(DeviceInfo {
                user_agent: Some("Mozilla/5.0".to_string()),
                ip: Some("192.168.1.1".to_string()),
                geo: None,
            }),
            site: None,
            context: HashMap::new(),
        }
    }

    fn aps_bid(bid_id: &str, price: f64) -> Bid {
        Bid {
            slot_id: "header-banner".to_string(),
            candidate_id: Some(format!(
                "{:012x}",
                bid_id.bytes().map(u64::from).sum::<u64>()
            )),
            candidate_provider: Some("aps".to_string()),
            renderer_reservation_id: None,
            price: Some(price),
            currency: "USD".to_string(),
            creative: None,
            width: 728,
            height: 90,
            bidder: "aps".to_string(),
            returned_seat: None,
            adomain: Some(vec!["advertiser.example".to_string()]),
            nurl: None,
            burl: None,
            bid_id: Some(bid_id.to_string()),
            ad_id: None,
            creative_id: Some(format!("creative-{bid_id}")),
            renderer: Some(BidRenderSourceV1::Aps(ApsRendererV1 {
                version: 1,
                account_id: "example-account".to_string(),
                bid_id: bid_id.to_string(),
                creative_id: Some(format!("creative-{bid_id}")),
                tag_type: ApsTagType::Iframe,
                creative_url: format!("https://creative.example/{bid_id}"),
                aax_response: format!("fictional-{bid_id}-base64"),
                width: 728,
                height: 90,
            })),
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn test_build_mediation_request() {
        let config = AdServerMockConfig {
            enabled: true,
            endpoint: "http://localhost:6767/adserver/mediate".to_string(),
            timeout_ms: 500,
            price_floor: Some(1.00),
            context_query_params: BTreeMap::new(),
        };

        let provider = AdServerMockProvider::new(config);
        let auction_request = create_test_auction_request();

        let bidder_responses = vec![
            AuctionResponse {
                provider: "aps".to_string(),
                status: BidStatus::Success,
                bids: vec![Bid {
                    slot_id: "header-banner".to_string(),
                    candidate_id: Some("AAAAAAAAAAAA".to_string()),
                    candidate_provider: Some("aps".to_string()),
                    renderer_reservation_id: None,
                    price: Some(3.00),
                    currency: "USD".to_string(),
                    creative: Some("<div>APS Ad</div>".to_string()),
                    width: 728,
                    height: 90,
                    bidder: "aps".to_string(),
                    returned_seat: None,
                    adomain: Some(vec!["advertiser.example".to_string()]),
                    nurl: None,
                    burl: None,
                    bid_id: None,
                    ad_id: None,
                    creative_id: None,
                    renderer: None,
                    cache_id: None,
                    cache_host: None,
                    cache_path: None,
                    metadata: HashMap::new(),
                }],
                response_time_ms: 150,
                metadata: HashMap::new(),
            },
            AuctionResponse {
                provider: "test-bidder".to_string(),
                status: BidStatus::Success,
                bids: vec![Bid {
                    slot_id: "header-banner".to_string(),
                    candidate_id: Some("BBBBBBBBBBBB".to_string()),
                    candidate_provider: Some("test-bidder".to_string()),
                    renderer_reservation_id: None,
                    price: Some(3.50),
                    currency: "USD".to_string(),
                    creative: Some("<div>Test Ad</div>".to_string()),
                    width: 728,
                    height: 90,
                    bidder: "test-bidder".to_string(),
                    returned_seat: None,
                    adomain: None,
                    nurl: Some("https://ssp.example/win?id=mock-bid-001".to_string()),
                    burl: Some("https://ssp.example/bill?id=mock-bid-001".to_string()),
                    bid_id: None,
                    ad_id: Some("mock-bid-001".to_string()),
                    creative_id: None,
                    renderer: None,
                    cache_id: None,
                    cache_host: None,
                    cache_path: None,
                    metadata: HashMap::new(),
                }],
                response_time_ms: 120,
                metadata: HashMap::new(),
            },
        ];

        let mediation_req = provider
            .build_mediation_request(&auction_request, &bidder_responses)
            .expect("should build mediation request");

        // Verify structure
        assert_eq!(mediation_req["id"], "test-auction-123");
        assert_eq!(
            mediation_req["imp"]
                .as_array()
                .expect("imp should be array")
                .len(),
            1
        );
        assert_eq!(
            mediation_req["ext"]["bidder_responses"]
                .as_array()
                .expect("bidder_responses should be array")
                .len(),
            2
        );
        assert_eq!(mediation_req["ext"]["config"]["price_floor"], 1.00);
        assert_eq!(
            mediation_req["ext"]["bidder_responses"][0]["bids"][0]["ext"]["trusted_server"]["candidate_id"],
            "AAAAAAAAAAAA"
        );
        assert_eq!(
            mediation_req["ext"]["bidder_responses"][1]["bids"][0]["ext"]["trusted_server"]["candidate_id"],
            "BBBBBBBBBBBB"
        );
    }

    #[test]
    fn test_parse_mediation_response() {
        let config = AdServerMockConfig::default();
        let provider = AdServerMockProvider::new(config);
        let source = aps_bid("selected", 1.0);
        let candidate_id = source
            .candidate_id
            .clone()
            .expect("source should have candidate id");
        let bid_index = HashMap::from([(candidate_id.clone(), source)]);

        let mediation_response = json!({
            "id": "test-auction-123",
            "seatbid": [
                {
                    "seat": "aps",
                    "bid": [
                        {
                            "id": "bid-001",
                            "impid": "header-banner",
                            "price": 3.50,
                            "ext": {"trusted_server": {"candidate_id": candidate_id}}
                        }
                    ]
                }
            ],
            "cur": "USD"
        });

        let auction_response =
            provider.parse_mediation_response(&mediation_response, 200, &bid_index);

        assert_eq!(auction_response.provider, "adserver_mock");
        assert_eq!(auction_response.status, BidStatus::Success);
        assert_eq!(auction_response.bids.len(), 1);
        assert_eq!(auction_response.response_time_ms, 200);

        let bid = &auction_response.bids[0];
        assert_eq!(bid.slot_id, "header-banner");
        assert_eq!(bid.price, Some(3.50)); // Mediation returns decoded price
        assert_eq!(bid.bidder, "aps");
        assert_eq!(bid.width, 728);
        assert_eq!(bid.height, 90);
    }

    #[test]
    fn parse_mediation_response_restores_original_bid_render_fields() {
        let provider = AdServerMockProvider::new(AdServerMockConfig::default());
        let candidate_id = "AAAAAAAAAAAA";
        let mediation_response = json!({
            "id": "test-auction-123",
            "seatbid": [
                {
                    "seat": "prebid",
                    "bid": [
                        {
                            "id": "mediated-bid-001",
                            "impid": "header-banner",
                            "price": 0.20,
                            "ext": {"trusted_server": {"candidate_id": candidate_id}}
                        }
                    ]
                }
            ],
            "cur": "USD"
        });
        let mut bid_index = BidIndex::new();
        bid_index.insert(
            candidate_id.to_string(),
            Bid {
                slot_id: "header-banner".to_string(),
                candidate_id: Some(candidate_id.to_string()),
                candidate_provider: Some("prebid".to_string()),
                renderer_reservation_id: None,
                price: Some(0.20),
                currency: "USD".to_string(),
                creative: Some("<div>Original Ad</div>".to_string()),
                adomain: Some(vec!["example.com".to_string()]),
                bidder: "mocktioneer".to_string(),
                returned_seat: None,
                width: 728,
                height: 90,
                nurl: Some("https://ssp.example/win".to_string()),
                burl: Some("https://ssp.example/bill".to_string()),
                bid_id: Some("source-bid-id".to_string()),
                ad_id: Some("bid-impression-id".to_string()),
                creative_id: Some("source-creative-id".to_string()),
                renderer: Some(BidRenderSourceV1::Aps(ApsRendererV1 {
                    version: 1,
                    account_id: "example-account".to_string(),
                    bid_id: "source-bid-id".to_string(),
                    creative_id: Some("source-creative-id".to_string()),
                    tag_type: ApsTagType::Iframe,
                    creative_url: "https://creative.example/render".to_string(),
                    aax_response: "fictional-base64".to_string(),
                    width: 728,
                    height: 90,
                })),
                cache_id: Some("cache-uuid".to_string()),
                cache_host: Some("cache.example".to_string()),
                cache_path: Some("/cache".to_string()),
                metadata: HashMap::new(),
            },
        );

        let auction_response =
            provider.parse_mediation_response(&mediation_response, 42, &bid_index);

        assert_eq!(auction_response.status, BidStatus::Success);
        assert_eq!(auction_response.bids.len(), 1);
        let bid = &auction_response.bids[0];
        assert_eq!(
            bid.bidder, "mocktioneer",
            "should preserve underlying bidder for hb_bidder targeting"
        );
        assert_eq!(
            bid.nurl.as_deref(),
            Some("https://ssp.example/win"),
            "should restore nurl"
        );
        assert_eq!(
            bid.burl.as_deref(),
            Some("https://ssp.example/bill"),
            "should restore burl"
        );
        assert_eq!(
            bid.bid_id.as_deref(),
            Some("mediated-bid-001"),
            "should carry the mediated OpenRTB bid id so hb_adid always has a source"
        );
        assert_eq!(
            bid.ad_id.as_deref(),
            Some("bid-impression-id"),
            "should restore ad_id"
        );
        assert_eq!(bid.creative_id.as_deref(), Some("source-creative-id"));
        assert!(bid.renderer.is_some(), "should restore typed renderer");
        assert_eq!(
            bid.cache_id.as_deref(),
            Some("cache-uuid"),
            "should restore PBS cache UUID"
        );
        assert_eq!(
            bid.cache_host.as_deref(),
            Some("cache.example"),
            "should restore PBS cache host"
        );
        assert_eq!(
            bid.cache_path.as_deref(),
            Some("/cache"),
            "should restore PBS cache path"
        );
        assert!(
            bid.returned_seat.is_none(),
            "a matched original with no returned seat must not inherit mediator seat"
        );
    }

    #[test]
    fn parse_mediation_response_falls_back_to_original_bid_id() {
        // A mediator that omits the per-bid `id` must not strand a pass-through
        // bid whose only hb_adid source is its OpenRTB bid id.
        let provider = AdServerMockProvider::new(AdServerMockConfig::default());
        let mediation_response = json!({
            "id": "test-auction-123",
            "seatbid": [{
                "seat": "prebid",
                "bid": [{
                    "impid": "header-banner",
                    "price": 0.20,
                    "adm": "<div>Mediated Ad</div>",
                    "w": 728,
                    "h": 90,
                    "crid": "example-bidder-creative"
                }]
            }],
            "cur": "USD"
        });
        let mut bid_index = BidIndex::new();
        bid_index.insert(
            (
                "prebid".to_string(),
                "header-banner".to_string(),
                "example-bidder".to_string(),
            ),
            Bid {
                slot_id: "header-banner".to_string(),
                price: Some(0.20),
                currency: "USD".to_string(),
                creative: Some("<div>Original Ad</div>".to_string()),
                adomain: None,
                bidder: "example-bidder".to_string(),
                returned_seat: None,
                width: 728,
                height: 90,
                nurl: None,
                burl: None,
                bid_id: Some("019f7e2a-b45b-70b0-a2d1-b651c430700b".to_string()),
                ad_id: None,
                creative_id: None,
                renderer: None,
                cache_id: None,
                cache_host: None,
                cache_path: None,
                metadata: HashMap::new(),
            },
        );

        let auction_response =
            provider.parse_mediation_response(&mediation_response, 42, &bid_index);

        assert_eq!(
            auction_response.bids[0].bid_id.as_deref(),
            Some("019f7e2a-b45b-70b0-a2d1-b651c430700b"),
            "should restore the original SSP bid id when the mediator omits one"
        );
    }

    #[test]
    fn mediation_response_rejects_every_source_authority_substitution() {
        let provider = AdServerMockProvider::new(AdServerMockConfig::default());
        let candidate_id = "AAAAAAAAAAAA";
        let mut source = aps_bid("source-bid", 1.0);
        source.candidate_id = Some(candidate_id.to_string());
        source.renderer = None;
        source.creative = Some("<div>source creative</div>".to_string());
        source.nurl = Some("https://ssp.example/win".to_string());
        source.burl = Some("https://ssp.example/bill".to_string());
        let bid_index = BidIndex::from([(candidate_id.to_string(), source)]);

        for substitution in [
            "seat", "w", "h", "crid", "adomain", "nurl", "burl", "adm", "native", "renderer",
        ] {
            let mut response = json!({
                "id": "test-auction-123",
                "seatbid": [{
                    "seat": "aps",
                    "bid": [{
                        "id": "selection-1",
                        "impid": "header-banner",
                        "price": 2.0,
                        "ext": {"trusted_server": {"candidate_id": candidate_id}}
                    }]
                }],
                "cur": "USD"
            });
            match substitution {
                "seat" => response["seatbid"][0]["seat"] = json!("substituted-provider"),
                "w" => response["seatbid"][0]["bid"][0]["w"] = json!(1),
                "h" => response["seatbid"][0]["bid"][0]["h"] = json!(1),
                "crid" => response["seatbid"][0]["bid"][0]["crid"] = json!("other"),
                "adomain" => {
                    response["seatbid"][0]["bid"][0]["adomain"] = json!(["evil.example"]);
                }
                "nurl" => {
                    response["seatbid"][0]["bid"][0]["nurl"] = json!("https://evil.example/win");
                }
                "burl" => {
                    response["seatbid"][0]["bid"][0]["burl"] = json!("https://evil.example/bill");
                }
                "adm" => {
                    response["seatbid"][0]["bid"][0]["adm"] =
                        json!("<script>substituted()</script>");
                }
                "native" => {
                    response["seatbid"][0]["bid"][0]["native"] = json!({"ver": "1.2"});
                }
                "renderer" => {
                    response["seatbid"][0]["bid"][0]["renderer"] =
                        json!({"url": "https://evil.example/render.js"});
                }
                _ => unreachable!("closed substitution corpus"),
            }

            let parsed = provider.parse_mediation_response(&response, 1, &bid_index);
            assert_eq!(
                parsed.status,
                BidStatus::Error,
                "{substitution} authority must not survive mediation"
            );
            assert!(parsed.bids.is_empty());
        }
    }

    #[test]
    fn candidate_index_preserves_multiple_same_provider_slot_bids() {
        let provider = AdServerMockProvider::new(AdServerMockConfig::default());
        let response = AuctionResponse::success(
            "aps",
            vec![aps_bid("selected", 2.0), aps_bid("losing", 1.0)],
            1,
        );
        let index = build_bid_index(std::slice::from_ref(&response))
            .expect("unique candidates should build an exact index");
        assert_eq!(index.len(), 2);
        let selected_candidate_id = response.bids[0]
            .candidate_id
            .clone()
            .expect("selected bid should have candidate id");
        let mediation_request = provider
            .build_mediation_request(
                &create_test_auction_request(),
                std::slice::from_ref(&response),
            )
            .expect("should build mediation request with candidate provenance");
        assert_eq!(
            mediation_request["ext"]["bidder_responses"][0]["bids"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(
            mediation_request["ext"]["bidder_responses"][0]["bids"][0]["ext"]["trusted_server"]["candidate_id"],
            selected_candidate_id
        );

        let mediated = provider.parse_mediation_response(
            &json!({
                "seatbid": [{
                    "seat": "aps",
                    "bid": [{
                        "impid": "header-banner",
                        "price": 2.0,
                        "ext": {"trusted_server": {"candidate_id": selected_candidate_id}}
                    }]
                }]
            }),
            2,
            &index,
        );
        let winner = mediated
            .bids
            .first()
            .expect("should restore mediated APS winner");
        assert_eq!(winner.bid_id.as_deref(), Some("selected"));
        assert_eq!(
            winner
                .renderer
                .as_ref()
                .expect("should restore APS renderer")
                .as_aps()
                .expect("should be APS renderer")
                .bid_id,
            "selected"
        );
        assert!(winner.creative.is_none());
    }

    #[test]
    fn test_parse_empty_mediation_response() {
        let config = AdServerMockConfig::default();
        let provider = AdServerMockProvider::new(config);

        let mediation_response = json!({
            "id": "test-auction-123",
            "seatbid": [],
            "cur": "USD"
        });

        let auction_response =
            provider.parse_mediation_response(&mediation_response, 100, &BidIndex::new());

        assert_eq!(auction_response.status, BidStatus::NoBid);
        assert_eq!(auction_response.bids.len(), 0);
    }

    #[test]
    fn missing_seatbid_is_mediation_failure_not_no_bid() {
        let provider = AdServerMockProvider::new(AdServerMockConfig::default());

        let response = provider.parse_mediation_response(&json!({}), 100, &BidIndex::new());

        assert_eq!(response.status, BidStatus::Error);
        assert_eq!(
            response.metadata["mediation_error"],
            "invalid_candidate_provenance"
        );
    }

    #[test]
    fn test_mediation_request_handles_decoded_bid_without_creative() {
        // Typed-renderer bids retain their decoded price when sent to mediation.
        let config = AdServerMockConfig::default();
        let provider = AdServerMockProvider::new(config);

        let auction_request = AuctionRequest {
            id: "test-auction".to_string(),
            slots: vec![AdSlot {
                id: "slot-1".to_string(),
                formats: vec![AdFormat {
                    media_type: MediaType::Banner,
                    width: 300,
                    height: 250,
                }],
                floor_price: None,
                targeting: HashMap::new(),
                bidders: HashMap::new(),
            }],
            publisher: PublisherInfo {
                domain: "test.com".to_string(),
                page_url: None,
            },
            user: UserInfo {
                id: Some("user-1".to_string()),
                consent: None,
                eids: None,
            },
            device: None,
            site: None,
            context: HashMap::new(),
        };

        let bidder_responses = vec![AuctionResponse {
            provider: "aps".to_string(),
            status: BidStatus::Success,
            bids: vec![Bid {
                slot_id: "slot-1".to_string(),
                candidate_id: Some("CCCCCCCCCCCC".to_string()),
                candidate_provider: Some("aps".to_string()),
                renderer_reservation_id: None,
                price: Some(1.75),
                currency: "USD".to_string(),
                creative: None,
                width: 300,
                height: 250,
                bidder: "aps".to_string(),
                returned_seat: None,
                adomain: Some(vec!["advertiser.example".to_string()]),
                nurl: None,
                burl: None,
                bid_id: None,
                ad_id: None,
                creative_id: None,
                renderer: None,
                cache_id: None,
                cache_host: None,
                cache_path: None,
                metadata: HashMap::new(),
            }],
            response_time_ms: 100,
            metadata: HashMap::new(),
        }];

        let mediation_req = provider
            .build_mediation_request(&auction_request, &bidder_responses)
            .expect("should build request");

        // Verify the mediation request structure
        assert_eq!(mediation_req["id"], "test-auction");

        let bidder_resp = &mediation_req["ext"]["bidder_responses"][0];
        assert_eq!(bidder_resp["bidder"], "aps");

        let bid = &bidder_resp["bids"][0];
        assert_eq!(bid["imp_id"], "slot-1");
        assert_eq!(bid["ext"]["trusted_server"]["candidate_id"], "CCCCCCCCCCCC");

        assert_eq!(
            bid["price"].as_f64(),
            Some(1.75),
            "should preserve the decoded APS price"
        );
        // adm should be null (not a string)
        assert!(
            bid["adm"].is_null(),
            "Creative-less bids should have null adm, got: {:?}",
            bid["adm"]
        );
    }

    #[test]
    fn test_provider_metadata() {
        let config = AdServerMockConfig::default();
        let provider = AdServerMockProvider::new(config);

        assert_eq!(provider.provider_name(), "adserver_mock");
        assert!(!provider.is_enabled()); // Default is disabled
        assert_eq!(provider.timeout_ms(), 500);
        assert!(provider.supports_media_type(&MediaType::Banner));
        assert!(!provider.supports_media_type(&MediaType::Video));
        assert!(!provider.supports_media_type(&MediaType::Native));
    }

    #[test]
    fn test_mediation_request_includes_consent() {
        use crate::consent::ConsentContext;

        let config = AdServerMockConfig {
            enabled: true,
            endpoint: "http://localhost:6767/adserver/mediate".to_string(),
            timeout_ms: 500,
            price_floor: None,
            context_query_params: BTreeMap::new(),
        };

        let provider = AdServerMockProvider::new(config);

        let mut request = create_test_auction_request();
        request.user.consent = Some(ConsentContext {
            raw_tc_string: Some("BOEFEAyO".to_string()),
            gdpr_applies: true,
            raw_us_privacy: Some("1YNN".to_string()),
            raw_gpp_string: Some("DBACNYA~CPXxRfAPXxRfA".to_string()),
            gpp_section_ids: Some(vec![2, 6]),
            ..Default::default()
        });

        let mediation_req = provider
            .build_mediation_request(&request, &[])
            .expect("should build request");

        let consent = &mediation_req["ext"]["consent"];
        assert_eq!(consent["gdpr"], 1);
        assert_eq!(consent["consent"], "BOEFEAyO");
        assert_eq!(consent["us_privacy"], "1YNN");
        assert_eq!(consent["gpp"], "DBACNYA~CPXxRfAPXxRfA");
        assert_eq!(consent["gpp_sid"], json!([2, 6]));
    }

    #[test]
    fn test_mediation_request_no_consent() {
        let config = AdServerMockConfig::default();
        let provider = AdServerMockProvider::new(config);
        let request = create_test_auction_request(); // consent is None

        let mediation_req = provider
            .build_mediation_request(&request, &[])
            .expect("should build request");

        assert!(
            mediation_req["ext"]["consent"].is_null(),
            "consent should be null when no consent context"
        );
    }

    #[test]
    fn malformed_mediator_selection_fails_closed() {
        let config = AdServerMockConfig::default();
        let provider = AdServerMockProvider::new(config);

        let mediation_response = json!({
            "id": "test-auction-123",
            "seatbid": [
                {
                    "seat": "test-bidder",
                    "bid": [
                        {
                            "id": "bid-001",
                            "impid": "header-banner",
                            "price": 3.50,
                            "adm": "<div>Valid Ad</div>",
                            "w": 728,
                            "h": 90,
                        },
                        {
                            "id": "bid-002",
                            "impid": "sidebar",
                            // Note: No "price" field - mediation failed to decode
                            "adm": "<div>Failed decode</div>",
                            "w": 300,
                            "h": 250,
                        }
                    ]
                }
            ],
            "cur": "USD"
        });

        let auction_response =
            provider.parse_mediation_response(&mediation_response, 200, &BidIndex::new());

        assert_eq!(auction_response.status, BidStatus::Error);
        assert!(auction_response.bids.is_empty());
        assert_eq!(
            auction_response.metadata["mediation_error"],
            "invalid_candidate_provenance"
        );
    }

    #[test]
    fn test_build_endpoint_url_with_context_query_params() {
        let config = AdServerMockConfig {
            enabled: true,
            endpoint: "http://localhost:6767/adserver/mediate".to_string(),
            timeout_ms: 500,
            price_floor: None,
            context_query_params: BTreeMap::from([(
                "permutive_segments".to_string(),
                "permutive".to_string(),
            )]),
        };
        let provider = AdServerMockProvider::new(config);

        let mut request = create_test_auction_request();
        request.context.insert(
            "permutive_segments".to_string(),
            ContextValue::StringList(vec![
                "10000001".into(),
                "10000003".into(),
                "adv".into(),
                "bhgp".into(),
            ]),
        );

        let url = provider.build_endpoint_url(&request);
        assert_eq!(
            url,
            "http://localhost:6767/adserver/mediate?permutive=10000001%2C10000003%2Cadv%2Cbhgp"
        );
    }

    #[test]
    fn test_build_endpoint_url_no_mapping_no_params() {
        // With an empty context_query_params, no query params are appended
        // even if context contains data.
        let config = AdServerMockConfig {
            enabled: true,
            endpoint: "http://localhost:6767/adserver/mediate".to_string(),
            timeout_ms: 500,
            price_floor: None,
            context_query_params: BTreeMap::new(),
        };
        let provider = AdServerMockProvider::new(config);

        let mut request = create_test_auction_request();
        request.context.insert(
            "permutive_segments".to_string(),
            ContextValue::StringList(vec!["10000001".into()]),
        );

        let url = provider.build_endpoint_url(&request);
        assert_eq!(url, "http://localhost:6767/adserver/mediate");
    }

    #[test]
    fn test_build_endpoint_url_empty_array_skipped() {
        let config = AdServerMockConfig {
            context_query_params: BTreeMap::from([(
                "permutive_segments".to_string(),
                "permutive".to_string(),
            )]),
            ..Default::default()
        };
        let provider = AdServerMockProvider::new(config);

        let mut request = create_test_auction_request();
        request.context.insert(
            "permutive_segments".to_string(),
            ContextValue::StringList(vec![]),
        );

        let url = provider.build_endpoint_url(&request);
        assert!(
            !url.contains("permutive="),
            "Empty segments should not add query param"
        );
    }

    #[test]
    fn test_build_endpoint_url_preserves_existing_query_params() {
        let config = AdServerMockConfig {
            enabled: true,
            endpoint: "http://localhost:6767/adserver/mediate?debug=true".to_string(),
            timeout_ms: 500,
            price_floor: None,
            context_query_params: BTreeMap::from([(
                "permutive_segments".to_string(),
                "permutive".to_string(),
            )]),
        };
        let provider = AdServerMockProvider::new(config);

        let mut request = create_test_auction_request();
        request.context.insert(
            "permutive_segments".to_string(),
            ContextValue::StringList(vec!["123".into(), "adv".into()]),
        );

        let url = provider.build_endpoint_url(&request);
        assert_eq!(
            url,
            "http://localhost:6767/adserver/mediate?debug=true&permutive=123%2Cadv"
        );
    }
}
