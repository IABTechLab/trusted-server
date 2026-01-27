use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;

use lol_html::{text, Settings as RewriterSettings};

use crate::integrations::{IntegrationHtmlContext, IntegrationHtmlPostProcessor};

use super::rsc::rewrite_rsc_scripts_combined_with_limit;
use super::rsc_placeholders::{
    NextJsRscPostProcessState, RSC_PAYLOAD_PLACEHOLDER_PREFIX, RSC_PAYLOAD_PLACEHOLDER_SUFFIX,
};
use super::shared::find_rsc_push_payload_range;
use super::{NextJsIntegrationConfig, NEXTJS_INTEGRATION_ID};

pub(crate) struct NextJsHtmlPostProcessor {
    config: Arc<NextJsIntegrationConfig>,
}

impl NextJsHtmlPostProcessor {
    pub(crate) fn new(config: Arc<NextJsIntegrationConfig>) -> Self {
        Self { config }
    }
}

impl IntegrationHtmlPostProcessor for NextJsHtmlPostProcessor {
    fn integration_id(&self) -> &'static str {
        NEXTJS_INTEGRATION_ID
    }

    fn should_process(&self, html: &str, ctx: &IntegrationHtmlContext<'_>) -> bool {
        if !self.config.enabled || self.config.rewrite_attributes.is_empty() {
            return false;
        }

        // Check if we have captured placeholders from streaming
        if let Some(state) = ctx
            .document_state
            .get::<Mutex<NextJsRscPostProcessState>>(NEXTJS_INTEGRATION_ID)
        {
            let guard = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !guard.payloads.is_empty() {
                return true;
            }
        }

        // Also check if HTML contains RSC scripts that weren't captured during streaming
        // (e.g., fragmented scripts that we skipped during the streaming pass)
        html.contains("__next_f.push") && html.contains(ctx.origin_host)
    }

    fn post_process(&self, html: &mut String, ctx: &IntegrationHtmlContext<'_>) -> bool {
        // Try to get payloads captured during streaming (placeholder approach)
        let payloads = ctx
            .document_state
            .get::<Mutex<NextJsRscPostProcessState>>(NEXTJS_INTEGRATION_ID)
            .map(|state| {
                let mut guard = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard.take_payloads()
            })
            .unwrap_or_default();

        if !payloads.is_empty() {
            // Placeholder approach: substitute placeholders with rewritten payloads
            return self.substitute_placeholders(html, ctx, payloads);
        }

        // Fallback: re-parse HTML to find RSC scripts that weren't captured during streaming
        // (e.g., fragmented scripts that we skipped during the streaming pass)
        post_process_rsc_html_in_place_with_limit(
            html,
            ctx.origin_host,
            ctx.request_host,
            ctx.request_scheme,
            self.config.max_combined_payload_bytes,
        )
    }
}

impl NextJsHtmlPostProcessor {
    /// Substitute placeholders with rewritten payloads (fast path for unfragmented scripts).
    fn substitute_placeholders(
        &self,
        html: &mut String,
        ctx: &IntegrationHtmlContext<'_>,
        payloads: Vec<String>,
    ) -> bool {
        let payload_refs: Vec<&str> = payloads.iter().map(String::as_str).collect();
        let mut rewritten_payloads = rewrite_rsc_scripts_combined_with_limit(
            payload_refs.as_slice(),
            ctx.origin_host,
            ctx.request_host,
            ctx.request_scheme,
            self.config.max_combined_payload_bytes,
        );

        if rewritten_payloads.len() != payloads.len() {
            log::warn!(
                "NextJs post-process skipping due to rewrite payload count mismatch: original={}, rewritten={}",
                payloads.len(),
                rewritten_payloads.len()
            );
            rewritten_payloads = payloads;
        }

        if log::log_enabled!(log::Level::Debug) {
            let origin_count_before: usize = rewritten_payloads
                .iter()
                .map(|p| p.matches(ctx.origin_host).count())
                .sum();
            log::debug!(
                "NextJs post-processor substituting RSC payloads: scripts={}, origin_urls={}, origin={}, proxy={}://{}, html_len={}",
                rewritten_payloads.len(),
                origin_count_before,
                ctx.origin_host,
                ctx.request_scheme,
                ctx.request_host,
                html.len()
            );
        }

        let (updated, replaced) =
            substitute_rsc_payload_placeholders(html.as_str(), &rewritten_payloads);

        let expected = rewritten_payloads.len();
        if replaced != expected {
            log::warn!(
                "NextJs post-process placeholder substitution count mismatch: expected={}, replaced={}",
                expected,
                replaced
            );
        }

        if contains_rsc_payload_placeholders(&updated) {
            log::error!(
                "NextJs post-process left RSC placeholders in output; attempting fallback substitution (scripts={})",
                expected
            );

            let fallback =
                substitute_rsc_payload_placeholders_exact(html.as_str(), &rewritten_payloads);

            if contains_rsc_payload_placeholders(&fallback) {
                log::error!(
                    "NextJs post-process fallback substitution still left RSC placeholders in output; hydration may break (scripts={})",
                    expected
                );
            }

            *html = fallback;
            return true;
        }

        *html = updated;
        true
    }
}

