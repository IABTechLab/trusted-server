use super::runtime::TestError;
use error_stack::{Result, ResultExt};
use scraper::{Html, Selector};

/// Assert that HTML contains script tag with expected tsjs module reference
///
/// # Errors
///
/// Returns [`TestError::ScriptTagNotFound`] if no matching script tag exists
/// Returns [`TestError::MissingModule`] if expected module ID is not found
pub fn assert_script_tag_present(html: &str, expected_modules: &[&str]) -> Result<(), TestError> {
    let document = Html::parse_document(html);

    // Find script tags with src attribute containing /static/tsjs=
    let selector = Selector::parse("script[src*='/static/tsjs=']")
        .change_context(TestError::InvalidSelector)?;

    let mut found_script = false;

    for element in document.select(&selector) {
        if let Some(src) = element.value().attr("src") {
            found_script = true;

            // Extract module IDs from URL query parameter
            // Expected format: /static/tsjs=core,prebid,lockr
            if let Some(query_part) = src.split("tsjs=").nth(1) {
                let modules: Vec<&str> = query_part.split(',').collect();

                // Verify all expected modules are present
                for expected in expected_modules {
                    if !modules.contains(expected) {
                        return Err(error_stack::report!(TestError::MissingModule {
                            module: (*expected).to_string()
                        }));
                    }
                }

                return Ok(());
            }
        }
    }

    if !found_script {
        return Err(error_stack::report!(TestError::ScriptTagNotFound));
    }

    Ok(())
}

/// Assert that HTML attributes are rewritten with expected prefix
///
/// Verifies that the trusted-server correctly rewrites attributes like href, src, srcset
/// to use the first-party proxy endpoint.
///
/// # Arguments
///
/// * `html` - HTML content to parse
/// * `selector` - CSS selector to find target elements (e.g., "a[href]", "img[src]")
/// * `attr_name` - Attribute name to check (e.g., "href", "src", "srcset")
/// * `expected_prefix` - Expected URL prefix after rewriting (e.g., "/first-party/proxy?url=")
///
/// # Errors
///
/// Returns [`TestError::InvalidSelector`] if CSS selector is malformed
/// Returns [`TestError::ElementNotFound`] if no matching elements exist
/// Returns [`TestError::AttributeNotRewritten`] if attribute doesn't have expected prefix
pub fn assert_attribute_rewritten(
    html: &str,
    selector: &str,
    attr_name: &str,
    expected_prefix: &str,
) -> Result<(), TestError> {
    let document = Html::parse_document(html);

    let parsed_selector =
        Selector::parse(selector).change_context(TestError::InvalidSelector)?;

    let mut found_element = false;

    for element in document.select(&parsed_selector) {
        if let Some(attr_value) = element.value().attr(attr_name) {
            found_element = true;

            if !attr_value.starts_with(expected_prefix) {
                return Err(error_stack::report!(TestError::AttributeNotRewritten));
            }
        }
    }

    if !found_element {
        return Err(error_stack::report!(TestError::ElementNotFound));
    }

    Ok(())
}

/// Assert that HTTP response contains GDPR consent signal
///
/// Verifies that the trusted-server correctly propagates GDPR consent signals
/// in response headers or body.
///
/// # Errors
///
/// Returns [`TestError::GdprSignalMissing`] if no GDPR signal is found
pub fn assert_gdpr_signal(response: &reqwest::blocking::Response) -> Result<(), TestError> {
    // Check for GDPR signal in headers
    // The trusted-server sets X-GDPR-Consent header when GDPR applies
    if response.headers().contains_key("X-GDPR-Consent") {
        return Ok(());
    }

    // Alternative: Check for GDPR signal in response body
    // Some integrations may embed GDPR state in the injected script
    if let Ok(body) = response.text() {
        if body.contains("gdprApplies") || body.contains("gdpr_consent") {
            return Ok(());
        }
    }

    Err(error_stack::report!(TestError::GdprSignalMissing))
}

/// Assert that HTML contains expected number of elements matching selector
///
/// # Errors
///
/// Returns [`TestError::InvalidSelector`] if CSS selector is malformed
pub fn assert_element_count(
    html: &str,
    selector: &str,
    expected_count: usize,
) -> Result<(), TestError> {
    let document = Html::parse_document(html);

    let parsed_selector =
        Selector::parse(selector).change_context(TestError::InvalidSelector)?;

    let actual_count = document.select(&parsed_selector).count();

    if actual_count != expected_count {
        return Err(error_stack::report!(TestError::ElementNotFound).attach_printable(
            format!(
                "Expected {} elements matching '{}', found {}",
                expected_count, selector, actual_count
            ),
        ));
    }

    Ok(())
}

/// Assert that script tag is injected at the start of `<head>`
///
/// This verifies the HTML processor correctly positions the script tag
/// to ensure early execution before other scripts.
///
/// # Errors
///
/// Returns [`TestError::ScriptTagNotFound`] if script is not at expected position
pub fn assert_script_position(html: &str) -> Result<(), TestError> {
    let document = Html::parse_document(html);

    // Find the <head> element
    let head_selector = Selector::parse("head").change_context(TestError::InvalidSelector)?;

    let head = document
        .select(&head_selector)
        .next()
        .ok_or_else(|| error_stack::report!(TestError::ElementNotFound))?;

    // Get first child of <head>
    let first_child = head
        .first_child()
        .ok_or_else(|| error_stack::report!(TestError::ElementNotFound))?;

    // Verify first child is our injected script tag
    if let Some(element) = first_child.value().as_element() {
        if element.name() == "script" {
            if let Some(src) = element.attr("src") {
                if src.contains("/static/tsjs=") {
                    return Ok(());
                }
            }
        }
    }

    Err(error_stack::report!(TestError::ScriptTagNotFound).attach_printable(
        "Script tag not found at start of <head>",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assert_script_tag_present_success() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head>
                <script src="/static/tsjs=core,prebid,lockr"></script>
            </head>
            <body></body>
            </html>
        "#;

        assert_script_tag_present(html, &["core", "prebid"])
            .expect("should find script tag with expected modules");
    }

    #[test]
    fn test_assert_script_tag_present_missing_module() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head>
                <script src="/static/tsjs=core,prebid"></script>
            </head>
            <body></body>
            </html>
        "#;

        let result = assert_script_tag_present(html, &["core", "lockr"]);
        assert!(
            result.is_err(),
            "should fail when expected module is missing"
        );
    }

    #[test]
    fn test_assert_attribute_rewritten_success() {
        let html = r#"
            <a href="/first-party/proxy?url=https://example.com">Link</a>
        "#;

        assert_attribute_rewritten(html, "a[href]", "href", "/first-party/proxy?url=")
            .expect("should find rewritten attribute");
    }

    #[test]
    fn test_assert_attribute_rewritten_not_rewritten() {
        let html = r#"
            <a href="https://example.com">Link</a>
        "#;

        let result = assert_attribute_rewritten(html, "a[href]", "href", "/first-party/proxy?url=");
        assert!(result.is_err(), "should fail when attribute not rewritten");
    }

    #[test]
    fn test_assert_element_count_success() {
        let html = r#"
            <div>
                <p>First</p>
                <p>Second</p>
                <p>Third</p>
            </div>
        "#;

        assert_element_count(html, "p", 3).expect("should count elements correctly");
    }

    #[test]
    fn test_assert_element_count_mismatch() {
        let html = r#"
            <div>
                <p>First</p>
                <p>Second</p>
            </div>
        "#;

        let result = assert_element_count(html, "p", 3);
        assert!(result.is_err(), "should fail when count doesn't match");
    }
}
