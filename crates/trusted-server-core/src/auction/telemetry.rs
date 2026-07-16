//! Auction telemetry row construction and sink abstraction.
//!
//! Core owns the privacy-preserving auction observation model and pure row
//! builder. Platform adapters provide the concrete sink implementation.

use std::collections::HashSet;
use std::time::Instant;

use chrono::Utc;
use error_stack::Report;
use serde::Serialize;
use uuid::Uuid;

use crate::auction::orchestrator::OrchestrationResult;
use crate::auction::types::{AuctionRequest, AuctionResponse, Bid, BidStatus, MediaType};
use crate::ec::EcContext;
use crate::error::TrustedServerError;
use crate::platform::RuntimeServices;

const MAX_PAGE_PATH_BYTES: usize = 256;
const DYNAMIC_SEGMENT_REPLACEMENT: &str = ":id";

/// Source path that initiated an auction candidate.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AuctionSource {
    /// Initial publisher navigation using server-side ad templates.
    InitialNavigation,
    /// SPA navigation through `GET /__ts/page-bids`.
    SpaNavigation,
    /// Explicit `POST /auction` API.
    AuctionApi,
}

impl AuctionSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::InitialNavigation => "initial_navigation",
            Self::SpaNavigation => "spa_navigation",
            Self::AuctionApi => "auction_api",
        }
    }
}

/// Terminal status for one auction observation.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AuctionTerminalStatus {
    /// Auction completed and produced an [`OrchestrationResult`].
    Completed,
    /// Auction execution failed after initiation.
    ExecutionFailed,
    /// No provider request could be launched.
    DispatchFailed,
    /// Split-phase auction was dispatched but could not be collected.
    Abandoned,
    /// Candidate was skipped by policy before provider calls were made.
    Skipped,
}

impl AuctionTerminalStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::ExecutionFailed => "execution_failed",
            Self::DispatchFailed => "dispatch_failed",
            Self::Abandoned => "abandoned",
            Self::Skipped => "skipped",
        }
    }
}

/// Provider call that was still pending when a split auction was abandoned.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct AbandonedProviderCall {
    /// Provider name.
    pub provider: String,
    /// Provider role, usually `bidder` or `mediator`.
    pub provider_role: &'static str,
    /// Optional elapsed time for this provider at abandonment.
    pub response_time_ms: Option<u32>,
}

impl AbandonedProviderCall {
    /// Construct an abandoned bidder call.
    #[must_use]
    pub fn bidder(provider: impl Into<String>, response_time_ms: Option<u32>) -> Self {
        Self {
            provider: provider.into(),
            provider_role: "bidder",
            response_time_ms,
        }
    }
}

/// Privacy-preserving context shared by all rows in one auction observation.
#[derive(Debug, Clone)]
pub struct AuctionObservationContext {
    /// Fresh telemetry UUID, independent of EC and internal auction IDs.
    pub auction_id: Uuid,
    /// Source path that initiated the auction candidate.
    pub auction_source: AuctionSource,
    /// Publisher domain.
    pub publisher_domain: String,
    /// Normalized, bounded page path.
    pub page_path: String,
    /// Country code, when available.
    pub country: String,
    /// Region code, when available.
    pub region: Option<String>,
    /// `0` = desktop, `1` = mobile, `2` = unknown.
    pub is_mobile: u8,
    /// `0` = bot, `1` = browser, `2` = unknown.
    pub is_known_browser: u8,
    /// Whether GDPR applies.
    pub gdpr_applies: bool,
    /// Whether any consent signal was present.
    pub consent_present: bool,
    /// Requested slot count for this candidate.
    pub slot_count: u16,
    started_at: Instant,
}

impl AuctionObservationContext {
    /// Build an observation context from an auction request.
    #[must_use]
    pub fn from_auction_request(
        auction_source: AuctionSource,
        request: &AuctionRequest,
        ec_context: &EcContext,
    ) -> Self {
        let raw_path = request
            .publisher
            .page_url
            .as_deref()
            .and_then(|page_url| url::Url::parse(page_url).ok())
            .map(|url| url.path().to_owned())
            .unwrap_or_else(|| "/".to_owned());
        Self::from_parts(
            auction_source,
            &request.publisher.domain,
            &raw_path,
            request.slots.len(),
            ec_context,
        )
    }

