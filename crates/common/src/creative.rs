//! Creative HTML/CSS rewriting utilities.
//!
//! Goals:
//! - Normalize external asset fetches in ad creatives (HTML/CSS) to a single
//!   first-party proxy endpoint so the publisher can control egress.
//! - Leave relative URLs and non-network schemes untouched.
//!
//! Key behaviors:
//! - Absolute and protocol-relative URLs (http/https or `//`) are proxied to
//!   `/first-party/proxy?tsurl=<base-url>&<original-query-params>&tstoken=<sig>` across these locations:
//!   - `<img src>`, `data-src`, `[srcset]`, `[imagesrcset]`
//!   - `<script src>`
//!   - `<video src>`, `<audio src>`, `<source src>`
//!   - `<object data>`, `<embed src>`
//!   - `<input type="image" src>`
//!   - SVG: `<image href|xlink:href>`, `<use href|xlink:href>`
//!   - `<iframe src>`
//!   - `<link rel~="stylesheet|preload|prefetch" href>` and `imagesrcset`
//!   - Inline styles (`[style]`) and `<style>` blocks: url(...) values are rewritten
//! - Relative URLs (e.g., `/path`, `../path`, `local/file`) remain unchanged.
//! - Non-network schemes are ignored: `data:`, `javascript:`, `mailto:`, `tel:`,
//!   `blob:`, `about:`.
//!
//! Notable helpers:
//! - `to_abs(&str) -> Option<String>`: Normalizes a string to an absolute URL if
//!   it is already absolute or protocol-relative; returns `None` otherwise or for
//!   non-network schemes.
//! - `rewrite_srcset(&str, &Settings) -> String`: Rewrites `srcset`/`imagesrcset`
//!   values, proxying absolute candidates and preserving descriptors (`1x`,
//!   `1.5x`, `100w`).
//! - `split_srcset_candidates(&str) -> Vec<&str>`: Robust splitting that supports
//!   commas with or without spaces and avoids splitting the mediatype/data comma
//!   in a leading `data:` URL.
//! - `rewrite_css_body(&str, &Settings) -> String`: Rewrites url(...) occurrences
//!   inside CSS bodies.
//!
//! See the tests in this module for comprehensive cases, including irregular
//! spacing, no-space commas, and `data:` handling.

use crate::http_util::compute_encrypted_sha256_token;
use crate::settings::Settings;
use crate::streaming_processor::StreamProcessor;
use crate::tsjs;
use lol_html::{element, html_content::ContentType, text, HtmlRewriter, Settings as HtmlSettings};
use std::io;

// Helper: normalize to absolute URL if http/https or protocol-relative. Otherwise None.
// Checks against the rewrite blacklist to exclude configured domains/patterns from proxying.
pub(super) fn to_abs(u: &str, settings: &Settings) -> Option<String> {
    let t = u.trim();
    if t.is_empty() {
        return None;
    }

    // Skip if excluded from rewrites in settings
    if settings.rewrite.is_excluded(t) {
        return None;
    }

    // Skip non-network schemes commonly found in creatives
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("data:")
        || lower.starts_with("javascript:")
        || lower.starts_with("mailto:")
        || lower.starts_with("tel:")
        || lower.starts_with("blob:")
        || lower.starts_with("about:")
    {
        return None;
    }

    if t.starts_with("//") {
        Some(format!("https:{}", t))
    } else if lower.starts_with("http://") || lower.starts_with("https://") {
        Some(t.to_string())
    } else {
        None
    }
}

// Helper: rewrite url(...) occurrences inside a CSS style string to first-party proxy.
pub(super) fn rewrite_style_urls(style: &str, settings: &Settings) -> String {
    // naive url(...) rewrite for absolute/protocol-relative URLs
    let lower = style.to_ascii_lowercase();
    let mut out = String::with_capacity(style.len() + 16);
    let mut write_pos = 0usize;
    let mut scan = 0usize;
    while let Some(off) = lower[scan..].find("url(") {
        let start = scan + off;
        let open = start + 4; // after 'url('
                              // write prefix including 'url('
        out.push_str(&style[write_pos..open]);
        // find closing ')'
        let close = match lower[open..].find(')') {
            Some(c) => open + c,
            None => {
                out.push_str(&style[open..]);
                return out;
            }
        };
        // trim spaces and quotes
        let bytes = style.as_bytes();
        let mut s = open;
        while s < close && bytes[s].is_ascii_whitespace() {
            s += 1;
        }
        let mut e = close;
        while e > s && bytes[e - 1].is_ascii_whitespace() {
            e -= 1;
        }
        let mut quoted = false;
        let (qs, qe) = if s < e && (bytes[s] == b'"' || bytes[s] == b'\'') {
            quoted = true;
            (s + 1, if e > s + 1 { e - 1 } else { e })
        } else {
            (s, e)
        };
        let url_val = &style[qs..qe];
        let new_val = if let Some(abs) = to_abs(url_val, settings) {
            build_proxy_url(settings, &abs)
        } else {
            url_val.to_string()
        };
        if quoted {
            let q = style.as_bytes()[s] as char;
            out.push(q);
            out.push_str(&new_val);
            out.push(q);
        } else {
            out.push_str(&new_val);
        }
        out.push(')');
        write_pos = close + 1;
        scan = write_pos;
    }
    out.push_str(&style[write_pos..]);
    out
}

