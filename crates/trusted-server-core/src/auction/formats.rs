//! Auction request/response format conversions.
//!
//! This module handles:
//! - Parsing incoming tsjs/Prebid.js format requests
//! - Converting internal auction results to `OpenRTB` 2.x responses

use edgezero_core::body::Body as EdgeBody;
use error_stack::{ensure, Report, ResultExt};
use http::{header, HeaderValue, Response, StatusCode};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;

use crate::constants::{HEADER_X_TS_EC_CONSENT, HEADER_X_TS_EIDS, HEADER_X_TS_EIDS_TRUNCATED};
use crate::creative;
use crate::ec::eids::encode_eids_header;
use crate::error::TrustedServerError;
use crate::openrtb::{to_openrtb_i32, OpenRtbBid, OpenRtbResponse, ResponseExt, SeatBid, ToExt};
use crate::settings::Settings;

use super::orchestrator::OrchestrationResult;
use super::types::{AdFormat, AdSlot, AuctionRequest, MediaType, OrchestratorExt, ProviderSummary};
use super::validation::{
    validate_slots, AuctionInputLimits, FiniteNonNegativeF64, RawAdSlot, RawBidder, TargetingValue,
};

/// Apply private no-store caching headers to auction endpoint responses.
pub fn apply_auction_response_privacy(response: &mut Response<EdgeBody>) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
}

/// Request body for `POST /auction` (tsjs / Prebid.js wire format).
///
/// `adUnits` lists the placements to bid on. `config` carries optional
/// context values (e.g. audience segments) filtered through
/// [`auction.allowed_context_keys`][`crate::settings::AuctionConfig::allowed_context_keys`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdRequest {
    pub version: Option<u8>,
    pub page_url: Option<String>,
    pub ad_units: Vec<AdUnit>,
    pub config: Option<JsonValue>,
    pub eids: Option<JsonValue>,
}

/// A single ad placement in an [`AdRequest`].
///
/// `code` identifies the slot (e.g. `"atf_sidebar_ad"`) and becomes the
/// impression ID in the outgoing `OpenRTB` request.
///
/// `bids` is optional. When absent or empty the PBS provider falls back to
/// a stored-request keyed by `code` (`imp.ext.prebid.storedrequest.id`).
/// When present, each entry's params are forwarded inline to PBS as
/// `imp.ext.prebid.bidder.<bidder>`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdUnit {
    pub code: String,
    pub media_types: Option<MediaTypes>,
    pub bids: Option<Vec<BidConfig>>,
    pub floor_usd: Option<f64>,
    #[serde(default)]
    pub targeting: BTreeMap<String, JsonValue>,
}

