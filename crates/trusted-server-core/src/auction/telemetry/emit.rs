//! Wiring helper that emits completed-auction telemetry from a handler.
//!
//! Reads geo and consent off the `AuctionRequest` (a handler's local copies may
//! have been moved). Device signals are unknown (`2`) until a later plan threads
//! them. The sink write is buffered/non-blocking in production.

use crate::auction::orchestrator::OrchestrationResult;
use crate::auction::telemetry::context::build_observation_context;
use crate::auction::telemetry::mapping::build_completed_auction_events;
use crate::auction::telemetry::types::AuctionSource;
use crate::auction::types::AuctionRequest;
use crate::ec::device::DeviceSignals;
use crate::platform::RuntimeServices;

/// Build and emit completed-auction telemetry for a finished auction.
pub fn emit_completed_auction_telemetry(
    services: &RuntimeServices,
    source: AuctionSource,
    request: &AuctionRequest,
    result: &OrchestrationResult,
) {
    let user_agent = request
        .device
        .as_ref()
        .and_then(|device| device.user_agent.as_deref())
        .unwrap_or("");
    let client_info = services.client_info();
    let signals = DeviceSignals::derive(
        user_agent,
        client_info.tls_ja4.as_deref(),
        client_info.h2_fingerprint.as_deref(),
    );
    // Map the optional browser-legitimacy bit to the 0/1/2 schema column.
    let is_known_browser = match signals.known_browser {
        Some(true) => 1,
        Some(false) => 0,
        None => 2,
    };
    let observation = build_observation_context(
        source,
        &request.publisher.domain,
        request.publisher.page_url.as_deref(),
        request
            .device
            .as_ref()
            .and_then(|device| device.geo.as_ref()),
        request.user.consent.as_ref(),
        signals.is_mobile,
        is_known_browser,
    );
    let slot_count = u16::try_from(request.slots.len()).unwrap_or(u16::MAX);
    let rows = build_completed_auction_events(&observation, slot_count, result);
    services.auction_event_sink().emit(&rows);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::telemetry::{EventKind, InMemorySink};
    use crate::auction::types::{DeviceInfo, PublisherInfo, UserInfo};
    use crate::platform::test_support::noop_services;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn request() -> AuctionRequest {
        AuctionRequest {
            id: "internal-id".to_string(),
            slots: vec![],
            publisher: PublisherInfo {
                domain: "example.com".to_string(),
                page_url: Some("https://example.com/news?x=1".to_string()),
            },
            user: UserInfo {
                id: None,
                consent: None,
                eids: None,
            },
            device: None,
            site: None,
            context: HashMap::new(),
        }
    }

    fn empty_result() -> OrchestrationResult {
        OrchestrationResult {
            provider_responses: vec![],
            mediator_response: None,
            winning_bids: HashMap::new(),
            total_time_ms: 0,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn emits_one_summary_tagged_with_the_given_source() {
        let sink = Arc::new(InMemorySink::default());
        let services = noop_services().with_auction_event_sink(sink.clone());

        emit_completed_auction_telemetry(
            &services,
            AuctionSource::SpaNavigation,
            &request(),
            &empty_result(),
        );

        let rows = sink.rows();
        let summary = rows
            .iter()
            .find(|r| r.event_kind == EventKind::Summary)
            .expect("should emit a summary row");
        assert_eq!(
            summary.auction_source,
            AuctionSource::SpaNavigation,
            "should tag the summary with the given source"
        );
        assert_eq!(
            summary.publisher_domain, "example.com",
            "should carry the publisher domain"
        );
        assert_eq!(
            summary.page_path, "/news",
            "should carry the normalized page path"
        );
    }

    #[test]
    fn derives_is_mobile_from_user_agent() {
        let sink = Arc::new(InMemorySink::default());
        let services = noop_services().with_auction_event_sink(sink.clone());
        let mut req = request();
        req.device = Some(DeviceInfo {
            user_agent: Some(
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15"
                    .to_string(),
            ),
            ip: None,
            geo: None,
        });

        emit_completed_auction_telemetry(
            &services,
            AuctionSource::AuctionApi,
            &req,
            &empty_result(),
        );

        let rows = sink.rows();
        let summary = rows
            .iter()
            .find(|r| r.event_kind == EventKind::Summary)
            .expect("should emit a summary row");
        assert_eq!(
            summary.is_mobile, 1,
            "an iPhone user agent should classify as mobile"
        );
        assert_eq!(
            summary.is_known_browser, 2,
            "with no JA4/H2 fingerprint the browser-legitimacy signal is unknown"
        );
    }
}
