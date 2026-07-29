use std::sync::LazyLock;

use edgezero_core::body::Body as EdgeBody;
use http::Request;

use super::AuctionContext;
use crate::platform::{RuntimeServices, test_support::noop_services};
use crate::settings::Settings;

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
