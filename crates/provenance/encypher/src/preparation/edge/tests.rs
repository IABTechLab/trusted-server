//! The delivery instance's preparation routes, driven through their real
//! router with a scripted transport only at the network boundary: the
//! loopback reader pipeline, the Encypher API and the preparation worker.
//!
//! Every refusal here is one a forged or stale publication must meet before
//! anything is stored, and most must meet it before the API is asked
//! anything. The real probe cases run the fresh probe build the parent
//! provides, signed with a test release key, against receipts the candidate
//! API produced; see [`real_probe_receipts_publish_only_the_exact_output`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use edgezero_core::key_value_store::KvPage;
use futures::executor::block_on;
use http::HeaderMap;
use serde_json::Value;
use trusted_server_core::html_processor::test_support::page_settings;
use trusted_server_core::platform::test_support::build_services_with_http_client;
use trusted_server_core::platform::{
    PlatformError, PlatformHttpClient, PlatformKvStore, PlatformPendingRequest, PlatformResponse,
    PlatformSelectResult,
};

use super::*;
use crate::preparation::pipeline::{
    PREPARED_PROTOCOL, PlannedMedia, PrepareRequest, PreparedSourceRef, ProbeRelease,
    ProbeSelection, StructureChunk, TextCacheScope, TextSignRequest,
};

const TOKEN: &str = "cwk_test_worker_token_value";
const POLICY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER_POLICY: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const PUBLICATION: &str = "publication-secret-for-tests";
const SIGNAL: &str = "signal-secret-for-tests";
const HOST: &str = "publisher.example.com";
const PLAN_ID: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
const NONCE: &str = "0123456789abcdef0123456789abcdef";
const MARKER: &str = "\u{E0100}\u{E0101}\u{FE01}";
const SOURCE: &[u8] = b"\x89PNG\r\n\x1a\n-source-pixels";
const OTHER_SOURCE: &[u8] = b"\x89PNG\r\n\x1a\n-other-pixels!";
const ETAG: &str = "\"v1\"";
/// Core's configured-origin identity of the media route a large source is
/// published from, and of the same route after an operator remapped it to
/// another folder of the same origin host.
const CONFIGURED_SOURCE: &str = "https://publisher.example.com/library-a\0/media/";
const REMAPPED_SOURCE: &str = "https://publisher.example.com/library-b\0/media/";

// --- doubles at the network and storage boundaries ------------------------

/// One request the scripted transport received.
#[derive(Debug, Clone)]
struct Seen {
    method: Method,
    uri: String,
    headers: HeaderMap,
    body: Bytes,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

type Responder = Box<dyn Fn(&Seen) -> Response<EdgeBody> + Send + Sync>;

/// A transport that answers each request from what it asked and records
/// every request.
struct ScriptedHttp {
    responder: Responder,
    seen: Mutex<Vec<Seen>>,
}

impl ScriptedHttp {
    fn new(responder: impl Fn(&Seen) -> Response<EdgeBody> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            responder: Box::new(responder),
            seen: Mutex::new(Vec::new()),
        })
    }

    /// A transport that refuses everything, for routes that must refuse
    /// before reaching any network.
    fn refusing() -> Arc<Self> {
        Self::new(|_| answer(500, Vec::new(), &[]))
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("should lock requests").clone()
    }

    fn api_calls(&self) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|seen| seen.uri.starts_with("https://api.example.com"))
            .collect()
    }
}

#[async_trait(?Send)]
impl PlatformHttpClient for ScriptedHttp {
    async fn send(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        let (parts, body) = request.request.into_parts();
        let body = body
            .into_bytes_bounded(64 * 1024 * 1024)
            .await
            .expect("should read the request body");
        let seen = Seen {
            method: parts.method,
            uri: parts.uri.to_string(),
            headers: parts.headers,
            body,
        };
        let response = (self.responder)(&seen);
        self.seen.lock().expect("should lock requests").push(seen);
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

/// An HTTP answer of `status` with `body` and `headers`.
fn answer(status: u16, body: Vec<u8>, headers: &[(&str, String)]) -> Response<EdgeBody> {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(*name, value.as_str());
    }
    builder
        .body(EdgeBody::from(body))
        .expect("should build an answer")
}

/// A runtime KV store held in memory, whose reads of keys starting with
/// `failing_prefix` fail.
#[derive(Default)]
struct MemoryKv {
    entries: Mutex<HashMap<String, Bytes>>,
    failing_prefix: Option<&'static str>,
}

impl MemoryKv {
    fn failing(prefix: &'static str) -> Self {
        Self {
            failing_prefix: Some(prefix),
            ..Self::default()
        }
    }

    fn get(&self, key: &str) -> Option<Bytes> {
        self.entries.lock().expect("should lock").get(key).cloned()
    }

    fn put(&self, key: &str, value: &impl serde::Serialize) {
        self.entries.lock().expect("should lock").insert(
            key.to_owned(),
            Bytes::from(serde_json::to_vec(value).expect("should encode")),
        );
    }
}

#[async_trait(?Send)]
impl PlatformKvStore for MemoryKv {
    async fn get_bytes(&self, key: &str) -> Result<Option<Bytes>, KvError> {
        if self
            .failing_prefix
            .is_some_and(|prefix| key.starts_with(prefix))
        {
            return Err(KvError::Unavailable);
        }
        Ok(self.get(key))
    }

    async fn put_bytes(&self, key: &str, value: Bytes) -> Result<(), KvError> {
        self.entries
            .lock()
            .expect("should lock")
            .insert(key.to_owned(), value);
        Ok(())
    }

    async fn put_bytes_with_ttl(
        &self,
        key: &str,
        value: Bytes,
        _ttl: Duration,
    ) -> Result<(), KvError> {
        self.put_bytes(key, value).await
    }

    async fn delete(&self, key: &str) -> Result<(), KvError> {
        self.entries.lock().expect("should lock").remove(key);
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

// --- the deployment under test --------------------------------------------

fn config_value(domain: &str, public_host: &str, routes: &[&str]) -> Value {
    json!({
        "api_base_url": "https://api.example.com",
        "domain": domain,
        "worker_token": TOKEN,
        "max_large_media_bytes": 1_073_741_824_u64,
        "execution": "async_prepared",
        "preparation": {
            "worker_url": "https://prep.publisher.example.com",
            "loopback_url": format!("https://{public_host}"),
            "deployment_id": "test",
            "public_host": public_host,
            "signal_secret": SIGNAL,
            "publication_secret": PUBLICATION,
            "routes": routes,
            "variants": [{"name": "edition", "values": ["uk", "us"]}],
        },
    })
}

fn routes_for(config: Value) -> PreparationRoutes {
    let config: EncypherConfig = serde_json::from_value(config).expect("should parse config");
    PreparationRoutes::new(Arc::new(Signer::new(config, 5_000_000)))
}

fn routes() -> PreparationRoutes {
    routes_for(config_value(HOST, HOST, &["/news/", "/media/"]))
}

fn services(http: &Arc<ScriptedHttp>, kv: &Arc<MemoryKv>) -> RuntimeServices {
    build_services_with_http_client(Arc::clone(http) as Arc<dyn PlatformHttpClient>)
        .with_kv_store(Arc::clone(kv) as Arc<dyn PlatformKvStore>)
}

fn call(
    routes: &PreparationRoutes,
    services: &RuntimeServices,
    method: Method,
    path: &str,
    token: Option<&str>,
    body: Vec<u8>,
) -> Response<EdgeBody> {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("https://{HOST}{path}"));
    if let Some(token) = token {
        builder = builder.header(TOKEN_HEADER, token);
    }
    let request = builder
        .body(EdgeBody::from(body))
        .expect("should build the request");
    block_on(routes.route(request, services)).expect("should answer")
}

fn publish(
    routes: &PreparationRoutes,
    services: &RuntimeServices,
    token: &str,
    path_identity: &str,
    envelope: &ResultEnvelope,
) -> Response<EdgeBody> {
    call(
        routes,
        services,
        Method::PUT,
        &format!("{RESULTS_PREFIX}{path_identity}"),
        Some(token),
        serde_json::to_vec(envelope).expect("should encode the envelope"),
    )
}

fn body_of(response: Response<EdgeBody>) -> Bytes {
    block_on(response.into_body().into_bytes_bounded(64 * 1024 * 1024))
        .expect("should read the body")
}

/// The refusal code of `response`, after checking its status.
fn refused_with(response: Response<EdgeBody>, status: StatusCode) -> String {
    assert_eq!(response.status(), status, "should answer {status}");
    let body: Value = serde_json::from_slice(&body_of(response)).expect("should answer JSON");
    body["error"]["code"]
        .as_str()
        .expect("should name a code")
        .to_owned()
}

// --- envelopes ------------------------------------------------------------

fn page_url(path: &str) -> String {
    format!("https://{HOST}{path}")
}

fn media_request(carrier: Carrier, source: &[u8], mime: &str, path: &str) -> PrepareRequest {
    PrepareRequest {
        contract_version: 2,
        carrier_requirement: match carrier {
            Carrier::External => "external_allowed",
            Carrier::Embedded => "embedded_required",
        }
        .to_owned(),
        page_url: page_url(path),
        expected_policy_digest: POLICY.to_owned(),
        protocol: PREPARED_PROTOCOL.to_owned(),
        probe: ProbeSelection {
            sha256: "e".repeat(64),
            planner_id: "d".repeat(64),
        },
        source: PreparedSourceRef {
            reference: page_url(path),
            origin_host: HOST.to_owned(),
            mime: mime.to_owned(),
            sha256: sha256_hex(source),
            byte_length: u64::try_from(source.len()).expect("should fit"),
            reference_kind: "origin".to_owned(),
        },
        structure: vec![StructureChunk {
            offset: 0,
            b64: "AAAA".to_owned(),
        }],
    }
}

fn planned(request: PrepareRequest) -> PlannedMedia {
    PlannedMedia {
        digest: request.source.sha256.clone(),
        request,
        release: ProbeRelease {
            abi: 1,
            planner_id: "d".repeat(64),
            release: 3,
            sha256: "e".repeat(64),
            signature: "AAAA".to_owned(),
        },
        plan_id: PLAN_ID.to_owned(),
        geometry: json!({"disposition": "external"}),
        idempotency_key: "f".repeat(64),
    }
}

fn media_identity(routes: &PreparationRoutes, request: &PrepareRequest, variant: &str) -> String {
    let carrier = request.carrier();
    let mime = match carrier {
        Carrier::External => canonical_media_type(&request.source.mime),
        Carrier::Embedded => request.source.mime.clone(),
    };
    MediaIdentity {
        credential_context: routes.signer.credential_context(),
        policy_digest: &request.expected_policy_digest,
        page_url: &request.page_url,
        variant,
        carrier,
        mime: &mime,
        source_sha256: &request.source.sha256,
    }
    .digest()
}

fn envelope(identity: &str, item: EnvelopeItem) -> ResultEnvelope {
    ResultEnvelope {
        protocol: PREPARATION_PROTOCOL.to_owned(),
        deployment_id: "test".to_owned(),
        identity: identity.to_owned(),
        retirement_epoch: 0,
        variant: String::new(),
        item,
    }
}

fn media_envelope(routes: &PreparationRoutes) -> (String, ResultEnvelope) {
    let request = media_request(Carrier::External, SOURCE, "image/png", "/media/a.png");
    let identity = media_identity(routes, &request, "");
    let item = EnvelopeItem::Media {
        planned: planned(request),
        source_b64: STANDARD.encode(SOURCE),
    };
    (identity.clone(), envelope(&identity, item))
}

fn text_request(text: &str) -> TextSignRequest {
    TextSignRequest {
        text: text.to_owned(),
        page_url: page_url("/news/a"),
        document_title: Some("Harbor".to_owned()),
        boundary_selector: "article".to_owned(),
        expected_policy_digest: POLICY.to_owned(),
        cache_scope: TextCacheScope::ExactRequest,
    }
}

fn text_envelope(routes: &PreparationRoutes, request: TextSignRequest) -> (String, ResultEnvelope) {
    let identity = TextIdentity {
        credential_context: routes.signer.credential_context(),
        policy_digest: &request.expected_policy_digest,
        page_url: &request.page_url,
        variant: "",
        text: &request.text,
        document_title: request.document_title.as_deref(),
        boundary_selector: &request.boundary_selector,
    }
    .digest();
    let item = EnvelopeItem::Text {
        request,
        idempotency_key: "9".repeat(64),
    };
    (identity.clone(), envelope(&identity, item))
}

fn sign_answer(policy: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "success": true,
        "policy_digest": policy,
        "embedding_plan": {
            "index_unit": "codepoint",
            "operations": [{"insert_after_index": 5, "marker": MARKER}],
        },
        "manifest_url": "https://api.example.com/manifests/doc-1",
        "verification_url": "https://api.example.com/verify/doc-1",
        "document_id": "doc-1",
    }))
    .expect("should encode the sign answer")
}

