//! The standalone SVG document an inline `<svg>` is signed as.
//!
//! A browser builds an inline SVG's tree with the HTML parser, which reads
//! it differently from an XML parser: it needs no namespace declaration,
//! keeps the first of two attributes whose names differ only in case, and
//! decodes HTML character references. What is signed must be a standalone
//! SVG document whose XML tree is the tree the browser built, so a verifier
//! reading the signed bytes sees what the reader saw.
//!
//! The fragment is parsed as HTML in a `body` context, into scraper's
//! namespace-aware tree, as the browser parsed it, and refused unless that
//! yields exactly one `<svg>` root with nothing displaced beside it but
//! whitespace. Where the original bytes already parse as XML to the same
//! tree, they are signed unchanged. Otherwise, unless the fragment carries
//! provenance of its own, which is never rewritten, the browser's tree is
//! serialized as XML and kept only when that serialization parses to the
//! same tree both as HTML and as XML.
//!
//! Trees are compared by element namespace and local name, the set of
//! ordinary attributes by namespace, local name and value, text with
//! adjacent text merged and CDATA read as text, and comments. Namespace
//! declarations are not ordinary attributes and are not compared: only the
//! namespaces they resolve are. Anything else, a processing instruction or a
//! doctype, makes two trees unequal. XML is always parsed with DTDs
//! refused, so no entity is ever declared or resolved.

use std::borrow::Cow;
use std::marker::PhantomData;

use ego_tree::NodeRef;
use ego_tree::iter::Children;
use html5ever::tendril::TendrilSink as _;
use html5ever::{LocalName, Namespace, ParseOpts, QualName, parse_fragment};
use scraper::node::Element;
use scraper::{HtmlTreeSink, Node};

use crate::embedded;
use crate::processor::Reason;

const HTML_NAMESPACE: &str = "http://www.w3.org/1999/xhtml";
const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";
const XLINK_NAMESPACE: &str = "http://www.w3.org/1999/xlink";
const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

/// The most nodes an inline SVG may hold.
const MAX_NODES: usize = 10_000;
/// The deepest an inline SVG's elements may nest.
const MAX_DEPTH: usize = 256;
/// Nodes the HTML fragment parser makes besides the fragment's own: the
/// fragment root, the context element and the root `html` element.
const PARSER_NODES: usize = 3;

/// The declarations the serialization's root carries.
const SVG_DECLARATION: &str = " xmlns=\"http://www.w3.org/2000/svg\"";
const XLINK_DECLARATION: &str = " xmlns:xlink=\"http://www.w3.org/1999/xlink\"";

/// The standalone SVG document `fragment`, an inline `<svg>` element as
/// written in an HTML page, is signed as: `fragment` itself where it
/// already parses as XML to the tree the browser built from it, else that
/// tree serialized as XML.
///
/// # Errors
///
/// - [`Reason::MediaTooLarge`] where `fragment` is longer than `max_bytes`.
/// - [`Reason::SvgProvenanceNotEquivalent`] where `fragment` carries
///   provenance of its own and is not, as written, equivalent: it is never
///   rewritten.
/// - [`Reason::InlineSvgNotEquivalent`] where the browser's tree is not one
///   `<svg>` root, exceeds the node or depth bound, or has no XML
///   serialization that parses back to it as both HTML and XML.
pub(crate) fn signable_svg(fragment: &str, max_bytes: usize) -> Result<Cow<'_, str>, Reason> {
    if fragment.len() > max_bytes {
        return Err(Reason::MediaTooLarge);
    }
    let parsed = parse_html(fragment).ok_or(Reason::InlineSvgNotEquivalent)?;
    let root = parsed.root().ok_or(Reason::InlineSvgNotEquivalent)?;
    if let Some(document) = parse_xml(fragment)
        && equal(&Browser, root, &Xml(PhantomData), document.root_element())
    {
        return Ok(Cow::Borrowed(fragment));
    }
    if carries_provenance(fragment, root) {
        return Err(Reason::SvgProvenanceNotEquivalent);
    }
    let serialized = serialize(root, max_bytes).ok_or(Reason::InlineSvgNotEquivalent)?;
    let equivalent = {
        let as_html = parse_html(&serialized);
        let as_xml = parse_xml(&serialized);
        match (as_html.as_ref().and_then(Parsed::root), as_xml) {
            (Some(as_html), Some(as_xml)) => {
                equal(&Browser, root, &Browser, as_html)
                    && equal(&Browser, root, &Xml(PhantomData), as_xml.root_element())
            }
            _ => false,
        }
    };
    if equivalent {
        Ok(Cow::Owned(serialized))
    } else {
        Err(Reason::InlineSvgNotEquivalent)
    }
}