#[inline]
fn build_signed_url_for(
    settings: &Settings,
    clear_url: &str,
    base_path: &str,
    extra: &[(String, String)],
) -> String {
    let Ok(mut u) = url::Url::parse(clear_url) else {
        return clear_url.to_string();
    };

    let mut pairs: Vec<(String, String)> = u
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    if !extra.is_empty() {
        pairs.extend(extra.iter().cloned());
    }

    u.set_query(None);
    u.set_fragment(None);
    let tsurl = u.as_str().to_string();

    let full_for_token = if pairs.is_empty() {
        tsurl.clone()
    } else {
        let mut s = url::form_urlencoded::Serializer::new(String::new());
        for (k, v) in &pairs {
            s.append_pair(k, v);
        }
        format!("{}?{}", tsurl, s.finish())
    };

    let token = compute_encrypted_sha256_token(settings, &full_for_token);

    let mut qs = url::form_urlencoded::Serializer::new(String::new());
    qs.append_pair("tsurl", &tsurl);
    for (k, v) in &pairs {
        qs.append_pair(k, v);
    }
    qs.append_pair("tstoken", &token);
    format!("{}?{}", base_path, qs.finish())
}

#[inline]
pub(super) fn build_proxy_url(settings: &Settings, clear_url: &str) -> String {
    build_signed_url_for(settings, clear_url, "/first-party/proxy", &[])
}

#[inline]
pub(super) fn build_proxy_url_with_extras(
    settings: &Settings,
    clear_url: &str,
    extra: &[(String, String)],
) -> String {
    build_signed_url_for(settings, clear_url, "/first-party/proxy", extra)
}

#[inline]
pub(super) fn build_click_url(settings: &Settings, clear_url: &str) -> String {
    build_signed_url_for(settings, clear_url, "/first-party/click", &[])
}

// Note: previously we exposed canonical without token; now we store the full signed
// click URL in data-tsclick and derive canonicals on the client when needed.

#[inline]
pub(super) fn proxy_if_abs(settings: &Settings, val: &str) -> Option<String> {
    to_abs(val, settings).map(|abs| build_proxy_url(settings, &abs))
}

/// Split a srcset/imagesrcset attribute into candidate strings.
/// - Splits on commas that separate candidates; whitespace after the comma is optional
/// - Avoids splitting on the mediatype/data comma of a leading `data:` URL
///   (e.g., `data:image/png;base64,AAAA 1x, ...`).
///   Note: this implementation only protects the first mediatype/data comma; it does not
///   attempt to handle additional commas inside a `data:` payload (rare in ad creatives).
pub(super) fn split_srcset_candidates(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut items = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b',' {
            // Determine if this comma is the mediatype/data separator in a data: URL.
            // Look at the current candidate prefix from `start` to `i` and see if it begins with
            // `data:` (ignoring leading whitespace) and has no whitespace before this comma.
            let prefix = &s[start..i];
            let trimmed = prefix.trim_start();
            let lower = trimmed.to_ascii_lowercase();
            let is_data_scheme = lower.starts_with("data:");
            let has_ws_before_comma = trimmed.chars().any(|c| c.is_ascii_whitespace());
            let comma_is_data_delim = is_data_scheme && !has_ws_before_comma;
            if comma_is_data_delim {
                // Skip splitting at this comma; it's within the data: URL itself
                i += 1;
                continue;
            }

            // This is a candidate separator. Push item and advance start past comma and any spaces.
            let piece = &s[start..i];
            items.push(piece);
            i += 1; // skip comma
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            start = i;
            continue;
        }
        i += 1;
    }
    if start < bytes.len() {
        items.push(&s[start..]);
    }
    items
}

/// Helper: rewrite a `srcset`/`imagesrcset` attribute value.
/// - Proxies absolute or protocol-relative candidates via first-party endpoint
/// - Preserves descriptors (e.g., `1x`, `1.5x`, `100w`)
/// - Leaves relative candidates unchanged
pub(super) fn rewrite_srcset(srcset: &str, settings: &Settings) -> String {
    let mut out_items: Vec<String> = Vec::new();
    for item in split_srcset_candidates(srcset) {
        let it = item.trim();
        if it.is_empty() {
            continue;
        }
        let mut parts = it.split_whitespace();
        let url = parts.next().unwrap_or("");
        let descriptor = parts.collect::<Vec<_>>().join(" ");
        let rewritten = if let Some(abs) = to_abs(url, settings) {
            build_proxy_url(settings, &abs)
        } else {
            url.to_string()
        };
        if descriptor.is_empty() {
            out_items.push(rewritten);
        } else {
            out_items.push(format!("{} {}", rewritten, descriptor));
        }
    }
    out_items.join(", ")
}

#[inline]
pub(super) fn proxied_attr_value(settings: &Settings, attr_val: Option<String>) -> Option<String> {
    match attr_val {
        Some(v) => proxy_if_abs(settings, &v),
        None => None,
    }
}

/// Rewrite a full CSS stylesheet body by normalizing url(...) references to the
/// unified first-party proxy. Relative URLs are left unchanged.
pub fn rewrite_css_body(css: &str, settings: &Settings) -> String {
    rewrite_style_urls(css, settings)
}

