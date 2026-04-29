use std::sync::LazyLock;

use fastly::Request;

use super::AuctionContext;
use crate::platform::{test_support::noop_services, ClientInfo, RuntimeServices};
use crate::settings::Settings;

static TEST_SERVICES: LazyLock<RuntimeServices> = LazyLock::new(noop_services);

pub(crate) fn create_test_auction_context<'a>(
    settings: &'a Settings,
    request: &'a Request,
    client_info: &'a ClientInfo,
    timeout_ms: u32,
) -> AuctionContext<'a> {
    let services: &'static RuntimeServices = &TEST_SERVICES;
    AuctionContext {
        settings,
        request,
        client_info,
        timeout_ms,
        provider_responses: None,
        services,
    }
}