/// Refuse all edits inside an original inline SVG that carries provenance.
/// A fragment whose structure cannot be inspected within the parser's limits
/// is also left intact, rather than assuming it has no provenance.
pub(crate) fn descendant_edit_refusal(fragment: &str) -> Option<Reason> {
    let Some(parsed) = parse_html(fragment) else {
        return Some(Reason::InlineSvgNotEquivalent);
    };
    let Some(root) = parsed.root() else {
        return Some(Reason::InlineSvgNotEquivalent);
    };
    carries_provenance(fragment, root).then_some(Reason::SvgExistingProvenance)
}

/// Whether the inline SVG under `root` carries provenance of its own: the
/// manifest carrier the signer's writer finds, an element or attribute in
/// the `c2pa` prefix, a declaration of a C2PA namespace, or a provenance
/// reference such as XMP's `dcterms:provenance`. The HTML parser keeps such
/// prefixed names whole, as written. Ordinary text that mentions C2PA is
/// not provenance.
fn carries_provenance(fragment: &str, root: NodeRef<'_, Node>) -> bool {
    if embedded::existing_carrier(fragment.as_bytes()).is_some() {
        return true;
    }
    root.descendants().any(|node| {
        let Node::Element(element) = node.value() else {
            return false;
        };
        names_provenance(&element.name.local)
            || element.attrs.iter().any(|(name, value)| {
                names_provenance(&name.local)
                    || (is_declaration(&name.ns, &name.local) && is_c2pa_namespace(value))
            })
    })
}

/// Whether a name the HTML parser kept whole, prefix included, is in the
/// `c2pa` prefix or names a provenance reference.
fn names_provenance(name: &str) -> bool {
    let (prefix, local) = name.rsplit_once(':').unwrap_or(("", name));
    prefix.eq_ignore_ascii_case("c2pa") || local.eq_ignore_ascii_case("provenance")
}

fn is_c2pa_namespace(uri: &str) -> bool {
    let uri = uri.trim();
    ["http://c2pa.org/", "https://c2pa.org/"]
        .iter()
        .any(|start| {
            uri.get(..start.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(start))
        })
}

/// scraper's tree for one fragment, with its `<svg>` root.
struct Parsed {
    html: scraper::Html,
    root: ego_tree::NodeId,
}

impl Parsed {
    fn root(&self) -> Option<NodeRef<'_, Node>> {
        self.html.tree.get(self.root)
    }
}

/// `fragment` parsed as HTML in a `body` context with scripting on, as a
/// browser parses inline SVG, into scraper's tree, or `None` unless that
/// yields exactly one `<svg>` element at the top, with only whitespace text
/// beside it, within the node bound.
fn parse_html(fragment: &str) -> Option<Parsed> {
    // Every element and comment the tokenizer starts begins at a `<`, so
    // this bounds what the parse allocates before the exact count below.
    if fragment.bytes().filter(|byte| *byte == b'<').count() > MAX_NODES {
        return None;
    }
    let context = QualName::new(
        None,
        Namespace::from(HTML_NAMESPACE),
        LocalName::from("body"),
    );
    let html = parse_fragment(
        HtmlTreeSink::new(scraper::Html::new_fragment()),
        ParseOpts::default(),
        context,
        Vec::new(),
        true,
    )
    .one(fragment);
    if html.tree.values().count() > MAX_NODES + PARSER_NODES {
        return None;
    }
    let top = html
        .tree
        .root()
        .children()
        .find(|child| child.value().is_element())?;
    let mut root = None;
    for child in top.children() {
        match child.value() {
            Node::Element(element)
                if root.is_none()
                    && &*element.name.ns == SVG_NAMESPACE
                    && &*element.name.local == "svg" =>
            {
                root = Some(child.id());
            }
            Node::Text(text) if text.chars().all(is_html_whitespace) => {}
            _ => return None,
        }
    }
    let root = root?;
    Some(Parsed { html, root })
}

fn is_html_whitespace(character: char) -> bool {
    matches!(character, '\t' | '\n' | '\u{c}' | '\r' | ' ')
}

/// `text` parsed as XML with DTDs refused, so no entity is declared or
/// resolved, or `None` where it is not well-formed.
fn parse_xml(text: &str) -> Option<roxmltree::Document<'_>> {
    let options = roxmltree::ParsingOptions {
        allow_dtd: false,
        nodes_limit: u32::try_from(MAX_NODES + 1).unwrap_or(u32::MAX),
        ..roxmltree::ParsingOptions::default()
    };
    roxmltree::Document::parse_with_options(text, options).ok()
}

