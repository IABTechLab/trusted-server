//! Page changes, which a module makes to a document on its way to a reader.
//!
//! A module changes a page through a middleware and in no other way. It
//! registers the ones it supplies, and one runs only where an entry in the
//! settings names it. Each phase has its own ordered list of entries. An
//! entry covers one media type, optionally only the requests under a path
//! prefix, and names the middleware to run in order. A response takes the
//! first entry that covers it, and each middleware is handed the document as
//! the ones before it left it.
//!
//! ```toml
//! [[fetch]]
//! media_type = "text/html"
//! path = "/news/"
//! middleware = ["example.strip", "example.tag"]
//!
//! [[fetch]]
//! media_type = "text/html"
//! middleware = ["example.strip"]
//!
//! [[serve]]
//! media_type = "text/html"
//! middleware = ["example.reader"]
//! ```
//!
//! An entry carries no settings and switches nothing on. Whether a module
//! runs is decided by the section that selects it, and an entry places and
//! orders the page changes of the modules that run. A middleware reads its
//! settings from its module's own table.
//!
//! [`MiddlewarePhase`] says when each phase runs.
//!
//! A middleware whose decision needs the request's services, or has to wait
//! on a store or the network, prepares. Before the document is processed it
//! is called in the request's handler, and what it answers is kept for its
//! `create` under its phase and name. It may refuse the request instead,
//! which is then logged and answered with the status it gives and no
//! document, and nothing is stored.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use http::{HeaderName, HeaderValue, StatusCode};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::integrations::{AttributeRewriteAction, IntegrationDocumentState, ScriptRewriteAction};
use crate::module_context::{ModuleCall, ModuleContext};
use crate::permissions::PermissionSet;
use crate::platform::PlatformIdentity;
use crate::streaming_processor::StreamProcessor;

/// The media type a document is matched as, and the one a middleware runs on
/// unless it says it handles another.
pub const HTML_MEDIA_TYPE: &str = "text/html";

/// Whether a middleware may run on a response of `media_type`, written as a
/// bare `type/subtype` in lower case.
///
/// HTML and the text assets that stream through core as text: every `text/`
/// type except `text/x-component`, and JavaScript and JSON.
#[must_use]
pub fn is_dispatched_media_type(media_type: &str) -> bool {
    media_type != "text/x-component"
        && (media_type.starts_with("text/")
            || matches!(media_type, "application/javascript" | "application/json"))
}

/// The type and subtype of a content type header, in lower case and without
/// parameters, which is what an entry's `media_type` is matched against.
/// `text/html; charset=utf-8` gives `text/html`.
#[must_use]
pub fn media_type_of(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// Whether a content type declares no charset or declares UTF-8, the one a
/// middleware's stream processor is handed. A body in another charset passes
/// through untouched, because a processor that writes UTF-8 into it would
/// corrupt it.
#[must_use]
pub fn is_utf8_or_undeclared_charset(content_type: &str) -> bool {
    content_type.split(';').skip(1).all(|parameter| {
        parameter.split_once('=').is_none_or(|(name, value)| {
            !name.trim().eq_ignore_ascii_case("charset")
                || value.trim().trim_matches('"').eq_ignore_ascii_case("utf-8")
        })
    })
}

/// When a middleware runs, relative to the store of shared templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MiddlewarePhase {
    /// On the document as the origin sent it. What is left is what a shared
    /// template stores, so a fetch middleware is handed nothing a request
    /// left.
    Fetch,
    /// On one reader's copy, on its way out, whether it came from the store
    /// or from the origin. A serve middleware is handed what the modules'
    /// request preparers left for this reader's request, and nothing written
    /// here is stored.
    Serve,
}

impl MiddlewarePhase {
    /// Both phases, in the order they happen.
    pub const ALL: [Self; 2] = [Self::Fetch, Self::Serve];

    /// The name the phase's entries are written under in the settings.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::Serve => "serve",
        }
    }
}

