//! The module's tests, which drive it through `AssetCacheModule::answer` as
//! core does, with `StubHttpClient` as the origin and `MemoryLoadingCache` as
//! the loading cache.

use std::sync::Arc;
use std::time::Duration;

use edgezero_core::body::Body as EdgeBody;
use http::{HeaderMap, HeaderName, HeaderValue, Response, StatusCode, Uri, header};
use trusted_server_core::asset_cache::{
    AssetAnswer, AssetCacheModule, AssetFile, AssetVia, OriginRequest,
};
use trusted_server_core::integrations::{IntegrationBuilder, IntegrationRegistry};
use trusted_server_core::platform::test_support::{
    MemoryLoadingCache, StubHttpClient, build_services_with_http_client,
};
use trusted_server_core::platform::{LoadingCaches, PlatformLoadingCache, RuntimeServices};
use trusted_server_core::settings::{LoadingCacheConfig, Settings};

use crate::entry::{AssetEntry, MAX_ASSET_COPY_BYTES};
use crate::{ASSETS_CACHE, ASSETS_VARY_CACHE, AssetCache, MODULE, builder};

/// The cache policy of a file the origin lets be kept for five minutes.
const KEPT_FOR_FIVE_MINUTES: &str = "public, max-age=300";

/// The most bytes a test reads of a response body.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// The host the reader asked.
const READER_HOST: &str = "www.example.com";

/// A stylesheet the publisher's origin sends.
const STYLESHEET_URL: &str = "https://origin.example.com/site.css";

/// A stylesheet an asset route fetches from its origin.
const ROUTE_STYLESHEET_URL: &str = "https://assets.example.com/.images/site.css";

/// What names the route's origin, being its origin URL and prefix.
const ROUTE_IDENTITY: &str = "https://assets.example.com\0/.images/";

const ORIGIN_BACKEND: &str = "origin-backend";

fn settings() -> Settings {
    trusted_server_core::test_support::tests::create_test_settings()
}

fn module() -> AssetCache {
    AssetCache::from_settings(&settings())
}

fn services(stub: &Arc<StubHttpClient>, cache: &Arc<MemoryLoadingCache>) -> RuntimeServices {
    build_services_with_http_client(Arc::clone(stub) as _)
        .with_loading_cache(Arc::clone(cache) as _)
}

fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in pairs {
        headers.append(
            HeaderName::from_static(name),
            HeaderValue::from_str(value).expect("should be a header value"),
        );
    }
    headers
}

/// The request core would send for `url`, carrying `request_headers`.
fn origin(url: &str, request_headers: HeaderMap, streaming: bool) -> OriginRequest {
    let uri = url.parse::<Uri>().expect("should parse the URL");
    OriginRequest::for_test(uri, request_headers, ORIGIN_BACKEND, streaming)
}

/// A file the publisher path offers for `url`, carrying `request_headers`.
fn publisher_file(url: &'static str, request_headers: HeaderMap) -> AssetFile<'static> {
    AssetFile::for_test(
        AssetVia::Publisher,
        READER_HOST,
        url,
        origin(url, request_headers, false),
    )
}

/// A file the asset route offers.
fn route_file() -> AssetFile<'static> {
    let mut file = AssetFile::for_test(
        AssetVia::Route,
        READER_HOST,
        ROUTE_STYLESHEET_URL,
        origin(ROUTE_STYLESHEET_URL, HeaderMap::new(), false),
    );
    file.origin_identity = ROUTE_IDENTITY;
    file
}

/// Queues the origin's answer, a `200` of `content_type` with `extra`.
fn queue(stub: &StubHttpClient, content_type: &str, body: &[u8], extra: &[(&str, &str)]) {
    let mut response_headers = vec![("content-type", content_type)];
    response_headers.extend_from_slice(extra);
    stub.push_response_with_headers(200, body.to_vec(), response_headers);
}

