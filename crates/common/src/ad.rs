use error_stack::{Report, ResultExt};
use fastly::{Request, Response};
use serde::Deserialize;
use serde_json::Value as JsonValue;
use std::collections::HashMap;

use crate::error::TrustedServerError;
use crate::openrtb;
use crate::settings::Settings;
use fastly::http::{header, StatusCode};
use serde_json::Value as Json;
// pixel HTML rewrite lives in crate::pixel

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
    bids: Option<Vec<Bid>>, // Prebid-style bids in adUnit
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdRequest {
    ad_units: Vec<AdUnit>,
    #[allow(dead_code)]
    config: Option<JsonValue>,
}

#[derive(Debug, Deserialize)]
struct Bid {
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
                        .filter(|&s| s.len() >= 2)
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

// Wrapper that allows tests to intercept the PBS call used by the GET handler.
async fn pbs_auction_for_get(
    settings: &Settings,
    req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    #[cfg(test)]
    {
        if let Some(body) = MOCK_PBS_JSON.with(|c| c.borrow_mut().take()) {
            return Ok(Response::from_status(StatusCode::OK)
                .with_header(header::CONTENT_TYPE, "application/json")
                .with_body(body));
        }
    }
    crate::prebid_proxy::handle_prebid_auction(settings, req).await
}

#[cfg(test)]
thread_local! {
    static MOCK_PBS_JSON: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn set_mock_pbs_response(body: Vec<u8>) {
    MOCK_PBS_JSON.with(|c| *c.borrow_mut() = Some(body));
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

    log::info!("/third-party/ad: received {} adUnits", body.ad_units.len());
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

    crate::prebid_proxy::handle_prebid_auction(settings, req).await
}

/// GET variant for first-party slot rendering: /first-party/ad?slot=code[&w=300&h=250]
pub async fn handle_server_ad_get(
    settings: &Settings,
    mut req: Request,
) -> Result<Response, Report<TrustedServerError>> {
    // Parse query
    let url = req.get_url_str();
    let parsed = url::Url::parse(url).change_context(TrustedServerError::Prebid {
        message: "Invalid first-party serve-ad URL".to_string(),
    })?;
    let qp = parsed
        .query_pairs()
        .into_owned()
        .collect::<std::collections::HashMap<_, _>>();
    let slot = qp.get("slot").cloned().unwrap_or_default();
    let w = qp
        .get("w")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(300);
    let h = qp
        .get("h")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(250);
    if slot.is_empty() {
        return Err(Report::new(TrustedServerError::BadRequest {
            message: "missing slot".to_string(),
        }));
    }

    // Build a synthetic AdRequest with a single unit for this slot
    let ad_req = AdRequest {
        ad_units: vec![AdUnit {
            code: slot.clone(),
            media_types: Some(MediaTypes {
                banner: Some(BannerUnit {
                    sizes: vec![vec![w, h]],
                }),
            }),
            bids: None,
        }],
        config: None,
    };

    // Convert to OpenRTB and delegate to PBS
    let ortb = build_openrtb_from_ts(&ad_req, settings);
    req.set_body_json(&ortb)
        .change_context(TrustedServerError::Prebid {
            message: "Failed to set OpenRTB body".to_string(),
        })?;
    let mut pbs_resp = pbs_auction_for_get(settings, req).await?;

    // Try to extract HTML creative for this slot
    let body_bytes = pbs_resp.take_body_bytes();
    let html = match serde_json::from_slice::<Json>(&body_bytes) {
        Ok(json) => {
            extract_adm_for_slot(&json, &slot).unwrap_or_else(|| "<!-- no creative -->".to_string())
        }
        Err(_) => String::from_utf8(body_bytes).unwrap_or_else(|_| "".to_string()),
    };

    let rewritten = crate::creative::rewrite_creative_html(&html, settings);

    Ok(Response::from_status(StatusCode::OK)
        .with_header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .with_body(rewritten))
}

fn extract_adm_for_slot(json: &Json, slot: &str) -> Option<String> {
    let seatbids = json.get("seatbid")?.as_array()?;
    for sb in seatbids {
        if let Some(bids) = sb.get("bid").and_then(|b| b.as_array()) {
            for b in bids {
                let impid = b.get("impid").and_then(|v| v.as_str()).unwrap_or("");
                if impid == slot {
                    if let Some(adm) = b.get("adm").and_then(|v| v.as_str()) {
                        return Some(adm.to_string());
                    }
                }
            }
        }
    }
    // Fallback to first available adm
    for sb in seatbids {
        if let Some(bids) = sb.get("bid").and_then(|b| b.as_array()) {
            for b in bids {
                if let Some(adm) = b.get("adm").and_then(|v| v.as_str()) {
                    return Some(adm.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::tests::create_test_settings;
    use fastly::http::{Method, StatusCode};
    use fastly::Request;
    use serde_json::json;

    #[test]
    fn build_openrtb_defaults_when_missing_sizes_and_bids() {
        let settings = create_test_settings();
        let req = AdRequest {
            ad_units: vec![AdUnit {
                code: "slot1".to_string(),
                media_types: None,
                bids: None,
            }],
            config: None,
        };
        let ortb = build_openrtb_from_ts(&req, &settings);
        assert_eq!(ortb.imp.len(), 1);
        let imp = &ortb.imp[0];
        assert_eq!(imp.id, "slot1");
        let banner = imp.banner.as_ref().expect("banner present");
        assert_eq!(banner.format.len(), 1);
        assert_eq!(banner.format[0].w, 300);
        assert_eq!(banner.format[0].h, 250);
        let bidders = &imp.ext.as_ref().unwrap().prebid.bidder;
        for b in &settings.prebid.bidders {
            assert!(bidders.contains_key(b), "missing bidder {}", b);
        }
        assert_eq!(bidders.len(), settings.prebid.bidders.len());
        let site = ortb.site.as_ref().expect("site present");
        assert_eq!(
            site.domain.as_deref(),
            Some(settings.publisher.domain.as_str())
        );
        assert_eq!(
            site.page.as_deref(),
            Some(format!("https://{}", settings.publisher.domain).as_str())
        );
    }

    #[test]
    fn build_openrtb_uses_provided_sizes_and_bids() {
        let settings = create_test_settings();
        let req = AdRequest {
            ad_units: vec![AdUnit {
                code: "slot2".to_string(),
                media_types: Some(MediaTypes {
                    banner: Some(BannerUnit {
                        sizes: vec![vec![728, 90], vec![300, 250]],
                    }),
                }),
                bids: Some(vec![
                    Bid {
                        bidder: "openx".to_string(),
                        params: json!({"unit":"123"}),
                    },
                    Bid {
                        bidder: "rubicon".to_string(),
                        params: json!({}),
                    },
                ]),
            }],
            config: None,
        };
        let ortb = build_openrtb_from_ts(&req, &settings);
        let imp = &ortb.imp[0];
        let banner = imp.banner.as_ref().unwrap();
        assert_eq!(banner.format.len(), 2);
        assert!(banner.format.iter().any(|f| f.w == 728 && f.h == 90));
        assert!(banner.format.iter().any(|f| f.w == 300 && f.h == 250));
        let bidders = &imp.ext.as_ref().unwrap().prebid.bidder;
        assert!(bidders.contains_key("openx"));
        assert!(bidders.contains_key("rubicon"));
        // When bids provided, do not add defaults
        assert_eq!(bidders.len(), 2);
    }

    #[test]
    fn extract_adm_picks_matching_slot_then_fallback() {
        let json = json!({
            "seatbid": [
                { "bid": [
                    { "impid": "slot2", "adm": "<div>two</div>" },
                    { "impid": "slot1", "adm": "<div>one</div>" }
                ]}
            ]
        });
        let adm = extract_adm_for_slot(&json, "slot1").expect("adm present");
        assert!(adm.contains("one"));

        let json2 = json!({
            "seatbid": [
                { "bid": [ { "impid": "other", "adm": "<div>x</div>" } ] }
            ]
        });
        let adm2 = extract_adm_for_slot(&json2, "slot-missing").expect("fallback adm");
        assert!(adm2.contains("x"));
    }

    #[tokio::test]
    async fn handle_server_ad_get_missing_slot_returns_400() {
        let settings = create_test_settings();
        let req = Request::new(
            Method::GET,
            "https://example.com/first-party/ad?w=300&h=250",
        );
        let err = handle_server_ad_get(&settings, req)
            .await
            .expect_err("expected error");
        // ensure this is a BadRequest surfacing a 400 mapping
        assert!(err.to_string().contains("missing slot"));
    }

    #[tokio::test]
    async fn handle_server_ad_get_returns_html_ct_when_adm_present() {
        let settings = create_test_settings();
        // Mock PBS JSON with matching impid and simple HTML adm
        let mock = serde_json::json!({
            "seatbid": [{
                "bid": [{ "impid": "slotA", "adm": "<div>creative</div>" }]
            }]
        });
        super::set_mock_pbs_response(serde_json::to_vec(&mock).unwrap());

        let req = Request::new(
            Method::GET,
            "https://example.com/first-party/ad?slot=slotA&w=300&h=250",
        );
        let mut res = handle_server_ad_get(&settings, req).await.unwrap();
        assert_eq!(res.get_status(), StatusCode::OK);
        let ct = res
            .get_header(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(ct.contains("text/html"));
        let body = String::from_utf8(res.take_body_bytes()).unwrap();
        assert!(body.contains("creative"));
    }

    #[tokio::test]
    async fn handle_server_ad_get_rewrites_1x1_pixels() {
        let settings = create_test_settings();
        // Mock PBS JSON with a 1x1 img pixel in adm
        let mock = serde_json::json!({
            "seatbid": [{
                "bid": [{ "impid": "slotP", "adm": "<html><body><img width=\"1\" height=\"1\" src=\"https://tracker.example/p.gif\"></body></html>" }]
            }]
        });
        super::set_mock_pbs_response(serde_json::to_vec(&mock).unwrap());

        let req = Request::new(
            Method::GET,
            "https://example.com/first-party/ad?slot=slotP&w=300&h=250",
        );
        let mut res = handle_server_ad_get(&settings, req).await.unwrap();
        assert_eq!(res.get_status(), StatusCode::OK);
        let body = String::from_utf8(res.take_body_bytes()).unwrap();
        // Should rewrite to unified first-party proxy endpoint with clear tsurl + tstoken
        assert!(body.contains("/first-party/proxy?tsurl="));
    }
}
