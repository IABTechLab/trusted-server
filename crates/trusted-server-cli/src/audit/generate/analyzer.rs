use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use scraper::{Html, Selector};
use url::Url;

use crate::audit::generate::collector::CollectedPage;
use crate::audit::generate::{AssetParty, AuditArtifact, AuditedAsset, DetectedIntegration};
use crate::error::{report_error, CliResult};

static GTM_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bGTM-[A-Z0-9]+\b").expect("should compile GTM regex"));
static GPT_INLINE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:googletag|gpt\.js|googletagservices|securepubads)\b")
        .expect("should compile GPT inline regex")
});
static DIDOMI_INLINE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bdidomi\b").expect("should compile Didomi inline regex"));
static DATADOME_INLINE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bdatadome\b").expect("should compile DataDome inline regex")
});
static PERMUTIVE_INLINE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bpermutive\b").expect("should compile Permutive inline regex")
});
static LOCKR_INLINE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:\blockr\b|\bloc\.kr\b)").expect("should compile Lockr inline regex")
});
static PREBID_INLINE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:\bprebid\b|\bpbjs\b)").expect("should compile Prebid inline regex")
});

pub(crate) fn analyze_collected_page(collected: &CollectedPage) -> CliResult<AuditArtifact> {
    let final_url = collected
        .final_url()
        .map_err(|error| report_error(format!("invalid final URL: {error}")))?;
    let requested_url = collected
        .requested_url()
        .map_err(|error| report_error(format!("invalid requested URL: {error}")))?;

    let document = Html::parse_document(&collected.html);
    let title_selector = Selector::parse("title").expect("should parse title selector");
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

    for tag in &collected.script_tags {
        if let Some(src) = &tag.src {
            if let Ok(asset_url) = final_url.join(src) {
                let integration = detect_integration_from_url(&asset_url);
                record_integration(&mut integrations, &integration, asset_url.as_str());
                insert_asset(&mut assets_by_url, &final_url, &asset_url, integration);
            } else {
                warnings.push(format!("could not resolve script URL `{src}`"));
            }
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

    let page_title = collected
        .page_title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(ToOwned::to_owned)
        .or(derived_title);

    Ok(AuditArtifact {
        audited_url: final_url.to_string(),
        page_title,
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
    let asset = assets_by_url
        .entry(asset_url.to_string())
        .or_insert_with(|| AuditedAsset {
            kind: "script".to_string(),
            url: asset_url.to_string(),
            host: asset_url.host_str().unwrap_or_default().to_string(),
            party: classify_party(page_url, asset_url),
            integration: None,
        });

    if asset.integration.is_none() {
        asset.integration = integration;
    }
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

pub(crate) fn classify_party(page_url: &Url, asset_url: &Url) -> AssetParty {
    let page_host = page_url.host_str().unwrap_or_default();
    let asset_host = asset_url.host_str().unwrap_or_default();

    if host_matches(page_host, asset_host) {
        AssetParty::FirstParty
    } else {
        AssetParty::ThirdParty
    }
}

fn host_matches(page_host: &str, asset_host: &str) -> bool {
    // This is an advisory heuristic, not public-suffix-aware eTLD+1 classification.
    asset_host == page_host
        || asset_host
            .strip_suffix(page_host)
            .is_some_and(|prefix| prefix.ends_with('.'))
        || page_host
            .strip_suffix(asset_host)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

pub(crate) fn detect_integration_from_url(url: &Url) -> Option<String> {
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

pub(crate) fn detect_integrations_from_inline_script(script: &str) -> Vec<(String, String)> {
    let mut matches = Vec::new();

    if let Some(container_id) = GTM_REGEX.find(script) {
        matches.push((
            "google_tag_manager".to_string(),
            container_id.as_str().to_string(),
        ));
    }

    for (integration, regex) in [
        ("gpt", &*GPT_INLINE_REGEX),
        ("didomi", &*DIDOMI_INLINE_REGEX),
        ("datadome", &*DATADOME_INLINE_REGEX),
        ("permutive", &*PERMUTIVE_INLINE_REGEX),
        ("lockr", &*LOCKR_INLINE_REGEX),
        ("prebid", &*PREBID_INLINE_REGEX),
    ] {
        if regex.is_match(script) {
            matches.push((
                integration.to_string(),
                format!("inline script matched `{integration}`"),
            ));
        }
    }

    matches
}

pub(crate) fn extract_gtm_container_id(artifact: &AuditArtifact) -> Option<String> {
    for integration in &artifact.detected_integrations {
        if integration.id == "google_tag_manager" && GTM_REGEX.is_match(&integration.evidence) {
            return Some(integration.evidence.clone());
        }
    }

    for asset in &artifact.assets {
        if asset.integration.as_deref() == Some("google_tag_manager") {
            if let Some(matched) = GTM_REGEX.find(asset.url.as_str()) {
                return Some(matched.as_str().to_string());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::generate::collector::{CollectedRequest, CollectedScriptTag};

    fn page_url() -> Url {
        Url::parse("https://publisher.example/page").expect("should parse URL")
    }

    #[test]
    fn analyze_collected_page_merges_dom_and_network_scripts() {
        let collected = CollectedPage {
            requested_url: "https://publisher.example/page".to_string(),
            final_url: "https://publisher.example/page".to_string(),
            page_title: Some("Browser Title".to_string()),
            html: r#"<html><head><title>HTML Title</title></head></html>"#.to_string(),
            script_tags: vec![
                CollectedScriptTag {
                    src: Some("https://www.googletagmanager.com/gtm.js?id=GTM-ABCD123".to_string()),
                    inline_text: None,
                },
                CollectedScriptTag {
                    src: Some("https://securepubads.g.doubleclick.net/tag/js/gpt.js".to_string()),
                    inline_text: None,
                },
            ],
            network_requests: vec![CollectedRequest {
                url: "https://cdn.example.com/dynamic.js".to_string(),
                resource_type: Some("Script".to_string()),
            }],
            gpt_slots: Vec::new(),
            warnings: vec!["partial settle".to_string()],
        };

        let artifact = analyze_collected_page(&collected).expect("should analyze collected page");

        assert_eq!(artifact.page_title.as_deref(), Some("Browser Title"));
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
    fn analyze_collected_page_uses_html_title_when_browser_title_absent() {
        let collected = CollectedPage {
            requested_url: "https://publisher.example/page".to_string(),
            final_url: "https://publisher.example/page".to_string(),
            page_title: None,
            html: "<html><head><title>HTML Title</title></head></html>".to_string(),
            script_tags: Vec::new(),
            network_requests: Vec::new(),
            gpt_slots: Vec::new(),
            warnings: Vec::new(),
        };

        let artifact = analyze_collected_page(&collected).expect("should analyze collected page");

        assert_eq!(artifact.page_title.as_deref(), Some("HTML Title"));
    }

    #[test]
    fn analyze_collected_page_uses_html_title_when_browser_title_is_empty() {
        let collected = CollectedPage {
            requested_url: "https://publisher.example/page".to_string(),
            final_url: "https://publisher.example/page".to_string(),
            page_title: Some("   ".to_string()),
            html: "<html><head><title>HTML Title</title></head></html>".to_string(),
            script_tags: Vec::new(),
            network_requests: Vec::new(),
            gpt_slots: Vec::new(),
            warnings: Vec::new(),
        };

        let artifact = analyze_collected_page(&collected).expect("should analyze collected page");

        assert_eq!(artifact.page_title.as_deref(), Some("HTML Title"));
    }

    #[test]
    fn analyze_collected_page_deduplicates_scripts_and_updates_integration() {
        let collected = CollectedPage {
            requested_url: "https://publisher.example/page".to_string(),
            final_url: "https://publisher.example/page".to_string(),
            page_title: None,
            html: r#"<html><head><script src="https://cdn.example.com/prebid.js"></script></head></html>"#
                .to_string(),
            script_tags: vec![CollectedScriptTag {
                src: Some("https://cdn.example.com/prebid.js".to_string()),
                inline_text: None,
            }],
            network_requests: vec![CollectedRequest {
                url: "https://cdn.example.com/prebid.js".to_string(),
                resource_type: Some("script".to_string()),
            }],
            gpt_slots: Vec::new(),
            warnings: Vec::new(),
        };

        let artifact = analyze_collected_page(&collected).expect("should analyze collected page");

        assert_eq!(
            artifact.js_asset_count, 1,
            "should deduplicate identical script URLs"
        );
        assert_eq!(
            artifact.assets[0].integration.as_deref(),
            Some("prebid"),
            "should preserve detected integration on deduped asset"
        );
    }

    #[test]
    fn analyze_collected_page_resolves_relative_scripts_and_warns_on_invalid_src() {
        let collected = CollectedPage {
            requested_url: "https://publisher.example/page".to_string(),
            final_url: "https://publisher.example/path/page".to_string(),
            page_title: None,
            html: "<html><head></head></html>".to_string(),
            script_tags: vec![
                CollectedScriptTag {
                    src: Some("/static/app.js".to_string()),
                    inline_text: None,
                },
                CollectedScriptTag {
                    src: Some("http://[invalid".to_string()),
                    inline_text: None,
                },
            ],
            network_requests: Vec::new(),
            gpt_slots: Vec::new(),
            warnings: Vec::new(),
        };

        let artifact = analyze_collected_page(&collected).expect("should analyze collected page");

        assert!(
            artifact
                .assets
                .iter()
                .any(|asset| asset.url == "https://publisher.example/static/app.js"),
            "should resolve relative URL against final URL"
        );
        assert!(
            artifact
                .warnings
                .iter()
                .any(|warning| warning.contains("could not resolve script URL")),
            "should warn about malformed script URL"
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
            gpt_slots: Vec::new(),
            warnings: Vec::new(),
        };

        let artifact = analyze_collected_page(&collected).expect("should analyze collected page");

        assert_eq!(
            artifact.audited_url, "https://www.publisher.example/landing",
            "should report the final audited URL"
        );
        assert!(
            artifact.warnings.iter().any(|warning| warning.contains(
                "page redirected from `http://publisher.example/page` to `https://www.publisher.example/landing`"
            )),
            "should preserve redirect context in warnings"
        );
    }

    #[test]
    fn classify_party_uses_host_relationship() {
        let page = page_url();
        let exact = Url::parse("https://publisher.example/app.js").expect("should parse URL");
        let subdomain =
            Url::parse("https://cdn.publisher.example/app.js").expect("should parse URL");
        let parent = Url::parse("https://example/app.js").expect("should parse URL");
        let unrelated = Url::parse("https://cdn.example.com/app.js").expect("should parse URL");

        assert_eq!(classify_party(&page, &exact), AssetParty::FirstParty);
        assert_eq!(classify_party(&page, &subdomain), AssetParty::FirstParty);
        assert_eq!(classify_party(&page, &parent), AssetParty::FirstParty);
        assert_eq!(classify_party(&page, &unrelated), AssetParty::ThirdParty);
    }

    #[test]
    fn detect_integrations_from_inline_script_reads_standard_gtm_snippet() {
        let matches = detect_integrations_from_inline_script(
            r#"(function(w,d,s,l,i){w[l]=w[l]||[];})(window,document,'script','dataLayer','GTM-ABC123');"#,
        );

        assert!(
            matches.iter().any(
                |(integration, evidence)| integration == "google_tag_manager"
                    && evidence == "GTM-ABC123"
            ),
            "should detect GTM IDs followed by snippet punctuation"
        );
    }

    #[test]
    fn detect_integrations_from_inline_script_reads_case_insensitive_markers() {
        let matches = detect_integrations_from_inline_script("window.PREBID = window.Didomi;");

        assert!(matches
            .iter()
            .any(|(integration, _)| integration == "prebid"));
        assert!(matches
            .iter()
            .any(|(integration, _)| integration == "didomi"));
    }

    #[test]
    fn detect_integrations_from_inline_script_avoids_short_substring_matches() {
        let matches = detect_integrations_from_inline_script("const svgptimize = blockrResult;");

        assert!(
            !matches.iter().any(|(integration, _)| integration == "gpt"),
            "should not match incidental GPT substrings"
        );
        assert!(
            !matches
                .iter()
                .any(|(integration, _)| integration == "lockr"),
            "should not match lockr inside a larger token"
        );
    }

    #[test]
    fn detect_integration_from_url_recognizes_known_patterns() {
        let cases = [
            (
                "https://www.googletagmanager.com/gtm.js?id=GTM-ABC123",
                "google_tag_manager",
            ),
            (
                "https://securepubads.g.doubleclick.net/tag/js/gpt.js",
                "gpt",
            ),
            ("https://sdk.privacy-center.org/sdk.js", "didomi"),
            ("https://js.datadome.co/tags.js", "datadome"),
            ("https://cdn.permutive.com/sdk.js", "permutive"),
            ("https://identity.loc.kr/sdk.js", "lockr"),
            ("https://cdn.example.com/prebid.js", "prebid"),
        ];

        for (url, expected) in cases {
            let parsed = Url::parse(url).expect("should parse URL");
            assert_eq!(
                detect_integration_from_url(&parsed).as_deref(),
                Some(expected),
                "should detect {expected}"
            );
        }
    }

    #[test]
    fn extract_gtm_container_id_reads_query_parameter_urls() {
        let artifact = AuditArtifact {
            audited_url: "https://publisher.example".to_string(),
            page_title: None,
            js_asset_count: 1,
            third_party_asset_count: 1,
            detected_integrations: Vec::new(),
            assets: vec![AuditedAsset {
                kind: "script".to_string(),
                url: "https://www.googletagmanager.com/gtm.js?id=GTM-ABC123&l=dataLayer"
                    .to_string(),
                host: "www.googletagmanager.com".to_string(),
                party: AssetParty::ThirdParty,
                integration: Some("google_tag_manager".to_string()),
            }],
            warnings: Vec::new(),
        };

        assert_eq!(
            extract_gtm_container_id(&artifact).as_deref(),
            Some("GTM-ABC123"),
            "should extract GTM container IDs before query separators"
        );
    }

    #[test]
    fn artifact_serialization_uses_expected_shape() {
        let artifact = AuditArtifact {
            audited_url: "https://publisher.example".to_string(),
            page_title: None,
            js_asset_count: 1,
            third_party_asset_count: 1,
            detected_integrations: vec![DetectedIntegration {
                id: "gpt".to_string(),
                evidence: "https://securepubads.g.doubleclick.net/tag/js/gpt.js".to_string(),
            }],
            assets: vec![AuditedAsset {
                kind: "script".to_string(),
                url: "https://securepubads.g.doubleclick.net/tag/js/gpt.js".to_string(),
                host: "securepubads.g.doubleclick.net".to_string(),
                party: AssetParty::ThirdParty,
                integration: Some("gpt".to_string()),
            }],
            warnings: Vec::new(),
        };

        let toml = toml::to_string_pretty(&artifact).expect("should serialize artifact");

        assert!(toml.contains("audited_url = \"https://publisher.example\""));
        assert!(toml.contains("party = \"third-party\""));
        assert!(!toml.contains("page_title"));
    }
}