const ARTICLE_TEXT: &str = "Harbor council votes on the ferry schedule for the coming season.";

// --- authentication, deployment and identity ------------------------------

#[test]
fn publication_needs_the_publication_scope_this_deployment_and_its_own_identity() {
    let routes = routes();
    let http = ScriptedHttp::refusing();
    let kv = Arc::new(MemoryKv::default());
    let services = services(&http, &kv);
    let (identity, genuine) = media_envelope(&routes);

    let signal_scope = publish(&routes, &services, SIGNAL, &identity, &genuine);
    assert_eq!(
        refused_with(signal_scope, StatusCode::UNAUTHORIZED),
        "unauthorized",
        "the reader-signal secret should not publish"
    );

    let mut other_deployment = genuine.clone();
    other_deployment.deployment_id = "other".to_owned();
    assert_eq!(
        refused_with(
            publish(
                &routes,
                &services,
                PUBLICATION,
                &identity,
                &other_deployment
            ),
            StatusCode::FORBIDDEN
        ),
        "wrong_deployment"
    );

    let elsewhere = "1".repeat(64);
    assert_eq!(
        refused_with(
            publish(&routes, &services, PUBLICATION, &elsewhere, &genuine),
            StatusCode::FORBIDDEN
        ),
        "wrong_deployment",
        "an envelope should publish only under the identity it names"
    );

    let mut unlisted = genuine.clone();
    unlisted.variant = "edition=fr".to_owned();
    assert_eq!(
        refused_with(
            publish(&routes, &services, PUBLICATION, &identity, &unlisted),
            StatusCode::FORBIDDEN
        ),
        "variant_not_admitted"
    );

    assert!(http.seen().is_empty(), "nothing should reach any network");
    assert!(
        kv.get(&result_key(&identity)).is_none(),
        "nothing should be stored"
    );
}

#[test]
fn a_retired_or_unreadable_marker_refuses_publication_before_the_api_is_asked() {
    let routes = routes();
    let (identity, genuine) = media_envelope(&routes);

    let http = ScriptedHttp::refusing();
    let kv = Arc::new(MemoryKv::default());
    kv.put(&marker_key(&identity), &RetirementMarker { epoch: 2 });
    let retired = publish(
        &routes,
        &services(&http, &kv),
        PUBLICATION,
        &identity,
        &genuine,
    );
    assert_eq!(refused_with(retired, StatusCode::CONFLICT), "retired");
    assert!(kv.get(&result_key(&identity)).is_none());

    let unreadable = Arc::new(MemoryKv::failing("encypher:prep:v1:retired:"));
    let refused = publish(
        &routes,
        &services(&http, &unreadable),
        PUBLICATION,
        &identity,
        &genuine,
    );
    assert_eq!(
        refused_with(refused, StatusCode::SERVICE_UNAVAILABLE),
        "marker_unavailable",
        "a marker that cannot be read should refuse, never publish"
    );
    assert!(unreadable.get(&result_key(&identity)).is_none());
    assert!(http.seen().is_empty(), "the API should never be asked");
}

#[test]
fn worker_supplied_identities_sources_and_carriers_are_never_taken_on_trust() {
    let routes = routes();
    let http = ScriptedHttp::refusing();
    let kv = Arc::new(MemoryKv::default());
    let services = services(&http, &kv);

    // An identity for one source over a request naming another.
    let request = media_request(Carrier::External, SOURCE, "image/png", "/media/a.png");
    let other = media_request(Carrier::External, OTHER_SOURCE, "image/png", "/media/a.png");
    let wrong_identity = media_identity(&routes, &other, "");
    let substituted = envelope(
        &wrong_identity,
        EnvelopeItem::Media {
            planned: planned(request.clone()),
            source_b64: STANDARD.encode(SOURCE),
        },
    );
    assert_eq!(
        refused_with(
            publish(
                &routes,
                &services,
                PUBLICATION,
                &wrong_identity,
                &substituted
            ),
            StatusCode::UNPROCESSABLE_ENTITY
        ),
        "identity_mismatch"
    );

    // Self-consistent identity and request, but bytes one byte off.
    let identity = media_identity(&routes, &request, "");
    let mut tampered = SOURCE.to_vec();
    *tampered.last_mut().expect("should have bytes") ^= 1;
    let one_byte = envelope(
        &identity,
        EnvelopeItem::Media {
            planned: planned(request.clone()),
            source_b64: STANDARD.encode(&tampered),
        },
    );
    assert_eq!(
        refused_with(
            publish(&routes, &services, PUBLICATION, &identity, &one_byte),
            StatusCode::UNPROCESSABLE_ENTITY
        ),
        "source_digest_mismatch"
    );

    // An embedded item whose source bytes are not the ones it names.
    let embedded = media_request(Carrier::Embedded, SOURCE, "image/png", "/news/a");
    let embedded_identity = media_identity(&routes, &embedded, "");
    let embedded_forgery = envelope(
        &embedded_identity,
        EnvelopeItem::Embedded {
            planned: planned(embedded.clone()),
            source_b64: STANDARD.encode(OTHER_SOURCE),
            output_b64: STANDARD.encode(OTHER_SOURCE),
        },
    );
    assert_eq!(
        refused_with(
            publish(
                &routes,
                &services,
                PUBLICATION,
                &embedded_identity,
                &embedded_forgery
            ),
            StatusCode::UNPROCESSABLE_ENTITY
        ),
        "source_digest_mismatch"
    );

    // An external plan presented as embedded, and the reverse.
    let as_embedded = envelope(
        &identity,
        EnvelopeItem::Embedded {
            planned: planned(request.clone()),
            source_b64: STANDARD.encode(SOURCE),
            output_b64: STANDARD.encode(SOURCE),
        },
    );
    assert_eq!(
        refused_with(
            publish(&routes, &services, PUBLICATION, &identity, &as_embedded),
            StatusCode::UNPROCESSABLE_ENTITY
        ),
        "carrier_mismatch"
    );
    let as_external = envelope(
        &embedded_identity,
        EnvelopeItem::Media {
            planned: planned(embedded),
            source_b64: STANDARD.encode(SOURCE),
        },
    );
    assert_eq!(
        refused_with(
            publish(
                &routes,
                &services,
                PUBLICATION,
                &embedded_identity,
                &as_external
            ),
            StatusCode::UNPROCESSABLE_ENTITY
        ),
        "carrier_mismatch"
    );

    // Addresses outside the claimed host or the configured routes.
    for path_or_host in [
        "https://evil.example.net/media/a.png",
        "https://publisher.example.com/private/a.png",
    ] {
        let mut outside = request.clone();
        outside.page_url = path_or_host.to_owned();
        let outside_identity = media_identity(&routes, &outside, "");
        let item = envelope(
            &outside_identity,
            EnvelopeItem::Media {
                planned: planned(outside),
                source_b64: STANDARD.encode(SOURCE),
            },
        );
        assert_eq!(
            refused_with(
                publish(&routes, &services, PUBLICATION, &outside_identity, &item),
                StatusCode::UNPROCESSABLE_ENTITY
            ),
            "page_url_outside_deployment",
            "{path_or_host} should be refused"
        );
    }

    assert!(
        http.seen().is_empty(),
        "every forgery should be refused before the API is asked"
    );
    assert!(
        kv.entries.lock().expect("should lock").is_empty(),
        "nothing should be stored"
    );
}

