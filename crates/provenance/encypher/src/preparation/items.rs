//! The items of one delivered document, with their preparation identities.
//!
//! The reader computes these from the bytes it is about to deliver and the
//! worker from the snapshot the delivery instance gave it, through the same
//! functions, so a stable document yields the same identities on both.
//! Text comes from the one article extraction every path uses; embedded
//! media from the one scan of `data:` URIs and inline SVG that decides
//! which ranges may be edited.

use std::borrow::Cow;

use super::pipeline::{TextCacheScope, TextSignRequest};
use super::protocol::{Carrier, MediaIdentity, TextIdentity};
use crate::data_uri::DataUri;
use crate::embedded;
use crate::media_api::sha256_hex;
use crate::processor::{Article, Reason, extract_article};
use crate::svg_equivalence;
use crate::text;

/// What a document is, which decides what in it is an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    /// An HTML page: its article's text and its embedded media.
    Page,
    /// A stylesheet: the media its `url()`s embed.
    Stylesheet,
}

/// What every identity of one document binds besides the item itself.
#[derive(Debug, Clone, Copy)]
pub struct ItemContext<'a> {
    /// See [`super::protocol::credential_context`].
    pub credential_context: &'a str,
    /// The policy the items are prepared under.
    pub policy_digest: &'a str,
    /// The query-free address the document is delivered on.
    pub page_url: &'a str,
    /// The admitted variant.
    pub variant: &'a str,
    /// The configured article selector override, empty for none.
    pub article_selector: &'a str,
    /// The least visible text an article is signed with.
    pub min_text_length: usize,
    /// The largest embedded media item.
    pub max_media_bytes: usize,
}

/// An article's text item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextItem {
    /// Its identity.
    pub identity: String,
    /// The exact sign request it is prepared with.
    pub request: TextSignRequest,
    /// The scalars of its text.
    pub scalars: usize,
}

/// Where an embedded item sits in its document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    /// A media `data:` URI, its exact byte range.
    DataUri {
        /// The first byte.
        start: usize,
        /// One past the last byte.
        end: usize,
    },
    /// An outermost inline `<svg>`, its exact byte range.
    InlineSvg {
        /// The first byte.
        start: usize,
        /// One past the last byte.
        end: usize,
    },
}

impl Site {
    fn range(self) -> (usize, usize) {
        match self {
            Self::DataUri { start, end } | Self::InlineSvg { start, end } => (start, end),
        }
    }
}

/// One embedded media item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedItem {
    /// Its identity.
    pub identity: String,
    /// Where it sits.
    pub site: Site,
    /// Its canonical media type.
    pub mime: String,
    /// Its exact source bytes: the decoded payload, or the standalone SVG
    /// document proven equivalent to the browser's tree.
    pub source: Vec<u8>,
    /// SHA-256 of `source`.
    pub sha256: String,
}

/// The items of one document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentItems {
    /// The article, where the page has one.
    pub text: Option<TextItem>,
    /// Why the page's article is not an item, where it is not.
    pub text_refusal: Option<&'static str>,
    /// Embedded media, in document order, never overlapping.
    pub embedded: Vec<EmbeddedItem>,
    /// Embedded media left as written, each with why.
    pub refused: Vec<&'static str>,
}

/// The identity of an article, without constructing an owned signing request.
#[must_use]
pub(crate) fn text_identity(
    context: &ItemContext<'_>,
    article: &Article,
    document_title: Option<&str>,
) -> String {
    TextIdentity {
        credential_context: context.credential_context,
        policy_digest: context.policy_digest,
        page_url: context.page_url,
        variant: context.variant,
        text: &article.text,
        document_title,
        boundary_selector: &article.boundary.selector,
    }
    .digest()
}

