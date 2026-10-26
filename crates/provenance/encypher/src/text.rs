use sha2::{Digest as _, Sha256};

/// Elements whose content canonical text drops: edge-core's set in
/// `integrations/edge-core/fragments.js`.
const TEXT_IGNORED: [&str; 7] = ["script", "style", "noscript", "svg", "math", "code", "pre"];

/// Elements whose content browsers never parse as markup (raw text and RCDATA).
/// A `/>` spelling does not close them, and structural scans skip their content.
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

/// Ordered entity replacements after the initial `&amp;` pass.
const LATER_NAMED_ENTITIES: [(&str, char); 16] = [
    ("&lt;", '<'),
    ("&gt;", '>'),
    ("&quot;", '"'),
    ("&#039;", '\''),
    ("&apos;", '\''),
    ("&#x27;", '\''),
    ("&#x2F;", '/'),
    ("&nbsp;", '\u{00a0}'),
    ("&ndash;", '\u{2013}'),
    ("&mdash;", '\u{2014}'),
    ("&lsquo;", '\u{2018}'),
    ("&rsquo;", '\u{2019}'),
    ("&ldquo;", '\u{201c}'),
    ("&rdquo;", '\u{201d}'),
    ("&hellip;", '\u{2026}'),
    ("&bull;", '\u{2022}'),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TagKind {
    Opening,
    Closing,
}

/// Canonical extraction preserves edge-core's permissive `< whitespace name`
/// handling. Structural consumers follow HTML element-token starts so invalid
/// less-than text cannot hide or invent article metadata.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ScanMode {
    Canonical,
    Structural,
}

/// One structural tag outside comments and ignored or raw-text content.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StructuralTag<'a> {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) name: &'a str,
    pub(crate) kind: TagKind,
    token: &'a str,
}

/// Structural document facts collected in one pass before signing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DocumentMetadata {
    pub(crate) manifest_present: bool,
    pub(crate) head_close: Option<usize>,
    pub(crate) title_range: Option<(usize, usize)>,
}

impl<'a> StructuralTag<'a> {
    /// Returns the first value for an actual attribute on an opening tag.
    pub(crate) fn attribute(&self, expected: &str) -> Option<&'a str> {
        if self.kind != TagKind::Opening {
            return None;
        }
        let content = self
            .token
            .strip_prefix('<')?
            .trim_start_matches(is_js_whitespace);
        let mut attributes = content.get(self.name.len()..)?.strip_suffix('>')?;
        if attributes.ends_with('/') {
            attributes = attributes.get(..attributes.len().saturating_sub(1))?;
        }
        let bytes = attributes.as_bytes();
        let mut cursor = 0_usize;
        while cursor < bytes.len() {
            while bytes
                .get(cursor)
                .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'/')
            {
                cursor += 1;
            }
            if cursor >= bytes.len() {
                break;
            }

            let name_start = cursor;
            while bytes.get(cursor).is_some_and(|byte| {
                !byte.is_ascii_whitespace() && !matches!(*byte, b'/' | b'>' | b'=')
            }) {
                cursor += 1;
            }
            if cursor == name_start {
                cursor += 1;
                continue;
            }
            let name = attributes.get(name_start..cursor)?;
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }

            let mut value = "";
            if bytes.get(cursor) == Some(&b'=') {
                cursor += 1;
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
                if let Some(quote @ (b'"' | b'\'')) = bytes.get(cursor).copied() {
                    cursor += 1;
                    let value_start = cursor;
                    while bytes.get(cursor).is_some_and(|byte| *byte != quote) {
                        cursor += 1;
                    }
                    value = attributes.get(value_start..cursor)?;
                    if cursor < bytes.len() {
                        cursor += 1;
                    }
                } else {
                    let value_start = cursor;
                    while bytes
                        .get(cursor)
                        .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>')
                    {
                        cursor += 1;
                    }
                    value = attributes.get(value_start..cursor)?;
                }
            }

            if name.eq_ignore_ascii_case(expected) {
                return Some(value);
            }
        }
        None
    }
}

/// One canonical character and the source byte range that produced it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CanonicalChar {
    pub(crate) ch: char,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