// --- text: recovered, never signed ----------------------------------------

#[test]
fn text_publication_refuses_missing_or_unknown_cache_scope_before_recovery() {
    let routes = routes();
    let http = ScriptedHttp::refusing();
    let kv = Arc::new(MemoryKv::default());
    let (identity, genuine) = text_envelope(&routes, text_request(ARTICLE_TEXT));
    for scope in [None, Some(json!("legacy")), Some(Value::Null)] {
        let mut body = serde_json::to_value(&genuine).expect("should encode an envelope");
        let request = body["item"]["request"]
            .as_object_mut()
            .expect("should carry a request");
        if let Some(scope) = scope {
            request.insert("cache_scope".to_owned(), scope);
        } else {
            request.remove("cache_scope");
        }
        let response = call(
            &routes,
            &services(&http, &kv),
            Method::PUT,
            &format!("{RESULTS_PREFIX}{identity}"),
            Some(PUBLICATION),
            serde_json::to_vec(&body).expect("should encode an unsupported request"),
        );
        assert_eq!(
            refused_with(response, StatusCode::BAD_REQUEST),
            "malformed_request"
        );
    }
    assert!(
        http.seen().is_empty(),
        "unsupported scope must never reach recovery"
    );
    assert!(kv.entries.lock().expect("should lock").is_empty());
}

#[test]
fn text_publication_refuses_old_cache_identity_and_substituted_title_or_url() {
    let routes = routes();
    let http = ScriptedHttp::refusing();
    let kv = Arc::new(MemoryKv::default());
    let request = text_request(ARTICLE_TEXT);
    let (identity, genuine) = text_envelope(&routes, request.clone());
    let text_hash = crate::media_api::sha256_hex(request.text.as_bytes());
    let old_identity = super::super::protocol::domain_digest(
        "encypher-preparation/1:text",
        &[
            routes.signer.credential_context().as_bytes(),
            request.expected_policy_digest.as_bytes(),
            request.page_url.as_bytes(),
            b"",
            super::super::protocol::TEXT_PROJECTION_VERSION.as_bytes(),
            text_hash.as_bytes(),
            b"1",
            b"Harbor",
            request.boundary_selector.as_bytes(),
        ],
    );
    let mut old = genuine.clone();
    old.identity = old_identity.clone();
    assert_eq!(
        refused_with(
            publish(
                &routes,
                &services(&http, &kv),
                PUBLICATION,
                &old_identity,
                &old
            ),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        "identity_mismatch",
        "even an exact-scoped envelope must not publish under the former cache domain"
    );
    for changed_request in [
        TextSignRequest {
            document_title: Some("Revised harbor title".to_owned()),
            ..request.clone()
        },
        TextSignRequest {
            page_url: page_url("/news/mirror"),
            ..request
        },
    ] {
        let (_, mut changed) = text_envelope(&routes, changed_request);
        changed.identity = identity.clone();
        assert_eq!(
            refused_with(
                publish(
                    &routes,
                    &services(&http, &kv),
                    PUBLICATION,
                    &identity,
                    &changed
                ),
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            "identity_mismatch",
            "title and URL cannot change under an existing prepared identity"
        );
    }
    assert!(
        http.seen().is_empty(),
        "stale identities must never reach recovery"
    );
    assert!(kv.entries.lock().expect("should lock").is_empty());
}
#[test]
fn a_text_result_is_recovered_never_signed_and_published_under_its_identity() {
    let routes = routes();
    let http = ScriptedHttp::new(|seen| {
        if seen.uri == "https://api.example.com/api/v1/public/cdn/sign/recover" {
            answer(200, sign_answer(POLICY), &[])
        } else {
            answer(500, Vec::new(), &[])
        }
    });
    let kv = Arc::new(MemoryKv::default());
    let request = text_request(ARTICLE_TEXT);
    let (identity, genuine) = text_envelope(&routes, request.clone());

    let response = publish(
        &routes,
        &services(&http, &kv),
        PUBLICATION,
        &identity,
        &genuine,
    );

    assert_eq!(response.status(), StatusCode::OK, "should publish");
    let calls = http.seen();
    assert_eq!(calls.len(), 1, "should make exactly one API call");
    let call = &calls[0];
    assert_eq!(call.method, Method::POST);
    assert_eq!(
        call.uri, "https://api.example.com/api/v1/public/cdn/sign/recover",
        "should recover, never call the sign route"
    );
    assert_eq!(
        call.header("idempotency-key"),
        Some("9".repeat(64).as_str())
    );
    assert_eq!(call.header("x-encypher-worker-token"), Some(TOKEN));
    let recovered: Value = serde_json::from_slice(&call.body).expect("should carry JSON");
    assert_eq!(recovered["cache_scope"], "exact_request");
    assert_eq!(
        serde_json::from_slice::<TextSignRequest>(&call.body).expect("should send the request"),
        request,
        "should recover exactly the frozen request"
    );
    let record: PublishedRecord = serde_json::from_slice(
        &kv.get(&result_key(&identity))
            .expect("should store the record"),
    )
    .expect("should store a record");
    assert_eq!(record.identity, identity);
    assert!(
        matches!(record.result, PublishedResult::Text { scalars, .. } if scalars == ARTICLE_TEXT.chars().count()),
        "should store the recovered plan for this exact text"
    );

    // A worker that changes the text under the same identity is refused.
    let (_, mut changed) = text_envelope(
        &routes,
        text_request("A different article entirely, long enough."),
    );
    changed.identity = identity.clone();
    assert_eq!(
        refused_with(
            publish(
                &routes,
                &services(&http, &kv),
                PUBLICATION,
                &identity,
                &changed
            ),
            StatusCode::UNPROCESSABLE_ENTITY
        ),
        "identity_mismatch"
    );
}

#[test]
fn a_text_result_the_api_no_longer_holds_or_holds_under_another_policy_is_refused() {
    let routes = routes();
    let (identity, genuine) = text_envelope(&routes, text_request(ARTICLE_TEXT));
    for (status, body, expected_status, code) in [
        (404, Vec::new(), StatusCode::GONE, "result_not_retained"),
        (
            200,
            sign_answer(OTHER_POLICY),
            StatusCode::CONFLICT,
            "policy_digest_mismatch",
        ),
    ] {
        let http = ScriptedHttp::new(move |_| answer(status, body.clone(), &[]));
        let kv = Arc::new(MemoryKv::default());
        let response = publish(
            &routes,
            &services(&http, &kv),
            PUBLICATION,
            &identity,
            &genuine,
        );
        assert_eq!(refused_with(response, expected_status), code);
        assert!(
            kv.get(&result_key(&identity)).is_none(),
            "{code}: nothing stored"
        );
        assert!(
            http.seen()
                .iter()
                .all(|seen| !seen.uri.ends_with("/cdn/sign")),
            "{code}: should never call the sign route"
        );
    }
}

// --- retirement markers ---------------------------------------------------

fn retire(
    routes: &PreparationRoutes,
    services: &RuntimeServices,
    token: &str,
    identity: &str,
    epoch: u64,
) -> Response<EdgeBody> {
    let marker = MarkerEnvelope {
        protocol: PREPARATION_PROTOCOL.to_owned(),
        deployment_id: "test".to_owned(),
        identity: identity.to_owned(),
        epoch,
    };
    call(
        routes,
        services,
        Method::PUT,
        &format!("{MARKERS_PREFIX}{identity}"),
        Some(token),
        serde_json::to_vec(&marker).expect("should encode the marker"),
    )
}

fn stored_marker(kv: &MemoryKv, identity: &str) -> RetirementMarker {
    serde_json::from_slice(
        &kv.get(&marker_key(identity))
            .expect("should store a marker"),
    )
    .expect("should store a marker")
}

#[test]
fn retirement_markers_are_monotonic_and_refuse_late_results() {
    let routes = routes();
    let http = ScriptedHttp::new(|_| answer(200, sign_answer(POLICY), &[]));
    let kv = Arc::new(MemoryKv::default());
    let services = services(&http, &kv);
    let (identity, genuine) = text_envelope(&routes, text_request(ARTICLE_TEXT));
    assert_eq!(
        publish(&routes, &services, PUBLICATION, &identity, &genuine).status(),
        StatusCode::OK
    );
    assert!(kv.get(&result_key(&identity)).is_some());

    assert_eq!(
        refused_with(
            retire(&routes, &services, SIGNAL, &identity, 3),
            StatusCode::UNAUTHORIZED
        ),
        "unauthorized",
        "the reader-signal secret should not retire"
    );
    let acknowledged = retire(&routes, &services, PUBLICATION, &identity, 3);
    assert_eq!(acknowledged.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&body_of(acknowledged)).expect("should answer JSON");
    assert_eq!(body["acknowledged"], json!(identity));
    assert_eq!(body["epoch"], json!(3));
    assert_eq!(stored_marker(&kv, &identity).epoch, 3);
    assert!(
        kv.get(&result_key(&identity)).is_none(),
        "should drop the retired result"
    );

    let older = retire(&routes, &services, PUBLICATION, &identity, 1);
    assert_eq!(older.status(), StatusCode::OK);
    assert_eq!(
        stored_marker(&kv, &identity).epoch,
        3,
        "a delayed older marker should never lower the epoch"
    );

    let calls_before = http.seen().len();
    let late = publish(&routes, &services, PUBLICATION, &identity, &genuine);
    assert_eq!(refused_with(late, StatusCode::CONFLICT), "retired");
    assert_eq!(
        http.seen().len(),
        calls_before,
        "a late result should be refused without asking the API"
    );
    assert!(kv.get(&result_key(&identity)).is_none());
    assert_eq!(
        stored_marker(&kv, &identity).epoch,
        3,
        "the marker should stand"
    );
}

// --- large media: the full source is read again ---------------------------

/// What the loopback reader pipeline answers for a large source.
#[derive(Clone)]
struct Origin {
    bytes: Arc<Vec<u8>>,
    etag: &'static str,
    facts: Option<SnapshotOrigin>,
    facts_secret: &'static str,
    content_type: &'static str,
}

impl Origin {
    fn honest(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Arc::new(bytes),
            etag: ETAG,
            facts: Some(SnapshotOrigin {
                status: 206,
                origin_host: HOST.to_owned(),
                origin_identity_sha256: sha256_hex(CONFIGURED_SOURCE.as_bytes()),
                ..SnapshotOrigin::default()
            }),
            facts_secret: PUBLICATION,
            content_type: "image/png",
        }
    }

    /// Answers a pinned range read the way the reader pipeline does, with
    /// facts authenticated for the capability's nonce; anything else, the
    /// API's recovery included, finds nothing.
    fn responder(self) -> impl Fn(&Seen) -> Response<EdgeBody> + Send + Sync + 'static {
        move |seen| {
            let Some(capability) = seen.header(CAPABILITY_HEADER) else {
                return answer(404, Vec::new(), &[]);
            };
            let nonce = capability.split('.').nth(2).expect("should carry a nonce");
            let range = seen
                .header("range")
                .and_then(|value| value.strip_prefix("bytes="))
                .and_then(|value| value.split_once('-'))
                .expect("should read a range");
            let first: usize = range.0.parse().expect("should parse");
            let last: usize = range.1.parse().expect("should parse");
            let mut headers = vec![
                (
                    "content-range",
                    format!("bytes {first}-{last}/{}", self.bytes.len()),
                ),
                ("etag", self.etag.to_owned()),
                ("content-type", self.content_type.to_owned()),
            ];
            if let Some(facts) = &self.facts {
                headers.push((FACTS_HEADER, facts_header(self.facts_secret, nonce, facts)));
            }
            answer(206, self.bytes[first..=last].to_vec(), &headers)
        }
    }
}

