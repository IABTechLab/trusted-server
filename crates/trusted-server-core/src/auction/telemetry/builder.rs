//! Pure builder that turns an auction observation into telemetry rows.

use std::collections::HashSet;

use crate::auction::orchestrator::OrchestrationResult;
use crate::auction::telemetry::types::{
    AuctionEventRow, AuctionObservationContext, EventKind, ProviderCallOutcome, TerminalOutcome,
};
use crate::auction::types::Bid;

/// Build all telemetry rows for one auction observation.
///
/// Always emits exactly one summary row, one provider-call row per entry in
/// `provider_calls`, and (when `result` is `Some`) one bid row per returned bid
/// plus one row for any winning slot not matched to a returned bid.
#[must_use]
pub fn build_auction_events(
    ctx: &AuctionObservationContext,
    outcome: &TerminalOutcome,
    provider_calls: &[ProviderCallOutcome],
    result: Option<&OrchestrationResult>,
) -> Vec<AuctionEventRow> {
    let mut rows = Vec::new();
    rows.push(summary_row(ctx, outcome));
    for call in provider_calls {
        rows.push(provider_call_row(ctx, call));
    }
    if let Some(result) = result {
        rows.extend(build_bid_rows(ctx, result));
    }
    rows
}

/// Build the single summary row.
fn summary_row(ctx: &AuctionObservationContext, outcome: &TerminalOutcome) -> AuctionEventRow {
    let mut row = AuctionEventRow::base(ctx, EventKind::Summary);
    row.terminal_status = Some(outcome.status);
    row.terminal_reason = outcome.reason.clone();
    row.slot_count = outcome.slot_count;
    row.total_time_ms = outcome.total_time_ms;
    row.winning_bid_count = outcome.winning_bid_count;
    row
}

/// Build one provider-call row.
fn provider_call_row(
    ctx: &AuctionObservationContext,
    call: &ProviderCallOutcome,
) -> AuctionEventRow {
    let mut row = AuctionEventRow::base(ctx, EventKind::ProviderCall);
    row.provider = Some(call.provider.clone());
    row.provider_role = Some(call.role);
    row.status = Some(call.status);
    row.provider_response_time_ms = call.response_time_ms;
    row.provider_bid_count = call.bid_count;
    row
}

/// Build bid rows from a completed orchestration result.
fn build_bid_rows(
    ctx: &AuctionObservationContext,
    result: &OrchestrationResult,
) -> Vec<AuctionEventRow> {
    let mut rows = Vec::new();
    // Slots whose winning row has already been emitted, so each slot has at
    // most one `is_win = 1` row.
    let mut claimed_slots = HashSet::new();

    for response in &result.provider_responses {
        for bid in &response.bids {
            let winner = result.winning_bids.get(&bid.slot_id);
            let is_win = match winner {
                Some(winner) => {
                    matches_winner(bid, winner) && !claimed_slots.contains(&bid.slot_id)
                }
                None => false,
            };
            let price_override = if is_win && bid.price.is_none() {
                winner.and_then(|winner| winner.price)
            } else {
                None
            };
            if is_win {
                claimed_slots.insert(bid.slot_id.clone());
            }
            rows.push(bid_row(
                ctx,
                &response.provider,
                bid,
                is_win,
                price_override,
            ));
        }
    }

    // Any winning slot not matched to a returned bid gets one canonical
    // mediator-derived winner row.
    let mediator_provider = result
        .mediator_response
        .as_ref()
        .map(|response| response.provider.clone())
        .unwrap_or_else(|| "mediator".to_string());
    for (slot_id, winner) in &result.winning_bids {
        if !claimed_slots.contains(slot_id) {
            rows.push(bid_row(ctx, &mediator_provider, winner, true, winner.price));
        }
    }

    rows
}

/// Whether a returned bid is the winner for its slot.
fn matches_winner(candidate: &Bid, winner: &Bid) -> bool {
    if candidate.slot_id != winner.slot_id || candidate.bidder != winner.bidder {
        return false;
    }
    match (&candidate.ad_id, &winner.ad_id) {
        (Some(left), Some(right)) => left == right,
        // Fall back to (slot, seat) identity when ad ids are absent.
        _ => true,
    }
}

/// Build one bid row. `price_override` carries a mediator-decoded price for a
/// winning bid whose own price is null.
fn bid_row(
    ctx: &AuctionObservationContext,
    provider: &str,
    bid: &Bid,
    is_win: bool,
    price_override: Option<f64>,
) -> AuctionEventRow {
    let mut row = AuctionEventRow::base(ctx, EventKind::Bid);
    row.provider = Some(provider.to_string());
    row.slot_id = Some(bid.slot_id.clone());
    row.slot_w = Some(clamp_dimension(bid.width));
    row.slot_h = Some(clamp_dimension(bid.height));
    row.seat = Some(bid.bidder.clone());
    row.price_cpm = price_override.or(bid.price);
    row.currency = Some(bid.currency.clone());
    row.is_win = Some(u8::from(is_win));
    row.ad_domain = bid
        .adomain
        .as_ref()
        .and_then(|domains| domains.first().cloned());
    row.ad_id = bid.ad_id.clone();
    row
}

