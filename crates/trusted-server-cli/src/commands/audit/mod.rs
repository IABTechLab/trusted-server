mod analyzer;
pub(crate) mod browser_collector;
pub(crate) mod collector;

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use rand::RngCore as _;

use serde::Serialize;
use url::Url;

use crate::commands::audit::collector::AuditCollector;
use crate::commands::config::init::EXAMPLE_CONFIG;
use crate::error::{CliResult, cli_error, report_error};

use analyzer::{analyze_collected_page, extract_gtm_container_id};

/// Arguments for the `ts audit` command.
#[derive(Debug, clap::Args)]
pub(crate) struct AuditArgs {
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
    pub(crate) js_asset_proxy_candidate_count: usize,
}

#[derive(Debug, Clone)]
struct DraftConfig {
    toml: String,
    js_asset_proxy_candidate_count: usize,
}

#[derive(Debug, Clone)]
struct JsAssetProxySection {
    toml: String,
    candidate_count: usize,
}

#[derive(Debug, Default)]
struct JsAssetProxySkipCounts {
    first_party: usize,
    malformed_url: usize,
    non_https: usize,
    duplicate_url: usize,
    non_script: usize,
}

#[derive(Debug)]
struct JsAssetProxyCandidate<'a> {
    origin_url: String,
    integration: Option<&'a str>,
}

trait OpaqueAssetPathGenerator {
    fn next_path(&mut self) -> String;
}

#[derive(Debug, Default)]
struct RandomOpaqueAssetPathGenerator;

