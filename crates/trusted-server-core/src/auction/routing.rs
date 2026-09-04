//! Internal admission normalization and config-first provider routing.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::IpAddr;

use edgezero_core::body::Body as EdgeBody;
use http::{HeaderValue, Request, header};
use serde_json::Value;

use super::plan::{AuctionPlan, BidderId, ProviderId, RoutingMode};
use super::types::{AdSlot, AuctionRequest, MediaType};

const TRUSTED_SERVER_ENVELOPE: &str = "trustedServer";
const BIDDER_PARAMS_FIELD: &str = "bidderParams";
const ZONE_FIELD: &str = "zone";

/// Maximum bidder entries admitted from one browser `bidderParams` envelope.
pub(crate) const MAX_BIDDER_ENTRIES: usize = 128;
/// Maximum UTF-8 byte length of the optional Prebid zone fact.
pub(crate) const MAX_PREBID_ZONE_BYTES: usize = 256;

/// Immutable provider-local routing output in deterministic provider-ID order.
#[derive(Debug, Clone)]
pub(crate) struct RoutedAuction {
    inputs: Vec<ProviderAuctionInput>,
    skipped_no_eligible_provider_ids: Vec<ProviderId>,
    diagnostics: RoutingDiagnostics,
    transport_headers: PrebidTransportHeaders,
    attested_client_ip: Option<IpAddr>,
    dnt: Option<bool>,
}

impl RoutedAuction {
    pub(crate) fn inputs(&self) -> &[ProviderAuctionInput] {
        &self.inputs
    }

    /// Providers skipped because they had no eligible banner slots.
    ///
    /// IDs retain the compiled plan's deterministic provider-ID order.
    pub(crate) fn skipped_no_eligible_provider_ids(&self) -> &[ProviderId] {
        &self.skipped_no_eligible_provider_ids
    }

    pub(crate) fn diagnostics(&self) -> RoutingDiagnostics {
        self.diagnostics
    }

    /// Request headers approved for later Prebid transport forwarding.
    pub(crate) fn prebid_transport_headers(&self) -> &PrebidTransportHeaders {
        &self.transport_headers
    }

    /// Platform-attested client IP for transport forwarding.
    pub(crate) fn attested_client_ip(&self) -> Option<IpAddr> {
        self.attested_client_ip
    }

    /// Normalized Do Not Track fact; raw request headers are not exposed to profiles.
    pub(crate) fn dnt(&self) -> Option<bool> {
        self.dnt
    }
}

/// Saturating, count-only routing diagnostics.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub(crate) struct RoutingDiagnostics {
    unroutable_bidder_count: u32,
    malformed_envelope_count: u32,
    malformed_direct_demand_count: u32,
    unroutable_trusted_provider_count: u32,
}

impl RoutingDiagnostics {
    pub(crate) fn unroutable_bidder_count(self) -> u32 {
        self.unroutable_bidder_count
    }

    #[cfg(test)]
    pub(crate) fn malformed_envelope_count(self) -> u32 {
        self.malformed_envelope_count
    }

    #[cfg(test)]
    pub(crate) fn malformed_direct_demand_count(self) -> u32 {
        self.malformed_direct_demand_count
    }

    #[cfg(test)]
    pub(crate) fn unroutable_trusted_provider_count(self) -> u32 {
        self.unroutable_trusted_provider_count
    }

    fn record_unroutable_bidder(&mut self) {
        self.unroutable_bidder_count = self.unroutable_bidder_count.saturating_add(1);
    }

    fn record_malformed_envelope(&mut self) {
        self.malformed_envelope_count = self.malformed_envelope_count.saturating_add(1);
    }

    fn record_malformed_direct_demand(&mut self) {
        self.malformed_direct_demand_count = self.malformed_direct_demand_count.saturating_add(1);
    }

    fn record_unroutable_trusted_provider(&mut self) {
        self.unroutable_trusted_provider_count =
            self.unroutable_trusted_provider_count.saturating_add(1);
    }

    #[cfg(test)]
    pub(crate) fn saturated_for_test() -> Self {
        let mut diagnostics = Self {
            unroutable_bidder_count: u32::MAX,
            ..Self::default()
        };
        diagnostics.record_unroutable_bidder();
        diagnostics
    }
}

/// Provider-local immutable auction input.
#[derive(Debug, Clone)]
pub(crate) struct ProviderAuctionInput {
    provider_id: ProviderId,
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "retained in routed input to pin the provider budget invariant"
        )
    )]
    timeout_ms: u32,
    common_request: AuctionRequest,
    slots: Vec<ProviderSlotInput>,
}

impl ProviderAuctionInput {
    pub(crate) fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    #[cfg(test)]
    pub(crate) fn timeout_ms(&self) -> u32 {
        self.timeout_ms
    }

