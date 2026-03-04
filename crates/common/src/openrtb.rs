use serde::Serialize;
use serde_json::{Map, Value};

use crate::auction::types::OrchestratorExt;

pub type Object = trusted_server_openrtb::Object;
pub type OpenRtbRequest = trusted_server_openrtb::BidRequest;
pub type OpenRtbResponse = trusted_server_openrtb::BidResponse;
pub type OpenRtbBid = trusted_server_openrtb::Bid;

pub use trusted_server_openrtb::{
    Banner, Device, Format, Geo, Imp, Publisher, Regs, SeatBid, Site, User,
};

fn clamp_u32_to_i32(value: u32) -> i32 {
    value.min(i32::MAX as u32) as i32
}

pub fn object_from_serializable<T: Serialize>(value: &T) -> Object {
    match serde_json::to_value(value) {
        Ok(Value::Object(map)) => map,
        Ok(_) | Err(_) => Map::new(),
    }
}

pub fn maybe_object_from_serializable<T: Serialize>(value: &T) -> Option<Object> {
    let map = object_from_serializable(value);
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

#[must_use]
pub fn build_format(width: u32, height: u32) -> Format {
    Format {
        w: Some(clamp_u32_to_i32(width)),
        h: Some(clamp_u32_to_i32(height)),
        wratio: None,
        hratio: None,
        wmin: None,
        ext: None,
    }
}

#[must_use]
pub fn build_banner(formats: Vec<Format>) -> Banner {
    Banner {
        format: Some(formats),
        w: None,
        h: None,
        btype: None,
        battr: None,
        pos: None,
        mimes: None,
        topframe: None,
        expdir: None,
        api: None,
        id: None,
        vcm: None,
        ext: None,
    }
}

#[must_use]
pub fn build_imp(
    id: String,
    banner: Option<Banner>,
    bidfloor: Option<f64>,
    bidfloorcur: Option<String>,
    secure: Option<i32>,
    tagid: Option<String>,
    ext: Option<Object>,
) -> Imp {
    Imp {
        id,
        metric: None,
        banner,
        video: None,
        audio: None,
        native: None,
        pmp: None,
        displaymanager: None,
        displaymanagerver: None,
        instl: None,
        tagid,
        bidfloor,
        bidfloorcur,
        clickbrowser: None,
        secure,
        iframebuster: None,
        rwdd: None,
        ssai: None,
        exp: None,
        qty: None,
        dt: None,
        refresh: None,
        ext,
    }
}

#[must_use]
pub fn build_site(
    domain: Option<String>,
    page: Option<String>,
    r#ref: Option<String>,
    publisher: Option<Publisher>,
) -> Site {
    Site {
        id: None,
        name: None,
        domain,
        cattax: None,
        cat: None,
        sectioncat: None,
        pagecat: None,
        page,
        r#ref,
        search: None,
        mobile: None,
        privacypolicy: None,
        publisher,
        content: None,
        keywords: None,
        kwarray: None,
        ext: None,
    }
}

/// Build a minimal `Publisher` object with just a domain.
#[must_use]
pub fn build_publisher(domain: Option<String>) -> Publisher {
    Publisher {
        id: None,
        name: None,
        cattax: None,
        cat: None,
        domain,
        ext: None,
    }
}

#[must_use]
pub fn build_user(id: Option<String>, consent: Option<String>, ext: Option<Object>) -> User {
    User {
        id,
        buyeruid: None,
        yob: None,
        gender: None,
        keywords: None,
        kwarray: None,
        customdata: None,
        geo: None,
        data: None,
        consent,
        eids: None,
        ext,
    }
}

#[must_use]
pub fn build_geo(
    country: Option<String>,
    city: Option<String>,
    region: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    metro: Option<String>,
) -> Geo {
    Geo {
        lat,
        lon,
        r#type: Some(2),
        accuracy: None,
        lastfix: None,
        ipservice: None,
        country,
        region,
        metro,
        city,
        zip: None,
        utcoffset: None,
        ext: None,
    }
}

#[must_use]
pub fn build_device(
    ua: Option<String>,
    ip: Option<String>,
    geo: Option<Geo>,
    dnt: Option<i32>,
    language: Option<String>,
) -> Device {
    Device {
        geo,
        dnt,
        lmt: None,
        ua,
        sua: None,
        ip,
        ipv6: None,
        devicetype: None,
        make: None,
        model: None,
        os: None,
        osv: None,
        hwv: None,
        h: None,
        w: None,
        ppi: None,
        pxratio: None,
        js: None,
        geofetch: None,
        flashver: None,
        language,
        langb: None,
        carrier: None,
        mccmnc: None,
        connectiontype: None,
        ifa: None,
        didsha1: None,
        didmd5: None,
        dpidsha1: None,
        dpidmd5: None,
        macsha1: None,
        macmd5: None,
        ext: None,
    }
}

#[must_use]
pub fn build_regs(gdpr: Option<i32>, us_privacy: Option<String>, ext: Option<Object>) -> Regs {
    Regs {
        coppa: None,
        gdpr,
        us_privacy,
        gpp: None,
        gpp_sid: None,
        ext,
    }
}

/// Parameters for building an `OpenRTB` bid request.
pub struct OpenRtbRequestParams {
    pub id: String,
    pub imp: Vec<Imp>,
    pub site: Option<Site>,
    pub user: Option<User>,
    pub device: Option<Device>,
    pub regs: Option<Regs>,
    pub test: Option<i32>,
    pub tmax: Option<i32>,
    pub cur: Option<Vec<String>>,
    pub ext: Option<Object>,
}

#[must_use]
pub fn build_openrtb_request(params: OpenRtbRequestParams) -> OpenRtbRequest {
    OpenRtbRequest {
        id: params.id,
        imp: params.imp,
        site: params.site,
        app: None,
        dooh: None,
        device: params.device,
        user: params.user,
        test: params.test,
        at: None,
        tmax: params.tmax,
        wseat: None,
        bseat: None,
        allimps: None,
        cur: params.cur,
        wlang: None,
        wlangb: None,
        acat: None,
        bcat: None,
        cattax: None,
        badv: None,
        bapp: None,
        source: None,
        regs: params.regs,
        ext: params.ext,
    }
}

pub struct OpenRtbBidFields {
    pub id: String,
    pub impid: String,
    pub price: f64,
    pub adm: Option<String>,
    pub crid: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub adomain: Option<Vec<String>>,
}

#[must_use]
pub fn build_openrtb_bid(fields: OpenRtbBidFields) -> OpenRtbBid {
    OpenRtbBid {
        id: fields.id,
        impid: fields.impid,
        price: fields.price,
        nurl: None,
        burl: None,
        lurl: None,
        adm: fields.adm,
        adid: None,
        adomain: fields.adomain,
        bundle: None,
        iurl: None,
        cid: None,
        crid: fields.crid,
        tactic: None,
        cattax: None,
        cat: None,
        attr: None,
        apis: None,
        api: None,
        protocol: None,
        qagmediarating: None,
        language: None,
        langb: None,
        dealid: None,
        w: fields.width.map(clamp_u32_to_i32),
        h: fields.height.map(clamp_u32_to_i32),
        wratio: None,
        hratio: None,
        exp: None,
        dur: None,
        mtype: None,
        slotinpod: None,
        ext: None,
    }
}

#[must_use]
pub fn build_seat_bid(seat: Option<String>, bid: Vec<OpenRtbBid>) -> SeatBid {
    SeatBid {
        bid,
        seat,
        group: None,
        ext: None,
    }
}

#[must_use]
pub fn build_openrtb_response(
    id: String,
    seatbid: Vec<SeatBid>,
    ext: Option<Object>,
) -> OpenRtbResponse {
    OpenRtbResponse {
        id,
        seatbid: Some(seatbid),
        bidid: None,
        cur: None,
        customdata: None,
        nbr: None,
        ext,
    }
}

#[derive(Debug, Serialize)]
pub struct UserExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic_fresh: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RequestExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prebid: Option<PrebidExt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trusted_server: Option<TrustedServerExt>,
}

