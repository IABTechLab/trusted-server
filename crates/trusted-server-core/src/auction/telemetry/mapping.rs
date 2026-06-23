//! Maps a real `OrchestrationResult` into telemetry inputs.
//!
//! This is the adapter between the orchestrator's output types and the pure
//! telemetry builder. It performs no I/O and does not modify the auction.

use crate::auction::orchestrator::OrchestrationResult;
use crate::auction::telemetry::types::{
    ProviderCallOutcome, ProviderCallStatus, ProviderRole,
};
use crate::auction::types::{AuctionResponse, BidStatus};

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
/// orchestrator's `error_type` metadata; an unrecognized or absent value falls
/// back to `TransportError` since the orchestrator only emits the three known
/// error types.
fn provider_call_status(response: &AuctionResponse) -> ProviderCallStatus {
    match response.status {
        BidStatus::Success => ProviderCallStatus::Success,
        BidStatus::NoBid => ProviderCallStatus::NoBid,
        BidStatus::Pending => ProviderCallStatus::Timeout,
        BidStatus::Error => match response
            .metadata
            .get("error_type")
            .and_then(|value| value.as_str())
        {
            Some("launch_failed") => ProviderCallStatus::LaunchError,
            Some("parse_response") => ProviderCallStatus::ParseError,
            Some("transport") => ProviderCallStatus::TransportError,
            _ => ProviderCallStatus::TransportError,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::orchestrator::OrchestrationResult;
    use crate::auction::types::{AuctionResponse, Bid, BidStatus};
    use crate::auction::telemetry::types::{ProviderCallStatus, ProviderRole};
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
            metadata.insert("error_type".to_string(), serde_json::json!(kind));
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

    #[test]
    fn maps_each_status_to_the_expected_provider_call_status() {
        let res = result(
            vec![
                response("prebid", BidStatus::Success, 40, vec![bid("s1", "kargo")], None),
                response("rubicon", BidStatus::NoBid, 30, vec![], None),
                response("ix", BidStatus::Error, 10, vec![], Some("launch_failed")),
                response("appnexus", BidStatus::Error, 55, vec![], Some("parse_response")),
                response("openx", BidStatus::Error, 60, vec![], Some("transport")),
                response("smaato", BidStatus::Error, 5, vec![], None),
                response("teads", BidStatus::Pending, 70, vec![], None),
            ],
            None,
        );

        let calls = provider_calls_from_result(&res);

        assert_eq!(calls.len(), 7, "should emit one outcome per provider response");
        assert_eq!(calls[0].status, ProviderCallStatus::Success, "Success maps to Success");
        assert_eq!(calls[0].bid_count, Some(1), "should count returned bids");
        assert_eq!(calls[0].response_time_ms, Some(40), "should carry response time");
        assert_eq!(calls[0].role, ProviderRole::Bidder, "provider responses are bidders");
        assert_eq!(calls[1].status, ProviderCallStatus::NoBid, "NoBid maps to NoBid");
        assert_eq!(calls[2].status, ProviderCallStatus::LaunchError, "launch_failed maps to LaunchError");
        assert_eq!(calls[3].status, ProviderCallStatus::ParseError, "parse_response maps to ParseError");
        assert_eq!(calls[4].status, ProviderCallStatus::TransportError, "transport maps to TransportError");
        assert_eq!(
            calls[5].status,
            ProviderCallStatus::TransportError,
            "an Error with no recognized error_type falls back to TransportError"
        );
        assert_eq!(calls[6].status, ProviderCallStatus::Timeout, "Pending maps to Timeout");
    }

    #[test]
    fn appends_a_mediator_outcome_when_present() {
        let res = result(
            vec![response("prebid", BidStatus::Success, 40, vec![bid("s1", "kargo")], None)],
            Some(response("mediator", BidStatus::Success, 12, vec![], None)),
        );

        let calls = provider_calls_from_result(&res);

        assert_eq!(calls.len(), 2, "should append one outcome for the mediator");
        let mediator = calls.last().expect("should have a mediator outcome");
        assert_eq!(mediator.role, ProviderRole::Mediator, "mediator outcome uses the Mediator role");
        assert_eq!(mediator.provider, "mediator", "should carry the mediator provider name");
    }
}