#[derive(Debug, Clone, Copy)]
struct MappedChar {
    ch: char,
    start: usize,
    end: usize,
}

/// The first replacement pass as a lazy stream. It folds raw `&amp;` tokens
/// without materializing one mapping record per input character.
#[derive(Clone)]
struct AmpUnits<'a> {
    input: &'a str,
    cursor: usize,
}

impl<'a> AmpUnits<'a> {
    fn new(input: &'a str) -> Self {
        Self { input, cursor: 0 }
    }
}

impl Iterator for AmpUnits<'_> {
    type Item = MappedChar;

    fn next(&mut self) -> Option<Self::Item> {
        let tail = self.input.get(self.cursor..)?;
        let start = self.cursor;
        if tail.starts_with("&amp;") {
            self.cursor += "&amp;".len();
            return Some(MappedChar {
                ch: '&',
                start,
                end: self.cursor,
            });
        }
        let ch = tail.chars().next()?;
        self.cursor += ch.len_utf8();
        Some(MappedChar {
            ch,
            start,
            end: self.cursor,
        })
    }
}

/// Ordered named-entity passes after [`AmpUnits`].
#[derive(Clone)]
struct NamedUnits<'a> {
    units: AmpUnits<'a>,
}

impl Iterator for NamedUnits<'_> {
    type Item = MappedChar;

    fn next(&mut self) -> Option<Self::Item> {
        let first = self.units.next()?;
        if first.ch != '&' {
            return Some(first);
        }
        for (encoded, decoded) in LATER_NAMED_ENTITIES {
            let Some(suffix) = encoded.strip_prefix('&') else {
                continue;
            };
            if let Some((units, end)) = match_literal_suffix(&self.units, suffix) {
                self.units = units;
                return Some(MappedChar {
                    ch: decoded,
                    start: first.start,
                    end,
                });
            }
        }
        Some(first)
    }
}

/// Decimal numeric-entity pass after all named replacements.
#[derive(Clone)]
struct DecimalUnits<'a> {
    units: NamedUnits<'a>,
}

impl Iterator for DecimalUnits<'_> {
    type Item = MappedChar;

    fn next(&mut self) -> Option<Self::Item> {
        let first = self.units.next()?;
        if first.ch == '&'
            && let Some((units, decoded, end)) = match_numeric_suffix(&self.units, false)
        {
            self.units = units;
            return Some(MappedChar {
                ch: decoded,
                start: first.start,
                end,
            });
        }
        Some(first)
    }
}

/// Final hexadecimal numeric-entity pass over the decimal output.
struct DecodedChars<'a> {
    units: DecimalUnits<'a>,
}

impl<'a> DecodedChars<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            units: DecimalUnits {
                units: NamedUnits {
                    units: AmpUnits::new(input),
                },
            },
        }
    }
}

impl Iterator for DecodedChars<'_> {
    type Item = MappedChar;

    fn next(&mut self) -> Option<Self::Item> {
        let first = self.units.next()?;
        if first.ch == '&'
            && let Some((units, decoded, end)) = match_numeric_suffix(&self.units, true)
        {
            self.units = units;
            return Some(MappedChar {
                ch: decoded,
                start: first.start,
                end,
            });
        }
        Some(first)
    }
}

/// Visits canonical characters in order with their absolute source byte ranges.
/// Stops the shared HTML scan as soon as `visit` returns `false`.
pub(crate) fn visit_canonical_text(html: &str, mut visit: impl FnMut(CanonicalChar) -> bool) {
    let mut previous_end = None;
    scan_html(
        html,
        ScanMode::Canonical,
        is_canonical_ignored,
        |start, end| {
            let Some(raw) = html.get(start..end) else {
                return true;
            };
            visit_canonical_fragment(raw, start, &mut previous_end, &mut visit)
        },
        |_| true,
    );
}

