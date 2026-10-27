use super::*;

use std::sync::Arc;

use base64::Engine as _;

use edgezero_core::body::Body as EdgeBody;
use edgezero_core::key_value_store::KvPage;
use futures::executor::block_on;
use http::{Method, Request, header};
use serde_json::json;
use trusted_server_core::auction::compile_auction_plan;
use trusted_server_core::auction::orchestrator::AuctionOrchestrator;
use trusted_server_core::cache_policy::EdgeCacheHeader;
use trusted_server_core::ec::EcContext;
use trusted_server_core::html_processor::test_support::page_settings;
use trusted_server_core::integrations::IntegrationRegistry;
use trusted_server_core::platform::test_support::{
    StubHttpClient, build_services_with_http_client,
};
use trusted_server_core::platform::{
    KvError, PlatformHttpClient, PlatformKvStore, RuntimeServices,
};
use trusted_server_core::publisher::{
    AppContext, AuctionDispatch, buffer_publisher_response_async, handle_publisher_request,
    publisher_response_into_streaming_response,
};

const TOKEN: &str = "cwk_test_worker_token_value";
const MARKER: &str = "\u{E0100}\u{E0101}\u{FE01}";
const MANIFEST_URL: &str = "https://api.example.com/manifests/doc-1";
const POLICY_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ARTICLE: &str = "<!doctype html><html><head><title>Harbor Council Votes</title></head>\
    <body><nav>Home</nav><article><h1>Harbor council votes on the ferry schedule</h1>\
    <p>The harbor council met on Tuesday evening and approved a revised ferry schedule \
    for the coming season.</p><p>Residents asked for earlier departures on weekdays.</p>\
    </article><footer>Footer</footer></body></html>";

fn module_settings(table: &str, serve: bool) -> String {
    let entry = if serve {
        format!("[[serve]]\nmedia_type = \"text/html\"\nmiddleware = [\"{MODULE}\"]\n")
    } else {
        String::new()
    };
    format!("[provenance]\nmodule = \"encypher\"\n\n[provenance.encypher]\n{table}\n{entry}")
}

fn table() -> String {
    format!(
        "api_base_url = \"https://api.example.com\"\ndomain = \"publisher.example.com\"\n\
         worker_token = \"{TOKEN}\"\n"
    )
}

fn placed() -> Settings {
    page_settings(&module_settings(&table(), true))
}

fn config_value() -> serde_json::Value {
    json!({
        "api_base_url": "https://api.example.com",
        "domain": "publisher.example.com",
        "worker_token": TOKEN,
    })
}

fn parsed(value: &serde_json::Value) -> Result<EncypherConfig, String> {
    let config: EncypherConfig =
        serde_json::from_value(value.clone()).map_err(|error| error.to_string())?;
    config.validate().map_err(|error| error.to_string())?;
    Ok(config)
}

#[test]
fn module_constant_is_the_crate_folder() {
    assert_eq!(
        MODULE,
        trusted_server_core::module_name!(),
        "should be named by the folder this crate lives in"
    );
}

#[test]
fn an_unselected_module_registers_nothing() {
    let settings = page_settings("");

    assert!(register(&settings).expect("should read").is_none());
    assert!(!validate(&settings).expect("should read"));
}

#[test]
fn a_selected_and_placed_module_registers_its_middleware_and_media_signer() {
    let settings = placed();

    assert!(validate(&settings).expect("should validate"));
    let registration = register(&settings)
        .expect("should register")
        .expect("a selected module with a worker token should register");
    let names: Vec<&str> = registration
        .middleware
        .iter()
        .map(|middleware| middleware.middleware_id())
        .collect();
    assert_eq!(
        names,
        [MODULE, STYLESHEET_MIDDLEWARE],
        "should supply the page and stylesheet middleware"
    );
    assert_eq!(
        registration
            .media_signer
            .as_ref()
            .map(|signer| signer.signer_id()),
        Some(MODULE),
        "should register the media signer alongside the text path"
    );
    let stylesheet = &registration.middleware[1];
    assert!(stylesheet.handles_media_type("text/css"));
    assert!(
        stylesheet.handles_path("/assets/site.css"),
        "the stylesheet path should not inherit the page path's asset exclusions"
    );
}

#[test]
fn a_module_named_in_no_serve_entry_is_refused() {
    let settings = page_settings(&module_settings(&table(), false));

    let error = validate(&settings).expect_err("should refuse the document");

    assert!(
        format!("{error:?}").contains("no [[serve]] entry names"),
        "should name the missing entry: {error:?}"
    );
}

#[test]
fn a_blank_worker_token_registers_nothing() {
    for token in ["", "  \t"] {
        let table = format!(
            "api_base_url = \"https://api.example.com\"\ndomain = \"publisher.example.com\"\n\
             worker_token = \"{token}\"\n"
        );
        let settings = page_settings(&module_settings(&table, true));

        assert!(
            register(&settings).expect("should read").is_none(),
            "token {token:?} should not register"
        );
    }
}

#[test]
fn required_settings_and_bounds_are_enforced() {
    for field in ["domain", "api_base_url", "worker_token"] {
        let mut config = config_value();
        config
            .as_object_mut()
            .expect("should be an object")
            .remove(field);
        assert!(
            parsed(&config).is_err(),
            "config without {field} should fail"
        );
    }
    for (field, value) in [
        ("api_base_url", json!("http://api.example.com")),
        ("api_base_url", json!("https://api.example.com/?q=1")),
        ("domain", json!("Publisher.example.com")),
        ("domain", json!("www.publisher.example.com")),
        ("domain", json!("publisher.example.com/path")),
        ("domain", json!("localhost")),
        ("timeout_ms", json!(100)),
        ("timeout_ms", json!(20_000)),
        ("min_text_length", json!(10)),
        ("negative_ttl_seconds", json!(10)),
        ("policy_ttl_seconds", json!(301)),
        ("article_selector", json!("a".repeat(92))),
        ("max_media_bytes", json!(100)),
        ("max_media_bytes", json!(8_000_001)),
        ("max_media_sign_calls_per_document", json!(0)),
        ("max_media_sign_calls_per_document", json!(129)),
        ("max_large_media_bytes", json!(1)),
        (
            "max_large_media_bytes",
            json!(MAX_PREPARED_SOURCE_BYTES + 1),
        ),
        ("max_large_media_ms", json!(4_999)),
        ("max_large_media_ms", json!(600_001)),
        ("enabled", json!(true)),
        ("path_prefixes", json!(["/news/"])),
    ] {
        let mut config = config_value();
        config[field] = value.clone();
        assert!(
            parsed(&config).is_err(),
            "{field} = {value} should be rejected"
        );
    }
    for value in [0, MIN_LARGE_MEDIA_BYTES, MAX_PREPARED_SOURCE_BYTES] {
        let mut config = config_value();
        config["max_large_media_bytes"] = json!(value);
        assert!(
            parsed(&config).is_ok(),
            "max_large_media_bytes = {value} should be accepted, 0 turning the lane off"
        );
    }
    for limit in [1, 64, 128] {
        let mut config = config_value();
        config["max_media_sign_calls_per_document"] = json!(limit);
        let accepted = parsed(&config).expect("bounded media-call limit");
        assert_eq!(accepted.max_media_sign_calls_per_document, limit);
    }
    for (max_hold_ms, timeout_ms, valid) in [
        (499, 250, false),
        (500, 250, true),
        (15_000, 3_000, true),
        (15_001, 3_000, false),
        (3_000, 3_000, false),
        (3_001, 3_000, true),
    ] {
        let mut config = config_value();
        config["max_hold_ms"] = json!(max_hold_ms);
        config["timeout_ms"] = json!(timeout_ms);
        assert_eq!(
            parsed(&config).is_ok(),
            valid,
            "max_hold_ms {max_hold_ms} with timeout_ms {timeout_ms}"
        );
    }
}

