//! Auction request/response format conversions.
//!
//! This module handles:
//! - Parsing incoming tsjs/Prebid.js format requests
//! - Converting internal auction results to `OpenRTB` 2.x responses

use error_stack::{ensure, Report, ResultExt};
use fastly::http::{header, StatusCode};
use fastly::{Request, Response};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use uuid::Uuid;

use crate::auction::types::OrchestratorExt;
use crate::creative;
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::openrtb::{OpenRtbBid, OpenRtbResponse, ResponseExt, SeatBid};
use crate::settings::Settings;
use crate::synthetic::{generate_synthetic_id, get_or_generate_synthetic_id};

use super::orchestrator::OrchestrationResult;
use super::types::{
    AdFormat, AdSlot, AuctionRequest, DeviceInfo, MediaType, PublisherInfo, SiteInfo, UserInfo,
};

/// Request body format for auction endpoints (tsjs/Prebid.js format).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdRequest {
    pub ad_units: Vec<AdUnit>,
    #[allow(dead_code)]
    pub config: Option<JsonValue>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdUnit {
    pub code: String,
    pub media_types: Option<MediaTypes>,
    pub bids: Option<Vec<BidConfig>>,
}

/// Bidder configuration from the request.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BidConfig {
    pub bidder: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaTypes {
    pub banner: Option<BannerUnit>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BannerUnit {
    pub sizes: Vec<Vec<u32>>,
}

/// Convert tsjs/Prebid.js request format to internal `AuctionRequest`.
///
/// # Errors
///
/// Returns an error if:
/// - Synthetic ID generation fails
/// - Request contains invalid banner sizes (must be [width, height])
pub fn convert_tsjs_to_auction_request(
    body: &AdRequest,
    settings: &Settings,
    req: &Request,
) -> Result<AuctionRequest, Report<TrustedServerError>> {
    // Generate synthetic ID
    let synthetic_id = get_or_generate_synthetic_id(settings, req).change_context(
        TrustedServerError::Auction {
            message: "Failed to generate synthetic ID".to_string(),
        },
    )?;
    let fresh_id =
        generate_synthetic_id(settings, req).change_context(TrustedServerError::Auction {
            message: "Failed to generate fresh ID".to_string(),
        })?;

    // Convert ad units to slots
    let mut slots = Vec::new();
    for unit in &body.ad_units {
        if let Some(media_types) = &unit.media_types {
            if let Some(banner) = &media_types.banner {
                let mut formats = Vec::new();
                for size in &banner.sizes {
                    ensure!(
                        size.len() == 2,
                        TrustedServerError::BadRequest {
                            message: "Invalid banner size; expected [width, height]".to_string(),
                        }
                    );

                    formats.push(AdFormat {
                        width: size[0],
                        height: size[1],
                        media_type: MediaType::Banner,
                    });
                }

                // Extract bidder params from the bids array
                let mut bidders = HashMap::new();
                if let Some(bids) = &unit.bids {
                    for bid in bids {
                        bidders.insert(bid.bidder.clone(), bid.params.clone());
                    }
                }

                slots.push(AdSlot {
                    id: unit.code.clone(),
                    formats,
                    floor_price: None,
                    targeting: HashMap::new(),
                    bidders,
                });
            }
        }
    }

    // Build device info with user-agent (always) and geo (if available)
    let device = Some(DeviceInfo {
        user_agent: req
            .get_header_str("user-agent")
            .map(std::string::ToString::to_string),
        ip: req.get_client_ip_addr().map(|ip| ip.to_string()),
        geo: GeoInfo::from_request(req),
    });

    // Extract optional Permutive segments from the request config
    let mut context = HashMap::new();
    if let Some(ref config) = body.config {
        if let Some(segments) = config.get("permutive_segments") {
            if segments.is_array() {
                log::info!(
                    "Auction request includes {} Permutive segments",
                    segments.as_array().map_or(0, Vec::len)
                );
                context.insert("permutive_segments".to_string(), segments.clone());
            }
        }
    }

    Ok(AuctionRequest {
        id: Uuid::new_v4().to_string(),
        slots,
        publisher: PublisherInfo {
            domain: settings.publisher.domain.clone(),
            page_url: Some(format!("https://{}", settings.publisher.domain)),
        },
        user: UserInfo {
            id: synthetic_id,
            fresh_id,
            consent: None,
        },
        device,
        site: Some(SiteInfo {
            domain: settings.publisher.domain.clone(),
            page: format!("https://{}", settings.publisher.domain),
        }),
        context,
    })
}

/// Convert `OrchestrationResult` to `OpenRTB` response format.
///
/// Returns rewritten creative HTML directly in the `adm` field for inline delivery.
///
/// # Errors
///
/// Returns an error if:
/// - A winning bid is missing a price
/// - The response serialization fails
pub fn convert_to_openrtb_response(
    result: &OrchestrationResult,
    settings: &Settings,
    auction_request: &AuctionRequest,
) -> Result<Response, Report<TrustedServerError>> {
    // Build OpenRTB-style seatbid array
    let mut seatbids = Vec::new();

    for (slot_id, bid) in &result.winning_bids {
        let price = bid.price.ok_or_else(|| {
            Report::new(TrustedServerError::Auction {
                message: format!(
                    "Winning bid for slot '{}' from '{}' has no decoded price",
                    slot_id, bid.bidder
                ),
            })
        })?;

        // Process creative HTML if present - rewrite URLs and return inline
        let creative_html = if let Some(ref raw_creative) = bid.creative {
            // Rewrite creative HTML with proxy URLs for first-party delivery
            let rewritten = creative::rewrite_creative_html(settings, raw_creative);

            log::debug!(
                "Rewritten creative for auction {} slot {} ({} bytes)",
                auction_request.id,
                slot_id,
                rewritten.len()
            );

            rewritten
        } else {
            // No creative provided (e.g., from mediation layer that returns iframe URLs)
            log::warn!(
                "No creative HTML for auction {} slot {} - mediation should have provided creative",
                auction_request.id,
                slot_id
            );
            String::new()
        };

        let openrtb_bid = OpenRtbBid {
            id: format!("{}-{}", bid.bidder, slot_id),
            impid: slot_id.to_string(),
            price,
            adm: Some(creative_html),
            crid: Some(format!("{}-creative", bid.bidder)),
            w: Some(bid.width),
            h: Some(bid.height),
            adomain: Some(bid.adomain.clone().unwrap_or_default()),
        };

        seatbids.push(SeatBid {
            seat: Some(bid.bidder.clone()),
            bid: vec![openrtb_bid],
        });
    }

    // Determine strategy name for response metadata
    let strategy_name = if settings.auction.has_mediator() {
        "parallel_mediation"
    } else {
        "parallel_only"
    };

    let response_body = OpenRtbResponse {
        id: auction_request.id.to_string(),
        seatbid: seatbids,
        ext: Some(ResponseExt {
            orchestrator: OrchestratorExt {
                strategy: strategy_name.to_string(),
                providers: result.provider_responses.len(),
                total_bids: result.total_bids(),
                time_ms: result.total_time_ms,
            },
        }),
    };

    let body_bytes =
        serde_json::to_vec(&response_body).change_context(TrustedServerError::Auction {
            message: "Failed to serialize auction response".to_string(),
        })?;

    Ok(Response::from_status(StatusCode::OK)
        .with_header(header::CONTENT_TYPE, "application/json")
        .with_header("X-Synthetic-ID", &auction_request.user.id)
        .with_header("X-Synthetic-Fresh", &auction_request.user.fresh_id)
        .with_header("X-Synthetic-Trusted-Server", &auction_request.user.id)
        .with_body(body_bytes))
}
