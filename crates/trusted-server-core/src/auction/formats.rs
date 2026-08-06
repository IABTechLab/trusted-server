//! Auction request/response format conversions.
//!
//! This module handles:
//! - Parsing incoming tsjs/Prebid.js format requests
//! - Converting internal auction results to `OpenRTB` 2.x responses

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt, ensure};
use http::{HeaderValue, Request, Response, StatusCode, header};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::{BTreeMap, HashMap, HashSet};
use url::Url;
use uuid::Uuid;

use crate::auction::context::ContextValue;
use crate::consent::ConsentContext;
use crate::constants::{HEADER_X_TS_EC_CONSENT, HEADER_X_TS_EIDS, HEADER_X_TS_EIDS_TRUNCATED};
use crate::creative;
use crate::ec::eids::encode_eids_header;
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::openrtb::{
    BidExt, BidTrustedServerExt, OpenRtbBid, OpenRtbResponse, ResponseExt, SeatBid, ToExt,
    to_openrtb_i32,
};
use crate::platform::RuntimeServices;
use crate::settings::Settings;

use super::orchestrator::OrchestrationResult;
use super::types::{
    AdFormat, AdSlot, AuctionDecisionSetV1, AuctionDropReason, AuctionDropReasons, AuctionRequest,
    AuctionSlotFailureReason, BidRenderSourceV1, BrowserAuctionBidV1, BrowserAuctionProjectionV1,
    DeviceInfo, MAX_BROWSER_AUCTION_PROJECTION_BYTES, MAX_BROWSER_AUCTION_RESULTS,
    MAX_BROWSER_AUCTION_TARGETING_ENTRIES, MediaType, OrchestratorExt, ProviderSummary,
    PublisherInfo, RENDER_DIMENSION_MAX, RENDER_DIMENSION_MIN, SiteInfo, SlotAuctionDecisionV1,
    UserInfo, classify_aps_renderer_v1, record_auction_drop,
};