/// What a compared node is.
enum Kind<'a> {
    Element { namespace: &'a str, name: &'a str },
    Text(&'a str),
    Comment(&'a str),
    Other,
}

/// A tree, as the comparison walks it.
trait View<'a> {
    type Node: Copy;
    type Children: Iterator<Item = Self::Node>;

    fn kind(&self, node: Self::Node) -> Kind<'a>;
    /// The element's ordinary attributes as (namespace, local name, value),
    /// sorted, namespace declarations excluded.
    fn attributes(&self, node: Self::Node) -> Vec<(&'a str, &'a str, &'a str)>;
    fn children(&self, node: Self::Node) -> Self::Children;
}

/// Whether an attribute is a namespace declaration rather than an ordinary
/// attribute. The HTML parser puts `xmlns` and `xmlns:xlink` in the XMLNS
/// namespace and leaves any other `xmlns:` prefixed name unnamespaced.
fn is_declaration(namespace: &str, name: &str) -> bool {
    namespace == XMLNS_NAMESPACE
        || (namespace.is_empty() && (name == "xmlns" || name.starts_with("xmlns:")))
}

/// scraper's tree, whose node references carry the tree.
struct Browser;

impl<'a> View<'a> for Browser {
    type Node = NodeRef<'a, Node>;
    type Children = Children<'a, Node>;

    fn kind(&self, node: Self::Node) -> Kind<'a> {
        match node.value() {
            Node::Element(element) => Kind::Element {
                namespace: &element.name.ns,
                name: &element.name.local,
            },
            Node::Text(text) => Kind::Text(&text.text),
            Node::Comment(comment) => Kind::Comment(&comment.comment),
            _ => Kind::Other,
        }
    }

    fn attributes(&self, node: Self::Node) -> Vec<(&'a str, &'a str, &'a str)> {
        let Node::Element(element) = node.value() else {
            return Vec::new();
        };
        let mut ordinary: Vec<(&'a str, &'a str, &'a str)> = element
            .attrs
            .iter()
            .map(|(name, value)| (&*name.ns, &*name.local, &**value))
            .filter(|(namespace, name, _)| !is_declaration(namespace, name))
            .collect();
        ordinary.sort_unstable();
        ordinary
    }

    fn children(&self, node: Self::Node) -> Self::Children {
        node.children()
    }
}

/// A roxmltree document, whose nodes carry their own references.
struct Xml<'a, 'input: 'a>(PhantomData<roxmltree::Node<'a, 'input>>);

impl<'a, 'input: 'a> View<'a> for Xml<'a, 'input> {
    type Node = roxmltree::Node<'a, 'input>;
    type Children = roxmltree::Children<'a, 'input>;

    fn kind(&self, node: Self::Node) -> Kind<'a> {
        match node.node_type() {
            roxmltree::NodeType::Element => {
                let name = node.tag_name();
                Kind::Element {
                    namespace: name.namespace().unwrap_or_default(),
                    name: name.name(),
                }
            }
            roxmltree::NodeType::Text => Kind::Text(node.text().unwrap_or_default()),
            roxmltree::NodeType::Comment => Kind::Comment(node.text().unwrap_or_default()),
            roxmltree::NodeType::Root | roxmltree::NodeType::PI => Kind::Other,
        }
    }

    fn attributes(&self, node: Self::Node) -> Vec<(&'a str, &'a str, &'a str)> {
        // roxmltree keeps namespace declarations apart from attributes.
        let mut ordinary: Vec<_> = node
            .attributes()
            .map(|attribute| {
                (
                    attribute.namespace().unwrap_or_default(),
                    attribute.name(),
                    attribute.value(),
                )
            })
            .collect();
        ordinary.sort_unstable();
        ordinary
    }

    fn children(&self, node: Self::Node) -> Self::Children {
        node.children()
    }
}

