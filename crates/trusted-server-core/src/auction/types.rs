//! Core types for auction requests and responses.

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt as _, bail, ensure};
use http::Request;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

use crate::auction::context::ContextValue;
use crate::error::TrustedServerError;
use crate::geo::GeoInfo;
use crate::platform::RuntimeServices;
use crate::settings::Settings;

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Represents a unified auction request across all providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuctionRequest {
    /// Unique auction ID
    pub id: String,
    /// Ad slots/impressions being auctioned
    pub slots: Vec<AdSlot>,
    /// Publisher information
    pub publisher: PublisherInfo,
    /// User information (privacy-preserving)
    pub user: UserInfo,
    /// Device information
    pub device: Option<DeviceInfo>,
    /// Site information
    pub site: Option<SiteInfo>,
    /// Additional context forwarded from the JS client payload.
    pub context: HashMap<String, ContextValue>,
}

/// Represents a single ad slot/impression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdSlot {
    /// Slot identifier (e.g., "header-banner")
    pub id: String,
    /// Media types and formats supported
    pub formats: Vec<AdFormat>,
    /// Floor price if any
    pub floor_price: Option<f64>,
    /// Slot-specific targeting
    pub targeting: HashMap<String, serde_json::Value>,
    /// Bidder configurations (bidder name -> params)
    pub bidders: HashMap<String, serde_json::Value>,
}

/// Ad format specification.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdFormat {
    pub media_type: MediaType,
    pub width: u32,
    pub height: u32,
}

/// Media type enumeration.
///
/// `Default` is `Banner` for programmatic construction only. Do **not** add
/// `#[serde(default)]` to any field of this type: it would coerce an
/// unknown/missing media type to `Banner` rather than failing, silently
/// mis-typing video/native slots. Deserialization must stay strict.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    #[default]
    Banner,
    Video,
    Native,
}

/// Publisher information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublisherInfo {
    pub domain: String,
    pub page_url: Option<String>,
}

/// Privacy-preserving user information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    /// Stable EC ID (from cookie or freshly generated).
    /// `None` when EC is unavailable or consent denies it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Decoded consent context for this request.
    ///
    /// Carries both raw consent strings (for `OpenRTB` forwarding) and decoded
    /// structured data (for TS-level enforcement and observability).
    /// Skipped during serde since it is populated at runtime from request
    /// cookies/headers, not from stored data.
    #[serde(skip)]
    pub consent: Option<crate::consent::ConsentContext>,
    /// Extended User IDs parsed from the [`crate::constants::COOKIE_TS_EIDS`] cookie.
    ///
    /// Raw (un-gated) values from the browser; consent gating via
    /// [`crate::consent::gate_eids_by_consent`] is applied centrally in the
    /// endpoint handlers (the auction and page-bids paths) before any EID
    /// reaches a bid request — the provider layer just forwards already-gated
    /// EIDs.
    #[serde(skip)]
    pub eids: Option<Vec<crate::openrtb::Eid>>,
}

/// Device information from request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub geo: Option<GeoInfo>,
}

/// Site information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SiteInfo {
    pub domain: String,
    pub page: String,
}

