use crate::common::assertions;
use crate::common::runtime::{TestError, origin_port};
use error_stack::ResultExt as _;

/// Standard test scenarios applicable to all frontend frameworks.
///
/// Each scenario tests a core trusted-server behavior that should work
/// regardless of the underlying framework serving HTML.
#[derive(Debug, Clone)]
pub enum TestScenario {
    /// Verify `<script>` tag is injected into the HTML response.
    HtmlInjection,

    /// Verify `/static/tsjs=tsjs-unified.min.js` endpoint serves the JS bundle.
    ScriptServing,

    /// Verify origin host URLs in `href`/`src` attributes are rewritten to the proxy host.
    AttributeRewriting,

    /// Verify `/static/tsjs=unknown.js` returns 404, not HTML or a fallback.
    ScriptServingUnknownFile404,
}

/// Framework-specific custom scenarios that test framework-unique behaviors.
#[derive(Debug, Clone)]
pub enum CustomScenario {
    /// Next.js: React Server Components Flight format is preserved.
    NextJsRscFlight,

    /// Next.js: Server Actions POST requests pass through correctly.
    NextJsServerActions,

    /// Next.js: API routes return JSON without HTML injection.
    NextJsApiRoute,

    /// Next.js: Form action URLs are rewritten from origin to proxy.
    NextJsFormAction,

    /// WordPress: Admin pages (`/wp-admin/`) receive script injection.
    ///
    /// The trusted server currently injects into ALL HTML responses
    /// regardless of path. This test documents that behavior and guards
    /// against unintended changes.
    WordPressAdminInjection,
}

impl TestScenario {
    /// Execute this scenario against a running runtime.
    ///
    /// # Arguments
    ///
    /// * `base_url` - The base URL of the running runtime (e.g. `http://127.0.0.1:12345`)
    /// * `framework_id` - Identifier for the framework (used in error context)
    ///
    /// # Errors
    ///
    /// Returns a [`TestError`] variant depending on which assertion fails.
    pub fn run(&self, base_url: &str, framework_id: &str) -> error_stack::Result<(), TestError> {
        match self {
            Self::HtmlInjection => {
                let resp = reqwest::blocking::get(base_url)
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: HtmlInjection, framework: {framework_id}"
                    ))?;

                let html = resp
                    .text()
                    .change_context(TestError::ResponseParse)
                    .attach_printable(format!("framework: {framework_id}"))?;

                assertions::assert_unique_script_tag(&html)
                    .attach_printable(format!("framework: {framework_id}"))?;

                Ok(())
            }