    /// Common privacy-approved request data. Its slot list is always empty.
    pub(crate) fn common_request(&self) -> &AuctionRequest {
        &self.common_request
    }

    pub(crate) fn slots(&self) -> &[ProviderSlotInput] {
        &self.slots
    }
}

/// One eligible slot with only the demand assigned to this provider.
#[derive(Debug, Clone)]
pub(crate) struct ProviderSlotInput {
    slot: AdSlot,
    bidder_params: BTreeMap<BidderId, Value>,
    prebid_zone: Option<String>,
    trusted_stored_request: bool,
}

impl ProviderSlotInput {
    /// Common slot facts. The legacy `bidders` map is always empty.
    pub(crate) fn slot(&self) -> &AdSlot {
        &self.slot
    }

    pub(crate) fn bidder_params(&self) -> &BTreeMap<BidderId, Value> {
        &self.bidder_params
    }

    pub(crate) fn prebid_zone(&self) -> Option<&str> {
        self.prebid_zone.as_deref()
    }

    pub(crate) fn has_trusted_stored_request(&self) -> bool {
        self.trusted_stored_request
    }
}

/// Request headers approved for later Prebid transport forwarding.
///
/// Values remain as raw [`HeaderValue`] instances so non-ASCII bytes retain
/// the same legacy handling. Client-supplied `X-Forwarded-For` is never read.
#[derive(Debug, Clone, Default)]
pub(crate) struct PrebidTransportHeaders {
    cookie: Option<HeaderValue>,
    user_agent: Option<HeaderValue>,
    referer: Option<HeaderValue>,
    accept_language: Option<HeaderValue>,
}

impl PrebidTransportHeaders {
    pub(crate) fn cookie(&self) -> Option<&HeaderValue> {
        self.cookie.as_ref()
    }

    pub(crate) fn user_agent(&self) -> Option<&HeaderValue> {
        self.user_agent.as_ref()
    }

    pub(crate) fn referer(&self) -> Option<&HeaderValue> {
        self.referer.as_ref()
    }

    pub(crate) fn accept_language(&self) -> Option<&HeaderValue> {
        self.accept_language.as_ref()
    }

    fn snapshot(request: &Request<EdgeBody>) -> Self {
        Self {
            cookie: request.headers().get(header::COOKIE).cloned(),
            user_agent: request.headers().get(header::USER_AGENT).cloned(),
            referer: request.headers().get(header::REFERER).cloned(),
            accept_language: request.headers().get(header::ACCEPT_LANGUAGE).cloned(),
        }
    }
}

/// Server-owned explicit provider routes aligned with canonical auction slots.
///
/// This internal-only type has no deserializer, so browser input cannot select
/// a provider ID. The caller supplies one route list for each request slot.
#[derive(Debug, Default)]
pub(crate) struct TrustedProviderRoutes {
    routes_by_slot: Vec<Vec<ProviderId>>,
}

impl TrustedProviderRoutes {
    #[cfg(test)]
    pub(crate) fn new(routes_by_slot: Vec<Vec<ProviderId>>) -> Self {
        Self { routes_by_slot }
    }

    fn for_slot(&self, slot_index: usize) -> &[ProviderId] {
        self.routes_by_slot
            .get(slot_index)
            .map_or(&[], Vec::as_slice)
    }
}

#[derive(Debug, Default)]
struct NormalizedSlotDemand {
    bidder_params: BTreeMap<BidderId, Value>,
    stored_request: bool,
    prebid_zone: Option<String>,
}

#[derive(Debug)]
struct ProviderInputBuilder {
    provider_id: ProviderId,
    timeout_ms: u32,
    is_prebid: bool,
    routing: RoutingMode,
    slots: Vec<ProviderSlotInput>,
}

/// Normalize admitted demand and build deterministic provider-local inputs.
///
/// This helper consumes the canonical request so the common request retained by
/// each provider can be scrubbed of slots. It performs no I/O.
pub(crate) fn route_auction(
    request: AuctionRequest,
    inbound_request: &Request<EdgeBody>,
    plan: &AuctionPlan,
    attested_client_ip: Option<IpAddr>,
) -> RoutedAuction {
    route_auction_with_trusted_routes(
        request,
        inbound_request,
        plan,
        attested_client_ip,
        &TrustedProviderRoutes::default(),
    )
}

