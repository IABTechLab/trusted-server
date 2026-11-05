//! Neutral mediator preparation and consuming response contracts.

use super::types::{AuctionContext, AuctionRequest, AuctionResponse};
use crate::error::TrustedServerError;
use crate::platform::{
    PlatformBackendSpec, PlatformHttpRequest, PlatformPendingRequest, PlatformResponse,
};
use async_trait::async_trait;
use edgezero_core::body::Body;
use error_stack::{Report, ResultExt as _};
use http::Request;
use serde_json::Value;
use std::sync::Arc;

/// Typed compiler supplied by one catalog mediator definition.
pub type MediatorCompiler =
    fn(&Value) -> Result<Arc<dyn CompiledMediator>, Report<TrustedServerError>>;

/// Catalog-level typed mediator compiler.
#[derive(Clone, Copy)]
pub struct MediatorDefinition {
    /// Stable integration-owned mediator identity.
    pub id: &'static str,
    /// Compile immutable mediator configuration.
    pub compile: MediatorCompiler,
}

/// Immutable mediator behavior. Transport and deadlines remain in the engine.
pub trait CompiledMediator: Send + Sync {
    /// Integration identity retained for response correlation.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::auction::mediator::CompiledMediator;
    /// fn label(mediator: &dyn CompiledMediator) -> &str { mediator.id() }
    /// ```
    fn id(&self) -> &str;
    /// Maximum logical timeout in milliseconds.
    ///
    /// # Examples
    ///
    /// ```
    /// use trusted_server_core::auction::mediator::CompiledMediator;
    /// fn budget(mediator: &dyn CompiledMediator) -> u32 { mediator.timeout_ms() }
    /// ```
    fn timeout_ms(&self) -> u32;
    /// Build one exchange from the original request and plan-ordered outcomes.
    ///
    /// # Errors
    /// Returns an auction error when request construction fails.
    ///
    /// # Examples
    ///
    /// ```
    /// use error_stack::Report;
    /// use trusted_server_core::{auction::mediator::{CompiledMediator, MediatorPreparation,
    ///     PreparedMediatorExchange}, error::TrustedServerError};
    /// fn prepare(mediator: &dyn CompiledMediator, input: MediatorPreparation<'_>)
    ///     -> Result<PreparedMediatorExchange, Report<TrustedServerError>> { mediator.prepare(input) }
    /// ```
    fn prepare(
        &self,
        input: MediatorPreparation<'_>,
    ) -> Result<PreparedMediatorExchange, Report<TrustedServerError>>;

    // Compatibility for existing orchestration test doubles only. Remove when
    // those legacy AuctionProvider-based fixtures have been ported.
    #[cfg(test)]
    fn legacy_test_provider(&self) -> Option<Arc<dyn super::provider::AuctionProvider>> {
        None
    }
}

/// Bounded, ordered input for one mediation attempt.
pub struct MediatorPreparation<'a> {
    /// Original auction facts, including context and slots.
    pub request: &'a AuctionRequest,
    /// Provider outcomes in compiled plan order.
    pub provider_responses: &'a [AuctionResponse],
    /// Remaining logical budget.
    pub logical_budget_ms: u32,
    /// Runtime-canonicalized transport timeout.
    pub transport_budget_ms: u32,
}

/// Final HTTP request, backend declaration and its exact response interpreter.
pub struct PreparedMediatorExchange {
    /// Finalized outbound HTTP request.
    pub request: Request<Body>,
    /// Backend policy, retaining the integration's correlation identity.
    pub backend: PlatformBackendSpec,
    /// Request-bound consuming parser.
    pub parser: Box<dyn BoundMediatorResponseParser>,
}

