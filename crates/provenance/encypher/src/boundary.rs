use super::text::{StructuralTag, TagKind, scan_structural_tags};

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

#[derive(Debug, Clone, Copy)]
struct ElementMatch {
    start: usize,
    end: usize,
}

#[derive(Clone, Copy)]
enum FirstMatch<'a> {
    Unseen,
    Open { start: usize, name: &'a str },
    Complete(ElementMatch),
}

impl<'a> FirstMatch<'a> {
    fn observe(&mut self, tag: StructuralTag<'a>, opening_matches: bool) {
        match (*self, tag.kind) {
            (Self::Unseen, TagKind::Opening) if opening_matches => {
                *self = Self::Open {
                    start: tag.start,
                    name: tag.name,
                };
            }
            (Self::Open { start, name }, TagKind::Closing)
                if tag.name.eq_ignore_ascii_case(name) =>
            {
                *self = Self::Complete(ElementMatch {
                    start,
                    end: tag.end,
                });
            }
            _ => {}
        }
    }

    fn complete(self) -> Option<ElementMatch> {
        match self {
            Self::Complete(element) => Some(element),
            Self::Unseen | Self::Open { .. } => None,
        }
    }
}

#[derive(Clone, Copy)]
struct ActiveCount {
    start: usize,
    paragraphs: usize,
}

#[derive(Clone, Copy)]
struct ScoredMatch {
    element: ElementMatch,
    paragraphs: usize,
}

#[derive(Clone, Copy)]
struct CountedMatches {
    name: &'static str,
    active: Option<ActiveCount>,
    best: Option<ScoredMatch>,
}

impl CountedMatches {
    const fn new(name: &'static str) -> Self {
        Self {
            name,
            active: None,
            best: None,
        }
    }

    fn observe(&mut self, tag: StructuralTag<'_>) {
        match tag.kind {
            TagKind::Opening => {
                if let Some(active) = &mut self.active {
                    if tag.name.eq_ignore_ascii_case("p") {
                        active.paragraphs += 1;
                    }
                } else if tag.name.eq_ignore_ascii_case(self.name) {
                    self.active = Some(ActiveCount {
                        start: tag.start,
                        paragraphs: 0,
                    });
                }
            }
            TagKind::Closing if tag.name.eq_ignore_ascii_case(self.name) => {
                let Some(active) = self.active.take() else {
                    return;
                };
                let candidate = ScoredMatch {
                    element: ElementMatch {
                        start: active.start,
                        end: tag.end,
                    },
                    paragraphs: active.paragraphs,
                };
                if self
                    .best
                    .is_none_or(|best| candidate.paragraphs > best.paragraphs)
                {
                    self.best = Some(candidate);
                }
            }
            TagKind::Closing => {}
        }
    }
}

enum Selector<'a> {
    Tag(&'a str),
    Class(&'a str),
    Attribute(&'a str, &'a str),
}

impl Selector<'_> {
    fn matches(&self, tag: &StructuralTag<'_>) -> bool {
        match *self {
            Self::Tag(name) => tag.name.eq_ignore_ascii_case(name),
            Self::Class(name) => tag
                .attribute("class")
                .is_some_and(|value| class_matches(value, name)),
            Self::Attribute(attribute, value) => attribute_matches(tag, attribute, value),
        }
    }
}