/// The production-signed release every real-probe fixture was planned with.
fn signed_release() -> ProbeRelease {
    serde_json::from_slice(include_bytes!("../../fixtures/prepared/descriptor.json"))
        .expect("should parse the production-signed descriptor")
}

/// A store already holding the signed release's verified artifact, as one
/// that loaded it before does, so a publication needs no API call to run it.
fn probe_kv() -> Arc<MemoryKv> {
    let kv = Arc::new(MemoryKv::default());
    kv.entries.lock().expect("should lock").insert(
        format!("{PROBE_KEY_PREFIX}:{}", signed_release().sha256),
        Bytes::from(fixture("probe.wasm")),
    );
    kv
}

/// Appends one PNG chunk of `kind` holding `data`, with its CRC.
fn png_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&u32::try_from(data.len()).expect("should fit").to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let mut crc = !0_u32;
    for byte in kind.iter().chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    png.extend_from_slice(&(!crc).to_be_bytes());
}

/// The retained real PNG with `chunks` inserted before its `IEND`.
fn png_with(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
    let base: &[u8] = include_bytes!("../../fixtures/prepared/external-png/source.png");
    let (body, end) = base.split_at(base.len() - 12);
    assert_eq!(&end[4..8], b"IEND", "the fixture should end with IEND");
    let mut png = body.to_vec();
    for (kind, data) in chunks {
        png_chunk(&mut png, kind, data);
    }
    png.extend_from_slice(end);
    png
}

/// A PNG of exactly `length` bytes: the real fixture with one private
/// chunk of sevens before its `IEND`, whose last data byte is `last`.
fn large_png(length: usize, last: u8) -> Vec<u8> {
    let base = png_with(&[]).len();
    let mut data = vec![7_u8; length - base - 12];
    *data.last_mut().expect("should have data") = last;
    let png = png_with(&[(b"prVt", data.as_slice())]);
    assert_eq!(png.len(), length, "should be exactly the asked length");
    png
}

/// An XMP packet naming a remote manifest with `dcterms:provenance`.
const PROVENANCE_XMP: &[u8] = br#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:dcterms="http://purl.org/dc/terms/" dcterms:provenance="https://manifests.example.com/original.c2pa"/></rdf:RDF></x:xmpmeta>"#;

/// The uncompressed `iTXt` data carrying `packet` as the PNG's XMP.
fn xmp_itxt(packet: &[u8]) -> Vec<u8> {
    let mut data = b"XML:com.adobe.xmp\0\0\0\0\0".to_vec();
    data.extend_from_slice(packet);
    data
}

fn large_envelope(routes: &PreparationRoutes, planned_source: &[u8]) -> (String, ResultEnvelope) {
    large_envelope_bound(
        routes,
        planned_source,
        &sha256_hex(CONFIGURED_SOURCE.as_bytes()),
    )
}

/// A large envelope whose worker says it read the configured source
/// `origin_identity_sha256` names, under an identity bound to it.
fn large_envelope_bound(
    routes: &PreparationRoutes,
    planned_source: &[u8],
    origin_identity_sha256: &str,
) -> (String, ResultEnvelope) {
    let release = signed_release();
    let mut request = media_request(
        Carrier::External,
        planned_source,
        "image/png",
        "/media/ferry.png",
    );
    request.probe = ProbeSelection {
        sha256: release.sha256.clone(),
        planner_id: release.planner_id.clone(),
    };
    let identity = LargeLocator {
        credential_context: routes.signer.credential_context(),
        policy_digest: POLICY,
        reader_url: &request.page_url,
        variant: "",
        origin_host: HOST,
        origin_identity_sha256,
        mime: "image/png",
        etag: ETAG,
        length: request.source.byte_length,
    }
    .digest();
    let mut planned = planned(request);
    planned.release = release;
    let item = EnvelopeItem::Large {
        planned,
        etag: ETAG.to_owned(),
        origin_identity_sha256: origin_identity_sha256.to_owned(),
    };
    (identity.clone(), envelope(&identity, item))
}

/// The pinned reads `seen` made, in order, as their `Range` headers.
fn pinned_ranges(seen: &[Seen]) -> Vec<String> {
    seen.iter()
        .filter(|seen| seen.header(CAPABILITY_HEADER).is_some())
        .map(|seen| seen.header("range").unwrap_or_default().to_owned())
        .collect()
}

/// Whether a `bytes=first-last` range is at most one pinned block.
fn within_block(range: &str) -> bool {
    range
        .strip_prefix("bytes=")
        .and_then(|range| range.split_once('-'))
        .and_then(|(first, last)| Some(last.parse::<u64>().ok()? - first.parse::<u64>().ok()?))
        .is_some_and(|span| span < PINNED_BLOCK_BYTES)
}

#[test]
fn large_publication_hashes_every_byte_of_the_pinned_version_before_the_api_is_asked() {
    let routes = routes();
    let length = usize::try_from(MAX_RANGE_BYTES).expect("should fit") + 10;
    let source = large_png(length, 0);

    // The worker's claimed digest is the source's, but the version the tag
    // names differs near its end, beyond the first range.
    let served = large_png(length, 1);
    let http = ScriptedHttp::new(Origin::honest(served).responder());
    let kv = probe_kv();
    let (identity, forged) = large_envelope(&routes, &source);
    let response = publish(
        &routes,
        &services(&http, &kv),
        PUBLICATION,
        &identity,
        &forged,
    );
    assert_eq!(
        refused_with(response, StatusCode::UNPROCESSABLE_ENTITY),
        "source_digest_mismatch"
    );
    assert!(http.api_calls().is_empty(), "the API should never be asked");
    assert!(kv.get(&result_key(&identity)).is_none());

    // An honest version passes the full read, and only then is the API's
    // receipt asked for; here it holds none.
    let http = ScriptedHttp::new(Origin::honest(source.clone()).responder());
    let kv = probe_kv();
    let (identity, genuine) = large_envelope(&routes, &source);
    let response = publish(
        &routes,
        &services(&http, &kv),
        PUBLICATION,
        &identity,
        &genuine,
    );
    assert_eq!(
        refused_with(response, StatusCode::GONE),
        "result_not_retained"
    );
    let seen = http.seen();
    let reads: Vec<&Seen> = seen
        .iter()
        .filter(|seen| seen.header(CAPABILITY_HEADER).is_some())
        .collect();
    let ranges = pinned_ranges(&seen);
    let (inspection, hashing) = ranges.split_at(ranges.len() - 2);
    assert_eq!(
        hashing,
        [
            format!("bytes=0-{}", MAX_RANGE_BYTES - 1),
            format!("bytes={}-{}", MAX_RANGE_BYTES, length - 1),
        ],
        "should read every byte in pinned ranges"
    );
    assert!(
        !inspection.is_empty() && inspection.iter().all(|range| within_block(range)),
        "the provenance inspection should read only blocks of the version, first"
    );
    for read in &reads {
        assert_eq!(read.uri, format!("https://{HOST}/media/ferry.png"));
        assert_eq!(
            read.header("if-match"),
            Some(ETAG),
            "should pin the version"
        );
        assert_eq!(read.header("accept-encoding"), Some("identity"));
        let capability = read.header(CAPABILITY_HEADER).expect("should carry it");
        assert!(
            verify_snapshot_capability(PUBLICATION, "/media/ferry.png", capability, unix_now())
                .is_some(),
            "should carry a capability for exactly this path"
        );
    }
    let last_read = seen
        .iter()
        .rposition(|seen| seen.header(CAPABILITY_HEADER).is_some())
        .expect("should read");
    let recover = seen
        .iter()
        .position(|seen| seen.uri.ends_with("/media/hash/recover"))
        .expect("should ask for the receipt");
    assert!(
        recover > last_read,
        "the receipt should be asked for only after the read"
    );
    assert!(kv.get(&result_key(&identity)).is_none());
}

