use error_stack::{Report, ResultExt};
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use url::Url;

use crate::audit::collector::{CollectedPage, CollectedRequest, CollectedScriptTag};
use crate::error::CliError;

#[allow(dead_code)]
pub fn collect_page_via_http(target_url: &Url) -> Result<CollectedPage, Report<CliError>> {
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

    let final_url = response.url().clone();
    let status = response.status();
    if !status.is_success() {
        return Err(
            Report::new(CliError::Audit).attach(format!("audit request returned HTTP {status}"))
        );
    }

    let body = response.text().change_context(CliError::Audit)?;
    let document = Html::parse_document(&body);
    let title_selector = Selector::parse("title").expect("should parse title selector");
    let script_selector = Selector::parse("script").expect("should parse script selector");
    let page_title = document
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

    let mut script_tags = Vec::new();
    for element in document.select(&script_selector) {
        script_tags.push(CollectedScriptTag {
            src: element
                .value()
                .attr("src")
                .and_then(|src| final_url.join(src).ok())
                .map(|url| url.to_string()),
            inline_text: element
                .value()
                .attr("src")
                .is_none()
                .then(|| element.text().collect::<Vec<_>>().join(" "))
                .filter(|text| !text.trim().is_empty()),
        });
    }

    Ok(CollectedPage {
        requested_url: target_url.to_string(),
        final_url: final_url.to_string(),
        page_title,
        html: body,
        script_tags,
        network_requests: vec![CollectedRequest {
            url: final_url.to_string(),
            method: "GET".to_string(),
            resource_type: Some("document".to_string()),
            status: Some(status.as_u16()),
        }],
        warnings: Vec::new(),
    })
}
