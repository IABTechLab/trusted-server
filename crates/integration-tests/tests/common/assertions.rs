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

/// Assert that exactly one `<script id="trustedserver-js">` exists in the HTML.
///
/// This is a stricter variant of [`assert_script_tag_present`] that verifies
/// uniqueness: the trusted-server must inject exactly one script tag with
/// `id="trustedserver-js"` and a `src` containing `/static/tsjs=`.
///
/// # Errors
///
/// Returns [`TestError::ScriptTagNotFound`] if zero or more than one matching
/// script tag is found.
pub fn assert_unique_script_tag(html: &str) -> Result<(), TestError> {
    let document = Html::parse_document(html);
    let selector = parse_selector("script#trustedserver-js[src*='/static/tsjs=']")?;

    let count = document.select(&selector).count();

    if count == 0 {
        return Err(error_stack::report!(TestError::ScriptTagNotFound)
            .attach_printable("No script#trustedserver-js[src*='/static/tsjs='] found"));
    }

    if count > 1 {
        return Err(error_stack::report!(TestError::ScriptTagNotFound).attach_printable(
            format!("Expected exactly 1 script#trustedserver-js, found {count}"),
        ));
    }

    Ok(())
}

/// Assert that origin host URLs in `href`/`src` attributes have been rewritten.
///
/// Checks that the proxied HTML no longer contains the origin host in `href`
/// or `src` attributes, and that at least one attribute now contains the proxy
/// host. This verifies the HTML processor's URL rewriting behavior.
///
/// # Errors
///
/// Returns [`TestError::AttributeNotRewritten`] if origin URLs remain or if
/// no proxy URLs are found.
pub fn assert_attributes_rewritten(
    html: &str,
    origin_host: &str,
    proxy_base_url: &str,
) -> Result<(), TestError> {
    let document = Html::parse_document(html);

    // Extract proxy host from base_url (e.g. "http://127.0.0.1:12345" -> "127.0.0.1:12345")
    let proxy_host = proxy_base_url
        .strip_prefix("http://")
        .or_else(|| proxy_base_url.strip_prefix("https://"))
        .unwrap_or(proxy_base_url);

    // Check all elements with href or src attributes
    let all_selector = parse_selector("[href], [src]")?;
    let mut found_proxy_url = false;

    for element in document.select(&all_selector) {
        for attr in ["href", "src"] {
            if let Some(value) = element.value().attr(attr) {
                // Origin host should NOT appear in any attribute
                if value.contains(origin_host) {
                    return Err(error_stack::report!(TestError::AttributeNotRewritten)
                        .attach_printable(format!(
                            "Origin host still present in {attr}=\"{value}\""
                        )));
                }

                // Track whether we find at least one rewritten proxy URL
                if value.contains(proxy_host) {
                    found_proxy_url = true;
                }
            }
        }
    }

    if !found_proxy_url {
        return Err(error_stack::report!(TestError::AttributeNotRewritten)
            .attach_printable(format!(
                "No attributes rewritten to proxy host ({proxy_host}). \
                 Check that the origin HTML contains absolute URLs with {origin_host}"
            )));
    }

    Ok(())
}

/// Assert that URL attributes inside ad-slot elements are rewritten.
///
/// Specifically checks that `href`/`src` attributes on elements *within*
/// `[data-ad-unit]` containers have been rewritten from origin to proxy host.
/// This catches regressions limited to ad markup that a page-wide scan might miss.
///
/// # Errors
///
/// Returns [`TestError::AttributeNotRewritten`] if origin URLs remain inside
/// ad slots or if no rewritten proxy URLs are found inside ad slots.
pub fn assert_ad_slot_urls_rewritten(
    html: &str,
    origin_host: &str,
    proxy_base_url: &str,
) -> Result<(), TestError> {
    let document = Html::parse_document(html);

    let proxy_host = proxy_base_url
        .strip_prefix("http://")
        .or_else(|| proxy_base_url.strip_prefix("https://"))
        .unwrap_or(proxy_base_url);

    // Select elements with href/src that are descendants of [data-ad-unit]
    let ad_link_selector = parse_selector("[data-ad-unit] [href], [data-ad-unit] [src]")?;
    let mut found_proxy_url = false;

    for element in document.select(&ad_link_selector) {
        for attr in ["href", "src"] {
            if let Some(value) = element.value().attr(attr) {
                if value.contains(origin_host) {
                    return Err(error_stack::report!(TestError::AttributeNotRewritten)
                        .attach_printable(format!(
                            "Origin host still present inside ad slot: {attr}=\"{value}\""
                        )));
                }
                if value.contains(proxy_host) {
                    found_proxy_url = true;
                }
            }
        }
    }

    if !found_proxy_url {
        return Err(error_stack::report!(TestError::AttributeNotRewritten).attach_printable(
            format!(
                "No URL attributes inside ad slots rewritten to proxy host ({proxy_host}). \
                 Ensure ad-slot fixtures contain href/src with origin host"
            ),
        ));
    }

    Ok(())
}