/// What the reader is served for `file`, as core serves it: the module's
/// answer, or the origin's response when the module passes the file back.
async fn serve(
    module: &AssetCache,
    services: &RuntimeServices,
    file: AssetFile<'_>,
) -> Result<Response<EdgeBody>, String> {
    let origin = file.origin.clone();
    match module.answer(services, file).await {
        AssetAnswer::Serve(response) => Ok(response),
        AssetAnswer::OriginFailed(report) => Err(format!("{report:?}")),
        AssetAnswer::AskOrigin => services
            .http_client()
            .send(origin.platform_request())
            .await
            .map(|sent| sent.response)
            .map_err(|report| format!("{report:?}")),
    }
}

async fn served_body(
    module: &AssetCache,
    services: &RuntimeServices,
    file: AssetFile<'_>,
) -> Vec<u8> {
    let response = serve(module, services, file)
        .await
        .expect("should serve the file");
    body_of(response).await
}

async fn body_of(response: Response<EdgeBody>) -> Vec<u8> {
    response
        .into_body()
        .into_bytes_bounded(MAX_BODY_BYTES)
        .await
        .expect("should read the body")
        .to_vec()
}

/// The time left to each copy `cache` holds, leaving out the records and the
/// marks of what may not be shared, in no order.
fn asset_copy_lifetimes(cache: &MemoryLoadingCache) -> Vec<Duration> {
    let copies = format!("/{ASSETS_CACHE}/");
    cache
        .keys()
        .iter()
        .filter(|key| key.contains(&copies))
        .filter_map(|key| cache.held(key))
        .filter(|(value, _)| matches!(AssetEntry::decode(value), Ok(AssetEntry::Copy(_))))
        .map(|(_, left)| left.unwrap_or(Duration::MAX))
        .collect()
}

#[tokio::test]
async fn an_image_is_kept_as_sent_and_one_fetch_serves_three_requests() {
    let stub = Arc::new(StubHttpClient::new());
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services(&stub, &cache);
    let module = module();
    let image = [0x89_u8, b'P', b'N', b'G', 0, 1, 2, 3];
    queue(
        &stub,
        "image/png",
        &image,
        &[("cache-control", KEPT_FOR_FIVE_MINUTES)],
    );

    for _ in 0..3 {
        let file = publisher_file("https://origin.example.com/a.png", HeaderMap::new());
        assert_eq!(served_body(&module, &services, file).await, image);
    }

    assert_eq!(
        stub.recorded_request_uris().len(),
        1,
        "one fetch serves all three"
    );
}

#[tokio::test]
async fn a_stylesheet_is_served_from_one_fetch_with_its_age() {
    let stub = Arc::new(StubHttpClient::new());
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services(&stub, &cache);
    let module = module();
    queue(
        &stub,
        "text/css",
        b"body{color:red}",
        &[("cache-control", KEPT_FOR_FIVE_MINUTES)],
    );

    let first = served_body(
        &module,
        &services,
        publisher_file(STYLESHEET_URL, HeaderMap::new()),
    )
    .await;
    let second = serve(
        &module,
        &services,
        publisher_file(STYLESHEET_URL, HeaderMap::new()),
    )
    .await
    .expect("should serve the copy");
    assert!(
        second.headers().contains_key(header::AGE),
        "a kept copy says how long it has been kept"
    );
    let second = body_of(second).await;

    assert_eq!(
        stub.recorded_request_uris().len(),
        1,
        "one fetch serves both"
    );
    assert_eq!(
        second, first,
        "the copy is what the first reader was served"
    );
}

#[tokio::test]
async fn a_wildcard_vary_a_cookie_vary_a_private_or_a_no_store_response_is_never_kept() {
    for (cache_control, vary, kept) in [
        (KEPT_FOR_FIVE_MINUTES, None, true),
        (KEPT_FOR_FIVE_MINUTES, Some("*"), false),
        (KEPT_FOR_FIVE_MINUTES, Some("Cookie"), false),
        ("private, max-age=300", None, false),
        ("no-store", None, false),
        ("public, max-age=300, no-store", None, false),
    ] {
        let stub = Arc::new(StubHttpClient::new());
        let cache = Arc::new(MemoryLoadingCache::default());
        let services = services(&stub, &cache);
        let module = module();
        for _ in 0..2 {
            let mut extra = vec![("cache-control", cache_control)];
            extra.extend(vary.map(|vary| ("vary", vary)));
            queue(&stub, "text/css", b"body{}", &extra);
            let file = publisher_file(STYLESHEET_URL, HeaderMap::new());
            assert_eq!(served_body(&module, &services, file).await, b"body{}");
        }

        assert_eq!(
            stub.recorded_request_uris().len(),
            if kept { 1 } else { 2 },
            "Cache-Control {cache_control} and Vary {vary:?}"
        );
        assert_eq!(
            asset_copy_lifetimes(&cache).is_empty(),
            !kept,
            "Cache-Control {cache_control} and Vary {vary:?}"
        );
    }
}

