use super::api::EmbeddingPlan;
use super::boundary::Boundary;
use super::text::visit_canonical_text;

const MAGIC_PREFIX: &str = "\u{e01df}\u{e01e0}\u{e01e1}\u{e01e2}";
const MANIFEST_META_PREFIX: &str = r#"<meta name="c2pa-manifest-url" content=""#;
const MANIFEST_META_SUFFIX: &str = r#"">"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplyError {
    Alignment,
    TooLarge,
}

/// Reports whether a string contains the four-character Encypher magic prefix.
#[must_use]
pub fn has_existing_markers(s: &str) -> bool {
    s.contains(MAGIC_PREFIX)
}

/// Applies a codepoint embedding plan and optional manifest insertion in one
/// exact-capacity output allocation.
///
/// The plan must already have passed [`super::api::validate_plan`]. At an
/// equal source offset, plan markers precede the manifest in plan order.
pub(crate) fn apply_plan(
    html: &str,
    boundary: &Boundary,
    plan: &EmbeddingPlan,
    manifest: Option<(&str, usize)>,
    max_output_bytes: usize,
) -> Result<String, ApplyError> {
    let marker_bytes = plan
        .operations
        .iter()
        .try_fold(0usize, |total, operation| {
            total.checked_add(operation.marker.len())
        })
        .ok_or(ApplyError::TooLarge)?;
    let manifest_bytes = manifest
        .map_or(Some(0), |(url, _)| manifest_meta_len(url))
        .ok_or(ApplyError::TooLarge)?;
    let output_len = html
        .len()
        .checked_add(marker_bytes)
        .and_then(|with_markers| with_markers.checked_add(manifest_bytes))
        .ok_or(ApplyError::TooLarge)?;
    if output_len > max_output_bytes {
        return Err(ApplyError::TooLarge);
    }

    let boundary_html = html
        .get(boundary.start..boundary.end)
        .ok_or(ApplyError::Alignment)?;
    let offsets_needed = match plan
        .operations
        .iter()
        .filter_map(|operation| usize::try_from(operation.insert_after_index).ok())
        .max()
    {
        Some(index) => index.checked_add(1).ok_or(ApplyError::Alignment)?,
        None => 0,
    };
    let mut insertion_offsets = Vec::with_capacity(offsets_needed.min(boundary_html.len()));
    let mut first_visible_start = None;
    visit_canonical_text(boundary_html, |item| {
        if first_visible_start.is_none() {
            first_visible_start = Some(item.start);
        }
        if insertion_offsets.len() < offsets_needed {
            insertion_offsets.push(item.end);
        }
        insertion_offsets.len() < offsets_needed
    });
    let first_visible_start = first_visible_start.ok_or(ApplyError::Alignment)?;

    let mut insertions = Vec::with_capacity(plan.operations.len());
    for operation in &plan.operations {
        let relative = if operation.insert_after_index == -1 {
            first_visible_start
        } else {
            let index =
                usize::try_from(operation.insert_after_index).map_err(|_| ApplyError::Alignment)?;
            insertion_offsets
                .get(index)
                .copied()
                .ok_or(ApplyError::Alignment)?
        };
        let absolute = boundary
            .start
            .checked_add(relative)
            .ok_or(ApplyError::Alignment)?;
        insertions.push((absolute, operation.marker.as_str()));
    }
    // Stable sorting preserves API order for multiple markers at the same offset.
    insertions.sort_by_key(|(offset, _)| *offset);

    let mut result = String::with_capacity(output_len);
    let mut cursor = 0;
    let mut manifest = manifest;
    for (offset, marker) in insertions {
        if manifest.is_some_and(|(_, manifest_offset)| manifest_offset < offset) {
            let (url, manifest_offset) = manifest.take().ok_or(ApplyError::Alignment)?;
            result.push_str(
                html.get(cursor..manifest_offset)
                    .ok_or(ApplyError::Alignment)?,
            );
            push_manifest_meta(&mut result, url);
            cursor = manifest_offset;
        }
        result.push_str(html.get(cursor..offset).ok_or(ApplyError::Alignment)?);
        result.push_str(marker);
        cursor = offset;
    }
    if let Some((url, manifest_offset)) = manifest {
        result.push_str(
            html.get(cursor..manifest_offset)
                .ok_or(ApplyError::Alignment)?,
        );
        push_manifest_meta(&mut result, url);
        cursor = manifest_offset;
    }
    result.push_str(html.get(cursor..).ok_or(ApplyError::Alignment)?);
    Ok(result)
}