/// Context passed to auction providers.
///
/// # The `request` field is path-dependent
///
/// `request` carries the **real downstream client request** in the dispatch
/// path ([`AuctionOrchestrator::run_auction`][run] and
/// [`dispatch_auction`][dispatch]). Providers there can read client headers
/// (DNT, User-Agent, cookies, X-* customs) directly off it.
///
/// In the **collect path** ([`collect_dispatched_auction`][collect]) the
/// mediator is invoked with a synthetic placeholder request
/// (`https://placeholder.invalid/`), because the real client request has
/// already been consumed by `send_async` during dispatch and the host pipeline
/// can't lend it across the `.await`. **Mediators must not depend on reading
/// client state from `context.request`** — the placeholder has none of the
/// real headers. If a future mediator needs that data, snapshot it into a new
/// field on this struct at dispatch time and stash it on the
/// [`DispatchedAuction`] token so collect can attach it to the mediator's
/// context. See <https://github.com/IABTechLab/trusted-server/issues/680>
/// (P2-1) for the open follow-up.
///
/// [run]: crate::auction::AuctionOrchestrator::run_auction
/// [dispatch]: crate::auction::AuctionOrchestrator::dispatch_auction
/// [collect]: crate::auction::AuctionOrchestrator::collect_dispatched_auction
pub struct AuctionContext<'a> {
    pub settings: &'a Settings,
    pub request: &'a Request<EdgeBody>,
    pub timeout_ms: u32,
    /// Provider responses from the bidding phase, used by mediators.
    /// This is `None` for regular bidders and `Some` when calling a mediator.
    pub provider_responses: Option<&'a [AuctionResponse]>,
    /// Platform services (config store, secret store, etc.) for use by providers.
    pub services: &'a RuntimeServices,
}

/// URL used by the orchestrator when invoking a mediator from the collect
/// path. Providers can `debug_assert` against this value to catch a mediator
/// that has accidentally started depending on `context.request` carrying real
/// client headers.
pub const MEDIATOR_PLACEHOLDER_URL: &str = "https://placeholder.invalid/";

/// Response from a single auction provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuctionResponse {
    /// Provider that generated this response
    pub provider: String,
    /// Bids returned
    pub bids: Vec<Bid>,
    /// Status of the auction
    pub status: BidStatus,
    /// Response time in milliseconds
    pub response_time_ms: u64,
    /// Provider-specific metadata
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Wire key carrying the renderer type tag.
///
/// A payload may not use this key, since it would collide with the tag when
/// the descriptor is serialized flat.
const RENDERER_TYPE_KEY: &str = "type";

/// Browser renderer capability carried by a bid: a type tag and the payload
/// the auction provider that produced the bid defines.
///
/// Serialized flat, as `{"type": "<tag>", ...payload}`, so a page receives
/// the same bytes whether the provider lives in core or in its own crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BidRenderer {
    #[serde(rename = "type")]
    renderer_type: String,
    #[serde(flatten)]
    payload: serde_json::Map<String, serde_json::Value>,
}

impl BidRenderer {
    /// Build a descriptor from a type tag and the provider's JSON payload.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Auction`] when `payload` is not a JSON
    /// object, or when it carries its own `type` key, which would collide with
    /// the tag once the descriptor is serialized flat.
    ///
    /// # Examples
    ///
    /// ```
    /// use serde_json::json;
    /// use trusted_server_core::auction::types::BidRenderer;
    ///
    /// let renderer = BidRenderer::new("example", json!({ "version": 1 }))
    ///     .expect("should accept an object payload");
    /// assert_eq!(renderer.renderer_type(), "example");
    /// ```
    pub fn new(
        renderer_type: &str,
        payload: serde_json::Value,
    ) -> Result<Self, Report<TrustedServerError>> {
        let serde_json::Value::Object(payload) = payload else {
            bail!(TrustedServerError::Auction {
                message: format!("Renderer '{renderer_type}' payload must be a JSON object"),
            });
        };
        ensure!(
            !payload.contains_key(RENDERER_TYPE_KEY),
            TrustedServerError::Auction {
                message: format!(
                    "Renderer '{renderer_type}' payload must not carry a '{RENDERER_TYPE_KEY}' key"
                ),
            }
        );
        Ok(Self {
            renderer_type: renderer_type.to_string(),
            payload,
        })
    }