            Self::AttributeRewriting => {
                // Verify that absolute origin URLs in href/src attributes are
                // rewritten to the proxy host. The test fixtures embed links
                // like `http://127.0.0.1:8888/page` which the HTML processor
                // should rewrite to `http://127.0.0.1:{proxy_port}/page`.
                let resp = reqwest::blocking::get(base_url)
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: AttributeRewriting, framework: {framework_id}"
                    ))?;

                let html = resp
                    .text()
                    .change_context(TestError::ResponseParse)
                    .attach_printable(format!("framework: {framework_id}"))?;

                let origin_host = format!("127.0.0.1:{}", origin_port());

                assertions::assert_attributes_rewritten(&html, &origin_host, base_url)
                    .attach_printable(format!("framework: {framework_id}"))?;

                // Verify URL attributes inside ad-slot elements are also rewritten
                assertions::assert_ad_slot_urls_rewritten(&html, &origin_host, base_url)
                    .attach_printable(format!("framework: {framework_id}"))?;

                // Verify non-URL attributes like data-ad-unit are preserved unchanged
                assertions::assert_data_ad_units_preserved(
                    &html,
                    &["/test/banner", "/test/sidebar"],
                )
                .attach_printable(format!("framework: {framework_id}"))?;

                Ok(())
            }

            Self::ScriptServing => {
                let url = format!("{base_url}/static/tsjs=tsjs-unified.min.js");

                let resp = reqwest::blocking::get(&url)
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: ScriptServing, framework: {framework_id}"
                    ))?;

                if !resp.status().is_success() {
                    return Err(
                        error_stack::report!(TestError::HttpRequest).attach_printable(format!(
                            "Script serving returned {}, expected 2xx",
                            resp.status()
                        )),
                    );
                }

                // Verify content type is JavaScript
                let content_type = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();

                let body = resp
                    .text()
                    .change_context(TestError::ResponseParse)
                    .attach_printable(format!("framework: {framework_id}"))?;

                if !content_type.contains("javascript") {
                    return Err(error_stack::report!(TestError::ResponseParse)
                        .attach_printable(format!(
                            "Expected JavaScript content-type, got: {content_type}"
                        )));
                }

                // Verify body is non-empty and contains expected bundle markers
                if body.is_empty() {
                    return Err(error_stack::report!(TestError::ResponseParse)
                        .attach_printable("Script bundle body is empty"));
                }

                // The unified bundle should contain the TSJS core initialization
                if !body.contains("trustedserver") && !body.contains("tsjs") {
                    return Err(error_stack::report!(TestError::ResponseParse)
                        .attach_printable(
                            "Script bundle does not contain expected trustedserver/tsjs markers",
                        ));
                }

                Ok(())
            }

            Self::ScriptServingUnknownFile404 => {
                let url = format!("{base_url}/static/tsjs=unknown.js");

                let resp = reqwest::blocking::get(&url)
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: ScriptServingUnknownFile404, framework: {framework_id}"
                    ))?;

                let status = resp.status().as_u16();

                if status != 404 {
                    return Err(error_stack::report!(TestError::HttpRequest)
                        .attach_printable(format!(
                            "Expected 404 for unknown tsjs file, got {status}"
                        )));
                }

                // Response should not be HTML (which would indicate a fallback to origin)
                let content_type = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");

                if content_type.contains("text/html") {
                    return Err(error_stack::report!(TestError::ResponseParse)
                        .attach_printable(
                            "Unknown tsjs file returned HTML instead of a proper 404",
                        ));
                }

                Ok(())
            }
        }
    }
}

impl CustomScenario {
    /// Execute this custom scenario against a running runtime.
    ///
    /// # Errors
    ///
    /// Returns a [`TestError`] variant depending on which assertion fails.
    pub fn run(&self, base_url: &str, framework_id: &str) -> error_stack::Result<(), TestError> {
        match self {
            Self::NextJsRscFlight => {
                // Verify RSC Flight format responses are not corrupted.
                // When the proxy mishandles the RSC header, it returns text/html
                // instead of the expected Flight payload — so we fail on HTML.
                let client = reqwest::blocking::Client::new();
                let resp = client
                    .get(base_url)
                    .header("RSC", "1")
                    .header("Next-Router-State-Tree", "%5B%22%22%5D")
                    .send()
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: NextJsRscFlight, framework: {framework_id}"
                    ))?;

                let content_type = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();

                let body = resp.text().change_context(TestError::ResponseParse)?;

                // RSC responses must NOT be HTML — that means the proxy
                // swallowed the RSC header and treated it as a page request
                if content_type.contains("text/html") {
                    return Err(error_stack::report!(TestError::ResponseParse)
                        .attach_printable(format!(
                            "RSC request returned text/html instead of Flight payload (content-type: {content_type})"
                        )));
                }

                // If the response is a Flight payload, it must not contain injected scripts
                if body.contains("/static/tsjs=") {
                    return Err(error_stack::report!(TestError::ScriptTagNotFound)
                        .attach_printable(
                            "Script tag should NOT be injected in RSC Flight responses",
                        ));
                }

                Ok(())
            }

