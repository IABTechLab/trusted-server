//! The Prebid Server demand implementation, `prebid_server`.
//!
//! Prebid Server takes the shared `OpenRTB` baseline with its own impression
//! and request extensions, forwards the raw headers its auction needs, and
//! answers with Prebid's own response shape.

use core::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use error_stack::Report;
use http::HeaderMap;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use url::Url;

use crate::auction::demand::{
    CompiledDemand, DemandFieldPolicy, DemandImplementation, DemandResponse, DemandTimeoutDefault,
    DemandTransport, ProviderAuctionInput, RegsPolicy, RequestExtensions,
};
use crate::auction::types::AuctionResponse;
use crate::consent::{ConsentContext, ConsentSource};
use crate::consent_config::ConsentForwardingMode;
use crate::error::TrustedServerError;
use crate::integrations::prebid::{
    BidParamOverrideEngine, BidParamOverrideRule, apply_prebid_transport_headers,
    compile_profile_override_rules, parse_planned_prebid_response,
};
use crate::platform::PlatformResponse;

/// The implementation id `[demand]` names.
pub const PREBID_SERVER_ID: &str = "prebid_server";

/// The canonical Prebid Server auction path.
const AUCTION_PATH: &str = "/openrtb2/auction";

/// The Prebid Server demand implementation.
pub static DEMAND: DemandImplementation = DemandImplementation {
    id: PREBID_SERVER_ID,
    default_timeout: DemandTimeoutDefault::Fixed(1000),
    allows_all_eligible: false,
    serves_stored_requests: true,
    canonicalize_endpoint,
    compile,
};

/// One compiled Prebid Server demand source.
#[derive(Debug, Clone)]
pub struct PrebidServerDemand {
    /// Include Prebid HTTP exchange diagnostics.
    pub debug: bool,
    /// Set `OpenRTB` test mode.
    pub test_mode: bool,
    /// Query fragment appended to the page URL for a test deployment.
    pub debug_query_params: Option<String>,
    /// Compiled override matching and merge index.
    pub(crate) override_engine: BidParamOverrideEngine,
    /// Consent transport policy.
    pub consent_forwarding: ConsentForwardingMode,
}

/// The operator settings `[demand.<name>]` holds for this implementation.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PrebidServerSettings {
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

fn compile(
    settings: &Map<String, Value>,
) -> Result<Arc<dyn CompiledDemand>, Report<TrustedServerError>> {
    let settings =
        PrebidServerSettings::deserialize(Value::Object(settings.clone())).map_err(|error| {
            Report::new(TrustedServerError::Configuration {
                message: format!("invalid `{PREBID_SERVER_ID}` settings: {error}"),
            })
        })?;
    let override_engine = compile_profile_override_rules(
        &settings.bid_param_zone_overrides,
        &settings.bid_param_overrides,
        &settings.bid_param_override_rules,
    )?;
    Ok(Arc::new(PrebidServerDemand {
        debug: settings.debug,
        test_mode: settings.test_mode,
        debug_query_params: settings.debug_query_params,
        override_engine,
        consent_forwarding: settings.consent_forwarding,
    }))
}

/// Complete an endpoint that names the host alone with the auction path.
fn canonicalize_endpoint(endpoint: &mut Url) -> Result<(), String> {
    match endpoint.path() {
        "" | "/" | "/openrtb2/auction/" => endpoint.set_path(AUCTION_PATH),
        _ => {}
    }
    Ok(())
}

#[async_trait(?Send)]
impl CompiledDemand for PrebidServerDemand {
    fn field_policy(&self) -> DemandFieldPolicy {
        DemandFieldPolicy {
            imp_tagid: true,
            site_ref: true,
            precise_geo: true,
            additional_consent: true,
            test: self.test_mode,
            unsigned_request_identity: true,
            consumes_bidder_params: true,
            regs: RegsPolicy::Jurisdiction,
            ..DemandFieldPolicy::default()
        }
    }

    fn site_page(&self, publisher_page: Option<&str>, site_domain: &str) -> Option<String> {
        let _ = site_domain;
        publisher_page.map(|page| {
            self.debug_query_params.as_deref().map_or_else(
                || page.to_owned(),
                |query| append_query_fragment(page, query),
            )
        })
    }