/// Request body for `POST /auction` (tsjs / Prebid.js wire format).
///
/// `adUnits` lists the placements to bid on. `config` carries optional
/// context values (e.g. audience segments) filtered through
/// [`auction.allowed_context_keys`][`crate::settings::AuctionConfig::allowed_context_keys`].
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdRequest {
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
#[serde(rename_all = "camelCase")]
pub struct MediaTypes {
    pub banner: Option<BannerUnit>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BannerUnit {
    pub sizes: Vec<Vec<u32>>,
}

const MAX_PUBLISHER_PAGE_URL_BYTES: usize = 8192;

/// Sanitize publisher page identity before forwarding it into the bidstream.
///
/// The candidate must be credential-free HTTP(S) on the configured publisher
/// host or a DNS-boundary subdomain. Query and fragment components are always
/// removed to prevent client-controlled private data from reaching providers.
/// Accepting publisher subdomains is deliberate; deployments with delegated or
/// user-content subdomains should account for that inventory trust boundary.
pub(crate) fn sanitize_publisher_page_url(
    candidate: Option<&str>,
    publisher_domain: &str,
) -> String {
    let fallback = format!("https://{publisher_domain}");
    let Some(candidate) = candidate.filter(|value| value.len() <= MAX_PUBLISHER_PAGE_URL_BYTES)
    else {
        log::debug!("Auction page URL: using publisher origin because input is absent or invalid");
        return fallback;
    };
    let Ok(mut parsed) = Url::parse(candidate) else {
        log::debug!("Auction page URL: using publisher origin because input is not a URL");
        return fallback;
    };
    let publisher_domain = publisher_domain.trim_end_matches('.').to_ascii_lowercase();
    let host_matches = !publisher_domain.is_empty()
        && parsed.host_str().is_some_and(|host| {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            host == publisher_domain
                || host
                    .strip_suffix(&publisher_domain)
                    .is_some_and(|prefix| prefix.ends_with('.'))
        });
    if !matches!(parsed.scheme(), "http" | "https")
        || !host_matches
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        log::debug!(
            "Auction page URL: using publisher origin because input is not publisher-owned HTTP(S)"
        );
        return fallback;
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    parsed.to_string()
}

fn publisher_page_url(req: &Request<EdgeBody>, publisher_domain: &str) -> String {
    let candidate = req.headers().get(header::REFERER).and_then(|value| {
        value.to_str().map_or_else(
            |_| {
                log::debug!("Auction page URL: ignoring non-ASCII Referer header");
                None
            },
            Some,
        )
    });
    sanitize_publisher_page_url(candidate, publisher_domain)
}

/// Convert tsjs/Prebid.js request format to internal [`AuctionRequest`].
///
/// The `consent` parameter carries decoded consent signals extracted from the
/// incoming request's cookies and headers. It is populated by the caller
/// (the `/auction` endpoint handler) and forwarded through to the
/// [`OpenRTB`][`crate::openrtb::OpenRtbRequest`] bid request.
///
/// The `ec_id` is generated by the caller before the consent pipeline
/// runs, so that KV Store operations can use it as a key.
///
/// # Errors
///
/// Returns an error if the request contains invalid banner sizes
/// (must be `[width, height]`).
pub fn convert_tsjs_to_auction_request(
    body: &AdRequest,
    settings: &Settings,
    services: &RuntimeServices,
    req: &Request<EdgeBody>,
    consent: ConsentContext,
    ec_id: Option<&str>,
    geo: Option<GeoInfo>,
) -> Result<AuctionRequest, Report<TrustedServerError>> {
    let ec_id = ec_id.map(str::to_owned);

    // Convert ad units to slots
    let mut slots = Vec::new();
    for unit in &body.ad_units {
        if let Some(media_types) = &unit.media_types
            && let Some(banner) = &media_types.banner
        {
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

    // Build device info with user-agent (always) and geo (if available)
    let device = Some(DeviceInfo {
        user_agent: req
            .headers()
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        ip: services.client_info().client_ip.map(|ip| ip.to_string()),
        geo,
    });

    // Forward allowed config entries from the JS request into the context map.
    // Only keys listed in `auction.allowed_context_keys` are accepted;
    // unrecognised keys are silently dropped to prevent injection of
    // arbitrary data by a malicious client payload.
    let mut context = HashMap::new();
    if let Some(ref config) = body.config
        && let Some(obj) = config.as_object()
    {
        for (key, value) in obj {
            if settings.auction.allowed_context_keys.contains(key) {
                match serde_json::from_value::<ContextValue>(value.clone()) {
                    Ok(cv) => {
                        context.insert(key.clone(), cv);
                    }
                    Err(_) => {
                        log::debug!(
                            "Auction context: dropping key '{}' with unsupported type",
                            key
                        );
                    }
                }
            } else {
                log::debug!("Auction context: dropping disallowed key '{}'", key);
            }
        }
        if !context.is_empty() {
            log::debug!(
                "Auction request context: {} entries ({})",
                context.len(),
                context.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
    }

    let page_url = publisher_page_url(req, &settings.publisher.domain);

    Ok(AuctionRequest {
        id: Uuid::new_v4().to_string(),
        slots,
        publisher: PublisherInfo {
            domain: settings.publisher.domain.clone(),
            page_url: Some(page_url.clone()),
        },
        user: UserInfo {
            id: ec_id,
            consent: Some(consent),
            eids: None,
        },
        device,
        site: Some(SiteInfo {
            domain: settings.publisher.domain.clone(),
            page: page_url,
        }),
        context,
    })
}

/// Delivery facts produced while serializing winning bids.
#[derive(Debug, Default)]
pub(crate) struct AuctionDeliveryReport {
    /// Winning slot IDs included in the serialized response.
    pub delivered_winner_slots: HashSet<String>,
    /// Winners omitted because they could not be delivered safely.
    pub dropped_winner_count: usize,
    /// Machine-readable reasons for omitted winners.
    pub dropped_winner_reasons: AuctionDropReasons,
}

impl AuctionDeliveryReport {
    fn record_drop(&mut self, reason: AuctionDropReason) {
        self.dropped_winner_count += 1;
        record_auction_drop(&mut self.dropped_winner_reasons, reason);
    }
}

/// Serialized response and the delivery facts used to produce it.
pub(crate) struct OpenRtbResponseConversion {
    /// HTTP response returned to the auction client.
    pub response: Response<EdgeBody>,
    /// Delivery facts for telemetry and diagnostics.
    pub delivery: AuctionDeliveryReport,
}

#[allow(
    dead_code,
    reason = "pure coordinated-cutover contract is exercised directly until Task 19 wires endpoints"
)]
pub(crate) mod coordinated_cutover_v1 {
    use super::*;

    /// Validated projection plus its exact canonical UTF-8 representation.
    #[derive(Debug, Clone)]
    pub(crate) struct CanonicalBrowserAuctionProjectionV1 {
        /// Deep-owned, validated projection in canonical result/bid/targeting order.
        pub projection: BrowserAuctionProjectionV1,
        /// Whitespace-free JSON using schema field order.
        pub json: Vec<u8>,
        /// Whether the exact aggregate overflow rule replaced every winner.
        pub reduced_for_size: bool,
    }

    fn projection_contract_error(message: impl Into<String>) -> Report<TrustedServerError> {
        Report::new(TrustedServerError::Auction {
            message: message.into(),
        })
    }

    fn is_base64url_byte(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
    }

    fn valid_auction_id(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')
            })
    }

    fn valid_candidate_id(value: &str) -> bool {
        value.len() == 12 && value.bytes().all(is_base64url_byte)
    }

    fn valid_renderer_reservation_id(value: &str) -> bool {
        value
            .strip_prefix("r1_")
            .is_some_and(|token| token.len() == 22 && token.bytes().all(is_base64url_byte))
    }

    fn valid_provider_name(value: &str) -> bool {
        let bytes = value.as_bytes();
        (1..=64).contains(&bytes.len())
            && bytes[0].is_ascii_alphanumeric()
            && bytes[1..]
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'_' | b'-'))
    }

    fn valid_bounded_text(value: &str, maximum_bytes: usize) -> bool {
        !value.is_empty()
            && value.len() <= maximum_bytes
            && !value
                .chars()
                .any(|character| matches!(character, '\0'..='\u{1f}' | '\u{7f}'))
    }

    fn valid_targeting_key(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 20
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    }

    fn valid_targeting(targeting: &BTreeMap<String, String>) -> bool {
        targeting.len() <= MAX_BROWSER_AUCTION_TARGETING_ENTRIES
            && targeting.iter().all(|(key, value)| {
                key != "hb_adid"
                    && valid_targeting_key(key)
                    && valid_bounded_text(value, 160)
                    && value.chars().count() <= 40
            })
    }

    fn valid_render_dimension(value: u32) -> bool {
        (RENDER_DIMENSION_MIN..=RENDER_DIMENSION_MAX).contains(&u64::from(value))
    }

    fn valid_cache_id(value: &str) -> bool {
        let Ok(uuid) = Uuid::parse_str(value) else {
            return false;
        };
        uuid.hyphenated().to_string().eq_ignore_ascii_case(value)
            && matches!(uuid.get_version_num(), 1..=5)
            && uuid.get_variant() == uuid::Variant::RFC4122
    }

    fn valid_cache_fetch_url(fetch_url: &str, cache_id: &str) -> bool {
        if fetch_url.len() > 4096 {
            return false;
        }
        let Ok(url) = Url::parse(fetch_url) else {
            return false;
        };
        let query = format!("uuid={cache_id}");
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.query() == Some(query.as_str())
            && url
                .query_pairs()
                .exactly_one()
                .is_ok_and(|(key, value)| key == "uuid" && value == cache_id)
    }

    trait ExactlyOne: Iterator + Sized {
        fn exactly_one(mut self) -> Result<Self::Item, ()> {
            let Some(value) = self.next() else {
                return Err(());
            };
            if self.next().is_some() {
                return Err(());
            }
            Ok(value)
        }
    }

    impl<I: Iterator> ExactlyOne for I {}

    fn render_source_dimensions(source: &BidRenderSourceV1) -> (u32, u32) {
        match source {
            BidRenderSourceV1::Aps(source) => (source.width, source.height),
            BidRenderSourceV1::Adm(source) => (source.width, source.height),
            BidRenderSourceV1::Cache(source) => (source.width, source.height),
        }
    }

    fn valid_render_source(source: &BidRenderSourceV1, publisher_origin: &str) -> bool {
        let (width, height) = render_source_dimensions(source);
        if !valid_render_dimension(width) || !valid_render_dimension(height) {
            return false;
        }

        match source {
            BidRenderSourceV1::Aps(source) => {
                source.version == 1
                    && serde_json::to_value(BidRenderSourceV1::Aps(source.clone())).is_ok_and(
                        |value| {
                            classify_aps_renderer_v1(&value, publisher_origin)
                                == crate::auction::types::ApsRendererValidationResult::Accepted
                        },
                    )
            }
            BidRenderSourceV1::Adm(source) => {
                source.version == 1 && !source.adm.is_empty() && source.adm.len() <= 512 * 1024
            }
            BidRenderSourceV1::Cache(source) => {
                source.version == 1
                    && valid_cache_id(&source.cache_id)
                    && valid_cache_fetch_url(&source.fetch_url, &source.cache_id)
            }
        }
    }

    fn valid_browser_bid(bid: &BrowserAuctionBidV1, publisher_origin: &str) -> bool {
        valid_candidate_id(&bid.candidate_id)
            && valid_bounded_text(&bid.slot, 256)
            && valid_provider_name(&bid.provider)
            && valid_bounded_text(&bid.upstream_bid_id, 64)
            && bid.cpm.is_finite()
            && bid.cpm >= 0.0
            && bid.currency == "USD"
            && valid_targeting(&bid.targeting)
            && valid_renderer_reservation_id(&bid.renderer_reservation_id)
            && valid_render_source(&bid.render_source, publisher_origin)
    }

    fn validate_decision_set(
        decision_set: &AuctionDecisionSetV1,
    ) -> Result<(), Report<TrustedServerError>> {
        ensure!(
            decision_set.version == 1,
            projection_contract_error("Browser auction decision version must be 1")
        );
        ensure!(
            valid_auction_id(&decision_set.auction_id),
            projection_contract_error("Browser auction id violates the version-1 grammar")
        );
        ensure!(
            decision_set.results.len() <= MAX_BROWSER_AUCTION_RESULTS,
            projection_contract_error("Browser auction result count exceeds 256")
        );

        let mut slots = HashSet::new();
        let mut candidates = HashSet::new();
        for result in &decision_set.results {
            ensure!(
                valid_bounded_text(result.slot(), 256) && slots.insert(result.slot()),
                projection_contract_error("Browser auction result slots must be valid and unique")
            );
            if let SlotAuctionDecisionV1::Winner { candidate_id, .. } = result {
                ensure!(
                    valid_candidate_id(candidate_id) && candidates.insert(candidate_id),
                    projection_contract_error(
                        "Browser auction winner candidates must be valid and unique"
                    )
                );
            }
        }
        Ok(())
    }

    /// Validate, reorder, and canonically serialize a complete browser auction projection.
    ///
    /// Winner-local projection failures become `winner_not_renderable`. Aggregate
    /// overflow applies the contract's all-winners reduction; it never selects a
    /// response-order-dependent subset.
    pub(crate) fn canonicalize_browser_auction_projection_v1(
        input: BrowserAuctionProjectionV1,
        publisher_origin: &str,
    ) -> Result<CanonicalBrowserAuctionProjectionV1, Report<TrustedServerError>> {
        ensure!(
            input.version == 1,
            projection_contract_error("Browser auction projection version must be 1")
        );
        validate_decision_set(&input.auction)?;
        ensure!(
            input.bids.len() <= MAX_BROWSER_AUCTION_RESULTS,
            projection_contract_error("Browser auction bid count exceeds 256")
        );

        let publisher_origin = Url::parse(publisher_origin)
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
            .map(|url| url.origin().ascii_serialization())
            .ok_or_else(|| projection_contract_error("Publisher origin is invalid"))?;

        let mut bids_by_candidate = HashMap::with_capacity(input.bids.len());
        for bid in input.bids {
            let candidate_id = bid.candidate_id.clone();
            ensure!(
                bids_by_candidate.insert(candidate_id, bid).is_none(),
                projection_contract_error("Browser auction candidate bids must be unique")
            );
        }

        let mut reservation_ids = HashSet::new();
        let mut canonical_bids = Vec::new();
        let mut canonical_results = Vec::with_capacity(input.auction.results.len());
        for result in input.auction.results {
            match result {
                SlotAuctionDecisionV1::Winner { slot, candidate_id } => {
                    let bid = bids_by_candidate.remove(&candidate_id);
                    if let Some(bid) = bid.filter(|bid| {
                        bid.slot == slot
                            && valid_browser_bid(bid, &publisher_origin)
                            && reservation_ids.insert(bid.renderer_reservation_id.clone())
                    }) {
                        canonical_results
                            .push(SlotAuctionDecisionV1::Winner { slot, candidate_id });
                        canonical_bids.push(bid);
                    } else {
                        canonical_results.push(SlotAuctionDecisionV1::Failed {
                            slot,
                            reason: AuctionSlotFailureReason::WinnerNotRenderable,
                        });
                    }
                }
                non_winner => canonical_results.push(non_winner),
            }
        }
        ensure!(
            bids_by_candidate.is_empty(),
            projection_contract_error("Browser auction contains a bid without a winner decision")
        );

        let mut projection = BrowserAuctionProjectionV1 {
            version: 1,
            auction: AuctionDecisionSetV1 {
                version: 1,
                auction_id: input.auction.auction_id,
                results: canonical_results,
            },
            bids: canonical_bids,
        };
        let mut json =
            serde_json::to_vec(&projection).change_context(TrustedServerError::Auction {
                message: "Failed to serialize browser auction projection".to_string(),
            })?;
        let reduced_for_size = json.len() > MAX_BROWSER_AUCTION_PROJECTION_BYTES;
        if reduced_for_size {
            projection.auction.results = projection
                .auction
                .results
                .into_iter()
                .map(|result| match result {
                    SlotAuctionDecisionV1::Winner { slot, .. } => SlotAuctionDecisionV1::Failed {
                        slot,
                        reason: AuctionSlotFailureReason::WinnerNotRenderable,
                    },
                    non_winner => non_winner,
                })
                .collect();
            projection.bids.clear();
            json = serde_json::to_vec(&projection).change_context(TrustedServerError::Auction {
                message: "Failed to serialize reduced browser auction projection".to_string(),
            })?;
            ensure!(
                json.len() <= MAX_BROWSER_AUCTION_PROJECTION_BYTES,
                projection_contract_error("Reduced browser auction projection exceeds 8 MiB")
            );
        }

        Ok(CanonicalBrowserAuctionProjectionV1 {
            projection,
            json,
            reduced_for_size,
        })
    }

    #[derive(Serialize)]
    struct TrustedServerOpenRtbBidExtV1<'a> {
        candidate_id: &'a str,
        slot_id: &'a str,
        render_source: &'a BidRenderSourceV1,
    }

    #[derive(Serialize)]
    struct OpenRtbBidExtV1<'a> {
        trusted_server: TrustedServerOpenRtbBidExtV1<'a>,
    }

    #[derive(Serialize)]
    struct TrustedServerOpenRtbBidV1<'a> {
        id: &'a str,
        impid: &'a str,
        price: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        adm: Option<&'a str>,
        w: u32,
        h: u32,
        ext: OpenRtbBidExtV1<'a>,
    }

    #[derive(Serialize)]
    struct TrustedServerSeatBidV1<'a> {
        seat: &'a str,
        bid: Vec<TrustedServerOpenRtbBidV1<'a>>,
    }

    #[derive(Serialize)]
    struct TrustedServerResponseExtInnerV1<'a> {
        slot_results: &'a AuctionDecisionSetV1,
    }

    #[derive(Serialize)]
    struct TrustedServerResponseExtV1<'a> {
        trusted_server: TrustedServerResponseExtInnerV1<'a>,
    }

    #[derive(Serialize)]
    struct TrustedServerAuctionResponseWireV1<'a> {
        id: &'a str,
        seatbid: Vec<TrustedServerSeatBidV1<'a>>,
        cur: &'static str,
        ext: TrustedServerResponseExtV1<'a>,
    }

    /// Serialize the coordinated-cutover exact `/auction` winner wire.
    ///
    /// This remains a pure contract function until Task 19 switches the endpoint.
    pub(crate) fn serialize_trusted_server_auction_response_v1(
        canonical: &CanonicalBrowserAuctionProjectionV1,
    ) -> Result<Vec<u8>, Report<TrustedServerError>> {
        let seatbid = canonical
            .projection
            .bids
            .iter()
            .map(|bid| {
                let (width, height) = render_source_dimensions(&bid.render_source);
                TrustedServerSeatBidV1 {
                    seat: &bid.provider,
                    bid: vec![TrustedServerOpenRtbBidV1 {
                        id: &bid.renderer_reservation_id,
                        impid: &bid.slot,
                        price: bid.cpm,
                        // `render_source` is the sole browser authority. Standard
                        // `adm` is optional on the exact wire and omitted by the
                        // producer to avoid duplicating up to 512 KiB per winner.
                        adm: None,
                        w: width,
                        h: height,
                        ext: OpenRtbBidExtV1 {
                            trusted_server: TrustedServerOpenRtbBidExtV1 {
                                candidate_id: &bid.candidate_id,
                                slot_id: &bid.slot,
                                render_source: &bid.render_source,
                            },
                        },
                    }],
                }
            })
            .collect();
        let response = TrustedServerAuctionResponseWireV1 {
            id: &canonical.projection.auction.auction_id,
            seatbid,
            cur: "USD",
            ext: TrustedServerResponseExtV1 {
                trusted_server: TrustedServerResponseExtInnerV1 {
                    slot_results: &canonical.projection.auction,
                },
            },
        };
        serde_json::to_vec(&response).change_context(TrustedServerError::Auction {
            message: "Failed to serialize exact trusted-server auction response".to_string(),
        })
    }
}

