//! Pure expected-slot projection from the runtime creative-opportunity matcher.
//!
//! This module owns path/URL normalization and converts the slots matched by
//! [`match_slots`] into stable, owned [`ExpectedSlot`] records for output and
//! browser-evidence comparison. It must not duplicate glob-matching semantics.

use trusted_server_core::auction::types::MediaType;
use trusted_server_core::creative_opportunities::{CreativeOpportunitiesConfig, match_slots};
use url::Url;

/// The expected slots for a single page path, in configured slot order.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpectedSlots {
    /// The page path the slots were matched against.
    pub path: String,
    /// Matched slots projected into stable records, in configured order.
    pub slots: Vec<ExpectedSlot>,
}

/// A single configured slot expected to appear for a page path.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpectedSlot {
    /// The slot identifier.
    pub id: String,
    /// Resolved HTML `div` element ID (override or the slot id).
    pub div_id: String,
    /// Resolved GAM unit path: the rendered `gam_unit_path` template (or
    /// `/<gam_network_id>/<id>` when the slot has none).
    ///
    /// `None` when a dynamic template renders beyond GAM's unit-path byte limit
    /// for this path's section. Runtime validation rejects such a config, so
    /// this only occurs for a config that would fail to load; the slot is then
    /// reported unconfirmable rather than matched against a wrong path.
    pub gam_unit_path: Option<String>,
    /// Configured ad formats.
    pub formats: Vec<ExpectedFormat>,
    /// Configured provider names, in `aps`, `prebid` order.
    pub providers: Vec<String>,
    /// Configured APS slot ID, when the `aps` provider is set. Used to match
    /// `apstag.fetchBids` evidence; not part of the §8 JSON output.
    pub aps_slot_id: Option<String>,
    /// Glob patterns configured for this slot.
    pub page_patterns: Vec<String>,
}

/// A configured ad format as a stable width/height/media-type record.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpectedFormat {
    /// Creative width in pixels.
    pub width: u32,
    /// Creative height in pixels.
    pub height: u32,
    /// Media type rendered as a stable string (`banner`, `video`, `native`).
    pub media_type: String,
}

/// Projects the slots matching `path` into stable expected-slot records.
///
/// Uses [`match_slots`] so glob semantics stay identical to the runtime, and
/// preserves configured slot order. `path` is assumed already normalized via
/// [`normalize_path_or_url`].
///
/// `gam_unit_path` templates are rendered against the section the runtime would
/// derive from `path` (per the config's `section_root`/`section_segment`
/// policy), so `{section}`-bearing configs project the same unit path the live
/// page requests.
// Shared projection used by the audit verifier; the static commands match slots
// directly against the runtime matcher.
#[must_use]
pub fn expected_slots_for_path(path: &str, config: &CreativeOpportunitiesConfig) -> ExpectedSlots {
    let section = config.section_for_path(path);
    let slots = match_slots(&config.slot, path)
        .into_iter()
        .map(|slot| ExpectedSlot {
            id: slot.id.clone(),
            div_id: slot.resolved_div_id().to_string(),
            gam_unit_path: slot.render_gam_unit_path(&config.gam_network_id, &section),
            formats: slot
                .formats
                .iter()
                .map(|format| ExpectedFormat {
                    width: format.width,
                    height: format.height,
                    media_type: media_type_str(&format.media_type).to_string(),
                })
                .collect(),
            providers: provider_names(slot),
            aps_slot_id: slot.providers.aps.as_ref().map(|aps| aps.slot_id.clone()),
            page_patterns: slot.page_patterns.clone(),
        })
        .collect();

    ExpectedSlots {
        path: path.to_string(),
        slots,
    }
}

fn media_type_str(media_type: &MediaType) -> &'static str {
    match media_type {
        MediaType::Banner => "banner",
        MediaType::Video => "video",
        MediaType::Native => "native",
    }
}

fn provider_names(
    slot: &trusted_server_core::creative_opportunities::CreativeOpportunitySlot,
) -> Vec<String> {
    let mut providers = Vec::new();
    if slot.providers.aps.is_some() {
        providers.push("aps".to_string());
    }
    if slot.providers.prebid.is_some() {
        providers.push("prebid".to_string());
    }
    providers
}

