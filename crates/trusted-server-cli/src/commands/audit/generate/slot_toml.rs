//! TOML-side slot config: the [`RenderSlot`] model, run merging, rendering,
//! and in-place `[creative_opportunities]` splicing for `ts audit ad-templates
//! generate`.

use std::collections::{BTreeMap, BTreeSet};

use toml_edit::{DocumentMut, Item};
use trusted_server_core::auction::types::MediaType;
use trusted_server_core::creative_opportunities::{
    CreativeOpportunitiesConfig, CreativeOpportunitySlot,
};

#[cfg(test)]
use crate::commands::audit::generate::gpt_slots;
use crate::error::{CliResult, cli_error, report_error};

/// A slot ready to render — the union of discovered and existing fields, without
/// the core type's `pub(crate)` compiled-pattern cache.
#[derive(Debug, Clone)]
pub(super) struct RenderSlot {
    id: String,
    div_id: Option<String>,
    gam_unit_path: Option<String>,
    page_patterns: Vec<String>,
    /// `(width, height, non-banner media type)`.
    formats: Vec<(u32, u32, Option<&'static str>)>,
    floor_price: Option<f64>,
    targeting: BTreeMap<String, String>,
    aps_slot_id: Option<String>,
    /// `Some` when the slot runs Prebid; the map is per-bidder params (often empty).
    prebid_bidders: Option<BTreeMap<String, serde_json::Value>>,
}

impl RenderSlot {
    /// The stable exact identity fallback used when no configured div prefix
    /// matches a discovered slot.
    fn key(&self) -> String {
        self.div_id
            .as_deref()
            .unwrap_or(&self.id)
            .trim_end_matches('-')
            .to_string()
    }

    /// Builds a slot from one page's discovery.
    ///
    /// Superseded in production by [`RenderSlot::from_evidence`], which reads
    /// cross-page evidence; retained as test scaffolding for the merge cases.
    #[cfg(test)]
    fn from_discovered(slot: &gpt_slots::DiscoveredSlot, patterns: &[String]) -> Self {
        Self {
            id: slot.id.clone(),
            div_id: Some(slot.div_id.clone()),
            gam_unit_path: Some(slot.gam_unit_path.clone()),
            page_patterns: patterns.to_vec(),
            formats: slot
                .formats
                .iter()
                .map(|&(width, height)| (width, height, None))
                .collect(),
            floor_price: None,
            targeting: BTreeMap::new(),
            aps_slot_id: None,
            prebid_bidders: slot.has_prebid.then(BTreeMap::new),
        }
    }

    /// Builds a slot from cross-page evidence and the inferred unit path.
    ///
    /// `gam_unit_path` is `None` when inference refused to represent the slot;
    /// the slot is still written so its div and formats are not lost, and the
    /// runtime falls back to the default `/<network_id>/<id>` path.
    pub(super) fn from_evidence(
        id: &str,
        div_id: &str,
        gam_unit_path: Option<String>,
        formats: impl IntoIterator<Item = (u32, u32)>,
        page_patterns: Vec<String>,
        has_prebid: bool,
    ) -> Self {
        Self {
            id: id.to_string(),
            div_id: Some(div_id.to_string()),
            gam_unit_path,
            page_patterns,
            formats: formats
                .into_iter()
                .map(|(width, height)| (width, height, None))
                .collect(),
            floor_price: None,
            targeting: BTreeMap::new(),
            aps_slot_id: None,
            prebid_bidders: has_prebid.then(BTreeMap::new),
        }
    }

    fn from_existing(slot: &CreativeOpportunitySlot) -> Self {
        Self {
            id: slot.id.clone(),
            div_id: slot.div_id.clone(),
            gam_unit_path: slot.gam_unit_path.clone(),
            page_patterns: slot.page_patterns.clone(),
            formats: slot
                .formats
                .iter()
                .map(|format| {
                    (
                        format.width,
                        format.height,
                        media_type_label(&format.media_type),
                    )
                })
                .collect(),
            floor_price: slot.floor_price,
            targeting: slot
                .targeting
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            aps_slot_id: slot.providers.aps.as_ref().map(|aps| aps.slot_id.clone()),
            prebid_bidders: slot.providers.prebid.as_ref().map(|prebid| {
                prebid
                    .bidders
                    .iter()
                    .map(|(name, params)| (name.clone(), params.clone()))
                    .collect()
            }),
        }
    }
}

/// The non-default (non-banner) media-type label to emit, or `None` for banner.
fn media_type_label(media_type: &MediaType) -> Option<&'static str> {
    match media_type {
        MediaType::Banner => None,
        MediaType::Video => Some("video"),
        MediaType::Native => Some("native"),
    }
}

/// Merges discovered slots into the existing slot set, keyed by [`RenderSlot::key`].
///
/// - `--replace` (or no existing slots): the result is exactly the discovered set.
/// - Otherwise existing slots are preserved (covering other pages / hand-tuned
///   fields); a slot re-seen this run has its page patterns and formats unioned;
///   slots seen only this run are appended.
#[cfg(test)]
pub(super) fn merge_slots(
    existing: Option<&CreativeOpportunitiesConfig>,
    discovered: &gpt_slots::DiscoveredSlots,
    run_patterns: &[String],
    replace: bool,
) -> Vec<RenderSlot> {
    let discovered_slots: Vec<RenderSlot> = discovered
        .slots
        .iter()
        .map(|slot| RenderSlot::from_discovered(slot, run_patterns))
        .collect();
    merge_render_slots(existing, discovered_slots, replace)
}

/// Merges already-built slots into the existing set.
///
/// Same reconciliation as [`merge_slots`], but the caller supplies the slots —
/// the crawl path builds them from cross-page evidence rather than from one
/// page's discoveries. A slot re-seen this run keeps its configured fields and
/// gains this run's patterns; a genuinely new slot is appended with a
/// non-colliding id.
#[cfg(test)]
pub(super) fn merge_render_slots(
    existing: Option<&CreativeOpportunitiesConfig>,
    discovered_slots: Vec<RenderSlot>,
    replace: bool,
) -> Vec<RenderSlot> {
    merge_render_slots_with_diagnostics(existing, discovered_slots, replace).0
}