/// Route an auction with server-owned explicit provider routes.
///
/// Only server-generated entry points may construct [`TrustedProviderRoutes`].
/// Browser/default admission must use [`route_auction`].
pub(crate) fn route_auction_with_trusted_routes(
    mut request: AuctionRequest,
    inbound_request: &Request<EdgeBody>,
    plan: &AuctionPlan,
    attested_client_ip: Option<IpAddr>,
    trusted_routes: &TrustedProviderRoutes,
) -> RoutedAuction {
    let slots = std::mem::take(&mut request.slots);
    let common_request = request;
    let transport_headers = PrebidTransportHeaders::snapshot(inbound_request);
    let dnt = inbound_request
        .headers()
        .get("dnt")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim() == "1")
        .then_some(true);
    let mut diagnostics = RoutingDiagnostics::default();
    let mut builders = plan
        .providers()
        .iter()
        .map(|provider| ProviderInputBuilder {
            provider_id: provider.id.clone(),
            timeout_ms: provider.timeout_ms,
            is_prebid: provider.profile.is_prebid_server(),
            routing: provider.routing,
            slots: Vec::new(),
        })
        .collect::<Vec<_>>();
    let provider_indices = builders
        .iter()
        .enumerate()
        .map(|(index, provider)| (provider.provider_id.clone(), index))
        .collect::<BTreeMap<_, _>>();

    for (slot_index, slot) in slots.into_iter().enumerate() {
        let Some(common_slot) = eligible_banner_slot(&slot) else {
            continue;
        };
        let demand = normalize_slot_demand(&slot.bidders, &mut diagnostics);
        let mut routed_params = vec![BTreeMap::new(); builders.len()];
        for (bidder, params) in demand.bidder_params {
            let Some(provider) = plan.provider_for_bidder(&bidder) else {
                diagnostics.record_unroutable_bidder();
                continue;
            };
            let provider_index = *provider_indices
                .get(&provider.id)
                .expect("should resolve compiled provider index");
            routed_params[provider_index].insert(bidder, params);
        }
        let mut trusted_provider_indices = BTreeSet::new();
        for provider_id in trusted_routes.for_slot(slot_index) {
            let Some(provider_index) = provider_indices.get(provider_id).copied() else {
                diagnostics.record_unroutable_trusted_provider();
                continue;
            };
            trusted_provider_indices.insert(provider_index);
        }

        for (provider_index, builder) in builders.iter_mut().enumerate() {
            let bidder_params = std::mem::take(&mut routed_params[provider_index]);
            let trusted_route = trusted_provider_indices.contains(&provider_index);
            let trusted_stored_request =
                builder.is_prebid && demand.stored_request && bidder_params.is_empty();
            let include = builder.routing == RoutingMode::AllEligible
                || !bidder_params.is_empty()
                || trusted_stored_request
                || trusted_route;
            if !include {
                continue;
            }
            builder.slots.push(ProviderSlotInput {
                slot: common_slot.clone(),
                bidder_params,
                prebid_zone: builder
                    .is_prebid
                    .then(|| demand.prebid_zone.clone())
                    .flatten(),
                trusted_stored_request,
            });
        }
    }

    let mut skipped_no_eligible_provider_ids = Vec::new();
    let inputs = builders
        .into_iter()
        .filter_map(|builder| {
            if builder.slots.is_empty() {
                skipped_no_eligible_provider_ids.push(builder.provider_id);
                return None;
            }
            Some(ProviderAuctionInput {
                provider_id: builder.provider_id,
                timeout_ms: builder.timeout_ms,
                common_request: common_request.clone(),
                slots: builder.slots,
            })
        })
        .collect();

    RoutedAuction {
        inputs,
        skipped_no_eligible_provider_ids,
        diagnostics,
        transport_headers,
        attested_client_ip,
        dnt,
    }
}

fn eligible_banner_slot(slot: &AdSlot) -> Option<AdSlot> {
    let formats = slot
        .formats
        .iter()
        .filter(|format| {
            format.media_type == MediaType::Banner
                && i32::try_from(format.width).is_ok_and(|width| width > 0)
                && i32::try_from(format.height).is_ok_and(|height| height > 0)
        })
        .cloned()
        .collect::<Vec<_>>();
    if formats.is_empty() {
        return None;
    }
    Some(AdSlot {
        id: slot.id.clone(),
        formats,
        floor_price: slot.floor_price,
        targeting: slot.targeting.clone(),
        bidders: HashMap::new(),
    })
}

fn normalize_slot_demand(
    bidders: &HashMap<String, Value>,
    diagnostics: &mut RoutingDiagnostics,
) -> NormalizedSlotDemand {
    if bidders.is_empty() {
        return NormalizedSlotDemand {
            stored_request: true,
            ..Default::default()
        };
    }

    let mut demand = NormalizedSlotDemand::default();
    if let Some(envelope) = bidders.get(TRUSTED_SERVER_ENVELOPE) {
        match normalize_envelope(envelope) {
            Some(normalized) => demand = normalized,
            None => diagnostics.record_malformed_envelope(),
        }
    }

    let mut direct = bidders
        .iter()
        .filter(|(key, _)| key.as_str() != TRUSTED_SERVER_ENVELOPE)
        .collect::<Vec<_>>();
    direct.sort_by_key(|(left, _)| *left);
    for (raw_bidder, params) in direct {
        let Ok(bidder) = raw_bidder.parse::<BidderId>() else {
            diagnostics.record_malformed_direct_demand();
            continue;
        };
        if bidder.as_str() == TRUSTED_SERVER_ENVELOPE || !is_usable_params(params) {
            diagnostics.record_malformed_direct_demand();
            continue;
        }
        demand.bidder_params.insert(bidder, params.clone());
    }
    demand
}