fn contains_rsc_payload_placeholders(html: &str) -> bool {
    let mut cursor = 0usize;
    while let Some(next) = html[cursor..].find(RSC_PAYLOAD_PLACEHOLDER_PREFIX) {
        let start = cursor + next;
        let after_prefix = start + RSC_PAYLOAD_PLACEHOLDER_PREFIX.len();
        let mut idx_end = after_prefix;
        while idx_end < html.len() && html.as_bytes()[idx_end].is_ascii_digit() {
            idx_end += 1;
        }
        if idx_end > after_prefix && html[idx_end..].starts_with(RSC_PAYLOAD_PLACEHOLDER_SUFFIX) {
            return true;
        }
        cursor = after_prefix;
    }
    false
}

fn substitute_rsc_payload_placeholders(html: &str, replacements: &[String]) -> (String, usize) {
    let mut output = String::with_capacity(html.len());
    let mut cursor = 0usize;
    let mut replaced = 0usize;

    while let Some(next) = html[cursor..].find(RSC_PAYLOAD_PLACEHOLDER_PREFIX) {
        let start = cursor + next;
        output.push_str(&html[cursor..start]);

        let after_prefix = start + RSC_PAYLOAD_PLACEHOLDER_PREFIX.len();
        let mut idx_end = after_prefix;
        while idx_end < html.len() && html.as_bytes()[idx_end].is_ascii_digit() {
            idx_end += 1;
        }

        let suffix_ok =
            idx_end > after_prefix && html[idx_end..].starts_with(RSC_PAYLOAD_PLACEHOLDER_SUFFIX);
        if !suffix_ok {
            output.push_str(RSC_PAYLOAD_PLACEHOLDER_PREFIX);
            cursor = after_prefix;
            continue;
        }

        let idx_str = &html[after_prefix..idx_end];
        let Ok(index) = idx_str.parse::<usize>() else {
            output.push_str(RSC_PAYLOAD_PLACEHOLDER_PREFIX);
            output.push_str(idx_str);
            output.push_str(RSC_PAYLOAD_PLACEHOLDER_SUFFIX);
            cursor = idx_end + RSC_PAYLOAD_PLACEHOLDER_SUFFIX.len();
            continue;
        };

        let Some(replacement) = replacements.get(index) else {
            output.push_str(RSC_PAYLOAD_PLACEHOLDER_PREFIX);
            output.push_str(idx_str);
            output.push_str(RSC_PAYLOAD_PLACEHOLDER_SUFFIX);
            cursor = idx_end + RSC_PAYLOAD_PLACEHOLDER_SUFFIX.len();
            continue;
        };

        output.push_str(replacement);
        replaced += 1;
        cursor = idx_end + RSC_PAYLOAD_PLACEHOLDER_SUFFIX.len();
    }

    output.push_str(&html[cursor..]);
    (output, replaced)
}

fn substitute_rsc_payload_placeholders_exact(html: &str, replacements: &[String]) -> String {
    let mut out = html.to_string();
    for (index, replacement) in replacements.iter().enumerate() {
        let placeholder =
            format!("{RSC_PAYLOAD_PLACEHOLDER_PREFIX}{index}{RSC_PAYLOAD_PLACEHOLDER_SUFFIX}");
        out = out.replace(&placeholder, replacement);
    }
    out
}

