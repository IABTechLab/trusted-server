use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use base64::Engine as _;
use error_stack::Report;
use futures::FutureExt as _;
use futures::StreamExt as _;
use futures::channel::oneshot;
use futures::executor::block_on;
use sha2::Digest as _;

use super::*;
use crate::error::TrustedServerError;
use crate::media_signing::test_support::MANIFEST_LINK;
use crate::media_signing::{Answered, MediaJob, MediaNamespacing, MediaSigning};
use crate::origin_auth::OriginSigner;
use crate::platform::test_support::{
    ElapsedTimer, MemoryLoadingCache, StoppedTimer, build_services_with_http_client,
};
use crate::platform::{
    CacheSpec, PlatformError, PlatformHttpClient, PlatformLoadingCache, PlatformPendingRequest,
    PlatformResponse, PlatformSelectResult, PlatformTimer,
};
use crate::test_support::tests::create_test_settings;

const PATH: &str = "/media/42";
const FETCHED_URL: &str = "https://origin.example.com/media/42";
const QUERY_FETCHED_URL: &str = "https://origin.example.com/media/42?v=2";
const READER_URL: &str = "https://www.example.com/media/42";
const ORIGIN_IDENTITY: &str = "https://origin.example.com\0origin.example.com";
const ETAG_V1: &str = "\"v1\"";
const ETAG_V2: &str = "\"v2\"";
/// The size of each chunk the stub origin streams, so every body arrives
/// in several pieces.
const CHUNK: usize = 5;
/// The `Authorization` the test origin signer writes.
const ORIGIN_SIGNATURE: &str = "TestOrigin signed";

fn source() -> Vec<u8> {
    (0_u8..64).collect()
}

fn other_source() -> Vec<u8> {
    (100_u8..164).collect()
}

fn identity() -> VariantIdentity<'static> {
    VariantIdentity {
        reader_url: READER_URL,
        origin_identity: ORIGIN_IDENTITY,
        fetched_url: FETCHED_URL,
        reader_query_free: true,
    }
}

/// An origin answer for `bytes` at version `etag`: the bytes `range`
/// names as a `206`, or all of them as a `200`, streamed in [`CHUNK`]s and
/// cut with a stream error after `cut_at` bytes where given.
fn serve(
    bytes: &[u8],
    etag: &str,
    range: Option<(u64, u64)>,
    cut_at: Option<usize>,
) -> Response<EdgeBody> {
    let total = bytes.len();
    let (status, slice) = match range {
        Some((first, last)) => {
            let first = usize::try_from(first).expect("should fit a test offset");
            let last = usize::try_from(last).expect("should fit a test offset");
            (StatusCode::PARTIAL_CONTENT, bytes[first..=last].to_vec())
        }
        None => (StatusCode::OK, bytes.to_vec()),
    };
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, "public, max-age=600")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, slice.len());
    if let Some((first, last)) = range {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {first}-{last}/{total}"),
        );
    }
    let kept = cut_at.map_or(slice.len(), |cut| cut.min(slice.len()));
    let mut items: Vec<Result<Bytes, std::io::Error>> = slice[..kept]
        .chunks(CHUNK)
        .map(|chunk| Ok(Bytes::copy_from_slice(chunk)))
        .collect();
    if cut_at.is_some() {
        items.push(Err(std::io::Error::other("origin connection cut")));
    }
    builder
        .body(EdgeBody::from_stream(futures::stream::iter(items)))
        .expect("should build an origin answer")
}

/// The origin's whole `200` for the reader's first request.
fn whole() -> Response<EdgeBody> {
    serve(&source(), ETAG_V1, None, None)
}

/// The origin's answer to a `HEAD`: the whole's headers, no body.
fn head_answer(bytes: &[u8], etag: &str) -> Response<EdgeBody> {
    let (parts, _) = serve(bytes, etag, None, None).into_parts();
    Response::from_parts(parts, EdgeBody::empty())
}

fn not_modified(etag: &str) -> Response<EdgeBody> {
    Response::builder()
        .status(StatusCode::NOT_MODIFIED)
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, "public, max-age=600")
        .body(EdgeBody::empty())
        .expect("should build a not modified answer")
}

fn assert_unchanged_not_modified(delivery: Delivery, headers: &HeaderMap) {
    let Delivery::Send(response) = delivery else {
        panic!("should send the original not modified response");
    };
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(response.headers(), headers, "should preserve every header");
    let (_, body) = collect(response);
    assert_eq!(body, Some(Vec::new()), "should preserve the empty body");
}

/// A `bytes=first-last` request range.
fn requested_range(headers: &HeaderMap) -> Option<(u64, u64)> {
    let value = headers.get(header::RANGE)?.to_str().ok()?;
    let (first, last) = value.strip_prefix("bytes=")?.split_once('-')?;
    Some((first.parse().ok()?, last.parse().ok()?))
}

struct VersionChange {
    after: usize,
    bytes: Vec<u8>,
    etag: String,
    headers: Vec<(HeaderName, HeaderValue)>,
}

#[derive(Default)]
struct OriginState {
    bytes: Vec<u8>,
    etag: String,
    /// From the request after this many on, the origin serves the other
    /// version, answering it with these headers too.
    flip: Option<VersionChange>,
    /// Headers every answer carries besides [`serve`]'s own, replacing
    /// them.
    answer_headers: Vec<(HeaderName, HeaderValue)>,
    /// Override the status after this many requests.
    status_after: Option<(usize, StatusCode)>,
    /// The next answer's stream is cut after this many bytes.
    cut_next: Option<usize>,
    sends: usize,
    wrong_content_range: bool,
    requests: Vec<HeaderMap>,
}

/// An origin that honors `Range` and `If-Match` and can change version.
struct RangeOrigin {
    state: Mutex<OriginState>,
}

impl RangeOrigin {
    fn new(bytes: &[u8], etag: &str) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(OriginState {
                bytes: bytes.to_vec(),
                etag: etag.to_owned(),
                ..OriginState::default()
            }),
        })
    }

    /// Serves `bytes` at `etag` from the request after the `after`th on.
    fn flip_after(&self, after: usize, bytes: &[u8], etag: &str) {
        self.flip_after_with(after, bytes, etag, Vec::new());
    }

    /// As [`Self::flip_after`], the new version's answers carrying
    /// `headers` too.
    fn flip_after_with(
        &self,
        after: usize,
        bytes: &[u8],
        etag: &str,
        headers: Vec<(HeaderName, HeaderValue)>,
    ) {
        self.state.lock().expect("should lock origin").flip = Some(VersionChange {
            after,
            bytes: bytes.to_vec(),
            etag: etag.to_owned(),
            headers,
        });
    }

    fn cut_next_at(&self, at: usize) {
        self.state.lock().expect("should lock origin").cut_next = Some(at);
    }

    fn answer_wrong_content_range(&self) {
        self.state
            .lock()
            .expect("should lock origin")
            .wrong_content_range = true;
    }

    fn requests(&self) -> Vec<HeaderMap> {
        self.state
            .lock()
            .expect("should lock origin")
            .requests
            .clone()
    }

    /// The ranges every request asked for, in order.
    fn ranges(&self) -> Vec<Option<(u64, u64)>> {
        self.requests().iter().map(requested_range).collect()
    }

    /// How many requests read the whole version, which only hashing does
    /// besides a whole delivery.
    fn whole_reads(&self) -> usize {
        self.ranges()
            .iter()
            .filter(|range| **range == Some((0, 63)))
            .count()
    }
}

#[async_trait::async_trait(?Send)]
impl PlatformHttpClient for RangeOrigin {
    fn supports_streaming_responses(&self) -> bool {
        true
    }

