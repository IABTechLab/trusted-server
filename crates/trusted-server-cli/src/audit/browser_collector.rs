use std::path::{Path, PathBuf};
use std::time::Duration;

use chromiumoxide::ArcHttpRequest;
use chromiumoxide::browser::{Browser, BrowserConfig};
use error_stack::{Report, ResultExt};
use futures::StreamExt as _;
use serde::Deserialize;
use tokio::runtime::Builder;
use tokio::time::sleep;
use url::Url;
use which::which;

use crate::audit::collector::{CollectedPage, CollectedRequest, CollectedScriptTag};
use crate::error::CliError;

const SETTLE_QUIET_PERIOD: Duration = Duration::from_millis(750);
const SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const SETTLE_MAX_WAIT: Duration = Duration::from_secs(6);

pub fn collect_page_via_browser(target_url: &Url) -> Result<CollectedPage, Report<CliError>> {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .change_context(CliError::Audit)
        .attach("failed to build Tokio runtime for browser audit")?;

    runtime.block_on(collect_page_via_browser_async(target_url))
}

async fn collect_page_via_browser_async(
    target_url: &Url,
) -> Result<CollectedPage, Report<CliError>> {
    let chrome_executable = find_browser_executable()?;
    let config = BrowserConfig::builder()
        .chrome_executable(chrome_executable)
        .new_headless_mode()
        .build()
        .map_err(|error| Report::new(CliError::Audit).attach(error))
        .attach("failed to build Chromium configuration for audit")?;

    let (mut browser, mut handler) = Browser::launch(config)
        .await
        .change_context(CliError::Audit)
        .attach("failed to launch Chrome/Chromium for audit")?;

    let handler_task = tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            if event.is_err() {
                break;
            }
        }
    });

    let page = browser
        .new_page("about:blank")
        .await
        .change_context(CliError::Audit)
        .attach("failed to create browser page for audit")?;

    page.evaluate_on_new_document(
        r#"
        Object.defineProperty(Object.getPrototypeOf(navigator), 'webdriver', {
            get: () => false,
        });
        "#,
    )
    .await
    .change_context(CliError::Audit)
    .attach("failed to inject browser audit init script")?;

    page.goto(target_url.as_str())
        .await
        .change_context(CliError::Audit)
        .attach(format!("failed to navigate to `{target_url}`"))?;

    let navigation_response = page
        .wait_for_navigation_response()
        .await
        .change_context(CliError::Audit)
        .attach("failed to read main document navigation response")?;
    validate_navigation_response(target_url, navigation_response)?;

    let mut warnings = Vec::new();
    if !wait_for_page_settle(&page).await? {
        warnings.push(
            "browser audit timed out while waiting for the page to settle; results may be partial"
                .to_string(),
        );
    }

    let final_url = page
        .url()
        .await
        .change_context(CliError::Audit)
        .attach("failed to read final page URL")?
        .ok_or_else(|| {
            Report::new(CliError::Audit).attach("browser page URL was empty after navigation")
        })?;
    let page_title = page
        .get_title()
        .await
        .change_context(CliError::Audit)
        .attach("failed to read page title")?;
    let html = page
        .content()
        .await
        .change_context(CliError::Audit)
        .attach("failed to read rendered page HTML")?;

    let script_tags: Vec<BrowserScriptTag> = page
        .evaluate(
            r#"() => Array.from(document.scripts).map((script) => ({
                src: script.src || null,
                inline_text: script.src ? null : (script.textContent || null),
            }))"#,
        )
        .await
        .change_context(CliError::Audit)
        .attach("failed to read rendered script tags")?
        .into_value()
        .change_context(CliError::Audit)
        .attach("failed to decode rendered script tag data")?;

    let network_requests: Vec<BrowserPerformanceEntry> = page
        .evaluate(
            r#"() => performance.getEntriesByType('resource').map((entry) => ({
                url: entry.name,
                initiator_type: entry.initiatorType || null,
            }))"#,
        )
        .await
        .change_context(CliError::Audit)
        .attach("failed to read browser performance resource entries")?
        .into_value()
        .change_context(CliError::Audit)
        .attach("failed to decode browser performance resource data")?;

    browser
        .close()
        .await
        .change_context(CliError::Audit)
        .attach("failed to close browser after audit")?;
    let _ = handler_task.await;

    Ok(CollectedPage {
        requested_url: target_url.to_string(),
        final_url,
        page_title: page_title.filter(|title| !title.trim().is_empty()),
        html,
        script_tags: script_tags
            .into_iter()
            .map(|script| CollectedScriptTag {
                src: script.src,
                inline_text: script.inline_text.filter(|text| !text.trim().is_empty()),
            })
            .collect(),
        network_requests: network_requests
            .into_iter()
            .map(|entry| CollectedRequest {
                url: entry.url,
                method: "GET".to_string(),
                resource_type: entry.initiator_type,
                status: None,
            })
            .collect(),
        warnings,
    })
}