#[derive(Debug, Clone, Copy)]
struct RscPushScriptRange {
    payload_start: usize,
    payload_end: usize,
}

fn find_rsc_push_scripts(html: &str) -> Vec<RscPushScriptRange> {
    if !html.contains("__next_f") {
        return Vec::new();
    }

    let ranges: Rc<RefCell<Vec<RscPushScriptRange>>> = Rc::new(RefCell::new(Vec::new()));
    let buffer: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    let buffering = Rc::new(Cell::new(false));
    let buffer_start = Rc::new(Cell::new(0usize));

    let settings = RewriterSettings {
        element_content_handlers: vec![text!("script", {
            let ranges = Rc::clone(&ranges);
            let buffer = Rc::clone(&buffer);
            let buffering = Rc::clone(&buffering);
            let buffer_start = Rc::clone(&buffer_start);
            move |t| {
                if !buffering.get() && t.last_in_text_node() {
                    let script = t.as_str();
                    if !script.contains("__next_f") {
                        return Ok(());
                    }

                    let Some((payload_start_rel, payload_end_rel)) =
                        find_rsc_push_payload_range(script)
                    else {
                        return Ok(());
                    };

                    let loc = t.source_location().bytes();
                    ranges.borrow_mut().push(RscPushScriptRange {
                        payload_start: loc.start + payload_start_rel,
                        payload_end: loc.start + payload_end_rel,
                    });
                    return Ok(());
                }

                if !buffering.get() {
                    buffering.set(true);
                    buffer_start.set(t.source_location().bytes().start);
                }
                buffer.borrow_mut().push_str(t.as_str());

                if !t.last_in_text_node() {
                    return Ok(());
                }

                buffering.set(false);
                let script = std::mem::take(&mut *buffer.borrow_mut());
                if !script.contains("__next_f") {
                    return Ok(());
                }

                let Some((payload_start_rel, payload_end_rel)) =
                    find_rsc_push_payload_range(&script)
                else {
                    return Ok(());
                };

                let base = buffer_start.get();
                ranges.borrow_mut().push(RscPushScriptRange {
                    payload_start: base + payload_start_rel,
                    payload_end: base + payload_end_rel,
                });

                Ok(())
            }
        })],
        ..RewriterSettings::default()
    };

    let mut rewriter = lol_html::HtmlRewriter::new(settings, |_chunk: &[u8]| {});
    if rewriter.write(html.as_bytes()).is_err() || rewriter.end().is_err() {
        return Vec::new();
    }

    let result = std::mem::take(&mut *ranges.borrow_mut());
    result
}

/// Rewrite RSC payload URLs in HTML by re-parsing the document.
///
/// # Deprecation
///
/// This function is **deprecated** in favor of the placeholder-based approach used in production:
/// - `NextJsRscPlaceholderRewriter` captures payloads during the initial `lol_html` pass
/// - `NextJsHtmlPostProcessor` rewrites and substitutes them at end-of-document
///
/// This function re-parses HTML with `lol_html`, which is slower than the placeholder approach.
/// It remains available for testing and backward compatibility.
#[deprecated(
    since = "0.1.0",
    note = "Use NextJsHtmlPostProcessor for production RSC rewriting. This function re-parses HTML."
)]
#[must_use]
pub fn post_process_rsc_html(
    html: &str,
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
) -> String {
    let mut result = html.to_string();
    #[allow(deprecated)]
    post_process_rsc_html_in_place(&mut result, origin_host, request_host, request_scheme);
    result
}

/// Rewrite RSC payload URLs in HTML in place by re-parsing the document.
///
/// # Deprecation
///
/// This function is **deprecated** in favor of the placeholder-based approach used in production.
/// See [`post_process_rsc_html`] for details.
#[deprecated(
    since = "0.1.0",
    note = "Use NextJsHtmlPostProcessor for production RSC rewriting. This function re-parses HTML."
)]
pub fn post_process_rsc_html_in_place(
    html: &mut String,
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
) -> bool {
    post_process_rsc_html_in_place_with_limit(
        html,
        origin_host,
        request_host,
        request_scheme,
        super::rsc::DEFAULT_MAX_COMBINED_PAYLOAD_BYTES,
    )
}

