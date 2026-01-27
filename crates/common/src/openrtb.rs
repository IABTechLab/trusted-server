use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Minimal subset of OpenRTB 2.x bid request used by Trusted Server.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct OpenRtbRequest {
    /// Unique ID of the bid request, provided by the exchange.
    pub id: String,
    pub imp: Vec<Imp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<Site>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<User>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regs: Option<Regs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<Device>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<RequestExt>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Imp {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub banner: Option<Banner>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<ImpExt>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Banner {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub format: Vec<Format>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Format {
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Site {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct User {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<UserExt>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct UserExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic_fresh: Option<String>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Regs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<RegsExt>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct RegsExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub us_privacy: Option<String>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Device {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub geo: Option<Geo>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Geo {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub geo_type: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct RequestExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trusted_server: Option<TrustedServerExt>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct TrustedServerExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kid: Option<String>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ImpExt {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prebid: Option<PrebidImpExt>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct PrebidImpExt {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub bidder: HashMap<String, Value>,
}

/// Minimal subset of OpenRTB 2.x bid response used by Trusted Server.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct OpenRtbResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seatbid: Option<Vec<SeatBid>>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct SeatBid {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bid: Option<Vec<Bid>>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Bid {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nurl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub burl: Option<String>,
    #[serde(default, flatten)]
    pub extra: HashMap<String, Value>,
}
