//! Neutral, request-stateless `OpenRTB` profile and bound exchange contracts.

use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::body::Body;
use error_stack::Report;
use http::Request;
use serde_json::Value;
use url::Url;

use super::openrtb::{OpenRtbBuildOutcome, RequestFinalization};
use super::plan::ProviderPlan;
use super::routing::{ProviderAuctionInput, RoutedAuction};
use super::types::AuctionResponse;
use crate::error::TrustedServerError;
use crate::platform::PlatformResponse;

/// A registered profile's provider-timeout default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileTimeoutDefault {
    /// Inherit the configured auction timeout.
    Auction,
    /// Use this fixed profile timeout.
    Fixed(u32),
}

/// The routing facts understood by the neutral slot router.
#[derive(Debug, Clone, Copy)]
pub struct ProfileRoutingPolicy {
    /// Permit all eligible slots without explicit demand.
    pub allows_all_eligible: bool,
    /// Recognize trusted stored-request demand and zone facts.
    pub recognizes_stored_requests: bool,
    /// Consume routed bidder parameter objects.
    pub consumes_bidder_params: bool,
}

/// Typed compiler supplied by one catalog profile definition.
pub type OpenRtbProfileCompiler =
    fn(&Value) -> Result<Arc<dyn CompiledOpenRtbProfile>, Report<TrustedServerError>>;

/// Catalog-level compiler and endpoint policy for one `OpenRTB` profile.
#[derive(Clone, Copy)]
pub struct OpenRtbProfileDefinition {
    /// Stable configuration profile identifier.
    pub id: &'static str,
    /// Default logical timeout.
    pub default_timeout: ProfileTimeoutDefault,
    /// Supported routing policy, available before typed configuration compilation.
    pub routing_policy: ProfileRoutingPolicy,
    /// Profile-owned typed configuration compiler.
    pub compile: OpenRtbProfileCompiler,
    /// Validate and canonicalize an already HTTPS-validated endpoint.
    pub canonicalize_endpoint: fn(&mut Url) -> Result<(), Report<TrustedServerError>>,
}

impl core::fmt::Debug for OpenRtbProfileDefinition {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("OpenRtbProfileDefinition")
            .field("id", &self.id)
            .field("default_timeout", &self.default_timeout)
            .finish_non_exhaustive()
    }
}

/// Immutable compiled behavior. Request-local state belongs to its bound parser.
pub trait CompiledOpenRtbProfile: core::fmt::Debug + Send + Sync {
    /// Stable diagnostic profile identifier, never used for engine dispatch.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::auction::profile::CompiledOpenRtbProfile;
    /// fn label(profile: &dyn CompiledOpenRtbProfile) -> &'static str { profile.id() }
    /// ```
    fn id(&self) -> &'static str;
    /// Neutral demand admission policy.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::auction::profile::CompiledOpenRtbProfile;
    /// fn admits_all(profile: &dyn CompiledOpenRtbProfile) -> bool { profile.routing_policy().allows_all_eligible }
    /// ```
    fn routing_policy(&self) -> ProfileRoutingPolicy;
    /// Specialize the common `OpenRTB` envelope before signing.
    ///
    /// # Errors
    /// Returns an auction error when request specialization fails.
    ///
    /// # Examples
    ///
    /// ```
    /// use error_stack::Report;
    /// use trusted_server_core::{auction::{profile::CompiledOpenRtbProfile,
    ///     routing::{ProviderAuctionInput, RoutedAuction}}, error::TrustedServerError, openrtb::OpenRtbRequest};
    /// fn specialize(profile: &dyn CompiledOpenRtbProfile, request: &mut OpenRtbRequest,
    ///     input: &ProviderAuctionInput, facts: &RoutedAuction) -> Result<(), Report<TrustedServerError>> {
    ///     profile.specialize_request(request, input, facts)
    /// }
    /// ```
    fn specialize_request(
        &self,
        request: &mut crate::openrtb::OpenRtbRequest,
        input: &ProviderAuctionInput,
        routed: &RoutedAuction,
    ) -> Result<(), Report<TrustedServerError>>;
    /// Keep unsigned first-party request identity in the request extension.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::auction::profile::CompiledOpenRtbProfile;
    /// fn retains_identity(profile: &dyn CompiledOpenRtbProfile) -> bool { profile.retains_unsigned_identity() }
    /// ```
    fn retains_unsigned_identity(&self) -> bool {
        false
    }
    /// Prepare the finalized outbound exchange, or skip without transport.
    ///
    /// # Errors
    /// Returns an auction error when request construction fails.
    ///
    /// # Examples
    ///
    /// ```
    /// use error_stack::Report;
    /// use trusted_server_core::{auction::profile::{CompiledOpenRtbProfile,
    ///     OpenRtbPreparation, PreparedOpenRtbOutcome}, error::TrustedServerError};
    /// fn prepare(profile: &dyn CompiledOpenRtbProfile, input: OpenRtbPreparation<'_>)
    ///     -> Result<PreparedOpenRtbOutcome, Report<TrustedServerError>> { profile.prepare(input) }
    /// ```
    fn prepare(
        &self,
        input: OpenRtbPreparation<'_>,
    ) -> Result<PreparedOpenRtbOutcome, Report<TrustedServerError>>;
}