/// Assert that `data-ad-unit` attribute values are preserved unchanged.
///
/// The trusted-server rewrites URL-bearing attributes (`href`, `src`) but must
/// NOT modify non-URL attributes like `data-ad-unit`. This assertion verifies
/// that ad-slot markup passes through the proxy intact.
///
/// # Arguments
///
/// * `html` - The proxied HTML response body.
/// * `expected_units` - The `data-ad-unit` values that must be present unchanged.
///
/// # Errors
///
/// Returns [`TestError::AttributeNotRewritten`] if any expected `data-ad-unit`
/// value is missing from the proxied HTML.
pub fn assert_data_ad_units_preserved(html: &str, expected_units: &[&str]) -> Result<(), TestError> {
    let document = Html::parse_document(html);
    let selector = parse_selector("[data-ad-unit]")?;

    let found_units: Vec<String> = document
        .select(&selector)
        .filter_map(|el| el.value().attr("data-ad-unit").map(String::from))
        .collect();

    for expected in expected_units {
        if !found_units.iter().any(|u| u == expected) {
            return Err(error_stack::report!(TestError::AttributeNotRewritten)
                .attach_printable(format!(
                    "data-ad-unit=\"{expected}\" not found in proxied HTML. \
                     Found units: {found_units:?}"
                )));
        }
    }

    Ok(())
}

