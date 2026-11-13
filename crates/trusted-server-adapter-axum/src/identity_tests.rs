//! End-to-end identity-provider tests for the Axum adapter.
//!
//! Each test drives publisher navigations through the full router against a
//! local stub server bound to `127.0.0.1:0`. Requests to the fictional
//! `example.com` hosts are rewritten to the stub, so lookups go through the
//! real [`AxumPlatformHttpClient`] with its deadline handling.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use edgezero_core::http::{HeaderValue, Request, Response, Uri, header};
use edgezero_core::router::RouterService;
use error_stack::Report;
use trusted_server_core::ec::kv_types::KvEntry;
use trusted_server_core::geo::GeoInfo;
use trusted_server_core::platform::{
    ClientInfo, PlatformError, PlatformGeo, PlatformHttpClient, PlatformHttpRequest,
    PlatformPendingRequest, PlatformResponse, PlatformSelectResult, RuntimeServices,
    UnavailableKvStore,
};
use trusted_server_core::settings::Settings;

use crate::app::{AppState, build_router, build_state_with_services};
use crate::identity::build_ec_request_state;
use crate::platform::{
    AxumPlatformBackend, AxumPlatformConfigStore, AxumPlatformHttpClient, AxumPlatformSecretStore,
};

const PUBLISHER_URL: &str = "https://publisher.example.com/article";
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
const FIRST_PUBCID: &str = "11111111-1111-4111-8111-111111111111";
const SECOND_PUBCID: &str = "22222222-2222-4222-8222-222222222222";
const PARTNER_TOKEN: &str = "example-partner-token-for-identify-tests";

/// Local stub serving the publisher origin and the ID5 endpoint.
struct Stub {
    address: String,
    id5_requests: Arc<AtomicUsize>,
    origin_requests: Arc<AtomicUsize>,
}

async fn start_stub() -> Stub {
    let id5_requests = Arc::new(AtomicUsize::new(0));
    let origin_requests = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&id5_requests);
    let origin_counter = Arc::clone(&origin_requests);
    let app = axum::Router::new().fallback(move |uri: axum::http::Uri| {
        let counter = Arc::clone(&counter);
        let origin_counter = Arc::clone(&origin_counter);
        async move {
            if !uri.path().starts_with("/id5") {
                origin_counter.fetch_add(1, Ordering::SeqCst);
            }
            if uri.path().starts_with("/id5") {
                counter.fetch_add(1, Ordering::SeqCst);
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    serde_json::json!({
                        "universal_uid": "id5-example-uid",
                        "signature": "example-signature",
                    })
                    .to_string(),
                )
            } else {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/html")],
                    "<html><head></head><body>example</body></html>".to_owned(),
                )
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind the stub server");
    let address = listener
        .local_addr()
        .expect("should read the stub address")
        .to_string();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("should serve stub requests");
    });
    Stub {
        address,
        id5_requests,
        origin_requests,
    }
}

/// Sends every request to the stub, prefixing the path with `/id5` for the
/// ID5 host so the stub can tell the upstreams apart.
struct StubRoutingClient {
    inner: AxumPlatformHttpClient,
    address: String,
}