#[test]
fn article_selector_rejects_malformed_classes_at_settings_admission() {
    for selector in [
        ".",
        "..article",
        ".123article",
        ".article.body",
        ".article body",
        ".article>p",
        ".article,main",
        ".article#body",
        ".article:hover",
        ".article[data-body=yes]",
        ".article\\:body",
    ] {
        let table = format!("{}article_selector = {}\n", table(), json!(selector));
        let settings = page_settings(&module_settings(&table, true));
        assert!(
            validate(&settings).is_err(),
            "malformed class selector {selector:?} should fail settings admission"
        );
        assert!(
            register(&settings).is_err(),
            "malformed class selector {selector:?} should not register"
        );
    }
}

#[test]
fn article_selector_rejects_malformed_attributes_at_settings_admission() {
    for selector in [
        "[",
        "[itemprop]",
        "[=articleBody]",
        "[itemprop=]",
        "[itemprop=articleBody",
        "[itemprop=articleBody]p",
        "[itemprop~=articleBody]",
        "[itemprop=articleBody main]",
        "[itemprop=articleBody=main]",
        "[itemprop=[articleBody]]",
        "[itemprop=\"articleBody']",
        "[itemprop='articleBody' i]",
        "[itemprop=articleBody][role=main]",
        "[itemprop=articleBody],main",
        "[itemprop='article\\Body']",
    ] {
        let table = format!("{}article_selector = {}\n", table(), json!(selector));
        let settings = page_settings(&module_settings(&table, true));
        assert!(
            validate(&settings).is_err(),
            "malformed attribute selector {selector:?} should fail settings admission"
        );
        assert!(
            register(&settings).is_err(),
            "malformed attribute selector {selector:?} should not register"
        );
    }
}

#[test]
fn article_selector_rejects_malformed_tags_at_settings_admission() {
    for selector in [
        " ",
        " article",
        "article ",
        "123article",
        "<article>",
        "article.body",
        "article p",
        "article>p",
        "article+p",
        "article~p",
        "article,main",
        "article#body",
        "article:hover",
        "article[role=main]",
        "#article",
        "*",
    ] {
        let table = format!("{}article_selector = {}\n", table(), json!(selector));
        let settings = page_settings(&module_settings(&table, true));
        assert!(
            validate(&settings).is_err(),
            "malformed tag selector {selector:?} should fail settings admission"
        );
        assert!(
            register(&settings).is_err(),
            "malformed tag selector {selector:?} should not register"
        );
    }
}

#[test]
fn article_selector_accepts_supported_syntax_and_selects_the_override() {
    let selected = "<custom-story class=\"story-body _story -story --story\" \
        data-body=\"article body\" itemprop=\"articleBody\">Selected.</custom-story>";
    let html = format!("<article>Fallback.</article>{selected}");
    for selector in [
        ".story-body",
        "._story",
        ".-story",
        ".--story",
        "[itemprop=articleBody]",
        "[itemprop='articleBody']",
        "[itemprop=\"articleBody\"]",
        "[data-body='article body']",
        "custom-story",
        "CUSTOM-STORY",
    ] {
        let table = format!("{}article_selector = {}\n", table(), json!(selector));
        let settings = page_settings(&module_settings(&table, true));
        assert!(
            validate(&settings).expect("should admit supported selector"),
            "supported selector {selector:?} should select the module"
        );
        assert!(
            register(&settings)
                .expect("should register supported selector")
                .is_some(),
            "supported selector {selector:?} should register"
        );
        let boundary = boundary::find_article_boundary(&html, Some(selector))
            .expect("should find the configured override");
        assert_eq!(
            boundary.selector,
            format!("override:{selector}"),
            "supported selector should take priority over the article fallback"
        );
        assert_eq!(html.get(boundary.start..boundary.end), Some(selected));
    }
}

#[test]
fn article_selector_empty_or_valid_but_absent_keeps_article_fallback() {
    let selected = "<article><p>Fallback article.</p></article>";
    let html = format!("<body><main>Other region.</main>{selected}</body>");
    for selector in ["", ".absent-story", "[data-body=absent]", "custom-story"] {
        let table = format!("{}article_selector = {}\n", table(), json!(selector));
        let settings = page_settings(&module_settings(&table, true));
        assert!(
            validate(&settings).expect("should admit empty or absent selector"),
            "selector {selector:?} should select the module"
        );
        let boundary = boundary::find_article_boundary(&html, Some(selector))
            .expect("should find the fallback article");
        assert_eq!(boundary.selector, "article");
        assert_eq!(html.get(boundary.start..boundary.end), Some(selected));
    }
}

#[test]
fn excluded_paths_leave_the_middleware_out_of_the_chain() {
    let middleware = Encypher {
        signer: Arc::new(Signer::new(
            parsed(&config_value()).expect("should parse"),
            5_000_000,
        )),
    };

    assert!(middleware.handles_path("/news/harbor-vote"));
    for path in [
        "/wp-admin/post",
        "/feed/",
        "/sitemap.xml",
        "/app.js",
        "/api/v1/x",
    ] {
        assert!(!middleware.handles_path(path), "{path} should be left out");
    }
    assert_eq!(middleware.phases(), [MiddlewarePhase::Serve]);
}

/// A runtime KV store held in memory.
#[derive(Default)]
struct MemoryKv(std::sync::Mutex<std::collections::HashMap<String, bytes::Bytes>>);

#[async_trait::async_trait(?Send)]
impl PlatformKvStore for MemoryKv {
    async fn get_bytes(&self, key: &str) -> Result<Option<bytes::Bytes>, KvError> {
        Ok(self.0.lock().expect("should lock").get(key).cloned())
    }

    async fn put_bytes(&self, key: &str, value: bytes::Bytes) -> Result<(), KvError> {
        self.0
            .lock()
            .expect("should lock")
            .insert(key.to_owned(), value);
        Ok(())
    }

