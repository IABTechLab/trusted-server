use std::sync::LazyLock;

use regex::Regex;
use sha2::{Digest as _, Sha256};

/// Elements whose content canonical text drops: edge-core's set in
/// `integrations/edge-core/fragments.js`.
const TEXT_IGNORED: [&str; 7] = ["script", "style", "noscript", "svg", "math", "code", "pre"];

/// Elements whose content browsers never parse as markup (raw text and RCDATA).
/// A `/>` spelling does not close them, and the closing-tag search skips them.
const RAW_TEXT: [&str; 9] = [
    "script",
    "style",
    "noscript",
    "title",
    "textarea",
    "xmp",
    "iframe",
    "noembed",
    "plaintext",
];

static TITLE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?is)<title[^>]*>(.*?)</title>").ok());
static DECIMAL_ENTITY: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"&#([0-9]+);").ok());
static HEX_ENTITY: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"&#x([0-9a-fA-F]+);").ok());

/// A byte range containing one raw text run in an HTML document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fragment {
    /// Inclusive UTF-8 byte offset at which the text begins.
    pub start: usize,
    /// Exclusive UTF-8 byte offset at which the text ends.
    pub end: usize,
}

/// Extracts visible raw-text fragments and their UTF-8 byte ranges.
#[must_use]
pub fn extract_fragments(html: &str) -> Vec<Fragment> {
    let mut fragments = Vec::new();
    scan_html(
        html,
        |name| contains_tag(&TEXT_IGNORED, name),
        |start, end| {
            if html
                .get(start..end)
                .is_some_and(|raw| raw.chars().any(|ch| !is_js_whitespace(ch)))
            {
                fragments.push(Fragment { start, end });
            }
        },
        |_, _| {},
    );
    fragments
}

/// Decodes, normalizes, trims, and joins raw text fragments with single spaces.
#[must_use]
pub fn assemble_text(html: &str, fragments: &[Fragment]) -> String {
    let mut output = String::new();
    for fragment in fragments {
        if let Some(raw) = html.get(fragment.start..fragment.end) {
            let decoded = decode_html_entities(raw);
            let normalized = normalize_js_whitespace(&decoded);
            if !normalized.is_empty() {
                if !output.is_empty() {
                    output.push(' ');
                }
                output.push_str(&normalized);
            }
        }
    }
    output
}

/// Returns the canonical visible text of an HTML string.
#[must_use]
pub fn visible_text(html: &str) -> String {
    assemble_text(html, &extract_fragments(html))
}

/// Returns a lowercase SHA-256 digest of `text` as 64 hexadecimal characters.
#[must_use]
pub fn content_hash_hex(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    let mut output = String::with_capacity(64);
    for byte in digest {
        if let Some(high) = char::from_digit(u32::from(byte >> 4), 16) {
            output.push(high);
        }
        if let Some(low) = char::from_digit(u32::from(byte & 0x0f), 16) {
            output.push(low);
        }
    }
    output
}

/// Returns at most `max_scalars` Unicode scalar values without splitting one.
#[must_use]
pub fn truncate_scalars(text: &str, max_scalars: usize) -> &str {
    let Some((end, _)) = text.char_indices().nth(max_scalars) else {
        return text;
    };
    text.get(..end).unwrap_or(text)
}

/// Counts Unicode scalar values in `text`.
#[must_use]
pub fn scalar_count(text: &str) -> usize {
    text.chars().count()
}

/// Extracts the first title, decodes entities, normalizes whitespace, and caps it at 500 scalars.
#[must_use]
pub fn extract_title(html: &str) -> Option<String> {
    let regex = TITLE.as_ref()?;
    let raw = regex.captures(html)?.get(1)?.as_str();
    let decoded = decode_html_entities(raw);
    let mut normalized = normalize_js_whitespace(&decoded);
    normalized.truncate(truncate_scalars(&normalized, 500).len());
    Some(normalized)
}

