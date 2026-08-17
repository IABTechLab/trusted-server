mod analyzer;
pub(crate) mod browser_collector;
pub(crate) mod collector;
mod crawl_plan;
mod evidence;
mod gpt_slots;
mod page_patterns;
mod slot_toml;
mod unit_template;
mod validate;

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use trusted_server_core::creative_opportunities::{
    CreativeOpportunitiesConfig, compile_page_pattern,
};
use url::Url;

use crate::commands::audit::generate::collector::AuditCollector;
use crate::commands::audit::generate::slot_toml::{
    render_slots, replace_key_in_section, resolve_network_id, splice_creative_slots, toml_string,
};
use crate::commands::config::init::EXAMPLE_CONFIG;
use crate::error::{CliResult, cli_error, report_error};

use analyzer::{analyze_collected_page, extract_gtm_container_id};

pub(crate) use crawl_plan::CrawlBudget;

/// Writes `contents` to `path` atomically: a same-directory temp file is
/// written and fsynced, then renamed over the target, then the directory entry
/// is fsynced.
///
/// A plain `fs::write` truncates the destination before writing, so a full disk
/// or an interrupted run would leave an operator's `trusted-server.toml` empty
/// or half-written. `rename` within a directory is atomic, so a reader sees
/// either the old file or the complete new one.
///
/// The target's existing permissions are carried onto the replacement, since
/// the temp file is created 0600 and the config may intentionally be broader.
///
/// # Errors
///
/// Returns the underlying I/O error when the temp file cannot be created,
/// written, synced, or renamed over `path`.
fn write_file_atomically(path: &Path, contents: &str) -> std::io::Result<()> {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));

    let mut temp = tempfile::Builder::new()
        .prefix(".ts-audit-")
        .tempfile_in(directory)?;
    temp.write_all(contents.as_bytes())?;
    temp.as_file().sync_all()?;
    if let Ok(metadata) = fs::metadata(path) {
        temp.as_file().set_permissions(metadata.permissions())?;
    }
    temp.persist(path).map_err(|error| error.error)?;

    // Best-effort durability for the rename itself. Opening a directory handle
    // is not portable (Windows rejects it), and the content is already safely
    // on disk either way, so a failure here is not worth failing the command.
    let _ = fs::File::open(directory).and_then(|handle| handle.sync_all());
    Ok(())
}

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
    #[arg(long = "cookie", value_name = "NAME=VALUE", value_parser = crate::commands::audit::parse_cookie)]
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
        write_file_atomically(path, &outputs.js_assets_toml).map_err(|error| {
            report_error(format!(
                "failed to write JS asset audit {}: {error}",
                path.display()
            ))
        })?;
        written_paths.push(path.display().to_string());
    }
    if let Some(path) = &plan.config_path {
        write_file_atomically(path, &outputs.draft_config_toml).map_err(|error| {
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
                &format!("gam_network_id = {}", toml_string(network_id)),
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
             id = {id}\n\
             div_id = {div_id}\n\
             gam_unit_path = {gam_unit_path}\n\
             page_patterns = [{page_pattern}]\n\
             formats = [{formats}]\n",
            id = toml_string(&slot.id),
            div_id = toml_string(&slot.div_id),
            gam_unit_path = toml_string(&slot.gam_unit_path),
            page_pattern = toml_string(page_pattern),
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
/// Everything one `ts audit ad-templates generate` invocation needs.
pub(crate) struct UpdateSlotsRequest<'a> {
    /// Page URL to start from; also bounds the crawl to its origin.
    pub(crate) url: &'a str,
    /// Operator config to rewrite in place.
    pub(crate) config_path: &'a Path,
    /// The config's current `[creative_opportunities]`, when it has one.
    pub(crate) existing_creative: Option<&'a CreativeOpportunitiesConfig>,
    /// Explicit `--page-pattern` values. When non-empty these apply to every
    /// slot and pattern inference is skipped entirely.
    pub(crate) page_patterns: &'a [String],
    /// Replace existing slots rather than merging into them.
    pub(crate) replace: bool,
    /// Cookies to carry into the crawl.
    pub(crate) cookies: &'a [(String, String)],
    /// Print the candidate instead of writing it.
    pub(crate) dry_run: bool,
    /// Crawl bounds.
    pub(crate) budget: crawl_plan::CrawlBudget,
}

/// Share of crawled pages that may yield no slots before the run is refused.
///
/// A bot-protection challenge serves an interstitial that loads fine and
/// contains no ad stack, so it looks like a page with no slots. Writing a config
/// from a crawl that was mostly challenges would silently narrow the operator's
/// slot set; refusing is the safer failure.
const MAX_EMPTY_PAGE_SHARE: f64 = 0.25;

/// Runs `ts audit ad-templates generate`: crawl the site's sections, reconcile
/// what each slot looked like across them, infer a `{section}` ad-unit template
/// where the evidence proves one, and rewrite the config's slot array in place.
///
/// # Errors
///
/// Returns an error when the config cannot be read, the root page cannot be
/// collected, no slots are discovered, too many pages came back empty, the
/// pages disagree about the GAM network id, or the resulting config would not
/// load.
pub(crate) fn run_update_slots(
    request: &UpdateSlotsRequest<'_>,
    collector: &dyn AuditCollector,
    out: &mut dyn Write,
) -> CliResult<()> {
    let target_url = parse_audit_url(request.url)?;
    let existing = fs::read_to_string(request.config_path).map_err(|error| {
        report_error(format!(
            "failed to read config {}: {error}",
            request.config_path.display()
        ))
    })?;

    let root = collector.collect_page(&target_url, request.cookies)?;
    let root_url = root.final_url().unwrap_or_else(|_| target_url.clone());
    let mut table = evidence::EvidenceTable::default();
    let mut notes = Vec::new();
    fold_collected(&mut table, &root_url, &root)?;

    // One page per section is enough: ad slots repeat per section, so the crawl
    // is sized by the publisher's taxonomy rather than its catalogue.
    let plan = crawl_plan::plan_crawl(&root_url, &root.links, &root.sitemap_locs, request.budget);
    notes.extend(plan.notes.iter().cloned());
    crawl_sections(collector, &plan, request.cookies, &mut table, &mut notes)?;

    if table.is_empty() {
        return cli_error("no ad-template slots were discovered on any crawled page");
    }
    guard_challenge_rate(&table)?;

    let discovered_network_id = table.network_id()?;
    let network_id = resolve_network_id(
        request.existing_creative,
        discovered_network_id.as_deref(),
        request.replace,
    );

    // Templating needs a network id to bind `{network_id}` against; without one
    // every path stays literal.
    let inference = network_id
        .as_deref()
        .map(|id| unit_template::infer_unit_templates(&table, id));
    if let Some(outcome) = &inference {
        notes.extend(outcome.diagnostics.iter().cloned());
    }
    let policy = inference
        .as_ref()
        .and_then(|outcome| outcome.policy.clone());

    let slots = build_render_slots(&table, inference.as_ref(), policy.as_ref(), request)?;
    let merged = slot_toml::merge_render_slots(request.existing_creative, slots, request.replace);
    let rendered_slots = render_slots(&merged);
    let updated = splice_creative_slots(
        &existing,
        &slot_toml::CreativeSectionKeys {
            network_id: network_id.as_deref(),
            section_root: policy.as_ref().map(|policy| policy.section_root.as_str()),
            section_segment: policy.as_ref().map(|policy| policy.section_segment),
        },
        &rendered_slots,
    )?;

    // Everything above is derived from a live, page-controlled ad stack, so the
    // candidate has to clear the runtime's own load path before it can replace
    // the operator's file. This runs on the dry-run path too — otherwise "the
    // preview looked fine" would not be evidence that the config loads.
    notes.extend(validate::check_candidate(&updated, &existing)?);

    for note in &notes {
        writeln!(out, "note: {note}")
            .map_err(|error| report_error(format!("failed to write command output: {error}")))?;
    }
    if policy.is_some() {
        writeln!(
            out,
            "note: this config now uses a {{section}} ad-unit template. Deploy a \
             template-aware binary BEFORE pushing it, and do not roll that binary \
             back while this config is live — an older binary rejects the whole \
             config and serves an error on every route."
        )
        .map_err(|error| report_error(format!("failed to write command output: {error}")))?;
    }

    if request.dry_run {
        writeln!(out, "{updated}")
            .map_err(|error| report_error(format!("failed to write preview: {error}")))?;
        return Ok(());
    }
    write_file_atomically(request.config_path, &updated).map_err(|error| {
        report_error(format!(
            "failed to write config {}: {error}",
            request.config_path.display()
        ))
    })?;
    writeln!(
        out,
        "Wrote {} slot(s) to {} ({} slot(s) seen across {} page(s))",
        merged.len(),
        request.config_path.display(),
        table.slot_count(),
        table.pages().len(),
    )
    .map_err(|error| report_error(format!("failed to write command output: {error}")))
}

/// Discovers a collected page's slots and folds them into `table`.
fn fold_collected(
    table: &mut evidence::EvidenceTable,
    url: &Url,
    collected: &collector::CollectedPage,
) -> CliResult<()> {
    let artifact = analyze_collected_page(collected)?;
    let page_has_prebid = artifact
        .detected_integrations
        .iter()
        .any(|integration| integration.id == "prebid");
    let discovered = gpt_slots::discover_gpt_slots(
        &collected.gpt_slots,
        &collected.network_requests,
        page_has_prebid,
    );
    table.fold_page(url.path(), &discovered);
    Ok(())
}

/// Walks the planned section pages, folding each into `table`.
///
/// A page that fails to collect is recorded as a note rather than aborting: on a
/// multi-section crawl one blocked or slow page should not discard the sections
/// that did work. The empty-page guard afterwards catches the case where enough
/// of them failed that the result is untrustworthy.
fn crawl_sections(
    collector: &dyn AuditCollector,
    plan: &crawl_plan::CrawlPlan,
    cookies: &[(String, String)],
    table: &mut evidence::EvidenceTable,
    notes: &mut Vec<String>,
) -> CliResult<()> {
    let targets = plan.targets();
    if targets.is_empty() {
        notes.push(
            "no additional site sections were discovered, so only the requested page was \
             audited; pass explicit --page-pattern values or more URLs to widen coverage"
                .to_string(),
        );
        return Ok(());
    }

    let mut fold_error = None;
    collector.collect_pages(&targets, cookies, &mut |url, collected| {
        match collected {
            Ok(page) => {
                let final_url = page.final_url().unwrap_or_else(|_| url.clone());
                if let Err(error) = fold_collected(table, &final_url, &page) {
                    fold_error = Some(error);
                    return Ok(collector::ControlFlow::Stop);
                }
            }
            Err(error) => notes.push(format!("skipped `{url}`: {error}")),
        }
        Ok(collector::ControlFlow::Continue)
    })?;
    match fold_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Refuses a crawl where too many pages produced no slots.
fn guard_challenge_rate(table: &evidence::EvidenceTable) -> CliResult<()> {
    let total = table.pages().len();
    let empty = table.empty_pages().len();
    if total == 0 || (empty as f64) <= (total as f64) * MAX_EMPTY_PAGE_SHARE {
        return Ok(());
    }
    let blocked: Vec<&str> = table.empty_pages().iter().map(String::as_str).collect();
    cli_error(format!(
        "{empty} of {total} crawled page(s) produced no ad slots ({}), which usually means \
         bot protection served a challenge instead of the real page. Refusing to write a \
         config from partial evidence; re-run with a valid --cookie for the origin",
        blocked.join(", ")
    ))
}

/// Turns the evidence table into slots ready to render.
fn build_render_slots(
    table: &evidence::EvidenceTable,
    inference: Option<&unit_template::InferenceOutcome>,
    policy: Option<&unit_template::SectionPolicy>,
    request: &UpdateSlotsRequest<'_>,
) -> CliResult<Vec<slot_toml::RenderSlot>> {
    // Explicit `--page-pattern` values are an operator override: they apply to
    // every slot and disable inference from observed paths entirely.
    let explicit = !request.page_patterns.is_empty();
    if explicit {
        validate_page_patterns(request.page_patterns)?;
    }
    let section_segment = policy.map_or(0, |policy| policy.section_segment);

    let mut slots = Vec::with_capacity(table.slot_count());
    for slot in table.slots() {
        let patterns = if explicit {
            request.page_patterns.to_vec()
        } else {
            let derived = page_patterns::patterns_for_paths(slot.paths(), section_segment);
            validate_page_patterns(&derived)?;
            derived
        };
        let unit_path = match inference.and_then(|outcome| outcome.decision(&slot.div_id)) {
            Some(unit_template::SlotDecision::Template(template)) => Some(template.clone()),
            Some(unit_template::SlotDecision::Literal(path)) => Some(path.clone()),
            // Refused: write the slot without a path rather than a wrong one.
            Some(unit_template::SlotDecision::Refuse { .. }) | None => None,
        };
        slots.push(slot_toml::RenderSlot::from_evidence(
            &slot.id,
            &slot.div_id,
            unit_path,
            slot.formats.iter().copied(),
            patterns,
            slot.has_prebid,
        ));
    }
    Ok(slots)
}
/// Rejects any page pattern the runtime's glob compiler would not accept.
///
/// Uses [`compile_page_pattern`] so the accepted set is exactly what
/// `CreativeOpportunitySlot::compile_patterns` accepts at startup, including the
/// `**`→`*` normalisation. All patterns are reported at once so an operator
/// passing several `--page-pattern` values fixes them in one pass.
///
/// # Errors
///
/// Returns a user-facing error listing every pattern that does not compile.
fn validate_page_patterns(patterns: &[String]) -> CliResult<()> {
    let invalid: Vec<String> = patterns
        .iter()
        .filter_map(|pattern| compile_page_pattern(pattern).err())
        .collect();
    if invalid.is_empty() {
        return Ok(());
    }
    cli_error(format!(
        "refusing to write invalid page pattern(s): {}",
        invalid.join("; ")
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use tempfile::TempDir;

    use super::*;
    use crate::app_config::AppConfigArgs;
    use crate::commands::audit::generate::collector::{
        CollectedPage, CollectedRequest, CollectedScriptTag,
    };
    use crate::commands::config::init::EXAMPLE_CONFIG;

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

    /// A collector serving a distinct page per URL, recording the crawl order.
    struct SiteCollector {
        pages: std::collections::HashMap<String, CollectedPage>,
        visited: std::cell::RefCell<Vec<String>>,
    }

    impl SiteCollector {
        fn new(pages: Vec<(&str, CollectedPage)>) -> Self {
            Self {
                pages: pages
                    .into_iter()
                    .map(|(url, page)| (url.to_string(), page))
                    .collect(),
                visited: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl AuditCollector for SiteCollector {
        fn collect_page(
            &self,
            target_url: &Url,
            _cookies: &[(String, String)],
        ) -> CliResult<CollectedPage> {
            self.visited.borrow_mut().push(target_url.to_string());
            self.pages
                .get(target_url.as_str())
                .cloned()
                .ok_or_else(|| report_error(format!("no fake page for {target_url}")))
        }
    }

    /// Builds a page carrying one GPT slot plus same-origin nav links.
    fn site_page(url: &str, unit_path: &str, nav_paths: &[&str]) -> CollectedPage {
        let mut page = collected_page();
        page.requested_url = url.to_string();
        page.final_url = url.to_string();
        page.gpt_slots = vec![collector::CollectedGptSlot {
            gam_unit_path: unit_path.to_string(),
            div_id: "ad-header-0".to_string(),
            sizes: vec![(728, 90)],
        }];
        page.links = nav_paths
            .iter()
            .map(|path| collector::CollectedLink {
                url: format!("https://publisher.example{path}"),
                in_nav: true,
            })
            .collect();
        page
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
            links: Vec::new(),
            sitemap_locs: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// A collected page carrying one discoverable GPT slot, for `run_update_slots`.
    fn collected_page_with_header_slot() -> CollectedPage {
        let mut collected = collected_page();
        collected.requested_url = "https://publisher.example/".to_string();
        collected.final_url = "https://publisher.example/".to_string();
        collected.gpt_slots = vec![collector::CollectedGptSlot {
            gam_unit_path: "/222/homepage/header".to_string(),
            div_id: "div-gpt-ad-header".to_string(),
            sizes: vec![(728, 90)],
        }];
        collected
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
            links: Vec::new(),
            sitemap_locs: Vec::new(),
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

    #[test]
    fn render_discovered_slots_escapes_page_controlled_strings() {
        // Slot fields scraped from the live page must be escaped so a quote
        // cannot inject TOML into the drafted config.
        let registry = vec![collector::CollectedGptSlot {
            gam_unit_path: "/222/homepage/head\"er".to_string(),
            div_id: "div-gpt-ad-head\"er".to_string(),
            sizes: vec![(728, 90)],
        }];
        let slots = gpt_slots::discover_gpt_slots(&registry, &[], false);
        let url = Url::parse("https://publisher.example/").expect("should parse URL");

        let rendered = render_discovered_slots(&url, &slots);

        let value = toml::from_str::<toml::Value>(&rendered)
            .expect("should render valid TOML despite embedded quotes");
        let slot = &value["creative_opportunities"]["slot"][0];
        assert_eq!(
            slot["div_id"].as_str(),
            Some("div-gpt-ad-head\"er"),
            "should keep the quote as data, not TOML syntax"
        );
    }

    #[test]
    fn update_slots_defaults_pattern_to_final_url_after_redirect() {
        let temp = TempDir::new().expect("should create temp dir");
        let config_path = temp.path().join("trusted-server.toml");
        fs::write(
            &config_path,
            "[creative_opportunities]\ngam_network_id = \"111\"\n",
        )
        .expect("should write config");
        // The requested URL redirects; slots are scraped from the final page.
        let mut collected = collected_page();
        collected.requested_url = "https://publisher.example/".to_string();
        collected.final_url = "https://publisher.example/news/story".to_string();
        collected.gpt_slots = vec![collector::CollectedGptSlot {
            gam_unit_path: "/222/homepage/header".to_string(),
            div_id: "div-gpt-ad-header".to_string(),
            sizes: vec![(728, 90)],
        }];
        let collector = FakeCollector::new(collected);
        let mut out = Vec::new();

        run_update_slots(
            &UpdateSlotsRequest {
                url: "https://publisher.example/",
                config_path: &config_path,
                existing_creative: None,
                page_patterns: &[],
                replace: false,
                cookies: &[],
                dry_run: false,
                budget: CrawlBudget::default(),
            },
            &collector,
            &mut out,
        )
        .expect("should update slots");

        let written = fs::read_to_string(&config_path).expect("should read config");
        let value = toml::from_str::<toml::Value>(&written).expect("valid TOML");
        let patterns: Vec<&str> = value["creative_opportunities"]["slot"][0]["page_patterns"]
            .as_array()
            .expect("page_patterns array")
            .iter()
            .map(|entry| entry.as_str().expect("pattern string"))
            .collect();
        // Patterns come from the post-redirect path: had the requested `/` been
        // used, this would be `["/"]`. They now cover the whole section rather
        // than only the one article that happened to be scraped.
        assert_eq!(
            patterns,
            ["/news", "/news/*"],
            "should derive section patterns from the post-redirect path"
        );
    }

    #[test]
    fn update_slots_rejects_invalid_page_pattern_without_touching_config() {
        let temp = TempDir::new().expect("should create temp dir");
        let config_path = temp.path().join("trusted-server.toml");
        let original = "[creative_opportunities]\ngam_network_id = \"111\"\n";
        fs::write(&config_path, original).expect("should write config");
        let collector = FakeCollector::new(collected_page_with_header_slot());
        let mut out = Vec::new();

        let error = run_update_slots(
            &UpdateSlotsRequest {
                url: "https://publisher.example/",
                config_path: &config_path,
                existing_creative: None,
                page_patterns: &["[".to_string()],
                replace: false,
                cookies: &[],
                dry_run: false,
                budget: CrawlBudget::default(),
            },
            &collector,
            &mut out,
        )
        .expect_err("should reject an invalid glob");

        assert!(
            format!("{error:?}").contains("page pattern '['"),
            "error should name the offending pattern, got {error:?}"
        );
        assert_eq!(
            fs::read_to_string(&config_path).expect("should read config"),
            original,
            "a rejected pattern must leave the operator config untouched"
        );
    }

    #[test]
    fn update_slots_accepts_double_star_pattern_like_the_runtime() {
        // `/20**` does not compile directly but the runtime normalises it to
        // `/20*`; validation must accept exactly what the runtime accepts.
        let temp = TempDir::new().expect("should create temp dir");
        let config_path = temp.path().join("trusted-server.toml");
        fs::write(
            &config_path,
            "[creative_opportunities]\ngam_network_id = \"111\"\n",
        )
        .expect("should write config");
        let collector = FakeCollector::new(collected_page_with_header_slot());
        let mut out = Vec::new();

        run_update_slots(
            &UpdateSlotsRequest {
                url: "https://publisher.example/",
                config_path: &config_path,
                existing_creative: None,
                page_patterns: &["/20**".to_string()],
                replace: false,
                cookies: &[],
                dry_run: false,
                budget: CrawlBudget::default(),
            },
            &collector,
            &mut out,
        )
        .expect("should accept a runtime-normalisable pattern");

        let written = fs::read_to_string(&config_path).expect("should read config");
        let value = toml::from_str::<toml::Value>(&written).expect("valid TOML");
        assert_eq!(
            value["creative_opportunities"]["slot"][0]["page_patterns"][0].as_str(),
            Some("/20**")
        );
    }

    #[test]
    fn update_slots_write_replaces_the_config_without_leaving_temp_files() {
        let temp = TempDir::new().expect("should create temp dir");
        let config_path = temp.path().join("trusted-server.toml");
        fs::write(
            &config_path,
            "[creative_opportunities]\ngam_network_id = \"111\"\n",
        )
        .expect("should write config");
        let collector = FakeCollector::new(collected_page_with_header_slot());
        let mut out = Vec::new();

        run_update_slots(
            &UpdateSlotsRequest {
                url: "https://publisher.example/",
                config_path: &config_path,
                existing_creative: None,
                page_patterns: &[],
                replace: false,
                cookies: &[],
                dry_run: false,
                budget: CrawlBudget::default(),
            },
            &collector,
            &mut out,
        )
        .expect("should update slots");

        let entries: Vec<String> = fs::read_dir(temp.path())
            .expect("should read temp dir")
            .map(|entry| {
                entry
                    .expect("should read entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            entries,
            ["trusted-server.toml"],
            "the atomic write should leave no stray temp file behind"
        );
        let written = fs::read_to_string(&config_path).expect("should read config");
        toml::from_str::<toml::Value>(&written).expect("rewritten config is valid TOML");
    }

    /// A full, loadable config with real secrets substituted, so the write-side
    /// validation gate is live rather than downgraded by a broken baseline.
    fn loadable_config() -> String {
        EXAMPLE_CONFIG
            .replace(
                "replace-with-admin-password-32-bytes",
                "test-admin-password-32-bytes-minimum",
            )
            .replace(
                "trusted-server-placeholder-secret",
                "test-ec-passphrase-32-bytes-minimum",
            )
            .replace(
                "change-me-proxy-secret",
                "test-proxy-secret-32-bytes-minimum",
            )
    }

    #[test]
    fn a_crawl_writes_a_section_template_and_per_section_patterns() {
        // The end-to-end payoff: crawl sections, reconcile the slot across them,
        // infer `{section}`, and write a config the runtime loads.
        let temp = TempDir::new().expect("should create temp dir");
        let config_path = temp.path().join("trusted-server.toml");
        fs::write(&config_path, loadable_config()).expect("should write config");

        let nav = ["/news", "/deals"];
        let collector = SiteCollector::new(vec![
            (
                "https://publisher.example/",
                site_page(
                    "https://publisher.example/",
                    "/123456789/site/homepage",
                    &nav,
                ),
            ),
            (
                "https://publisher.example/news",
                site_page(
                    "https://publisher.example/news",
                    "/123456789/site/news",
                    &nav,
                ),
            ),
            (
                "https://publisher.example/deals",
                site_page(
                    "https://publisher.example/deals",
                    "/123456789/site/deals",
                    &nav,
                ),
            ),
        ]);
        let mut out = Vec::new();

        run_update_slots(
            &UpdateSlotsRequest {
                url: "https://publisher.example/",
                config_path: &config_path,
                existing_creative: None,
                page_patterns: &[],
                replace: false,
                cookies: &[],
                dry_run: false,
                budget: CrawlBudget::default(),
            },
            &collector,
            &mut out,
        )
        .expect("should crawl and update slots");

        let written = fs::read_to_string(&config_path).expect("should read config");
        let value = toml::from_str::<toml::Value>(&written).expect("valid TOML");
        let creative = &value["creative_opportunities"];

        assert_eq!(
            creative["section_root"].as_str(),
            Some("homepage"),
            "the unvisited-section fallback should come from the root page"
        );
        assert_eq!(creative["section_segment"].as_integer(), Some(0));
        let slot = &creative["slot"][0];
        assert_eq!(
            slot["gam_unit_path"].as_str(),
            Some("/{network_id}/site/{section}"),
            "the varying segment should become a template"
        );
        let patterns: Vec<&str> = slot["page_patterns"]
            .as_array()
            .expect("patterns array")
            .iter()
            .map(|entry| entry.as_str().expect("pattern"))
            .collect();
        assert_eq!(
            patterns,
            ["/", "/deals", "/deals/*", "/news", "/news/*"],
            "each witnessed section should contribute both halves of its pair"
        );

        // The whole point of the gate: what was written must actually load.
        trusted_server_core::settings::Settings::from_toml(&written)
            .expect("generated config must load through the runtime path");

        let report = String::from_utf8(out).expect("utf8 output");
        assert!(
            report.contains("Deploy a template-aware binary BEFORE pushing"),
            "a templated config must warn about the rollback contract, got:\n{report}"
        );
    }

    #[test]
    fn a_crawl_refuses_when_most_pages_are_challenged() {
        // Bot protection serves an interstitial that loads fine and has no ad
        // stack, so it looks like a page with no slots. Writing from that would
        // silently narrow the operator's slot set.
        let temp = TempDir::new().expect("should create temp dir");
        let config_path = temp.path().join("trusted-server.toml");
        let original = loadable_config();
        fs::write(&config_path, &original).expect("should write config");

        let nav = ["/news", "/deals"];
        let mut blocked_news = site_page("https://publisher.example/news", "/123456789/x", &nav);
        blocked_news.gpt_slots.clear();
        let mut blocked_deals = site_page("https://publisher.example/deals", "/123456789/x", &nav);
        blocked_deals.gpt_slots.clear();
        let collector = SiteCollector::new(vec![
            (
                "https://publisher.example/",
                site_page(
                    "https://publisher.example/",
                    "/123456789/site/homepage",
                    &nav,
                ),
            ),
            ("https://publisher.example/news", blocked_news),
            ("https://publisher.example/deals", blocked_deals),
        ]);
        let mut out = Vec::new();

        let error = run_update_slots(
            &UpdateSlotsRequest {
                url: "https://publisher.example/",
                config_path: &config_path,
                existing_creative: None,
                page_patterns: &[],
                replace: false,
                cookies: &[],
                dry_run: false,
                budget: CrawlBudget::default(),
            },
            &collector,
            &mut out,
        )
        .expect_err("a mostly-challenged crawl should refuse");

        assert!(
            format!("{error:?}").contains("bot protection"),
            "the error should name the likely cause, got {error:?}"
        );
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            original,
            "a refused run must leave the config untouched"
        );
    }

    #[test]
    fn max_pages_one_restores_single_page_behavior() {
        let temp = TempDir::new().expect("should create temp dir");
        let config_path = temp.path().join("trusted-server.toml");
        fs::write(&config_path, loadable_config()).expect("should write config");

        let nav = ["/news", "/deals"];
        let collector = SiteCollector::new(vec![(
            "https://publisher.example/",
            site_page(
                "https://publisher.example/",
                "/123456789/site/homepage",
                &nav,
            ),
        )]);
        let mut out = Vec::new();

        run_update_slots(
            &UpdateSlotsRequest {
                url: "https://publisher.example/",
                config_path: &config_path,
                existing_creative: None,
                page_patterns: &[],
                replace: false,
                cookies: &[],
                dry_run: false,
                budget: CrawlBudget {
                    max_sections: 8,
                    max_pages: 1,
                },
            },
            &collector,
            &mut out,
        )
        .expect("should update from the single page");

        assert_eq!(
            collector.visited.borrow().len(),
            1,
            "max_pages = 1 must not crawl beyond the requested page"
        );
        let written = fs::read_to_string(&config_path).expect("read config");
        let value = toml::from_str::<toml::Value>(&written).expect("valid TOML");
        assert!(
            value["creative_opportunities"]
                .get("section_root")
                .is_none(),
            "one page cannot witness a section, so no rollback-fatal key may be written"
        );
        assert_eq!(
            value["creative_opportunities"]["slot"][0]["gam_unit_path"].as_str(),
            Some("/123456789/site/homepage"),
            "a single page keeps the literal path"
        );
    }

    #[test]
    fn generated_config_loads_through_the_runtime_settings_path() {
        // The end-to-end contract: whatever `generate` writes must survive the
        // same load path the adapter runs at startup. An unloadable config is a
        // full-site outage once pushed, not a degraded ad stack.
        let temp = TempDir::new().expect("should create temp dir");
        let config_path = temp.path().join("trusted-server.toml");
        let baseline = loadable_config();
        trusted_server_core::settings::Settings::from_toml(&baseline)
            .expect("test baseline must itself be loadable or the gate is not exercised");
        fs::write(&config_path, &baseline).expect("should write config");
        let collector = FakeCollector::new(collected_page_with_header_slot());
        let mut out = Vec::new();

        run_update_slots(
            &UpdateSlotsRequest {
                url: "https://publisher.example/",
                config_path: &config_path,
                existing_creative: None,
                page_patterns: &[],
                replace: false,
                cookies: &[],
                dry_run: false,
                budget: CrawlBudget::default(),
            },
            &collector,
            &mut out,
        )
        .expect("should update slots");

        let written = fs::read_to_string(&config_path).expect("should read config");
        let settings = trusted_server_core::settings::Settings::from_toml(&written)
            .expect("generated config must load through the runtime path");
        let creative = settings
            .creative_opportunities
            .expect("generated config should carry creative opportunities");
        assert_eq!(
            creative.slot.len(),
            1,
            "the discovered slot should be present after a real load"
        );
        assert_eq!(
            creative.slot[0].div_id.as_deref(),
            Some("div-gpt-ad-header")
        );
    }

    #[test]
    fn update_slots_dry_run_does_not_persist_environment_overlay_config() {
        let temp = TempDir::new().expect("should create temp dir");
        let manifest_path = temp.path().join("edgezero.toml");
        let config_path = temp.path().join("trusted-server.toml");
        fs::write(&manifest_path, "[app]\nname = \"trusted-server\"\n")
            .expect("should write manifest");
        let config = EXAMPLE_CONFIG
            .replace(
                "replace-with-admin-password-32-bytes",
                "test-admin-password-32-bytes-minimum",
            )
            .replace(
                "trusted-server-placeholder-secret",
                "test-ec-passphrase-32-bytes-minimum",
            )
            .replace(
                "change-me-proxy-secret",
                "test-proxy-secret-32-bytes-minimum",
            );
        let config = format!(
            "{config}\n\
             [[creative_opportunities.slot]]\n\
             id = \"file-only\"\n\
             div_id = \"div-gpt-ad-file\"\n\
             gam_unit_path = \"/123456789/homepage/file\"\n\
             page_patterns = [\"/\"]\n\
             formats = [{{ width = 728, height = 90 }}]\n"
        );
        fs::write(&config_path, config).expect("should write config");
        let args = AppConfigArgs {
            app_config: Some(config_path.clone()),
            manifest: manifest_path,
            no_env: false,
        };

        temp_env::with_var(
            "TRUSTED_SERVER__CREATIVE_OPPORTUNITIES__GAM_NETWORK_ID",
            Some("987654321"),
            || {
                let effective = crate::app_config::load_settings(&args)
                    .expect("should load effective settings");
                assert_eq!(
                    effective
                        .settings
                        .creative_opportunities
                        .as_ref()
                        .expect("should have creative config")
                        .gam_network_id,
                    "987654321",
                    "test environment should override the network id"
                );
                let loaded = crate::app_config::load_file_settings(&args)
                    .expect("should load file-only settings");
                let mut collected = collected_page();
                collected.gpt_slots = vec![collector::CollectedGptSlot {
                    gam_unit_path: "/123456789/homepage/file".to_string(),
                    div_id: "div-gpt-ad-file".to_string(),
                    sizes: vec![(728, 90)],
                }];
                let collector = FakeCollector::new(collected);
                let mut out = Vec::new();

                run_update_slots(
                    &UpdateSlotsRequest {
                        url: "https://publisher.example/",
                        config_path: &loaded.app_config_path,
                        existing_creative: loaded.settings.creative_opportunities.as_ref(),
                        page_patterns: &[],
                        replace: false,
                        cookies: &[],
                        dry_run: true,
                        budget: CrawlBudget::default(),
                    },
                    &collector,
                    &mut out,
                )
                .expect("should render dry-run update");

                let output = String::from_utf8(out).expect("output should be UTF-8");
                assert!(
                    output.contains("id = \"file-only\""),
                    "dry run should preserve the file-backed slot"
                );
                assert!(
                    output.contains("gam_network_id = \"123456789\""),
                    "dry run should preserve the file-backed network id"
                );
                assert!(
                    !output.contains("987654321"),
                    "dry run must not persist environment-only config"
                );
            },
        );
    }
}