    async fn send(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        let mut state = self.state.lock().expect("should lock origin");
        if state
            .flip
            .as_ref()
            .is_some_and(|flip| state.sends >= flip.after)
        {
            let flip = state.flip.take().expect("should have a due version change");
            state.bytes = flip.bytes;
            state.etag = flip.etag;
            state.answer_headers = flip.headers;
        }
        state.sends += 1;
        let headers = request.request.headers().clone();
        state.requests.push(headers.clone());
        if let Some(if_match) = headers.get(header::IF_MATCH)
            && if_match.as_bytes() != state.etag.as_bytes()
        {
            let refused = Response::builder()
                .status(StatusCode::PRECONDITION_FAILED)
                .header(header::ETAG, state.etag.as_str())
                .body(EdgeBody::empty())
                .expect("should build a refusal");
            return Ok(PlatformResponse::new(refused));
        }
        let range = requested_range(&headers);
        let cut = state.cut_next.take();
        let mut response = serve(&state.bytes, &state.etag, range, cut);
        if let Some((after, status)) = state.status_after
            && state.sends > after
        {
            *response.status_mut() = status;
        }
        for (name, value) in &state.answer_headers {
            response.headers_mut().insert(name.clone(), value.clone());
        }
        if state.wrong_content_range && range.is_some() {
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_static("bytes 0-0/1"),
            );
        }
        Ok(PlatformResponse::new(response))
    }

    async fn send_async(
        &self,
        _request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
        Err(Report::new(PlatformError::HttpClient))
    }

    async fn select(
        &self,
        _pending_requests: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>> {
        Err(Report::new(PlatformError::HttpClient))
    }
}

/// An origin signer that writes [`ORIGIN_SIGNATURE`] and owns every
/// `x-test-origin-*` header.
struct TestOriginSigner;

impl OriginSigner for TestOriginSigner {
    fn sign(
        &self,
        _method: &Method,
        _url: &url::Url,
        headers: &mut HeaderMap,
        _now: SystemTime,
    ) -> Result<(), Report<TrustedServerError>> {
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static(ORIGIN_SIGNATURE),
        );
        Ok(())
    }

    fn remove_namespace_headers(&self, headers: &mut HeaderMap) {
        headers.remove("x-test-origin-key");
    }
}

/// One large job the stub signer was handed.
#[derive(Debug, Clone)]
struct LargeJobRecord {
    source_sha256: String,
    size: u64,
    remaining: Duration,
    source_reference: Option<String>,
    reader_reference: Option<String>,
    origin_host: String,
}

/// How the stub signer answers a large job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LargeBehavior {
    /// Signs, adding [`MANIFEST_LINK`].
    Sign,
    /// Signs after confirming the pinned version is still current.
    ConfirmThenSign,
    /// Refuses, as a source with provenance of its own is refused.
    Refuse,
    /// Never answers.
    Hang,
}

/// A signer whose bounded limit is 16 bytes and whose large lane takes up
/// to 1 KiB.
struct LargeSigner {
    behavior: LargeBehavior,
    namespace: Mutex<Result<String, MediaSigningFailure>>,
    namespace_gate: Mutex<Option<oneshot::Receiver<()>>>,
    large_duration: Duration,
    attempts: Mutex<usize>,
    jobs: Mutex<Vec<LargeJobRecord>>,
    failures: Mutex<Vec<&'static str>>,
}

impl LargeSigner {
    fn new(behavior: LargeBehavior) -> Arc<Self> {
        Arc::new(Self {
            behavior,
            namespace: Mutex::new(Ok("policy-1".to_owned())),
            namespace_gate: Mutex::new(None),
            large_duration: Duration::from_secs(60),
            attempts: Mutex::new(0),
            jobs: Mutex::new(Vec::new()),
            failures: Mutex::new(Vec::new()),
        })
    }

    fn set_namespace(&self, namespace: Result<String, MediaSigningFailure>) {
        *self.namespace.lock().expect("should lock namespace") = namespace;
    }

    /// How many times core called `sign_large`.
    fn attempts(&self) -> usize {
        *self.attempts.lock().expect("should lock attempts")
    }

    fn jobs(&self) -> Vec<LargeJobRecord> {
        self.jobs.lock().expect("should lock jobs").clone()
    }

    fn failures(&self) -> Vec<&'static str> {
        self.failures.lock().expect("should lock failures").clone()
    }
}

impl MediaSigner for LargeSigner {
    fn signer_id(&self) -> &'static str {
        "testing.large-media"
    }

    fn handles_path(&self, path: &str) -> bool {
        !path.starts_with("/api/")
    }

    fn handles_media_type(&self, media_type: &str) -> bool {
        media_type.starts_with("image/")
    }

    fn max_source_bytes(&self) -> usize {
        16
    }

    fn namespace<'a>(&'a self, _services: &'a RuntimeServices) -> MediaNamespacing<'a> {
        let namespace = self
            .namespace
            .lock()
            .expect("should lock namespace")
            .clone();
        let gate = self
            .namespace_gate
            .lock()
            .expect("should lock namespace gate")
            .take();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.await.expect("should release namespace");
            }
            namespace
        })
    }

    fn sign<'a>(
        &'a self,
        _services: &'a RuntimeServices,
        _job: &'a MediaJob<'a>,
    ) -> MediaSigning<'a> {
        Box::pin(async {
            Err(MediaSigningFailure {
                reason: "bounded_not_expected",
            })
        })
    }

    fn max_large_source_bytes(&self) -> u64 {
        1024
    }

    fn max_large_media_duration(&self) -> Duration {
        self.large_duration
    }

    fn sign_large<'a>(
        &'a self,
        _services: &'a RuntimeServices,
        job: &'a LargeMediaJob<'a>,
    ) -> LargeMediaSigning<'a> {
        Box::pin(async move {
            *self.attempts.lock().expect("should lock attempts") += 1;
            match self.behavior {
                LargeBehavior::Refuse => {
                    return Err(MediaSigningFailure {
                        reason: "source_has_embedded_provenance",
                    });
                }
                LargeBehavior::Hang => futures::future::pending::<()>().await,
                LargeBehavior::ConfirmThenSign => {
                    if let Err(error) = job.source.confirm_current().await {
                        self.failures
                            .lock()
                            .expect("should lock failures")
                            .push(error.code());
                        return Err(MediaSigningFailure {
                            reason: error.code(),
                        });
                    }
                }
                LargeBehavior::Sign => {}
            }
            self.jobs
                .lock()
                .expect("should lock jobs")
                .push(LargeJobRecord {
                    source_sha256: job.source_sha256.to_owned(),
                    size: job.source.size(),
                    remaining: job.remaining,
                    source_reference: job.source_reference.map(str::to_owned),
                    reader_reference: job.reader_reference.map(str::to_owned),
                    origin_host: job.source_origin_host.to_owned(),
                });
            let namespace = self
                .namespace
                .lock()
                .expect("should lock namespace")
                .clone()
                .unwrap_or_default();
            Ok(LargeSignedMedia {
                headers: vec![(header::LINK, HeaderValue::from_static(MANIFEST_LINK))],
                namespace,
            })
        })
    }
}

#[derive(Default)]
struct PendingLargeCache {
    calls: Mutex<usize>,
}

#[async_trait::async_trait(?Send)]
impl PlatformLoadingCache for PendingLargeCache {
    async fn get_or_load(
        &self,
        _services: &RuntimeServices,
        _spec: &CacheSpec,
        _key: &StoreKey,
        _load: Loader,
    ) -> Result<Bytes, LoadError> {
        *self.calls.lock().expect("should lock cache calls") += 1;
        futures::future::pending().await
    }

    async fn remove(&self, _services: &RuntimeServices, _spec: &CacheSpec, _key: &StoreKey) {
        panic!("should never remove a pending cache outcome");
    }
}

fn services_with_timer(
    signer: &Arc<LargeSigner>,
    origin: &Arc<RangeOrigin>,
    timer: Arc<dyn PlatformTimer>,
) -> RuntimeServices {
    build_services_with_http_client(Arc::clone(origin) as Arc<dyn PlatformHttpClient>)
        .with_media_signer(Some(Arc::clone(signer) as Arc<dyn MediaSigner>))
        .with_loading_cache(Arc::new(MemoryLoadingCache::default()))
        .with_timer(timer)
}

