//! Simplified HTML processor that combines URL replacement and integration injection
//!
//! This module provides a `StreamProcessor` implementation for HTML content.
use std::cell::{Cell, RefCell};
use std::io;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lol_html::{
    EndTagHandler, Settings as RewriterSettings, element, end,
    html_content::{ContentType, EndTag, TextChunk, TextType},
    text,
};

use crate::integrations::{
    AttributeRewriteAction, IntegrationDocumentState, IntegrationRegistry, IntegrationRequestState,
    ScriptRewriteAction,
};
use crate::middleware::{
    ElementHandler, MatchedAttribute, MiddlewareChain, MiddlewareContext, MiddlewarePlan, Prepared,
    ResponseHeader, TextHandler,
};
use crate::platform::PlatformIdentity;
use crate::prefix::{PUBLIC_PREFIX_ATTRIBUTE, PublicPrefix};
use crate::publisher::build_empty_bids_script;
use crate::settings::Settings;
use crate::streaming_processor::{HtmlRewriterAdapter, StreamProcessor};
use crate::tsjs;

mod script_text;

use script_text::ScriptTextGuard;

struct HtmlWithStreamingProcessors {
    inner: Box<dyn StreamProcessor>,
    processors: Vec<Box<dyn StreamProcessor>>,
}

impl StreamProcessor for HtmlWithStreamingProcessors {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> Result<Vec<u8>, io::Error> {
        let mut output = self.inner.process_chunk(chunk, is_last)?;
        for processor in &mut self.processors {
            output = processor.process_chunk(&output, is_last)?;
        }
        Ok(output)
    }

    fn reset(&mut self) {
        self.inner.reset();
        for processor in &mut self.processors {
            processor.reset();
        }
    }
}

/// What the `</body>` seam injects.
///
/// This is a decision, not a side effect of whether the `<head>` script exists.
/// An earlier shape gated body-close injection on `ad_slots_script.is_some()`,
/// which coupled two independent choices: once a shared-template mode stopped
/// emitting the head script, body-close injection silently stopped too.
///
/// See `docs/superpowers/archive/2026-08-08-esi-cacheable-root-validation-design.md`
/// §6.7.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum BodyCloseInjection {
    /// Emit nothing because no slots matched under the inline path.
    #[default]
    None,
    /// Read the auction result from `ad_bids_state` and inject it, falling back to
    /// an empty payload. Today's shipped behaviour.
    InlineBids,
    /// Emit a request-specific marker at a structural body end. The publisher
    /// streaming controller removes it after the auction completes.
    DeferredInlineMarker(String),
    /// Emit this markup verbatim — an inert marker the assembly step splits on.
    /// Must be identical for every request that reaches the transform, or the
    /// cached template is not shared-safe.
    Marker(String),
}

/// Configuration for HTML processing
#[derive(Clone)]
pub struct HtmlProcessorConfig {
    pub origin_host: String,
    pub request_host: String,
    pub request_scheme: String,
    pub integrations: IntegrationRegistry,
    /// Pre-computed
    /// `<script>(window.tsjs=window.tsjs||{}).permissions=...;</script>`.
    /// Injected at `<head>` open, ahead of [`Self::ad_slots_script`] and the
    /// tsjs bundle, so page code can read the request's permission state before
    /// anything runs. `None` under a shared-template mode, where the head is
    /// cached and served to many readers and nothing request-scoped may appear
    /// in it, so the seam carries the state there instead.
    pub permissions_script: Option<String>,
    /// Pre-computed `<script>(window.tsjs=window.tsjs||{}).adSlots=...;</script>`.
    /// Injected at `<head>` open. `None` when no slots matched.
    pub ad_slots_script: Option<String>,
    /// Shared auction result — written by auction task before HTML processing begins.
    /// Handler reads this in `el.on_end_tag()` on the body element.
    /// `None` means no auction ran; inject empty `tsjs.bids = {}` as fallback.
    pub ad_bids_state: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// Maximum bytes an integration may retain while processing one script or
    /// unresolved streaming group.
    pub max_buffered_body_bytes: usize,
    /// What modules left on the request, which the serve chain is handed
    /// where it runs on the same pass as the fetch chain. The fetch chain is
    /// handed none of it, because what that chain writes may be stored and
    /// served to other readers.
    pub request_state: IntegrationRequestState,
    /// What the `</body>` seam injects. Decided by the caller rather than inferred
    /// from [`Self::ad_slots_script`].
    pub body_close: BodyCloseInjection,
    /// Set when the document delivers a response-bound CSP nonce in its own markup.
    ///
    /// `None` on every path that cannot store a shared template, so an ordinary inline
    /// request does not pay for handlers whose only consumer is the template-cache gate.
    pub csp_nonce_observed: Option<Arc<AtomicBool>>,
    /// The deployment's public prefix for this document, written on the
    /// bundle tag so the client script names its cookies and storage entries
    /// after it, see [`crate::prefix`]. `None` writes no attribute.
    pub public_prefix: Option<PublicPrefix>,
    /// What the middleware prepared for this document, see
    /// [`Middleware::prepare`](crate::middleware::Middleware::prepare).
    pub prepared: Arc<Prepared>,
}

impl HtmlProcessorConfig {
    /// Create from settings and request parameters
    #[must_use]
    pub fn from_settings(
        settings: &Settings,
        integrations: &IntegrationRegistry,
        origin_host: &str,
        request_host: &str,
        request_scheme: &str,
    ) -> Self {
        Self {
            origin_host: origin_host.to_owned(),
            request_host: request_host.to_owned(),
            request_scheme: request_scheme.to_owned(),
            integrations: integrations.clone(),
            permissions_script: None,
            ad_slots_script: None,
            ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
            max_buffered_body_bytes: settings.publisher.max_buffered_body_bytes,
            request_state: IntegrationRequestState::default(),
            body_close: BodyCloseInjection::None,
            csp_nonce_observed: None,
            public_prefix: None,
            prepared: Arc::new(Prepared::default()),
        }
    }

    /// Attach the streaming-auction `<script>` payloads to a config built via
    /// [`HtmlProcessorConfig::from_settings`].
    ///
    /// Callers that drive the auction-hold streaming path use this rather than
    /// constructing [`HtmlProcessorConfig`] inline so the canonical
    /// [`from_settings`](Self::from_settings) builder stays the single source of
    /// truth: future fields added there are inherited automatically.
    #[must_use]
    pub fn with_ad_state(
        mut self,
        ad_slots_script: Option<String>,
        ad_bids_state: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    ) -> Self {
        self.ad_slots_script = ad_slots_script;
        self.ad_bids_state = ad_bids_state;
        self
    }

    /// Attach the head script carrying this request's permission state.
    ///
    /// Separate from [`with_ad_state`](Self::with_ad_state) because the two are
    /// independent decisions: the permission state travels on every HTML
    /// document the processor handles, whether or not the ad stack ran.
    #[must_use]
    pub fn with_permissions_script(mut self, permissions_script: Option<String>) -> Self {
        self.permissions_script = permissions_script;
        self
    }

    /// Set what the `</body>` seam injects.
    ///
    /// Separate from [`with_ad_state`](Self::with_ad_state) because the two are
    /// independent decisions: a shared-template mode emits no head script and
    /// still needs a body-close marker.
    #[must_use]
    pub fn with_body_close(mut self, body_close: BodyCloseInjection) -> Self {
        self.body_close = body_close;
        self
    }

    /// Attach what modules left on the request, for the serve chain.
    #[must_use]
    pub fn with_request_state(mut self, request_state: IntegrationRequestState) -> Self {
        self.request_state = request_state;
        self
    }

    /// Watch the document for a response-bound CSP nonce delivered in its own markup.
    ///
    /// Pass `Some` only when the completed transform may be stored as a shared template;
    /// nothing else reads the observation.
    #[must_use]
    pub fn with_csp_nonce_observer(mut self, observed: Option<Arc<AtomicBool>>) -> Self {
        self.csp_nonce_observed = observed;
        self
    }

    /// Sets the public prefix the bundle tag tells the client script.
    #[must_use]
    pub fn with_public_prefix(mut self, public_prefix: PublicPrefix) -> Self {
        self.public_prefix = Some(public_prefix);
        self
    }

    /// Attach what the middleware prepared for this document.
    #[must_use]
    pub fn with_prepared(mut self, prepared: Arc<Prepared>) -> Self {
        self.prepared = prepared;
        self
    }
}

/// The chunk of text being rewritten, as the last handler to change it left
/// it, shared by every text handler of one document.
///
/// The parser hands every handler a chunk as the origin sent it and writes
/// only the last replacement, so without this a handler would undo the change
/// of each handler before it.
#[derive(Default)]
struct TextEdits(RefCell<String>);

impl TextEdits {
    /// The chunk as the handlers before this one left it.
    fn current(&self, chunk: &TextChunk<'_>) -> String {
        if chunk.removed() {
            self.0.borrow().clone()
        } else {
            chunk.as_str().to_owned()
        }
    }

    /// Writes `text` in the chunk's place, as it is.
    ///
    /// Written as markup, because a handler is handed the text as the page
    /// wrote it. Writing it as text would escape `&`, `<` and `>`, and a
    /// browser decodes none of those inside a script.
    fn replace(&self, chunk: &mut TextChunk<'_>, text: &str) {
        chunk.replace(text, ContentType::Html);
        text.clone_into(&mut self.0.borrow_mut());
    }

    /// Leaves the chunk out, along with anything an earlier handler wrote in
    /// its place.
    fn remove(&self, chunk: &mut TextChunk<'_>) {
        self.replace(chunk, "");
    }
}

/// Create an HTML processor that moves the origin's address to the
/// publisher's and writes the script bundle, with no middleware.
///
/// # Panics
///
/// Panics if the `ad_bids_state` `Mutex` is poisoned. This cannot happen in
/// normal operation since no code holds the lock across a panic boundary.
#[must_use]
pub fn create_html_processor(config: HtmlProcessorConfig) -> impl StreamProcessor {
    build_html_processor(config, MiddlewarePlan::default())
}

