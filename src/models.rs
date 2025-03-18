use serde::Deserialize;

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AdResponse {
    pub network_id: String,
    pub site_id: String,
    pub page_id: String,
    pub format_id: String,
    pub advertiser_id: String,
    pub campaign_id: String,
    pub insertion_id: String,
    pub creative_id: String,
    pub creative_url: String,
    pub callbacks: Vec<Callback>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Callback {
    #[serde(rename = "type")]
    pub callback_type: String,
    pub url: String,
}
