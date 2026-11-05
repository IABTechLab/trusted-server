//! APS request specialization and request-bound response parser.
use super::{ApsDebugRequest, parse_planned_aps_response};
use crate::auction::openrtb::{OpenRtbBuildOutcome, build_regs, ignored_bidder_params_count};
use crate::auction::profile::{
    BoundOpenRtbResponseParser, CompiledOpenRtbProfile, OpenRtbPreparation,
    OpenRtbProfileDefinition, PreparedOpenRtbExchange, PreparedOpenRtbOutcome,
    ProfileRoutingPolicy, ProfileTimeoutDefault, build_profile_request, no_impressions,
    outbound_request,
};
use crate::auction::routing::{ProviderAuctionInput, RoutedAuction};
use crate::auction::types::AuctionResponse;
use crate::error::TrustedServerError;
use crate::openrtb::OpenRtbRequest;
use crate::platform::PlatformResponse;
use async_trait::async_trait;
use edgezero_core::body::Body;
use error_stack::Report;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use url::Url;
const APS_PROFILE_ID: &str = "aps";
pub(crate) const DEFINITION: OpenRtbProfileDefinition = OpenRtbProfileDefinition {
    id: APS_PROFILE_ID,
    default_timeout: ProfileTimeoutDefault::Fixed(800),
    routing_policy: ProfileRoutingPolicy {
        allows_all_eligible: true,
        recognizes_stored_requests: false,
        consumes_bidder_params: false,
    },
    compile: compile_aps,
    canonicalize_endpoint: |endpoint| {
        if endpoint
            .path()
            .trim_end_matches('/')
            .ends_with("/e/dtb/bid")
        {
            return Err(Report::new(TrustedServerError::Configuration {
                message: "uses unsupported legacy APS endpoint `/e/dtb/bid`".to_string(),
            }));
        }
        Ok(())
    },
};
/// Compiled APS profile configuration.
#[derive(Debug, Clone)]
pub struct ApsProfilePlan {
    /// APS account identifier.
    pub account_id: String,
    /// Include APS request/response diagnostics.
    pub debug: bool,
    /// Permit APS script creatives.
    pub allow_script_creatives: bool,
    /// Optional authorized inventory domain.
    pub inventory_domain: Option<String>,
    /// Optional canonical inventory page origin.
    pub inventory_page_origin: Option<String>,
}

fn compile_aps(
    value: &Value,
) -> Result<Arc<dyn CompiledOpenRtbProfile>, Report<TrustedServerError>> {
    let config = super::compile_profile_config(value.clone())?;
    Ok(Arc::new(ApsProfilePlan {
        account_id: config.account_id,
        debug: config.debug,
        allow_script_creatives: config.allow_script_creatives,
        inventory_domain: config.inventory_domain,
        inventory_page_origin: config.inventory_page_origin,
    }))
}

impl CompiledOpenRtbProfile for ApsProfilePlan {
    fn id(&self) -> &'static str {
        APS_PROFILE_ID
    }
    fn routing_policy(&self) -> ProfileRoutingPolicy {
        DEFINITION.routing_policy
    }

    fn specialize_request(
        &self,
        request: &mut OpenRtbRequest,
        input: &ProviderAuctionInput,
        _: &RoutedAuction,
    ) -> Result<(), Report<TrustedServerError>> {
        let common = input.common_request();
        let domain = self
            .inventory_domain
            .clone()
            .unwrap_or_else(|| common.publisher.domain.clone());
        if let Some(site) = request.site.as_mut() {
            site.domain = Some(domain.clone());
            site.page = aps_inventory_page(self, common.publisher.page_url.as_deref(), &domain);
            if let Some(publisher) = site.publisher.as_mut() {
                publisher.domain = Some(domain);
            }
        }
        request.regs = build_regs(common.user.consent.as_ref(), true);
        for imp in &mut request.imp {
            if let Some(banner) = imp.banner.as_mut() {
                banner.w = banner.format.first().and_then(|format| format.w);
                banner.h = banner.format.first().and_then(|format| format.h);
                banner.topframe = Some(false);
            }
        }
        request.ext = Some(Map::from_iter([
            (
                "account".to_string(),
                Value::String(self.account_id.clone()),
            ),
            (
                "sdk".to_string(),
                json!({"source": "prebid", "version": "2.2.0"}),
            ),
        ]));
        Ok(())
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
        let request = outbound_request(&request, context.provider.endpoint.as_str(), false)?;
        let debug_request = match (self.debug, request.body()) {
            (true, Body::Once(body)) => Some(ApsDebugRequest::capture(body, request.headers())),
            _ => None,
        };
        Ok(PreparedOpenRtbOutcome::Exchange(PreparedOpenRtbExchange {
            request,
            parser: Box::new(ApsResponseParser {
                profile: self.clone(),
                provider_id: context.input.provider_id().as_str().to_string(),
                endpoint: context.provider.endpoint.as_str().to_string(),
                input: context.input.clone(),
                debug_request,
            }),
        }))
    }
}
fn aps_inventory_page(
    plan: &ApsProfilePlan,
    publisher_page: Option<&str>,
    domain: &str,
) -> Option<String> {
    let fallback = publisher_page
        .and_then(valid_aps_page_url)
        .unwrap_or_else(|| format!("https://{domain}"));
    let Some(origin) = plan.inventory_page_origin.as_deref() else {
        return Some(fallback);
    };
    let (Ok(mut canonical), Ok(current)) = (Url::parse(origin), Url::parse(&fallback)) else {
        return Some(fallback);
    };
    canonical.set_path(current.path());
    canonical.set_query(current.query());
    canonical.set_fragment(None);
    Some(canonical.to_string())
}

fn valid_aps_page_url(value: &str) -> Option<String> {
    const MAX_APS_PAGE_URL_BYTES: usize = 8192;

    if value.len() > MAX_APS_PAGE_URL_BYTES {
        return None;
    }
    let parsed = Url::parse(value).ok()?;
    (matches!(parsed.scheme(), "http" | "https")
        && parsed.host_str().is_some()
        && parsed.username().is_empty()
        && parsed.password().is_none())
    .then(|| parsed.to_string())
}

struct ApsResponseParser {
    profile: ApsProfilePlan,
    provider_id: String,
    endpoint: String,
    input: ProviderAuctionInput,
    debug_request: Option<ApsDebugRequest>,
}
#[async_trait(?Send)]
impl BoundOpenRtbResponseParser for ApsResponseParser {
    async fn parse(
        self: Box<Self>,
        response: PlatformResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        match parse_planned_aps_response(
            &self.provider_id,
            &self.profile,
            &self.endpoint,
            &self.input,
            response,
            response_time_ms,
            self.debug_request,
        )
        .await
        {
            Ok(parsed) => Ok(parsed),
            Err(error) => {
                log::warn!(
                    "Provider '{}' APS response parse failed: {:?}",
                    self.provider_id,
                    error
                );
                Ok(AuctionResponse::error(&self.provider_id, response_time_ms).with_metadata("error_type", json!("parse_response"))
                    .with_metadata("routing", json!({"unused_bidder_params_count": ignored_bidder_params_count(&self.input)})))
            }
        }
    }
}