/// The text item of `article` in `html`.
#[must_use]
pub(crate) fn text_item(context: &ItemContext<'_>, html: &str, article: &Article) -> TextItem {
    let title = text::title_from_range(html, article.title_range);
    let identity = text_identity(context, article, title.as_deref());
    TextItem {
        identity,
        request: TextSignRequest {
            text: article.text.clone(),
            page_url: context.page_url.to_owned(),
            document_title: title,
            boundary_selector: article.boundary.selector.clone(),
            expected_policy_digest: context.policy_digest.to_owned(),
            cache_scope: TextCacheScope::ExactRequest,
        },
        scalars: article.scalars,
    }
}

/// The identity of an embedded item with `mime` and `sha256` in a document
/// of `context`.
#[must_use]
pub fn embedded_identity(context: &ItemContext<'_>, mime: &str, sha256: &str) -> String {
    MediaIdentity {
        credential_context: context.credential_context,
        policy_digest: context.policy_digest,
        page_url: context.page_url,
        variant: context.variant,
        carrier: Carrier::Embedded,
        mime,
        source_sha256: sha256,
    }
    .digest()
}

/// The embedded media items of `document`. An inline SVG is one item, and
/// a `data:` URI inside an inline SVG item is carried by it rather than
/// prepared on its own, so no two items overlap and an SVG's identity is
/// that of the markup the browser parsed.
#[must_use]
pub fn embedded_items(
    context: &ItemContext<'_>,
    document: &str,
    kind: DocumentKind,
    policies: Option<&[String]>,
) -> (Vec<EmbeddedItem>, Vec<&'static str>) {
    let scan = match kind {
        DocumentKind::Page => embedded::scan_html(document, policies),
        DocumentKind::Stylesheet => embedded::scan_stylesheet(document),
    };
    let mut refused: Vec<&'static str> = scan.refused.iter().map(|reason| reason.code()).collect();
    let mut items = Vec::new();
    let mut svg_ranges = Vec::new();
    if kind == DocumentKind::Page {
        for (start, end) in scan.svgs.iter().copied() {
            let Some(fragment) = document.get(start..end) else {
                continue;
            };
            match svg_equivalence::signable_svg(fragment, context.max_media_bytes) {
                Ok(svg) => {
                    let source = match svg {
                        Cow::Borrowed(text) => text.as_bytes().to_vec(),
                        Cow::Owned(text) => text.into_bytes(),
                    };
                    let sha256 = sha256_hex(&source);
                    svg_ranges.push((start, end));
                    items.push(EmbeddedItem {
                        identity: embedded_identity(context, "image/svg+xml", &sha256),
                        site: Site::InlineSvg { start, end },
                        mime: "image/svg+xml".to_owned(),
                        source,
                        sha256,
                    });
                }
                Err(reason) => refused.push(reason.code()),
            }
        }
    }
    for found in &scan.found {
        if svg_ranges
            .iter()
            .any(|(start, end)| found.start < *end && *start < found.end)
        {
            refused.push("inside_inline_svg");
            continue;
        }
        let Some(uri) = document
            .get(found.start..found.end)
            .and_then(DataUri::parse)
        else {
            continue;
        };
        let Ok(source) = uri.decode() else {
            refused.push(Reason::MalformedDataUri.code());
            continue;
        };
        if source.len() > context.max_media_bytes {
            refused.push(Reason::MediaTooLarge.code());
            continue;
        }
        let mime = uri.media_type();
        let sha256 = sha256_hex(&source);
        items.push(EmbeddedItem {
            identity: embedded_identity(context, &mime, &sha256),
            site: Site::DataUri {
                start: found.start,
                end: found.end,
            },
            mime,
            source,
            sha256,
        });
    }
    items.sort_by_key(|item| item.site.range());
    (items, refused)
}

/// Every item of `document`: the article's text first, then its embedded
/// media.
#[must_use]
pub fn document_items(
    context: &ItemContext<'_>,
    document: &str,
    kind: DocumentKind,
    policies: Option<&[String]>,
) -> DocumentItems {
    let (text, text_refusal) = match kind {
        DocumentKind::Page => {
            match extract_article(document, context.article_selector, context.min_text_length) {
                Ok(article) => (Some(text_item(context, document, &article)), None),
                Err(reason) => (None, Some(reason.code())),
            }
        }
        DocumentKind::Stylesheet => (None, None),
    };
    let (embedded, refused) = embedded_items(context, document, kind, policies);
    DocumentItems {
        text,
        text_refusal,
        embedded,
        refused,
    }
}

