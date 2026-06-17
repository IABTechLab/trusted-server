mod analyzer;
pub(crate) mod browser_collector;
pub(crate) mod collector;

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use url::Url;

use crate::audit::collector::AuditCollector;
use crate::config_init::EXAMPLE_CONFIG;
use crate::error::{cli_error, report_error, CliResult};
use crate::run::AuditArgs;

use analyzer::{analyze_collected_page, extract_gtm_container_id};

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
    let draft_config_toml = build_draft_config(&final_url, &artifact)?;

    Ok(AuditOutputs {
        artifact,
        js_assets_toml,
        draft_config_toml,
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
        "Audited {}\nTitle: {}\nJS assets: {}\nThird-party assets: {}\nDetected integrations: {}\nWrote: {}{}",
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
        if written.is_empty() {
            "none".to_string()
        } else {
            written.join(", ")
        },
        draft_note
    )
    .map_err(|error| report_error(format!("failed to write command output: {error}")))
}

fn build_draft_config(target_url: &Url, artifact: &AuditArtifact) -> CliResult<String> {
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

    Ok(draft)
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
    use crate::audit::collector::{CollectedPage, CollectedRequest, CollectedScriptTag};

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
            html: r#"<html><head><title>Example Publisher</title><script src="https://securepubads.g.doubleclick.net/tag/js/gpt.js"></script></head></html>"#.to_string(),
            script_tags: vec![CollectedScriptTag {
                src: Some("https://www.googletagmanager.com/gtm.js?id=GTM-ABC123".to_string()),
                inline_text: None,
            }],
            network_requests: vec![CollectedRequest {
                url: "https://cdn.publisher.example/app.js".to_string(),
                method: "GET".to_string(),
                resource_type: Some("script".to_string()),
                status: None,
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
