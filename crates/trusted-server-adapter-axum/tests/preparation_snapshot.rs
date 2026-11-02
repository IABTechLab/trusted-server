//! The Encypher preparation routes' source reads over the dev server's real
//! HTTP client, with the source body held open.
//!
//! Each test builds the full application router from settings, so the
//! snapshot and publication routes run as deployed and send their loopback
//! reads through `AxumPlatformHttpClient` over a real socket. A local origin
//! stands in for the reader pipeline at the configured loopback address: it
//! answers with origin facts authenticated for the read's nonce, exactly as
//! the module's media hook writes them, and then writes no more of the body
//! until the client leaves.
//!
//! A route that collects the whole source before deciding would wait on
//! that body forever. Each answer must instead arrive while the body is
//! still open, and the unfinished read must then be dropped. No test
//! allocates more than a few kilobytes of source, whatever length the
//! origin declares.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use edgezero_core::body::Body as EdgeBody;
use edgezero_core::http::{Request, Response, request_builder};
use edgezero_core::router::RouterService;
use edgezero_core::store_registry::KvRegistry;
use serde_json::{Value, json};
use trusted_server_adapter_axum::app::TrustedServerApp;
use trusted_server_adapter_axum::platform::open_kv_handle;
use trusted_server_core::settings::Settings;
use trusted_server_core::test_support::tests::crate_test_settings_str;
use trusted_server_provenance_encypher::preparation::pipeline::{
    PREPARED_PROTOCOL, PlannedMedia, PrepareRequest, PreparedSourceRef, ProbeRelease,
    ProbeSelection, StructureChunk,
};
use trusted_server_provenance_encypher::preparation::protocol::{
    CAPABILITY_HEADER, EnvelopeItem, FACTS_HEADER, LargeLocator, PREPARATION_PROTOCOL,
    RESULTS_PREFIX, ResultEnvelope, SNAPSHOT_META_HEADER, SNAPSHOT_PATH, SnapshotMeta,
    SnapshotMode, SnapshotOrigin, SnapshotRequest, TOKEN_HEADER, Target, credential_context,
    facts_header,
};

const HOST: &str = "test-publisher.com";
const API_BASE_URL: &str = "https://api.example.com";
const WORKER_TOKEN: &str = "cwk_test_worker_token_value";
const PUBLICATION: &str = "publication-secret-for-tests";
const NONCE: &str = "0123456789abcdef0123456789abcdef";
const SOURCE_PATH: &str = "/media/ferry.mp4";
const ORIGIN_HOST: &str = "origin.test-publisher.com";
/// SHA-256 of core's configured-origin identity for the media route,
/// `"https://origin.test-publisher.com\0/media/"`, as the reader
/// pipeline's facts report it.
const ORIGIN_IDENTITY_SHA256: &str =
    "81b7719358847724cc3023237b51295905f20fe6bb6047494fde364673764afd";
const ETAG: &str = "\"v1\"";
/// The snapshot bound these settings give: the larger of the document and
/// media limits.
const SNAPSHOT_LIMIT: usize = 4096;
/// A declared source length far past every bound, never sent.
const DECLARED_LENGTH: u64 = 1_073_741_824;
/// How long an answer may take while the source body is held open.
const PROMPT: Duration = Duration::from_secs(5);

// --- the held origin -------------------------------------------------------

type Answer = dyn Fn(&str) -> Vec<u8> + Send + Sync;

/// A local origin that writes each request's answer and then holds the
/// connection open, its body unfinished, until the client leaves.
struct HeldOrigin {
    address: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    reader_left: Arc<AtomicBool>,
}