fn post_process_rsc_html_in_place_with_limit(
    html: &mut String,
    origin_host: &str,
    request_host: &str,
    request_scheme: &str,
    max_combined_payload_bytes: usize,
) -> bool {
    let mut scripts = find_rsc_push_scripts(html.as_str());
    if scripts.is_empty() {
        return false;
    }

    scripts.sort_by_key(|s| s.payload_start);
    let mut previous_end = 0usize;
    for script in &scripts {
        if script.payload_start > script.payload_end {
            log::warn!(
                "NextJs post-process skipping due to invalid payload range: start={}, end={}",
                script.payload_start,
                script.payload_end
            );
            return false;
        }
        if script.payload_end > html.len()
            || !html.is_char_boundary(script.payload_start)
            || !html.is_char_boundary(script.payload_end)
        {
            log::warn!(
                "NextJs post-process skipping due to non-UTF8 boundary payload range: start={}, end={}, html_len={}",
                script.payload_start,
                script.payload_end,
                html.len()
            );
            return false;
        }
        if script.payload_start < previous_end {
            log::warn!(
                "NextJs post-process skipping due to overlapping payload ranges: prev_end={}, start={}, end={}",
                previous_end,
                script.payload_start,
                script.payload_end
            );
            return false;
        }
        previous_end = script.payload_end;
    }

    let rewritten_payloads = {
        let Some(payloads) = scripts
            .iter()
            .map(|s| html.get(s.payload_start..s.payload_end))
            .collect::<Option<Vec<_>>>()
        else {
            log::warn!(
                "NextJs post-process skipping due to invalid UTF-8 payload slicing despite boundary checks"
            );
            return false;
        };

        if !payloads.iter().any(|p| p.contains(origin_host)) {
            return false;
        }

        if log::log_enabled!(log::Level::Debug) {
            let origin_count_before: usize = payloads
                .iter()
                .map(|p| p.matches(origin_host).count())
                .sum();
            log::debug!(
                "post_process_rsc_html: {} scripts, {} origin URLs, origin={}, proxy={}://{}",
                payloads.len(),
                origin_count_before,
                origin_host,
                request_scheme,
                request_host
            );
        }

        let rewritten_payloads = rewrite_rsc_scripts_combined_with_limit(
            payloads.as_slice(),
            origin_host,
            request_host,
            request_scheme,
            max_combined_payload_bytes,
        );

        if rewritten_payloads.len() != payloads.len() {
            log::warn!(
                "NextJs post-process skipping due to rewrite payload count mismatch: original={}, rewritten={}",
                payloads.len(),
                rewritten_payloads.len()
            );
            return false;
        }

        let changed = payloads
            .iter()
            .zip(&rewritten_payloads)
            .any(|(original, rewritten)| *original != rewritten);
        if !changed {
            return false;
        }

        rewritten_payloads
    };

    for (i, script) in scripts.iter().enumerate().rev() {
        html.replace_range(
            script.payload_start..script.payload_end,
            &rewritten_payloads[i],
        );
    }

    true
}

#[cfg(test)]
#[allow(
    deprecated, // Tests use deprecated post_process_rsc_html for legacy API coverage
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::panic,
    clippy::unwrap_used
)]
mod tests {
    use super::*;