#[cfg(test)]
use coordinated_cutover_v1::{
    canonicalize_browser_auction_projection_v1, serialize_trusted_server_auction_response_v1,
};

/// Convert `OrchestrationResult` to `OpenRTB` response format.
///
/// Creative HTML in the `adm` field is optionally sanitized and optionally
/// rewritten according to the auction configuration
/// ([`AuctionConfig::sanitize_creatives`], opt-in, and
/// [`AuctionConfig::rewrite_creatives`], default-on); with both disabled the
/// creative ships exactly as the bidder returned it, subject to the 1 MiB
/// per-creative cap.
///
/// [`AuctionConfig::sanitize_creatives`]: crate::auction_config_types::AuctionConfig::sanitize_creatives
/// [`AuctionConfig::rewrite_creatives`]: crate::auction_config_types::AuctionConfig::rewrite_creatives
///
/// # Errors
///
/// Returns an error if:
/// - A winning bid is missing a price or render source
/// - The response serialization fails
pub fn convert_to_openrtb_response(
    result: &OrchestrationResult,
    settings: &Settings,
    auction_request: &AuctionRequest,
    ec_allowed: bool,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    Ok(
        convert_to_openrtb_response_with_report(result, settings, auction_request, ec_allowed)?
            .response,
    )
}