fn services_for(signer: &Arc<LargeSigner>, origin: &Arc<RangeOrigin>) -> RuntimeServices {
    services_with_timer(signer, origin, Arc::new(StoppedTimer))
}

fn claim_for(
    services: &RuntimeServices,
    method: &Method,
    pairs: &[(HeaderName, &str)],
) -> MediaClaim {
    let mut headers = HeaderMap::new();
    headers.insert("sec-fetch-dest", HeaderValue::from_static("image"));
    for (name, value) in pairs {
        headers.insert(
            name.clone(),
            HeaderValue::from_str(value).expect("should build a header"),
        );
    }
    MediaClaim::of_request(
        &create_test_settings(),
        services,
        method,
        PATH,
        &headers,
        true,
    )
    .expect("should claim a media path")
}

/// How core asks the origin again in a test: the address, the request's
/// headers before signing, and the origin's signing where it has one.
struct TestOrigin<'a> {
    identity: VariantIdentity<'a>,
    headers: HeaderMap,
    auth: Option<OriginSigning>,
    unsigned_headers: Vec<HeaderName>,
    response_header_rewrite: Option<crate::response_header_rewrite::OriginHeaderRewrite<'a>>,
}

impl TestOrigin<'static> {
    /// An origin request that forwarded the reader's `pairs`, as a route
    /// forwards a reader's range and conditions.
    fn forwarding(pairs: &[(HeaderName, &str)]) -> Self {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                name.clone(),
                HeaderValue::from_str(value).expect("should build a header"),
            );
        }
        Self {
            headers,
            ..Self::plain()
        }
    }

    fn plain() -> Self {
        Self {
            identity: identity(),
            headers: HeaderMap::new(),
            auth: None,
            unsigned_headers: Vec::new(),
            response_header_rewrite: None,
        }
    }
}

/// Delivers the origin's `response` through `origin`, by the large lane
/// where core admits it. Whether it was admitted, and the answer.
fn run_through(
    claim: &MediaClaim,
    services: &RuntimeServices,
    origin: &TestOrigin<'_>,
    response: Response<EdgeBody>,
) -> (bool, Response<EdgeBody>) {
    block_on(async {
        let facts = OriginFacts::of(response.headers());
        match claim
            .deliver(
                services,
                &origin.identity,
                Answered::Origin(facts),
                response,
            )
            .await
        {
            Delivery::Large(answer) => {
                let url = url::Url::parse(origin.identity.fetched_url)
                    .expect("should parse the fetched address");
                let pinned = PinnedOrigin {
                    url: &url,
                    headers: &origin.headers,
                    backend_name: "origin",
                    auth: origin.auth.as_ref(),
                    unsigned_headers: &origin.unsigned_headers,
                    response_header_rewrite: origin.response_header_rewrite,
                };
                (
                    true,
                    claim
                        .deliver_large(services, &origin.identity, pinned, answer)
                        .await,
                )
            }
            Delivery::Send(response) => (false, response),
            Delivery::FetchWhole(_) => {
                panic!("should never ask for a bounded whole of a large source")
            }
        }
    })
}

fn run(
    claim: &MediaClaim,
    services: &RuntimeServices,
    response: Response<EdgeBody>,
) -> (bool, Response<EdgeBody>) {
    run_through(claim, services, &TestOrigin::plain(), response)
}

/// The body's bytes, or `None` where its stream failed.
fn collect(response: Response<EdgeBody>) -> (Response<EdgeBody>, Option<Vec<u8>>) {
    let (parts, body) = response.into_parts();
    let bytes = block_on(async {
        match body {
            EdgeBody::Once(bytes) => Some(bytes.to_vec()),
            EdgeBody::Stream(mut stream) => {
                let mut collected = Vec::new();
                while let Some(chunk) = stream.next().await {
                    collected.extend_from_slice(&chunk.ok()?);
                }
                Some(collected)
            }
        }
    });
    (Response::from_parts(parts, EdgeBody::empty()), bytes)
}

fn header_of<'a>(response: &'a Response<EdgeBody>, name: &str) -> Option<&'a str> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
}

fn whole_digest_header(bytes: &[u8]) -> String {
    format!(
        "sha-256=:{}:",
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(bytes))
    )
}

#[test]
fn hashes_a_large_source_once_and_delivers_the_exact_pinned_bytes() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::ConfirmThenSign);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[]);

    let (large, response) = run(&claim, &services, whole());
    let (response, body) = collect(response);

    assert!(
        large,
        "should admit a source past the bounded limit with a strong tag"
    );
    let jobs = signer.jobs();
    assert_eq!(jobs.len(), 1, "should sign once");
    assert_eq!(
        jobs[0].source_sha256,
        hex::encode(Sha256::digest(source())),
        "should hand the signer the digest of every source byte"
    );
    assert_eq!(jobs[0].size, 64, "should pin the declared length");
    assert!(
        jobs[0].remaining > Duration::ZERO && jobs[0].remaining <= Duration::from_secs(60),
        "should tell the signer what is left of its budget"
    );
    assert_eq!(jobs[0].source_reference.as_deref(), Some(FETCHED_URL));
    assert_eq!(jobs[0].reader_reference.as_deref(), Some(READER_URL));
    assert_eq!(jobs[0].origin_host, "origin.example.com");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body,
        Some(source()),
        "should deliver the origin's exact bytes"
    );
    assert_eq!(
        header_of(&response, "etag"),
        Some(ETAG_V1),
        "should keep the origin's validator"
    );
    assert_eq!(header_of(&response, "content-length"), Some("64"));
    assert_eq!(
        header_of(&response, "link"),
        Some(MANIFEST_LINK),
        "should carry the manifest link"
    );
    assert_eq!(
        header_of(&response, "repr-digest"),
        Some(whole_digest_header(&source()).as_str()),
        "should describe the whole representation"
    );
    let requests = origin.requests();
    for request in &requests {
        assert_eq!(
            request
                .get(header::IF_MATCH)
                .and_then(|value| value.to_str().ok()),
            Some(ETAG_V1),
            "should pin every origin read to the version"
        );
        assert!(
            request.get(header::RANGE).is_some(),
            "should ask for exact ranges"
        );
    }
    assert_eq!(
        origin.ranges(),
        vec![Some((0, 63)), Some((0, 0)), Some((0, 63))],
        "should hash once, confirm, then deliver every byte of the pinned version"
    );
}

#[test]
fn answers_a_reader_range_with_the_exact_original_bytes() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::ConfirmThenSign);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[(header::RANGE, "bytes=10-19")]);

    let (large, response) = run(
        &claim,
        &services,
        serve(&source(), ETAG_V1, Some((10, 19)), None),
    );
    let (response, body) = collect(response);

    assert!(
        large,
        "should admit a part whose total is past the bounded limit"
    );
    assert_eq!(
        signer.jobs()[0].source_sha256,
        hex::encode(Sha256::digest(source())),
        "should hash the whole version, not the part"
    );
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        header_of(&response, "content-range"),
        Some("bytes 10-19/64")
    );
    assert_eq!(header_of(&response, "content-length"), Some("10"));
    assert_eq!(
        body,
        Some(source()[10..20].to_vec()),
        "should send the original bytes of the range"
    );
    assert_eq!(header_of(&response, "link"), Some(MANIFEST_LINK));
    assert_eq!(header_of(&response, "etag"), Some(ETAG_V1));
    assert_eq!(
        origin.ranges(),
        vec![Some((0, 63)), Some((0, 0)), Some((10, 19))],
        "should hash the whole pinned version once, then read the reader's range pinned"
    );

    let (_, response) = run(
        &claim,
        &services,
        serve(&source(), ETAG_V1, Some((10, 19)), None),
    );
    let (response, body) = collect(response);
    assert_eq!(
        origin.whole_reads(),
        1,
        "a second range should never hash again"
    );
    assert_eq!(body, Some(source()[10..20].to_vec()));
    assert_eq!(header_of(&response, "link"), Some(MANIFEST_LINK));
}