/// Finds the first closing tag outside comments, the canonical-text ignored
/// elements, and raw-text or RCDATA elements.
#[must_use]
pub fn structural_close_tag(html: &str, tag: &str) -> Option<usize> {
    let mut found = None;
    scan_html(
        html,
        |name| contains_tag(&TEXT_IGNORED, name) || contains_tag(&RAW_TEXT, name),
        |_, _| {},
        |start, token| {
            if found.is_none()
                && closing_tag_name(token).is_some_and(|name| name.eq_ignore_ascii_case(tag))
            {
                found = Some(start);
            }
        },
    );
    found
}

/// Scans tags, comments and text runs; both consumers share its comment and
/// quoted-attribute handling. `ignores` picks the elements whose content is
/// skipped: canonical text passes [`TEXT_IGNORED`], and the structural
/// closing-tag search passes [`TEXT_IGNORED`] and [`RAW_TEXT`].
fn scan_html<'a>(
    html: &'a str,
    ignores: impl Fn(&str) -> bool,
    mut text: impl FnMut(usize, usize),
    mut tag: impl FnMut(usize, &'a str),
) {
    let bytes = html.as_bytes();
    let mut cursor = 0;
    let mut ignored: Option<&str> = None;
    while cursor < bytes.len() {
        if let Some(name) = ignored {
            let Some(open) = find_byte(bytes, b'<', cursor) else {
                break;
            };
            cursor = open + 1;
            if !starts_with_at(bytes, open, b"</") {
                continue;
            }
            if name.eq_ignore_ascii_case("plaintext")
                || !bytes
                    .get(open + 2..open + 2 + name.len())
                    .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name.as_bytes()))
                || !bytes.get(open + 2 + name.len()).is_some_and(|byte| {
                    byte.is_ascii_whitespace() || *byte == b'>' || *byte == b'/'
                })
            {
                continue;
            }
            let Some(end) = tag_end(bytes, open + 2) else {
                break;
            };
            // As in browsers, attributes and `/` do not stop an end tag from
            // closing. Unlike edge-core's depth count, no other end tag closes.
            ignored = None;
            cursor = end + 1;
            continue;
        }
        if starts_with_at(bytes, cursor, b"<!--") {
            let Some(end) = comment_end(bytes, cursor) else {
                break;
            };
            cursor = end;
        } else if bytes[cursor] == b'<' {
            let Some(end) = tag_end(bytes, cursor + 1) else {
                break;
            };
            let Some(token) = html.get(cursor..=end) else {
                break;
            };
            tag(cursor, token);
            if let Some(name) = opening_tag_name(token).filter(|name| ignores(name))
                // As in edge-core, `/>` makes an element empty, except raw-text
                // elements, whose content browsers never parse as markup.
                && (!is_self_closing(token) || contains_tag(&RAW_TEXT, name))
            {
                ignored = Some(name);
            }
            cursor = end + 1;
        } else {
            let end = find_byte(bytes, b'<', cursor).unwrap_or(bytes.len());
            text(cursor, end);
            cursor = end;
        }
    }
}

/// Returns the offset just past the comment opening at `start`. As in browsers,
/// `<!-->` and `<!--->` are whole comments, and `--!>` also ends one.
fn comment_end(bytes: &[u8], start: usize) -> Option<usize> {
    let body = start + 4;
    if starts_with_at(bytes, body, b">") {
        return Some(body + 1);
    }
    if starts_with_at(bytes, body, b"->") {
        return Some(body + 2);
    }
    let dash = find_bytes(bytes, b"-->", body).map(|end| end + 3);
    let bang = find_bytes(bytes, b"--!>", body).map(|end| end + 4);
    match (dash, bang) {
        (Some(dash), Some(bang)) => Some(dash.min(bang)),
        (end, None) | (None, end) => end,
    }
}

/// Tokenizer states between a tag's name and its closing `>`.
#[derive(Clone, Copy)]
enum TagState {
    Name,
    BeforeAttributeName,
    AttributeName,
    BeforeValue,
    Quoted(u8),
    Unquoted,
}

