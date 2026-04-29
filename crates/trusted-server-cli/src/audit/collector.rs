use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CollectedPage {
    pub requested_url: String,
    pub final_url: String,
    pub page_title: Option<String>,
    pub html: String,
    pub script_tags: Vec<CollectedScriptTag>,
    pub network_requests: Vec<CollectedRequest>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CollectedScriptTag {
    pub src: Option<String>,
    pub inline_text: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CollectedRequest {
    pub url: String,
    pub method: String,
    pub resource_type: Option<String>,
    pub status: Option<u16>,
}

impl CollectedPage {
    pub fn requested_url(&self) -> Result<Url, url::ParseError> {
        Url::parse(&self.requested_url)
    }

    pub fn final_url(&self) -> Result<Url, url::ParseError> {
        Url::parse(&self.final_url)
    }
}