/// Finds an article boundary using the edge-core priority chain, excluding JSON-LD P5.
#[must_use]
pub fn find_article_boundary(html: &str, override_selector: Option<&str>) -> Option<Boundary> {
    let override_label = override_selector.filter(|selector| !selector.is_empty());
    let override_matcher = override_label.and_then(parse_selector);
    let mut override_element = FirstMatch::Unseen;
    let mut article = CountedMatches::new("article");
    let mut cms = [FirstMatch::Unseen; CMS_CONTENT_CLASSES.len()];
    let mut itemprop = FirstMatch::Unseen;
    let mut main = FirstMatch::Unseen;
    let mut role = FirstMatch::Unseen;
    let mut body = FirstMatch::Unseen;
    let mut div_cluster = CountedMatches::new("div");
    let mut section_cluster = CountedMatches::new("section");
    let mut main_cluster = CountedMatches::new("main");

    // All priority candidates share one structural pass. Each first-match
    // state deliberately keeps an unclosed first opening from being rescued
    // by a later one, matching the previous selector scans.
    scan_structural_tags(html, |tag| {
        let opening = tag.kind == TagKind::Opening;
        override_element.observe(
            tag,
            opening
                && override_matcher
                    .as_ref()
                    .is_some_and(|selector| selector.matches(&tag)),
        );
        let class = opening.then(|| tag.attribute("class")).flatten();
        for (state, expected) in cms.iter_mut().zip(CMS_CONTENT_CLASSES) {
            state.observe(
                tag,
                class.is_some_and(|value| class_matches(value, expected)),
            );
        }
        itemprop.observe(
            tag,
            opening && attribute_matches(&tag, "itemprop", "articleBody"),
        );
        main.observe(tag, opening && tag.name.eq_ignore_ascii_case("main"));
        role.observe(tag, opening && attribute_matches(&tag, "role", "main"));
        body.observe(tag, opening && tag.name.eq_ignore_ascii_case("body"));
        article.observe(tag);
        div_cluster.observe(tag);
        section_cluster.observe(tag);
        main_cluster.observe(tag);
        true
    });

    if let (Some(selector), Some(element)) = (override_label, override_element.complete()) {
        return Some(boundary(element, format!("override:{selector}")));
    }
    if let Some(selected) = article.best {
        return Some(boundary(selected.element, "article"));
    }
    for (state, class_name) in cms.into_iter().zip(CMS_CONTENT_CLASSES) {
        if let Some(element) = state.complete() {
            return Some(boundary(element, format!("class:{class_name}")));
        }
    }
    if let Some(element) = itemprop.complete() {
        return Some(boundary(element, "itemprop:articleBody"));
    }
    if let Some(element) = main.complete() {
        return Some(boundary(element, "main"));
    }
    if let Some(element) = role.complete() {
        return Some(boundary(element, "role:main"));
    }

    let mut cluster = None;
    for candidate in [div_cluster.best, section_cluster.best, main_cluster.best]
        .into_iter()
        .flatten()
        .filter(|candidate| candidate.paragraphs > 4)
    {
        if cluster.is_none_or(|best: ScoredMatch| candidate.paragraphs > best.paragraphs) {
            cluster = Some(candidate);
        }
    }
    if let Some(selected) = cluster {
        return Some(boundary(selected.element, "p-cluster"));
    }
    body.complete().map(|element| boundary(element, "body"))
}

fn boundary(element: ElementMatch, selector: impl Into<String>) -> Boundary {
    Boundary {
        start: element.start,
        end: element.end,
        selector: selector.into(),
    }
}

fn parse_selector(selector: &str) -> Option<Selector<'_>> {
    if let Some(class_name) = selector.strip_prefix('.').filter(|name| !name.is_empty()) {
        return Some(Selector::Class(class_name));
    }
    if selector.starts_with('[') {
        let (attribute, value) = parse_attribute_selector(selector)?;
        return Some(Selector::Attribute(attribute, value));
    }
    Some(Selector::Tag(selector))
}

fn parse_attribute_selector(selector: &str) -> Option<(&str, &str)> {
    let content = selector.strip_prefix('[')?.strip_suffix(']')?;
    let (attribute, raw_value) = content.split_once('=')?;
    if attribute.is_empty()
        || !attribute
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return None;
    }
    let value = match (
        raw_value.as_bytes().first().copied(),
        raw_value.as_bytes().last().copied(),
    ) {
        (Some(first @ (b'"' | b'\'')), Some(last)) if first == last => {
            raw_value.get(1..raw_value.len().saturating_sub(1))?
        }
        (Some(b'"' | b'\''), _) => return None,
        _ => raw_value,
    };
    (!value.is_empty()
        && !value
            .bytes()
            .any(|byte| matches!(byte, b'"' | b'\'' | b']')))
    .then_some((attribute, value))
}