/// The edit that puts an embedded item's output in place of its source in
/// `document`, or `None` where the output does not fit the site: an SVG
/// that is not UTF-8.
#[must_use]
pub fn embedded_edit(
    document: &str,
    item: &EmbeddedItem,
    output: &[u8],
) -> Option<(usize, usize, String)> {
    match item.site {
        Site::DataUri { start, end } => {
            let uri = DataUri::parse(document.get(start..end)?)?;
            Some((start, end, uri.encode(output)))
        }
        Site::InlineSvg { start, end } => {
            Some((start, end, String::from_utf8(output.to_vec()).ok()?))
        }
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    use super::*;

    const POLICY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn context() -> ItemContext<'static> {
        ItemContext {
            credential_context: "0123456789abcdef",
            policy_digest: POLICY,
            page_url: "https://www.publisher.example.com/news/a",
            variant: "",
            article_selector: "",
            min_text_length: 50,
            max_media_bytes: 1024 * 1024,
        }
    }

    fn page(pixel: &[u8]) -> String {
        format!(
            "<!doctype html><html><head><title>Harbor</title></head><body><article>\
             <h1>Harbor council votes on the ferry schedule</h1><p>The harbor council met on \
             Tuesday evening and approved a revised ferry schedule for the season.</p>\
             <img src=\"data:image/png;base64,{}\"></article></body></html>",
            STANDARD.encode(pixel)
        )
    }

    #[test]
    fn identical_documents_yield_identical_items_and_one_changed_item_changes_only_its_own() {
        let first = document_items(&context(), &page(b"pixel-one"), DocumentKind::Page, None);
        let again = document_items(&context(), &page(b"pixel-one"), DocumentKind::Page, None);
        assert_eq!(
            first, again,
            "a stable document should yield the same items"
        );
        let text = first.text.as_ref().expect("should find the article");
        assert_eq!(first.embedded.len(), 1, "should find the embedded image");

        let changed = document_items(&context(), &page(b"pixel-two"), DocumentKind::Page, None);
        assert_eq!(
            changed.text.as_ref().map(|item| &item.identity),
            Some(&text.identity),
            "a changed image should leave the article's identity unchanged"
        );
        assert_ne!(
            changed.embedded[0].identity, first.embedded[0].identity,
            "the changed image should take a new identity"
        );
    }

    #[test]
    fn policy_change_changes_every_identity() {
        let document = page(b"pixel-one");
        let first = document_items(&context(), &document, DocumentKind::Page, None);
        let other_policy = "b".repeat(64);
        let changed = document_items(
            &ItemContext {
                policy_digest: &other_policy,
                ..context()
            },
            &document,
            DocumentKind::Page,
            None,
        );
        assert_ne!(
            first.text.map(|item| item.identity),
            changed.text.map(|item| item.identity),
            "a new policy should never reuse a result made under the old one"
        );
        assert_ne!(first.embedded[0].identity, changed.embedded[0].identity);
    }

    #[test]
    fn embedded_edit_replaces_only_the_items_own_range() {
        let document = page(b"pixel-one");
        let items = document_items(&context(), &document, DocumentKind::Page, None);
        let item = &items.embedded[0];
        let (start, end, text) =
            embedded_edit(&document, item, b"signed-pixel").expect("should build the edit");
        assert!(
            text.starts_with("data:image/png;base64,"),
            "should keep the URI's own type"
        );
        let output =
            embedded::splice(&document, &[(start, end, text)], usize::MAX).expect("should splice");
        assert!(output.contains(&STANDARD.encode(b"signed-pixel")));
        assert!(!output.contains(&STANDARD.encode(b"pixel-one")));
    }
}
