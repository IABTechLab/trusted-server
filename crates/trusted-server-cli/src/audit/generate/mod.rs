mod analyzer;
pub(crate) mod browser_collector;
pub(crate) mod collector;
mod gpt_slots;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use trusted_server_core::auction::types::MediaType;
use trusted_server_core::creative_opportunities::{
    CreativeOpportunitiesConfig, CreativeOpportunitySlot,
};
use url::Url;

use crate::audit::generate::collector::AuditCollector;
use crate::config_init::EXAMPLE_CONFIG;
use crate::error::{cli_error, report_error, CliResult};

use analyzer::{analyze_collected_page, extract_gtm_container_id};

/// Arguments for `ts audit generate <url>` — bootstraps draft Trusted Server
/// config and JavaScript asset audit files from a live page (issue #800).
#[derive(Debug, clap::Args)]
pub(crate) struct GenerateArgs {
    /// Public HTTP(S) URL to audit.
    pub(crate) url: String,
    /// JavaScript asset audit output path.
    #[arg(long)]
    pub(crate) js_assets: Option<std::path::PathBuf>,
    /// Draft Trusted Server config output path.
    #[arg(long)]
    pub(crate) config: Option<std::path::PathBuf>,
    /// Do not write the JavaScript asset audit file.
    #[arg(long)]
    pub(crate) no_js_assets: bool,
    /// Do not write the draft Trusted Server config file.
    #[arg(long)]
    pub(crate) no_config: bool,
    /// Overwrite existing output files.
    #[arg(long)]
    pub(crate) force: bool,
    /// Cookie to send with the page request, as `name=value`. Repeatable.
    /// Use to carry an existing session (e.g. a valid bot-protection clearance
    /// cookie) so the origin serves the real page instead of a challenge.
    #[arg(long = "cookie", value_name = "NAME=VALUE", value_parser = crate::audit::parse_cookie)]
    pub(crate) cookies: Vec<(String, String)>,
}

