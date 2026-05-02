//! Creative opportunity slot templates and URL matching.

use std::collections::HashMap;

use glob::Pattern;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auction::types::{AdFormat, AdSlot, MediaType};
use crate::price_bucket::PriceGranularity;

/// Global settings for creative opportunity matching.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreativeOpportunitiesConfig {
    /// GAM network ID used when slot-level unit paths are omitted.
    pub gam_network_id: String,
    /// Auction timeout for server-side template auctions.
    #[serde(default)]
    pub auction_timeout_ms: Option<u32>,
    /// Price granularity used when converting bids to GPT targeting.
    #[serde(default = "PriceGranularity::dense")]
    pub price_granularity: PriceGranularity,
}

/// A URL-matched ad slot template.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreativeOpportunitySlot {
    /// Stable Trusted Server slot ID.
    pub id: String,
    /// Optional full GAM unit path.
    pub gam_unit_path: Option<String>,
    /// Optional HTML div ID for GPT slot definition.
    pub div_id: Option<String>,
    /// URL path patterns that enable this slot.
    pub page_patterns: Vec<String>,
    /// Supported creative formats.
    pub formats: Vec<CreativeOpportunityFormat>,
    /// Optional slot-level floor price.
    pub floor_price: Option<f64>,
    /// Static slot targeting.
    #[serde(default)]
    pub targeting: HashMap<String, String>,
    /// Provider-specific slot params.
    #[serde(default)]
    pub providers: SlotProviders,
}

impl CreativeOpportunitySlot {
    /// Returns whether this slot matches the provided URL path.
    #[must_use]
    pub fn matches_path(&self, path: &str) -> bool {
        self.page_patterns
            .iter()
            .any(|pattern| pattern_matches_path(pattern, path))
    }

    /// Resolve the GAM unit path for this slot.
    #[must_use]
    pub fn resolved_gam_unit_path(&self, config: &CreativeOpportunitiesConfig) -> String {
        self.gam_unit_path
            .clone()
            .unwrap_or_else(|| format!("/{}/{}", config.gam_network_id, self.id))
    }

    /// Resolve the GPT div ID for this slot.
    #[must_use]
    pub fn resolved_div_id(&self) -> String {
        self.div_id.clone().unwrap_or_else(|| self.id.clone())
    }

    /// Convert this template into an auction [`AdSlot`].
    #[must_use]
    pub fn to_ad_slot(&self) -> AdSlot {
        let targeting = self
            .targeting
            .iter()
            .map(|(key, value)| (key.clone(), json!(value)))
            .collect();

        let mut bidders = HashMap::new();
        if let Some(aps) = &self.providers.aps {
            bidders.insert("aps".to_string(), json!({ "slotID": aps.slot_id }));
        }

        AdSlot {
            id: self.id.clone(),
            formats: self
                .formats
                .iter()
                .map(CreativeOpportunityFormat::to_ad_format)
                .collect(),
            floor_price: self.floor_price,
            targeting,
            bidders,
        }
    }
}

/// A creative format supported by a slot.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CreativeOpportunityFormat {
    /// Width in CSS pixels.
    pub width: u32,
    /// Height in CSS pixels.
    pub height: u32,
    /// Media type for this format.
    #[serde(default = "MediaType::banner")]
    pub media_type: MediaType,
}

impl CreativeOpportunityFormat {
    fn to_ad_format(&self) -> AdFormat {
        AdFormat {
            media_type: self.media_type.clone(),
            width: self.width,
            height: self.height,
        }
    }
}

/// Provider-specific params for a slot.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SlotProviders {
    /// APS/TAM slot params.
    pub aps: Option<ApsSlotParams>,
}

/// APS/TAM slot params.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApsSlotParams {
    /// APS slot ID forwarded as `slotID`.
    pub slot_id: String,
}

/// Slot template file.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CreativeOpportunitiesFile {
    /// Slot templates in this file.
    #[serde(rename = "slot", default)]
    pub slots: Vec<CreativeOpportunitySlot>,
}

/// Validate that a slot ID is safe for targeting keys and DOM-derived uses.
#[must_use]
pub fn validate_slot_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

/// Return all slots that match `path`.
#[must_use]
pub fn match_slots<'a>(
    slots: &'a [CreativeOpportunitySlot],
    path: &str,
) -> Vec<&'a CreativeOpportunitySlot> {
    slots
        .iter()
        .filter(|slot| slot.matches_path(path))
        .collect()
}