    /// Build a descriptor by serializing a provider's own payload type.
    ///
    /// # Errors
    ///
    /// Returns [`TrustedServerError::Auction`] when `payload` cannot be
    /// serialized, and when the serialized form is rejected by
    /// [`new`](Self::new).
    ///
    /// # Examples
    ///
    /// ```
    /// use serde::Serialize;
    /// use trusted_server_core::auction::types::BidRenderer;
    ///
    /// #[derive(Serialize)]
    /// struct ExampleRendererV1 {
    ///     version: u8,
    /// }
    ///
    /// let renderer = BidRenderer::from_typed("example", &ExampleRendererV1 { version: 1 })
    ///     .expect("should accept a struct payload");
    /// assert_eq!(renderer.renderer_type(), "example");
    /// ```
    pub fn from_typed<T: Serialize>(
        renderer_type: &str,
        payload: &T,
    ) -> Result<Self, Report<TrustedServerError>> {
        let payload =
            serde_json::to_value(payload).change_context(TrustedServerError::Auction {
                message: format!("Failed to serialize renderer '{renderer_type}' payload"),
            })?;
        Self::new(renderer_type, payload)
    }

    /// Return the renderer type tag a page reads to select its renderer.
    #[must_use]
    pub fn renderer_type(&self) -> &str {
        &self.renderer_type
    }

    /// Deserialize the payload into the provider's own descriptor type.
    ///
    /// Returns `None` when the descriptor carries a different tag, and when the
    /// payload does not match `T`.
    ///
    /// Clones the whole payload map and deserializes all of it, so use
    /// [`payload_field`](Self::payload_field) when the caller wants one field.
    /// An APS payload carries a base64 creative envelope of up to 256 KB.
    #[must_use]
    pub fn payload_as<T: DeserializeOwned>(&self, renderer_type: &str) -> Option<T> {
        if self.renderer_type != renderer_type {
            return None;
        }
        serde_json::from_value(serde_json::Value::Object(self.payload.clone())).ok()
    }

    /// Borrow one field of the payload, copying nothing.
    ///
    /// Returns `None` when the descriptor carries a different tag, and when
    /// the payload has no such key. `key` is the wire key, so a payload type
    /// that renames its fields for serialization must be asked for the
    /// renamed form.
    ///
    /// Unlike [`payload_as`](Self::payload_as) this reads the one field
    /// asked for and does not check that the rest of the payload matches the
    /// provider's descriptor type.
    ///
    /// # Examples
    ///
    /// ```
    /// use serde_json::json;
    /// use trusted_server_core::auction::types::BidRenderer;
    ///
    /// let renderer = BidRenderer::new("example", json!({ "bidId": "fictional-bid-id" }))
    ///     .expect("should accept an object payload");
    ///
    /// assert_eq!(
    ///     renderer
    ///         .payload_field("example", "bidId")
    ///         .and_then(serde_json::Value::as_str),
    ///     Some("fictional-bid-id"),
    /// );
    /// assert!(renderer.payload_field("other", "bidId").is_none());
    /// ```
    #[must_use]
    pub fn payload_field(&self, renderer_type: &str, key: &str) -> Option<&serde_json::Value> {
        if self.renderer_type != renderer_type {
            return None;
        }
        self.payload.get(key)
    }
}

/// Individual bid from a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bid {
    /// Slot this bid is for
    pub slot_id: String,
    /// Bid price in CPM.
    pub price: Option<f64>,
    /// Currency code (e.g., "USD")
    pub currency: String,
    /// Creative markup (HTML/VAST).
    ///
    /// `None` when the bid uses a typed [`BidRenderer`] instead.
    pub creative: Option<String>,
    /// Advertiser domain
    pub adomain: Option<Vec<String>>,
    /// Bidder/seat identifier
    pub bidder: String,
    /// Width of creative
    pub width: u32,
    /// Height of creative
    pub height: u32,
    /// Win notification URL
    pub nurl: Option<String>,
    /// Billing notification URL
    pub burl: Option<String>,
    /// `OpenRTB` bid identifier — the `id` of the bid object itself.
    ///
    /// Distinct from [`ad_id`](Self::ad_id): unique per bid instance rather
    /// than a creative identifier. Always present per the `OpenRTB` spec, so it
    /// is the last-resort `hb_adid` source for bidders that return neither a
    /// Prebid Cache UUID nor `adid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bid_id: Option<String>,
    /// Ad ID from the bidder.
    pub ad_id: Option<String>,
    /// Optional `OpenRTB` creative identifier.
    pub creative_id: Option<String>,
    /// Typed browser renderer capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renderer: Option<BidRenderer>,
    /// Prebid Cache UUID for this bid.
    ///
    /// Populated from `ext.prebid.cache.bids.cacheId` in the PBS response.
    /// Used as `hb_adid` targeting value in `window.tsjs.bids`. `None` for
    /// non-PBS providers (e.g., APS) and PBS bids without Prebid Cache enabled.
    pub cache_id: Option<String>,
    /// Prebid Cache host (e.g., `"openads.adsrvr.org"`).
    ///
    /// Populated from the host of `ext.prebid.cache.bids.url`. Used as
    /// `hb_cache_host` targeting value. `None` when cache is absent.
    pub cache_host: Option<String>,
    /// Prebid Cache path (e.g., `"/cache"`).
    ///
    /// Populated from the path of `ext.prebid.cache.bids.url`. Used as
    /// `hb_cache_path` targeting value. `None` when cache is absent.
    pub cache_path: Option<String>,
    /// Provider-specific bid metadata.
    pub metadata: HashMap<String, serde_json::Value>,
}

