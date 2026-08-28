use std::sync::LazyLock;

use edgezero_core::body::Body as EdgeBody;
use http::Request;

use error_stack::Report;

use super::provider::{AuctionProvider, ProviderRequestOutcome};
use super::types::{AuctionContext, AuctionRequest, AuctionResponse};
use crate::error::TrustedServerError;
use crate::platform::{PlatformResponse, RuntimeServices, test_support::noop_services};
use crate::settings::Settings;

/// Timeout the named test provider reports; no test asserts on the value.
const NAMED_TEST_PROVIDER_TIMEOUT_MS: u32 = 2000;

static TEST_SERVICES: LazyLock<RuntimeServices> = LazyLock::new(noop_services);

pub(crate) fn create_test_auction_context<'a>(
    settings: &'a Settings,
    request: &'a Request<EdgeBody>,
    timeout_ms: u32,
) -> AuctionContext<'a> {
    let services: &'static RuntimeServices = &TEST_SERVICES;
    AuctionContext {
        settings,
        request,
        timeout_ms,
        provider_responses: None,
        services,
    }
}

/// A provider that reports the name it was constructed with and answers every
/// bid request immediately, for tests that only need a named registration.
pub(crate) struct NamedTestProvider {
    name: &'static str,
}

impl NamedTestProvider {
    /// Creates a provider that reports `name` as its provider name.
    pub(crate) const fn new(name: &'static str) -> Self {
        Self { name }
    }
}

#[async_trait::async_trait(?Send)]
impl AuctionProvider for NamedTestProvider {
    fn provider_name(&self) -> &'static str {
        self.name
    }

    async fn request_bids(
        &self,
        _request: &AuctionRequest,
        _context: &AuctionContext<'_>,
    ) -> Result<ProviderRequestOutcome, Report<TrustedServerError>> {
        Ok(ProviderRequestOutcome::Immediate(AuctionResponse::success(
            self.name,
            vec![],
            0,
        )))
    }

    async fn parse_response(
        &self,
        _response: PlatformResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        Ok(AuctionResponse::success(
            self.name,
            vec![],
            response_time_ms,
        ))
    }

    fn timeout_ms(&self) -> u32 {
        NAMED_TEST_PROVIDER_TIMEOUT_MS
    }
}
