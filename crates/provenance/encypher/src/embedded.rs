//! Media embedded in a document: `data:` URIs in HTML attributes, `srcset`
//! candidates, inline styles and stylesheets, and inline `<svg>` elements.
//!
//! Scanning is pure. It finds the exact byte range of each media `data:`
//! URI the module can rewrite without changing anything else, and the reason
//! for each one it cannot: an inline style a page security policy admits by
//! hash, or whose policies the module cannot see, a subresource integrity
//! value on the element, or an escape the module would have to decode. A
//! refused URI stays as written and is reported as unsigned. Inline SVG is
//! signed as the standalone SVG document
//! [`signable_svg`](crate::svg_equivalence::signable_svg) proves equivalent
//! to what the browser parsed, and only kept when the signer's writer added
//! its manifest carrier and changed nothing else.

use crate::data_uri::DataUri;
use crate::processor::Reason;
use crate::text::{
    comment_end, find_byte, find_bytes, is_structural_element_start, raw_close_name_end,
    starts_with_at, tag_end, take_tag_name,
};

/// Elements whose content the HTML tokenizer never reads as markup. Unlike
/// the text path, `noscript` content is scanned as markup, because readers
/// without scripts see the images inside it.
const RAW_TEXT: [&str; 9] = [
    "script",
    "style",
    "title",
    "textarea",
    "xmp",
    "iframe",
    "noembed",
    "noframes",
    "plaintext",
];

/// Where a found `data:` URI sits, which decides what may refuse its edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Site {
    /// A URL attribute or a `srcset` candidate.
    Attribute,
    /// A `style` attribute, which a page policy may admit by hash.
    StyleAttribute,
    /// A `<style>` element, which a page policy may admit by hash, or by the
    /// nonce the element carries, at `nonce`, whatever its text. Inside an
    /// inline `<svg>` (`foreign`) its text is markup, so character
    /// references in it are decoded.
    StyleElement {
        nonce: Option<(usize, usize)>,
        foreign: bool,
    },
    /// A stylesheet served as its own document.
    Stylesheet,
}

/// One media `data:` URI the module may rewrite: its exact byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// What a document scan found.
#[derive(Debug, Default)]
pub(crate) struct Scan {
    /// URIs to sign, in document order, never overlapping.
    pub(crate) found: Vec<Found>,
    /// Media URIs left as written, each with why.
    pub(crate) refused: Vec<Reason>,
    /// Editable outermost inline `<svg>` elements, as byte ranges, in
    /// document order. Original provenance-bearing ranges are excluded.
    pub(crate) svgs: Vec<(usize, usize)>,
    /// Original ranges protected from core and every middleware, including
    /// descendants, using the same classification as the signing scan.
    pub(crate) protected: Vec<(usize, usize)>,
}

impl Scan {
    /// Whether the document embeds no media at all.
    pub(crate) fn is_empty(&self) -> bool {
        self.found.is_empty() && self.refused.is_empty() && self.svgs.is_empty()
    }
}

/// One attribute of a start tag, with its value's absolute byte range.
struct Attribute<'a> {
    name: &'a str,
    value: Option<(usize, usize)>,
}

/// The attributes of the start tag `token`, which begins at `offset`.
fn attributes(token: &str, offset: usize) -> Vec<Attribute<'_>> {
    let bytes = token.as_bytes();
    let mut attributes = Vec::new();
    let Some(name) = token.get(1..).and_then(take_tag_name) else {
        return attributes;
    };
    let mut cursor = 1 + name.len();
    let end = bytes.len().saturating_sub(1);
    while cursor < end {
        while cursor < end
            && bytes
                .get(cursor)
                .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'/')
        {
            cursor += 1;
        }
        let name_start = cursor;
        while cursor < end
            && bytes.get(cursor).is_some_and(|byte| {
                !byte.is_ascii_whitespace() && !matches!(*byte, b'/' | b'>' | b'=')
            })
        {
            cursor += 1;
        }
        if cursor == name_start {
            cursor += 1;
            continue;
        }
        let Some(name) = token.get(name_start..cursor) else {
            break;
        };
        while cursor < end && bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        let mut value = None;
        if bytes.get(cursor) == Some(&b'=') && cursor < end {
            cursor += 1;
            while cursor < end && bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            if let Some(quote @ (b'"' | b'\'')) = bytes.get(cursor).copied() {
                cursor += 1;
                let value_start = cursor;
                while cursor < end && bytes.get(cursor).is_some_and(|byte| *byte != quote) {
                    cursor += 1;
                }
                value = Some((offset + value_start, offset + cursor));
                cursor += 1;
            } else {
                let value_start = cursor;
                while cursor < end
                    && bytes
                        .get(cursor)
                        .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>')
                {
                    cursor += 1;
                }
                value = Some((offset + value_start, offset + cursor));
            }
        }
        attributes.push(Attribute { name, value });
    }
    attributes
}

fn is_raw_text(name: &str) -> bool {
    RAW_TEXT
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// The range of `value` without leading and trailing ASCII whitespace,
/// which a URL attribute ignores.
fn trimmed(document: &str, (start, end): (usize, usize)) -> (usize, usize) {
    let value = document.get(start..end).unwrap_or_default();
    let leading = value.len()
        - value
            .trim_start_matches(|ch: char| ch.is_ascii_whitespace())
            .len();
    let trailing = value.len()
        - value
            .trim_end_matches(|ch: char| ch.is_ascii_whitespace())
            .len();
    (
        start + leading,
        end.saturating_sub(trailing).max(start + leading),
    )
}

/// What a scan collects before the page's policies are applied.
struct Collector<'a> {
    document: &'a str,
    scan: Scan,
    /// Inline style URIs, kept only if every policy leaves them editable.
    inline: Vec<(Found, Site)>,
    /// `Content-Security-Policy` values of `<meta http-equiv>` elements.
    meta_policies: Vec<&'a str>,
    /// A `<meta>` policy written in a way the module does not read.
    unreadable_meta_policy: bool,
}