/// Per-provider summary included in the auction response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSummary {
    /// Provider name (e.g., "prebid", "aps").
    pub name: String,
    /// Bid status from this provider.
    pub status: BidStatus,
    /// Number of bids returned.
    pub bid_count: usize,
    /// Unique bidder/seat names (e.g., "kargo", "pubmatic", "ix").
    pub bidders: Vec<String>,
    /// Response time in milliseconds.
    pub time_ms: u64,
    /// Provider-specific metadata (from [`AuctionResponse::metadata`]).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,
}

impl From<&AuctionResponse> for ProviderSummary {
    fn from(response: &AuctionResponse) -> Self {
        let mut bidders: Vec<String> = response.bids.iter().map(|b| b.bidder.clone()).collect();
        bidders.sort_unstable();
        bidders.dedup();

        Self {
            name: response.provider.clone(),
            status: response.status.clone(),
            bid_count: response.bids.len(),
            bidders,
            time_ms: response.response_time_ms,
            metadata: response.metadata.clone(),
        }
    }
}

/// `OpenRTB` response metadata for the orchestrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorExt {
    pub strategy: String,
    pub providers: usize,
    pub total_bids: usize,
    pub time_ms: u64,
    /// Per-provider breakdown of the auction.
    #[serde(default)]
    pub provider_details: Vec<ProviderSummary>,
    /// Winners omitted during final response conversion.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped_winner_count: usize,
    /// Machine-readable reasons for omitted winners.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dropped_winner_reasons: BTreeMap<String, usize>,
}

/// Status of bid response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BidStatus {
    /// Auction completed successfully
    Success,
    /// No bids returned
    NoBid,
    /// Auction failed/timed out
    Error,
    /// Auction still in progress
    Pending,
}

impl AuctionResponse {
    /// Create a new successful auction response.
    pub fn success(provider: impl Into<String>, bids: Vec<Bid>, response_time_ms: u64) -> Self {
        Self {
            provider: provider.into(),
            bids,
            status: BidStatus::Success,
            response_time_ms,
            metadata: HashMap::new(),
        }
    }

    /// Create a no-bid response.
    pub fn no_bid(provider: impl Into<String>, response_time_ms: u64) -> Self {
        Self {
            provider: provider.into(),
            bids: Vec::new(),
            status: BidStatus::NoBid,
            response_time_ms,
            metadata: HashMap::new(),
        }
    }

    /// Create an error response.
    pub fn error(provider: impl Into<String>, response_time_ms: u64) -> Self {
        Self {
            provider: provider.into(),
            bids: Vec::new(),
            status: BidStatus::Error,
            response_time_ms,
            metadata: HashMap::new(),
        }
    }