/// Normalizes a page path or full URL into a request path.
///
/// Full `scheme://` inputs are parsed and reduced to their path; bare inputs have
/// query and fragment stripped and a leading `/` ensured. Empty paths become `/`.
///
/// # Errors
///
/// Returns a user-facing string when a `scheme://` input cannot be parsed as a URL.
pub fn normalize_path_or_url(input: &str) -> Result<String, String> {
    if input.contains("://") {
        let url = Url::parse(input).map_err(|err| format!("invalid URL `{input}`: {err}"))?;
        let path = url.path();
        return Ok(if path.is_empty() {
            "/".to_string()
        } else {
            path.to_string()
        });
    }

    let without_fragment = input.split('#').next().unwrap_or(input);
    let path = without_fragment
        .split('?')
        .next()
        .unwrap_or(without_fragment);
    if path.is_empty() {
        Ok("/".to_string())
    } else if path.starts_with('/') {
        Ok(path.to_string())
    } else {
        Ok(format!("/{path}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creative_config_with_slots(patterns: &[&str]) -> CreativeOpportunitiesConfig {
        let page_patterns = patterns
            .iter()
            .map(|pattern| format!("\"{pattern}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let toml = format!(
            "enabled = true\n\
             gam_network_id = \"123\"\n\
             \n\
             [[slot]]\n\
             id = \"atf\"\n\
             gam_unit_path = \"/123/news/atf\"\n\
             div_id = \"ad-atf-\"\n\
             page_patterns = [{page_patterns}]\n\
             formats = [{{ width = 300, height = 250 }}]\n\
             \n\
             [slot.providers.prebid]\n\
             bidders = {{}}\n"
        );
        let mut config = toml::from_str::<CreativeOpportunitiesConfig>(&toml)
            .expect("should deserialize creative opportunities config");
        config.compile_slots();
        config
    }

    #[test]
    fn expected_slots_use_runtime_matcher_and_config_order() {
        let config = creative_config_with_slots(&["/news/*", "/"]);
        let expected = expected_slots_for_path("/news/story", &config);

        assert_eq!(expected.path, "/news/story");
        assert_eq!(
            expected
                .slots
                .iter()
                .map(|slot| slot.id.as_str())
                .collect::<Vec<_>>(),
            ["atf"]
        );
        assert_eq!(expected.slots[0].div_id, "ad-atf-");
        assert_eq!(
            expected.slots[0].gam_unit_path.as_deref(),
            Some("/123/news/atf")
        );
        assert_eq!(expected.slots[0].providers, ["prebid"]);
        assert_eq!(
            expected.slots[0].formats,
            vec![ExpectedFormat {
                width: 300,
                height: 250,
                media_type: "banner".to_string(),
            }]
        );
    }

    #[test]
    fn expected_slots_default_resolution_without_overrides() {
        let toml = "enabled = true\n\
             gam_network_id = \"42\"\n\
             \n\
             [[slot]]\n\
             id = \"footer\"\n\
             page_patterns = [\"/\"]\n\
             formats = [{ width = 728, height = 90 }]\n";
        let mut config =
            toml::from_str::<CreativeOpportunitiesConfig>(toml).expect("should deserialize");
        config.compile_slots();

        let expected = expected_slots_for_path("/", &config);
        assert_eq!(expected.slots[0].div_id, "footer");
        assert_eq!(
            expected.slots[0].gam_unit_path.as_deref(),
            Some("/42/footer")
        );
        assert!(expected.slots[0].providers.is_empty());
    }

    #[test]
    fn expected_slots_render_section_templates_per_path() {
        let toml = "enabled = true\n\
             gam_network_id = \"99999\"\n\
             section_root = \"homepage\"\n\
             \n\
             [[slot]]\n\
             id = \"ad-header-0\"\n\
             gam_unit_path = \"/{network_id}/example/{section}\"\n\
             page_patterns = [\"/\", \"/news\", \"/news/*\"]\n\
             formats = [{ width = 728, height = 90 }]\n";
        let mut config =
            toml::from_str::<CreativeOpportunitiesConfig>(toml).expect("should deserialize");
        config.compile_slots();

        // A path with a section segment renders that segment.
        assert_eq!(
            expected_slots_for_path("/news/story", &config).slots[0]
                .gam_unit_path
                .as_deref(),
            Some("/99999/example/news"),
            "a section template should render the path's section"
        );
        // The site root falls back to the configured section_root.
        assert_eq!(
            expected_slots_for_path("/", &config).slots[0]
                .gam_unit_path
                .as_deref(),
            Some("/99999/example/homepage"),
            "the root path should render section_root"
        );
    }

    #[test]
    fn expected_slots_report_unrenderable_dynamic_template_as_none() {
        // A `{section}` template that renders past GAM's 100-byte unit-path
        // limit. `validate_runtime` rejects this config, so the verifier reports
        // the slot as unconfirmable rather than matching a truncated path.
        let toml = "enabled = true\n\
             gam_network_id = \"99999\"\n\
             section_root = \"homepage\"\n\
             \n\
             [[slot]]\n\
             id = \"ad-header-0\"\n\
             gam_unit_path = \"/{section}/{section}\"\n\
             page_patterns = [\"/*\"]\n\
             formats = [{ width = 728, height = 90 }]\n";
        let mut config =
            toml::from_str::<CreativeOpportunitiesConfig>(toml).expect("should deserialize");
        config.compile_slots();

        let long_path = format!("/{}", "a".repeat(60));
        let expected = expected_slots_for_path(&long_path, &config);

        assert_eq!(
            expected.slots[0].gam_unit_path, None,
            "an over-limit dynamic render should project as None"
        );
    }

    #[test]
    fn normalize_path_or_url_strips_query_and_fragment() {
        assert_eq!(
            normalize_path_or_url("https://www.example.com/news/story?x=1#top")
                .expect("should normalize"),
            "/news/story"
        );
        assert_eq!(
            normalize_path_or_url("news/story?x=1").expect("should normalize"),
            "/news/story"
        );
    }

    #[test]
    fn normalize_path_or_url_roots_empty_input() {
        assert_eq!(
            normalize_path_or_url("https://www.example.com").expect("should normalize"),
            "/"
        );
        assert_eq!(normalize_path_or_url("").expect("should normalize"), "/");
    }
}
