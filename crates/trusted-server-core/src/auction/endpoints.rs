//! HTTP endpoint handlers for auction requests.

use error_stack::{Report, ResultExt};
use fastly::{Request, Response};

use crate::auction::formats::AdRequest;
use crate::consent;
use crate::cookies::handle_request_cookies;
use crate::edge_cookie::get_or_generate_ec_id;
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

    // Generate EC ID early so the consent pipeline can use it for
    // KV Store fallback/write operations.
    let ec_id = get_or_generate_ec_id(settings, services, &req).change_context(
        TrustedServerError::Auction {
            message: "Failed to generate EC ID".to_string(),
        },
    )?;

    // Extract consent from request cookies, headers, and geo.
    let cookie_jar = handle_request_cookies(&req)?;
    let geo = services
        .geo()
        .lookup(services.client_info.client_ip)
        .unwrap_or_else(|e| {
            log::warn!("geo lookup failed: {e}");
            None
        });
    let consent_context = consent::build_consent_context(&consent::ConsentPipelineInput {
        jar: cookie_jar.as_ref(),
        req: &req,
        config: &settings.consent,
        geo: geo.as_ref(),
        ec_id: Some(ec_id.as_str()),
    });

    // Convert tsjs request format to auction request
    let auction_request = convert_tsjs_to_auction_request(
        &body,
        settings,
        services,
        &req,
        consent_context,
        &ec_id,
        geo,
    )?;

    // Create auction context
    let context = AuctionContext {
        settings,
        request: &req,
        client_info: &services.client_info,
        timeout_ms: settings.auction.timeout_ms,
        provider_responses: None,
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
