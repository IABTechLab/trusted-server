use std::io::{self, Cursor};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use super::*;
use crate::html_processor::{
    HtmlProcessorConfig, ReaderDocument, create_html_processor,
    create_html_processor_with_middleware, create_serve_processor,
};
use crate::integrations::registry_test_support::request_fixture;
use crate::integrations::{IntegrationRegistry, IntegrationRequestState};
use crate::streaming_processor::{Compression, PipelineConfig, StreamingPipeline};
use crate::test_support::tests::create_test_settings;

const ORIGIN_HOST: &str = "origin.example.com";
const REQUEST_HOST: &str = "test.example.com";
const BUNDLE_TAG: &str = "id=\"trustedserver-js\"";

type CreateFn = dyn Fn(&MiddlewareContext<'_>) -> MiddlewareAction + Send + Sync;

/// A middleware made from a function, under a name of the test's choosing.
/// It says it runs in both phases, and a test runs it in the one its chain
/// names.
struct FromFn {
    id: &'static str,
    create: Box<CreateFn>,
}

impl Middleware for FromFn {
    fn middleware_id(&self) -> &'static str {
        self.id
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &MiddlewarePhase::ALL
    }

    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
        (self.create)(context)
    }
}

fn middleware(
    id: &'static str,
    create: impl Fn(&MiddlewareContext<'_>) -> MiddlewareAction + Send + Sync + 'static,
) -> Arc<dyn Middleware> {
    Arc::new(FromFn {
        id,
        create: Box::new(create),
    })
}

fn chain_of(middleware: Vec<Arc<dyn Middleware>>) -> MiddlewareChain {
    MiddlewareChain::new(MiddlewarePhase::Fetch, middleware)
}

fn serve_chain_of(middleware: Vec<Arc<dyn Middleware>>) -> MiddlewareChain {
    MiddlewareChain::new(MiddlewarePhase::Serve, middleware)
}

/// What a serve chain is told of a reader whose request left `request_state`.
fn reader(request_state: &IntegrationRequestState) -> ReaderDocument<'_> {
    ReaderDocument {
        request_host: REQUEST_HOST,
        request_scheme: "https",
        origin_host: ORIGIN_HOST,
        platform: test_support::platform(),
        request_state,
        max_buffered_script_bytes: 16 * 1024 * 1024,
    }
}

/// `html`, a document core has already rewritten, as a reader whose request
/// left `request_state` receives it after `chain` has run, read `chunk_size`
/// bytes at a time. A chain with nothing to do leaves it as it was.
fn served_with(
    chain: &MiddlewareChain,
    request_state: &IntegrationRequestState,
    html: &str,
    chunk_size: usize,
) -> String {
    let Some(mut processor) = create_serve_processor(chain, &reader(request_state))
        .expect("should plan the reader's middleware")
        .processor
    else {
        return html.to_owned();
    };
    let mut output = Vec::new();
    for chunk in html.as_bytes().chunks(chunk_size) {
        output.extend(
            processor
                .process_chunk(chunk, false)
                .expect("should process a piece of the reader's copy"),
        );
    }
    output.extend(
        processor
            .process_chunk(&[], true)
            .expect("should finish the reader's copy"),
    );
    String::from_utf8(output).expect("should leave the reader's copy UTF-8")
}

fn served(chain: &MiddlewareChain, html: &str) -> String {
    served_with(chain, &IntegrationRequestState::default(), html, 8192)
}

/// A middleware that writes one header on the reader's response.
fn header(id: &'static str, name: &'static str, value: &'static str) -> Arc<dyn Middleware> {
    middleware(id, move |_| MiddlewareAction {
        response_headers: vec![(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        )],
        ..MiddlewareAction::pass()
    })
}

/// A middleware that writes `markup` at the start of `<head>`.
fn head(id: &'static str, markup: &'static str) -> Arc<dyn Middleware> {
    middleware(id, move |_| MiddlewareAction {
        head_inserts: vec![markup.to_owned()],
        ..MiddlewareAction::pass()
    })
}

