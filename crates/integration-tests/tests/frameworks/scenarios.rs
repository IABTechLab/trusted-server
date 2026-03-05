use crate::common::assertions;
use crate::common::runtime::TestError;
use error_stack::ResultExt as _;

/// Standard test scenarios applicable to all frontend frameworks.
///
/// Each scenario tests a core trusted-server behavior that should work
/// regardless of the underlying framework serving HTML.
#[derive(Debug, Clone)]
pub enum TestScenario {
    /// Verify `<script>` tag is injected into `<head>`.
    HtmlInjection,

    /// Verify `/static/tsjs=` endpoint serves JS bundles for given modules.
    ScriptServing { modules: Vec<&'static str> },

    /// Verify `tsjs-*` attributes are rewritten correctly.
    AttributeRewriting,

    /// Verify GDPR consent signals propagate through the response.
    GdprSignal,
}

/// Framework-specific custom scenarios that test framework-unique behaviors.
#[derive(Debug, Clone)]
pub enum CustomScenario {
    /// WordPress: script injection does not break admin pages.
    WordPressAdminInjection,

    /// Next.js: React Server Components Flight format is preserved.
    NextJsRscFlight,

    /// Next.js: Server Actions POST requests pass through correctly.
    NextJsServerActions,
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

                assertions::assert_script_tag_present(&html, &["core"])
                    .attach_printable(format!("framework: {framework_id}"))?;

                Ok(())
            }

            Self::ScriptServing { modules } => {
                let url = format!("{base_url}/static/tsjs={}", modules.join(","));

                let resp = reqwest::blocking::get(&url)
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: ScriptServing, framework: {framework_id}, modules: {modules:?}"
                    ))?;

                if !resp.status().is_success() {
                    return Err(
                        error_stack::report!(TestError::HttpRequest).attach_printable(format!(
                            "Script serving returned {}, expected 2xx",
                            resp.status()
                        )),
                    );
                }

                let body = resp.text().change_context(TestError::ResponseParse)?;

                if !body.contains("tsjs") {
                    return Err(error_stack::report!(TestError::ScriptTagNotFound)
                        .attach_printable("Response body does not contain tsjs marker"));
                }

                Ok(())
            }

            Self::AttributeRewriting => {
                let resp = reqwest::blocking::get(base_url)
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: AttributeRewriting, framework: {framework_id}"
                    ))?;

                let html = resp.text().change_context(TestError::ResponseParse)?;

                // Check for elements with data-ad-unit that should have tsjs- prefixed attributes
                assertions::assert_attribute_rewritten(
                    &html,
                    "[data-ad-unit]",
                    "data-ad-unit",
                    "tsjs-",
                )
                .attach_printable(format!("framework: {framework_id}"))?;

                Ok(())
            }

            Self::GdprSignal => {
                let resp = reqwest::blocking::get(base_url)
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!("scenario: GdprSignal, framework: {framework_id}"))?;

                let body = resp.text().change_context(TestError::ResponseParse)?;

                assertions::assert_gdpr_signal_in_body(&body)
                    .attach_printable(format!("framework: {framework_id}"))?;

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
            Self::WordPressAdminInjection => {
                // Verify that WordPress admin pages (/wp-admin/) do not get script injection
                let url = format!("{base_url}/wp-admin/");
                let resp = reqwest::blocking::get(&url)
                    .change_context(TestError::HttpRequest)
                    .attach_printable(format!(
                        "scenario: WordPressAdminInjection, framework: {framework_id}"
                    ))?;

                let html = resp.text().change_context(TestError::ResponseParse)?;

                // Admin pages should NOT have tsjs script injected
                if html.contains("/static/tsjs=") {
                    return Err(error_stack::report!(TestError::ScriptTagNotFound)
                        .attach_printable(
                            "Script tag should NOT be injected on WordPress admin pages",
                        ));
                }

                Ok(())
            }

            Self::NextJsRscFlight => {
                // Verify RSC Flight format responses are not corrupted
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
                    .unwrap_or("");

                // RSC responses should have text/x-component content type
                // and should NOT have script injection (they're not HTML)
                if content_type.contains("text/x-component") {
                    let body = resp.text().change_context(TestError::ResponseParse)?;

                    if body.contains("/static/tsjs=") {
                        return Err(error_stack::report!(TestError::ScriptTagNotFound)
                            .attach_printable(
                                "Script tag should NOT be injected in RSC Flight responses",
                            ));
                    }
                }

                Ok(())
            }

            Self::NextJsServerActions => {
                // Verify POST requests for server actions pass through correctly
                let client = reqwest::blocking::Client::new();
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

                // Server action responses should pass through without modification
                // A 4xx or 5xx error is acceptable here since we don't have real server actions,
                // but a connection error would indicate the proxy is blocking POST requests
                if resp.status().is_server_error() {
                    return Err(
                        error_stack::report!(TestError::HttpRequest).attach_printable(format!(
                            "Server action request failed with {}",
                            resp.status()
                        )),
                    );
                }

                Ok(())
            }
        }
    }
}