    /// Build an observation context from publisher request parts.
    #[must_use]
    pub fn from_parts(
        auction_source: AuctionSource,
        publisher_domain: &str,
        raw_page_path: &str,
        slot_count: usize,
        ec_context: &EcContext,
    ) -> Self {
        let device = ec_context.device_signals();
        let geo = ec_context.geo_info();
        let consent = ec_context.consent();
        let slot_count = u16::try_from(slot_count).unwrap_or(u16::MAX);
        Self {
            auction_id: Uuid::new_v4(),
            auction_source,
            publisher_domain: publisher_domain.to_owned(),
            page_path: normalize_page_path(raw_page_path),
            country: geo
                .map(|info| info.country.clone())
                .filter(|country| !country.is_empty())
                .unwrap_or_else(|| "ZZ".to_owned()),
            region: geo.and_then(|info| info.region.clone()),
            is_mobile: device.map_or(2, |signals| signals.is_mobile),
            is_known_browser: match device.and_then(|signals| signals.known_browser) {
                Some(true) => 1,
                Some(false) => 0,
                None => 2,
            },
            gdpr_applies: consent.gdpr_applies,
            consent_present: !consent.is_empty(),
            slot_count,
            started_at: Instant::now(),
        }
    }

    /// Return elapsed milliseconds since the observation was created.
    #[must_use]
    pub fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    #[cfg(test)]
    fn for_test(auction_source: AuctionSource, page_path: &str, slot_count: u16) -> Self {
        Self {
            auction_id: Uuid::new_v4(),
            auction_source,
            publisher_domain: "test-publisher.example".to_owned(),
            page_path: normalize_page_path(page_path),
            country: "US".to_owned(),
            region: Some("CA".to_owned()),
            is_mobile: 0,
            is_known_browser: 1,
            gdpr_applies: false,
            consent_present: false,
            slot_count,
            started_at: Instant::now(),
        }
    }
}

/// Terminal outcome used by the row builder.
pub enum AuctionTerminalOutcome<'a> {
    /// Completed auction.
    Completed {
        /// Auction request used for slot and bid context.
        request: &'a AuctionRequest,
        /// Orchestration result.
        result: &'a OrchestrationResult,
    },
    /// Execution failure.
    ExecutionFailed {
        /// Optional auction request.
        request: Option<&'a AuctionRequest>,
        /// Provider responses observed before the failure.
        provider_responses: &'a [AuctionResponse],
        /// Bounded terminal reason.
        reason: &'a str,
        /// Elapsed time in milliseconds.
        elapsed_ms: u64,
    },
    /// Dispatch failure.
    DispatchFailed {
        /// Auction request.
        request: &'a AuctionRequest,
        /// Provider responses observed during dispatch.
        provider_responses: &'a [AuctionResponse],
        /// Bounded terminal reason.
        reason: &'a str,
        /// Elapsed time in milliseconds.
        elapsed_ms: u64,
    },
    /// Split auction was abandoned.
    Abandoned {
        /// Auction request.
        request: &'a AuctionRequest,
        /// Provider responses observed before abandonment.
        provider_responses: &'a [AuctionResponse],
        /// Providers still pending at abandonment time.
        abandoned_providers: &'a [AbandonedProviderCall],
        /// Bounded terminal reason.
        reason: &'a str,
        /// Elapsed time in milliseconds.
        elapsed_ms: u64,
    },
    /// Candidate was skipped before provider calls.
    Skipped {
        /// Bounded terminal reason.
        reason: &'a str,
        /// Elapsed time in milliseconds.
        elapsed_ms: u64,
    },
}

