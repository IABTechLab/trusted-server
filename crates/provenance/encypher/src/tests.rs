use super::*;

use std::sync::Arc;

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
fn a_selected_and_placed_module_registers_its_middleware() {
    let settings = placed();

    assert!(validate(&settings).expect("should validate"));
    assert!(
        register(&settings).expect("should register").is_some(),
        "a selected module with a worker token should register"
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
    let defaults = parsed(&config_value()).expect("should parse defaults");
    assert_eq!(defaults.policy_ttl_seconds, 300);
    assert_eq!(defaults.plan_ttl_seconds, 604_800);
    assert_eq!(defaults.max_hold_ms, 5_000);
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

fn page_request(method: Method) -> Request<EdgeBody> {
    Request::builder()
        .method(method)
        .uri("https://publisher.example.com/news/harbor-vote?utm_source=feed")
        .header(header::HOST, "publisher.example.com")
        .body(EdgeBody::empty())
        .expect("should build the page request")
}

/// The page body a reader receives from the publisher path, through the
/// buffered or the streaming finalizer.
fn served(
    settings: &Settings,
    services: &RuntimeServices,
    request: Request<EdgeBody>,
    streaming: bool,
) -> String {
    let registry = IntegrationRegistry::with_registrations(settings, &[builder()])
        .expect("should build the registry");
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
        .await
        .expect("should fetch the page");
        let response = if streaming {
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
        .expect("should finish the page");
        let body = response
            .into_body()
            .into_bytes_bounded(16 * 1024 * 1024)
            .await
            .expect("should read the body");
        String::from_utf8(body.to_vec()).expect("should be UTF-8")
    })
}

fn services_for(stub: &Arc<StubHttpClient>) -> RuntimeServices {
    build_services_with_http_client(Arc::clone(stub) as Arc<dyn PlatformHttpClient>)
        .with_kv_store(Arc::new(MemoryKv::default()))
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