    async fn put_bytes_with_ttl(
        &self,
        key: &str,
        value: bytes::Bytes,
        _ttl: std::time::Duration,
    ) -> Result<(), KvError> {
        self.put_bytes(key, value).await
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        self.0.lock().expect("should lock").remove(key);
        Ok(())
    }

    async fn list_keys_page(
        &self,
        _prefix: &str,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<KvPage, KvError> {
        Ok(KvPage::default())
    }
}

/// An origin answering [`ARTICLE`] with `cache_control`, then the policy and
/// sign answers of the API.
fn stub(cache_control: &str) -> Arc<StubHttpClient> {
    let stub = Arc::new(StubHttpClient::new());
    stub.push_response_with_headers(
        200,
        ARTICLE.as_bytes().to_vec(),
        vec![
            ("content-type", "text/html; charset=utf-8"),
            ("cache-control", cache_control),
        ],
    );
    stub.push_response(
        200,
        serde_json::to_vec(&json!({ "policy_digest": POLICY_DIGEST })).expect("should encode"),
    );
    stub.push_response(
        200,
        serde_json::to_vec(&json!({
            "success": true,
            "policy_digest": POLICY_DIGEST,
            "embedding_plan": {
                "index_unit": "codepoint",
                "operations": [{ "insert_after_index": 5, "marker": MARKER }],
            },
            "manifest_url": MANIFEST_URL,
            "verification_url": "https://api.example.com/verify/doc-1",
            "document_id": "doc-1",
        }))
        .expect("should encode"),
    );
    stub
}

fn origin_stub(cache_control: &str) -> Arc<StubHttpClient> {
    let stub = Arc::new(StubHttpClient::new());
    stub.push_response_with_headers(
        200,
        ARTICLE.as_bytes().to_vec(),
        vec![
            ("content-type", "text/html; charset=utf-8"),
            ("cache-control", cache_control),
        ],
    );
    stub
}

fn page_request(method: Method) -> Request<EdgeBody> {
    Request::builder()
        .method(method)
        .uri("https://publisher.example.com/news/harbor-vote?utm_source=feed")
        .header(header::HOST, "publisher.example.com")
        .body(EdgeBody::empty())
        .expect("should build the page request")
}

/// The response a reader receives from the publisher path, through the
/// buffered or the streaming finalizer.
fn served_response(
    settings: &Settings,
    services: &RuntimeServices,
    request: Request<EdgeBody>,
    streaming: bool,
) -> http::Response<EdgeBody> {
    try_served_response(settings, services, request, streaming).expect("should finish the page")
}

fn try_served_response(
    settings: &Settings,
    services: &RuntimeServices,
    request: Request<EdgeBody>,
    streaming: bool,
) -> Result<http::Response<EdgeBody>, error_stack::Report<TrustedServerError>> {
    let registry = IntegrationRegistry::with_registrations(settings, &[builder()])
        .expect("should build the registry");
    // As every adapter does, the request's services carry the registry's
    // media signer.
    let services = &services.clone().with_media_signer(registry.media_signer());
    let plan = compile_auction_plan(settings).expect("should compile the auction plan");
    let orchestrator = Arc::new(AuctionOrchestrator::from_plan(Arc::new(plan), None));
    let method = request.method().clone();
    block_on(async {
        let mut ec_context =
            EcContext::read_from_request_resolving_geo(settings, &request, services)
                .await
                .expect("should read the request's state");
        let publisher = handle_publisher_request(
            AppContext {
                settings,
                integration_registry: &registry,
            },
            services,
            None,
            &mut ec_context,
            AuctionDispatch {
                orchestrator: &orchestrator,
                slots: &[],
                registry: None,
            },
            request,
            EdgeCacheHeader::SurrogateControl,
        )
        .await?;
        if streaming {
            publisher_response_into_streaming_response(
                publisher,
                &method,
                Arc::new(settings.clone()),
                &registry,
                Arc::clone(&orchestrator),
                services.clone(),
            )
            .await
        } else {
            buffer_publisher_response_async(
                publisher,
                &method,
                settings,
                &registry,
                &orchestrator,
                services,
            )
            .await
        }
    })
}

/// The page body a reader receives from the publisher path.
fn served(
    settings: &Settings,
    services: &RuntimeServices,
    request: Request<EdgeBody>,
    streaming: bool,
) -> String {
    let response = served_response(settings, services, request, streaming);
    block_on(async {
        let body = response
            .into_body()
            .into_bytes_bounded(16 * 1024 * 1024)
            .await
            .expect("should read the body");
        String::from_utf8(body.to_vec()).expect("should be UTF-8")
    })
}

fn services_for(stub: &Arc<StubHttpClient>) -> RuntimeServices {
    services_for_kv(stub, Arc::new(MemoryKv::default()))
}

fn services_for_kv(stub: &Arc<StubHttpClient>, kv: Arc<MemoryKv>) -> RuntimeServices {
    build_services_with_http_client(Arc::clone(stub) as Arc<dyn PlatformHttpClient>)
        .with_kv_store(kv)
}

#[test]
fn the_publisher_path_signs_an_article_through_both_finalizers() {
    for streaming in [false, true] {
        let stub = stub("public, max-age=60");
        let page = served(
            &placed(),
            &services_for(&stub),
            page_request(Method::GET),
            streaming,
        );

        assert!(
            page.contains(MARKER),
            "streaming={streaming}: should embed the marker: {page}"
        );
        assert!(
            page.contains("c2pa-manifest-url") && page.contains(MANIFEST_URL),
            "streaming={streaming}: should write the manifest meta tag"
        );
        assert_eq!(
            stub.recorded_request_methods(),
            ["GET", "GET", "POST"],
            "streaming={streaming}: origin, policy, then sign"
        );
        // The test request carries no TLS signal, so the scheme the publisher
        // path detects is `http`.
        let policy_uri = &stub.recorded_request_uris()[1];
        assert!(
            policy_uri
                .ends_with("page_url=http%3A%2F%2Fpublisher.example.com%2Fnews%2Fharbor-vote"),
            "should send the page address without its query: {policy_uri}"
        );
    }
}

#[test]
fn an_article_on_a_publisher_addressed_by_path_is_signed_for_its_own_address() {
    let stub = stub("public, max-age=60");
    let mut request = page_request(Method::GET);
    request
        .extensions_mut()
        .insert(trusted_server_core::addressing::RemovedPathPrefix::new(
            "/www.site.example".to_owned(),
        ));

    let page = served(&placed(), &services_for(&stub), request, false);

    assert!(page.contains(MARKER), "should sign the article: {page}");
    let policy_uri = &stub.recorded_request_uris()[1];
    assert!(
        policy_uri.ends_with(
            "page_url=http%3A%2F%2Fpublisher.example.com%2Fwww.site.example%2Fnews%2Fharbor-vote"
        ),
        "should ask the policy for the address the reader asked for: {policy_uri}"
    );
    let sign = String::from_utf8(stub.recorded_request_bodies()[2].clone()).expect("JSON body");
    assert!(
        sign.contains(
            "\"page_url\":\"http://publisher.example.com/www.site.example/news/harbor-vote\""
        ),
        "should sign the article for the address the reader asked for: {sign}"
    );
}

/// The unsigned bytes produced by the same public publisher pipeline before
/// its final writer applies the configured bound. Core middleware may rewrite
/// the origin body before Encypher sees it, so the origin fixture length is not
/// the final unsigned length.
fn unsigned_public_page() -> String {
    let stub = origin_stub("public, max-age=60");
    let page = served(
        &placed(),
        &services_for(&stub),
        page_request(Method::GET),
        false,
    );
    assert!(
        page.len() > ARTICLE.len(),
        "fixture must retain the HTML rewriter's output overhead"
    );
    page
}

#[test]
fn fresh_expansion_over_the_buffered_limit_returns_the_unsigned_page() {
    let unsigned = unsigned_public_page();
    let mut settings = placed();
    settings.publisher.max_buffered_body_bytes = unsigned.len();
    let stub = stub("public, max-age=60");
    let response = served_response(
        &settings,
        &services_for(&stub),
        page_request(Method::GET),
        false,
    );

    assert_eq!(response.status(), 200);
    let body = block_on(response.into_body().into_bytes_bounded(unsigned.len()))
        .expect("the unsigned pipeline output should fit its exact buffered limit");
    assert_eq!(body.as_ref(), unsigned.as_bytes());
    assert_eq!(stub.recorded_request_methods(), ["GET", "GET", "POST"]);
}

#[test]
fn cached_expansion_over_the_buffered_limit_returns_the_unsigned_page() {
    let unsigned = unsigned_public_page();
    let kv = Arc::new(MemoryKv::default());
    let first = stub("public, max-age=60");
    let signed = served(
        &placed(),
        &services_for_kv(&first, Arc::clone(&kv)),
        page_request(Method::GET),
        false,
    );
    assert!(signed.contains(MARKER), "should seed a valid plan");

    let mut settings = placed();
    settings.publisher.max_buffered_body_bytes = unsigned.len();
    let cached = origin_stub("public, max-age=60");
    let response = served_response(
        &settings,
        &services_for_kv(&cached, kv),
        page_request(Method::GET),
        false,
    );

    assert_eq!(response.status(), 200);
    let body = block_on(response.into_body().into_bytes_bounded(unsigned.len()))
        .expect("the unsigned pipeline output should fit its exact buffered limit");
    assert_eq!(body.as_ref(), unsigned.as_bytes());
    assert_eq!(
        cached.recorded_request_methods(),
        ["GET"],
        "the cached policy and plan should make no Encypher API request"
    );
}

#[test]
fn a_page_the_origin_marked_private_is_never_sent() {
    let stub = stub("private, max-age=0");

    let page = served(
        &placed(),
        &services_for(&stub),
        page_request(Method::GET),
        false,
    );

    assert!(
        !page.contains(MARKER),
        "should not sign a reader's own page"
    );
    assert_eq!(
        stub.recorded_request_methods(),
        ["GET"],
        "should call the origin and nothing else"
    );
}

/// The pixels of a small image an origin embeds or serves.
const SOURCE_IMAGE: &[u8] = b"\x89PNG\r\n\x1a\n-source-pixels";
/// The same image with an embedded C2PA manifest, as the media endpoint
/// answers it.
const SIGNED_IMAGE: &[u8] = b"\x89PNG\r\n\x1a\n-source-pixels-caBX-jumb-c2pa-manifest";

fn encoded(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The headers of an embedded-manifest answer of the media endpoint for
/// `body` of `content_type`.
fn media_answer_as(body: &[u8], content_type: &str) -> Vec<(String, String)> {
    [
        ("content-type", content_type.to_owned()),
        ("x-encypher-policy-digest", POLICY_DIGEST.to_owned()),
        ("x-encypher-signed-sha256", media_api::sha256_hex(body)),
        ("x-encypher-manifest-instance-id", "xmp:iid:0001".to_owned()),
        ("x-encypher-manifest-disposition", "embedded".to_owned()),
        ("x-encypher-manifest-link-required", "false".to_owned()),
        (
            "x-encypher-verification-url",
            "https://verify.example.com/doc-2".to_owned(),
        ),
        ("x-encypher-document-id", "doc-2".to_owned()),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value))
    .collect()
}

/// [`ARTICLE`] with a lazily loaded embedded image inside the article.
fn article_with_image() -> String {
    ARTICLE.replace(
        "</article>",
        &format!(
            "<img loading=\"lazy\" src=\"data:image/png;base64,{}\" alt=\"Harbor\"></article>",
            encoded(SOURCE_IMAGE)
        ),
    )
}

/// An origin answering `body` as `content_type`, publicly cacheable.
fn origin_answer(stub: &StubHttpClient, body: &[u8], content_type: &str) {
    stub.push_response_with_headers(
        200,
        body.to_vec(),
        vec![
            ("content-type", content_type),
            ("cache-control", "public, max-age=60"),
        ],
    );
}

/// The policy answer the media lane asks for live, before it signs or reuses
/// embedded media.
fn live_policy(stub: &StubHttpClient) {
    stub.push_response(
        200,
        serde_json::to_vec(&json!({ "policy_digest": POLICY_DIGEST })).expect("should encode"),
    );
}

#[test]
fn a_revoked_credential_stops_embedded_media_reuse_while_the_cached_policy_lives() {
    let kv = Arc::new(MemoryKv::default());
    let first = Arc::new(StubHttpClient::new());
    origin_answer(
        &first,
        article_with_image().as_bytes(),
        "text/html; charset=utf-8",
    );
    text_answers(&first);
    live_policy(&first);
    first.push_response_with_headers(200, SIGNED_IMAGE.to_vec(), media_answer(SIGNED_IMAGE));
    let warmed = served(
        &placed(),
        &services_for_kv(&first, Arc::clone(&kv)),
        page_request(Method::GET),
        false,
    );
    assert!(
        warmed.contains(&encoded(SIGNED_IMAGE)),
        "should warm the policy and media caches"
    );

    // The cached policy and the cached signed image are both still fresh;
    // the service now refuses the credential.
    let revoked = Arc::new(StubHttpClient::new());
    origin_answer(
        &revoked,
        article_with_image().as_bytes(),
        "text/html; charset=utf-8",
    );
    revoked.push_response(403, b"{}".to_vec());
    let page = served(
        &placed(),
        &services_for_kv(&revoked, kv),
        page_request(Method::GET),
        false,
    );

    assert!(
        !page.contains(&encoded(SIGNED_IMAGE)),
        "should never reuse a signed image under a revoked credential"
    );
    assert!(
        page.contains(&encoded(SOURCE_IMAGE)),
        "should deliver the image as the origin wrote it"
    );
    assert!(
        !revoked
            .recorded_request_methods()
            .iter()
            .any(|method| method == "POST"),
        "should sign nothing"
    );
}

/// The policy and article-sign answers [`stub`] queues after the origin.
fn text_answers(stub: &StubHttpClient) {
    stub.push_response(
        200,
        serde_json::to_vec(&json!({ "policy_digest": POLICY_DIGEST })).expect("should encode"),
    );
    stub.push_response(
        200,
        serde_json::to_vec(&json!({
            "success": true,
            "policy_digest": POLICY_DIGEST,
            "embedding_plan": {
                "index_unit": "codepoint",
                "operations": [{ "insert_after_index": 5, "marker": MARKER }],
            },
            "manifest_url": MANIFEST_URL,
        }))
        .expect("should encode"),
    );
}

#[test]
fn the_publisher_path_signs_the_article_then_its_embedded_image() {
    for streaming in [false, true] {
        let stub = Arc::new(StubHttpClient::new());
        origin_answer(
            &stub,
            article_with_image().as_bytes(),
            "text/html; charset=utf-8",
        );
        text_answers(&stub);
        live_policy(&stub);
        stub.push_response_with_headers(200, SIGNED_IMAGE.to_vec(), media_answer(SIGNED_IMAGE));

        let page = served(
            &placed(),
            &services_for(&stub),
            page_request(Method::GET),
            streaming,
        );

        assert!(
            page.contains(MARKER),
            "streaming={streaming}: should sign the text"
        );
        assert!(
            page.contains(&format!(
                "src=\"data:image/png;base64,{}\" alt=\"Harbor\"",
                encoded(SIGNED_IMAGE)
            )),
            "streaming={streaming}: should deliver the signed image bytes in place: {page}"
        );
        assert!(
            !page.contains(&encoded(SOURCE_IMAGE)),
            "streaming={streaming}: should leave no unsigned copy"
        );
        assert_eq!(
            stub.recorded_request_methods(),
            ["GET", "GET", "POST", "GET", "POST"],
            "streaming={streaming}: origin, policy, text sign, live policy, media sign"
        );
        assert!(
            stub.recorded_request_uris()[4].ends_with("/api/v1/public/cdn/media/sign"),
            "streaming={streaming}: should call the media endpoint"
        );
        let media_body = &stub.recorded_request_bodies()[4];
        assert!(
            media_body
                .windows(SOURCE_IMAGE.len())
                .any(|window| window == SOURCE_IMAGE),
            "streaming={streaming}: should send the decoded source bytes"
        );
    }
}

#[test]
fn a_refused_embedded_image_stays_as_written_and_the_text_is_still_signed() {
    let stub = Arc::new(StubHttpClient::new());
    origin_answer(
        &stub,
        article_with_image().as_bytes(),
        "text/html; charset=utf-8",
    );
    text_answers(&stub);
    live_policy(&stub);
    stub.push_response(
        422,
        serde_json::to_vec(&json!({
            "success": false,
            "error": {"code": "UNSUPPORTED_MEDIA_TYPE", "message": "x"},
        }))
        .expect("should encode"),
    );

    let page = served(
        &placed(),
        &services_for(&stub),
        page_request(Method::GET),
        false,
    );

    assert!(page.contains(MARKER), "should still sign the text");
    assert!(
        page.contains(&encoded(SOURCE_IMAGE)),
        "should leave the unsigned image as the origin wrote it"
    );
    assert!(
        !page.contains(&encoded(SIGNED_IMAGE)),
        "should never present unsigned bytes as signed"
    );
}

/// The module's settings with a stylesheet entry, scoped by `path` when one
/// is given.
fn stylesheet_settings(path: Option<&str>) -> String {
    let path = path.map_or(String::new(), |path| format!("path = \"{path}\"\n"));
    format!(
        "{}\n[[serve]]\nmedia_type = \"text/css\"\n{path}middleware = [\"{STYLESHEET_MIDDLEWARE}\"]\n",
        module_settings(&table(), true)
    )
}

#[test]
fn a_stylesheet_entry_for_every_path_is_refused() {
    for path in [None, Some("/")] {
        let settings = page_settings(&stylesheet_settings(path));
        let error = validate(&settings).expect_err("should refuse an unscoped entry");
        assert!(
            format!("{error:?}").contains("integrity"),
            "{path:?}: should explain the integrity risk: {error:?}"
        );
    }
    let scoped = page_settings(&stylesheet_settings(Some("/assets/")));
    assert!(validate(&scoped).expect("should accept a scoped entry"));
}

#[test]
fn the_publisher_path_signs_media_a_stylesheet_embeds() {
    let settings = page_settings(&stylesheet_settings(Some("/assets/")));
    let stylesheet = format!(
        ".hero{{background-image:url(\"data:image/png;base64,{}\")}}",
        encoded(SOURCE_IMAGE)
    );
    let stub = Arc::new(StubHttpClient::new());
    origin_answer(&stub, stylesheet.as_bytes(), "text/css; charset=utf-8");
    stub.push_response(
        200,
        serde_json::to_vec(&json!({ "policy_digest": POLICY_DIGEST })).expect("should encode"),
    );
    live_policy(&stub);
    stub.push_response_with_headers(200, SIGNED_IMAGE.to_vec(), media_answer(SIGNED_IMAGE));
    let request = Request::builder()
        .method(Method::GET)
        .uri("https://publisher.example.com/assets/site.css")
        .header(header::HOST, "publisher.example.com")
        .body(EdgeBody::empty())
        .expect("should build the stylesheet request");

    let served_stylesheet = served(&settings, &services_for(&stub), request, false);

    assert_eq!(
        served_stylesheet,
        format!(
            ".hero{{background-image:url(\"data:image/png;base64,{}\")}}",
            encoded(SIGNED_IMAGE)
        ),
        "should replace the embedded image with its signed bytes and nothing else"
    );
}

/// The headers of an embedded-manifest PNG answer for `body`.
fn media_answer(body: &[u8]) -> Vec<(String, String)> {
    media_answer_as(body, "image/png")
}

fn media_request(path: &str) -> Request<EdgeBody> {
    Request::builder()
        .method(Method::GET)
        .uri(format!("https://publisher.example.com{path}"))
        .header(header::HOST, "publisher.example.com")
        .header(header::ACCEPT, "*/*")
        .body(EdgeBody::empty())
        .expect("should build the media request")
}

fn body_of(response: http::Response<EdgeBody>) -> bytes::Bytes {
    block_on(response.into_body().into_bytes_bounded(16 * 1024 * 1024))
        .expect("should read the body")
}

#[test]
fn the_publisher_path_delivers_the_signed_bytes_of_images_audio_and_video_on_any_path() {
    for (path, content_type) in [
        ("/images/harbor.png", "image/png"),
        ("/media/4711", "image/png"),
        ("/video/ferry.mp4", "video/mp4"),
        ("/audio/council.mp3", "audio/mpeg"),
    ] {
        let stub = Arc::new(StubHttpClient::new());
        origin_answer(&stub, SOURCE_IMAGE, content_type);
        stub.push_response(
            200,
            serde_json::to_vec(&json!({ "policy_digest": POLICY_DIGEST })).expect("should encode"),
        );
        stub.push_response_with_headers(
            200,
            SIGNED_IMAGE.to_vec(),
            media_answer_as(SIGNED_IMAGE, content_type),
        );

        let response = served_response(&placed(), &services_for(&stub), media_request(path), false);

        assert_eq!(response.status(), 200, "{path}: should answer");
        let headers = response.headers().clone();
        assert_eq!(
            headers
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(content_type),
            "{path}: should keep the media type"
        );
        assert_eq!(
            headers
                .get("x-encypher-document-id")
                .and_then(|value| value.to_str().ok()),
            Some("doc-2"),
            "{path}: should name the signed document"
        );
        assert_eq!(
            headers
                .get(header::ETAG)
                .and_then(|value| value.to_str().ok()),
            Some(format!("\"sig-{}\"", media_api::sha256_hex(SIGNED_IMAGE)).as_str()),
            "{path}: should validate the signed bytes, not the origin's"
        );
        assert_eq!(
            body_of(response).as_ref(),
            SIGNED_IMAGE,
            "{path}: should deliver the exact signed bytes"
        );
        assert_eq!(
            stub.recorded_request_methods(),
            ["GET", "GET", "POST"],
            "{path}: origin, policy, media sign"
        );
        let metadata = &stub.recorded_request_bodies()[2];
        assert!(
            metadata
                .windows(SOURCE_IMAGE.len())
                .any(|window| window == SOURCE_IMAGE),
            "{path}: should send the captured origin bytes"
        );
        assert!(
            metadata
                .windows(b"\"origin_host\":\"".len())
                .any(|window| window == b"\"origin_host\":\""),
            "{path}: should name the host core fetched from"
        );
    }
}

#[test]
fn a_failed_media_signature_delivers_the_origin_bytes_unmarked() {
    let stub = Arc::new(StubHttpClient::new());
    origin_answer(&stub, SOURCE_IMAGE, "image/png");
    stub.push_response(
        200,
        serde_json::to_vec(&json!({ "policy_digest": POLICY_DIGEST })).expect("should encode"),
    );
    // An answer whose bytes are not the ones it names is never delivered.
    stub.push_response_with_headers(
        200,
        b"\x89PNG\r\n\x1a\nswapped-c2pa".to_vec(),
        media_answer(SIGNED_IMAGE),
    );

    let response = served_response(
        &placed(),
        &services_for(&stub),
        media_request("/images/harbor.png"),
        false,
    );

    assert!(
        response.headers().get("x-encypher-document-id").is_none(),
        "should claim no signature"
    );
    assert_eq!(
        body_of(response).as_ref(),
        SOURCE_IMAGE,
        "should deliver the origin's bytes as they were"
    );
}

#[test]
fn a_media_segment_passes_through_as_the_origin_sent_it() {
    // A C2PA verifiable-segment-info segment is bound by its stream's init
    // manifest. Signing it alone would fail, and the unsigned fallback would
    // make it private and drop its validators.
    const SEGMENT: &[u8] = b"\0\0\0\x18stypmsdh\0\0\0\0msdhmsixmoof-mdat-emsg";
    let stub = Arc::new(StubHttpClient::new());
    stub.push_response_with_headers(
        200,
        SEGMENT.to_vec(),
        vec![
            ("content-type", "video/iso.segment"),
            ("cache-control", "public, max-age=60"),
            ("etag", "\"segment-7\""),
        ],
    );

    let response = served_response(
        &placed(),
        &services_for(&stub),
        media_request("/live/ferry/segment-7.m4s"),
        false,
    );

    assert_eq!(response.status(), 200, "should answer");
    let headers = response.headers().clone();
    assert_eq!(
        headers
            .get(header::ETAG)
            .and_then(|value| value.to_str().ok()),
        Some("\"segment-7\""),
        "should keep the origin's validator"
    );
    assert!(
        headers
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| !value.contains("private")),
        "should not make the segment private"
    );
    assert!(
        headers.get("x-encypher-document-id").is_none(),
        "should claim no signature"
    );
    assert_eq!(
        body_of(response).as_ref(),
        SEGMENT,
        "should deliver the segment byte for byte"
    );
    assert_eq!(
        stub.recorded_request_methods(),
        ["GET"],
        "should ask neither the policy nor the signing endpoint"
    );
}