fn pattern_matches_path(pattern: &str, path: &str) -> bool {
    if pattern == "/" {
        return path == "/";
    }

    compile_url_pattern(pattern).is_some_and(|compiled| compiled.matches(path))
}

fn compile_url_pattern(pattern: &str) -> Option<Pattern> {
    Pattern::new(pattern)
        .or_else(|_| Pattern::new(&pattern.replace("**", "*")))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_slot(id: &str, patterns: Vec<&str>) -> CreativeOpportunitySlot {
        CreativeOpportunitySlot {
            id: id.to_string(),
            gam_unit_path: None,
            div_id: None,
            page_patterns: patterns.into_iter().map(String::from).collect(),
            formats: vec![CreativeOpportunityFormat {
                width: 300,
                height: 250,
                media_type: crate::auction::types::MediaType::Banner,
            }],
            floor_price: Some(0.50),
            targeting: Default::default(),
            providers: Default::default(),
        }
    }

    #[test]
    fn article_pattern_matches_multi_segment_article_paths() {
        let slot = make_slot("atf", vec!["/20**"]);

        assert!(slot.matches_path("/2024/01/my-article"));
        assert!(slot.matches_path("/2026/05/02/story"));
        assert!(!slot.matches_path("/about"));
    }

    #[test]
    fn root_pattern_matches_homepage_only() {
        let slot = make_slot("home", vec!["/"]);

        assert!(slot.matches_path("/"));
        assert!(!slot.matches_path("/about"));
        assert!(!slot.matches_path("/2024/01/my-article"));
    }

    #[test]
    fn slot_ids_allow_alnum_underscore_and_dash_only() {
        assert!(validate_slot_id("atf_sidebar-1"));
        assert!(validate_slot_id("A1_b-2"));
        assert!(!validate_slot_id(""));
        assert!(!validate_slot_id("atf/sidebar"));
        assert!(!validate_slot_id("atf sidebar"));
        assert!(!validate_slot_id("atf.sidebar"));
    }

    #[test]
    fn resolved_gam_unit_path_defaults_to_network_and_slot_id() {
        let config = CreativeOpportunitiesConfig {
            gam_network_id: "21765378893".to_string(),
            auction_timeout_ms: Some(500),
            price_granularity: crate::price_bucket::PriceGranularity::Dense,
        };
        let mut slot = make_slot("atf_sidebar", vec!["/20**"]);

        assert_eq!(
            slot.resolved_gam_unit_path(&config),
            "/21765378893/atf_sidebar"
        );

        slot.gam_unit_path = Some("/21765378893/custom/path".to_string());
        assert_eq!(
            slot.resolved_gam_unit_path(&config),
            "/21765378893/custom/path"
        );
    }

    #[test]
    fn resolved_div_id_defaults_to_slot_id() {
        let mut slot = make_slot("atf_sidebar", vec!["/20**"]);

        assert_eq!(slot.resolved_div_id(), "atf_sidebar");

        slot.div_id = Some("div-atf-sidebar".to_string());
        assert_eq!(slot.resolved_div_id(), "div-atf-sidebar");
    }

    #[test]
    fn to_ad_slot_transfers_formats_floor_targeting_and_aps_slot_id() {
        let mut slot = make_slot("atf_sidebar", vec!["/20**"]);
        slot.targeting.insert("pos".to_string(), "atf".to_string());
        slot.providers.aps = Some(ApsSlotParams {
            slot_id: "aps-atf-sidebar".to_string(),
        });

        let ad_slot = slot.to_ad_slot();

        assert_eq!(ad_slot.id, "atf_sidebar");
        assert_eq!(ad_slot.formats.len(), 1);
        assert_eq!(ad_slot.formats[0].media_type, MediaType::Banner);
        assert_eq!(ad_slot.formats[0].width, 300);
        assert_eq!(ad_slot.formats[0].height, 250);
        assert_eq!(ad_slot.floor_price, Some(0.50));
        assert_eq!(ad_slot.targeting.get("pos"), Some(&json!("atf")));
        assert_eq!(
            ad_slot.bidders.get("aps").and_then(|v| v.get("slotID")),
            Some(&json!("aps-atf-sidebar"))
        );
    }

    #[test]
    fn empty_slot_file_parses_and_produces_zero_matches() {
        let file: CreativeOpportunitiesFile = toml::from_str("").expect("should parse empty file");

        assert!(file.slots.is_empty());
        assert!(match_slots(&file.slots, "/2024/01/my-article").is_empty());
    }
}