/// Rewrite ad creative HTML to first-party endpoints.
/// - 1x1 `<img>` pixels → `/first-party/proxy?tsurl=&lt;base-url&gt;&lt;params&gt;&tstoken=&lt;sig&gt;`
/// - Non-pixel absolute images → `/first-party/proxy?tsurl=&lt;base-url&gt;&lt;params&gt;&tstoken=&lt;sig&gt;`
/// - `<iframe src>` (absolute or protocol-relative) → `/first-party/proxy?tsurl=&lt;base-url&gt;&lt;params&gt;&tstoken=&lt;sig&gt;`
/// - Injects the `tsjs-creative` script once at the top of `<body>` to safeguard click URLs inside creatives
///   (served from `/static/tsjs=tsjs-creative.min.js`).
pub fn rewrite_creative_html(markup: &str, settings: &Settings) -> String {
    // No size parsing needed now; all absolute/protocol-relative URLs are proxied uniformly.
    let mut out = Vec::with_capacity(markup.len() + 64);
    let injected_ts_creative = std::cell::Cell::new(false);
    let mut rewriter = HtmlRewriter::new(
        HtmlSettings {
            element_content_handlers: vec![
                // Inject unified tsjs bundle at the top of body once
                element!("body", {
                    let injected = injected_ts_creative.clone();
                    move |el| {
                        if !injected.get() {
                            let script_tag = tsjs::unified_script_tag();
                            el.prepend(&script_tag, ContentType::Html);
                            injected.set(true);
                        }
                        Ok(())
                    }
                }),
                // Image src + data-src
                element!("img", |el| {
                    if let Some(src) = el.get_attribute("src") {
                        if let Some(p) = proxy_if_abs(settings, &src) {
                            let _ = el.set_attribute("src", &p);
                        }
                    }
                    if let Some(dsrc) = el.get_attribute("data-src") {
                        if let Some(p) = proxy_if_abs(settings, &dsrc) {
                            let _ = el.set_attribute("data-src", &p);
                        }
                    }
                    Ok(())
                }),
                // External scripts
                element!("script[src]", |el| {
                    if let Some(p) = proxied_attr_value(settings, el.get_attribute("src")) {
                        let _ = el.set_attribute("src", &p);
                    }
                    Ok(())
                }),
                // Stylesheets and preloads
                element!("link[href]", |el| {
                    let rel = el
                        .get_attribute("rel")
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    if rel.contains("stylesheet")
                        || rel.contains("preload")
                        || rel.contains("prefetch")
                    {
                        if let Some(p) = proxied_attr_value(settings, el.get_attribute("href")) {
                            let _ = el.set_attribute("href", &p);
                        }
                        if let Some(srcset) = el.get_attribute("imagesrcset") {
                            let rewritten = rewrite_srcset(&srcset, settings);
                            if rewritten != srcset {
                                let _ = el.set_attribute("imagesrcset", &rewritten);
                            }
                        }
                    }
                    Ok(())
                }),
                // Media sources
                element!("video[src], audio[src], source[src]", |el| {
                    if let Some(p) = proxied_attr_value(settings, el.get_attribute("src")) {
                        let _ = el.set_attribute("src", &p);
                    }
                    Ok(())
                }),
                // Object/embed
                element!("object[data]", |el| {
                    if let Some(p) = proxied_attr_value(settings, el.get_attribute("data")) {
                        let _ = el.set_attribute("data", &p);
                    }
                    Ok(())
                }),
                element!("embed[src]", |el| {
                    if let Some(p) = proxied_attr_value(settings, el.get_attribute("src")) {
                        let _ = el.set_attribute("src", &p);
                    }
                    Ok(())
                }),
                // Input type=image
                element!("input[src]", |el| {
                    if let Some(t) = el.get_attribute("type") {
                        if !t.eq_ignore_ascii_case("image") {
                            return Ok(());
                        }
                    } else {
                        return Ok(());
                    }
                    if let Some(p) = proxied_attr_value(settings, el.get_attribute("src")) {
                        let _ = el.set_attribute("src", &p);
                    }
                    Ok(())
                }),
                // SVG hrefs
                element!(
                    "image[href], image[xlink\\:href], use[href], use[xlink\\:href]",
                    |el| {
                        for attr in ["href", "xlink:href"] {
                            if let Some(p) = proxied_attr_value(settings, el.get_attribute(attr)) {
                                let _ = el.set_attribute(attr, &p);
                            }
                        }
                        Ok(())
                    }
                ),
                // Click-through links
                element!("a[href], area[href]", |el| {
                    if let Some(href) = el.get_attribute("href") {
                        if let Some(abs) = to_abs(&href, settings) {
                            let click = build_click_url(settings, &abs);
                            let _ = el.set_attribute("href", &click);
                            let _ = el.set_attribute("data-tsclick", &click);
                        }
                    }
                    Ok(())
                }),
                // Inline style url(...)
                element!("[style]", |el| {
                    if let Some(st) = el.get_attribute("style") {
                        let rewritten = rewrite_style_urls(&st, settings);
                        if rewritten != st {
                            let _ = el.set_attribute("style", &rewritten);
                        }
                    }
                    Ok(())
                }),
                // <style> blocks
                text!("style", |t| {
                    let s = t.as_str();
                    let rewritten = rewrite_style_urls(s, settings);
                    if rewritten != s {
                        t.replace(&rewritten, ContentType::Text);
                    }
                    Ok(())
                }),
                // iframes
                element!("iframe", |el| {
                    if let Some(src) = el.get_attribute("src") {
                        if let Some(p) = proxy_if_abs(settings, src.as_str()) {
                            let _ = el.set_attribute("src", &p);
                        }
                    }
                    Ok(())
                }),
                // srcset + imagesrcset
                element!("[srcset]", |el| {
                    if let Some(srcset) = el.get_attribute("srcset") {
                        let rewritten = rewrite_srcset(&srcset, settings);
                        if rewritten != srcset {
                            let _ = el.set_attribute("srcset", &rewritten);
                        }
                    }
                    Ok(())
                }),
                element!("[imagesrcset]", |el| {
                    if let Some(srcset) = el.get_attribute("imagesrcset") {
                        let rewritten = rewrite_srcset(&srcset, settings);
                        if rewritten != srcset {
                            let _ = el.set_attribute("imagesrcset", &rewritten);
                        }
                    }
                    Ok(())
                }),
            ],
            ..HtmlSettings::default()
        },
        |c: &[u8]| out.extend_from_slice(c),
    );

    let _ = rewriter.write(markup.as_bytes());
    let _ = rewriter.end();
    String::from_utf8(out).unwrap_or_else(|_| markup.to_string())
}

