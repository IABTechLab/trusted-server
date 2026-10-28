//! What a deployment shows to anyone who asks, with no credential.
//!
//! A publisher can point a reader, a regulator or an auditor at these
//! addresses, and each sees what this deployment decided for their own
//! request without having to take anybody's word for it. Every address is
//! fixed under `/_ts`, so it is the same on every deployment.
//!
//! | Address | What it shows |
//! | --- | --- |
//! | [`permissions::PERMISSIONS_PAGE_PATH`] | The permissions resolved for the request, the signals that produced them and the terms the data is held under |
//! | [`config::CONFIG_PAGE_PATH`] | The settings the deployment is running, with every secret and every value sensitive by default masked |
//! | [`data::DATA_PAGE_PATH`] | What the deployment holds against the Edge Cookie the request carries, with every identifier masked |
//!
//! The first two answer as a page, and as data with `.json` added. The third
//! is about one reader, so it answers a browser opening it as a page and
//! nothing else.
//!
//! Operator data is not here. It is in the platform's own stores, reached
//! with the platform's own authentication, and never through the publisher's
//! domain.

use serde_json::Value;

pub mod config;
pub mod data;
pub mod permissions;

/// What a masked value shows as.
pub const MASK: &str = "XXXX";

const DATA_FORM_LEDE: &str =
    "Add <code>.json</code> to this address for the same information as data.";

/// Renders `payload` as a page titled `title`.
#[must_use]
pub fn render_page(title: &str, payload: &Value) -> String {
    page(title, DATA_FORM_LEDE, payload)
}

/// Renders `payload` as [`render_page`] does, under `lede` in place of the
/// line saying where the data form is, for an address that has none.
#[must_use]
pub fn render_page_with_lede(title: &str, lede: &str, payload: &Value) -> String {
    page(title, &html_escape(lede), payload)
}

/// The page, with `lede` written into it as markup.
fn page(title: &str, lede: &str, payload: &Value) -> String {
    let pretty = serde_json::to_string_pretty(payload).unwrap_or_else(|_| "{}".to_owned());
    format!(
        "<!DOCTYPE html>\n\
<html lang=\"en\">\n\
<head>\n\
<meta charset=\"utf-8\">\n\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
<meta name=\"robots\" content=\"noindex\">\n\
<title>{title}</title>\n\
<style>\n\
body{{font-family:system-ui,-apple-system,\"Segoe UI\",Roboto,sans-serif;\
margin:0;padding:2rem 1rem;background:#fff;color:#232628;line-height:1.5}}\n\
main{{max-width:52rem;margin:0 auto}}\n\
h1{{font-size:1.4rem;margin:0 0 .25rem}}\n\
p.lede{{margin:0 0 1.5rem;color:#555}}\n\
pre{{background:#f5f5f5;padding:1rem;border-radius:6px;overflow-x:auto;\
font-size:.85rem;line-height:1.45}}\n\
@media(prefers-color-scheme:dark){{body{{background:#232628;color:#f4f4f4}}\
p.lede{{color:#bbb}}pre{{background:#1a1c1d}}}}\n\
</style>\n\
</head>\n\
<body>\n\
<main>\n\
<h1>{title}</h1>\n\
<p class=\"lede\">{lede}</p>\n\
<pre>{body}</pre>\n\
</main>\n\
</body>\n\
</html>\n",
        title = html_escape(title),
        body = html_escape(&pretty),
    )
}

/// Escapes `value` for HTML text content.
fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_page_escapes_whatever_its_payload_and_title_carry() {
        let html = render_page(
            "A <title>",
            &json!({ "value": "</pre><script>x()</script> & more" }),
        );

        assert!(
            !html.contains("<script>x()"),
            "should not let a value open a tag"
        );
        assert!(
            html.contains("&lt;/pre&gt;&lt;script&gt;x()&lt;/script&gt; &amp; more"),
            "should show the value as text"
        );
        assert!(
            html.contains("<title>A &lt;title&gt;</title>"),
            "should escape the title as well"
        );
    }

    #[test]
    fn a_page_tells_the_reader_where_the_data_form_is() {
        let html = render_page("Permissions", &json!({}));

        assert!(
            html.contains("<code>.json</code>"),
            "should say how to get the same information as data"
        );
        assert!(
            html.contains("<meta name=\"robots\" content=\"noindex\">"),
            "should keep the page out of search indexes"
        );
    }

    #[test]
    fn a_page_with_a_lede_of_its_own_says_that_instead() {
        let html = render_page_with_lede("Your data", "Held for <you> & no other", &json!({}));

        assert!(
            html.contains("<p class=\"lede\">Held for &lt;you&gt; &amp; no other</p>"),
            "should show the lede as text"
        );
        assert!(
            !html.contains(".json"),
            "should not point at a data form the address does not have"
        );
    }
}
