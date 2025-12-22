//! Auction orchestration module for managing multi-provider bidding.
//!
//! This module provides an extensible framework for running auctions across
//! multiple providers (Prebid, Amazon APS, Google GAM, etc.) with support for
//! parallel execution and mediation strategies.
//!
//! Note: Individual auction providers are located in the `integrations` module
//! (e.g., `crate::integrations::aps`, `crate::integrations::gam`, `crate::integrations::prebid`).

use crate::settings::Settings;
use std::sync::{Arc, OnceLock};

pub mod config;
pub mod orchestrator;
pub mod provider;
pub mod types;

pub use config::AuctionConfig;
pub use orchestrator::AuctionOrchestrator;
pub use provider::AuctionProvider;
pub use types::{
    AdFormat, AuctionContext, AuctionRequest, AuctionResponse, Bid, BidStatus, MediaType,
};

/// Global auction orchestrator singleton.
///
/// Initialized once on first access with the provided settings.
/// All providers are registered during initialization.
static GLOBAL_ORCHESTRATOR: OnceLock<AuctionOrchestrator> = OnceLock::new();

/// Type alias for provider builder functions.
type ProviderBuilder = fn(&Settings) -> Vec<Arc<dyn AuctionProvider>>;

/// Returns the list of all available provider builder functions.
///
/// This list is used to auto-discover and register auction providers from settings.
/// Each builder function checks the settings for its specific provider configuration
/// and returns any enabled providers.
fn provider_builders() -> &'static [ProviderBuilder] {
    &[
        crate::integrations::prebid::register_auction_provider,
        crate::integrations::aps::register_providers,
        crate::integrations::gam::register_providers,
    ]
}

/// Initialize the global auction orchestrator.
///
/// This function should be called once at application startup to initialize the orchestrator
/// with the application settings. All auction providers are automatically discovered and
/// registered during initialization.
///
/// # Arguments
/// * `settings` - Application settings used to configure the orchestrator and providers
///
/// # Returns
/// Reference to the global orchestrator instance
///
/// # Panics
/// Panics if called more than once (orchestrator already initialized)
pub fn init_orchestrator(settings: &Settings) -> &'static AuctionOrchestrator {
    GLOBAL_ORCHESTRATOR.get_or_init(|| {
        log::info!("Initializing global auction orchestrator");

        let mut orchestrator = AuctionOrchestrator::new(settings.auction.clone());

        // Auto-discover and register all auction providers from settings
        for builder in provider_builders() {
            for provider in builder(settings) {
                orchestrator.register_provider(provider);
            }
        }

        log::info!(
            "Auction orchestrator initialized with {} providers",
            orchestrator.provider_count()
        );

        orchestrator
    })
}

/// Get the global auction orchestrator.
///
/// Returns a reference to the orchestrator if it has been initialized via `init_orchestrator()`.
///
/// # Returns
/// * `Some(&'static AuctionOrchestrator)` if the orchestrator has been initialized
/// * `None` if `init_orchestrator()` has not been called yet
pub fn get_orchestrator() -> Option<&'static AuctionOrchestrator> {
    GLOBAL_ORCHESTRATOR.get()
}

// ============================================================================
// Top-Level Auction Handler
// ============================================================================

use error_stack::{Report, ResultExt};
use fastly::http::{header, StatusCode};
use fastly::{Request, Response};
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use std::collections::HashMap;
use uuid::Uuid;

use crate::creative;
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::synthetic::{generate_synthetic_id, get_or_generate_synthetic_id};

/// Request body format for auction endpoints (tsjs/Prebid.js format).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdRequest {
    ad_units: Vec<AdUnit>,
    #[allow(dead_code)]
    config: Option<JsonValue>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdUnit {
    code: String,
    media_types: Option<MediaTypes>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaTypes {
    banner: Option<BannerUnit>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BannerUnit {
    sizes: Vec<Vec<u32>>,
}

/// Handle auction request from /third-party/ad or /auction/run endpoints.
///
/// This is the main entry point for running header bidding auctions.
/// It orchestrates bids from multiple providers (Prebid, APS, GAM, etc.) and returns
/// the winning bids in OpenRTB format.
pub async fn handle_auction(
    settings: &Settings,
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

    // Get the global orchestrator (should be initialized at startup)
    let orchestrator = get_orchestrator().ok_or_else(|| {
        Report::new(TrustedServerError::Auction {
            message: "Auction orchestrator not initialized. Call init_orchestrator() at startup."
                .to_string(),
        })
    })?;

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
        "Auction completed: {} bidders, {} winning bids, {}ms total",
        result.bidder_responses.len(),
        result.winning_bids.len(),
        result.total_time_ms
    );

    // Convert to OpenRTB response format
    convert_to_openrtb_response(&result, settings)
}

/// Convert tsjs/Prebid.js request format to internal AuctionRequest.
fn convert_tsjs_to_auction_request(
    body: &AdRequest,
    settings: &Settings,
    req: &Request,
) -> Result<AuctionRequest, Report<TrustedServerError>> {
    use types::{AdSlot, DeviceInfo, PublisherInfo, SiteInfo, UserInfo};

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
                let formats: Vec<AdFormat> = banner
                    .sizes
                    .iter()
                    .map(|size| AdFormat {
                        width: size[0],
                        height: size[1],
                        media_type: MediaType::Banner,
                    })
                    .collect();

                slots.push(AdSlot {
                    id: unit.code.clone(),
                    formats,
                    floor_price: None,
                    targeting: std::collections::HashMap::new(),
                });
            }
        }
    }

    // Get geo info if available
    let device = GeoInfo::from_request(req).map(|geo| DeviceInfo {
        user_agent: req.get_header_str("user-agent").map(|s| s.to_string()),
        ip: req.get_client_ip_addr().map(|ip| ip.to_string()),
        geo: Some(types::GeoInfo {
            country: Some(geo.country),
            region: geo.region,
            city: Some(geo.city),
        }),
    });

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
        context: HashMap::new(),
    })
}

/// Convert OrchestrationResult to OpenRTB response format.
fn convert_to_openrtb_response(
    result: &orchestrator::OrchestrationResult,
    settings: &Settings,
) -> Result<Response, Report<TrustedServerError>> {
    // Build OpenRTB-style seatbid array
    let mut seatbids = Vec::new();

    for (slot_id, bid) in &result.winning_bids {
        let rewritten_creative = creative::rewrite_creative_html(&bid.creative, settings);

        let bid_obj = json!({
            "id": format!("{}-{}", bid.bidder, slot_id),
            "impid": slot_id,
            "price": bid.price,
            "adm": rewritten_creative,
            "crid": format!("{}-creative", bid.bidder),
            "w": bid.width,
            "h": bid.height,
            "adomain": bid.adomain.clone().unwrap_or_default(),
        });

        seatbids.push(json!({
            "seat": bid.bidder,
            "bid": [bid_obj]
        }));
    }

    let response_body = json!({
        "id": "auction-response",
        "seatbid": seatbids,
        "ext": {
            "orchestrator": {
                "strategy": settings.auction.strategy,
                "bidders": result.bidder_responses.len(),
                "total_bids": result.total_bids(),
                "time_ms": result.total_time_ms
            }
        }
    });

    let body_bytes =
        serde_json::to_vec(&response_body).change_context(TrustedServerError::Auction {
            message: "Failed to serialize auction response".to_string(),
        })?;

    Ok(Response::from_status(StatusCode::OK)
        .with_header(header::CONTENT_TYPE, "application/json")
        .with_body(body_bytes))
}