#[test]
fn a_cold_head_starts_no_load_and_a_warm_one_is_decorated() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::Sign);
    let services = services_for(&signer, &origin);
    let head = claim_for(&services, &Method::HEAD, &[]);

    let (large, response) = run(&head, &services, head_answer(&source(), ETAG_V1));
    let (response, body) = collect(response);
    assert!(large, "should admit a HEAD of a large source");
    assert!(
        origin.requests().is_empty(),
        "a cold HEAD should never read the origin again"
    );
    assert_eq!(
        signer.attempts(),
        0,
        "a cold HEAD should never call the signer"
    );
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_of(&response, "content-length"), Some("64"));
    assert_eq!(header_of(&response, "etag"), Some(ETAG_V1));
    assert_eq!(header_of(&response, "link"), None);
    assert_eq!(body, Some(Vec::new()), "a HEAD carries no body");

    let get = claim_for(&services, &Method::GET, &[]);
    let _ = collect(run(&get, &services, whole()).1);
    let reads = origin.requests().len();
    let (_, response) = run(&head, &services, head_answer(&source(), ETAG_V1));
    let (response, body) = collect(response);
    assert_eq!(
        header_of(&response, "link"),
        Some(MANIFEST_LINK),
        "a warm HEAD carries the manifest"
    );
    assert_eq!(
        header_of(&response, "repr-digest"),
        Some(whole_digest_header(&source()).as_str())
    );
    assert_eq!(header_of(&response, "content-length"), Some("64"));
    assert_eq!(body, Some(Vec::new()));
    assert_eq!(
        origin.requests().len(),
        reads,
        "a warm HEAD should read nothing"
    );
    assert_eq!(signer.attempts(), 1);
}

#[test]
fn only_an_origin_that_says_it_serves_byte_ranges_starts_a_load() {
    for accept_ranges in [None, Some("none")] {
        let origin = RangeOrigin::new(&source(), ETAG_V1);
        let signer = LargeSigner::new(LargeBehavior::Sign);
        let services = services_for(&signer, &origin);
        let claim = claim_for(&services, &Method::GET, &[]);
        let mut answer = whole();
        answer.headers_mut().remove(header::ACCEPT_RANGES);
        if let Some(value) = accept_ranges {
            answer
                .headers_mut()
                .insert(header::ACCEPT_RANGES, HeaderValue::from_static(value));
        }

        let (_, response) = run(&claim, &services, answer);
        let (response, body) = collect(response);

        assert!(
            origin.requests().is_empty(),
            "{accept_ranges:?}: should never hash"
        );
        assert_eq!(signer.attempts(), 0, "{accept_ranges:?}");
        assert_eq!(
            body,
            Some(source()),
            "{accept_ranges:?}: should send the origin's answer"
        );
        assert_eq!(
            header_of(&response, "etag"),
            Some(ETAG_V1),
            "{accept_ranges:?}"
        );
        assert_eq!(header_of(&response, "link"), None, "{accept_ranges:?}");
    }

    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::Sign);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[]);
    let _ = collect(run(&claim, &services, whole()).1);
    let mut answer = whole();
    answer.headers_mut().remove(header::ACCEPT_RANGES);
    let (_, response) = run(&claim, &services, answer);
    let (response, body) = collect(response);
    assert_eq!(body, Some(source()));
    assert_eq!(
        header_of(&response, "link"),
        Some(MANIFEST_LINK),
        "a kept signed outcome still decorates the version"
    );
    assert_eq!(signer.attempts(), 1);
}

#[test]
fn a_kept_outcome_answers_without_hashing_and_never_outlives_the_policy() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::Sign);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[]);
    let _ = collect(run(&claim, &services, whole()).1);
    let reads = origin.requests().len();

    let (_, response) = run(&claim, &services, whole());
    let (response, body) = collect(response);
    assert_eq!(
        signer.attempts(),
        1,
        "should reuse the outcome kept for the version"
    );
    assert_eq!(
        origin.requests().len(),
        reads,
        "should send the origin's own answer"
    );
    assert_eq!(body, Some(source()));
    assert_eq!(header_of(&response, "link"), Some(MANIFEST_LINK));

    let (_, revalidated) = run(&claim, &services, not_modified(ETAG_V1));
    assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        header_of(&revalidated, "link"),
        Some(MANIFEST_LINK),
        "should tell a revalidating reader of the manifest"
    );

    signer.set_namespace(Ok("policy-2".to_owned()));
    let _ = collect(run(&claim, &services, whole()).1);
    assert_eq!(
        signer.attempts(),
        2,
        "a changed policy should never find the old outcome"
    );

    signer.set_namespace(Err(MediaSigningFailure { reason: "revoked" }));
    let reads = origin.requests().len();
    let (_, response) = run(&claim, &services, whole());
    let (response, body) = collect(response);
    assert_eq!(signer.attempts(), 2, "a revoked policy should sign nothing");
    assert_eq!(origin.requests().len(), reads, "should hash nothing");
    assert_eq!(
        header_of(&response, "link"),
        None,
        "should carry no manifest"
    );
    assert_eq!(
        header_of(&response, "etag"),
        Some(ETAG_V1),
        "should keep the origin's validator on its unchanged bytes"
    );
    assert_eq!(body, Some(source()));
}

#[test]
fn a_refused_source_is_hashed_once_and_then_sent_as_it_came() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::Refuse);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[(header::RANGE, "bytes=0-9")]);
    let forwarding = TestOrigin::forwarding(&[(header::RANGE, "bytes=0-9")]);

    let (_, response) = run_through(
        &claim,
        &services,
        &forwarding,
        serve(&source(), ETAG_V1, Some((0, 9)), None),
    );
    let (response, body) = collect(response);
    assert_eq!(signer.attempts(), 1);
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(header_of(&response, "content-range"), Some("bytes 0-9/64"));
    assert_eq!(body, Some(source()[0..10].to_vec()));
    assert_eq!(header_of(&response, "link"), None);
    assert_eq!(
        header_of(&response, "etag"),
        Some(ETAG_V1),
        "should keep the origin's validator"
    );
    assert_eq!(
        header_of(&response, "cache-control"),
        Some(crate::cache_policy::NO_STORE_PRIVATE_CACHE_CONTROL),
        "a fresh fallback is held by no cache, even for the same version"
    );
    let fresh = origin
        .requests()
        .last()
        .cloned()
        .expect("should ask the origin afresh");
    assert_eq!(
        requested_range(&fresh),
        Some((0, 9)),
        "should ask afresh exactly as the reader asked"
    );
    assert!(
        fresh.get(header::IF_MATCH).is_none(),
        "an unsigned answer is not pinned"
    );
    let reads = origin.requests().len();

    for _ in 0..3 {
        let (_, response) = run_through(
            &claim,
            &services,
            &forwarding,
            serve(&source(), ETAG_V1, Some((0, 9)), None),
        );
        let (response, body) = collect(response);
        assert_eq!(body, Some(source()[0..10].to_vec()));
        assert_eq!(header_of(&response, "etag"), Some(ETAG_V1));
        assert_eq!(header_of(&response, "link"), None);
    }
    assert_eq!(
        signer.attempts(),
        1,
        "a kept refusal should never call the signer again"
    );
    assert_eq!(
        origin.requests().len(),
        reads,
        "a kept refusal should never read the origin again"
    );
    assert_eq!(origin.whole_reads(), 1, "should hash once in all");
}