/// Merges slots and reports configured prefixes that claimed several live divs.
pub(super) fn merge_render_slots_with_diagnostics(
    existing: Option<&CreativeOpportunitiesConfig>,
    discovered_slots: Vec<RenderSlot>,
    replace: bool,
) -> (Vec<RenderSlot>, Vec<String>) {
    let existing_slots = existing.map(|config| config.slot.as_slice()).unwrap_or(&[]);
    if replace || existing_slots.is_empty() {
        return (discovered_slots, Vec::new());
    }

    let mut merged: Vec<RenderSlot> = existing_slots
        .iter()
        .map(RenderSlot::from_existing)
        .collect();
    let mut prefix_claims: BTreeMap<usize, BTreeSet<String>> = BTreeMap::new();
    for mut slot in discovered_slots {
        if let Some(index) = matching_slot_index(&merged, &slot) {
            if index < existing_slots.len()
                && let (Some(prefix), Some(discovered_div)) =
                    (merged[index].div_id.as_deref(), slot.div_id.as_deref())
                && discovered_div.starts_with(prefix)
            {
                prefix_claims
                    .entry(index)
                    .or_default()
                    .insert(discovered_div.to_string());
            }
            let present = &mut merged[index];
            for pattern in &slot.page_patterns {
                if !present.page_patterns.contains(pattern) {
                    present.page_patterns.push(pattern.clone());
                }
            }
            for format in &slot.formats {
                if !present.formats.contains(format) {
                    present.formats.push(*format);
                }
            }
        } else {
            slot.id = unique_slot_id(&slot.id, &merged);
            merged.push(slot);
        }
    }
    let diagnostics = prefix_claims
        .into_iter()
        .filter(|(_, divs)| divs.len() > 1)
        .map(|(index, divs)| {
            let slot = &merged[index];
            let sample = divs.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
            let remainder = divs.len().saturating_sub(5);
            let suffix = if remainder == 0 {
                String::new()
            } else {
                format!(", and {remainder} more")
            };
            format!(
                "configured slot `{}` with div_id prefix `{}` matched {} discovered divs \
                 ({sample}{suffix}); review whether they are distinct placements",
                slot.id,
                slot.div_id.as_deref().unwrap_or_default(),
                divs.len(),
            )
        })
        .collect();
    (merged, diagnostics)
}

fn unique_slot_id(candidate: &str, existing: &[RenderSlot]) -> String {
    if existing.iter().all(|slot| slot.id != candidate) {
        return candidate.to_string();
    }

    let mut suffix = 2_usize;
    loop {
        let unique = format!("{candidate}-{suffix}");
        if existing.iter().all(|slot| slot.id != unique) {
            return unique;
        }
        suffix += 1;
    }
}

/// Finds the most specific configured slot matching a discovered live div.
///
/// Configured `div_id` values are runtime prefixes. Exact matches naturally
/// win because they are the longest possible prefix; equal-length ties retain
/// config order. The prior exact key behavior remains as a fallback.
fn matching_slot_index(existing: &[RenderSlot], discovered: &RenderSlot) -> Option<usize> {
    if let Some(discovered_div) = discovered.div_id.as_deref() {
        let mut best = None;
        let mut best_length = 0;
        for (index, slot) in existing.iter().enumerate() {
            let Some(prefix) = slot.div_id.as_deref().filter(|prefix| !prefix.is_empty()) else {
                continue;
            };
            if discovered_div.starts_with(prefix) && prefix.len() > best_length {
                best = Some(index);
                best_length = prefix.len();
            }
        }
        if best.is_some() {
            return best;
        }
    }

    let key = discovered.key();
    existing.iter().position(|slot| slot.key() == key)
}

/// Header comment emitted above the managed slot array. Stripped from the
/// preserved scalar block on re-splice (see [`is_managed_comment_line`]) so
/// repeated `generate` runs don't accumulate duplicate copies.
const MANAGED_SLOTS_COMMENT: &str = "# Slots managed by `ts audit ad-templates generate`.";
/// Second line of the managed-slot header comment.
const MANAGED_SLOTS_REVIEW_COMMENT: &str =
    "# Review page_patterns and formats before validating/pushing.";

/// Renders merged slots as compact `[[creative_opportunities.slot]]` TOML blocks.
pub(super) fn render_slots(slots: &[RenderSlot]) -> String {
    let mut out = format!("\n{MANAGED_SLOTS_COMMENT}\n{MANAGED_SLOTS_REVIEW_COMMENT}\n");
    for slot in slots {
        out.push_str("\n[[creative_opportunities.slot]]\n");
        out.push_str(&format!("id = {}\n", toml_string(&slot.id)));
        if let Some(div_id) = &slot.div_id {
            out.push_str(&format!("div_id = {}\n", toml_string(div_id)));
        }
        if let Some(path) = &slot.gam_unit_path {
            out.push_str(&format!("gam_unit_path = {}\n", toml_string(path)));
        }
        let patterns = slot
            .page_patterns
            .iter()
            .map(|pattern| toml_string(pattern))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("page_patterns = [{patterns}]\n"));
        let formats = slot
            .formats
            .iter()
            .map(|(width, height, media_type)| match media_type {
                Some(kind) => {
                    format!("{{ width = {width}, height = {height}, media_type = \"{kind}\" }}")
                }
                None => format!("{{ width = {width}, height = {height} }}"),
            })
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!("formats = [{formats}]\n"));
        if let Some(floor) = slot.floor_price {
            // `f64` Display prints `NaN`, which is not valid TOML (`nan` is);
            // normalize non-finite values so the spliced config stays parseable.
            if floor.is_finite() {
                out.push_str(&format!("floor_price = {floor}\n"));
            } else if floor.is_nan() {
                out.push_str("floor_price = nan\n");
            } else if floor.is_sign_positive() {
                out.push_str("floor_price = inf\n");
            } else {
                out.push_str("floor_price = -inf\n");
            }
        }
        if !slot.targeting.is_empty() {
            let pairs = slot
                .targeting
                .iter()
                .map(|(key, value)| format!("{} = {}", toml_key(key), toml_string(value)))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("targeting = {{ {pairs} }}\n"));
        }
        if let Some(slot_id) = &slot.aps_slot_id {
            out.push_str("[creative_opportunities.slot.providers.aps]\n");
            out.push_str(&format!("slot_id = {}\n", toml_string(slot_id)));
        }
        if let Some(bidders) = &slot.prebid_bidders {
            out.push_str("[creative_opportunities.slot.providers.prebid]\n");
            let rendered = bidders
                .iter()
                .map(|(name, params)| format!("{} = {}", toml_key(name), toml_inline_value(params)))
                .collect::<Vec<_>>()
                .join(", ");
            if rendered.is_empty() {
                out.push_str("bidders = {}\n");
            } else {
                out.push_str(&format!("bidders = {{ {rendered} }}\n"));
            }
        }
    }
    out
}

/// Quotes and escapes a string as a TOML basic string, including control chars.
pub(super) fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // TOML basic strings reject U+0000..U+001F and DEL (U+007F).
            control if (control as u32) < 0x20 || control == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", control as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Renders a TOML table key: bare when it is a valid bare key, else a quoted key.
