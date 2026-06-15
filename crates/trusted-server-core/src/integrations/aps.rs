//! Amazon Publisher Services (APS/TAM) integration.
//!
//! This module provides the APS auction provider for server-side bidding.

use async_trait::async_trait;
use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt};
use http::{header, Method};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use std::collections::HashMap;
use std::time::Duration;
use validator::Validate;

use crate::auction::provider::AuctionProvider;
use crate::auction::types::{AuctionContext, AuctionRequest, AuctionResponse, Bid, MediaType};
use crate::backend::BackendConfig;
use crate::error::TrustedServerError;
use crate::integrations::{
    collect_response_bounded, ensure_integration_backend, UPSTREAM_RTB_MAX_RESPONSE_BYTES,
};
use crate::platform::{PlatformHttpRequest, PlatformPendingRequest, PlatformResponse};
use crate::settings::IntegrationConfig;

// ============================================================================
// APS TAM API Types
// ============================================================================

/// APS TAM bid request format based on /e/dtb/bid endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApsBidRequest {
    /// Publisher ID
    #[serde(rename = "pubId")]
    pub_id: String,

    /// Slot configurations
    slots: Vec<ApsSlot>,

    /// Page URL
    #[serde(rename = "pageUrl", skip_serializing_if = "Option::is_none")]
    page_url: Option<String>,

    /// User agent
    #[serde(rename = "ua", skip_serializing_if = "Option::is_none")]
    user_agent: Option<String>,

    /// Timeout in milliseconds
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout: Option<u32>,

    /// GDPR consent information.
    #[serde(skip_serializing_if = "Option::is_none")]
    gdpr: Option<ApsGdprConsent>,

    /// US Privacy (CCPA) string.
    #[serde(rename = "usPrivacy", skip_serializing_if = "Option::is_none")]
    us_privacy: Option<String>,

    /// GPP consent string.
    #[serde(skip_serializing_if = "Option::is_none")]
    gpp: Option<String>,

    /// GPP section IDs as comma-separated string.
    #[serde(rename = "gppSid", skip_serializing_if = "Option::is_none")]
    gpp_sid: Option<String>,
}

/// GDPR consent information for APS requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApsGdprConsent {
    /// Whether GDPR applies to this request.
    enabled: bool,
    /// TCF v2 consent string.
    #[serde(skip_serializing_if = "Option::is_none")]
    consent: Option<String>,
}

/// APS slot configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApsSlot {
    /// Slot identifier
    #[serde(rename = "slotID")]
    slot_id: String,

    /// Ad sizes [[width, height], ...]
    sizes: Vec<[u32; 2]>,

    /// Slot name (optional)
    #[serde(rename = "slotName", skip_serializing_if = "Option::is_none")]
    slot_name: Option<String>,
}

/// APS TAM bid response format matching real Amazon API.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApsBidResponse {
    /// Contextual wrapper containing all response data
    contextual: ApsContextual,
}

/// APS Contextual response containing slots and metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApsContextual {
    /// Array of slot responses (one per requested slot)
    #[serde(default)]
    slots: Vec<ApsSlotResponse>,

    /// Event tracking host
    #[serde(skip_serializing_if = "Option::is_none")]
    host: Option<String>,

    /// Response status ("ok", "error", etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,

    /// Client-side feature enablement flag
    #[serde(skip_serializing_if = "Option::is_none")]
    cfe: Option<bool>,

    /// Event tracking enabled
    #[serde(skip_serializing_if = "Option::is_none")]
    ev: Option<bool>,

    /// Client feature name (CSM script path)
    #[serde(skip_serializing_if = "Option::is_none")]
    cfn: Option<String>,

    /// Callback version
    #[serde(skip_serializing_if = "Option::is_none")]
    cb: Option<String>,

    /// Campaign tracking URL
    #[serde(skip_serializing_if = "Option::is_none")]
    cmp: Option<String>,
}

/// Individual APS slot response matching real Amazon format.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApsSlotResponse {
    /// Slot ID this response is for
    #[serde(rename = "slotID")]
    slot_id: String,

    /// Creative size (e.g., "300x250")
    size: String,

    /// Creative ID
    #[serde(skip_serializing_if = "Option::is_none")]
    crid: Option<String>,

    /// Media type ("d" for display, "v" for video)
    #[serde(rename = "mediaType", skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,

    /// Fill indicator flag ("1" = filled, "0" = no fill)
    #[serde(skip_serializing_if = "Option::is_none")]
    fif: Option<String>,

    /// List of targeting key names that are set on this slot
    #[serde(default)]
    targeting: Vec<String>,

    /// List of metadata field names
    #[serde(default)]
    meta: Vec<String>,

    // Targeting key-value pairs (returned as flat fields)
    /// Amazon impression ID (unique identifier for this bid)
    #[serde(skip_serializing_if = "Option::is_none")]
    amzniid: Option<String>,

    /// Amazon encoded bid price
    #[serde(skip_serializing_if = "Option::is_none")]
    amznbid: Option<String>,

    /// Amazon encoded price (alternative encoding)
    #[serde(skip_serializing_if = "Option::is_none")]
    amznp: Option<String>,

    /// Amazon size in `WxH` format (e.g., "300x250")
    #[serde(skip_serializing_if = "Option::is_none")]
    amznsz: Option<String>,

    /// Amazon auction context type ("OPEN", "PRIVATE", etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    amznactt: Option<String>,
}

