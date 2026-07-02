use std::path::{Path, PathBuf};
use std::time::Duration;

use chromiumoxide::browser::{Browser, BrowserConfig};
use chromiumoxide::ArcHttpRequest;
use futures::StreamExt as _;
use serde::Deserialize;
use tempfile::TempDir;
use tokio::runtime::Builder;
use tokio::time::{sleep, timeout};
use url::Url;
use which::which;

use crate::audit::generate::collector::{
    AuditCollector, CollectedPage, CollectedRequest, CollectedScriptTag,
};
use crate::error::{report_error, CliResult};

const SETTLE_QUIET_PERIOD: Duration = Duration::from_millis(750);
const SETTLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const SETTLE_MAX_WAIT: Duration = Duration::from_secs(6);
const NAVIGATION_TIMEOUT: Duration = Duration::from_secs(30);
const BROWSER_CLOSE_TIMEOUT: Duration = Duration::from_secs(5);
const RESOURCE_TIMING_BUFFER_WARNING_THRESHOLD: usize = 250;
const RESOURCE_TIMING_BUFFER_WARNING: &str =
    "browser resource timing buffer reached its default size; some network assets may be missing";

#[derive(Default)]
pub(crate) struct BrowserAuditCollector;

impl AuditCollector for BrowserAuditCollector {
    fn collect_page(&self, target_url: &Url) -> CliResult<CollectedPage> {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                report_error(format!(
                    "failed to build Tokio runtime for browser audit: {error}"
                ))
            })?;

        runtime.block_on(collect_page_via_browser_async(target_url))
    }
}

async fn collect_page_via_browser_async(target_url: &Url) -> CliResult<CollectedPage> {
    let chrome_executable = find_browser_executable()?;
    let user_data_dir = TempDir::new().map_err(|error| {
        report_error(format!(
            "failed to create temporary browser profile for audit: {error}"
        ))
    })?;
    let config = BrowserConfig::builder()
        .chrome_executable(chrome_executable)
        .user_data_dir(user_data_dir.path())
        .new_headless_mode()
        .build()
        .map_err(|error| {
            report_error(format!(
                "failed to build Chromium configuration for audit: {error}"
            ))
        })?;

    let (mut browser, mut handler) = Browser::launch(config).await.map_err(|error| {
        report_error(format!(
            "failed to launch Chrome/Chromium for audit: {error}"
        ))
    })?;

    let handler_task = tokio::spawn(async move {
        while let Some(event) = handler.next().await {
            if event.is_err() {
                break;
            }
        }
    });

    let result = collect_page_from_browser(&mut browser, target_url).await;

    let close_result = timeout(BROWSER_CLOSE_TIMEOUT, browser.close())
        .await
        .map_err(|_| report_error("timed out closing browser after audit"))
        .and_then(|result| {
            result.map_err(|error| {
                report_error(format!("failed to close browser after audit: {error}"))
            })
        });
    if close_result.is_err() {
        handler_task.abort();
    }
    let _ = handler_task.await;

    match (result, close_result) {
        (Ok(collected), Ok(_)) => Ok(collected),
        (Ok(_), Err(error)) | (Err(error), _) => Err(error),
    }
}

async fn collect_page_from_browser(
    browser: &mut Browser,
    target_url: &Url,
) -> CliResult<CollectedPage> {
    let page = browser.new_page("about:blank").await.map_err(|error| {
        report_error(format!("failed to create browser page for audit: {error}"))
    })?;

    timeout(NAVIGATION_TIMEOUT, page.goto(target_url.as_str()))
        .await
        .map_err(|_| report_error(format!("timed out navigating to `{target_url}`")))?
        .map_err(|error| report_error(format!("failed to navigate to `{target_url}`: {error}")))?;

    let navigation_response = timeout(NAVIGATION_TIMEOUT, page.wait_for_navigation_response())
        .await
        .map_err(|_| {
            report_error(format!(
                "timed out waiting for main document navigation response from `{target_url}`"
            ))
        })?
        .map_err(|error| {
            report_error(format!(
                "failed to read main document navigation response: {error}"
            ))
        })?;

    let mut warnings = Vec::new();
    if let Some(warning) = validate_navigation_response(navigation_response)? {
        warnings.push(warning);
    }
    if !wait_for_page_settle(&page).await? {
        warnings.push(
            "browser audit timed out while waiting for the page to settle; results may be partial"
                .to_string(),
        );
    }

    let final_url = page
        .url()
        .await
        .map_err(|error| report_error(format!("failed to read final page URL: {error}")))?
        .ok_or_else(|| report_error("browser page URL was empty after navigation"))?;
    let page_title = page
        .get_title()
        .await
        .map_err(|error| report_error(format!("failed to read page title: {error}")))?;
    let html = page
        .content()
        .await
        .map_err(|error| report_error(format!("failed to read rendered page HTML: {error}")))?;

    let script_tags: Vec<BrowserScriptTag> = page
        .evaluate(
            r#"() => Array.from(document.scripts).map((script) => ({
                src: script.src || null,
                inline_text: script.src ? null : (script.textContent || null),
            }))"#,
        )
        .await
        .map_err(|error| report_error(format!("failed to read rendered script tags: {error}")))?
        .into_value()
        .map_err(|error| {
            report_error(format!(
                "failed to decode rendered script tag data: {error}"
            ))
        })?;

    let network_requests: Vec<BrowserPerformanceEntry> = page
        .evaluate(
            r#"() => performance.getEntriesByType('resource').map((entry) => ({
                url: entry.name,
                initiator_type: entry.initiatorType || null,
            }))"#,
        )
        .await
        .map_err(|error| {
            report_error(format!(
                "failed to read browser performance resource entries: {error}"
            ))
        })?
        .into_value()
        .map_err(|error| {
            report_error(format!(
                "failed to decode browser performance resource data: {error}"
            ))
        })?;

    if let Some(warning) = resource_timing_buffer_warning(network_requests.len()) {
        warnings.push(warning.to_string());
    }

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
                resource_type: entry.initiator_type,
            })
            .collect(),
        warnings,
    })
}

