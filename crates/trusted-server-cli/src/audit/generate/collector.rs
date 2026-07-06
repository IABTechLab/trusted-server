use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::CliResult;

pub(crate) trait AuditCollector {
    /// Collects a live page. `cookies` are `(name, value)` pairs set on the
    /// browser context before navigation (scoped to `target_url`) so an existing
    /// session — e.g. a valid bot-protection clearance cookie — can carry the
    /// audit past an origin challenge.
    fn collect_page(
        &self,
        target_url: &Url,
        cookies: &[(String, String)],
    ) -> CliResult<CollectedPage>;
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CollectedPage {
    pub(crate) requested_url: String,
    pub(crate) final_url: String,
    pub(crate) page_title: Option<String>,
    pub(crate) html: String,
    pub(crate) script_tags: Vec<CollectedScriptTag>,
    pub(crate) network_requests: Vec<CollectedRequest>,
    /// Slots read from the live GPT registry (`googletag.pubads().getSlots()`).
    ///
    /// Populated at `defineSlot` time, so this captures configured slots even
    /// when the ad request never fires (consent-gated or iframe-issued).
    #[serde(default)]
    pub(crate) gpt_slots: Vec<CollectedGptSlot>,
    pub(crate) warnings: Vec<String>,
}

/// A single slot read from the page's live GPT registry.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CollectedGptSlot {
    /// The GAM ad-unit path (`slot.getAdUnitPath()`).
    pub(crate) gam_unit_path: String,
    /// The slot's div element id (`slot.getSlotElementId()`).
    pub(crate) div_id: String,
    /// Numeric `[width, height]` sizes (`slot.getSizes()`, fluid entries dropped).
    pub(crate) sizes: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CollectedScriptTag {
    pub(crate) src: Option<String>,
    pub(crate) inline_text: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct CollectedRequest {
    pub(crate) url: String,
    pub(crate) resource_type: Option<String>,
}

impl CollectedPage {
    pub(crate) fn requested_url(&self) -> Result<Url, url::ParseError> {
        Url::parse(&self.requested_url)
    }

    pub(crate) fn final_url(&self) -> Result<Url, url::ParseError> {
        Url::parse(&self.final_url)
    }
}