/// [`create_html_processor`], with the middleware of the `fetch` chain run
/// on the document as well, and then those of the `serve` chain where the
/// document goes to one reader and is not stored.
///
/// `serve` is `None` for a document that may be stored, whose serve chain
/// runs on each reader's copy instead, through [`create_serve_processor`].
/// The headers returned are what the serve chain writes on the response,
/// none when `serve` is `None`.
///
/// # Errors
///
/// When a handler's selector does not parse, naming the selector.
///
/// # Panics
///
/// As [`create_html_processor`].
pub fn create_html_processor_with_middleware(
    config: HtmlProcessorConfig,
    fetch: &MiddlewareChain,
    serve: Option<&MiddlewareChain>,
) -> Result<(impl StreamProcessor + use<>, Vec<ResponseHeader>), String> {
    let document_state = IntegrationDocumentState::default();
    let plan = fetch.plan(&MiddlewareContext {
        phase: fetch.phase(),
        request_host: &config.request_host,
        request_scheme: &config.request_scheme,
        origin_host: &config.origin_host,
        platform: config.integrations.platform(),
        document_state: &document_state,
        max_buffered_script_bytes: config.max_buffered_body_bytes,
        prepared: &config.prepared,
    })?;
    let reader = match serve {
        Some(serve) => create_serve_processor(
            serve,
            &ReaderDocument {
                request_host: &config.request_host,
                request_scheme: &config.request_scheme,
                origin_host: &config.origin_host,
                platform: config.integrations.platform(),
                request_state: &config.request_state,
                max_buffered_script_bytes: config.max_buffered_body_bytes,
                prepared: &config.prepared,
            },
        )?,
        None => ServePass::default(),
    };
    let mut processor = build_html_processor(config, plan);
    processor.processors.extend(reader.processor);
    Ok((processor, reader.response_headers))
}

/// What a serve chain is told about the reader's copy it works on.
#[derive(Debug, Clone, Copy)]
pub struct ReaderDocument<'a> {
    /// Publisher-facing host the reader asked for.
    pub request_host: &'a str,
    /// Publisher-facing scheme the reader asked for.
    pub request_scheme: &'a str,
    /// The host the document names as its own, being the `Host` its source
    /// sends.
    pub origin_host: &'a str,
    /// What the adapter reports about the platform the deployment runs on.
    pub platform: &'a PlatformIdentity,
    /// What modules left on this reader's request for their page changes.
    pub request_state: &'a IntegrationRequestState,
    /// The most a middleware may hold of one script while it decides.
    pub max_buffered_script_bytes: usize,
    /// What the middleware prepared for this document, see
    /// [`Middleware::prepare`](crate::middleware::Middleware::prepare).
    pub prepared: &'a Prepared,
}

/// What a serve chain does to one reader's copy of a document core has
/// already rewritten, and to the reader's response.
///
/// Nothing of core's own is written or rewritten a second time. Head markup
/// goes straight before the script bundle's tag and markup for after the
/// bundle straight after it, so a document core wrote no bundle into gets
/// neither.
///
/// # Errors
///
/// When a handler's selector does not parse, naming the selector.
pub fn create_serve_processor(
    chain: &MiddlewareChain,
    document: &ReaderDocument<'_>,
) -> Result<ServePass, String> {
    if chain.is_empty() {
        return Ok(ServePass::default());
    }
    let document_state = IntegrationDocumentState::default();
    document.request_state.seed(&document_state);
    let mut plan = chain.plan(&MiddlewareContext {
        phase: chain.phase(),
        request_host: document.request_host,
        request_scheme: document.request_scheme,
        origin_host: document.origin_host,
        platform: document.platform,
        document_state: &document_state,
        max_buffered_script_bytes: document.max_buffered_script_bytes,
        prepared: document.prepared,
    })?;
    let response_headers = std::mem::take(&mut plan.response_headers);
    let processor: Option<Box<dyn StreamProcessor>> = if plan.is_empty() {
        None
    } else {
        Some(Box::new(build_serve_processor(plan)))
    };
    Ok(ServePass {
        processor,
        response_headers,
    })
}

/// What a serve chain does to one reader's response.
#[derive(Default)]
pub struct ServePass {
    /// The processor over the reader's copy, or `None` when the chain
    /// leaves the document as it arrived.
    pub processor: Option<Box<dyn StreamProcessor>>,
    /// The headers the chain writes on the response.
    pub response_headers: Vec<ResponseHeader>,
}

/// The tag of the script bundle, which core writes once at the start of
/// `<head>`.
const BUNDLE_SELECTOR: &str = "script#trustedserver-js";

/// One selector and what a handler does with what it matches.
type ContentHandler = (
    std::borrow::Cow<'static, lol_html::Selector>,
    lol_html::ElementContentHandlers<'static>,
);

/// The handlers that carry out a plan's element and text handlers.
///
/// The text handlers share `edits` with every text handler registered before
/// them, so each is handed a chunk as the one before it left it. A
/// [`ScriptTextGuard`] registered after them writes what they left in a
/// script, so that their text cannot end the script or keep its end tag from
/// closing it.
///
/// A selector that does not parse panics here, which
/// [`MiddlewareChain::plan`] rules out by refusing to plan one.
fn middleware_content_handlers(
    element_handlers: Vec<Box<dyn ElementHandler>>,
    text_handlers: Vec<Box<dyn TextHandler>>,
    edits: &Rc<TextEdits>,
) -> Vec<ContentHandler> {
    let mut handlers: Vec<ContentHandler> = Vec::new();
    for mut handler in element_handlers {
        let selector = handler.selector().to_owned();
        let attribute = handler.attribute().to_owned();
        handlers.push(element!(selector, move |el| {
            if el.removed() {
                return Ok(());
            }
            let Some(value) = el.get_attribute(&attribute) else {
                return Ok(());
            };
            let element_name = el.tag_name();
            match handler.decide(&MatchedAttribute {
                element_name: &element_name,
                attribute_name: &attribute,
                value: &value,
            }) {
                AttributeRewriteAction::Keep => {}
                AttributeRewriteAction::Replace(replacement) => {
                    if replacement != value {
                        el.set_attribute(&attribute, &replacement)?;
                    }
                }
                AttributeRewriteAction::RemoveElement => el.remove(),
            }
            Ok(())
        }));
    }
    let guarded = !text_handlers.is_empty();
    for mut handler in text_handlers {
        let selector = handler.selector().to_owned();
        let edits = Rc::clone(edits);
        handlers.push(text!(selector, move |text| {
            let current = edits.current(text);
            match handler.decide(&current, text.last_in_text_node()) {
                ScriptRewriteAction::Keep => {}
                ScriptRewriteAction::Replace(rewritten) => edits.replace(text, &rewritten),
                ScriptRewriteAction::RemoveNode => edits.remove(text),
            }
            Ok(())
        }));
    }
    if guarded {
        let edits = Rc::clone(edits);
        let mut guard = ScriptTextGuard::default();
        handlers.push(text!("script", move |text| {
            if text.text_type() == TextType::ScriptData {
                let written = edits.current(text);
                if let Some(safe) = guard.write(&written, text.removed(), text.last_in_text_node())
                {
                    edits.replace(text, &safe);
                }
            }
            Ok(())
        }));
    }
    handlers
}

/// The handler that writes `inserts` immediately after the opening `<body>`
/// tag, once, or `None` when there is nothing to write, so a document whose
/// middleware write nothing there gets no handler.
fn body_open_handler(inserts: &[String]) -> Option<ContentHandler> {
    if inserts.is_empty() {
        return None;
    }
    let markup = inserts.concat();
    let written = Cell::new(false);
    Some(element!("body", move |el| {
        // Once per document, because a template fragment can carry a second
        // `<body>`.
        if !written.replace(true) {
            el.prepend(&markup, ContentType::Html);
        }
        Ok(())
    }))
}

/// The processor behind [`create_serve_processor`], carrying out `plan` and
/// nothing else.
fn build_serve_processor(plan: MiddlewarePlan) -> HtmlWithStreamingProcessors {
    let MiddlewarePlan {
        head_inserts,
        after_bundle_inserts,
        body_open_inserts,
        element_handlers,
        text_handlers,
        processors,
        // The caller writes the plan's headers on the response.
        response_headers: _,
    } = plan;
    let mut element_content_handlers: Vec<ContentHandler> = Vec::new();
    if let Some(handler) = body_open_handler(&body_open_inserts) {
        element_content_handlers.push(handler);
    }
    if !head_inserts.is_empty() || !after_bundle_inserts.is_empty() {
        let before = head_inserts.concat();
        let after = after_bundle_inserts.concat();
        let written = Cell::new(false);
        element_content_handlers.push(element!(BUNDLE_SELECTOR, move |el| {
            // Core's tag is the first in the document. An element of the
            // page's own under the same id is left alone.
            if !written.replace(true) {
                el.before(&before, ContentType::Html);
                el.after(&after, ContentType::Html);
            }
            Ok(())
        }));
    }
    let edits = Rc::new(TextEdits::default());
    element_content_handlers.extend(middleware_content_handlers(
        element_handlers,
        text_handlers,
        &edits,
    ));
    HtmlWithStreamingProcessors {
        inner: Box::new(HtmlRewriterAdapter::new(RewriterSettings {
            element_content_handlers,
            ..RewriterSettings::default()
        })),
        processors,
    }
}

/// `value` with each occurrence of a form's text replaced by that form's
/// replacement, in one pass from the start, so no replacement is read again.
///
/// The earliest match is taken each time, and the longer of two that start at
/// the same place. A form with empty text is ignored.
fn replace_each_once(value: &str, forms: &[(String, String)]) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    loop {
        let next = forms
            .iter()
            .filter(|(text, _)| !text.is_empty())
            .filter_map(|(text, replacement)| {
                rest.find(text.as_str())
                    .map(|at| (at, text.len(), replacement))
            })
            .min_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.cmp(&a.1)));
        let Some((at, len, replacement)) = next else {
            break;
        };
        out.push_str(&rest[..at]);
        out.push_str(replacement);
        rest = &rest[at + len..];
    }
    out.push_str(rest);
    out
}