#[test]
fn a_declined_signed_origin_is_asked_afresh_with_a_fresh_signature() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::Refuse);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[(header::RANGE, "bytes=0-9")]);
    let mut headers = HeaderMap::new();
    headers.insert(header::RANGE, HeaderValue::from_static("bytes=0-9"));
    headers.insert(
        header::REFERER,
        HeaderValue::from_static("https://www.example.com/page"),
    );
    headers.insert("x-test-origin-key", HeaderValue::from_static("from-reader"));
    let signed = TestOrigin {
        headers,
        auth: Some(OriginSigning::new(Arc::new(TestOriginSigner), None)),
        unsigned_headers: vec![header::REFERER],
        ..TestOrigin::plain()
    };

    let (_, response) = run_through(
        &claim,
        &services,
        &signed,
        serve(&source(), ETAG_V1, Some((0, 9)), None),
    );
    let (response, body) = collect(response);

    assert_eq!(body, Some(source()[0..10].to_vec()));
    assert_eq!(header_of(&response, "link"), None);
    let fresh = origin
        .requests()
        .last()
        .cloned()
        .expect("should ask the origin afresh");
    assert_eq!(
        fresh
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok()),
        Some(ORIGIN_SIGNATURE),
        "should sign the fresh request afresh"
    );
    assert_eq!(requested_range(&fresh), Some((0, 9)));
    assert!(fresh.get(header::REFERER).is_none());
    assert!(fresh.get("x-test-origin-key").is_none());
    assert!(fresh.get(header::IF_MATCH).is_none());
}

#[test]
fn a_not_modified_pending_namespace_obeys_the_large_budget() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let mut signer = LargeSigner::new(LargeBehavior::Sign);
    let (namespace_ready, namespace_gate) = oneshot::channel();
    let configured = Arc::get_mut(&mut signer).expect("should exclusively own the signer");
    configured.large_duration = Duration::from_secs(5);
    *configured
        .namespace_gate
        .lock()
        .expect("should lock namespace gate") = Some(namespace_gate);
    let timer = Arc::new(ElapsedTimer::default());
    let cache = Arc::new(PendingLargeCache::default());
    let services = services_with_timer(
        &signer,
        &origin,
        Arc::clone(&timer) as Arc<dyn PlatformTimer>,
    )
    .with_loading_cache(Arc::clone(&cache) as Arc<dyn PlatformLoadingCache>);
    let claim = claim_for(&services, &Method::GET, &[]);
    let response = not_modified(ETAG_V1);
    let headers = response.headers().clone();
    let facts = OriginFacts::of(response.headers());

    let delivery = claim
        .deliver(&services, &identity(), Answered::Origin(facts), response)
        .now_or_never()
        .expect("should stop waiting for the namespace at the large deadline");

    assert_unchanged_not_modified(delivery, &headers);
    assert_eq!(*cache.calls.lock().expect("should lock cache calls"), 0);
    assert!(origin.requests().is_empty());
    assert_eq!(signer.attempts(), 0);
    let requested = timer.requested.lock().expect("should lock timer requests");
    assert_eq!(requested.len(), 1);
    assert!(requested[0] > Duration::ZERO && requested[0] <= Duration::from_secs(5));
    drop(namespace_ready);
}

#[test]
fn a_not_modified_pending_cache_obeys_the_large_budget() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let mut signer = LargeSigner::new(LargeBehavior::Sign);
    Arc::get_mut(&mut signer)
        .expect("should exclusively own the signer")
        .large_duration = Duration::from_secs(5);
    let timer = Arc::new(ElapsedTimer::default());
    let cache = Arc::new(PendingLargeCache::default());
    let services = services_with_timer(
        &signer,
        &origin,
        Arc::clone(&timer) as Arc<dyn PlatformTimer>,
    )
    .with_loading_cache(Arc::clone(&cache) as Arc<dyn PlatformLoadingCache>);
    let claim = claim_for(&services, &Method::GET, &[]);
    let response = not_modified(ETAG_V1);
    let headers = response.headers().clone();
    let facts = OriginFacts::of(response.headers());

    let delivery = claim
        .deliver(&services, &identity(), Answered::Origin(facts), response)
        .now_or_never()
        .expect("should stop waiting for the cached outcome at the large deadline");

    assert_unchanged_not_modified(delivery, &headers);
    assert_eq!(*cache.calls.lock().expect("should lock cache calls"), 1);
    assert!(origin.requests().is_empty());
    assert_eq!(signer.attempts(), 0);
    let requested = timer.requested.lock().expect("should lock timer requests");
    assert_eq!(requested.len(), 1);
    assert!(requested[0] > Duration::ZERO && requested[0] <= Duration::from_secs(5));
}

#[test]
fn a_not_modified_without_a_timer_or_positive_large_budget_starts_no_io() {
    for (has_timer, budget) in [(false, Duration::from_secs(5)), (true, Duration::ZERO)] {
        let origin = RangeOrigin::new(&source(), ETAG_V1);
        let mut signer = LargeSigner::new(LargeBehavior::Sign);
        let (namespace_ready, namespace_gate) = oneshot::channel();
        let configured = Arc::get_mut(&mut signer).expect("should exclusively own the signer");
        configured.large_duration = budget;
        *configured
            .namespace_gate
            .lock()
            .expect("should lock namespace gate") = Some(namespace_gate);
        let cache = Arc::new(PendingLargeCache::default());
        let services =
            build_services_with_http_client(Arc::clone(&origin) as Arc<dyn PlatformHttpClient>)
                .with_media_signer(Some(Arc::clone(&signer) as Arc<dyn MediaSigner>))
                .with_loading_cache(Arc::clone(&cache) as Arc<dyn PlatformLoadingCache>);
        let services = if has_timer {
            services.with_timer(Arc::new(ElapsedTimer::default()))
        } else {
            services
        };
        let claim = claim_for(&services, &Method::GET, &[]);
        let response = not_modified(ETAG_V1);
        let headers = response.headers().clone();
        let facts = OriginFacts::of(response.headers());

        let delivery = claim
            .deliver(&services, &identity(), Answered::Origin(facts), response)
            .now_or_never()
            .expect("should refuse before polling namespace without a timer and positive budget");

        assert_unchanged_not_modified(delivery, &headers);
        assert!(
            signer
                .namespace_gate
                .lock()
                .expect("should lock namespace gate")
                .is_some(),
            "should never call namespace resolution"
        );
        assert_eq!(*cache.calls.lock().expect("should lock cache calls"), 0);
        assert!(origin.requests().is_empty());
        assert_eq!(signer.attempts(), 0);
        drop(namespace_ready);
    }
}

#[test]
fn a_namespace_pending_past_the_large_budget_starts_no_hash_or_signing() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let mut signer = LargeSigner::new(LargeBehavior::Sign);
    let (namespace_ready, namespace_gate) = oneshot::channel();
    let configured = Arc::get_mut(&mut signer).expect("should exclusively own the signer");
    configured.large_duration = Duration::from_secs(5);
    *configured
        .namespace_gate
        .lock()
        .expect("should lock namespace gate") = Some(namespace_gate);
    let timer = Arc::new(ElapsedTimer::default());
    let services = services_with_timer(
        &signer,
        &origin,
        Arc::clone(&timer) as Arc<dyn PlatformTimer>,
    );
    let claim = claim_for(&services, &Method::GET, &[]);
    let response = whole();
    let facts = OriginFacts::of(response.headers());
    let Delivery::Large(answer) =
        block_on(claim.deliver(&services, &identity(), Answered::Origin(facts), response))
    else {
        panic!("should admit the source to the large lane");
    };
    let url = url::Url::parse(FETCHED_URL).expect("should parse the fetched address");
    let headers = HeaderMap::new();
    let pinned = PinnedOrigin {
        url: &url,
        headers: &headers,
        backend_name: "origin",
        auth: None,
        unsigned_headers: &[],
        response_header_rewrite: None,
    };

    // Keep the policy future unresolved while the deterministic large timer
    // expires. A missing namespace race must fail here, never hang the test.
    let response = claim
        .deliver_large(&services, &identity(), pinned, answer)
        .now_or_never()
        .expect("should return unsigned when the large budget expires during namespace resolution");
    let (response, body) = collect(response);

    assert_eq!(body, Some(source()), "should preserve the original body");
    assert_eq!(header_of(&response, "etag"), Some(ETAG_V1));
    assert_eq!(header_of(&response, "link"), None);
    assert_eq!(header_of(&response, "repr-digest"), None);
    assert!(origin.requests().is_empty(), "should never start hashing");
    assert_eq!(signer.attempts(), 0, "should never start signing");
    assert!(signer.jobs().is_empty());
    let requested = timer.requested.lock().expect("should lock timer requests");
    assert_eq!(requested.len(), 1, "should race the namespace once");
    assert!(
        requested[0] > Duration::ZERO && requested[0] <= Duration::from_secs(5),
        "should use the large budget rather than the policy's own hold limit"
    );
    drop(namespace_ready);
}