/// Whether the trees under `a_root` and `b_root` are equal, within the
/// depth bound. The walk keeps one pair of child iterators per open element
/// and copies neither tree.
fn equal<'a, 'b, A: View<'a>, B: View<'b>>(a: &A, a_root: A::Node, b: &B, b_root: B::Node) -> bool {
    if !same(a, a_root, b, b_root) {
        return false;
    }
    let mut open = vec![(a.children(a_root), b.children(b_root))];
    while let Some((mut a_children, mut b_children)) = open.pop() {
        match (a_children.next(), b_children.next()) {
            (None, None) => {}
            (Some(a_node), Some(b_node)) => {
                if !same(a, a_node, b, b_node) {
                    return false;
                }
                let element = matches!(a.kind(a_node), Kind::Element { .. });
                open.push((a_children, b_children));
                if element {
                    if open.len() >= MAX_DEPTH {
                        return false;
                    }
                    open.push((a.children(a_node), b.children(b_node)));
                }
            }
            _ => return false,
        }
    }
    true
}

fn same<'a, 'b, A: View<'a>, B: View<'b>>(a: &A, a_node: A::Node, b: &B, b_node: B::Node) -> bool {
    match (a.kind(a_node), b.kind(b_node)) {
        (
            Kind::Element {
                namespace: a_namespace,
                name: a_name,
            },
            Kind::Element {
                namespace: b_namespace,
                name: b_name,
            },
        ) => {
            a_namespace == b_namespace
                && a_name == b_name
                && a.attributes(a_node) == b.attributes(b_node)
        }
        (Kind::Text(a_text), Kind::Text(b_text))
        | (Kind::Comment(a_text), Kind::Comment(b_text)) => a_text == b_text,
        _ => false,
    }
}

/// The tree under `root` written as a standalone SVG document, or `None`
/// where XML cannot carry it: an element outside the SVG namespace, an
/// attribute outside no namespace, `XLink` or XML, a name XML cannot spell, a
/// character XML forbids, a comment XML cannot hold, any other node, or
/// more than `max_bytes`.
///
/// The root declares the SVG namespace, and the `XLink` prefix where an
/// attribute uses it. Every element and ordinary attribute is the tree's,
/// with its value, which is the HTML parser's: the first of attributes that
/// differ only in case, and character references decoded. Values escape
/// `&`, `<`, `>`, `"`, tab, line feed and carriage return, and text escapes
/// `&`, `<`, `>` and carriage return, so XML's attribute-value and line-end
/// normalization leaves them as parsed.
fn serialize(root: NodeRef<'_, Node>, max_bytes: usize) -> Option<String> {
    let Node::Element(element) = root.value() else {
        return None;
    };
    let mut output = SvgOutput::new(max_bytes);
    let mut uses_xlink = false;
    write_start(&mut output, element, &mut uses_xlink)?;
    // The root's declarations follow its name: the XLink one is known only
    // once every attribute is written.
    let declarations = 1 + element.name.local.len();
    output.insert_str(declarations, SVG_DECLARATION)?;
    let mut open: Vec<(Children<'_, Node>, &str)> = Vec::new();
    if root.first_child().is_some() {
        output.push('>')?;
        open.push((root.children(), &*element.name.local));
    } else {
        output.push_str("/>")?;
    }
    while let Some((mut children, name)) = open.pop() {
        let Some(child) = children.next() else {
            output.push_str("</")?;
            output.push_str(name)?;
            output.push('>')?;
            continue;
        };
        open.push((children, name));
        match child.value() {
            Node::Element(element) => {
                write_start(&mut output, element, &mut uses_xlink)?;
                if child.first_child().is_some() {
                    if open.len() >= MAX_DEPTH {
                        return None;
                    }
                    output.push('>')?;
                    open.push((child.children(), &*element.name.local));
                } else {
                    output.push_str("/>")?;
                }
            }
            Node::Text(text) => write_text(&mut output, &text.text)?,
            Node::Comment(comment) => write_comment(&mut output, &comment.comment)?,
            _ => return None,
        }
    }
    if uses_xlink {
        output.insert_str(declarations + SVG_DECLARATION.len(), XLINK_DECLARATION)?;
    }
    Some(output.text)
}

/// A serializer buffer whose length and requested allocation never exceed
/// the output limit. Grow geometrically, but request the exact bounded target
/// so String's implicit doubling cannot allocate beyond it.
struct SvgOutput {
    text: String,
    max_bytes: usize,
}

impl SvgOutput {
    fn new(max_bytes: usize) -> Self {
        Self {
            text: String::new(),
            max_bytes,
        }
    }

    fn reserve(&mut self, additional: usize) -> Option<()> {
        let needed = self.text.len().checked_add(additional)?;
        if needed > self.max_bytes {
            return None;
        }
        if needed > self.text.capacity() {
            let target = self
                .text
                .capacity()
                .checked_mul(2)
                .unwrap_or(self.max_bytes)
                .max(64.min(self.max_bytes))
                .max(needed)
                .min(self.max_bytes);
            self.text
                .try_reserve_exact(target.checked_sub(self.text.len())?)
                .ok()?;
        }
        Some(())
    }

    fn push_str(&mut self, text: &str) -> Option<()> {
        self.reserve(text.len())?;
        self.text.push_str(text);
        Some(())
    }

    fn push(&mut self, character: char) -> Option<()> {
        self.reserve(character.len_utf8())?;
        self.text.push(character);
        Some(())
    }

    fn insert_str(&mut self, at: usize, text: &str) -> Option<()> {
        self.reserve(text.len())?;
        self.text.insert_str(at, text);
        Some(())
    }
}

/// Writes `<name` and the element's ordinary attributes, noting an `XLink` one.
fn write_start(output: &mut SvgOutput, element: &Element, uses_xlink: &mut bool) -> Option<()> {
    if &*element.name.ns != SVG_NAMESPACE || !is_xml_local_name(&element.name.local) {
        return None;
    }
    output.push('<')?;
    output.push_str(&element.name.local)?;
    for (name, value) in element.attrs.iter() {
        let namespace = &*name.ns;
        let local = &*name.local;
        if is_declaration(namespace, local) {
            continue;
        }
        let prefix = match namespace {
            "" => "",
            XLINK_NAMESPACE => {
                *uses_xlink = true;
                "xlink:"
            }
            XML_NAMESPACE => "xml:",
            _ => return None,
        };
        if !is_xml_local_name(local) {
            return None;
        }
        output.push(' ')?;
        output.push_str(prefix)?;
        output.push_str(local)?;
        output.push_str("=\"")?;
        for character in value.chars() {
            match character {
                '&' => output.push_str("&amp;"),
                '<' => output.push_str("&lt;"),
                '>' => output.push_str("&gt;"),
                '"' => output.push_str("&quot;"),
                '\t' => output.push_str("&#9;"),
                '\n' => output.push_str("&#10;"),
                '\r' => output.push_str("&#13;"),
                character if is_xml_char(character) => output.push(character),
                _ => return None,
            }?;
        }
        output.push('"')?;
    }
    Some(())
}

fn write_text(output: &mut SvgOutput, text: &str) -> Option<()> {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\r' => output.push_str("&#13;"),
            character if is_xml_char(character) => output.push(character),
            _ => return None,
        }?;
    }
    Some(())
}