impl fmt::Display for MiddlewarePhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// What a middleware is told about the document it is asked to change.
#[derive(Debug, Clone, Copy)]
pub struct MiddlewareContext<'a> {
    /// Which phase is running.
    pub phase: MiddlewarePhase,
    /// Publisher-facing host the reader asked for.
    pub request_host: &'a str,
    /// Publisher-facing scheme the reader asked for.
    pub request_scheme: &'a str,
    /// Host the document was fetched from, the same for a page served from
    /// the store as for one fetched for this request.
    pub origin_host: &'a str,
    /// What the adapter reports about the platform the deployment runs on.
    pub platform: &'a PlatformIdentity,
    /// State shared by the middleware working on this document in this
    /// phase.
    pub document_state: &'a IntegrationDocumentState,
    /// The most a middleware may hold of one script while it decides.
    pub max_buffered_script_bytes: usize,
    /// What the middleware prepared for this document, see
    /// [`Middleware::prepare`].
    pub prepared: &'a Prepared,
}

impl MiddlewareContext<'_> {
    /// What the middleware named `id` prepared for this phase, see
    /// [`Prepared::take`].
    #[must_use]
    pub fn take_prepared<T: Any>(&self, id: &str) -> Option<T> {
        self.prepared.take(self.phase, id)
    }
}

/// One attribute of one matched element, as a handler is asked about it.
#[derive(Debug, Clone, Copy)]
pub struct MatchedAttribute<'a> {
    /// The element's tag name, in lower case.
    pub element_name: &'a str,
    /// The attribute the handler judges.
    pub attribute_name: &'a str,
    /// The attribute's value, as the handlers before this one left it. Core
    /// has already moved the origin's address in it to the reader's host, in
    /// either phase.
    pub value: &'a str,
}

/// A middleware's decision about one attribute of the elements a selector
/// matches, being to keep the element, replace the attribute's value or
/// remove the element.
pub trait ElementHandler {
    /// The CSS selector of the elements this handler is asked about.
    fn selector(&self) -> &str;

    /// The attribute whose value the handler judges. An element the selector
    /// matches that carries no such attribute is left as it is.
    fn attribute(&self) -> &str;