/// One row for the `auction_events_raw` datasource.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AuctionEventRow {
    /// Terminal observation timestamp in UTC.
    pub event_ts: String,
    /// `summary`, `provider_call`, or `bid`.
    pub event_kind: String,
    /// Fresh telemetry auction UUID.
    pub auction_id: String,
    /// Source path label.
    pub auction_source: String,
    /// Publisher domain.
    pub publisher_domain: String,
    /// Normalized page path.
    pub page_path: String,
    /// Country code.
    pub country: String,
    /// Region code.
    pub region: Option<String>,
    /// `0` = desktop, `1` = mobile, `2` = unknown.
    pub is_mobile: u8,
    /// `0` = bot, `1` = browser, `2` = unknown.
    pub is_known_browser: u8,
    /// `0` or `1`.
    pub gdpr_applies: u8,
    /// `0` or `1`.
    pub consent_present: u8,
    /// Summary terminal status.
    pub terminal_status: Option<String>,
    /// Summary terminal reason.
    pub terminal_reason: Option<String>,
    /// Requested slots.
    pub slot_count: Option<u16>,
    /// Total elapsed time.
    pub total_time_ms: Option<u32>,
    /// Number of winning bids.
    pub winning_bid_count: Option<u16>,
    /// Provider name.
    pub provider: Option<String>,
    /// `bidder` or `mediator`.
    pub provider_role: Option<String>,
    /// Provider-call status.
    pub status: Option<String>,
    /// Provider elapsed time.
    pub provider_response_time_ms: Option<u32>,
    /// Parsed provider bid count.
    pub provider_bid_count: Option<u16>,
    /// Bid slot ID.
    pub slot_id: Option<String>,
    /// Creative width.
    pub slot_w: Option<u16>,
    /// Creative height.
    pub slot_h: Option<u16>,
    /// Media type.
    pub media_type: Option<String>,
    /// Seat/bidder.
    pub seat: Option<String>,
    /// Decoded CPM.
    pub price_cpm: Option<f64>,
    /// Currency.
    pub currency: Option<String>,
    /// Whether this is the canonical winning row for its slot.
    pub is_win: Option<u8>,
    /// Advertiser domain.
    pub ad_domain: Option<String>,
    /// Creative/ad ID.
    pub ad_id: Option<String>,
}

