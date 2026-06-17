use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::CliResult;

pub(crate) trait AuditCollector {
    fn collect_page(&self, target_url: &Url) -> CliResult<CollectedPage>;
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CollectedPage {
    pub(crate) requested_url: String,
    pub(crate) final_url: String,
    pub(crate) page_title: Option<String>,
    pub(crate) html: String,
    pub(crate) script_tags: Vec<CollectedScriptTag>,
    pub(crate) network_requests: Vec<CollectedRequest>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CollectedScriptTag {
    pub(crate) src: Option<String>,
    pub(crate) inline_text: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CollectedRequest {
    pub(crate) url: String,
    pub(crate) method: String,
    pub(crate) resource_type: Option<String>,
    pub(crate) status: Option<u16>,
}

impl CollectedPage {
    pub(crate) fn requested_url(&self) -> Result<Url, url::ParseError> {
        Url::parse(&self.requested_url)
    }

    pub(crate) fn final_url(&self) -> Result<Url, url::ParseError> {
        Url::parse(&self.final_url)
    }
}
