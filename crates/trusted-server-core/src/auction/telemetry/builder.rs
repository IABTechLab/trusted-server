//! Pure builder that turns an auction observation into telemetry rows.

use crate::auction::orchestrator::OrchestrationResult;
use crate::auction::telemetry::types::{
    AuctionEventRow, AuctionObservationContext, EventKind, ProviderCallOutcome, TerminalOutcome,
};

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

/// Build bid rows from a completed orchestration result. Implemented in Task 6.
fn build_bid_rows(
    _ctx: &AuctionObservationContext,
    _result: &OrchestrationResult,
) -> Vec<AuctionEventRow> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::telemetry::types::{
        AuctionObservationContext, AuctionSource, EventKind, ProviderCallOutcome,
        ProviderCallStatus, ProviderRole, TerminalOutcome, TerminalStatus,
    };

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
}