impl AuctionEventRow {
    fn base(observation: &AuctionObservationContext, event_kind: &str, event_ts: &str) -> Self {
        Self {
            event_ts: event_ts.to_owned(),
            event_kind: event_kind.to_owned(),
            auction_id: observation.auction_id.to_string(),
            auction_source: observation.auction_source.as_str().to_owned(),
            publisher_domain: observation.publisher_domain.clone(),
            page_path: observation.page_path.clone(),
            country: observation.country.clone(),
            region: observation.region.clone(),
            is_mobile: observation.is_mobile,
            is_known_browser: observation.is_known_browser,
            gdpr_applies: u8::from(observation.gdpr_applies),
            consent_present: u8::from(observation.consent_present),
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

/// A bounded group of rows emitted for one auction observation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuctionEventBatch {
    rows: Vec<AuctionEventRow>,
}

impl AuctionEventBatch {
    /// Create a batch from rows.
    #[must_use]
    pub fn new(rows: Vec<AuctionEventRow>) -> Self {
        Self { rows }
    }

    /// Return rows in this batch.
    #[must_use]
    pub fn rows(&self) -> &[AuctionEventRow] {
        &self.rows
    }

    /// Return the number of rows in this batch.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// Return true when there are no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Serialize rows as newline-delimited JSON and enforce a maximum body size.
    ///
    /// # Errors
    ///
    /// Returns an error when serialization fails or the body exceeds `max_body_bytes`.
    pub fn to_ndjson(&self, max_body_bytes: usize) -> Result<String, Report<TrustedServerError>> {
        let mut body = String::new();
        for row in &self.rows {
            let line = serde_json::to_string(row).map_err(|err| {
                Report::new(TrustedServerError::Proxy {
                    message: format!("failed to serialize auction telemetry row: {err}"),
                })
            })?;
            body.push_str(&line);
            body.push('\n');
            if body.len() > max_body_bytes {
                return Err(Report::new(TrustedServerError::Proxy {
                    message: format!(
                        "auction telemetry payload exceeds {max_body_bytes} byte limit"
                    ),
                }));
            }
        }
        Ok(body)
    }
}

/// Sink for auction telemetry batches.
#[async_trait::async_trait(?Send)]
pub trait AuctionTelemetrySink: Send + Sync {
    /// Return whether this sink emits telemetry.
    ///
    /// Callers use this as a cheap hot-path gate before allocating telemetry
    /// rows. Enabled sinks may still drop individual empty or invalid batches.
    fn is_enabled(&self) -> bool {
        true
    }

    /// Emit an auction telemetry batch.
    ///
    /// # Errors
    ///
    /// Returns an error when the sink cannot start emission. Callers should use
    /// [`emit_auction_events_best_effort_lazy`] on the hot path.
    async fn emit_auction_events(
        &self,
        services: &RuntimeServices,
        batch: AuctionEventBatch,
    ) -> Result<(), Report<TrustedServerError>>;
}

/// No-op auction telemetry sink used when telemetry is disabled.
pub struct NoopAuctionTelemetrySink;

#[async_trait::async_trait(?Send)]
impl AuctionTelemetrySink for NoopAuctionTelemetrySink {
    fn is_enabled(&self) -> bool {
        false
    }

    async fn emit_auction_events(
        &self,
        _services: &RuntimeServices,
        _batch: AuctionEventBatch,
    ) -> Result<(), Report<TrustedServerError>> {
        Ok(())
    }
}

/// Emit a telemetry batch without letting errors affect customer traffic.
pub async fn emit_auction_events_best_effort(services: &RuntimeServices, batch: AuctionEventBatch) {
    emit_auction_events_best_effort_lazy(services, || batch).await;
}

/// Lazily build and emit a telemetry batch without affecting customer traffic.
///
/// The batch builder is skipped when the configured sink is disabled, avoiding
/// per-auction row allocations on the default no-op telemetry path.
pub async fn emit_auction_events_best_effort_lazy(
    services: &RuntimeServices,
    build_batch: impl FnOnce() -> AuctionEventBatch,
) {
    let sink = services.auction_telemetry_sink();
    if !sink.is_enabled() {
        return;
    }

    let batch = build_batch();
    if batch.is_empty() {
        return;
    }
    if let Err(err) = sink.emit_auction_events(services, batch).await {
        log::warn!("auction telemetry emission skipped: {err:?}");
    }
}

/// Build auction telemetry rows for one terminal observation.
#[allow(
    clippy::needless_pass_by_value,
    reason = "call sites hand off terminal observations by value to keep ownership explicit"
)]
#[must_use]
pub fn build_auction_events(
    observation: AuctionObservationContext,
    terminal: AuctionTerminalOutcome<'_>,
) -> AuctionEventBatch {
    let event_ts = current_event_timestamp();
    let mut rows = Vec::new();

    match terminal {
        AuctionTerminalOutcome::Completed { request, result } => {
            push_summary(
                &mut rows,
                &observation,
                &event_ts,
                AuctionTerminalStatus::Completed,
                None,
                result.total_time_ms,
                result.winning_bids.len(),
            );
            push_provider_rows(
                &mut rows,
                &observation,
                &event_ts,
                &result.provider_responses,
                "bidder",
            );
            if let Some(mediator_response) = &result.mediator_response {
                push_provider_row(
                    &mut rows,
                    &observation,
                    &event_ts,
                    mediator_response,
                    "mediator",
                );
            }
            push_bid_rows(&mut rows, &observation, &event_ts, request, result);
        }
        AuctionTerminalOutcome::ExecutionFailed {
            request: _,
            provider_responses,
            reason,
            elapsed_ms,
        } => {
            push_summary(
                &mut rows,
                &observation,
                &event_ts,
                AuctionTerminalStatus::ExecutionFailed,
                Some(reason),
                elapsed_ms,
                0,
            );
            push_provider_rows(
                &mut rows,
                &observation,
                &event_ts,
                provider_responses,
                "bidder",
            );
        }
        AuctionTerminalOutcome::DispatchFailed {
            request: _,
            provider_responses,
            reason,
            elapsed_ms,
        } => {
            push_summary(
                &mut rows,
                &observation,
                &event_ts,
                AuctionTerminalStatus::DispatchFailed,
                Some(reason),
                elapsed_ms,
                0,
            );
            push_provider_rows(
                &mut rows,
                &observation,
                &event_ts,
                provider_responses,
                "bidder",
            );
        }
        AuctionTerminalOutcome::Abandoned {
            request: _,
            provider_responses,
            abandoned_providers,
            reason,
            elapsed_ms,
        } => {
            push_summary(
                &mut rows,
                &observation,
                &event_ts,
                AuctionTerminalStatus::Abandoned,
                Some(reason),
                elapsed_ms,
                0,
            );
            push_provider_rows(
                &mut rows,
                &observation,
                &event_ts,
                provider_responses,
                "bidder",
            );
            for provider in abandoned_providers {
                let mut row = AuctionEventRow::base(&observation, "provider_call", &event_ts);
                row.provider = Some(provider.provider.clone());
                row.provider_role = Some(provider.provider_role.to_owned());
                row.status = Some("abandoned".to_owned());
                row.provider_response_time_ms = provider.response_time_ms;
                row.provider_bid_count = Some(0);
                rows.push(row);
            }
        }
        AuctionTerminalOutcome::Skipped { reason, elapsed_ms } => {
            push_summary(
                &mut rows,
                &observation,
                &event_ts,
                AuctionTerminalStatus::Skipped,
                Some(reason),
                elapsed_ms,
                0,
            );
        }
    }

    AuctionEventBatch::new(rows)
}