#[test]
fn an_exhausted_large_budget_starts_no_hash_or_signing() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::Sign);
    let services = services_with_timer(&signer, &origin, Arc::new(ElapsedTimer::default()));
    let load = LargeLoad {
        signer: Arc::clone(&signer) as Arc<dyn MediaSigner>,
        services,
        url: url::Url::parse(FETCHED_URL).expect("should parse the fetched address"),
        headers: HeaderMap::new(),
        backend_name: "origin".to_owned(),
        auth: None,
        unsigned_headers: Vec::new(),
        version: SourceVersion::new(ETAG_V1, to_u64(source().len()))
            .expect("should pin the test version"),
        reader_url: READER_URL.to_owned(),
        content_type: "image/png".to_owned(),
        references: LargeReferences::of(&identity(), false),
        namespace: "policy-1".to_owned(),
        budget: LargeBudget {
            started: Instant::now(),
            total: Duration::ZERO,
        },
        keep_for: Duration::from_secs(600),
    };

    let loaded = block_on(load.run());

    assert!(
        matches!(
            decode_kept_outcome(&loaded.bytes),
            Some(KeptOutcome::Declined)
        ),
        "should keep an unsigned outcome"
    );
    assert!(origin.requests().is_empty(), "should never start hashing");
    assert_eq!(signer.attempts(), 0, "should never start signing");
}

#[test]
fn a_timed_out_source_is_hashed_once_and_then_sent_as_it_came() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::Hang);
    let services = services_with_timer(&signer, &origin, Arc::new(ElapsedTimer::default()));
    let claim = claim_for(&services, &Method::GET, &[]);

    let (large, response) = run(&claim, &services, whole());
    let (response, body) = collect(response);
    assert!(large);
    assert!(
        signer.jobs().is_empty(),
        "a signer out of time signs nothing"
    );
    assert_eq!(body, Some(source()));
    assert_eq!(header_of(&response, "link"), None);
    assert_eq!(header_of(&response, "etag"), Some(ETAG_V1));
    let reads = origin.requests().len();

    let (_, response) = run(&claim, &services, whole());
    let (_, body) = collect(response);
    assert_eq!(body, Some(source()));
    assert_eq!(
        signer.attempts(),
        1,
        "a kept timeout should never call the signer again"
    );
    assert_eq!(
        origin.requests().len(),
        reads,
        "a kept timeout should never hash again"
    );
}

#[test]
fn a_version_change_before_publication_signs_nothing_and_sends_the_current_version() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    // The hash reads the first version; the signer's confirmation and the
    // fresh answer meet the second.
    origin.flip_after(1, &other_source(), ETAG_V2);
    let signer = LargeSigner::new(LargeBehavior::ConfirmThenSign);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[]);

    let (large, response) = run(&claim, &services, whole());
    let (response, body) = collect(response);

    assert!(large);
    assert!(signer.jobs().is_empty(), "should publish nothing");
    assert_eq!(signer.failures(), vec!["source_version_changed"]);
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "an unsigned answer is the origin's, not a refusal"
    );
    assert_eq!(
        body,
        Some(other_source()),
        "should send the version the origin now serves"
    );
    assert_eq!(
        header_of(&response, "etag"),
        Some(ETAG_V2),
        "should describe the bytes sent by their own validator"
    );
    assert_eq!(header_of(&response, "link"), None);
    assert_eq!(header_of(&response, "repr-digest"), None);
}

#[test]
fn a_fresh_fallback_is_never_cached_and_keeps_both_security_policies() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let caller_policy = "img-src 'self'";
    let fresh_policy = "default-src 'none'";
    // The same version answers afresh with a shorter lifetime and a
    // security policy of its own.
    origin.flip_after_with(
        1,
        &source(),
        ETAG_V1,
        vec![
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=5"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(fresh_policy),
            ),
        ],
    );
    let signer = LargeSigner::new(LargeBehavior::Refuse);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[]);
    let mut answer = whole();
    answer.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(caller_policy),
    );

    let (_, response) = run(&claim, &services, answer);
    let (response, body) = collect(response);

    assert_eq!(body, Some(source()));
    assert_eq!(header_of(&response, "etag"), Some(ETAG_V1));
    assert_eq!(
        header_of(&response, "cache-control"),
        Some(crate::cache_policy::NO_STORE_PRIVATE_CACHE_CONTROL),
        "should keep neither the first answer's lifetime nor the fresh one's"
    );
    let policies: Vec<_> = response
        .headers()
        .get_all(header::CONTENT_SECURITY_POLICY)
        .iter()
        .collect();
    assert_eq!(
        policies,
        vec![caller_policy, fresh_policy],
        "should add the fresh policy to the caller's, never replace it"
    );
}

#[test]
fn a_refused_source_that_changed_keeps_the_new_versions_provenance_and_privacy() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let origin_link = "<https://manifests.example.com/new>; rel=\"c2pa-manifest\"";
    // The hash reads the first version; the fresh answer meets a second
    // that names a manifest of its own and may be held by no shared cache.
    origin.flip_after_with(
        1,
        &other_source(),
        ETAG_V2,
        vec![
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, no-store"),
            ),
            (header::LINK, HeaderValue::from_static(origin_link)),
            (
                header::SET_COOKIE,
                HeaderValue::from_static("session=reader-1"),
            ),
        ],
    );
    let signer = LargeSigner::new(LargeBehavior::Refuse);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[]);
    let mut answer = whole();
    answer.headers_mut().insert(
        header::EXPIRES,
        HeaderValue::from_static("Wed, 21 Oct 2026 07:28:00 GMT"),
    );
    answer
        .headers_mut()
        .insert(header::AGE, HeaderValue::from_static("30"));

    let (_, response) = run(&claim, &services, answer);
    let (response, body) = collect(response);

    assert_eq!(signer.attempts(), 1);
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body,
        Some(other_source()),
        "should send the version the origin now serves"
    );
    assert_eq!(header_of(&response, "etag"), Some(ETAG_V2));
    let links: Vec<_> = response.headers().get_all(header::LINK).iter().collect();
    assert_eq!(
        links,
        vec![origin_link],
        "should keep the new version's manifest link"
    );
    assert_eq!(
        header_of(&response, "cache-control"),
        Some(crate::cache_policy::NO_STORE_PRIVATE_CACHE_CONTROL),
        "should never send the first version's shared cache policy with other bytes"
    );
    assert_eq!(header_of(&response, "expires"), None);
    assert_eq!(header_of(&response, "age"), None);
    assert_eq!(
        header_of(&response, "set-cookie"),
        None,
        "should pass on no cookie the caller's handling never saw"
    );
    assert_eq!(header_of(&response, "repr-digest"), None);
}

