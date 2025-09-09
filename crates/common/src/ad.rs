use error_stack::{Report, ResultExt};
use fastly::{Request, Response};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::collections::HashMap;

use crate::error::TrustedServerError;
use crate::openrtb;
use crate::prebid_proxy::handle_prebid_auction;
use crate::settings::Settings;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BannerUnit {
    sizes: Vec<Vec<u32>>, // [[w,h], ...]
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaTypes {
    #[allow(dead_code)]
    banner: Option<BannerUnit>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdUnit {
    code: String,
    #[allow(dead_code)]
    media_types: Option<MediaTypes>,
    #[serde(default)]
    bids: Option<Vec<TsBid>>, // Prebid-style bids in adUnit
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdRequest {
    ad_units: Vec<AdUnit>,
    #[allow(dead_code)]
    config: Option<JsonValue>,
}

#[derive(Debug, Deserialize)]
struct TsBid {
    bidder: String,
    #[serde(default)]
    params: JsonValue,
}

/// Build a minimal typed OpenRTB request from tsjs ad units.
fn build_openrtb_from_ts(req: &AdRequest, settings: &Settings) -> openrtb::OpenRtbRequest {
    use openrtb::{Banner, Format, Imp, ImpExt, OpenRtbRequest, PrebidImpExt, Site};
    use uuid::Uuid;

    let imps: Vec<Imp> = req
        .ad_units
        .iter()
        .map(|unit| {
            let formats: Vec<Format> = unit
                .media_types
                .as_ref()
                .and_then(|mt| mt.banner.as_ref())
                .map(|b| {
                    b.sizes
                        .iter()
                        .filter(|&s| (s.len() >= 2))
                        .map(|s| Format { w: s[0], h: s[1] })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| vec![Format { w: 300, h: 250 }]);

            // Build bidder map from unit.bids or fallback to settings.prebid.bidders
            let mut bidder: HashMap<String, JsonValue> = HashMap::new();
            if let Some(bids) = &unit.bids {
                for b in bids {
                    bidder.insert(b.bidder.clone(), b.params.clone());
                }
            }
            if bidder.is_empty() {
                for b in &settings.prebid.bidders {
                    bidder.insert(b.clone(), JsonValue::Object(serde_json::Map::new()));
                }
            }

            Imp {
                id: unit.code.clone(),
                banner: Some(Banner { format: formats }),
                ext: Some(ImpExt {
                    prebid: PrebidImpExt { bidder },
                }),
            }
        })
        .collect();

    OpenRtbRequest {
        id: Uuid::new_v4().to_string(),
        imp: imps,
        site: Some(Site {
            domain: Some(settings.publisher.domain.clone()),
            page: Some(format!("https://{}", settings.publisher.domain)),
        }),
    }
}

/// Handle tsjs ad requests and proxy to Prebid Server using the existing proxy pipeline.
pub async fn handle_server_ad(
    settings: &Settings,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    // Parse incoming tsjs request
    let body: AdRequest = serde_json::from_slice(&req.take_body_bytes()).change_context(
        TrustedServerError::Prebid {
            message: "Failed to parse tsjs auction request".to_string(),
        },
    )?;

    log::info!("/serve-ad: received {} adUnits", body.ad_units.len());
    for u in &body.ad_units {
        if let Some(mt) = &u.media_types {
            if let Some(b) = &mt.banner {
                log::debug!("unit={} sizes={:?}", u.code, b.sizes);
            } else {
                log::debug!("unit={} sizes=(none)", u.code);
            }
        } else {
            log::debug!("unit={} mediaTypes=(none)", u.code);
        }
    }

    // Build minimal OpenRTB request
    let openrtb = build_openrtb_from_ts(&body, settings);
    // Serialize once for logging/debug
    if let Ok(preview) = serde_json::to_string(&openrtb) {
        log::debug!(
            "OpenRTB payload (truncated): {}",
            &preview.chars().take(512).collect::<String>()
        );
    }

    // Reuse the existing Prebid Server proxy path by setting the body and delegating
    req.set_body_json(&openrtb)
        .change_context(TrustedServerError::Prebid {
            message: "Failed to set OpenRTB body".to_string(),
        })?;

    handle_prebid_auction(settings, req).await
}