    /// Decides what happens to one matched element.
    fn decide(&mut self, matched: &MatchedAttribute<'_>) -> AttributeRewriteAction;
}

/// The question an [`AttributeRewrite`] asks about each value.
pub type AttributeRewriteFn = dyn Fn(&MatchedAttribute<'_>) -> AttributeRewriteAction;

/// An [`ElementHandler`] made from a function, such as one that points a
/// vendor's script at a first-party path.
pub struct AttributeRewrite {
    selector: String,
    attribute: &'static str,
    decide: Rc<AttributeRewriteFn>,
}

impl AttributeRewrite {
    /// Asks `decide` about `attribute` on every element that carries it.
    #[must_use]
    pub fn new(attribute: &'static str, decide: Rc<AttributeRewriteFn>) -> Self {
        Self::matching(&format!("[{attribute}]"), attribute, decide)
    }

    /// Asks `decide` about `attribute` on the elements `selector` matches.
    #[must_use]
    pub fn matching(
        selector: &str,
        attribute: &'static str,
        decide: Rc<AttributeRewriteFn>,
    ) -> Self {
        Self {
            selector: selector.to_owned(),
            attribute,
            decide,
        }
    }

    /// One handler for each of `attributes`, each asking `decide` about every
    /// element that carries it, for a decision that is the same wherever an
    /// address is written.
    #[must_use]
    pub fn each(
        attributes: &[&'static str],
        decide: &Rc<AttributeRewriteFn>,
    ) -> Vec<Box<dyn ElementHandler>> {
        attributes
            .iter()
            .map(|attribute| {
                Box::new(Self::new(attribute, Rc::clone(decide))) as Box<dyn ElementHandler>
            })
            .collect()
    }
}

impl ElementHandler for AttributeRewrite {
    fn selector(&self) -> &str {
        &self.selector
    }

    fn attribute(&self) -> &str {
        self.attribute
    }

    fn decide(&mut self, matched: &MatchedAttribute<'_>) -> AttributeRewriteAction {
        (self.decide)(matched)
    }
}

/// A middleware's decision about the text inside the elements a selector
/// matches, being to keep it, replace it or remove it.
///
/// Text arrives in chunks. A handler that judges a whole script removes each
/// chunk as it arrives while keeping a copy, and writes back what it kept, as
/// it wants it, with the last chunk.
pub trait TextHandler {
    /// The CSS selector of the elements whose text this handler is asked
    /// about.
    fn selector(&self) -> &str;

    /// Decides what happens to one chunk of text. `text` is the chunk as the
    /// handlers before this one left it, and `is_last` says whether it is the
    /// last chunk of its text node.
    fn decide(&mut self, text: &str, is_last: bool) -> ScriptRewriteAction;
}

/// A header a middleware writes on the reader's response.
pub type ResponseHeader = (HeaderName, HeaderValue);

/// Writes `headers` on `response`, each replacing a header of the same name.
pub fn write_response_headers<T>(response: &mut http::Response<T>, headers: Vec<ResponseHeader>) {
    for (name, value) in headers {
        response.headers_mut().insert(name, value);
    }
}

/// What a middleware does to one document and its response. A middleware
/// may do several of these at once, and an action with nothing set leaves
/// the document as it arrived.
#[derive(Default)]
pub struct MiddlewareAction {
    /// Markup to write in `<head>` ahead of the script bundle, in the order
    /// given. A document with no `<head>` gets none.
    pub head_inserts: Vec<String>,
    /// Markup to write straight after the main script bundle and before any
    /// deferred one, for a script that needs the bundle to have run and has
    /// to run before the page's own scripts.
    pub after_bundle_inserts: Vec<String>,
    /// Markup to write immediately after the opening `<body>` tag, once per
    /// document, in the order given, for a vendor's installation whose second
    /// part goes there and nowhere else. A document with no `<body>` gets
    /// none, and a second `<body>` in a template fragment gets nothing.
    pub body_open_inserts: Vec<String>,
    /// Decisions about elements.
    pub element_handlers: Vec<Box<dyn ElementHandler>>,
    /// Decisions about the text inside elements.
    pub text_handlers: Vec<Box<dyn TextHandler>>,
    /// A processor over the document the handlers left.
    pub stream: Option<Box<dyn StreamProcessor>>,
    /// Headers to write on the reader's response, each replacing one of the
    /// same name. Only a serve middleware may write one, because what a
    /// fetch middleware leaves is stored for every reader and a header goes
    /// on one reader's response.
    pub response_headers: Vec<ResponseHeader>,
}

impl MiddlewareAction {
    /// Nothing to do for this document.
    #[must_use]
    pub fn pass() -> Self {
        Self::default()
    }

    /// Whether the action leaves the document as it arrived.
    #[must_use]
    pub fn is_pass(&self) -> bool {
        self.is_stream_only() && self.stream.is_none() && self.response_headers.is_empty()
    }

    /// Whether the action decides nothing about HTML, so it can run on a
    /// media type that is not HTML.
    #[must_use]
    pub fn is_stream_only(&self) -> bool {
        self.head_inserts.is_empty()
            && self.after_bundle_inserts.is_empty()
            && self.body_open_inserts.is_empty()
            && self.element_handlers.is_empty()
            && self.text_handlers.is_empty()
    }

    /// Checks every handler's selector parses, before a handler is built
    /// from it.
    fn validate_selectors(&self) -> Result<(), String> {
        let check = |selector: &str| {
            selector
                .parse::<lol_html::Selector>()
                .map(|_| ())
                .map_err(|error| format!("`{selector}` is not a usable selector: {error}"))
        };
        for handler in &self.element_handlers {
            check(handler.selector())?;
        }
        for handler in &self.text_handlers {
            check(handler.selector())?;
        }
        Ok(())
    }
}

/// The names of `headers`, for a debug view.
fn header_names(headers: &[ResponseHeader]) -> Vec<&str> {
    headers.iter().map(|(name, _)| name.as_str()).collect()
}

impl fmt::Debug for MiddlewareAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MiddlewareAction")
            .field("head_inserts", &self.head_inserts)
            .field("after_bundle_inserts", &self.after_bundle_inserts)
            .field("body_open_inserts", &self.body_open_inserts)
            .field(
                "element_handlers",
                &selectors_of_elements(&self.element_handlers),
            )
            .field("text_handlers", &selectors_of_text(&self.text_handlers))
            .field("stream", &self.stream.is_some())
            .field("response_headers", &header_names(&self.response_headers))
            .finish()
    }
}

fn selectors_of_elements(handlers: &[Box<dyn ElementHandler>]) -> Vec<String> {
    handlers
        .iter()
        .map(|handler| format!("{}@{}", handler.selector(), handler.attribute()))
        .collect()
}

fn selectors_of_text(handlers: &[Box<dyn TextHandler>]) -> Vec<String> {
    handlers
        .iter()
        .map(|handler| handler.selector().to_owned())
        .collect()
}

/// A middleware's answer to a request it gives no document to, see
/// [`Middleware::prepare`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The status the request is answered with, a client or server error.
    /// Any other is answered as `500 Internal Server Error`.
    pub status: StatusCode,
    /// What `Retry-After` says, for a status that asks the caller back.
    pub retry_after: Option<Duration>,
    /// What the response says, as plain text.
    pub message: String,
}