/// One ISO BMFF box of `kind` around `body`.
fn bmff_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let size = u32::try_from(body.len() + 8).expect("should fit a 32-bit size");
    [&size.to_be_bytes()[..], kind, body].concat()
}

#[test]
fn fragmented_stream_parts_typed_as_mp4_pass_through_as_the_origin_sent_them() {
    let init = [
        bmff_box(b"ftyp", b"cmfc\0\0\0\0iso6cmfc"),
        bmff_box(
            b"moov",
            &[
                bmff_box(b"mvhd", &[0; 100]),
                bmff_box(b"mvex", &bmff_box(b"trex", &[0; 24])),
                bmff_box(b"pssh", &[0; 32]),
            ]
            .concat(),
        ),
    ]
    .concat();
    let segment = [
        bmff_box(b"styp", b"msdh\0\0\0\0msdhmsix"),
        bmff_box(b"emsg", b"\0\0\0\0urn:c2pa:verifiable-segment-info\0fseg\0"),
        bmff_box(b"sidx", &[0; 32]),
        bmff_box(b"moof", &bmff_box(b"mfhd", &[0; 8])),
        bmff_box(b"mdat", b"coded-samples"),
    ]
    .concat();
    for (path, part) in [
        ("/live/ferry/init.mp4", init),
        ("/live/ferry/7.m4s", segment),
    ] {
        // No `no-transform`: the bytes alone keep the part from being signed.
        let stub = Arc::new(StubHttpClient::new());
        stub.push_response_with_headers(
            200,
            part.clone(),
            vec![
                ("content-type", "video/mp4"),
                ("cache-control", "public, max-age=60"),
                ("etag", "\"part-7\""),
            ],
        );

        let response = served_response(&placed(), &services_for(&stub), media_request(path), false);

        assert_eq!(response.status(), 200, "{path}: should answer");
        let headers = response.headers().clone();
        assert_eq!(
            headers
                .get(header::ETAG)
                .and_then(|value| value.to_str().ok()),
            Some("\"part-7\""),
            "{path}: should keep the origin's validator"
        );
        assert!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| !value.contains("private")),
            "{path}: should not make the part private"
        );
        assert!(
            headers.get(header::LINK).is_none() && headers.get("x-encypher-document-id").is_none(),
            "{path}: should add no manifest"
        );
        assert_eq!(
            body_of(response).as_ref(),
            part.as_slice(),
            "{path}: should deliver the part byte for byte"
        );
        assert_eq!(
            stub.recorded_request_methods(),
            ["GET"],
            "{path}: should ask neither the policy nor the signing endpoint"
        );
    }
}