impl<'a> Collector<'a> {
    fn new(document: &'a str) -> Self {
        Self {
            document,
            scan: Scan::default(),
            inline: Vec::new(),
            meta_policies: Vec::new(),
            unreadable_meta_policy: false,
        }
    }

    /// Keep the URI at `range` when it is media the module can rewrite in
    /// place, or report why it is left as written.
    fn consider(&mut self, (start, end): (usize, usize), site: Site, integrity: bool) {
        let Some(text) = self.document.get(start..end) else {
            return;
        };
        if DataUri::parse(text).is_none() {
            return;
        }
        let refusal = if integrity {
            Some(Reason::IntegrityAttribute)
        } else if text.contains('\\') && site != Site::Attribute {
            Some(Reason::CssEscape)
        } else if text.contains('&')
            && matches!(
                site,
                Site::Attribute | Site::StyleAttribute | Site::StyleElement { foreign: true, .. }
            )
        {
            // A character reference the module would have to decode, and
            // could not write back as it was.
            Some(Reason::CharacterReference)
        } else {
            None
        };
        let found = Found { start, end };
        match (refusal, site) {
            (Some(reason), _) => self.scan.refused.push(reason),
            (None, Site::StyleAttribute | Site::StyleElement { .. }) => {
                self.inline.push((found, site));
            }
            (None, _) => self.scan.found.push(found),
        }
    }

    /// Each `srcset` candidate URL in the value at `range`.
    fn srcset(&mut self, (start, end): (usize, usize), integrity: bool) {
        let bytes = self.document.as_bytes();
        let mut cursor = start;
        while cursor < end {
            while cursor < end
                && bytes
                    .get(cursor)
                    .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b',')
            {
                cursor += 1;
            }
            let url_start = cursor;
            while cursor < end
                && bytes
                    .get(cursor)
                    .is_some_and(|byte| !byte.is_ascii_whitespace())
            {
                cursor += 1;
            }
            let mut url_end = cursor;
            while url_end > url_start && bytes.get(url_end - 1) == Some(&b',') {
                url_end -= 1;
            }
            if url_end > url_start {
                self.consider((url_start, url_end), Site::Attribute, integrity);
            }
            if url_end < cursor {
                continue;
            }
            // The descriptors run to the next comma.
            while cursor < end && bytes.get(cursor).is_some_and(|byte| *byte != b',') {
                cursor += 1;
            }
        }
    }

    /// One start tag's media URIs.
    fn tag(&mut self, name: &str, token: &'a str, offset: usize) {
        let attributes = attributes(token, offset);
        let integrity = attributes
            .iter()
            .any(|attribute| attribute.name.eq_ignore_ascii_case("integrity"));
        if name.eq_ignore_ascii_case("meta") {
            self.meta_policy(&attributes);
        }
        for attribute in &attributes {
            let Some(range) = attribute.value else {
                continue;
            };
            if attribute.name.eq_ignore_ascii_case("style") {
                self.css(range, Site::StyleAttribute);
            } else if attribute
                .name
                .get(attribute.name.len().saturating_sub(6)..)
                .is_some_and(|suffix| suffix.eq_ignore_ascii_case("srcset"))
            {
                self.srcset(range, integrity);
            } else if is_url_attribute(attribute.name) {
                let range = trimmed(self.document, range);
                let starts_data = self
                    .document
                    .get(range.0..range.0.saturating_add(5).min(range.1))
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"));
                if starts_data {
                    self.consider(range, Site::Attribute, integrity);
                }
            }
        }
    }

    /// Keeps the policy of a `<meta http-equiv="Content-Security-Policy">`.
    fn meta_policy(&mut self, attributes: &[Attribute<'_>]) {
        let document = self.document;
        let value = |expected: &str| {
            attributes
                .iter()
                .find(|attribute| attribute.name.eq_ignore_ascii_case(expected))
                .and_then(|attribute| attribute.value)
                .and_then(|(start, end)| document.get(start..end))
        };
        let is_policy = value("http-equiv")
            .is_some_and(|name| name.trim().eq_ignore_ascii_case("content-security-policy"));
        if !is_policy {
            return;
        }
        match value("content") {
            Some(policy) if !policy.contains('&') => self.meta_policies.push(policy),
            _ => self.unreadable_meta_policy = true,
        }
    }

    /// The media URLs of the CSS at `range`.
    fn css(&mut self, range: (usize, usize), site: Site) {
        for uri in css_urls(self.document, range) {
            self.consider(uri, site, false);
        }
    }

    /// Classify the original outer SVG before returning any descendant edit.
    /// All media collected since its start belongs to this range. A protected
    /// or incomplete SVG is kept byte-for-byte, including root attributes,
    /// CSS and nested SVGs; it is not submitted for a new whole-asset signature.
    /// Returns whether the range is editable; only an explicit outer closing
    /// tag makes it eligible for whole-SVG signing.
    fn finish_svg(
        &mut self,
        start: usize,
        end: usize,
        complete: bool,
        (found, inline, refused): (usize, usize, usize),
    ) -> bool {
        let refusal = self
            .document
            .get(start..end)
            .map_or(
                Some(Reason::InlineSvgNotEquivalent),
                crate::svg_equivalence::descendant_edit_refusal,
            )
            .or_else(|| (!complete).then_some(Reason::InlineSvgNotEquivalent));
        if let Some(reason) = refusal {
            self.scan.found.truncate(found);
            self.inline.truncate(inline);
            self.scan.refused.truncate(refused);
            self.scan.refused.push(reason);
            self.scan.protected.push((start, end));
            false
        } else {
            true
        }
    }

    /// The scan, with each inline style URI kept only where `policies` (the
    /// response's policy header values, `None` when unknown) and the page's
    /// `<meta>` policies leave its style editable.
    fn finish(self, policies: Option<&[String]>) -> Scan {
        let Self {
            document,
            mut scan,
            inline,
            meta_policies,
            unreadable_meta_policy,
        } = self;
        let all: Vec<&str> = policies
            .unwrap_or_default()
            .iter()
            .map(String::as_str)
            .chain(meta_policies)
            .flat_map(|value| value.split(','))
            .collect();
        for (found, site) in inline {
            let refusal = if policies.is_none() || unreadable_meta_policy {
                Some(Reason::CspUnknown)
            } else {
                let locked = match site {
                    Site::StyleElement { nonce, .. } => {
                        let nonce = nonce.and_then(|(start, end)| document.get(start..end));
                        locks(&all, &["style-src-elem", "style-src", "default-src"], nonce)
                    }
                    _ => locks(&all, &["style-src-attr", "style-src", "default-src"], None),
                };
                locked.then_some(Reason::CspStyleHash)
            };
            match refusal {
                Some(reason) => scan.refused.push(reason),
                None => scan.found.push(found),
            }
        }
        scan.found.sort_by_key(|found| found.start);
        scan
    }
}