fn normalize_envelope(envelope: &Value) -> Option<NormalizedSlotDemand> {
    let object = envelope.as_object()?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), BIDDER_PARAMS_FIELD | ZONE_FIELD))
    {
        return None;
    }
    let prebid_zone = match object.get(ZONE_FIELD) {
        None => None,
        Some(Value::String(zone)) if zone.len() <= MAX_PREBID_ZONE_BYTES => Some(zone.clone()),
        Some(_) => return None,
    };
    let Some(raw_params) = object.get(BIDDER_PARAMS_FIELD) else {
        return Some(NormalizedSlotDemand {
            stored_request: true,
            prebid_zone,
            ..Default::default()
        });
    };
    if raw_params.is_null() {
        return Some(NormalizedSlotDemand {
            stored_request: true,
            prebid_zone,
            ..Default::default()
        });
    }
    let params = raw_params.as_object()?;
    if params.is_empty() {
        return Some(NormalizedSlotDemand {
            stored_request: true,
            prebid_zone,
            ..Default::default()
        });
    }
    if params.len() > MAX_BIDDER_ENTRIES {
        return None;
    }

    let mut bidder_params = BTreeMap::new();
    for (raw_bidder, value) in params {
        let bidder = raw_bidder.parse::<BidderId>().ok()?;
        if bidder.as_str() == TRUSTED_SERVER_ENVELOPE || !value.is_object() {
            return None;
        }
        bidder_params.insert(bidder, value.clone());
    }
    Some(NormalizedSlotDemand {
        bidder_params,
        stored_request: false,
        prebid_zone,
    })
}