#[test]
fn large_publication_reads_the_admitted_variant_not_the_query_free_resource() {
    let routes = routes();
    let length = usize::try_from(MAX_RANGE_BYTES).expect("should fit a range") + 10;
    let source_b = large_png(length, 7);
    let source_a = large_png(length, 8);
    assert_eq!(source_a.len(), source_b.len());
    assert_ne!(sha256_hex(&source_a), sha256_hex(&source_b));
    let base = Origin::honest(source_a).responder();
    let variant = Origin::honest(source_b.clone()).responder();
    let http = ScriptedHttp::new(move |seen| {
        if seen.uri == format!("https://{HOST}/media/ferry.png?edition=uk") {
            variant(seen)
        } else {
            base(seen)
        }
    });
    let kv = probe_kv();
    let (_, mut genuine) = large_envelope(&routes, &source_b);
    genuine.variant = "edition=uk".to_owned();
    let EnvelopeItem::Large {
        planned,
        etag,
        origin_identity_sha256,
    } = &genuine.item
    else {
        panic!("should have a large publication");
    };
    genuine.identity = LargeLocator {
        credential_context: routes.signer.credential_context(),
        policy_digest: POLICY,
        reader_url: &planned.request.page_url,
        variant: &genuine.variant,
        origin_host: HOST,
        origin_identity_sha256,
        mime: "image/png",
        etag,
        length: planned.request.source.byte_length,
    }
    .digest();
    let response = publish(
        &routes,
        &services(&http, &kv),
        PUBLICATION,
        &genuine.identity,
        &genuine,
    );
    assert_eq!(
        refused_with(response, StatusCode::GONE),
        "result_not_retained",
        "B's digest should pass before the absent API receipt refuses publication"
    );
    let seen = http.seen();
    let reads: Vec<&Seen> = seen
        .iter()
        .filter(|seen| seen.header(CAPABILITY_HEADER).is_some())
        .collect();
    let ranges = pinned_ranges(&seen);
    assert_eq!(
        ranges[ranges.len() - 2..],
        [
            format!("bytes=0-{}", MAX_RANGE_BYTES - 1),
            format!("bytes={}-{}", MAX_RANGE_BYTES, length - 1),
        ],
        "should hash all of B in pinned ranges"
    );
    for read in &reads {
        assert_eq!(
            read.uri,
            format!("https://{HOST}/media/ferry.png?edition=uk")
        );
        assert_eq!(read.header("if-match"), Some(ETAG));
        let capability = read.header(CAPABILITY_HEADER).expect("should carry it");
        assert!(
            verify_snapshot_capability(
                PUBLICATION,
                "/media/ferry.png?edition=uk",
                capability,
                unix_now(),
            )
            .is_some(),
            "the read capability should bind B's exact path and variant"
        );
        assert!(
            verify_snapshot_capability(PUBLICATION, "/media/ferry.png", capability, unix_now())
                .is_none(),
            "B's capability should not authorize A"
        );
    }
    let recover = seen
        .iter()
        .position(|request| request.uri.ends_with("/media/hash/recover"))
        .expect("should ask for the receipt after hashing B");
    let last_read = seen
        .iter()
        .rposition(|request| request.header(CAPABILITY_HEADER).is_some())
        .expect("should read B");
    assert!(recover > last_read);
    assert!(kv.get(&result_key(&genuine.identity)).is_none());
}

#[test]
fn large_publication_refuses_reads_the_origin_facts_do_not_admit() {
    let routes = routes();
    let source = vec![7_u8; 1024];
    let mut private = Origin::honest(source.clone());
    private.facts = Some(SnapshotOrigin {
        private: true,
        ..private.facts.clone().expect("should have facts")
    });
    let mut unobserved = Origin::honest(source.clone());
    unobserved.facts = None;
    let mut forged_facts = Origin::honest(source.clone());
    forged_facts.facts_secret = "an-origin-guessing-the-secret";
    let mut elsewhere = Origin::honest(source.clone());
    elsewhere.facts = Some(SnapshotOrigin {
        origin_host: "cdn.other.example".to_owned(),
        ..elsewhere.facts.clone().expect("should have facts")
    });
    // The same host answers from another configured source, or reports none.
    let mut remapped = Origin::honest(source.clone());
    remapped.facts = Some(SnapshotOrigin {
        origin_identity_sha256: sha256_hex(REMAPPED_SOURCE.as_bytes()),
        ..remapped.facts.clone().expect("should have facts")
    });
    let mut unbound = Origin::honest(source.clone());
    unbound.facts = Some(SnapshotOrigin {
        origin_identity_sha256: String::new(),
        ..unbound.facts.clone().expect("should have facts")
    });
    let mut encoded = Origin::honest(source.clone());
    encoded.facts = Some(SnapshotOrigin {
        content_encoding: "gzip".to_owned(),
        ..encoded.facts.clone().expect("should have facts")
    });
    let mut provenance = Origin::honest(source.clone());
    provenance.facts = Some(SnapshotOrigin {
        manifest_link: true,
        ..provenance.facts.clone().expect("should have facts")
    });
    let mut retagged = Origin::honest(source.clone());
    retagged.etag = "\"v2\"";
    let mut retyped = Origin::honest(source.clone());
    retyped.content_type = "image/webp";

    for (origin, status, code) in [
        (private, StatusCode::FORBIDDEN, "source_private"),
        (
            unobserved,
            StatusCode::UNPROCESSABLE_ENTITY,
            "snapshot_unobserved",
        ),
        (
            forged_facts,
            StatusCode::UNPROCESSABLE_ENTITY,
            "snapshot_unobserved",
        ),
        (
            elsewhere,
            StatusCode::UNPROCESSABLE_ENTITY,
            "source_origin_mismatch",
        ),
        (
            remapped,
            StatusCode::UNPROCESSABLE_ENTITY,
            "source_identity_mismatch",
        ),
        (
            unbound,
            StatusCode::UNPROCESSABLE_ENTITY,
            "source_identity_mismatch",
        ),
        (encoded, StatusCode::UNPROCESSABLE_ENTITY, "content_encoded"),
        (
            provenance,
            StatusCode::UNPROCESSABLE_ENTITY,
            "source_has_provenance_link",
        ),
        (
            retagged,
            StatusCode::UNPROCESSABLE_ENTITY,
            "source_version_changed",
        ),
        (
            retyped,
            StatusCode::UNPROCESSABLE_ENTITY,
            "source_type_mismatch",
        ),
    ] {
        let http = ScriptedHttp::new(origin.responder());
        let kv = probe_kv();
        let (identity, genuine) = large_envelope(&routes, &source);
        let response = publish(
            &routes,
            &services(&http, &kv),
            PUBLICATION,
            &identity,
            &genuine,
        );
        assert_eq!(refused_with(response, status), code);
        assert!(
            http.api_calls().is_empty(),
            "{code}: the API should never be asked"
        );
        assert!(
            kv.get(&result_key(&identity)).is_none(),
            "{code}: nothing stored"
        );
    }
}

/// Publication binds the configured source, not only the host: a route an
/// operator remapped serves another resource under the same public URL,
/// host, entity tag, type and length. A result for source A is refused
/// against authenticated facts for source B, whether B answers every read
/// or only the reads after the first, before the API is asked or anything
/// is stored; the genuine source A still reaches receipt recovery.
#[test]
fn large_publication_refuses_a_version_another_configured_source_serves() {
    let routes = routes();
    let length = usize::try_from(MAX_RANGE_BYTES).expect("should fit") + 10;
    let source = large_png(length, 0);
    let configured = sha256_hex(CONFIGURED_SOURCE.as_bytes());
    let remapped = sha256_hex(REMAPPED_SOURCE.as_bytes());
    let mut served_by_b = Origin::honest(source.clone());
    served_by_b.facts = Some(SnapshotOrigin {
        origin_identity_sha256: remapped.clone(),
        ..served_by_b.facts.clone().expect("should have facts")
    });

    // B answers every read: the first pinned read refuses A's result.
    let http = ScriptedHttp::new(served_by_b.clone().responder());
    let kv = probe_kv();
    let (identity, genuine) = large_envelope(&routes, &source);
    let response = publish(
        &routes,
        &services(&http, &kv),
        PUBLICATION,
        &identity,
        &genuine,
    );
    assert_eq!(
        refused_with(response, StatusCode::UNPROCESSABLE_ENTITY),
        "source_identity_mismatch"
    );
    assert_eq!(
        pinned_ranges(&http.seen()).len(),
        1,
        "should refuse at the first pinned read"
    );
    assert!(http.api_calls().is_empty(), "the API should never be asked");
    assert!(kv.get(&result_key(&identity)).is_none(), "nothing stored");

    // The remap lands after the first hashing range: every later read is
    // checked again, so the rest of the version cannot come from B.
    let switched = AtomicBool::new(false);
    let first_hash_range = format!("bytes=0-{}", MAX_RANGE_BYTES - 1);
    let from_a = Origin::honest(source.clone()).responder();
    let from_b = served_by_b.responder();
    let http = ScriptedHttp::new(move |seen| {
        if switched.load(Ordering::SeqCst) {
            return from_b(seen);
        }
        if seen.header("range") == Some(first_hash_range.as_str()) {
            switched.store(true, Ordering::SeqCst);
        }
        from_a(seen)
    });
    let kv = probe_kv();
    let response = publish(
        &routes,
        &services(&http, &kv),
        PUBLICATION,
        &identity,
        &genuine,
    );
    assert_eq!(
        refused_with(response, StatusCode::UNPROCESSABLE_ENTITY),
        "source_identity_mismatch"
    );
    assert_eq!(
        pinned_ranges(&http.seen()).last().map(String::as_str),
        Some(format!("bytes={}-{}", MAX_RANGE_BYTES, length - 1).as_str()),
        "should refuse the hashing read B answered"
    );
    assert!(http.api_calls().is_empty(), "the API should never be asked");
    assert!(kv.get(&result_key(&identity)).is_none(), "nothing stored");

    // A worker that says it read B is refused where A answers.
    let http = ScriptedHttp::new(Origin::honest(source.clone()).responder());
    let kv = probe_kv();
    let (claimed_b, envelope_b) = large_envelope_bound(&routes, &source, &remapped);
    assert_ne!(
        claimed_b, identity,
        "B's result should have its own identity"
    );
    let response = publish(
        &routes,
        &services(&http, &kv),
        PUBLICATION,
        &claimed_b,
        &envelope_b,
    );
    assert_eq!(
        refused_with(response, StatusCode::UNPROCESSABLE_ENTITY),
        "source_identity_mismatch"
    );
    assert!(http.api_calls().is_empty(), "the API should never be asked");
    assert!(kv.get(&result_key(&claimed_b)).is_none(), "nothing stored");

    // Genuine A passes every pinned read and the full hash, and only then
    // asks for the receipt, which this API does not hold.
    let http = ScriptedHttp::new(Origin::honest(source.clone()).responder());
    let kv = probe_kv();
    let (identity_a, genuine_a) = large_envelope_bound(&routes, &source, &configured);
    assert_eq!(identity_a, identity);
    let response = publish(
        &routes,
        &services(&http, &kv),
        PUBLICATION,
        &identity_a,
        &genuine_a,
    );
    assert_eq!(
        refused_with(response, StatusCode::GONE),
        "result_not_retained",
        "A should pass every source check before the absent receipt refuses it"
    );
    assert!(
        http.seen()
            .iter()
            .any(|seen| seen.uri.ends_with("/media/hash/recover")),
        "should ask for A's receipt"
    );
}