/// Decodes and normalizes one scanner text run, joining it to the previous
/// nonempty run with the same synthetic space used by edge-core.
fn visit_canonical_fragment(
    raw: &str,
    offset: usize,
    previous_end: &mut Option<usize>,
    visit: &mut impl FnMut(CanonicalChar) -> bool,
) -> bool {
    let mut emitted_in_fragment = false;
    let mut pending_space: Option<MappedChar> = None;
    for item in DecodedChars::new(raw) {
        if is_js_whitespace(item.ch) {
            if emitted_in_fragment {
                if let Some(space) = pending_space.as_mut() {
                    space.end = item.end;
                } else {
                    pending_space = Some(MappedChar {
                        ch: ' ',
                        start: item.start,
                        end: item.end,
                    });
                }
            }
            continue;
        }

        if emitted_in_fragment {
            if let Some(space) = pending_space.take() {
                let mapped = CanonicalChar {
                    ch: ' ',
                    start: offset + space.start,
                    end: offset + space.end,
                };
                if !visit(mapped) {
                    return false;
                }
            }
        } else {
            if let Some(end) = *previous_end
                && !visit(CanonicalChar {
                    ch: ' ',
                    start: end,
                    end,
                })
            {
                return false;
            }
            emitted_in_fragment = true;
        }

        let mapped = CanonicalChar {
            ch: item.ch,
            start: offset + item.start,
            end: offset + item.end,
        };
        *previous_end = Some(mapped.end);
        if !visit(mapped) {
            return false;
        }
    }
    true
}