/// Exact byte length of a manifest meta element after attribute escaping.
fn manifest_meta_len(manifest_url: &str) -> Option<usize> {
    manifest_url.chars().try_fold(
        MANIFEST_META_PREFIX.len() + MANIFEST_META_SUFFIX.len(),
        |total, character| {
            let bytes = match character {
                '&' => 5,
                '"' => 6,
                '<' | '>' => 4,
                _ => character.len_utf8(),
            };
            total.checked_add(bytes)
        },
    )
}

fn push_manifest_meta(output: &mut String, manifest_url: &str) {
    output.push_str(MANIFEST_META_PREFIX);
    for character in manifest_url.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '"' => output.push_str("&quot;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(character),
        }
    }
    output.push_str(MANIFEST_META_SUFFIX);
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;
    use crate::api::{EmbeddingPlan, PlanOperation};
    use crate::boundary::{Boundary, find_article_boundary};
    use crate::text::{document_metadata, visible_text};

    fn apply(html: &str, boundary: &Boundary, plan: &EmbeddingPlan) -> Option<String> {
        apply_plan(html, boundary, plan, None, usize::MAX).ok()
    }

    fn manifest_output(html: &str, manifest_url: &str) -> String {
        let metadata = document_metadata(html);
        if manifest_url.is_empty() || metadata.manifest_present {
            return html.to_owned();
        }
        let Some(offset) = metadata.head_close else {
            return html.to_owned();
        };
        let mut output =
            String::with_capacity(html.len() + manifest_meta_len(manifest_url).unwrap_or(0));
        output.push_str(html.get(..offset).expect("metadata offset should be valid"));
        push_manifest_meta(&mut output, manifest_url);
        output.push_str(html.get(offset..).expect("metadata offset should be valid"));
        output
    }

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
        let output = apply(&html, &boundary, &plan).expect("valid plan");
        assert_eq!(
            output,
            format!("<article>&\u{fe00}{}</article>", "&".repeat(99_999))
        );
    }

    #[test]
    fn ordered_entity_replacements_preserve_original_insertion_spans() {
        let html = "<article>&amp;lt;|&amp;#65;|&#38;lt;</article>";
        let boundary = find_article_boundary(html, None).expect("article");
        let plan = EmbeddingPlan {
            index_unit: "codepoint".to_owned(),
            operations: vec![
                PlanOperation {
                    insert_after_index: 0,
                    marker: "\u{fe00}".to_owned(),
                },
                PlanOperation {
                    insert_after_index: 2,
                    marker: "\u{fe01}".to_owned(),
                },
                PlanOperation {
                    insert_after_index: 4,
                    marker: "\u{fe02}".to_owned(),
                },
            ],
        };

        assert_eq!(
            visible_text(
                html.get(boundary.start..boundary.end)
                    .expect("boundary should be valid"),
                usize::MAX,
            )
            .0,
            "<|A|&lt;"
        );
        assert_eq!(
            apply(html, &boundary, &plan),
            Some(
                "<article>&amp;lt;\u{fe00}|&amp;#65;\u{fe01}|&#38;\u{fe02}lt;</article>".to_owned()
            )
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
        let output = apply(&html, &boundary, &plan).expect("valid dense plan");
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

    #[test]
    fn sparse_plan_on_markup_dense_article_bounds_fragment_work() {
        let html = format!("<article>{}</article>", "<b>a</b>".repeat(612_500));
        let boundary = find_article_boundary(&html, None).expect("article");
        let plan = EmbeddingPlan {
            index_unit: "codepoint".to_owned(),
            operations: vec![
                PlanOperation {
                    insert_after_index: -1,
                    marker: "\u{fe00}".to_owned(),
                },
                PlanOperation {
                    insert_after_index: 0,
                    marker: "\u{fe01}".to_owned(),
                },
                PlanOperation {
                    insert_after_index: 0,
                    marker: "\u{fe02}".to_owned(),
                },
            ],
        };

        let output = apply(&html, &boundary, &plan).expect("valid sparse plan");
        let marker_bytes = plan
            .operations
            .iter()
            .map(|operation| operation.marker.len())
            .sum::<usize>();
        assert_eq!(output.len(), html.len() + marker_bytes);
        assert!(output.starts_with("<article><b>\u{fe00}a\u{fe01}\u{fe02}</b>"));
        assert!(output.ends_with("</article>"));
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
            apply(html, &boundary, &plan),
            Some("🐟<article>A\u{fe00}\u{fe01}&nbsp;B</article>".to_string())
        );
    }

    #[test]
    fn manifest_meta_uses_structural_head_close_and_escapes() {
        let html = "<head><script>\"</head>\"</script></head><body></body>";
        let output = manifest_output(html, "https://example.com/?a=1&b=\"x\"");
        assert!(output.contains("</script><meta name=\"c2pa-manifest-url\""));
        assert!(output.contains("&amp;b=&quot;x&quot;"));
    }

    #[test]
    fn manifest_detection_ignores_inert_markup() {
        for html in [
            r#"<!-- <meta name="c2pa-manifest-url" content="https://comment.example"> -->"#,
            r#"<script>const meta = '<meta name="c2pa-manifest-url" content="https://script.example">';</script>"#,
            r#"<div data-template='<meta name="c2pa-manifest-url" content="https://attribute.example">'></div>"#,
        ] {
            assert!(
                !document_metadata(html).manifest_present,
                "inert markup must not declare a manifest: {html}"
            );
        }
        assert!(
            document_metadata(
                r#"<head><meta content="https://real.example" name="c2pa-manifest-url"></head>"#
            )
            .manifest_present
        );
    }

    #[test]
    fn invalid_less_than_and_declarations_do_not_hide_manifest_meta() {
        let html = concat!(
            "text < y ",
            "<!bogus \"quoted > remainder\">",
            "<?meta>",
            r#"<meta name="c2pa-manifest-url" content="https://real.example">"#
        );
        assert!(document_metadata(html).manifest_present);
        assert!(
            !document_metadata(
                r#"< meta name="c2pa-manifest-url"><!meta name="c2pa-manifest-url">"#
            )
            .manifest_present
        );
    }

    #[test]
    fn manifest_meta_does_not_activate_quoted_markup() {
        let prefix = r#"<head><meta data-x="></head><img src=x onerror=alert(1)>">"#;
        let html = format!("{prefix}</head><article>Visible</article>");
        assert_eq!(
            manifest_output(&html, "javascript:alert(1)"),
            format!(
                "{prefix}<meta name=\"c2pa-manifest-url\" content=\"javascript:alert(1)\"></head><article>Visible</article>"
            )
        );
        // The URL is inert metadata, not an href, script source or refresh target.
        let script =
            r#"<head><script>const x = "</bogus><article>hidden</article></head>";</script>"#;
        assert_eq!(
            manifest_output(
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
            manifest_output(&html, "https://manifest.example.com/m"),
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
            let text = visible_text(boundary_html, usize::MAX).0;
            assert_eq!(text, fixture.text_rust, "{}", fixture.id);
            if fixture.text_divergence.is_some() {
                assert_ne!(text, fixture.text, "{}", fixture.id);
            } else {
                assert_eq!(text, fixture.text, "{}", fixture.id);
            }
            assert_eq!(
                apply(&fixture.html, &boundary, &fixture.plan),
                Some(fixture.expected_output),
                "{}",
                fixture.id
            );
            if let Some(invalid_plan) = fixture.invalid_plan {
                assert_eq!(
                    apply(&fixture.html, &boundary, &invalid_plan),
                    fixture.expected_invalid_output,
                    "{}",
                    fixture.id
                );
            }
            let meta = manifest_output(&fixture.html, &fixture.manifest_url);
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
                let _ = visible_text(&prefix, usize::MAX);
                if let Some(boundary) =
                    find_article_boundary(&prefix, fixture.override_selector.as_deref())
                {
                    let _ = apply(&prefix, &boundary, &fixture.plan);
                }
                let _ = manifest_output(&prefix, &fixture.manifest_url);
            }
        }
    }
}
