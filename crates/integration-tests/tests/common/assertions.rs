use super::runtime::TestError;
use error_stack::Result;
use scraper::{Html, Selector};

/// Parse a CSS selector, mapping the error to [`TestError::InvalidSelector`]
fn parse_selector(selector: &str) -> Result<Selector, TestError> {
    Selector::parse(selector).map_err(|_| error_stack::report!(TestError::InvalidSelector))
}

/// Assert that HTML contains the trustedserver-js script tag.
///
/// Looks for `<script src="/static/tsjs=...">` injected by the trusted-server
/// HTML processor. The URL format is `/static/tsjs=tsjs-unified.min.js?v={hash}`.
///
/// # Errors
///
/// Returns [`TestError::ScriptTagNotFound`] if no matching script tag exists.
pub fn assert_script_tag_present(html: &str) -> Result<(), TestError> {
    let document = Html::parse_document(html);
    let selector = parse_selector("script[src*='/static/tsjs=']")?;

    if document.select(&selector).next().is_some() {
        return Ok(());
    }

    Err(error_stack::report!(TestError::ScriptTagNotFound))
}

/// Assert that HTML attributes matching a selector are rewritten with expected prefix.
///
/// Verifies that the trusted-server correctly rewrites attributes (e.g. `href`, `src`)
/// to use the first-party proxy endpoint.
///
/// # Errors
///
/// Returns [`TestError::InvalidSelector`] if CSS selector is malformed.
/// Returns [`TestError::ElementNotFound`] if no matching elements exist.
/// Returns [`TestError::AttributeNotRewritten`] if attribute does not have expected prefix.
pub fn assert_attribute_rewritten(
    html: &str,
    css_selector: &str,
    attr_name: &str,
    expected_prefix: &str,
) -> Result<(), TestError> {
    let document = Html::parse_document(html);
    let selector = parse_selector(css_selector)?;

    let mut found_element = false;

    for element in document.select(&selector) {
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

/// Assert that an HTTP response body contains GDPR consent signals.
///
/// Checks for GDPR signal presence in the response body text. The trusted-server
/// may embed GDPR state in injected scripts or propagate consent via response content.
///
/// # Errors
///
/// Returns [`TestError::GdprSignalMissing`] if no GDPR signal is found.
pub fn assert_gdpr_signal_in_body(body: &str) -> Result<(), TestError> {
    if body.contains("gdprApplies") || body.contains("gdpr_consent") || body.contains("__tcfapi") {
        return Ok(());
    }

    Err(error_stack::report!(TestError::GdprSignalMissing))
}

/// Assert that HTML contains expected number of elements matching a selector.
///
/// # Errors
///
/// Returns [`TestError::InvalidSelector`] if CSS selector is malformed.
/// Returns [`TestError::ElementNotFound`] if actual count differs from expected.
pub fn assert_element_count(
    html: &str,
    css_selector: &str,
    expected_count: usize,
) -> Result<(), TestError> {
    let document = Html::parse_document(html);
    let selector = parse_selector(css_selector)?;

    let actual_count = document.select(&selector).count();

    if actual_count != expected_count {
        return Err(
            error_stack::report!(TestError::ElementNotFound).attach_printable(format!(
                "Expected {} elements matching '{}', found {}",
                expected_count, css_selector, actual_count
            )),
        );
    }

    Ok(())
}

/// Assert that the tsjs script tag is injected at the start of `<head>`.
///
/// Verifies the HTML processor positions the script tag before other elements
/// to ensure early execution.
///
/// # Errors
///
/// Returns [`TestError::ScriptTagNotFound`] if script is not at expected position.
/// Returns [`TestError::ElementNotFound`] if `<head>` element is missing.
pub fn assert_script_position(html: &str) -> Result<(), TestError> {
    let document = Html::parse_document(html);
    let head_selector = parse_selector("head")?;

    let head = document
        .select(&head_selector)
        .next()
        .ok_or_else(|| error_stack::report!(TestError::ElementNotFound))?;

    // Check first element child of <head> is our script
    for child in head.children() {
        if let Some(element) = child.value().as_element() {
            if element.name() == "script" {
                if let Some(src) = element.attr("src") {
                    if src.contains("/static/tsjs=") {
                        return Ok(());
                    }
                }
            }
            // First element child is not our script
            break;
        }
    }

    Err(error_stack::report!(TestError::ScriptTagNotFound)
        .attach_printable("Script tag not found at start of <head>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_tag_present_with_unified_url() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head>
                <script src="/static/tsjs=tsjs-unified.min.js?v=abc123" id="trustedserver-js"></script>
            </head>
            <body></body>
            </html>
        "#;

        assert_script_tag_present(html).expect("should find trustedserver-js script tag");
    }

    #[test]
    fn script_tag_present_fails_when_no_script() {
        let html = r#"
            <!DOCTYPE html>
            <html><head></head><body></body></html>
        "#;

        let result = assert_script_tag_present(html);
        assert!(result.is_err(), "should fail when no script tag exists");
    }

    #[test]
    fn script_tag_present_fails_when_wrong_script() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head>
                <script src="/some/other/script.js"></script>
            </head>
            <body></body>
            </html>
        "#;

        let result = assert_script_tag_present(html);
        assert!(
            result.is_err(),
            "should fail when script tag is not tsjs"
        );
    }

    #[test]
    fn attribute_rewritten_with_expected_prefix() {
        let html = r#"
            <a href="/first-party/proxy?url=https://example.com">Link</a>
        "#;

        assert_attribute_rewritten(html, "a[href]", "href", "/first-party/proxy?url=")
            .expect("should find rewritten attribute");
    }

    #[test]
    fn attribute_rewritten_fails_when_not_rewritten() {
        let html = r#"
            <a href="https://example.com">Link</a>
        "#;

        let result = assert_attribute_rewritten(html, "a[href]", "href", "/first-party/proxy?url=");
        assert!(result.is_err(), "should fail when attribute not rewritten");
    }

    #[test]
    fn element_count_matches() {
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
    fn element_count_fails_on_mismatch() {
        let html = r#"
            <div>
                <p>First</p>
                <p>Second</p>
            </div>
        "#;

        let result = assert_element_count(html, "p", 3);
        assert!(result.is_err(), "should fail when count does not match");
    }

    #[test]
    fn gdpr_signal_detected_in_body() {
        assert_gdpr_signal_in_body("window.__tcfapi = function() {}")
            .expect("should detect TCF API signal");

        assert_gdpr_signal_in_body("var gdprApplies = true;")
            .expect("should detect gdprApplies signal");
    }

    #[test]
    fn gdpr_signal_missing_from_body() {
        let result = assert_gdpr_signal_in_body("<html><body>No GDPR here</body></html>");
        assert!(result.is_err(), "should fail when no GDPR signal exists");
    }
}