fn push_summary(
    rows: &mut Vec<AuctionEventRow>,
    observation: &AuctionObservationContext,
    event_ts: &str,
    status: AuctionTerminalStatus,
    reason: Option<&str>,
    elapsed_ms: u64,
    winning_bid_count: usize,
) {
    let mut row = AuctionEventRow::base(observation, "summary", event_ts);
    row.terminal_status = Some(status.as_str().to_owned());
    row.terminal_reason = reason.map(sanitize_reason);
    row.slot_count = Some(observation.slot_count);
    row.total_time_ms = Some(u32::try_from(elapsed_ms).unwrap_or(u32::MAX));
    row.winning_bid_count = Some(u16::try_from(winning_bid_count).unwrap_or(u16::MAX));
    rows.push(row);
}

fn push_provider_rows(
    rows: &mut Vec<AuctionEventRow>,
    observation: &AuctionObservationContext,
    event_ts: &str,
    responses: &[AuctionResponse],
    role: &str,
) {
    for response in responses {
        push_provider_row(rows, observation, event_ts, response, role);
    }
}

fn push_provider_row(
    rows: &mut Vec<AuctionEventRow>,
    observation: &AuctionObservationContext,
    event_ts: &str,
    response: &AuctionResponse,
    role: &str,
) {
    let mut row = AuctionEventRow::base(observation, "provider_call", event_ts);
    row.provider = Some(response.provider.clone());
    row.provider_role = Some(role.to_owned());
    row.status = Some(provider_status(response).to_owned());
    row.provider_response_time_ms =
        Some(u32::try_from(response.response_time_ms).unwrap_or(u32::MAX));
    row.provider_bid_count = Some(u16::try_from(response.bids.len()).unwrap_or(u16::MAX));
    rows.push(row);
}

fn push_bid_rows(
    rows: &mut Vec<AuctionEventRow>,
    observation: &AuctionObservationContext,
    event_ts: &str,
    request: &AuctionRequest,
    result: &OrchestrationResult,
) {
    let mut matched_wins = HashSet::new();

    for response in &result.provider_responses {
        for bid in &response.bids {
            let matched_slot = result
                .winning_bids
                .iter()
                .find(|(slot_id, winning)| {
                    !matched_wins.contains(*slot_id) && bid_matches_winning_bid(bid, winning)
                })
                .map(|(slot_id, winning)| (slot_id.clone(), winning));
            let (is_win, price) = if let Some((slot_id, winning)) = matched_slot {
                matched_wins.insert(slot_id);
                (1, bid.price.or(winning.price))
            } else {
                (0, bid.price)
            };
            rows.push(bid_row(
                observation,
                event_ts,
                request,
                &response.provider,
                bid,
                is_win,
                price,
            ));
        }
    }

    if let Some(mediator_response) = &result.mediator_response {
        for (slot_id, winning) in &result.winning_bids {
            if matched_wins.contains(slot_id) {
                continue;
            }
            if mediator_response
                .bids
                .iter()
                .any(|bid| bid_matches_winning_bid(bid, winning))
            {
                rows.push(bid_row(
                    observation,
                    event_ts,
                    request,
                    &mediator_response.provider,
                    winning,
                    1,
                    winning.price,
                ));
                matched_wins.insert(slot_id.clone());
            }
        }
    }
}