#[tokio::test]
async fn s_maxage_then_max_age_set_how_long_a_copy_is_kept() {
    for (cache_control, seconds) in [
        ("public, max-age=300", 300),
        ("public, max-age=300, s-maxage=60", 60),
        ("public, s-maxage=600, max-age=30", 600),
    ] {
        let stub = Arc::new(StubHttpClient::new());
        let cache = Arc::new(MemoryLoadingCache::default());
        let services = services(&stub, &cache);
        queue(
            &stub,
            "text/css",
            b"body{}",
            &[("cache-control", cache_control)],
        );

        let file = publisher_file(STYLESHEET_URL, HeaderMap::new());
        let _ = served_body(&module(), &services, file).await;

        let lifetimes = asset_copy_lifetimes(&cache);
        assert_eq!(lifetimes.len(), 1, "{cache_control}");
        let expected = Duration::from_secs(seconds);
        assert!(
            lifetimes[0] <= expected && lifetimes[0] > expected - Duration::from_secs(5),
            "{cache_control} should keep a copy for {expected:?}, not {:?}",
            lifetimes[0]
        );
    }
}

#[tokio::test]
async fn a_header_the_origin_varies_on_separates_kept_copies() {
    let stub = Arc::new(StubHttpClient::new());
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services(&stub, &cache);
    let module = module();
    for edition in ["red", "blue"] {
        queue(
            &stub,
            "text/css",
            format!("/* {edition} */").as_bytes(),
            &[
                ("cache-control", KEPT_FOR_FIVE_MINUTES),
                ("vary", "X-Edition"),
            ],
        );
    }

    for edition in ["red", "blue", "red", "blue"] {
        let file = publisher_file(STYLESHEET_URL, headers(&[("x-edition", edition)]));
        let css = served_body(&module, &services, file).await;
        assert_eq!(css, format!("/* {edition} */").into_bytes());
    }

    assert_eq!(
        stub.recorded_request_uris().len(),
        2,
        "each edition is fetched once"
    );
}

#[tokio::test]
async fn a_body_that_outgrows_a_copy_is_served_whole_from_the_one_fetch() {
    for streaming in [false, true] {
        let stub = Arc::new(StubHttpClient::new());
        stub.set_streaming_responses_supported(streaming);
        let cache = Arc::new(MemoryLoadingCache::default());
        let services = services(&stub, &cache);
        let large = vec![b'a'; MAX_ASSET_COPY_BYTES + 1];
        for _ in 0..2 {
            queue(
                &stub,
                "text/plain",
                &large,
                &[("cache-control", KEPT_FOR_FIVE_MINUTES)],
            );
        }
        let url = "https://origin.example.com/big.txt";
        let file = AssetFile::for_test(
            AssetVia::Publisher,
            READER_HOST,
            url,
            origin(url, HeaderMap::new(), streaming),
        );

        let body = served_body(&module(), &services, file).await;

        assert_eq!(
            body, large,
            "streaming {streaming}: the reader is served the whole body"
        );
        assert_eq!(
            stub.recorded_request_uris().len(),
            1,
            "streaming {streaming}: a body that outgrows a copy is served from the one fetch"
        );
        assert!(asset_copy_lifetimes(&cache).is_empty());
    }
}