/// All facts needed to prepare one exact provider exchange.
pub struct OpenRtbPreparation<'a> {
    /// Provider identity, endpoint and common execution policy.
    pub provider: &'a ProviderPlan,
    /// Privacy-filtered routed demand.
    pub input: &'a ProviderAuctionInput,
    /// Ingress transport facts and attested client IP.
    pub routed: &'a RoutedAuction,
    /// Remaining logical auction budget.
    pub logical_budget_ms: u32,
    /// Canonical transport budget selected by the runtime.
    pub transport_budget_ms: u32,
    /// Optional signer and fixed request signing identity.
    pub finalization: RequestFinalization<'a>,
}

/// Prepared profile result before any backend is registered.
#[allow(
    clippy::large_enum_variant,
    reason = "the finalized HTTP request is consumed directly by transport; boxing adds one allocation per exchange"
)]
pub enum PreparedOpenRtbOutcome {
    /// One finalized outbound request with its exact parser.
    Exchange(PreparedOpenRtbExchange),
    /// No usable impressions. Metadata is retained without transport.
    Skip {
        /// Profile-specific normalized skip metadata.
        metadata: std::collections::HashMap<String, Value>,
    },
}

/// One provider-bound, request-local exchange.
pub struct PreparedOpenRtbExchange {
    /// Final HTTP request including profile-owned headers.
    pub request: Request<Body>,
    /// Consuming parser owning only state for this request.
    pub parser: Box<dyn BoundOpenRtbResponseParser>,
}