    /// Add metadata to the response.
    pub fn with_metadata(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::aps::{APS_RENDERER_TYPE, ApsRendererV1, ApsTagType};
    use serde_json::json;

    fn make_bid(bidder: &str) -> Bid {
        Bid {
            slot_id: "slot-1".to_owned(),
            price: Some(1.0),
            currency: "USD".to_owned(),
            creative: None,
            adomain: None,
            bidder: bidder.to_owned(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            bid_id: None,
            ad_id: None,
            creative_id: None,
            renderer: None,
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn provider_summary_from_successful_response() {
        let response = AuctionResponse::success(
            "prebid",
            vec![make_bid("kargo"), make_bid("pubmatic"), make_bid("ix")],
            95,
        );

        let summary = ProviderSummary::from(&response);

        assert_eq!(summary.name, "prebid", "should use provider name");
        assert_eq!(summary.status, BidStatus::Success, "should preserve status");
        assert_eq!(summary.bid_count, 3, "should count all bids");
        assert_eq!(
            summary.bidders,
            vec!["ix", "kargo", "pubmatic"],
            "should list unique bidders sorted"
        );
        assert_eq!(summary.time_ms, 95, "should preserve response time");
        assert!(summary.metadata.is_empty(), "should have no metadata");
    }

    #[test]
    fn provider_summary_deduplicates_bidder_names() {
        let response = AuctionResponse::success(
            "prebid",
            vec![make_bid("kargo"), make_bid("kargo"), make_bid("pubmatic")],
            50,
        );

        let summary = ProviderSummary::from(&response);

        assert_eq!(
            summary.bid_count, 3,
            "should count all bids including dupes"
        );
        assert_eq!(
            summary.bidders,
            vec!["kargo", "pubmatic"],
            "should deduplicate bidder names"
        );
    }

    #[test]
    fn provider_summary_from_no_bid_response() {
        let response = AuctionResponse::no_bid("aps", 110);

        let summary = ProviderSummary::from(&response);

        assert_eq!(summary.name, "aps", "should use provider name");
        assert_eq!(
            summary.status,
            BidStatus::NoBid,
            "should preserve no-bid status"
        );
        assert_eq!(summary.bid_count, 0, "should have zero bids");
        assert!(summary.bidders.is_empty(), "should have no bidders");
    }

    #[test]
    fn provider_summary_from_error_response() {
        let response = AuctionResponse::error("prebid", 200);

        let summary = ProviderSummary::from(&response);

        assert_eq!(
            summary.status,
            BidStatus::Error,
            "should preserve error status"
        );
        assert_eq!(summary.bid_count, 0, "should have zero bids");
        assert!(summary.bidders.is_empty(), "should have no bidders");
    }

    #[test]
    fn provider_summary_passes_through_metadata() {
        let response = AuctionResponse::success("prebid", vec![make_bid("kargo")], 80)
            .with_metadata("responsetimemillis", json!({"kargo": 70, "pubmatic": 90}))
            .with_metadata("errors", json!({"pubmatic": [{"code": 1}]}));

        let summary = ProviderSummary::from(&response);

        assert_eq!(summary.metadata.len(), 2, "should forward all metadata");
        assert_eq!(
            summary.metadata["responsetimemillis"],
            json!({"kargo": 70, "pubmatic": 90}),
            "should preserve responsetimemillis"
        );
        assert_eq!(
            summary.metadata["errors"],
            json!({"pubmatic": [{"code": 1}]}),
            "should preserve errors"
        );
    }

    #[test]
    fn provider_summary_skips_metadata_in_serialization_when_empty() {
        let response = AuctionResponse::no_bid("aps", 100);
        let summary = ProviderSummary::from(&response);

        let json = serde_json::to_value(&summary).expect("should serialize");

        assert!(
            json.get("metadata").is_none(),
            "should omit metadata field when empty"
        );
    }

    #[test]
    fn bid_with_cache_fields_round_trips_through_json() {
        let bid = Bid {
            slot_id: "atf".to_string(),
            price: Some(1.50),
            currency: "USD".to_string(),
            creative: None,
            adomain: None,
            bidder: "thetradedesk".to_string(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            bid_id: None,
            ad_id: Some("bid-id".to_string()),
            creative_id: None,
            renderer: None,
            cache_id: Some("cache-uuid".to_string()),
            cache_host: Some("cache.example.com".to_string()),
            cache_path: Some("/pbc/v1/cache".to_string()),
            metadata: HashMap::new(),
        };
        let json = serde_json::to_string(&bid).expect("should serialize Bid");
        let restored: Bid = serde_json::from_str(&json).expect("should deserialize Bid");
        assert_eq!(
            restored.cache_id.as_deref(),
            Some("cache-uuid"),
            "should round-trip cache_id"
        );
        assert_eq!(
            restored.cache_host.as_deref(),
            Some("cache.example.com"),
            "should round-trip cache_host"
        );
        assert_eq!(
            restored.cache_path.as_deref(),
            Some("/pbc/v1/cache"),
            "should round-trip cache_path"
        );
    }

    #[test]
    fn aps_renderer_serializes_to_versioned_camel_case_contract() {
        let renderer = BidRenderer::from_typed(
            APS_RENDERER_TYPE,
            &ApsRendererV1 {
                version: 1,
                account_id: "example-account-id".to_string(),
                bid_id: "fictional-bid-id".to_string(),
                creative_id: Some("fictional-creative-id".to_string()),
                tag_type: ApsTagType::Iframe,
                creative_url: "https://creative.example/render".to_string(),
                aax_response: "base64-data".to_string(),
                width: 300,
                height: 250,
            },
        )
        .expect("should build APS renderer descriptor");

        let serialized = serde_json::to_value(&renderer).expect("should serialize renderer");

        assert_eq!(
            serialized,
            json!({
                "type": "aps",
                "version": 1,
                "accountId": "example-account-id",
                "bidId": "fictional-bid-id",
                "creativeId": "fictional-creative-id",
                "tagType": "iframe",
                "creativeUrl": "https://creative.example/render",
                "aaxResponse": "base64-data",
                "width": 300,
                "height": 250
            }),
            "should match renderer wire contract"
        );
    }

    #[test]
    fn aps_renderer_omits_absent_creative_id() {
        let renderer = BidRenderer::from_typed(
            APS_RENDERER_TYPE,
            &ApsRendererV1 {
                version: 1,
                account_id: "example-account-id".to_string(),
                bid_id: "fictional-bid-id".to_string(),
                creative_id: None,
                tag_type: ApsTagType::Iframe,
                creative_url: "https://creative.example/render".to_string(),
                aax_response: "base64-data".to_string(),
                width: 300,
                height: 250,
            },
        )
        .expect("should build APS renderer descriptor");

        let serialized = serde_json::to_value(&renderer).expect("should serialize renderer");

        assert!(
            serialized.get("creativeId").is_none(),
            "should omit absent creative ID"
        );
    }

    /// Rewrites every object in `value` with its keys in sorted order, so
    /// serializing the result gives one fixed key order.
    ///
    /// `serde_json::Map` is a `BTreeMap`, which serializes keys in sorted
    /// order, only while the crate's `preserve_order` feature is off. With the
    /// feature on it is an `IndexMap` and the order follows insertion instead.
    /// Nothing in this crate asks for the feature, but Cargo unifies features
    /// across everything built for one target, and `trusted-server-cli` pulls
    /// it in through `edgezero-cli` and then `handlebars`. A maintainer
    /// running `cargo test --workspace --target <host>` therefore builds this
    /// crate with `preserve_order` on, and a test that pinned insertion order
    /// would fail there for no reason. Sorting both sides removes the
    /// dependence on which map `serde_json` was built with.
    fn with_sorted_keys(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let mut keys = map.keys().collect::<Vec<_>>();
                keys.sort_unstable();
                let mut sorted = serde_json::Map::with_capacity(keys.len());
                for key in keys {
                    let child = map.get(key).expect("should find a key the map just listed");
                    sorted.insert(key.clone(), with_sorted_keys(child));
                }
                serde_json::Value::Object(sorted)
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(with_sorted_keys).collect())
            }
            scalar => scalar.clone(),
        }
    }

    #[test]
    fn the_open_renderer_serializes_to_the_same_bytes_as_the_aps_variant_did() {
        // Literal strings captured from the closed-enum form before this
        // change, through the same `serde_json::to_value` path production
        // uses: `BidExt::to_ext` for the OpenRTB response extension, and
        // `build_bid_map` for `window.tsjs.bids`.
        //
        // Both sides go through `with_sorted_keys` first, because the key
        // order `serde_json` emits is not ours to pin, and that function
        // explains why. Sorting settles the order without weakening what is
        // pinned, since two objects serialize to the same sorted bytes only
        // when they carry exactly the same keys with exactly the same values.
        let full = BidRenderer::from_typed(
            APS_RENDERER_TYPE,
            &ApsRendererV1 {
                version: 1,
                account_id: "example-account-id".to_string(),
                bid_id: "fictional-bid-id".to_string(),
                creative_id: Some("fictional-creative-id".to_string()),
                tag_type: ApsTagType::Iframe,
                creative_url: "https://creative.example/render".to_string(),
                aax_response: "base64-data".to_string(),
                width: 300,
                height: 250,
            },
        )
        .expect("should build APS renderer descriptor");
        let absent = BidRenderer::from_typed(
            APS_RENDERER_TYPE,
            &ApsRendererV1 {
                version: 1,
                account_id: "example-account-id".to_string(),
                bid_id: "fictional-bid-id".to_string(),
                creative_id: None,
                tag_type: ApsTagType::Script,
                creative_url: "https://creative.example/render".to_string(),
                aax_response: "base64-data".to_string(),
                width: 300,
                height: 250,
            },
        )
        .expect("should build APS renderer descriptor");

        let full_bytes = serde_json::to_string(&with_sorted_keys(
            &serde_json::to_value(&full).expect("should convert renderer to a JSON value"),
        ))
        .expect("should serialize renderer");
        let absent_bytes = serde_json::to_string(&with_sorted_keys(
            &serde_json::to_value(&absent).expect("should convert renderer to a JSON value"),
        ))
        .expect("should serialize renderer");

        assert_eq!(
            full_bytes,
            "{\"aaxResponse\":\"base64-data\",\"accountId\":\"example-account-id\",\"bidId\":\"fictional-bid-id\",\"creativeId\":\"fictional-creative-id\",\"creativeUrl\":\"https://creative.example/render\",\"height\":250,\"tagType\":\"iframe\",\"type\":\"aps\",\"version\":1,\"width\":300}",
            "should serialize to the bytes the closed enum produced"
        );
        assert_eq!(
            absent_bytes,
            "{\"aaxResponse\":\"base64-data\",\"accountId\":\"example-account-id\",\"bidId\":\"fictional-bid-id\",\"creativeUrl\":\"https://creative.example/render\",\"height\":250,\"tagType\":\"script\",\"type\":\"aps\",\"version\":1,\"width\":300}",
            "should serialize to the bytes the closed enum produced with no creative ID"
        );
    }

    #[test]
    fn renderer_round_trips_through_its_wire_form() {
        let descriptor = ApsRendererV1 {
            version: 1,
            account_id: "example-account-id".to_string(),
            bid_id: "fictional-bid-id".to_string(),
            creative_id: Some("fictional-creative-id".to_string()),
            tag_type: ApsTagType::Iframe,
            creative_url: "https://creative.example/render".to_string(),
            aax_response: "base64-data".to_string(),
            width: 300,
            height: 250,
        };
        let renderer = BidRenderer::from_typed(APS_RENDERER_TYPE, &descriptor)
            .expect("should build APS renderer descriptor");

        let serialized = serde_json::to_string(&renderer).expect("should serialize renderer");
        let restored: BidRenderer =
            serde_json::from_str(&serialized).expect("should deserialize renderer");

        assert_eq!(
            restored.renderer_type(),
            APS_RENDERER_TYPE,
            "should round-trip the renderer type tag"
        );
        assert_eq!(
            restored
                .payload_as::<ApsRendererV1>(APS_RENDERER_TYPE)
                .expect("should deserialize the APS payload"),
            descriptor,
            "should round-trip the provider payload"
        );
    }

    #[test]
    fn renderer_payload_is_hidden_from_a_different_type_tag() {
        let renderer = BidRenderer::new(APS_RENDERER_TYPE, json!({ "version": 1 }))
            .expect("should build renderer descriptor");

        assert!(
            renderer.payload_as::<ApsRendererV1>("example").is_none(),
            "should refuse a payload requested under a different tag"
        );
    }

    #[test]
    fn renderer_payload_field_borrows_the_same_value_the_whole_descriptor_carries() {
        let descriptor = ApsRendererV1 {
            version: 1,
            account_id: "example-account-id".to_string(),
            bid_id: "fictional-bid-id".to_string(),
            creative_id: Some("fictional-creative-id".to_string()),
            tag_type: ApsTagType::Iframe,
            creative_url: "https://creative.example/render".to_string(),
            aax_response: "base64-data".to_string(),
            width: 300,
            height: 250,
        };
        let renderer = BidRenderer::from_typed(APS_RENDERER_TYPE, &descriptor)
            .expect("should build APS renderer descriptor");

        assert_eq!(
            renderer
                .payload_field(APS_RENDERER_TYPE, "bidId")
                .and_then(serde_json::Value::as_str),
            Some(descriptor.bid_id.as_str()),
            "should read the same bid id the whole descriptor carries"
        );
        assert_eq!(
            renderer
                .payload_field(APS_RENDERER_TYPE, "bidId")
                .and_then(serde_json::Value::as_str),
            renderer
                .payload_as::<ApsRendererV1>(APS_RENDERER_TYPE)
                .as_ref()
                .map(|full| full.bid_id.as_str()),
            "should agree with the field read through the whole descriptor"
        );
        assert!(
            renderer
                .payload_field(APS_RENDERER_TYPE, "notAKey")
                .is_none(),
            "should return nothing for a key the payload does not carry"
        );
    }

    #[test]
    fn renderer_payload_field_is_hidden_from_a_different_type_tag() {
        let renderer = BidRenderer::new(APS_RENDERER_TYPE, json!({ "bidId": "fictional-bid-id" }))
            .expect("should build renderer descriptor");

        assert!(
            renderer.payload_field("example", "bidId").is_none(),
            "should refuse a field requested under a different tag"
        );
    }

    #[test]
    fn renderer_rejects_a_payload_that_is_not_an_object() {
        assert!(
            BidRenderer::new(APS_RENDERER_TYPE, json!("not-an-object")).is_err(),
            "should reject a payload that is not a JSON object"
        );
    }

    #[test]
    fn renderer_rejects_a_payload_carrying_its_own_type_key() {
        assert!(
            BidRenderer::new(APS_RENDERER_TYPE, json!({ "type": "other", "version": 1 })).is_err(),
            "should reject a payload that would collide with the type tag"
        );
    }

    #[test]
    fn media_type_defaults_to_banner() {
        assert_eq!(
            MediaType::default(),
            MediaType::Banner,
            "should default to Banner for serde field defaults"
        );
    }

    #[test]
    fn bid_has_ad_id_field() {
        let bid = Bid {
            slot_id: "s".to_string(),
            price: Some(1.0),
            currency: "USD".to_string(),
            creative: None,
            adomain: None,
            bidder: "kargo".to_string(),
            width: 300,
            height: 250,
            nurl: None,
            burl: None,
            bid_id: None,
            ad_id: Some("prebid-ad-id-abc".to_string()),
            creative_id: None,
            renderer: None,
            cache_id: None,
            cache_host: None,
            cache_path: None,
            metadata: Default::default(),
        };
        assert_eq!(bid.ad_id.as_deref(), Some("prebid-ad-id-abc"));
    }
}