/// Returns the offset of the `>` ending the tag whose name starts at `start`.
/// As in the HTML tokenizer, a quote delimits only a value it begins.
fn tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut state = TagState::Name;
    for (offset, byte) in bytes.get(start..)?.iter().copied().enumerate() {
        let space = byte.is_ascii_whitespace();
        state = match state {
            TagState::Quoted(quote) => {
                if byte == quote {
                    TagState::BeforeAttributeName
                } else {
                    state
                }
            }
            _ if byte == b'>' => return Some(start + offset),
            TagState::Name if space || byte == b'/' => TagState::BeforeAttributeName,
            TagState::BeforeAttributeName if !space && byte != b'/' => TagState::AttributeName,
            TagState::AttributeName if byte == b'=' => TagState::BeforeValue,
            TagState::AttributeName if byte == b'/' => TagState::BeforeAttributeName,
            TagState::BeforeValue if byte == b'"' || byte == b'\'' => TagState::Quoted(byte),
            TagState::BeforeValue if !space => TagState::Unquoted,
            TagState::Unquoted if space => TagState::BeforeAttributeName,
            _ => state,
        };
    }
    None
}

pub(crate) fn decode_html_entities(input: &str) -> String {
    let mut result = input.to_owned();
    for (encoded, decoded) in [
        ("&amp;", "&"),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#039;", "'"),
        ("&apos;", "'"),
        ("&#x27;", "'"),
        ("&#x2F;", "/"),
        ("&nbsp;", "\u{00a0}"),
        ("&ndash;", "\u{2013}"),
        ("&mdash;", "\u{2014}"),
        ("&lsquo;", "\u{2018}"),
        ("&rsquo;", "\u{2019}"),
        ("&ldquo;", "\u{201c}"),
        ("&rdquo;", "\u{201d}"),
        ("&hellip;", "\u{2026}"),
        ("&bull;", "\u{2022}"),
    ] {
        if result.contains(encoded) {
            result = result.replace(encoded, decoded);
        }
    }
    result = decode_numeric_entities(&result, false);
    decode_numeric_entities(&result, true)
}

pub(crate) fn normalize_js_whitespace(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut pending_space = false;
    for ch in input.chars() {
        if is_js_whitespace(ch) {
            pending_space = !output.is_empty();
        } else {
            if pending_space {
                output.push(' ');
                pending_space = false;
            }
            output.push(ch);
        }
    }
    output
}

pub(crate) fn is_js_whitespace(ch: char) -> bool {
    matches!(
        ch,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

fn decode_numeric_entities(input: &str, hexadecimal: bool) -> String {
    let regex = if hexadecimal {
        &*HEX_ENTITY
    } else {
        &*DECIMAL_ENTITY
    };
    let Some(regex) = regex.as_ref() else {
        return input.to_owned();
    };
    let radix = if hexadecimal { 16 } else { 10 };
    regex
        .replace_all(input, |captures: &regex::Captures<'_>| {
            captures
                .get(1)
                .and_then(|digits| u32::from_str_radix(digits.as_str(), radix).ok())
                .and_then(char::from_u32)
                .map_or_else(
                    || {
                        captures
                            .get(0)
                            .map_or("", |matched| matched.as_str())
                            .to_owned()
                    },
                    |decoded| decoded.to_string(),
                )
        })
        .into_owned()
}

fn find_byte(bytes: &[u8], needle: u8, start: usize) -> Option<usize> {
    let relative = bytes
        .get(start..)?
        .iter()
        .position(|byte| *byte == needle)?;
    Some(start.saturating_add(relative))
}

fn find_bytes(bytes: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(start.min(bytes.len()));
    }
    let haystack = bytes.get(start..)?;
    let relative = haystack
        .windows(needle.len())
        .position(|window| window == needle)?;
    Some(start.saturating_add(relative))
}

fn starts_with_at(bytes: &[u8], start: usize, needle: &[u8]) -> bool {
    bytes
        .get(start..)
        .is_some_and(|tail| tail.starts_with(needle))
}