#[test]
fn a_fresh_fallback_keeps_redirect_challenge_and_retry_response_semantics() {
    for (status, name, value) in [
        (
            StatusCode::TEMPORARY_REDIRECT,
            header::LOCATION,
            "/media/moved",
        ),
        (
            StatusCode::UNAUTHORIZED,
            header::WWW_AUTHENTICATE,
            "Bearer realm=\"media\"",
        ),
        (StatusCode::SERVICE_UNAVAILABLE, header::RETRY_AFTER, "60"),
    ] {
        let origin = RangeOrigin::new(&source(), ETAG_V1);
        origin.flip_after_with(
            1,
            &source(),
            ETAG_V1,
            vec![(name.clone(), HeaderValue::from_static(value))],
        );
        origin
            .state
            .lock()
            .expect("should lock origin")
            .status_after = Some((1, status));
        let signer = LargeSigner::new(LargeBehavior::Refuse);
        let services = services_for(&signer, &origin);
        let claim = claim_for(&services, &Method::GET, &[]);
        let (_, response) = run(&claim, &services, whole());
        let (response, _) = collect(response);

        assert_eq!(response.status(), status);
        assert_eq!(
            response.headers().get(&name),
            Some(&HeaderValue::from_static(value))
        );
        assert_eq!(
            header_of(&response, "cache-control"),
            Some(crate::cache_policy::NO_STORE_PRIVATE_CACHE_CONTROL),
        );
        assert_eq!(signer.attempts(), 1);
        assert_eq!(
            origin.requests().len(),
            2,
            "should not follow or retry the fresh response"
        );
    }
}

#[test]
fn a_fresh_publisher_redirect_uses_the_existing_origin_url_mapping_without_discarded_cookies() {
    for (location, expected) in [
        (
            "https://origin.example.com/media/moved",
            "https://www.example.com/media/moved",
        ),
        (FETCHED_URL, FETCHED_URL),
    ] {
        let origin = RangeOrigin::new(&source(), ETAG_V1);
        origin.flip_after_with(
            1,
            &source(),
            ETAG_V1,
            vec![
                (header::LOCATION, HeaderValue::from_static(location)),
                (
                    header::SET_COOKIE,
                    HeaderValue::from_static("session=new; Path=/"),
                ),
                (
                    header::CONTENT_LOCATION,
                    HeaderValue::from_static("https://origin.example.com/media/fresh"),
                ),
            ],
        );
        origin
            .state
            .lock()
            .expect("should lock origin")
            .status_after = Some((1, StatusCode::TEMPORARY_REDIRECT));
        let signer = LargeSigner::new(LargeBehavior::Refuse);
        let services = services_for(&signer, &origin);
        let claim = claim_for(&services, &Method::GET, &[]);
        let publisher = TestOrigin {
            response_header_rewrite: Some(crate::response_header_rewrite::OriginHeaderRewrite {
                origin_host: "origin.example.com",
                origin_host_header: "origin.example.com",
                origin_path: "",
                request_host: "www.example.com",
                request_scheme: "https",
                origin_scheme: "https",
                request_path_and_query: PATH,
                request_is_get_or_head: true,
            }),
            ..TestOrigin::plain()
        };
        let mut initial = whole();
        initial
            .headers_mut()
            .insert(header::CONTENT_LOCATION, HeaderValue::from_static("/stale"));
        let (_, response) = run_through(&claim, &services, &publisher, initial);
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            response.headers().get(header::LOCATION),
            Some(&HeaderValue::from_static(expected)),
            "a discarded cookie must not justify redirecting to the same serving URL",
        );
        assert!(!response.headers().contains_key(header::SET_COOKIE));
        assert_eq!(
            response.headers().get(header::CONTENT_LOCATION),
            Some(&HeaderValue::from_static(
                "https://www.example.com/media/fresh"
            )),
        );
    }
}

#[test]
fn a_version_change_after_signing_is_refused_rather_than_mixed() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    // The hash reads the first version; the signed delivery meets the
    // second.
    origin.flip_after(1, &other_source(), ETAG_V2);
    let signer = LargeSigner::new(LargeBehavior::Sign);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[]);

    let (_, response) = run(&claim, &services, whole());
    let (response, body) = collect(response);

    assert_eq!(signer.jobs().len(), 1);
    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "should never send other bytes under the signed version's headers"
    );
    assert_eq!(header_of(&response, "link"), None);
    assert_eq!(body, Some(Vec::new()));
}

#[test]
fn a_cut_hash_read_signs_nothing_and_is_never_resumed() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    origin.cut_next_at(23);
    let signer = LargeSigner::new(LargeBehavior::Sign);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[]);

    let (large, response) = run(&claim, &services, whole());
    let (response, body) = collect(response);

    assert!(large);
    assert_eq!(signer.attempts(), 0, "should sign nothing from a cut read");
    assert_eq!(
        origin.ranges(),
        vec![Some((0, 63)), None],
        "should neither resume nor restart the hash, only ask the origin afresh once as the reader asked"
    );
    assert_eq!(
        body,
        Some(source()),
        "should deliver the origin's fresh answer"
    );
    assert_eq!(
        header_of(&response, "link"),
        None,
        "should carry no manifest"
    );
    assert_eq!(header_of(&response, "etag"), Some(ETAG_V1));

    let (_, response) = run(&claim, &services, whole());
    let (_, body) = collect(response);
    assert_eq!(body, Some(source()));
    assert_eq!(
        origin.requests().len(),
        2,
        "a kept failure should never hash again"
    );

    let origin = RangeOrigin::new(&source(), ETAG_V1);
    origin.cut_next_at(23);
    origin.flip_after(1, &other_source(), ETAG_V2);
    let signer = LargeSigner::new(LargeBehavior::Sign);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[]);
    let (_, response) = run(&claim, &services, whole());
    let (response, body) = collect(response);
    assert_eq!(signer.attempts(), 0);
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "an unsigned answer is the origin's, not a refusal"
    );
    assert_eq!(body, Some(other_source()));
    assert_eq!(header_of(&response, "etag"), Some(ETAG_V2));
    assert_eq!(header_of(&response, "link"), None);
}

#[test]
fn a_source_no_public_address_names_is_never_hashed() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::Sign);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[]);
    let queried = TestOrigin {
        identity: VariantIdentity {
            fetched_url: QUERY_FETCHED_URL,
            reader_query_free: false,
            ..identity()
        },
        ..TestOrigin::plain()
    };

    for _ in 0..2 {
        let (_, response) = run_through(&claim, &services, &queried, whole());
        let (response, body) = collect(response);
        assert_eq!(
            body,
            Some(source()),
            "should send the origin's answer as it came"
        );
        assert_eq!(header_of(&response, "etag"), Some(ETAG_V1));
        assert_eq!(header_of(&response, "link"), None);
    }
    assert!(
        origin.requests().is_empty(),
        "should never read a source no public address names"
    );
    assert_eq!(signer.attempts(), 0);

    let reader_named = TestOrigin {
        identity: VariantIdentity {
            fetched_url: QUERY_FETCHED_URL,
            ..identity()
        },
        ..TestOrigin::plain()
    };
    let _ = collect(run_through(&claim, &services, &reader_named, whole()).1);
    let jobs = signer.jobs();
    assert_eq!(
        jobs[0].source_reference, None,
        "should never name a fetched address with a query"
    );
    assert_eq!(
        jobs[0].reader_reference.as_deref(),
        Some(READER_URL),
        "should name the reader's query-free address"
    );
}

