use std::sync::LazyLock;

use regex::Regex;

const CMS_CONTENT_CLASSES: [&str; 13] = [
    "entry-content",
    "wp-block-post-content",
    "post-content",
    "article-body",
    "article-content",
    "story-body",
    "story-body-text",
    "gh-content",
    "w-richtext",
    "sqs-block-content",
    "body markup",
    "e-content",
    "RichTextStoryBody",
];

static ARTICLE: LazyLock<Option<Regex>> = LazyLock::new(|| tag_opening("article"));
static MAIN: LazyLock<Option<Regex>> = LazyLock::new(|| tag_opening("main"));
static BODY: LazyLock<Option<Regex>> = LazyLock::new(|| tag_opening("body"));
static CLUSTER_CONTAINERS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    ["div", "section", "main"]
        .into_iter()
        .filter_map(tag_opening)
        .collect()
});
static PARAGRAPH: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"(?i)<p(?-u:\b)").ok());
static CMS_CLASSES: LazyLock<Vec<(&str, Regex)>> = LazyLock::new(|| {
    CMS_CONTENT_CLASSES
        .into_iter()
        .filter_map(|class_name| Some((class_name, class_opening(class_name)?)))
        .collect()
});
static ITEMPROP_ARTICLE_BODY: LazyLock<Option<Regex>> =
    LazyLock::new(|| attribute_opening("itemprop", "articleBody"));
static ROLE_MAIN: LazyLock<Option<Regex>> = LazyLock::new(|| attribute_opening("role", "main"));
static SELECTOR_ATTRIBUTE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"\[([A-Za-z0-9_]+)=["']?([^"'\]]+)["']?\]"#).ok());

/// The UTF-8 byte range and detection label of an article's outer HTML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Boundary {
    /// Inclusive byte offset of the opening element.
    pub start: usize,
    /// Exclusive byte offset after the first matching closing element.
    pub end: usize,
    /// Detection-chain selector label.
    pub selector: String,
}

/// Finds an article boundary using the edge-core priority chain, excluding JSON-LD P5.
#[must_use]
pub fn find_article_boundary(html: &str, override_selector: Option<&str>) -> Option<Boundary> {
    if let Some(selector) = override_selector.filter(|selector| !selector.is_empty())
        && let Some(range) = match_selector(html, selector)
    {
        return Some(Boundary {
            start: range.0,
            end: range.1,
            selector: format!("override:{selector}"),
        });
    }

    let articles = ARTICLE
        .as_ref()
        .map_or_else(Vec::new, |opening| match_all(html, opening));
    if let Some(first) = articles.first().copied() {
        let mut selected = first;
        let mut selected_count = count_paragraphs(html.get(first.0..first.1).unwrap_or(""));
        for candidate in articles.iter().copied().skip(1) {
            let candidate_count =
                count_paragraphs(html.get(candidate.0..candidate.1).unwrap_or(""));
            if candidate_count > selected_count {
                selected = candidate;
                selected_count = candidate_count;
            }
        }
        return Some(boundary(selected, "article"));
    }

    for (class_name, opening) in CMS_CLASSES.iter() {
        if let Some(range) = match_first(html, opening) {
            return Some(boundary(range, &format!("class:{class_name}")));
        }
    }

    if let Some(range) = match_fixed(html, &ITEMPROP_ARTICLE_BODY) {
        return Some(boundary(range, "itemprop:articleBody"));
    }
    if let Some(range) = match_fixed(html, &MAIN) {
        return Some(boundary(range, "main"));
    }
    if let Some(range) = match_fixed(html, &ROLE_MAIN) {
        return Some(boundary(range, "role:main"));
    }
    if let Some(range) = find_largest_p_cluster(html) {
        return Some(boundary(range, "p-cluster"));
    }
    match_fixed(html, &BODY).map(|range| boundary(range, "body"))
}

fn boundary((start, end): (usize, usize), selector: &str) -> Boundary {
    Boundary {
        start,
        end,
        selector: selector.to_owned(),
    }
}

// Every opening pattern captures the element's tag name as group 1.
fn tag_opening(tag_name: &str) -> Option<Regex> {
    Regex::new(&format!(
        r"(?i)<({})(?-u:\b)[^>]*>",
        regex::escape(tag_name)
    ))
    .ok()
}

fn class_opening(class_name: &str) -> Option<Regex> {
    Regex::new(&format!(
        r#"(?i)<([A-Za-z0-9_]+)(?-u:\b)[^>]*class\s*=\s*["'][^"']*(?-u:\b){}(?-u:\b)[^"']*["'][^>]*>"#,
        regex::escape(class_name)
    ))
    .ok()
}