#[test]
fn a_progressive_mp4_is_still_handed_to_the_signing_endpoint() {
    let progressive = [
        bmff_box(b"ftyp", b"isom\0\0\0\0isomavc1"),
        bmff_box(b"moov", &bmff_box(b"trak", &[0; 16])),
        bmff_box(b"mdat", b"coded-samples"),
    ]
    .concat();
    let stub = Arc::new(StubHttpClient::new());
    origin_answer(&stub, &progressive, "video/mp4");
    live_policy(&stub);
    stub.push_response_with_headers(
        200,
        SIGNED_IMAGE.to_vec(),
        media_answer_as(SIGNED_IMAGE, "video/mp4"),
    );

    let _response = served_response(
        &placed(),
        &services_for(&stub),
        media_request("/video/ferry.mp4"),
        false,
    );

    assert_eq!(
        stub.recorded_request_methods(),
        ["GET", "GET", "POST"],
        "origin, policy, media sign"
    );
    let sent = &stub.recorded_request_bodies()[2];
    assert!(
        sent.windows(progressive.len())
            .any(|window| window == progressive.as_slice()),
        "should send the progressive file's exact bytes"
    );
}

/// Signed source bytes deliberately contain targets of core's attribute and
/// text URL replacement before the provenance carrier/reference appears.
fn protected_svg(reference: bool) -> String {
    let provenance = if reference {
        "<metadata><rdf:Description dcterms:provenance='https://origin.example.com/source.c2pa'/></metadata>"
    } else {
        "<metadata><c2pa:manifest xmlns:c2pa='http://c2pa.org/manifest'>QUJD</c2pa:manifest></metadata>"
    };
    format!(
        "<svg viewBox='0 0 10 10' xmlns:xlink='http://www.w3.org/1999/xlink'>\
         <image href='https://origin.example.com/original.png'/>\
         <a xlink:href='https://origin.example.com/story'><text>https://origin.example.com/</text></a>\
         <svg><image href='data:image/png;base64,{}'/></svg>\
         <style>.x{{fill:url(https://origin.example.com/fill.svg)}}</style>\
         <text>{}</text>{provenance}</svg>",
        encoded(SOURCE_IMAGE),
        "origin.example.com ".repeat(1024),
    )
}