/// Whether any of `policies` admits inline styles, under the first directive
/// of `order` it names, by hash, so an edit would block the style. The
/// element's `nonce` value admits it whatever its text where the directive
/// lists that exact nonce; another nonce does not.
fn locks(policies: &[&str], order: &[&str], nonce: Option<&str>) -> bool {
    policies.iter().any(|policy| {
        let directives: Vec<(&str, &str)> = policy
            .split(';')
            .map(|directive| {
                let directive = directive.trim();
                directive
                    .split_once(|ch: char| ch.is_ascii_whitespace())
                    .unwrap_or((directive, ""))
            })
            .collect();
        let Some(sources) = order.iter().find_map(|wanted| {
            directives
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
                .map(|(_, sources)| *sources)
        }) else {
            return false;
        };
        let hashed = sources.split_ascii_whitespace().any(|source| {
            ["'sha256-", "'sha384-", "'sha512-"].iter().any(|prefix| {
                source
                    .get(..prefix.len())
                    .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
            })
        });
        // The `'nonce-` keyword is case-insensitive, its value is not.
        let nonced = nonce.is_some_and(|value| {
            !value.is_empty()
                && sources.split_ascii_whitespace().any(|source| {
                    source
                        .get(..7)
                        .is_some_and(|start| start.eq_ignore_ascii_case("'nonce-"))
                        && source.get(7..).and_then(|rest| rest.strip_suffix('\'')) == Some(value)
                })
        });
        hashed && !nonced
    })
}