    fn body_consent<'c>(&self, consent: Option<&'c ConsentContext>) -> Option<&'c ConsentContext> {
        consent.filter(|value| {
            self.consent_forwarding.includes_body_consent()
                || !matches!(value.source, ConsentSource::Cookie)
        })
    }

    fn augment_request(
        &self,
        extensions: &mut RequestExtensions<'_>,
        _input: &ProviderAuctionInput,
    ) -> Result<(), Report<TrustedServerError>> {
        for impression in &mut extensions.impressions {
            let slot = impression.slot;
            let bidder = slot
                .bidder_params()
                .iter()
                .filter_map(|(bidder, params)| {
                    let mut params = params.clone();
                    self.override_engine
                        .apply_routed(bidder.as_str(), slot.zone(), &mut params);
                    params
                        .as_object()
                        .is_some_and(|params| !params.is_empty())
                        .then(|| (bidder.as_str().to_string(), params))
                })
                .collect::<Map<_, _>>();
            let mut prebid = Map::new();
            if bidder.is_empty() {
                if slot.is_stored_request() || !slot.bidder_params().is_empty() {
                    prebid.insert("storedrequest".to_string(), json!({"id": slot.slot().id}));
                }
            } else {
                prebid.insert("bidder".to_string(), Value::Object(bidder));
            }
            debug_assert!(
                !prebid.is_empty(),
                "should never route a demandless slot to Prebid Server"
            );
            *impression.ext = Some(Map::from_iter([(
                "prebid".to_string(),
                Value::Object(prebid),
            )]));
        }
        let mut prebid_request = Map::new();
        if self.debug {
            prebid_request.insert("debug".to_string(), Value::Bool(true));
            prebid_request.insert("returnallbidstatus".to_string(), Value::Bool(true));
        }
        *extensions.request = Some(Map::from_iter([(
            "prebid".to_string(),
            Value::Object(prebid_request),
        )]));
        Ok(())
    }

    fn prepare_outbound(&self, headers: &mut HeaderMap, transport: DemandTransport<'_>) {
        apply_prebid_transport_headers(
            transport.headers,
            headers,
            self.consent_forwarding,
            transport.attested_client_ip,
        );
    }

    async fn parse_response(
        &self,
        context: DemandResponse<'_>,
        response: PlatformResponse,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let provider_id = context.provider_id;
        let response_time_ms = context.response_time_ms;
        let auction_id = context.input.common_request().id.clone();
        match parse_planned_prebid_response(
            provider_id,
            self,
            context.input,
            response,
            response_time_ms,
            &auction_id,
        )
        .await
        {
            Ok(parsed) => Ok(parsed),
            Err(error) => {
                log::warn!("Provider '{provider_id}' PBS response parse failed: {error:?}");
                Ok(AuctionResponse::error(provider_id, response_time_ms)
                    .with_metadata("error_type", json!("parse_response")))
            }
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Append a query fragment the deployment set, without repeating one the page
/// already carries.
fn append_query_fragment(url: &str, query: &str) -> String {
    if query.is_empty() || url.contains(query) {
        return url.to_string();
    }
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}{query}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(value: &str) -> String {
        let mut endpoint = Url::parse(value).expect("should parse endpoint");
        canonicalize_endpoint(&mut endpoint).expect("should canonicalize endpoint");
        endpoint.to_string()
    }

    #[test]
    fn endpoint_without_a_path_takes_the_auction_path() {
        assert_eq!(
            canonical("https://pbs.example"),
            "https://pbs.example/openrtb2/auction"
        );
        assert_eq!(
            canonical("https://pbs.example/"),
            "https://pbs.example/openrtb2/auction"
        );
        assert_eq!(
            canonical("https://pbs.example/openrtb2/auction/"),
            "https://pbs.example/openrtb2/auction"
        );
    }

    #[test]
    fn endpoint_with_its_own_path_and_query_is_left_alone() {
        assert_eq!(
            canonical("https://pbs.example/openrtb2/auction?region=example"),
            "https://pbs.example/openrtb2/auction?region=example"
        );
        assert_eq!(
            canonical("https://pbs.example/custom/path"),
            "https://pbs.example/custom/path"
        );
    }

    #[test]
    fn settings_reject_a_key_the_implementation_does_not_know() {
        let settings = Map::from_iter([("debugg".to_string(), Value::Bool(true))]);
        let error = match compile(&settings) {
            Ok(_) => panic!("should reject an unknown setting"),
            Err(error) => error,
        };
        assert!(
            format!("{error:?}").contains("debugg"),
            "should name the unknown setting"
        );
    }

    #[test]
    fn a_page_keeps_one_copy_of_the_debug_query() {
        let demand = PrebidServerDemand {
            debug: false,
            test_mode: false,
            debug_query_params: Some("pbjs_debug=true".to_string()),
            override_engine: BidParamOverrideEngine::default(),
            consent_forwarding: ConsentForwardingMode::default(),
        };
        assert_eq!(
            demand.site_page(Some("https://example.com/news"), "example.com"),
            Some("https://example.com/news?pbjs_debug=true".to_string())
        );
        assert_eq!(
            demand.site_page(
                Some("https://example.com/news?pbjs_debug=true"),
                "example.com"
            ),
            Some("https://example.com/news?pbjs_debug=true".to_string())
        );
    }
}