fn article_with_protected_svg(svg: &str) -> String {
    ARTICLE.replace(
        "</article>",
        &format!(
            "{svg}<a id='outside' href='https://origin.example.com/outside'>\
             https://origin.example.com/outside</a></article>"
        ),
    )
}

fn assert_source_and_surroundings(page: &str, svg: &str) {
    assert!(
        page.contains(svg),
        "must retain the complete original SVG byte slice"
    );
    assert!(
        page.contains("http://publisher.example.com/outside"),
        "core's ordinary outside URL edits must still run: {page}"
    );
    assert!(page.contains(MARKER), "article signing must still run");
    assert!(
        !page.contains("<!--ts-source-"),
        "opaque markers must never reach readers"
    );
}

#[test]
fn publisher_protects_original_svg_before_core_without_rehost_routes() {
    for streaming in [false, true] {
        for reference in [false, true] {
            let svg = protected_svg(reference);
            let settings = placed();
            assert!(
                settings
                    .proxy
                    .asset_routes
                    .iter()
                    .all(|route| !route.rehost_page_references)
            );
            let stub = Arc::new(StubHttpClient::new());
            origin_answer(
                &stub,
                article_with_protected_svg(&svg).as_bytes(),
                "text/html; charset=utf-8",
            );
            text_answers(&stub);
            let page = served(
                &settings,
                &services_for(&stub),
                page_request(Method::GET),
                streaming,
            );
            assert_source_and_surroundings(&page, &svg);
            assert_eq!(
                stub.recorded_request_methods(),
                ["GET", "GET", "POST"],
                "protected descendants must not be submitted for media signing"
            );
            let bodies = stub.recorded_request_bodies();
            let sign: serde_json::Value = serde_json::from_slice(&bodies[2]).expect("text request");
            let control = Arc::new(StubHttpClient::new());
            origin_answer(
                &control,
                article_with_protected_svg("").as_bytes(),
                "text/html; charset=utf-8",
            );
            text_answers(&control);
            let _ = served(
                &settings,
                &services_for(&control),
                page_request(Method::GET),
                streaming,
            );
            let control_bodies = control.recorded_request_bodies();
            let control_sign: serde_json::Value =
                serde_json::from_slice(&control_bodies[2]).expect("control text request");
            assert_eq!(
                sign["text"], control_sign["text"],
                "adding protected SVG must not alter the surrounding canonical article text"
            );
            assert!(!String::from_utf8_lossy(&bodies[2]).contains("ts-source-"));
        }
    }
}

