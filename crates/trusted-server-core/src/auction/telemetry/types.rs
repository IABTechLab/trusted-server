//! Value types for auction telemetry rows.
//!
//! These types are pure data: no I/O, no clock, no Fastly dependency. They are
//! shared by the builder and serialized as NDJSON by the Fastly sink.

use serde::Serialize;
use uuid::Uuid;

/// Auction initiation path that produced an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuctionSource {
    /// Initial publisher navigation via split-phase SSAT.
    InitialNavigation,
    /// Single-page-app navigation via `GET /__ts/page-bids`.
    SpaNavigation,
    /// Explicit `POST /auction` API call.
    AuctionApi,
}

/// Terminal status of a candidate auction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    /// Produced an `OrchestrationResult`, including a valid zero-bid result.
    Completed,
    /// Synchronous orchestration failed.
    ExecutionFailed,
    /// No provider request could be launched.
    DispatchFailed,
    /// Split-phase SSAT launched providers but could not collect them.
    Abandoned,
    /// Matched slots existed but policy prevented initiation.
    Skipped,
}

/// Outcome of a single provider call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCallStatus {
    /// Provider returned at least one bid.
    Success,
    /// Provider responded with no bid.
    #[serde(rename = "nobid")]
    NoBid,
    /// Provider request could not be launched.
    LaunchError,
    /// Provider response could not be parsed.
    ParseError,
    /// Provider request failed in transport.
    TransportError,
    /// Provider did not respond before the auction deadline.
    Timeout,
    /// Provider was dispatched but never collected.
    Abandoned,
}

/// Role a provider played in the auction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRole {
    /// A bidder.
    Bidder,
    /// The mediation layer.
    Mediator,
}

/// Discriminator for the row grain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// One per candidate auction.
    Summary,
    /// One per provider call.
    ProviderCall,
    /// One per returned bid (or unmatched mediator winner).
    Bid,
}

/// Immutable, PII-free snapshot describing one candidate auction.
///
/// Built once per candidate auction by the wiring layer and carried to the
/// terminal observation point. Contains no EC id, raw user agent, IP, or
/// internal `AuctionRequest.id`.
#[derive(Debug, Clone)]
pub struct AuctionObservationContext {
    /// Telemetry-only identifier, minted independently of any request id.
    pub auction_id: Uuid,
    /// Initiation path.
    pub source: AuctionSource,
    /// Publisher domain.
    pub publisher_domain: String,
    /// Bounded, normalized route. No query string or fragment.
    pub page_path: String,
    /// Coarse country from geo lookup.
    pub country: String,
    /// Coarse region from geo lookup, when available.
    pub region: Option<String>,
    /// `0` = desktop, `1` = mobile, `2` = unknown.
    pub is_mobile: u8,
    /// `0` = bot, `1` = browser, `2` = unknown.
    pub is_known_browser: u8,
    /// Whether GDPR applies for this request.
    pub gdpr_applies: bool,
    /// Whether any consent signal was present.
    pub consent_present: bool,
}

/// Terminal outcome of a candidate auction, used for the summary row.
#[derive(Debug, Clone)]
pub struct TerminalOutcome {
    /// Terminal status.
    pub status: TerminalStatus,
    /// Bounded machine-readable reason, e.g. for `skipped` cases.
    pub reason: Option<String>,
    /// Requested slot count.
    pub slot_count: Option<u16>,
    /// Elapsed time until completion or abandonment.
    pub total_time_ms: Option<u32>,
    /// Winning bid count; zero for non-completed outcomes.
    pub winning_bid_count: Option<u16>,
}

/// Outcome of a single provider call, used for provider-call rows.
#[derive(Debug, Clone)]
pub struct ProviderCallOutcome {
    /// Provider name, e.g. `prebid`, `aps`, or a mediator name.
    pub provider: String,
    /// Role the provider played.
    pub role: ProviderRole,
    /// Provider call status.
    pub status: ProviderCallStatus,
    /// Provider call latency, when known.
    pub response_time_ms: Option<u32>,
    /// Number of parsed bids, when known.
    pub bid_count: Option<u16>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_context_holds_snapshotted_primitives() {
        let ctx = AuctionObservationContext {
            auction_id: uuid::Uuid::nil(),
            source: AuctionSource::AuctionApi,
            publisher_domain: "example.com".to_string(),
            page_path: "/news".to_string(),
            country: "US".to_string(),
            region: Some("CA".to_string()),
            is_mobile: 1,
            is_known_browser: 1,
            gdpr_applies: false,
            consent_present: true,
        };
        assert_eq!(ctx.source, AuctionSource::AuctionApi, "should retain source");
        assert_eq!(ctx.region.as_deref(), Some("CA"), "should retain region");
    }

    #[test]
    fn enums_serialize_to_expected_wire_strings() {
        assert_eq!(
            serde_json::to_string(&AuctionSource::InitialNavigation)
                .expect("should serialize source"),
            "\"initial_navigation\"",
            "should use snake_case wire form"
        );
        assert_eq!(
            serde_json::to_string(&TerminalStatus::ExecutionFailed)
                .expect("should serialize status"),
            "\"execution_failed\"",
            "should use snake_case wire form"
        );
        assert_eq!(
            serde_json::to_string(&ProviderCallStatus::NoBid)
                .expect("should serialize provider status"),
            "\"nobid\"",
            "should render NoBid as the single token nobid"
        );
        assert_eq!(
            serde_json::to_string(&EventKind::ProviderCall)
                .expect("should serialize kind"),
            "\"provider_call\"",
            "should use snake_case wire form"
        );
    }
}
