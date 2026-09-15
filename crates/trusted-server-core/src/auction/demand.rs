//! Demand and ad server implementations, the seam through which an integration
//! supplies the providers an auction calls.
//!
//! A deployment selects its demand sources with `[demand] provider = [...]` and
//! its ad server with `[adserver] provider = "..."`. Each selected name resolves
//! to an implementation that an integration builder registers, so core names no
//! vendor. The common `OpenRTB` driver builds every standard request field, sends
//! the request and enforces privacy. An implementation decides only the choices
//! this module exposes, which are its field policy, its own extensions, its
//! outbound headers and how its responses are read.

use core::any::Any;
use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{HeaderMap, Request};
use serde_json::{Map, Value};
use url::Url;

use crate::consent::ConsentContext;
use crate::error::TrustedServerError;
use crate::openrtb::OpenRtbRequest;
use crate::platform::PlatformResponse;

pub use super::routing::{ProviderAuctionInput, ProviderSlotInput, TransportHeaders};
use super::provider::AuctionProvider;
use super::types::AuctionResponse;

/// The longest primary `Accept-Language` tag a conservative source receives.
///
/// A source that sets no limit receives the whole primary tag.
pub const CONSERVATIVE_LANGUAGE_MAX_BYTES: usize = 8;

/// Where a demand source's timeout comes from when its table sets none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemandTimeoutDefault {
    /// The whole-auction timeout.
    Auction,
    /// A fixed timeout, in milliseconds.
    Fixed(u32),
}

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
/// The common driver constructs every standard field and reads this policy
/// while it does. A policy can leave data out of what central privacy
/// enforcement approved, and it cannot add data that enforcement removed.
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
    /// Send Google Additional Consent as `user.ext.ConsentedProvidersSettings`.
    pub additional_consent: bool,
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

/// Facts an implementation may use when it adjusts the outbound request.
#[derive(Debug, Clone, Copy)]
pub struct DemandTransport<'a> {
    /// The request headers admitted for forwarding.
    pub headers: &'a TransportHeaders,
    /// The client address the platform attested, never one a client supplied.
    pub attested_client_ip: Option<IpAddr>,
}

/// Facts an implementation reads while it parses a response.
#[derive(Clone, Copy)]
pub struct DemandResponse<'a> {
    /// The configured name of the demand source.
    pub provider_id: &'a str,
    /// The canonical endpoint the request was sent to.
    pub endpoint: &'a str,
    /// The routed input the request was built from.
    pub input: &'a ProviderAuctionInput,
    /// How long the request took, in milliseconds.
    pub response_time_ms: u64,
    /// State [`CompiledDemand::capture_request`] returned for this request.
    pub captured: Option<&'a (dyn Any + Send + Sync)>,
}

/// The request and response behavior of one selected demand source, compiled
/// from its `[demand.<name>]` table at startup.
///
/// Every method except [`augment_request`](Self::augment_request) and
/// [`parse_response`](Self::parse_response) has a default that suits a source
/// with no special handling.
#[async_trait(?Send)]
pub trait CompiledDemand: Send + Sync {
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
        request: &mut OpenRtbRequest,
        input: &ProviderAuctionInput,
    ) -> Result<(), Report<TrustedServerError>>;

    /// Adjusts the outbound HTTP request before it is sent, for example to
    /// forward admitted headers.
    fn prepare_outbound(&self, outbound: &mut Request<EdgeBody>, transport: DemandTransport<'_>) {
        let _ = (outbound, transport);
    }

    /// Keeps request-local state the response parser needs, taken from the
    /// serialized body and the outbound headers.
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

    /// This compiled source as [`Any`], so the implementation that created it
    /// can read its own settings back, for example while it registers routes.
    fn as_any(&self) -> &dyn Any;
}

/// A demand implementation an integration registers, named by `[demand]`
/// `provider` or by an `implementation` line.
#[derive(Clone, Copy)]
pub struct DemandImplementation {
    /// The implementation id, in `snake_case`.
    pub id: &'static str,
    /// The timeout a source uses when its table sets none.
    pub default_timeout: DemandTimeoutDefault,
    /// Whether `routing = "all_eligible"` is allowed.
    pub allows_all_eligible: bool,
    /// Whether a slot that carries no bidder parameters is still sent, as a
    /// stored request, with the slot's zone.
    pub serves_stored_requests: bool,
    /// Checks and canonicalizes the endpoint after the common checks passed.
    pub canonicalize_endpoint: fn(&mut Url) -> Result<(), String>,
    /// Compiles one source from its settings. The map holds the table's own
    /// settings, without `implementation` and the common keys the driver
    /// reads (`endpoint`, `timeout_ms`, `routing` and `notifications`).
    pub compile: fn(&Map<String, Value>) -> Result<Arc<dyn CompiledDemand>, Report<TrustedServerError>>,
}

impl core::fmt::Debug for DemandImplementation {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("DemandImplementation")
            .field("id", &self.id)
            .field("default_timeout", &self.default_timeout)
            .field("allows_all_eligible", &self.allows_all_eligible)
            .field("serves_stored_requests", &self.serves_stored_requests)
            .finish_non_exhaustive()
    }
}

/// Accepts every endpoint the common checks accepted, unchanged.
///
/// # Errors
///
/// Never returns an error.
pub fn accept_endpoint(endpoint: &mut Url) -> Result<(), String> {
    let _ = endpoint;
    Ok(())
}

/// An ad server implementation an integration registers, named by
/// `[adserver] provider` or by an `implementation` line.
#[derive(Clone, Copy)]
pub struct AdServerImplementation {
    /// The implementation id, in `snake_case`.
    pub id: &'static str,
    /// Builds the ad server from its configured name and its settings. The map
    /// holds the table's own settings without `implementation`, and the name
    /// is what the ad server reports itself as in responses and telemetry.
    pub build: fn(&str, &Map<String, Value>) -> Result<Arc<dyn AuctionProvider>, Report<TrustedServerError>>,
}

impl core::fmt::Debug for AdServerImplementation {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("AdServerImplementation")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