/// Consuming response interpretation for one prepared provider request.
#[async_trait(?Send)]
pub trait BoundOpenRtbResponseParser: Send + Sync {
    /// Interpret one bounded upstream response.
    ///
    /// # Errors
    /// Returns an auction error when response collection or interpretation fails.
    ///
    /// # Examples
    ///
    /// ```
    /// use error_stack::Report;
    /// use trusted_server_core::{auction::{AuctionResponse, profile::BoundOpenRtbResponseParser},
    ///     error::TrustedServerError, platform::PlatformResponse};
    /// async fn parse(parser: Box<dyn BoundOpenRtbResponseParser>, response: PlatformResponse)
    ///     -> Result<AuctionResponse, Report<TrustedServerError>> { parser.parse(response, 1).await }
    /// ```
    async fn parse(
        self: Box<Self>,
        response: PlatformResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>>;
}

/// Build the shared envelope, specialize it, then sign the final request.
///
/// # Errors
/// Returns an auction error for invalid extension or signing state.
///
/// # Examples
///
/// ```
/// use error_stack::Report;
/// use trusted_server_core::auction::{
///     openrtb::{OpenRtbBuildOutcome, RequestFinalization},
///     profile::{CompiledOpenRtbProfile, build_profile_request},
///     routing::{ProviderAuctionInput, RoutedAuction},
/// };
/// use trusted_server_core::error::TrustedServerError;
/// fn build(profile: &dyn CompiledOpenRtbProfile, input: &ProviderAuctionInput,
///     facts: &RoutedAuction, signing: &RequestFinalization<'_>)
///     -> Result<OpenRtbBuildOutcome, Report<TrustedServerError>> {
///     build_profile_request(profile, input, facts, 100, signing)
/// }
/// ```
pub fn build_profile_request(
    profile: &dyn CompiledOpenRtbProfile,
    input: &ProviderAuctionInput,
    routed: &RoutedAuction,
    effective_timeout_ms: u32,
    finalization: &RequestFinalization<'_>,
) -> Result<OpenRtbBuildOutcome, Report<TrustedServerError>> {
    let mut request = super::openrtb::build_common_request(input, routed, effective_timeout_ms);
    if request.imp.is_empty() {
        return Ok(OpenRtbBuildOutcome::NoImpressions);
    }
    profile.specialize_request(&mut request, input, routed)?;
    if request.imp.is_empty() {
        return Ok(OpenRtbBuildOutcome::NoImpressions);
    }
    super::openrtb::finalize_request(
        &mut request,
        profile.retains_unsigned_identity(),
        finalization,
    )?;
    Ok(OpenRtbBuildOutcome::Ready(request))
}

/// Serialize a finalized request into one buffered body without changing its wire representation.
///
/// The returned body is [`Body::Once`]; diagnostic capture can borrow its bytes.
///
/// # Errors
/// Returns an auction error when serialization or HTTP construction fails.
///
/// # Examples
///
/// ```
/// use trusted_server_core::auction::profile::outbound_request;
/// use trusted_server_core::openrtb::OpenRtbRequest;
/// let request = outbound_request(&OpenRtbRequest::default(),
///     "https://bidder.example/openrtb", true).expect("should build outbound request");
/// assert_eq!(request.headers()[http::header::ACCEPT], "application/json");
/// assert!(matches!(request.body(), edgezero_core::body::Body::Once(_)));
/// ```
pub fn outbound_request(
    request: &crate::openrtb::OpenRtbRequest,
    endpoint: &str,
    accept_json: bool,
) -> Result<Request<Body>, Report<TrustedServerError>> {
    let body = serde_json::to_vec(request).map_err(|error| {
        Report::new(TrustedServerError::Auction {
            message: format!("OpenRTB request serialization failed: {error}"),
        })
    })?;
    let mut builder = Request::builder()
        .method(http::Method::POST)
        .uri(endpoint)
        .header(http::header::CONTENT_TYPE, "application/json");
    if accept_json {
        builder = builder.header(http::header::ACCEPT, "application/json");
    }
    builder.body(Body::from(body)).map_err(|error| {
        Report::new(TrustedServerError::Auction {
            message: format!("OpenRTB request construction failed: {error}"),
        })
    })
}

/// Construct the existing no-impressions outcome without backend or transport effects.
///
/// # Examples
///
/// ```
/// use trusted_server_core::auction::profile::{PreparedOpenRtbOutcome, no_impressions};
/// assert!(matches!(no_impressions(), PreparedOpenRtbOutcome::Skip { .. }));
/// ```
#[must_use]
pub fn no_impressions() -> PreparedOpenRtbOutcome {
    PreparedOpenRtbOutcome::Skip {
        metadata: std::collections::HashMap::from([(
            "routing".to_string(),
            serde_json::json!({"skipped_no_usable_demand": true}),
        )]),
    }
}