impl HeldOrigin {
    fn start(answer: impl Fn(&str) -> Vec<u8> + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("should bind a local origin");
        let address = listener
            .local_addr()
            .expect("should read the local origin's address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let reader_left = Arc::new(AtomicBool::new(false));
        let answer: Arc<Answer> = Arc::new(answer);
        let accepted_requests = Arc::clone(&requests);
        let accepted_left = Arc::clone(&reader_left);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    return;
                };
                let answer = Arc::clone(&answer);
                let requests = Arc::clone(&accepted_requests);
                let reader_left = Arc::clone(&accepted_left);
                std::thread::spawn(move || {
                    answer_and_hold(stream, answer.as_ref(), &requests, &reader_left);
                });
            }
        });
        Self {
            address,
            requests,
            reader_left,
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .expect("should read the recorded requests")
            .clone()
    }

    /// Whether the client closed its connection within [`PROMPT`].
    async fn reader_left(&self) -> bool {
        tokio::time::timeout(PROMPT, async {
            while !self.reader_left.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .is_ok()
    }
}

/// Reads one request head, writes its answer, then waits for the client to
/// close the connection without writing anything more.
fn answer_and_hold(
    mut stream: TcpStream,
    answer: &Answer,
    requests: &Mutex<Vec<String>>,
    reader_left: &AtomicBool,
) {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 || !matches!(stream.read(&mut byte), Ok(1)) {
            return;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let response = answer(&head);
    requests
        .lock()
        .expect("should record the request")
        .push(head);
    if stream.write_all(&response).is_ok() && stream.flush().is_ok() {
        let mut scratch = [0_u8; 1024];
        while matches!(stream.read(&mut scratch), Ok(read) if read > 0) {}
    }
    reader_left.store(true, Ordering::SeqCst);
}

/// The value of header `name` in a raw request head.
fn header_in(head: &str, name: &str) -> Option<String> {
    head.lines().skip(1).find_map(|line| {
        let (found, value) = line.split_once(':')?;
        found
            .trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    })
}

/// A response head with `status` and `headers`.
fn response_head(status: &str, headers: &[(&str, String)]) -> Vec<u8> {
    let mut head = String::from("HTTP/1.1 ");
    head.push_str(status);
    head.push_str("\r\n");
    for (name, value) in headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    head.into_bytes()
}

/// The origin facts the reader pipeline reports for a public source.
fn public_facts(status: u16) -> SnapshotOrigin {
    SnapshotOrigin {
        status,
        origin_host: ORIGIN_HOST.to_owned(),
        origin_identity_sha256: ORIGIN_IDENTITY_SHA256.to_owned(),
        ..SnapshotOrigin::default()
    }
}

/// The facts header a read carries.
enum Facts {
    /// Authenticated for the read's nonce under the publication secret.
    Genuine(SnapshotOrigin),
    /// Written by someone without the publication secret.
    Forged(SnapshotOrigin),
    /// None at all: the module did not observe the read.
    Missing,
}

/// A held media answer's headers: its type and validator, the facts and
/// `extra`.
fn media_headers<'a>(
    facts: &Facts,
    nonce: &str,
    extra: &[(&'a str, String)],
) -> Vec<(&'a str, String)> {
    let mut headers = vec![
        ("content-type", "video/mp4".to_owned()),
        ("etag", ETAG.to_owned()),
    ];
    match facts {
        Facts::Genuine(origin) => {
            headers.push((FACTS_HEADER, facts_header(PUBLICATION, nonce, origin)));
        }
        Facts::Forged(origin) => headers.push((
            FACTS_HEADER,
            facts_header("not-the-publication-secret", nonce, origin),
        )),
        Facts::Missing => {}
    }
    headers.extend_from_slice(extra);
    headers
}

/// One chunk of a chunked body carrying `bytes`, with no final chunk after
/// it, so the body never ends.
fn unfinished_chunked_body(bytes: &[u8]) -> Vec<u8> {
    let length = bytes.len();
    let mut body = format!("{length:x}\r\n").into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n");
    body
}

/// A `200` media answer declaring [`DECLARED_LENGTH`], with only its first
/// sixteen bytes written.
fn declared_oversized_answer(facts: Facts) -> impl Fn(&str) -> Vec<u8> + Send + Sync + 'static {
    move |_| {
        let mut answer = response_head(
            "200 OK",
            &media_headers(
                &facts,
                NONCE,
                &[("content-length", DECLARED_LENGTH.to_string())],
            ),
        );
        answer.extend_from_slice(&[7_u8; 16]);
        answer
    }
}

// --- the application -------------------------------------------------------

/// The asynchronous mode's settings, reading snapshots from `loopback`.
fn settings(loopback: SocketAddr) -> Settings {
    Settings::from_toml(&format!(
        "{base}\n[provenance]\nmodule = \"encypher\"\n\n[provenance.encypher]\n\
         api_base_url = \"{API_BASE_URL}\"\ndomain = \"{HOST}\"\n\
         worker_token = \"{WORKER_TOKEN}\"\nexecution = \"async_prepared\"\n\
         max_document_bytes = {SNAPSHOT_LIMIT}\nmax_media_bytes = {SNAPSHOT_LIMIT}\n\
         max_large_media_bytes = 16777216\n\n\
         [provenance.encypher.preparation]\n\
         worker_url = \"https://prep.{HOST}\"\n\
         loopback_url = \"http://{loopback}\"\ndeployment_id = \"test\"\n\
         public_host = \"{HOST}\"\nsignal_secret = \"signal-secret-for-tests\"\n\
         publication_secret = \"{PUBLICATION}\"\nroutes = [\"/media/\"]\n\n\
         [[serve]]\nmedia_type = \"text/html\"\nmiddleware = [\"provenance.encypher\"]\n",
        base = crate_test_settings_str(),
    ))
    .expect("should load the asynchronous prepared settings")
    // A loaded domain carries its policy, and this test decides under the
    // repository's sample.
    .with_permission_policy(trusted_server_core::permissions::PermissionPolicy::sample())
}

fn router(origin: &HeldOrigin) -> RouterService {
    TrustedServerApp::routes_with_settings(settings(origin.address))
        .expect("should build the application router")
}

/// `router`'s answer to `request`, which must arrive within [`PROMPT`].
async fn answer_within(router: &RouterService, request: Request) -> Response {
    tokio::time::timeout(PROMPT, router.oneshot(request))
        .await
        .expect("should answer while the source body is still open")
        .expect("the router should answer")
}

/// The worker's snapshot request for the source, in `mode`.
fn snapshot_request(mode: SnapshotMode) -> Request {
    let body = serde_json::to_vec(&SnapshotRequest {
        protocol: PREPARATION_PROTOCOL.to_owned(),
        deployment_id: "test".to_owned(),
        target: Target {
            path: SOURCE_PATH.to_owned(),
            variant: String::new(),
        },
        nonce: NONCE.to_owned(),
        read: mode,
    })
    .expect("should encode the snapshot request");
    request_builder()
        .method("POST")
        .uri(format!("https://{HOST}{SNAPSHOT_PATH}"))
        .header("host", HOST)
        .header(TOKEN_HEADER, PUBLICATION)
        .body(EdgeBody::from(body))
        .expect("should build the snapshot request")
}

async fn snapshot(origin: &HeldOrigin, mode: SnapshotMode) -> Response {
    answer_within(&router(origin), snapshot_request(mode)).await
}

async fn body_of(response: Response) -> Vec<u8> {
    response
        .into_body()
        .into_bytes_bounded(1024 * 1024)
        .await
        .expect("should read the route's answer")
        .to_vec()
}

/// The status and refusal code of `response`.
async fn refusal(response: Response) -> (u16, String) {
    let status = response.status().as_u16();
    let body: Value =
        serde_json::from_slice(&body_of(response).await).expect("a refusal should be JSON");
    let code = body["error"]["code"]
        .as_str()
        .expect("a refusal should name its code")
        .to_owned();
    (status, code)
}

fn snapshot_meta(response: &Response) -> SnapshotMeta {
    response
        .headers()
        .get(SNAPSHOT_META_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| STANDARD.decode(value).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .expect("should report the snapshot's metadata")
}

// --- snapshots -------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_whole_snapshot_declared_past_its_bound_is_refused_while_the_body_is_open() {
    let origin = HeldOrigin::start(declared_oversized_answer(Facts::Genuine(public_facts(200))));

    let response = snapshot(&origin, SnapshotMode::Whole).await;

    assert_eq!(
        refusal(response).await,
        (413, "snapshot_too_large".to_owned()),
        "a large public source should be refused as too large, which sends the worker to its \
         head-only fallback, rather than fail the read"
    );
    assert!(
        origin.reader_left().await,
        "the snapshot should drop its unfinished source read"
    );
    assert_eq!(origin.requests().len(), 1, "should read the source once");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_whole_snapshot_of_unknown_length_is_refused_once_it_passes_its_bound() {
    let body = unfinished_chunked_body(&[b'x'; SNAPSHOT_LIMIT + 1]);
    let origin = HeldOrigin::start(move |_| {
        let mut answer = response_head(
            "200 OK",
            &media_headers(
                &Facts::Genuine(public_facts(200)),
                NONCE,
                &[("transfer-encoding", "chunked".to_owned())],
            ),
        );
        // One chunk past the bound, and never the final chunk.
        answer.extend_from_slice(&body);
        answer
    });

    let response = snapshot(&origin, SnapshotMode::Whole).await;

    assert_eq!(
        refusal(response).await,
        (413, "snapshot_too_large".to_owned()),
        "a body without a declared length should be refused as soon as it passes the bound"
    );
    assert!(
        origin.reader_left().await,
        "the snapshot should drop its unfinished source read"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_head_snapshot_reports_a_large_source_while_its_body_is_open() {
    let origin = HeldOrigin::start(declared_oversized_answer(Facts::Genuine(public_facts(200))));

    let response = snapshot(&origin, SnapshotMode::Head).await;

    assert_eq!(response.status().as_u16(), 200, "should answer the head");
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("private, no-store"),
        "the snapshot's own transport should stay private"
    );
    let meta = snapshot_meta(&response);
    assert_eq!(meta.page_url, format!("https://{HOST}{SOURCE_PATH}"));
    assert_eq!(meta.content_type, "video/mp4");
    assert_eq!(meta.etag.as_deref(), Some(ETAG));
    assert_eq!(
        meta.length,
        Some(DECLARED_LENGTH),
        "should report the declared length without reading the body"
    );
    assert_eq!(
        meta.origin,
        public_facts(200),
        "should report the origin's own authenticated facts"
    );
    assert!(
        body_of(response).await.is_empty(),
        "a head snapshot carries no bytes"
    );
    assert!(
        origin.reader_left().await,
        "the head snapshot should drop the source body unread"
    );
    assert_eq!(origin.requests().len(), 1, "should read the source once");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn private_and_unobserved_sources_are_refused_while_their_bodies_are_open() {
    let private = SnapshotOrigin {
        private: true,
        ..public_facts(200)
    };
    let cases = [
        (
            SnapshotMode::Whole,
            Facts::Genuine(private.clone()),
            (403, "source_private"),
        ),
        (
            SnapshotMode::Head,
            Facts::Genuine(private),
            (403, "source_private"),
        ),
        (
            SnapshotMode::Whole,
            Facts::Forged(public_facts(200)),
            (422, "snapshot_unobserved"),
        ),
        (
            SnapshotMode::Head,
            Facts::Forged(public_facts(200)),
            (422, "snapshot_unobserved"),
        ),
        (
            SnapshotMode::Whole,
            Facts::Missing,
            (422, "snapshot_unobserved"),
        ),
        (
            SnapshotMode::Head,
            Facts::Missing,
            (422, "snapshot_unobserved"),
        ),
    ];
    for (mode, facts, (status, code)) in cases {
        let origin = HeldOrigin::start(declared_oversized_answer(facts));

        let response = snapshot(&origin, mode.clone()).await;

        assert_eq!(
            refusal(response).await,
            (status, code.to_owned()),
            "a {mode:?} snapshot should refuse a source its facts do not admit, before any size \
             answer and without its body"
        );
        assert!(
            origin.reader_left().await,
            "a refused {mode:?} snapshot should drop its unfinished source read"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_whole_snapshot_within_its_bound_still_returns_every_byte() {
    let source: Vec<u8> = (0..=255_u8).cycle().take(SNAPSHOT_LIMIT).collect();
    let served = source.clone();
    let origin = HeldOrigin::start(move |_| {
        let mut answer = response_head(
            "200 OK",
            &media_headers(
                &Facts::Genuine(public_facts(200)),
                NONCE,
                &[("content-length", served.len().to_string())],
            ),
        );
        answer.extend_from_slice(&served);
        answer
    });

    let response = snapshot(&origin, SnapshotMode::Whole).await;

    assert_eq!(
        response.status().as_u16(),
        200,
        "should answer the snapshot"
    );
    let meta = snapshot_meta(&response);
    assert_eq!(
        meta.length,
        Some(u64::try_from(source.len()).expect("should fit"))
    );
    assert_eq!(meta.origin, public_facts(200));
    assert_eq!(
        body_of(response).await,
        source,
        "a source exactly at the bound should arrive whole"
    );
}

// --- pinned publication reads ------------------------------------------------

/// The production-signed probe release the publication runs.
fn signed_release() -> ProbeRelease {
    serde_json::from_slice(include_bytes!(
        "../../provenance/encypher/src/fixtures/prepared/descriptor.json"
    ))
    .expect("should parse the production-signed descriptor")
}

/// Keeps the signed release's artifact in `kv`, as a store that loaded it
/// before does, so publication runs it without asking the API.
async fn keep_probe(kv: &edgezero_core::key_value_store::KvHandle) {
    kv.put_bytes(
        &format!("encypher:probe:v1:{}", signed_release().sha256),
        bytes::Bytes::from_static(include_bytes!(
            "../../provenance/encypher/src/fixtures/prepared/probe.wasm"
        )),
    )
    .await
    .expect("should keep the probe artifact");
}

/// A large envelope for a `length`-byte source at [`SOURCE_PATH`].
fn large_envelope(length: u64) -> ResultEnvelope {
    let release = signed_release();
    let page_url = format!("https://{HOST}{SOURCE_PATH}");
    let policy = "a".repeat(64);
    let source_sha256 = "c".repeat(64);
    let identity = LargeLocator {
        credential_context: &credential_context(API_BASE_URL, HOST, WORKER_TOKEN),
        policy_digest: &policy,
        reader_url: &page_url,
        variant: "",
        origin_host: ORIGIN_HOST,
        origin_identity_sha256: ORIGIN_IDENTITY_SHA256,
        mime: "video/mp4",
        etag: ETAG,
        length,
    }
    .digest();
    let request = PrepareRequest {
        contract_version: 2,
        carrier_requirement: "external_allowed".to_owned(),
        page_url: page_url.clone(),
        expected_policy_digest: policy,
        protocol: PREPARED_PROTOCOL.to_owned(),
        probe: ProbeSelection {
            sha256: release.sha256.clone(),
            planner_id: release.planner_id.clone(),
        },
        source: PreparedSourceRef {
            reference: page_url,
            origin_host: ORIGIN_HOST.to_owned(),
            mime: "video/mp4".to_owned(),
            sha256: source_sha256.clone(),
            byte_length: length,
            reference_kind: "origin".to_owned(),
        },
        structure: vec![StructureChunk {
            offset: 0,
            b64: "AAAA".to_owned(),
        }],
    };
    ResultEnvelope {
        protocol: PREPARATION_PROTOCOL.to_owned(),
        deployment_id: "test".to_owned(),
        identity,
        retirement_epoch: 0,
        variant: String::new(),
        item: EnvelopeItem::Large {
            planned: PlannedMedia {
                request,
                release,
                plan_id: "0f8fad5b-d9cb-469f-a165-70867728950e".to_owned(),
                geometry: json!({"disposition": "external"}),
                digest: source_sha256,
                idempotency_key: "f".repeat(64),
            },
            etag: ETAG.to_owned(),
            origin_identity_sha256: ORIGIN_IDENTITY_SHA256.to_owned(),
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_pinned_range_longer_than_it_names_is_refused_while_its_body_is_open() {
    const LENGTH: u64 = 1024;
    let origin = HeldOrigin::start(|head| {
        let nonce = header_in(head, CAPABILITY_HEADER)
            .and_then(|capability| capability.split('.').nth(2).map(str::to_owned))
            .unwrap_or_default();
        // The range's headers are exact, but its body runs past the range
        // and never ends.
        let mut answer = response_head(
            "206 Partial Content",
            &media_headers(
                &Facts::Genuine(public_facts(206)),
                &nonce,
                &[
                    ("content-range", format!("bytes 0-{}/{LENGTH}", LENGTH - 1)),
                    ("transfer-encoding", "chunked".to_owned()),
                ],
            ),
        );
        answer.extend_from_slice(&unfinished_chunked_body(&[b'x'; 1025]));
        answer
    });
    let directory = tempfile::tempdir().expect("should create a KV directory");
    let kv = open_kv_handle(&directory.path().join("kv.redb")).expect("should open the KV store");
    keep_probe(&kv).await;
    let envelope = large_envelope(LENGTH);
    let mut request = request_builder()
        .method("PUT")
        .uri(format!(
            "https://{HOST}{RESULTS_PREFIX}{}",
            envelope.identity
        ))
        .header("host", HOST)
        .header(TOKEN_HEADER, PUBLICATION)
        .body(EdgeBody::from(
            serde_json::to_vec(&envelope).expect("should encode the envelope"),
        ))
        .expect("should build the publication request");
    request
        .extensions_mut()
        .insert(KvRegistry::single_id("default".to_owned(), kv));

    let response = answer_within(&router(&origin), request).await;

    assert_eq!(
        refusal(response).await,
        (422, "source_range_invalid".to_owned()),
        "a pinned range whose body runs past the range should be refused as soon as it does"
    );
    assert!(
        origin.reader_left().await,
        "the publication should drop its unfinished pinned read"
    );
    let requests = origin.requests();
    assert_eq!(requests.len(), 1, "should stop at the first invalid range");
    assert_eq!(
        header_in(&requests[0], "range").as_deref(),
        Some("bytes=0-1023"),
        "should read the pinned range"
    );
}