/// Inline bidder params for one SSP within an [`AdUnit`].
///
/// `params` is passed verbatim to the corresponding PBS bidder adapter.
/// When the `bids` array is absent, the slot falls back to PBS stored
/// requests — see [`AdUnit`] for details.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BidConfig {
    pub bidder: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct MediaTypes {
    pub banner: Option<BannerUnit>,
    pub video: Option<VideoUnit>,
    pub native: Option<NativeUnit>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BannerUnit {
    pub sizes: Vec<Vec<u32>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoUnit {
    #[serde(default)]
    pub player_size: Vec<Vec<u32>>,
    #[serde(default)]
    pub sizes: Vec<Vec<u32>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeUnit {
    #[serde(default)]
    pub sizes: Vec<Vec<u32>>,
}

/// Validate the ad units in `body` and project them into canonical [`AdSlot`]s.
///
/// Shared by the `/auction`, page-bids, and initial-navigation canonical
/// request builders so the `/auction` slot contract has a single validation
/// path.
///
/// # Errors
///
/// Returns an error when any ad unit fails the shared slot validation (missing
/// formats, invalid sizes, disallowed targeting, etc.).
pub(crate) fn validated_ad_slots(
    body: &AdRequest,
) -> Result<Vec<AdSlot>, Report<TrustedServerError>> {
    let raw_slots = body
        .ad_units
        .iter()
        .map(raw_slot_from_ad_unit)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(validate_slots(raw_slots, &AuctionInputLimits::default())?
        .into_iter()
        .map(|slot| AdSlot {
            id: slot.id.as_str().to_string(),
            formats: slot.formats,
            floor_price: slot.floor_usd.map(FiniteNonNegativeF64::get),
            targeting: slot
                .targeting
                .into_iter()
                .map(|(key, value)| (key, targeting_value_to_json(value)))
                .collect(),
            bidders: slot.bidders.into_iter().collect(),
        })
        .collect())
}

fn raw_slot_from_ad_unit(unit: &AdUnit) -> Result<RawAdSlot, Report<TrustedServerError>> {
    let formats = formats_from_media_types(unit.media_types.as_ref())?;
    let bidders = unit
        .bids
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|bid| RawBidder {
            name: bid.bidder.clone(),
            params: bid.params.clone(),
        })
        .collect();

    Ok(RawAdSlot {
        id: unit.code.clone(),
        formats,
        floor_usd: unit.floor_usd,
        targeting: unit.targeting.clone(),
        bidders,
    })
}

fn formats_from_media_types(
    media_types: Option<&MediaTypes>,
) -> Result<Vec<AdFormat>, Report<TrustedServerError>> {
    let mut formats = Vec::new();
    let Some(media_types) = media_types else {
        return Ok(formats);
    };

    if let Some(banner) = &media_types.banner {
        append_sized_formats(&mut formats, &banner.sizes, &MediaType::Banner, "banner")?;
    }
    if let Some(video) = &media_types.video {
        let sizes = if video.player_size.is_empty() {
            &video.sizes
        } else {
            &video.player_size
        };
        append_sized_formats(&mut formats, sizes, &MediaType::Video, "video")?;
    }
    if let Some(native) = &media_types.native {
        append_sized_formats(&mut formats, &native.sizes, &MediaType::Native, "native")?;
    }

    Ok(formats)
}

fn append_sized_formats(
    formats: &mut Vec<AdFormat>,
    sizes: &[Vec<u32>],
    media_type: &MediaType,
    media_type_name: &str,
) -> Result<(), Report<TrustedServerError>> {
    for size in sizes {
        ensure!(
            size.len() == 2,
            TrustedServerError::BadRequest {
                message: format!("Invalid {media_type_name} size; expected [width, height]"),
            }
        );

        formats.push(AdFormat {
            width: size[0],
            height: size[1],
            media_type: media_type.clone(),
        });
    }

    Ok(())
}

fn targeting_value_to_json(value: TargetingValue) -> JsonValue {
    match value {
        TargetingValue::String(value) => JsonValue::String(value),
        TargetingValue::Number(value) => JsonValue::from(value),
        TargetingValue::Boolean(value) => JsonValue::Bool(value),
        TargetingValue::Array(values) => {
            JsonValue::Array(values.into_iter().map(targeting_value_to_json).collect())
        }
    }
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
    ec_allowed: bool,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    // Build OpenRTB-style seatbid array
    let mut seatbids = Vec::with_capacity(result.winning_bids.len());

    for (slot_id, bid) in &result.winning_bids {
        let price = bid.price.ok_or_else(|| {
            Report::new(TrustedServerError::Auction {
                message: format!(
                    "Winning bid for slot '{}' from '{}' has no decoded price",
                    slot_id, bid.bidder
                ),
            })
        })?;

        let bid_context = format!(
            "auction {} slot {} bidder {}",
            auction_request.id, slot_id, bid.bidder
        );
        let width = to_openrtb_i32(bid.width, "width", &bid_context);
        let height = to_openrtb_i32(bid.height, "height", &bid_context);

        // Process creative HTML if present - — sanitize dangerous markup first, then rewrite URLs.
        let creative_html = if let Some(ref raw_creative) = bid.creative {
            let sanitized = creative::sanitize_creative_html(raw_creative);
            let rewritten = creative::rewrite_creative_html(settings, &sanitized);

            log::debug!(
                "Processed creative for auction {} slot {} ({} → {} → {} bytes)",
                auction_request.id,
                slot_id,
                raw_creative.len(),
                sanitized.len(),
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
            id: Some(format!("{}-{}", bid.bidder, slot_id)),
            impid: Some(slot_id.to_string()),
            price: Some(price),
            adm: Some(creative_html),
            crid: Some(format!("{}-creative", bid.bidder)),
            w: width,
            h: height,
            adomain: bid.adomain.clone().unwrap_or_default(),
            ..Default::default()
        };

        seatbids.push(SeatBid {
            seat: Some(bid.bidder.clone()),
            bid: vec![openrtb_bid],
            ..Default::default()
        });
    }

    // Determine strategy name for response metadata
    let strategy_name = if settings.auction.has_mediator() {
        "parallel_mediation"
    } else {
        "parallel_only"
    };

    // Build per-provider summaries from the orchestration result
    let provider_details: Vec<ProviderSummary> = result
        .provider_responses
        .iter()
        .map(ProviderSummary::from)
        .collect();

    let response_body = OpenRtbResponse {
        id: Some(auction_request.id.to_string()),
        seatbid: seatbids,
        ext: ResponseExt {
            orchestrator: OrchestratorExt {
                strategy: strategy_name.to_string(),
                providers: result.provider_responses.len(),
                total_bids: result.total_bids(),
                time_ms: result.total_time_ms,
                provider_details,
            },
        }
        .to_ext(),
        ..Default::default()
    };

    let body_bytes =
        serde_json::to_vec(&response_body).change_context(TrustedServerError::Auction {
            message: "Failed to serialize auction response".to_string(),
        })?;

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(EdgeBody::from(body_bytes))
        .change_context(TrustedServerError::Auction {
            message: "Failed to build auction response".to_string(),
        })?;

    // Signal consent status independently of whether EIDs were resolved.
    if ec_allowed {
        response
            .headers_mut()
            .insert(HEADER_X_TS_EC_CONSENT, HeaderValue::from_static("ok"));
    }

    // Attach EID response headers when consent-gated EIDs are available.
    if let Some(ref eids) = auction_request.user.eids {
        let (encoded, truncated) = encode_eids_header(eids)?;
        let header_val =
            HeaderValue::from_str(&encoded).change_context(TrustedServerError::Auction {
                message: "Failed to encode EIDs header value".to_string(),
            })?;
        response.headers_mut().insert(HEADER_X_TS_EIDS, header_val);
        if truncated {
            response
                .headers_mut()
                .insert(HEADER_X_TS_EIDS_TRUNCATED, HeaderValue::from_static("true"));
        }
    }

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::types::{AuctionResponse, Bid, BidStatus, PublisherInfo, UserInfo};
    use crate::consent::ConsentContext;
    use crate::openrtb::{Eid, Uid};
    use crate::test_support::tests::create_test_settings;
    use serde_json::json;
    use std::collections::HashMap;

    fn make_settings() -> Settings {
        create_test_settings()
    }

    fn make_auction_request() -> AuctionRequest {
        AuctionRequest {
            id: "auction-1".to_string(),
            slots: vec![AdSlot {
                id: "div-gpt-top".to_string(),
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
                domain: "publisher.example.com".to_string(),
                page_url: Some("https://publisher.example.com".to_string()),
            },
            user: UserInfo {
                id: Some("ec-id".to_string()),
                consent: Some(ConsentContext::default()),
                eids: None,
            },
            device: None,
            site: None,
            context: HashMap::new(),
        }
    }

    fn make_empty_result() -> OrchestrationResult {
        OrchestrationResult {
            provider_responses: Vec::new(),
            mediator_response: None,
            winning_bids: HashMap::new(),
            total_time_ms: 10,
            metadata: HashMap::new(),
        }
    }

    fn make_bid(slot_id: &str, bidder: &str, price: Option<f64>) -> Bid {
        Bid {
            slot_id: slot_id.to_string(),
            price,
            currency: "USD".to_string(),
            creative: Some("<div>Ad</div>".to_string()),
            adomain: Some(vec!["advertiser.example.com".to_string()]),
            bidder: bidder.to_string(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            ad_id: None,
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: HashMap::new(),
        }
    }

    fn make_result(bid: Bid) -> OrchestrationResult {
        OrchestrationResult {
            provider_responses: vec![AuctionResponse {
                provider: "prebid".to_string(),
                bids: vec![bid.clone()],
                status: BidStatus::Success,
                response_time_ms: 42,
                metadata: HashMap::new(),
            }],
            mediator_response: None,
            winning_bids: HashMap::from([(bid.slot_id.clone(), bid)]),
            total_time_ms: 50,
            metadata: HashMap::new(),
        }
    }

    fn response_json(response: Response<EdgeBody>) -> JsonValue {
        serde_json::from_slice(&response.into_body().into_bytes().unwrap_or_default())
            .expect("should parse JSON response")
    }

    #[test]
    fn validated_ad_slots_maps_media_floor_targeting_and_bidders() {
        let body = AdRequest {
            version: Some(2),
            page_url: None,
            ad_units: vec![AdUnit {
                code: "div-gpt-top".to_string(),
                media_types: Some(MediaTypes {
                    banner: Some(BannerUnit {
                        sizes: vec![vec![300, 250], vec![728, 90]],
                    }),
                    video: None,
                    native: None,
                }),
                bids: Some(vec![BidConfig {
                    bidder: "appnexus".to_string(),
                    params: json!({ "placementId": 123 }),
                }]),
                floor_usd: Some(0.75),
                targeting: BTreeMap::from([("pos".to_string(), json!("atf"))]),
            }],
            config: None,
            eids: None,
        };

        let slots = validated_ad_slots(&body).expect("should validate ad slots");

        assert_eq!(slots.len(), 1, "should produce one slot");
        let slot = &slots[0];
        assert_eq!(slot.id, "div-gpt-top", "should preserve ad unit code");
        assert_eq!(
            slot.formats,
            vec![
                AdFormat {
                    width: 300,
                    height: 250,
                    media_type: MediaType::Banner,
                },
                AdFormat {
                    width: 728,
                    height: 90,
                    media_type: MediaType::Banner,
                },
            ],
            "should map banner sizes to formats"
        );
        assert_eq!(slot.floor_price, Some(0.75), "should preserve floor");
        assert_eq!(
            slot.targeting.get("pos"),
            Some(&json!("atf")),
            "should preserve slot targeting"
        );
        assert_eq!(
            slot.bidders.get("appnexus"),
            Some(&json!({ "placementId": 123 })),
            "should preserve bidder params"
        );
    }

    #[test]
    fn response_includes_eid_headers_when_eids_present() {
        let mut request = make_auction_request();
        request.user.eids = Some(vec![Eid {
            source: "ssp.com".to_owned(),
            uids: vec![Uid {
                id: "uid-1".to_owned(),
                atype: Some(3),
                ext: None,
            }],
        }]);

        let settings = make_settings();
        let result = make_empty_result();

        let response = convert_to_openrtb_response(&result, &settings, &request, true)
            .expect("should build response");

        assert!(
            response.headers().get(&HEADER_X_TS_EIDS).is_some(),
            "should include x-ts-eids header when EIDs are present"
        );
        assert_eq!(
            response
                .headers()
                .get(&HEADER_X_TS_EC_CONSENT)
                .and_then(|v| v.to_str().ok()),
            Some("ok"),
            "should include x-ts-ec-consent: ok when ec_allowed is true"
        );
        assert!(
            response
                .headers()
                .get(&HEADER_X_TS_EIDS_TRUNCATED)
                .is_none(),
            "should not include truncated header for small payload"
        );
    }

    #[test]
    fn response_sets_consent_header_even_without_eids() {
        let request = make_auction_request();
        let settings = make_settings();
        let result = make_empty_result();

        let response = convert_to_openrtb_response(&result, &settings, &request, true)
            .expect("should build response");

        assert_eq!(
            response
                .headers()
                .get(&HEADER_X_TS_EC_CONSENT)
                .and_then(|v| v.to_str().ok()),
            Some("ok"),
            "should set x-ts-ec-consent: ok based on consent, not EID presence"
        );
        assert!(
            response.headers().get(&HEADER_X_TS_EIDS).is_none(),
            "should omit x-ts-eids when no EIDs available"
        );
    }

    #[test]
    fn response_omits_consent_header_when_not_allowed() {
        let request = make_auction_request();
        let settings = make_settings();
        let result = make_empty_result();

        let response = convert_to_openrtb_response(&result, &settings, &request, false)
            .expect("should build response");

        assert!(
            response.headers().get(&HEADER_X_TS_EC_CONSENT).is_none(),
            "should omit x-ts-ec-consent when ec_allowed is false"
        );
        assert!(
            response.headers().get(&HEADER_X_TS_EIDS).is_none(),
            "should omit x-ts-eids when no EIDs available"
        );
    }

    #[test]
    fn response_omits_ec_header_when_ec_id_is_none() {
        let mut request = make_auction_request();
        request.user.id = None;

        let settings = make_settings();
        let result = make_empty_result();

        let response = convert_to_openrtb_response(&result, &settings, &request, false)
            .expect("should build response");

        assert!(
            response.headers().get("x-ts-ec").is_none(),
            "should omit x-ts-ec when no EC ID is available"
        );
    }

    #[test]
    fn convert_to_openrtb_response_serializes_winning_bid_and_orchestrator_ext() {
        let settings = make_settings();
        let auction_request = make_auction_request();
        let result = make_result(make_bid("div-gpt-top", "appnexus", Some(2.75)));

        let response = convert_to_openrtb_response(&result, &settings, &auction_request, true)
            .expect("should convert auction result to OpenRTB response");

        assert_eq!(response.status(), StatusCode::OK, "should return OK");
        assert_eq!(
            response
                .headers()
                .get(&header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json"),
            "should set JSON content type"
        );
        assert_eq!(
            response
                .headers()
                .get(&HEADER_X_TS_EC_CONSENT)
                .and_then(|v| v.to_str().ok()),
            Some("ok"),
            "should set EC consent header when allowed"
        );
        assert!(
            response.headers().get("x-ts-ec").is_none(),
            "should not emit removed EC ID header"
        );

        let json = response_json(response);
        assert_eq!(json["id"], json!("auction-1"), "should preserve auction ID");
        assert_eq!(
            json["seatbid"][0]["seat"],
            json!("appnexus"),
            "should use bidder as seat"
        );
        let bid = &json["seatbid"][0]["bid"][0];
        assert_eq!(bid["id"], json!("appnexus-div-gpt-top"));
        assert_eq!(bid["impid"], json!("div-gpt-top"));
        assert_eq!(bid["price"], json!(2.75));
        assert_eq!(bid["adm"], json!("<div>Ad</div>"));
        assert_eq!(bid["crid"], json!("appnexus-creative"));
        assert_eq!(bid["w"], json!(300));
        assert_eq!(bid["h"], json!(250));
        assert_eq!(bid["adomain"], json!(["advertiser.example.com"]));
        assert_eq!(
            json["ext"]["orchestrator"]["strategy"],
            json!("parallel_only"),
            "should use default parallel-only strategy"
        );
        assert_eq!(json["ext"]["orchestrator"]["providers"], json!(1));
        assert_eq!(json["ext"]["orchestrator"]["total_bids"], json!(1));
        assert_eq!(json["ext"]["orchestrator"]["time_ms"], json!(50));
        assert_eq!(
            json["ext"]["orchestrator"]["provider_details"][0]["name"],
            json!("prebid"),
            "should include provider summary details"
        );
    }

    #[test]
    fn convert_to_openrtb_response_serializes_missing_creative_as_empty_adm() {
        let settings = make_settings();
        let auction_request = make_auction_request();
        let mut bid = make_bid("div-gpt-top", "appnexus", Some(2.75));
        bid.creative = None;
        let result = make_result(bid);

        let response = convert_to_openrtb_response(&result, &settings, &auction_request, false)
            .expect("should convert bid without creative HTML");

        assert_eq!(response.status(), StatusCode::OK, "should return OK");
        let json = response_json(response);
        assert_eq!(
            json["seatbid"][0]["bid"][0]["adm"],
            json!(""),
            "should serialize missing creative as empty adm"
        );
    }

    #[test]
    fn convert_to_openrtb_response_omits_missing_adomain() {
        let settings = make_settings();
        let auction_request = make_auction_request();
        let mut bid = make_bid("div-gpt-top", "appnexus", Some(2.75));
        bid.adomain = None;
        let result = make_result(bid);

        let response = convert_to_openrtb_response(&result, &settings, &auction_request, false)
            .expect("should convert bid without advertiser domains");

        assert_eq!(response.status(), StatusCode::OK, "should return OK");
        let json = response_json(response);
        let bid = json["seatbid"][0]["bid"][0]
            .as_object()
            .expect("should serialize bid as object");
        assert!(
            !bid.contains_key("adomain"),
            "should preserve current wire format by omitting empty adomain"
        );
    }

    #[test]
    fn convert_to_openrtb_response_allows_empty_winning_bids() {
        let settings = make_settings();
        let auction_request = make_auction_request();
        let result = OrchestrationResult {
            provider_responses: vec![],
            mediator_response: None,
            winning_bids: HashMap::new(),
            total_time_ms: 50,
            metadata: HashMap::new(),
        };

        let response = convert_to_openrtb_response(&result, &settings, &auction_request, false)
            .expect("should convert auction result without winning bids");

        assert_eq!(response.status(), StatusCode::OK, "should return OK");
        let json = response_json(response);
        assert!(
            json.get("seatbid").is_none(),
            "should preserve current wire format by omitting empty seatbid"
        );
        assert_eq!(
            json["ext"]["orchestrator"]["total_bids"],
            json!(0),
            "should report zero total bids"
        );
    }

    #[test]
    fn convert_to_openrtb_response_serializes_multiple_winning_bids() {
        let settings = make_settings();
        let auction_request = make_auction_request();
        let top_bid = make_bid("div-gpt-top", "appnexus", Some(2.75));
        let mut sidebar_bid = make_bid("div-gpt-sidebar", "rubicon", Some(1.25));
        sidebar_bid.creative = Some("<div>Sidebar</div>".to_string());
        let result = OrchestrationResult {
            provider_responses: vec![AuctionResponse {
                provider: "prebid".to_string(),
                bids: vec![top_bid.clone(), sidebar_bid.clone()],
                status: BidStatus::Success,
                response_time_ms: 42,
                metadata: HashMap::new(),
            }],
            mediator_response: None,
            winning_bids: HashMap::from([
                (top_bid.slot_id.clone(), top_bid),
                (sidebar_bid.slot_id.clone(), sidebar_bid),
            ]),
            total_time_ms: 50,
            metadata: HashMap::new(),
        };

        let response = convert_to_openrtb_response(&result, &settings, &auction_request, false)
            .expect("should convert multiple winning bids");
        let json = response_json(response);
        let seatbids = json["seatbid"]
            .as_array()
            .expect("should serialize seatbid array");

        assert_eq!(seatbids.len(), 2, "should emit one seatbid per winner");

        let top_seatbid = seatbids
            .iter()
            .find(|seatbid| seatbid["bid"][0]["impid"].as_str() == Some("div-gpt-top"))
            .expect("should include top slot seatbid");
        assert_eq!(
            top_seatbid["seat"],
            json!("appnexus"),
            "should preserve top bidder as seat"
        );
        let top_bid = &top_seatbid["bid"][0];
        assert_eq!(
            top_bid["id"],
            json!("appnexus-div-gpt-top"),
            "should preserve top bid ID"
        );
        assert_eq!(
            top_bid["impid"],
            json!("div-gpt-top"),
            "should preserve top slot impid"
        );
        assert_eq!(top_bid["price"], json!(2.75), "should preserve top price");
        assert_eq!(
            top_bid["adm"],
            json!("<div>Ad</div>"),
            "should preserve top creative"
        );

        let sidebar_seatbid = seatbids
            .iter()
            .find(|seatbid| seatbid["bid"][0]["impid"].as_str() == Some("div-gpt-sidebar"))
            .expect("should include sidebar slot seatbid");
        assert_eq!(
            sidebar_seatbid["seat"],
            json!("rubicon"),
            "should preserve sidebar bidder as seat"
        );
        let sidebar_bid = &sidebar_seatbid["bid"][0];
        assert_eq!(
            sidebar_bid["id"],
            json!("rubicon-div-gpt-sidebar"),
            "should preserve sidebar bid ID"
        );
        assert_eq!(
            sidebar_bid["impid"],
            json!("div-gpt-sidebar"),
            "should preserve sidebar slot impid"
        );
        assert_eq!(
            sidebar_bid["price"],
            json!(1.25),
            "should preserve sidebar price"
        );
        assert_eq!(
            sidebar_bid["adm"],
            json!("<div>Sidebar</div>"),
            "should preserve sidebar creative"
        );
        assert_eq!(
            json["ext"]["orchestrator"]["total_bids"],
            json!(2),
            "should count both provider bids"
        );
    }

    #[test]
    fn convert_to_openrtb_response_uses_parallel_mediation_when_mediator_configured() {
        let mut settings = make_settings();
        settings.auction.mediator = Some("adserver_mock".to_string());
        let auction_request = make_auction_request();
        let result = make_result(make_bid("div-gpt-top", "appnexus", Some(2.75)));

        let response = convert_to_openrtb_response(&result, &settings, &auction_request, false)
            .expect("should convert auction result to OpenRTB response");
        let json = response_json(response);

        assert_eq!(
            json["ext"]["orchestrator"]["strategy"],
            json!("parallel_mediation"),
            "should use mediation strategy when mediator is configured"
        );
    }

    #[test]
    fn convert_to_openrtb_response_errors_when_winning_bid_has_no_price() {
        let settings = make_settings();
        let auction_request = make_auction_request();
        let result = make_result(make_bid("div-gpt-top", "appnexus", None));

        let err = convert_to_openrtb_response(&result, &settings, &auction_request, false)
            .expect_err("should reject winning bid without decoded price");

        assert!(
            format!("{err:?}").contains("has no decoded price"),
            "should explain missing decoded price"
        );
    }

    #[test]
    fn convert_to_openrtb_response_omits_out_of_range_dimensions() {
        let settings = make_settings();
        let auction_request = make_auction_request();
        let mut bid = make_bid("div-gpt-top", "appnexus", Some(2.75));
        bid.width = u32::MAX;
        bid.height = u32::MAX;
        let result = make_result(bid);

        let response = convert_to_openrtb_response(&result, &settings, &auction_request, false)
            .expect("should convert bid with out-of-range OpenRTB dimensions");
        let json = response_json(response);
        let bid = &json["seatbid"][0]["bid"][0];

        assert!(bid.get("w").is_none(), "should omit out-of-range width");
        assert!(bid.get("h").is_none(), "should omit out-of-range height");
    }
}