/// Stream processor for creative HTML that rewrites URLs to first-party proxy.
///
/// This processor buffers input chunks and processes the complete HTML document
/// when the stream ends, using `rewrite_creative_html` internally.
pub struct CreativeHtmlProcessor {
    settings: Settings,
    buffer: Vec<u8>,
}

impl CreativeHtmlProcessor {
    /// Create a new HTML processor with the given settings.
    pub fn new(settings: &Settings) -> Self {
        Self {
            settings: settings.clone(),
            buffer: Vec::new(),
        }
    }
}

impl StreamProcessor for CreativeHtmlProcessor {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
        self.buffer.extend_from_slice(chunk);

        if is_last {
            let markup = String::from_utf8(std::mem::take(&mut self.buffer))
                .map_err(|e| io::Error::other(format!("Invalid UTF-8 in HTML: {}", e)))?;

            let rewritten = rewrite_creative_html(&markup, &self.settings);
            Ok(rewritten.into_bytes())
        } else {
            Ok(Vec::new())
        }
    }

    fn reset(&mut self) {
        self.buffer.clear();
    }
}

/// Stream processor for CSS that rewrites url() references to first-party proxy.
///
/// This processor buffers input chunks and processes the complete CSS
/// when the stream ends, using `rewrite_css_body` internally.
pub struct CreativeCssProcessor {
    settings: Settings,
    buffer: Vec<u8>,
}

impl CreativeCssProcessor {
    /// Create a new CSS processor with the given settings.
    pub fn new(settings: &Settings) -> Self {
        Self {
            settings: settings.clone(),
            buffer: Vec::new(),
        }
    }
}

impl StreamProcessor for CreativeCssProcessor {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
        self.buffer.extend_from_slice(chunk);

