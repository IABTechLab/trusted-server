use std::sync::LazyLock;

use regex::Regex;

use super::api::EmbeddingPlan;
use super::boundary::Boundary;
use super::text::{
    assemble_text, decode_html_entities, extract_fragments, is_js_whitespace, structural_close_tag,
};

const MAGIC_PREFIX: &str = "\u{e01df}\u{e01e0}\u{e01e1}\u{e01e2}";

static MANIFEST_META: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r#"(?i)<meta\s+[^>]*name\s*=\s*['"]c2pa-manifest-url['"][^>]*>"#).ok()
});

struct MappedChar {
    ch: char,
    start: usize,
    end: usize,
}

/// Reports whether a string contains the four-character Encypher magic prefix.
#[must_use]
pub fn has_existing_markers(s: &str) -> bool {
    s.contains(MAGIC_PREFIX)
}

/// Reports whether HTML already declares a C2PA manifest URL meta element.
#[must_use]
pub fn has_manifest_meta(html: &str) -> bool {
    MANIFEST_META
        .as_ref()
        .is_some_and(|regex| regex.is_match(html))
}

/// Applies a codepoint embedding plan within an article boundary.
///
/// The plan must already have passed [`super::api::validate_plan`]; markers are
/// inserted verbatim. Returns `None` when the boundary has no visible text, an
/// index has no text position, or text-to-byte alignment fails.
#[must_use]
pub fn apply_plan(html: &str, boundary: &Boundary, plan: &EmbeddingPlan) -> Option<String> {
    let boundary_html = html.get(boundary.start..boundary.end)?;
    let fragments = extract_fragments(boundary_html);
    let visible = assemble_text(boundary_html, &fragments);
    let visible_chars: Vec<char> = visible.chars().collect();
    if visible_chars.is_empty() {
        return None;
    }

    let mut insertion_offsets = vec![None; visible_chars.len()];
    let mut first_visible_start = None;
    let mut previous_visible_end = None;
    let mut visible_cursor = 0_usize;

    for fragment in fragments {
        let raw = boundary_html.get(fragment.start..fragment.end)?;
        let normalized = normalize_mapped(decode_with_offsets(raw));
        if normalized.is_empty() {
            continue;
        }

        while visible_chars.get(visible_cursor) == Some(&' ') {
            if let Some(offset) = previous_visible_end
                && let Some(slot) = insertion_offsets.get_mut(visible_cursor)
            {
                *slot = Some(offset);
            }
            visible_cursor = visible_cursor.saturating_add(1);
        }

        for item in normalized {
            let target = visible_chars.get(visible_cursor)?;
            if !equivalent(item.ch, *target) {
                return None;
            }
            let start = fragment.start.checked_add(item.start)?;
            let end = fragment.start.checked_add(item.end)?;
            if first_visible_start.is_none() {
                first_visible_start = Some(start);
            }
            let slot = insertion_offsets.get_mut(visible_cursor)?;
            *slot = Some(end);
            previous_visible_end = Some(end);
            visible_cursor = visible_cursor.saturating_add(1);
        }
    }

    while visible_chars.get(visible_cursor) == Some(&' ') {
        if let Some(offset) = previous_visible_end {
            let slot = insertion_offsets.get_mut(visible_cursor)?;
            *slot = Some(offset);
        }
        visible_cursor = visible_cursor.saturating_add(1);
    }
    if visible_cursor != visible_chars.len() {
        return None;
    }
    let first_visible_start = first_visible_start?;

    let mut insertions = Vec::with_capacity(plan.operations.len());
    let mut output_len = html.len();
    for operation in &plan.operations {
        let relative = if operation.insert_after_index == -1 {
            first_visible_start
        } else {
            let index = usize::try_from(operation.insert_after_index).ok()?;
            insertion_offsets.get(index).copied().flatten()?
        };
        let absolute = boundary.start.checked_add(relative)?;
        output_len = output_len.checked_add(operation.marker.len())?;
        insertions.push((absolute, operation.marker.as_str()));
    }
    // Stable sorting preserves API order for multiple markers at the same offset.
    insertions.sort_by_key(|(offset, _)| *offset);
    let mut result = String::with_capacity(output_len);
    let mut cursor = 0;
    for (offset, marker) in insertions {
        result.push_str(html.get(cursor..offset)?);
        result.push_str(marker);
        cursor = offset;
    }
    result.push_str(html.get(cursor..)?);
    Some(result)
}