/// Clamp a `u32` creative dimension into the `u16` schema column without
/// panicking. Real creative sizes are always well within `u16`.
fn clamp_dimension(value: u32) -> u16 {
    value.min(u32::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::telemetry::types::{
        AuctionObservationContext, AuctionSource, EventKind, ProviderCallOutcome,
        ProviderCallStatus, ProviderRole, TerminalOutcome, TerminalStatus,
    };
    use crate::auction::types::{AuctionResponse, Bid, BidStatus};
    use std::collections::HashMap;

    fn ctx(source: AuctionSource) -> AuctionObservationContext {
        AuctionObservationContext {
            auction_id: uuid::Uuid::nil(),
            source,
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

    fn bid(slot: &str, bidder: &str, price: Option<f64>, ad_id: Option<&str>) -> Bid {
        Bid {
            slot_id: slot.to_string(),
            price,
            currency: "USD".to_string(),
            creative: None,
            adomain: Some(vec!["advertiser.example".to_string()]),
            bidder: bidder.to_string(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            ad_id: ad_id.map(str::to_string),
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: HashMap::new(),
        }
    }

    fn response(provider: &str, bids: Vec<Bid>, status: BidStatus) -> AuctionResponse {
        AuctionResponse {
            provider: provider.to_string(),
            bids,
            status,
            response_time_ms: 42,
            metadata: HashMap::new(),
        }
    }

    fn completed_outcome() -> TerminalOutcome {
        TerminalOutcome {
            status: TerminalStatus::Completed,
            reason: None,
            slot_count: Some(1),
            total_time_ms: Some(50),
            winning_bid_count: Some(1),
        }
    }

    #[test]
    fn abandoned_auction_emits_summary_plus_provider_calls_no_bids() {
        let outcome = TerminalOutcome {
            status: TerminalStatus::Abandoned,
            reason: Some("origin_unrewritable".to_string()),
            slot_count: Some(2),
            total_time_ms: Some(120),
            winning_bid_count: Some(0),
        };
        let calls = vec![
            ProviderCallOutcome {
                provider: "prebid".to_string(),
                role: ProviderRole::Bidder,
                status: ProviderCallStatus::Abandoned,
                response_time_ms: None,
                bid_count: None,
            },
            ProviderCallOutcome {
                provider: "aps".to_string(),
                role: ProviderRole::Bidder,
                status: ProviderCallStatus::Abandoned,
                response_time_ms: None,
                bid_count: None,
            },
        ];

        let rows = build_auction_events(
            &ctx(AuctionSource::InitialNavigation),
            &outcome,
            &calls,
            None,
        );

        let summaries: Vec<_> = rows
            .iter()
            .filter(|r| r.event_kind == EventKind::Summary)
            .collect();
        assert_eq!(summaries.len(), 1, "should emit exactly one summary row");
        assert_eq!(
            summaries[0].terminal_status,
            Some(TerminalStatus::Abandoned),
            "should record the terminal status on the summary"
        );
        assert_eq!(
            rows.iter()
                .filter(|r| r.event_kind == EventKind::ProviderCall)
                .count(),
            2,
            "should emit one provider-call row per outcome"
        );
        assert_eq!(
            rows.iter()
                .filter(|r| r.event_kind == EventKind::Bid)
                .count(),
            0,
            "should emit no bid rows when there is no result"
        );
    }

    #[test]
    fn skipped_auction_emits_only_a_summary() {
        let outcome = TerminalOutcome {
            status: TerminalStatus::Skipped,
            reason: Some("consent".to_string()),
            slot_count: Some(3),
            total_time_ms: None,
            winning_bid_count: Some(0),
        };
        let rows = build_auction_events(&ctx(AuctionSource::AuctionApi), &outcome, &[], None);
        assert_eq!(rows.len(), 1, "should emit only the summary row");
        assert_eq!(
            rows[0].event_kind,
            EventKind::Summary,
            "should be a summary"
        );
        assert_eq!(
            rows[0].terminal_reason.as_deref(),
            Some("consent"),
            "should carry the reason"
        );
    }

    #[test]
    fn emits_one_bid_row_per_returned_bid_with_single_winner() {
        let winner = bid("slot-1", "kargo", Some(2.5), Some("creative-1"));
        let mut winning_bids = HashMap::new();
        winning_bids.insert("slot-1".to_string(), winner.clone());
        let result = OrchestrationResult {
            provider_responses: vec![response(
                "prebid",
                vec![
                    bid("slot-1", "kargo", Some(2.5), Some("creative-1")),
                    bid("slot-1", "ix", Some(1.0), Some("creative-2")),
                ],
                BidStatus::Success,
            )],
            mediator_response: None,
            winning_bids,
            total_time_ms: 50,
            metadata: HashMap::new(),
        };

        let rows = build_auction_events(
            &ctx(AuctionSource::AuctionApi),
            &completed_outcome(),
            &[],
            Some(&result),
        );
        let bid_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.event_kind == EventKind::Bid)
            .collect();

        assert_eq!(bid_rows.len(), 2, "should emit one row per returned bid");
        assert_eq!(
            bid_rows.iter().filter(|r| r.is_win == Some(1)).count(),
            1,
            "should mark exactly one winning row per slot"
        );
        let winning = bid_rows
            .iter()
            .find(|r| r.is_win == Some(1))
            .expect("should have a winner");
        assert_eq!(
            winning.seat.as_deref(),
            Some("kargo"),
            "should win for the matched seat"
        );
    }

    #[test]
    fn fills_decoded_price_on_null_priced_winner() {
        let winner = bid("slot-1", "aps", Some(3.1), Some("amzn-1"));
        let mut winning_bids = HashMap::new();
        winning_bids.insert("slot-1".to_string(), winner);
        let result = OrchestrationResult {
            // The original APS bid has no decoded price.
            provider_responses: vec![response(
                "aps",
                vec![bid("slot-1", "aps", None, Some("amzn-1"))],
                BidStatus::Success,
            )],
            mediator_response: Some(response("mediator", vec![], BidStatus::Success)),
            winning_bids,
            total_time_ms: 60,
            metadata: HashMap::new(),
        };

        let rows = build_auction_events(
            &ctx(AuctionSource::AuctionApi),
            &completed_outcome(),
            &[],
            Some(&result),
        );
        let winning = rows
            .iter()
            .find(|r| r.event_kind == EventKind::Bid && r.is_win == Some(1))
            .expect("should have a winning bid row");
        assert_eq!(
            winning.price_cpm,
            Some(3.1),
            "should fill decoded winner price on a null-priced bid"
        );
    }

    #[test]
    fn unmatched_winner_emits_one_mediator_row() {
        let winner = bid("slot-9", "exclusive-seat", Some(5.0), Some("only-here"));
        let mut winning_bids = HashMap::new();
        winning_bids.insert("slot-9".to_string(), winner);
        let result = OrchestrationResult {
            // No provider response contains the winning bid.
            provider_responses: vec![response("prebid", vec![], BidStatus::NoBid)],
            mediator_response: Some(response("mediator", vec![], BidStatus::Success)),
            winning_bids,
            total_time_ms: 70,
            metadata: HashMap::new(),
        };

        let rows = build_auction_events(
            &ctx(AuctionSource::AuctionApi),
            &completed_outcome(),
            &[],
            Some(&result),
        );
        let bid_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.event_kind == EventKind::Bid)
            .collect();
        assert_eq!(
            bid_rows.len(),
            1,
            "should synthesize one row for the unmatched winner"
        );
        assert_eq!(
            bid_rows[0].is_win,
            Some(1),
            "should mark the synthesized row as the win"
        );
        assert_eq!(
            bid_rows[0].provider.as_deref(),
            Some("mediator"),
            "should attribute it to the mediator"
        );
    }

    #[test]
    fn completed_result_with_mixed_providers_produces_expected_grains() {
        // Arrange: one successful provider with two bids (one wins), one no-bid
        // provider, and an explicit provider-call list mirroring those outcomes.
        let winner = bid("slot-1", "kargo", Some(4.0), Some("c-1"));
        let mut winning_bids = HashMap::new();
        winning_bids.insert("slot-1".to_string(), winner);
        let result = OrchestrationResult {
            provider_responses: vec![
                response(
                    "prebid",
                    vec![
                        bid("slot-1", "kargo", Some(4.0), Some("c-1")),
                        bid("slot-1", "ix", Some(2.0), Some("c-2")),
                    ],
                    BidStatus::Success,
                ),
                response("aps", vec![], BidStatus::NoBid),
            ],
            mediator_response: None,
            winning_bids,
            total_time_ms: 88,
            metadata: HashMap::new(),
        };
        let calls = vec![
            ProviderCallOutcome {
                provider: "prebid".to_string(),
                role: ProviderRole::Bidder,
                status: ProviderCallStatus::Success,
                response_time_ms: Some(42),
                bid_count: Some(2),
            },
            ProviderCallOutcome {
                provider: "aps".to_string(),
                role: ProviderRole::Bidder,
                status: ProviderCallStatus::NoBid,
                response_time_ms: Some(40),
                bid_count: Some(0),
            },
        ];

        // Act
        let rows = build_auction_events(
            &ctx(AuctionSource::SpaNavigation),
            &completed_outcome(),
            &calls,
            Some(&result),
        );

        // Assert: exactly one summary, two provider-call rows, two bid rows, and no
        // invented seats on the no-bid provider.
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
        let bid_rows: Vec<_> = rows
            .iter()
            .filter(|r| r.event_kind == EventKind::Bid)
            .collect();
        assert_eq!(
            bid_rows.len(),
            2,
            "should emit a bid row only for returned bids"
        );
        assert!(
            bid_rows.iter().all(|r| r.seat.is_some()),
            "should never emit a bid row without a seat"
        );
        assert_eq!(
            bid_rows.iter().filter(|r| r.is_win == Some(1)).count(),
            1,
            "should mark exactly one winning bid"
        );
    }
}