impl Refusal {
    /// A `503 Service Unavailable` asking the caller back after
    /// `retry_after`.
    #[must_use]
    pub fn service_unavailable(retry_after: Duration, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            retry_after: Some(retry_after),
            message: message.into(),
        }
    }

    /// The refusal as the request is answered with it, logged once as a
    /// warning naming `middleware`. A status that is not a client or server
    /// error becomes `500 Internal Server Error`, because a refusal gives no
    /// document.
    fn checked(mut self, middleware: &str, phase: MiddlewarePhase) -> Self {
        if self.status.is_client_error() || self.status.is_server_error() {
            log::warn!(
                "The middleware `{middleware}` refused the request in the {phase} phase with {}",
                self.status
            );
        } else {
            log::warn!(
                "The middleware `{middleware}` refused the request in the {phase} phase with {}, \
                 which a refusal may not use, so it is answered with {}",
                self.status,
                StatusCode::INTERNAL_SERVER_ERROR
            );
            self.status = StatusCode::INTERNAL_SERVER_ERROR;
        }
        self
    }
}

/// What a middleware prepared for one document, see
/// [`Middleware::prepare`].
pub type Preparation<'a> = Pin<Box<dyn Future<Output = Result<Box<dyn Any + Send>, Refusal>> + 'a>>;

/// What the middleware of one document prepared, each under its own name
/// and phase, for its `create` in that phase to take.
#[derive(Default)]
pub struct Prepared(Mutex<PreparedValues>);

/// What each middleware prepared, under its phase and name.
type PreparedValues = HashMap<(MiddlewarePhase, &'static str), Box<dyn Any + Send>>;

impl Prepared {
    /// Keeps what the middleware named `id` prepared in `phase`.
    pub fn insert(&self, phase: MiddlewarePhase, id: &'static str, value: Box<dyn Any + Send>) {
        self.lock().insert((phase, id), value);
    }

    /// What the middleware named `id` prepared in `phase`, as the type it
    /// prepared, taken out so it is used once. `None` when it prepared
    /// nothing or something of another type.
    #[must_use]
    pub fn take<T: Any>(&self, phase: MiddlewarePhase, id: &str) -> Option<T> {
        let mut values = self.lock();
        let key = values
            .keys()
            .find(|(at, name)| *at == phase && *name == id)
            .copied()?;
        if !values.get(&key)?.is::<T>() {
            return None;
        }
        let value = values.remove(&key)?;
        value.downcast::<T>().ok().map(|value| *value)
    }

    /// Whether nothing was prepared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    fn lock(&self) -> MutexGuard<'_, PreparedValues> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for Prepared {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: Vec<String> = self
            .lock()
            .keys()
            .map(|(phase, name)| format!("{phase} {name}"))
            .collect();
        formatter
            .debug_struct("Prepared")
            .field("names", &names)
            .finish()
    }
}

/// A page change a module supplies.
///
/// A registered middleware holds no request state. For each document it is
/// asked once what it will do, and what it answers is used for that document
/// alone.
pub trait Middleware: Send + Sync {
    /// The name an entry selects this middleware by. A module names its
    /// middleware by its own name, as [`crate::module_name!`] gives it, with a
    /// part of its own after it when the module supplies several, and core
    /// names its own by a bare word.
    fn middleware_id(&self) -> &'static str;

    /// Whether this middleware works on a response of `media_type`, given as
    /// a bare `type/subtype`.
    fn handles_media_type(&self, media_type: &str) -> bool {
        media_type == HTML_MEDIA_TYPE
    }

    /// Whether this middleware works on a response to a request for `path`,
    /// every path by default. An entry's `path` is a prefix, so a middleware
    /// made for one file says which path is that file, and is left out of
    /// the chain for any other path the entry covers.
    fn handles_path(&self, path: &str) -> bool {
        let _ = path;
        true
    }