/// Injects an escaped manifest meta element before the structural closing head tag.
///
/// Returns the input unchanged when the URL is empty, a manifest meta already exists,
/// or no structural closing head tag exists.
#[must_use]
pub fn inject_manifest_meta(html: &str, manifest_url: &str) -> String {
    if html.is_empty() || manifest_url.is_empty() || has_manifest_meta(html) {
        return html.to_owned();
    }
    let Some(offset) = structural_close_tag(html, "head") else {
        return html.to_owned();
    };
    let safe_url = manifest_url
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let meta = format!(r#"<meta name="c2pa-manifest-url" content="{safe_url}">"#);
    let Some(before) = html.get(..offset) else {
        return html.to_owned();
    };
    let Some(after) = html.get(offset..) else {
        return html.to_owned();
    };
    let mut output = String::with_capacity(html.len().saturating_add(meta.len()));
    output.push_str(before);
    output.push_str(&meta);
    output.push_str(after);
    output
}

fn decode_with_offsets(raw: &str) -> Vec<MappedChar> {
    let mut mapped = Vec::new();
    let mut cursor = 0_usize;
    while cursor < raw.len() {
        if raw.as_bytes().get(cursor) == Some(&b'&')
            && let Some(relative_semicolon) = raw
                .as_bytes()
                .get(cursor.saturating_add(1)..)
                // Entity names/numbers cannot contain another ampersand or markup.
                // Stop there rather than rescanning the suffix for every literal '&'.
                .and_then(|tail| {
                    let end = tail
                        .iter()
                        .position(|byte| !byte.is_ascii_alphanumeric() && *byte != b'#')?;
                    (tail[end] == b';').then_some(end)
                })
        {
            let end = cursor
                .saturating_add(1)
                .saturating_add(relative_semicolon)
                .saturating_add(1);
            if let Some(token) = raw.get(cursor..end) {
                let decoded = decode_html_entities(token);
                if decoded != token {
                    mapped.extend(decoded.chars().map(|ch| MappedChar {
                        ch,
                        start: cursor,
                        end,
                    }));
                    cursor = end;
                    continue;
                }
            }
        }

        let Some(ch) = raw.get(cursor..).and_then(|tail| tail.chars().next()) else {
            break;
        };
        let end = cursor.saturating_add(ch.len_utf8());
        mapped.push(MappedChar {
            ch,
            start: cursor,
            end,
        });
        cursor = end;
    }
    mapped
}

fn normalize_mapped(mapped: Vec<MappedChar>) -> Vec<MappedChar> {
    let mut normalized: Vec<MappedChar> = Vec::with_capacity(mapped.len());
    for item in mapped {
        if is_js_whitespace(item.ch) {
            let Some(last) = normalized.last_mut() else {
                continue;
            };
            if last.ch == ' ' {
                last.end = item.end;
            } else {
                normalized.push(MappedChar {
                    ch: ' ',
                    start: item.start,
                    end: item.end,
                });
            }
        } else {
            normalized.push(item);
        }
    }
    while normalized.last().is_some_and(|item| item.ch == ' ') {
        normalized.pop();
    }
    normalized
}

fn equivalent(left: char, right: char) -> bool {
    left == right || matches!((left, right), ('\u{00a0}', ' ') | (' ', '\u{00a0}'))
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;
    use crate::api::{EmbeddingPlan, PlanOperation};
    use crate::boundary::{Boundary, find_article_boundary};
    use crate::text::{assemble_text, extract_fragments};

    #[test]
    fn adversarial_ampersands_have_bounded_embedding_work() {
        let html = format!("<article>{}</article>", "&".repeat(100_000));
        let boundary = find_article_boundary(&html, None).expect("article");
        let plan = EmbeddingPlan {
            index_unit: "codepoint".to_owned(),
            operations: vec![PlanOperation {
                insert_after_index: 0,
                marker: "\u{fe00}".to_owned(),
            }],
        };
        let output = apply_plan(&html, &boundary, &plan).expect("valid plan");
        assert_eq!(
            output,
            format!("<article>&\u{fe00}{}</article>", "&".repeat(99_999))
        );
    }

    #[test]
    fn dense_plan_on_large_document_preserves_operation_order() {
        let html = format!(
            "<article>{}</article><!--{}-->",
            "a".repeat(50_000),
            "x".repeat(4_900_000)
        );
        let boundary = find_article_boundary(&html, None).expect("article");
        let plan = EmbeddingPlan {
            index_unit: "codepoint".to_owned(),
            operations: (0..20_000)
                .rev()
                .map(|index| PlanOperation {
                    insert_after_index: index,
                    marker: "\u{fe00}".to_owned(),
                })
                .collect(),
        };
        let output = apply_plan(&html, &boundary, &plan).expect("valid dense plan");
        assert_eq!(
            output,
            format!(
                "<article>{}{}</article><!--{}-->",
                "a\u{fe00}".repeat(20_000),
                "a".repeat(30_000),
                "x".repeat(4_900_000)
            )
        );
    }

    #[derive(Deserialize)]
    struct FixtureFile {
        cases: Vec<FixtureCase>,
    }

    #[derive(Deserialize)]
    struct FixtureCase {
        id: String,
        html: String,
        override_selector: Option<String>,
        boundary: FixtureBoundary,
        text: String,
        text_rust: String,
        text_divergence: Option<String>,
        plan: EmbeddingPlan,
        expected_output: String,
        invalid_plan: Option<EmbeddingPlan>,
        expected_invalid_output: Option<String>,
        manifest_url: String,
        expected_meta_output_js: String,
        expected_meta_output_rust: String,
        meta_divergence: Option<String>,
    }

    #[derive(Deserialize)]
    struct FixtureBoundary {
        selector: String,
        start: usize,
        end: usize,
    }

    fn fixtures() -> FixtureFile {
        serde_json::from_str(include_str!("fixtures/parity.json"))
            .expect("parity fixtures should deserialize")
    }

    #[test]
    fn groups_same_offset_in_plan_order() {
        let html = "🐟<article>A&nbsp;B</article>";
        let start = html.find("<article>").expect("should find boundary");
        let boundary = Boundary {
            start,
            end: html.len(),
            selector: "article".to_string(),
        };
        let plan = EmbeddingPlan {
            index_unit: "codepoint".to_string(),
            operations: vec![
                PlanOperation {
                    insert_after_index: 0,
                    marker: "\u{fe00}".to_string(),
                },
                PlanOperation {
                    insert_after_index: 0,
                    marker: "\u{fe01}".to_string(),
                },
            ],
        };
        assert_eq!(
            apply_plan(html, &boundary, &plan),
            Some("🐟<article>A\u{fe00}\u{fe01}&nbsp;B</article>".to_string())
        );
    }

    #[test]
    fn manifest_meta_uses_structural_head_close_and_escapes() {
        let html = "<head><script>\"</head>\"</script></head><body></body>";
        let output = inject_manifest_meta(html, "https://example.com/?a=1&b=\"x\"");
        assert!(output.contains("</script><meta name=\"c2pa-manifest-url\""));
        assert!(output.contains("&amp;b=&quot;x&quot;"));
    }

    #[test]
    fn manifest_meta_does_not_activate_quoted_markup() {
        let prefix = r#"<head><meta data-x="></head><img src=x onerror=alert(1)>">"#;
        let html = format!("{prefix}</head><article>Visible</article>");
        assert_eq!(
            inject_manifest_meta(&html, "javascript:alert(1)"),
            format!(
                "{prefix}<meta name=\"c2pa-manifest-url\" content=\"javascript:alert(1)\"></head><article>Visible</article>"
            )
        );
        // The URL is inert metadata, not an href, script source or refresh target.
        let script =
            r#"<head><script>const x = "</bogus><article>hidden</article></head>";</script>"#;
        assert_eq!(
            inject_manifest_meta(
                &format!("{script}</head>"),
                "data:text/html,<script>alert(1)</script>"
            ),
            format!(
                "{script}<meta name=\"c2pa-manifest-url\" content=\"data:text/html,&lt;script&gt;alert(1)&lt;/script&gt;\"></head>"
            )
        );
    }

    #[test]
    fn manifest_meta_ignores_quotes_inside_unquoted_values() {
        // The browser reads `x"y` as an unquoted value, so the second meta's quoted
        // content, including its `</head>`, is attribute text.
        let prefix =
            r#"<head><meta name=x"y><meta content="a>b</head><img src=x onerror=alert(1)>">"#;
        let html = format!("{prefix}</head><body>Visible</body>");
        assert_eq!(
            inject_manifest_meta(&html, "https://manifest.example.com/m"),
            format!(
                "{prefix}<meta name=\"c2pa-manifest-url\" content=\"https://manifest.example.com/m\"></head><body>Visible</body>"
            )
        );
    }

    #[test]
    fn edge_core_parity_fixtures_match() {
        for fixture in fixtures().cases {
            let boundary =
                find_article_boundary(&fixture.html, fixture.override_selector.as_deref())
                    .unwrap_or_else(|| panic!("{} should have a boundary", fixture.id));
            assert_eq!(
                boundary.selector, fixture.boundary.selector,
                "{}",
                fixture.id
            );
            assert_eq!(boundary.start, fixture.boundary.start, "{}", fixture.id);
            assert_eq!(boundary.end, fixture.boundary.end, "{}", fixture.id);
            let boundary_html = fixture
                .html
                .get(boundary.start..boundary.end)
                .expect("fixture boundary should be valid");
            let fragments = extract_fragments(boundary_html);
            let text = assemble_text(boundary_html, &fragments);
            assert_eq!(text, fixture.text_rust, "{}", fixture.id);
            if fixture.text_divergence.is_some() {
                assert_ne!(text, fixture.text, "{}", fixture.id);
            } else {
                assert_eq!(text, fixture.text, "{}", fixture.id);
            }
            assert_eq!(
                apply_plan(&fixture.html, &boundary, &fixture.plan),
                Some(fixture.expected_output),
                "{}",
                fixture.id
            );
            if let Some(invalid_plan) = fixture.invalid_plan {
                assert_eq!(
                    apply_plan(&fixture.html, &boundary, &invalid_plan),
                    fixture.expected_invalid_output,
                    "{}",
                    fixture.id
                );
            }
            let meta = inject_manifest_meta(&fixture.html, &fixture.manifest_url);
            assert_eq!(meta, fixture.expected_meta_output_rust, "{}", fixture.id);
            if fixture.meta_divergence.is_some() {
                assert_ne!(meta, fixture.expected_meta_output_js, "{}", fixture.id);
            } else {
                assert_eq!(meta, fixture.expected_meta_output_js, "{}", fixture.id);
            }
        }
    }

    #[test]
    fn every_fixture_byte_prefix_is_panic_free() {
        for fixture in fixtures().cases {
            for end in 0..=fixture.html.len() {
                let prefix = String::from_utf8_lossy(
                    fixture
                        .html
                        .as_bytes()
                        .get(..end)
                        .expect("prefix range should be valid"),
                );
                let fragments = extract_fragments(&prefix);
                let _ = assemble_text(&prefix, &fragments);
                if let Some(boundary) =
                    find_article_boundary(&prefix, fixture.override_selector.as_deref())
                {
                    let _ = apply_plan(&prefix, &boundary, &fixture.plan);
                }
                let _ = inject_manifest_meta(&prefix, &fixture.manifest_url);
            }
        }
    }
}
