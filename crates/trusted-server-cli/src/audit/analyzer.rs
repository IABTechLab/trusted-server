use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use scraper::{Html, Selector};
use url::Url;

use crate::audit::collector::CollectedPage;
use crate::audit::{AssetParty, AuditArtifact, AuditedAsset, DetectedIntegration};
use crate::error::CliError;
use error_stack::Report;

static GTM_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"GTM-[A-Z0-9]+$").expect("should compile GTM regex"));

pub fn analyze_collected_page(
    collected: &CollectedPage,
) -> Result<AuditArtifact, Report<CliError>> {
    let final_url = collected.final_url().map_err(|error| {
        Report::new(CliError::Audit).attach(format!("invalid final URL: {error}"))
    })?;
    let requested_url = collected.requested_url().map_err(|error| {
        Report::new(CliError::Audit).attach(format!("invalid requested URL: {error}"))
    })?;

    let document = Html::parse_document(&collected.html);
    let title_selector = Selector::parse("title").expect("should parse title selector");
    let script_selector = Selector::parse("script").expect("should parse script selector");
    let derived_title = document
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

    let mut assets_by_url = BTreeMap::<String, AuditedAsset>::new();
    let mut integrations = BTreeMap::<String, String>::new();
    let mut warnings = collected.warnings.clone();

    if requested_url != final_url {
        warnings.push(format!(
            "page redirected from `{requested_url}` to `{final_url}`"
        ));
    }

    for element in document.select(&script_selector) {
        if let Some(src) = element.value().attr("src") {
            if let Ok(asset_url) = final_url.join(src) {
                let integration = detect_integration_from_url(&asset_url);
                record_integration(&mut integrations, &integration, asset_url.as_str());
                insert_asset(&mut assets_by_url, &final_url, &asset_url, integration);
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

    for tag in &collected.script_tags {
        if let Some(src) = &tag.src
            && let Ok(asset_url) = Url::parse(src)
        {
            let integration = detect_integration_from_url(&asset_url);
            record_integration(&mut integrations, &integration, asset_url.as_str());
            insert_asset(&mut assets_by_url, &final_url, &asset_url, integration);
        }

        if let Some(inline_text) = &tag.inline_text {
            for (integration_id, evidence) in detect_integrations_from_inline_script(inline_text) {
                integrations.entry(integration_id).or_insert(evidence);
            }
        }
    }

    for request in &collected.network_requests {
        let is_script = request
            .resource_type
            .as_deref()
            .is_some_and(|resource_type| resource_type.eq_ignore_ascii_case("script"));
        if !is_script {
            continue;
        }
        if let Ok(asset_url) = Url::parse(&request.url) {
            let integration = detect_integration_from_url(&asset_url);
            record_integration(&mut integrations, &integration, asset_url.as_str());
            insert_asset(&mut assets_by_url, &final_url, &asset_url, integration);
        }
    }

    let assets = assets_by_url.into_values().collect::<Vec<_>>();
    let third_party_asset_count = assets
        .iter()
        .filter(|asset| asset.party == AssetParty::ThirdParty)
        .count();

    Ok(AuditArtifact {
        audited_url: final_url.to_string(),
        page_title: collected.page_title.clone().or(derived_title),
        js_asset_count: assets.len(),
        third_party_asset_count,
        detected_integrations: integrations
            .into_iter()
            .map(|(id, evidence)| DetectedIntegration { id, evidence })
            .collect(),
        assets,
        warnings,
    })
}

fn insert_asset(
    assets_by_url: &mut BTreeMap<String, AuditedAsset>,
    page_url: &Url,
    asset_url: &Url,
    integration: Option<String>,
) {
    assets_by_url
        .entry(asset_url.to_string())
        .or_insert_with(|| AuditedAsset {
            kind: "script".to_string(),
            url: asset_url.to_string(),
            host: asset_url.host_str().unwrap_or_default().to_string(),
            party: classify_party(page_url, asset_url),
            integration,
        });
}

fn record_integration(
    integrations: &mut BTreeMap<String, String>,
    integration: &Option<String>,
    evidence: &str,
) {
    if let Some(integration_id) = integration {
        integrations
            .entry(integration_id.clone())
            .or_insert_with(|| evidence.to_string());
    }
}

pub fn classify_party(page_url: &Url, asset_url: &Url) -> AssetParty {
    let page_host = page_url.host_str().unwrap_or_default();
    let asset_host = asset_url.host_str().unwrap_or_default();

    if host_matches(page_host, asset_host) {
        AssetParty::FirstParty
    } else {
        AssetParty::ThirdParty
    }
}

fn host_matches(page_host: &str, asset_host: &str) -> bool {
    asset_host == page_host
        || asset_host
            .strip_suffix(page_host)
            .is_some_and(|prefix| prefix.ends_with('.'))
        || page_host
            .strip_suffix(asset_host)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

pub fn detect_integration_from_url(url: &Url) -> Option<String> {
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

pub fn detect_integrations_from_inline_script(script: &str) -> Vec<(String, String)> {
    let mut matches = Vec::new();

    if let Some(container_id) = GTM_REGEX.find(script) {
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

pub fn extract_gtm_container_id(artifact: &AuditArtifact) -> Option<String> {
    for integration in &artifact.detected_integrations {
        if integration.id == "google_tag_manager" && GTM_REGEX.is_match(&integration.evidence) {
            return Some(integration.evidence.clone());
        }
    }

    for asset in &artifact.assets {
        if asset.integration.as_deref() == Some("google_tag_manager")
            && let Some(matched) = GTM_REGEX.find(asset.url.as_str())
        {
            return Some(matched.as_str().to_string());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::collector::{CollectedRequest, CollectedScriptTag};

    #[test]
    fn analyze_collected_page_merges_dom_and_network_scripts() {
        let collected = CollectedPage {
            requested_url: "https://publisher.example/page".to_string(),
            final_url: "https://publisher.example/page".to_string(),
            page_title: Some("Example Publisher".to_string()),
            html: r#"<html><head><script src="https://www.googletagmanager.com/gtm.js?id=GTM-ABCD123"></script></head></html>"#.to_string(),
            script_tags: vec![CollectedScriptTag {
                src: Some("https://securepubads.g.doubleclick.net/tag/js/gpt.js".to_string()),
                inline_text: None,
            }],
            network_requests: vec![CollectedRequest {
                url: "https://cdn.example.com/dynamic.js".to_string(),
                method: "GET".to_string(),
                resource_type: Some("Script".to_string()),
                status: Some(200),
            }],
            warnings: vec!["partial settle".to_string()],
        };

        let artifact = analyze_collected_page(&collected).expect("should analyze collected page");

        assert_eq!(
            artifact.js_asset_count, 3,
            "should merge all script evidence"
        );
        assert_eq!(artifact.warnings, vec!["partial settle".to_string()]);
        assert!(
            artifact
                .detected_integrations
                .iter()
                .any(|integration| integration.id == "google_tag_manager"),
            "should preserve GTM detection"
        );
        assert!(
            artifact
                .detected_integrations
                .iter()
                .any(|integration| integration.id == "gpt"),
            "should detect GPT from browser collected scripts"
        );
    }

    #[test]
    fn analyze_collected_page_deduplicates_scripts() {
        let collected = CollectedPage {
            requested_url: "https://publisher.example/page".to_string(),
            final_url: "https://publisher.example/page".to_string(),
            page_title: None,
            html:
                r#"<html><head><script src="https://cdn.example.com/a.js"></script></head></html>"#
                    .to_string(),
            script_tags: vec![CollectedScriptTag {
                src: Some("https://cdn.example.com/a.js".to_string()),
                inline_text: None,
            }],
            network_requests: vec![CollectedRequest {
                url: "https://cdn.example.com/a.js".to_string(),
                method: "GET".to_string(),
                resource_type: Some("script".to_string()),
                status: Some(200),
            }],
            warnings: Vec::new(),
        };

        let artifact = analyze_collected_page(&collected).expect("should analyze collected page");

        assert_eq!(
            artifact.js_asset_count, 1,
            "should deduplicate identical script URLs"
        );
    }

    #[test]
    fn analyze_collected_page_uses_final_url_and_records_redirect_warning() {
        let collected = CollectedPage {
            requested_url: "http://publisher.example/page".to_string(),
            final_url: "https://www.publisher.example/landing".to_string(),
            page_title: Some("Example Publisher".to_string()),
            html: "<html><head></head></html>".to_string(),
            script_tags: Vec::new(),
            network_requests: Vec::new(),
            warnings: Vec::new(),
        };

        let artifact = analyze_collected_page(&collected).expect("should analyze collected page");

        assert_eq!(
            artifact.audited_url, "https://www.publisher.example/landing",
            "should report the final audited URL"
        );
        assert!(
            artifact
                .warnings
                .iter()
                .any(|warning| warning.contains("page redirected from `http://publisher.example/page` to `https://www.publisher.example/landing`")),
            "should preserve redirect context in warnings"
        );
    }
}
