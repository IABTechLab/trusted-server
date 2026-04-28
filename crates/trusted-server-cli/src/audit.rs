use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use error_stack::{Report, ResultExt};
use regex::Regex;
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use serde::Serialize;
use url::Url;

use crate::config::{STARTER_CONFIG_TEMPLATE, ensure_writable_path};
use crate::error::CliError;

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

pub fn perform_audit(target_url: &Url) -> Result<AuditOutputs, Report<CliError>> {
    let client = Client::builder()
        .user_agent("trusted-server-cli/0.1")
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .change_context(CliError::Audit)?;

    let response = client
        .get(target_url.clone())
        .send()
        .change_context(CliError::Audit)
        .attach(format!("failed to load `{}`", target_url))?;

    if !response.status().is_success() {
        return Err(Report::new(CliError::Audit)
            .attach(format!("audit request returned HTTP {}", response.status())));
    }

    let body = response.text().change_context(CliError::Audit)?;
    let artifact = analyze_html(target_url, &body)?;
    let js_assets_toml = toml::to_string_pretty(&artifact).change_context(CliError::Audit)?;
    let draft_config_toml = build_draft_config(target_url, &artifact)?;

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

pub fn analyze_html(target_url: &Url, html: &str) -> Result<AuditArtifact, Report<CliError>> {
    let document = Html::parse_document(html);
    let title_selector = Selector::parse("title").expect("should parse title selector");
    let script_selector = Selector::parse("script").expect("should parse script selector");
    let title = document
        .select(&title_selector)
        .next()
        .map(|element| {
            element
                .text()
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_string()
        })
        .filter(|title| !title.is_empty());

    let mut assets = Vec::new();
    let mut integrations = BTreeMap::<String, String>::new();
    let mut warnings = Vec::new();

    for element in document.select(&script_selector) {
        if let Some(src) = element.value().attr("src") {
            if let Ok(asset_url) = target_url.join(src) {
                let host = asset_url.host_str().unwrap_or_default().to_string();
                let integration = detect_integration_from_url(&asset_url);
                if let Some(integration_id) = &integration {
                    integrations
                        .entry(integration_id.clone())
                        .or_insert_with(|| asset_url.as_str().to_string());
                }
                assets.push(AuditedAsset {
                    kind: "script".to_string(),
                    url: asset_url.to_string(),
                    host: host.clone(),
                    party: classify_party(target_url, &asset_url),
                    integration,
                });
            } else {
                warnings.push(format!("could not resolve script URL `{src}`"));
            }
        } else {
            let inline_text = element.text().collect::<Vec<_>>().join(" ");
            for (integration_id, evidence) in detect_integrations_from_inline_script(&inline_text) {
                integrations.entry(integration_id).or_insert(evidence);
            }
        }
    }

    let detected_integrations = integrations
        .into_iter()
        .map(|(id, evidence)| DetectedIntegration { id, evidence })
        .collect::<Vec<_>>();

    let third_party_asset_count = assets
        .iter()
        .filter(|asset| asset.party == AssetParty::ThirdParty)
        .count();

    Ok(AuditArtifact {
        audited_url: target_url.to_string(),
        page_title: title,
        js_asset_count: assets.len(),
        third_party_asset_count,
        detected_integrations,
        assets,
        warnings,
    })
}

fn classify_party(page_url: &Url, asset_url: &Url) -> AssetParty {
    let page_host = page_url.host_str().unwrap_or_default();
    let asset_host = asset_url.host_str().unwrap_or_default();

    if asset_host == page_host
        || asset_host.ends_with(&format!(".{page_host}"))
        || page_host.ends_with(&format!(".{asset_host}"))
    {
        AssetParty::FirstParty
    } else {
        AssetParty::ThirdParty
    }
}

fn detect_integration_from_url(url: &Url) -> Option<String> {
    let host = url.host_str().unwrap_or_default();
    let path = url.path();
    let value = format!("{host}{path}").to_ascii_lowercase();

    if value.contains("googletagmanager.com") {
        Some("google_tag_manager".to_string())
    } else if value.contains("securepubads.g.doubleclick.net")
        || value.contains("googletagservices.com")
        || value.contains("doubleclick.net/tag/js/gpt")
    {
        Some("gpt".to_string())
    } else if value.contains("privacy-center.org") {
        Some("didomi".to_string())
    } else if value.contains("datadome.co") {
        Some("datadome".to_string())
    } else if value.contains("permutive") {
        Some("permutive".to_string())
    } else if value.contains("loc.kr") {
        Some("lockr".to_string())
    } else if value.contains("prebid") {
        Some("prebid".to_string())
    } else {
        None
    }
}

fn detect_integrations_from_inline_script(script: &str) -> Vec<(String, String)> {
    let mut matches = Vec::new();
    let gtm_regex = Regex::new(r"GTM-[A-Z0-9]+$").expect("should compile GTM regex");

    if let Some(container_id) = gtm_regex.find(script) {
        matches.push((
            "google_tag_manager".to_string(),
            container_id.as_str().to_string(),
        ));
    }

    let lowered = script.to_ascii_lowercase();
    for integration in ["gpt", "didomi", "datadome", "permutive", "lockr", "prebid"] {
        if lowered.contains(integration) {
            matches.push((
                integration.to_string(),
                format!("inline script matched `{integration}`"),
            ));
        }
    }

    matches
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

fn extract_gtm_container_id(artifact: &AuditArtifact) -> Option<String> {
    let regex = Regex::new(r"GTM-[A-Z0-9]+$").expect("should compile GTM regex");

    for integration in &artifact.detected_integrations {
        if integration.id == "google_tag_manager" && regex.is_match(&integration.evidence) {
            return Some(integration.evidence.clone());
        }
    }

    for asset in &artifact.assets {
        if asset.integration.as_deref() == Some("google_tag_manager")
            && let Some(matched) = regex.find(asset.url.as_str())
        {
            return Some(matched.as_str().to_string());
        }
    }

    None
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
}