#[tokio::test]
async fn a_response_whose_vary_changed_is_served_from_its_one_fetch() {
    let stub = Arc::new(StubHttpClient::new());
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services(&stub, &cache);
    let module = module();
    for (edition, vary) in [
        ("red", "X-Edition"),
        ("blue", "X-Region"),
        ("blue", "X-Region"),
    ] {
        queue(
            &stub,
            "text/css",
            format!("/* {edition} */").as_bytes(),
            &[("cache-control", KEPT_FOR_FIVE_MINUTES), ("vary", vary)],
        );
    }

    let red = publisher_file(STYLESHEET_URL, headers(&[("x-edition", "red")]));
    let blue = publisher_file(STYLESHEET_URL, headers(&[("x-edition", "blue")]));
    let red = served_body(&module, &services, red).await;
    let blue = served_body(&module, &services, blue).await;

    assert_eq!(red, b"/* red */");
    assert_eq!(
        blue, b"/* blue */",
        "the request whose fetch found the new Vary is served that fetch"
    );
    assert_eq!(
        stub.recorded_request_uris().len(),
        2,
        "each request fetches the origin once"
    );
}

#[tokio::test]
async fn a_load_that_fails_is_the_answer_and_the_next_request_loads_again() {
    let stub = Arc::new(StubHttpClient::new());
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services(&stub, &cache);
    let module = module();

    // Nothing is queued, so the stub fails the origin request.
    let answer = module
        .answer(&services, publisher_file(STYLESHEET_URL, HeaderMap::new()))
        .await;
    assert!(
        matches!(answer, AssetAnswer::OriginFailed(_)),
        "a failed load is the answer: {answer:?}"
    );
    assert_eq!(
        stub.recorded_request_uris().len(),
        1,
        "the origin is asked once"
    );
    assert!(cache.keys().is_empty(), "nothing is kept for a failed load");

    queue(
        &stub,
        "text/css",
        b"body{}",
        &[("cache-control", KEPT_FOR_FIVE_MINUTES)],
    );
    let file = publisher_file(STYLESHEET_URL, HeaderMap::new());
    assert_eq!(
        served_body(&module, &services, file).await,
        b"body{}",
        "the next request loads the file again"
    );
}