/// Assert that a form's `action` attribute has been rewritten from origin to proxy.
///
/// Finds a `<form>` matching the given CSS selector and verifies its `action`
/// attribute no longer contains the origin host and instead points to the proxy.
///
/// # Errors
///
/// Returns [`TestError::AttributeNotRewritten`] if the form is not found, has
/// no `action`, or the `action` still contains the origin host.
pub fn assert_form_action_rewritten(
    html: &str,
    form_selector: &str,
    origin_host: &str,
    proxy_base_url: &str,
) -> Result<(), TestError> {
    let document = Html::parse_document(html);
    let selector = parse_selector(form_selector)?;

    let proxy_host = proxy_base_url
        .strip_prefix("http://")
        .or_else(|| proxy_base_url.strip_prefix("https://"))
        .unwrap_or(proxy_base_url);

    let form = document.select(&selector).next().ok_or_else(|| {
        error_stack::report!(TestError::AttributeNotRewritten)
            .attach_printable(format!("No form matching '{form_selector}' found in HTML"))
    })?;

    let action = form.value().attr("action").ok_or_else(|| {
        error_stack::report!(TestError::AttributeNotRewritten)
            .attach_printable(format!("Form '{form_selector}' has no action attribute"))
    })?;

    if action.contains(origin_host) {
        return Err(error_stack::report!(TestError::AttributeNotRewritten).attach_printable(
            format!("Form action still contains origin host: action=\"{action}\""),
        ));
    }

    if !action.contains(proxy_host) {
        return Err(error_stack::report!(TestError::AttributeNotRewritten).attach_printable(
            format!(
                "Form action not rewritten to proxy host ({proxy_host}): action=\"{action}\""
            ),
        ));
    }

    Ok(())
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
    fn attributes_rewritten_detects_proxy_host() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head></head>
            <body>
                <a id="origin-link" href="http://127.0.0.1:9999/page">Link</a>
                <img id="origin-img" src="http://127.0.0.1:9999/images/test.png">
            </body>
            </html>
        "#;

        assert_attributes_rewritten(html, "127.0.0.1:8888", "http://127.0.0.1:9999")
            .expect("should detect rewritten proxy URLs");
    }

    #[test]
    fn attributes_rewritten_fails_when_origin_remains() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head></head>
            <body>
                <a href="http://127.0.0.1:8888/page">Link</a>
            </body>
            </html>
        "#;

        let result = assert_attributes_rewritten(html, "127.0.0.1:8888", "http://127.0.0.1:9999");
        assert!(
            result.is_err(),
            "should fail when origin host is still in attributes"
        );
    }

    #[test]
    fn attributes_rewritten_fails_when_no_proxy_urls() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head></head>
            <body>
                <a href="/relative/path">Link</a>
            </body>
            </html>
        "#;

        let result = assert_attributes_rewritten(html, "127.0.0.1:8888", "http://127.0.0.1:9999");
        assert!(
            result.is_err(),
            "should fail when no attributes contain proxy host"
        );
    }

    #[test]
    fn ad_slot_urls_rewritten_passes() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <body>
                <div data-ad-unit="/test/banner">
                    <a href="http://127.0.0.1:9999/ad/banner-landing">Banner</a>
                    <img src="http://127.0.0.1:9999/ad/banner.png">
                </div>
            </body>
            </html>
        "#;

        assert_ad_slot_urls_rewritten(html, "127.0.0.1:8888", "http://127.0.0.1:9999")
            .expect("should detect rewritten URLs inside ad slots");
    }

    #[test]
    fn ad_slot_urls_rewritten_fails_when_origin_remains() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <body>
                <div data-ad-unit="/test/banner">
                    <a href="http://127.0.0.1:8888/ad/banner-landing">Banner</a>
                </div>
            </body>
            </html>
        "#;

        let result =
            assert_ad_slot_urls_rewritten(html, "127.0.0.1:8888", "http://127.0.0.1:9999");
        assert!(
            result.is_err(),
            "should fail when origin host remains inside ad slot"
        );
    }

    #[test]
    fn ad_slot_urls_rewritten_fails_when_no_urls_in_slots() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <body>
                <div data-ad-unit="/test/banner">
                    <p>No links here</p>
                </div>
            </body>
            </html>
        "#;

        let result =
            assert_ad_slot_urls_rewritten(html, "127.0.0.1:8888", "http://127.0.0.1:9999");
        assert!(
            result.is_err(),
            "should fail when no URL attributes exist inside ad slots"
        );
    }

    #[test]
    fn unique_script_tag_passes_with_exactly_one() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head>
                <script src="/static/tsjs=tsjs-unified.min.js?v=abc123" id="trustedserver-js"></script>
            </head>
            <body></body>
            </html>
        "#;

        assert_unique_script_tag(html).expect("should find exactly one trustedserver-js");
    }

    #[test]
    fn unique_script_tag_fails_when_missing() {
        let html = r#"
            <!DOCTYPE html>
            <html><head></head><body></body></html>
        "#;

        let result = assert_unique_script_tag(html);
        assert!(result.is_err(), "should fail when no trustedserver-js exists");
    }

    #[test]
    fn unique_script_tag_fails_when_duplicated() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head>
                <script src="/static/tsjs=tsjs-unified.min.js?v=abc" id="trustedserver-js"></script>
                <script src="/static/tsjs=tsjs-unified.min.js?v=abc" id="trustedserver-js"></script>
            </head>
            <body></body>
            </html>
        "#;

        let result = assert_unique_script_tag(html);
        assert!(result.is_err(), "should fail when multiple trustedserver-js exist");
    }

    #[test]
    fn unique_script_tag_fails_when_no_id() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <head>
                <script src="/static/tsjs=tsjs-unified.min.js?v=abc"></script>
            </head>
            <body></body>
            </html>
        "#;

        let result = assert_unique_script_tag(html);
        assert!(result.is_err(), "should fail when script has no id");
    }

    #[test]
    fn data_ad_units_preserved_passes() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <body>
                <div data-ad-unit="/test/banner">Ad</div>
                <div data-ad-unit="/test/sidebar">Ad</div>
            </body>
            </html>
        "#;

        assert_data_ad_units_preserved(html, &["/test/banner", "/test/sidebar"])
            .expect("should find both ad units");
    }

    #[test]
    fn data_ad_units_preserved_fails_when_missing() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <body>
                <div data-ad-unit="/test/banner">Ad</div>
            </body>
            </html>
        "#;

        let result = assert_data_ad_units_preserved(html, &["/test/banner", "/test/sidebar"]);
        assert!(result.is_err(), "should fail when expected ad unit is missing");
    }

    #[test]
    fn form_action_rewritten_passes() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <body>
                <form id="contact-form" action="http://127.0.0.1:9999/api/contact" method="POST">
                    <input name="name" />
                </form>
            </body>
            </html>
        "#;

        assert_form_action_rewritten(html, "form#contact-form", "127.0.0.1:8888", "http://127.0.0.1:9999")
            .expect("should detect rewritten form action");
    }

    #[test]
    fn form_action_rewritten_fails_when_origin_remains() {
        let html = r#"
            <!DOCTYPE html>
            <html>
            <body>
                <form id="contact-form" action="http://127.0.0.1:8888/api/contact" method="POST">
                    <input name="name" />
                </form>
            </body>
            </html>
        "#;

        let result = assert_form_action_rewritten(
            html,
            "form#contact-form",
            "127.0.0.1:8888",
            "http://127.0.0.1:9999",
        );
        assert!(
            result.is_err(),
            "should fail when origin host remains in form action"
        );
    }

    #[test]
    fn form_action_rewritten_fails_when_no_form() {
        let html = r#"
            <!DOCTYPE html>
            <html><body><p>No form here</p></body></html>
        "#;

        let result = assert_form_action_rewritten(
            html,
            "form#contact-form",
            "127.0.0.1:8888",
            "http://127.0.0.1:9999",
        );
        assert!(result.is_err(), "should fail when form is not found");
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