    fn find_rsc_push_scripts_chunked(
        html: &str,
        chunk_size: usize,
    ) -> (Vec<RscPushScriptRange>, bool) {
        if !html.contains("__next_f") {
            return (Vec::new(), false);
        }

        let ranges: Rc<RefCell<Vec<RscPushScriptRange>>> = Rc::new(RefCell::new(Vec::new()));
        let buffer: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let buffering = Rc::new(Cell::new(false));
        let buffer_start = Rc::new(Cell::new(0usize));
        let saw_partial = Rc::new(Cell::new(false));

        let settings = RewriterSettings {
            element_content_handlers: vec![text!("script", {
                let ranges = Rc::clone(&ranges);
                let buffer = Rc::clone(&buffer);
                let buffering = Rc::clone(&buffering);
                let buffer_start = Rc::clone(&buffer_start);
                let saw_partial = Rc::clone(&saw_partial);
                move |t| {
                    if !t.last_in_text_node() {
                        saw_partial.set(true);
                    }

                    if !buffering.get() && t.last_in_text_node() {
                        let script = t.as_str();
                        if !script.contains("__next_f") {
                            return Ok(());
                        }

                        let Some((payload_start_rel, payload_end_rel)) =
                            find_rsc_push_payload_range(script)
                        else {
                            return Ok(());
                        };

                        let loc = t.source_location().bytes();
                        ranges.borrow_mut().push(RscPushScriptRange {
                            payload_start: loc.start + payload_start_rel,
                            payload_end: loc.start + payload_end_rel,
                        });
                        return Ok(());
                    }

                    if !buffering.get() {
                        buffering.set(true);
                        buffer_start.set(t.source_location().bytes().start);
                    }
                    buffer.borrow_mut().push_str(t.as_str());

                    if !t.last_in_text_node() {
                        return Ok(());
                    }

                    buffering.set(false);
                    let script = std::mem::take(&mut *buffer.borrow_mut());
                    if !script.contains("__next_f") {
                        return Ok(());
                    }

                    let Some((payload_start_rel, payload_end_rel)) =
                        find_rsc_push_payload_range(&script)
                    else {
                        return Ok(());
                    };

                    let base = buffer_start.get();
                    ranges.borrow_mut().push(RscPushScriptRange {
                        payload_start: base + payload_start_rel,
                        payload_end: base + payload_end_rel,
                    });

                    Ok(())
                }
            })],
            ..RewriterSettings::default()
        };

        let mut rewriter = lol_html::HtmlRewriter::new(settings, |_chunk: &[u8]| {});
        let chunk_size = chunk_size.max(1);
        for chunk in html.as_bytes().chunks(chunk_size) {
            if rewriter.write(chunk).is_err() {
                return (Vec::new(), saw_partial.get());
            }
        }
        if rewriter.end().is_err() {
            return (Vec::new(), saw_partial.get());
        }

        let result = std::mem::take(&mut *ranges.borrow_mut());
        (result, saw_partial.get())
    }

    #[test]
    fn post_process_rsc_html_rewrites_cross_script_tchunks() {
        let html = r#"<html><body>
<script>self.__next_f.push([1,"other:data\n1a:T3e,partial content"])</script>
<script>self.__next_f.push([1," with https://origin.example.com/page goes here"])</script>
</body></html>"#;

        let result = post_process_rsc_html(html, "origin.example.com", "test.example.com", "https");

        assert!(
            result.contains("test.example.com/page"),
            "URL should be rewritten. Got: {}",
            result
        );
        assert!(
            result.contains(":T3c,"),
            "T-chunk length should be updated. Got: {}",
            result
        );
        assert!(result.contains("<html>") && result.contains("</html>"));
        assert!(result.contains("self.__next_f.push"));
    }

    #[test]
    fn finds_rsc_push_scripts_with_fragmented_script_text_chunks() {
        let filler = "a".repeat(32 * 1024);
        let payload = format!("{filler} https://origin.example.com/page");
        let html = format!(
            r#"<html><body><script>self.__next_f.push([1,"{payload}"])</script></body></html>"#
        );

        let (scripts, saw_partial) = find_rsc_push_scripts_chunked(&html, 64);

        assert!(
            saw_partial,
            "should observe fragmented script text chunks when writing input in small pieces"
        );
        assert_eq!(
            scripts.len(),
            1,
            "Should find exactly one RSC payload script"
        );

        let extracted = &html[scripts[0].payload_start..scripts[0].payload_end];
        assert_eq!(
            extracted.len(),
            payload.len(),
            "Extracted payload length should match the original payload"
        );
        assert!(
            extracted.ends_with("https://origin.example.com/page"),
            "Extracted payload should contain the origin URL"
        );
    }

    #[test]
    fn finds_assignment_push_form() {
        let html = r#"<html><body><script>(self.__next_f=self.__next_f||[]).push([1,"payload"])</script></body></html>"#;
        let scripts = find_rsc_push_scripts(html);
        assert_eq!(
            scripts.len(),
            1,
            "Should find exactly one RSC payload script"
        );
        let payload = &html[scripts[0].payload_start..scripts[0].payload_end];
        assert_eq!(payload, "payload", "Should capture the payload string");
    }