/// Writes a comment XML can hold: no `--` inside, no `-` at the end.
fn write_comment(output: &mut SvgOutput, text: &str) -> Option<()> {
    if text.contains("--") || text.ends_with('-') || !text.chars().all(is_xml_char) {
        return None;
    }
    output.push_str("<!--")?;
    output.push_str(text)?;
    output.push_str("-->")?;
    Some(())
}

/// A character XML 1.0 allows in a document.
fn is_xml_char(character: char) -> bool {
    matches!(
        character,
        '\t' | '\n'
            | '\r'
            | '\u{20}'..='\u{d7ff}'
            | '\u{e000}'..='\u{fffd}'
            | '\u{10000}'..='\u{10ffff}'
    )
}

/// A name XML can write without a prefix. Conservative: a name it refuses
/// is never written, and anything this admits that XML reads otherwise
/// fails the equivalence check.
fn is_xml_local_name(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
        && characters
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: usize = 1024 * 1024;

    fn signed(fragment: &str) -> Result<Cow<'_, str>, Reason> {
        signable_svg(fragment, LIMIT)
    }

    /// The serialization `fragment` is signed as, checked to be a standalone
    /// SVG document whose tree, read as XML and again as HTML, is the
    /// browser's tree of `fragment`.
    fn normalized(fragment: &str) -> String {
        let Ok(Cow::Owned(serialized)) = signed(fragment) else {
            panic!("should serialize {fragment}, got {:?}", signed(fragment));
        };
        let browser = parse_html(fragment).expect("should parse the original as HTML");
        let browser = browser.root().expect("should have an svg root");
        let reparsed = parse_html(&serialized).expect("should parse the serialization as HTML");
        let reparsed = reparsed.root().expect("should keep one svg root");
        let document = parse_xml(&serialized).expect("should be standalone XML");
        assert_eq!(
            document.root_element().tag_name().namespace(),
            Some(SVG_NAMESPACE),
            "should declare the SVG namespace"
        );
        assert!(
            equal(&Browser, browser, &Browser, reparsed),
            "should read back as the browser's tree: {serialized}"
        );
        assert!(
            equal(
                &Browser,
                browser,
                &Xml(PhantomData),
                document.root_element()
            ),
            "should read as XML to the browser's tree: {serialized}"
        );
        serialized
    }

    fn assert_unchanged(fragment: &str) {
        assert!(
            matches!(signed(fragment), Ok(Cow::Borrowed(kept)) if kept == fragment),
            "should sign {fragment} as written, got {:?}",
            signed(fragment)
        );
    }

    /// The first SVG element named `name` in `serialized`, read as XML.
    fn element<'a, 'input>(
        document: &'a roxmltree::Document<'input>,
        name: &str,
    ) -> roxmltree::Node<'a, 'input> {
        document
            .descendants()
            .find(|node| node.has_tag_name((SVG_NAMESPACE, name)))
            .unwrap_or_else(|| panic!("should hold a {name} element"))
    }

    #[test]
    fn the_first_of_two_case_variant_attributes_is_the_one_signed() {
        // The captured logo: the browser keeps `viewbox`'s value as
        // `viewBox` and drops the later `viewBox`.
        let serialized = normalized(
            "<svg viewbox=\"0 0 300 300.251\" version=\"1.1\" xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 300 300.25\">\n <path d=\"M0 0h1\"/>\n</svg>",
        );
        let document = parse_xml(&serialized).expect("should be XML");
        let root = document.root_element();
        assert_eq!(root.attribute("viewBox"), Some("0 0 300 300.251"));
        assert_eq!(root.attribute("viewbox"), None);
        assert_eq!(root.attribute("version"), Some("1.1"));
        assert_eq!(element(&document, "path").attribute("d"), Some("M0 0h1"));
    }

    #[test]
    fn an_svg_without_its_namespace_is_signed_in_it() {
        let serialized = normalized(
            "<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\" class=\"bi\" width=\"16\" height=\"16\" fill=\"currentColor\"><g><path d=\"M18.244 2.25h3.308\"></path></g></svg>",
        );
        let document = parse_xml(&serialized).expect("should be XML");
        let root = document.root_element();
        assert_eq!(root.attribute("fill"), Some("currentColor"));
        assert_eq!(root.attribute("aria-hidden"), Some("true"));
        assert_eq!(
            element(&document, "path").attribute("d"),
            Some("M18.244 2.25h3.308")
        );
    }

    #[test]
    fn a_standalone_svg_is_signed_as_written_whatever_its_declarations() {
        assert_unchanged(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"16\" height=\"16\" fill=\"currentColor\" class=\"bi bi-search\" viewBox=\"0 0 16 16\"><path d=\"M11.742 10.344\"/></svg>",
        );
        assert_unchanged(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" xml:space=\"preserve\"><use xlink:href=\"#a\"/></svg>",
        );
        assert_unchanged(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><style>.st0{fill:currentColor}a>b{}</style><path class=\"st0\" d=\"M0 0\"/><!-- art --></svg>",
        );
        assert_unchanged(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><text>&#038;&#8217;&amp;</text></svg>",
        );
    }

    #[test]
    fn an_xlink_attribute_is_signed_in_the_xlink_namespace() {
        for fragment in [
            "<svg><use xlink:href=\"#a\"/></svg>",
            // A wrong binding is a declaration the browser ignores.
            "<svg xmlns:xlink=\"urn:wrong\"><use xlink:href=\"#a\"/></svg>",
        ] {
            let serialized = normalized(fragment);
            let document = parse_xml(&serialized).expect("should be XML");
            assert_eq!(
                element(&document, "use").attribute((XLINK_NAMESPACE, "href")),
                Some("#a"),
                "for {fragment}"
            );
        }
        assert!(
            !normalized("<svg><rect/></svg>").contains("xmlns:xlink"),
            "should declare XLink only where it is used"
        );
    }

    #[test]
    fn wrapped_attribute_whitespace_keeps_its_parsed_value() {
        let serialized = normalized(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><path d=\"M0 0\n L1 1\tZ\"/></svg>",
        );
        let document = parse_xml(&serialized).expect("should be XML");
        assert_eq!(
            element(&document, "path").attribute("d"),
            Some("M0 0\n L1 1\tZ")
        );
    }

    #[test]
    fn html_character_references_are_signed_as_their_characters() {
        let serialized = normalized(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><title>A&nbsp;&amp;&lt;B</title><text x=\"&quot;1&quot;\">&#8217;</text></svg>",
        );
        let document = parse_xml(&serialized).expect("should be XML");
        assert_eq!(element(&document, "title").text(), Some("A\u{a0}&<B"));
        let text = element(&document, "text");
        assert_eq!(text.attribute("x"), Some("\"1\""));
        assert_eq!(text.text(), Some("\u{2019}"));
    }

    #[test]
    fn a_nul_reference_is_the_browser_s_replacement_character() {
        let serialized = normalized("<svg><text>a&#0;b</text></svg>");
        let document = parse_xml(&serialized).expect("should be XML");
        assert_eq!(element(&document, "text").text(), Some("a\u{fffd}b"));
    }

    #[test]
    fn cdata_is_signed_as_its_text() {
        let serialized = normalized("<svg><style><![CDATA[a<b]]></style></svg>");
        let document = parse_xml(&serialized).expect("should be XML");
        assert_eq!(element(&document, "style").text(), Some("a<b"));
    }

    #[test]
    fn a_doctype_is_never_resolved_or_carried() {
        let serialized =
            normalized("<svg><!DOCTYPE svg SYSTEM \"file:///etc/passwd\"><text>&e;</text></svg>");
        assert!(!serialized.contains("DOCTYPE"), "should carry no doctype");
        let document = parse_xml(&serialized).expect("should be XML");
        assert_eq!(element(&document, "text").text(), Some("&e;"));
    }

    #[test]
    fn markup_that_breaks_out_of_the_svg_is_refused() {
        for fragment in [
            "<svg><p>x</p></svg>",
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><g><div>x</div></g></svg>",
            "<svg><foreignObject><div>x</div></foreignObject></svg>",
            "<svg><title><b>x</b></title></svg>",
        ] {
            assert_eq!(
                signed(fragment),
                Err(Reason::InlineSvgNotEquivalent),
                "should refuse {fragment}"
            );
        }
    }

    #[test]
    fn what_xml_cannot_carry_is_refused() {
        for fragment in [
            "<svg><!--a--b--></svg>",
            "<svg><!--a---></svg>",
            "<svg><text>&#1;</text></svg>",
            "<svg><text>&#xFFFE;</text></svg>",
            "<svg xmlns:sodipodi=\"urn:s\" sodipodi:docname=\"a\"></svg>",
            "<svg><sodipodi:namedview/></svg>",
        ] {
            assert_eq!(
                signed(fragment),
                Err(Reason::InlineSvgNotEquivalent),
                "should refuse {fragment}"
            );
        }
    }

    #[test]
    fn provenance_the_svg_carries_is_never_rewritten() {
        for fragment in [
            // The writer's carrier, without and with the SVG namespace.
            "<svg viewBox=\"0 0 1 1\"><metadata><c2pa:manifest xmlns:c2pa=\"http://c2pa.org/manifest\">QUJD</c2pa:manifest></metadata></svg>",
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><metadata><c2pa:manifest xmlns:c2pa=\"http://c2pa.org/manifest\">QUJD</c2pa:manifest></metadata></svg>",
            // An unclosed carrier element.
            "<svg><metadata><c2pa:manifest>QUJD</metadata></svg>",
            // A C2PA namespace declared for anything.
            "<svg><metadata xmlns:c=\"https://c2pa.org/manifest\"/></svg>",
            // An XMP provenance reference, which need not name C2PA.
            "<svg><metadata><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:dcterms=\"http://purl.org/dc/terms/\"><rdf:Description dcterms:provenance=\"https://example.com/m\"/></rdf:RDF></metadata></svg>",
            "<svg><metadata><dcterms:provenance>https://example.com/m</dcterms:provenance></metadata></svg>",
        ] {
            assert_eq!(
                signed(fragment),
                Err(Reason::SvgProvenanceNotEquivalent),
                "should leave {fragment} as written"
            );
        }
    }

    #[test]
    fn a_caption_that_mentions_c2pa_is_not_provenance() {
        let serialized = normalized(
            "<svg><title>Signed with C2PA</title><text class=\"c2pa\">c2pa</text></svg>",
        );
        let document = parse_xml(&serialized).expect("should be XML");
        assert_eq!(element(&document, "title").text(), Some("Signed with C2PA"));
    }

    #[test]
    fn serializer_checks_root_attributes_text_and_namespace_growth_before_writing() {
        let fragment = "<svg data-label='\"&amp;&lt;&gt;'><use xlink:href=\"#a\"/><text>&gt;&amp;</text><!-- art --></svg>";
        let parsed = parse_html(fragment).expect("should parse");
        let root = parsed.root().expect("should have a root");
        let serialized = serialize(root, LIMIT).expect("should serialize");
        assert!(serialized.contains(SVG_DECLARATION));
        assert!(serialized.contains(XLINK_DECLARATION));
        for limit in 0..serialized.len() {
            assert!(
                serialize(root, limit).is_none(),
                "must refuse limit {limit}"
            );
        }
        assert_eq!(serialize(root, serialized.len()), Some(serialized));

        let fragment = format!("<svg data-label='{}'/>", "\"".repeat(1000));
        let parsed = parse_html(&fragment).expect("should parse a large root attribute");
        let Node::Element(element) = parsed.root().expect("should have a root").value() else {
            panic!("should be an element");
        };
        let mut output = SvgOutput::new(127);
        assert!(write_start(&mut output, element, &mut false).is_none());
        assert!(output.text.len() <= 127);
        assert!(output.text.capacity() <= 127);
        assert_eq!(
            signable_svg(&fragment, 1100),
            Err(Reason::InlineSvgNotEquivalent)
        );

        let fragment = format!("<svg><text>{}</text></svg>", ">".repeat(1000));
        assert_eq!(
            signable_svg(&fragment, 1100),
            Err(Reason::InlineSvgNotEquivalent)
        );
    }

    #[test]
    fn serializer_buffer_growth_is_bounded_amortized_and_not_the_media_limit() {
        let mut output = SvgOutput::new(5_000_000);
        assert_eq!(output.text.capacity(), 0);
        output.push_str("<svg").expect("should write a small start");
        assert_eq!(output.text.capacity(), 64);
        assert!(
            output.reserve(usize::MAX).is_none(),
            "must reject length overflow"
        );
        assert_eq!(output.text, "<svg");

        let mut output = SvgOutput::new(129);
        let mut allocations = 0;
        for _ in 0..129 {
            let before = output.text.capacity();
            output.push('x').expect("should fit exactly");
            allocations += usize::from(output.text.capacity() != before);
            assert!(output.text.capacity() <= 129);
        }
        assert_eq!(
            allocations, 3,
            "should grow to 64, 128, then the bounded 129"
        );
        assert!(output.push('x').is_none());
        assert!(output.insert_str(0, SVG_DECLARATION).is_none());
        assert_eq!(output.text.len(), 129);

        let mut output = SvgOutput::new(5);
        output.push('x').expect("should fit");
        assert!(output.push('\u{1f600}').is_some(), "must count UTF-8 bytes");
        assert!(output.push('x').is_none());
        assert_eq!(output.text.capacity(), 5);
        assert!(SvgOutput::new(0).push('<').is_none());
    }

    #[test]
    fn text_comment_and_escape_writers_never_append_beyond_the_limit() {
        for (text, escaped) in [
            ("&", "&amp;"),
            ("<", "&lt;"),
            (">", "&gt;"),
            ("\r", "&#13;"),
        ] {
            for limit in 0..escaped.len() {
                let mut output = SvgOutput::new(limit);
                assert!(write_text(&mut output, text).is_none());
                assert!(output.text.len() <= limit);
                assert!(output.text.capacity() <= limit);
            }
            let mut output = SvgOutput::new(escaped.len());
            write_text(&mut output, text).expect("should fit the complete escape");
            assert_eq!(output.text, escaped);
        }
        for limit in 0..10 {
            let mut output = SvgOutput::new(limit);
            assert!(write_comment(&mut output, "art").is_none());
            assert!(output.text.len() <= limit);
            assert!(output.text.capacity() <= limit);
        }
        let mut output = SvgOutput::new(10);
        write_comment(&mut output, "art").expect("should fit the complete comment");
        assert_eq!(output.text, "<!--art-->");
    }

    #[test]
    fn the_size_node_and_depth_bounds_refuse() {
        let fragment = "<svg xmlns=\"http://www.w3.org/2000/svg\"/>";
        assert_eq!(
            signable_svg(fragment, fragment.len() - 1),
            Err(Reason::MediaTooLarge)
        );
        let wide = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\">{}</svg>",
            "<g/>".repeat(MAX_NODES)
        );
        assert_eq!(signed(&wide), Err(Reason::InlineSvgNotEquivalent));
        let deep = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\">{}{}</svg>",
            "<g>".repeat(MAX_DEPTH),
            "</g>".repeat(MAX_DEPTH)
        );
        assert_eq!(signed(&deep), Err(Reason::InlineSvgNotEquivalent));
        let nested = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\">{}{}</svg>",
            "<g>".repeat(MAX_DEPTH - 2),
            "</g>".repeat(MAX_DEPTH - 2)
        );
        assert_unchanged(&nested);
    }
}