fn toml_key(key: &str) -> String {
    let is_bare = !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
    if is_bare {
        key.to_string()
    } else {
        toml_string(key)
    }
}

/// Renders a JSON value as a compact inline TOML value (for prebid bidder params).
fn toml_inline_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "{}".to_string(),
        serde_json::Value::Bool(bool) => bool.to_string(),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::String(string) => toml_string(string),
        serde_json::Value::Array(items) => {
            let rendered = items
                .iter()
                .map(toml_inline_value)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{rendered}]")
        }
        serde_json::Value::Object(map) => {
            let rendered = map
                .iter()
                .map(|(key, value)| format!("{} = {}", toml_key(key), toml_inline_value(value)))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{ {rendered} }}")
        }
    }
}

/// The config-level values a splice writes alongside the slot array.
#[derive(Debug, Clone, Default)]
pub(super) struct CreativeSectionKeys<'a> {
    /// GAM network id, when one was resolved.
    pub(super) network_id: Option<&'a str>,
    /// `section_root`, written only when a slot uses a `{section}` template.
    pub(super) section_root: Option<&'a str>,
    /// `section_segment`, written only alongside `section_root`.
    pub(super) section_segment: Option<usize>,
}

/// Structurally replaces the generator-managed creative-opportunities fields.
///
/// All unrelated TOML items and their decorations remain in the parsed
/// document. Missing inferred scalar values preserve their existing values; a
/// fresh section is created only when a network id is available.
pub(super) fn splice_creative_slots(
    existing: &str,
    keys: &CreativeSectionKeys<'_>,
    rendered_slots: &str,
) -> CliResult<String> {
    let mut document = existing.parse::<DocumentMut>().map_err(|error| {
        report_error(format!(
            "failed to parse target config before updating slots: {error}"
        ))
    })?;
    let had_section = document.get("creative_opportunities").is_some();
    if !had_section && keys.network_id.is_none() {
        return cli_error(
            "refusing to create a `[creative_opportunities]` section without a \
             GAM network id: none could be determined from the audited page, and \
             the key is required. Add `[creative_opportunities]` with a \
             `gam_network_id` to the config and re-run",
        );
    }

    let generated = format!(
        "[creative_opportunities]\n{}\n",
        rendered_slots.trim_matches('\n')
    );
    let mut generated = generated
        .parse::<DocumentMut>()
        .map_err(|error| report_error(format!("failed to parse generated slot tables: {error}")))?;
    let generated_slots = generated["creative_opportunities"]
        .as_table_mut()
        .and_then(|table| table.remove("slot"))
        .unwrap_or_else(|| Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));

    if !had_section {
        document["creative_opportunities"] = Item::Table(toml_edit::Table::new());
    }
    let creative = document["creative_opportunities"]
        .as_table_mut()
        .ok_or_else(|| {
            report_error(
                "target config's `creative_opportunities` value is not an editable table; \
                 rewrite it as a `[creative_opportunities]` table and re-run",
            )
        })?;
    if let Some(network_id) = keys.network_id {
        creative["gam_network_id"] = toml_edit::value(network_id);
    }
    if let Some(section_root) = keys.section_root {
        creative["section_root"] = toml_edit::value(section_root);
        if let Some(section_segment) = keys.section_segment {
            creative["section_segment"] = toml_edit::value(section_segment as i64);
        }
    }
    creative.insert("slot", generated_slots);

    let mut result = document.to_string();
    if uses_crlf(existing) {
        result = result.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    ensure_only_managed_fields_changed(existing, &result)?;
    Ok(result)
}

/// Verifies that the structural update changed only generator-managed fields.
fn ensure_only_managed_fields_changed(before: &str, after: &str) -> CliResult<()> {
    fn unmanaged(document: &str) -> CliResult<toml::Value> {
        let mut value = toml::from_str::<toml::Value>(document)
            .map_err(|error| report_error(format!("failed to validate updated config: {error}")))?;
        if let Some(root) = value.as_table_mut() {
            let remove_empty = if let Some(creative) = root
                .get_mut("creative_opportunities")
                .and_then(toml::Value::as_table_mut)
            {
                for key in ["slot", "gam_network_id", "section_root", "section_segment"] {
                    creative.remove(key);
                }
                creative.is_empty()
            } else {
                false
            };
            if remove_empty {
                root.remove("creative_opportunities");
            }
        }
        Ok(value)
    }

    if unmanaged(before)? != unmanaged(after)? {
        return cli_error(
            "refusing to update config because fields outside the managed \
             creative-opportunities keys would change",
        );
    }
    Ok(())
}

/// Whether `document` uses CRLF line endings (so edits preserve them).
fn uses_crlf(document: &str) -> bool {
    document.contains("\r\n")
}

/// Strips a trailing inline `# comment` from a candidate table-header line.
///
/// Only valid on header candidates: header lines cannot contain `#` before the
/// closing bracket unless it is inside a quoted key, which the configs this
/// updater manages never use.
fn strip_inline_comment(line: &str) -> &str {
    match line.find('#') {
        Some(position) => line[..position].trim_end(),
        None => line,
    }
}

/// Whether `line` is exactly the `section_header` table header (for example
/// `[creative_opportunities]`), tolerating surrounding whitespace and a
/// trailing inline `# comment` — both valid TOML.
#[cfg(test)]
fn is_table_header(line: &str, section_header: &str) -> bool {
    strip_inline_comment(line.trim()) == section_header
}