/// One request-local mediator response interpreter.
#[async_trait(?Send)]
pub trait BoundMediatorResponseParser: Send + Sync {
    /// Consume the upstream response into the normalized mediation result.
    ///
    /// # Errors
    /// Returns an auction error when response collection or parsing fails.
    ///
    /// # Examples
    ///
    /// ```
    /// use error_stack::Report;
    /// use trusted_server_core::{auction::{AuctionResponse, mediator::BoundMediatorResponseParser},
    ///     error::TrustedServerError, platform::PlatformResponse};
    /// async fn parse(parser: Box<dyn BoundMediatorResponseParser>, response: PlatformResponse)
    ///     -> Result<AuctionResponse, Report<TrustedServerError>> { parser.parse(response, 1).await }
    /// ```
    async fn parse(
        self: Box<Self>,
        response: PlatformResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>>;
}

pub(crate) enum MediatorRequestOutcome {
    Pending {
        request: PlatformPendingRequest,
        parser: Box<dyn BoundMediatorResponseParser>,
    },
    #[cfg(test)]
    Immediate(AuctionResponse),
}

/// Register and dispatch a prepared exchange without interpreting its payload.
pub(crate) async fn launch_mediator(
    mediator: &dyn CompiledMediator,
    request: &AuctionRequest,
    context: &AuctionContext<'_>,
) -> Result<MediatorRequestOutcome, Report<TrustedServerError>> {
    #[cfg(test)]
    if let Some(provider) = mediator.legacy_test_provider() {
        return launch_legacy_test_mediator(provider, request, context).await;
    }
    let exchange = mediator.prepare(MediatorPreparation {
        request,
        provider_responses: context.provider_responses.unwrap_or(&[]),
        logical_budget_ms: context.timeout_ms,
        transport_budget_ms: context.transport_timeout_ms,
    })?;
    let name = context
        .services
        .backend()
        .ensure(&exchange.backend)
        .change_context(TrustedServerError::Auction {
            message: format!("Mediator {} backend registration failed", mediator.id()),
        })?;
    let request = context
        .services
        .http_client()
        .send_async(PlatformHttpRequest::new(exchange.request, name).with_cache_bypass())
        .await
        .change_context(TrustedServerError::Auction {
            message: format!("Mediator {} request launch failed", mediator.id()),
        })?;
    Ok(MediatorRequestOutcome::Pending {
        request,
        parser: exchange.parser,
    })
}

#[cfg(test)]
struct LegacyTestMediator {
    provider: Arc<dyn super::provider::AuctionProvider>,
}
#[cfg(test)]
impl CompiledMediator for LegacyTestMediator {
    fn id(&self) -> &str {
        self.provider.provider_name()
    }
    fn timeout_ms(&self) -> u32 {
        self.provider.timeout_ms()
    }
    fn prepare(
        &self,
        _: MediatorPreparation<'_>,
    ) -> Result<PreparedMediatorExchange, Report<TrustedServerError>> {
        unreachable!("legacy orchestration fixtures dispatch only through their test adapter")
    }
    fn legacy_test_provider(&self) -> Option<Arc<dyn super::provider::AuctionProvider>> {
        Some(Arc::clone(&self.provider))
    }
}

#[cfg(test)]
pub(crate) fn legacy_test_mediator(
    provider: Arc<dyn super::provider::AuctionProvider>,
) -> Arc<dyn CompiledMediator> {
    Arc::new(LegacyTestMediator { provider })
}

#[cfg(test)]
struct LegacyTestParser {
    provider: Arc<dyn super::provider::AuctionProvider>,
    request: AuctionRequest,
    settings: crate::settings::Settings,
    services: crate::platform::RuntimeServices,
    logical_budget_ms: u32,
    transport_budget_ms: u32,
    responses: Vec<AuctionResponse>,
    state: Option<super::provider::ProviderParseState>,
}
#[cfg(test)]
#[async_trait(?Send)]
impl BoundMediatorResponseParser for LegacyTestParser {
    async fn parse(
        self: Box<Self>,
        response: PlatformResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let placeholder = Request::builder()
            .uri(super::types::MEDIATOR_PLACEHOLDER_URL)
            .body(Body::empty())
            .expect("should build test mediator placeholder");
        let context = AuctionContext {
            settings: &self.settings,
            request: &placeholder,
            timeout_ms: self.logical_budget_ms,
            transport_timeout_ms: self.transport_budget_ms,
            provider_responses: Some(&self.responses),
            services: &self.services,
        };
        self.provider
            .parse_response_with_context_and_state(
                response,
                response_time_ms,
                &self.request,
                &context,
                self.state.as_deref(),
            )
            .await
    }
}
#[cfg(test)]
async fn launch_legacy_test_mediator(
    provider: Arc<dyn super::provider::AuctionProvider>,
    request: &AuctionRequest,
    context: &AuctionContext<'_>,
) -> Result<MediatorRequestOutcome, Report<TrustedServerError>> {
    match provider.request_bids(request, context).await? {
        super::provider::ProviderRequestOutcome::Immediate(response) => {
            Ok(MediatorRequestOutcome::Immediate(response))
        }
        super::provider::ProviderRequestOutcome::Pending {
            request: pending,
            parse_state,
        } => Ok(MediatorRequestOutcome::Pending {
            request: pending,
            parser: Box::new(LegacyTestParser {
                provider,
                request: request.clone(),
                settings: context.settings.clone(),
                services: context.services.clone(),
                logical_budget_ms: context.timeout_ms,
                transport_budget_ms: context.transport_timeout_ms,
                responses: context.provider_responses.unwrap_or(&[]).to_vec(),
                state: parse_state,
            }),
        }),
    }
}