    #[test]
    fn finds_window_next_f_push_with_case_insensitive_script_tags() {
        let html = r#"<SCRIPT>window.__next_f.push([1,'payload']);</SCRIPT>"#;
        let scripts = find_rsc_push_scripts(html);
        assert_eq!(
            scripts.len(),
            1,
            "Should find exactly one RSC payload script"
        );
        let payload = &html[scripts[0].payload_start..scripts[0].payload_end];
        assert_eq!(payload, "payload", "Should capture the payload string");
    }

    #[test]
    fn post_process_rsc_html_handles_prettified_format() {
        let html = r#"<html><body>
    <script>
      self.__next_f.push([
        1,
        '445:{"ID":878799,"title":"News","url":"http://origin.example.com/news","target":""}'
      ]);
    </script>
    <script>
      self.__next_f.push([
        1,
        '446:{"url":"https://origin.example.com/reviews"}'
      ]);
    </script>
</body></html>"#;

        let result = post_process_rsc_html(html, "origin.example.com", "test.example.com", "https");

        assert!(
            result.contains("test.example.com/news"),
            "First URL should be rewritten. Got: {}",
            result
        );
        assert!(
            result.contains("test.example.com/reviews"),
            "Second URL should be rewritten. Got: {}",
            result
        );
        assert!(
            !result.contains("origin.example.com"),
            "No origin URLs should remain. Got: {}",
            result
        );
        assert!(result.contains("<html>") && result.contains("</html>"));
        assert!(result.contains("self.__next_f.push"));
    }

    #[test]
    fn post_process_rewrites_html_href_inside_tchunk() {
        fn calculate_unescaped_byte_length_for_test(s: &str) -> usize {
            let bytes = s.as_bytes();
            let mut pos = 0usize;
            let mut count = 0usize;

            while pos < bytes.len() {
                if bytes[pos] == b'\\' && pos + 1 < bytes.len() {
                    let esc = bytes[pos + 1];

                    if matches!(
                        esc,
                        b'n' | b'r' | b't' | b'b' | b'f' | b'v' | b'"' | b'\'' | b'\\' | b'/'
                    ) {
                        pos += 2;
                        count += 1;
                        continue;
                    }

                    if esc == b'x' && pos + 3 < bytes.len() {
                        pos += 4;
                        count += 1;
                        continue;
                    }

                    if esc == b'u' && pos + 5 < bytes.len() {
                        let hex = &s[pos + 2..pos + 6];
                        if hex.chars().all(|c| c.is_ascii_hexdigit()) {
                            if let Ok(code_unit) = u16::from_str_radix(hex, 16) {
                                // Surrogate pairs use UTF-16 and expand to 4 bytes in UTF-8.
                                if (0xD800..=0xDBFF).contains(&code_unit)
                                    && pos + 11 < bytes.len()
                                    && bytes[pos + 6] == b'\\'
                                    && bytes[pos + 7] == b'u'
                                {
                                    let hex2 = &s[pos + 8..pos + 12];
                                    if hex2.chars().all(|c| c.is_ascii_hexdigit()) {
                                        if let Ok(code_unit2) = u16::from_str_radix(hex2, 16) {
                                            if (0xDC00..=0xDFFF).contains(&code_unit2) {
                                                pos += 12;
                                                count += 4;
                                                continue;
                                            }
                                        }
                                    }
                                }

                                let c = char::from_u32(code_unit as u32).unwrap_or('\u{FFFD}');
                                pos += 6;
                                count += c.len_utf8();
                                continue;
                            }
                        }
                    }
                }

                if bytes[pos] < 0x80 {
                    pos += 1;
                    count += 1;
                } else {
                    let c = s[pos..].chars().next().unwrap_or('\u{FFFD}');
                    pos += c.len_utf8();
                    count += c.len_utf8();
                }
            }

            count
        }

        let tchunk_content = r#"\u003cdiv\u003e\u003ca href="https://origin.example.com/about-us"\u003eAbout\u003c/a\u003e\u003c/div\u003e"#;
        let declared_len_hex = format!(
            "{:x}",
            calculate_unescaped_byte_length_for_test(tchunk_content)
        );
        let html = format!(
            r#"<html><body>
    <script>
      self.__next_f.push([
        1,
        '53d:T{declared_len_hex},{tchunk_content}'
      ]);
    </script>
</body></html>"#
        );

        let result =
            post_process_rsc_html(&html, "origin.example.com", "test.example.com", "https");

        assert!(
            result.contains("test.example.com/about-us"),
            "HTML href URL in T-chunk should be rewritten. Got: {}",
            result
        );
        assert!(
            !result.contains("origin.example.com"),
            "No origin URLs should remain. Got: {}",
            result
        );
        assert!(
            !result.contains(&format!(":T{declared_len_hex},")),
            "T-chunk length should have been recalculated. Got: {}",
            result
        );
    }