async fn wait_for_page_settle(page: &chromiumoxide::Page) -> Result<bool, Report<CliError>> {
    let mut elapsed = Duration::ZERO;
    let mut previous_count = None;
    let mut stable_for = Duration::ZERO;

    while elapsed < SETTLE_MAX_WAIT {
        let ready_state: String = page
            .evaluate("document.readyState")
            .await
            .change_context(CliError::Audit)?
            .into_value()
            .change_context(CliError::Audit)?;
        let resource_count: usize = page
            .evaluate("performance.getEntriesByType('resource').length")
            .await
            .change_context(CliError::Audit)?
            .into_value()
            .change_context(CliError::Audit)?;

        if ready_state == "complete" {
            if previous_count == Some(resource_count) {
                stable_for += SETTLE_POLL_INTERVAL;
            } else {
                stable_for = Duration::ZERO;
            }

            if stable_for >= SETTLE_QUIET_PERIOD {
                return Ok(true);
            }
        }

        previous_count = Some(resource_count);
        sleep(SETTLE_POLL_INTERVAL).await;
        elapsed += SETTLE_POLL_INTERVAL;
    }

    Ok(false)
}

fn validate_navigation_response(
    target_url: &Url,
    navigation_response: ArcHttpRequest,
) -> Result<(), Report<CliError>> {
    if !matches!(target_url.scheme(), "http" | "https") {
        return Ok(());
    }

    let request = navigation_response.ok_or_else(|| {
        Report::new(CliError::Audit)
            .attach("browser audit did not capture the main document response")
    })?;

    if let Some(failure_text) = &request.failure_text {
        return Err(Report::new(CliError::Audit)
            .attach(format!("main document request failed: {failure_text}")));
    }

    let response = request.response.as_ref().ok_or_else(|| {
        Report::new(CliError::Audit)
            .attach("browser audit did not capture the main document HTTP response")
    })?;

    if is_successful_navigation_status(response.status) {
        return Ok(());
    }

    Err(Report::new(CliError::Audit).attach(format!(
        "audit request returned HTTP {} {} for `{}`",
        response.status, response.status_text, response.url
    )))
}

fn is_successful_navigation_status(status: i64) -> bool {
    (200..400).contains(&status)
}

fn find_browser_executable() -> Result<PathBuf, Report<CliError>> {
    for candidate in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "chrome",
        "Google Chrome",
        "Google Chrome for Testing",
    ] {
        if let Ok(path) = which(candidate) {
            return Ok(path);
        }
    }

    for candidate in browser_executable_fallbacks() {
        let candidate_path = Path::new(candidate);
        if candidate_path.is_file() {
            return Ok(candidate_path.to_path_buf());
        }
    }

    Err(Report::new(CliError::Audit).attach(
        "Chrome/Chromium was not found on PATH or in the standard local install locations checked by `ts audit`. Install a local Chrome or Chromium binary before running `ts audit`.",
    ))
}

fn browser_executable_fallbacks() -> &'static [&'static str] {
    #[cfg(target_os = "macos")]
    {
        &[
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing",
        ]
    }

    #[cfg(not(target_os = "macos"))]
    {
        &[]
    }
}

#[derive(Debug, Deserialize)]
struct BrowserScriptTag {
    src: Option<String>,
    inline_text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BrowserPerformanceEntry {
    url: String,
    initiator_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_navigation_status_allows_redirects_but_rejects_errors() {
        assert!(is_successful_navigation_status(200));
        assert!(is_successful_navigation_status(302));
        assert!(is_successful_navigation_status(399));
        assert!(!is_successful_navigation_status(199));
        assert!(!is_successful_navigation_status(404));
        assert!(!is_successful_navigation_status(500));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn browser_fallbacks_include_standard_macos_google_chrome_path() {
        assert!(browser_executable_fallbacks().iter().any(|candidate| {
            *candidate == "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
        }));
    }
}
