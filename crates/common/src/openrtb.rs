use serde::Serialize;
use serde_json::Value;

/// Minimal subset of `OpenRTB` 2.x bid request used by Trusted Server.
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub struct OpenRtbRequest {
    /// Unique ID of the bid request, provided by the exchange.
    pub id: String,
    pub imp: Vec<Imp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub site: Option<Site>,
}

#[derive(Debug, Serialize)]
pub struct Imp {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub banner: Option<Banner>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ext: Option<ImpExt>,
}

#[derive(Debug, Serialize)]
pub struct Banner {
    pub format: Vec<Format>,
}

#[derive(Debug, Serialize)]
pub struct Format {
    pub w: u32,
    pub h: u32,
}

#[derive(Debug, Serialize)]
pub struct Site {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ImpExt {
    pub prebid: PrebidImpExt,
}

#[derive(Debug, Serialize)]
pub struct PrebidImpExt {
    pub bidder: std::collections::HashMap<String, Value>,
}
