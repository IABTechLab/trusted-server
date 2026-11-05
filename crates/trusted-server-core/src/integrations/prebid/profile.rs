//! Prebid Server request specialization and request-bound response parser.
use super::{
    BidParamOverrideEngine, BidParamOverrideRule, PrebidConsentCookies,
    apply_prebid_transport_headers, compile_profile_override_rules, parse_planned_prebid_response,
};
use crate::auction::openrtb::{
    OpenRtbBuildOutcome, build_regs, header_string, normalized_language,
};
use crate::auction::profile::{
    BoundOpenRtbResponseParser, CompiledOpenRtbProfile, OpenRtbPreparation,
    OpenRtbProfileDefinition, PreparedOpenRtbExchange, PreparedOpenRtbOutcome,
    ProfileRoutingPolicy, ProfileTimeoutDefault, build_profile_request, no_impressions,
    outbound_request,
};
use crate::auction::routing::{ProviderAuctionInput, RoutedAuction};
use crate::auction::types::AuctionResponse;
use crate::consent_config::ConsentForwardingMode;
use crate::error::TrustedServerError;
use crate::openrtb::{ConsentedProvidersSettings, OpenRtbRequest, ToExt as _, UserExt};
use crate::platform::PlatformResponse;
use async_trait::async_trait;
use error_stack::Report;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
const PREBID_PROFILE_ID: &str = "prebid-server";
pub(crate) const DEFINITION: OpenRtbProfileDefinition = OpenRtbProfileDefinition {
    id: PREBID_PROFILE_ID,
    default_timeout: ProfileTimeoutDefault::Fixed(1000),
    routing_policy: ProfileRoutingPolicy {
        allows_all_eligible: false,
        recognizes_stored_requests: true,
        consumes_bidder_params: true,
    },
    compile: compile_prebid,
    canonicalize_endpoint: |endpoint| {
        match endpoint.path() {
            "" | "/" => endpoint.set_path("/openrtb2/auction"),
            "/openrtb2/auction/" => endpoint.set_path("/openrtb2/auction"),
            _ => {}
        };
        Ok(())
    },
};
/// Compiled Prebid profile configuration.
#[derive(Debug, Clone)]
pub struct PrebidProfilePlan {
    /// Include Prebid HTTP exchange diagnostics.
    pub debug: bool,
    /// Set `OpenRTB` test mode.
    pub test_mode: bool,
    /// Optional query fragment appended to the page URL under legacy rules.
    pub debug_query_params: Option<String>,
    /// Compiled override matching and merge index.
    pub(crate) override_engine: BidParamOverrideEngine,
    /// Consent transport policy.
    pub consent_forwarding: ConsentForwardingMode,
}

/// Typed operator configuration compiled into a [`PrebidProfilePlan`].
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct PrebidProfileConfig {
    #[serde(default)]
    debug: bool,
    #[serde(default)]
    test_mode: bool,
    #[serde(default)]
    debug_query_params: Option<String>,
    #[serde(default)]
    bid_param_zone_overrides: BTreeMap<String, BTreeMap<String, Map<String, Value>>>,
    #[serde(default)]
    bid_param_overrides: BTreeMap<String, Map<String, Value>>,
    #[serde(default)]
    bid_param_override_rules: Vec<BidParamOverrideRule>,
    #[serde(default)]
    consent_forwarding: ConsentForwardingMode,
}

fn compile_prebid(
    value: &Value,
) -> Result<Arc<dyn CompiledOpenRtbProfile>, Report<TrustedServerError>> {
    let config: PrebidProfileConfig = serde_json::from_value(value.clone()).map_err(|error| {
        Report::new(TrustedServerError::Configuration {
            message: format!("invalid `prebid-server` profile_config: {error}"),
        })
    })?;
    let override_engine = compile_profile_override_rules(
        &config.bid_param_zone_overrides,
        &config.bid_param_overrides,
        &config.bid_param_override_rules,
    )?;
    Ok(Arc::new(PrebidProfilePlan {
        debug: config.debug,
        test_mode: config.test_mode,
        debug_query_params: config.debug_query_params,
        override_engine,
        consent_forwarding: config.consent_forwarding,
    }))
}