const DEFAULT_JS_ASSETS_PATH: &str = "js-assets.toml";
const DEFAULT_CONFIG_PATH: &str = "trusted-server.toml";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum AssetParty {
    FirstParty,
    ThirdParty,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct AuditedAsset {
    pub(crate) kind: String,
    pub(crate) url: String,
    pub(crate) host: String,
    pub(crate) party: AssetParty,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) integration: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct DetectedIntegration {
    pub(crate) id: String,
    pub(crate) evidence: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct AuditArtifact {
    pub(crate) audited_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) page_title: Option<String>,
    pub(crate) js_asset_count: usize,
    pub(crate) third_party_asset_count: usize,
    pub(crate) detected_integrations: Vec<DetectedIntegration>,
    pub(crate) assets: Vec<AuditedAsset>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct AuditOutputs {
    pub(crate) artifact: AuditArtifact,
    pub(crate) js_assets_toml: String,
    pub(crate) draft_config_toml: String,
    pub(crate) ad_slot_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuditOutputPlan {
    js_assets_path: Option<PathBuf>,
    config_path: Option<PathBuf>,
}

pub(crate) fn run_generate(
    args: &GenerateArgs,
    collector: &dyn AuditCollector,
    out: &mut dyn Write,
) -> CliResult<()> {
    let target_url = parse_audit_url(&args.url)?;
    let plan = resolve_output_plan(args)?;
    let collected = collector.collect_page(&target_url, &args.cookies)?;
    let outputs = build_audit_outputs(&collected)?;
    let wrote_config = plan.config_path.is_some();
    let written = write_audit_outputs(&outputs, &plan)?;
    write_success_summary(&outputs, &written, wrote_config, out)
}

fn parse_audit_url(value: &str) -> CliResult<Url> {
    let url = Url::parse(value)
        .map_err(|error| report_error(format!("invalid audit URL `{value}`: {error}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return cli_error(format!(
            "`ts audit` only supports http/https URLs, got `{}`",
            url.scheme()
        ));
    }
    Ok(url)
}

fn resolve_output_plan(args: &GenerateArgs) -> CliResult<AuditOutputPlan> {
    if args.no_js_assets && args.no_config {
        return cli_error("nothing to do: both --no-js-assets and --no-config were set");
    }

    let js_assets_path = if args.no_js_assets {
        None
    } else {
        Some(resolve_output_path(
            args.js_assets.as_deref(),
            DEFAULT_JS_ASSETS_PATH,
        )?)
    };
    let config_path = if args.no_config {
        None
    } else {
        Some(resolve_output_path(
            args.config.as_deref(),
            DEFAULT_CONFIG_PATH,
        )?)
    };

    if js_assets_path.is_some() && js_assets_path == config_path {
        return cli_error("audit output paths must be distinct");
    }

    for path in [&js_assets_path, &config_path].into_iter().flatten() {
        if path.exists() && !args.force {
            return cli_error(format!(
                "refusing to overwrite existing file `{}`; re-run with --force",
                path.display()
            ));
        }
    }

    Ok(AuditOutputPlan {
        js_assets_path,
        config_path,
    })
}

fn resolve_output_path(path: Option<&Path>, default: &str) -> CliResult<PathBuf> {
    let candidate = path.unwrap_or_else(|| Path::new(default));
    if candidate.is_absolute() {
        Ok(candidate.to_path_buf())
    } else {
        Ok(std::env::current_dir()
            .map_err(|error| report_error(format!("failed to read current directory: {error}")))?
            .join(candidate))
    }
}

fn build_audit_outputs(collected: &collector::CollectedPage) -> CliResult<AuditOutputs> {
    let artifact = analyze_collected_page(collected)?;
    let final_url = collected
        .final_url()
        .map_err(|error| report_error(format!("invalid final URL: {error}")))?;
    let js_assets_toml = toml::to_string_pretty(&artifact)
        .map_err(|error| report_error(format!("failed to serialize audit artifact: {error}")))?;
    let page_has_prebid = artifact
        .detected_integrations
        .iter()
        .any(|integration| integration.id == "prebid");
    let slots = gpt_slots::discover_gpt_slots(
        &collected.gpt_slots,
        &collected.network_requests,
        page_has_prebid,
    );
    let ad_slot_count = slots.slots.len();
    let draft_config_toml = build_draft_config(&final_url, &artifact, &slots)?;

    Ok(AuditOutputs {
        artifact,
        js_assets_toml,
        draft_config_toml,
        ad_slot_count,
    })
}

fn write_audit_outputs(outputs: &AuditOutputs, plan: &AuditOutputPlan) -> CliResult<Vec<String>> {
    let selected_paths = [&plan.js_assets_path, &plan.config_path]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    for path in &selected_paths {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| {
                report_error(format!(
                    "failed to create parent directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
    }

    let mut written_paths = Vec::new();
    if let Some(path) = &plan.js_assets_path {
        fs::write(path, &outputs.js_assets_toml).map_err(|error| {
            report_error(format!(
                "failed to write JS asset audit {}: {error}",
                path.display()
            ))
        })?;
        written_paths.push(path.display().to_string());
    }
    if let Some(path) = &plan.config_path {
        fs::write(path, &outputs.draft_config_toml).map_err(|error| {
            report_error(format!(
                "failed to write draft config {}: {error}",
                path.display()
            ))
        })?;
        written_paths.push(path.display().to_string());
    }

    Ok(written_paths)
}

fn write_success_summary(
    outputs: &AuditOutputs,
    written: &[String],
    wrote_config: bool,
    out: &mut dyn Write,
) -> CliResult<()> {
    let integrations = outputs
        .artifact
        .detected_integrations
        .iter()
        .map(|integration| integration.id.as_str())
        .collect::<Vec<_>>();
    let draft_note = if wrote_config {
        "\nDraft config: review before validation and push"
    } else {
        ""
    };
    writeln!(
        out,
        "Audited {}\nTitle: {}\nJS assets: {}\nThird-party assets: {}\nAd slots: {}\nDetected integrations: {}\nWrote: {}{}",
        outputs.artifact.audited_url,
        outputs
            .artifact
            .page_title
            .as_deref()
            .unwrap_or("<unknown>"),
        outputs.artifact.js_asset_count,
        outputs.artifact.third_party_asset_count,
        outputs.ad_slot_count,
        if integrations.is_empty() {
            "none".to_string()
        } else {
            integrations.join(", ")
        },
        if written.is_empty() {
            "none".to_string()
        } else {
            written.join(", ")
        },
        draft_note
    )
    .map_err(|error| report_error(format!("failed to write command output: {error}")))
}

fn build_draft_config(
    target_url: &Url,
    artifact: &AuditArtifact,
    slots: &gpt_slots::DiscoveredSlots,
) -> CliResult<String> {
    let host = target_url
        .host_str()
        .ok_or_else(|| report_error("audited URL is missing a host"))?;
    let origin = target_url.origin().ascii_serialization();
    let mut draft = EXAMPLE_CONFIG.to_string();

    draft = replace_key_in_section(
        &draft,
        "publisher",
        "domain",
        &format!("domain = \"{host}\""),
    )?;
    draft = replace_key_in_section(
        &draft,
        "publisher",
        "cookie_domain",
        &format!("cookie_domain = \".{host}\""),
    )?;
    draft = replace_key_in_section(
        &draft,
        "publisher",
        "origin_url",
        &format!("origin_url = \"{origin}\""),
    )?;

    let detected = artifact
        .detected_integrations
        .iter()
        .map(|integration| integration.id.as_str())
        .collect::<BTreeSet<_>>();

    if detected.contains("gpt") {
        draft = replace_key_in_section(&draft, "integrations.gpt", "enabled", "enabled = true")?;
    }
    if detected.contains("didomi") {
        draft = replace_key_in_section(&draft, "integrations.didomi", "enabled", "enabled = true")?;
    }
    if detected.contains("datadome") {
        draft =
            replace_key_in_section(&draft, "integrations.datadome", "enabled", "enabled = true")?;
    }

    let mut manual_review = Vec::new();
    if detected.contains("google_tag_manager") {
        if let Some(gtm_id) = extract_gtm_container_id(artifact) {
            draft = replace_key_in_section(
                &draft,
                "integrations.google_tag_manager",
                "enabled",
                "enabled = true",
            )?;
            draft = replace_key_in_section(
                &draft,
                "integrations.google_tag_manager",
                "container_id",
                &format!("container_id = \"{gtm_id}\""),
            )?;
        } else {
            manual_review.push("google_tag_manager");
        }
    }

    for integration in detected {
        if !matches!(
            integration,
            "gpt" | "didomi" | "datadome" | "google_tag_manager"
        ) {
            manual_review.push(integration);
        }
    }

    if !manual_review.is_empty() {
        if !draft.ends_with('\n') {
            draft.push('\n');
        }
        draft.push_str("\n# Audit findings requiring manual review\n");
        for integration in manual_review {
            draft.push_str(&format!(
                "# - Detected {integration}; review the corresponding [integrations.{integration}] section before enabling it.\n"
            ));
        }
    }

    if !slots.slots.is_empty() {
        if let Some(network_id) = &slots.gam_network_id {
            draft = replace_key_in_section(
                &draft,
                "creative_opportunities",
                "gam_network_id",
                &format!("gam_network_id = \"{network_id}\""),
            )?;
        }
        draft.push_str(&render_discovered_slots(target_url, slots));
    }

    Ok(draft)
}

/// Renders discovered GPT slots as appended `[[creative_opportunities.slot]]`
/// tables. Page patterns default to the audited path and are flagged for review.
fn render_discovered_slots(target_url: &Url, slots: &gpt_slots::DiscoveredSlots) -> String {
    let path = target_url.path();
    let page_pattern = if path.is_empty() { "/" } else { path };

    let mut out = String::from(
        "\n# Slots discovered from live GPT ad requests during the audit.\n\
         # Review page_patterns and formats before validating/pushing.\n",
    );
    for slot in &slots.slots {
        let formats = slot
            .formats
            .iter()
            .map(|(width, height)| format!("{{ width = {width}, height = {height} }}"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "\n[[creative_opportunities.slot]]\n\
             id = \"{id}\"\n\
             div_id = \"{div_id}\"\n\
             gam_unit_path = \"{gam_unit_path}\"\n\
             page_patterns = [\"{page_pattern}\"]\n\
             formats = [{formats}]\n",
            id = slot.id,
            div_id = slot.div_id,
            gam_unit_path = slot.gam_unit_path,
        ));
        if slot.has_prebid {
            out.push_str("[creative_opportunities.slot.providers.prebid]\nbidders = {}\n");
        }
    }
    out
}

/// Runs `ts audit ad-templates generate`: scrape the live page's GPT slots and
/// rewrite only the `[creative_opportunities]` slot array in `config_path` in
/// place, preserving every other section and comment.
///
/// # Errors
///
/// Returns an error when the config cannot be read, the page cannot be
/// collected, no slots are discovered, or the config has no
/// `[creative_opportunities]` section to update.
#[allow(clippy::too_many_arguments, reason = "cohesive one-shot command entry")]
pub(crate) fn run_update_slots(
    url: &str,
    config_path: &Path,
    existing_creative: Option<&CreativeOpportunitiesConfig>,
    page_patterns: &[String],
    replace: bool,
    cookies: &[(String, String)],
    dry_run: bool,
    collector: &dyn AuditCollector,
    out: &mut dyn Write,
) -> CliResult<()> {
    let target_url = parse_audit_url(url)?;
    let existing = fs::read_to_string(config_path).map_err(|error| {
        report_error(format!(
            "failed to read config {}: {error}",
            config_path.display()
        ))
    })?;

    let collected = collector.collect_page(&target_url, cookies)?;
    let artifact = analyze_collected_page(&collected)?;
    let page_has_prebid = artifact
        .detected_integrations
        .iter()
        .any(|integration| integration.id == "prebid");
    let discovered = gpt_slots::discover_gpt_slots(
        &collected.gpt_slots,
        &collected.network_requests,
        page_has_prebid,
    );
    if discovered.slots.is_empty() {
        return cli_error("no ad-template slots were discovered on the page");
    }

    // Patterns for slots seen on this run: the `--page-pattern` values, or the
    // audited path when none are given (preserving single-page behavior).
    let run_patterns: Vec<String> = if page_patterns.is_empty() {
        vec![default_page_pattern(&target_url)]
    } else {
        page_patterns.to_vec()
    };

    let merged = merge_slots(existing_creative, &discovered, &run_patterns, replace);
    let network_id = resolve_network_id(
        existing_creative,
        discovered.gam_network_id.as_deref(),
        replace,
    );
    let rendered_slots = render_slots(&merged);
    let updated = splice_creative_slots(&existing, network_id.as_deref(), &rendered_slots)?;

    if dry_run {
        writeln!(out, "{updated}")
            .map_err(|error| report_error(format!("failed to write preview: {error}")))?;
        return Ok(());
    }
    fs::write(config_path, &updated).map_err(|error| {
        report_error(format!(
            "failed to write config {}: {error}",
            config_path.display()
        ))
    })?;
    writeln!(
        out,
        "Wrote {} slot(s) to {} ({} discovered this run)",
        merged.len(),
        config_path.display(),
        discovered.slots.len(),
    )
    .map_err(|error| report_error(format!("failed to write command output: {error}")))
}

/// Chooses the `gam_network_id` to write.
///
/// The existing id is kept only when a real merge preserves existing slots.
/// On `--replace`, or when the config had no slots (e.g. a placeholder
/// `[creative_opportunities]` section), the discovered id wins — mirroring
/// [`merge_slots`], which returns discovered-only in those cases.
fn resolve_network_id(
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

/// The default page pattern for a scraped URL: its path, or `/` for the root.
fn default_page_pattern(target_url: &Url) -> String {
    let path = target_url.path();
    if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    }
}

/// A slot ready to render — the union of discovered and existing fields, without
/// the core type's `pub(crate)` compiled-pattern cache.
#[derive(Debug, Clone)]
struct RenderSlot {
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
    /// The stable identity used to match slots across runs: the div id (or slot
    /// id), with any trailing `-` trimmed so hand-authored stems still match.
    fn key(&self) -> String {
        self.div_id
            .as_deref()
            .unwrap_or(&self.id)
            .trim_end_matches('-')
            .to_string()
    }

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
///   fields); a slot re-seen this run has `run_patterns` unioned into its
///   `page_patterns`; slots seen only this run are appended.
fn merge_slots(
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

    let existing_slots = existing.map(|config| config.slot.as_slice()).unwrap_or(&[]);
    if replace || existing_slots.is_empty() {
        return discovered_slots;
    }

    let mut merged: Vec<RenderSlot> = existing_slots
        .iter()
        .map(RenderSlot::from_existing)
        .collect();
    for slot in discovered_slots {
        let key = slot.key();
        if let Some(present) = merged.iter_mut().find(|existing| existing.key() == key) {
            for pattern in &slot.page_patterns {
                if !present.page_patterns.contains(pattern) {
                    present.page_patterns.push(pattern.clone());
                }
            }
        } else {
            merged.push(slot);
        }
    }
    merged
}

/// Renders merged slots as compact `[[creative_opportunities.slot]]` TOML blocks.
fn render_slots(slots: &[RenderSlot]) -> String {
    let mut out = String::from(
        "\n# Slots managed by `ts audit ad-templates generate`.\n\
         # Review page_patterns and formats before validating/pushing.\n",
    );
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
            out.push_str(&format!("floor_price = {floor}\n"));
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
fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
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

/// Rewrites the `[creative_opportunities]` slot array of `existing` with the
/// pre-rendered `rendered_slots` text, updating `gam_network_id` and preserving
/// all other sections and comments.
///
/// If the config has no `[creative_opportunities]` section, a fresh one is
/// appended so `generate` works against a config that omits it.
fn splice_creative_slots(
    existing: &str,
    network_id: Option<&str>,
    rendered_slots: &str,
) -> CliResult<String> {
    let rendered = rendered_slots.trim_matches('\n');

    // No section yet — append a fresh one with the network id and slots.
    if !existing
        .lines()
        .any(|line| line.trim() == "[creative_opportunities]")
    {
        let mut result = existing.to_string();
        if !result.is_empty() && !result.ends_with('\n') {
            result.push('\n');
        }
        result.push_str("\n[creative_opportunities]\n");
        if let Some(network_id) = network_id {
            result.push_str(&format!("gam_network_id = \"{network_id}\"\n"));
        }
        result.push_str(rendered);
        result.push('\n');
        return Ok(result);
    }

    // Section exists — update `gam_network_id` (best-effort) and replace slots.
    let mut document = existing.to_string();
    if let Some(network_id) = network_id {
        if let Ok(updated) = replace_key_in_section(
            &document,
            "creative_opportunities",
            "gam_network_id",
            &format!("gam_network_id = \"{network_id}\""),
        ) {
            document = updated;
        }
    }

    let lines: Vec<&str> = document.lines().collect();
    let header = lines
        .iter()
        .position(|line| line.trim() == "[creative_opportunities]")
        .ok_or_else(|| {
            report_error("target config has no [creative_opportunities] section to update")
        })?;

    let is_slot_table = |line: &str| {
        let trimmed = line.trim_start();
        trimmed.starts_with("[[creative_opportunities.slot]]")
            || trimmed.starts_with("[creative_opportunities.slot.")
    };
    let is_unrelated_table = |line: &str| {
        let trimmed = line.trim_start();
        trimmed.starts_with('[') && !is_slot_table(line) && trimmed != "[creative_opportunities]"
    };

    // Where the existing slot array begins (first slot table after the header),
    // else the end of the scalar block (first unrelated table, or EOF).
    let existing_start = lines[header + 1..]
        .iter()
        .position(|line| is_slot_table(line))
        .map(|offset| header + 1 + offset);
    let start = existing_start.unwrap_or_else(|| {
        lines[header + 1..]
            .iter()
            .position(|line| is_unrelated_table(line))
            .map_or(lines.len(), |offset| header + 1 + offset)
    });
    // Where the slot array ends: first unrelated top-level table, or EOF.
    let end = lines[start..]
        .iter()
        .position(|line| is_unrelated_table(line))
        .map_or(lines.len(), |offset| start + offset);

    let mut result = lines[..start].join("\n");
    if !result.is_empty() {
        result.push('\n');
    }
    result.push_str(rendered);
    result.push('\n');
    let tail = lines[end..].join("\n");
    if !tail.is_empty() {
        result.push('\n');
        result.push_str(&tail);
    }
    if existing.ends_with('\n') && !result.ends_with('\n') {
        result.push('\n');
    }
    Ok(result)
}

fn replace_key_in_section(
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
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = trimmed == section_header;
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
    Ok(output)
}

fn is_key_line(trimmed_line: &str, key: &str) -> bool {
    trimmed_line
        .strip_prefix(key)
        .and_then(|remaining| remaining.trim_start().strip_prefix('='))
        .is_some()
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use tempfile::TempDir;

    use super::*;
    use crate::audit::generate::collector::{CollectedPage, CollectedRequest, CollectedScriptTag};

    struct FakeCollector {
        collected: CollectedPage,
        calls: Cell<usize>,
    }

    impl FakeCollector {
        fn new(collected: CollectedPage) -> Self {
            Self {
                collected,
                calls: Cell::new(0),
            }
        }
    }

    impl AuditCollector for FakeCollector {
        fn collect_page(
            &self,
            _target_url: &Url,
            _cookies: &[(String, String)],
        ) -> CliResult<CollectedPage> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.collected.clone())
        }
    }

    fn collected_page() -> CollectedPage {
        CollectedPage {
            requested_url: "https://publisher.example/page".to_string(),
            final_url: "https://publisher.example/page".to_string(),
            page_title: Some("Example Publisher".to_string()),
            html: r#"<html><head><title>Example Publisher</title></head></html>"#.to_string(),
            script_tags: vec![
                CollectedScriptTag {
                    src: Some("https://www.googletagmanager.com/gtm.js?id=GTM-ABC123".to_string()),
                    inline_text: None,
                },
                CollectedScriptTag {
                    src: Some("https://securepubads.g.doubleclick.net/tag/js/gpt.js".to_string()),
                    inline_text: None,
                },
            ],
            network_requests: vec![CollectedRequest {
                url: "https://cdn.publisher.example/app.js".to_string(),
                resource_type: Some("script".to_string()),
            }],
            gpt_slots: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn audit_args(url: &str) -> GenerateArgs {
        GenerateArgs {
            url: url.to_string(),
            js_assets: None,
            config: None,
            no_js_assets: false,
            no_config: false,
            force: false,
            cookies: Vec::new(),
        }
    }

    #[test]
    fn parse_audit_url_accepts_http_and_https() {
        assert!(parse_audit_url("http://publisher.example").is_ok());
        assert!(parse_audit_url("https://publisher.example").is_ok());
    }

    #[test]
    fn parse_audit_url_rejects_non_http_schemes() {
        for url in [
            "file:///etc/passwd",
            "data:text/html,hello",
            "chrome://version",
        ] {
            let error = parse_audit_url(url).expect_err("should reject non-http URL");
            assert!(
                format!("{error:?}").contains("only supports http/https"),
                "should explain scheme restriction"
            );
        }
    }

    #[test]
    fn resolve_output_plan_rejects_no_outputs() {
        let mut args = audit_args("https://publisher.example");
        args.no_js_assets = true;
        args.no_config = true;

        let error = resolve_output_plan(&args).expect_err("should reject empty output set");

        assert!(
            format!("{error:?}").contains("nothing to do"),
            "should explain no-output error"
        );
    }

    #[test]
    fn resolve_output_plan_rejects_existing_files_without_force() {
        let temp = TempDir::new().expect("should create temp dir");
        let path = temp.path().join("js-assets.toml");
        fs::write(&path, "existing").expect("should write existing file");
        let mut args = audit_args("https://publisher.example");
        args.js_assets = Some(path);
        args.no_config = true;

        let error = resolve_output_plan(&args).expect_err("should reject overwrite");

        assert!(
            format!("{error:?}").contains("refusing to overwrite"),
            "should explain overwrite refusal"
        );
    }

    #[test]
    fn resolve_output_plan_allows_existing_files_with_force() {
        let temp = TempDir::new().expect("should create temp dir");
        let path = temp.path().join("js-assets.toml");
        fs::write(&path, "existing").expect("should write existing file");
        let mut args = audit_args("https://publisher.example");
        args.js_assets = Some(path.clone());
        args.no_config = true;
        args.force = true;

        let plan = resolve_output_plan(&args).expect("should allow forced overwrite");

        assert_eq!(plan.js_assets_path.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn run_generate_writes_selected_outputs_and_summary() {
        let temp = TempDir::new().expect("should create temp dir");
        let js_assets = temp.path().join("audit/js-assets.toml");
        let config = temp.path().join("audit/trusted-server.toml");
        let args = GenerateArgs {
            url: "https://publisher.example/page".to_string(),
            js_assets: Some(js_assets.clone()),
            config: Some(config.clone()),
            no_js_assets: false,
            no_config: false,
            force: false,
            cookies: Vec::new(),
        };
        let collector = FakeCollector::new(collected_page());
        let mut out = Vec::new();

        run_generate(&args, &collector, &mut out).expect("should run audit");

        assert_eq!(collector.calls.get(), 1, "should collect page once");
        assert!(js_assets.exists(), "should write JS assets");
        assert!(config.exists(), "should write draft config");
        let summary = String::from_utf8(out).expect("summary should be UTF-8");
        assert!(summary.contains("Audited https://publisher.example/page"));
        assert!(summary.contains("Detected integrations: google_tag_manager, gpt"));
        assert!(summary.contains("Draft config: review before validation and push"));
    }

    #[test]
    fn run_generate_respects_no_config() {
        let temp = TempDir::new().expect("should create temp dir");
        let js_assets = temp.path().join("js-assets.toml");
        let mut args = audit_args("https://publisher.example/page");
        args.js_assets = Some(js_assets.clone());
        args.no_config = true;
        let collector = FakeCollector::new(collected_page());

        run_generate(&args, &collector, &mut Vec::new()).expect("should run audit");

        assert!(js_assets.exists(), "should write assets");
        assert!(
            !temp.path().join("trusted-server.toml").exists(),
            "should not write config"
        );
    }

    #[test]
    fn run_generate_respects_no_js_assets() {
        let temp = TempDir::new().expect("should create temp dir");
        let config = temp.path().join("trusted-server.toml");
        let mut args = audit_args("https://publisher.example/page");
        args.config = Some(config.clone());
        args.no_js_assets = true;
        let collector = FakeCollector::new(collected_page());
        let mut out = Vec::new();

        run_generate(&args, &collector, &mut out).expect("should run audit");

        assert!(config.exists(), "should write config");
        assert!(
            !temp.path().join("js-assets.toml").exists(),
            "should not write JS assets"
        );
        let summary = String::from_utf8(out).expect("summary should be UTF-8");
        assert!(summary.contains("Draft config: review before validation and push"));
    }

    #[test]
    fn run_generate_writes_collector_warnings_to_asset_artifact() {
        let temp = TempDir::new().expect("should create temp dir");
        let js_assets = temp.path().join("js-assets.toml");
        let mut args = audit_args("https://publisher.example/page");
        args.js_assets = Some(js_assets.clone());
        args.no_config = true;
        let mut collected = collected_page();
        collected.warnings.push(
            "browser audit timed out while waiting for the page to settle; results may be partial"
                .to_string(),
        );
        let collector = FakeCollector::new(collected);

        run_generate(&args, &collector, &mut Vec::new()).expect("should run audit");

        let artifact = fs::read_to_string(js_assets).expect("should read artifact");
        assert!(
            artifact.contains("results may be partial"),
            "should persist collector warning"
        );
    }

    #[test]
    fn run_generate_conflict_prevents_collection() {
        let temp = TempDir::new().expect("should create temp dir");
        let js_assets = temp.path().join("js-assets.toml");
        fs::write(&js_assets, "existing").expect("should write existing file");
        let mut args = audit_args("https://publisher.example/page");
        args.js_assets = Some(js_assets);
        args.no_config = true;
        let collector = FakeCollector::new(collected_page());

        let error = run_generate(&args, &collector, &mut Vec::new())
            .expect_err("should reject existing output");

        assert_eq!(collector.calls.get(), 0, "should not collect page");
        assert!(
            format!("{error:?}").contains("refusing to overwrite"),
            "should report overwrite conflict"
        );
    }

    #[test]
    fn build_draft_config_uses_final_url_and_detected_integrations() {
        let url = Url::parse("https://www.publisher.example:8443/path").expect("should parse URL");
        let artifact = AuditArtifact {
            audited_url: url.to_string(),
            page_title: Some("Example".to_string()),
            js_asset_count: 2,
            third_party_asset_count: 2,
            detected_integrations: vec![
                DetectedIntegration {
                    id: "google_tag_manager".to_string(),
                    evidence: "GTM-ABC123".to_string(),
                },
                DetectedIntegration {
                    id: "gpt".to_string(),
                    evidence: "https://securepubads.g.doubleclick.net/tag/js/gpt.js".to_string(),
                },
                DetectedIntegration {
                    id: "prebid".to_string(),
                    evidence: "inline script matched `prebid`".to_string(),
                },
            ],
            assets: Vec::new(),
            warnings: Vec::new(),
        };

        let draft = build_draft_config(&url, &artifact, &gpt_slots::DiscoveredSlots::default())
            .expect("should build draft config");

        assert!(draft.contains("domain = \"www.publisher.example\""));
        assert!(draft.contains("cookie_domain = \".www.publisher.example\""));
        assert!(draft.contains("origin_url = \"https://www.publisher.example:8443\""));
        assert!(draft.contains("[integrations.gpt]\nenabled = true"));
        assert!(draft.contains("[integrations.google_tag_manager]\nenabled = true"));
        assert!(draft.contains("container_id = \"GTM-ABC123\""));
        assert!(draft.contains("Detected prebid"));
        toml::from_str::<toml::Value>(&draft).expect("draft should parse as TOML");
    }

    #[test]
    fn build_draft_config_does_not_enable_gtm_without_container_id() {
        let url = Url::parse("https://publisher.example/path").expect("should parse URL");
        let artifact = AuditArtifact {
            audited_url: url.to_string(),
            page_title: None,
            js_asset_count: 1,
            third_party_asset_count: 1,
            detected_integrations: vec![DetectedIntegration {
                id: "google_tag_manager".to_string(),
                evidence: "https://www.googletagmanager.com/gtm.js".to_string(),
            }],
            assets: Vec::new(),
            warnings: Vec::new(),
        };

        let draft = build_draft_config(&url, &artifact, &gpt_slots::DiscoveredSlots::default())
            .expect("should build draft config");

        assert!(draft.contains("[integrations.google_tag_manager]\nenabled = false"));
        assert!(draft.contains("Detected google_tag_manager"));
    }

    #[test]
    fn build_audit_outputs_reconstructs_creative_opportunity_slots() {
        let collected = CollectedPage {
            requested_url: "https://example.com/".to_string(),
            final_url: "https://example.com/".to_string(),
            page_title: Some("Example Publisher".to_string()),
            html: "<html><head></head></html>".to_string(),
            script_tags: Vec::new(),
            network_requests: vec![CollectedRequest {
                url: "https://securepubads.g.doubleclick.net/gampad/ads?\
                      iu_parts=123456789%2Cdesktop%2Chomepage%2Cleaderboard1\
                      &prev_iu_szs=970x250%7C4x1%7C620x366\
                      &dids=div-gpt-ad-leaderboard-1\
                      &prev_scp=baseDivId%3Ddiv-gpt-ad-leaderboard-1%26test%3Dprebid"
                    .to_string(),
                resource_type: Some("fetch".to_string()),
            }],
            gpt_slots: Vec::new(),
            warnings: Vec::new(),
        };

        let outputs = build_audit_outputs(&collected).expect("should build outputs");
        assert_eq!(outputs.ad_slot_count, 1, "should discover one slot");

        // The drafted config must be valid TOML with the reconstructed slot.
        let value =
            toml::from_str::<toml::Value>(&outputs.draft_config_toml).expect("draft parses");
        let creative = &value["creative_opportunities"];
        assert_eq!(creative["gam_network_id"].as_str(), Some("123456789"));
        let slot = &creative["slot"][0];
        assert_eq!(slot["id"].as_str(), Some("leaderboard-1"));
        assert_eq!(
            slot["gam_unit_path"].as_str(),
            Some("/123456789/desktop/homepage/leaderboard1")
        );
        assert_eq!(
            slot["formats"][0]["width"].as_integer(),
            Some(970),
            "should keep the 970x250 pixel size"
        );
        assert!(
            slot["providers"]["prebid"].is_table(),
            "prev_scp test=prebid should emit a prebid provider"
        );
    }

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

        let out = splice_creative_slots(existing, Some("222"), &header_rendered())
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
    fn splice_creates_section_when_absent() {
        // Config with no [creative_opportunities] at all — generate should append it.
        let existing = "[publisher]\ndomain = \"x\"\n\n[auction]\nenabled = true\n";

        let out = splice_creative_slots(existing, Some("222"), &header_rendered())
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
    fn splice_inserts_when_no_existing_slots() {
        let existing =
            "[creative_opportunities]\ngam_network_id = \"111\"\n\n[auction]\nenabled = true\n";

        let out = splice_creative_slots(existing, Some("222"), &header_rendered())
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
    fn default_page_pattern_uses_path_or_root() {
        assert_eq!(
            default_page_pattern(&Url::parse("https://x/news/story").expect("url")),
            "/news/story"
        );
        assert_eq!(
            default_page_pattern(&Url::parse("https://x/").expect("url")),
            "/"
        );
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