impl StubRoutingClient {
    fn rewrite(&self, mut request: PlatformHttpRequest) -> PlatformHttpRequest {
        let uri = request.request.uri();
        let prefix = if uri.host() == Some("id5.example.com") {
            "/id5"
        } else {
            ""
        };
        let path = uri.path_and_query().map_or("/", |path| path.as_str());
        let rewritten: Uri = format!("http://{}{prefix}{path}", self.address)
            .parse()
            .expect("should build the stub URI");
        *request.request.uri_mut() = rewritten;
        request
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformHttpClient for StubRoutingClient {
    async fn send(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        self.inner.send(self.rewrite(request)).await
    }

    async fn send_async(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
        self.inner.send_async(self.rewrite(request)).await
    }

    async fn select(
        &self,
        pending_requests: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>> {
        self.inner.select(pending_requests).await
    }

    async fn select_until(
        &self,
        pending_requests: Vec<PlatformPendingRequest>,
        deadline: web_time::Instant,
    ) -> Result<Option<PlatformSelectResult>, Report<PlatformError>> {
        self.inner.select_until(pending_requests, deadline).await
    }
}

struct FixedGeo {
    country: &'static str,
}

impl PlatformGeo for FixedGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(Some(GeoInfo {
            country: self.country.to_owned(),
            city: "Example City".to_owned(),
            continent: "Example".to_owned(),
            latitude: 0.0,
            longitude: 0.0,
            metro_code: 0,
            region: None,
            asn: None,
        }))
    }
}

fn services(stub: &Stub, country: &'static str) -> RuntimeServices {
    RuntimeServices::builder()
        .config_store(Arc::new(AxumPlatformConfigStore))
        .secret_store(Arc::new(AxumPlatformSecretStore))
        .kv_store(Arc::new(UnavailableKvStore))
        .backend(Arc::new(AxumPlatformBackend))
        .http_client(Arc::new(StubRoutingClient {
            inner: AxumPlatformHttpClient::new(),
            address: stub.address.clone(),
        }))
        .geo(Arc::new(FixedGeo { country }))
        .client_info(ClientInfo {
            client_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10))),
            ..ClientInfo::default()
        })
        .build()
}

fn settings(providers: &str) -> Settings {
    Settings::from_toml(&format!(
        r#"
        [[handlers]]
        path = "^/_ts/admin"
        username = "example-user"
        password = "example-password"

        [publisher]
        domain = "publisher.example.com"
        cookie_domain = ".publisher.example.com"
        origin_url = "https://origin.publisher.example.com"
        proxy_secret = "fictional-proxy-secret"

        [ec]
        passphrase = "fictional-passphrase-at-least-32-bytes"
        ec_store = "example_ec_store"

        [[ec.partners]]
        name = "pubcid.org"
        source_domain = "pubcid.org"
        bidstream_enabled = true
        api_token = "{PARTNER_TOKEN}"

        [[ec.partners]]
        name = "id5.example.com"
        source_domain = "id5.example.com"
        bidstream_enabled = true

        [[proxy.asset_routes]]
        prefix = "/assets/"
        origin_url = "https://assets.example.com"

        {providers}
        "#
    ))
    .expect("should parse identity test settings")
}

const SHAREDID_PROVIDER: &str = r#"
    [[ec.identity_providers]]
    id = "sharedid"
    kind = "sharedid"
"#;

fn id5_provider(collect: &str) -> String {
    format!(
        r#"
        [[ec.identity_providers]]
        id = "id5"
        kind = "id5"
        endpoint = "https://id5.example.com/gs/v2"
        allowed_domains = ["id5.example.com"]
        sources = ["id5.example.com"]
        collect = "{collect}"
        [ec.identity_providers.settings]
        partner_id = 1234
        "#
    )
}

struct Harness {
    state: Arc<AppState>,
    router: RouterService,
    stub: Stub,
}

async fn harness(providers: &str, country: &'static str) -> Harness {
    let stub = start_stub().await;
    let state = build_state_with_services(settings(providers), Some(services(&stub, country)))
        .expect("should build state with identity providers");
    let router = build_router(&state);
    Harness {
        state,
        router,
        stub,
    }
}

impl Harness {
    /// Sends a top-level document navigation and returns the `ts-ec` value
    /// it set, if any.
    async fn navigate(&self, cookie: &str) -> Option<String> {
        let request: Request = edgezero_core::http::request_builder()
            .method("GET")
            .uri(PUBLISHER_URL)
            .header(header::HOST, "publisher.example.com")
            .header(header::ACCEPT, "text/html")
            .header(header::USER_AGENT, BROWSER_UA)
            .header("sec-fetch-dest", "document")
            .header("sec-fetch-mode", "navigate")
            .header(header::COOKIE, cookie)
            .body(edgezero_core::body::Body::empty())
            .expect("should build navigation");
        let response = self
            .router
            .oneshot(request)
            .await
            .expect("should serve navigation");
        assert_eq!(response.status(), 200, "should serve the origin page");
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find_map(|value| value.strip_prefix("ts-ec="))
            .and_then(|value| value.split(';').next())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    }

