//! Maps a real `OrchestrationResult` into telemetry inputs.
//!
//! This is the adapter between the orchestrator's output types and the pure
//! telemetry builder. It performs no I/O and does not modify the auction.

use crate::auction::orchestrator::OrchestrationResult;
use crate::auction::telemetry::builder::build_auction_events;
use crate::auction::telemetry::types::{
    AuctionEventRow, AuctionObservationContext, ProviderCallOutcome, ProviderCallStatus,
    ProviderRole, TerminalOutcome, TerminalStatus,
};
use crate::auction::types::{AuctionResponse, BidStatus, ProviderErrorType};

/// Build one provider-call outcome per provider response, plus one for the
/// mediator when a mediator response is present.
#[must_use]
pub fn provider_calls_from_result(result: &OrchestrationResult) -> Vec<ProviderCallOutcome> {
    let mut calls: Vec<ProviderCallOutcome> = result
        .provider_responses
        .iter()
        .map(|response| provider_call_outcome(response, ProviderRole::Bidder))
        .collect();
    if let Some(mediator) = &result.mediator_response {
        calls.push(provider_call_outcome(mediator, ProviderRole::Mediator));
    }
    calls
}

/// Map one response to a provider-call outcome with the given role.
fn provider_call_outcome(response: &AuctionResponse, role: ProviderRole) -> ProviderCallOutcome {
    ProviderCallOutcome {
        provider: response.provider.clone(),
        role,
        status: provider_call_status(response),
        response_time_ms: Some(clamp_u32(response.response_time_ms)),
        bid_count: Some(clamp_u16(response.bids.len())),
    }
}

/// Classify a response into a provider-call status. For `Error`, read the
/// orchestrator's provider error type metadata; an unrecognized or absent value
/// falls back to `TransportError`.
fn provider_call_status(response: &AuctionResponse) -> ProviderCallStatus {
    match response.status {
        BidStatus::Success => ProviderCallStatus::Success,
        BidStatus::NoBid => ProviderCallStatus::NoBid,
        BidStatus::Pending => ProviderCallStatus::Timeout,
        BidStatus::Error => match response.provider_error_type() {
            Some(ProviderErrorType::LaunchFailed) => ProviderCallStatus::LaunchError,
            Some(ProviderErrorType::ParseResponse) => ProviderCallStatus::ParseError,
            Some(ProviderErrorType::Transport) => ProviderCallStatus::TransportError,
            Some(ProviderErrorType::Timeout) => ProviderCallStatus::Timeout,
            None => ProviderCallStatus::TransportError,
        },
    }
}

/// Clamp a `u64` millisecond count into the `u32` schema column without
/// panicking.
fn clamp_u32(value: u64) -> u32 {
    value.min(u64::from(u32::MAX)) as u32
}

/// Clamp a count into the `u16` schema column without panicking.
fn clamp_u16(value: usize) -> u16 {
    value.min(usize::from(u16::MAX)) as u16
}

/// Build the terminal outcome for a completed auction. `slot_count` is the
/// number of requested slots, which the result alone does not carry.
#[must_use]
pub fn completed_outcome(result: &OrchestrationResult, slot_count: u16) -> TerminalOutcome {
    TerminalOutcome {
        status: TerminalStatus::Completed,
        reason: None,
        slot_count: Some(slot_count),
        total_time_ms: Some(clamp_u32(result.total_time_ms)),
        winning_bid_count: Some(clamp_u16(result.winning_bids.len())),
    }
}