fn bid_row(
    observation: &AuctionObservationContext,
    event_ts: &str,
    request: &AuctionRequest,
    provider: &str,
    bid: &Bid,
    is_win: u8,
    price: Option<f64>,
) -> AuctionEventRow {
    let mut row = AuctionEventRow::base(observation, "bid", event_ts);
    row.provider = Some(provider.to_owned());
    row.slot_id = Some(bid.slot_id.clone());
    row.slot_w = Some(u16::try_from(bid.width).unwrap_or(u16::MAX));
    row.slot_h = Some(u16::try_from(bid.height).unwrap_or(u16::MAX));
    row.media_type = media_type_for_slot(request, &bid.slot_id).map(str::to_owned);
    row.seat = Some(bid.bidder.clone());
    row.price_cpm = price;
    row.currency = Some(bid.currency.clone());
    row.is_win = Some(is_win);
    row.ad_domain = bid
        .adomain
        .as_ref()
        .and_then(|domains| domains.first().cloned());
    row.ad_id = bid.ad_id.clone();
    row
}

fn bid_matches_winning_bid(candidate: &Bid, winning: &Bid) -> bool {
    if candidate.slot_id != winning.slot_id || candidate.bidder != winning.bidder {
        return false;
    }
    match winning.ad_id.as_deref() {
        Some(winning_ad_id) => candidate.ad_id.as_deref() == Some(winning_ad_id),
        None => true,
    }
}

fn media_type_for_slot<'a>(request: &'a AuctionRequest, slot_id: &str) -> Option<&'a str> {
    request
        .slots
        .iter()
        .find(|slot| slot.id == slot_id)
        .and_then(|slot| slot.formats.first())
        .map(|format| match format.media_type {
            MediaType::Banner => "banner",
            MediaType::Video => "video",
            MediaType::Native => "native",
        })
}

fn provider_status(response: &AuctionResponse) -> &'static str {
    match response.status {
        BidStatus::Success => "success",
        BidStatus::NoBid => "nobid",
        BidStatus::Error => match response
            .metadata
            .get("error_type")
            .and_then(serde_json::Value::as_str)
        {
            Some("launch_failed") => "launch_error",
            Some("parse_response") => "parse_error",
            Some("transport") => "transport_error",
            Some("timeout") => "timeout",
            _ => "transport_error",
        },
        BidStatus::Pending => "timeout",
    }
}

fn current_event_timestamp() -> String {
    Utc::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

fn sanitize_reason(reason: &str) -> String {
    let mut output = String::new();
    for ch in reason.chars().take(64) {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            output.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_whitespace() || ch == '/' || ch == ':' {
            output.push('_');
        }
    }
    if output.is_empty() {
        "unknown".to_owned()
    } else {
        output
    }
}

/// Normalize a raw page path into a bounded, low-cardinality route label.
#[must_use]
pub fn normalize_page_path(raw: &str) -> String {
    let without_query = raw.split(['?', '#']).next().unwrap_or("/");
    let path = if without_query.starts_with('/') {
        without_query.to_owned()
    } else {
        format!("/{without_query}")
    };
    let mut normalized = String::new();
    for segment in path.split('/') {
        if segment.is_empty() {
            if normalized.is_empty() {
                normalized.push('/');
            }
            continue;
        }
        if normalized.len() > 1 && !normalized.ends_with('/') {
            normalized.push('/');
        }
        if is_dynamic_segment(segment) {
            normalized.push_str(DYNAMIC_SEGMENT_REPLACEMENT);
        } else {
            normalized.push_str(segment);
        }
        if normalized.len() >= MAX_PAGE_PATH_BYTES {
            truncate_to_char_boundary(&mut normalized, MAX_PAGE_PATH_BYTES);
            break;
        }
    }
    if normalized.is_empty() {
        "/".to_owned()
    } else {
        normalized
    }
}

fn truncate_to_char_boundary(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let boundary = (0..=max_bytes)
        .rev()
        .find(|index| value.is_char_boundary(*index))
        .unwrap_or(0);
    value.truncate(boundary);
}

fn is_dynamic_segment(segment: &str) -> bool {
    let trimmed = segment.trim();
    if trimmed.len() >= 6 && trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return true;
    }
    if looks_like_uuid(trimmed) {
        return true;
    }
    if trimmed.len() >= 16
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() || matches!(ch, '-' | '_'))
    {
        return true;
    }
    trimmed.len() >= 24
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