            Self::NextJsServerActions => {
                // Verify POST requests pass through the proxy to the origin.
                // The minimal Next.js app has no real server actions, so
                // Next.js returns 404 for the unknown action ID. This proves
                // the proxy forwarded the POST to the origin rather than
                // rejecting or mishandling it.
                let client = reqwest::blocking::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .change_context(TestError::HttpRequest)?;

                let resp = client
                    .post(base_url)
                    .header("Next-Action", "test-action-id")
                    .header("Content-Type", "text/plain;charset=UTF-8")
                    .body("[]")
                    .send()
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: NextJsServerActions, framework: {framework_id}"
                    ))?;

                let status = resp.status().as_u16();
                let body = resp.text().unwrap_or_default();

                // Next.js returns 404 for unknown server action IDs.
                // With App Router client components in the layout, Next.js
                // may return a "soft 404" (HTTP 200 with a not-found page).
                // Accept 200, 404, or 405 — all prove the proxy forwarded
                // the POST to the origin rather than rejecting it.
                match status {
                    404 | 405 => {}
                    200 => {
                        // Soft 404: verify the body is a Next.js not-found page
                        assert!(
                            body.contains("404") || body.contains("not found")
                                || body.contains("Not Found"),
                            "should contain 404 indicator in soft-404 response body"
                        );
                    }
                    _ => {
                        return Err(
                            error_stack::report!(TestError::HttpRequest).attach_printable(
                                format!(
                                    "Expected 200/404/405 for unknown server action, got {status}; body: {body}"
                                ),
                            ),
                        );
                    }
                }

                Ok(())
            }

            Self::NextJsApiRoute => {
                // Verify API routes return JSON without HTML injection.
                // The proxy should pass JSON responses through unchanged.
                let url = format!("{base_url}/api/hello");

                let resp = reqwest::blocking::get(&url)
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: NextJsApiRoute, framework: {framework_id}"
                    ))?;

                let status = resp.status().as_u16();
                if status != 200 {
                    return Err(
                        error_stack::report!(TestError::HttpRequest).attach_printable(format!(
                            "Expected 200 for API route, got {status}"
                        )),
                    );
                }

                let content_type = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();

                if !content_type.contains("application/json") {
                    return Err(error_stack::report!(TestError::ResponseParse)
                        .attach_printable(format!(
                            "Expected application/json content-type, got: {content_type}"
                        )));
                }

                let body = resp
                    .text()
                    .change_context(TestError::ResponseParse)
                    .attach_printable(format!("framework: {framework_id}"))?;

                // JSON responses must not contain HTML injection
                if body.contains("<script") || body.contains("/static/tsjs=") {
                    return Err(error_stack::report!(TestError::ResponseParse)
                        .attach_printable(
                            "API route response contains script injection — JSON should pass through unchanged",
                        ));
                }

                // Verify it's valid JSON with expected structure
                let json: serde_json::Value = serde_json::from_str(&body)
                    .map_err(|e| {
                        error_stack::report!(TestError::ResponseParse)
                            .attach_printable(format!("API response is not valid JSON: {e}"))
                    })?;

                if json.get("message").is_none() {
                    return Err(error_stack::report!(TestError::ResponseParse)
                        .attach_printable(
                            "API response missing expected 'message' field",
                        ));
                }

                Ok(())
            }

            Self::NextJsFormAction => {
                // Verify form action URLs are rewritten from origin to proxy.
                let url = format!("{base_url}/contact");

                let resp = reqwest::blocking::get(&url)
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: NextJsFormAction, framework: {framework_id}"
                    ))?;

                let html = resp
                    .text()
                    .change_context(TestError::ResponseParse)
                    .attach_printable(format!("framework: {framework_id}"))?;

                let origin_host = format!("127.0.0.1:{}", origin_port());

                assertions::assert_form_action_rewritten(
                    &html,
                    "form#contact-form",
                    &origin_host,
                    base_url,
                )
                .attach_printable(format!("framework: {framework_id}"))?;

                Ok(())
            }

            Self::WordPressAdminInjection => {
                // Verify that /wp-admin/ pages also receive script injection.
                // The trusted server injects into ALL HTML responses regardless
                // of path. This test documents that behavior — if admin-path
                // exclusion is added in the future, this test should be updated
                // to assert NO injection instead.
                let url = format!("{base_url}/wp-admin/");

                let resp = reqwest::blocking::get(&url)
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: WordPressAdminInjection, framework: {framework_id}"
                    ))?;

                let html = resp
                    .text()
                    .change_context(TestError::ResponseParse)
                    .attach_printable(format!("framework: {framework_id}"))?;

                assertions::assert_script_tag_present(&html)
                    .attach_printable(format!(
                        "Admin page should receive injection (current behavior). \
                         framework: {framework_id}"
                    ))?;

                Ok(())
            }
        }
    }
}
