use crate::http_util::encode_url;
use crate::settings::Settings;
use lol_html::{element, HtmlRewriter, Settings as HtmlSettings};

/// Rewrite ad creative HTML to first-party endpoints.
/// - 1x1 <img> pixels → `/first-party/proxy?u=<base64-url-safe-no-pad>`
/// - Non-pixel absolute images → `/first-party/proxy?u=<base64-url-safe-no-pad>`
/// - <iframe src> (absolute or protocol-relative) → `/first-party/proxy?u=<base64-url-safe-no-pad>`
pub fn rewrite_creative_html(markup: &str, settings: &Settings) -> String {
    // No size parsing needed now; all absolute/protocol-relative URLs are proxied uniformly.

    // No client-side 1x1 pixel detection needed; server logs likely pixels heuristically.

    let mut out = Vec::with_capacity(markup.len() + 64);
    let mut rewriter = HtmlRewriter::new(
        HtmlSettings {
            element_content_handlers: vec![
                element!("img", |el| {
                    if let Some(src) = el.get_attribute("src") {
                        let src_trim = src.trim();
                        let abs = if src_trim.starts_with("//") {
                            format!("https:{}", src_trim)
                        } else {
                            src_trim.to_string()
                        };
                        if abs.starts_with("http://") || abs.starts_with("https://") {
                            let encoded = encode_url(settings, &abs);
                            let proxied = format!("/first-party/proxy?u={}", encoded);
                            let _ = el.set_attribute("src", &proxied);
                        }
                    }
                    Ok(())
                }),
                element!("iframe", |el| {
                    if let Some(src) = el.get_attribute("src") {
                        let src_trim = src.trim();
                        let abs = if src_trim.starts_with("//") {
                            format!("https:{}", src_trim)
                        } else {
                            src_trim.to_string()
                        };
                        if abs.starts_with("http://") || abs.starts_with("https://") {
                            let encoded = encode_url(settings, &abs);
                            let proxied = format!("/first-party/proxy?u={}", encoded);
                            let _ = el.set_attribute("src", &proxied);
                        }
                    }
                    Ok(())
                }),
                element!("[srcset]", |el| {
                    if let Some(srcset) = el.get_attribute("srcset") {
                        let mut out_items: Vec<String> = Vec::new();
                        for item in srcset.split(',') {
                            let it = item.trim();
                            if it.is_empty() {
                                continue;
                            }
                            let mut parts = it.split_whitespace();
                            let url = parts.next().unwrap_or("");
                            let descriptor = parts.collect::<Vec<_>>().join(" ");
                            let abs = if url.starts_with("//") {
                                format!("https:{}", url)
                            } else {
                                url.to_string()
                            };
                            let rewritten =
                                if abs.starts_with("http://") || abs.starts_with("https://") {
                                    let enc = encode_url(settings, &abs);
                                    format!("/first-party/proxy?u={}", enc)
                                } else {
                                    url.to_string()
                                };
                            if descriptor.is_empty() {
                                out_items.push(rewritten);
                            } else {
                                out_items.push(format!("{} {}", rewritten, descriptor));
                            }
                        }
                        let _ = el.set_attribute("srcset", &out_items.join(", "));
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

#[cfg(test)]
mod tests {
    use super::rewrite_creative_html;

    #[test]
    fn rewrites_width_height_attrs() {
        use crate::http_util::encode_url;
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<div><img width="1" height="1" src="https://t.example/p.gif"></div>"#;
        let out = rewrite_creative_html(html, &settings);
        let _expected = encode_url(&settings, "https://t.example/p.gif");
        assert!(out.contains("/first-party/proxy?u="));
    }

    #[test]
    fn rewrites_style_1x1_px() {
        use crate::http_util::encode_url;
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<img style="width:1px; height:1px" src="https://t.example/a.png">"#;
        let out = rewrite_creative_html(html, &settings);
        let _expected = encode_url(&settings, "https://t.example/a.png");
        assert!(out.contains("/first-party/proxy?u="));
    }

    #[test]
    fn rewrites_style_1x1_no_units_and_messy_spacing() {
        use crate::http_util::encode_url;
        let settings = crate::test_support::tests::create_test_settings();
        let html =
            r#"<img style="  HEIGHT : 1 ;   width: 1  ; display:block" src="//cdn.example/p">"#;
        let out = rewrite_creative_html(html, &settings);
        let _expected = encode_url(&settings, "https://cdn.example/p");
        assert!(out.contains("/first-party/proxy?u="));
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
        assert!(out.contains("/first-party/proxy?u="));
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
        assert!(out.contains("/first-party/proxy?u="));

        let html2 = r#"<iframe src="//cdn.example/ad.html"></iframe>"#;
        let out2 = rewrite_creative_html(html2, &settings);
        assert!(out2.contains("/first-party/proxy?u="));

        let html3 = r#"<iframe src="/local/ad.html"></iframe>"#;
        let out3 = rewrite_creative_html(html3, &settings);
        assert!(out3.contains("<iframe src=\"/local/ad.html\""));
        assert!(!out3.contains("/first-party/proxy?u="));
    }

    #[test]
    fn rewrites_srcset_absolute_candidates_and_preserves_descriptors() {
        // Absolute + protocol-relative get rewritten to /first-party/proxy?u=..., relative remains
        let settings = crate::test_support::tests::create_test_settings();
        let html = r#"<img srcset="https://cdn.example/img-1x.png 1x, //cdn.example/img-2x.png 2x, /local/img.png 1x">"#;
        let out = rewrite_creative_html(html, &settings);

        // Should have at least two proxied candidates
        let cnt = out.matches("/first-party/proxy?u=").count();
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
            out.matches("/first-party/proxy?u=").count() >= 2,
            "srcset not fully rewritten: {}",
            out
        );
        // Relative preserved
        assert!(out.contains("/local/img.webp 1x"));
        // Fallback img unchanged (relative)
        assert!(out.contains("<img src=\"/fallback.jpg\""));
    }
}