/// The worker's configured-source digest must be one: an empty or
/// malformed value refuses the result before any source read.
#[test]
fn large_publication_refuses_a_malformed_configured_source_digest() {
    let routes = routes();
    let source = vec![7_u8; 1024];
    let upper = sha256_hex(CONFIGURED_SOURCE.as_bytes()).to_ascii_uppercase();
    let short = "a".repeat(63);
    let not_hex = "g".repeat(64);
    for digest in ["", upper.as_str(), short.as_str(), not_hex.as_str()] {
        let http = ScriptedHttp::new(Origin::honest(source.clone()).responder());
        let kv = probe_kv();
        let (identity, envelope) = large_envelope_bound(&routes, &source, digest);
        let response = publish(
            &routes,
            &services(&http, &kv),
            PUBLICATION,
            &identity,
            &envelope,
        );
        assert_eq!(
            refused_with(response, StatusCode::UNPROCESSABLE_ENTITY),
            "origin_identity_invalid",
            "{digest:?}"
        );
        assert!(
            http.seen().is_empty(),
            "{digest:?}: should neither read the source nor ask the API"
        );
        assert!(
            kv.get(&result_key(&identity)).is_none(),
            "{digest:?}: nothing stored"
        );
    }
}

/// A large source whose XMP names a remote manifest keeps its provenance:
/// the publication inspects the pinned version itself, whatever the
/// worker's disclosure showed, and refuses before it reads the version in
/// full or asks the API anything.
#[test]
fn large_publication_keeps_the_provenance_the_pinned_version_carries() {
    let routes = routes();
    let length = usize::try_from(MAX_RANGE_BYTES).expect("should fit") + 10;
    let fill = length - png_with(&[]).len() - 12 - (12 + xmp_itxt(PROVENANCE_XMP).len());
    let xmp = xmp_itxt(PROVENANCE_XMP);
    let filler = vec![7_u8; fill];
    let source = png_with(&[(b"iTXt", xmp.as_slice()), (b"prVt", filler.as_slice())]);
    assert_eq!(source.len(), length);
    let http = ScriptedHttp::new(Origin::honest(source.clone()).responder());
    let kv = probe_kv();
    let (identity, genuine) = large_envelope(&routes, &source);
    let response = publish(
        &routes,
        &services(&http, &kv),
        PUBLICATION,
        &identity,
        &genuine,
    );
    assert_eq!(
        refused_with(response, StatusCode::UNPROCESSABLE_ENTITY),
        "source_has_provenance_reference",
        "the source's own provenance should be kept"
    );
    assert!(http.api_calls().is_empty(), "the API should never be asked");
    assert!(
        pinned_ranges(&http.seen())
            .iter()
            .all(|range| within_block(range)),
        "the version should never be read in full"
    );
    assert!(kv.get(&result_key(&identity)).is_none(), "nothing stored");
}

// --- snapshots ------------------------------------------------------------

fn snapshot_request(path: &str, variant: &str, nonce: &str) -> Vec<u8> {
    serde_json::to_vec(&SnapshotRequest {
        protocol: PREPARATION_PROTOCOL.to_owned(),
        deployment_id: "test".to_owned(),
        target: super::super::protocol::Target {
            path: path.to_owned(),
            variant: variant.to_owned(),
        },
        nonce: nonce.to_owned(),
        read: SnapshotMode::Whole,
    })
    .expect("should encode the snapshot request")
}

const PAGE: &str = "<html><body><article>Harbor council votes</article></body></html>";

fn page_origin(facts: Option<SnapshotOrigin>, secret: &'static str) -> Arc<ScriptedHttp> {
    ScriptedHttp::new(move |seen| {
        let mut headers = vec![
            ("content-type", "text/html; charset=utf-8".to_owned()),
            ("etag", "\"p1\"".to_owned()),
            ("content-length", PAGE.len().to_string()),
        ];
        if let (Some(facts), Some(capability)) = (&facts, seen.header(CAPABILITY_HEADER)) {
            let nonce = capability.split('.').nth(2).expect("should carry a nonce");
            headers.push((FACTS_HEADER, facts_header(secret, nonce, facts)));
        }
        answer(200, PAGE.as_bytes().to_vec(), &headers)
    })
}

fn public_page_facts() -> SnapshotOrigin {
    SnapshotOrigin {
        status: 200,
        origin_host: HOST.to_owned(),
        ..SnapshotOrigin::default()
    }
}

#[test]
fn a_snapshot_reads_one_configured_target_through_the_reader_pipeline() {
    let routes = routes();
    let http = page_origin(Some(public_page_facts()), PUBLICATION);
    let kv = Arc::new(MemoryKv::default());
    let services = services(&http, &kv);

    let response = call(
        &routes,
        &services,
        Method::POST,
        SNAPSHOT_PATH,
        Some(PUBLICATION),
        snapshot_request("/news/a", "edition=uk", NONCE),
    );

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("private, no-store"),
        "the snapshot's own transport should be private"
    );
    let meta = response
        .headers()
        .get(SNAPSHOT_META_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| STANDARD.decode(value).ok())
        .and_then(|bytes| serde_json::from_slice::<SnapshotMeta>(&bytes).ok())
        .expect("should report the snapshot's metadata");
    assert_eq!(meta.page_url, "https://publisher.example.com/news/a");
    assert_eq!(
        meta.origin,
        public_page_facts(),
        "should report the origin's own facts"
    );
    assert_eq!(meta.etag.as_deref(), Some("\"p1\""));
    assert_eq!(body_of(response), PAGE.as_bytes());

    let seen = http.seen();
    assert_eq!(seen.len(), 1, "should read once, through the loopback");
    let read = &seen[0];
    assert_eq!(read.uri, "https://publisher.example.com/news/a?edition=uk");
    assert_eq!(read.header("accept-encoding"), Some("identity"));
    assert!(read.header("cookie").is_none() && read.header("authorization").is_none());
    assert_eq!(
        verify_snapshot_capability(
            PUBLICATION,
            "/news/a?edition=uk",
            read.header(CAPABILITY_HEADER)
                .expect("should carry a capability"),
            unix_now()
        )
        .as_deref(),
        Some(NONCE),
        "the capability should name exactly this target and the worker's nonce"
    );
}

#[test]
fn snapshots_admit_only_the_requested_range_or_an_exact_whole_response() {
    let cases: &[(u16, u64, u64, &[&str], bool)] = &[
        (206, 8, 15, &["bytes 8-15/16"], true),
        (206, 8, 15, &["bytes 0-7/16"], false),
        (206, 8, 15, &["bytes 8-15/16", "bytes 8-15/16"], false),
        (206, 8, 15, &["bytes 8-15/*"], false),
        (200, 8, 15, &[], false),
        (200, 0, 15, &[], true),
        (206, 0, 15, &[], false),
    ];
    for &(status, first, last, ranges, admitted) in cases {
        let ranges: Vec<String> = ranges.iter().map(|value| (*value).to_owned()).collect();
        let expected = b"abcdefghijklmnop"[first as usize..=last as usize].to_vec();
        let returned = expected.clone();
        let http = ScriptedHttp::new(move |seen| {
            let capability = seen
                .header(CAPABILITY_HEADER)
                .expect("should authenticate the read");
            let nonce = capability
                .split('.')
                .nth(2)
                .expect("should carry the nonce");
            let facts = SnapshotOrigin {
                status,
                origin_host: HOST.to_owned(),
                ..SnapshotOrigin::default()
            };
            let mut response = answer(
                status,
                returned.clone(),
                &[
                    ("etag", ETAG.to_owned()),
                    ("content-type", "video/mp4".to_owned()),
                    ("content-length", returned.len().to_string()),
                    (FACTS_HEADER, facts_header(PUBLICATION, nonce, &facts)),
                ],
            );
            for range in &ranges {
                response.headers_mut().append(
                    header::CONTENT_RANGE,
                    HeaderValue::from_str(range).expect("should be a header value"),
                );
            }
            response
        });
        let routes = routes();
        let kv = Arc::new(MemoryKv::default());
        let services = services(&http, &kv);
        let request = SnapshotRequest {
            protocol: PREPARATION_PROTOCOL.to_owned(),
            deployment_id: "test".to_owned(),
            target: super::super::protocol::Target {
                path: "/media/ferry.mp4".to_owned(),
                variant: String::new(),
            },
            nonce: NONCE.to_owned(),
            read: SnapshotMode::Range {
                first,
                last,
                if_match: ETAG.to_owned(),
            },
        };
        let response = call(
            &routes,
            &services,
            Method::POST,
            SNAPSHOT_PATH,
            Some(PUBLICATION),
            serde_json::to_vec(&request).expect("should encode the range request"),
        );
        if admitted {
            assert_eq!(response.status(), StatusCode::OK);
            let meta: SnapshotMeta = serde_json::from_slice(
                &STANDARD
                    .decode(response.headers()[SNAPSHOT_META_HEADER].as_bytes())
                    .expect("should encode metadata"),
            )
            .expect("should report source metadata");
            assert_eq!(meta.length, Some(16));
            assert_eq!(body_of(response), expected);
        } else {
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            assert!(response.headers().get(SNAPSHOT_META_HEADER).is_none());
        }
    }
}

