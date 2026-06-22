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

/// One serialized telemetry row. A single flat shape covers all three grains;
/// fields that do not apply to a row kind are `None` and serialize to JSON
/// `null` so the NDJSON shape is stable across rows.
///
/// `event_ts` is intentionally absent: core is clock-free and the sink or
/// Tinybird supplies the timestamp.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AuctionEventRow {
    /// Row grain discriminator.
    pub event_kind: EventKind,
    /// Telemetry id, hyphenated UUID string.
    pub auction_id: String,
    /// Initiation path.
    pub auction_source: AuctionSource,
    /// Publisher domain.
    pub publisher_domain: String,
    /// Bounded normalized route.
    pub page_path: String,
    /// Coarse country.
    pub country: String,
    /// Coarse region.
    pub region: Option<String>,
    /// `0`/`1`/`2` device class.
    pub is_mobile: u8,
    /// `0`/`1`/`2` browser-legitimacy class.
    pub is_known_browser: u8,
    /// `0`/`1`.
    pub gdpr_applies: u8,
    /// `0`/`1`.
    pub consent_present: u8,
    /// Summary: terminal status.
    pub terminal_status: Option<TerminalStatus>,
    /// Summary: bounded reason.
    pub terminal_reason: Option<String>,
    /// Summary: requested slots.
    pub slot_count: Option<u16>,
    /// Summary: elapsed ms.
    pub total_time_ms: Option<u32>,
    /// Summary: winning bid count.
    pub winning_bid_count: Option<u16>,
    /// Provider-call and bid: provider name.
    pub provider: Option<String>,
    /// Provider-call: role.
    pub provider_role: Option<ProviderRole>,
    /// Provider-call: status.
    pub status: Option<ProviderCallStatus>,
    /// Provider-call: latency ms.
    pub provider_response_time_ms: Option<u32>,
    /// Provider-call: parsed bid count.
    pub provider_bid_count: Option<u16>,
    /// Bid: slot id.
    pub slot_id: Option<String>,
    /// Bid: returned creative width.
    pub slot_w: Option<u16>,
    /// Bid: returned creative height.
    pub slot_h: Option<u16>,
    /// Bid: media type, filled by a later wiring plan.
    pub media_type: Option<String>,
    /// Bid: seat/bidder name.
    pub seat: Option<String>,
    /// Bid: decoded CPM when available.
    pub price_cpm: Option<f64>,
    /// Bid: currency.
    pub currency: Option<String>,
    /// Bid: `1` for the one canonical winning row per slot, else `0`.
    pub is_win: Option<u8>,
    /// Bid: first advertiser domain.
    pub ad_domain: Option<String>,
    /// Bid: creative id.
    pub ad_id: Option<String>,
}

impl AuctionEventRow {
    /// Build a row with the shared columns filled from `ctx` and every
    /// kind-specific column set to `None`.
    #[must_use]
    pub fn base(ctx: &AuctionObservationContext, kind: EventKind) -> Self {
        Self {
            event_kind: kind,
            auction_id: ctx.auction_id.to_string(),
            auction_source: ctx.source,
            publisher_domain: ctx.publisher_domain.clone(),
            page_path: ctx.page_path.clone(),
            country: ctx.country.clone(),
            region: ctx.region.clone(),
            is_mobile: ctx.is_mobile,
            is_known_browser: ctx.is_known_browser,
            gdpr_applies: u8::from(ctx.gdpr_applies),
            consent_present: u8::from(ctx.consent_present),
            terminal_status: None,
            terminal_reason: None,
            slot_count: None,
            total_time_ms: None,
            winning_bid_count: None,
            provider: None,
            provider_role: None,
            status: None,
            provider_response_time_ms: None,
            provider_bid_count: None,
            slot_id: None,
            slot_w: None,
            slot_h: None,
            media_type: None,
            seat: None,
            price_cpm: None,
            currency: None,
            is_win: None,
            ad_domain: None,
            ad_id: None,
        }
    }
}

/// Serialize rows as newline-delimited JSON with no trailing newline.
///
/// # Errors
///
/// Returns the underlying `serde_json` error if a row cannot be serialized.
pub fn to_ndjson(rows: &[AuctionEventRow]) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&serde_json::to_string(row)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_context() -> AuctionObservationContext {
        AuctionObservationContext {
            auction_id: uuid::Uuid::nil(),
            source: AuctionSource::SpaNavigation,
            publisher_domain: "example.com".to_string(),
            page_path: "/p".to_string(),
            country: "US".to_string(),
            region: None,
            is_mobile: 0,
            is_known_browser: 1,
            gdpr_applies: true,
            consent_present: false,
        }
    }

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

    #[test]
    fn base_row_fills_shared_fields_and_nulls_the_rest() {
        let row = AuctionEventRow::base(&sample_context(), EventKind::Summary);
        assert_eq!(row.event_kind, EventKind::Summary, "should set kind");
        assert_eq!(row.gdpr_applies, 1, "should map true to 1");
        assert_eq!(row.consent_present, 0, "should map false to 0");
        assert!(row.terminal_status.is_none(), "should null summary fields");
        assert!(row.provider.is_none(), "should null provider fields");
        assert!(row.slot_id.is_none(), "should null bid fields");
    }

    #[test]
    fn to_ndjson_is_one_compact_object_per_line() {
        let rows = vec![
            AuctionEventRow::base(&sample_context(), EventKind::Summary),
            AuctionEventRow::base(&sample_context(), EventKind::Bid),
        ];
        let ndjson = to_ndjson(&rows).expect("should serialize rows");
        let lines: Vec<&str> = ndjson.split('\n').collect();
        assert_eq!(lines.len(), 2, "should emit one line per row with no trailing newline");
        for line in &lines {
            let value: serde_json::Value =
                serde_json::from_str(line).expect("each line should be valid JSON");
            assert!(value.get("event_kind").is_some(), "should always include event_kind");
            assert!(value.get("auction_id").is_some(), "should always include auction_id");
            assert!(value.get("region").is_some(), "should include region key even when null");
        }
    }
}