fn attribute_matches(tag: &StructuralTag<'_>, attribute: &str, expected: &str) -> bool {
    tag.attribute(attribute)
        .is_some_and(|value| value.eq_ignore_ascii_case(expected))
}

fn class_matches(value: &str, expected: &str) -> bool {
    let mut expected_parts = expected.split_ascii_whitespace();
    let Some(first) = expected_parts.next() else {
        return false;
    };
    let second = expected_parts.next();
    if expected_parts.next().is_some() {
        return false;
    }
    let mut previous_was_first = false;
    for class in value.split_ascii_whitespace() {
        if second.is_some_and(|second| previous_was_first && class.eq_ignore_ascii_case(second)) {
            return true;
        }
        previous_was_first = class.eq_ignore_ascii_case(first);
        if second.is_none() && previous_was_first {
            return true;
        }
    }
    false
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
    fn article_detection_ignores_inert_opening_markup() {
        let real = "<article><p>Real article.</p></article>";
        for inert in [
            "<!-- <article><p>Comment template.</p></article> -->",
            r#"<script>const template = "<article><p>Script template.</p></article>";</script>"#,
            r#"<div data-template="<article><p>Attribute template.</p></article>"></div>"#,
        ] {
            let html = format!("{inert}{real}");
            let boundary = find_article_boundary(&html, None).expect("should find real article");
            assert_eq!(
                html.get(boundary.start..boundary.end),
                Some(real),
                "inert markup must not become the article: {inert}"
            );
        }
    }

    #[test]
    fn inert_closing_markup_does_not_truncate_an_article() {
        for inert in [
            "<!-- </article> -->",
            r#"<script>const close = "</article>";</script>"#,
            r#"<span data-close="</article>">Attribute text.</span>"#,
        ] {
            let html = format!("<article><p>Start.</p>{inert}<p>End.</p></article>");
            let boundary = find_article_boundary(&html, None).expect("should find article");
            assert_eq!(
                html.get(boundary.start..boundary.end),
                Some(html.as_str()),
                "inert markup must not close the article: {inert}"
            );
        }
    }

    #[test]
    fn invalid_less_than_and_declarations_do_not_hide_or_create_articles() {
        let real = "<article><p>Real article.</p></article>";
        for prefix in [
            "text < y ",
            "< article><p>False spaced article.</p></ article>",
            "<!article><p>False declaration.</p></article>",
            "<?article><p>False instruction.</p></article>",
            "<!bogus \"quoted > remainder\">",
        ] {
            let html = format!("{prefix}{real}");
            let boundary = find_article_boundary(&html, None).expect("should find real article");
            assert_eq!(
                html.get(boundary.start..boundary.end),
                Some(real),
                "invalid markup must not hide or create the article: {prefix}"
            );
        }

        let html = "<article><p>Start.</p></ article><p>End.</p></article>";
        let boundary = find_article_boundary(html, None).expect("should find article");
        assert_eq!(html.get(boundary.start..boundary.end), Some(html));
    }

    #[test]
    fn dense_documents_select_articles_without_a_document_sized_tag_index() {
        let padding = "<i></i>".repeat(700_000);
        let first = "<article><p>First.</p></article>";
        let second = "<article><p>Second.</p><p>Wins.</p></article>";
        let html = format!("{padding}{first}{second}");

        let boundary = find_article_boundary(&html, None).expect("should find article");
        assert_eq!(boundary.selector, "article");
        assert_eq!(html.get(boundary.start..boundary.end), Some(second));
    }

    #[test]
    fn selects_largest_paragraph_cluster_at_five() {
        let html = "<body><div><p>1</p><p>2</p><p>3</p><p>4</p><p>5</p></div></body>";
        let boundary = find_article_boundary(html, None).expect("should find cluster");
        assert_eq!(boundary.selector, "p-cluster");
    }
}