/// The processor behind [`create_html_processor`] and
/// [`create_html_processor_with_middleware`], carrying out `plan`.
///
/// A handler in `plan` whose selector does not parse panics here, which
/// [`MiddlewareChain::plan`] rules out by refusing to plan one.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the returned processor owns request configuration captured by its handlers"
)]
fn build_html_processor(
    config: HtmlProcessorConfig,
    plan: MiddlewarePlan,
) -> HtmlWithStreamingProcessors {
    let MiddlewarePlan {
        head_inserts: middleware_head_inserts,
        after_bundle_inserts: middleware_after_bundle_inserts,
        body_open_inserts: middleware_body_open_inserts,
        element_handlers: middleware_element_handlers,
        text_handlers: middleware_text_handlers,
        processors: middleware_processors,
        // A fetch plan writes no headers, which `MiddlewareChain::plan`
        // refuses.
        response_headers: _,
    } = plan;
    // The spellings of the origin's address in a URL attribute, each with its
    // replacement, built once per document. They are replaced in one pass and a
    // replacement is never read again, because a request host that begins with
    // the origin host would otherwise match a later spelling inside an earlier
    // spelling's replacement.
    struct UrlPatterns {
        origin_host: String,
        request_host: String,
        /// The `https://`, `http://` and protocol-relative spellings.
        url_forms: Vec<(String, String)>,
        /// The same and the bare host, which a `srcset` candidate may name.
        srcset_forms: Vec<(String, String)>,
    }

    impl UrlPatterns {
        fn new(origin_host: &str, request_host: &str, request_scheme: &str) -> Self {
            let replacement_url = format!("{request_scheme}://{request_host}");
            let url_forms = vec![
                (format!("https://{origin_host}"), replacement_url.clone()),
                (format!("http://{origin_host}"), replacement_url),
                (format!("//{origin_host}"), format!("//{request_host}")),
            ];
            let mut srcset_forms = url_forms.clone();
            srcset_forms.push((origin_host.to_owned(), request_host.to_owned()));
            Self {
                origin_host: origin_host.to_owned(),
                request_host: request_host.to_owned(),
                url_forms,
                srcset_forms,
            }
        }

        fn rewrite_url_value(&self, value: &str) -> Option<String> {
            if !value.contains(&self.origin_host) {
                return None;
            }

            // A value that begins with the bare host is moved when the host
            // ends there, so `origin.example.com/news` moves and
            // `origin.example.com.mirror.example` does not.
            let rewritten = match value.strip_prefix(self.origin_host.as_str()) {
                Some(rest)
                    if rest.is_empty()
                        || matches!(rest.as_bytes().first(), Some(b'/' | b'?' | b'#')) =>
                {
                    format!(
                        "{}{}",
                        self.request_host,
                        replace_each_once(rest, &self.url_forms)
                    )
                }
                _ => replace_each_once(value, &self.url_forms),
            };

            (rewritten != value).then_some(rewritten)
        }
    }

    let patterns = Rc::new(UrlPatterns::new(
        &config.origin_host,
        &config.request_host,
        &config.request_scheme,
    ));

    let injected_tsjs = Rc::new(Cell::new(false));
    let injected_bids = Arc::new(AtomicBool::new(false));
    let integration_registry = config.integrations.clone();
    let ad_slots_script = config.ad_slots_script.clone();
    let permissions_script = config.permissions_script.clone();
    let body_close = config.body_close.clone();
    let ad_bids_state = config.ad_bids_state.clone();
    let public_prefix = config.public_prefix.clone();

    // No source-comment neutralization here: rewriting a publisher comment that happens
    // to match the reserved marker would change publisher content bytes. Collisions are
    // detected on the completed transform instead, where the response can be refused
    // outright rather than silently edited.
    let mut document_content_handlers = Vec::new();
    if let BodyCloseInjection::Marker(marker) = &body_close {
        let marker = marker.clone();
        let injected_bids = Arc::clone(&injected_bids);
        document_content_handlers.push(end!(move |document_end| {
            // HTML fragments and malformed-but-renderable documents may never expose a
            // body end tag. Always mint a transform-owned terminal seam in that case;
            // otherwise source bytes equal to the reserved marker could be mistaken for
            // ownership by the post-transform exact-count validator.
            if !injected_bids.swap(true, Ordering::SeqCst) {
                document_end.append(&marker, ContentType::Html);
            }
            Ok(())
        }));
    }

    let mut element_content_handlers = vec![
        // Inject unified tsjs bundle once at the start of <head>
        element!("head", {
            let injected_tsjs = injected_tsjs.clone();
            let integrations = integration_registry.clone();
            let ad_slots_script = ad_slots_script.clone();
            let permissions_script = permissions_script.clone();
            let public_prefix = public_prefix.clone();
            move |el| {
                if !injected_tsjs.get() {
                    let mut snippet = String::new();
                    // The permission state goes first, ahead of the slots and
                    // the bundle, because both of those and any vendor module
                    // may read it as soon as they run.
                    if let Some(ref state_script) = permissions_script {
                        snippet.push_str(state_script);
                    }
                    // Inject ad slots script first so it appears before tsjs bundle.
                    if let Some(ref slots_script) = ad_slots_script {
                        snippet.push_str(slots_script);
                    }
                    // What the middleware write ahead of the bundle, such as
                    // a module's configuration, which is then in place when
                    // the bundle's own start-up code reads it.
                    for insert in &middleware_head_inserts {
                        snippet.push_str(insert);
                    }
                    // Main bundle: core + non-deferred integrations (synchronous).
                    let immediate_parts = integrations.js_parts_immediate();
                    let mut script_attributes: Vec<(&str, &str)> =
                        integrations.tsjs_script_tag_attributes();
                    // The names the client script writes in the browser start
                    // with the deployment's public prefix, which it is told
                    // here rather than holding a copy of its own. Every part
                    // is letters, digits, `_`, `-` and `.`, so the value
                    // cannot leave the attribute.
                    if let Some(prefix) = public_prefix.as_ref() {
                        script_attributes.push((PUBLIC_PREFIX_ATTRIBUTE, prefix.as_str()));
                    }
                    snippet.push_str(&tsjs::tsjs_script_tag_with_attributes(
                        &immediate_parts,
                        &script_attributes,
                    ));
                    // What a middleware loads after the bundle and ahead of
                    // the page's own scripts in the origin head.
                    for insert in &middleware_after_bundle_inserts {
                        snippet.push_str(insert);
                    }
                    // Deferred bundles: large modules like prebid loaded after
                    // HTML parsing completes. Empty when none are enabled.
                    let deferred_parts = integrations.js_parts_deferred();
                    snippet.push_str(&tsjs::tsjs_deferred_script_tags(&deferred_parts));
                    el.prepend(&snippet, ContentType::Html);
                    injected_tsjs.set(true);
                }
                Ok(())
            }
        }),
        // Inject tsjs.bids before </body> via end_tag_handlers — only when
        // slots matched this URL. When no slots matched, skip injection entirely
        // so the publisher's existing client-side Prebid/GPT flow is unmodified
        // (dual-mode rollout: calling tsjs.adInit with empty slots would invoke
        // enableSingleRequest/enableServices and conflict with the publisher's GPT init).
        // Guard with AtomicBool so the script is only injected once even if
        // the origin HTML contains multiple <body> elements (e.g. template fragments).
        element!("body", {
            let state = ad_bids_state.clone();
            let injected_bids = injected_bids.clone();
            let body_close = body_close.clone();
            move |el| {
                if matches!(body_close, BodyCloseInjection::None) {
                    return Ok(());
                }
                let state = state.clone();
                let injected_bids = injected_bids.clone();
                let body_close = body_close.clone();
                if let Some(handlers) = el.end_tag_handlers() {
                    let handler: EndTagHandler<'static> =
                        Box::new(move |end_tag: &mut EndTag<'_>| {
                            if injected_bids.swap(true, Ordering::SeqCst) {
                                return Ok(());
                            }
                            let markup = match &body_close {
                                // Verbatim, and identical on every request that
                                // reaches the transform — that is what makes the
                                // cached template shared-safe.
                                BodyCloseInjection::Marker(marker)
                                | BodyCloseInjection::DeferredInlineMarker(marker) => {
                                    marker.clone()
                                }
                                BodyCloseInjection::InlineBids => {
                                    let script_guard = state.lock().expect("should lock bid state");
                                    match &*script_guard {
                                        Some(s) => s.clone(),
                                        None => build_empty_bids_script(),
                                    }
                                }
                                // Unreachable: the element handler returned early
                                // above. Kept exhaustive rather than using `_` so a
                                // new variant is a compile error here.
                                BodyCloseInjection::None => return Ok(()),
                            };
                            end_tag.before(&markup, ContentType::Html);
                            Ok(())
                        });
                    handlers.push(handler);
                } else if matches!(
                    body_close,
                    BodyCloseInjection::InlineBids | BodyCloseInjection::DeferredInlineMarker(_)
                ) {
                    // No end tag (implicitly closed or EOF `<body>`): lol_html
                    // cannot attach an end-tag handler, so tsjs.bids/adInit() are
                    // never injected even though adSlots was injected at `<head>`.
                    // The whole server-side ad feature then silently fails to
                    // render — warn so the failure is diagnosable.
                    log::warn!(
                        "`<body>` has no end tag (implicitly closed or EOF); tsjs.bids and adInit() were not injected — server-side ads will not render"
                    );
                }
                Ok(())
            }
        }),
        // Replace URLs in href attributes
        element!("[href]", {
            let patterns = patterns.clone();
            move |el| {
                if let Some(href) = el.get_attribute("href")
                    && let Some(rewritten) = patterns.rewrite_url_value(&href)
                {
                    el.set_attribute("href", &rewritten)?;
                }
                Ok(())
            }
        }),
        // Replace URLs in src attributes
        element!("[src]", {
            let patterns = patterns.clone();
            move |el| {
                if let Some(src) = el.get_attribute("src")
                    && let Some(rewritten) = patterns.rewrite_url_value(&src)
                {
                    el.set_attribute("src", &rewritten)?;
                }
                Ok(())
            }
        }),
        // Replace URLs in action attributes
        element!("[action]", {
            let patterns = patterns.clone();
            move |el| {
                if let Some(action) = el.get_attribute("action")
                    && let Some(rewritten) = patterns.rewrite_url_value(&action)
                {
                    el.set_attribute("action", &rewritten)?;
                }
                Ok(())
            }
        }),
        // Replace URLs in srcset attributes (for responsive images)
        element!("[srcset]", {
            let patterns = patterns.clone();
            move |el| {
                if let Some(srcset) = el.get_attribute("srcset") {
                    let rewritten = replace_each_once(&srcset, &patterns.srcset_forms);
                    if rewritten != srcset {
                        el.set_attribute("srcset", &rewritten)?;
                    }
                }
                Ok(())
            }
        }),
        // Replace URLs in imagesrcset attributes (for link preload)
        element!("[imagesrcset]", {
            let patterns = patterns.clone();
            move |el| {
                if let Some(imagesrcset) = el.get_attribute("imagesrcset") {
                    let rewritten = replace_each_once(&imagesrcset, &patterns.url_forms);
                    if rewritten != imagesrcset {
                        el.set_attribute("imagesrcset", &rewritten)?;
                    }
                }
                Ok(())
            }
        }),
    ];

    // A response-bound nonce is only safe for the response that carried it, and the
    // response-header gate cannot see one the origin delivered in the markup instead.
    // Observed structurally rather than by scanning the output bytes, which cannot tell a
    // `nonce` attribute from the same word inside a script.
    if let Some(observed) = config.csp_nonce_observed.clone() {
        let meta_observed = Arc::clone(&observed);
        element_content_handlers.push(element!("meta[http-equiv][content]", move |el| {
            let delivers_csp = el.get_attribute("http-equiv").is_some_and(|equiv| {
                matches!(
                    equiv.trim().to_ascii_lowercase().as_str(),
                    "content-security-policy" | "content-security-policy-report-only"
                )
            });
            if delivers_csp
                && el
                    .get_attribute("content")
                    .is_some_and(|policy| policy.to_ascii_lowercase().contains("'nonce-"))
            {
                meta_observed.store(true, Ordering::SeqCst);
            }
            Ok(())
        }));
        // `lol_html` does not entity-decode quoted meta CSP content for the check above.
        // Reject nonce attributes independently so an entity-encoded meta policy cannot
        // hide executable nonce-bound content from the template-cache safety scan.
        element_content_handlers.push(element!("[nonce]", move |_el| {
            observed.store(true, Ordering::SeqCst);
            Ok(())
        }));
    }

    // Shared by every text handler, so each is handed a chunk as the one
    // before it left it.
    let edits = Rc::new(TextEdits::default());

    // Registered after core's own handlers, so an element handler judges an
    // address as it will be served.
    if let Some(handler) = body_open_handler(&middleware_body_open_inserts) {
        element_content_handlers.push(handler);
    }
    element_content_handlers.extend(middleware_content_handlers(
        middleware_element_handlers,
        middleware_text_handlers,
        &edits,
    ));

    let rewriter_settings = RewriterSettings {
        document_content_handlers,
        element_content_handlers,
        ..RewriterSettings::default()
    };

    let inner = HtmlRewriterAdapter::new(rewriter_settings);

    HtmlWithStreamingProcessors {
        inner: Box::new(inner),
        processors: middleware_processors,
    }
}