fn looks_like_uuid(value: &str) -> bool {
    let parts: Vec<_> = value.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    let lengths = [8, 4, 4, 4, 12];
    parts
        .iter()
        .zip(lengths)
        .all(|(part, len)| part.len() == len && part.chars().all(|ch| ch.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::HashMap;

    use serde_json::json;

    use crate::auction::types::{AdFormat, AdSlot, PublisherInfo, UserInfo};

    use super::*;

    fn test_request(id: &str) -> AuctionRequest {
        AuctionRequest {
            id: id.to_owned(),
            slots: vec![AdSlot {
                id: "slot-1".to_owned(),
                formats: vec![AdFormat {
                    media_type: MediaType::Banner,
                    width: 300,
                    height: 250,
                }],
                floor_price: None,
                targeting: HashMap::new(),
                bidders: HashMap::new(),
            }],
            publisher: PublisherInfo {
                domain: "test-publisher.example".to_owned(),
                page_url: Some("https://test-publisher.example/articles/123456?x=1".to_owned()),
            },
            user: UserInfo {
                id: Some("ec-value-that-must-not-leak".to_owned()),
                consent: None,
                eids: None,
            },
            device: None,
            site: None,
            context: HashMap::new(),
        }
    }

    fn bid(slot_id: &str, bidder: &str, ad_id: Option<&str>, price: Option<f64>) -> Bid {
        Bid {
            slot_id: slot_id.to_owned(),
            price,
            currency: "USD".to_owned(),
            creative: None,
            adomain: Some(vec!["advertiser.example".to_owned()]),
            bidder: bidder.to_owned(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            ad_id: ad_id.map(str::to_owned),
            bid_id: None,
            crid: None,
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn emit_lazy_skips_builder_when_sink_is_disabled() {
        let services = crate::platform::test_support::noop_services();
        let builder_called = Cell::new(false);

        futures::executor::block_on(emit_auction_events_best_effort_lazy(&services, || {
            builder_called.set(true);
            AuctionEventBatch::default()
        }));

        assert!(
            !builder_called.get(),
            "disabled telemetry sink should not build auction rows"
        );
    }

    #[test]
    fn normalize_page_path_strips_query_and_redacts_dynamic_segments() {
        assert_eq!(
            normalize_page_path("/article/123456/comments?user=abc#frag"),
            "/article/:id/comments",
            "should strip query and redact long numeric segment"
        );
        assert_eq!(
            normalize_page_path("product/550e8400-e29b-41d4-a716-446655440000"),
            "/product/:id",
            "should force leading slash and redact UUID"
        );
    }

    #[test]
    fn normalize_page_path_truncates_unicode_at_char_boundary() {
        let normalized = normalize_page_path(&format!("/{}", "é".repeat(200)));

        assert!(
            normalized.len() <= MAX_PAGE_PATH_BYTES,
            "should stay within byte cap"
        );
        assert!(
            normalized.is_char_boundary(normalized.len()),
            "should not panic or split utf8 when truncating"
        );
    }

    #[test]
    fn build_completed_events_keeps_summary_provider_and_bid_grains_separate() {
        let request = test_request("ts-ec-derived-id");
        let provider_success = AuctionResponse::success(
            "prebid",
            vec![bid("slot-1", "kargo", Some("ad-1"), Some(1.25))],
            42,
        );
        let provider_no_bid = AuctionResponse::no_bid("aps", 55);
        let provider_error =
            AuctionResponse::error("mock", 12).with_metadata("error_type", json!("parse_response"));
        let winning = provider_success.bids[0].clone();
        let result = OrchestrationResult {
            provider_responses: vec![provider_success, provider_no_bid, provider_error],
            mediator_response: None,
            winning_bids: HashMap::from([("slot-1".to_owned(), winning)]),
            total_time_ms: 99,
            metadata: HashMap::new(),
        };
        let observation =
            AuctionObservationContext::for_test(AuctionSource::AuctionApi, "/article/1", 1);

        let batch = build_auction_events(
            observation,
            AuctionTerminalOutcome::Completed {
                request: &request,
                result: &result,
            },
        );

        let rows = batch.rows();
        assert_eq!(
            rows.iter()
                .filter(|row| row.event_kind == "summary")
                .count(),
            1,
            "should emit exactly one summary row"
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row.event_kind == "provider_call")
                .count(),
            3,
            "should emit one provider row per provider response"
        );
        assert_eq!(
            rows.iter().filter(|row| row.event_kind == "bid").count(),
            1,
            "should emit bid rows only for actual bids"
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.provider.as_deref() == Some("aps"))
                .and_then(|row| row.slot_id.as_deref()),
            None,
            "no-bid provider row should not invent slot data"
        );
        assert_eq!(
            rows.iter()
                .find(|row| row.provider.as_deref() == Some("mock"))
                .and_then(|row| row.status.as_deref()),
            Some("parse_error"),
            "should map provider parse failures"
        );
    }

    #[test]
    fn mediated_win_marks_original_bid_once() {
        let request = test_request("req");
        let original_bid = bid("slot-1", "kargo", Some("ad-1"), None);
        let provider_success = AuctionResponse::success("prebid", vec![original_bid], 42);
        let mediator_bid = bid("slot-1", "kargo", Some("ad-1"), Some(2.0));
        let mediator_response =
            AuctionResponse::success("adserver_mock", vec![mediator_bid.clone()], 15);
        let result = OrchestrationResult {
            provider_responses: vec![provider_success],
            mediator_response: Some(mediator_response),
            winning_bids: HashMap::from([("slot-1".to_owned(), mediator_bid)]),
            total_time_ms: 80,
            metadata: HashMap::new(),
        };
        let observation =
            AuctionObservationContext::for_test(AuctionSource::InitialNavigation, "/", 1);

        let batch = build_auction_events(
            observation,
            AuctionTerminalOutcome::Completed {
                request: &request,
                result: &result,
            },
        );
        let winning_rows: Vec<_> = batch
            .rows()
            .iter()
            .filter(|row| row.event_kind == "bid" && row.is_win == Some(1))
            .collect();

        assert_eq!(winning_rows.len(), 1, "should have one canonical winner");
        assert_eq!(
            winning_rows[0].provider.as_deref(),
            Some("prebid"),
            "should mark original provider row when mediator winner matches"
        );
        assert_eq!(
            winning_rows[0].price_cpm,
            Some(2.0),
            "should copy mediator decoded price onto original null-price bid"
        );
    }

    #[test]
    fn ndjson_serialization_has_one_json_object_per_line_and_no_private_ids() {
        let request = test_request("ts-ec-derived-id");
        let result = OrchestrationResult {
            provider_responses: Vec::new(),
            mediator_response: None,
            winning_bids: HashMap::new(),
            total_time_ms: 1,
            metadata: HashMap::new(),
        };
        let observation =
            AuctionObservationContext::for_test(AuctionSource::AuctionApi, "/auction", 1);

        let body = build_auction_events(
            observation,
            AuctionTerminalOutcome::Completed {
                request: &request,
                result: &result,
            },
        )
        .to_ndjson(4096)
        .expect("should serialize ndjson");

        assert!(body.ends_with('\n'), "should end each row with newline");
        for line in body.lines() {
            let parsed: serde_json::Value = serde_json::from_str(line).expect("should parse row");
            assert_eq!(parsed["event_kind"], "summary");
        }
        assert!(
            !body.contains("ts-ec-derived-id") && !body.contains("ec-value-that-must-not-leak"),
            "should not serialize internal auction request IDs or EC values"
        );
    }
}