use trusted_server_core::platform::{
    PlatformTemplateCache, PlatformTemplateCacheReservation, TemplateCacheError, TemplateCacheKey,
    TemplateCacheLookup, TemplateCacheMiss, TemplateCacheReservation, TemplateEntry,
    TemplateMetadata,
};

type TemplateEntries = Arc<std::sync::Mutex<std::collections::HashMap<String, TemplateEntry>>>;

#[derive(Default)]
struct SourceTemplateCache(TemplateEntries);

struct SourceTemplateReservation {
    entries: TemplateEntries,
    key: String,
}

impl PlatformTemplateCacheReservation for SourceTemplateReservation {
    fn insert(
        self: Box<Self>,
        metadata: &TemplateMetadata,
        body: Vec<u8>,
        _max_age: std::time::Duration,
    ) -> Result<(), TemplateCacheError> {
        self.entries.lock().expect("lock templates").insert(
            self.key.clone(),
            TemplateEntry {
                metadata: metadata.clone(),
                body,
            },
        );
        Ok(())
    }

    fn cancel(self: Box<Self>) -> Result<(), TemplateCacheError> {
        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformTemplateCache for SourceTemplateCache {
    async fn lookup_or_reserve(
        &self,
        key: &TemplateCacheKey,
    ) -> Result<TemplateCacheLookup, TemplateCacheError> {
        if let Ok(entry) = self.get(key).await {
            return Ok(TemplateCacheLookup::Hit(entry));
        }
        Ok(TemplateCacheLookup::Reserved(
            TemplateCacheReservation::new(Box::new(SourceTemplateReservation {
                entries: Arc::clone(&self.0),
                key: key.to_cache_key(),
            })),
        ))
    }

    async fn get(&self, key: &TemplateCacheKey) -> Result<TemplateEntry, TemplateCacheMiss> {
        self.0
            .lock()
            .expect("lock templates")
            .get(&key.to_cache_key())
            .cloned()
            .ok_or(TemplateCacheMiss::NotFound)
    }

    async fn put(
        &self,
        key: &TemplateCacheKey,
        metadata: &TemplateMetadata,
        body: Vec<u8>,
        _max_age: std::time::Duration,
    ) -> Result<(), TemplateCacheError> {
        self.0.lock().expect("lock templates").insert(
            key.to_cache_key(),
            TemplateEntry {
                metadata: metadata.clone(),
                body,
            },
        );
        Ok(())
    }

    async fn purge_url(&self, key: &TemplateCacheKey) -> Result<(), TemplateCacheError> {
        self.0
            .lock()
            .expect("lock templates")
            .remove(&key.to_cache_key());
        Ok(())
    }
}

#[test]
fn publisher_restores_before_template_publication_and_protects_warm_reader_copies() {
    for streaming in [false, true] {
        let settings = page_settings(&format!(
            "{}\n[creative_opportunities]\ngam_network_id = \"12345\"\nassembly_mode = \"esi\"\n",
            module_settings(&table(), true)
        ));
        let cache = Arc::new(SourceTemplateCache::default());
        let stub = Arc::new(StubHttpClient::new());
        let services = services_for(&stub).with_template_cache(cache.clone());
        let svg = protected_svg(true);
        origin_answer(
            &stub,
            article_with_protected_svg(&svg).as_bytes(),
            "text/html; charset=utf-8",
        );
        text_answers(&stub);
        let request = || {
            let mut request = page_request(Method::GET);
            request
                .headers_mut()
                .insert("sec-fetch-dest", "document".parse().expect("header"));
            request
                .headers_mut()
                .insert("sec-fetch-mode", "navigate".parse().expect("header"));
            request
        };
        let cold = served_response(&settings, &services, request(), streaming);
        assert_eq!(cold.headers()["x-ts-template-cache"], "miss-stored");
        assert_source_and_surroundings(std::str::from_utf8(&body_of(cold)).expect("UTF-8"), &svg);
        {
            let entries = cache.0.lock().expect("lock templates");
            assert_eq!(
                entries.len(),
                1,
                "must exercise real shared-template storage"
            );
            let template =
                String::from_utf8_lossy(&entries.values().next().expect("template").body);
            assert!(
                template.contains(&svg),
                "cache must contain restored original SVG"
            );
            assert!(!template.contains("<!--ts-source-"));
            assert!(
                !template.contains(MARKER),
                "reader signing must not enter shared templates"
            );
        }
        let calls = stub.recorded_request_methods();
        let warm = served_response(&settings, &services, request(), streaming);
        assert_eq!(warm.headers()["x-ts-template-cache"], "hit");
        assert_source_and_surroundings(std::str::from_utf8(&body_of(warm)).expect("UTF-8"), &svg);
        assert_eq!(
            stub.recorded_request_methods(),
            calls,
            "warm template and text plan must avoid origin/API I/O"
        );
    }
}

#[test]
fn protected_svg_nonce_refuses_shared_template_storage_after_restoration() {
    for streaming in [false, true] {
        let settings = page_settings(&format!(
            "{}\n[creative_opportunities]\ngam_network_id = \"12345\"\nassembly_mode = \"esi\"\n",
            module_settings(&table(), true)
        ));
        let cache = Arc::new(SourceTemplateCache::default());
        let stub = Arc::new(StubHttpClient::new());
        let services = services_for(&stub).with_template_cache(cache.clone());
        let svg = protected_svg(true).replace(
            "<text>https://origin.example.com/</text>",
            "<text nonce='per-response'>https://origin.example.com/</text>",
        );
        origin_answer(
            &stub,
            article_with_protected_svg(&svg).as_bytes(),
            "text/html; charset=utf-8",
        );
        text_answers(&stub);
        let mut request = page_request(Method::GET);
        request
            .headers_mut()
            .insert("sec-fetch-dest", "document".parse().expect("header"));
        request
            .headers_mut()
            .insert("sec-fetch-mode", "navigate".parse().expect("header"));
        let response = served_response(&settings, &services, request, streaming);
        assert_source_and_surroundings(
            std::str::from_utf8(&body_of(response)).expect("UTF-8"),
            &svg,
        );
        assert!(
            cache.0.lock().expect("cache").is_empty(),
            "response-bound nonce must not enter shared storage"
        );
    }
}

#[test]
fn protecting_signed_svg_keeps_unsigned_svg_normalization_and_signing() {
    let protected = "<svg xmlns:c2pa='http://c2pa.org/manifest' c2pa:manifest='source'/>";
    let unsigned = "<svg viewbox='0 0 2 2'><path d='M0 0'/></svg>";
    let normalized = svg_equivalence::signable_svg(unsigned, 8192).expect("equivalent SVG");
    let root_end = normalized.find('>').expect("root end") + 1;
    let signed = format!(
        "{}<metadata><c2pa:manifest xmlns:c2pa=\"http://c2pa.org/manifest\">QUJD</c2pa:manifest></metadata>{}",
        &normalized[..root_end],
        &normalized[root_end..]
    );
    let stub = Arc::new(StubHttpClient::new());
    let article = article_with_protected_svg(&format!("{protected}{unsigned}"));
    origin_answer(&stub, article.as_bytes(), "text/html; charset=utf-8");
    text_answers(&stub);
    live_policy(&stub);
    stub.push_response_with_headers(
        200,
        signed.as_bytes().to_vec(),
        media_answer_as(signed.as_bytes(), "image/svg+xml"),
    );
    let page = served(
        &placed(),
        &services_for(&stub),
        page_request(Method::GET),
        false,
    );
    assert_source_and_surroundings(&page, protected);
    assert!(
        page.contains(&signed),
        "unsigned SVG must still be normalized and signed"
    );
    let bodies = stub.recorded_request_bodies();
    assert_eq!(bodies.len(), 5);
    assert!(
        bodies[4]
            .windows(normalized.len())
            .any(|bytes| bytes == normalized.as_bytes())
    );
    assert!(
        !bodies[4]
            .windows(protected.len())
            .any(|bytes| bytes == protected.as_bytes())
    );
}

#[test]
fn publisher_refuses_an_oversized_protected_original_instead_of_serving_rewritten_svg() {
    for streaming in [false, true] {
        let mut settings = placed();
        settings.publisher.max_buffered_body_bytes = 1024;
        let stub = Arc::new(StubHttpClient::new());
        origin_answer(
            &stub,
            article_with_protected_svg(&protected_svg(false)).as_bytes(),
            "text/html; charset=utf-8",
        );
        let response = try_served_response(
            &settings,
            &services_for(&stub),
            page_request(Method::GET),
            streaming,
        );
        if let Ok(response) = response {
            assert!(
                block_on(response.into_body().into_bytes_bounded(1024 * 1024)).is_err(),
                "a lazy response must fail before publishing an altered protected SVG"
            );
        }
        assert_eq!(
            stub.recorded_request_methods(),
            ["GET"],
            "refuse before signing"
        );
    }
}
