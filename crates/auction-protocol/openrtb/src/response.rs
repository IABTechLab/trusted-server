//! Reading an ordinary `OpenRTB` 2.6 bid response into core's bids.

use std::collections::HashMap;

use error_stack::{Report, ResultExt as _};
use http::StatusCode;
use serde_json::{Value, json};
use trusted_server_core::auction::admission::{
    BidDimensionIndex, BidRejectionReason, ResponseAdmissionDiagnostics, build_bid_dimension_index,
    parse_optional_bid_dimension, resolve_bid_dimensions,
};
use trusted_server_core::auction::demand::{DemandResponse, ProviderAuctionInput};
use trusted_server_core::auction::types::{AuctionResponse, Bid};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::platform::PlatformResponse;

use crate::request::DEFAULT_CURRENCY;

/// The most a demand source's response body may hold.
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Reads an ordinary `OpenRTB` response. A `204` is a no-bid, any other
/// non-success status or unreadable JSON is an error response, and a body over
/// the size limit is an `Err`.
///
/// A demand implementation whose exchange answers the ordinary way has its
/// response read through this.
///
/// # Errors
///
/// Returns an error when the response body cannot be read.
pub async fn parse_openrtb_response(
    context: DemandResponse<'_>,
    response: PlatformResponse,
) -> Result<AuctionResponse, Report<TrustedServerError>> {
    let provider_id = context.provider_id;
    let response_time_ms = context.response_time_ms;
    let response = response.response;
    let status = response.status();
    if status == StatusCode::NO_CONTENT {
        return Ok(AuctionResponse::no_bid(provider_id, response_time_ms));
    }
    if !status.is_success() {
        if status.is_redirection() {
            log::warn!(
                "Provider '{provider_id}' returned a redirect; generic OpenRTB redirects are refused"
            );
        }
        return Ok(AuctionResponse::error(provider_id, response_time_ms)
            .with_metadata("error_type", json!("http_status"))
            .with_metadata("http_status", json!(status.as_u16())));
    }

    let body = response
        .into_body()
        .into_bytes_bounded(MAX_RESPONSE_BYTES)
        .await
        .change_context(TrustedServerError::Auction {
            message: format!("Provider {provider_id} response body failed"),
        })?;
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(error) => {
            log::warn!("Provider '{provider_id}' response JSON was invalid: {error}");
            return Ok(AuctionResponse::error(provider_id, response_time_ms)
                .with_metadata("error_type", json!("parse_response")));
        }
    };

    Ok(extract_standard_response(
        provider_id,
        context.input,
        &value,
        response_time_ms,
    ))
}

/// Reads ordinary `OpenRTB` bids independently, admitting each against the
/// slots the request asked for. The response id is informational.
#[must_use]
pub fn extract_standard_response(
    provider_id: &str,
    input: &ProviderAuctionInput,
    value: &Value,
    response_time_ms: u64,
) -> AuctionResponse {
    let Some(response) = value.as_object() else {
        return AuctionResponse::error(provider_id, response_time_ms)
            .with_metadata("error_type", json!("parse_response"));
    };
    match response.get("cur") {
        None => {}
        Some(Value::String(currency)) if currency.eq_ignore_ascii_case(DEFAULT_CURRENCY) => {}
        Some(Value::String(currency)) => {
            return AuctionResponse::no_bid(provider_id, response_time_ms)
                .with_metadata("unsupported_currency", json!(currency));
        }
        Some(_) => {
            return AuctionResponse::error(provider_id, response_time_ms)
                .with_metadata("error_type", json!("parse_response"));
        }
    }
    let dimensions_by_slot = build_bid_dimension_index(input);
    let mut diagnostics = ResponseAdmissionDiagnostics::default();
    let mut bids = Vec::new();
    for seatbid in response
        .get("seatbid")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let returned_seat = seatbid
            .get("seat")
            .and_then(Value::as_str)
            .filter(|seat| !seat.is_empty());
        let Some(entries) = seatbid.get("bid").and_then(Value::as_array) else {
            continue;
        };
        for value in entries {
            match extract_standard_bid(value, returned_seat, &dimensions_by_slot) {
                Ok(bid) => bids.push(bid),
                Err(reason) => diagnostics.record(reason),
            }
        }
    }
    let mut parsed = if bids.is_empty() {
        AuctionResponse::no_bid(provider_id, response_time_ms)
    } else {
        AuctionResponse::success(provider_id, bids, response_time_ms)
    };
    diagnostics.attach_to(&mut parsed);
    parsed
}

fn extract_standard_bid(
    value: &Value,
    returned_seat: Option<&str>,
    dimensions_by_slot: &BidDimensionIndex,
) -> Result<Bid, BidRejectionReason> {
    let slot_id = value
        .get("impid")
        .and_then(Value::as_str)
        .filter(|slot_id| !slot_id.is_empty())
        .ok_or(BidRejectionReason::InvalidBid)?
        .to_string();
    let width = parse_optional_bid_dimension(value, "w")?;
    let height = parse_optional_bid_dimension(value, "h")?;
    let (width, height) = resolve_bid_dimensions(dimensions_by_slot, &slot_id, width, height)?;
    let price = value
        .get("price")
        .and_then(Value::as_f64)
        .filter(|price| price.is_finite() && *price >= 0.0)
        .ok_or(BidRejectionReason::InvalidBid)?;
    let creative = value
        .get("adm")
        .and_then(Value::as_str)
        .filter(|creative| !creative.is_empty())
        .map(str::to_string)
        .ok_or(BidRejectionReason::InvalidBid)?;
    Ok(Bid {
        slot_id,
        price: Some(price),
        currency: DEFAULT_CURRENCY.to_string(),
        creative: Some(creative),
        adomain: value
            .get("adomain")
            .and_then(Value::as_array)
            .map(|domains| {
                domains
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            }),
        bidder: returned_seat.unwrap_or("unknown").to_string(),
        returned_seat: returned_seat.map(str::to_string),
        width,
        height,
        nurl: value
            .get("nurl")
            .and_then(Value::as_str)
            .map(str::to_string),
        burl: value
            .get("burl")
            .and_then(Value::as_str)
            .map(str::to_string),
        bid_id: value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string),
        ad_id: value
            .get("adid")
            .and_then(Value::as_str)
            .map(str::to_string),
        creative_id: value
            .get("crid")
            .and_then(Value::as_str)
            .map(str::to_string),
        renderer: None,
        cache_id: None,
        cache_host: None,
        cache_path: None,
        metadata: HashMap::new(),
    })
}