    /// Whether this middleware writes the whole document, whatever the origin
    /// sent. Where a `[[fetch]]` entry with a `path` names it, core makes the
    /// document on an empty one in place of any answer it would not hand the
    /// middleware as a `200` of the entry's media type, as it does where the
    /// origin answers `404`. `false` by default, for a middleware that changes
    /// what the origin sent.
    fn writes_whole_document(&self) -> bool {
        false
    }

    /// Whether this middleware needs the origin's whole body. Where an entry
    /// names it for a request's path, core asks the origin without the
    /// reader's conditional and range headers, as it does for a page the ad
    /// stack answers, because a `304` carries no body and a `206` only part of
    /// one. `false` by default.
    fn needs_whole_body(&self) -> bool {
        false
    }

    /// The phases an entry may name this middleware in.
    fn phases(&self) -> &[MiddlewarePhase];

    /// The permissions this middleware declares, which decide whether a
    /// gated value is passed to it in [`prepare`](Self::prepare). None by
    /// default.
    fn required_permissions(&self) -> PermissionSet {
        PermissionSet::none()
    }

    /// Work done for one document before `create` is asked, where deciding
    /// needs the request's services or has to wait, on a store or the
    /// network. `None`, the default, means none is needed.
    fn prepare<'a>(
        &'a self,
        call: ModuleCall<'a>,
        context: &'a MiddlewareContext<'a>,
    ) -> Option<Preparation<'a>> {
        let _ = (call, context);
        None
    }

    /// Decides what to do with one document.
    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction;

    /// Text a stored page is keyed by, being what this middleware writes
    /// that no setting decides, such as markup compiled into its crate. A
    /// change to it stores pages again, where otherwise a page stored before
    /// the change keeps the old markup until it expires. Empty when the
    /// settings decide everything the middleware writes.
    fn template_key(&self) -> &str {
        ""
    }
}

/// One entry of `[[fetch]]` or `[[serve]]`, being which responses it covers
/// and the middleware run on them, in the order written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhaseEntry {
    /// The media type the entry covers, written as a bare `type/subtype`.
    pub media_type: String,
    /// A prefix of the request path the entry covers, compared as written,
    /// or every path when absent. A folder is written with its closing
    /// slash, because `/news` also begins `/newsletter`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The middleware run on a response the entry covers, in the order
    /// written.
    pub middleware: Vec<String>,
}

impl PhaseEntry {
    /// Whether the entry covers a response of `media_type` to a request for
    /// `path`.
    #[must_use]
    pub fn covers(&self, media_type: &str, path: &str) -> bool {
        self.media_type == media_type && self.covers_path(path)
    }

    /// Whether the entry covers a request for `path`, whatever the media type
    /// of the response.
    #[must_use]
    pub fn covers_path(&self, path: &str) -> bool {
        self.path
            .as_deref()
            .is_none_or(|prefix| path.starts_with(prefix))
    }

    /// The prefix the paths the entry covers begin with. The path of a
    /// request for a page begins with `/`, so `/` covers the same pages as
    /// no path at all.
    fn prefix(&self) -> &str {
        self.path.as_deref().unwrap_or("/")
    }
}

/// One phase's entries, in the order written.
///
/// A response takes the first entry that covers it and runs that entry's
/// middleware alone, so an entry with a path goes before the entry covering
/// every path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PhaseEntries(Vec<PhaseEntry>);

impl PhaseEntries {
    /// Entries, in the order they are matched.
    #[must_use]
    pub fn new(entries: Vec<PhaseEntry>) -> Self {
        Self(entries)
    }

    /// The entries, in the order they are matched.
    #[must_use]
    pub fn entries(&self) -> &[PhaseEntry] {
        &self.0
    }

    /// Whether the phase has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The names the first entry covering a response selects, in the order
    /// they run, or none when no entry covers it.
    #[must_use]
    pub fn for_response(&self, media_type: &str, path: &str) -> &[String] {
        self.0
            .iter()
            .find(|entry| entry.covers(media_type, path))
            .map_or(&[][..], |entry| entry.middleware.as_slice())
    }