pub(super) fn replace_key_in_section(
    document: &str,
    section: &str,
    key: &str,
    replacement_line: &str,
) -> CliResult<String> {
    let section_header = format!("[{section}]");
    let mut in_section = false;
    let mut replaced = false;
    let mut saw_section = false;
    let mut lines = Vec::new();

    for line in document.lines() {
        let trimmed = line.trim();
        let header_candidate = strip_inline_comment(trimmed);
        if header_candidate.starts_with('[') && header_candidate.ends_with(']') {
            in_section = header_candidate == section_header;
            saw_section |= in_section;
        }

        if in_section && !replaced && is_key_line(trimmed, key) {
            lines.push(replacement_line.to_string());
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }

    if !saw_section {
        return cli_error(format!(
            "failed to update starter config because section `{section_header}` was not found"
        ));
    }
    if !replaced {
        return cli_error(format!(
            "failed to update starter config because key `{key}` was not found in `{section_header}`"
        ));
    }

    let mut output = lines.join("\n");
    if document.ends_with('\n') {
        output.push('\n');
    }
    if uses_crlf(document) {
        // `lines()` stripped the `\r`s; restore the document's CRLF endings.
        output = output.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    Ok(output)
}

/// Sets `key` in `section`, replacing an existing assignment or inserting one.
///
/// [`replace_key_in_section`] can only rewrite a key that is already present, so
/// it cannot add `section_root` or `section_segment` to a config that predates
/// them — which is every config a first templated run touches. This inserts
/// immediately after the section header instead, keeping the new key inside the
/// section's scalar block rather than stranding it after a subtable, where TOML
/// would read it as belonging to that subtable.
///
/// # Errors
///
/// Returns an error when `section` is not present in the document.
#[cfg(test)]
pub(super) fn upsert_key_in_section(
    document: &str,
    section: &str,
    key: &str,
    replacement_line: &str,
) -> CliResult<String> {
    if let Ok(replaced) = replace_key_in_section(document, section, key, replacement_line) {
        return Ok(replaced);
    }

    let section_header = format!("[{section}]");
    let Some(header_index) = document
        .lines()
        .position(|line| is_table_header(line, &section_header))
    else {
        return cli_error(format!(
            "failed to update config because section `{section_header}` was not found"
        ));
    };

    let mut lines: Vec<String> = document.lines().map(str::to_string).collect();
    lines.insert(header_index + 1, replacement_line.to_string());

    let mut output = lines.join("\n");
    if document.ends_with('\n') {
        output.push('\n');
    }
    if uses_crlf(document) {
        output = output.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    Ok(output)
}

fn is_key_line(trimmed_line: &str, key: &str) -> bool {
    trimmed_line
        .strip_prefix(key)
        .and_then(|remaining| remaining.trim_start().strip_prefix('='))
        .is_some()
}

/// Chooses the `gam_network_id` to write.
///
/// The existing id is kept only when a real merge preserves existing slots.
/// On `--replace`, or when the config had no slots (e.g. a placeholder
/// `[creative_opportunities]` section), the discovered id wins — mirroring
/// [`merge_slots`], which returns discovered-only in those cases.
pub(super) fn resolve_network_id(
    existing: Option<&CreativeOpportunitiesConfig>,
    discovered_network_id: Option<&str>,
    replace: bool,
) -> Option<String> {
    let existing_network_id = existing.map(|config| config.gam_network_id.clone());
    let preserving_existing = !replace && existing.is_some_and(|config| !config.slot.is_empty());
    if preserving_existing {
        existing_network_id.or_else(|| discovered_network_id.map(str::to_string))
    } else {
        discovered_network_id
            .map(str::to_string)
            .or(existing_network_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::audit::generate::collector;

    fn discovered_header_slot() -> gpt_slots::DiscoveredSlots {
        let registry = vec![collector::CollectedGptSlot {
            gam_unit_path: "/222/homepage/header".to_string(),
            div_id: "div-gpt-ad-header".to_string(),
            sizes: vec![(728, 90)],
        }];
        gpt_slots::discover_gpt_slots(&registry, &[], false)
    }

    /// Rendered slot text for the discovered header slot, patterns = `/`.
    fn header_rendered() -> String {
        let merged = merge_slots(None, &discovered_header_slot(), &["/".to_string()], true);
        render_slots(&merged)
    }

    /// Section keys carrying only a network id, the common test case.
    fn network_keys(network_id: &str) -> CreativeSectionKeys<'_> {
        CreativeSectionKeys {
            network_id: Some(network_id),
            ..CreativeSectionKeys::default()
        }
    }

    fn existing_config(toml_str: &str) -> CreativeOpportunitiesConfig {
        toml::from_str::<CreativeOpportunitiesConfig>(toml_str).expect("valid creative config")
    }

    #[test]
    fn splice_replaces_slots_and_preserves_other_sections() {
        let existing = "[publisher]\ndomain = \"x\"\n\n\
             [creative_opportunities]\ngam_network_id = \"111\"\nprice_granularity = \"dense\"\n\n\
             [[creative_opportunities.slot]]\nid = \"old\"\ndiv_id = \"old\"\n\
             gam_unit_path = \"/111/old\"\npage_patterns = [\"/\"]\n\
             formats = [{ width = 300, height = 250 }]\n\n\
             [auction]\nenabled = true\n";

        let out = splice_creative_slots(existing, &network_keys("222"), &header_rendered())
            .expect("should splice");

        assert!(
            out.contains("gam_network_id = \"222\""),
            "network id updated"
        );
        assert!(!out.contains("id = \"old\""), "old slot removed");
        assert!(
            out.contains("gam_unit_path = \"/222/homepage/header\""),
            "new slot written"
        );
        assert!(
            out.contains("[publisher]") && out.contains("domain = \"x\""),
            "publisher section preserved"
        );
        assert!(
            out.contains("[auction]") && out.contains("enabled = true"),
            "trailing auction section preserved"
        );
        toml::from_str::<toml::Value>(&out).expect("spliced config is valid TOML");
    }

    #[test]
    fn splice_updates_a_quoted_section_header_structurally() {
        let existing = "[\"creative_opportunities\"]\ngam_network_id = \"111\"\n";

        let updated = splice_creative_slots(existing, &network_keys("222"), &header_rendered())
            .expect("should update quoted table structurally");

        assert_eq!(updated.matches("creative_opportunities").count(), 2);
        assert!(updated.contains("gam_network_id = \"222\""));
        toml::from_str::<toml::Value>(&updated).expect("should remain valid TOML");
    }

    #[test]
    fn splice_preserves_multiline_values_comments_and_noncontiguous_tables() {
        let existing = "title = \"publisher\" # keep this comment\n\
            description = \"\"\"a line that looks like [creative_opportunities]\n\
            and another [[creative_opportunities.slot]] line\"\"\"\n\
            dimensions = [\n  300,\n  250,\n]\n\n\
            [creative_opportunities] # managed section\n\
            gam_network_id = \"111\" # old network\n\n\
            [[creative_opportunities.slot]]\nid = \"old-a\"\ndiv_id = \"old-a\"\n\
            gam_unit_path = \"/111/a\"\npage_patterns = [\"/\"]\n\
            formats = [{ width = 300, height = 250 }]\n\n\
            [auction]\nenabled = true # keep auction comment\n\n\
            [[creative_opportunities.slot]]\nid = \"old-b\"\ndiv_id = \"old-b\"\n\
            gam_unit_path = \"/111/b\"\npage_patterns = [\"/b\"]\n\
            formats = [{ width = 320, height = 50 }]\n";

        let updated = splice_creative_slots(existing, &network_keys("222"), &header_rendered())
            .expect("should update structurally");

        assert!(updated.contains("looks like [creative_opportunities]"));
        assert!(updated.contains("dimensions = [\n  300,\n  250,\n]"));
        assert!(updated.contains("enabled = true # keep auction comment"));
        assert!(!updated.contains("id = \"old-a\""));
        assert!(!updated.contains("id = \"old-b\""));
        let value = toml::from_str::<toml::Value>(&updated).expect("should remain valid TOML");
        assert_eq!(
            value["creative_opportunities"]["slot"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn splice_rejects_top_level_inline_creative_opportunities_table() {
        let existing = "creative_opportunities = { gam_network_id = \"111\" }\n";

        let error = splice_creative_slots(existing, &network_keys("222"), &header_rendered())
            .expect_err("should refuse a top-level inline table");

        assert!(
            format!("{error:?}").contains("rewrite it as"),
            "error should tell the operator to rewrite the section, got {error:?}"
        );
    }

    /// Section keys for a templated run: network id plus the section policy.
    fn template_keys<'a>(
        network_id: &'a str,
        root: &'a str,
        segment: usize,
    ) -> CreativeSectionKeys<'a> {
        CreativeSectionKeys {
            network_id: Some(network_id),
            section_root: Some(root),
            section_segment: Some(segment),
        }
    }

    #[test]
    fn splice_inserts_section_policy_keys_a_config_does_not_have_yet() {
        // The whole point of `upsert`: every config predating templating lacks
        // these keys, so a replace-only writer could never add them.
        let existing = "[creative_opportunities]\ngam_network_id = \"111\"\n\n\
             [auction]\nenabled = true\n";

        let out = splice_creative_slots(
            existing,
            &template_keys("222", "homepage", 0),
            &header_rendered(),
        )
        .expect("should splice");

        let value = toml::from_str::<toml::Value>(&out).expect("spliced config is valid TOML");
        let creative = &value["creative_opportunities"];
        assert_eq!(creative["gam_network_id"].as_str(), Some("222"));
        assert_eq!(creative["section_root"].as_str(), Some("homepage"));
        assert_eq!(creative["section_segment"].as_integer(), Some(0));
        assert_eq!(
            value["auction"]["enabled"].as_bool(),
            Some(true),
            "inserting must not disturb later sections"
        );
    }

    #[test]
    fn splice_replaces_section_policy_keys_that_are_already_present() {
        let existing = "[creative_opportunities]\ngam_network_id = \"111\"\n\
             section_root = \"old\"\nsection_segment = 2\n";

        let out = splice_creative_slots(
            existing,
            &template_keys("111", "homepage", 1),
            &header_rendered(),
        )
        .expect("should splice");

        let value = toml::from_str::<toml::Value>(&out).expect("valid TOML");
        let creative = &value["creative_opportunities"];
        assert_eq!(creative["section_root"].as_str(), Some("homepage"));
        assert_eq!(creative["section_segment"].as_integer(), Some(1));
        assert_eq!(
            out.matches("section_root").count(),
            1,
            "the key must be replaced, not duplicated"
        );
    }

    #[test]
    fn splice_omits_section_policy_when_no_slot_needs_it() {
        // `section_root`/`section_segment` are `deny_unknown_fields` additions:
        // writing them into a config that does not need them would make it
        // unloadable by an older binary for no benefit.
        let existing = "[creative_opportunities]\ngam_network_id = \"111\"\n";

        let out = splice_creative_slots(existing, &network_keys("222"), &header_rendered())
            .expect("should splice");

        assert!(
            !out.contains("section_root") && !out.contains("section_segment"),
            "an untemplated run must not add rollback-fatal keys, got:\n{out}"
        );
    }

    #[test]
    fn splice_writes_section_policy_into_a_freshly_created_section() {
        let existing = "[publisher]\ndomain = \"x\"\n";

        let out = splice_creative_slots(
            existing,
            &template_keys("222", "homepage", 0),
            &header_rendered(),
        )
        .expect("should append a fresh section");

        let value = toml::from_str::<toml::Value>(&out).expect("valid TOML");
        let creative = &value["creative_opportunities"];
        assert_eq!(creative["gam_network_id"].as_str(), Some("222"));
        assert_eq!(creative["section_root"].as_str(), Some("homepage"));
        assert_eq!(creative["section_segment"].as_integer(), Some(0));
    }

    #[test]
    fn upsert_keeps_an_inserted_key_inside_the_section_scalar_block() {
        // Appending at the end of the section would land the key after a
        // subtable, where TOML reads it as part of that subtable instead.
        let document = "[creative_opportunities]\ngam_network_id = \"111\"\n\n\
             [[creative_opportunities.slot]]\nid = \"a\"\n\
             page_patterns = [\"/\"]\nformats = [{ width = 1, height = 1 }]\n";

        let out = upsert_key_in_section(
            document,
            "creative_opportunities",
            "section_root",
            "section_root = \"homepage\"",
        )
        .expect("should insert");

        let value = toml::from_str::<toml::Value>(&out).expect("valid TOML");
        assert_eq!(
            value["creative_opportunities"]["section_root"].as_str(),
            Some("homepage"),
            "the key must belong to the section, not the slot subtable"
        );
    }

    #[test]
    fn splice_refuses_fresh_section_without_a_network_id() {
        // Reachable whenever the scraped unit path has no all-digit leading
        // segment (MCM/child-network paths). Writing the section anyway produces
        // a config missing a required field, which fails load and takes every
        // route to the startup error router once pushed.
        let existing = "[publisher]\ndomain = \"x\"\n";

        let error = splice_creative_slots(
            existing,
            &CreativeSectionKeys::default(),
            &header_rendered(),
        )
        .expect_err("should refuse to create a section with no network id");

        assert!(
            format!("{error:?}").contains("without a GAM network id"),
            "error should name the missing network id, got {error:?}"
        );
    }

    #[test]
    fn splice_appends_section_when_config_has_none() {
        let existing = "[publisher]\ndomain = \"x\"\n";

        let out = splice_creative_slots(existing, &network_keys("222"), &header_rendered())
            .expect("should append a fresh section");

        let value = toml::from_str::<toml::Value>(&out).expect("appended config is valid TOML");
        assert_eq!(
            value["creative_opportunities"]["gam_network_id"].as_str(),
            Some("222")
        );
    }

    #[test]
    fn splice_preserves_section_scalars_and_provider_subtables() {
        // Mirrors the templated operator shape: section policy scalars in the
        // head block and a per-slot prebid provider subtable.
        let existing = "[creative_opportunities]\n\
             gam_network_id = \"111\"\n\
             auction_timeout_ms = 2000\n\
             section_root = \"homepage\"\n\n\
             [[creative_opportunities.slot]]\n\
             id = \"ad-header-0\"\n\
             div_id = \"ad-header-0\"\n\
             gam_unit_path = \"/{network_id}/example/{section}\"\n\
             page_patterns = [\"/\"]\n\
             formats = [{ width = 728, height = 90 }]\n\
             [creative_opportunities.slot.providers.prebid]\n\
             bidders = {}\n\n\
             [auction]\nenabled = true\n";
        let existing_config = existing_config(
            &existing
                .replace("[creative_opportunities]\n", "")
                .replace("[[creative_opportunities.slot]]", "[[slot]]")
                .replace("[creative_opportunities.slot.", "[slot.")
                .replace("\n[auction]\nenabled = true\n", ""),
        );
        let discovered = discovered_header_slot();
        let merged = merge_slots(
            Some(&existing_config),
            &discovered,
            &["/news/*".to_string()],
            false,
        );

        let out = splice_creative_slots(existing, &network_keys("111"), &render_slots(&merged))
            .expect("should splice");

        let value = toml::from_str::<toml::Value>(&out).expect("spliced config is valid TOML");
        let creative = &value["creative_opportunities"];
        assert_eq!(
            creative["section_root"].as_str(),
            Some("homepage"),
            "section policy scalars must survive the splice"
        );
        assert_eq!(creative["auction_timeout_ms"].as_integer(), Some(2000));
        assert_eq!(
            creative["slot"][0]["gam_unit_path"].as_str(),
            Some("/{network_id}/example/{section}"),
            "an existing templated unit path must not be rewritten to a literal"
        );
        assert!(
            creative["slot"][0]["providers"]["prebid"]["bidders"].is_table(),
            "the prebid provider subtable must be re-emitted"
        );
        assert_eq!(
            value["auction"]["enabled"].as_bool(),
            Some(true),
            "trailing sections must be preserved"
        );
    }

    #[test]
    fn splice_preserves_crlf_line_endings() {
        let existing = "[creative_opportunities]\r\ngam_network_id = \"111\"\r\n\r\n\
             [auction]\r\nenabled = true\r\n";

        let out = splice_creative_slots(existing, &network_keys("222"), &header_rendered())
            .expect("should splice");

        assert!(
            !out.replace("\r\n", "").contains('\n'),
            "every line ending should stay CRLF"
        );
        let value = toml::from_str::<toml::Value>(&out).expect("spliced CRLF config is valid TOML");
        assert_eq!(
            value["creative_opportunities"]["gam_network_id"].as_str(),
            Some("222"),
            "network id updated in CRLF config"
        );
    }

    #[test]
    fn render_slots_writes_non_finite_floor_price_as_valid_toml() {
        let slot = RenderSlot {
            id: "header".to_string(),
            div_id: Some("div-gpt-ad-header".to_string()),
            gam_unit_path: Some("/222/homepage/header".to_string()),
            page_patterns: vec!["/".to_string()],
            formats: vec![(728, 90, None)],
            floor_price: Some(f64::NAN),
            targeting: BTreeMap::new(),
            aps_slot_id: None,
            prebid_bidders: None,
        };

        let rendered = render_slots(&[slot]);

        assert!(
            rendered.contains("floor_price = nan"),
            "NaN should render as TOML `nan`, not Rust `NaN`"
        );
        toml::from_str::<toml::Value>(&rendered).expect("rendered slots are valid TOML");
    }

    #[test]
    fn splice_creates_section_when_absent() {
        // Config with no [creative_opportunities] at all — generate should append it.
        let existing = "[publisher]\ndomain = \"x\"\n\n[auction]\nenabled = true\n";

        let out = splice_creative_slots(existing, &network_keys("222"), &header_rendered())
            .expect("should splice");

        let value = toml::from_str::<toml::Value>(&out).expect("valid TOML");
        assert_eq!(
            value["creative_opportunities"]["gam_network_id"].as_str(),
            Some("222"),
            "appended section carries the discovered network id"
        );
        assert_eq!(
            value["creative_opportunities"]["slot"][0]["id"].as_str(),
            Some("header")
        );
        assert!(
            value["publisher"]["domain"].as_str() == Some("x")
                && value["auction"]["enabled"].as_bool() == Some(true),
            "existing sections preserved when appending"
        );
    }

    #[test]
    fn resplice_does_not_accumulate_managed_comment() {
        // A re-run splices into a config that already carries the managed
        // header comment; it must keep exactly one copy, not append another.
        let first = splice_creative_slots(
            "[publisher]\ndomain = \"x\"\n\n[auction]\nenabled = true\n",
            &network_keys("222"),
            &header_rendered(),
        )
        .expect("first splice");
        let second = splice_creative_slots(&first, &network_keys("222"), &header_rendered())
            .expect("second splice");
        let third = splice_creative_slots(&second, &network_keys("222"), &header_rendered())
            .expect("third splice");

        assert_eq!(
            third
                .lines()
                .filter(|line| line.trim() == MANAGED_SLOTS_COMMENT)
                .count(),
            1,
            "managed header comment must not accumulate across re-splices"
        );
        toml::from_str::<toml::Value>(&third).expect("re-spliced config stays valid TOML");
    }

    #[test]
    fn splice_recognizes_inline_commented_section_header() {
        // `[creative_opportunities] # comment` is valid TOML; the splice must
        // update it in place instead of appending a duplicate section.
        let existing = "[creative_opportunities] # ad templates\ngam_network_id = \"111\"\n\n\
             [auction] # flags\nenabled = true\n";

        let out = splice_creative_slots(existing, &network_keys("222"), &header_rendered())
            .expect("should splice");

        assert_eq!(
            out.lines()
                .filter(|line| is_table_header(line, "[creative_opportunities]"))
                .count(),
            1,
            "commented header must not be duplicated"
        );
        let value = toml::from_str::<toml::Value>(&out).expect("spliced config is valid TOML");
        assert_eq!(
            value["creative_opportunities"]["gam_network_id"].as_str(),
            Some("222"),
            "network id updated under a commented header"
        );
        assert_eq!(
            value["creative_opportunities"]["slot"][0]["id"].as_str(),
            Some("header")
        );
        assert_eq!(
            value["auction"]["enabled"].as_bool(),
            Some(true),
            "commented trailing section preserved"
        );
    }

    #[test]
    fn splice_inserts_when_no_existing_slots() {
        let existing =
            "[creative_opportunities]\ngam_network_id = \"111\"\n\n[auction]\nenabled = true\n";

        let out = splice_creative_slots(existing, &network_keys("222"), &header_rendered())
            .expect("should splice");

        let value = toml::from_str::<toml::Value>(&out).expect("valid TOML");
        assert_eq!(
            value["creative_opportunities"]["slot"][0]["id"].as_str(),
            Some("header"),
            "inserted slot id strips the div-gpt-ad- prefix"
        );
        assert_eq!(
            value["creative_opportunities"]["slot"][0]["div_id"].as_str(),
            Some("div-gpt-ad-header"),
            "div_id keeps the stable stem"
        );
        assert!(
            value["auction"]["enabled"].as_bool() == Some(true),
            "auction section preserved after inserted slots"
        );
    }

    #[test]
    fn splice_replaces_inline_slot_array() {
        let existing = "[creative_opportunities]\n\
             gam_network_id = \"111\"\n\
             slot = [{ id = \"old\", div_id = \"old\", gam_unit_path = \"/111/old\", page_patterns = [\"/\"], formats = [{ width = 300, height = 250 }] }]\n\n\
             [auction]\nenabled = true\n";

        let out = splice_creative_slots(existing, &network_keys("222"), &header_rendered())
            .expect("should replace inline slot array");

        let value = toml::from_str::<toml::Value>(&out).expect("spliced config should be valid");
        let slots = value["creative_opportunities"]["slot"]
            .as_array()
            .expect("slots should be an array");
        assert_eq!(slots.len(), 1, "old inline slot should be removed");
        assert_eq!(slots[0]["id"].as_str(), Some("header"));
        assert_eq!(
            value["auction"]["enabled"].as_bool(),
            Some(true),
            "unrelated tables should be preserved"
        );
    }

    #[test]
    fn splice_replaces_inline_slot_map() {
        let existing = "[creative_opportunities]\n\
             gam_network_id = \"111\"\n\
             slot = { \"0\" = { id = \"old\", div_id = \"old\", gam_unit_path = \"/111/old\", page_patterns = [\"/\"], formats = [{ width = 300, height = 250 }] } }\n";

        let out = splice_creative_slots(existing, &network_keys("222"), &header_rendered())
            .expect("should replace inline slot map");

        let value = toml::from_str::<toml::Value>(&out).expect("spliced config should be valid");
        let slots = value["creative_opportunities"]["slot"]
            .as_array()
            .expect("slots should be an array");
        assert_eq!(slots.len(), 1, "old inline slot should be removed");
        assert_eq!(slots[0]["id"].as_str(), Some("header"));
    }

    #[test]
    fn merge_second_run_unions_page_patterns() {
        // Existing slot on "/"; re-discovered this run with "/news/*".
        let existing = existing_config(
            "gam_network_id = \"222\"\n\n\
             [[slot]]\nid = \"header\"\ndiv_id = \"div-gpt-ad-header\"\n\
             gam_unit_path = \"/222/homepage/header\"\npage_patterns = [\"/\"]\n\
             formats = [{ width = 728, height = 90 }]\n",
        );

        let merged = merge_slots(
            Some(&existing),
            &discovered_header_slot(),
            &["/news/*".to_string()],
            false,
        );

        assert_eq!(merged.len(), 1, "same slot is not duplicated");
        assert_eq!(
            merged[0].page_patterns,
            vec!["/".to_string(), "/news/*".to_string()],
            "this run's pattern is unioned into the existing slot"
        );
    }

    #[test]
    fn merge_second_run_unions_formats() {
        let existing = existing_config(
            "gam_network_id = \"222\"\n\n\
             [[slot]]\nid = \"header\"\ndiv_id = \"div-gpt-ad-header\"\n\
             gam_unit_path = \"/222/homepage/header\"\npage_patterns = [\"/\"]\n\
             formats = [{ width = 728, height = 90 }]\n",
        );
        let registry = vec![collector::CollectedGptSlot {
            gam_unit_path: "/222/homepage/header".to_string(),
            div_id: "div-gpt-ad-header".to_string(),
            sizes: vec![(728, 90), (970, 250)],
        }];
        let discovered = gpt_slots::discover_gpt_slots(&registry, &[], false);

        let merged = merge_slots(Some(&existing), &discovered, &["/".to_string()], false);

        assert_eq!(
            merged[0].formats,
            [(728, 90, None), (970, 250, None)],
            "a later audit must retain newly observed formats"
        );
    }

    #[test]
    fn merge_uses_longest_existing_div_prefix() {
        let existing = existing_config(
            "gam_network_id = \"222\"\n\n\
             [[slot]]\nid = \"broad\"\ndiv_id = \"ad-\"\n\
             gam_unit_path = \"/222/broad\"\npage_patterns = [\"/broad/*\"]\n\
             formats = [{ width = 300, height = 250 }]\n\n\
             [[slot]]\nid = \"atf\"\ndiv_id = \"ad-atf-\"\n\
             gam_unit_path = \"/222/atf\"\npage_patterns = [\"/\"]\n\
             formats = [{ width = 728, height = 90 }]\n",
        );
        let registry = vec![collector::CollectedGptSlot {
            gam_unit_path: "/222/atf".to_string(),
            div_id: "ad-atf-0".to_string(),
            sizes: vec![(728, 90)],
        }];
        let discovered = gpt_slots::discover_gpt_slots(&registry, &[], false);

        let merged = merge_slots(
            Some(&existing),
            &discovered,
            &["/news/*".to_string()],
            false,
        );

        assert_eq!(
            merged.len(),
            2,
            "prefix match should not append a duplicate"
        );
        let broad = merged
            .iter()
            .find(|slot| slot.id == "broad")
            .expect("should keep broad slot");
        assert_eq!(
            broad.page_patterns,
            ["/broad/*"],
            "shorter prefix should not claim the discovered div"
        );
        let atf = merged
            .iter()
            .find(|slot| slot.id == "atf")
            .expect("should keep specific slot");
        assert_eq!(
            atf.page_patterns,
            ["/", "/news/*"],
            "longest matching prefix should receive this run's pattern"
        );
    }

    #[test]
    fn merge_reports_when_a_broad_prefix_claims_multiple_discovered_divs() {
        let existing = existing_config(
            "gam_network_id = \"222\"\n\n\
             [[slot]]\nid = \"broad\"\ndiv_id = \"ad-\"\n\
             gam_unit_path = \"/222/broad\"\npage_patterns = [\"/\"]\n\
             formats = [{ width = 300, height = 250 }]\n",
        );
        let discovered = vec![
            RenderSlot::from_evidence(
                "header",
                "ad-header",
                Some("/222/header".to_string()),
                [(728, 90)],
                vec!["/".to_string()],
                false,
            ),
            RenderSlot::from_evidence(
                "footer",
                "ad-footer",
                Some("/222/footer".to_string()),
                [(300, 250)],
                vec!["/".to_string()],
                false,
            ),
        ];

        let (merged, diagnostics) =
            merge_render_slots_with_diagnostics(Some(&existing), discovered, false);

        assert_eq!(
            merged.len(),
            1,
            "the configured prefix still controls merging"
        );
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("matched 2 discovered divs"));
        assert!(diagnostics[0].contains("ad-footer"));
        assert!(diagnostics[0].contains("ad-header"));
    }

    #[test]
    fn merge_renames_new_slot_id_that_collides_with_existing_config() {
        let existing = existing_config(
            "gam_network_id = \"222\"\n\n\
             [[slot]]\nid = \"header-main\"\ndiv_id = \"legacy-header\"\n\
             gam_unit_path = \"/222/legacy\"\npage_patterns = [\"/\"]\n\
             formats = [{ width = 300, height = 250 }]\n",
        );
        let registry = vec![collector::CollectedGptSlot {
            gam_unit_path: "/222/header".to_string(),
            div_id: "div-gpt-ad-header.main".to_string(),
            sizes: vec![(728, 90)],
        }];
        let discovered = gpt_slots::discover_gpt_slots(&registry, &[], false);

        let merged = merge_slots(
            Some(&existing),
            &discovered,
            &["/news/*".to_string()],
            false,
        );
        let ids = merged
            .iter()
            .map(|slot| slot.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, ["header-main", "header-main-2"]);
    }

    #[test]
    fn merge_keeps_existing_only_slots() {
        // Existing has header + sidebar; this run re-sees only header.
        let existing = existing_config(
            "gam_network_id = \"222\"\n\n\
             [[slot]]\nid = \"header\"\ndiv_id = \"div-gpt-ad-header\"\n\
             gam_unit_path = \"/222/homepage/header\"\npage_patterns = [\"/\"]\n\
             formats = [{ width = 728, height = 90 }]\n\n\
             [[slot]]\nid = \"sidebar\"\ndiv_id = \"ad-sidebar\"\n\
             gam_unit_path = \"/222/sidebar\"\npage_patterns = [\"/news/*\"]\n\
             formats = [{ width = 300, height = 250 }]\nfloor_price = 0.5\n",
        );

        let merged = merge_slots(
            Some(&existing),
            &discovered_header_slot(),
            &["/".to_string()],
            false,
        );

        let ids: Vec<&str> = merged.iter().map(|slot| slot.id.as_str()).collect();
        assert_eq!(ids, vec!["header", "sidebar"], "sidebar preserved");
        let sidebar = merged
            .iter()
            .find(|slot| slot.id == "sidebar")
            .expect("sidebar");
        assert_eq!(
            sidebar.floor_price,
            Some(0.5),
            "hand-tuned fields preserved"
        );
    }

    #[test]
    fn merge_replace_wipes_existing() {
        let existing = existing_config(
            "gam_network_id = \"222\"\n\n\
             [[slot]]\nid = \"sidebar\"\ndiv_id = \"ad-sidebar\"\n\
             gam_unit_path = \"/222/sidebar\"\npage_patterns = [\"/\"]\n\
             formats = [{ width = 300, height = 250 }]\n",
        );

        let merged = merge_slots(
            Some(&existing),
            &discovered_header_slot(),
            &["/".to_string()],
            true,
        );

        let ids: Vec<&str> = merged.iter().map(|slot| slot.id.as_str()).collect();
        assert_eq!(ids, vec!["header"], "--replace keeps only discovered slots");
    }

    #[test]
    fn resolve_network_id_prefers_discovered_unless_preserving_existing() {
        let with_slots = existing_config(
            "gam_network_id = \"111\"\n\n[[slot]]\nid = \"s\"\ndiv_id = \"ad-s\"\n\
             gam_unit_path = \"/111/s\"\npage_patterns = [\"/\"]\n\
             formats = [{ width = 300, height = 250 }]\n",
        );
        let empty = existing_config("gam_network_id = \"111\"\n");

        // Real merge → keep existing.
        assert_eq!(
            resolve_network_id(Some(&with_slots), Some("222"), false).as_deref(),
            Some("111")
        );
        // Placeholder section with no slots → discovered wins.
        assert_eq!(
            resolve_network_id(Some(&empty), Some("222"), false).as_deref(),
            Some("222")
        );
        // --replace → discovered wins.
        assert_eq!(
            resolve_network_id(Some(&with_slots), Some("222"), true).as_deref(),
            Some("222")
        );
        // No existing config → discovered.
        assert_eq!(
            resolve_network_id(None, Some("222"), false).as_deref(),
            Some("222")
        );
    }

    #[test]
    fn toml_key_quotes_only_non_bare_keys() {
        assert_eq!(toml_key("zone"), "zone");
        assert_eq!(toml_key("ad-loc"), "ad-loc");
        assert_eq!(toml_key("a.b"), "\"a.b\"");
        assert_eq!(toml_key("with space"), "\"with space\"");
        assert_eq!(toml_key(""), "\"\"");
    }

    #[test]
    fn toml_string_escapes_quotes_backslashes_and_controls() {
        assert_eq!(toml_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(toml_string("line\nbreak\t!"), "\"line\\nbreak\\t!\"");
    }

    #[test]
    fn toml_string_escapes_del_control_char() {
        assert_eq!(toml_string("a\u{7f}b"), "\"a\\u007Fb\"");
        let doc = format!("value = {}", toml_string("a\u{7f}b"));
        let value = toml::from_str::<toml::Value>(&doc).expect("DEL escapes to valid TOML");
        assert_eq!(
            value["value"].as_str(),
            Some("a\u{7f}b"),
            "escaped DEL round-trips as data"
        );
    }

    #[test]
    fn replace_key_handles_inline_commented_headers() {
        let document = "[creative_opportunities] # managed\ngam_network_id = \"111\"\n\n\
             [auction] # flags\nenabled = true\n";

        let updated = replace_key_in_section(
            document,
            "creative_opportunities",
            "gam_network_id",
            "gam_network_id = \"222\"",
        )
        .expect("should find the commented section header");

        assert!(
            updated.contains("gam_network_id = \"222\""),
            "key replaced under a commented header"
        );
        assert!(
            updated.contains("enabled = true"),
            "later commented section left untouched"
        );
    }

    #[test]
    fn render_quotes_exotic_targeting_keys_to_valid_toml() {
        let existing = existing_config(
            "gam_network_id = \"1\"\n\n\
             [[slot]]\nid = \"s\"\ndiv_id = \"ad-s\"\ngam_unit_path = \"/1/s\"\n\
             page_patterns = [\"/\"]\nformats = [{ width = 300, height = 250 }]\n\
             targeting = { \"a.b\" = \"x\" }\n",
        );

        let merged = merge_slots(
            Some(&existing),
            &discovered_header_slot(),
            &["/".to_string()],
            false,
        );
        let doc = format!(
            "[creative_opportunities]\ngam_network_id = \"1\"\n{}",
            render_slots(&merged)
        );

        toml::from_str::<toml::Value>(&doc).expect("exotic targeting key renders as valid TOML");
    }
}