async fn wait_for_page_settle(page: &chromiumoxide::Page) -> CliResult<bool> {
    let mut elapsed = Duration::ZERO;
    let mut previous_count = None;
    let mut stable_for = Duration::ZERO;

    while elapsed < SETTLE_MAX_WAIT {
        let ready_state: String = page
            .evaluate("document.readyState")
            .await
            .map_err(|error| report_error(format!("failed to read document ready state: {error}")))?
            .into_value()
            .map_err(|error| {
                report_error(format!("failed to decode document ready state: {error}"))
            })?;
        let resource_count: usize = page
            .evaluate("performance.getEntriesByType('resource').length")
            .await
            .map_err(|error| report_error(format!("failed to read resource count: {error}")))?
            .into_value()
            .map_err(|error| report_error(format!("failed to decode resource count: {error}")))?;

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

fn validate_navigation_response(navigation_response: ArcHttpRequest) -> CliResult<Option<String>> {
    let request = navigation_response
        .ok_or_else(|| report_error("browser audit did not capture the main document response"))?;

    if let Some(failure_text) = &request.failure_text {
        return Err(report_error(format!(
            "main document request failed: {failure_text}"
        )));
    }

    let response = request.response.as_ref().ok_or_else(|| {
        report_error("browser audit did not capture the main document HTTP response")
    })?;

    if is_successful_navigation_status(response.status) {
        return Ok(None);
    }

    Ok(Some(format!(
        "audit request returned HTTP {} {} for `{}`; results may be partial",
        response.status, response.status_text, response.url
    )))
}

fn is_successful_navigation_status(status: i64) -> bool {
    (200..400).contains(&status)
}

fn resource_timing_buffer_warning(resource_count: usize) -> Option<&'static str> {
    (resource_count >= RESOURCE_TIMING_BUFFER_WARNING_THRESHOLD)
        .then_some(RESOURCE_TIMING_BUFFER_WARNING)
}

fn find_browser_executable() -> CliResult<PathBuf> {
    for candidate in browser_executable_path_candidates() {
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

    Err(report_error(
        "Chrome/Chromium was not found on PATH or in the standard local install locations checked by `ts audit`. Install a local Chrome or Chromium binary before running `ts audit`.",
    ))
}

fn browser_executable_path_candidates() -> &'static [&'static str] {
    &[
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "chrome",
        "Google Chrome",
        "Google Chrome for Testing",
    ]
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

    #[cfg(target_os = "linux")]
    {
        &[
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
        ]
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
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
    use std::sync::Arc;

    use chromiumoxide::cdp::browser_protocol::network::{Headers, RequestId, Response};
    use chromiumoxide::cdp::browser_protocol::security::SecurityState;
    use chromiumoxide::handler::http::HttpRequest;

    use super::*;

    #[test]
    fn successful_navigation_status_allows_redirects_but_rejects_errors() {
        assert!(is_successful_navigation_status(200));
        assert!(is_successful_navigation_status(302));
        assert!(is_successful_navigation_status(399));
        assert!(!is_successful_navigation_status(199));
        assert!(!is_successful_navigation_status(400));
        assert!(!is_successful_navigation_status(500));
    }

    #[test]
    fn navigation_response_returns_warning_for_http_error_status() {
        let warning =
            validate_navigation_response(navigation_response_with_status(403, "Forbidden"))
                .expect("should validate navigation response")
                .expect("should return warning for HTTP error status");

        assert_eq!(
            warning,
            "audit request returned HTTP 403 Forbidden for `https://example.com/`; results may be partial",
            "should warn and continue when the main document returns an HTTP error"
        );
    }

    #[test]
    fn resource_timing_buffer_warning_starts_at_threshold() {
        assert_eq!(
            resource_timing_buffer_warning(RESOURCE_TIMING_BUFFER_WARNING_THRESHOLD - 1),
            None,
            "should not warn before the resource timing buffer threshold"
        );
        assert_eq!(
            resource_timing_buffer_warning(RESOURCE_TIMING_BUFFER_WARNING_THRESHOLD),
            Some(RESOURCE_TIMING_BUFFER_WARNING),
            "should warn when the resource timing buffer reaches the threshold"
        );
    }

    #[test]
    fn browser_path_candidates_include_common_names() {
        let candidates = browser_executable_path_candidates();

        assert!(candidates.contains(&"google-chrome"));
        assert!(candidates.contains(&"chromium"));
        assert!(candidates.contains(&"Google Chrome for Testing"));
    }

    fn navigation_response_with_status(status: i64, status_text: &str) -> ArcHttpRequest {
        let mut request =
            HttpRequest::new(RequestId::new("request-1"), None, None, false, Vec::new());
        request.response = Some(
            Response::builder()
                .url("https://example.com/")
                .status(status)
                .status_text(status_text)
                .headers(Headers::default())
                .mime_type("text/html")
                .charset("utf-8")
                .connection_reused(false)
                .connection_id(1.0)
                .encoded_data_length(0.0)
                .security_state(SecurityState::Secure)
                .build()
                .expect("should build navigation response"),
        );

        Some(Arc::new(request))
    }
}