/// Whether the attribute `name` holds one URL a browser loads or links:
/// the standard URL attributes and the lazy-loading `data-*` ones that
/// scripts copy into them. A `data:` value in any other attribute, such as
/// `title`, `value` or `content`, is text and is left alone.
fn is_url_attribute(name: &str) -> bool {
    [
        "src",
        "href",
        "xlink:href",
        "poster",
        "data",
        "background",
        "data-src",
        "data-lazy-src",
        "data-original",
        "data-bg",
        "data-background",
        "data-poster",
        "data-href",
    ]
    .iter()
    .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// The `Content-Security-Policy` and `-Report-Only` values of `headers`, as
/// [`scan_html`] reads them, or `None` when a value is not text and so
/// cannot be read.
pub(crate) fn content_security_policies(headers: &http::HeaderMap) -> Option<Vec<String>> {
    let mut policies = Vec::new();
    for name in [
        "content-security-policy",
        "content-security-policy-report-only",
    ] {
        for value in headers.get_all(name) {
            policies.push(value.to_str().ok()?.to_owned());
        }
    }
    Some(policies)
}

/// Finds media in an HTML document. Inline style media is kept only when
/// `policies` (the response's policy header values, `None` when the module
/// cannot see them) and the page's `<meta>` policies admit no inline style
/// by hash, as an edit would block it. Original provenance-bearing or
/// structurally uncertain SVG ranges admit no edits, including descendants.
pub(crate) fn scan_html(document: &str, policies: Option<&[String]>) -> Scan {
    let bytes = document.as_bytes();
    let mut collector = Collector::new(document);
    let mut svg_depth = 0_usize;
    let mut svg_start = 0_usize;
    let mut svg_checkpoint = (0, 0, 0);
    let mut cursor = 0_usize;
    while cursor < bytes.len() {
        if starts_with_at(bytes, cursor, b"<!--") {
            cursor = comment_end(bytes, cursor).unwrap_or(bytes.len());
            continue;
        }
        if starts_with_at(bytes, cursor, b"<![CDATA[") {
            cursor = find_bytes(bytes, b"]]>", cursor + 9).map_or(bytes.len(), |end| end + 3);
            continue;
        }
        if bytes.get(cursor) == Some(&b'<')
            && bytes
                .get(cursor + 1)
                .is_some_and(|byte| matches!(byte, b'!' | b'?'))
        {
            cursor = find_byte(bytes, b'>', cursor + 2).map_or(bytes.len(), |end| end + 1);
            continue;
        }
        if bytes.get(cursor) != Some(&b'<') || !is_structural_element_start(bytes, cursor) {
            cursor = find_byte(bytes, b'<', cursor + 1).unwrap_or(bytes.len());
            continue;
        }
        let Some(end) = tag_end(bytes, cursor + 1) else {
            break;
        };
        let Some(token) = document.get(cursor..=end) else {
            break;
        };
        let closing = bytes.get(cursor + 1) == Some(&b'/');
        let name_from = if closing { 2 } else { 1 };
        let Some(name) = token.get(name_from..).and_then(take_tag_name) else {
            cursor = end + 1;
            continue;
        };
        let self_closing = token
            .strip_suffix('>')
            .is_some_and(|body| body.trim_end().ends_with('/'));
        if name.eq_ignore_ascii_case("svg") && !closing && svg_depth == 0 {
            svg_start = cursor;
            svg_checkpoint = (
                collector.scan.found.len(),
                collector.inline.len(),
                collector.scan.refused.len(),
            );
            if self_closing {
                collector.tag(name, token, cursor);
                collector.finish_svg(svg_start, end + 1, true, svg_checkpoint);
                cursor = end + 1;
                continue;
            }
        }
        if name.eq_ignore_ascii_case("svg") {
            if closing {
                if svg_depth == 1 && collector.finish_svg(svg_start, end + 1, true, svg_checkpoint)
                {
                    collector.scan.svgs.push((svg_start, end + 1));
                }
                svg_depth = svg_depth.saturating_sub(1);
            } else if !self_closing {
                svg_depth += 1;
            }
        }
        if closing {
            cursor = end + 1;
            continue;
        }
        collector.tag(name, token, cursor);
        // Inside an inline `<svg>` the tokenizer reads `<script>`, `<title>`
        // and the rest as markup; only `<style>` keeps its text to the end
        // tag here, scanned as CSS whose character references are decoded.
        let foreign = svg_depth > 0;
        let style = name.eq_ignore_ascii_case("style");
        if !is_raw_text(name) || (foreign && !style) {
            cursor = end + 1;
            continue;
        }
        let content_start = end + 1;
        let (content_end, after) = raw_content_end(bytes, content_start, name);
        if style {
            let nonce = attributes(token, cursor)
                .into_iter()
                .find(|attribute| attribute.name.eq_ignore_ascii_case("nonce"))
                .and_then(|attribute| attribute.value);
            collector.css(
                (content_start, content_end),
                Site::StyleElement { nonce, foreign },
            );
        }
        cursor = after;
    }
    if svg_depth > 0 {
        collector.finish_svg(svg_start, document.len(), false, svg_checkpoint);
    }
    collector.finish(policies)
}

/// Original inline SVG ranges core must hold opaque before rewriting HTML.
/// Reuse the signing scan, including its incomplete/uncertain SVG refusals.
pub(crate) fn source_protection(
    original: &[u8],
) -> Result<Vec<std::ops::Range<usize>>, std::io::Error> {
    let document = std::str::from_utf8(original)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    Ok(scan_html(document, None)
        .protected
        .into_iter()
        .map(|(start, end)| start..end)
        .collect())
}

/// Where a raw-text element's content ends, and where scanning resumes.
fn raw_content_end(bytes: &[u8], start: usize, name: &str) -> (usize, usize) {
    let mut cursor = start;
    while let Some(open) = find_byte(bytes, b'<', cursor) {
        if let Some(name_end) = raw_close_name_end(bytes, open, name)
            && let Some(close) = tag_end(bytes, name_end)
        {
            return (open, close + 1);
        }
        cursor = open + 1;
    }
    (bytes.len(), bytes.len())
}

/// Finds media in a stylesheet served as its own document.
pub(crate) fn scan_stylesheet(document: &str) -> Scan {
    let mut collector = Collector::new(document);
    collector.css((0, document.len()), Site::Stylesheet);
    collector.scan
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') || byte >= 0x80
}

/// The URL ranges in the CSS at `range`: each `url()` and each string
/// argument of `image-set()`, outside comments, as written, escapes
/// included.
fn css_urls(document: &str, (start, end): (usize, usize)) -> Vec<(usize, usize)> {
    let bytes = document.as_bytes();
    let mut urls = Vec::new();
    let mut functions: Vec<&str> = Vec::new();
    let mut cursor = start;
    while cursor < end {
        let Some(&byte) = bytes.get(cursor) else {
            break;
        };
        if byte == b'/' && bytes.get(cursor + 1) == Some(&b'*') {
            cursor = find_bytes(bytes, b"*/", cursor + 2).map_or(end, |close| close + 2);
            continue;
        }
        if byte == b'"' || byte == b'\'' {
            let (value_end, after) = css_string(bytes, cursor + 1, end, byte);
            let in_url = functions.last().is_some_and(|name| {
                name.eq_ignore_ascii_case("url")
                    || name.eq_ignore_ascii_case("image-set")
                    || name.eq_ignore_ascii_case("-webkit-image-set")
            });
            if in_url {
                urls.push((cursor + 1, value_end));
            }
            cursor = after;
            continue;
        }
        if byte == b'(' {
            functions.push("");
            cursor += 1;
            continue;
        }
        if byte == b')' {
            functions.pop();
            cursor += 1;
            continue;
        }
        if !is_ident_byte(byte) {
            cursor += 1;
            continue;
        }
        let ident_start = cursor;
        while cursor < end && bytes.get(cursor).copied().is_some_and(is_ident_byte) {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'(') {
            continue;
        }
        let name = document.get(ident_start..cursor).unwrap_or_default();
        cursor += 1;
        if !name.eq_ignore_ascii_case("url") {
            functions.push(name);
            continue;
        }
        let mut value_start = cursor;
        while value_start < end && bytes.get(value_start).is_some_and(u8::is_ascii_whitespace) {
            value_start += 1;
        }
        if matches!(bytes.get(value_start), Some(b'"' | b'\'')) {
            functions.push(name);
            cursor = value_start;
            continue;
        }
        let mut value_end = value_start;
        while value_end < end
            && bytes
                .get(value_end)
                .is_some_and(|byte| *byte != b')' && !byte.is_ascii_whitespace())
        {
            value_end += 1;
        }
        if value_end > value_start {
            urls.push((value_start, value_end));
        }
        cursor = find_byte(bytes, b')', value_end).map_or(end, |close| close + 1);
    }
    urls
}

/// The end of the CSS string whose content starts at `start`, and where
/// scanning resumes. An escaped quote does not end it.
fn css_string(bytes: &[u8], start: usize, end: usize, quote: u8) -> (usize, usize) {
    let mut cursor = start;
    while cursor < end {
        match bytes.get(cursor) {
            Some(b'\\') => cursor += 2,
            Some(&byte) if byte == quote => return (cursor, cursor + 1),
            // An unescaped newline ends a CSS string as a bad string.
            Some(b'\n') | None => return (cursor, cursor + 1),
            Some(_) => cursor += 1,
        }
    }
    (end.min(cursor), end)
}

/// Replaces each range in `edits`, sorted and disjoint, with its text, in
/// one exact-sized output. `None` when the output would exceed `max_bytes`.
pub(crate) fn splice(
    document: &str,
    edits: &[(usize, usize, String)],
    max_bytes: usize,
) -> Option<String> {
    let mut length = document.len();
    for (start, end, text) in edits {
        length = length
            .checked_sub(end.checked_sub(*start)?)?
            .checked_add(text.len())?;
    }
    if length > max_bytes {
        return None;
    }
    let mut output = String::with_capacity(length);
    let mut cursor = 0;
    for (start, end, text) in edits {
        output.push_str(document.get(cursor..*start)?);
        output.push_str(text);
        cursor = *end;
    }
    output.push_str(document.get(cursor..)?);
    Some(output)
}

/// The manifest carrier the signer's SVG writer inserts as the root's first
/// child, around canonical base64 of the manifest store.
const CARRIER_OPEN: &[u8] = b"<metadata><c2pa:manifest xmlns:c2pa=\"http://c2pa.org/manifest\">";
const CARRIER_CLOSE: &[u8] = b"</c2pa:manifest></metadata>";
/// The processing-instruction carrier the writer removes from a source that
/// holds one, though it never writes one.
const PI_OPEN: &[u8] = b"<?c2pa-manifest ";
const PI_CLOSE: &[u8] = b"?>";

/// Whether `signed` is exactly what the signer's SVG writer makes of
/// `original`: `original` with any manifest carrier it already held
/// removed, and one `<metadata><c2pa:manifest>` carrier of canonical base64
/// inserted right after the root `<svg>` start tag. Every other byte,
/// styles, `currentColor` and `<use>` references included, is then the
/// original's.
///
/// This checks the shape of the change only. That the carrier holds a valid
/// signature over these bytes rests on the authenticated endpoint and the
/// digest it answers with, not on this check.
///
/// # Errors
///
/// Returns [`Reason::InlineSvgAltered`] otherwise.
pub(crate) fn verify_svg_carrier(original: &[u8], signed: &[u8]) -> Result<(), Reason> {
    let clean = match existing_carrier(original) {
        Some((start, end)) => {
            let mut clean = original.get(..start).unwrap_or_default().to_vec();
            clean.extend_from_slice(original.get(end..).unwrap_or_default());
            clean
        }
        None => original.to_vec(),
    };
    let at = root_start_end(&clean).ok_or(Reason::InlineSvgAltered)?;
    let (head, tail) = clean.split_at(at);
    let carrier = signed
        .strip_prefix(head)
        .and_then(|rest| rest.strip_suffix(tail))
        .ok_or(Reason::InlineSvgAltered)?;
    let body = carrier
        .strip_prefix(CARRIER_OPEN)
        .and_then(|rest| rest.strip_suffix(CARRIER_CLOSE))
        .ok_or(Reason::InlineSvgAltered)?;
    if is_canonical_base64(body) {
        Ok(())
    } else {
        Err(Reason::InlineSvgAltered)
    }
}

/// The byte range of a manifest carrier `svg` already holds, as the signer's
/// writer finds it: a `<c2pa:manifest>` element, with a directly enclosing
/// `<metadata>` element, or a `<?c2pa-manifest ?>` instruction.
pub(crate) fn existing_carrier(svg: &[u8]) -> Option<(usize, usize)> {
    if let Some(start) = find_bytes(svg, b"<c2pa:manifest", 0) {
        let close = find_bytes(svg, b"</c2pa:manifest>", start)?;
        let mut end = close + b"</c2pa:manifest>".len();
        let mut start = start;
        if start >= b"<metadata>".len()
            && svg.get(start - b"<metadata>".len()..start) == Some(b"<metadata>".as_slice())
            && starts_with_at(svg, end, b"</metadata>")
        {
            start -= b"<metadata>".len();
            end += b"</metadata>".len();
        }
        return Some((start, end));
    }
    let start = find_bytes(svg, PI_OPEN, 0)?;
    let end = find_bytes(svg, PI_CLOSE, start)? + PI_CLOSE.len();
    Some((start, end))
}

/// The offset just past the root `<svg>` start tag, after any XML
/// declaration, comment or doctype before it.
fn root_start_end(svg: &[u8]) -> Option<usize> {
    let mut cursor = 0;
    loop {
        let open = find_byte(svg, b'<', cursor)?;
        if starts_with_at(svg, open, b"<?") {
            cursor = find_bytes(svg, b"?>", open)? + 2;
        } else if starts_with_at(svg, open, b"<!--") {
            cursor = comment_end(svg, open)?;
        } else if starts_with_at(svg, open, b"<!") {
            cursor = find_byte(svg, b'>', open)? + 1;
        } else {
            let name_end = open + 4;
            let is_svg = svg
                .get(open + 1..name_end)
                .is_some_and(|name| name.eq_ignore_ascii_case(b"svg"))
                && svg
                    .get(name_end)
                    .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'>');
            return is_svg
                .then(|| tag_end(svg, open + 1))
                .flatten()
                .map(|end| end + 1);
        }
    }
}

