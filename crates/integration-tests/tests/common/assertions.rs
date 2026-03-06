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
        assert!(result.is_err(), "should fail when script tag is not tsjs");
    }
}
