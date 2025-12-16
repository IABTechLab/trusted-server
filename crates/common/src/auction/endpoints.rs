//! HTTP endpoint handlers for auction requests.

use error_stack::{Report, ResultExt};
use fastly::{Request, Response};

use crate::auction::formats::AdRequest;
use crate::error::TrustedServerError;
use crate::settings::Settings;

use super::formats::{convert_to_openrtb_response, convert_tsjs_to_auction_request};
use super::types::AuctionContext;
use super::AuctionOrchestrator;

/// Handle auction request from /auction endpoint.
///
/// This is the main entry point for running header bidding auctions.
/// It orchestrates bids from multiple providers (Prebid, APS, GAM, etc.) and returns
/// the winning bids in OpenRTB format with creative HTML inline in the `adm` field.
pub async fn handle_auction(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    // Parse request body
    let body: AdRequest = serde_json::from_slice(&req.take_body_bytes()).change_context(
        TrustedServerError::Auction {
            message: "Failed to parse auction request body".to_string(),
        },
    )?;

    log::info!(
        "Auction request received for {} ad units",
        body.ad_units.len()
    );

    // Convert tsjs request format to auction request
    let auction_request = convert_tsjs_to_auction_request(&body, settings, &req)?;

    // Create auction context
    let context = AuctionContext {
        settings,
        request: &req,
        timeout_ms: settings.auction.timeout_ms,
    };

    // Run the auction
    let result = orchestrator
        .run_auction(&auction_request, &context)
        .await
        .change_context(TrustedServerError::Auction {
            message: "Auction orchestration failed".to_string(),
        })?;

    log::info!(
        "Auction completed: {} providers, {} winning bids, {}ms total",
        result.provider_responses.len(),
        result.winning_bids.len(),
        result.total_time_ms
    );

    // Convert to OpenRTB response format with inline creative HTML
    convert_to_openrtb_response(&result, settings, &auction_request)
}