#[tokio::test]
async fn a_request_carrying_authorization_or_a_cookie_is_fetched_for_itself() {
    type Mark = fn(&mut AssetFile<'static>);
    let marks: [(&str, Mark); 2] = [
        ("Authorization", |file| file.carries_authorization = true),
        ("Cookie", |file| file.carries_cookie = true),
    ];
    for (carried, mark) in marks {
        let stub = Arc::new(StubHttpClient::new());
        let cache = Arc::new(MemoryLoadingCache::default());
        let services = services(&stub, &cache);
        let module = module();
        for reader in ["one", "two"] {
            queue(
                &stub,
                "application/json",
                format!("{{\"reader\":\"{reader}\"}}").as_bytes(),
                &[("cache-control", "max-age=300")],
            );
            let mut file = publisher_file(STYLESHEET_URL, HeaderMap::new());
            mark(&mut file);
            let origin = file.origin.clone();

            let answer = module.answer(&services, file).await;
            assert!(
                matches!(answer, AssetAnswer::AskOrigin),
                "a request carrying {carried} is passed back to core: {answer:?}"
            );
            let sent = services
                .http_client()
                .send(origin.platform_request())
                .await
                .expect("core asks the origin");
            assert_eq!(
                body_of(sent.response).await,
                format!("{{\"reader\":\"{reader}\"}}").into_bytes(),
                "reader {reader} is served the answer made for them"
            );
        }

        assert_eq!(
            stub.recorded_request_uris().len(),
            2,
            "each request carrying {carried} is fetched for itself"
        );
        assert!(
            asset_copy_lifetimes(&cache).is_empty(),
            "no answer to a request carrying {carried} is kept"
        );
    }
}

#[tokio::test]
async fn an_answer_that_sets_a_cookie_is_served_only_to_the_reader_it_was_made_for() {
    let stub = Arc::new(StubHttpClient::new());
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services(&stub, &cache);
    let module = module();
    for reader in ["one", "two"] {
        let cookie = format!("visitor={reader}");
        queue(
            &stub,
            "text/css",
            format!("/* {reader} */").as_bytes(),
            &[
                ("cache-control", KEPT_FOR_FIVE_MINUTES),
                ("set-cookie", cookie.as_str()),
            ],
        );
    }

    for reader in ["one", "two"] {
        let file = publisher_file(STYLESHEET_URL, HeaderMap::new());
        let response = serve(&module, &services, file)
            .await
            .expect("should serve the file");
        let cookie = format!("visitor={reader}");
        assert!(
            response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .any(|value| value.as_bytes() == cookie.as_bytes()),
            "reader {reader} is served the cookie the origin set for them"
        );
        assert_eq!(
            body_of(response).await,
            format!("/* {reader} */").into_bytes()
        );
    }

    assert_eq!(
        stub.recorded_request_uris().len(),
        2,
        "each reader's request is fetched for itself"
    );
    assert!(
        asset_copy_lifetimes(&cache).is_empty(),
        "an answer that sets a cookie is kept for no one"
    );
}

/// A loading cache module's cache, which keeps what it loads.
fn memory_module_cache() -> Arc<dyn PlatformLoadingCache> {
    Arc::new(MemoryLoadingCache::default())
}

/// A loading cache module, and this module, as a deployment links them.
fn linked_builders() -> [IntegrationBuilder; 2] {
    [
        IntegrationBuilder::implementations("loading-cache.memory", "a-crate")
            .with_loading_cache(memory_module_cache),
        builder(),
    ]
}

#[tokio::test]
async fn one_fetch_serves_two_requests_when_the_settings_name_no_loading_cache_module() {
    let caches =
        trusted_server_core::integrations::loading_caches_from(&settings(), &linked_builders())
            .expect("should select the module's caches");
    let stub = Arc::new(StubHttpClient::new());
    let services =
        build_services_with_http_client(Arc::clone(&stub) as _).with_loading_caches(caches);
    let module = module();
    queue(
        &stub,
        "text/css",
        b"body{}",
        &[("cache-control", KEPT_FOR_FIVE_MINUTES)],
    );

    for _ in 0..2 {
        let file = publisher_file(STYLESHEET_URL, HeaderMap::new());
        assert_eq!(served_body(&module, &services, file).await, b"body{}");
    }

    assert_eq!(
        stub.recorded_request_uris().len(),
        1,
        "both caches take the one loading cache module the build links"
    );
}

#[tokio::test]
async fn a_document_is_never_answered_from_a_copy() {
    let stub = Arc::new(StubHttpClient::new());
    queue(
        &stub,
        "text/css",
        b"body{}",
        &[("cache-control", KEPT_FOR_FIVE_MINUTES)],
    );
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services(&stub, &cache);
    let mut file = publisher_file(STYLESHEET_URL, HeaderMap::new());
    file.is_document = true;

    let answer = module().answer(&services, file).await;

    assert!(
        matches!(answer, AssetAnswer::AskOrigin),
        "a document is passed back, however shareable its origin's answer: {answer:?}"
    );
    assert!(
        stub.recorded_request_uris().is_empty(),
        "the module asks no origin for a document"
    );
}

fn whole_body_stub() -> Arc<StubHttpClient> {
    let stub = Arc::new(StubHttpClient::new());
    stub.set_streaming_responses_supported(false);
    stub
}

fn queue_route_stylesheet(stub: &StubHttpClient) {
    queue(
        stub,
        "text/css",
        b"body { color: red }",
        &[("cache-control", KEPT_FOR_FIVE_MINUTES)],
    );
}

#[tokio::test]
async fn a_routes_answer_is_held_and_serves_the_next_request() {
    let stub = whole_body_stub();
    queue_route_stylesheet(&stub);
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services(&stub, &cache);
    let module = module();

    let first = serve(&module, &services, route_file())
        .await
        .expect("should serve the file");
    let second = serve(&module, &services, route_file())
        .await
        .expect("should serve the copy");

    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        stub.recorded_request_uris().len(),
        1,
        "the origin is asked once for two requests of one file"
    );
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        second
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/css"),
        "the copy is served with the headers that describe it"
    );
    assert!(
        second.headers().contains_key(header::AGE),
        "the copy is served with an Age"
    );
    assert_eq!(body_of(second).await, b"body { color: red }");
    let lifetimes = asset_copy_lifetimes(&cache);
    assert!(
        lifetimes.len() == 1
            && lifetimes[0] > Duration::from_secs(295)
            && lifetimes[0] <= Duration::from_secs(300),
        "the copy is kept for the lifetime the origin gave it, less the time since: \
         {lifetimes:?}"
    );
}