    /// Sends `request` through the router.
    async fn send(&self, request: Request) -> Response {
        self.router
            .oneshot(request)
            .await
            .expect("should serve the request")
    }

    /// Mints an EC on a first navigation and returns it.
    async fn mint_ec(&self, consent_cookie: &str) -> String {
        self.navigate(consent_cookie)
            .await
            .expect("should mint an EC on the first navigation")
    }

    fn entry(&self, ec_id: &str) -> Option<KvEntry> {
        self.state
            .identity_graph()
            .expect("should have an identity graph")
            .get(ec_id)
            .expect("should read the identity graph")
            .map(|(entry, _)| entry)
    }

    fn generation(&self, ec_id: &str) -> u64 {
        self.state
            .identity_graph()
            .expect("should have an identity graph")
            .get(ec_id)
            .expect("should read the identity graph")
            .expect("should find the EC row")
            .1
    }

    fn stored_uids(&self, ec_id: &str) -> Vec<String> {
        self.entry(ec_id)
            .map(|entry| entry.ids.values().map(|id| id.uid.clone()).collect())
            .unwrap_or_default()
    }
}

/// Encodes a minimal TCF v2 TC string granting `purposes`.
fn tc_string(purposes: &[u8]) -> String {
    fn write_bits(buf: &mut [u8], offset: usize, width: usize, value: u64) {
        for i in 0..width {
            if (value >> (width - 1 - i)) & 1 == 1 {
                let bit = offset + i;
                buf[bit / 8] |= 1 << (7 - bit % 8);
            }
        }
    }
    // Core segment, empty vendor consent and legitimate-interest sections,
    // and no publisher restrictions.
    let mut buf = vec![0_u8; (213 + 17 + 17 + 12_usize).div_ceil(8)];
    let deciseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("should read the clock")
        .as_millis()
        / 100;
    let deciseconds = u64::try_from(deciseconds).expect("should fit in u64");
    write_bits(&mut buf, 0, 6, 2);
    write_bits(&mut buf, 6, 36, deciseconds);
    write_bits(&mut buf, 42, 36, deciseconds);
    write_bits(&mut buf, 78, 12, 7);
    write_bits(&mut buf, 90, 12, 1);
    write_bits(&mut buf, 102, 6, 1);
    write_bits(&mut buf, 108, 6, u64::from(b'E' - b'A'));
    write_bits(&mut buf, 114, 6, u64::from(b'N' - b'A'));
    write_bits(&mut buf, 120, 12, 42);
    write_bits(&mut buf, 132, 6, 2);
    for purpose in purposes {
        write_bits(&mut buf, 152 + usize::from(*purpose) - 1, 1, 1);
    }
    write_bits(&mut buf, 201, 6, u64::from(b'E' - b'A'));
    write_bits(&mut buf, 207, 6, u64::from(b'N' - b'A'));
    URL_SAFE_NO_PAD.encode(&buf)
}