impl OpaqueAssetPathGenerator for RandomOpaqueAssetPathGenerator {
    fn next_path(&mut self) -> String {
        let mut bytes = [0_u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        format!("/assets/{}.js", lowercase_hex(&bytes))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuditOutputPlan {
    js_assets_path: Option<PathBuf>,
    config_path: Option<PathBuf>,
}

pub(crate) fn run_audit(
    args: &AuditArgs,
    collector: &dyn AuditCollector,
    out: &mut dyn Write,
) -> CliResult<()> {
    let target_url = parse_audit_url(&args.url)?;
    let plan = resolve_output_plan(args)?;
    let collected = collector.collect_page(&target_url)?;
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

fn resolve_output_plan(args: &AuditArgs) -> CliResult<AuditOutputPlan> {
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
    let mut path_generator = RandomOpaqueAssetPathGenerator;
    let draft_config =
        build_draft_config_with_generator(&final_url, &artifact, &mut path_generator)?;

    Ok(AuditOutputs {
        artifact,
        js_assets_toml,
        draft_config_toml: draft_config.toml,
        js_asset_proxy_candidate_count: draft_config.js_asset_proxy_candidate_count,
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
    let asset_proxy_note = if wrote_config && outputs.js_asset_proxy_candidate_count > 0 {
        format!(
            "{} disabled entries written to draft config",
            outputs.js_asset_proxy_candidate_count
        )
    } else if wrote_config {
        "none".to_string()
    } else {
        "not written (--no-config)".to_string()
    };
    writeln!(
        out,
        "Audited {}\nTitle: {}\nJS assets: {}\nThird-party assets: {}\nDetected integrations: {}\nJS asset proxy candidates: {}\nWrote: {}{}",
        outputs.artifact.audited_url,
        outputs
            .artifact
            .page_title
            .as_deref()
            .unwrap_or("<unknown>"),
        outputs.artifact.js_asset_count,
        outputs.artifact.third_party_asset_count,
        if integrations.is_empty() {
            "none".to_string()
        } else {
            integrations.join(", ")
        },
        asset_proxy_note,
        if written.is_empty() {
            "none".to_string()
        } else {
            written.join(", ")
        },
        draft_note
    )
    .map_err(|error| report_error(format!("failed to write command output: {error}")))
}

#[cfg(test)]
fn build_draft_config(target_url: &Url, artifact: &AuditArtifact) -> CliResult<String> {
    let mut path_generator = RandomOpaqueAssetPathGenerator;
    Ok(build_draft_config_with_generator(target_url, artifact, &mut path_generator)?.toml)
}

fn build_draft_config_with_generator(
    target_url: &Url,
    artifact: &AuditArtifact,
    path_generator: &mut dyn OpaqueAssetPathGenerator,
) -> CliResult<DraftConfig> {
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

    let asset_proxy_section = build_js_asset_proxy_section(artifact, path_generator)?;
    draft = replace_js_asset_proxy_section(&draft, &asset_proxy_section.toml)?;

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

    Ok(DraftConfig {
        toml: draft,
        js_asset_proxy_candidate_count: asset_proxy_section.candidate_count,
    })
}

fn build_js_asset_proxy_section(
    artifact: &AuditArtifact,
    path_generator: &mut dyn OpaqueAssetPathGenerator,
) -> CliResult<JsAssetProxySection> {
    let (candidates, skipped) = select_js_asset_proxy_candidates(artifact);
    let mut used_paths = BTreeSet::new();
    let mut toml = String::new();

    toml.push_str("[integrations.js_asset_proxy]\n");
    toml.push_str("enabled = false\n");
    toml.push_str("cache_ttl_seconds = 3600\n\n");
    toml.push_str("# Generated by `ts audit`; review before enabling.\n");
    toml.push_str(
        "# Audit note: some discovered scripts may be runtime-injected and may not appear\n",
    );
    toml.push_str(
        "# in origin HTML. JS Asset Proxy rewrites only exact script src values present in\n",
    );
    toml.push_str("# HTML processed by Trusted Server.\n");

    if candidates.is_empty() {
        toml.push_str(
            "# No eligible third-party HTTPS script assets were detected by `ts audit`.\n",
        );
    }

    for candidate in &candidates {
        let generated_path = generate_unique_asset_path(path_generator, &mut used_paths)?;
        toml.push('\n');
        toml.push_str("# Generated by `ts audit`; review before enabling.\n");
        if let Some(integration) = candidate.integration {
            let integration = sanitized_comment_value(integration);
            toml.push_str(&format!("# Detected integration: {integration}\n"));
            toml.push_str(&format!(
                "# Native integration may be preferable: [integrations.{integration}]\n"
            ));
        }
        toml.push_str("[[integrations.js_asset_proxy.assets]]\n");
        toml.push_str(&format!("path = {}\n", toml_quoted_string(&generated_path)));
        toml.push_str(&format!(
            "origin_url = {}\n",
            toml_quoted_string(&candidate.origin_url)
        ));
        toml.push_str("proxy = \"disabled\"\n");
    }

    append_js_asset_proxy_skip_comments(&mut toml, &skipped);
    toml.push('\n');

    Ok(JsAssetProxySection {
        toml,
        candidate_count: candidates.len(),
    })
}

fn select_js_asset_proxy_candidates(
    artifact: &AuditArtifact,
) -> (Vec<JsAssetProxyCandidate<'_>>, JsAssetProxySkipCounts) {
    let mut candidates = Vec::new();
    let mut skipped = JsAssetProxySkipCounts::default();
    let mut seen_origin_urls = BTreeSet::new();

    for asset in &artifact.assets {
        if asset.kind != "script" {
            skipped.non_script += 1;
            continue;
        }
        if asset.party != AssetParty::ThirdParty {
            skipped.first_party += 1;
            continue;
        }

        let Ok(url) = Url::parse(&asset.url) else {
            skipped.malformed_url += 1;
            continue;
        };
        if url.host_str().is_none() {
            skipped.malformed_url += 1;
            continue;
        }
        if url.scheme() != "https" {
            skipped.non_https += 1;
            continue;
        }

        let origin_url = url.to_string();
        if !seen_origin_urls.insert(origin_url.clone()) {
            skipped.duplicate_url += 1;
            continue;
        }

        candidates.push(JsAssetProxyCandidate {
            origin_url,
            integration: asset.integration.as_deref(),
        });
    }

    (candidates, skipped)
}

fn generate_unique_asset_path(
    path_generator: &mut dyn OpaqueAssetPathGenerator,
    used_paths: &mut BTreeSet<String>,
) -> CliResult<String> {
    for _ in 0..128 {
        let path = path_generator.next_path();
        if !is_valid_generated_asset_path(&path) {
            return cli_error(format!(
                "generated JS asset proxy path `{path}` is invalid; expected /assets/<hex>.js"
            ));
        }
        if used_paths.insert(path.clone()) {
            return Ok(path);
        }
    }

    cli_error("failed to generate a unique JS asset proxy path after 128 attempts")
}

fn is_valid_generated_asset_path(path: &str) -> bool {
    let Some(opaque_id) = path
        .strip_prefix("/assets/")
        .and_then(|value| value.strip_suffix(".js"))
    else {
        return false;
    };

    !opaque_id.is_empty()
        && opaque_id
            .chars()
            .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
}

fn replace_js_asset_proxy_section(document: &str, replacement: &str) -> CliResult<String> {
    let lines = document.lines().collect::<Vec<_>>();
    let start = lines
        .iter()
        .position(|line| line.trim() == "[integrations.js_asset_proxy]")
        .ok_or_else(|| {
            report_error(
                "failed to update starter config because section `[integrations.js_asset_proxy]` was not found",
            )
        })?;
    let mut end = start + 1;

    while end < lines.len() {
        let trimmed = lines[end].trim();
        if trimmed.starts_with('[')
            && trimmed.ends_with(']')
            && trimmed != "[[integrations.js_asset_proxy.assets]]"
        {
            break;
        }
        end += 1;
    }

    let mut output_lines = Vec::new();
    output_lines.extend_from_slice(&lines[..start]);
    output_lines.extend(replacement.trim_end_matches('\n').lines());
    if end < lines.len() {
        output_lines.push("");
    }
    output_lines.extend_from_slice(&lines[end..]);

    let mut output = output_lines.join("\n");
    if document.ends_with('\n') {
        output.push('\n');
    }
    Ok(output)
}

fn append_js_asset_proxy_skip_comments(toml: &mut String, skipped: &JsAssetProxySkipCounts) {
    if skipped.first_party == 0
        && skipped.malformed_url == 0
        && skipped.non_https == 0
        && skipped.duplicate_url == 0
        && skipped.non_script == 0
    {
        return;
    }

    toml.push('\n');
    toml.push_str("# Skipped JS Asset Proxy audit candidates:\n");
    append_skip_count(toml, skipped.first_party, "first-party script");
    append_skip_count(toml, skipped.malformed_url, "malformed script URL");
    append_skip_count(toml, skipped.non_https, "non-HTTPS third-party script");
    append_skip_count(toml, skipped.duplicate_url, "duplicate script URL");
    append_skip_count(toml, skipped.non_script, "non-script asset");
}

fn append_skip_count(toml: &mut String, count: usize, label: &str) {
    if count == 0 {
        return;
    }

    let plural = if count == 1 { "" } else { "s" };
    toml.push_str(&format!("# - {count} {label}{plural}\n"));
}

fn sanitized_comment_value(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect()
}

fn toml_quoted_string(value: &str) -> String {
    let mut quoted = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            ch if ch.is_control() => {
                write!(&mut quoted, "\\u{:04X}", ch as u32).expect("should write to string");
            }
            ch => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

fn lowercase_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
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
    use std::collections::VecDeque;

    use tempfile::TempDir;

    use super::*;
    use crate::commands::audit::collector::{CollectedPage, CollectedRequest, CollectedScriptTag};

    struct FakeCollector {
        collected: CollectedPage,
        calls: Cell<usize>,
    }

    struct FixedPathGenerator {
        paths: VecDeque<String>,
    }

    impl FixedPathGenerator {
        fn new(paths: &[&str]) -> Self {
            Self {
                paths: paths.iter().map(|path| (*path).to_string()).collect(),
            }
        }
    }

    impl OpaqueAssetPathGenerator for FixedPathGenerator {
        fn next_path(&mut self) -> String {
            self.paths
                .pop_front()
                .expect("should have a fixed generated asset path")
        }
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
        fn collect_page(&self, _target_url: &Url) -> CliResult<CollectedPage> {
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
            warnings: Vec::new(),
        }
    }

    fn audit_args(url: &str) -> AuditArgs {
        AuditArgs {
            url: url.to_string(),
            js_assets: None,
            config: None,
            no_js_assets: false,
            no_config: false,
            force: false,
        }
    }

    fn audited_asset(url: &str, party: AssetParty, integration: Option<&str>) -> AuditedAsset {
        AuditedAsset {
            kind: "script".to_string(),
            url: url.to_string(),
            host: Url::parse(url)
                .ok()
                .and_then(|parsed| parsed.host_str().map(str::to_string))
                .unwrap_or_default(),
            party,
            integration: integration.map(str::to_string),
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
    fn run_audit_writes_selected_outputs_and_summary() {
        let temp = TempDir::new().expect("should create temp dir");
        let js_assets = temp.path().join("audit/js-assets.toml");
        let config = temp.path().join("audit/trusted-server.toml");
        let args = AuditArgs {
            url: "https://publisher.example/page".to_string(),
            js_assets: Some(js_assets.clone()),
            config: Some(config.clone()),
            no_js_assets: false,
            no_config: false,
            force: false,
        };
        let collector = FakeCollector::new(collected_page());
        let mut out = Vec::new();

        run_audit(&args, &collector, &mut out).expect("should run audit");

        assert_eq!(collector.calls.get(), 1, "should collect page once");
        assert!(js_assets.exists(), "should write JS assets");
        assert!(config.exists(), "should write draft config");
        let summary = String::from_utf8(out).expect("summary should be UTF-8");
        assert!(summary.contains("Audited https://publisher.example/page"));
        assert!(summary.contains("Detected integrations: google_tag_manager, gpt"));
        assert!(summary.contains("Draft config: review before validation and push"));
    }

    #[test]
    fn run_audit_respects_no_config() {
        let temp = TempDir::new().expect("should create temp dir");
        let js_assets = temp.path().join("js-assets.toml");
        let mut args = audit_args("https://publisher.example/page");
        args.js_assets = Some(js_assets.clone());
        args.no_config = true;
        let collector = FakeCollector::new(collected_page());

        run_audit(&args, &collector, &mut Vec::new()).expect("should run audit");

        assert!(js_assets.exists(), "should write assets");
        assert!(
            !temp.path().join("trusted-server.toml").exists(),
            "should not write config"
        );
    }

    #[test]
    fn run_audit_respects_no_js_assets() {
        let temp = TempDir::new().expect("should create temp dir");
        let config = temp.path().join("trusted-server.toml");
        let mut args = audit_args("https://publisher.example/page");
        args.config = Some(config.clone());
        args.no_js_assets = true;
        let collector = FakeCollector::new(collected_page());
        let mut out = Vec::new();

        run_audit(&args, &collector, &mut out).expect("should run audit");

        assert!(config.exists(), "should write config");
        assert!(
            !temp.path().join("js-assets.toml").exists(),
            "should not write JS assets"
        );
        let summary = String::from_utf8(out).expect("summary should be UTF-8");
        assert!(summary.contains("Draft config: review before validation and push"));
    }

    #[test]
    fn run_audit_writes_collector_warnings_to_asset_artifact() {
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

        run_audit(&args, &collector, &mut Vec::new()).expect("should run audit");

        let artifact = fs::read_to_string(js_assets).expect("should read artifact");
        assert!(
            artifact.contains("results may be partial"),
            "should persist collector warning"
        );
    }

    #[test]
    fn run_audit_conflict_prevents_collection() {
        let temp = TempDir::new().expect("should create temp dir");
        let js_assets = temp.path().join("js-assets.toml");
        fs::write(&js_assets, "existing").expect("should write existing file");
        let mut args = audit_args("https://publisher.example/page");
        args.js_assets = Some(js_assets);
        args.no_config = true;
        let collector = FakeCollector::new(collected_page());

        let error = run_audit(&args, &collector, &mut Vec::new())
            .expect_err("should reject existing output");

        assert_eq!(collector.calls.get(), 0, "should not collect page");
        assert!(
            format!("{error:?}").contains("refusing to overwrite"),
            "should report overwrite conflict"
        );
    }

    #[test]
    fn build_draft_config_writes_disabled_js_asset_proxy_candidates() {
        let url = Url::parse("https://publisher.example/page").expect("should parse URL");
        let artifact = AuditArtifact {
            audited_url: url.to_string(),
            page_title: Some("Example".to_string()),
            js_asset_count: 2,
            third_party_asset_count: 2,
            detected_integrations: vec![DetectedIntegration {
                id: "gpt".to_string(),
                evidence: "https://securepubads.g.doubleclick.net/tag/js/gpt.js".to_string(),
            }],
            assets: vec![
                audited_asset(
                    "https://cdn.vendor.example/sdk.js",
                    AssetParty::ThirdParty,
                    None,
                ),
                audited_asset(
                    "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
                    AssetParty::ThirdParty,
                    Some("gpt"),
                ),
            ],
            warnings: Vec::new(),
        };
        let mut generator = FixedPathGenerator::new(&[
            "/assets/aaaaaaaaaaaaaaaaaaaaaaaa.js",
            "/assets/bbbbbbbbbbbbbbbbbbbbbbbb.js",
        ]);

        let draft = build_draft_config_with_generator(&url, &artifact, &mut generator)
            .expect("should build draft config");

        assert_eq!(
            draft.js_asset_proxy_candidate_count, 2,
            "should report generated disabled entries"
        );
        assert!(draft
            .toml
            .contains("[integrations.js_asset_proxy]\nenabled = false"));
        assert!(draft.toml.contains("/assets/aaaaaaaaaaaaaaaaaaaaaaaa.js"));
        assert!(draft.toml.contains("/assets/bbbbbbbbbbbbbbbbbbbbbbbb.js"));
        assert!(draft
            .toml
            .contains("origin_url = \"https://cdn.vendor.example/sdk.js\""));
        assert!(draft.toml.contains("proxy = \"disabled\""));
        assert!(draft.toml.contains("Detected integration: gpt"));
        assert!(draft
            .toml
            .contains("Native integration may be preferable: [integrations.gpt]"));
        assert!(
            !draft.toml.contains("example-vendor-loader"),
            "should remove starter-template placeholder asset"
        );
        toml::from_str::<toml::Value>(&draft.toml).expect("draft should parse as TOML");
    }

    #[test]
    fn generated_asset_proxy_paths_are_opaque() {
        let url = Url::parse("https://publisher.example/page").expect("should parse URL");
        let artifact = AuditArtifact {
            audited_url: url.to_string(),
            page_title: None,
            js_asset_count: 1,
            third_party_asset_count: 1,
            detected_integrations: Vec::new(),
            assets: vec![audited_asset(
                "https://cdn.vendor.example/vendor-loader.js",
                AssetParty::ThirdParty,
                None,
            )],
            warnings: Vec::new(),
        };
        let mut generator = FixedPathGenerator::new(&["/assets/0123456789abcdef01234567.js"]);

        let draft = build_draft_config_with_generator(&url, &artifact, &mut generator)
            .expect("should build draft config");
        let path_line = draft
            .toml
            .lines()
            .find(|line| line.starts_with("path = ") && line.contains("0123456789abcdef"))
            .expect("should include generated path");

        assert!(path_line.contains("/assets/0123456789abcdef01234567.js"));
        assert!(
            !path_line.contains("vendor")
                && !path_line.contains("cdn")
                && !path_line.contains("loader"),
            "generated path should not include vendor, domain, or filename semantics"
        );
    }

    #[test]
    fn asset_proxy_generation_deduplicates_and_summarizes_skips() {
        let url = Url::parse("https://publisher.example/page").expect("should parse URL");
        let artifact = AuditArtifact {
            audited_url: url.to_string(),
            page_title: None,
            js_asset_count: 4,
            third_party_asset_count: 3,
            detected_integrations: Vec::new(),
            assets: vec![
                audited_asset(
                    "https://cdn.vendor.example/sdk.js",
                    AssetParty::ThirdParty,
                    None,
                ),
                audited_asset(
                    "https://cdn.vendor.example/sdk.js",
                    AssetParty::ThirdParty,
                    None,
                ),
                audited_asset(
                    "https://publisher.example/app.js",
                    AssetParty::FirstParty,
                    None,
                ),
                audited_asset(
                    "http://cdn.vendor.example/insecure.js",
                    AssetParty::ThirdParty,
                    None,
                ),
            ],
            warnings: Vec::new(),
        };
        let mut generator = FixedPathGenerator::new(&["/assets/111111111111111111111111.js"]);

        let draft = build_draft_config_with_generator(&url, &artifact, &mut generator)
            .expect("should build draft config");

        assert_eq!(draft.js_asset_proxy_candidate_count, 1);
        assert_eq!(
            draft
                .toml
                .matches("[[integrations.js_asset_proxy.assets]]")
                .count(),
            1,
            "should only emit one candidate entry"
        );
        assert!(draft.toml.contains("# - 1 first-party script"));
        assert!(draft.toml.contains("# - 1 non-HTTPS third-party script"));
        assert!(draft.toml.contains("# - 1 duplicate script URL"));
    }

    #[test]
    fn asset_proxy_generation_with_no_candidates_removes_placeholder_asset() {
        let url = Url::parse("https://publisher.example/page").expect("should parse URL");
        let artifact = AuditArtifact {
            audited_url: url.to_string(),
            page_title: None,
            js_asset_count: 1,
            third_party_asset_count: 0,
            detected_integrations: Vec::new(),
            assets: vec![audited_asset(
                "https://publisher.example/app.js",
                AssetParty::FirstParty,
                None,
            )],
            warnings: Vec::new(),
        };
        let mut generator = FixedPathGenerator::new(&[]);

        let draft = build_draft_config_with_generator(&url, &artifact, &mut generator)
            .expect("should build draft config");

        assert_eq!(draft.js_asset_proxy_candidate_count, 0);
        assert!(draft
            .toml
            .contains("No eligible third-party HTTPS script assets"));
        assert!(
            !draft
                .toml
                .contains("[[integrations.js_asset_proxy.assets]]"),
            "should not emit asset array entries without candidates"
        );
        assert!(
            !draft.toml.contains("example-vendor-loader"),
            "should remove starter-template placeholder asset"
        );
        toml::from_str::<toml::Value>(&draft.toml).expect("draft should parse as TOML");
    }

    #[test]
    fn run_audit_summary_reports_written_asset_proxy_candidates() {
        let temp = TempDir::new().expect("should create temp dir");
        let config = temp.path().join("trusted-server.toml");
        let mut args = audit_args("https://publisher.example/page");
        args.config = Some(config);
        args.no_js_assets = true;
        let collector = FakeCollector::new(collected_page());
        let mut out = Vec::new();

        run_audit(&args, &collector, &mut out).expect("should run audit");

        let summary = String::from_utf8(out).expect("summary should be UTF-8");
        assert!(summary.contains("JS asset proxy candidates:"));
        assert!(summary.contains("disabled entries written to draft config"));
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

        let draft = build_draft_config(&url, &artifact).expect("should build draft config");

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

        let draft = build_draft_config(&url, &artifact).expect("should build draft config");

        assert!(draft.contains("[integrations.google_tag_manager]\nenabled = false"));
        assert!(draft.contains("Detected google_tag_manager"));
    }
}