    /// Every name the entries select, each once, in the order first written.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = Vec::new();
        for entry in &self.0 {
            for name in &entry.middleware {
                if !names.contains(&name.as_str()) {
                    names.push(name);
                }
            }
        }
        names
    }

    /// The entry that makes the document the origin does not have at
    /// `path`, being the first written, for a media type core streams other
    /// than HTML, whose `path` is `path` itself and that names a middleware.
    /// A longer path the entry's `path` begins, and a folder, written with its
    /// closing slash, get nothing made, so a page the origin does not have
    /// keeps the origin's answer.
    #[must_use]
    pub fn making(&self, path: &str) -> Option<&PhaseEntry> {
        if path.ends_with('/') {
            return None;
        }
        self.0.iter().find(|entry| {
            entry.media_type != HTML_MEDIA_TYPE
                && is_dispatched_media_type(&entry.media_type)
                && entry.path.as_deref() == Some(path)
                && !entry.middleware.is_empty()
        })
    }

    /// Checks each entry's media type, path and names, and that no entry is
    /// hidden behind an earlier one, or covers the same pages as one.
    ///
    /// # Errors
    ///
    /// A message naming the entry by its position, and what is wrong with it.
    pub fn validate(&self, phase: MiddlewarePhase) -> Result<(), String> {
        for (index, entry) in self.0.iter().enumerate() {
            let at = format!("[[{phase}]] entry {}", index + 1);
            if entry.media_type != entry.media_type.to_ascii_lowercase()
                || entry
                    .media_type
                    .contains(|c: char| c == ';' || c == '*' || c.is_whitespace())
                || entry
                    .media_type
                    .split_once('/')
                    .is_none_or(|(_, subtype)| subtype.is_empty())
                || !is_dispatched_media_type(&entry.media_type)
            {
                return Err(format!(
                    "{at} covers `{}`, which is not a media type a middleware runs on. Write a \
                     bare `type/subtype` in lower case with no parameters, being `text/html`, \
                     another `text/` type other than `text/x-component`, \
                     `application/javascript` or `application/json`",
                    entry.media_type
                ));
            }
            if let Some(path) = &entry.path {
                if !path.starts_with('/') {
                    return Err(format!(
                        "{at}: path `{path}` must start with `/`, being a prefix of the request \
                         path"
                    ));
                }
                if path.contains(['*', '?', '#']) {
                    return Err(format!(
                        "{at}: path `{path}` is a prefix of the request path, compared as \
                         written, so it holds no pattern, query or fragment. Write `/news/` to \
                         cover everything under that folder"
                    ));
                }
            }
            if entry.middleware.is_empty() {
                return Err(format!(
                    "{at} names no middleware. Remove the entry rather than leaving an empty list"
                ));
            }
            for (position, name) in entry.middleware.iter().enumerate() {
                if !crate::module_name::is_valid(name) {
                    return Err(format!(
                        "{at} names `{name}`, which is not a middleware name. A name is parts \
                         joined by `.`, each in lower case letters, digits, `_` or `-`, such as \
                         `cmp.example`"
                    ));
                }
                if entry.middleware[..position].contains(name) {
                    return Err(format!("{at} names `{name}` more than once"));
                }
            }
            if let Some(earlier) = self.0[..index].iter().position(|earlier| {
                earlier.media_type == entry.media_type
                    && entry.prefix().starts_with(earlier.prefix())
            }) {
                return Err(if self.0[earlier].prefix() == entry.prefix() {
                    format!(
                        "{at} covers the same pages as [[{phase}]] entry {}, and a response \
                         takes the first entry that covers it, so this one is never reached. \
                         Name every middleware for those pages in one entry's list",
                        earlier + 1
                    )
                } else {
                    format!(
                        "{at} is never reached, because [[{phase}]] entry {} covers every \
                         response it covers and a response takes the first entry that covers \
                         it. Put the entry with the longer path first",
                        earlier + 1
                    )
                });
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for PhaseEntries {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let entries = Vec::<PhaseEntry>::deserialize(deserializer).map_err(|error| {
            de::Error::custom(format!(
                "{error}. A phase is a list of entries, each written as [[fetch]] or [[serve]] \
                 with media_type and middleware, and path where it covers part of the site"
            ))
        })?;
        Ok(Self(entries))
    }
}

impl Serialize for PhaseEntries {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

/// The middleware one entry selects, in the order they run, for one media
/// type.
#[derive(Clone)]
pub struct MiddlewareChain {
    phase: MiddlewarePhase,
    media_type: String,
    middleware: Vec<Arc<dyn Middleware>>,
}

impl fmt::Debug for MiddlewareChain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MiddlewareChain")
            .field("phase", &self.phase)
            .field("media_type", &self.media_type)
            .field("middleware", &self.ids())
            .finish()
    }
}

impl MiddlewareChain {
    /// A chain of middleware already in the order they run, for responses of
    /// `media_type`.
    #[must_use]
    pub fn new(
        phase: MiddlewarePhase,
        media_type: &str,
        middleware: Vec<Arc<dyn Middleware>>,
    ) -> Self {
        Self {
            phase,
            media_type: media_type.to_owned(),
            middleware,
        }
    }

    /// The phase this chain runs in.
    #[must_use]
    pub fn phase(&self) -> MiddlewarePhase {
        self.phase
    }

    /// The media type of the responses this chain runs on.
    #[must_use]
    pub fn media_type(&self) -> &str {
        &self.media_type
    }

    /// The middleware names, in the order they run.
    #[must_use]
    pub fn ids(&self) -> Vec<&'static str> {
        self.middleware
            .iter()
            .map(|middleware| middleware.middleware_id())
            .collect()
    }

    /// Whether the chain would do nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.middleware.is_empty()
    }

    /// Whether a middleware in the chain writes the whole document, see
    /// [`Middleware::writes_whole_document`].
    #[must_use]
    pub fn writes_whole_document(&self) -> bool {
        self.middleware
            .iter()
            .any(|middleware| middleware.writes_whole_document())
    }

    /// Runs the preparation of every middleware that has one, in the order
    /// they run, keeping what each prepared in the context's [`Prepared`].
    /// Each is called as the module it belongs to, with the permissions it
    /// declares.
    ///
    /// # Errors
    ///
    /// The first refusal, which ends the request. It is logged once, as a
    /// warning naming the middleware, and one whose status is not a client
    /// or server error is answered as `500 Internal Server Error`.
    pub async fn prepare(
        &self,
        module_context: &ModuleContext<'_>,
        context: &MiddlewareContext<'_>,
    ) -> Result<(), Refusal> {
        for middleware in &self.middleware {
            let id = middleware.middleware_id();
            let call = module_context.call(id, middleware.required_permissions());
            if let Some(preparation) = middleware.prepare(call, context) {
                let value = preparation
                    .await
                    .map_err(|refusal| refusal.checked(id, context.phase))?;
                context.prepared.insert(context.phase, id, value);
            }
        }
        Ok(())
    }

    /// Asks every middleware in turn what it will do with one document.
    ///
    /// Checks each handler's selector, refusing the document when one does
    /// not parse. On a media type other than HTML, an action that decides
    /// about HTML is refused the same way.
    ///
    /// # Errors
    ///
    /// A message naming the middleware and the selector that does not parse,
    /// or the HTML decision it made about a document that is not HTML.
    pub fn plan(&self, context: &MiddlewareContext<'_>) -> Result<MiddlewarePlan, String> {
        let mut plan = MiddlewarePlan::default();
        for middleware in &self.middleware {
            let action = middleware.create(context);
            action.validate_selectors().map_err(|problem| {
                format!(
                    "the {} middleware `{}` cannot run, because {problem}",
                    self.phase,
                    middleware.middleware_id()
                )
            })?;
            if self.media_type != HTML_MEDIA_TYPE && !action.is_stream_only() {
                return Err(format!(
                    "the {} middleware `{}` cannot run on `{}`, because it decides about HTML \
                     and that is not HTML. On another media type an action carries a stream \
                     processor alone",
                    self.phase,
                    middleware.middleware_id(),
                    self.media_type
                ));
            }
            if !action.response_headers.is_empty() && self.phase != MiddlewarePhase::Serve {
                return Err(format!(
                    "the {} middleware `{}` cannot run, because it writes a response header, \
                     which only a serve middleware does",
                    self.phase,
                    middleware.middleware_id()
                ));
            }
            plan.head_inserts.extend(action.head_inserts);
            plan.after_bundle_inserts
                .extend(action.after_bundle_inserts);
            plan.body_open_inserts.extend(action.body_open_inserts);
            plan.element_handlers.extend(action.element_handlers);
            plan.text_handlers.extend(action.text_handlers);
            plan.processors.extend(action.stream);
            plan.response_headers.extend(action.response_headers);
        }
        Ok(plan)
    }
}

