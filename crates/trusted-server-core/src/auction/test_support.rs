use std::sync::LazyLock;

use edgezero_core::body::Body as EdgeBody;
use http::Request;

use super::{AuctionContext, AuctionSource};
use crate::auction::types::AuctionTraceContext;
use crate::platform::{RuntimeServices, test_support::noop_services};
use crate::settings::Settings;

static TEST_SERVICES: LazyLock<RuntimeServices> = LazyLock::new(noop_services);
static TEST_TRACE: LazyLock<AuctionTraceContext> =
    LazyLock::new(|| AuctionTraceContext::new(AuctionSource::AuctionApi));

pub(crate) fn test_trace() -> &'static AuctionTraceContext {
    &TEST_TRACE
}

pub(crate) fn create_test_auction_context<'a>(
    settings: &'a Settings,
    request: &'a Request<EdgeBody>,
    timeout_ms: u32,
) -> AuctionContext<'a> {
    let services: &'static RuntimeServices = &TEST_SERVICES;
    AuctionContext {
        trace: test_trace(),
        settings,
        request,
        timeout_ms,
        provider_responses: None,
        services,
    }
}