/// A middleware with one element handler.
fn element(
    id: &'static str,
    selector: &'static str,
    attribute: &'static str,
    decide: impl Fn(&MatchedAttribute<'_>) -> AttributeRewriteAction + Send + Sync + Clone + 'static,
) -> Arc<dyn Middleware> {
    middleware(id, move |_| MiddlewareAction {
        element_handlers: vec![Box::new(AttributeRewrite::matching(
            selector,
            attribute,
            Rc::new(decide.clone()),
        ))],
        ..MiddlewareAction::pass()
    })
}

struct TextFn<F> {
    selector: &'static str,
    decide: F,
}

impl<F> TextHandler for TextFn<F>
where
    F: FnMut(&str, bool) -> ScriptRewriteAction,
{
    fn selector(&self) -> &str {
        self.selector
    }

    fn decide(&mut self, text: &str, is_last: bool) -> ScriptRewriteAction {
        (self.decide)(text, is_last)
    }
}

/// A middleware with one text handler, made afresh for each document.
fn text<F>(
    id: &'static str,
    selector: &'static str,
    make: impl Fn() -> F + Send + Sync + 'static,
) -> Arc<dyn Middleware>
where
    F: FnMut(&str, bool) -> ScriptRewriteAction + 'static,
{
    middleware(id, move |_| MiddlewareAction {
        text_handlers: vec![Box::new(TextFn {
            selector,
            decide: make(),
        })],
        ..MiddlewareAction::pass()
    })
}

fn config() -> HtmlProcessorConfig {
    HtmlProcessorConfig::from_settings(
        &create_test_settings(),
        &IntegrationRegistry::default(),
        ORIGIN_HOST,
        REQUEST_HOST,
        "https",
    )
}

/// `html` as a reader receives it, read from the origin `chunk_size` bytes
/// at a time.
fn page_with(
    config: HtmlProcessorConfig,
    chain: &MiddlewareChain,
    html: &str,
    chunk_size: usize,
) -> String {
    let (processor, _) = create_html_processor_with_middleware(config, chain, None)
        .expect("should plan the document's middleware");
    let mut pipeline = StreamingPipeline::new(
        PipelineConfig {
            input_compression: Compression::None,
            output_compression: Compression::None,
            chunk_size,
        },
        processor,
    );
    let mut output = Vec::new();
    pipeline
        .process(Cursor::new(html.as_bytes()), &mut output)
        .expect("should process the document");
    String::from_utf8(output).expect("should leave the document UTF-8")
}

fn page(chain: &MiddlewareChain, html: &str) -> String {
    page_with(config(), chain, html, 8192)
}

fn index_of(page: &str, needle: &str) -> usize {
    page.find(needle)
        .unwrap_or_else(|| panic!("should find `{needle}` in: {page}"))
}

fn entry(path: Option<&str>, names: &[&str]) -> PhaseEntry {
    PhaseEntry {
        media_type: HTML_MEDIA_TYPE.to_owned(),
        path: path.map(str::to_owned),
        middleware: names.iter().map(|name| (*name).to_owned()).collect(),
    }
}

fn refusal(entries: Vec<PhaseEntry>) -> String {
    PhaseEntries::new(entries)
        .validate(MiddlewarePhase::Fetch)
        .expect_err("should refuse the entries")
}

// ---------------------------------------------------------------------------
// Entries
// ---------------------------------------------------------------------------

#[test]
fn a_response_takes_the_first_entry_that_covers_it() {
    let entries = PhaseEntries::new(vec![
        entry(Some("/news/"), &["example.news", "example.all"]),
        entry(None, &["example.all"]),
    ]);

    assert_eq!(
        entries.for_response(HTML_MEDIA_TYPE, "/news/today"),
        ["example.news", "example.all"],
        "should take the entry for its path, which is written first"
    );
    assert_eq!(
        entries.for_response(HTML_MEDIA_TYPE, "/sport/today"),
        ["example.all"],
        "should fall to the entry covering every path"
    );
}

#[test]
fn a_response_no_entry_covers_runs_nothing() {
    let entries = PhaseEntries::new(vec![entry(Some("/news/"), &["example.news"])]);

    assert!(
        entries
            .for_response(HTML_MEDIA_TYPE, "/sport/today")
            .is_empty(),
        "should run nothing on a path no entry covers"
    );
    assert!(
        entries
            .for_response("text/css", "/news/site.css")
            .is_empty(),
        "should run nothing on a media type no entry covers"
    );
    assert!(
        PhaseEntries::default()
            .for_response(HTML_MEDIA_TYPE, "/")
            .is_empty(),
        "should run nothing when there are no entries"
    );
}

#[test]
fn a_path_is_a_prefix_compared_as_written() {
    let entries = PhaseEntries::new(vec![entry(Some("/news"), &["example.news"])]);

    assert_eq!(
        entries.for_response(HTML_MEDIA_TYPE, "/newsletter"),
        ["example.news"],
        "should cover every path that begins with the prefix, which is why a folder is \
         written with its closing slash"
    );
    assert!(
        entries.for_response(HTML_MEDIA_TYPE, "/News").is_empty(),
        "should compare the path as written, upper and lower case apart"
    );
}

#[test]
fn names_lists_each_middleware_once_in_the_order_first_written() {
    let entries = PhaseEntries::new(vec![
        entry(Some("/news/"), &["example.b", "example.a"]),
        entry(None, &["example.a", "example.c"]),
    ]);

    assert_eq!(entries.names(), ["example.b", "example.a", "example.c"]);
}

#[test]
fn entries_from_the_longest_path_to_every_path_are_accepted() {
    PhaseEntries::new(vec![
        entry(Some("/news/sport/"), &["example.a"]),
        entry(Some("/news/"), &["example.b"]),
        entry(Some("/weather/"), &["example.c"]),
        entry(None, &["example.d"]),
    ])
    .validate(MiddlewarePhase::Fetch)
    .expect("should accept entries no earlier entry hides");
}

#[test]
fn an_entry_for_a_media_type_other_than_html_is_refused() {
    for media_type in ["text/css", "text/html; charset=utf-8", "TEXT/HTML", "*/*"] {
        let mut css = entry(None, &["example.a"]);
        css.media_type = media_type.to_owned();

        let message = refusal(vec![css]);

        assert!(
            message.contains("[[fetch]] entry 1")
                && message.contains(&format!("covers `{media_type}`"))
                && message.contains("media_type = \"text/html\""),
            "should name the entry and the media type to write: {message}"
        );
    }
}

#[test]
fn an_entry_whose_path_could_cover_nothing_is_refused() {
    let message = refusal(vec![entry(Some("news/"), &["example.a"])]);
    assert!(
        message.contains("[[fetch]] entry 1") && message.contains("must start with `/`"),
        "should refuse a path that is not a request path: {message}"
    );

    for path in ["/news/*", "/news/?page=2", "/news/#top"] {
        let message = refusal(vec![entry(Some(path), &["example.a"])]);
        assert!(
            message.contains(&format!("path `{path}`")) && message.contains("compared as written"),
            "should refuse a pattern, a query and a fragment, none of which a request path \
             holds: {message}"
        );
    }
}

#[test]
fn an_entry_that_names_nothing_is_refused() {
    let message = refusal(vec![entry(None, &[])]);

    assert!(
        message.contains("[[fetch]] entry 1 names no middleware"),
        "should refuse an entry that would run nothing: {message}"
    );
}

#[test]
fn an_entry_naming_something_that_is_no_name_is_refused() {
    let message = refusal(vec![entry(None, &["example.a", "Not A Name"])]);
    assert!(
        message.contains("names `Not A Name`, which is not a middleware name"),
        "should refuse a name no middleware could have: {message}"
    );

    let message = refusal(vec![entry(None, &["example.a", "example.b", "example.a"])]);
    assert!(
        message.contains("names `example.a` more than once"),
        "should refuse a middleware named twice in one entry: {message}"
    );
}

#[test]
fn an_entry_hidden_behind_an_earlier_one_is_refused() {
    let cases = [
        // A longer path after the shorter one that covers it.
        vec![
            entry(Some("/news/"), &["example.a"]),
            entry(Some("/news/sport/"), &["example.b"]),
        ],
        // A path after the entry covering every path.
        vec![
            entry(None, &["example.a"]),
            entry(Some("/news/"), &["example.b"]),
        ],
    ];

    for entries in cases {
        let message = refusal(entries);
        assert!(
            message.contains("[[fetch]] entry 2 is never reached")
                && message.contains("[[fetch]] entry 1 covers every response it covers")
                && message.contains("Put the entry with the longer path first"),
            "should name the hidden entry and the one hiding it: {message}"
        );
    }
}

#[test]
fn an_entry_for_the_same_pages_as_an_earlier_one_is_told_to_share_its_list() {
    // What an operator writes who gives each module an entry of its own.
    let cases = [
        vec![
            entry(Some("/news/"), &["example.a"]),
            entry(Some("/news/"), &["example.b"]),
        ],
        vec![entry(None, &["example.a"]), entry(None, &["example.b"])],
    ];

    for entries in cases {
        let message = refusal(entries);
        assert!(
            message.contains("[[fetch]] entry 2 covers the same pages as [[fetch]] entry 1")
                && message.contains("Name every middleware for those pages in one entry's list"),
            "should say the two lists belong in one entry: {message}"
        );
    }
}

#[test]
fn a_path_of_slash_covers_the_same_pages_as_no_path() {
    // The path of a request for a page begins with `/`, so whichever of the
    // two entries comes second is never reached.
    let cases = [
        vec![
            entry(Some("/"), &["example.a"]),
            entry(None, &["example.b"]),
        ],
        vec![
            entry(None, &["example.a"]),
            entry(Some("/"), &["example.b"]),
        ],
    ];

    for entries in cases {
        let message = refusal(entries);
        assert!(
            message.contains("[[fetch]] entry 2 covers the same pages as [[fetch]] entry 1")
                && message.contains("Name every middleware for those pages in one entry's list"),
            "should refuse the entry that is never reached: {message}"
        );
    }
}

#[test]
fn entries_are_read_from_a_list_and_written_back_as_they_were() {
    let written = json!([
        { "media_type": "text/html", "path": "/news/", "middleware": ["example.a", "example.b"] },
        { "media_type": "text/html", "middleware": ["example.a"] },
    ]);

    let entries: PhaseEntries =
        serde_json::from_value(written.clone()).expect("should read the entries");

    assert_eq!(
        entries,
        PhaseEntries::new(vec![
            entry(Some("/news/"), &["example.a", "example.b"]),
            entry(None, &["example.a"]),
        ])
    );
    assert_eq!(
        serde_json::to_value(&entries).expect("should write the entries"),
        written,
        "should write an entry with no path without one"
    );
}

#[test]
fn an_entry_with_a_key_it_does_not_read_is_refused() {
    let error = serde_json::from_value::<PhaseEntries>(json!([
        { "media_type": "text/html", "middleware": ["example.a"], "settings": {} },
    ]))
    .expect_err("should refuse a key an entry does not read");

    assert!(
        error.to_string().contains("unknown field `settings`"),
        "should name the key: {error}"
    );
}

#[test]
fn a_phase_written_as_a_table_is_told_it_is_a_list() {
    let error = serde_json::from_value::<PhaseEntries>(json!({
        "media_type": "text/html",
        "middleware": ["example.a"],
    }))
    .expect_err("should refuse a phase that is not a list");

    assert!(
        error
            .to_string()
            .contains("A phase is a list of entries, each written as [[fetch]]"),
        "should say how a phase is written: {error}"
    );
}

// ---------------------------------------------------------------------------
// What the document pipeline does with a chain
// ---------------------------------------------------------------------------

const PAGE: &str = "<html><head><title>Page</title></head><body></body></html>";

#[test]
fn a_chain_that_names_nothing_leaves_the_page_as_it_would_be_without_one() {
    let html = r#"<html><head><title>Page</title></head><body>
        <a href="https://origin.example.com/one">one</a>
        <script>var a = 1 && 2;</script>
    </body></html>"#;

    let mut plain = create_html_processor(config());
    let without = String::from_utf8(
        crate::streaming_processor::StreamProcessor::process_chunk(
            &mut plain,
            html.as_bytes(),
            true,
        )
        .expect("should process the document"),
    )
    .expect("should leave the document UTF-8");

    assert_eq!(page(&chain_of(Vec::new()), html), without);
}

#[test]
fn head_markup_goes_ahead_of_the_bundle_in_the_order_the_middleware_are_named() {
    let chain = chain_of(vec![
        head("example.first", "<meta name=\"first\">"),
        head("example.second", "<meta name=\"second\">"),
    ]);

    let page = page(&chain, PAGE);

    let first = index_of(&page, "name=\"first\"");
    let second = index_of(&page, "name=\"second\"");
    let bundle = index_of(&page, BUNDLE_TAG);
    let title = index_of(&page, "<title>");
    assert!(
        first < second && second < bundle && bundle < title,
        "should write head markup in the order named, ahead of the bundle and of the \
         page's own head: {page}"
    );
    assert_eq!(
        page.matches("name=\"first\"").count(),
        1,
        "should write head markup once"
    );
}

#[test]
fn markup_for_after_the_bundle_goes_between_the_bundle_and_the_page_s_own_head() {
    let chain = chain_of(vec![middleware("example.after", |_| MiddlewareAction {
        after_bundle_inserts: vec!["<script id=\"after\"></script>".to_owned()],
        ..MiddlewareAction::pass()
    })]);

    let page = page(&chain, PAGE);

    let bundle = index_of(&page, BUNDLE_TAG);
    let after = index_of(&page, "id=\"after\"");
    let title = index_of(&page, "<title>");
    assert!(
        bundle < after && after < title,
        "should write the markup straight after the bundle: {page}"
    );
}

#[test]
fn a_page_with_no_head_gets_no_head_markup() {
    let chain = chain_of(vec![head("example.first", "<meta name=\"first\">")]);

    let page = page(&chain, "<p>a fragment</p>");

    assert_eq!(
        page, "<p>a fragment</p>",
        "should leave a document with no head as it arrived"
    );
}

#[test]
fn an_element_handler_is_asked_about_an_address_as_it_will_be_served() {
    let chain = chain_of(vec![element("example.seen", "a.x", "href", |matched| {
        AttributeRewriteAction::replace(format!(
            "seen:{}:{}:{}",
            matched.element_name, matched.attribute_name, matched.value
        ))
    })]);

    let page = page(
        &chain,
        r#"<html><body><a class="x" href="https://origin.example.com/page">x</a><a href="https://origin.example.com/other">y</a></body></html>"#,
    );

    assert!(
        page.contains(r#"<a class="x" href="seen:a:href:https://test.example.com/page">"#),
        "should hand the handler the address after core moved it to the reader's host: {page}"
    );
    assert!(
        page.contains(r#"<a href="https://test.example.com/other">"#),
        "should leave an element the selector does not match to core: {page}"
    );
}

#[test]
fn one_decision_can_be_asked_about_each_of_several_attributes() {
    let chain = chain_of(vec![middleware("example.each", |_| {
        let decide: Rc<AttributeRewriteFn> = Rc::new(|matched| {
            if matched.value == "/vendor.js" {
                AttributeRewriteAction::replace(format!(
                    "/first-party.js#{}",
                    matched.attribute_name
                ))
            } else {
                AttributeRewriteAction::keep()
            }
        });
        MiddlewareAction {
            element_handlers: AttributeRewrite::each(&["src", "href"], &decide),
            ..MiddlewareAction::pass()
        }
    })]);

    let page = page(
        &chain,
        r#"<html><body><script src="/vendor.js"></script><link href="/vendor.js"><img src="/other.png"><a data-src="/vendor.js">x</a></body></html>"#,
    );

    assert!(
        page.contains(r#"<script src="/first-party.js#src"></script>"#)
            && page.contains(r#"<link href="/first-party.js#href">"#),
        "should ask the one decision about each attribute named: {page}"
    );
    assert!(
        page.contains(r#"<img src="/other.png">"#) && page.contains(r#"<a data-src="/vendor.js">"#),
        "should leave another value and another attribute alone: {page}"
    );
}

#[test]
fn a_module_s_tests_can_call_a_middleware_with_a_test_page_s_context() {
    use crate::html_processor::test_support::{ORIGIN_HOST, REQUEST_HOST};

    let document_state = crate::integrations::IntegrationDocumentState::default();
    let context = test_support::context(MiddlewarePhase::Serve, &document_state);

    assert_eq!(context.phase, MiddlewarePhase::Serve);
    assert_eq!(
        (
            context.request_scheme,
            context.request_host,
            context.origin_host
        ),
        ("https", REQUEST_HOST, ORIGIN_HOST),
        "should describe the page the recording helpers serve"
    );
}

#[test]
fn an_element_handler_can_remove_the_element() {
    let chain = chain_of(vec![element("example.remove", "a.gone", "href", |_| {
        AttributeRewriteAction::remove_element()
    })]);

    let page = page(
        &chain,
        r#"<html><body><a class="gone" href="/x">removed</a><a class="gone">no address</a><a href="/y">kept</a></body></html>"#,
    );

    assert!(
        !page.contains("removed"),
        "should remove the element and what it holds: {page}"
    );
    assert!(
        page.contains(r#"<a class="gone">no address</a>"#),
        "should leave an element that carries no such attribute as it is: {page}"
    );
    assert!(page.contains(r#"<a href="/y">kept</a>"#));
}

#[test]
fn handlers_on_one_element_are_asked_in_order_each_seeing_the_last_value() {
    let chain = chain_of(vec![
        element("example.one", "a", "href", |_| {
            AttributeRewriteAction::replace("/one")
        }),
        element("example.two", "a[href]", "href", |matched| {
            AttributeRewriteAction::replace(format!("{}/two", matched.value))
        }),
    ]);

    let page = page(
        &chain,
        r#"<html><body><a href="/start">x</a></body></html>"#,
    );

    assert!(
        page.contains(r#"<a href="/one/two">"#),
        "should hand the second handler the value the first left: {page}"
    );
}

#[test]
fn an_element_an_earlier_handler_removed_is_not_asked_about_again() {
    let asked = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    let chain = chain_of(vec![
        element("example.remove", "a", "href", |_| {
            AttributeRewriteAction::remove_element()
        }),
        element("example.count", "a", "href", move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            AttributeRewriteAction::keep()
        }),
    ]);

    let page = page(&chain, r#"<html><body><a href="/x">x</a></body></html>"#);

    assert!(!page.contains("<a "), "should remove the element: {page}");
    assert_eq!(
        asked.load(Ordering::SeqCst),
        0,
        "should not ask a later handler about an element that is already gone"
    );
}

#[test]
fn a_text_handler_is_handed_the_text_as_the_one_before_it_left_it() {
    let chain = chain_of(vec![
        text("example.one", "script.x", || {
            |text: &str, _is_last: bool| {
                if text.contains("alpha") {
                    ScriptRewriteAction::replace(text.replace("alpha", "beta"))
                } else {
                    ScriptRewriteAction::keep()
                }
            }
        }),
        text("example.two", "script", || {
            |text: &str, _is_last: bool| {
                if text.contains("beta") {
                    ScriptRewriteAction::replace(text.replace("beta", "gamma"))
                } else {
                    ScriptRewriteAction::keep()
                }
            }
        }),
    ]);

    let page = page(
        &chain,
        r#"<html><body><script class="x">var a = "alpha";</script><script>var b = "alpha";</script></body></html>"#,
    );

    assert!(
        page.contains(r#"<script class="x">var a = "gamma";</script>"#),
        "should hand the second handler what the first wrote: {page}"
    );
    assert!(
        page.contains(r#"<script>var b = "alpha";</script>"#),
        "should leave a script only the second matches, which holds nothing it changes: {page}"
    );
}

#[test]
fn text_a_handler_removed_is_empty_for_the_next_which_may_write_in_its_place() {
    let chain = chain_of(vec![
        text("example.remove", "script", || {
            |_text: &str, _is_last: bool| ScriptRewriteAction::remove_node()
        }),
        text("example.write", "script", || {
            |text: &str, is_last: bool| {
                assert_eq!(text, "", "should hand over nothing where text was removed");
                if is_last {
                    ScriptRewriteAction::replace("written()")
                } else {
                    ScriptRewriteAction::keep()
                }
            }
        }),
    ]);

    let page = page(
        &chain,
        "<html><body><script>removed()</script></body></html>",
    );

    assert!(
        page.contains("<script>written()</script>"),
        "should write the later handler's text in place of what was removed: {page}"
    );
}

#[test]
fn a_text_handler_s_replacement_is_written_as_it_is() {
    let chain = chain_of(vec![text("example.script", "script", || {
        |text: &str, _is_last: bool| {
            if text.is_empty() {
                ScriptRewriteAction::keep()
            } else {
                ScriptRewriteAction::replace("if (a && b < c || d > e) { go(\"?x=1&y=2\"); }")
            }
        }
    })]);

    let page = page(&chain, "<html><body><script>old()</script></body></html>");

    assert!(
        page.contains("<script>if (a && b < c || d > e) { go(\"?x=1&y=2\"); }</script>"),
        "should write a script's text with no character escaped: {page}"
    );
}

#[test]
fn a_handler_that_holds_a_script_writes_it_back_whole() {
    // The script reaches the handler in pieces, because the document is read
    // seven bytes at a time, and the word to change lies across pieces.
    let chain = chain_of(vec![text("example.hold", "script", || {
        let mut held = String::new();
        move |text: &str, is_last: bool| {
            held.push_str(text);
            if !is_last {
                return ScriptRewriteAction::remove_node();
            }
            ScriptRewriteAction::replace(std::mem::take(&mut held).replace("ORIGINAL", "CHANGED"))
        }
    })]);

    let page = page_with(
        config(),
        &chain,
        "<html><body><script>one(); two(\"ORIGINAL\"); three();</script><p>after</p></body></html>",
        7,
    );

    assert!(
        page.contains("<script>one(); two(\"CHANGED\"); three();</script><p>after</p>"),
        "should write the whole script once, in order, with the word changed: {page}"
    );
}

const SCRIPT_PAGE: &str = "<html><body><script>one(); two();</script><p>after</p></body></html>";

/// A middleware that writes `script` as the text of every script.
fn writes_script(script: &str) -> Arc<dyn Middleware> {
    let script = script.to_owned();
    text("example.script", "script", move || {
        let script = script.clone();
        move |_text: &str, is_last: bool| {
            if is_last {
                ScriptRewriteAction::replace(script.clone())
            } else {
                ScriptRewriteAction::remove_node()
            }
        }
    })
}

/// The text of each script in `page`, and the name of each element, as a
/// browser's tokenizer reads the page.
fn as_a_browser_reads(page: &str) -> (Vec<String>, Vec<String>) {
    let scripts = std::cell::RefCell::new((String::new(), Vec::new()));
    let elements = std::cell::RefCell::new(Vec::new());
    lol_html::rewrite_str(
        page,
        lol_html::RewriteStrSettings {
            element_content_handlers: vec![
                lol_html::element!("*", |element| {
                    elements.borrow_mut().push(element.tag_name());
                    Ok(())
                }),
                lol_html::text!("script", |text| {
                    let mut scripts = scripts.borrow_mut();
                    let (script, read) = &mut *scripts;
                    script.push_str(text.as_str());
                    if text.last_in_text_node() {
                        read.push(std::mem::take(script));
                    }
                    Ok(())
                }),
            ],
            ..lol_html::RewriteStrSettings::new()
        },
    )
    .expect("should read the page");
    (scripts.into_inner().1, elements.into_inner())
}

/// Asserts that a browser reads `page` as one script holding `script`,
/// followed by the paragraph, and no other element.
fn assert_script_stays_whole(page: &str, script: &str) {
    let (scripts, elements) = as_a_browser_reads(page);
    assert_eq!(
        scripts,
        [script],
        "should read the script's text as written, up to its own end tag: {page}"
    );
    assert_eq!(
        elements,
        ["html", "body", "script", "p"],
        "should read what follows the script as markup, and nothing in it: {page}"
    );
}

#[test]
fn script_text_that_would_end_the_script_is_written_so_the_script_stays_whole() {
    for (written, expected) in [
        (
            "var s = \"</script><img src=x onerror=alert(1)>\";",
            "var s = \"\\u003c/script><img src=x onerror=alert(1)>\";",
        ),
        (
            "var s = \"</SCRIPT ><img src=x onerror=alert(1)>\";",
            "var s = \"\\u003c/SCRIPT ><img src=x onerror=alert(1)>\";",
        ),
    ] {
        let page = page(&chain_of(vec![writes_script(written)]), SCRIPT_PAGE);

        assert!(
            page.contains(&format!("<script>{expected}</script><p>after</p>")),
            "should write the `<` of the closing tag as `\\u003c`: {page}"
        );
        assert_script_stays_whole(&page, expected);
    }
}

#[test]
fn script_text_with_a_comment_opening_leaves_the_end_tag_closing_the_script() {
    // After `<!--`, `<script` would keep the script's own end tag from
    // closing it, and the rest of the page would be read as script.
    for (written, expected) in [
        ("var s = \"<!--\";", "var s = \"<!--\";"),
        (
            "var s = \"<!--<script>\";",
            "var s = \"<!--\\u003cscript>\";",
        ),
    ] {
        let page = page(&chain_of(vec![writes_script(written)]), SCRIPT_PAGE);

        assert!(
            page.contains(&format!("<script>{expected}</script><p>after</p>")),
            "should write `<!--` as it is, and the `<` of a script tag after it as \
             `\\u003c`: {page}"
        );
        assert_script_stays_whole(&page, expected);
    }
}

#[test]
fn script_text_written_in_pieces_cannot_end_the_script_between_them() {
    // The page is read a byte at a time, so the script's text arrives in
    // several chunks. The handler writes the `<` of a closing tag in place
    // of the first chunk and the rest of the tag in place of the last.
    let chain = chain_of(vec![text("example.pieces", "script", || {
        let mut first = true;
        move |_text: &str, is_last: bool| {
            if is_last {
                ScriptRewriteAction::replace("/script><img src=x onerror=alert(1)>")
            } else if std::mem::take(&mut first) {
                ScriptRewriteAction::replace("<")
            } else {
                ScriptRewriteAction::remove_node()
            }
        }
    })]);

    let page = page_with(config(), &chain, SCRIPT_PAGE, 1);

    let expected = "\\u003c/script><img src=x onerror=alert(1)>";
    assert!(
        page.contains(&format!("<script>{expected}</script><p>after</p>")),
        "should escape a closing tag whose `<` was written with an earlier chunk: {page}"
    );
    assert_script_stays_whole(&page, expected);
}

#[test]
fn script_text_a_serve_middleware_writes_cannot_end_the_script_either() {
    let chain = serve_chain_of(vec![writes_script(
        "var s = \"</script><img src=x onerror=alert(1)>\";",
    )]);

    let page = served(&chain, SCRIPT_PAGE);

    let expected = "var s = \"\\u003c/script><img src=x onerror=alert(1)>\";";
    assert!(
        page.contains(&format!("<script>{expected}</script><p>after</p>")),
        "should keep the text a serve middleware writes inside its script: {page}"
    );
    assert_script_stays_whole(&page, expected);
}

#[test]
fn a_script_no_middleware_changes_is_written_as_the_page_wrote_it() {
    // An old page hides a script inside `<!--` and writes a script tag from
    // within it, which the parser reads as the script's own text.
    let html = "<html><body><script><!--\ndocument.write('<script src=\"a.js\"></script>');\n\
                //--></script><p>after</p></body></html>";
    let chain = chain_of(vec![text("example.keep", "script", || {
        |_text: &str, _is_last: bool| ScriptRewriteAction::keep()
    })]);

    for chunk_size in [1, 7, 8192] {
        assert_eq!(
            page_with(config(), &chain, html, chunk_size),
            html,
            "should leave a script no middleware changed byte for byte, read {chunk_size} \
             bytes at a time"
        );
    }
}

/// Appends a comment to the document, and swaps one word for another.
struct Rewording {
    from: &'static str,
    to: &'static str,
    comment: &'static str,
}

impl crate::streaming_processor::StreamProcessor for Rewording {
    fn process_chunk(&mut self, chunk: &[u8], is_last: bool) -> io::Result<Vec<u8>> {
        let mut text = String::from_utf8_lossy(chunk).replace(self.from, self.to);
        if is_last {
            text.push_str(self.comment);
        }
        Ok(text.into_bytes())
    }
}

#[test]
fn a_stream_processor_is_handed_the_document_the_handlers_left() {
    let chain = chain_of(vec![
        middleware("example.first", |_| MiddlewareAction {
            element_handlers: vec![Box::new(AttributeRewrite::matching(
                "a",
                "href",
                Rc::new(|_| AttributeRewriteAction::replace("/from-handler")),
            ))],
            stream: Some(Box::new(Rewording {
                from: "/from-handler",
                to: "/from-first",
                comment: "<!--first-->",
            })),
            ..MiddlewareAction::pass()
        }),
        middleware("example.second", |_| MiddlewareAction {
            stream: Some(Box::new(Rewording {
                from: "/from-first",
                to: "/from-second",
                comment: "<!--second-->",
            })),
            ..MiddlewareAction::pass()
        }),
    ]);

    let page = page(
        &chain,
        r#"<html><body><a href="/start">x</a></body></html>"#,
    );

    assert!(
        page.contains(r#"<a href="/from-second">"#),
        "should hand each processor what the handlers and the processor before it left: {page}"
    );
    assert!(
        page.ends_with("<!--first--><!--second-->"),
        "should run the processors in the order their middleware are named: {page}"
    );
}

/// What one middleware leaves in the document's state for another.
struct Shared(&'static str);

#[test]
fn the_middleware_of_one_document_share_its_state_and_see_nothing_a_request_left() {
    const KEY: &str = "example";
    let chain = chain_of(vec![
        middleware("example.leaves", |context| {
            context
                .document_state
                .get_or_insert_with(KEY, || Shared("left by the first"));
            MiddlewareAction::pass()
        }),
        middleware("example.reads", |context| {
            let shared = context
                .document_state
                .get::<Shared>(KEY)
                .map_or("nothing", |shared| shared.0);
            let request_mark = context
                .document_state
                .get::<request_fixture::Mark>(request_fixture::ID)
                .is_some();
            MiddlewareAction {
                head_inserts: vec![format!(
                    "<meta name=\"state\" content=\"{shared}; request mark {request_mark}\">"
                )],
                ..MiddlewareAction::pass()
            }
        }),
    ]);
    // The request left a mark, which a serve middleware may read.
    let config = config().with_request_state(request_fixture::marked());

    let page = page_with(config, &chain, PAGE, 8192);

    assert!(
        page.contains("content=\"left by the first; request mark false\""),
        "should share the document's state along the chain, with nothing of the request in \
         it: {page}"
    );
}

#[test]
fn each_document_is_planned_afresh() {
    let created = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&created);
    let chain = chain_of(vec![middleware("example.counts", move |context| {
        let nth = counted.fetch_add(1, Ordering::SeqCst) + 1;
        let earlier = context.document_state.get::<Shared>("example").is_some();
        context
            .document_state
            .get_or_insert_with("example", || Shared("this document"));
        MiddlewareAction {
            head_inserts: vec![format!(
                "<meta name=\"nth\" content=\"{nth}; earlier state {earlier}\">"
            )],
            ..MiddlewareAction::pass()
        }
    })]);

    let first = page(&chain, PAGE);
    let second = page(&chain, PAGE);

    assert!(first.contains("content=\"1; earlier state false\""));
    assert!(
        second.contains("content=\"2; earlier state false\""),
        "should ask the middleware again for the next document, with a state of its own: \
         {second}"
    );
    assert_eq!(created.load(Ordering::SeqCst), 2);
}

#[test]
fn a_middleware_is_told_the_phase_the_hosts_and_how_much_it_may_hold() {
    let chain = chain_of(vec![middleware("example.context", |context| {
        MiddlewareAction {
            head_inserts: vec![format!(
                "<meta name=\"context\" content=\"{} {}://{} from {} holding {}\">",
                context.phase,
                context.request_scheme,
                context.request_host,
                context.origin_host,
                context.max_buffered_script_bytes
            )],
            ..MiddlewareAction::pass()
        }
    })]);
    let most = create_test_settings().publisher.max_buffered_body_bytes;

    let page = page(&chain, PAGE);

    assert!(
        page.contains(&format!(
            "content=\"fetch https://{REQUEST_HOST} from {ORIGIN_HOST} holding {most}\""
        )),
        "should tell the middleware about the document: {page}"
    );
}

#[test]
fn a_selector_that_does_not_parse_refuses_the_document_naming_the_middleware() {
    let chains = [
        chain_of(vec![
            head("example.fine", "<meta name=\"fine\">"),
            element("example.broken", "a[", "href", |_| {
                AttributeRewriteAction::keep()
            }),
        ]),
        chain_of(vec![text("example.broken", "a[", || {
            |_text: &str, _is_last: bool| ScriptRewriteAction::keep()
        })]),
    ];

    for chain in chains {
        let message = create_html_processor_with_middleware(config(), &chain, None)
            .err()
            .expect("should refuse to plan the document");

        assert!(
            message.contains("the fetch middleware `example.broken` cannot run")
                && message.contains("`a[` is not a usable selector"),
            "should name the middleware and the selector: {message}"
        );
    }
}

#[test]
fn a_fetch_middleware_that_writes_a_response_header_is_refused() {
    let chain = chain_of(vec![header("example.version", "x-example", "1")]);

    let message = create_html_processor_with_middleware(config(), &chain, None)
        .err()
        .expect("should refuse to plan the document");

    assert!(
        message.contains("the fetch middleware `example.version` cannot run")
            && message.contains("writes a response header"),
        "should name the middleware and the reason: {message}"
    );
}

#[test]
fn a_serve_middleware_s_headers_are_written_on_the_reader_s_response() {
    let chain = serve_chain_of(vec![
        header("example.a", "x-example", "a"),
        header("example.b", "x-example", "b"),
        header("example.c", "x-other", "c"),
    ]);

    let pass = create_serve_processor(&chain, &reader(&IntegrationRequestState::default()))
        .expect("should plan the reader's middleware");
    let mut response = http::Response::new(());
    write_response_headers(&mut response, pass.response_headers);

    assert!(
        pass.processor.is_none(),
        "should leave the document as it arrived"
    );
    assert_eq!(
        response
            .headers()
            .get("x-example")
            .map(HeaderValue::as_bytes),
        Some(b"b".as_slice()),
        "a later middleware's header should replace an earlier one's"
    );
    assert_eq!(
        response.headers().get("x-other").map(HeaderValue::as_bytes),
        Some(b"c".as_slice()),
        "should write a header no other middleware wrote"
    );
}

#[test]
fn the_inline_path_hands_back_the_serve_chain_s_headers() {
    let serve = serve_chain_of(vec![header("example.version", "x-example", "1")]);

    let (_, headers) =
        create_html_processor_with_middleware(config(), &chain_of(Vec::new()), Some(&serve))
            .expect("should plan the document");

    assert_eq!(
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_bytes()))
            .collect::<Vec<_>>(),
        [("x-example", b"1".as_slice())],
        "should hand back the serve chain's headers for the response"
    );
}

#[test]
fn an_action_with_only_a_header_is_not_a_pass() {
    let action = MiddlewareAction {
        response_headers: vec![(
            HeaderName::from_static("x-example"),
            HeaderValue::from_static("1"),
        )],
        ..MiddlewareAction::pass()
    };

    assert!(!action.is_pass(), "a header is a change to the response");
    assert!(MiddlewareAction::pass().is_pass());
}

#[test]
fn a_middleware_is_told_the_platform_the_deployment_runs_on() {
    let platform = PlatformIdentity {
        service_version: "39".to_owned(),
        ..PlatformIdentity::named("example-platform")
    };
    let document_state = IntegrationDocumentState::default();

    let context = test_support::context_on(MiddlewarePhase::Serve, &document_state, &platform);

    assert_eq!(context.platform.name, "example-platform");
    assert_eq!(context.platform.service_version, "39");
    assert_eq!(
        test_support::context(MiddlewarePhase::Serve, &document_state)
            .platform
            .name,
        "test-platform",
        "a test context should name the test platform"
    );
}

#[test]
fn a_chain_reports_its_phase_and_the_names_in_the_order_they_run() {
    let chain = chain_of(vec![
        head("example.b", "<meta name=\"b\">"),
        head("example.a", "<meta name=\"a\">"),
    ]);

    assert_eq!(chain.phase(), MiddlewarePhase::Fetch);
    assert_eq!(chain.ids(), ["example.b", "example.a"]);
    assert!(!chain.is_empty());
    assert!(chain_of(Vec::new()).is_empty());
}

// ---------------------------------------------------------------------------
// What the document pipeline does with a serve chain
// ---------------------------------------------------------------------------

/// [`PAGE`] as core leaves it for a store, with the script bundle in its
/// head and one link.
fn rewritten_page() -> String {
    page(
        &chain_of(Vec::new()),
        r#"<html><head><title>Page</title></head><body><a href="https://origin.example.com/x">x</a><script>var a = 1;</script></body></html>"#,
    )
}

#[test]
fn a_serve_chain_writes_head_markup_straight_before_the_bundle_and_the_rest_straight_after() {
    let chain = serve_chain_of(vec![
        middleware("example.first", |_| MiddlewareAction {
            head_inserts: vec!["<meta name=\"first\">".to_owned()],
            after_bundle_inserts: vec!["<script id=\"after-first\"></script>".to_owned()],
            ..MiddlewareAction::pass()
        }),
        middleware("example.second", |_| MiddlewareAction {
            head_inserts: vec!["<meta name=\"second\">".to_owned()],
            after_bundle_inserts: vec!["<script id=\"after-second\"></script>".to_owned()],
            ..MiddlewareAction::pass()
        }),
    ]);
    let rewritten = rewritten_page();

    let page = served(&chain, &rewritten);

    assert!(
        page.contains("<meta name=\"first\"><meta name=\"second\"><script src="),
        "should write head markup in the order named, straight before the bundle: {page}"
    );
    assert!(
        page.contains(
            "id=\"trustedserver-js\"></script><script id=\"after-first\"></script><script \
             id=\"after-second\"></script>"
        ),
        "should write the markup for after the bundle straight after it: {page}"
    );
    assert_eq!(
        page.matches(BUNDLE_TAG).count(),
        1,
        "should write no second bundle: {page}"
    );
    assert_eq!(
        page.replace("<meta name=\"first\"><meta name=\"second\">", "")
            .replace(
                "<script id=\"after-first\"></script><script id=\"after-second\"></script>",
                ""
            ),
        rewritten,
        "should change nothing else of a document core has already rewritten"
    );
}

#[test]
fn a_serve_chain_writes_head_markup_once_where_the_page_repeats_the_bundle_s_id() {
    let chain = serve_chain_of(vec![head("example.first", "<meta name=\"first\">")]);
    let rewritten = rewritten_page().replace(
        "</body>",
        "<script id=\"trustedserver-js\">var own = 1;</script></body>",
    );

    let page = served(&chain, &rewritten);

    assert_eq!(
        page.matches("name=\"first\"").count(),
        1,
        "should write head markup at core's tag alone: {page}"
    );
    assert!(
        index_of(&page, "name=\"first\"") < index_of(&page, "<title>"),
        "should write it in the head: {page}"
    );
}

#[test]
fn a_reader_s_copy_with_no_bundle_gets_no_head_markup() {
    let chain = serve_chain_of(vec![head("example.first", "<meta name=\"first\">")]);

    let page = served(&chain, "<p>a fragment</p>");

    assert_eq!(page, "<p>a fragment</p>");
}

#[test]
fn a_serve_chain_s_handlers_work_on_the_page_as_core_left_it() {
    let chain = serve_chain_of(vec![
        element("example.link", "a", "href", |matched| {
            AttributeRewriteAction::replace(format!("{}?reader=1", matched.value))
        }),
        text("example.script", "script", || {
            |text: &str, _is_last: bool| {
                if text.contains("var a = 1;") {
                    ScriptRewriteAction::replace(text.replace("var a = 1;", "var a = 2 && 3;"))
                } else {
                    ScriptRewriteAction::keep()
                }
            }
        }),
    ]);

    let page = served(&chain, &rewritten_page());

    assert!(
        page.contains(r#"<a href="https://test.example.com/x?reader=1">"#),
        "should hand an element handler the address core already moved: {page}"
    );
    assert!(
        page.contains("<script>var a = 2 && 3;</script>"),
        "should write a text handler's replacement as it is: {page}"
    );
}

#[test]
fn a_serve_chain_s_stream_processor_runs_over_the_reader_s_copy() {
    let chain = serve_chain_of(vec![middleware("example.stream", |_| MiddlewareAction {
        stream: Some(Box::new(Rewording {
            from: "<title>Page</title>",
            to: "<title>Reader</title>",
            comment: "<!--reader-->",
        })),
        ..MiddlewareAction::pass()
    })]);

    let page = served(&chain, &rewritten_page());

    assert!(page.contains("<title>Reader</title>") && page.ends_with("<!--reader-->"));
}

#[test]
fn a_serve_middleware_reads_what_the_request_left() {
    let chain = serve_chain_of(vec![middleware("example.reads", |context| {
        let marked = context
            .document_state
            .get::<request_fixture::Mark>(request_fixture::ID)
            .is_some();
        MiddlewareAction {
            head_inserts: vec![format!(
                "<meta name=\"reader\" content=\"{} {}://{} from {}; marked {marked}\">",
                context.phase, context.request_scheme, context.request_host, context.origin_host
            )],
            ..MiddlewareAction::pass()
        }
    })]);
    let rewritten = rewritten_page();

    let marked = served_with(&chain, &request_fixture::marked(), &rewritten, 8192);
    let unmarked = served(&chain, &rewritten);

    let told = format!("serve https://{REQUEST_HOST} from {ORIGIN_HOST}");
    assert!(
        marked.contains(&format!("content=\"{told}; marked true\"")),
        "should hand a serve middleware what the request left: {marked}"
    );
    assert!(
        unmarked.contains(&format!("content=\"{told}; marked false\"")),
        "should hand it nothing where the request left nothing: {unmarked}"
    );
}

#[test]
fn a_serve_chain_with_nothing_to_do_makes_no_processor() {
    let state = IntegrationRequestState::default();

    let none = create_serve_processor(&serve_chain_of(Vec::new()), &reader(&state))
        .expect("should plan an empty chain");
    assert!(
        none.processor.is_none(),
        "should make no processor for no middleware"
    );

    let passes = serve_chain_of(vec![middleware("example.pass", |_| {
        MiddlewareAction::pass()
    })]);
    let none =
        create_serve_processor(&passes, &reader(&state)).expect("should plan a chain that passes");
    assert!(
        none.processor.is_none(),
        "should make no processor where every middleware leaves the page alone"
    );
}

#[test]
fn a_serve_selector_that_does_not_parse_refuses_the_reader_s_copy() {
    let chain = serve_chain_of(vec![element("example.broken", "a[", "href", |_| {
        AttributeRewriteAction::keep()
    })]);

    let message = create_serve_processor(&chain, &reader(&IntegrationRequestState::default()))
        .err()
        .expect("should refuse to plan the reader's copy");

    assert!(
        message.contains("the serve middleware `example.broken` cannot run")
            && message.contains("`a[` is not a usable selector"),
        "should name the phase, the middleware and the selector: {message}"
    );
}

#[test]
fn on_a_page_that_is_not_stored_the_serve_chain_follows_the_fetch_chain_on_the_one_pass() {
    let fetch = chain_of(vec![
        head("example.fetch-head", "<meta name=\"fetch\">"),
        element("example.fetch-link", "a", "href", |_| {
            AttributeRewriteAction::replace("/fetch")
        }),
    ]);
    let serve = serve_chain_of(vec![
        head("example.serve-head", "<meta name=\"serve\">"),
        element("example.serve-link", "a", "href", |matched| {
            AttributeRewriteAction::replace(format!("{}/serve", matched.value))
        }),
    ]);
    let html =
        r#"<html><head><title>Page</title></head><body><a href="/start">x</a></body></html>"#;

    // Read whole, and then seven bytes at a time, so the second rewriter is
    // fed the first one's output in pieces.
    for chunk_size in [8192, 7] {
        let (processor, _) = create_html_processor_with_middleware(config(), &fetch, Some(&serve))
            .expect("should plan both chains");
        let mut pipeline = StreamingPipeline::new(
            PipelineConfig {
                input_compression: Compression::None,
                output_compression: Compression::None,
                chunk_size,
            },
            processor,
        );
        let mut output = Vec::new();
        pipeline
            .process(Cursor::new(html.as_bytes()), &mut output)
            .expect("should process the document");
        let page = String::from_utf8(output).expect("should leave the document UTF-8");

        assert!(
            page.contains(r#"<a href="/fetch/serve">"#),
            "should hand the serve chain the page as the fetch chain left it: {page}"
        );
        let fetch_head = index_of(&page, "name=\"fetch\"");
        let serve_head = index_of(&page, "name=\"serve\"");
        assert!(
            fetch_head < serve_head && serve_head < index_of(&page, BUNDLE_TAG),
            "should write the serve chain's head markup after the fetch chain's and ahead of \
             the bundle: {page}"
        );
    }
}

// ---------------------------------------------------------------------------
// The settings
// ---------------------------------------------------------------------------

fn settings_with_document(extra: &str) -> Result<crate::settings::Settings, String> {
    let document = format!(
        "{}\n{extra}",
        crate::test_support::tests::crate_test_settings_str()
    );
    crate::settings::Settings::from_toml(&document).map_err(|error| format!("{error:?}"))
}

#[test]
fn fetch_entries_are_read_from_the_settings_in_the_order_written() {
    let settings = settings_with_document(
        r#"
[[fetch]]
media_type = "text/html"
path = "/news/"
middleware = ["example.news", "example.all"]

[[fetch]]
media_type = "text/html"
middleware = ["example.all"]
"#,
    )
    .expect("should read settings with fetch entries");

    assert_eq!(
        settings.phase_entries(MiddlewarePhase::Fetch),
        &PhaseEntries::new(vec![
            entry(Some("/news/"), &["example.news", "example.all"]),
            entry(None, &["example.all"]),
        ])
    );
}

#[test]
fn serve_entries_are_read_from_the_settings_beside_the_fetch_entries() {
    let settings = settings_with_document(
        r#"
[[fetch]]
media_type = "text/html"
middleware = ["example.all"]

[[serve]]
media_type = "text/html"
path = "/news/"
middleware = ["example.reader"]
"#,
    )
    .expect("should read settings with an entry in each phase");

    assert_eq!(
        settings.phase_entries(MiddlewarePhase::Fetch),
        &PhaseEntries::new(vec![entry(None, &["example.all"])])
    );
    assert_eq!(
        settings.phase_entries(MiddlewarePhase::Serve),
        &PhaseEntries::new(vec![entry(Some("/news/"), &["example.reader"])])
    );
    let written = serde_json::to_value(&settings).expect("should write the settings");
    assert_eq!(
        written.get("serve"),
        Some(&json!([
            { "media_type": "text/html", "path": "/news/", "middleware": ["example.reader"] }
        ])),
        "should write the serve entries a document holds"
    );
}

#[test]
fn settings_with_a_misshapen_serve_entry_are_refused_naming_the_phase() {
    let message =
        settings_with_document("\n[[serve]]\nmedia_type = \"text/html\"\nmiddleware = []\n")
            .expect_err("should refuse the settings");

    assert!(
        message.contains("[[serve]] entry 1 names no middleware"),
        "should name the phase and the entry: {message}"
    );
}

#[test]
fn settings_without_entries_write_no_fetch_key() {
    let settings = settings_with_document("").expect("should read settings with no entries");
    let written = serde_json::to_value(&settings).expect("should write the settings");
    assert!(
        written.get("fetch").is_none() && written.get("serve").is_none(),
        "should leave both keys out of a stored document that wrote no entries"
    );

    let settings = settings_with_document(
        "\n[[fetch]]\nmedia_type = \"text/html\"\nmiddleware = [\"example.all\"]\n",
    )
    .expect("should read settings with one entry");
    let written = serde_json::to_value(&settings).expect("should write the settings");
    assert_eq!(
        written.get("fetch"),
        Some(&json!([{ "media_type": "text/html", "middleware": ["example.all"] }])),
        "should write the entries a document holds"
    );
}

#[test]
fn settings_with_a_misshapen_entry_are_refused() {
    let cases = [
        (
            "\n[[fetch]]\nmedia_type = \"text/css\"\nmiddleware = [\"example.all\"]\n",
            "[[fetch]] entry 1 covers `text/css`",
        ),
        (
            "\n[fetch]\nmedia_type = \"text/html\"\nmiddleware = [\"example.all\"]\n",
            "A phase is a list of entries, each written as [[fetch]]",
        ),
        (
            "\n[[fetch]]\nmedia_type = \"text/html\"\nmiddleware = [\"example.all\"]\nenabled = true\n",
            "unknown field `enabled`",
        ),
        (
            "\n[[fetch]]\nmedia_type = \"text/html\"\n",
            "missing field `middleware`",
        ),
    ];

    for (document, expected) in cases {
        let message = settings_with_document(document).expect_err("should refuse the settings");
        assert!(
            message.contains(expected),
            "should say `{expected}`: {message}"
        );
    }
}

#[test]
fn settings_with_an_entry_after_one_for_the_path_slash_are_refused() {
    let message = settings_with_document(
        r#"
[[fetch]]
media_type = "text/html"
path = "/"
middleware = ["example.root"]

[[fetch]]
media_type = "text/html"
middleware = ["example.all"]
"#,
    )
    .expect_err("should refuse settings with an entry that is never reached");

    assert!(
        message.contains("[[fetch]] entry 2 covers the same pages as [[fetch]] entry 1"),
        "should name the entry that is never reached: {message}"
    );
}

#[test]
fn deploy_validation_refuses_a_misshapen_entry() {
    let mut settings = create_test_settings();
    settings.fetch = PhaseEntries::new(vec![
        entry(None, &["example.all"]),
        entry(Some("/news/"), &["example.news"]),
    ]);

    let error = crate::config::validate_settings_for_deploy(&settings)
        .expect_err("should refuse entries with one that is never reached");

    assert!(
        error
            .to_string()
            .contains("[[fetch]] entry 2 is never reached"),
        "should name the entry: {error}"
    );
}