#[test]
fn a_snapshot_refuses_unconfigured_targets_before_reading_and_unadmitted_answers_after() {
    let routes = routes();
    for (path, variant, nonce, status, code) in [
        (
            "/private/a",
            "",
            NONCE,
            StatusCode::FORBIDDEN,
            "target_not_configured",
        ),
        (
            "/news/a",
            "edition=fr",
            NONCE,
            StatusCode::FORBIDDEN,
            "target_not_configured",
        ),
        (
            "/news/a",
            "utm_source=x",
            NONCE,
            StatusCode::FORBIDDEN,
            "target_not_configured",
        ),
        (
            "/news/../private",
            "",
            NONCE,
            StatusCode::FORBIDDEN,
            "target_not_configured",
        ),
        (
            "/news/a",
            "",
            "short",
            StatusCode::BAD_REQUEST,
            "nonce_invalid",
        ),
    ] {
        let http = ScriptedHttp::refusing();
        let kv = Arc::new(MemoryKv::default());
        let response = call(
            &routes,
            &services(&http, &kv),
            Method::POST,
            SNAPSHOT_PATH,
            Some(PUBLICATION),
            snapshot_request(path, variant, nonce),
        );
        assert_eq!(refused_with(response, status), code, "{path}?{variant}");
        assert!(http.seen().is_empty(), "{path}: nothing should be read");
    }

    let private = SnapshotOrigin {
        private: true,
        ..public_page_facts()
    };
    let linked = SnapshotOrigin {
        manifest_link: true,
        ..public_page_facts()
    };
    for (http, status, code) in [
        (
            page_origin(None, PUBLICATION),
            StatusCode::UNPROCESSABLE_ENTITY,
            "snapshot_unobserved",
        ),
        (
            page_origin(Some(public_page_facts()), "an-origin-guessing-the-secret"),
            StatusCode::UNPROCESSABLE_ENTITY,
            "snapshot_unobserved",
        ),
        (
            page_origin(Some(private), PUBLICATION),
            StatusCode::FORBIDDEN,
            "source_private",
        ),
        (
            page_origin(Some(linked), PUBLICATION),
            StatusCode::UNPROCESSABLE_ENTITY,
            "source_has_provenance_link",
        ),
    ] {
        let kv = Arc::new(MemoryKv::default());
        let response = call(
            &routes,
            &services(&http, &kv),
            Method::POST,
            SNAPSHOT_PATH,
            Some(PUBLICATION),
            snapshot_request("/news/a", "", NONCE),
        );
        assert_eq!(refused_with(response, status), code);
    }

    let http = ScriptedHttp::refusing();
    let kv = Arc::new(MemoryKv::default());
    let signal = call(
        &routes,
        &services(&http, &kv),
        Method::POST,
        SNAPSHOT_PATH,
        Some(SIGNAL),
        snapshot_request("/news/a", "", NONCE),
    );
    assert_eq!(
        refused_with(signal, StatusCode::UNAUTHORIZED),
        "unauthorized",
        "the reader-signal secret should not read sources"
    );
    assert!(http.seen().is_empty());
}

// --- public source snapshots ----------------------------------------------

#[test]
fn a_public_source_is_served_only_as_the_exact_bytes_its_digest_names() {
    let routes = routes();
    let sha = sha256_hex(SOURCE);
    let http = ScriptedHttp::new(|_| {
        answer(
            200,
            SOURCE.to_vec(),
            &[("content-type", "image/png".to_owned())],
        )
    });
    let kv = Arc::new(MemoryKv::default());
    let response = call(
        &routes,
        &services(&http, &kv),
        Method::GET,
        &format!("{PUBLIC_SOURCE_PREFIX}{sha}"),
        None,
        Vec::new(),
    );
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();
    assert_eq!(
        headers
            .get("x-content-type-options")
            .map(HeaderValue::as_bytes),
        Some(&b"nosniff"[..])
    );
    assert!(
        headers
            .get(header::CONTENT_DISPOSITION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("attachment")),
        "should never be rendered inline"
    );
    assert_eq!(
        headers
            .get("content-security-policy")
            .and_then(|value| value.to_str().ok()),
        Some("sandbox; default-src 'none'; base-uri 'none'; form-action 'none'")
    );
    assert_eq!(body_of(response), SOURCE);
    let seen = http.seen();
    assert_eq!(
        seen[0].uri,
        format!("https://prep.publisher.example.com{WORKER_SOURCE_PREFIX}{sha}")
    );
    assert_eq!(seen[0].header(TOKEN_HEADER), Some(PUBLICATION));

    let lying = ScriptedHttp::new(|_| {
        answer(
            200,
            OTHER_SOURCE.to_vec(),
            &[("content-type", "image/png".to_owned())],
        )
    });
    let response = call(
        &routes,
        &services(&lying, &kv),
        Method::GET,
        &format!("{PUBLIC_SOURCE_PREFIX}{sha}"),
        None,
        Vec::new(),
    );
    assert_eq!(
        refused_with(response, StatusCode::NOT_FOUND),
        "not_found",
        "bytes that are not the digest's should never be served"
    );

    let http = ScriptedHttp::refusing();
    let response = call(
        &routes,
        &services(&http, &kv),
        Method::GET,
        &format!("{PUBLIC_SOURCE_PREFIX}not-a-digest"),
        None,
        Vec::new(),
    );
    assert_eq!(refused_with(response, StatusCode::NOT_FOUND), "not_found");
    assert!(http.seen().is_empty());
}

// --- the request preparer -------------------------------------------------

fn settings(execution: &str) -> Settings {
    page_settings(&format!(
        "[provenance]\nmodule = \"encypher\"\n\n[provenance.encypher]\n\
         api_base_url = \"https://api.example.com\"\ndomain = \"{HOST}\"\n\
         worker_token = \"{TOKEN}\"\nexecution = \"{execution}\"\n\n\
         [provenance.encypher.preparation]\n\
         worker_url = \"https://prep.publisher.example.com\"\n\
         loopback_url = \"https://{HOST}\"\ndeployment_id = \"test\"\n\
         public_host = \"{HOST}\"\nsignal_secret = \"{SIGNAL}\"\n\
         publication_secret = \"{PUBLICATION}\"\nroutes = [\"/news/\"]\n\n\
         [[serve]]\nmedia_type = \"text/html\"\nmiddleware = [\"{MODULE}\"]\n"
    ))
}

fn prepared(settings: &Settings, path_and_query: &str, capabilities: &[&str]) -> Request<EdgeBody> {
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri(format!("https://{HOST}{path_and_query}"));
    for capability in capabilities {
        builder = builder.header(CAPABILITY_HEADER, *capability);
    }
    let mut request = builder.body(EdgeBody::empty()).expect("should build");
    prepare_request(settings, &mut request).expect("should prepare");
    request
}

fn grant(request: &Request<EdgeBody>) -> Option<String> {
    IntegrationRequestState::of(request)
        .get::<SnapshotGrant>(INTEGRATION_ID)
        .map(|grant| grant.nonce.clone())
}

#[test]
fn only_a_verified_capability_for_the_exact_target_leaves_a_snapshot_grant() {
    let asynchronous = settings("async_prepared");
    let expires = unix_now() + 60;
    let genuine = snapshot_capability(PUBLICATION, "/news/a?edition=uk", NONCE, expires);
    let other_path = snapshot_capability(PUBLICATION, "/news/b", NONCE, expires);
    let forged = snapshot_capability(
        "a-reader-guessing-the-secret",
        "/news/a?edition=uk",
        NONCE,
        expires,
    );

    let request = prepared(&asynchronous, "/news/a?edition=uk", &[&genuine]);
    assert_eq!(grant(&request).as_deref(), Some(NONCE));
    assert!(
        request.headers().get(CAPABILITY_HEADER).is_none(),
        "the capability should never travel on"
    );
    assert!(
        IntegrationRequestState::of(&request).is_empty(),
        "a grant is an observation and keeps the reader's shared path"
    );

    for (name, path, capabilities) in [
        (
            "another target",
            "/news/a?edition=uk",
            vec![other_path.as_str()],
        ),
        (
            "another query",
            "/news/a?edition=us",
            vec![genuine.as_str()],
        ),
        (
            "a forged secret",
            "/news/a?edition=uk",
            vec![forged.as_str()],
        ),
        (
            "two values",
            "/news/a?edition=uk",
            vec![genuine.as_str(), genuine.as_str()],
        ),
        (
            "a malformed value",
            "/news/a?edition=uk",
            vec!["v1.not-a-capability"],
        ),
    ] {
        let request = prepared(&asynchronous, path, &capabilities);
        assert!(grant(&request).is_none(), "{name} should leave no grant");
        assert!(
            request.headers().get(CAPABILITY_HEADER).is_none(),
            "{name}: the header should be removed"
        );
    }

    let synchronous = prepared(&settings("synchronous"), "/news/a?edition=uk", &[&genuine]);
    assert!(
        grant(&synchronous).is_none(),
        "the synchronous mode grants nothing"
    );
    assert!(
        synchronous.headers().get(CAPABILITY_HEADER).is_none(),
        "the header is removed whether or not the asynchronous mode runs"
    );
}