/// What one document's chain will do, in the order it happens.
#[derive(Default)]
pub struct MiddlewarePlan {
    /// Markup for `<head>`, ahead of the script bundle, in the order it is
    /// written.
    pub head_inserts: Vec<String>,
    /// Markup for straight after the main script bundle, in the order it is
    /// written.
    pub after_bundle_inserts: Vec<String>,
    /// Markup for immediately after the opening `<body>` tag, in the order it
    /// is written.
    pub body_open_inserts: Vec<String>,
    /// Element handlers, in the order the middleware were named. Two that
    /// match one element are both asked, in this order.
    pub element_handlers: Vec<Box<dyn ElementHandler>>,
    /// Text handlers, in the order the middleware were named.
    pub text_handlers: Vec<Box<dyn TextHandler>>,
    /// Stream processors, each handed what the one before it produced.
    pub processors: Vec<Box<dyn StreamProcessor>>,
    /// Headers for the reader's response, in the order the middleware were
    /// named, a later one replacing an earlier of the same name.
    pub response_headers: Vec<ResponseHeader>,
}

impl MiddlewarePlan {
    /// Whether the chain decided to do nothing to this document.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.head_inserts.is_empty()
            && self.after_bundle_inserts.is_empty()
            && self.body_open_inserts.is_empty()
            && self.element_handlers.is_empty()
            && self.text_handlers.is_empty()
            && self.processors.is_empty()
            && self.response_headers.is_empty()
    }
}