#[test]
fn a_signed_origin_address_is_never_named_and_its_reads_carry_no_reader_header() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::ConfirmThenSign);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[]);
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("origin.example.com"));
    headers.insert(
        header::REFERER,
        HeaderValue::from_static("https://www.example.com/page"),
    );
    headers.insert(
        header::IF_NONE_MATCH,
        HeaderValue::from_static("\"reader\""),
    );
    headers.insert("x-test-origin-key", HeaderValue::from_static("from-reader"));
    let signed = TestOrigin {
        headers,
        auth: Some(OriginSigning::new(Arc::new(TestOriginSigner), None)),
        unsigned_headers: vec![header::REFERER],
        ..TestOrigin::plain()
    };

    let (_, response) = run_through(&claim, &services, &signed, whole());
    let (response, body) = collect(response);

    let jobs = signer.jobs();
    assert_eq!(
        jobs[0].source_reference, None,
        "should never name an address core signs"
    );
    assert_eq!(jobs[0].reader_reference.as_deref(), Some(READER_URL));
    assert_eq!(body, Some(source()));
    assert_eq!(header_of(&response, "link"), Some(MANIFEST_LINK));
    let requests = origin.requests();
    assert_eq!(requests.len(), 3, "should hash, confirm and deliver");
    for request in &requests {
        assert_eq!(
            request
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some(ORIGIN_SIGNATURE),
            "should sign every pinned read afresh"
        );
        assert!(
            request.get(header::REFERER).is_none(),
            "should send no reader header a signed read must not carry"
        );
        assert!(
            request.get(header::IF_NONE_MATCH).is_none(),
            "should send none of the reader's conditions"
        );
        assert!(
            request.get("x-test-origin-key").is_none(),
            "should send no header in the signer's namespace"
        );
        assert_eq!(
            request
                .get(header::IF_MATCH)
                .and_then(|value| value.to_str().ok()),
            Some(ETAG_V1)
        );
    }

    let queried_reader = TestOrigin {
        identity: VariantIdentity {
            reader_query_free: false,
            ..identity()
        },
        auth: Some(OriginSigning::new(Arc::new(TestOriginSigner), None)),
        ..TestOrigin::plain()
    };
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::Sign);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[]);
    let _ = collect(run_through(&claim, &services, &queried_reader, whole()).1);
    assert!(
        origin.requests().is_empty(),
        "should decline before hashing when only the signed address could name the source"
    );
    assert_eq!(signer.attempts(), 0);
}

#[test]
fn an_answer_whose_outcome_cannot_be_kept_is_never_hashed() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::Sign);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[]);
    for cache_control in ["no-cache", "public"] {
        let mut answer = whole();
        answer.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(cache_control),
        );
        let (large, response) = run(&claim, &services, answer);
        let (_, body) = collect(response);
        assert!(!large, "{cache_control}: should not enter the large lane");
        assert_eq!(body, Some(source()));
    }

    let keeps_nothing =
        build_services_with_http_client(Arc::clone(&origin) as Arc<dyn PlatformHttpClient>)
            .with_media_signer(Some(Arc::clone(&signer) as Arc<dyn MediaSigner>))
            .with_timer(Arc::new(StoppedTimer));
    let claim = claim_for(&keeps_nothing, &Method::GET, &[]);
    let (large, response) = run(&claim, &keeps_nothing, whole());
    let (_, body) = collect(response);
    assert!(
        !large,
        "should not enter the large lane where nothing is kept"
    );
    assert_eq!(body, Some(source()));

    assert!(
        origin.requests().is_empty(),
        "should never read the origin again"
    );
    assert_eq!(signer.attempts(), 0);
}

#[test]
fn an_origin_manifest_link_is_kept_rather_than_competed_with() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::Sign);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[]);
    let origin_link = "<https://manifests.example.com/own>; rel=\"c2pa-manifest\"";
    let mut answer = whole();
    answer
        .headers_mut()
        .insert(header::LINK, HeaderValue::from_static(origin_link));

    let (large, response) = run(&claim, &services, answer);
    let (response, body) = collect(response);

    assert!(
        !large,
        "should not sign a source that names its own manifest"
    );
    assert_eq!(body, Some(source()));
    let links: Vec<_> = response.headers().get_all(header::LINK).iter().collect();
    assert_eq!(
        links,
        vec![origin_link],
        "should keep only the origin's manifest link"
    );
    assert!(origin.requests().is_empty());
    assert_eq!(signer.attempts(), 0);
}

#[test]
fn a_warm_not_modified_naming_an_origin_manifest_gets_no_kept_outcome() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::Sign);
    let services = services_for(&signer, &origin);
    let claim = claim_for(&services, &Method::GET, &[]);
    let _ = collect(run(&claim, &services, whole()).1);
    assert_eq!(
        signer.jobs().len(),
        1,
        "should keep a signed outcome for the version"
    );

    let (_, plain) = run(&claim, &services, not_modified(ETAG_V1));
    assert_eq!(
        header_of(&plain, "link"),
        Some(MANIFEST_LINK),
        "the kept outcome decorates a 304 without a link of the origin's"
    );

    let origin_link = "<https://manifests.example.com/own>; rel=\"c2pa-manifest\"";
    let mut linked = not_modified(ETAG_V1);
    linked
        .headers_mut()
        .insert(header::LINK, HeaderValue::from_static(origin_link));
    let (_, response) = run(&claim, &services, linked);

    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    let links: Vec<_> = response.headers().get_all(header::LINK).iter().collect();
    assert_eq!(
        links,
        vec![origin_link],
        "should keep the origin's manifest link alone"
    );
    assert_eq!(header_of(&response, "repr-digest"), None);
    assert_eq!(header_of(&response, "etag"), Some(ETAG_V1));
    assert_eq!(signer.attempts(), 1, "should never sign again");
}

#[test]
fn a_source_without_a_strong_tag_is_never_given_a_version() {
    for etag in ["W/\"v1\"", ""] {
        let origin = RangeOrigin::new(&source(), ETAG_V1);
        let signer = LargeSigner::new(LargeBehavior::Sign);
        let services = services_for(&signer, &origin);
        let claim = claim_for(&services, &Method::GET, &[]);
        let mut answer = whole();
        if etag.is_empty() {
            answer.headers_mut().remove(header::ETAG);
        } else {
            answer
                .headers_mut()
                .insert(header::ETAG, HeaderValue::from_static("W/\"v1\""));
        }

        let (large, response) = run(&claim, &services, answer);
        let (response, body) = collect(response);

        assert!(!large, "{etag:?}: should not enter the large lane");
        assert_eq!(signer.attempts(), 0);
        assert!(
            origin.requests().is_empty(),
            "{etag:?}: should not ask the origin again"
        );
        assert_eq!(
            body,
            Some(source()),
            "{etag:?}: should send the origin's answer as it came"
        );
        assert_eq!(header_of(&response, "link"), None);
    }
}

#[test]
fn a_pinned_read_refuses_another_version_or_range() {
    let origin = RangeOrigin::new(&source(), ETAG_V1);
    let signer = LargeSigner::new(LargeBehavior::Sign);
    let services = services_for(&signer, &origin);
    let url = url::Url::parse(FETCHED_URL).expect("should parse the fetched address");
    let headers = HeaderMap::new();
    let version = SourceVersion::new(ETAG_V1, 64).expect("should take a strong tag");
    let reader = VersionedMediaSource::new(&services, &url, &headers, "origin", version);

    let mut part = [0_u8; 8];
    block_on(reader.read_exact_at(8, &mut part)).expect("should read the pinned range");
    assert_eq!(part.to_vec(), source()[8..16].to_vec());
    assert_eq!(
        block_on(reader.read_exact_at(60, &mut part)),
        Err(MediaSourceError::OutOfBounds),
        "should refuse a read past the version before asking"
    );

    origin.answer_wrong_content_range();
    assert_eq!(
        block_on(reader.read_exact_at(8, &mut part)),
        Err(MediaSourceError::InvalidAnswer {
            reason: "pinned_read_content_range"
        })
    );

    origin.flip_after(0, &other_source(), ETAG_V2);
    assert_eq!(
        block_on(reader.confirm_current()),
        Err(MediaSourceError::VersionChanged)
    );
    assert!(
        SourceVersion::new("W/\"v1\"", 64).is_none(),
        "a weak tag names no version"
    );
}
