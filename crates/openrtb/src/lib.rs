//! `OpenRTB` 2.6 request and response data model.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub type Object = Map<String, Value>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BidRequest {
    pub id: String,
    pub imp: Vec<Imp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<Site>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<App>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dooh: Option<Dooh>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<Device>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmax: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wseat: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bseat: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allimps: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cur: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wlang: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wlangb: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acat: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bcat: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cattax: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub badv: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bapp: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Source>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regs: Option<Regs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fd: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pchain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schain: Option<SupplyChain>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Regs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coppa: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gdpr: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub us_privacy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpp_sid: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Imp {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric: Option<Vec<Metric>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub banner: Option<Banner>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<Video>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<Audio>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pmp: Option<Pmp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub displaymanager: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub displaymanagerver: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instl: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tagid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bidfloor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bidfloorcur: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clickbrowser: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secure: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iframebuster: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rwdd: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssai: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exp: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qty: Option<Qty>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dt: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh: Option<Refresh>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metric {
    #[serde(rename = "type")]
    pub r#type: String,
    pub value: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Banner {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<Vec<Format>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub w: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub btype: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battr: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pos: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mimes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topframe: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expdir: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vcm: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Video {
    pub mimes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minduration: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maxduration: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rqddurs: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocols: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub w: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startdelay: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placement: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linearity: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipmin: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipafter: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battr: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maxextended: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minbitrate: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maxbitrate: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boxingallowed: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playbackmethod: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pos: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub companionad: Option<Vec<Banner>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub companiontype: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub podid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub podseq: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slotinpod: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maxseq: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poddur: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mincpmpersec: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durfloors: Option<Vec<DurFloors>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Audio {
    pub mimes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minduration: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maxduration: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rqddurs: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocols: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startdelay: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placement: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub linearity: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipmin: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipafter: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battr: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maxextended: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minbitrate: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maxbitrate: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub companionad: Option<Vec<Banner>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub companiontype: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub podid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub podseq: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slotinpod: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maxseq: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poddur: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mincpmpersec: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durfloors: Option<Vec<DurFloors>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Native {
    pub request: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ver: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub battr: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Format {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub w: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wratio: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hratio: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wmin: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pmp {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_auction: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deals: Option<Vec<Deal>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deal {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bidfloor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bidfloorcur: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durfloors: Option<Vec<DurFloors>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wseat: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wadomain: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Site {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cattax: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cat: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sectioncat: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pagecat: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mobile: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub privacypolicy: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<Publisher>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keywords: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kwarray: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct App {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storeurl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cattax: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cat: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sectioncat: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pagecat: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ver: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub privacypolicy: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paid: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<Publisher>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keywords: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kwarray: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dooh {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub venuetype: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub venuetypetax: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publisher: Option<Publisher>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keywords: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Publisher {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cattax: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cat: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Content {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub genre: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keywords: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kwarray: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cattax: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cat: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prodq: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rating: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub userrating: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qagmediarating: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub livestream: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sourcerel: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub len: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub langb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embeddable: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub producer: Option<Producer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<Network>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<Channel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Producer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cattax: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cat: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geo: Option<Geo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dnt: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lmt: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ua: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sua: Option<UserAgent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipv6: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devicetype: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub make: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub osv: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hwv: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub w: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ppi: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pxratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub js: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geofetch: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flashver: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub langb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub carrier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mccmnc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connectiontype: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ifa: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub didsha1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub didmd5: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dpidsha1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dpidmd5: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macsha1: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macmd5: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Geo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub r#type: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accuracy: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lastfix: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipservice: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metro: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub utcoffset: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buyeruid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yob: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gender: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keywords: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kwarray: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub customdata: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geo: Option<Geo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<Data>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eids: Option<Vec<Eid>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Data {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub segment: Option<Vec<Segment>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Network {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupplyChain {
    pub complete: i32,
    pub nodes: Vec<SupplyChainNode>,
    pub ver: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupplyChainNode {
    pub asi: String,
    pub sid: String,
    pub hp: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Eid {
    pub source: String,
    pub uids: Vec<Uid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Uid {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atype: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAgent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browsers: Option<Vec<BrandVersion>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<BrandVersion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mobile: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub architecture: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrandVersion {
    pub brand: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Qty {
    pub multiplier: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sourcetype: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Refresh {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refsettings: Option<Vec<RefSettings>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reftype: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minint: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurFloors {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mindur: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maxdur: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bidfloor: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BidResponse {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seatbid: Option<Vec<SeatBid>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bidid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cur: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub customdata: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nbr: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeatBid {
    pub bid: Vec<Bid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seat: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bid {
    pub id: String,
    pub impid: String,
    pub price: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nurl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub burl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lurl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adomain: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iurl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tactic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cattax: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cat: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attr: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apis: Option<Vec<i32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qagmediarating: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub langb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dealid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub w: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub h: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wratio: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hratio: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exp: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dur: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtype: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slotinpod: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<Object>,
}

#[cfg(test)]
mod tests {
    use super::BidRequest;
    use serde_json::json;

    #[test]
    fn preserves_openrtb_26_privacy_dooh_and_refresh_fields() {
        let payload = json!({
            "id": "request-1",
            "imp": [
                {
                    "id": "imp-1",
                    "banner": {
                        "w": 300,
                        "h": 250
                    },
                    "qty": {
                        "multiplier": 14.2,
                        "sourcetype": 1,
                        "vendor": "measurement.example"
                    },
                    "dt": 1735689600000.0,
                    "refresh": {
                        "refsettings": [
                            {
                                "reftype": 1,
                                "minint": 30
                            }
                        ],
                        "count": 2
                    },
                    "video": {
                        "mimes": ["video/mp4"],
                        "durfloors": [
                            {
                                "mindur": 1,
                                "bidfloor": 5.0
                            }
                        ]
                    }
                }
            ],
            "dooh": {
                "id": "screen-group-1",
                "venuetype": ["retail"],
                "venuetypetax": 1,
                "domain": "inventory.example"
            },
            "regs": {
                "gpp": "DBABMA~CPXxRfAPXxRfAAfKABENB-CgAAAAAAAAAAYgAAAAAAAA",
                "gpp_sid": [7],
                "gdpr": 1
            },
            "acat": ["IAB1"]
        });

        let bid_request: BidRequest =
            serde_json::from_value(payload.clone()).expect("should deserialize OpenRTB 2.6 fields");
        let serialized =
            serde_json::to_value(&bid_request).expect("should serialize OpenRTB 2.6 fields");

        assert_eq!(
            serialized["regs"]["gpp"], payload["regs"]["gpp"],
            "should preserve regs.gpp"
        );
        assert_eq!(
            serialized["regs"]["gpp_sid"], payload["regs"]["gpp_sid"],
            "should preserve regs.gpp_sid"
        );
        assert_eq!(
            serialized["acat"], payload["acat"],
            "should preserve bidrequest.acat"
        );
        assert_eq!(
            serialized["dooh"], payload["dooh"],
            "should preserve bidrequest.dooh"
        );
        assert_eq!(
            serialized["imp"][0]["qty"], payload["imp"][0]["qty"],
            "should preserve imp.qty"
        );
        assert_eq!(
            serialized["imp"][0]["dt"], payload["imp"][0]["dt"],
            "should preserve imp.dt"
        );
        assert_eq!(
            serialized["imp"][0]["refresh"], payload["imp"][0]["refresh"],
            "should preserve imp.refresh"
        );
        assert_eq!(
            serialized["imp"][0]["video"]["durfloors"], payload["imp"][0]["video"]["durfloors"],
            "should preserve video.durfloors"
        );
    }
}