impl fmt::Debug for MiddlewarePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MiddlewarePlan")
            .field("head_inserts", &self.head_inserts)
            .field("after_bundle_inserts", &self.after_bundle_inserts)
            .field("body_open_inserts", &self.body_open_inserts)
            .field(
                "element_handlers",
                &selectors_of_elements(&self.element_handlers),
            )
            .field("text_handlers", &selectors_of_text(&self.text_handlers))
            .field("processors", &self.processors.len())
            .field("response_headers", &header_names(&self.response_headers))
            .finish()
    }
}

/// What a module's own tests hand a middleware they call directly.
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support {
    use std::sync::OnceLock;

    use super::{MiddlewareContext, MiddlewarePhase};
    use crate::html_processor::test_support::{ORIGIN_HOST, REQUEST_HOST};
    use crate::integrations::IntegrationDocumentState;
    use crate::platform::PlatformIdentity;

    /// The platform a test page is served from, which reports its name and
    /// nothing else.
    #[must_use]
    pub fn platform() -> &'static PlatformIdentity {
        static PLATFORM: OnceLock<PlatformIdentity> = OnceLock::new();
        PLATFORM.get_or_init(|| PlatformIdentity::named("test-platform"))
    }

    /// What a middleware is told in `phase` about a test page, fetched from
    /// [`ORIGIN_HOST`] for a reader of [`REQUEST_HOST`] over HTTPS on
    /// [`platform`], with `document_state` as the document's.
    #[must_use]
    pub fn context(
        phase: MiddlewarePhase,
        document_state: &IntegrationDocumentState,
    ) -> MiddlewareContext<'_> {
        context_on(phase, document_state, platform())
    }

    /// As [`context`], on `platform`.
    #[must_use]
    pub fn context_on<'a>(
        phase: MiddlewarePhase,
        document_state: &'a IntegrationDocumentState,
        platform: &'a PlatformIdentity,
    ) -> MiddlewareContext<'a> {
        MiddlewareContext {
            phase,
            request_host: REQUEST_HOST,
            request_scheme: "https",
            origin_host: ORIGIN_HOST,
            platform,
            document_state,
            max_buffered_script_bytes: 16 * 1024 * 1024,
            prepared: nothing_prepared(),
        }
    }

    /// A [`Prepared`](super::Prepared) with nothing in it, for a middleware
    /// that prepares nothing.
    #[must_use]
    pub fn nothing_prepared() -> &'static super::Prepared {
        static NOTHING: std::sync::LazyLock<super::Prepared> =
            std::sync::LazyLock::new(super::Prepared::default);
        &NOTHING
    }
}

#[cfg(test)]
mod tests;