#[tokio::test]
async fn a_routes_answer_that_sets_a_cookie_is_fetched_each_time() {
    let stub = whole_body_stub();
    for _ in 0..2 {
        queue(
            &stub,
            "text/css",
            b"body { color: red }",
            &[
                ("cache-control", KEPT_FOR_FIVE_MINUTES),
                ("set-cookie", "session=1"),
            ],
        );
    }
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services(&stub, &cache);
    let module = module();

    for _ in 0..2 {
        let response = serve(&module, &services, route_file())
            .await
            .expect("should serve the file");
        assert_eq!(response.status(), StatusCode::OK);
    }

    assert_eq!(stub.recorded_request_uris().len(), 2);
    assert!(asset_copy_lifetimes(&cache).is_empty(), "no copy is kept");
}

#[tokio::test]
async fn a_readers_cookie_does_not_keep_a_routes_answer_from_a_copy() {
    let stub = whole_body_stub();
    queue_route_stylesheet(&stub);
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services(&stub, &cache);
    let module = module();

    for _ in 0..2 {
        let mut file = route_file();
        file.carries_cookie = true;
        let response = serve(&module, &services, file)
            .await
            .expect("should serve the file");
        assert_eq!(response.status(), StatusCode::OK);
    }

    assert_eq!(
        stub.recorded_request_uris().len(),
        1,
        "a route sends its origin no cookie, so both readers share one fetch"
    );
}

#[tokio::test]
async fn a_routes_file_that_asks_for_the_origin_is_fetched() {
    let stub = whole_body_stub();
    queue_route_stylesheet(&stub);
    queue_route_stylesheet(&stub);
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services(&stub, &cache);
    let module = module();

    let _ = served_body(&module, &services, route_file()).await;
    let mut asking = route_file();
    asking.asks_for_origin = true;
    let answer = module.answer(&services, asking).await;

    assert!(
        matches!(answer, AssetAnswer::AskOrigin),
        "a range, a conditional header, or a Cache-Control or Pragma that asks \
         for the origin is passed back to core: {answer:?}"
    );
    assert_eq!(stub.recorded_request_uris().len(), 1);
}

#[tokio::test]
async fn nothing_is_held_while_the_caches_keep_no_values() {
    let stub = whole_body_stub();
    queue_route_stylesheet(&stub);
    queue_route_stylesheet(&stub);
    let services = build_services_with_http_client(Arc::clone(&stub) as _);
    let module = module();

    for _ in 0..2 {
        let answer = module.answer(&services, route_file()).await;
        assert!(
            matches!(answer, AssetAnswer::AskOrigin),
            "a module whose caches keep nothing passes every file back: {answer:?}"
        );
    }

    assert!(
        stub.recorded_request_uris().is_empty(),
        "the module asks no origin itself"
    );
}

#[tokio::test]
async fn a_routes_load_that_fails_is_the_answer_after_one_origin_request() {
    let stub = whole_body_stub();
    let cache = Arc::new(MemoryLoadingCache::default());
    let services = services(&stub, &cache);

    // Nothing is queued, so the stub fails the origin request.
    let answer = module().answer(&services, route_file()).await;

    assert!(
        matches!(answer, AssetAnswer::OriginFailed(_)),
        "a failed load is the answer: {answer:?}"
    );
    assert_eq!(stub.recorded_request_uris().len(), 1);
}

#[test]
fn the_builder_declares_the_two_caches() {
    let builder = builder();

    assert_eq!(builder.module_name(), Some(MODULE));
    assert_eq!(builder.cache_features(), [ASSETS_CACHE, ASSETS_VARY_CACHE]);
}

