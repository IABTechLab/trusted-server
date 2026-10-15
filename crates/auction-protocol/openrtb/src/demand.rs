//! What a demand source that speaks `OpenRTB` decides, and the driver that
//! runs it through core's demand seam.
//!
//! The driver builds every standard request field the same way for all of
//! them and reads a response the ordinary way. A source decides only what
//! [`OpenRtbDemand`] exposes, which is its field policy, its own extensions,
//! its outbound headers and how its responses are read.

use core::any::Any;
use std::collections::HashSet;

use async_trait::async_trait;
use error_stack::{Report, ResultExt as _};
use http::{HeaderMap, HeaderValue, header};
use serde_json::{Map, Value};
use trusted_server_core::auction::demand::{
    CompiledDemand, DemandResponse, DemandTransport, OutboundRequest, ProviderAuctionInput,
    ProviderSlotInput, RequestFinalization,
};
use trusted_server_core::auction::types::AuctionResponse;
use trusted_server_core::consent::ConsentContext;
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::platform::PlatformResponse;

/// The longest primary `Accept-Language` tag a conservative source receives.
///
/// A source that sets no limit receives the whole primary tag.
pub const CONSERVATIVE_LANGUAGE_MAX_BYTES: usize = 8;

/// How the `regs` object is derived from the consent the auction admitted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RegsPolicy {
    /// No `regs` without consent data, and GDPR set from the applicability bit
    /// or from a GDPR jurisdiction.
    #[default]
    Jurisdiction,
    /// `regs` for any admitted consent, and GDPR set from the applicability bit
    /// alone.
    ApplicabilityBit,
}

/// The standard `OpenRTB` field choices one demand source makes.
///
/// The driver constructs every standard field and reads this policy while it
/// does. A policy can leave data out of what central privacy enforcement
/// approved, and it cannot add data that enforcement removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DemandFieldPolicy {
    /// Send the slot id as `imp.tagid`.
    pub imp_tagid: bool,
    /// Copy the first banner format into `banner.w` and `banner.h`, and send
    /// `banner.topframe` as not in the top frame.
    pub primary_banner_size: bool,
    /// Send the accepted `Referer` header as `site.ref`.
    pub site_ref: bool,
    /// Send precise latitude and longitude with the coarse geo.
    pub precise_geo: bool,
    /// The longest primary `Accept-Language` tag sent, or `None` for no limit.
    pub language_max_bytes: Option<usize>,
    /// How `regs` is derived from consent.
    pub regs: RegsPolicy,
    /// Mark the request as a test request.
    pub test: bool,
    /// Keep the Trusted Server host and scheme in `ext.trusted_server` when the
    /// request is not signed.
    pub unsigned_request_identity: bool,
    /// Send `Accept: application/json`.
    pub accept_json: bool,
    /// Treat routed bidder parameters as request data rather than reporting
    /// them as parameters the source ignored.
    pub consumes_bidder_params: bool,
}

/// The extension objects a source may add to a request the driver built, one
/// for the request and one for each impression.
///
/// This is the only part of the request a source writes. Every standard field
/// is the driver's, chosen through [`DemandFieldPolicy`], so a source can add
/// what its exchange needs and cannot change what privacy enforcement decided.
/// The `trusted_server` request extension is the driver's too, and a request
/// that claims it is refused.
pub struct RequestExtensions<'a> {
    /// The request-level `ext` object.
    pub request: &'a mut Option<Map<String, Value>>,
    /// The impressions the driver built, each beside the slot it came from.
    pub impressions: Vec<ImpressionExtension<'a>>,
}

/// One impression's `ext` object beside the routed slot it was built from.
pub struct ImpressionExtension<'a> {
    /// The routed slot, with its bidder parameters and zone.
    pub slot: &'a ProviderSlotInput,
    /// The impression's `ext` object.
    pub ext: &'a mut Option<Map<String, Value>>,
    /// Set by a source that has no demand to send for this slot. The driver
    /// leaves the impression out of the request, and makes no request at all
    /// when every impression is left out.
    pub omitted: bool,
}

/// What one demand source that speaks `OpenRTB` decides, compiled from its
/// `[demand.<name>]` table at startup and run by [`OpenRtbDriver`].
///
/// Every method except [`field_policy`](Self::field_policy),
/// [`augment_request`](Self::augment_request),
/// [`parse_response`](Self::parse_response) and [`as_any`](Self::as_any) has
/// a default that suits a source with no special handling.
#[async_trait(?Send)]
pub trait OpenRtbDemand: Send + Sync {
    /// The standard field choices this source makes.
    fn field_policy(&self) -> DemandFieldPolicy;