// --- real probe receipts --------------------------------------------------

fn fixture(relative: &str) -> Vec<u8> {
    let bytes: &[u8] = match relative {
        "probe.wasm" => include_bytes!("../../fixtures/prepared/probe.wasm"),
        "jpeg/envelope.json" => include_bytes!("../../fixtures/prepared/jpeg/envelope.json"),
        "jpeg/recover.json" => include_bytes!("../../fixtures/prepared/jpeg/recover.json"),
        "png/envelope.json" => include_bytes!("../../fixtures/prepared/png/envelope.json"),
        "png/recover.json" => include_bytes!("../../fixtures/prepared/png/recover.json"),
        "webp/envelope.json" => include_bytes!("../../fixtures/prepared/webp/envelope.json"),
        "webp/recover.json" => include_bytes!("../../fixtures/prepared/webp/recover.json"),
        "svg/envelope.json" => include_bytes!("../../fixtures/prepared/svg/envelope.json"),
        "svg/recover.json" => include_bytes!("../../fixtures/prepared/svg/recover.json"),
        "external-png/envelope.json" => {
            include_bytes!("../../fixtures/prepared/external-png/envelope.json")
        }
        "external-png/recover.json" => {
            include_bytes!("../../fixtures/prepared/external-png/recover.json")
        }
        _ => panic!("unknown prepared fixture: {relative}"),
    };
    bytes.to_vec()
}

/// The routes of a deployment claiming the host `envelope` names, with its
/// identity and deployment rewritten to that deployment's own.
fn deployment_for(envelope: &mut ResultEnvelope) -> PreparationRoutes {
    let planned = match &envelope.item {
        EnvelopeItem::Media { planned, .. } | EnvelopeItem::Embedded { planned, .. } => planned,
        _ => panic!("should be a bounded media case"),
    };
    let page = url::Url::parse(&planned.request.page_url).expect("should be a URL");
    let host = page.host_str().expect("should name a host").to_owned();
    let domain = host.strip_prefix("www.").unwrap_or(&host).to_owned();
    let routes = routes_for(config_value(&domain, &host, &["/"]));
    envelope.deployment_id = "test".to_owned();
    envelope.identity = media_identity(&routes, &planned.request, &envelope.variant);
    routes
}

/// A transport answering as the candidate API answered the worker.
fn candidate_api(recover: Vec<u8>, probe: Vec<u8>) -> Arc<ScriptedHttp> {
    ScriptedHttp::new(move |seen| {
        if seen.uri.ends_with("/api/v1/public/cdn/media/hash/recover") {
            answer(
                200,
                recover.clone(),
                &[("content-type", "application/json".to_owned())],
            )
        } else if seen.uri.contains("/api/v1/sign/media/probe/") {
            answer(200, probe.clone(), &[])
        } else {
            answer(500, Vec::new(), &[])
        }
    })
}

fn case_envelope(case: &str) -> ResultEnvelope {
    serde_json::from_slice(&fixture(&format!("{case}/envelope.json")))
        .expect("should be a result envelope")
}

/// Publishes `envelope` against `case`'s recorded receipt, returning the
/// answer and whether a record was stored.
fn publish_against(case: &str, mut envelope: ResultEnvelope) -> (Response<EdgeBody>, bool) {
    let routes = deployment_for(&mut envelope);
    let http = candidate_api(
        fixture(&format!("{case}/recover.json")),
        fixture("probe.wasm"),
    );
    let kv = Arc::new(MemoryKv::default());
    let identity = envelope.identity.clone();
    let response = publish(
        &routes,
        &services(&http, &kv),
        PUBLICATION,
        &identity,
        &envelope,
    );
    assert!(
        http.seen()
            .iter()
            .all(|seen| !seen.uri.ends_with("/hash/finalize")
                && !seen.uri.ends_with("/hash/prepare")
                && !seen.uri.ends_with("/cdn/sign")),
        "{case}: publication should never prepare, finalize or sign"
    );
    let stored = kv.get(&result_key(&identity)).is_some();
    (response, stored)
}

fn flip_last(b64: &str) -> String {
    let mut bytes = STANDARD.decode(b64).expect("should be base64");
    *bytes.last_mut().expect("should have bytes") ^= 1;
    STANDARD.encode(bytes)
}

/// Rewrites an embedded envelope's source to `source`, keeping the request
/// self-consistent with it, as a forging worker would.
fn with_source(mut envelope: ResultEnvelope, source: &[u8]) -> ResultEnvelope {
    if let EnvelopeItem::Embedded {
        planned,
        source_b64,
        ..
    } = &mut envelope.item
    {
        planned.request.source.sha256 = sha256_hex(source);
        planned.request.source.byte_length = u64::try_from(source.len()).expect("should fit");
        *source_b64 = STANDARD.encode(source);
    }
    envelope
}

fn embedded_parts(envelope: &ResultEnvelope) -> (Vec<u8>, String) {
    match &envelope.item {
        EnvelopeItem::Embedded {
            source_b64,
            output_b64,
            ..
        } => (
            STANDARD.decode(source_b64).expect("should be base64"),
            output_b64.clone(),
        ),
        _ => panic!("should be an embedded case"),
    }
}

/// The real probe, run on candidate-API receipts for JPEG, PNG, WebP and
/// inline SVG: the genuine output publishes, and every forgery of output or
/// source, however self-consistent, is refused before anything is stored.
#[test]
fn real_probe_receipts_publish_only_the_exact_output() {
    let cases = ["jpeg", "png", "webp", "svg"];
    for case in cases {
        let genuine = case_envelope(case);
        let (response, stored) = publish_against(case, genuine.clone());
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "{case}: should publish: {}",
            String::from_utf8_lossy(&body_of(response))
        );
        assert!(stored, "{case}: should store the verified record");

        // One byte of the output changed.
        let mut tampered = genuine.clone();
        if let EnvelopeItem::Embedded { output_b64, .. } = &mut tampered.item {
            *output_b64 = flip_last(output_b64);
        }
        let (response, stored) = publish_against(case, tampered);
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{case}: a one-byte output change should be refused"
        );
        assert!(!stored, "{case}: nothing stored for a tampered output");

        // Another case's genuine output substituted.
        let donor = cases
            .iter()
            .find(|other| **other != case)
            .expect("should have another case");
        let (_, donor_output) = embedded_parts(&case_envelope(donor));
        let mut substituted = genuine.clone();
        if let EnvelopeItem::Embedded { output_b64, .. } = &mut substituted.item {
            *output_b64 = donor_output;
        }
        let (response, stored) = publish_against(case, substituted);
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{case}: a substituted output should be refused"
        );
        assert!(!stored);

        // Changed pixels or markup with a self-consistent request: the
        // receipt is genuine, the source is not the one it signed.
        let (mut source, _) = embedded_parts(&genuine);
        let middle = source.len() / 2;
        source[middle] ^= 1;
        let (response, stored) = publish_against(case, with_source(genuine.clone(), &source));
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{case}: a changed source should be refused by the probe's checks"
        );
        assert!(!stored);
    }
}

#[test]
fn real_probe_receipt_refuses_tampered_external_source() {
    let genuine = case_envelope("external-png");
    let (response, stored) = publish_against("external-png", genuine.clone());
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "external: should publish: {}",
        String::from_utf8_lossy(&body_of(response))
    );
    assert!(stored);
    let mut tampered = genuine;
    if let EnvelopeItem::Media { source_b64, .. } = &mut tampered.item {
        *source_b64 = flip_last(source_b64);
    }
    let (response, stored) = publish_against("external-png", tampered);
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!stored);
}

/// A bounded source whose XMP names a remote manifest keeps its
/// provenance: an external manifest delivered over it would never be read.
/// The receipt is the fixture's own, for the source without that packet. No
/// genuine receipt for a provenance-bearing source exists, so the refusal
/// must come from the source itself, before the receipt is checked.
#[test]
fn real_probe_external_publication_keeps_the_source_provenance() {
    let xmp = xmp_itxt(PROVENANCE_XMP);
    let source = png_with(&[(b"iTXt", xmp.as_slice())]);
    let mut envelope = case_envelope("external-png");
    let EnvelopeItem::Media {
        planned,
        source_b64,
    } = &mut envelope.item
    else {
        panic!("should be an external case");
    };
    planned.request.source.sha256 = sha256_hex(&source);
    planned.request.source.byte_length = u64::try_from(source.len()).expect("should fit");
    planned.digest = planned.request.source.sha256.clone();
    *source_b64 = STANDARD.encode(&source);
    let (response, stored) = publish_against("external-png", envelope);
    assert_eq!(
        refused_with(response, StatusCode::UNPROCESSABLE_ENTITY),
        "source_has_provenance_reference",
        "the source's own provenance should be kept"
    );
    assert!(!stored, "nothing stored");
}

// --- validators -----------------------------------------------------------

#[test]
fn only_a_single_strong_validator_pins_a_source() {
    let mut headers = http::HeaderMap::new();
    headers.insert(header::ETAG, HeaderValue::from_static("\"v1\""));
    assert_eq!(single_strong_etag(&headers).as_deref(), Some("\"v1\""));
    headers.append(header::ETAG, HeaderValue::from_static("\"v2\""));
    assert!(
        single_strong_etag(&headers).is_none(),
        "two tags name no single version"
    );
    let mut weak = http::HeaderMap::new();
    weak.insert(header::ETAG, HeaderValue::from_static("W/\"v1\""));
    assert!(
        single_strong_etag(&weak).is_none(),
        "a weak tag pins nothing"
    );
}