#[test]
fn the_assets_table_sizes_both_caches_and_the_defaults_fill_what_it_leaves_unset() {
    let mut settings = settings();
    settings.cache.declared.insert(
        ASSETS_CACHE.to_owned(),
        LoadingCacheConfig {
            store_seconds: Some(600),
            ..LoadingCacheConfig::default()
        },
    );

    let module = AssetCache::from_settings(&settings);

    for spec in [&module.caches.records, &module.caches.copies] {
        assert_eq!(
            spec.lifetimes.store,
            Duration::from_secs(600),
            "{}",
            spec.name
        );
        assert_eq!(
            spec.lifetimes.memory,
            Duration::from_secs(5),
            "{}",
            spec.name
        );
        assert_eq!(
            spec.lifetimes.store_idle,
            Duration::from_secs(24 * 60 * 60),
            "{}",
            spec.name
        );
    }
    assert_eq!(module.caches.records.name, ASSETS_VARY_CACHE);
    assert_eq!(module.caches.copies.name, ASSETS_CACHE);
}

/// Guards core's check of the lifetimes in a declared cache's table, for the
/// table this module's builder declares.
#[test]
fn core_refuses_a_lifetime_out_of_range_in_the_assets_table() {
    let toml = format!(
        "{}\n[cache.51degrees-assets]\nstore_seconds = 0\n",
        trusted_server_core::test_support::tests::crate_test_settings_str()
    );

    let error = Settings::from_toml(&toml).expect_err("a lifetime out of range should not load");

    let message = format!("{error:?}");
    assert!(
        message.contains(
            "[cache.51degrees-assets] store_seconds is 0, and must be from 1 to 31536000"
        ),
        "{message}"
    );
}

/// The test settings with `tables` after them, loaded from a document.
fn settings_with_tables(tables: &str) -> Settings {
    let toml = format!(
        "{}\n{tables}",
        trusted_server_core::test_support::tests::crate_test_settings_str()
    );
    Settings::from_toml(&toml).expect("should load the document")
}