impl CompiledOpenRtbProfile for PrebidProfilePlan {
    fn id(&self) -> &'static str {
        PREBID_PROFILE_ID
    }
    fn routing_policy(&self) -> ProfileRoutingPolicy {
        DEFINITION.routing_policy
    }

    fn retains_unsigned_identity(&self) -> bool {
        true
    }
    fn specialize_request(
        &self,
        request: &mut OpenRtbRequest,
        input: &ProviderAuctionInput,
        routed: &RoutedAuction,
    ) -> Result<(), Report<TrustedServerError>> {
        let common = input.common_request();
        let cookies = PrebidConsentCookies::from_header(routed.transport_headers().cookie());
        let selected = cookies.body_consent(common.user.consent.as_ref(), self.consent_forwarding);
        let consent = selected.as_deref();
        if let Some(user) = request.user.as_mut() {
            let raw_tc = consent.and_then(|value| value.raw_tc_string.clone());
            user.consent = raw_tc.clone();
            user.ext = UserExt {
                consent: raw_tc,
                consented_providers_settings: consent
                    .and_then(|value| value.raw_ac_string.clone())
                    .map(|value| ConsentedProvidersSettings {
                        consented_providers: Some(value),
                    }),
                eids: common.user.eids.clone(),
            }
            .to_ext();
        }
        request.regs = build_regs(consent, false);
        if let Some(site) = request.site.as_mut() {
            site.page = common.publisher.page_url.as_deref().map(|page| {
                self.debug_query_params.as_deref().map_or_else(
                    || page.to_string(),
                    |query| append_query_fragment(page, query),
                )
            });
            site.r#ref = header_string(routed.transport_headers().referer());
        }
        let language = normalized_language(routed.transport_headers(), false);
        if request.device.is_none() && language.is_some() {
            request.device = Some(crate::openrtb::Device::default());
        }
        if let Some(device) = request.device.as_mut() {
            device.language = language;
            if let (Some(geo), Some(source)) = (
                device.geo.as_mut(),
                common
                    .device
                    .as_ref()
                    .and_then(|device| device.geo.as_ref()),
            ) {
                geo.lat = Some(source.latitude);
                geo.lon = Some(source.longitude);
            }
        }
        for imp in &mut request.imp {
            imp.tagid = imp.id.clone();
        }
        request.test = self.test_mode.then_some(true);
        apply_prebid(request, input, routed, self)
    }
    fn prepare(
        &self,
        context: OpenRtbPreparation<'_>,
    ) -> Result<PreparedOpenRtbOutcome, Report<TrustedServerError>> {
        let request = match build_profile_request(
            self,
            context.input,
            context.routed,
            context.logical_budget_ms,
            &context.finalization,
        )? {
            OpenRtbBuildOutcome::NoImpressions => return Ok(no_impressions()),
            OpenRtbBuildOutcome::Ready(request) => request,
        };
        let sent = request
            .imp
            .iter()
            .filter_map(|imp| imp.id.as_deref())
            .collect::<HashSet<_>>();
        let input = context
            .input
            .filtered_slots(|slot| sent.contains(slot.slot().id.as_str()));
        let mut request = outbound_request(&request, context.provider.endpoint.as_str(), false)?;
        apply_prebid_transport_headers(
            context.routed.transport_headers(),
            &mut request,
            self.consent_forwarding,
            context.routed.attested_client_ip(),
        );
        Ok(PreparedOpenRtbOutcome::Exchange(PreparedOpenRtbExchange {
            request,
            parser: Box::new(PrebidResponseParser {
                debug: self.debug,
                provider_id: context.input.provider_id().as_str().to_string(),
                auction_id: context.input.common_request().id.clone(),
                input,
            }),
        }))
    }
}

fn apply_prebid(
    request: &mut OpenRtbRequest,
    input: &ProviderAuctionInput,
    _routed: &RoutedAuction,
    plan: &PrebidProfilePlan,
) -> Result<(), Report<TrustedServerError>> {
    // Routing admits only slots with positive banner dimensions that fit in i32,
    // so build_imp produces one impression per routed slot, in the same order.
    debug_assert_eq!(
        request.imp.len(),
        input.slots().len(),
        "should keep one impression per routed slot"
    );
    // Filter paired impressions and slots together so later demand keeps its slot ID.
    request.imp = std::mem::take(&mut request.imp)
        .into_iter()
        .zip(input.slots())
        .filter_map(|(mut imp, slot)| {
            let bidder = slot
                .bidder_params()
                .iter()
                .filter_map(|(bidder, params)| {
                    let mut params = params.clone();
                    plan.override_engine
                        .apply_routed(bidder.as_str(), slot.zone(), &mut params);
                    params
                        .as_object()
                        .is_some_and(|params| !params.is_empty())
                        .then(|| (bidder.as_str().to_string(), params))
                })
                .collect::<Map<_, _>>();
            let mut prebid = Map::new();
            if !bidder.is_empty() {
                prebid.insert("bidder".to_string(), Value::Object(bidder));
            } else if slot.allows_stored_fallback() {
                prebid.insert("storedrequest".to_string(), json!({"id": slot.slot().id}));
            }
            if prebid.is_empty() {
                return None;
            }
            imp.ext = Some(Map::from_iter([(
                "prebid".to_string(),
                Value::Object(prebid),
            )]));
            Some(imp)
        })
        .collect();
    let mut prebid_request = Map::new();
    if plan.debug {
        prebid_request.insert("debug".to_string(), Value::Bool(true));
        prebid_request.insert("returnallbidstatus".to_string(), Value::Bool(true));
    }
    request.ext = Some(Map::from_iter([(
        "prebid".to_string(),
        Value::Object(prebid_request),
    )]));
    Ok(())
}

fn append_query_fragment(url: &str, query: &str) -> String {
    if query.is_empty() || url.contains(query) {
        return url.to_string();
    }
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}{query}")
}

struct PrebidResponseParser {
    debug: bool,
    provider_id: String,
    auction_id: String,
    input: ProviderAuctionInput,
}
#[async_trait(?Send)]
impl BoundOpenRtbResponseParser for PrebidResponseParser {
    async fn parse(
        self: Box<Self>,
        response: PlatformResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        match parse_planned_prebid_response(
            &self.provider_id,
            self.debug,
            &self.input,
            response,
            response_time_ms,
            &self.auction_id,
        )
        .await
        {
            Ok(parsed) => Ok(parsed),
            Err(error) => {
                log::warn!(
                    "Provider '{}' PBS response parse failed: {:?}",
                    self.provider_id,
                    error
                );
                Ok(AuctionResponse::error(&self.provider_id, response_time_ms)
                    .with_metadata("error_type", json!("parse_response")))
            }
        }
    }
}