fn attribute_opening(attribute: &str, value: &str) -> Option<Regex> {
    Regex::new(&format!(
        r#"(?i)<([A-Za-z0-9_]+)(?-u:\b)[^>]*{}\s*=\s*["']{}["'][^>]*>"#,
        regex::escape(attribute),
        regex::escape(value)
    ))
    .ok()
}

fn match_fixed(html: &str, opening: &LazyLock<Option<Regex>>) -> Option<(usize, usize)> {
    match_first(html, opening.as_ref()?)
}

fn match_first(html: &str, opening: &Regex) -> Option<(usize, usize)> {
    let captures = opening.captures(html)?;
    let whole = captures.get(0)?;
    close_first_element(html, whole.start(), whole.end(), captures.get(1)?.as_str())
}

fn match_all(html: &str, opening: &Regex) -> Vec<(usize, usize)> {
    let mut matches = Vec::new();
    let mut cursor = 0_usize;
    while let Some(tail) = html.get(cursor..) {
        let Some(captures) = opening.captures(tail) else {
            break;
        };
        let (Some(whole), Some(tag_name)) = (captures.get(0), captures.get(1)) else {
            break;
        };
        let start = cursor.saturating_add(whole.start());
        let opening_end = cursor.saturating_add(whole.end());
        let Some(range) = close_first_element(html, start, opening_end, tag_name.as_str()) else {
            break;
        };
        matches.push(range);
        cursor = range.1;
    }
    matches
}

// An operator-configured selector is compiled per document.
fn match_selector(html: &str, selector: &str) -> Option<(usize, usize)> {
    if let Some(class_name) = selector.strip_prefix('.') {
        return match_first(html, &class_opening(class_name)?);
    }
    if selector.starts_with('[')
        && let Some(captures) = SELECTOR_ATTRIBUTE.as_ref()?.captures(selector)
    {
        let opening = attribute_opening(captures.get(1)?.as_str(), captures.get(2)?.as_str())?;
        return match_first(html, &opening);
    }
    match_first(html, &tag_opening(selector)?)
}

/// Extends an opening tag to the first `</tag_name>` after it, ASCII case-insensitively.
fn close_first_element(
    html: &str,
    opening_start: usize,
    opening_end: usize,
    tag_name: &str,
) -> Option<(usize, usize)> {
    let closing = format!("</{tag_name}>");
    let tail = html.as_bytes().get(opening_end..)?;
    let offset = tail
        .windows(closing.len())
        .position(|window| window.eq_ignore_ascii_case(closing.as_bytes()))?;
    Some((
        opening_start,
        opening_end
            .saturating_add(offset)
            .saturating_add(closing.len()),
    ))
}

fn count_paragraphs(html: &str) -> usize {
    PARAGRAPH
        .as_ref()
        .map_or(0, |regex| regex.find_iter(html).count())
}

fn find_largest_p_cluster(html: &str) -> Option<(usize, usize)> {
    let mut best = None;
    let mut best_count = 4_usize;
    for opening in CLUSTER_CONTAINERS.iter() {
        for range in match_all(html, opening) {
            let count = count_paragraphs(html.get(range.0..range.1).unwrap_or(""));
            if count > best_count {
                best = Some(range);
                best_count = count;
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_priority_chain_and_byte_offsets() {
        let html = "🐟<main><p>Main.</p></main><article><p>Chosen.</p></article>";
        let boundary = find_article_boundary(html, None).expect("should find article");
        assert_eq!(boundary.selector, "article");
        assert_eq!(
            html.get(boundary.start..boundary.end),
            Some("<article><p>Chosen.</p></article>")
        );
    }

    #[test]
    fn nested_same_name_uses_first_closing_tag() {
        let html = "<article>outer<article>inner</article>tail</article>";
        let boundary = find_article_boundary(html, None).expect("should find article");
        assert_eq!(
            html.get(boundary.start..boundary.end),
            Some("<article>outer<article>inner</article>")
        );
    }

    #[test]
    fn closing_tags_match_ascii_case_insensitively() {
        let html = "<Article><p>One.</p></ARTICLE><DIV class=\"Entry-Content\">x</div>";
        let boundary = find_article_boundary(html, None).expect("should find article");
        assert_eq!(
            html.get(boundary.start..boundary.end),
            Some("<Article><p>One.</p></ARTICLE>")
        );
        let boundary =
            find_article_boundary(html, Some(".entry-content")).expect("should find override");
        assert_eq!(
            html.get(boundary.start..boundary.end),
            Some("<DIV class=\"Entry-Content\">x</div>")
        );
    }

    #[test]
    fn selects_largest_paragraph_cluster_at_five() {
        let html = "<body><div><p>1</p><p>2</p><p>3</p><p>4</p><p>5</p></div></body>";
        let boundary = find_article_boundary(html, None).expect("should find cluster");
        assert_eq!(boundary.selector, "p-cluster");
    }
}
