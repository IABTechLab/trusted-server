mod analyzer;
mod browser_collector;
mod collector;
mod http_collector;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use error_stack::{Report, ResultExt};
use serde::Serialize;
use url::Url;

use crate::config::{STARTER_CONFIG_TEMPLATE, ensure_writable_path};
use crate::error::CliError;

use analyzer::{analyze_collected_page, extract_gtm_container_id};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AssetParty {
    FirstParty,
    ThirdParty,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuditedAsset {
    pub kind: String,
    pub url: String,
    pub host: String,
    pub party: AssetParty,
    pub integration: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DetectedIntegration {
    pub id: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuditArtifact {
    pub audited_url: String,
    pub page_title: Option<String>,
    pub js_asset_count: usize,
    pub third_party_asset_count: usize,
    pub detected_integrations: Vec<DetectedIntegration>,
    pub assets: Vec<AuditedAsset>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AuditOutputs {
    pub artifact: AuditArtifact,
    pub js_assets_toml: String,
    pub draft_config_toml: String,
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn analyze_html(target_url: &Url, html: &str) -> Result<AuditArtifact, Report<CliError>> {
    analyzer::analyze_html(target_url, html)
}

pub fn perform_audit(target_url: &Url) -> Result<AuditOutputs, Report<CliError>> {
    let collected = browser_collector::collect_page_via_browser(target_url)?;
    build_audit_outputs(&collected)
}

fn build_audit_outputs(
    collected: &collector::CollectedPage,
) -> Result<AuditOutputs, Report<CliError>> {
    let artifact = analyze_collected_page(collected)?;
    let final_url = collected.final_url().map_err(|error| {
        Report::new(CliError::Audit).attach(format!("invalid final URL: {error}"))
    })?;
    let js_assets_toml = toml::to_string_pretty(&artifact).change_context(CliError::Audit)?;
    let draft_config_toml = build_draft_config(&final_url, &artifact)?;

    Ok(AuditOutputs {
        artifact,
        js_assets_toml,
        draft_config_toml,
    })
}

pub fn write_audit_outputs(
    outputs: &AuditOutputs,
    js_assets_path: Option<&Path>,
    config_path: Option<&Path>,
    force: bool,
) -> Result<Vec<String>, Report<CliError>> {
    let mut written_paths = Vec::new();

    if let Some(path) = js_assets_path {
        ensure_writable_path(path, force)?;
        fs::write(path, &outputs.js_assets_toml).change_context(CliError::Io)?;
        written_paths.push(path.display().to_string());
    }

    if let Some(path) = config_path {
        ensure_writable_path(path, force)?;
        fs::write(path, &outputs.draft_config_toml).change_context(CliError::Io)?;
        written_paths.push(path.display().to_string());
    }

    Ok(written_paths)
}

fn build_draft_config(
    target_url: &Url,
    artifact: &AuditArtifact,
) -> Result<String, Report<CliError>> {
    let mut draft = STARTER_CONFIG_TEMPLATE.to_string();
    let host = target_url
        .host_str()
        .ok_or_else(|| Report::new(CliError::Audit).attach("audited URL is missing a host"))?;
    let origin = format!("{}://{}", target_url.scheme(), host);

    draft = replace_once(
        &draft,
        "domain = \"test-publisher.com\"",
        &format!("domain = \"{host}\""),
    )?;
    draft = replace_once(
        &draft,
        "cookie_domain = \".test-publisher.com\"",
        &format!("cookie_domain = \".{host}\""),
    )?;
    draft = replace_once(
        &draft,
        "origin_url = \"https://origin.test-publisher.com\"",
        &format!("origin_url = \"{origin}\""),
    )?;

    let detected = artifact
        .detected_integrations
        .iter()
        .map(|integration| integration.id.as_str())
        .collect::<BTreeSet<_>>();

    if detected.contains("gpt") {
        draft = replace_once(
            &draft,
            "[integrations.gpt]\nenabled = false",
            "[integrations.gpt]\nenabled = true",
        )?;
    }
    if detected.contains("didomi") {
        draft = replace_once(
            &draft,
            "[integrations.didomi]\nenabled = false",
            "[integrations.didomi]\nenabled = true",
        )?;
    }
    if detected.contains("datadome") {
        draft = replace_once(
            &draft,
            "[integrations.datadome]\nenabled = false",
            "[integrations.datadome]\nenabled = true",
        )?;
    }

    if let Some(gtm_id) = extract_gtm_container_id(artifact) {
        draft = replace_once(
            &draft,
            "[integrations.google_tag_manager]\nenabled = false\ncontainer_id = \"GTM-XXXXXX\"",
            &format!(
                "[integrations.google_tag_manager]\nenabled = true\ncontainer_id = \"{gtm_id}\""
            ),
        )?;
    }

    let inferred_only = detected
        .iter()
        .filter(|integration| {
            !matches!(
                **integration,
                "gpt" | "didomi" | "datadome" | "google_tag_manager"
            )
        })
        .copied()
        .collect::<Vec<_>>();

    if !inferred_only.is_empty() {
        draft.push_str("\n# Audit findings requiring manual review\n");
        for integration in inferred_only {
            draft.push_str(&format!(
                "# - Detected {integration}; review the corresponding [integrations.{integration}] section before enabling it.\n"
            ));
        }
    }

    Ok(draft)
}

fn replace_once(
    haystack: &str,
    needle: &str,
    replacement: &str,
) -> Result<String, Report<CliError>> {
    let Some(index) = haystack.find(needle) else {
        return Err(Report::new(CliError::Audit).attach(format!(
            "failed to update starter config because `{needle}` was not found"
        )));
    };

    let mut output = String::with_capacity(haystack.len() - needle.len() + replacement.len());
    output.push_str(&haystack[..index]);
    output.push_str(replacement);
    output.push_str(&haystack[index + needle.len()..]);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzes_html_and_detects_integrations() {
        let url = Url::parse("https://publisher.example/page").expect("should parse URL");
        let html = r#"
            <html>
                <head>
                    <title>Example Publisher</title>
                    <script src="https://www.googletagmanager.com/gtm.js?id=GTM-ABCD123"></script>
                    <script src="https://securepubads.g.doubleclick.net/tag/js/gpt.js"></script>
                </head>
            </html>
        "#;

        let artifact = analyze_html(&url, html).expect("should analyze HTML");

        assert_eq!(artifact.page_title.as_deref(), Some("Example Publisher"));
        assert_eq!(artifact.js_asset_count, 2, "should count script assets");
        assert!(
            artifact
                .detected_integrations
                .iter()
                .any(|integration| integration.id == "google_tag_manager"),
            "should detect GTM"
        );
        assert!(
            artifact
                .detected_integrations
                .iter()
                .any(|integration| integration.id == "gpt"),
            "should detect GPT"
        );
    }

    #[test]
    fn builds_draft_config_with_detected_integrations() {
        let url = Url::parse("https://publisher.example/page").expect("should parse URL");
        let artifact = AuditArtifact {
            audited_url: url.to_string(),
            page_title: Some("Example".to_string()),
            js_asset_count: 1,
            third_party_asset_count: 1,
            detected_integrations: vec![
                DetectedIntegration {
                    id: "google_tag_manager".to_string(),
                    evidence: "GTM-ABCD123".to_string(),
                },
                DetectedIntegration {
                    id: "gpt".to_string(),
                    evidence: "gpt".to_string(),
                },
            ],
            assets: vec![AuditedAsset {
                kind: "script".to_string(),
                url: "https://www.googletagmanager.com/gtm.js?id=GTM-ABCD123".to_string(),
                host: "www.googletagmanager.com".to_string(),
                party: AssetParty::ThirdParty,
                integration: Some("google_tag_manager".to_string()),
            }],
            warnings: Vec::new(),
        };

        let draft = build_draft_config(&url, &artifact).expect("should build draft config");

        assert!(
            draft.contains("domain = \"publisher.example\""),
            "should replace publisher domain"
        );
        assert!(
            draft.contains("enabled = true\ncontainer_id = \"GTM-ABCD123\""),
            "should enable GTM with detected container ID"
        );
        assert!(
            draft.contains("[integrations.gpt]\nenabled = true"),
            "should enable GPT"
        );
    }

    #[test]
    fn build_audit_outputs_uses_final_redirected_url_for_config() {
        let collected = collector::CollectedPage {
            requested_url: "http://publisher.example/page".to_string(),
            final_url: "https://www.publisher.example/landing".to_string(),
            page_title: Some("Example Publisher".to_string()),
            html: "<html><head></head></html>".to_string(),
            script_tags: Vec::new(),
            network_requests: Vec::new(),
            warnings: Vec::new(),
        };

        let outputs = build_audit_outputs(&collected).expect("should build audit outputs");

        assert_eq!(
            outputs.artifact.audited_url, "https://www.publisher.example/landing",
            "should report the final audited URL"
        );
        assert!(
            outputs
                .draft_config_toml
                .contains("domain = \"www.publisher.example\""),
            "should derive the config domain from the final URL"
        );
        assert!(
            outputs
                .draft_config_toml
                .contains("origin_url = \"https://www.publisher.example\""),
            "should derive the config origin from the final URL"
        );
    }
}