/// Returns at most `max_scalars` decoded, normalized visible characters and
/// the number returned.
#[must_use]
pub fn visible_text(html: &str, max_scalars: usize) -> (String, usize) {
    if max_scalars == 0 {
        return (String::new(), 0);
    }
    let mut output = String::new();
    let mut scalars = 0;
    visit_canonical_text(html, |item| {
        output.push(item.ch);
        scalars += 1;
        scalars < max_scalars
    });
    (output, scalars)
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

/// Collects the document facts needed after article selection. The scan stops
/// as soon as a manifest is found because such documents are rejected.
#[must_use]
pub(crate) fn document_metadata(html: &str) -> DocumentMetadata {
    let mut metadata = DocumentMetadata::default();
    let mut title_start = None;
    scan_structural_tags(html, |tag| {
        if tag.kind == TagKind::Opening
            && tag.name.eq_ignore_ascii_case("meta")
            && tag
                .attribute("name")
                .is_some_and(|value| value.eq_ignore_ascii_case("c2pa-manifest-url"))
        {
            metadata.manifest_present = true;
            return false;
        }
        if metadata.head_close.is_none()
            && tag.kind == TagKind::Closing
            && tag.name.eq_ignore_ascii_case("head")
        {
            metadata.head_close = Some(tag.start);
        }
        if metadata.title_range.is_none() && tag.name.eq_ignore_ascii_case("title") {
            match tag.kind {
                TagKind::Opening if title_start.is_none() => title_start = Some(tag.end),
                TagKind::Closing => {
                    if let Some(start) = title_start.take() {
                        metadata.title_range = Some((start, tag.start));
                    }
                }
                TagKind::Opening => {}
            }
        }
        true
    });
    metadata
}

/// Decodes and normalizes a saved structural title range, capped at 500 scalars.
#[must_use]
pub(crate) fn title_from_range(html: &str, title_range: Option<(usize, usize)>) -> Option<String> {
    let (start, end) = title_range?;
    let raw = html.get(start..end)?;
    let mut title = String::new();
    let mut scalars = 0;
    let mut previous_end = None;
    visit_canonical_fragment(raw, start, &mut previous_end, &mut |item| {
        title.push(item.ch);
        scalars += 1;
        scalars < 500
    });
    Some(title)
}

/// Visits tags outside comments, ignored elements, and raw-text or RCDATA
/// elements. Tag tokens and all offsets borrow the original HTML.
pub(crate) fn scan_structural_tags<'a>(html: &'a str, tag: impl FnMut(StructuralTag<'a>) -> bool) {
    scan_html(
        html,
        ScanMode::Structural,
        |name| contains_tag(&TEXT_IGNORED, name) || contains_tag(&RAW_TEXT, name),
        |_, _| true,
        tag,
    );
}

/// Scans tags, comments and text runs with shared comment, raw-content and
/// quoted-attribute handling.
fn scan_html<'a>(
    html: &'a str,
    mode: ScanMode,
    ignores: impl Fn(&str) -> bool,
    mut text: impl FnMut(usize, usize) -> bool,
    mut tag: impl FnMut(StructuralTag<'a>) -> bool,
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
            if name.eq_ignore_ascii_case("plaintext") {
                continue;
            }
            let Some(name_end) = raw_close_name_end(bytes, open, name) else {
                continue;
            };
            let Some(end) = tag_end(bytes, name_end) else {
                break;
            };
            let Some(token) = html.get(open..=end) else {
                break;
            };
            let Some(parsed) = structural_tag(open, token, mode) else {
                continue;
            };
            if parsed.kind != TagKind::Closing || !parsed.name.eq_ignore_ascii_case(name) {
                continue;
            }
            // As in browsers, attributes and `/` do not stop an end tag from
            // closing. Unlike edge-core's depth count, no other end tag closes.
            if !tag(parsed) {
                return;
            }
            ignored = None;
            cursor = end + 1;
            continue;
        }
        if starts_with_at(bytes, cursor, b"<!--") {
            let Some(end) = comment_end(bytes, cursor) else {
                break;
            };
            cursor = end;
        } else if mode == ScanMode::Structural
            && bytes[cursor] == b'<'
            && bytes
                .get(cursor + 1)
                .is_some_and(|byte| matches!(byte, b'!' | b'?'))
        {
            let Some(end) = find_byte(bytes, b'>', cursor + 2) else {
                break;
            };
            cursor = end + 1;
        } else if bytes[cursor] == b'<' {
            if mode == ScanMode::Structural && !is_structural_element_start(bytes, cursor) {
                cursor += 1;
                continue;
            }
            let Some(end) = tag_end(bytes, cursor + 1) else {
                break;
            };
            let Some(token) = html.get(cursor..=end) else {
                break;
            };
            if let Some(parsed) = structural_tag(cursor, token, mode) {
                if !tag(parsed) {
                    return;
                }
                if parsed.kind == TagKind::Opening
                    && ignores(parsed.name)
                    // As in edge-core, `/>` makes an element empty, except raw-text
                    // elements, whose content browsers never parse as markup.
                    && (!is_self_closing(token) || contains_tag(&RAW_TEXT, parsed.name))
                {
                    ignored = Some(parsed.name);
                }
            }
            cursor = end + 1;
        } else {
            let end = find_byte(bytes, b'<', cursor).unwrap_or(bytes.len());
            if !text(cursor, end) {
                return;
            }
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

fn match_literal_suffix<I>(units: &I, expected: &str) -> Option<(I, usize)>
where
    I: Clone + Iterator<Item = MappedChar>,
{
    let mut trial = units.clone();
    let mut end = None;
    for expected_char in expected.chars() {
        let item = trial.next()?;
        if item.ch != expected_char {
            return None;
        }
        end = Some(item.end);
    }
    Some((trial, end?))
}

fn match_numeric_suffix<I>(units: &I, hexadecimal: bool) -> Option<(I, char, usize)>
where
    I: Clone + Iterator<Item = MappedChar>,
{
    let mut trial = units.clone();
    consume_expected(&mut trial, '#')?;
    if hexadecimal {
        consume_expected(&mut trial, 'x')?;
    }

    let radix = if hexadecimal { 16 } else { 10 };
    let mut value = 0_u32;
    let mut has_digits = false;
    loop {
        let item = trial.next()?;
        let digit = if (hexadecimal && item.ch.is_ascii_hexdigit())
            || (!hexadecimal && item.ch.is_ascii_digit())
        {
            item.ch.to_digit(radix)
        } else {
            None
        };
        if let Some(digit) = digit {
            has_digits = true;
            value = value.checked_mul(radix)?.checked_add(digit)?;
        } else if has_digits && item.ch == ';' {
            return Some((trial, char::from_u32(value)?, item.end));
        } else {
            return None;
        }
    }
}

fn consume_expected(units: &mut impl Iterator<Item = MappedChar>, expected: char) -> Option<()> {
    (units.next()?.ch == expected).then_some(())
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

fn structural_tag<'a>(start: usize, token: &'a str, mode: ScanMode) -> Option<StructuralTag<'a>> {
    let content = token.strip_prefix('<')?;
    let (kind, content) = if let Some(content) = content.strip_prefix('/') {
        (TagKind::Closing, content)
    } else {
        let content = match mode {
            ScanMode::Canonical => content.trim_start_matches(is_js_whitespace),
            ScanMode::Structural => content,
        };
        (TagKind::Opening, content)
    };
    let name = take_tag_name(content)?;
    Some(StructuralTag {
        start,
        end: start.checked_add(token.len())?,
        name,
        kind,
        token,
    })
}

fn is_structural_element_start(bytes: &[u8], start: usize) -> bool {
    match bytes.get(start + 1).copied() {
        Some(byte) if byte.is_ascii_alphabetic() => true,
        Some(b'/') => bytes.get(start + 2).is_some_and(u8::is_ascii_alphabetic),
        _ => false,
    }
}

fn raw_close_name_end(bytes: &[u8], start: usize, expected: &str) -> Option<usize> {
    if !starts_with_at(bytes, start, b"</") {
        return None;
    }
    let name_start = start.checked_add(2)?;
    let name_end = name_start.checked_add(expected.len())?;
    let actual = bytes.get(name_start..name_end)?;
    if !actual.eq_ignore_ascii_case(expected.as_bytes()) {
        return None;
    }
    bytes
        .get(name_end)
        .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>'))
        .then_some(name_end)
}

fn take_tag_name(content: &str) -> Option<&str> {
    let end = content
        .char_indices()
        .find(|(_, ch)| ch.is_ascii_whitespace() || matches!(*ch, '/' | '>'))
        .map_or(content.len(), |(index, _)| index);
    (end > 0).then(|| content.get(..end)).flatten()
}

fn is_canonical_ignored(name: &str) -> bool {
    TEXT_IGNORED.iter().any(|expected| {
        let Some(prefix) = name.get(..expected.len()) else {
            return false;
        };
        prefix.eq_ignore_ascii_case(expected)
            && name
                .as_bytes()
                .get(expected.len())
                .is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_')
    })
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

    fn canonical_text(html: &str) -> String {
        visible_text(html, usize::MAX).0
    }

    fn title(html: &str) -> Option<String> {
        let metadata = document_metadata(html);
        title_from_range(html, metadata.title_range)
    }

    #[test]
    fn quoted_markup_and_unmatched_script_tags_stay_inert() {
        for html in [
            r#"<head><meta data-x="></head><img src=x onerror=alert(1)>"></head><article>Visible</article>"#,
            r#"<head><script>const x = "</bogus><article>hidden</article></head>";</script></head><article>Visible</article>"#,
        ] {
            assert_eq!(document_metadata(html).head_close, html.rfind("</head>"));
            assert_eq!(canonical_text(html), "Visible");
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
            assert_eq!(document_metadata(&html).head_close, Some(prefix.len()));
        }
        assert_eq!(
            canonical_text("<code>a</code/>b<!--->c<!-- d --!>e<style>f</style\tx>g"),
            "b c e g"
        );
    }

    #[test]
    fn canonical_ignored_prefixes_follow_ascii_word_boundaries() {
        let html = concat!(
            "<code-snippet>hidden code</code-snippet>",
            "<pre-example>hidden pre</pre-example>",
            "<svg-icon>hidden svg</svg-icon>",
            "<code_snippet>kept underscore</code_snippet>",
            "<preformatted>kept letters</preformatted>"
        );
        assert_eq!(canonical_text(html), "kept underscore kept letters");
    }

    #[test]
    fn raw_text_decoy_end_tags_do_not_hide_the_real_close() {
        let decoys = "</x ".repeat(10_000);
        let html = format!(
            "<script>{decoys}</scripted><article>Hidden.</article></ScRiPt data-x>\
             <article>Real.</article>"
        );

        assert_eq!(canonical_text(&html), "Real.");
        let mut articles = 0;
        scan_structural_tags(&html, |tag| {
            if tag.kind == TagKind::Opening && tag.name.eq_ignore_ascii_case("article") {
                articles += 1;
            }
            true
        });
        assert_eq!(articles, 1);
    }

    #[test]
    fn canonical_visitor_stops_when_the_consumer_is_satisfied() {
        let html = format!("a{}", "&amp;".repeat(100_000));
        let mut visited = String::new();
        visit_canonical_text(&html, |item| {
            visited.push(item.ch);
            false
        });
        assert_eq!(visited, "a");
    }

    #[test]
    fn extracts_decodes_and_normalizes_visible_text() {
        let html = "<article> A&nbsp;B <em>&amp;</em><script>hidden</script> &#x43; </article>";
        assert_eq!(canonical_text(html), "A B & C");
    }

    #[test]
    fn zero_scalar_limit_does_not_emit_text() {
        assert_eq!(visible_text("visible text", 0), (String::new(), 0));
    }

    #[test]
    fn title_and_hash_helpers_follow_contract() {
        assert_eq!(
            title("<title> A&nbsp;  🐟 </title>"),
            Some("A 🐟".to_string())
        );
        assert_eq!(
            content_hash_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" // pragma: allowlist secret
        );
        let title_html = format!("<title>{}</title>", "🐟".repeat(501));
        assert_eq!(
            title(&title_html)
                .expect("should extract title")
                .chars()
                .count(),
            500
        );
    }

    #[test]
    fn title_cap_preserves_ordered_entities_and_exact_whitespace_normalization() {
        let canonical = format!("A<{}", "x".repeat(498));
        let html = format!(
            "<title> \tA&amp;lt;{}   {} </title>",
            "x".repeat(498),
            "&amp;#65;".repeat(100_000)
        );
        let title = title(&html).expect("should extract title");
        assert_eq!(title, canonical);
        assert_eq!(title.chars().count(), 500);
    }

    #[test]
    fn title_extraction_ignores_inert_markup() {
        let real = "<title>Real &amp;lt; title</title>";
        for inert in [
            "<!-- <title>Comment title</title> -->",
            r#"<script>const title = "<title>Script title</title>";</script>"#,
            r#"<div data-template="<title>Attribute title</title>"></div>"#,
        ] {
            let html = format!("{inert}{real}");
            assert_eq!(
                title(&html),
                Some("Real < title".to_owned()),
                "inert markup must not supply the title: {inert}"
            );
        }
    }

    #[test]
    fn finds_only_structural_closing_tag() {
        let html = "<head><!-- </head> --><script>\"</head>\"</script></head>";
        assert_eq!(document_metadata(html).head_close, html.rfind("</head>"));
    }

    #[test]
    fn invalid_less_than_and_declarations_do_not_hide_structural_metadata() {
        let html = concat!(
            "<head>text < y ",
            "<!bogus \"quoted > remainder\">",
            "<?title><title>Real title</title>",
            "</ head></head><body></body>"
        );
        let metadata = document_metadata(html);
        assert_eq!(
            title_from_range(html, metadata.title_range),
            Some("Real title".to_owned())
        );
        assert_eq!(metadata.head_close, html.rfind("</head>"));
    }

    #[test]
    fn matches_javascript_whitespace_and_entity_order() {
        assert_eq!(canonical_text("\u{feff}a\u{00a0}\u{2007}b"), "a b");
        assert_eq!(canonical_text("&amp;lt; &#65; &#x1F41F;"), "< A 🐟");
        assert_eq!(canonical_text("&amp;lt; &amp;#65; &#38;lt;"), "< A &lt;");
        assert_eq!(canonical_text("&amp;amp;"), "&amp;");
        assert_eq!(
            canonical_text("&#38;#x41; &#4294967296; &#x110000; &#xD800; &#; &#x; &bogus;"),
            "A &#4294967296; &#x110000; &#xD800; &#; &#x; &bogus;"
        );
        assert_eq!(
            canonical_text("&&#35;x41; &#38;&#35;&#120;&#52;&#49;&#59;"),
            "A A"
        );
    }

    #[test]
    fn large_plain_and_entity_runs_decode_without_a_per_character_map() {
        let plain = "a".repeat(5_000_000);
        assert_eq!(DecodedChars::new(&plain).count(), plain.len());

        let entities = "&amp;lt;".repeat(100_000);
        let (count, last_end) =
            DecodedChars::new(&entities).fold((0_usize, 0_usize), |(count, _), item| {
                assert_eq!(item.ch, '<');
                (count + 1, item.end)
            });
        assert_eq!(count, 100_000);
        assert_eq!(last_end, entities.len());
    }
}