/// The records take the lifetimes of `[cache.51degrees-assets]`, so a
/// lifetime in `[cache.51degrees-assets-vary]` is refused when the settings
/// load, whether or not `[asset-cache]` selects the module.
#[test]
fn each_lifetime_in_the_vary_table_is_refused_naming_the_table_and_the_setting() {
    // (setting, a value in its range)
    let lifetimes = [
        ("memory_seconds", "5"),
        ("store_seconds", "3600"),
        ("store_idle_seconds", "86400"),
    ];
    for (setting, value) in lifetimes {
        for selected in [false, true] {
            let mut settings = settings_with_tables(&format!(
                "[cache.51degrees-assets-vary]\n{setting} = {value}\n"
            ));
            if selected {
                settings.select_module("asset-cache", MODULE);
            }

            let mut refusals = vec![
                (
                    "loading the settings",
                    trusted_server_core::config::validate_settings_for_runtime_with(
                        &settings,
                        &[builder()],
                    )
                    .err(),
                ),
                (
                    "the deploy check",
                    trusted_server_core::config::validate_settings_for_deploy_with(
                        &settings,
                        &[builder()],
                    )
                    .err(),
                ),
            ];
            // The registry builds only a module a section selects.
            if selected {
                refusals.push((
                    "the registry",
                    IntegrationRegistry::with_registrations(&settings, &[builder()]).err(),
                ));
            }

            for (call, refusal) in refusals {
                let message = format!(
                    "{:?}",
                    refusal.unwrap_or_else(|| panic!(
                        "{call} should refuse `{setting}`, selected {selected}"
                    ))
                );
                assert!(
                    message.contains(&format!(
                        "[cache.51degrees-assets-vary] sets `{setting}`, which"
                    )) && message.contains("[cache.51degrees-assets]."),
                    "{call} should name the table, the setting and where lifetimes go, \
                     selected {selected}: {message}"
                );
                for (other, _) in lifetimes.iter().filter(|(other, _)| *other != setting) {
                    assert!(
                        !message.contains(&format!("`{other}`")),
                        "{call} should name only `{setting}`: {message}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_vary_table_refusal_names_every_lifetime_it_sets() {
    let settings = settings_with_tables(
        "[cache.51degrees-assets-vary]\nmemory_seconds = 5\nstore_seconds = 3600\n\
         store_idle_seconds = 86400\n",
    );

    let error = crate::validate(&settings).expect_err("no lifetime of the table is read");

    let message = format!("{error:?}");
    assert!(
        message.contains(
            "[cache.51degrees-assets-vary] sets `memory_seconds`, `store_seconds` and \
             `store_idle_seconds`, which"
        ),
        "{message}"
    );
}

/// A vary table that sets no lifetime loads, empty or naming its loading
/// cache module, beside an assets table that sets all three.
#[test]
fn a_vary_table_without_lifetimes_loads() {
    for vary in ["", "module = \"memory\"\n"] {
        let mut settings = settings_with_tables(&format!(
            "[cache.51degrees-assets]\nmemory_seconds = 5\nstore_seconds = 3600\n\
             store_idle_seconds = 86400\n\n[cache.51degrees-assets-vary]\n{vary}"
        ));
        settings.select_module("asset-cache", MODULE);
        let builders = linked_builders();

        assert!(
            settings.cache.declared.contains_key(ASSETS_VARY_CACHE),
            "the document should hold the vary table: {vary:?}"
        );
        trusted_server_core::config::validate_settings_for_runtime_with(&settings, &builders)
            .expect("loading the settings should accept the table");
        trusted_server_core::config::validate_settings_for_deploy_with(&settings, &builders)
            .expect("the deploy check should accept the table");
        let registry = IntegrationRegistry::with_registrations(&settings, &[builder()])
            .expect("the registry should accept the table");
        assert!(registry.asset_cache_module().is_some(), "{vary:?}");
        trusted_server_core::integrations::loading_caches_from(&settings, &builders)
            .expect("the caches should be made");
    }
}

#[tokio::test]
async fn a_record_cache_that_keeps_nothing_beside_one_that_does_has_the_origin_asked() {
    let stub = Arc::new(StubHttpClient::new());
    let mut caches = LoadingCaches::default();
    caches.insert(ASSETS_CACHE, memory_module_cache());
    let services = build_services_with_http_client(Arc::clone(&stub) as _)
        .with_loading_caches(Arc::new(caches));

    let answer = module()
        .answer(&services, publisher_file(STYLESHEET_URL, HeaderMap::new()))
        .await;

    assert!(
        matches!(answer, AssetAnswer::AskOrigin),
        "every request would run the record's load, so none is saved: {answer:?}"
    );
    assert!(stub.recorded_request_uris().is_empty());
}

#[test]
fn an_assets_table_naming_none_is_refused() {
    let mut settings = settings();
    settings.cache.declared.insert(
        ASSETS_CACHE.to_owned(),
        LoadingCacheConfig {
            module: Some("none".to_owned()),
            ..LoadingCacheConfig::default()
        },
    );

    let error =
        trusted_server_core::integrations::loading_caches_from(&settings, &linked_builders())
            .expect_err("no linked crate offers a loading cache named `none`");

    let message = format!("{error:?}");
    assert!(
        message.contains("[cache.51degrees-assets] module names `none`"),
        "{message}"
    );
}

#[test]
fn a_table_of_the_modules_own_is_refused_even_when_empty() {
    for table in [serde_json::json!({}), serde_json::json!({ "typo": true })] {
        let mut settings = settings();
        settings
            .insert_module_config("asset-cache", MODULE, &table)
            .expect("should write the table");

        let checked = crate::validate(&settings).expect_err("the module reads no table");
        let built = IntegrationRegistry::with_registrations(&settings, &[builder()])
            .err()
            .expect("the module reads no table");

        for message in [format!("{checked:?}"), format!("{built:?}")] {
            assert!(
                message.contains("[asset-cache.51degrees] is a table the module does not read"),
                "{table}: {message}"
            );
        }
    }
}

#[test]
fn asset_cache_selects_the_module() {
    let mut selected = settings();
    selected.select_module("asset-cache", MODULE);

    let registry = IntegrationRegistry::with_registrations(&selected, &[builder()])
        .expect("should build the registry");
    let unselected = IntegrationRegistry::with_registrations(&settings(), &[builder()])
        .expect("should build the registry");

    assert!(registry.asset_cache_module().is_some());
    assert!(
        unselected.asset_cache_module().is_none(),
        "a linked module holds nothing until [asset-cache] selects it"
    );
}

#[test]
fn module_constant_is_the_crate_folder() {
    assert_eq!(
        MODULE,
        trusted_server_core::module_name!(),
        "should be named by the folder this crate lives in"
    );
}
