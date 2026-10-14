//! Demand and ad server implementations, the seam through which an integration
//! supplies the providers an auction calls.
//!
//! A deployment selects its demand sources with `[demand] modules = [...]` and
//! its ad server with `[ad-server] module = "..."`. Each selected name resolves
//! to an implementation that an integration builder registers, so core names no
//! vendor and no protocol. Core routes each source the facts it may use, sends
//! the request the source builds from them and hands the answer back to the
//! source to read. The protocol a source speaks is the source's own, through
//! the crate of the standard it speaks.

use core::any::Any;
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use error_stack::Report;
use http::HeaderMap;
use serde_json::{Map, Value};
use url::Url;

use crate::error::TrustedServerError;
use crate::platform::PlatformResponse;
use crate::request_signing::{RequestSigner, SigningParams};

use super::provider::AuctionProvider;
pub use super::routing::{
    ProviderAuctionInput, ProviderSlotInput, RoutedAuction, TransportHeaders,
};
use super::types::AuctionResponse;

/// Where a demand source's timeout comes from when its table sets none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemandTimeoutDefault {
    /// The whole-auction timeout.
    Auction,
    /// A fixed timeout, in milliseconds.
    Fixed(u32),
}

/// The request facts a source may use beside its routed input.
#[derive(Debug, Clone, Copy)]
pub struct DemandTransport<'a> {
    /// The request headers admitted for forwarding.
    pub headers: &'a TransportHeaders,
    /// The client address the platform attested, never one a client supplied.
    pub attested_client_ip: Option<IpAddr>,
    /// Whether the request asked not to be tracked.
    pub dnt: Option<bool>,
}

/// The signing input core settles before a source builds its request. No
/// signer is loaded here.
pub struct RequestFinalization<'a> {
    /// The signer the auction admitted, or `None` for an unsigned request.
    pub signer: Option<&'a RequestSigner>,
    /// What the signature is made over.
    pub signing_params: SigningParams,
}

/// The request one source built from its routed input, ready to send.
pub struct OutboundRequest {
    /// The body as it is sent.
    pub body: Vec<u8>,
    /// The headers the source sets, `Content-Type` included.
    pub headers: HeaderMap,
    /// The ids of the slots the request asks for. The response is read
    /// against these slots alone.
    pub slot_ids: HashSet<String>,
    /// State the source keeps from the request for reading the response.
    pub captured: Option<Box<dyn Any + Send + Sync>>,
}

/// Facts a source reads while it parses a response.
#[derive(Clone, Copy)]
pub struct DemandResponse<'a> {
    /// The configured name of the demand source.
    pub provider_id: &'a str,
    /// The canonical endpoint the request was sent to.
    pub endpoint: &'a str,
    /// The routed input the request was built from, with the slots the
    /// request asked for.
    pub input: &'a ProviderAuctionInput,
    /// How long the request took, in milliseconds.
    pub response_time_ms: u64,
    /// State the source kept in [`OutboundRequest::captured`].
    pub captured: Option<&'a (dyn Any + Send + Sync)>,
}

/// The request and response behavior of one selected demand source, compiled
/// from its `[demand.<name>]` table at startup.
///
/// A source builds its request from the facts core routed to it, which are
/// the slots and their formats, the publisher, the permission state the
/// auction resolved and the identifiers the request may carry, and reads the
/// answer into core's bids. What the request looks like on the wire is the
/// source's own, through the crate of the protocol it speaks.
#[async_trait(?Send)]
pub trait CompiledDemand: Send + Sync {
    /// Builds the request for the routed input, or `None` when the source has
    /// nothing to ask for, in which case core makes no request.
    ///
    /// # Errors
    ///
    /// Returns an error when the request cannot be built for this input.
    fn build_request(
        &self,
        input: &ProviderAuctionInput,
        transport: DemandTransport<'_>,
        timeout_ms: u32,
        finalization: &RequestFinalization<'_>,
    ) -> Result<Option<OutboundRequest>, Report<TrustedServerError>>;

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

    /// Whether routed bidder parameters are request data for this source,
    /// rather than parameters it was routed and ignores.
    fn consumes_bidder_params(&self) -> bool {
        false
    }

    /// This compiled source as [`Any`], so the implementation that created it
    /// can read its own settings back, for example while it registers routes.
    fn as_any(&self) -> &dyn Any;
}

/// Compiles one demand source from its settings.
///
/// The map holds the table's own settings, without `implementation` and the
/// common keys core reads, which are `endpoint`, `timeout_ms`, `routing` and
/// `notifications`.
pub type CompileDemandFn =
    fn(&Map<String, Value>) -> Result<Arc<dyn CompiledDemand>, Report<TrustedServerError>>;

/// Builds an ad server from its configured name and its settings.
///
/// The map holds the table's own settings without `implementation`, and the
/// name is what the ad server reports itself as in responses and telemetry.
pub type BuildAdServerFn =
    fn(&str, &Map<String, Value>) -> Result<Arc<dyn AuctionProvider>, Report<TrustedServerError>>;

/// A demand implementation an integration registers, named by
/// `[demand] modules` or by an `implementation` line.
#[derive(Clone, Copy)]
pub struct DemandImplementation {
    /// The implementation's name, its module path.
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
    /// Compiles one source from its settings.
    pub compile: CompileDemandFn,
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
/// `[ad-server] module` or by an `implementation` line.
#[derive(Clone, Copy)]
pub struct AdServerImplementation {
    /// The implementation's name, its module path.
    pub id: &'static str,
    /// Builds the ad server from its configured name and its settings.
    pub build: BuildAdServerFn,
}

impl core::fmt::Debug for AdServerImplementation {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("AdServerImplementation")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