    /// The domain sent as `site.domain` and `site.publisher.domain`.
    fn site_domain(&self, publisher_domain: &str) -> String {
        publisher_domain.to_owned()
    }

    /// The page sent as `site.page`, given the publisher's page and the site
    /// domain [`site_domain`](Self::site_domain) chose.
    fn site_page(&self, publisher_page: Option<&str>, site_domain: &str) -> Option<String> {
        let _ = site_domain;
        publisher_page.map(str::to_owned)
    }

    /// The consent that may travel in the request body.
    fn body_consent<'c>(&self, consent: Option<&'c ConsentContext>) -> Option<&'c ConsentContext> {
        consent
    }

    /// Adds this source's own request and impression extensions.
    ///
    /// # Errors
    ///
    /// Returns an error when an extension cannot be built for this request.
    fn augment_request(
        &self,
        extensions: &mut RequestExtensions<'_>,
        input: &ProviderAuctionInput,
    ) -> Result<(), Report<TrustedServerError>>;

    /// Adds to the outbound headers before the request is sent, for example to
    /// forward admitted headers. The driver has already set `Content-Type`,
    /// and `Accept` when the field policy asks for it.
    fn prepare_outbound(&self, headers: &mut HeaderMap, transport: DemandTransport<'_>) {
        let _ = (headers, transport);
    }

    /// Keeps request-local state the response parser needs, taken from the
    /// serialized body and the outbound headers as they will be sent, after
    /// [`prepare_outbound`](Self::prepare_outbound) has run.
    fn capture_request(
        &self,
        body: &[u8],
        headers: &HeaderMap,
    ) -> Option<Box<dyn Any + Send + Sync>> {
        let _ = (body, headers);
        None
    }

    /// Reads the upstream response into an auction response.
    ///
    /// # Errors
    ///
    /// Returns an error when the response cannot be read at all. A response
    /// that arrived but is not usable is reported as an error response rather
    /// than as an `Err`.
    async fn parse_response(
        &self,
        context: DemandResponse<'_>,
        response: PlatformResponse,
    ) -> Result<AuctionResponse, Report<TrustedServerError>>;

    /// This source as [`Any`], so the implementation that created it can read
    /// its own settings back, for example while it registers routes.
    fn as_any(&self) -> &dyn Any;
}

/// Runs one [`OpenRtbDemand`] through core's demand seam.
///
/// An implementation's `compile` wraps the source it compiled in this, so
/// core sends the request the driver builds and hands the answer back to the
/// source's own reader. Core's [`CompiledDemand::as_any`] answers with the
/// wrapped source, so the implementation reads its own settings back as if
/// nothing stood between.
pub struct OpenRtbDriver {
    demand: Box<dyn OpenRtbDemand>,
}

impl OpenRtbDriver {
    /// The driver over one compiled source.
    #[must_use]
    pub fn new(demand: impl OpenRtbDemand + 'static) -> Self {
        Self {
            demand: Box::new(demand),
        }
    }

    /// The source the driver runs.
    #[must_use]
    pub fn demand(&self) -> &dyn OpenRtbDemand {
        self.demand.as_ref()
    }
}

#[async_trait(?Send)]
impl CompiledDemand for OpenRtbDriver {
    fn build_request(
        &self,
        input: &ProviderAuctionInput,
        transport: DemandTransport<'_>,
        timeout_ms: u32,
        finalization: &RequestFinalization<'_>,
    ) -> Result<Option<OutboundRequest>, Report<TrustedServerError>> {
        let demand = self.demand.as_ref();
        let Some(request) =
            crate::request::build_request(demand, input, transport, timeout_ms, finalization)?
        else {
            return Ok(None);
        };
        let slot_ids = request
            .imp
            .iter()
            .filter_map(|impression| impression.id.clone())
            .collect::<HashSet<_>>();
        let body = serde_json::to_vec(&request).change_context(TrustedServerError::Auction {
            message: format!(
                "Provider {} request serialization failed",
                input.provider_id().as_str()
            ),
        })?;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        if demand.field_policy().accept_json {
            headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        }
        demand.prepare_outbound(&mut headers, transport);
        let captured = demand.capture_request(&body, &headers);
        Ok(Some(OutboundRequest {
            body,
            headers,
            slot_ids,
            captured,
        }))
    }

    async fn parse_response(
        &self,
        context: DemandResponse<'_>,
        response: PlatformResponse,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        self.demand.parse_response(context, response).await
    }

    fn consumes_bidder_params(&self) -> bool {
        self.demand.field_policy().consumes_bidder_params
    }

    fn as_any(&self) -> &dyn Any {
        self.demand.as_any()
    }
}