fn is_usable_params(value: &Value) -> bool {
    value.as_object().is_some_and(|object| !object.is_empty())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use super::*;
    use crate::auction::plan::{
        AuctionPlanConfig, BidderRouteConfig, NotificationConfig, ProviderConfig,
    };
    use crate::auction::types::{AdFormat, DeviceInfo, PublisherInfo, SiteInfo, UserInfo};
    use http::HeaderName;
    use serde_json::{Map, json};

    fn provider(profile: &str, routing: RoutingMode) -> ProviderConfig {
        ProviderConfig {
            protocol: "openrtb-2.6".to_string(),
            profile: profile.to_string(),
            endpoint: format!("https://{profile}.example.test/openrtb"),
            timeout_ms: None,
            routing,
            notifications: NotificationConfig::default(),
            profile_config: if profile == "aps" {
                json!({"account_id": "example-account"})
            } else {
                json!({})
            },
        }
    }

    fn plan() -> AuctionPlan {
        AuctionPlan::compile(AuctionPlanConfig {
            timeout_ms: 900,
            providers: BTreeMap::from([
                (
                    ProviderId::from_str("aps-primary").expect("should parse provider"),
                    provider("aps", RoutingMode::AllEligible),
                ),
                (
                    ProviderId::from_str("pbs-a").expect("should parse provider"),
                    provider("prebid-server", RoutingMode::Explicit),
                ),
                (
                    ProviderId::from_str("pbs-b").expect("should parse provider"),
                    provider("prebid-server", RoutingMode::Explicit),
                ),
                (
                    ProviderId::from_str("standard-direct").expect("should parse provider"),
                    provider("standard", RoutingMode::Explicit),
                ),
            ]),
            bidders: BTreeMap::from([
                (
                    BidderId::from_str("alpha").expect("should parse bidder"),
                    BidderRouteConfig {
                        provider: ProviderId::from_str("pbs-a").expect("should parse provider"),
                    },
                ),
                (
                    BidderId::from_str("beta").expect("should parse bidder"),
                    BidderRouteConfig {
                        provider: ProviderId::from_str("standard-direct")
                            .expect("should parse provider"),
                    },
                ),
            ]),
            mediator: None,
            request_signing: None,
        })
        .expect("should compile plan")
    }

    fn explicit_plan() -> AuctionPlan {
        AuctionPlan::compile(AuctionPlanConfig {
            timeout_ms: 900,
            providers: BTreeMap::from([
                (
                    ProviderId::from_str("aps-primary").expect("should parse provider"),
                    provider("aps", RoutingMode::Explicit),
                ),
                (
                    ProviderId::from_str("pbs-a").expect("should parse provider"),
                    provider("prebid-server", RoutingMode::Explicit),
                ),
                (
                    ProviderId::from_str("pbs-b").expect("should parse provider"),
                    provider("prebid-server", RoutingMode::Explicit),
                ),
                (
                    ProviderId::from_str("standard-direct").expect("should parse provider"),
                    provider("standard", RoutingMode::Explicit),
                ),
            ]),
            bidders: BTreeMap::new(),
            mediator: None,
            request_signing: None,
        })
        .expect("should compile explicit plan")
    }

    fn slot(bidders: HashMap<String, Value>) -> AdSlot {
        AdSlot {
            id: "slot-1".to_string(),
            formats: vec![AdFormat {
                media_type: MediaType::Banner,
                width: 300,
                height: 250,
            }],
            floor_price: Some(0.5),
            targeting: HashMap::new(),
            bidders,
        }
    }

    fn request(slots: Vec<AdSlot>) -> AuctionRequest {
        AuctionRequest {
            id: "auction-1".to_string(),
            slots,
            publisher: PublisherInfo {
                domain: "publisher.example.test".to_string(),
                page_url: Some("https://publisher.example.test/article".to_string()),
            },
            user: UserInfo {
                id: None,
                consent: None,
                eids: None,
            },
            device: Some(DeviceInfo {
                user_agent: None,
                ip: None,
                geo: None,
            }),
            site: Some(SiteInfo {
                domain: "publisher.example.test".to_string(),
                page: "https://publisher.example.test/article".to_string(),
            }),
            context: HashMap::new(),
        }
    }

    fn inbound() -> Request<EdgeBody> {
        Request::builder()
            .uri("https://publisher.example.test/auction")
            .body(EdgeBody::empty())
            .expect("should build request")
    }

    fn envelope(bidder_params: Option<Value>) -> Value {
        let mut value = Map::new();
        if let Some(params) = bidder_params {
            value.insert(BIDDER_PARAMS_FIELD.to_string(), params);
        }
        Value::Object(value)
    }

    fn input<'a>(routed: &'a RoutedAuction, provider_id: &str) -> &'a ProviderAuctionInput {
        routed
            .inputs()
            .iter()
            .find(|input| input.provider_id().as_str() == provider_id)
            .expect("should find provider input")
    }

    #[test]
    fn missing_null_and_empty_envelope_params_fan_out_stored_routes() {
        let cases = [
            ("missing", envelope(None)),
            ("null", envelope(Some(Value::Null))),
            ("empty", envelope(Some(json!({})))),
        ];
        for (name, trusted_server) in cases {
            let routed = route_auction(
                request(vec![slot(HashMap::from([(
                    TRUSTED_SERVER_ENVELOPE.to_string(),
                    trusted_server,
                )]))]),
                &inbound(),
                &plan(),
                None,
            );
            let ids = routed
                .inputs()
                .iter()
                .map(|input| input.provider_id().as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                ids,
                vec!["aps-primary", "pbs-a", "pbs-b"],
                "{name} should fan out to both PBS providers while APS remains all-eligible"
            );
            assert!(
                input(&routed, "pbs-a").slots()[0].has_trusted_stored_request(),
                "{name} should create stored intent"
            );
            assert!(
                input(&routed, "pbs-b").slots()[0].has_trusted_stored_request(),
                "{name} should create stored intent for every PBS provider"
            );
        }
    }

    #[test]
    fn entirely_empty_bidder_map_is_trusted_stored_intent() {
        let routed = route_auction(
            request(vec![slot(HashMap::new())]),
            &inbound(),
            &plan(),
            None,
        );
        assert!(
            input(&routed, "pbs-a").slots()[0].has_trusted_stored_request(),
            "empty canonical demand should preserve stored-request behavior"
        );
        assert!(
            input(&routed, "pbs-b").slots()[0].has_trusted_stored_request(),
            "empty canonical demand should fan out to same-profile PBS plans"
        );
    }

    #[test]
    fn malformed_envelopes_are_atomic_and_do_not_trigger_stored_routes() {
        let too_many = Value::Object(
            (0..=MAX_BIDDER_ENTRIES)
                .map(|index| (format!("bidder-{index}"), json!({"placement": index})))
                .collect(),
        );
        let cases = vec![
            ("nonobject envelope", json!("bad")),
            ("nonobject params", envelope(Some(json!(true)))),
            ("invalid key", envelope(Some(json!({" bad": {"x": 1}})))),
            (
                "reserved key",
                envelope(Some(json!({"trustedServer": {"x": 1}}))),
            ),
            ("nonobject value", envelope(Some(json!({"alpha": 1})))),
            (
                "partial",
                envelope(Some(json!({"alpha": {"x": 1}, "beta": null}))),
            ),
            (
                "unknown field",
                json!({"bidderParams": {"alpha": {"x": 1}}, "endpoint": "https://bad.example"}),
            ),
            ("too many bidders", envelope(Some(too_many))),
            (
                "oversized zone",
                json!({"bidderParams": {}, "zone": "z".repeat(MAX_PREBID_ZONE_BYTES + 1)}),
            ),
            ("nonstring zone", json!({"bidderParams": {}, "zone": 1})),
        ];
        for (name, malformed) in cases {
            let bidders = HashMap::from([
                (TRUSTED_SERVER_ENVELOPE.to_string(), malformed),
                ("beta".to_string(), json!({"placement": "direct"})),
            ]);
            let routed = route_auction(request(vec![slot(bidders)]), &inbound(), &plan(), None);
            assert_eq!(
                routed.diagnostics().malformed_envelope_count(),
                1,
                "{name} should record one malformed envelope"
            );
            assert!(
                routed.inputs().iter().all(|provider| {
                    provider.provider_id().as_str() != "pbs-a"
                        && provider.provider_id().as_str() != "pbs-b"
                }),
                "{name} should not produce stored or inline PBS demand"
            );
            assert_eq!(
                input(&routed, "standard-direct").slots()[0]
                    .bidder_params()
                    .len(),
                1,
                "{name} should preserve independent valid direct demand"
            );
            assert!(
                routed
                    .inputs()
                    .iter()
                    .any(|provider| provider.provider_id().as_str() == "aps-primary"),
                "{name} should preserve independent all-eligible participation"
            );
        }
    }

    #[test]
    fn envelope_preserves_empty_bidder_params_without_rejecting_valid_siblings() {
        let normalized = normalize_envelope(&envelope(Some(json!({
            "alpha": {},
            "beta": {"placement": 42}
        }))))
        .expect("should preserve object-valued bidder params");

        assert_eq!(
            normalized
                .bidder_params
                .get(&BidderId::from_str("alpha").expect("should parse bidder")),
            Some(&json!({})),
            "should preserve empty params for profile overrides"
        );
        assert_eq!(
            normalized
                .bidder_params
                .get(&BidderId::from_str("beta").expect("should parse bidder")),
            Some(&json!({"placement": 42})),
            "should preserve valid sibling params"
        );
    }

    #[test]
    fn exact_envelope_bidder_entry_bound_is_accepted_and_next_entry_is_rejected() {
        let accepted = Value::Object(
            (0..MAX_BIDDER_ENTRIES)
                .map(|index| (format!("bidder-{index}"), json!({"placement": index})))
                .collect(),
        );
        let rejected = Value::Object(
            (0..=MAX_BIDDER_ENTRIES)
                .map(|index| (format!("bidder-{index}"), json!({"placement": index})))
                .collect(),
        );
        assert!(
            normalize_envelope(&envelope(Some(accepted))).is_some(),
            "exactly 128 envelope bidder entries should be admitted"
        );
        assert!(
            normalize_envelope(&envelope(Some(rejected))).is_none(),
            "129 envelope bidder entries should be rejected"
        );
    }

    #[test]
    fn unknown_bidder_is_counted_without_fallback() {
        let routed = route_auction(
            request(vec![slot(HashMap::from([(
                TRUSTED_SERVER_ENVELOPE.to_string(),
                envelope(Some(json!({"unknown": {"placement": 1}}))),
            )]))]),
            &inbound(),
            &plan(),
            None,
        );
        assert_eq!(
            routed.diagnostics().unroutable_bidder_count(),
            1,
            "unknown bidder should increment bounded diagnostics"
        );
        assert_eq!(
            routed.inputs().len(),
            1,
            "only all-eligible APS should remain"
        );
        assert_eq!(
            routed.inputs()[0].provider_id().as_str(),
            "aps-primary",
            "unknown demand should not cause PBS fallback"
        );
    }

    #[test]
    fn direct_usable_params_win_collision_and_unusable_direct_does_not_overwrite() {
        let cases = [
            ("usable direct", json!({"source": "direct"}), "direct", 0),
            ("empty direct", json!({}), "envelope", 1),
            ("null direct", Value::Null, "envelope", 1),
        ];
        for (name, direct, expected_source, malformed_count) in cases {
            let routed = route_auction(
                request(vec![slot(HashMap::from([
                    (
                        TRUSTED_SERVER_ENVELOPE.to_string(),
                        envelope(Some(json!({"alpha": {"source": "envelope"}}))),
                    ),
                    ("alpha".to_string(), direct),
                ]))]),
                &inbound(),
                &plan(),
                None,
            );
            assert_eq!(
                input(&routed, "pbs-a").slots()[0].bidder_params()
                    [&BidderId::from_str("alpha").expect("should parse bidder")]["source"],
                expected_source,
                "{name} should follow deterministic collision semantics"
            );
            assert_eq!(
                routed.diagnostics().malformed_direct_demand_count(),
                malformed_count,
                "{name} should record only unusable direct demand"
            );
        }
    }

    #[test]
    fn hash_map_insertion_order_does_not_change_routing() {
        let entries = [
            ("alpha".to_string(), json!({"a": 1})),
            ("unknown".to_string(), json!({"u": 1})),
            (
                TRUSTED_SERVER_ENVELOPE.to_string(),
                envelope(Some(json!({"alpha": {"a": 0}}))),
            ),
        ];
        let forward = HashMap::from(entries.clone());
        let reverse = entries.into_iter().rev().collect::<HashMap<_, _>>();
        let first = route_auction(request(vec![slot(forward)]), &inbound(), &plan(), None);
        let second = route_auction(request(vec![slot(reverse)]), &inbound(), &plan(), None);
        let summarize = |routed: &RoutedAuction| {
            routed
                .inputs()
                .iter()
                .map(|provider| {
                    (
                        provider.provider_id().as_str().to_string(),
                        provider.slots()[0]
                            .bidder_params()
                            .keys()
                            .map(|bidder| bidder.as_str().to_string())
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(summarize(&first), summarize(&second));
        assert_eq!(first.diagnostics(), second.diagnostics());
    }

    #[test]
    fn mixed_routing_filters_params_per_provider_and_inline_wins_over_stored() {
        let routed = route_auction(
            request(vec![slot(HashMap::from([
                (
                    TRUSTED_SERVER_ENVELOPE.to_string(),
                    json!({"zone": "home", "bidderParams": null}),
                ),
                ("alpha".to_string(), json!({"placement": "pbs"})),
                ("beta".to_string(), json!({"placement": "direct"})),
            ]))]),
            &inbound(),
            &plan(),
            None,
        );
        let ids = routed
            .inputs()
            .iter()
            .map(|provider| provider.provider_id().as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["aps-primary", "pbs-a", "pbs-b", "standard-direct"],
            "inputs should follow deterministic provider-ID order"
        );
        let aps = input(&routed, "aps-primary")
            .slots()
            .first()
            .expect("should have slot");
        assert!(
            aps.bidder_params().is_empty(),
            "APS must receive no foreign params"
        );
        assert_eq!(aps.prebid_zone(), None, "APS must receive no Prebid zone");
        let pbs_a = &input(&routed, "pbs-a").slots()[0];
        assert_eq!(pbs_a.bidder_params().len(), 1);
        assert!(
            !pbs_a.has_trusted_stored_request(),
            "inline params should win for this PBS provider"
        );
        assert_eq!(pbs_a.prebid_zone(), Some("home"));
        let pbs_b = &input(&routed, "pbs-b").slots()[0];
        assert!(pbs_b.bidder_params().is_empty());
        assert!(pbs_b.has_trusted_stored_request());
        let direct = &input(&routed, "standard-direct").slots()[0];
        assert_eq!(direct.bidder_params().len(), 1);
        assert!(direct.prebid_zone().is_none());
        for provider in routed.inputs() {
            assert!(provider.common_request().slots.is_empty());
            assert!(provider.slots()[0].slot().bidders.is_empty());
        }
    }

    #[test]
    fn nonbanner_and_zero_sized_formats_are_removed_and_empty_slots_are_omitted() {
        let mut mixed = slot(HashMap::new());
        mixed.formats = vec![
            AdFormat {
                media_type: MediaType::Video,
                width: 640,
                height: 360,
            },
            AdFormat {
                media_type: MediaType::Banner,
                width: 0,
                height: 250,
            },
            AdFormat {
                media_type: MediaType::Banner,
                width: u32::MAX,
                height: 250,
            },
            AdFormat {
                media_type: MediaType::Banner,
                width: 300,
                height: 250,
            },
        ];
        let mut invalid = slot(HashMap::new());
        invalid.id = "invalid".to_string();
        invalid.formats = vec![
            AdFormat {
                media_type: MediaType::Native,
                width: 1,
                height: 1,
            },
            AdFormat {
                media_type: MediaType::Banner,
                width: 300,
                height: 0,
            },
        ];
        let routed = route_auction(request(vec![mixed, invalid]), &inbound(), &plan(), None);
        assert_eq!(
            routed.inputs().len(),
            3,
            "APS and two PBS providers should receive the eligible slot"
        );
        for provider in routed.inputs() {
            assert_eq!(provider.slots().len(), 1);
            assert_eq!(provider.slots()[0].slot().id, "slot-1");
            assert_eq!(provider.slots()[0].slot().formats.len(), 1);
            assert_eq!(provider.slots()[0].slot().formats[0].width, 300);
        }
        let none = route_auction(
            request(vec![slot_with_formats(vec![AdFormat {
                media_type: MediaType::Video,
                width: 640,
                height: 360,
            }])]),
            &inbound(),
            &plan(),
            None,
        );
        assert!(
            none.inputs().is_empty(),
            "no eligible slots should omit every provider input"
        );
        assert_eq!(
            none.skipped_no_eligible_provider_ids()
                .iter()
                .map(ProviderId::as_str)
                .collect::<Vec<_>>(),
            vec!["aps-primary", "pbs-a", "pbs-b", "standard-direct"],
            "no-banner auction should retain every provider's deterministic skip outcome"
        );
    }

    #[test]
    fn explicit_no_demand_retains_deterministic_skip_outcomes() {
        let routed = route_auction(
            request(vec![slot(HashMap::from([(
                "unknown".to_string(),
                json!({"placement": "none"}),
            )]))]),
            &inbound(),
            &plan(),
            None,
        );
        assert_eq!(
            routed
                .skipped_no_eligible_provider_ids()
                .iter()
                .map(ProviderId::as_str)
                .collect::<Vec<_>>(),
            vec!["pbs-a", "pbs-b", "standard-direct"],
            "explicit providers with no routed demand should be retained as skipped"
        );
    }

    #[test]
    fn trusted_routes_admit_explicit_aps_and_standard_and_ignore_unknown_provider() {
        let trusted_routes = TrustedProviderRoutes::new(vec![vec![
            ProviderId::from_str("aps-primary").expect("should parse provider"),
            ProviderId::from_str("standard-direct").expect("should parse provider"),
            ProviderId::from_str("unknown-provider").expect("should parse provider"),
        ]]);
        let routed = route_auction_with_trusted_routes(
            request(vec![slot(HashMap::from([(
                "unknown".to_string(),
                json!({"placement": "none"}),
            )]))]),
            &inbound(),
            &explicit_plan(),
            None,
            &trusted_routes,
        );
        assert_eq!(
            routed
                .inputs()
                .iter()
                .map(|input| input.provider_id().as_str())
                .collect::<Vec<_>>(),
            vec!["aps-primary", "standard-direct"],
            "only known server-owned provider routes should admit explicit providers"
        );
        assert_eq!(
            routed.diagnostics().unroutable_trusted_provider_count(),
            1,
            "unknown trusted provider should be ignored and counted"
        );
        assert_eq!(
            routed
                .skipped_no_eligible_provider_ids()
                .iter()
                .map(ProviderId::as_str)
                .collect::<Vec<_>>(),
            vec!["pbs-a", "pbs-b"],
            "unrouted explicit providers should retain skip outcomes"
        );
    }

    fn slot_with_formats(formats: Vec<AdFormat>) -> AdSlot {
        let mut value = slot(HashMap::new());
        value.formats = formats;
        value
    }

    #[test]
    fn snapshots_first_headers_retains_raw_bytes_and_ignores_inbound_xff() {
        let mut inbound = inbound();
        inbound
            .headers_mut()
            .append(header::COOKIE, HeaderValue::from_static("first=1"));
        inbound
            .headers_mut()
            .append(header::COOKIE, HeaderValue::from_static("second=2"));
        inbound.headers_mut().append(
            header::USER_AGENT,
            HeaderValue::from_bytes(b"agent-\x80").expect("should accept raw header"),
        );
        inbound.headers_mut().append(
            header::REFERER,
            HeaderValue::from_static("https://publisher.example.test/article"),
        );
        inbound
            .headers_mut()
            .append(header::ACCEPT_LANGUAGE, HeaderValue::from_static("en-US"));
        inbound.headers_mut().append(
            HeaderName::from_static("x-forwarded-for"),
            HeaderValue::from_static("203.0.113.250"),
        );
        inbound.headers_mut().append(
            HeaderName::from_static("dnt"),
            HeaderValue::from_static(" 1 "),
        );
        let attested = IpAddr::from_str("192.0.2.10").expect("should parse IP");
        let routed = route_auction(
            request(vec![slot(HashMap::new())]),
            &inbound,
            &plan(),
            Some(attested),
        );
        let headers = routed.prebid_transport_headers();
        assert_eq!(headers.cookie(), Some(&HeaderValue::from_static("first=1")));
        assert_eq!(
            headers.user_agent().expect("should retain UA").as_bytes(),
            b"agent-\x80"
        );
        assert_eq!(
            headers.referer(),
            Some(&HeaderValue::from_static(
                "https://publisher.example.test/article"
            ))
        );
        assert_eq!(
            headers.accept_language(),
            Some(&HeaderValue::from_static("en-US"))
        );
        assert_eq!(routed.attested_client_ip(), Some(attested));
        assert_eq!(routed.dnt(), Some(true));
        for provider in routed.inputs() {
            let expected_timeout = match provider.provider_id().as_str() {
                "aps-primary" => 800,
                id if id.starts_with("pbs-") => 1000,
                _ => 900,
            };
            assert_eq!(provider.timeout_ms(), expected_timeout);
        }
    }
}
