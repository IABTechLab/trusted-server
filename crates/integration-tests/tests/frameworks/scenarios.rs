use crate::common::assertions;
use crate::common::runtime::TestError;
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
}

/// Framework-specific custom scenarios that test framework-unique behaviors.
#[derive(Debug, Clone)]
pub enum CustomScenario {
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

                assertions::assert_script_tag_present(&html)
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
                // A 4xx is acceptable since we don't have real server actions,
                // but a connection error would indicate the proxy is blocking POSTs
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