#[derive(Debug, Serialize)]
pub struct PrebidExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub returnallbidstatus: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct TrustedServerExt {
    /// Version of the signing protocol (e.g., "1.1")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_scheme: Option<String>,
    /// Unix timestamp in milliseconds for replay protection
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct ImpExt {
    pub prebid: PrebidImpExt,
}

#[derive(Debug, Serialize)]
pub struct PrebidImpExt {
    pub bidder: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Serialize)]
pub struct ResponseExt {
    pub orchestrator: OrchestratorExt,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auction::types::OrchestratorExt;

    #[test]
    fn openrtb_response_round_trips_through_builder() {
        let bid = build_openrtb_bid(OpenRtbBidFields {
            id: "bidder-a-slot-1".to_string(),
            impid: "slot-1".to_string(),
            price: 1.25,
            adm: Some("<div>Test Creative HTML</div>".to_string()),
            crid: Some("bidder-a-creative".to_string()),
            width: Some(300),
            height: Some(250),
            adomain: Some(vec!["example.com".to_string()]),
        });

        let seatbid = build_seat_bid(Some("bidder-a".to_string()), vec![bid]);

        let ext = maybe_object_from_serializable(&ResponseExt {
            orchestrator: OrchestratorExt {
                strategy: "parallel_only".to_string(),
                providers: 2,
                total_bids: 3,
                time_ms: 12,
                provider_details: vec![],
            },
        });

        let response = build_openrtb_response("auction-1".to_string(), vec![seatbid], ext);

        let serialized = serde_json::to_value(&response).expect("should serialize");
        let expected = serde_json::json!({
            "id": "auction-1",
            "seatbid": [{
                "seat": "bidder-a",
                "bid": [{
                    "id": "bidder-a-slot-1",
                    "impid": "slot-1",
                    "price": 1.25,
                    "adm": "<div>Test Creative HTML</div>",
                    "crid": "bidder-a-creative",
                    "w": 300,
                    "h": 250,
                    "adomain": ["example.com"]
                }]
            }],
            "ext": {
                "orchestrator": {
                    "strategy": "parallel_only",
                    "providers": 2,
                    "total_bids": 3,
                    "time_ms": 12,
                    "provider_details": []
                }
            }
        });

        assert_eq!(serialized, expected);
    }
}
