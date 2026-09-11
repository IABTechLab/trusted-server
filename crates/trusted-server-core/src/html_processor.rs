//! Simplified HTML processor that combines URL replacement and integration injection
//!
//! This module provides a `StreamProcessor` implementation for HTML content.
use std::cell::Cell;
use std::io;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lol_html::{
    EndTagHandler, Settings as RewriterSettings, element, end,
    html_content::{ContentType, EndTag},
    text,
};

use crate::integrations::datadome::{DATADOME_INTEGRATION_ID, DataDomeClientTagSuppressed};
use crate::integrations::gpt_diagnostics::GptDiagnosticsRequestDecision;
use crate::integrations::{
    AttributeRewriteOutcome, IntegrationAttributeContext, IntegrationDocumentState,
    IntegrationHtmlContext, IntegrationRegistry, IntegrationScriptContext, ScriptRewriteAction,
};
use crate::publisher::build_empty_bids_script;
use crate::settings::Settings;
use crate::streaming_processor::{HtmlRewriterAdapter, StreamProcessor};
use crate::tsjs;

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
    /// Request-scoped conditional diagnostics delivery decision.
    pub gpt_diagnostics: Option<GptDiagnosticsRequestDecision>,
    /// What the `</body>` seam injects. Decided by the caller rather than inferred
    /// from [`Self::ad_slots_script`].
    pub body_close: BodyCloseInjection,
    /// Whether to omit Trusted Server's automatic `DataDome` client-side tag.
    pub suppress_datadome_client_side_tag: bool,
    /// Set when the document delivers a response-bound CSP nonce in its own markup.
    ///
    /// `None` on every path that cannot store a shared template, so an ordinary inline
    /// request does not pay for handlers whose only consumer is the template-cache gate.
    pub csp_nonce_observed: Option<Arc<AtomicBool>>,
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
            ad_slots_script: None,
            ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
            max_buffered_body_bytes: settings.publisher.max_buffered_body_bytes,
            gpt_diagnostics: None,
            body_close: BodyCloseInjection::None,
            suppress_datadome_client_side_tag: false,
            csp_nonce_observed: None,
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

    /// Attach the request-scoped conditional diagnostics decision.
    #[must_use]
    pub fn with_gpt_diagnostics(mut self, decision: Option<GptDiagnosticsRequestDecision>) -> Self {
        self.gpt_diagnostics = decision;
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

    /// Attach the request-scoped `DataDome` client-tag suppression decision.
    #[must_use]
    pub fn with_datadome_client_tag_suppression(mut self, suppress: bool) -> Self {
        self.suppress_datadome_client_side_tag = suppress;
        self
    }
}