    #[test]
    fn handles_nextjs_inlined_data_nonce_fixture() {
        // Fixture mirrors Next.js `createInlinedDataReadableStream` output:
        // `<script nonce="...">self.__next_f.push([1,"..."]) </script>`
        let html = include_str!("fixtures/inlined-data-nonce.html");
        let scripts = find_rsc_push_scripts(html);
        assert_eq!(scripts.len(), 1, "Should find exactly one RSC data script");

        let rewritten =
            post_process_rsc_html(html, "origin.example.com", "proxy.example.com", "https");
        assert!(
            rewritten.contains("https://proxy.example.com/news"),
            "Fixture URL should be rewritten. Got: {rewritten}"
        );
        assert!(
            !rewritten.contains("https://origin.example.com/news"),
            "Origin URL should be removed. Got: {rewritten}"
        );
    }

    #[test]
    fn handles_nextjs_inlined_data_html_escaping_fixture() {
        // Fixture includes `\\u003c` escapes, matching Next.js `htmlEscapeJsonString` behavior.
        let html = include_str!("fixtures/inlined-data-escaped.html");
        let scripts = find_rsc_push_scripts(html);
        assert_eq!(scripts.len(), 1, "Should find exactly one RSC data script");

        let rewritten =
            post_process_rsc_html(html, "origin.example.com", "proxy.example.com", "https");
        assert!(
            rewritten.contains("https://proxy.example.com/about"),
            "Escaped fixture URL should be rewritten. Got: {rewritten}"
        );
        assert!(
            rewritten.contains(r#"\\u003ca href=\\\"https://proxy.example.com/about\\\""#),
            "Escaped HTML should remain escaped and rewritten. Got: {rewritten}"
        );
        assert!(
            !rewritten.contains("https://origin.example.com/about"),
            "Origin URL should be removed. Got: {rewritten}"
        );
    }

    #[test]
    fn handles_trailing_backslash_gracefully() {
        // Malformed content with trailing backslash should not panic
        let html = r#"<html><body>
<script>self.__next_f.push([1,"content with trailing backslash\"])</script>
<script>self.__next_f.push([1,"valid https://origin.example.com/page"])</script>
</body></html>"#;

        let scripts = find_rsc_push_scripts(html);
        // The first script is malformed (trailing backslash escapes the quote),
        // so it won't be detected as valid. The second one should be found.
        assert!(
            !scripts.is_empty(),
            "Should find at least the valid script. Found: {}",
            scripts.len()
        );

        // Should not panic during processing
        let result = post_process_rsc_html(html, "origin.example.com", "test.example.com", "https");
        assert!(
            result.contains("test.example.com") || result.contains("origin.example.com"),
            "Processing should complete without panic"
        );
    }

    #[test]
    fn handles_unterminated_string_gracefully() {
        // Content where string never closes - should not hang or panic
        let html = r#"<html><body>
<script>self.__next_f.push([1,"content without closing quote
</body></html>"#;

        let scripts = find_rsc_push_scripts(html);
        assert_eq!(
            scripts.len(),
            0,
            "Should not find scripts with unterminated strings"
        );
    }

    #[test]
    fn no_origin_returns_unchanged() {
        let html = r#"<html><body>
<script>self.__next_f.push([1,"content without origin URLs"])</script>
</body></html>"#;

        let result = post_process_rsc_html(html, "origin.example.com", "test.example.com", "https");
        assert_eq!(result, html, "HTML without origin should be unchanged");
    }
}