// ============================================================================
// Real APS Provider
// ============================================================================

/// Configuration for APS integration.
#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
pub struct ApsConfig {
    /// Whether APS integration is enabled
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// APS publisher ID (accepts both string and integer from config)
    #[serde(deserialize_with = "deserialize_pub_id")]
    pub pub_id: String,

    /// APS API endpoint
    #[serde(default = "default_endpoint")]
    #[validate(url)]
    pub endpoint: String,

    /// Timeout in milliseconds
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u32,
}

/// Custom deserializer for `pub_id` that accepts both string and integer
fn deserialize_pub_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};
    use std::fmt;

    struct PubIdVisitor;

    impl<'de> Visitor<'de> for PubIdVisitor {
        type Value = String;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a string or integer for pub_id")
        }

        fn visit_str<E>(self, value: &str) -> Result<String, E>
        where
            E: de::Error,
        {
            Ok(value.to_string())
        }

        fn visit_string<E>(self, value: String) -> Result<String, E>
        where
            E: de::Error,
        {
            Ok(value)
        }

        fn visit_i64<E>(self, value: i64) -> Result<String, E>
        where
            E: de::Error,
        {
            Ok(value.to_string())
        }

        fn visit_u64<E>(self, value: u64) -> Result<String, E>
        where
            E: de::Error,
        {
            Ok(value.to_string())
        }
    }

    deserializer.deserialize_any(PubIdVisitor)
}

fn default_enabled() -> bool {
    false
}

fn default_endpoint() -> String {
    "https://aax.amazon-adsystem.com/e/dtb/bid".to_string()
}

fn default_timeout_ms() -> u32 {
    800
}

impl Default for ApsConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            pub_id: String::new(),
            endpoint: default_endpoint(),
            timeout_ms: default_timeout_ms(),
        }
    }
}

impl IntegrationConfig for ApsConfig {
    fn is_enabled(&self) -> bool {
        self.enabled
    }
}

/// Amazon APS auction provider.
pub struct ApsAuctionProvider {
    config: ApsConfig,
    // Maps APS slot ID → creative opportunity slot ID for the in-flight request.
    // Written by request_bids before the async send; read by parse_response when the
    // response arrives. Safe because Fastly Compute runs each request in an isolated
    // single-threaded Wasm instance — the Mutex never contends in practice.
    //
    // Unlike adserver_mock's bid index (rebuilt in parse_response_with_context
    // from context.provider_responses), this map derives from the AuctionRequest,
    // which AuctionContext does not carry — migrating it off provider-instance
    // state needs the request threaded through the context first.
    slot_id_map: std::sync::Mutex<HashMap<String, String>>,
}