/// Running a page through the document pipeline and comparing what comes out
/// with a recording, for a module crate's own tests.
///
/// A recording is the page a reader receives, kept as a file beside the test.
/// It is how a change to the way a module makes its page change is shown to
/// leave the page as it was.
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support {
    use std::io::Cursor;

    use super::{HtmlProcessorConfig, create_html_processor_with_middleware};
    use crate::integrations::IntegrationRegistry;
    use crate::middleware::{HTML_MEDIA_TYPE, MiddlewarePhase, PhaseEntries, PhaseEntry};
    use crate::settings::Settings;
    use crate::streaming_processor::{
        Compression, PipelineConfig, StreamProcessor, StreamingPipeline,
    };

    /// The host a test page is fetched from.
    pub const ORIGIN_HOST: &str = "origin.example.com";

    /// The host a test page is served on, over HTTPS.
    pub const REQUEST_HOST: &str = "publisher.example.com";

    /// Set in the environment to write recordings in place of comparing with
    /// them.
    pub const RECORD_ENV: &str = "TS_RECORD_PAGES";

    /// What stands in a recording for the hash of the script bundle, which
    /// changes whenever any browser module does.
    const BUNDLE_HASH: &str = "BUNDLE-HASH";

    /// The path a test page is asked for at, which decides the entry of the
    /// settings that covers it.
    pub const REQUEST_PATH: &str = "/";

    /// Has `settings` run `names` on every page in `phase`, in the order
    /// given, in place of any entries that phase held.
    pub fn place_on_every_page(settings: &mut Settings, phase: MiddlewarePhase, names: &[&str]) {
        let entries = PhaseEntries::new(vec![PhaseEntry {
            media_type: HTML_MEDIA_TYPE.to_owned(),
            path: None,
            middleware: names.iter().map(|name| (*name).to_owned()).collect(),
        }]);
        match phase {
            MiddlewarePhase::Fetch => settings.fetch = entries,
            MiddlewarePhase::Serve => settings.serve = entries,
        }
    }

    /// The processor of a page asked for at [`REQUEST_PATH`] from a
    /// deployment running `settings` with `registry`'s modules, built from
    /// `config`, on which a test sets what it is about. The page is not
    /// stored, so both phases' middleware run on the one pass.
    ///
    /// # Panics
    ///
    /// When a middleware asks for a selector that does not parse.
    #[must_use]
    pub fn create_page_processor(
        settings: &Settings,
        registry: &IntegrationRegistry,
        config: HtmlProcessorConfig,
    ) -> impl StreamProcessor + use<> {
        let fetch = registry.middleware_chain(
            &settings.fetch,
            MiddlewarePhase::Fetch,
            HTML_MEDIA_TYPE,
            REQUEST_PATH,
        );
        let serve = registry.middleware_chain(
            &settings.serve,
            MiddlewarePhase::Serve,
            HTML_MEDIA_TYPE,
            REQUEST_PATH,
        );
        create_html_processor_with_middleware(config, &fetch, Some(&serve))
            .expect("should plan the page's middleware")
            .0
    }

    /// The page a reader of `html` receives from a deployment running
    /// `settings` with `registry`'s modules, read from the origin `chunk_size`
    /// bytes at a time and asked for at [`REQUEST_PATH`]. The page is not
    /// stored, so both phases' middleware run on the one pass.
    ///
    /// The script bundle's `?v=` hash is replaced by a fixed word, so a
    /// recording changes when a module's page change does and not when a
    /// browser module is rebuilt.
    ///
    /// # Panics
    ///
    /// When the pipeline refuses the page, or leaves something that is not
    /// UTF-8.
    #[must_use]
    pub fn processed_page(
        settings: &Settings,
        registry: &IntegrationRegistry,
        html: &str,
        chunk_size: usize,
    ) -> String {
        let config = HtmlProcessorConfig::from_settings(
            settings,
            registry,
            ORIGIN_HOST,
            REQUEST_HOST,
            "https",
        );
        let mut pipeline = StreamingPipeline::new(
            PipelineConfig {
                input_compression: Compression::None,
                output_compression: Compression::None,
                chunk_size,
            },
            create_page_processor(settings, registry, config),
        );
        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("should process the page");
        without_bundle_hashes(&String::from_utf8(output).expect("should leave the page UTF-8"))
    }

    /// `page` with each `?v=` hash of a script bundle address replaced.
    fn without_bundle_hashes(page: &str) -> String {
        const MARK: &str = "/static/tsjs=";
        let mut out = String::with_capacity(page.len());
        let mut rest = page;
        while let Some(at) = rest.find(MARK) {
            let (before, address) = rest.split_at(at + MARK.len());
            out.push_str(before);
            let end = address
                .find(['"', '\'', ' ', '>', '<'])
                .unwrap_or(address.len());
            let (address, after) = address.split_at(end);
            match address.split_once("?v=") {
                Some((file, hash))
                    if !hash.is_empty() && hash.bytes().all(|b| b.is_ascii_hexdigit()) =>
                {
                    out.push_str(file);
                    out.push_str("?v=");
                    out.push_str(BUNDLE_HASH);
                }
                _ => out.push_str(address),
            }
            rest = after;
        }
        out.push_str(rest);
        out
    }

    /// The least a deployment's settings must say, which a module's own
    /// settings are written after.
    const BASE_SETTINGS: &str = r#"
[source]
module = "origin"

[source.origin]
url = "https://origin.example.com"

[publisher]
domain = "publisher.example.com"
cookie_domain = ".publisher.example.com"
proxy_secret = "page-recording-proxy-secret"

[geo]
assume_single_jurisdiction = true

[ec]
module = "hmac"

[ec.hmac]
passphrase = "page-recording-passphrase-32-bytes"
"#;

    /// The settings of a deployment that writes `module_settings`, a TOML
    /// document selecting a module and holding its table, after the least a
    /// deployment must say.
    ///
    /// # Panics
    ///
    /// When the two together are not settings a deployment could load.
    #[must_use]
    pub fn page_settings(module_settings: &str) -> Settings {
        Settings::from_toml(&format!("{BASE_SETTINGS}\n{module_settings}"))
            .expect("should read the page's settings")
    }

    /// The size of the chunks a recorded page is read from the origin in,
    /// large enough that a test page arrives whole.
    const RECORDING_CHUNK_SIZE: usize = 8192;

    /// Checks the page `html` becomes is the one in `recorded`, byte for
    /// byte.
    ///
    /// The deployment writes `module_settings`, see [`page_settings`], and
    /// links `builders` beside core's own modules. `recorded` is the recording
    /// as the test embeds it and `path` is where it is kept. With
    /// [`RECORD_ENV`] set the recording is written to `path` instead, which is
    /// how a recording is made. A recording is made again only when the page
    /// is meant to change for every reader.
    ///
    /// # Panics
    ///
    /// When the page is not the recorded one, or the recording cannot be
    /// written.
    pub fn assert_page_is_recorded(
        module_settings: &str,
        builders: &[crate::integrations::IntegrationBuilder],
        html: &str,
        recorded: &str,
        path: &str,
    ) {
        let settings = &page_settings(module_settings);
        let registry = &IntegrationRegistry::with_registrations(settings, builders)
            .expect("should build the registry");
        let page = processed_page(settings, registry, html, RECORDING_CHUNK_SIZE);
        #[cfg(not(target_arch = "wasm32"))]
        if std::env::var_os(RECORD_ENV).is_some() {
            std::fs::write(path, &page).expect("should write the recording");
            return;
        }
        assert!(
            recorded == page,
            "the page is not the one recorded at {path}.\n--- recorded\n{recorded}\n--- now\n{page}\n\
             ---\nA recording changes only when the page is meant to change for every reader, \
             by running the test with {RECORD_ENV} set."
        );
    }

    #[cfg(test)]
    mod tests {
        use super::{place_on_every_page, processed_page, without_bundle_hashes};
        use crate::integrations::IntegrationRegistry;
        use crate::integrations::registry_test_support::middleware_fixture as fixture;
        use crate::middleware::MiddlewarePhase;

        #[test]
        fn a_test_page_runs_the_middleware_placed_on_every_page() {
            let mut settings = crate::test_support::tests::create_test_settings();
            settings.select_module("testing", fixture::MODULE);
            let html = "<html><head></head><body></body></html>";

            let registry = IntegrationRegistry::new(&settings).expect("should build a registry");
            let unplaced = processed_page(&settings, &registry, html, 8192);

            place_on_every_page(&mut settings, MiddlewarePhase::Fetch, &[fixture::HEAD]);
            place_on_every_page(&mut settings, MiddlewarePhase::Serve, &[fixture::READER]);
            let registry = IntegrationRegistry::new(&settings).expect("should build a registry");
            let placed = processed_page(&settings, &registry, html, 8192);

            assert!(
                !unplaced.contains("middleware-fixture"),
                "should run no middleware an entry does not name: {unplaced}"
            );
            assert!(
                placed.contains(&fixture::head_marker(""))
                    && placed.contains("middleware-fixture-reader"),
                "should run the middleware of both phases on a test page: {placed}"
            );
        }

        #[test]
        fn a_bundle_hash_is_replaced_and_nothing_else_is() {
            let page = "<script src=\"/static/tsjs=tsjs-unified.min.js?v=0a1B2c\" id=\"x\"></script>\
                        <script src='/static/tsjs=tsjs-prebid.min.js?v=ff00' defer></script>\
                        <a href=\"/static/tsjs=other.js?v=not-a-hash\">kept</a>\
                        <a href=\"/page?v=0a1b2c\">kept</a>";

            let replaced = without_bundle_hashes(page);

            assert_eq!(
                replaced,
                "<script src=\"/static/tsjs=tsjs-unified.min.js?v=BUNDLE-HASH\" id=\"x\"></script>\
                 <script src='/static/tsjs=tsjs-prebid.min.js?v=BUNDLE-HASH' defer></script>\
                 <a href=\"/static/tsjs=other.js?v=not-a-hash\">kept</a>\
                 <a href=\"/page?v=0a1b2c\">kept</a>"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
    use crate::test_support::tests::create_test_settings;
    use serde_json::json;
    use std::io::Cursor;
    use std::sync::Arc;

    // 1.1× accounts for the injected tsjs script tag plus URL attribute rewrites.
    // Observed growth on the test fixture is ≤1.01×; 1.1× gives headroom while
    // catching real regressions (e.g., double-injection or buffer leak).
    const MAX_GROWTH_FACTOR: f64 = 1.1;

    fn create_test_config() -> HtmlProcessorConfig {
        HtmlProcessorConfig {
            csp_nonce_observed: None,
            public_prefix: None,
            prepared: Arc::new(Prepared::default()),
            body_close: BodyCloseInjection::None,
            origin_host: "origin.example.com".to_owned(),
            request_host: "test.example.com".to_owned(),
            request_scheme: "https".to_owned(),
            integrations: IntegrationRegistry::default(),
            ad_slots_script: None,
            permissions_script: None,
            ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
            max_buffered_body_bytes: 16 * 1024 * 1024,
            request_state: IntegrationRequestState::default(),
        }
    }

    /// The client script names its cookies and storage entries after the
    /// public prefix, which its tag tells it. A document given no prefix gets
    /// no attribute.
    #[test]
    fn the_bundle_tag_tells_the_client_script_the_public_prefix() {
        fn process(public: Option<PublicPrefix>) -> String {
            let settings = create_test_settings();
            let registry = IntegrationRegistry::new(&settings).expect("should build the registry");
            let mut config = create_test_config();
            config.integrations = registry.clone();
            config.public_prefix = public;
            let mut processor = test_support::create_page_processor(&settings, &registry, config);
            let output = processor
                .process_chunk(b"<html><head></head><body></body></html>", true)
                .expect("should process HTML");

            String::from_utf8(output).expect("should produce valid UTF-8")
        }

        let told = process(Some(
            PublicPrefix::new(["_ex"]).expect("should accept the prefix"),
        ));
        assert!(
            told.contains(r#"data-ts-public-prefix="_ex""#),
            "the bundle tag should carry the public prefix: {told}"
        );
        let untold = process(None);
        assert!(!untold.contains(PUBLIC_PREFIX_ATTRIBUTE), "{untold}");
    }

    /// A body-open insert is written immediately after the opening body tag,
    /// once, however many body tags a fragment carries. The fetch processor's
    /// handler is the same function, and the seam probe's page test drives it.
    #[test]
    fn a_body_open_insert_is_written_once_after_the_opening_body_tag() {
        let plan = MiddlewarePlan {
            body_open_inserts: vec!["<p>first</p>".to_owned(), "<p>second</p>".to_owned()],
            ..MiddlewarePlan::default()
        };

        let output = build_serve_processor(plan)
            .process_chunk(
                b"<html><head></head><body><p>page</p></body><body></body></html>",
                true,
            )
            .expect("should process HTML");

        let out = String::from_utf8(output).expect("should be UTF-8");
        assert!(
            out.contains("<body><p>first</p><p>second</p><p>page</p>"),
            "the inserts follow the opening body tag in order: {out}"
        );
        assert_eq!(
            out.matches("<p>first</p>").count(),
            1,
            "once per document: {out}"
        );
    }

    #[test]
    fn a_registration_marks_only_the_bundle_it_asks_to_mark() {
        use crate::integrations::registry_test_support::tag_fixture as tag;

        fn process(mark_bundle: Option<bool>) -> String {
            let mut settings = create_test_settings();
            if let Some(mark_bundle) = mark_bundle {
                settings
                    .insert_module_config(
                        "testing",
                        tag::MODULE,
                        &json!({
                            "mark_bundle": mark_bundle
                        }),
                    )
                    .expect("should insert the stand-in's settings");
                test_support::place_on_every_page(
                    &mut settings,
                    crate::middleware::MiddlewarePhase::Fetch,
                    &[tag::MODULE],
                );
            }
            let registry = IntegrationRegistry::new(&settings).expect("should build the registry");
            let mut config = create_test_config();
            config.integrations = registry.clone();
            let mut processor = test_support::create_page_processor(&settings, &registry, config);
            let output = processor
                .process_chunk(b"<html><head></head><body></body></html>", true)
                .expect("should process HTML");

            String::from_utf8(output).expect("should produce valid UTF-8")
        }

        let marked = process(Some(true));
        let unmarked = process(Some(false));
        let without_the_integration = process(None);

        for html in [&marked, &unmarked, &without_the_integration] {
            assert_eq!(
                html.matches("id=\"trustedserver-js\"").count(),
                1,
                "should emit exactly one publisher bundle tag: {html}"
            );
        }
        let attribute = format!("{}=\"true\"", tag::BUNDLE_ATTRIBUTE);
        assert!(
            marked.contains(&attribute),
            "should mark the bundle a registration asks to mark"
        );
        assert!(
            !unmarked.contains(tag::BUNDLE_ATTRIBUTE),
            "should leave the bundle unmarked when the registration asks for nothing"
        );
        assert!(
            !without_the_integration.contains(tag::BUNDLE_ATTRIBUTE),
            "should leave the bundle unmarked when no section selects the integration"
        );

        let head_insert_index = marked
            .find(tag::HEAD_FLAG)
            .expect("should include the integration's head insert");
        let publisher_bundle_index = marked
            .find("id=\"trustedserver-js\"")
            .expect("should include the publisher bundle");
        assert!(
            head_insert_index < publisher_bundle_index,
            "should keep a middleware's head markup before the publisher bundle"
        );
    }

    #[test]
    fn what_a_module_left_on_the_request_is_written_around_the_unified_bundle_once() {
        use crate::integrations::registry_test_support::request_fixture;

        let html = "<html><head><title>Test</title></head><body></body></html>";
        let mut settings = create_test_settings();
        request_fixture::select_and_place(&mut settings);
        let registry =
            IntegrationRegistry::new(&settings).expect("should build integration registry");
        let mut config = create_test_config();
        config.integrations = registry.clone();
        config.request_state = request_fixture::marked();

        let processor = test_support::create_page_processor(&settings, &registry, config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);
        let mut output = Vec::new();

        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("should process HTML");
        let processed = String::from_utf8(output).expect("should produce valid UTF-8");
        let head_marker = request_fixture::HEAD_FLAG;
        let bundle_marker = "id=\"trustedserver-js\"";
        let module_marker = request_fixture::MODULE_FILE;

        assert_eq!(
            processed.matches(head_marker).count(),
            1,
            "should write the module's head insert once"
        );
        assert_eq!(
            processed.matches(bundle_marker).count(),
            1,
            "should inject the immediate TSJS bundle once"
        );
        assert_eq!(
            processed.matches(module_marker).count(),
            1,
            "should write the module's after-bundle insert once"
        );
        let head_index = processed
            .find(head_marker)
            .expect("should include the head insert");
        let bundle_index = processed
            .find(bundle_marker)
            .expect("should include immediate TSJS bundle");
        let module_index = processed
            .find(module_marker)
            .expect("should include the after-bundle insert");
        assert!(
            head_index < bundle_index,
            "should write the head insert before the bundle executes"
        );
        assert!(
            bundle_index < module_index,
            "should write the after-bundle insert after the bundle"
        );
    }

    #[test]
    fn test_create_html_processor_url_replacement() {
        let config = create_test_config();
        let processor = create_html_processor(config);

        // Create a pipeline to test the processor
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let html = r#"<html>
            <a href="https://origin.example.com/page">Link</a>
            <a href="//origin.example.com/proto">Proto</a>
            <a href="origin.example.com/bare">Bare</a>
            <img src="http://origin.example.com/image.jpg">
            <img src="//origin.example.com/image2.jpg">
            <form action="https://origin.example.com/submit">
            <form action="//origin.example.com/submit2">
        </html>"#;

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("pipeline should process HTML");

        let result = String::from_utf8(output).expect("output should be valid UTF-8");
        assert!(result.contains(r#"href="https://test.example.com/page""#));
        assert!(result.contains(r#"href="//test.example.com/proto""#));
        assert!(result.contains(r#"href="test.example.com/bare""#));
        assert!(result.contains(r#"src="https://test.example.com/image.jpg""#));
        assert!(result.contains(r#"src="//test.example.com/image2.jpg""#));
        assert!(result.contains(r#"action="https://test.example.com/submit""#));
        assert!(result.contains(r#"action="//test.example.com/submit2""#));
        assert!(!result.contains("origin.example.com"));
    }

    #[test]
    fn the_origin_s_address_is_moved_in_srcset_and_imagesrcset() {
        let mut processor = create_html_processor(create_test_config());

        let output = processor
            .process_chunk(
                br#"<html><head><link rel="preload" as="image" imagesrcset="https://origin.example.com/a.png 1x, http://origin.example.com/b.png 2x, //origin.example.com/c.png 3x"></head><body><img srcset="https://origin.example.com/a.png 1x, http://origin.example.com/b.png 2x, //origin.example.com/c.png 3x, origin.example.com/d.png 4x"><img srcset="https://cdn.example.net/e.png 1x"></body></html>"#,
                true,
            )
            .expect("should process the document");
        let processed = String::from_utf8(output).expect("should stay UTF-8");

        assert!(
            processed.contains(
                r#"imagesrcset="https://test.example.com/a.png 1x, https://test.example.com/b.png 2x, //test.example.com/c.png 3x""#
            ),
            "should move the origin's address in every candidate of an imagesrcset: {processed}"
        );
        assert!(
            processed.contains(
                r#"srcset="https://test.example.com/a.png 1x, https://test.example.com/b.png 2x, //test.example.com/c.png 3x, test.example.com/d.png 4x""#
            ),
            "should move the origin's address in every candidate of a srcset, a bare host \
             included: {processed}"
        );
        assert!(
            processed.contains(r#"<img srcset="https://cdn.example.net/e.png 1x">"#),
            "should leave a srcset that names another host as it is: {processed}"
        );
    }

    /// A request host that begins with the origin host, as a local host named
    /// after the publisher does, is written once in every address over either
    /// scheme, rather than matched again inside its own replacement.
    #[test]
    fn a_request_host_that_begins_with_the_origin_host_is_written_once() {
        for scheme in ["http", "https"] {
            let mut config = create_test_config();
            config.origin_host = "publisher.example".to_owned();
            config.request_host = "publisher.example.localhost".to_owned();
            config.request_scheme = scheme.to_owned();
            let mut processor = create_html_processor(config);

            let output = processor
                .process_chunk(
                    br#"<html><head><link rel="preload" as="image" imagesrcset="https://publisher.example/a.png 1x, //publisher.example/b.png 2x"></head><body><a href="https://publisher.example/news">n</a><a href="http://publisher.example/old">o</a><a href="publisher.example/bare">b</a><img src="//publisher.example/c.png" srcset="https://publisher.example/d.png 1x, publisher.example/e.png 2x"><form action="https://publisher.example/search"></form></body></html>"#,
                    true,
                )
                .expect("should process the document");
            let processed = String::from_utf8(output).expect("should stay UTF-8");

            assert!(
                !processed.contains(".localhost.localhost"),
                "should write the request host once over {scheme}: {processed}"
            );
            for expected in [
                format!(r#"href="{scheme}://publisher.example.localhost/news""#),
                format!(r#"href="{scheme}://publisher.example.localhost/old""#),
                r#"href="publisher.example.localhost/bare""#.to_owned(),
                r#"src="//publisher.example.localhost/c.png""#.to_owned(),
                format!(
                    r#"srcset="{scheme}://publisher.example.localhost/d.png 1x, publisher.example.localhost/e.png 2x""#
                ),
                format!(r#"action="{scheme}://publisher.example.localhost/search""#),
                format!(
                    r#"imagesrcset="{scheme}://publisher.example.localhost/a.png 1x, //publisher.example.localhost/b.png 2x""#
                ),
            ] {
                assert!(
                    processed.contains(&expected),
                    "should contain {expected} over {scheme}: {processed}"
                );
            }
        }
    }

    /// Hosts that do not begin with the origin host are written as before,
    /// both one with no part of the origin host in it and a `www.` host in
    /// front of an apex origin, whose `srcset` must not become `www.www.`.
    #[test]
    fn a_request_host_that_does_not_begin_with_the_origin_host_is_written_as_before() {
        for (origin, request) in [
            ("publisher.example", "publisher-example.localhost"),
            ("example.com", "www.example.com"),
        ] {
            let mut config = create_test_config();
            config.origin_host = origin.to_owned();
            config.request_host = request.to_owned();
            config.request_scheme = "https".to_owned();
            let mut processor = create_html_processor(config);

            let input = format!(
                r#"<html><head></head><body><a href="https://{origin}/a">a</a><a href="http://{origin}/b">b</a><img src="//{origin}/c.png" srcset="https://{origin}/d.png 1x, {origin}/e.png 2x"></body></html>"#
            );
            let output = processor
                .process_chunk(input.as_bytes(), true)
                .expect("should process the document");
            let processed = String::from_utf8(output).expect("should stay UTF-8");

            for expected in [
                format!(r#"href="https://{request}/a""#),
                format!(r#"href="https://{request}/b""#),
                format!(r#"src="//{request}/c.png""#),
                format!(r#"srcset="https://{request}/d.png 1x, {request}/e.png 2x""#),
            ] {
                assert!(
                    processed.contains(&expected),
                    "should contain {expected} for {origin} served as {request}: {processed}"
                );
            }
        }
    }

    #[test]
    fn each_form_is_replaced_once_from_the_start() {
        let forms = vec![
            (
                "https://a.example".to_owned(),
                "http://a.example.localhost".to_owned(),
            ),
            (
                "http://a.example".to_owned(),
                "http://a.example.localhost".to_owned(),
            ),
            ("//a.example".to_owned(), "//a.example.localhost".to_owned()),
            ("a.example".to_owned(), "a.example.localhost".to_owned()),
            (String::new(), "never".to_owned()),
        ];

        assert_eq!(
            replace_each_once(
                "https://a.example/1 http://a.example/2 //a.example/3 a.example/4",
                &forms
            ),
            "http://a.example.localhost/1 http://a.example.localhost/2 //a.example.localhost/3 a.example.localhost/4"
        );
        assert_eq!(replace_each_once("nothing here", &forms), "nothing here");
        assert_eq!(replace_each_once("", &forms), "");
    }

    #[test]
    fn test_html_processor_config_from_settings() {
        let settings = create_test_settings();
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");
        let config = HtmlProcessorConfig::from_settings(
            &settings,
            &registry,
            "origin.test-publisher.com",
            "proxy.example.com",
            "https",
        );

        assert_eq!(config.origin_host, "origin.test-publisher.com");
        assert_eq!(config.request_host, "proxy.example.com");
        assert_eq!(config.request_scheme, "https");
    }

    #[test]
    fn test_real_publisher_html() {
        // Test with publisher HTML from test_publisher.html
        let html = include_str!("html_processor.test.html");

        // Count URLs in the test HTML
        let original_urls = html.matches("www.test-publisher.com").count();
        let https_urls = html.matches("https://www.test-publisher.com").count();
        let protocol_relative_urls = html.matches("//www.test-publisher.com").count();

        println!("Test HTML stats:");
        println!("  Total URLs: {original_urls}");
        println!("  HTTPS URLs: {https_urls}");
        println!("  Protocol-relative URLs: {protocol_relative_urls}");

        // Process - replace test-publisher.com with our edge domain
        let mut config = create_test_config();
        config.origin_host = "www.test-publisher.com".to_owned(); // Match what's in the HTML
        config.request_host = "test-publisher-ts.edgecompute.app".to_owned();

        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("pipeline should process HTML");
        let result = String::from_utf8(output).expect("output should be valid UTF-8");

        // Assertions - only URL attribute replacements are expected
        // Check URL replacements (not all occurrences will be replaced since
        // we only rewrite attributes, not text/JSON/script bodies)
        let remaining_urls = result.matches("www.test-publisher.com").count();
        let replaced_urls = result.matches("test-publisher-ts.edgecompute.app").count();

        println!("After processing:");
        println!("  Remaining original URLs: {remaining_urls}");
        println!("  Edge domain URLs: {replaced_urls}");

        // Expect at least some replacements and fewer originals than before
        assert!(replaced_urls > 0, "Should replace some URLs in attributes");
        assert!(
            remaining_urls < original_urls,
            "Should reduce occurrences of original host in attributes"
        );

        // Verify HTML structure
        assert!(
            result.starts_with("<!DOCTYPE html>"),
            "Should preserve doctype"
        );
        assert!(
            result.trim_end().ends_with("</html>"),
            "Should preserve closing html tag"
        );

        // Verify content preservation
        assert!(
            result.contains("Mercedes CEO"),
            "Should preserve article title"
        );
        assert!(
            result.contains("test-publisher"),
            "Should preserve text content"
        );
        // No Prebid auto-configuration injection performed here
        assert!(
            !result.contains("window.__trustedServerPrebid"),
            "HtmlProcessor should not inject Prebid config"
        );
    }

    #[test]
    fn test_real_publisher_html_with_gzip() {
        use flate2::Compression as GzCompression;
        use flate2::read::GzDecoder;
        use flate2::write::GzEncoder;
        use std::io::{Read as _, Write as _};

        let html = include_str!("html_processor.test.html");

        // Count URLs in test HTML
        let _original_urls = html.matches("www.test-publisher.com").count();

        // Compress
        let mut encoder = GzEncoder::new(Vec::new(), GzCompression::default());
        encoder
            .write_all(html.as_bytes())
            .expect("should write to gzip encoder");
        let compressed_input = encoder.finish().expect("should finish gzip encoding");

        println!("Compressed input size: {} bytes", compressed_input.len());

        // Process with compression
        let mut config = create_test_config();
        config.origin_host = "www.test-publisher.com".to_owned(); // Match what's in the HTML
        config.request_host = "test-publisher-ts.edgecompute.app".to_owned();

        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::Gzip,
            output_compression: Compression::Gzip,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut compressed_output = Vec::new();
        pipeline
            .process(Cursor::new(&compressed_input), &mut compressed_output)
            .expect("pipeline should process gzipped HTML");

        // Ensure we produced output
        assert!(
            !compressed_output.is_empty(),
            "Should produce compressed output"
        );

        // Decompress and verify
        let mut decoder = GzDecoder::new(&*compressed_output);
        let mut decompressed = String::new();
        decoder
            .read_to_string(&mut decompressed)
            .expect("should decompress gzip output");

        let remaining_urls = decompressed.matches("www.test-publisher.com").count();
        let replaced_urls = decompressed
            .matches("test-publisher-ts.edgecompute.app")
            .count();

        assert!(replaced_urls > 0, "Should replace some URLs in attributes");
        assert!(
            remaining_urls < _original_urls,
            "Should reduce occurrences of original host in attributes"
        );

        // Verify structure
        assert!(
            decompressed.starts_with("<!DOCTYPE html>"),
            "Should preserve doctype"
        );
        assert!(
            decompressed.trim_end().ends_with("</html>"),
            "Should preserve closing html tag"
        );

        // Verify content preservation
        assert!(
            decompressed.contains("Mercedes CEO"),
            "Should preserve article title"
        );
        assert!(
            decompressed.contains("test-publisher"),
            "Should preserve text content"
        );
        // No Prebid auto-configuration injection performed here
        assert!(
            !decompressed.contains("window.__trustedServerPrebid"),
            "HtmlProcessor should not inject Prebid config"
        );
    }

    #[test]
    fn test_already_truncated_html_passthrough() {
        // Test that we don't make truncated HTML worse
        // This simulates receiving already-truncated HTML from origin

        let truncated_html =
            "<html><head><title>Test</title></head><body><p>This is a test that gets cut o";

        println!("Testing already-truncated HTML");
        println!("Input: '{truncated_html}'");

        let config = create_test_config();
        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        let result = pipeline.process(Cursor::new(truncated_html.as_bytes()), &mut output);

        assert!(
            result.is_ok(),
            "Should process truncated HTML without error"
        );

        let processed = String::from_utf8_lossy(&output);
        println!("Output: '{processed}'");

        // The processor should pass through the truncated HTML
        // It might add some closing tags, but shouldn't truncate further
        assert!(
            processed.len() >= truncated_html.len(),
            "Output should not be shorter than truncated input"
        );
    }

    #[test]
    fn test_truncated_html_validation() {
        // Simulated truncated HTML - ends mid-attribute
        let truncated_html = r#"<html lang="en"><head><meta charset="utf-8"><title>Test Publisher</title><link rel="preload" as="image" href="https://www.test-publisher.com/image.jpg"><script src="/js/prebid.min.js"></script></head><body><p>Article content from <a href="https://www.test-publisher.com/ar"#;

        // This HTML is clearly truncated - it ends in the middle of an attribute value
        println!("Testing truncated HTML (ends in middle of URL)");
        println!("Input length: {} bytes", truncated_html.len());

        // Check that the input is indeed truncated
        assert!(
            !truncated_html.contains("</html>"),
            "Input should be truncated (no closing html tag)"
        );
        assert!(
            !truncated_html.contains("</body>"),
            "Input should be truncated (no closing body tag)"
        );
        assert!(
            truncated_html.ends_with("/ar"),
            "Input should end with '/ar' showing truncation"
        );

        // Process it through our pipeline
        let mut config = create_test_config();
        config.origin_host = "www.test-publisher.com".to_owned(); // Match what's in the HTML
        config.request_host = "test-publisher-ts.edgecompute.app".to_owned();

        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();

        // The processor should handle truncated HTML gracefully
        let result = pipeline.process(Cursor::new(truncated_html.as_bytes()), &mut output);

        // Even with truncated input, processing should complete
        assert!(
            result.is_ok(),
            "Processing should complete even with truncated HTML"
        );

        let processed = String::from_utf8_lossy(&output);
        println!("Output length: {} bytes", processed.len());

        // The processor will try to fix the HTML structure
        // lol_html should handle the truncated input and still produce output

        // Check what we got back
        if processed.contains("</html>") {
            println!("Note: lol_html added closing tags to fix truncated HTML");
        }

        // The key issue is that truncated HTML should not cause a panic or error
        // The output might still be malformed, but it should process

        println!(
            "Last 100 chars of output: {}",
            processed
                .chars()
                .rev()
                .take(100)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>()
        );
    }

    #[test]
    fn html_stream_processors_compose_in_order_and_receive_final_once() {
        struct DecoratingProcessor {
            prefix: u8,
            final_calls: Arc<std::sync::atomic::AtomicUsize>,
        }

        impl StreamProcessor for DecoratingProcessor {
            fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> io::Result<Vec<u8>> {
                if is_last {
                    self.final_calls.fetch_add(1, Ordering::SeqCst);
                }
                let mut output = vec![self.prefix];
                output.extend_from_slice(chunk);
                Ok(output)
            }
        }

        let inner_final_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first_final_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let second_final_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut processor = HtmlWithStreamingProcessors {
            inner: Box::new(DecoratingProcessor {
                prefix: b'I',
                final_calls: Arc::clone(&inner_final_calls),
            }),
            processors: vec![
                Box::new(DecoratingProcessor {
                    prefix: b'A',
                    final_calls: Arc::clone(&first_final_calls),
                }),
                Box::new(DecoratingProcessor {
                    prefix: b'B',
                    final_calls: Arc::clone(&second_final_calls),
                }),
            ],
        };

        assert_eq!(
            processor
                .process_chunk(b"x", false)
                .expect("should process intermediate chunk"),
            b"BAIx",
            "should emit intermediate output in registration order",
        );
        assert_eq!(
            processor
                .process_chunk(b"y", true)
                .expect("should process final chunk"),
            b"BAIy",
            "should preserve processor order for final output",
        );
        assert_eq!(inner_final_calls.load(Ordering::SeqCst), 1);
        assert_eq!(first_final_calls.load(Ordering::SeqCst), 1);
        assert_eq!(second_final_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn injects_ad_slots_at_head_open() {
        let config = HtmlProcessorConfig {
            csp_nonce_observed: None,
            public_prefix: None,
            prepared: Arc::new(Prepared::default()),
            body_close: BodyCloseInjection::None,
            origin_host: "origin.example.com".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::empty_for_tests(),
            ad_slots_script: Some(
                r#"<script>(window.tsjs=window.tsjs||{}).adSlots=JSON.parse("[]");</script>"#
                    .to_string(),
            ),
            permissions_script: None,
            ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
            max_buffered_body_bytes: 16 * 1024 * 1024,
            request_state: IntegrationRequestState::default(),
        };
        let mut processor = create_html_processor(config);
        let output = processor
            .process_chunk(
                b"<html><head><title>T</title></head><body>content</body></html>",
                true,
            )
            .expect("should process");
        let html = std::str::from_utf8(&output).expect("should be utf8");
        assert!(
            html.contains("window.tsjs=window.tsjs||{}"),
            "should inject ad slots namespace at head-open"
        );
        assert!(
            html.contains(".adSlots=JSON.parse"),
            "should inject adSlots at head-open"
        );
        assert!(
            !html.contains("__ts_request_id"),
            "must NOT inject request_id"
        );
    }

    #[test]
    fn golden_script_tag_injected_at_head_start() {
        // The trusted-server script tag must be the FIRST child of <head>.
        // Any drift in injection position breaks the page initialization order.
        let html = r#"<!DOCTYPE html>
<html>
<head><meta charset="utf-8"><title>Test</title></head>
<body><p>Hello</p></body>
</html>"#;

        let config = create_test_config();
        let mut processor = create_html_processor(config);
        let output = processor
            .process_chunk(html.as_bytes(), true)
            .expect("should process HTML");
        let output_str = std::str::from_utf8(&output).expect("should be valid UTF-8");

        let head_pos = output_str.find("<head>").expect("should contain <head>");
        let script_pos = output_str
            .find("<script")
            .expect("should inject script tag");

        assert!(
            script_pos > head_pos,
            "script tag must appear after <head> opening: head_pos={head_pos}, script_pos={script_pos}"
        );

        // No other elements between <head> and the script tag
        let between = &output_str[head_pos + "<head>".len()..script_pos];
        let trimmed = between.trim();
        assert!(
            trimmed.is_empty(),
            "script tag must be first child of <head>, found content before it: {trimmed:?}"
        );
    }

    #[test]
    fn injects_ts_bids_before_body_close() {
        let bids_script = r#"<script>(window.tsjs=window.tsjs||{}).bids=JSON.parse("{\"atf\":{\"hb_pb\":\"1.00\"}}");</script>"#;
        let state = std::sync::Arc::new(std::sync::Mutex::new(Some(bids_script.to_string())));
        let config = HtmlProcessorConfig {
            csp_nonce_observed: None,
            public_prefix: None,
            prepared: Arc::new(Prepared::default()),
            body_close: BodyCloseInjection::InlineBids,
            origin_host: "origin.example.com".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::empty_for_tests(),
            ad_slots_script: Some(
                r#"<script>(window.tsjs=window.tsjs||{}).adSlots=[];</script>"#.to_string(),
            ),
            permissions_script: None,
            ad_bids_state: state,
            max_buffered_body_bytes: 16 * 1024 * 1024,
            request_state: IntegrationRequestState::default(),
        };
        let mut processor = create_html_processor(config);
        let output = processor
            .process_chunk(b"<html><head></head><body>content</body></html>", true)
            .expect("should process");
        let html = std::str::from_utf8(&output).expect("should be utf8");
        assert!(
            html.contains("window.tsjs=window.tsjs||{}"),
            "should inject _ts namespace for bids before </body>"
        );
        assert!(
            html.contains(".bids=JSON.parse"),
            "should inject bids before </body>"
        );
        let bids_pos = html
            .find("window.tsjs=window.tsjs||{}")
            .expect("bids namespace should be in output");
        let body_close_pos = html.find("</body>").expect("</body> should be in output");
        assert!(bids_pos < body_close_pos, "bids must appear before </body>");
    }

    #[test]
    fn injects_ts_bids_only_once_with_multiple_body_elements() {
        let bids_script = r#"<script>(window.tsjs=window.tsjs||{}).bids=JSON.parse("{\"atf\":{\"hb_pb\":\"1.00\"}}");</script>"#;
        let state = std::sync::Arc::new(std::sync::Mutex::new(Some(bids_script.to_string())));
        let config = HtmlProcessorConfig {
            csp_nonce_observed: None,
            public_prefix: None,
            prepared: Arc::new(Prepared::default()),
            body_close: BodyCloseInjection::InlineBids,
            origin_host: "origin.example.com".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::empty_for_tests(),
            ad_slots_script: Some(
                r#"<script>(window.tsjs=window.tsjs||{}).adSlots=[];</script>"#.to_string(),
            ),
            permissions_script: None,
            ad_bids_state: state,
            max_buffered_body_bytes: 16 * 1024 * 1024,
            request_state: IntegrationRequestState::default(),
        };
        let mut processor = create_html_processor(config);
        // Malformed HTML with two <body> elements (common in CMS template pages)
        let output = processor
            .process_chunk(b"<html><body><body>content</body></body></html>", true)
            .expect("should process");
        let html = std::str::from_utf8(&output).expect("should be utf8");
        assert_eq!(
            html.matches(".bids=JSON.parse").count(),
            1,
            "should inject tsjs.bids exactly once even with multiple <body> elements"
        );
    }

    #[test]
    fn golden_url_rewriting_replaces_origin_in_href() {
        // href attributes pointing at origin domain must be rewritten to proxy host.
        let origin = "https://origin.test-publisher.example.com";
        let html = format!(
            r#"<!DOCTYPE html><html><head></head><body>
        <a href="{origin}/page">Link</a>
        <img src="{origin}/img.png">
        </body></html>"#
        );

        let request_host = "proxy.test-publisher.example.com";
        let config = HtmlProcessorConfig {
            csp_nonce_observed: None,
            public_prefix: None,
            prepared: Arc::new(Prepared::default()),
            body_close: BodyCloseInjection::None,
            origin_host: "origin.test-publisher.example.com".to_string(),
            request_host: request_host.to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::default(),
            ad_slots_script: None,
            permissions_script: None,
            ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
            max_buffered_body_bytes: 16 * 1024 * 1024,
            request_state: IntegrationRequestState::default(),
        };
        let mut processor = create_html_processor(config);
        let output = processor
            .process_chunk(html.as_bytes(), true)
            .expect("should process HTML");
        let output_str = std::str::from_utf8(&output).expect("should be valid UTF-8");

        assert!(
            !output_str.contains("origin.test-publisher.example.com"),
            "origin host must not appear in rewritten HTML"
        );
        assert!(
            output_str.contains(request_host),
            "proxy host must appear in rewritten HTML"
        );
    }

    #[test]
    fn golden_integration_script_is_not_double_injected() {
        // Integration scripts from the registry must appear exactly once.
        let html = r#"<!DOCTYPE html>
<html><head></head><body><p>Content</p></body></html>"#;

        let config = create_test_config();
        let mut processor = create_html_processor(config);
        let output = processor
            .process_chunk(html.as_bytes(), true)
            .expect("should process HTML");
        let output_str = std::str::from_utf8(&output).expect("should be valid UTF-8");

        let script_count = output_str.matches("/static/tsjs=").count();
        assert_eq!(
            script_count, 1,
            "script tag must appear exactly once, found {script_count} occurrences"
        );
    }

    #[test]
    fn injects_empty_ts_bids_when_slots_matched_but_auction_returned_nothing() {
        // Slots matched (ad_slots_script is Some) but auction task never wrote a result
        // (state is None) — e.g. auction timed out with zero bids. Fallback to {}.
        let state = std::sync::Arc::new(std::sync::Mutex::new(None));
        let config = HtmlProcessorConfig {
            csp_nonce_observed: None,
            public_prefix: None,
            prepared: Arc::new(Prepared::default()),
            body_close: BodyCloseInjection::InlineBids,
            origin_host: "origin.example.com".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::empty_for_tests(),
            ad_slots_script: Some(
                r#"<script>(window.tsjs=window.tsjs||{}).adSlots=[];</script>"#.to_string(),
            ),
            permissions_script: None,
            ad_bids_state: state,
            max_buffered_body_bytes: 16 * 1024 * 1024,
            request_state: IntegrationRequestState::default(),
        };
        let mut processor = create_html_processor(config);
        let output = processor
            .process_chunk(b"<html><head></head><body>content</body></html>", true)
            .expect("should process");
        let html = std::str::from_utf8(&output).expect("should be utf8");
        assert!(
            html.contains("JSON.parse(\"{}\")"),
            "should inject empty bids fallback when auction produced nothing"
        );
    }

    #[test]
    fn does_not_inject_ts_bids_when_no_slots_matched() {
        // No slots matched this URL — ad_slots_script is None. tsjs.bids must be
        // omitted entirely so the publisher's existing client-side GPT flow is
        // unmodified (spec §8: "Existing client-side Prebid/GPT flow runs unmodified").
        let state = std::sync::Arc::new(std::sync::Mutex::new(None));
        let config = HtmlProcessorConfig {
            csp_nonce_observed: None,
            public_prefix: None,
            prepared: Arc::new(Prepared::default()),
            body_close: BodyCloseInjection::None,
            origin_host: "origin.example.com".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::empty_for_tests(),
            ad_slots_script: None,
            permissions_script: None,
            ad_bids_state: state,
            max_buffered_body_bytes: 16 * 1024 * 1024,
            request_state: IntegrationRequestState::default(),
        };
        let mut processor = create_html_processor(config);
        let output = processor
            .process_chunk(b"<html><head></head><body>content</body></html>", true)
            .expect("should process");
        let html = std::str::from_utf8(&output).expect("should be utf8");
        assert!(
            !html.contains("JSON.parse"),
            "should NOT inject tsjs.bids when no slots matched"
        );
    }

    fn marker_mode_config(marker: &str, observer: Option<Arc<AtomicBool>>) -> HtmlProcessorConfig {
        HtmlProcessorConfig {
            csp_nonce_observed: observer,
            public_prefix: None,
            body_close: BodyCloseInjection::Marker(marker.to_string()),
            origin_host: "origin.example.com".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::empty_for_tests(),
            ad_slots_script: None,
            permissions_script: None,
            ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
            max_buffered_body_bytes: 16 * 1024 * 1024,
            prepared: Arc::new(Prepared::default()),
            request_state: IntegrationRequestState::default(),
        }
    }

    fn render_marker_mode(marker: &str, source: &str) -> String {
        let mut processor = create_html_processor(marker_mode_config(marker, None));
        let output = processor
            .process_chunk(source.as_bytes(), true)
            .expect("should process the document");
        String::from_utf8(output).expect("output should be utf8")
    }

    #[test]
    fn marker_mode_ignores_a_body_close_written_in_script_data() {
        // A reverse byte search for `</body>` picks this string literal, because the
        // document has no structural close at all. Splicing a `<script>` payload there
        // emits a `</script>` inside the publisher's script and corrupts the document —
        // and, once stored, every warm reader of it. Only the parser can tell the
        // difference, so the parser places the marker.
        const MARKER: &str = "<!--ts-seam-slot-->";
        let source =
            r#"<html><head></head><script>const marker = "</body>";</script><p>a</p></html>"#;

        let html = render_marker_mode(MARKER, source);

        assert!(
            html.contains(r#"const marker = "</body>";"#),
            "should leave the publisher's script data byte for byte: {html}"
        );
        assert_eq!(
            html.matches(MARKER).count(),
            1,
            "should emit exactly one transform-owned marker: {html}"
        );
        assert!(
            html.ends_with(MARKER),
            "a document with no structural body close takes the terminal marker: {html}"
        );
    }

    #[test]
    fn marker_mode_prefers_the_structural_body_close_over_trailing_comment_data() {
        // A reverse byte search takes the *last* `</body>` sequence, which here lives in
        // trailing comment data, so the marker landed after the document's real end.
        const MARKER: &str = "<!--ts-seam-slot-->";
        let source = "<html><body><p>a</p></body><!-- </body> --></html>";

        let html = render_marker_mode(MARKER, source);

        assert!(
            html.contains(&format!("<p>a</p>{MARKER}</body>")),
            "should place the marker at the structural body close: {html}"
        );
        assert!(
            html.contains("<!-- </body> -->"),
            "should leave the publisher's trailing comment untouched: {html}"
        );
        assert_eq!(
            html.matches(MARKER).count(),
            1,
            "should emit exactly one transform-owned marker: {html}"
        );
    }

    #[test]
    fn deferred_inline_marker_uses_only_the_structural_body_end() {
        const TOKEN: &str = "<!--ts-inline-body-close-test-->";
        let state =
            std::sync::Arc::new(std::sync::Mutex::new(Some("must-not-be-read".to_string())));
        let mut config = marker_mode_config(TOKEN, None);
        config.body_close = BodyCloseInjection::DeferredInlineMarker(TOKEN.to_string());
        config.ad_bids_state = state;
        let mut processor = create_html_processor(config);
        let output = processor
            .process_chunk(
                br#"<html><body><script>const x="</body>";</script><!-- </body> --></body></html>"#,
                true,
            )
            .expect("should process deferred marker document");
        let html = String::from_utf8(output).expect("output should be UTF-8");

        assert_eq!(
            html.matches(TOKEN).count(),
            1,
            "should emit one marker: {html}"
        );
        assert!(
            html.contains(&format!("<!-- </body> -->{TOKEN}</body>")),
            "marker should precede only the structural close: {html}"
        );
        assert!(!html.contains("must-not-be-read"));
    }

    #[test]
    fn deferred_inline_marker_is_absent_without_an_explicit_body_end() {
        const TOKEN: &str = "<!--ts-inline-body-close-test-->";
        let mut config = marker_mode_config(TOKEN, None);
        config.body_close = BodyCloseInjection::DeferredInlineMarker(TOKEN.to_string());
        let mut processor = create_html_processor(config);
        let output = processor
            .process_chunk(b"<html><script>const x='</body>'</script></html>", true)
            .expect("should process bodyless document");
        let html = String::from_utf8(output).expect("output should be UTF-8");

        assert!(
            !html.contains(TOKEN),
            "bodyless document must have no marker: {html}"
        );
    }

    #[test]
    fn deferred_inline_marker_uses_parser_context_across_every_source_split() {
        const TOKEN: &str = "<!--ts-inline-body-close-test-->";
        for source in [
            "<html><body><p>x</p></BoDy></html>",
            "<html><body><script>const x='</body>';</script><p>later</p></body></html>",
            "<html><body><!-- </body> --><p>later</p></body></html>",
        ] {
            for split in 0..=source.len() {
                let mut config = marker_mode_config(TOKEN, None);
                config.body_close = BodyCloseInjection::DeferredInlineMarker(TOKEN.to_string());
                let mut processor = create_html_processor(config);
                let mut output = processor
                    .process_chunk(&source.as_bytes()[..split], false)
                    .expect("should process first source fragment");
                output.extend(
                    processor
                        .process_chunk(&source.as_bytes()[split..], true)
                        .expect("should process final source fragment"),
                );
                let html = String::from_utf8(output).expect("output should be UTF-8");
                assert_eq!(
                    html.matches(TOKEN).count(),
                    1,
                    "should mark one structural close for split {split}: {html}"
                );
                assert!(
                    html.to_ascii_lowercase()
                        .contains(&format!("{TOKEN}</body>")),
                    "marker should precede structural close for split {split}: {html}"
                );
            }
        }
    }

    #[test]
    fn a_nonce_bearing_meta_policy_is_observed() {
        let observed = Arc::new(AtomicBool::new(false));
        let mut processor =
            create_html_processor(marker_mode_config("<!--m-->", Some(Arc::clone(&observed))));

        processor
            .process_chunk(
                br#"<html><head><meta http-equiv="Content-Security-Policy" content="script-src 'nonce-abc123'"></head><body>a</body></html>"#,
                true,
            )
            .expect("should process the document");

        assert!(
            observed.load(Ordering::SeqCst),
            "a policy delivered in markup is invisible to the response-header gate"
        );
    }

    #[test]
    fn a_nonce_attribute_is_observed() {
        let observed = Arc::new(AtomicBool::new(false));
        let mut processor =
            create_html_processor(marker_mode_config("<!--m-->", Some(Arc::clone(&observed))));

        processor
            .process_chunk(
                b"<html><head><script nonce=\"abc123\"></script></head><body>a</body></html>",
                true,
            )
            .expect("should process the document");

        assert!(
            observed.load(Ordering::SeqCst),
            "a document written for a per-response nonce must not be shared"
        );
    }

    #[test]
    fn the_word_nonce_in_script_text_is_not_observed() {
        // The reason this is structural rather than a byte scan over the output.
        let observed = Arc::new(AtomicBool::new(false));
        let mut processor =
            create_html_processor(marker_mode_config("<!--m-->", Some(Arc::clone(&observed))));

        processor
            .process_chunk(
                br#"<html><head><script>var nonce = "not-a-policy";</script><meta http-equiv="refresh" content="0"></head><body>a</body></html>"#,
                true,
            )
            .expect("should process the document");

        assert!(
            !observed.load(Ordering::SeqCst),
            "ordinary script text must not cost a cacheable page its shared template"
        );
    }

    #[test]
    fn bodyless_marker_mode_emits_an_owned_terminal_seam_even_after_source_bytes() {
        const MARKER: &str = "<!--reserved-template-cache-seam-->";
        let config = HtmlProcessorConfig {
            csp_nonce_observed: None,
            public_prefix: None,
            prepared: Arc::new(Prepared::default()),
            body_close: BodyCloseInjection::Marker(MARKER.to_string()),
            origin_host: "origin.example.com".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::empty_for_tests(),
            ad_slots_script: None,
            permissions_script: None,
            ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
            max_buffered_body_bytes: 16 * 1024 * 1024,
            request_state: IntegrationRequestState::default(),
        };
        let source =
            format!(r#"<html><head></head><script>var collision="{MARKER}";</script></html>"#);

        let mut processor = create_html_processor(config);
        let output = processor
            .process_chunk(source.as_bytes(), true)
            .expect("should process bodyless HTML");
        let html = std::str::from_utf8(&output).expect("should be utf8");

        assert_eq!(
            html.matches(MARKER).count(),
            2,
            "one source occurrence plus the transform-owned terminal seam must survive processing; repeated markers are rejected before template caching"
        );
        assert!(
            html.ends_with(MARKER),
            "the transform-owned template-cache fallback must be unambiguously terminal"
        );
    }

    #[test]
    fn response_size_does_not_grow_disproportionately() {
        // Processing must not expand HTML by more than 1.1× (accounts for the
        // injected script tag + URL rewrites). Disproportionate growth indicates
        // a bug (e.g., double-processing, buffer leak).
        let html = include_str!("html_processor.test.html");
        let input_size = html.len();

        let config = create_test_config();
        let mut processor = create_html_processor(config);
        let output = processor
            .process_chunk(html.as_bytes(), true)
            .expect("should process HTML");

        let output_size = output.len();
        let growth_factor = output_size as f64 / input_size as f64;

        assert!(
            growth_factor < MAX_GROWTH_FACTOR,
            "processed HTML must not grow by more than {MAX_GROWTH_FACTOR}×: input={input_size}B output={output_size}B factor={growth_factor:.2}"
        );
    }
}