/// Create an HTML processor with URL replacement and integration hooks.
///
/// # Panics
///
/// Panics if the `ad_bids_state` `Mutex` is poisoned. This cannot happen in
/// normal operation since no code holds the lock across a panic boundary.
#[must_use]
#[allow(
    clippy::needless_pass_by_value,
    reason = "the returned processor owns request configuration captured by its handlers"
)]
pub fn create_html_processor(config: HtmlProcessorConfig) -> impl StreamProcessor {
    let stream_processor_factories = config.integrations.html_stream_processor_factories();
    let document_state = IntegrationDocumentState::default();
    if config.suppress_datadome_client_side_tag {
        document_state.get_or_insert_with(DATADOME_INTEGRATION_ID, || DataDomeClientTagSuppressed);
    }

    // Simplified URL patterns structure - stores only core data and generates variants on-demand
    struct UrlPatterns {
        origin_host: String,
        request_host: String,
        request_scheme: String,
    }

    impl UrlPatterns {
        fn https_origin(&self) -> String {
            format!("https://{}", self.origin_host)
        }

        fn http_origin(&self) -> String {
            format!("http://{}", self.origin_host)
        }

        fn protocol_relative_origin(&self) -> String {
            format!("//{}", self.origin_host)
        }

        fn replacement_url(&self) -> String {
            format!("{}://{}", self.request_scheme, self.request_host)
        }

        fn protocol_relative_replacement(&self) -> String {
            format!("//{}", self.request_host)
        }

        fn rewrite_url_value(&self, value: &str) -> Option<String> {
            if !value.contains(&self.origin_host) {
                return None;
            }

            let https_origin = self.https_origin();
            let http_origin = self.http_origin();
            let protocol_relative_origin = self.protocol_relative_origin();
            let replacement_url = self.replacement_url();
            let protocol_relative_replacement = self.protocol_relative_replacement();

            let mut rewritten = value
                .replace(&https_origin, &replacement_url)
                .replace(&http_origin, &replacement_url)
                .replace(&protocol_relative_origin, &protocol_relative_replacement);

            if rewritten.starts_with(&self.origin_host) {
                let suffix = &rewritten[self.origin_host.len()..];
                let boundary_ok = suffix.is_empty()
                    || matches!(suffix.as_bytes().first(), Some(b'/' | b'?' | b'#'));
                if boundary_ok {
                    rewritten = format!("{}{}", self.request_host, suffix);
                }
            }

            (rewritten != value).then_some(rewritten)
        }
    }

    let patterns = Rc::new(UrlPatterns {
        origin_host: config.origin_host.clone(),
        request_host: config.request_host.clone(),
        request_scheme: config.request_scheme.clone(),
    });

    let injected_tsjs = Rc::new(Cell::new(false));
    let injected_bids = Arc::new(AtomicBool::new(false));
    let integration_registry = config.integrations.clone();
    let script_rewriters = integration_registry.script_rewriters();
    let ad_slots_script = config.ad_slots_script.clone();
    let body_close = config.body_close.clone();
    let ad_bids_state = config.ad_bids_state.clone();
    let gpt_diagnostics = config.gpt_diagnostics.clone();

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
            let patterns = patterns.clone();
            let document_state = document_state.clone();
            let ad_slots_script = ad_slots_script.clone();
            let gpt_diagnostics = gpt_diagnostics.clone();
            move |el| {
                if !injected_tsjs.get() {
                    let mut snippet = String::new();
                    // Inject ad slots script first so it appears before tsjs bundle.
                    if let Some(ref slots_script) = ad_slots_script {
                        snippet.push_str(slots_script);
                    }
                    let ctx = IntegrationHtmlContext {
                        request_host: &patterns.request_host,
                        request_scheme: &patterns.request_scheme,
                        origin_host: &patterns.origin_host,
                        document_state: &document_state,
                    };
                    // First inject integration-specific config (e.g., window.__tsjs_prebid)
                    // so it's available when the bundle's auto-init code reads it.
                    for insert in integrations.head_inserts(&ctx) {
                        snippet.push_str(&insert);
                    }
                    if let Some(bootstrap) = gpt_diagnostics
                        .as_ref()
                        .and_then(GptDiagnosticsRequestDecision::bootstrap_script)
                    {
                        snippet.push_str(&bootstrap);
                    }
                    // Main bundle: core + non-deferred integrations (synchronous).
                    let immediate_ids = integrations.js_module_ids_immediate();
                    let script_attributes = integrations.tsjs_script_tag_attributes();
                    snippet.push_str(&tsjs::tsjs_script_tag_with_attributes(
                        &immediate_ids,
                        &script_attributes,
                    ));
                    // Active diagnostics loads synchronously after core so its
                    // GPT listeners precede publisher scripts in the origin head.
                    if let Some(module_tag) = gpt_diagnostics
                        .as_ref()
                        .and_then(GptDiagnosticsRequestDecision::module_script_tag)
                    {
                        snippet.push_str(&module_tag);
                    }
                    // Deferred bundles: large modules like prebid loaded after
                    // HTML parsing completes. Empty when none are enabled.
                    let deferred_ids = integrations.js_module_ids_deferred();
                    snippet.push_str(&tsjs::tsjs_deferred_script_tags(&deferred_ids));
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
            let integrations = integration_registry.clone();
            move |el| {
                if let Some(mut href) = el.get_attribute("href") {
                    let original_href = href.clone();
                    let element_name = el.tag_name();
                    if let Some(rewritten) = patterns.rewrite_url_value(&href) {
                        href = rewritten;
                    }

                    match integrations.rewrite_attribute(
                        "href",
                        &href,
                        &IntegrationAttributeContext {
                            attribute_name: "href",
                            element_name: &element_name,
                            request_host: &patterns.request_host,
                            request_scheme: &patterns.request_scheme,
                            origin_host: &patterns.origin_host,
                        },
                    ) {
                        AttributeRewriteOutcome::Unchanged => {}
                        AttributeRewriteOutcome::Replaced(integration_href) => {
                            href = integration_href;
                        }
                        AttributeRewriteOutcome::RemoveElement => {
                            el.remove();
                            return Ok(());
                        }
                    }

                    if href != original_href {
                        el.set_attribute("href", &href)?;
                    }
                }
                Ok(())
            }
        }),
        // Replace URLs in src attributes
        element!("[src]", {
            let patterns = patterns.clone();
            let integrations = integration_registry.clone();
            move |el| {
                if let Some(mut src) = el.get_attribute("src") {
                    let original_src = src.clone();
                    let element_name = el.tag_name();
                    if let Some(rewritten) = patterns.rewrite_url_value(&src) {
                        src = rewritten;
                    }
                    match integrations.rewrite_attribute(
                        "src",
                        &src,
                        &IntegrationAttributeContext {
                            attribute_name: "src",
                            element_name: &element_name,
                            request_host: &patterns.request_host,
                            request_scheme: &patterns.request_scheme,
                            origin_host: &patterns.origin_host,
                        },
                    ) {
                        AttributeRewriteOutcome::Unchanged => {}
                        AttributeRewriteOutcome::Replaced(integration_src) => {
                            src = integration_src;
                        }
                        AttributeRewriteOutcome::RemoveElement => {
                            el.remove();
                            return Ok(());
                        }
                    }

                    if src != original_src {
                        el.set_attribute("src", &src)?;
                    }
                }
                Ok(())
            }
        }),
        // Replace URLs in action attributes
        element!("[action]", {
            let patterns = patterns.clone();
            let integrations = integration_registry.clone();
            move |el| {
                if let Some(mut action) = el.get_attribute("action") {
                    let original_action = action.clone();
                    let element_name = el.tag_name();
                    if let Some(rewritten) = patterns.rewrite_url_value(&action) {
                        action = rewritten;
                    }

                    match integrations.rewrite_attribute(
                        "action",
                        &action,
                        &IntegrationAttributeContext {
                            attribute_name: "action",
                            element_name: &element_name,
                            request_host: &patterns.request_host,
                            request_scheme: &patterns.request_scheme,
                            origin_host: &patterns.origin_host,
                        },
                    ) {
                        AttributeRewriteOutcome::Unchanged => {}
                        AttributeRewriteOutcome::Replaced(integration_action) => {
                            action = integration_action;
                        }
                        AttributeRewriteOutcome::RemoveElement => {
                            el.remove();
                            return Ok(());
                        }
                    }

                    if action != original_action {
                        el.set_attribute("action", &action)?;
                    }
                }
                Ok(())
            }
        }),
        // Replace URLs in srcset attributes (for responsive images)
        element!("[srcset]", {
            let patterns = patterns.clone();
            let integrations = integration_registry.clone();
            move |el| {
                if let Some(mut srcset) = el.get_attribute("srcset") {
                    let original_srcset = srcset.clone();
                    let element_name = el.tag_name();
                    let new_srcset = srcset
                        .replace(&patterns.https_origin(), &patterns.replacement_url())
                        .replace(&patterns.http_origin(), &patterns.replacement_url())
                        .replace(
                            &patterns.protocol_relative_origin(),
                            &patterns.protocol_relative_replacement(),
                        )
                        .replace(&patterns.origin_host, &patterns.request_host);
                    if new_srcset != srcset {
                        srcset = new_srcset;
                    }

                    match integrations.rewrite_attribute(
                        "srcset",
                        &srcset,
                        &IntegrationAttributeContext {
                            attribute_name: "srcset",
                            element_name: &element_name,
                            request_host: &patterns.request_host,
                            request_scheme: &patterns.request_scheme,
                            origin_host: &patterns.origin_host,
                        },
                    ) {
                        AttributeRewriteOutcome::Unchanged => {}
                        AttributeRewriteOutcome::Replaced(integration_srcset) => {
                            srcset = integration_srcset;
                        }
                        AttributeRewriteOutcome::RemoveElement => {
                            el.remove();
                            return Ok(());
                        }
                    }

                    if srcset != original_srcset {
                        el.set_attribute("srcset", &srcset)?;
                    }
                }
                Ok(())
            }
        }),
        // Replace URLs in imagesrcset attributes (for link preload)
        element!("[imagesrcset]", {
            let patterns = patterns.clone();
            let integrations = integration_registry.clone();
            move |el| {
                if let Some(mut imagesrcset) = el.get_attribute("imagesrcset") {
                    let original_imagesrcset = imagesrcset.clone();
                    let element_name = el.tag_name();
                    let new_imagesrcset = imagesrcset
                        .replace(&patterns.https_origin(), &patterns.replacement_url())
                        .replace(&patterns.http_origin(), &patterns.replacement_url())
                        .replace(
                            &patterns.protocol_relative_origin(),
                            &patterns.protocol_relative_replacement(),
                        );
                    if new_imagesrcset != imagesrcset {
                        imagesrcset = new_imagesrcset;
                    }

                    match integrations.rewrite_attribute(
                        "imagesrcset",
                        &imagesrcset,
                        &IntegrationAttributeContext {
                            attribute_name: "imagesrcset",
                            element_name: &element_name,
                            request_host: &patterns.request_host,
                            request_scheme: &patterns.request_scheme,
                            origin_host: &patterns.origin_host,
                        },
                    ) {
                        AttributeRewriteOutcome::Unchanged => {}
                        AttributeRewriteOutcome::Replaced(integration_imagesrcset) => {
                            imagesrcset = integration_imagesrcset;
                        }
                        AttributeRewriteOutcome::RemoveElement => {
                            el.remove();
                            return Ok(());
                        }
                    }

                    if imagesrcset != original_imagesrcset {
                        el.set_attribute("imagesrcset", &imagesrcset)?;
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

    for script_rewriter in script_rewriters {
        let selector = script_rewriter.selector();
        let rewriter = script_rewriter.clone();
        let patterns = patterns.clone();
        let document_state = document_state.clone();
        element_content_handlers.push(text!(selector, {
            let rewriter = rewriter.clone();
            let patterns = patterns.clone();
            let document_state = document_state.clone();
            move |text| {
                let ctx = IntegrationScriptContext {
                    selector,
                    request_host: &patterns.request_host,
                    request_scheme: &patterns.request_scheme,
                    origin_host: &patterns.origin_host,
                    is_last_in_text_node: text.last_in_text_node(),
                    max_buffered_script_bytes: config.max_buffered_body_bytes,
                    document_state: &document_state,
                };
                match rewriter.rewrite(text.as_str(), &ctx) {
                    ScriptRewriteAction::Keep => {}
                    ScriptRewriteAction::Replace(rewritten) => {
                        text.replace(&rewritten, ContentType::Text);
                    }
                    ScriptRewriteAction::RemoveNode => {
                        text.remove();
                    }
                }
                Ok(())
            }
        }));
    }

    let rewriter_settings = RewriterSettings {
        document_content_handlers,
        element_content_handlers,
        ..RewriterSettings::default()
    };

    let inner = HtmlRewriterAdapter::new(rewriter_settings);

    let stream_context = crate::integrations::IntegrationHtmlStreamContext {
        request_host: config.request_host.clone(),
        request_scheme: config.request_scheme.clone(),
        origin_host: config.origin_host.clone(),
        document_state: document_state.clone(),
    };
    let processors = stream_processor_factories
        .into_iter()
        .map(|factory| factory.create(stream_context.clone()))
        .collect();
    HtmlWithStreamingProcessors {
        inner: Box::new(inner),
        processors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integrations::{
        AttributeRewriteAction, IntegrationAttributeContext, IntegrationAttributeRewriter,
        IntegrationHeadInjector, IntegrationHtmlContext,
    };
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
            body_close: BodyCloseInjection::None,
            origin_host: "origin.example.com".to_owned(),
            request_host: "test.example.com".to_owned(),
            request_scheme: "https".to_owned(),
            integrations: IntegrationRegistry::default(),
            ad_slots_script: None,
            ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
            max_buffered_body_bytes: 16 * 1024 * 1024,
            gpt_diagnostics: None,
            suppress_datadome_client_side_tag: false,
        }
    }

    #[test]
    fn integration_attribute_rewriter_can_remove_elements() {
        struct RemovingLinkRewriter;

        impl IntegrationAttributeRewriter for RemovingLinkRewriter {
            fn integration_id(&self) -> &'static str {
                "removing"
            }

            fn handles_attribute(&self, attribute: &str) -> bool {
                attribute == "href"
            }

            fn rewrite(
                &self,
                _attr_name: &str,
                attr_value: &str,
                _ctx: &IntegrationAttributeContext<'_>,
            ) -> AttributeRewriteAction {
                if attr_value.contains("remove-me") {
                    AttributeRewriteAction::remove_element()
                } else {
                    AttributeRewriteAction::keep()
                }
            }
        }

        let html = r#"<html><body>
            <a href="https://origin.example.com/remove-me">remove</a>
            <a href="https://origin.example.com/keep-me">keep</a>
        </body></html>"#;

        let mut config = create_test_config();
        config.integrations =
            IntegrationRegistry::from_rewriters(vec![Arc::new(RemovingLinkRewriter)], Vec::new());

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
        let processed = String::from_utf8(output).expect("output should be valid UTF-8");

        assert!(processed.contains("keep-me"));
        assert!(!processed.contains("remove-me"));
    }

    #[test]
    fn integration_head_injector_prepends_after_tsjs_once() {
        struct TestHeadInjector;

        impl IntegrationHeadInjector for TestHeadInjector {
            fn integration_id(&self) -> &'static str {
                "test"
            }

            fn head_inserts(&self, _ctx: &IntegrationHtmlContext<'_>) -> Vec<String> {
                vec!["<script>window.__testHeadInjector=true;</script>".to_owned()]
            }
        }

        let html = "<html><head><title>Test</title></head><body></body></html>";

        let mut config = create_test_config();
        config.integrations = IntegrationRegistry::from_rewriters_with_head_injectors(
            Vec::new(),
            Vec::new(),
            vec![Arc::new(TestHeadInjector)],
        );

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
        let processed = String::from_utf8(output).expect("output should be valid UTF-8");

        let tsjs_marker = "id=\"trustedserver-js\"";
        let head_marker = "window.__testHeadInjector=true";

        assert_eq!(
            processed.matches(tsjs_marker).count(),
            1,
            "should inject unified tsjs tag once"
        );
        assert_eq!(
            processed.matches(head_marker).count(),
            1,
            "should inject head snippet once"
        );

        let tsjs_index = processed
            .find(tsjs_marker)
            .expect("should include unified tsjs tag");
        let head_index = processed
            .find(head_marker)
            .expect("should include head snippet");
        let title_index = processed
            .find("<title>")
            .expect("should keep existing head content");

        assert!(
            head_index < tsjs_index,
            "should inject config before tsjs bundle so auto-init can read it"
        );
        assert!(
            tsjs_index < title_index,
            "should prepend all injected content before existing head content"
        );
    }

    #[test]
    fn integration_head_injector_marks_only_attribution_enabled_gpt_bundle() {
        fn process(gpt_config: Option<(bool, bool)>) -> String {
            let integrations = if let Some((enabled, gam_attribution_enabled)) = gpt_config {
                let mut settings = create_test_settings();
                settings
                    .integrations
                    .insert_config(
                        "gpt",
                        &json!({
                            "enabled": enabled,
                            "gam_attribution_enabled": gam_attribution_enabled
                        }),
                    )
                    .expect("should insert GPT config");
                IntegrationRegistry::new(&settings).expect("should build GPT registry")
            } else {
                IntegrationRegistry::empty_for_tests()
            };
            let mut config = create_test_config();
            config.integrations = integrations;
            let mut processor = create_html_processor(config);
            let output = processor
                .process_chunk(b"<html><head></head><body></body></html>", true)
                .expect("should process HTML");

            String::from_utf8(output).expect("should produce valid UTF-8")
        }

        let attributed = process(Some((true, true)));
        let unattributed = process(Some((true, false)));
        let disabled_gpt = process(Some((false, true)));
        let without_gpt = process(None);

        for html in [&attributed, &unattributed, &disabled_gpt, &without_gpt] {
            assert_eq!(
                html.matches("id=\"trustedserver-js\"").count(),
                1,
                "should emit exactly one publisher bundle tag: {html}"
            );
        }
        assert!(
            attributed.contains("data-ts-gam-attribution=\"true\""),
            "should mark only an attribution-enabled GPT publisher bundle"
        );
        assert!(
            !unattributed.contains("data-ts-gam-attribution"),
            "should leave an attribution-disabled GPT publisher bundle unmarked"
        );
        assert!(
            !disabled_gpt.contains("data-ts-gam-attribution"),
            "should let the GPT master switch suppress attribution metadata"
        );
        assert!(
            !without_gpt.contains("data-ts-gam-attribution"),
            "should leave a non-GPT publisher bundle unmarked"
        );

        let head_insert_index = attributed
            .find("window.__tsjs_installGptShim")
            .expect("should include the GPT head insert");
        let publisher_bundle_index = attributed
            .find("id=\"trustedserver-js\"")
            .expect("should include the publisher bundle");
        assert!(
            head_insert_index < publisher_bundle_index,
            "should keep integration head inserts before the publisher bundle"
        );
    }

    #[test]
    fn active_gpt_diagnostics_loads_standalone_after_unified_bundle_once() {
        let html = "<html><head><title>Test</title></head><body></body></html>";
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config("gpt_diagnostics", &json!({ "enabled": true }))
            .expect("should insert GPT diagnostics config");

        let mut request = http::Request::builder()
            .method(http::Method::GET)
            .uri("https://publisher.example/page?ts_console=1")
            .header("sec-fetch-dest", "document")
            .body(edgezero_core::body::Body::empty())
            .expect("should build activation request");
        let decision =
            crate::integrations::gpt_diagnostics::prepare_request(&settings, &mut request)
                .expect("should prepare diagnostics request");
        let mut config = create_test_config();
        config.integrations = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should build integration registry");
        config.gpt_diagnostics = Some(decision);

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
            .expect("should process HTML");
        let processed = String::from_utf8(output).expect("should produce valid UTF-8");
        let bootstrap_marker = "__tsjs_gpt_diagnostics_active";
        let bundle_marker = "id=\"trustedserver-js\"";
        let diagnostics_marker = "tsjs-gpt_diagnostics.min.js";

        assert_eq!(
            processed.matches(bootstrap_marker).count(),
            1,
            "should inject the diagnostics bootstrap once"
        );
        assert_eq!(
            processed.matches(bundle_marker).count(),
            1,
            "should inject the immediate TSJS bundle once"
        );
        assert_eq!(
            processed.matches(diagnostics_marker).count(),
            1,
            "should inject one standalone diagnostics module"
        );
        let bootstrap_index = processed
            .find(bootstrap_marker)
            .expect("should include diagnostics bootstrap");
        let bundle_index = processed
            .find(bundle_marker)
            .expect("should include immediate TSJS bundle");
        let diagnostics_index = processed
            .find(diagnostics_marker)
            .expect("should include standalone diagnostics module");
        assert!(
            bootstrap_index < bundle_index,
            "should activate before core executes"
        );
        assert!(
            bundle_index < diagnostics_index,
            "should load diagnostics after core"
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
    fn suppressed_datadome_tag_preserves_and_rewrites_publisher_tag() {
        let mut settings = create_test_settings();
        settings
            .integrations
            .insert_config(
                "datadome",
                &json!({
                    "enabled": true,
                    "client_side_key": "test-client-key",
                }),
            )
            .expect("should configure DataDome integration");
        let registry = IntegrationRegistry::new(&settings)
            .expect("should create integration registry with DataDome");
        let config = HtmlProcessorConfig::from_settings(
            &settings,
            &registry,
            "origin.example.com",
            "test.example.com",
            "https",
        )
        .with_datadome_client_tag_suppression(true);
        let mut processor = create_html_processor(config);

        let output = processor
            .process_chunk(
                br#"<html><head><script id="publisher-datadome" src="https://js.datadome.co/tags.js"></script></head><body>content</body></html>"#,
                true,
            )
            .expect("should process HTML");
        let html = String::from_utf8(output).expect("should produce UTF-8 HTML");

        assert!(
            !html.contains("window.ddjskey"),
            "should omit the DataDome client configuration"
        );
        assert!(
            html.contains("id=\"publisher-datadome\""),
            "should preserve the publisher-originated DataDome tag"
        );
        assert!(
            html.contains("src=\"/integrations/datadome/tags.js\""),
            "should rewrite the publisher-originated DataDome tag"
        );
        assert!(
            !html.contains("https://js.datadome.co/tags.js"),
            "should remove the original third-party DataDome URL"
        );
        assert_eq!(
            html.matches("/integrations/datadome/tags.js").count(),
            1,
            "should leave exactly one publisher-originated DataDome tag"
        );
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
    fn test_integration_registry_rewrites_integration_scripts() {
        let html = r#"<html><head>
            <script src="https://cdn.testlight.com/v1/testlight.js"></script>
        </head><body></body></html>"#;

        let mut settings = Settings::default();
        let shim_src = "https://edge.example.com/static/testlight.js".to_owned();
        settings
            .integrations
            .insert_config(
                "testlight",
                &json!({
                    "enabled": true,
                    "endpoint": "https://example.com/openrtb2/auction",
                    "rewrite_scripts": true,
                    "shim_src": shim_src,
                }),
            )
            .expect("should insert testlight config");

        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(
                crate::auction::compile_auction_plan(&settings)
                    .expect("should compile auction plan"),
            ),
        )
        .expect("should create registry");
        let mut config = create_test_config();
        config.integrations = registry;

        let processor = create_html_processor(config);
        let pipeline_config = PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size: 8192,
        };
        let mut pipeline = StreamingPipeline::new(pipeline_config, processor);

        let mut output = Vec::new();
        let result = pipeline.process(Cursor::new(html.as_bytes()), &mut output);
        result.unwrap();

        let processed = String::from_utf8_lossy(&output);
        assert!(
            processed.contains(&shim_src),
            "Integration shim should replace integration script reference"
        );
        assert!(
            !processed.contains("cdn.testlight.com"),
            "Original integration URL should be removed"
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
            body_close: BodyCloseInjection::None,
            origin_host: "origin.example.com".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::empty_for_tests(),
            ad_slots_script: Some(
                r#"<script>(window.tsjs=window.tsjs||{}).adSlots=JSON.parse("[]");</script>"#
                    .to_string(),
            ),
            ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
            max_buffered_body_bytes: 16 * 1024 * 1024,
            gpt_diagnostics: None,
            suppress_datadome_client_side_tag: false,
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
            body_close: BodyCloseInjection::InlineBids,
            origin_host: "origin.example.com".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::empty_for_tests(),
            ad_slots_script: Some(
                r#"<script>(window.tsjs=window.tsjs||{}).adSlots=[];</script>"#.to_string(),
            ),
            ad_bids_state: state,
            max_buffered_body_bytes: 16 * 1024 * 1024,
            gpt_diagnostics: None,
            suppress_datadome_client_side_tag: false,
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
            body_close: BodyCloseInjection::InlineBids,
            origin_host: "origin.example.com".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::empty_for_tests(),
            ad_slots_script: Some(
                r#"<script>(window.tsjs=window.tsjs||{}).adSlots=[];</script>"#.to_string(),
            ),
            ad_bids_state: state,
            max_buffered_body_bytes: 16 * 1024 * 1024,
            gpt_diagnostics: None,
            suppress_datadome_client_side_tag: false,
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
            body_close: BodyCloseInjection::None,
            origin_host: "origin.test-publisher.example.com".to_string(),
            request_host: request_host.to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::default(),
            ad_slots_script: None,
            ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
            max_buffered_body_bytes: 16 * 1024 * 1024,
            gpt_diagnostics: None,
            suppress_datadome_client_side_tag: false,
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
            body_close: BodyCloseInjection::InlineBids,
            origin_host: "origin.example.com".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::empty_for_tests(),
            ad_slots_script: Some(
                r#"<script>(window.tsjs=window.tsjs||{}).adSlots=[];</script>"#.to_string(),
            ),
            ad_bids_state: state,
            max_buffered_body_bytes: 16 * 1024 * 1024,
            gpt_diagnostics: None,
            suppress_datadome_client_side_tag: false,
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
            body_close: BodyCloseInjection::None,
            origin_host: "origin.example.com".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::empty_for_tests(),
            ad_slots_script: None,
            ad_bids_state: state,
            max_buffered_body_bytes: 16 * 1024 * 1024,
            gpt_diagnostics: None,
            suppress_datadome_client_side_tag: false,
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
            body_close: BodyCloseInjection::Marker(marker.to_string()),
            origin_host: "origin.example.com".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::empty_for_tests(),
            ad_slots_script: None,
            ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
            max_buffered_body_bytes: 16 * 1024 * 1024,
            gpt_diagnostics: None,
            suppress_datadome_client_side_tag: false,
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
    fn nextjs_output_overflow_restores_in_progress_script_at_every_split() {
        let mut settings = create_test_settings();
        settings.integrations.insert(
            "nextjs".to_owned(),
            json!({"enabled": true, "max_combined_payload_bytes": 128}),
        );
        let registry = IntegrationRegistry::with_plan(
            &settings,
            Arc::new(crate::auction::compile_auction_plan(&settings).expect("should compile plan")),
        )
        .expect("should create registry");
        let first = r#"<html><body><script>self.__next_f.push([1,"1:T3,ab"])</script>"#;
        let script = r#"self.__next_f.push([1,"c"])"#;
        let padding = "x".repeat(129);
        let expected = format!("{first}{padding}<script>{script}</script></body></html>");

        for split in 1..script.len() {
            let mut config = create_test_config();
            config.integrations = registry.clone();
            let mut processor = create_html_processor(config);
            let mut output = processor
                .process_chunk(first.as_bytes(), false)
                .expect("should process unresolved RSC group");
            let second = format!("{padding}<script>{}", &script[..split]);
            output.extend(
                processor
                    .process_chunk(second.as_bytes(), false)
                    .expect("should process output overflow and partial script"),
            );
            let third = format!("{}</script></body></html>", &script[split..]);
            output.extend(
                processor
                    .process_chunk(third.as_bytes(), true)
                    .expect("should finish bypassed script"),
            );
            assert_eq!(
                String::from_utf8(output).expect("should retain UTF-8"),
                expected,
                "should restore all original bytes when overflow occurs at script split {split}"
            );
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
            body_close: BodyCloseInjection::Marker(MARKER.to_string()),
            origin_host: "origin.example.com".to_string(),
            request_host: "example.com".to_string(),
            request_scheme: "https".to_string(),
            integrations: IntegrationRegistry::empty_for_tests(),
            ad_slots_script: None,
            ad_bids_state: std::sync::Arc::new(std::sync::Mutex::new(None)),
            max_buffered_body_bytes: 16 * 1024 * 1024,
            gpt_diagnostics: None,
            suppress_datadome_client_side_tag: false,
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