impl ApsAuctionProvider {
    /// Create a new APS auction provider.
    #[must_use]
    pub fn new(config: ApsConfig) -> Self {
        Self {
            config,
            slot_id_map: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Convert unified `AuctionRequest` to APS TAM bid request format.
    ///
    /// Returns the serialisable `ApsBidRequest` and a map of APS slot ID →
    /// creative-opportunity slot ID so the caller can remap bids in the response.
    /// Populates consent fields (GDPR, US Privacy, GPP) from the
    /// [`ConsentContext`](crate::consent::ConsentContext) attached to the request.
    ///
    /// `timeout_ms` is the effective auction budget for this provider (already
    /// capped by the orchestrator) — advertised to APS so it never expects more
    /// time than the edge will actually wait.
    fn to_aps_request(
        &self,
        request: &AuctionRequest,
        timeout_ms: u32,
    ) -> (ApsBidRequest, HashMap<String, String>) {
        let mut slot_id_map: HashMap<String, String> = HashMap::new();
        let slots: Vec<ApsSlot> = request
            .slots
            .iter()
            .map(|slot| {
                // Use the APS-specific slot ID from [slot.providers.aps] if configured;
                // fall back to the creative-opportunity slot ID otherwise.
                let aps_slot_id = slot
                    .bidders
                    .get("aps")
                    .and_then(|p| p.get("slotID"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&slot.id)
                    .to_string();
                slot_id_map.insert(aps_slot_id.clone(), slot.id.clone());

                // Extract sizes from banner formats
                let sizes: Vec<[u32; 2]> = slot
                    .formats
                    .iter()
                    .filter(|f| f.media_type == MediaType::Banner)
                    .map(|f| [f.width, f.height])
                    .collect();

                ApsSlot {
                    slot_id: aps_slot_id,
                    sizes,
                    slot_name: Some(slot.id.clone()),
                }
            })
            .collect();

        // Build consent fields from ConsentContext
        let consent_ctx = request.user.consent.as_ref();
        let gdpr = consent_ctx.map(|ctx| ApsGdprConsent {
            enabled: ctx.gdpr_applies,
            consent: ctx.raw_tc_string.clone(),
        });
        let us_privacy = consent_ctx.and_then(|ctx| ctx.raw_us_privacy.clone());
        let gpp = consent_ctx.and_then(|ctx| ctx.raw_gpp_string.clone());
        let gpp_sid = consent_ctx.and_then(|ctx| {
            ctx.gpp_section_ids.as_ref().map(|ids| {
                ids.iter()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            })
        });

        let bid_request = ApsBidRequest {
            pub_id: self.config.pub_id.clone(),
            slots,
            page_url: request.publisher.page_url.clone(),
            user_agent: request.device.as_ref().and_then(|d| d.user_agent.clone()),
            timeout: Some(timeout_ms),
            gdpr,
            us_privacy,
            gpp,
            gpp_sid,
        };
        (bid_request, slot_id_map)
    }

    /// Parse size string (e.g., "300x250") into width and height.
    fn parse_size(size: &str) -> Option<(u32, u32)> {
        let parts: Vec<&str> = size.split('x').collect();
        if parts.len() == 2 {
            if let (Ok(w), Ok(h)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                return Some((w, h));
            }
        }
        None
    }

    /// Parse a single APS slot response into unified Bid format.
    ///
    /// Note: Price is NOT decoded here. The encoded price is stored in metadata
    /// and will be decoded by the mediation layer (mocktioneer). This simulates
    /// real-world APS where only Amazon/GAM can decode the proprietary price encoding.
    fn parse_aps_slot(&self, slot: &ApsSlotResponse) -> Result<Bid, ()> {
        // Only process filled slots (fif == "1")
        if slot.fif.as_deref() != Some("1") {
            return Err(());
        }

        // Verify we have an encoded price field
        let encoded_price = slot.amznbid.as_ref().or(slot.amznp.as_ref());
        if encoded_price.is_none() {
            log::debug!(
                "APS: slot '{}' has no encoded price, skipping",
                slot.slot_id
            );
            return Err(());
        }

        // Parse size from "WxH" format
        let (width, height) = match Self::parse_size(&slot.size) {
            Some(dims) => dims,
            None => {
                log::debug!(
                    "APS: slot '{}' has malformed size '{}', skipping",
                    slot.slot_id,
                    slot.size
                );
                return Err(());
            }
        };

        // Build metadata from targeting keys - includes encoded price for mediation
        let mut metadata = HashMap::new();
        if let Some(ref amzniid) = slot.amzniid {
            metadata.insert("amzniid".to_string(), json!(amzniid));
        }
        if let Some(ref amznbid) = slot.amznbid {
            metadata.insert("amznbid".to_string(), json!(amznbid));
        }
        if let Some(ref amznp) = slot.amznp {
            metadata.insert("amznp".to_string(), json!(amznp));
        }
        if let Some(ref amznsz) = slot.amznsz {
            metadata.insert("amznsz".to_string(), json!(amznsz));
        }
        if let Some(ref amznactt) = slot.amznactt {
            metadata.insert("amznactt".to_string(), json!(amznactt));
        }

        // APS doesn't return creative HTML - only targeting keys
        // The creative will be generated by the mediation layer
        // Price is None - will be decoded by mediation layer from amznbid metadata
        Ok(Bid {
            slot_id: slot.slot_id.clone(),
            price: None, // Encoded price in metadata, decoded by mediation
            currency: "USD".to_string(),
            creative: None,
            adomain: None, // APS doesn't provide adomain in response
            bidder: "amazon-aps".to_string(),
            width,
            height,
            nurl: None, // Real APS uses client-side event tracking
            burl: None,
            ad_id: None,
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata,
        })
    }

    /// Parse APS TAM response into unified `AuctionResponse`.
    fn parse_aps_response(&self, json: &Json, response_time_ms: u64) -> AuctionResponse {
        let mut bids = Vec::new();

        // Try to parse as ApsBidResponse with contextual wrapper
        if let Ok(aps_response) = serde_json::from_value::<ApsBidResponse>(json.clone()) {
            log::debug!(
                "APS: parsed contextual response with {} slots",
                aps_response.contextual.slots.len()
            );

            // Take the map by value so it does not linger on the provider
            // across requests if the Fastly Compute runtime ever reuses Wasm
            // instances. Today each request gets its own instance so this is
            // belt-and-suspenders; tomorrow it may not be.
            let slot_map = std::mem::take(
                &mut *self
                    .slot_id_map
                    .lock()
                    .expect("should lock APS slot id map"),
            );
            for slot in aps_response.contextual.slots {
                match self.parse_aps_slot(&slot) {
                    Ok(mut bid) => {
                        // Remap APS slot ID (e.g. "aps-slot-atf-sidebar") back to the
                        // creative-opportunity slot ID (e.g. "atf_sidebar_ad") so the
                        // mediator and bid_map can match by creative slot ID.
                        if let Some(creative_id) = slot_map.get(&bid.slot_id) {
                            bid.slot_id = creative_id.clone();
                        }
                        let encoded_price = bid
                            .metadata
                            .get("amznbid")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        log::debug!(
                            "APS: parsed bid for slot '{}' with encoded price (to be decoded by mediation)",
                            bid.slot_id,
                        );
                        log::trace!(
                            "APS: slot '{}' encoded price: {}",
                            bid.slot_id,
                            encoded_price
                        );
                        bids.push(bid);
                    }
                    Err(_) => {
                        log::debug!("APS: skipped slot (no fill or invalid)");
                    }
                }
            }
        } else {
            log::warn!("APS: failed to parse response as contextual format");
        }

        if bids.is_empty() {
            AuctionResponse::no_bid("aps", response_time_ms)
        } else {
            AuctionResponse::success("aps", bids, response_time_ms)
        }
    }
}

#[async_trait(?Send)]
impl AuctionProvider for ApsAuctionProvider {
    fn provider_name(&self) -> &'static str {
        "aps"
    }

    async fn request_bids(
        &self,
        request: &AuctionRequest,
        context: &AuctionContext<'_>,
    ) -> Result<PlatformPendingRequest, Report<TrustedServerError>> {
        log::info!(
            "APS: requesting bids for {} slots (pub_id: {})",
            request.slots.len(),
            self.config.pub_id
        );

        // Transform to APS format; store the APS-slot-ID → creative-slot-ID map so
        // parse_response can remap bids back to the creative opportunity slot ID.
        // `context.timeout_ms` is the effective budget the orchestrator granted
        // this provider — the payload must advertise the same deadline the edge
        // backend enforces below.
        let (aps_request, slot_id_map) = self.to_aps_request(request, context.timeout_ms);
        *self
            .slot_id_map
            .lock()
            .expect("should lock APS slot id map") = slot_id_map;

        // Serialize to JSON
        let aps_json =
            serde_json::to_value(&aps_request).change_context(TrustedServerError::Auction {
                message: "Failed to serialize APS bid request".to_string(),
            })?;

        log::trace!("APS: sending bid request: {:?}", aps_json);

        // Create HTTP POST request
        let aps_body =
            serde_json::to_vec(&aps_json).change_context(TrustedServerError::Auction {
                message: "Failed to serialize APS request body".to_string(),
            })?;
        let aps_req = http::Request::builder()
            .method(Method::POST)
            .uri(&self.config.endpoint)
            .header(header::CONTENT_TYPE, "application/json")
            .body(EdgeBody::from(aps_body))
            .change_context(TrustedServerError::Auction {
                message: "Failed to build APS request".to_string(),
            })?;

        let backend_name = ensure_integration_backend(
            context.services,
            &self.config.endpoint,
            "aps",
            Some(Duration::from_millis(u64::from(context.timeout_ms))),
        )?;

        let pending = context
            .services
            .http_client()
            .send_async(PlatformHttpRequest::new(aps_req, backend_name))
            .await
            .change_context(TrustedServerError::Auction {
                message: "Failed to send async request to APS".to_string(),
            })?;

        Ok(pending)
    }

    async fn parse_response(
        &self,
        response: PlatformResponse,
        response_time_ms: u64,
    ) -> Result<AuctionResponse, Report<TrustedServerError>> {
        let response = response.response;

        // Check status code
        if !response.status().is_success() {
            log::warn!("APS returned non-success status: {}", response.status());
            return Ok(AuctionResponse::error("aps", response_time_ms));
        }

        // Parse response body — collect_response_bounded caps memory from misbehaving providers.
        let body_bytes =
            collect_response_bounded(response.into_body(), UPSTREAM_RTB_MAX_RESPONSE_BYTES, "aps")
                .await
                .change_context(TrustedServerError::Auction {
                    message: "Failed to read APS response body".to_string(),
                })?;
        let response_json: Json =
            serde_json::from_slice(&body_bytes).change_context(TrustedServerError::Auction {
                message: "Failed to parse APS response JSON".to_string(),
            })?;

        log::trace!("APS: received response: {:?}", response_json);

        // Transform to unified format
        let auction_response = self.parse_aps_response(&response_json, response_time_ms);

        log::info!(
            "APS returned {} bids in {}ms",
            auction_response.bids.len(),
            response_time_ms
        );

        Ok(auction_response)
    }

    fn supports_media_type(&self, media_type: &MediaType) -> bool {
        // APS supports banner and video formats
        matches!(media_type, MediaType::Banner | MediaType::Video)
    }

    fn timeout_ms(&self) -> u32 {
        self.config.timeout_ms
    }

    fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    fn backend_name(&self, timeout_ms: u32) -> Option<String> {
        BackendConfig::backend_name_for_url(
            &self.config.endpoint,
            true,
            Duration::from_millis(u64::from(timeout_ms)),
        )
        .inspect_err(|e| {
            log::error!(
                "Failed to create backend for APS endpoint '{}': {e:?}",
                self.config.endpoint
            );
        })
        .ok()
    }
}

// ============================================================================
// Provider Auto-Registration
// ============================================================================

use crate::settings::Settings;
use std::sync::Arc;

/// Auto-register APS provider based on settings configuration.
///
/// Returns the APS provider if enabled in settings.
///
/// # Errors
///
/// Returns an error when the APS provider is enabled with invalid
/// configuration.
pub fn register_providers(
    settings: &Settings,
) -> Result<Vec<Arc<dyn AuctionProvider>>, Report<TrustedServerError>> {
    let mut providers: Vec<Arc<dyn AuctionProvider>> = Vec::new();

    // Check for real APS provider configuration
    match settings.integration_config::<ApsConfig>("aps") {
        Ok(Some(config)) => {
            log::info!(
                "Registering real APS provider (pub_id: {}, endpoint: {})",
                config.pub_id,
                config.endpoint
            );
            providers.push(Arc::new(ApsAuctionProvider::new(config)));
        }
        Ok(None) => {
            log::debug!("APS integration config found but is disabled");
        }
        Err(e) => {
            return Err(e);
        }
    }

    Ok(providers)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::types::*;

    fn create_test_auction_request() -> AuctionRequest {
        AuctionRequest {
            id: "test-auction-123".to_string(),
            slots: vec![
                AdSlot {
                    id: "header-banner".to_string(),
                    formats: vec![
                        AdFormat {
                            media_type: MediaType::Banner,
                            width: 728,
                            height: 90,
                        },
                        AdFormat {
                            media_type: MediaType::Banner,
                            width: 970,
                            height: 250,
                        },
                    ],
                    floor_price: Some(1.50),
                    targeting: HashMap::new(),
                    bidders: HashMap::new(),
                },
                AdSlot {
                    id: "sidebar".to_string(),
                    formats: vec![AdFormat {
                        media_type: MediaType::Banner,
                        width: 300,
                        height: 250,
                    }],
                    floor_price: Some(1.00),
                    targeting: HashMap::new(),
                    bidders: HashMap::new(),
                },
            ],
            publisher: PublisherInfo {
                domain: "test.com".to_string(),
                page_url: Some("https://test.com/article".to_string()),
            },
            user: UserInfo {
                id: Some("user-123".to_string()),
                consent: None,
                eids: None,
            },
            device: Some(DeviceInfo {
                user_agent: Some("Mozilla/5.0".to_string()),
                ip: Some("192.168.1.1".to_string()),
                geo: None,
            }),
            site: None,
            context: HashMap::new(),
        }
    }

    #[test]
    fn test_aps_request_transformation() {
        let config = ApsConfig {
            enabled: true,
            pub_id: "5128".to_string(),
            endpoint: "https://aax.amazon-adsystem.com/e/dtb/bid".to_string(),
            timeout_ms: 800,
        };

        let provider = ApsAuctionProvider::new(config);
        let auction_request = create_test_auction_request();
        let (aps_request, _slot_id_map) = provider.to_aps_request(&auction_request, 800);

        // Verify basic fields
        assert_eq!(aps_request.pub_id, "5128");
        assert_eq!(aps_request.slots.len(), 2);
        assert_eq!(
            aps_request.page_url,
            Some("https://test.com/article".to_string())
        );
        assert_eq!(aps_request.user_agent, Some("Mozilla/5.0".to_string()));
        assert_eq!(aps_request.timeout, Some(800));

        // Verify first slot
        let slot1 = &aps_request.slots[0];
        assert_eq!(slot1.slot_id, "header-banner");
        assert_eq!(slot1.sizes.len(), 2);
        assert_eq!(slot1.sizes[0], [728, 90]);
        assert_eq!(slot1.sizes[1], [970, 250]);

        // Verify second slot
        let slot2 = &aps_request.slots[1];
        assert_eq!(slot2.slot_id, "sidebar");
        assert_eq!(slot2.sizes.len(), 1);
        assert_eq!(slot2.sizes[0], [300, 250]);
    }

    #[test]
    fn aps_slot_id_from_bidders_map_used_in_request_and_remapped_in_response() {
        use serde_json::json;

        let config = ApsConfig {
            enabled: true,
            pub_id: "5128".to_string(),
            endpoint: default_endpoint(),
            timeout_ms: 800,
        };
        let provider = ApsAuctionProvider::new(config);

        let mut bidders = HashMap::new();
        bidders.insert(
            "aps".to_string(),
            json!({ "slotID": "aps-slot-atf-sidebar" }),
        );
        let request = AuctionRequest {
            id: "test".to_string(),
            slots: vec![AdSlot {
                id: "atf_sidebar_ad".to_string(),
                formats: vec![AdFormat {
                    media_type: MediaType::Banner,
                    width: 300,
                    height: 250,
                }],
                floor_price: None,
                targeting: HashMap::new(),
                bidders,
            }],
            publisher: PublisherInfo {
                domain: "example.com".to_string(),
                page_url: None,
            },
            user: UserInfo {
                id: Some("user-1".to_string()),
                consent: None,
                eids: None,
            },
            device: None,
            site: None,
            context: HashMap::new(),
        };

        let (aps_request, slot_id_map) = provider.to_aps_request(&request, 800);
        assert_eq!(
            aps_request.slots[0].slot_id, "aps-slot-atf-sidebar",
            "should send configured APS slot ID to APS"
        );
        assert_eq!(
            slot_id_map.get("aps-slot-atf-sidebar").map(String::as_str),
            Some("atf_sidebar_ad"),
            "should build reverse map from APS slot ID to creative slot ID"
        );

        *provider.slot_id_map.lock().expect("should lock") = slot_id_map;

        let aps_response = json!({
            "contextual": {
                "slots": [{
                    "slotID": "aps-slot-atf-sidebar",
                    "size": "300x250",
                    "fif": "1",
                    "amznbid": "1gtm3q",
                    "meta": ["slotID"]
                }]
            }
        });

        let response = provider.parse_aps_response(&aps_response, 100);
        assert_eq!(response.bids.len(), 1, "should parse one bid");
        assert_eq!(
            response.bids[0].slot_id, "atf_sidebar_ad",
            "bid slot_id should be remapped to creative slot ID"
        );
    }

    #[test]
    fn test_aps_response_parsing_success() {
        let config = ApsConfig {
            enabled: true,
            pub_id: "5128".to_string(),
            endpoint: "https://aax.amazon-adsystem.com/e/dtb/bid".to_string(),
            timeout_ms: 800,
        };

        let provider = ApsAuctionProvider::new(config);

        // Test APS response in real contextual format
        let aps_response = json!({
            "contextual": {
                "slots": [
                    {
                        "slotID": "header-banner",
                        "size": "728x90",
                        "crid": "test-crid-123",
                        "mediaType": "d",
                        "fif": "1",
                        "targeting": ["amzniid", "amznbid", "amznp", "amznsz", "amznactt"],
                        "meta": ["slotID", "mediaType", "size"],
                        "amzniid": "test-impression-id",
                        "amznbid": "1kt0jk0",  // Proprietary Amazon encoding (not decodable)
                        "amznp": "1kt0jk0",
                        "amznsz": "728x90",
                        "amznactt": "OPEN"
                    },
                    {
                        "slotID": "sidebar",
                        "size": "300x250",
                        "crid": "test-crid-456",
                        "mediaType": "d",
                        "fif": "1",
                        "targeting": ["amzniid", "amznbid", "amznp", "amznsz", "amznactt"],
                        "meta": ["slotID", "mediaType", "size"],
                        "amzniid": "test-impression-id-2",
                        "amznbid": "ewpurk",  // Proprietary Amazon encoding (not decodable)
                        "amznp": "ewpurk",
                        "amznsz": "300x250",
                        "amznactt": "OPEN"
                    }
                ],
                "host": "https://aax-events.amazon-adsystem.com",
                "status": "ok",
                "cfe": true,
                "ev": true,
                "cb": "6"
            }
        });

        let auction_response = provider.parse_aps_response(&aps_response, 150);

        // Verify response
        assert_eq!(auction_response.provider, "aps");
        assert_eq!(auction_response.status, BidStatus::Success);
        assert_eq!(auction_response.bids.len(), 2);
        assert_eq!(auction_response.response_time_ms, 150);

        // Verify first bid - price is None (encoded, to be decoded by mediation)
        let bid1 = &auction_response.bids[0];
        assert_eq!(bid1.slot_id, "header-banner");
        assert_eq!(bid1.price, None); // Price is NOT decoded - it's in metadata for mediation
        assert_eq!(bid1.width, 728);
        assert_eq!(bid1.height, 90);
        assert_eq!(bid1.currency, "USD");
        assert_eq!(bid1.bidder, "amazon-aps");
        assert_eq!(bid1.adomain, None);
        assert!(bid1.metadata.contains_key("amzniid"));
        assert!(bid1.metadata.contains_key("amznbid")); // Encoded price stored here

        // Verify second bid
        let bid2 = &auction_response.bids[1];
        assert_eq!(bid2.slot_id, "sidebar");
        assert_eq!(bid2.price, None); // Price is NOT decoded
        assert_eq!(bid2.width, 300);
        assert_eq!(bid2.height, 250);
        assert!(bid2.metadata.contains_key("amznbid")); // Encoded price in metadata
    }

    #[test]
    fn test_aps_response_parsing_no_bid() {
        let config = ApsConfig {
            enabled: true,
            pub_id: "5128".to_string(),
            endpoint: "https://aax.amazon-adsystem.com/e/dtb/bid".to_string(),
            timeout_ms: 800,
        };

        let provider = ApsAuctionProvider::new(config);

        // Empty contextual response
        let aps_response = json!({
            "contextual": {
                "slots": [],
                "status": "ok"
            }
        });

        let auction_response = provider.parse_aps_response(&aps_response, 100);

        assert_eq!(auction_response.provider, "aps");
        assert_eq!(auction_response.status, BidStatus::NoBid);
        assert_eq!(auction_response.bids.len(), 0);
    }

    #[test]
    fn test_aps_response_parsing_invalid_bids() {
        let config = ApsConfig {
            enabled: true,
            pub_id: "5128".to_string(),
            endpoint: "https://aax.amazon-adsystem.com/e/dtb/bid".to_string(),
            timeout_ms: 800,
        };

        let provider = ApsAuctionProvider::new(config);

        // Response with invalid slots (not filled, zero price)
        let aps_response = json!({
            "contextual": {
                "slots": [
                    {
                        "slotID": "header-banner",
                        "size": "728x90",
                        "fif": "0",  // Not filled
                        "targeting": [],
                        "meta": []
                    },
                    {
                        "slotID": "sidebar",
                        "size": "300x250",
                        "fif": "1",
                        "targeting": [],
                        "meta": []
                        // Missing price encoding - will decode to 0.0
                    }
                ],
                "status": "ok"
            }
        });

        let auction_response = provider.parse_aps_response(&aps_response, 100);

        // Should return no-bid since all slots are invalid
        assert_eq!(auction_response.status, BidStatus::NoBid);
        assert_eq!(auction_response.bids.len(), 0);
    }

    #[test]
    fn test_aps_slot_parsing() {
        let config = ApsConfig {
            enabled: true,
            pub_id: "5128".to_string(),
            endpoint: "https://aax.amazon-adsystem.com/e/dtb/bid".to_string(),
            timeout_ms: 800,
        };

        let provider = ApsAuctionProvider::new(config);

        let aps_slot = ApsSlotResponse {
            slot_id: "test-slot".to_string(),
            size: "728x90".to_string(),
            crid: Some("crid-123".to_string()),
            media_type: Some("d".to_string()),
            fif: Some("1".to_string()),
            targeting: vec!["amzniid".to_string(), "amznbid".to_string()],
            meta: vec!["slotID".to_string()],
            amzniid: Some("impression-id-123".to_string()),
            amznbid: Some("1c7d4ow".to_string()), // Encoded price (to be decoded by mediation)
            amznp: Some("1c7d4ow".to_string()),
            amznsz: Some("728x90".to_string()),
            amznactt: Some("OPEN".to_string()),
        };

        let bid = provider
            .parse_aps_slot(&aps_slot)
            .expect("should parse slot");

        assert_eq!(bid.slot_id, "test-slot");
        assert_eq!(bid.price, None); // Price NOT decoded - stored in metadata for mediation
        assert_eq!(bid.width, 728);
        assert_eq!(bid.height, 90);
        assert_eq!(bid.currency, "USD");
        assert_eq!(bid.bidder, "amazon-aps");
        assert_eq!(bid.adomain, None);
        assert_eq!(bid.nurl, None); // Real APS uses client-side tracking
        assert_eq!(bid.burl, None);
        assert!(bid.metadata.contains_key("amzniid"));
        assert!(bid.metadata.contains_key("amznbid")); // Encoded price here
        assert!(bid.metadata.contains_key("amznsz"));
    }

    #[test]
    fn test_provider_metadata() {
        let config = ApsConfig::default();
        let provider = ApsAuctionProvider::new(config);

        assert_eq!(provider.provider_name(), "aps");
        assert!(!provider.is_enabled()); // Default is disabled
        assert_eq!(provider.timeout_ms(), 800);
        assert!(provider.supports_media_type(&MediaType::Banner));
        assert!(provider.supports_media_type(&MediaType::Video));
        assert!(!provider.supports_media_type(&MediaType::Native));
    }

    #[test]
    fn aps_payload_timeout_uses_effective_auction_budget_not_provider_config() {
        // Provider config says 1000ms but the auction budget grants only 500ms —
        // the payload must advertise the tighter effective deadline.
        let config = ApsConfig {
            enabled: true,
            pub_id: "5128".to_string(),
            endpoint: default_endpoint(),
            timeout_ms: 1000,
        };
        let provider = ApsAuctionProvider::new(config);
        let request = create_test_auction_request();

        let (aps_request, _slot_id_map) = provider.to_aps_request(&request, 500);

        assert_eq!(
            aps_request.timeout,
            Some(500),
            "should advertise the effective auction budget, not the provider config timeout"
        );
    }

    #[test]
    fn test_aps_request_includes_consent_fields() {
        use crate::consent::ConsentContext;

        let config = ApsConfig {
            enabled: true,
            pub_id: "5128".to_string(),
            endpoint: default_endpoint(),
            timeout_ms: 800,
        };
        let provider = ApsAuctionProvider::new(config);

        let mut request = create_test_auction_request();
        request.user.consent = Some(ConsentContext {
            raw_tc_string: Some("BOEFEAyOEFEAyAHABDENAI4AAAB9vABAASA".to_string()),
            gdpr_applies: true,
            raw_us_privacy: Some("1YNN".to_string()),
            raw_gpp_string: Some("DBACNYA~CPXxRfAPXxRfA".to_string()),
            gpp_section_ids: Some(vec![2, 6]),
            ..Default::default()
        });

        let (aps_request, _slot_id_map) = provider.to_aps_request(&request, 800);

        // Verify GDPR consent
        let gdpr = aps_request.gdpr.expect("should have gdpr");
        assert!(gdpr.enabled);
        assert_eq!(
            gdpr.consent.as_deref(),
            Some("BOEFEAyOEFEAyAHABDENAI4AAAB9vABAASA")
        );

        // Verify US Privacy
        assert_eq!(aps_request.us_privacy.as_deref(), Some("1YNN"));

        // Verify GPP
        assert_eq!(aps_request.gpp.as_deref(), Some("DBACNYA~CPXxRfAPXxRfA"));
        assert_eq!(aps_request.gpp_sid.as_deref(), Some("2,6"));
    }

    #[test]
    fn test_aps_request_no_consent() {
        let config = ApsConfig {
            enabled: true,
            pub_id: "5128".to_string(),
            endpoint: default_endpoint(),
            timeout_ms: 800,
        };
        let provider = ApsAuctionProvider::new(config);
        let request = create_test_auction_request(); // consent is None

        let (aps_request, _slot_id_map) = provider.to_aps_request(&request, 800);

        assert!(aps_request.gdpr.is_none());
        assert!(aps_request.us_privacy.is_none());
        assert!(aps_request.gpp.is_none());
        assert!(aps_request.gpp_sid.is_none());
    }

    #[test]
    fn test_aps_request_consent_serialization() {
        use crate::consent::ConsentContext;

        let config = ApsConfig {
            enabled: true,
            pub_id: "5128".to_string(),
            endpoint: default_endpoint(),
            timeout_ms: 800,
        };
        let provider = ApsAuctionProvider::new(config);

        let mut request = create_test_auction_request();
        request.user.consent = Some(ConsentContext {
            raw_tc_string: Some("BOE".to_string()),
            gdpr_applies: true,
            ..Default::default()
        });

        let (aps_request, _slot_id_map) = provider.to_aps_request(&request, 800);
        let json = serde_json::to_value(&aps_request).expect("should serialize");

        // GDPR fields present
        assert_eq!(json["gdpr"]["enabled"], true);
        assert_eq!(json["gdpr"]["consent"], "BOE");

        // Absent fields should not appear (skip_serializing_if)
        assert!(json.get("usPrivacy").is_none());
        assert!(json.get("gpp").is_none());
        assert!(json.get("gppSid").is_none());
    }

    #[test]
    fn test_aps_bids_have_no_creative_and_no_decoded_price() {
        // APS doesn't provide creative HTML - it only provides targeting keys
        // APS doesn't decode prices - only mediation layer can decode them
        // The creative will be generated by the mediation layer (e.g., GAM or ad server)
        let config = ApsConfig {
            enabled: true,
            pub_id: "5128".to_string(),
            endpoint: "https://aax.amazon-adsystem.com/e/dtb/bid".to_string(),
            timeout_ms: 800,
        };

        let provider = ApsAuctionProvider::new(config);

        let aps_slot = ApsSlotResponse {
            slot_id: "test-slot".to_string(),
            size: "300x250".to_string(),
            crid: Some("test-creative".to_string()),
            media_type: Some("d".to_string()),
            fif: Some("1".to_string()),
            targeting: vec!["amzniid".to_string(), "amznbid".to_string()],
            meta: vec!["slotID".to_string()],
            amzniid: Some("imp-123".to_string()),
            amznbid: Some("encoded-price".to_string()),
            amznp: Some("encoded-price-alt".to_string()),
            amznsz: Some("300x250".to_string()),
            amznactt: Some("OPEN".to_string()),
        };

        let bid = provider.parse_aps_slot(&aps_slot).expect("should parse");

        // Key assertions:
        // 1. creative should be None for APS bids
        assert_eq!(bid.creative, None, "APS bids should not have creative HTML");
        // 2. price should be None (encoded price in metadata, decoded by mediation)
        assert_eq!(bid.price, None, "APS bids should not have decoded price");

        // Verify targeting keys are in metadata (includes encoded price)
        assert!(bid.metadata.contains_key("amzniid"));
        assert!(bid.metadata.contains_key("amznbid")); // Encoded price
        assert_eq!(
            bid.metadata.get("amzniid").and_then(|v| v.as_str()),
            Some("imp-123")
        );
        assert_eq!(
            bid.metadata.get("amznbid").and_then(|v| v.as_str()),
            Some("encoded-price")
        );
    }
}