/// Build all telemetry rows for a completed auction. This is the single entry
/// point a wiring layer calls when `run_auction`/`collect_dispatched_auction`
/// returns an `OrchestrationResult`.
#[must_use]
pub fn build_completed_auction_events(
    ctx: &AuctionObservationContext,
    slot_count: u16,
    result: &OrchestrationResult,
) -> Vec<AuctionEventRow> {
    let outcome = completed_outcome(result, slot_count);
    let provider_calls = provider_calls_from_result(result);
    build_auction_events(ctx, &outcome, &provider_calls, Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::orchestrator::OrchestrationResult;
    use crate::auction::telemetry::types::{
        AuctionObservationContext, AuctionSource, EventKind, ProviderCallStatus, ProviderRole,
        TerminalStatus,
    };
    use crate::auction::types::{AuctionResponse, Bid, BidStatus};
    use std::collections::HashMap;

    fn bid(slot: &str, bidder: &str) -> Bid {
        Bid {
            slot_id: slot.to_string(),
            price: Some(1.0),
            currency: "USD".to_string(),
            creative: None,
            adomain: None,
            bidder: bidder.to_string(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            ad_id: None,
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: HashMap::new(),
        }
    }

    fn response(
        provider: &str,
        status: BidStatus,
        time: u64,
        bids: Vec<Bid>,
        error_type: Option<&str>,
    ) -> AuctionResponse {
        let mut metadata = HashMap::new();
        if let Some(kind) = error_type {
            metadata.insert(
                ProviderErrorType::METADATA_KEY.to_string(),
                serde_json::json!(kind),
            );
        }
        AuctionResponse {
            provider: provider.to_string(),
            bids,
            status,
            response_time_ms: time,
            metadata,
        }
    }

    fn result(
        provider_responses: Vec<AuctionResponse>,
        mediator_response: Option<AuctionResponse>,
    ) -> OrchestrationResult {
        OrchestrationResult {
            provider_responses,
            mediator_response,
            winning_bids: HashMap::new(),
            total_time_ms: 0,
            metadata: HashMap::new(),
        }
    }

    fn ctx() -> AuctionObservationContext {
        AuctionObservationContext {
            auction_id: uuid::Uuid::nil(),
            source: AuctionSource::AuctionApi,
            publisher_domain: "example.com".to_string(),
            page_path: "/p".to_string(),
            country: "US".to_string(),
            region: None,
            is_mobile: 1,
            is_known_browser: 1,
            gdpr_applies: false,
            consent_present: true,
        }
    }

    #[test]
    fn maps_each_status_to_the_expected_provider_call_status() {
        let res = result(
            vec![
                response(
                    "prebid",
                    BidStatus::Success,
                    40,
                    vec![bid("s1", "kargo")],
                    None,
                ),
                response("rubicon", BidStatus::NoBid, 30, vec![], None),
                response("ix", BidStatus::Error, 10, vec![], Some("launch_failed")),
                response(
                    "appnexus",
                    BidStatus::Error,
                    55,
                    vec![],
                    Some("parse_response"),
                ),
                response("openx", BidStatus::Error, 60, vec![], Some("transport")),
                response("smaato", BidStatus::Error, 5, vec![], None),
                response("teads", BidStatus::Pending, 70, vec![], None),
                response(
                    "timeout-bidder",
                    BidStatus::Error,
                    80,
                    vec![],
                    Some("timeout"),
                ),
            ],
            None,
        );

        let calls = provider_calls_from_result(&res);

        assert_eq!(
            calls.len(),
            8,
            "should emit one outcome per provider response"
        );
        assert_eq!(
            calls[0].status,
            ProviderCallStatus::Success,
            "Success maps to Success"
        );
        assert_eq!(calls[0].bid_count, Some(1), "should count returned bids");
        assert_eq!(
            calls[0].response_time_ms,
            Some(40),
            "should carry response time"
        );
        assert_eq!(
            calls[0].role,
            ProviderRole::Bidder,
            "provider responses are bidders"
        );
        assert_eq!(
            calls[1].status,
            ProviderCallStatus::NoBid,
            "NoBid maps to NoBid"
        );
        assert_eq!(
            calls[2].status,
            ProviderCallStatus::LaunchError,
            "launch_failed maps to LaunchError"
        );
        assert_eq!(
            calls[3].status,
            ProviderCallStatus::ParseError,
            "parse_response maps to ParseError"
        );
        assert_eq!(
            calls[4].status,
            ProviderCallStatus::TransportError,
            "transport maps to TransportError"
        );
        assert_eq!(
            calls[5].status,
            ProviderCallStatus::TransportError,
            "an Error with no recognized error_type falls back to TransportError"
        );
        assert_eq!(
            calls[6].status,
            ProviderCallStatus::Timeout,
            "Pending maps to Timeout"
        );
        assert_eq!(
            calls[7].status,
            ProviderCallStatus::Timeout,
            "timeout error metadata maps to Timeout"
        );
    }

    #[test]
    fn appends_a_mediator_outcome_when_present() {
        let res = result(
            vec![response(
                "prebid",
                BidStatus::Success,
                40,
                vec![bid("s1", "kargo")],
                None,
            )],
            Some(response("mediator", BidStatus::Success, 12, vec![], None)),
        );

        let calls = provider_calls_from_result(&res);

        assert_eq!(calls.len(), 2, "should append one outcome for the mediator");
        let mediator = calls.last().expect("should have a mediator outcome");
        assert_eq!(
            mediator.role,
            ProviderRole::Mediator,
            "mediator outcome uses the Mediator role"
        );
        assert_eq!(
            mediator.provider, "mediator",
            "should carry the mediator provider name"
        );
    }

    #[test]
    fn completed_outcome_carries_counts_from_the_result() {
        let mut res = result(
            vec![response(
                "prebid",
                BidStatus::Success,
                40,
                vec![bid("s1", "kargo")],
                None,
            )],
            None,
        );
        res.total_time_ms = 88;
        res.winning_bids
            .insert("s1".to_string(), bid("s1", "kargo"));

        let outcome = completed_outcome(&res, 2);

        assert_eq!(
            outcome.status,
            TerminalStatus::Completed,
            "should be Completed"
        );
        assert!(
            outcome.reason.is_none(),
            "completed auctions have no reason"
        );
        assert_eq!(
            outcome.slot_count,
            Some(2),
            "should carry the requested slot count"
        );
        assert_eq!(outcome.total_time_ms, Some(88), "should carry total time");
        assert_eq!(
            outcome.winning_bid_count,
            Some(1),
            "should count winning bids"
        );
    }

    #[test]
    fn build_completed_auction_events_emits_summary_provider_and_bid_rows() {
        let mut res = result(
            vec![
                response(
                    "prebid",
                    BidStatus::Success,
                    40,
                    vec![bid("s1", "kargo")],
                    None,
                ),
                response("aps", BidStatus::NoBid, 30, vec![], None),
            ],
            None,
        );
        res.winning_bids
            .insert("s1".to_string(), bid("s1", "kargo"));

        let rows = build_completed_auction_events(&ctx(), 1, &res);

        assert_eq!(
            rows.iter()
                .filter(|r| r.event_kind == EventKind::Summary)
                .count(),
            1,
            "should emit exactly one summary"
        );
        assert_eq!(
            rows.iter()
                .filter(|r| r.event_kind == EventKind::ProviderCall)
                .count(),
            2,
            "should emit one provider-call row per provider"
        );
        assert_eq!(
            rows.iter()
                .filter(|r| r.event_kind == EventKind::Bid)
                .count(),
            1,
            "should emit a bid row for the returned bid"
        );
        let summary = rows
            .iter()
            .find(|r| r.event_kind == EventKind::Summary)
            .expect("should have a summary row");
        assert_eq!(
            summary.terminal_status,
            Some(TerminalStatus::Completed),
            "summary is Completed"
        );
    }
}