/// Whether `body` is standard base64 with padding, as the writer encodes.
fn is_canonical_base64(body: &[u8]) -> bool {
    let padding = body.iter().rev().take_while(|byte| **byte == b'=').count();
    !body.is_empty()
        && body.len().is_multiple_of(4)
        && padding <= 2
        && body[..body.len() - padding]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &str = "data:image/png;base64,iVBORw0KGgo=";

    fn texts<'a>(document: &'a str, scan: &Scan) -> Vec<&'a str> {
        scan.found
            .iter()
            .map(|found| &document[found.start..found.end])
            .collect()
    }

    #[test]
    fn finds_media_in_src_srcset_poster_lazy_and_noscript_attributes() {
        let document = format!(
            "<img src=\"{PNG}\" alt=x><img srcset=\"/a.png 1x, {PNG} 2x\">\
             <video poster='{PNG}'></video><img data-src= {PNG} >\
             <picture><source data-srcset=\"{PNG} 1x, {PNG} 2x\"></picture>\
             <noscript><img src=\"{PNG}\"></noscript><a href=\"/x\">x</a>"
        );
        let scan = scan_html(&document, None);
        assert_eq!(
            texts(&document, &scan),
            vec![PNG; 7],
            "should find every media URI"
        );
        assert!(scan.refused.is_empty(), "should refuse none");
    }

    #[test]
    fn ignores_scripts_comments_and_non_media_data_uris() {
        let document = format!(
            "<script>var a = \"<img src='{PNG}'>\";</script><!-- <img src=\"{PNG}\"> -->\
             <iframe src=\"data:text/html,<p>x</p>\"></iframe><textarea><img src=\"{PNG}\"></textarea>"
        );
        let scan = scan_html(&document, None);
        assert!(scan.is_empty(), "should find no media: {:?}", scan.found);
    }

    #[test]
    fn integrity_and_character_references_are_refused_not_dropped() {
        let document = format!(
            "<link rel=preload as=image href=\"{PNG}\" integrity=\"sha384-x\">\
             <img src=\"data:image/svg+xml,%3Csvg%3E&amp;\">"
        );
        let scan = scan_html(&document, None);
        assert!(scan.found.is_empty(), "should rewrite neither");
        assert_eq!(
            scan.refused,
            vec![Reason::IntegrityAttribute, Reason::CharacterReference],
            "should report each as unsigned"
        );
    }

    #[test]
    fn inline_style_edits_wait_for_known_policies_without_style_hashes() {
        let document = format!(
            "<div style=\"background:url({PNG})\"></div>\
             <img src=\"{PNG}\">\
             <style nonce=\"n\">.a{{background-image:url('{PNG}')}}</style>"
        );
        let unknown = scan_html(&document, None);
        assert_eq!(
            texts(&document, &unknown),
            vec![PNG],
            "should edit only the attribute"
        );
        assert_eq!(
            unknown.refused,
            vec![Reason::CspUnknown, Reason::CspUnknown],
            "should report both styles under an unseen policy"
        );

        let open = [String::from("img-src 'self' data:")];
        let scan = scan_html(&document, Some(&open));
        assert_eq!(
            texts(&document, &scan),
            vec![PNG; 3],
            "should edit all, in order"
        );
        assert!(
            scan.found
                .windows(2)
                .all(|pair| pair[0].start < pair[1].start),
            "should keep document order"
        );

        let hashed = [String::from("style-src 'sha256-abc' 'nonce-n'")];
        let scan = scan_html(&document, Some(&hashed));
        assert_eq!(
            texts(&document, &scan).len(),
            2,
            "should edit the image and the nonced element"
        );
        assert_eq!(scan.refused, vec![Reason::CspStyleHash]);

        let meta = format!(
            "<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'sha256-abc'\">{document}"
        );
        let scan = scan_html(&meta, Some(&open));
        assert_eq!(texts(&meta, &scan), vec![PNG], "should honor a meta policy");
    }

    #[test]
    fn only_the_style_s_own_nonce_admits_an_edit_under_a_hash_policy() {
        let style = |nonce: &str| {
            format!("<style nonce=\"{nonce}\">.a{{background-image:url('{PNG}')}}</style>")
        };
        let policy = [String::from("style-src 'sha256-abc' 'NONCE-n0Nce'")];
        for (nonce, edited) in [
            ("n0Nce", true),
            ("other", false),
            ("n0nce", false),
            ("", false),
        ] {
            let document = style(nonce);
            let scan = scan_html(&document, Some(&policy));
            assert_eq!(
                scan.found.len(),
                usize::from(edited),
                "nonce {nonce:?}: should edit only on the exact listed value"
            );
            if !edited {
                assert_eq!(scan.refused, vec![Reason::CspStyleHash], "nonce {nonce:?}");
            }
        }
    }

    #[test]
    fn style_text_inside_inline_svg_is_markup_whose_references_are_decoded() {
        let uri = "data:image/svg+xml,%3Csvg%3E&amp;";
        let open = [String::from("img-src data:")];
        let in_svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><style>.a{{fill:url(\"{uri}\")}}</style></svg>"
        );
        let scan = scan_html(&in_svg, Some(&open));
        assert!(
            scan.found.is_empty(),
            "should not rewrite a decoded reference"
        );
        assert_eq!(scan.refused, vec![Reason::CharacterReference]);

        let in_html = format!("<style>.a{{background:url(\"{uri}\")}}</style>");
        let scan = scan_html(&in_html, Some(&open));
        assert_eq!(
            texts(&in_html, &scan),
            vec![uri],
            "an HTML style's text is raw, so its `&` is literal"
        );

        let script = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><script></script><image href=\"{PNG}\"/></svg>"
        );
        let scan = scan_html(&script, Some(&open));
        assert_eq!(
            texts(&script, &scan),
            vec![PNG],
            "should keep scanning after an SVG script"
        );
    }

    #[test]
    fn only_url_attributes_are_rewritten() {
        let document = format!(
            "<input value=\"{PNG}\"><meta property=\"og:image\" content=\"{PNG}\">\
             <img title=\"{PNG}\" alt=\"{PNG}\" data-lazy-src=\"{PNG}\" src=\"/a.gif\">\
             <div data-bg=\"{PNG}\"></div><object data=\"{PNG}\"></object>"
        );
        let scan = scan_html(&document, None);
        let found: Vec<usize> = scan.found.iter().map(|found| found.start).collect();
        let lazy = document.find("data-lazy-src=\"").expect("lazy") + "data-lazy-src=\"".len();
        let background = document.find("data-bg=\"").expect("bg") + "data-bg=\"".len();
        let object = document.find("data=\"data:").expect("object") + "data=\"".len();
        assert_eq!(
            found,
            vec![lazy, background, object],
            "should rewrite lazy, background and object URLs, never text attributes"
        );
    }

    #[test]
    fn policies_are_read_from_both_policy_headers() {
        let mut headers = http::HeaderMap::new();
        headers.append(
            "content-security-policy",
            http::HeaderValue::from_static("img-src data:"),
        );
        headers.append(
            "content-security-policy-report-only",
            http::HeaderValue::from_static("style-src 'sha256-abc'"),
        );
        assert_eq!(
            content_security_policies(&headers),
            Some(vec![
                "img-src data:".to_owned(),
                "style-src 'sha256-abc'".to_owned()
            ])
        );
    }

    #[test]
    fn stylesheets_find_url_and_image_set_media_outside_comments() {
        let document = format!(
            ".a{{background:url({PNG})}}/* url({PNG}) */.b{{background:url( \"{PNG}\" )}}\
             .c{{background-image:image-set(\"{PNG}\" 1x, url({PNG}) 2x)}}\
             .d{{background:url(data:image/png;base64,iVBO\\52w0KGgo=)}}.e{{content:\"{PNG}\"}}"
        );
        let scan = scan_stylesheet(&document);
        assert_eq!(
            texts(&document, &scan),
            vec![PNG; 4],
            "should find each media URL"
        );
        assert_eq!(
            scan.refused,
            vec![Reason::CssEscape],
            "should refuse the escaped URL"
        );
    }

    #[test]
    fn outermost_inline_svgs_are_found_with_exact_ranges() {
        let svg =
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><svg><rect/></svg><use href=\"#a\"/></svg>";
        let document = format!("<p>a</p>{svg}<svg/><p>b</p>");
        let scan = scan_html(&document, None);
        assert_eq!(scan.svgs.len(), 1, "should find one outermost element");
        let (start, end) = scan.svgs[0];
        assert_eq!(&document[start..end], svg, "should cover the exact bytes");
    }

    #[test]
    fn self_closing_svg_attributes_are_scanned_without_whole_asset_signing() {
        let document = format!("<svg href=\"{PNG}\"/>");
        let scan = scan_html(&document, Some(&[]));
        assert_eq!(texts(&document, &scan), [PNG]);
        assert!(scan.svgs.is_empty());
        assert!(scan.refused.is_empty());
    }

    #[test]
    fn original_svg_provenance_protects_attributes_css_and_nested_svg_ranges() {
        for provenance in [
            CARRIER,
            "<?c2pa-manifest QUJD?>",
            "<metadata><dcterms:provenance>https://example.com/m</dcterms:provenance></metadata>",
            "<metadata dcterms:provenance=\"https://example.com/m\"/>",
            "<metadata xmlns:proof=\"https://c2pa.org/manifest\"/>",
            "<metadata><c2pa:manifest>QUJD</metadata>",
        ] {
            let svg = format!(
                "{ROOT}<image href=\"{PNG}\"/>{provenance}\
                 <style>.a{{fill:url('{PNG}')}}</style>\
                 <g style=\"fill:url({PNG})\"><svg><image href=\"{PNG}\"/></svg></g></svg>"
            );
            let document = format!("<img src=\"{PNG}\">{svg}<img src=\"{PNG}\">");
            let scan = scan_html(&document, Some(&[]));
            assert_eq!(texts(&document, &scan), [PNG, PNG], "{provenance}");
            assert!(scan.svgs.is_empty(), "must not resign {provenance}");
            assert_eq!(
                scan.refused,
                [Reason::SvgExistingProvenance],
                "{provenance}"
            );
        }
    }

    #[test]
    fn unsigned_svg_descendants_remain_editable_without_a_false_caption_match() {
        let document = format!(
            "{ROOT}<title>Signed with C2PA</title><image href=\"{PNG}\"/>\
             <style>.a{{fill:url('{PNG}')}}</style>\
             <g style=\"fill:url({PNG})\"/></svg>"
        );
        let scan = scan_html(&document, Some(&[]));
        assert_eq!(texts(&document, &scan), [PNG; 3]);
        assert_eq!(scan.svgs, [(0, document.len())]);
        assert!(scan.refused.is_empty());
    }

    #[test]
    fn incomplete_or_uninspectable_svg_ranges_fail_closed_before_child_edits() {
        for svg in [
            format!("{ROOT}<image href=\"{PNG}\"/>{CARRIER}"),
            format!("{ROOT}<image href=\"{PNG}\"/>"),
            format!("{ROOT}<image href=\"{PNG}\"/><p>breakout</p></svg>"),
            format!(
                "{ROOT}<image href=\"{PNG}\"/>{}</svg>",
                "<g/>".repeat(10_000)
            ),
            format!("<svg dcterms:provenance=\"https://example.com/m\" href=\"{PNG}\"/>"),
        ] {
            let document = format!("<img src=\"{PNG}\">{svg}");
            let scan = scan_html(&document, Some(&[]));
            assert_eq!(texts(&document, &scan), [PNG], "{svg}");
            assert!(scan.svgs.is_empty(), "must not resign {svg}");
            assert_eq!(scan.refused.len(), 1);
        }
    }

    const ROOT: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1 1\">";
    const BODY: &str = "<style>.a{fill:currentColor}</style><use href=\"#i\"/></svg>";
    const CARRIER: &str = "<metadata><c2pa:manifest xmlns:c2pa=\"http://c2pa.org/manifest\">QUJD+/==</c2pa:manifest></metadata>";

    #[test]
    fn the_writer_s_carrier_after_the_root_start_tag_keeps_the_svg() {
        let original = format!("{ROOT}{BODY}");
        let signed = format!("{ROOT}{CARRIER}{BODY}");
        assert_eq!(
            verify_svg_carrier(original.as_bytes(), signed.as_bytes()),
            Ok(()),
            "should accept the writer's carrier"
        );
        let declared = format!("<?xml version=\"1.0\"?>\n<!-- art -->{ROOT}{BODY}");
        let signed = format!("<?xml version=\"1.0\"?>\n<!-- art -->{ROOT}{CARRIER}{BODY}");
        assert_eq!(
            verify_svg_carrier(declared.as_bytes(), signed.as_bytes()),
            Ok(()),
            "should find the root after a declaration and a comment"
        );
    }

    #[test]
    fn a_carrier_the_source_held_is_replaced_and_nothing_else() {
        let old_element = "<metadata><c2pa:manifest xmlns:c2pa=\"http://c2pa.org/manifest\">T0xE</c2pa:manifest></metadata>";
        for old in [old_element, "<?c2pa-manifest T0xE?>"] {
            let original = format!("{ROOT}<g/>{old}{BODY}");
            let signed = format!("{ROOT}{CARRIER}<g/>{BODY}");
            assert_eq!(
                verify_svg_carrier(original.as_bytes(), signed.as_bytes()),
                Ok(()),
                "should accept replacing {old}"
            );
        }
    }

    #[test]
    fn any_other_change_to_the_svg_is_refused() {
        let original = format!("{ROOT}{BODY}");
        let altered = [
            format!("{ROOT}{CARRIER}{BODY}").replace("currentColor", "#000"),
            format!("{ROOT}{}{CARRIER}", BODY.replace("</svg>", "")) + "</svg>",
            format!("{ROOT}<metadata><script>x</script></metadata>{BODY}"),
            format!("{ROOT}{}{BODY}", CARRIER.replace("QUJD+/==", "QU JD")),
            format!("{ROOT} {CARRIER}{BODY}"),
            format!("{ROOT}<?c2pa-manifest QUJD+/==?>{BODY}"),
            original.clone(),
        ];
        for signed in altered {
            assert_eq!(
                verify_svg_carrier(original.as_bytes(), signed.as_bytes()),
                Err(Reason::InlineSvgAltered),
                "should refuse {signed}"
            );
        }
    }

    #[test]
    fn splice_replaces_exact_ranges_within_budget() {
        let document = "<img src=\"A\"><img src=\"B\">";
        let edits = vec![(10, 11, "xx".to_owned()), (23, 24, "y".to_owned())];
        assert_eq!(
            splice(document, &edits, 100).as_deref(),
            Some("<img src=\"xx\"><img src=\"y\">")
        );
        assert_eq!(
            splice(document, &edits, document.len()),
            None,
            "should honor the budget"
        );
    }
}