pub(crate) fn convert_to_openrtb_response_with_report(
    result: &OrchestrationResult,
    settings: &Settings,
    auction_request: &AuctionRequest,
    ec_allowed: bool,
) -> Result<OpenRtbResponseConversion, Report<TrustedServerError>> {
    let mut seatbids = Vec::with_capacity(result.winning_bids.len());
    let mut delivery = AuctionDeliveryReport::default();

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

        let creative = bid
            .creative
            .as_deref()
            .filter(|creative| !creative.trim().is_empty());
        if creative.is_some() && bid.renderer.is_some() {
            log::warn!(
                "Auction {}: skipping winning bid for slot '{}' from '{}' because it has multiple render sources",
                auction_request.id,
                slot_id,
                bid.bidder
            );
            delivery.record_drop(AuctionDropReason::MultipleRenderSources);
            continue;
        }

        // Ordinary markup remains on the mandatory sanitize/rewrite path. A
        // typed render source is serialized separately and never enters the
        // HTML sanitizer.
        let (adm, ext) = if let Some(raw_creative) = creative {
            let processed = creative::process_auction_creative(settings, raw_creative);

            log::debug!(
                "Processed creative for auction {} slot {} bidder {} (sanitize {}, rewrite {}, raw {} bytes, output {} bytes)",
                auction_request.id,
                slot_id,
                bid.bidder,
                settings.auction.sanitize_creatives,
                settings.auction.rewrite_creatives,
                raw_creative.len(),
                processed.len()
            );

            (Some(processed), None)
        } else if let Some(renderer) = bid.renderer.as_ref() {
            let Some(ext) = (BidExt {
                trusted_server: BidTrustedServerExt { renderer },
            })
            .to_ext() else {
                log::warn!(
                    "Auction {}: skipping winning bid for slot '{}' from '{}' because its renderer extension could not be serialized",
                    auction_request.id,
                    slot_id,
                    bid.bidder
                );
                delivery.record_drop(AuctionDropReason::RendererExtensionSerializationFailed);
                continue;
            };
            (None, Some(ext))
        } else {
            log::warn!(
                "Auction {}: skipping winning bid for slot '{}' from '{}' because it has no render source",
                auction_request.id,
                slot_id,
                bid.bidder
            );
            delivery.record_drop(AuctionDropReason::NoRenderSource);
            continue;
        };

        let openrtb_bid = OpenRtbBid {
            id: bid
                .bid_id
                .clone()
                .or_else(|| Some(format!("{}-{}", bid.bidder, slot_id))),
            impid: Some(slot_id.to_string()),
            price: Some(price),
            adm,
            adid: bid.ad_id.clone(),
            crid: bid.creative_id.clone(),
            w: width,
            h: height,
            adomain: bid.adomain.clone().unwrap_or_default(),
            ext,
            ..Default::default()
        };

        seatbids.push(SeatBid {
            seat: Some(bid.bidder.clone()),
            bid: vec![openrtb_bid],
            ..Default::default()
        });
        delivery.delivered_winner_slots.insert(slot_id.clone());
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
                dropped_winner_count: delivery.dropped_winner_count,
                dropped_winner_reasons: delivery.dropped_winner_reasons.clone(),
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

    Ok(OpenRtbResponseConversion { response, delivery })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::types::{
        ApsRendererV1, ApsTagType, AuctionDecisionSetV1, AuctionResponse, Bid, BidRenderSourceV1,
        BidStatus,
    };
    use crate::openrtb::{Eid, Uid};
    use crate::platform::test_support::noop_services;
    use crate::test_support::tests::create_test_settings;
    use http::Method;
    use serde_json::json;
    use std::collections::HashSet;

    fn make_request() -> Request<EdgeBody> {
        Request::builder()
            .method(Method::POST)
            .uri("https://publisher.example.com/auction")
            .header(header::USER_AGENT, "Mozilla/5.0 test")
            .body(EdgeBody::empty())
            .expect("should build request")
    }

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
            decision_set: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "auction-1".to_string(),
                results: Vec::new(),
            },
            total_time_ms: 10,
            metadata: HashMap::new(),
        }
    }

    fn make_bid(slot_id: &str, bidder: &str, price: Option<f64>) -> Bid {
        Bid {
            slot_id: slot_id.to_string(),
            candidate_id: None,
            candidate_provider: None,
            renderer_reservation_id: None,
            price,
            currency: "USD".to_string(),
            creative: Some("<div>Ad</div>".to_string()),
            adomain: Some(vec!["advertiser.example.com".to_string()]),
            bidder: bidder.to_string(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            bid_id: Some(format!("{bidder}-{slot_id}")),
            ad_id: None,
            creative_id: Some(format!("{bidder}-creative")),
            renderer: None,
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: HashMap::new(),
        }
    }

    fn make_complete_creative_bid() -> Bid {
        let mut bid = make_bid("div-gpt-top", "appnexus", Some(2.75));
        bid.creative = Some(
            r#"<html><body><a href="https://advertiser.example.com/landing"><img src="https://cdn.example.com/ad.png" style="background-image:url(https://styles.example.com/bg.png)" onerror="auction-handler-marker()"></a><script>auction-script-marker</script></body></html>"#
                .to_string(),
        );
        bid
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
            decision_set: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "auction-1".to_string(),
                results: Vec::new(),
            },
            total_time_ms: 50,
            metadata: HashMap::new(),
        }
    }

    fn response_json(response: Response<EdgeBody>) -> JsonValue {
        serde_json::from_slice(&response.into_body().into_bytes().unwrap_or_default())
            .expect("should parse JSON response")
    }

    fn response_adm(response: Response<EdgeBody>) -> String {
        response_json(response)["seatbid"][0]["bid"][0]["adm"]
            .as_str()
            .expect("should serialize adm as a string")
            .to_string()
    }

    fn make_banner_body(config: Option<JsonValue>) -> AdRequest {
        AdRequest {
            ad_units: vec![AdUnit {
                code: "div-gpt-top".to_string(),
                media_types: Some(MediaTypes {
                    banner: Some(BannerUnit {
                        sizes: vec![vec![300, 250], vec![728, 90]],
                    }),
                }),
                bids: Some(vec![
                    BidConfig {
                        bidder: "appnexus".to_string(),
                        params: json!({ "placementId": 123 }),
                    },
                    BidConfig {
                        bidder: "rubicon".to_string(),
                        params: json!({ "accountId": 456 }),
                    },
                ]),
            }],
            config,
            eids: None,
        }
    }

    fn convert_body_to_auction_request(body: &AdRequest, settings: &Settings) -> AuctionRequest {
        let req = make_request();
        let services = noop_services();

        convert_tsjs_to_auction_request(
            body,
            settings,
            &services,
            &req,
            ConsentContext::default(),
            Some("existing-ec-id"),
            None,
        )
        .expect("should convert banner request")
    }

    #[test]
    fn response_serializes_prebid_immediate_no_bid_without_error_metadata() {
        let request = make_auction_request();
        let settings = make_settings();
        let mut result = make_empty_result();
        result.provider_responses = vec![AuctionResponse::no_bid("prebid", 0)];

        let response = convert_to_openrtb_response(&result, &settings, &request, false)
            .expect("should serialize immediate no-bid response");
        let json = response_json(response);
        let details = &json["ext"]["orchestrator"]["provider_details"][0];

        assert_eq!(details["name"], "prebid");
        assert_eq!(details["status"], "nobid");
        assert_eq!(details["bid_count"], 0);
        assert_eq!(details["bidders"], json!([]));
        assert!(
            details.get("metadata").is_none(),
            "should omit empty launch/error metadata"
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
    fn publisher_page_url_accepts_publisher_hosts_and_strips_private_components() {
        for (candidate, expected) in [
            (
                "https://publisher.example.com/article?id=1#comments",
                "https://publisher.example.com/article",
            ),
            (
                "http://news.publisher.example.com/nested/story?token=secret",
                "http://news.publisher.example.com/nested/story",
            ),
            (
                "https://www.publisher.example.com/",
                "https://www.publisher.example.com/",
            ),
        ] {
            let request = Request::builder()
                .uri("https://publisher.example.com/auction")
                .header(header::REFERER, candidate)
                .body(EdgeBody::empty())
                .expect("should build request");
            assert_eq!(
                publisher_page_url(&request, "Publisher.Example.Com"),
                expected,
                "should sanitize publisher-owned Referer"
            );
        }
    }

    #[test]
    fn publisher_page_url_rejects_untrusted_or_oversized_referers() {
        let oversized = format!(
            "https://publisher.example.com/{}",
            "x".repeat(MAX_PUBLISHER_PAGE_URL_BYTES)
        );
        for candidate in [
            "https://other.example/article",
            "https://publisher.example.com.evil.example/article",
            "https://evilpublisher.example.com/article",
            "javascript:alert(1)",
            "https://user:password@publisher.example.com/article",
            "not a url",
            &oversized,
        ] {
            let request = Request::builder()
                .uri("https://publisher.example.com/auction")
                .header(header::REFERER, candidate)
                .body(EdgeBody::empty())
                .expect("should build request");
            assert_eq!(
                publisher_page_url(&request, "publisher.example.com"),
                "https://publisher.example.com",
                "should reject {candidate}"
            );
        }
    }

    #[test]
    fn convert_tsjs_to_auction_request_maps_banner_sizes_to_formats() {
        let settings = make_settings();
        let body = make_banner_body(None);
        let auction_request = convert_body_to_auction_request(&body, &settings);

        assert_eq!(auction_request.slots.len(), 1, "should create one slot");
        let slot = &auction_request.slots[0];
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
            "should convert banner sizes to formats"
        );
    }

    #[test]
    fn convert_tsjs_to_auction_request_preserves_bidder_params() {
        let settings = make_settings();
        let body = make_banner_body(None);
        let auction_request = convert_body_to_auction_request(&body, &settings);
        let slot = &auction_request.slots[0];

        assert_eq!(
            slot.bidders.get("appnexus"),
            Some(&json!({ "placementId": 123 })),
            "should preserve bidder params"
        );
        assert_eq!(
            slot.bidders.get("rubicon"),
            Some(&json!({ "accountId": 456 })),
            "should preserve all bidder params"
        );
    }

    #[test]
    fn convert_tsjs_to_auction_request_populates_publisher_user_device_and_site_metadata() {
        let settings = make_settings();
        let body = make_banner_body(None);
        let auction_request = convert_body_to_auction_request(&body, &settings);

        assert_eq!(
            auction_request.publisher.domain, settings.publisher.domain,
            "should copy publisher domain"
        );
        assert_eq!(
            auction_request
                .site
                .as_ref()
                .map(|site| site.domain.as_str()),
            Some(settings.publisher.domain.as_str()),
            "should create site metadata from settings"
        );
        assert_eq!(
            auction_request.user.id.as_deref(),
            Some("existing-ec-id"),
            "should use caller-provided EC ID"
        );
        assert!(
            auction_request.user.consent.is_some(),
            "should preserve consent context"
        );
        assert!(
            auction_request.user.eids.is_none(),
            "should not attach EIDs during request conversion"
        );
        assert_eq!(
            auction_request
                .device
                .as_ref()
                .and_then(|device| device.user_agent.as_deref()),
            Some("Mozilla/5.0 test"),
            "should copy user-agent into device info"
        );
    }

    #[test]
    fn convert_tsjs_to_auction_request_propagates_missing_ec_id() {
        let settings = make_settings();
        let req = make_request();
        let services = noop_services();
        let auction_request = convert_tsjs_to_auction_request(
            &make_banner_body(None),
            &settings,
            &services,
            &req,
            ConsentContext::default(),
            None,
            None,
        )
        .expect("should convert request without EC ID");

        assert!(
            auction_request.user.id.is_none(),
            "should leave user ID unset when caller provides no EC ID"
        );
    }

    #[test]
    fn convert_tsjs_to_auction_request_filters_context_values() {
        let mut settings = make_settings();
        settings.auction.allowed_context_keys = HashSet::from([
            "segments".to_string(),
            "lockr_id".to_string(),
            "count".to_string(),
            "unsupported".to_string(),
        ]);
        let body = make_banner_body(Some(json!({
            "segments": ["seg-a", "seg-b"],
            "lockr_id": "lockr-123",
            "count": 2,
            "unsupported": { "nested": true },
            "blocked": "drop-me"
        })));
        let auction_request = convert_body_to_auction_request(&body, &settings);

        assert_eq!(
            auction_request.context.get("segments"),
            Some(&ContextValue::StringList(vec![
                "seg-a".to_string(),
                "seg-b".to_string()
            ])),
            "should keep allowed string-list context values"
        );
        assert_eq!(
            auction_request.context.get("lockr_id"),
            Some(&ContextValue::Text("lockr-123".to_string())),
            "should keep allowed text context values"
        );
        assert_eq!(
            auction_request.context.get("count"),
            Some(&ContextValue::Number(2.0)),
            "should keep allowed number context values"
        );
        assert!(
            !auction_request.context.contains_key("unsupported"),
            "should drop allowed keys with unsupported value types"
        );
        assert!(
            !auction_request.context.contains_key("blocked"),
            "should drop disallowed context keys"
        );
    }

    #[test]
    fn convert_tsjs_to_auction_request_allows_empty_banner_sizes() {
        let settings = make_settings();
        let req = make_request();
        let services = noop_services();
        let body = AdRequest {
            ad_units: vec![AdUnit {
                code: "div-gpt-top".to_string(),
                media_types: Some(MediaTypes {
                    banner: Some(BannerUnit { sizes: vec![] }),
                }),
                bids: None,
            }],
            config: None,
            eids: None,
        };

        let auction_request = convert_tsjs_to_auction_request(
            &body,
            &settings,
            &services,
            &req,
            ConsentContext::default(),
            Some("existing-ec-id"),
            None,
        )
        .expect("should convert request with empty banner sizes");

        assert_eq!(auction_request.slots.len(), 1, "should create one slot");
        assert!(
            auction_request.slots[0].formats.is_empty(),
            "should preserve current behavior by allowing empty formats"
        );
    }

    #[test]
    fn convert_tsjs_to_auction_request_rejects_banner_sizes_that_are_not_width_height_pairs() {
        let settings = make_settings();
        let req = make_request();
        let services = noop_services();
        let body = AdRequest {
            ad_units: vec![AdUnit {
                code: "div-gpt-top".to_string(),
                media_types: Some(MediaTypes {
                    banner: Some(BannerUnit {
                        sizes: vec![vec![300, 250, 1]],
                    }),
                }),
                bids: None,
            }],
            config: None,
            eids: None,
        };

        let err = convert_tsjs_to_auction_request(
            &body,
            &settings,
            &services,
            &req,
            ConsentContext::default(),
            Some("existing-ec-id"),
            None,
        )
        .expect_err("should reject malformed banner size");

        assert!(
            format!("{err:?}").contains("Invalid banner size; expected [width, height]"),
            "should explain invalid banner size"
        );
    }

    #[test]
    fn convert_tsjs_to_auction_request_skips_units_without_banner_media() {
        let settings = make_settings();
        let req = make_request();
        let services = noop_services();
        let body = AdRequest {
            ad_units: vec![
                AdUnit {
                    code: "no-media".to_string(),
                    media_types: None,
                    bids: None,
                },
                AdUnit {
                    code: "no-banner".to_string(),
                    media_types: Some(MediaTypes { banner: None }),
                    bids: None,
                },
            ],
            config: None,
            eids: None,
        };

        let auction_request = convert_tsjs_to_auction_request(
            &body,
            &settings,
            &services,
            &req,
            ConsentContext::default(),
            Some("existing-ec-id"),
            None,
        )
        .expect("should skip unsupported media units");

        assert!(
            auction_request.slots.is_empty(),
            "should only create slots for banner media"
        );
    }

    #[test]
    fn convert_to_openrtb_response_serializes_winning_bid_and_orchestrator_ext() {
        let mut settings = make_settings();
        settings.auction.rewrite_creatives = false;
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
    fn convert_to_openrtb_response_rewrites_sanitized_creative_when_enabled() {
        let mut settings = make_settings();
        settings.auction.sanitize_creatives = true;
        settings.auction.rewrite_creatives = true;
        let auction_request = make_auction_request();
        let result = make_result(make_complete_creative_bid());

        let response = convert_to_openrtb_response(&result, &settings, &auction_request, false)
            .expect("should convert creative with rewriting enabled");
        let adm = response_adm(response);

        assert!(
            adm.matches("/first-party/proxy?tsurl=").count() >= 2,
            "should rewrite image and inline CSS URLs through the proxy: {adm}"
        );
        assert!(
            adm.contains("/first-party/click?tsurl="),
            "should rewrite click URLs: {adm}"
        );
        assert!(
            adm.contains("data-tsclick"),
            "should add the click guard attribute: {adm}"
        );
        assert!(
            adm.contains("tsjs-unified.min.js"),
            "should inject the unified creative runtime: {adm}"
        );
        assert!(
            !adm.contains(r#"src="https://cdn.example.com/ad.png""#),
            "should not retain the image URL as a direct attribute: {adm}"
        );
        assert!(
            !adm.contains(r#"href="https://advertiser.example.com/landing""#),
            "should not retain the click URL as a direct attribute: {adm}"
        );
        assert!(
            !adm.contains("url(https://styles.example.com/bg.png)"),
            "should not retain the CSS URL as a direct value: {adm}"
        );
        assert!(
            !adm.contains("auction-script-marker"),
            "should remove malicious script content before rewriting: {adm}"
        );
        assert!(
            !adm.contains("auction-handler-marker") && !adm.contains("onerror"),
            "should remove event handlers before rewriting: {adm}"
        );
    }

    #[test]
    fn convert_to_openrtb_response_can_skip_sanitization_when_disabled() {
        // Sanitization strips every executable element with its inner content, which
        // destroys script-based creatives (the majority of programmatic display).
        // Publishers whose creatives render in a foreign-origin frame — where the
        // markup cannot reach the publisher origin — can opt out and deliver the
        // creative exactly as the bidder returned it.
        let mut settings = make_settings();
        settings.auction.rewrite_creatives = false;
        let auction_request = make_auction_request();
        let result = make_result(make_complete_creative_bid());

        let original = make_complete_creative_bid()
            .creative
            .expect("should have a creative fixture");

        let response = convert_to_openrtb_response(&result, &settings, &auction_request, false)
            .expect("should convert creative with rewriting disabled");
        let adm = response_adm(response);

        assert_eq!(
            adm, original,
            "should deliver the creative byte-for-byte as the bidder returned it"
        );
    }

    #[test]
    fn convert_to_openrtb_response_rewrites_raw_markup_without_sanitizing() {
        // The fourth mode: rewriting enabled while sanitization stays off. The
        // rewriter converts eligible resource/click URLs on the raw bidder
        // markup and preserves executable content.
        let mut settings = make_settings();
        settings.auction.sanitize_creatives = false;
        settings.auction.rewrite_creatives = true;
        let auction_request = make_auction_request();
        let result = make_result(make_complete_creative_bid());

        let response = convert_to_openrtb_response(&result, &settings, &auction_request, false)
            .expect("should convert creative with rewriting only");
        let adm = response_adm(response);

        assert!(
            adm.contains("/first-party/proxy?tsurl="),
            "should rewrite accepted resource URLs: {adm}"
        );
        assert!(
            adm.contains("/first-party/click?tsurl="),
            "should rewrite accepted click URLs: {adm}"
        );
        assert!(
            adm.contains("auction-script-marker"),
            "should preserve script content when sanitization is disabled: {adm}"
        );
        assert!(
            adm.contains("auction-handler-marker"),
            "should preserve event handlers when sanitization is disabled: {adm}"
        );
    }

    #[test]
    fn rewrite_creatives_defaults_to_enabled() {
        let config = crate::auction_config_types::AuctionConfig::default();
        assert!(
            !config.sanitize_creatives,
            "sanitization is opt-in: it blanks script-based creatives"
        );
        assert!(
            config.rewrite_creatives,
            "creative URL rewriting stays enabled by default"
        );
    }

    #[test]
    fn convert_to_openrtb_response_can_skip_rewriting_while_sanitizing() {
        let mut settings = make_settings();
        settings.auction.rewrite_creatives = false;
        settings.auction.sanitize_creatives = true;
        let auction_request = make_auction_request();
        let result = make_result(make_complete_creative_bid());

        let response = convert_to_openrtb_response(&result, &settings, &auction_request, false)
            .expect("should convert creative with rewriting disabled");
        let adm = response_adm(response);

        assert!(
            adm.contains(r#"src="https://cdn.example.com/ad.png""#),
            "should retain the sanitizer-accepted image URL: {adm}"
        );
        assert!(
            adm.contains(r#"href="https://advertiser.example.com/landing""#),
            "should retain the sanitizer-accepted click URL: {adm}"
        );
        assert!(
            adm.contains("url(https://styles.example.com/bg.png)"),
            "should retain the sanitizer-accepted CSS URL: {adm}"
        );
        assert!(
            !adm.contains("/first-party/proxy"),
            "should not rewrite resource URLs: {adm}"
        );
        assert!(
            !adm.contains("/first-party/click"),
            "should not rewrite click URLs: {adm}"
        );
        assert!(
            !adm.contains("data-tsclick"),
            "should not add the click guard attribute: {adm}"
        );
        assert!(
            !adm.contains("tsjs-unified.min.js"),
            "should not inject the unified creative runtime: {adm}"
        );
        assert!(
            !adm.contains("auction-script-marker"),
            "should still remove malicious script content: {adm}"
        );
        assert!(
            !adm.contains("auction-handler-marker") && !adm.contains("onerror"),
            "should still remove event handlers: {adm}"
        );
    }

    #[test]
    fn convert_to_openrtb_response_preserves_aps_debug_metadata() {
        let settings = make_settings();
        let auction_request = make_auction_request();
        let bid = make_bid("div-gpt-top", "aps", Some(2.75));
        let debug = json!({
            "httpcalls": {
                "aps": [{
                    "requestbody": "{\"id\":\"fictional-auction\"}",
                    "requestheaders": {"content-type": ["application/json"]},
                    "responsebody": "{\"seatbid\":[]}",
                    "responseheaders": {"content-type": ["application/json"]},
                    "status": 200,
                    "uri": "https://aps.example/openrtb"
                }]
            }
        });
        let result = OrchestrationResult {
            provider_responses: vec![AuctionResponse {
                provider: "aps".to_string(),
                bids: vec![bid.clone()],
                status: BidStatus::Success,
                response_time_ms: 42,
                metadata: HashMap::from([("debug".to_string(), debug.clone())]),
            }],
            mediator_response: None,
            winning_bids: HashMap::from([(bid.slot_id.clone(), bid)]),
            decision_set: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "auction-1".to_string(),
                results: Vec::new(),
            },
            total_time_ms: 50,
            metadata: HashMap::new(),
        };

        let response = convert_to_openrtb_response(&result, &settings, &auction_request, false)
            .expect("should convert APS response with debug metadata");
        let json = response_json(response);

        assert_eq!(
            json["ext"]["orchestrator"]["provider_details"][0]["metadata"]["debug"], debug,
            "should preserve APS debug metadata in the auction response"
        );
    }

    #[test]
    fn convert_to_openrtb_response_skips_invalid_winners_without_dropping_valid_slots() {
        let mut settings = make_settings();
        settings.auction.rewrite_creatives = false;
        let auction_request = make_auction_request();
        let mut missing = make_bid("missing", "invalid", Some(3.0));
        missing.creative = None;
        let mut whitespace = make_bid("whitespace", "invalid", Some(2.9));
        whitespace.creative = Some(" \n\t ".to_string());
        let ordinary = make_bid("ordinary", "appnexus", Some(2.75));
        let mut renderer = make_bid("renderer", "aps", Some(2.5));
        renderer.creative = Some("  ".to_string());
        renderer.bid_id = Some("upstream-renderer-bid".to_string());
        renderer.creative_id = None;
        renderer.renderer = Some(BidRenderSourceV1::Aps(ApsRendererV1 {
            version: 1,
            account_id: "example-account".to_string(),
            bid_id: "upstream-renderer-bid".to_string(),
            creative_id: None,
            tag_type: ApsTagType::Iframe,
            creative_url: "https://creative.example/render".to_string(),
            aax_response: "fictional-base64".to_string(),
            width: 300,
            height: 250,
        }));
        let result = OrchestrationResult {
            provider_responses: vec![],
            mediator_response: None,
            winning_bids: HashMap::from([
                (missing.slot_id.clone(), missing),
                (whitespace.slot_id.clone(), whitespace),
                (ordinary.slot_id.clone(), ordinary),
                (renderer.slot_id.clone(), renderer),
            ]),
            decision_set: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "auction-1".to_string(),
                results: Vec::new(),
            },
            total_time_ms: 50,
            metadata: HashMap::new(),
        };

        let conversion =
            convert_to_openrtb_response_with_report(&result, &settings, &auction_request, false)
                .expect("should omit invalid winners and preserve valid slots");
        assert_eq!(
            conversion.delivery.delivered_winner_slots,
            HashSet::from(["ordinary".to_string(), "renderer".to_string()]),
            "should report only serialized winners as delivered"
        );
        assert_eq!(conversion.delivery.dropped_winner_count, 2);
        assert_eq!(
            conversion.delivery.dropped_winner_reasons[&AuctionDropReason::NoRenderSource],
            2
        );
        let json = response_json(conversion.response);
        let bids: Vec<&JsonValue> = json["seatbid"]
            .as_array()
            .expect("should include valid seatbids")
            .iter()
            .map(|seatbid| &seatbid["bid"][0])
            .collect();

        assert_eq!(bids.len(), 2, "should omit only invalid winners");
        assert_eq!(json["ext"]["orchestrator"]["dropped_winner_count"], 2);
        assert_eq!(
            json["ext"]["orchestrator"]["dropped_winner_reasons"]["no_render_source"],
            2
        );
        let ordinary = bids
            .iter()
            .find(|bid| bid["impid"] == "ordinary")
            .expect("should preserve ordinary winner");
        assert_eq!(ordinary["adm"], "<div>Ad</div>");
        let renderer = bids
            .iter()
            .find(|bid| bid["impid"] == "renderer")
            .expect("should preserve renderer winner");
        assert!(renderer.get("adm").is_none(), "should omit renderer adm");
        assert!(
            renderer.get("crid").is_none(),
            "should omit absent upstream crid"
        );
        assert_eq!(renderer["id"], "upstream-renderer-bid");
        assert!(renderer.get("ext").is_some(), "should include renderer ext");
    }

    #[test]
    fn convert_to_openrtb_response_rejects_multiple_render_sources() {
        let mut settings = make_settings();
        settings.auction.rewrite_creatives = false;
        let auction_request = make_auction_request();
        let mut bid = make_bid("div-gpt-top", "aps", Some(2.75));
        bid.renderer = Some(BidRenderSourceV1::Aps(ApsRendererV1 {
            version: 1,
            account_id: "example-account".to_string(),
            bid_id: "fictional-bid".to_string(),
            creative_id: None,
            tag_type: ApsTagType::Iframe,
            creative_url: "https://creative.example/render".to_string(),
            aax_response: "fictional-base64".to_string(),
            width: 300,
            height: 250,
        }));
        let result = make_result(bid);

        let conversion =
            convert_to_openrtb_response_with_report(&result, &settings, &auction_request, false)
                .expect("should reject an ambiguous render source");
        let json = response_json(conversion.response);
        assert!(
            json["seatbid"].as_array().is_none_or(Vec::is_empty),
            "should not serialize an ambiguous winner"
        );
        assert_eq!(conversion.delivery.dropped_winner_count, 1);
        assert!(
            conversion
                .delivery
                .dropped_winner_reasons
                .contains_key(&AuctionDropReason::MultipleRenderSources),
            "should report the exact ambiguous-source reason"
        );
    }

    #[test]
    fn convert_to_openrtb_response_emits_typed_aps_renderer_without_adm() {
        let settings = make_settings();
        let auction_request = make_auction_request();
        let mut bid = make_bid("div-gpt-top", "aps", Some(2.75));
        bid.creative = None;
        bid.bid_id = Some("fictional-bid".to_string());
        bid.ad_id = Some("fictional-ad".to_string());
        bid.creative_id = Some("fictional-creative".to_string());
        bid.renderer = Some(BidRenderSourceV1::Aps(ApsRendererV1 {
            version: 1,
            account_id: "example-account".to_string(),
            bid_id: "fictional-bid".to_string(),
            creative_id: Some("fictional-creative".to_string()),
            tag_type: ApsTagType::Iframe,
            creative_url: "https://creative.example/render".to_string(),
            aax_response: "fictional-base64".to_string(),
            width: 300,
            height: 250,
        }));
        let result = make_result(bid);

        let response = convert_to_openrtb_response(&result, &settings, &auction_request, false)
            .expect("should convert APS renderer bid");
        let json = response_json(response);
        let bid = json["seatbid"][0]["bid"][0]
            .as_object()
            .expect("should serialize bid object");

        assert!(
            !bid.contains_key("adm"),
            "should omit adm for renderer bids"
        );
        assert_eq!(bid["id"], json!("fictional-bid"));
        assert_eq!(bid["adid"], json!("fictional-ad"));
        assert_eq!(bid["crid"], json!("fictional-creative"));
        assert_eq!(
            bid["ext"]["trusted_server"]["renderer"]["type"],
            json!("aps")
        );
        assert_eq!(
            bid["ext"]["trusted_server"]["renderer"]["bidId"],
            json!("fictional-bid")
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
            decision_set: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "auction-1".to_string(),
                results: Vec::new(),
            },
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
        let mut settings = make_settings();
        settings.auction.rewrite_creatives = false;
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
            decision_set: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "auction-1".to_string(),
                results: Vec::new(),
            },
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

#[cfg(test)]
mod convert_tests {
    use super::*;
    use crate::auction::types::{
        AdmRenderSourceV1, AuctionDecisionSetV1, BidRenderSourceV1, BrowserAuctionBidV1,
        BrowserAuctionProjectionV1, MAX_BROWSER_AUCTION_PROJECTION_BYTES, SlotAuctionDecisionV1,
    };
    use crate::consent::ConsentContext;
    use crate::platform::test_support::noop_services;
    use crate::test_support::tests::crate_test_settings_str;
    use http::Method;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn make_settings() -> Settings {
        Settings::from_toml(&crate_test_settings_str()).expect("should parse test settings")
    }

    fn make_req() -> Request<EdgeBody> {
        Request::builder()
            .method(Method::POST)
            .uri("https://test-publisher.com/auction")
            .body(EdgeBody::empty())
            .expect("should build test request")
    }

    fn call_convert(body: &AdRequest) -> AuctionRequest {
        let settings = make_settings();
        let services = noop_services();
        let req = make_req();
        convert_tsjs_to_auction_request(
            body,
            &settings,
            &services,
            &req,
            ConsentContext::default(),
            Some("test-ec-id"),
            None,
        )
        .expect("should convert without error")
    }

    #[test]
    fn no_bids_produces_empty_bidders_map() {
        // An ad unit with no `bids` array must produce an empty bidders map.
        // An empty bidders map triggers the PBS stored-request fallback:
        // the PBS provider sets imp.ext.prebid.storedrequest = { id: "<code>" }.
        let body = AdRequest {
            ad_units: vec![AdUnit {
                code: "atf_sidebar_ad".to_string(),
                media_types: Some(MediaTypes {
                    banner: Some(BannerUnit {
                        sizes: vec![vec![300, 250]],
                    }),
                }),
                bids: None,
            }],
            config: None,
            eids: None,
        };

        let auction_request = call_convert(&body);

        assert_eq!(auction_request.slots.len(), 1, "should have one slot");
        let slot = &auction_request.slots[0];
        assert_eq!(slot.id, "atf_sidebar_ad", "slot id should match unit code");
        assert!(
            slot.bidders.is_empty(),
            "absent bids array should yield empty bidders map (PBS stored-request path)"
        );
    }

    #[test]
    fn inline_bids_populate_bidders_map() {
        // When bids are supplied, each bidder+params pair should appear in the
        // slot's bidders map so PBS receives inline params.
        let body = AdRequest {
            ad_units: vec![AdUnit {
                code: "homepage_header_ad".to_string(),
                media_types: Some(MediaTypes {
                    banner: Some(BannerUnit {
                        sizes: vec![vec![970, 90]],
                    }),
                }),
                bids: Some(vec![BidConfig {
                    bidder: "kargo".to_string(),
                    params: serde_json::json!({ "placementId": "client_123" }),
                }]),
            }],
            config: None,
            eids: None,
        };

        let auction_request = call_convert(&body);

        let slot = &auction_request.slots[0];
        assert!(
            slot.bidders.contains_key("kargo"),
            "kargo bidder should be present in slot bidders map"
        );
        assert_eq!(
            slot.bidders["kargo"]["placementId"], "client_123",
            "bidder params should be forwarded verbatim"
        );
    }

    #[test]
    fn config_allowed_key_passes_through() {
        // Keys in auction.allowed_context_keys must reach the auction context.
        // The test settings do not set allowed_context_keys so the default
        // (empty) applies — verify a key is NOT present rather than IS.
        // To test the allow-list, inject a key via a custom settings string.
        let settings_str = format!(
            "{}\n[auction]\nallowed_context_keys = [\"permutive_segments\"]\n",
            crate_test_settings_str()
        );
        let settings = Settings::from_toml(&settings_str).expect("should parse");
        let services = noop_services();
        let req = make_req();

        let body = AdRequest {
            ad_units: vec![],
            config: Some(serde_json::json!({
                "permutive_segments": ["seg1", "seg2"],
                "disallowed_key": "should be dropped",
            })),
            eids: None,
        };

        let auction_request = convert_tsjs_to_auction_request(
            &body,
            &settings,
            &services,
            &req,
            ConsentContext::default(),
            Some("test-ec-id"),
            None,
        )
        .expect("should convert");

        assert!(
            auction_request.context.contains_key("permutive_segments"),
            "allowed key should be in auction context"
        );
        assert!(
            !auction_request.context.contains_key("disallowed_key"),
            "unlisted key should be dropped"
        );
    }

    #[test]
    fn invalid_banner_size_returns_error() {
        // Banner sizes must be [width, height] pairs; a 3-element size is invalid.
        let body = AdRequest {
            ad_units: vec![AdUnit {
                code: "bad_slot".to_string(),
                media_types: Some(MediaTypes {
                    banner: Some(BannerUnit {
                        sizes: vec![vec![300, 250, 99]], // invalid — 3 elements
                    }),
                }),
                bids: None,
            }],
            config: None,
            eids: None,
        };

        let settings = make_settings();
        let services = noop_services();
        let req = make_req();
        let result = convert_tsjs_to_auction_request(
            &body,
            &settings,
            &services,
            &req,
            ConsentContext::default(),
            Some("test-ec-id"),
            None,
        );

        assert!(
            result.is_err(),
            "3-element banner size should return an error"
        );
    }

    fn projection_candidate_id(index: usize) -> String {
        format!("{index:012x}")
    }

    fn projection_reservation_id(index: usize) -> String {
        format!("r1_{index:022x}")
    }

    fn projection_adm_bid(index: usize, slot: &str, adm: String) -> BrowserAuctionBidV1 {
        BrowserAuctionBidV1 {
            candidate_id: projection_candidate_id(index),
            slot: slot.to_string(),
            provider: "prebid".to_string(),
            upstream_bid_id: format!("upstream-{index}"),
            cpm: index as f64,
            currency: "USD".to_string(),
            targeting: BTreeMap::from([
                ("z_key".to_string(), "last".to_string()),
                ("a_key".to_string(), "first".to_string()),
            ]),
            renderer_reservation_id: projection_reservation_id(index),
            render_source: BidRenderSourceV1::Adm(AdmRenderSourceV1 {
                version: 1,
                adm,
                width: 300,
                height: 250,
            }),
        }
    }

    fn projection_with_adm_lengths(lengths: &[usize]) -> BrowserAuctionProjectionV1 {
        let results = lengths
            .iter()
            .enumerate()
            .map(|(index, _)| SlotAuctionDecisionV1::Winner {
                slot: format!("slot-{index}"),
                candidate_id: projection_candidate_id(index),
            })
            .collect();
        let bids = lengths
            .iter()
            .enumerate()
            .map(|(index, length)| {
                projection_adm_bid(index, &format!("slot-{index}"), "x".repeat(*length))
            })
            .rev()
            .collect();
        BrowserAuctionProjectionV1 {
            version: 1,
            auction: AuctionDecisionSetV1 {
                version: 1,
                auction_id: "auction-1".to_string(),
                results,
            },
            bids,
        }
    }

    #[test]
    fn canonical_projection_orders_bids_and_targeting_by_contract() {
        let input = projection_with_adm_lengths(&[1, 1]);
        let mut permuted = input.clone();
        permuted.bids.reverse();

        let canonical =
            canonicalize_browser_auction_projection_v1(input, "https://publisher.example")
                .expect("valid projection should canonicalize");
        let canonical_permuted =
            canonicalize_browser_auction_projection_v1(permuted, "https://publisher.example")
                .expect("response-order permutation should canonicalize");

        assert!(!canonical.reduced_for_size);
        assert_eq!(canonical.json, canonical_permuted.json);
        assert_eq!(canonical.projection.bids[0].slot, "slot-0");
        assert_eq!(canonical.projection.bids[1].slot, "slot-1");
        let json = String::from_utf8(canonical.json).expect("canonical JSON should be UTF-8");
        assert!(
            json.find("\"a_key\"") < json.find("\"z_key\""),
            "targeting keys should be lexically sorted"
        );
        assert!(
            json.starts_with("{\"version\":1,\"auction\":{\"version\":1,\"auctionId\":"),
            "top-level and decision-set fields should retain schema order: {json}"
        );
    }

    #[test]
    fn invalid_selected_winner_becomes_winner_not_renderable() {
        let mut input = projection_with_adm_lengths(&[1]);
        input.bids[0].renderer_reservation_id = "not-a-reservation".to_string();

        let canonical =
            canonicalize_browser_auction_projection_v1(input, "https://publisher.example")
                .expect("selected projection failure should remain an explicit slot result");

        assert!(canonical.projection.bids.is_empty());
        assert_eq!(
            canonical.projection.auction.results,
            vec![SlotAuctionDecisionV1::Failed {
                slot: "slot-0".to_string(),
                reason: crate::auction::types::AuctionSlotFailureReason::WinnerNotRenderable,
            }]
        );
    }

    #[test]
    fn canonical_projection_enforces_exact_eight_mib_all_winner_reduction() {
        let mut lengths = vec![512 * 1024; 15];
        lengths.push(1);
        let baseline = projection_with_adm_lengths(&lengths);
        let baseline_len = serde_json::to_vec(&baseline)
            .expect("typed baseline should serialize")
            .len();
        let exact_tail = 1 + MAX_BROWSER_AUCTION_PROJECTION_BYTES - baseline_len;
        assert!(
            exact_tail <= 512 * 1024,
            "tail ADM should remain individually valid"
        );

        for (delta, should_reduce) in [(-1_isize, false), (0, false), (1, true)] {
            lengths[15] = exact_tail
                .checked_add_signed(delta)
                .expect("positive exact tail");
            let input = projection_with_adm_lengths(&lengths);
            let canonical =
                canonicalize_browser_auction_projection_v1(input, "https://publisher.example")
                    .expect("boundary projection should canonicalize or reduce");
            assert_eq!(canonical.reduced_for_size, should_reduce, "delta {delta}");
            assert!(canonical.json.len() <= MAX_BROWSER_AUCTION_PROJECTION_BYTES);
            if should_reduce {
                assert!(canonical.projection.bids.is_empty());
                assert!(canonical.projection.auction.results.iter().all(|result| matches!(
                    result,
                    SlotAuctionDecisionV1::Failed {
                        reason: crate::auction::types::AuctionSlotFailureReason::WinnerNotRenderable,
                        ..
                    }
                )));
                let wire: JsonValue = serde_json::from_slice(
                    &serialize_trusted_server_auction_response_v1(&canonical)
                        .expect("reduced exact response should serialize"),
                )
                .expect("reduced exact response should be JSON");
                assert_eq!(wire["seatbid"], json!([]));
            } else {
                assert_eq!(
                    canonical.json.len(),
                    MAX_BROWSER_AUCTION_PROJECTION_BYTES
                        .checked_add_signed(delta)
                        .expect("boundary size should remain positive")
                );
                if delta == 0 {
                    let wire = serialize_trusted_server_auction_response_v1(&canonical)
                        .expect("exact-boundary response should serialize");
                    assert!(
                        wire.len() <= MAX_BROWSER_AUCTION_PROJECTION_BYTES,
                        "exact response should not exceed the admitted projection cap"
                    );
                }
            }
        }
    }

    #[test]
    fn exact_openrtb_serializer_uses_reservation_and_trusted_server_join_only() {
        let canonical = canonicalize_browser_auction_projection_v1(
            projection_with_adm_lengths(&[7]),
            "https://publisher.example",
        )
        .expect("projection should canonicalize");

        let json: JsonValue = serde_json::from_slice(
            &serialize_trusted_server_auction_response_v1(&canonical)
                .expect("exact response should serialize"),
        )
        .expect("exact response should be JSON");

        let bid = &json["seatbid"][0]["bid"][0];
        assert_eq!(bid["id"], projection_reservation_id(0));
        assert_eq!(bid["impid"], "slot-0");
        assert!(
            bid.get("adm").is_none(),
            "tagged render_source should be the sole browser authority"
        );
        assert_eq!(json["cur"], "USD");
        assert_eq!(
            bid["ext"]["trusted_server"],
            json!({
                "candidate_id": projection_candidate_id(0),
                "slot_id": "slot-0",
                "render_source": {
                    "type": "adm",
                    "version": 1,
                    "adm": "xxxxxxx",
                    "width": 300,
                    "height": 250,
                }
            })
        );
        assert_eq!(
            json["ext"]["trusted_server"]["slot_results"],
            serde_json::to_value(&canonical.projection.auction)
                .expect("decision set should serialize")
        );
    }

    #[test]
    fn exact_openrtb_serializer_carries_identity_generation_failure_without_a_bid() {
        let canonical = canonicalize_browser_auction_projection_v1(
            BrowserAuctionProjectionV1 {
                version: 1,
                auction: AuctionDecisionSetV1 {
                    version: 1,
                    auction_id: "auction-identity-failure".to_string(),
                    results: vec![SlotAuctionDecisionV1::Failed {
                        slot: "slot-0".to_string(),
                        reason: crate::auction::types::AuctionSlotFailureReason::IdentityGenerationFailed,
                    }],
                },
                bids: Vec::new(),
            },
            "https://publisher.example",
        )
        .expect("identity failure decision should canonicalize");

        let json: JsonValue = serde_json::from_slice(
            &serialize_trusted_server_auction_response_v1(&canonical)
                .expect("identity failure response should serialize"),
        )
        .expect("identity failure response should be JSON");

        assert_eq!(json["seatbid"], json!([]));
        assert_eq!(
            json["ext"]["trusted_server"]["slot_results"]["results"][0],
            json!({
                "slot": "slot-0",
                "outcome": "failed",
                "reason": "identity_generation_failed",
            })
        );
    }
}