        if is_last {
            let css = String::from_utf8(std::mem::take(&mut self.buffer))
                .map_err(|e| io::Error::other(format!("Invalid UTF-8 in CSS: {}", e)))?;

            let rewritten = rewrite_css_body(&css, &self.settings);
            Ok(rewritten.into_bytes())
        } else {
            Ok(Vec::new())
        }
    }

    fn reset(&mut self) {
        self.buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{rewrite_creative_html, rewrite_srcset, rewrite_style_urls, to_abs};

    #[test]
    fn rewrites_width_height_attrs() {
        use crate::http_util::encode_url;
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<div><img width="1" height="1" src="https://t.example/p.gif"></div>"#;
        let out = rewrite_creative_html(html, &settings);
        let _expected = encode_url(&settings, "https://t.example/p.gif");
        assert!(out.contains("/first-party/proxy?tsurl="), "{}", out);
    }

    #[test]
    fn injects_tsjs_creative_when_body_present() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<html><body><p>hello</p></body></html>"#;
        let out = rewrite_creative_html(html, &settings);
        assert!(
            out.contains("/static/tsjs=tsjs-unified.min.js"),
            "expected unified tsjs injection: {}",
            out
        );
        // Inject only once
        assert_eq!(out.matches("/static/tsjs=tsjs-unified.min.js").count(), 1);
    }

    #[test]
    fn injects_tsjs_unified_once_with_multiple_bodies() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<html><body>one</body><body>two</body></html>"#;
        let out = rewrite_creative_html(html, &settings);
        assert_eq!(out.matches("/static/tsjs=tsjs-unified.min.js").count(), 1);
    }

    #[test]
    fn to_abs_conversions() {
        let settings = crate::test_support::tests::create_test_settings();
        assert_eq!(
            to_abs("//cdn.example/x", &settings),
            Some("https://cdn.example/x".to_string())
        );
        assert_eq!(
            to_abs("HTTPS://cdn.example/x", &settings),
            Some("HTTPS://cdn.example/x".to_string())
        );
        assert_eq!(
            to_abs("http://cdn.example/x", &settings),
            Some("http://cdn.example/x".to_string())
        );
        assert_eq!(to_abs("/local/x", &settings), None);
        assert_eq!(
            to_abs("   //cdn.example/y  ", &settings),
            Some("https://cdn.example/y".to_string())
        );
        assert_eq!(to_abs("data:image/png;base64,abcd", &settings), None);
        assert_eq!(to_abs("javascript:alert(1)", &settings), None);
        assert_eq!(to_abs("mailto:test@example.com", &settings), None);
    }

    #[test]
    fn rewrite_style_urls_handles_absolute_and_relative() {
        let settings = crate::test_support::tests::create_test_settings();
        let css = "background:url(https://cdn.example/a.png) no-repeat; mask: url('//cdn.example/m.svg') 0 0 / cover; border-image: url(/local/border.png) 30";
        let out = rewrite_style_urls(css, &settings);
        // Absolute and protocol-relative rewritten
        assert!(
            out.matches("/first-party/proxy?tsurl=").count() >= 2,
            "{}",
            out
        );
        // Relative left as-is
        assert!(out.contains("url(/local/border.png)"));
    }

    #[test]
    fn rewrites_style_1x1_px() {
        use crate::http_util::encode_url;
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<img style="width:1px; height:1px" src="https://t.example/a.png">"#;
        let out = rewrite_creative_html(html, &settings);
        let _expected = encode_url(&settings, "https://t.example/a.png");
        assert!(out.contains("/first-party/proxy?tsurl="));
    }

    #[test]
    fn rewrites_style_1x1_no_units_and_messy_spacing() {
        use crate::http_util::encode_url;
        let settings = crate::test_support::tests::create_test_settings();
        let html =
            r#"<img style="  HEIGHT : 1 ;   width: 1  ; display:block" src="//cdn.example/p">"#;
        let out = rewrite_creative_html(html, &settings);
        let _expected = encode_url(&settings, "https://cdn.example/p");
        assert!(out.contains("/first-party/proxy?tsurl="));
    }

    #[test]
    fn rewrites_non_1x1_absolute_image_and_leaves_relative() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"
          <img width="300" height="250" src="https://t.example/a.gif">
          <img width="300" height="250" src="/local/pixel.gif">
        "#;
        let out = rewrite_creative_html(html, &settings);
        // Absolute image should be rewritten through first-party unified proxy
        assert!(out.contains("/first-party/proxy?tsurl="));
        // Original absolute URL may be transformed; ensure first-party path present
        // Relative should remain unchanged
        assert!(out.contains("/local/pixel.gif"));
    }

    #[test]
    fn rewrites_iframe_src_absolute_and_protocol_relative() {
        use crate::http_util::encode_url;
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<iframe src="https://cdn.example/ad.html"></iframe>"#;
        let out = rewrite_creative_html(html, &settings);
        let _expected = encode_url(&settings, "https://cdn.example/ad.html");
        assert!(out.contains("/first-party/proxy?tsurl="));

        let html2 = r#"<iframe src="//cdn.example/ad.html"></iframe>"#;
        let out2 = rewrite_creative_html(html2, &settings);
        assert!(out2.contains("/first-party/proxy?tsurl="));

        let html3 = r#"<iframe src="/local/ad.html"></iframe>"#;
        let out3 = rewrite_creative_html(html3, &settings);
        assert!(out3.contains("<iframe src=\"/local/ad.html\""));
        assert!(!out3.contains("/first-party/proxy?tsurl="));
    }

    #[test]
    fn rewrites_srcset_absolute_candidates_and_preserves_descriptors() {
        // Absolute + protocol-relative get rewritten to /first-party/proxy?tsurl=..., relative remains
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<img srcset="https://cdn.example/img-1x.png 1x, //cdn.example/img-2x.png 2x, /local/img.png 1x">"#;
        let out = rewrite_creative_html(html, &settings);

        // Should have at least two proxied candidates
        let cnt = out.matches("/first-party/proxy?tsurl=").count();
        assert!(
            cnt >= 2,
            "expected at least two rewritten candidates: {}",
            out
        );

        // Descriptors preserved
        assert!(out.contains(" 1x"));
        assert!(out.contains(" 2x"));

        // Relative left as-is
        assert!(out.contains("/local/img.png 1x"));
    }

    #[test]
    fn rewrites_source_srcset_inside_picture() {
        // Ensure <source srcset> inside <picture> is also rewritten
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"
          <picture>
            <source type="image/webp" srcset="https://cdn.example/img-1x.webp 1x, //cdn.example/img-2x.webp 2x, /local/img.webp 1x">
            <img src="/fallback.jpg" alt="">
          </picture>
        "#;
        let out = rewrite_creative_html(html, &settings);
        // Two rewritten absolute candidates expected
        assert!(
            out.matches("/first-party/proxy?tsurl=").count() >= 2,
            "srcset not fully rewritten: {}",
            out
        );
        // Relative preserved
        assert!(out.contains("/local/img.webp 1x"));
        // Fallback img unchanged (relative)
        assert!(out.contains("<img src=\"/fallback.jpg\""));
    }

    #[test]
    fn rewrites_srcset_no_space_commas_preserves_descriptors() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<img srcset="https://cdn.example/img-1x.png 1x,//cdn.example/img-1_5x.png 1.5x,/local/img.png 2x">"#;
        let out = rewrite_creative_html(html, &settings);
        // Absolute and protocol-relative candidates rewritten
        assert!(
            out.matches("/first-party/proxy?tsurl=").count() >= 2,
            "{}",
            out
        );
        // Descriptors preserved (including fractional)
        assert!(out.contains(" 1x"));
        assert!(out.contains(" 1.5x"));
        // Relative left unchanged
        assert!(out.contains("/local/img.png 2x"));
    }

    #[test]
    fn rewrites_srcset_relative_no_space_middle() {
        let settings = crate::test_support::tests::create_test_settings();
        // Relative candidate (no leading slash) in the middle, no space after commas
        let html =
            r#"<img srcset="https://cdn.example/a.png 1x,local/b.png 2x,//cdn.example/c.png 3x">"#;
        let out = rewrite_creative_html(html, &settings);
        // Two absolute/protocol-relative rewritten
        assert!(
            out.matches("/first-party/proxy?tsurl=").count() >= 2,
            "{}",
            out
        );
        // Relative preserved as-is
        assert!(out.contains("local/b.png 2x"));
    }

    #[test]
    fn rewrites_srcset_with_extra_spaces() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<img srcset="  https://cdn.example/a.png    1x  ,  //cdn.example/b.png   2x ,   /local/c.png   1x  ">"#;
        let out = rewrite_creative_html(html, &settings);
        // Two absolute/protocol-relative rewritten
        assert!(
            out.matches("/first-party/proxy?tsurl=").count() >= 2,
            "{}",
            out
        );
        // Relative preserved
        assert!(out.contains("/local/c.png 1x"));
        // Normalized spacing: single space before descriptor is acceptable
        assert!(out.contains(" 1x"));
        assert!(out.contains(" 2x"));
    }

    #[test]
    fn rewrites_script_src_and_leaves_relative() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"
          <script src="https://cdn.example/lib.js"></script>
          <script src="/local/app.js"></script>
        "#;
        let out = rewrite_creative_html(html, &settings);
        assert!(out.contains("/first-party/proxy?tsurl="));
        assert!(out.contains("<script src=\"/local/app.js\""));
    }

    #[test]
    fn rewrites_stylesheet_and_preload_links() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"
          <link rel="stylesheet" href="https://cdn.example/site.css">
          <link rel="preload" as="script" href="//cdn.example/app.js">
          <link rel="prefetch" href="https://cdn.example/next.css">
        "#;
        let out = rewrite_creative_html(html, &settings);
        let cnt = out.matches("/first-party/proxy?tsurl=").count();
        assert!(cnt >= 3, "expected 3 rewritten links: {}", out);
    }

    #[test]
    fn rewrites_media_sources_video_audio_source() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"
          <video src="https://cdn.example/v.mp4"></video>
          <audio src="//cdn.example/a.mp3"></audio>
          <video><source src="https://cdn.example/trailer.mp4"></video>
        "#;
        let out = rewrite_creative_html(html, &settings);
        assert!(out.matches("/first-party/proxy?tsurl=").count() >= 3);
    }

    #[test]
    fn rewrites_imagesrcset_absolute_candidates_and_preserves_descriptors() {
        let settings = crate::test_support::tests::create_test_settings();
        // Use a valid quoted attribute; the previous string had malformed escapes
        let html = r#"<div imagesrcset="https://cdn.example/img-1x.png 1x, //cdn.example/img-2x.png 2x, /local/img.png 1x"></div>"#;
        let out = rewrite_creative_html(html, &settings);
        let cnt = out.matches("/first-party/proxy?tsurl=").count();
        assert!(
            cnt >= 1,
            "expected at least one rewritten imagesrcset candidate: {}",
            out
        );
        assert!(out.contains("/local/img.png 1x"));
    }

    #[test]
    fn rewrites_imagesrcset_no_space_commas() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<div imagesrcset="https://cdn.example/a.png 1x,//cdn.example/b.png 2x,/local/c.png 1x"></div>"#;
        let out = rewrite_creative_html(html, &settings);
        // At least two rewritten
        assert!(
            out.matches("/first-party/proxy?tsurl=").count() >= 2,
            "{}",
            out
        );
        // Relative preserved
        assert!(out.contains("/local/c.png 1x"));
    }

    #[test]
    fn rewrites_imagesrcset_relative_no_space_middle() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<div imagesrcset="https://cdn.example/a.png 1x,local/b.png 2x,//cdn.example/c.png 3x"></div>"#;
        let out = rewrite_creative_html(html, &settings);
        // Two absolute/protocol-relative rewritten
        assert!(
            out.matches("/first-party/proxy?tsurl=").count() >= 2,
            "{}",
            out
        );
        // Relative preserved
        assert!(out.contains("local/b.png 2x"));
    }

    #[test]
    fn rewrites_imagesrcset_with_extra_spaces() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<div imagesrcset="  https://cdn.example/a.png    1x  ,  //cdn.example/b.png   2x ,   /local/c.png   1x  "></div>"#;
        let out = rewrite_creative_html(html, &settings);
        // Two absolute/protocol-relative rewritten
        assert!(
            out.matches("/first-party/proxy?tsurl=").count() >= 2,
            "{}",
            out
        );
        // Relative preserved
        assert!(out.contains("/local/c.png 1x"));
        // Normalized spacing present
        assert!(out.contains(" 1x"));
        assert!(out.contains(" 2x"));
    }

    #[test]
    fn rewrites_object_and_embed_and_input_image() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"
          <object data="https://cdn.example/a.swf"></object>
          <embed src="//cdn.example/b.swf"></embed>
          <input type="image" src="https://cdn.example/btn.png">
        "#;
        let out = rewrite_creative_html(html, &settings);
        assert!(out.matches("/first-party/proxy?tsurl=").count() >= 3);
    }

    #[test]
    fn rewrites_svg_href_variants() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"
          <svg>
            <image href="https://cdn.example/pic.svg" />
            <image xlink:href="//cdn.example/pic2.svg" />
            <use href="https://cdn.example/sprite.svg#icon" />
            <use xlink:href="//cdn.example/sprite2.svg#icon" />
          </svg>
        "#;
        let out = rewrite_creative_html(html, &settings);
        assert!(
            out.matches("/first-party/proxy?tsurl=").count() >= 4,
            "svg hrefs not rewritten: {}",
            out
        );
    }

    #[test]
    fn rewrites_inline_style_url_variants() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"
          <div style="background-image:url(https://cdn.example/bg.png);"></div>
          <div style="background:url('//cdn.example/bg2.jpg') no-repeat"></div>
          <div style='mask-image: url( //cdn.example/mask.svg )'></div>
        "#;
        let out = rewrite_creative_html(html, &settings);
        assert!(
            out.matches("/first-party/proxy?tsurl=").count() >= 3,
            "style url() not rewritten: {}",
            out
        );
        assert!(!out.contains("https://cdn.example/bg.png"));
    }

    #[test]
    fn rewrites_style_block_url_variants() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"
          <style>
            .a{background:url(https://cdn.example/s1.png)}
            .b{background-image:url('//cdn.example/s2.jpg')}
          </style>
        "#;
        let out = rewrite_creative_html(html, &settings);
        assert!(
            out.matches("/first-party/proxy?tsurl=").count() >= 2,
            "style block url() not rewritten: {}",
            out
        );
    }

    #[test]
    fn rewrite_srcset_w_and_x_descriptors() {
        let settings = crate::test_support::tests::create_test_settings();
        let srcset = "https://cdn.example/a.png 100w, //cdn.example/b.png 2x, /local/c.png 1x";
        let out = rewrite_srcset(srcset, &settings);
        assert!(out.contains(" 100w"));
        assert!(out.contains(" 2x"));
        assert!(out.contains("/local/c.png 1x"));
        assert!(
            out.matches("/first-party/proxy?tsurl=").count() >= 2,
            "{}",
            out
        );
    }

    #[test]
    fn rewrite_srcset_ignores_non_network_schemes() {
        let settings = crate::test_support::tests::create_test_settings();
        let srcset = "data:image/png;base64,AAAA 1x, https://cdn.example/a.png 2x";
        let out = rewrite_srcset(srcset, &settings);
        assert!(out.contains("data:image/png;base64,AAAA 1x"), "{}", out);
        assert!(out.contains("/first-party/proxy?tsurl="), "{}", out);
    }

    #[test]
    fn split_srcset_handles_no_space_after_commas() {
        let s = "https://cdn.example/a.png 1x,//cdn.example/b.png 2x,/local/c.png 1x";
        let items = super::split_srcset_candidates(s);
        assert_eq!(items.len(), 3, "{:?}", items);
        assert!(items[0].contains("a.png 1x"));
        assert!(items[1].contains("b.png 2x"));
        assert!(items[2].contains("/local/c.png 1x"));
    }

    #[test]
    fn split_srcset_preserves_data_url_comma() {
        let s = "data:image/png;base64,AAAA 1x,//cdn.example/b.png 2x";
        let items = super::split_srcset_candidates(s);
        assert_eq!(items.len(), 2, "{:?}", items);
        assert_eq!(items[0].trim(), "data:image/png;base64,AAAA 1x");
        assert!(items[1].trim().starts_with("//cdn.example/b.png 2x"));
    }

    #[test]
    fn link_rel_case_and_multi_values_rewritten() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"
          <link REL='StyleSheet preload' href='https://cdn.example/s.css' imagesrcset='https://cdn.example/a.png 1x, /local.png 1x'>
        "#;
        let out = rewrite_creative_html(html, &settings);
        // href + one imagesrcset candidate should be rewritten
        assert!(
            out.matches("/first-party/proxy?tsurl=").count() >= 2,
            "{}",
            out
        );
        assert!(out.contains("/local.png 1x"));
    }

    #[test]
    fn style_multiple_urls_and_relative_variants() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"
          <div style="background-image:url(https://cdn.example/a.png); mask: url(../rel.svg) center no-repeat; border-image:url('//cdn.example/b.png') 30 fill"></div>
        "#;
        let out = rewrite_creative_html(html, &settings);
        assert!(
            out.matches("/first-party/proxy?tsurl=").count() >= 2,
            "{}",
            out
        );
        assert!(out.contains("url(../rel.svg)"));
    }

    #[test]
    fn dont_proxy_non_network_schemes() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"
          <img src="data:image/png;base64,AAAA">
          <iframe src="about:blank"></iframe>
          <script src="javascript:alert(1)"></script>
        "#;
        let out = rewrite_creative_html(html, &settings);
        assert!(!out.contains("/first-party/proxy?tsurl="));
        assert!(out.contains("data:image/png;base64,AAAA"));
        assert!(out.contains("<iframe src=\"about:blank\""));
        assert!(out.contains("<script src=\"javascript:alert(1)\""));
    }

    #[test]
    fn rewrite_css_body_direct_smoke() {
        let settings = crate::test_support::tests::create_test_settings();
        let css = ".x{background:url(https://cdn.example/a.png)} .y{mask:url('//cdn.example/b.svg')} .z{background:url(/local.png)}";
        let out = super::rewrite_css_body(css, &settings);
        assert!(
            out.matches("/first-party/proxy?tsurl=").count() >= 2,
            "{}",
            out
        );
        assert!(out.contains("url(/local.png)"));
    }

    #[test]
    fn rewrites_anchor_click_to_first_party() {
        let settings = crate::test_support::tests::create_test_settings();
        let html =
            r#"<a href="https://ads.example.com/click?c=123">Buy</a> <a href="/local">Local</a>"#;
        let out = rewrite_creative_html(html, &settings);
        assert!(out.contains("/first-party/click?tsurl="), "{}", out);
        assert!(out.contains("tstoken="), "{}", out);
        assert!(out.contains("<a href=\"/local\""));
        // Ensure we expose data-tsclick for client guard
        assert!(out.contains("data-tsclick"), "{}", out);
    }

    #[test]
    fn to_abs_additional_cases() {
        let settings = crate::test_support::tests::create_test_settings();
        assert_eq!(
            to_abs("   https://cdn.example/a   ", &settings),
            Some("https://cdn.example/a".to_string())
        );
        assert_eq!(to_abs("blob:xyz", &settings), None);
        assert_eq!(to_abs("tel:+123", &settings), None);
        assert_eq!(to_abs("about:blank", &settings), None);
    }

    #[test]
    fn rewrites_lazy_img_data_src_and_data_srcset() {
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"
          <img data-src="https://cdn.example/lazy.png">
          <img data-srcset="https://cdn.example/img-1x.png 1x, //cdn.example/img-2x.png 2x, /local/img.png 1x">
        "#;
        let out = rewrite_creative_html(html, &settings);
        assert!(out.contains("data-src=\"/first-party/proxy?tsurl="));
        assert!(out.matches("/first-party/proxy?tsurl=").count() >= 1);
        // relative candidate remains
        assert!(out.contains("/local/img.png 1x"));
    }

    #[test]
    fn to_abs_respects_exclude_domains() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.rewrite.exclude_domains = vec!["trusted-cdn.example.com".to_string()];

        // Excluded domain should return None (not proxied)
        assert_eq!(
            to_abs("https://trusted-cdn.example.com/lib.js", &settings),
            None
        );

        // Non-excluded domain should return Some
        assert_eq!(
            to_abs("https://other-cdn.example.com/lib.js", &settings),
            Some("https://other-cdn.example.com/lib.js".to_string())
        );
    }

    #[test]
    fn to_abs_respects_wildcard_domains() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.rewrite.exclude_domains = vec!["*.cloudflare.com".to_string()];

        // Should exclude base domain
        assert_eq!(to_abs("https://cloudflare.com/cdn.js", &settings), None);

        // Should exclude subdomain
        assert_eq!(
            to_abs("https://cdnjs.cloudflare.com/lib.js", &settings),
            None
        );

        // Should not exclude different domain
        assert_eq!(
            to_abs("https://notcloudflare.com/lib.js", &settings),
            Some("https://notcloudflare.com/lib.js".to_string())
        );
    }

    #[test]
    fn rewrite_html_excludes_blacklisted_domains() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.rewrite.exclude_domains = vec!["trusted-cdn.example.com".to_string()];

        let html = r#"
            <img src="https://trusted-cdn.example.com/logo.png">
            <img src="https://other-cdn.example.com/banner.jpg">
        "#;

        let out = rewrite_creative_html(html, &settings);

        // Excluded domain should NOT be rewritten
        assert!(out.contains(r#"src="https://trusted-cdn.example.com/logo.png"#));

        // Non-excluded domain SHOULD be rewritten
        assert!(out.contains("/first-party/proxy?tsurl="));
        assert!(out.contains("other-cdn.example.com"));
    }

    #[test]
    fn rewrite_srcset_excludes_blacklisted_domains() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.rewrite.exclude_domains = vec!["trusted.example.com".to_string()];

        let html = r#"
            <img srcset="https://trusted.example.com/img-1x.png 1x, https://cdn.example.com/img-2x.png 2x">
        "#;

        let out = rewrite_creative_html(html, &settings);

        // Excluded domain should remain as-is
        assert!(out.contains("https://trusted.example.com/img-1x.png 1x"));

        // Non-excluded should be proxied
        assert!(out.contains("/first-party/proxy?tsurl="));
        assert!(out.contains("cdn.example.com"));
    }

    #[test]
    fn rewrite_style_urls_excludes_blacklisted_domains() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.rewrite.exclude_domains = vec!["fonts.googleapis.com".to_string()];

        let html = r#"
            <style>
                @font-face {
                    font-family: 'Test';
                    src: url(https://fonts.googleapis.com/font.woff2);
                }
                body {
                    background: url(https://cdn.example.com/bg.png);
                }
            </style>
        "#;

        let out = rewrite_creative_html(html, &settings);

        // Excluded domain should remain unchanged
        assert!(out.contains("url(https://fonts.googleapis.com/font.woff2)"));

        // Non-excluded should be proxied
        assert!(out.contains("/first-party/proxy?tsurl="));
        assert!(out.contains("cdn.example.com"));
    }

    #[test]
    fn rewrite_click_urls_excludes_blacklisted_domains() {
        let mut settings = crate::test_support::tests::create_test_settings();
        settings.rewrite.exclude_domains = vec!["trusted-landing.example.com".to_string()];

        let html = r#"
            <a href="https://trusted-landing.example.com/page">Trusted Link</a>
            <a href="https://advertiser.example.com/landing">Ad Link</a>
        "#;

        let out = rewrite_creative_html(html, &settings);

        // Excluded domain should NOT be rewritten to first-party click
        assert!(out.contains(r#"href="https://trusted-landing.example.com/page"#));
        // The excluded link should NOT have data-tsclick since it wasn't rewritten
        assert!(
            !out.contains(r#"<a href="https://trusted-landing.example.com/page" data-tsclick="#)
        );

        // Non-excluded should be rewritten and SHOULD have data-tsclick
        assert!(out.contains("/first-party/click?tsurl="));
        assert!(out.contains("advertiser.example.com"));
        assert!(out.contains("data-tsclick=\"/first-party/click"));
    }
}