fn opening_tag_name(token: &str) -> Option<&str> {
    let content = token
        .strip_prefix('<')?
        .trim_start_matches(is_js_whitespace);
    if content.starts_with('/') {
        return None;
    }
    take_tag_name(content)
}

fn closing_tag_name(token: &str) -> Option<&str> {
    let content = token.strip_prefix("</")?;
    let name = take_tag_name(content)?;
    let remainder = content.get(name.len()..)?.trim_matches(is_js_whitespace);
    (remainder == ">").then_some(name)
}

fn take_tag_name(content: &str) -> Option<&str> {
    let end = content
        .char_indices()
        .find(|(_, ch)| !(ch.is_ascii_alphanumeric() || *ch == '_'))
        .map_or(content.len(), |(index, _)| index);
    (end > 0).then(|| content.get(..end)).flatten()
}

fn contains_tag(tags: &[&str], name: &str) -> bool {
    tags.iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// Matches edge-core's `/\/\s*>$/` test on a complete tag token.
fn is_self_closing(token: &str) -> bool {
    token
        .strip_suffix('>')
        .is_some_and(|body| body.trim_end_matches(is_js_whitespace).ends_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_markup_and_unmatched_script_tags_stay_inert() {
        for html in [
            r#"<head><meta data-x="></head><img src=x onerror=alert(1)>"></head><article>Visible</article>"#,
            r#"<head><script>const x = "</bogus><article>hidden</article></head>";</script></head><article>Visible</article>"#,
        ] {
            assert_eq!(structural_close_tag(html, "head"), html.rfind("</head>"));
            assert_eq!(visible_text(html), "Visible");
        }
    }

    #[test]
    fn browser_end_tag_and_comment_forms_close_where_browsers_do() {
        // `</script x>` ends the script and `<!-->` is a whole comment, so each
        // later `</head>` sits inside an attribute value.
        for prefix in [
            r#"<head><script></script x><meta content="</script></head><img src=x onerror=alert(1)>">"#,
            r#"<head><!--><meta content="--></head><img src=x onerror=alert(1)>">"#,
        ] {
            let html = format!("{prefix}</head><article>Visible</article>");
            assert_eq!(structural_close_tag(&html, "head"), Some(prefix.len()));
        }
        assert_eq!(
            visible_text("<code>a</code/>b<!--->c<!-- d --!>e<style>f</style\tx>g"),
            "b c e g"
        );
    }

    #[test]
    fn extracts_decodes_and_normalizes_visible_text() {
        let html = "<article> A&nbsp;B <em>&amp;</em><script>hidden</script> &#x43; </article>";
        let fragments = extract_fragments(html);
        assert_eq!(assemble_text(html, &fragments), "A B & C");
        assert_eq!(visible_text(html), "A B & C");
    }

    #[test]
    fn title_hash_and_scalar_helpers_follow_contract() {
        assert_eq!(
            extract_title("<title> A&nbsp;  🐟 </title>"),
            Some("A 🐟".to_string())
        );
        assert_eq!(
            content_hash_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" // pragma: allowlist secret
        );
        assert_eq!(truncate_scalars("a🐟b", 2), "a🐟");
        assert_eq!(scalar_count("a🐟b"), 3);
        let title = format!("<title>{}</title>", "🐟".repeat(501));
        assert_eq!(
            extract_title(&title)
                .expect("should extract title")
                .chars()
                .count(),
            500
        );
    }

    #[test]
    fn finds_only_structural_closing_tag() {
        let html = "<head><!-- </head> --><script>\"</head>\"</script></head>";
        assert_eq!(structural_close_tag(html, "head"), html.rfind("</head>"));
    }

    #[test]
    fn matches_javascript_whitespace_and_entity_order() {
        assert_eq!(normalize_js_whitespace("\u{feff}a\u{00a0}\u{2007}b"), "a b");
        assert_eq!(decode_html_entities("&amp;lt; &#65; &#x1F41F;"), "< A 🐟");
    }
}
