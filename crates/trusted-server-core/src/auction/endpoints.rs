//! HTTP endpoint handlers for auction requests.

use error_stack::{Report, ResultExt};
use fastly::{Request, Response};

use crate::auction::formats::AdRequest;
use crate::ec::EcContext;
use crate::error::TrustedServerError;
use crate::platform::RuntimeServices;
use crate::settings::Settings;

use super::formats::{convert_to_openrtb_response, convert_tsjs_to_auction_request};
use super::types::AuctionContext;
use super::AuctionOrchestrator;

/// Handle auction request from /auction endpoint.
///
/// This is the main entry point for running header bidding auctions.
/// It orchestrates bids from multiple providers (Prebid, APS, GAM, etc.) and returns
/// the winning bids in `OpenRTB` format with creative HTML inline in the `adm` field.
///
/// # Errors
///
/// Returns an error if:
/// - The request body cannot be parsed
/// - The auction request conversion fails (e.g., invalid ad units)
/// - The auction execution fails
/// - The response cannot be serialized
pub async fn handle_auction(
    settings: &Settings,
    orchestrator: &AuctionOrchestrator,
    services: &RuntimeServices,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    // Read EC state before consuming the request body.
    let mut ec_context = EcContext::read_from_request(settings, &req).change_context(
        TrustedServerError::Auction {
            message: "Failed to read EC context".to_string(),
        },
    )?;

    // Auction is an organic handler — generate EC if needed.
    if let Err(err) = ec_context.generate_if_needed(settings) {
        log::warn!("EC generation failed for auction: {err:?}");
    }

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

    // Only forward the EC ID to auction partners when consent allows it.
    // A returning user may still have a ts-ec cookie but have since
    // withdrawn consent — forwarding that revoked ID to bidders would
    // defeat the consent gating.
    let ec_id = if ec_context.ec_allowed() {
        ec_context.ec_value().unwrap_or("")
    } else {
        ""
    };
    let consent_context = ec_context.consent().clone();

    let geo = services
        .geo()
        .lookup(services.client_info.client_ip)
        .unwrap_or_else(|e| {
            log::warn!("geo lookup failed: {e}");
            None
        });

    // Convert tsjs request format to auction request
    let auction_request = convert_tsjs_to_auction_request(
        &body,
        settings,
        services,
        &req,
        consent_context,
        ec_id,
        geo,
    )?;

    // Create auction context
    let context = AuctionContext {
        settings,
        request: &req,
        client_info: &services.client_info,
        timeout_ms: settings.auction.timeout_ms,
        provider_responses: None,
        services,
    };

    // Run the auction
    let result = orchestrator
        .run_auction(&auction_request, &context, services)
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