fn consented() -> String {
    format!("euconsent-v2={}", tc_string(&[1, 2, 3, 4, 7, 9, 10]))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharedid_cookie_is_stored_and_replaced_by_a_later_value() {
    let harness = harness(SHAREDID_PROVIDER, "DE").await;
    let consent = consented();
    let ec_id = harness.mint_ec(&consent).await;

    harness
        .navigate(&format!("{consent}; ts-ec={ec_id}; _pubcid={FIRST_PUBCID}"))
        .await;
    let first = harness.stored_uids(&ec_id);
    harness
        .navigate(&format!(
            "{consent}; ts-ec={ec_id}; _pubcid={SECOND_PUBCID}"
        ))
        .await;
    let second = harness.stored_uids(&ec_id);

    assert_eq!(first, vec![FIRST_PUBCID], "should store the SharedID");
    assert_eq!(
        second,
        vec![SECOND_PUBCID],
        "should replace the stored SharedID with the cookie's new value"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharedid_cookie_is_stored_on_the_navigation_that_mints_the_ec() {
    let harness = harness(SHAREDID_PROVIDER, "DE").await;
    let consent = consented();

    let ec_id = harness
        .mint_ec(&format!("{consent}; _pubcid={FIRST_PUBCID}"))
        .await;

    assert_eq!(
        harness.stored_uids(&ec_id),
        vec![FIRST_PUBCID],
        "should store the SharedID in the row the minting navigation creates"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharedid_opt_out_removes_the_stored_id() {
    let harness = harness(SHAREDID_PROVIDER, "DE").await;
    let consent = consented();
    let ec_id = harness.mint_ec(&consent).await;
    harness
        .navigate(&format!("{consent}; ts-ec={ec_id}; _pubcid={FIRST_PUBCID}"))
        .await;
    assert_eq!(
        harness.stored_uids(&ec_id),
        vec![FIRST_PUBCID],
        "should store the SharedID before the opt-out"
    );

    harness
        .navigate(&format!(
            "{consent}; ts-ec={ec_id}; _pubcid={FIRST_PUBCID}; _pubcid_optout=1"
        ))
        .await;

    assert!(
        harness.stored_uids(&ec_id).is_empty(),
        "should remove the SharedID after an opt-out"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharedid_opt_out_prevents_storing() {
    let harness = harness(SHAREDID_PROVIDER, "DE").await;
    let consent = consented();
    let ec_id = harness.mint_ec(&consent).await;

    harness
        .navigate(&format!(
            "{consent}; ts-ec={ec_id}; _pubcid={FIRST_PUBCID}; _pubcid_optout=1"
        ))
        .await;

    assert!(
        harness.entry(&ec_id).is_some(),
        "should have written the EC row on the first navigation"
    );
    assert!(
        harness.stored_uids(&ec_id).is_empty(),
        "should not store a SharedID while opted out"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sharedid_is_not_stored_without_eid_consent() {
    let harness = harness(SHAREDID_PROVIDER, "DE").await;
    // Purpose 1 allows the EC; Purpose 4 is denied, so no EID may be kept.
    let consent = format!("euconsent-v2={}", tc_string(&[1]));
    let ec_id = harness.mint_ec(&consent).await;

    harness
        .navigate(&format!("{consent}; ts-ec={ec_id}; _pubcid={FIRST_PUBCID}"))
        .await;

    assert!(
        harness.entry(&ec_id).is_some(),
        "should have written the EC row on the first navigation"
    );
    assert!(
        harness.stored_uids(&ec_id).is_empty(),
        "should store no SharedID without Purpose 4 consent"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn page_phase_http_lookup_stores_the_vendor_id() {
    let harness = harness(&id5_provider("page"), "AU").await;

    // ID5 does not send the EC ID, so it runs on the minting navigation.
    let ec_id = harness.mint_ec("").await;

    assert_eq!(
        harness.stub.id5_requests.load(Ordering::SeqCst),
        1,
        "should call the vendor endpoint once"
    );
    assert_eq!(
        harness.stored_uids(&ec_id),
        vec!["id5-example-uid"],
        "should store the vendor ID in the row the minting navigation creates"
    );
    let minted_generation = harness.generation(&ec_id);

    harness.navigate(&format!("ts-ec={ec_id}")).await;

    assert_eq!(
        harness.stub.id5_requests.load(Ordering::SeqCst),
        1,
        "should not look up again while the stored ID is fresh"
    );
    assert_eq!(
        harness.generation(&ec_id),
        minted_generation,
        "should not rewrite an unchanged row"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_response_http_lookup_stores_the_vendor_id() {
    let harness = harness(&id5_provider("after_response"), "AU").await;
    let ec_id = harness.mint_ec("").await;

    harness.navigate(&format!("ts-ec={ec_id}")).await;

    let mut stored = Vec::new();
    for _ in 0..100 {
        stored = harness.stored_uids(&ec_id);
        if !stored.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        harness.stub.id5_requests.load(Ordering::SeqCst),
        1,
        "should call the vendor endpoint once after the response"
    );
    assert_eq!(
        stored,
        vec!["id5-example-uid"],
        "should store the vendor ID in the follow-up write"
    );
}

/// Builds a browser request with `cookie` for `method` and `path`.
fn browser_request(method: &str, path: &str, cookie: &str) -> Request {
    edgezero_core::http::request_builder()
        .method(method)
        .uri(format!("https://publisher.example.com{path}"))
        .header(header::HOST, "publisher.example.com")
        .header(header::USER_AGENT, BROWSER_UA)
        .header(header::COOKIE, cookie)
        .body(edgezero_core::body::Body::empty())
        .expect("should build browser request")
}

fn set_cookies(response: &Response) -> Vec<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::to_owned)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_cookie_header_records_a_setup_error() {
    let harness = harness(SHAREDID_PROVIDER, "DE").await;
    let mut request = browser_request("GET", "/article", "");
    request.headers_mut().insert(
        header::COOKIE,
        HeaderValue::from_bytes(b"ts-ec=\xff").expect("should build a non-UTF-8 cookie header"),
    );

    let state = build_ec_request_state(&harness.state, &services(&harness.stub, "DE"), &request);

    assert!(
        state.setup_error.is_some(),
        "should record the EC context error so dispatch short-circuits"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auction_route_records_a_consent_withdrawal() {
    let harness = harness(SHAREDID_PROVIDER, "DE").await;
    let ec_id = harness.mint_ec(&consented()).await;
    let withdrawn = format!("euconsent-v2={}", tc_string(&[]));

    let response = harness
        .send(browser_request(
            "POST",
            "/auction",
            &format!("{withdrawn}; ts-ec={ec_id}"),
        ))
        .await;

    assert!(
        set_cookies(&response)
            .iter()
            .any(|cookie| cookie.starts_with("ts-ec=;")
                || cookie.starts_with("ts-ec=\"\";")
                || (cookie.starts_with("ts-ec=") && cookie.contains("Max-Age=0"))),
        "should expire the EC cookie on the auction response, got {:?}",
        set_cookies(&response)
    );
    assert!(
        !harness
            .entry(&ec_id)
            .expect("should keep the withdrawal tombstone")
            .consent
            .ok,
        "should tombstone the EC row"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn identify_returns_the_stored_ids() {
    let harness = harness(SHAREDID_PROVIDER, "DE").await;
    let consent = consented();
    let ec_id = harness
        .mint_ec(&format!("{consent}; _pubcid={FIRST_PUBCID}"))
        .await;

    let mut request = browser_request(
        "GET",
        "/_ts/api/v1/identify",
        &format!("{consent}; ts-ec={ec_id}"),
    );
    request.headers_mut().insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {PARTNER_TOKEN}"))
            .expect("should build the bearer header"),
    );

    let response = harness.send(request).await;

    assert_eq!(response.status(), 200, "should serve identify");
    let body = String::from_utf8(
        response
            .into_body()
            .into_bytes()
            .expect("should buffer the identify body")
            .to_vec(),
    )
    .expect("should read a UTF-8 identify body");
    assert!(
        body.contains(FIRST_PUBCID),
        "should return the stored SharedID, got {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_sync_route_is_served_without_a_bearer_token() {
    let harness = harness(SHAREDID_PROVIDER, "DE").await;

    let response = harness
        .send(browser_request("POST", "/_ts/api/v1/batch-sync", ""))
        .await;

    assert_eq!(
        response.status(),
        401,
        "should reject batch sync without a bearer token instead of proxying it"
    );
    assert_eq!(
        harness.stub.origin_requests.load(Ordering::SeqCst),
        0,
        "should not proxy batch sync to the origin"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn asset_route_skips_ec_finalization() {
    let harness = harness(SHAREDID_PROVIDER, "DE").await;
    let mut request = browser_request("GET", "/assets/logo.png", &consented());
    request
        .headers_mut()
        .insert("sec-fetch-dest", HeaderValue::from_static("document"));
    request
        .headers_mut()
        .insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
    request
        .headers_mut()
        .insert(header::ACCEPT, HeaderValue::from_static("text/html"));

    let response = harness.send(request).await;

    assert_eq!(
        harness.stub.origin_requests.load(Ordering::SeqCst),
        1,
        "should proxy the asset to its origin"
    );
    assert!(
        set_cookies(&response).is_empty(),
        "should set no EC cookie on an asset response, got {:?}",
        set_cookies(&response)
    );
}
