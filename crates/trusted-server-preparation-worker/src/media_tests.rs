//! Real signed-probe execution and durable worker preparation. Only HTTP is a fixture.
use std::collections::HashMap;
use std::sync::Mutex;

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::*;
use crate::client::Secrets;
use crate::store::{Bounds, JobState};
use trusted_server_provenance_encypher::preparation::protocol::{
    SNAPSHOT_META_HEADER, SnapshotMeta, SnapshotOrigin, SnapshotRequest,
};

struct Case {
    planned: PlannedMedia,
    source: Vec<u8>,
    output: Vec<u8>,
    prepare: Value,
    finalize: Value,
    recover: Value,
}

impl Case {
    fn read(name: &str) -> Self {
        let root = fixture_root().join(name);
        let envelope: ResultEnvelope = serde_json::from_slice(
            &std::fs::read(root.join("envelope.json"))
                .expect("should read exported real API envelope"),
        )
        .expect("should parse exported envelope");
        let (planned, source, output) = match envelope.item {
            EnvelopeItem::Embedded {
                planned,
                source_b64,
                output_b64,
            } => (
                planned,
                STANDARD.decode(source_b64).expect("should decode source"),
                STANDARD
                    .decode(output_b64)
                    .expect("should decode native output"),
            ),
            EnvelopeItem::Media {
                planned,
                source_b64,
            } => {
                let source = STANDARD
                    .decode(source_b64)
                    .expect("should decode external source");
                (planned, source.clone(), source)
            }
            _ => panic!("fixture should be a real prepared media envelope"),
        };
        let read = |name: &str| {
            serde_json::from_slice(
                &std::fs::read(root.join(name)).expect("should read exported API response"),
            )
            .expect("should parse API response")
        };
        Self {
            planned,
            source,
            output,
            prepare: read("prepare.json"),
            finalize: read("finalize.json"),
            recover: read("recover.json"),
        }
    }
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../provenance/encypher/src/fixtures/prepared")
}

struct Boundary {
    cases: Vec<Case>,
    active: Vec<usize>,
    extra_html: String,
    operations: HashMap<String, usize>,
    completed: Vec<bool>,
    preparations: usize,
    finalizations: usize,
    finalize_attempts: usize,
    retired_receipt: bool,
    capabilities: usize,
    probe_fetches: usize,
    publications: Vec<ResultEnvelope>,
    closed: bool,
    lose_receipt_once: bool,
    fail_publication: bool,
    standalone: bool,
    large: bool,
    short_range: bool,
    range_reads: usize,
    snapshot_reads: usize,
    remap_on_snapshot: Option<usize>,
    origin_identity_sha256: String,
    private: bool,
    etag: String,
    descriptor: Value,
    probe: Vec<u8>,
}

struct Harness {
    boundary: Arc<Mutex<Boundary>>,
    url: String,
    server: tokio::task::JoinHandle<()>,
}

impl Harness {
    async fn start(names: &[&str]) -> Self {
        let root = fixture_root();
        let cases: Vec<_> = names.iter().map(|name| Case::read(name)).collect();
        let descriptor = serde_json::from_slice(
            &std::fs::read(root.join("descriptor.json"))
                .expect("should read production-signed release descriptor"),
        )
        .expect("should parse descriptor");
        let probe = std::fs::read(root.join("probe.wasm"))
            .expect("should read shared production-authenticated probe");
        let completed = vec![false; cases.len()];
        let boundary = Arc::new(Mutex::new(Boundary {
            cases,
            active: vec![0],
            extra_html: String::new(),
            operations: HashMap::new(),
            completed,
            preparations: 0,
            finalizations: 0,
            finalize_attempts: 0,
            retired_receipt: false,
            capabilities: 0,
            probe_fetches: 0,
            publications: Vec::new(),
            closed: false,
            lose_receipt_once: false,
            fail_publication: false,
            standalone: false,
            large: false,
            short_range: false,
            range_reads: 0,
            snapshot_reads: 0,
            remap_on_snapshot: None,
            origin_identity_sha256: crate::hex_lower(&Sha256::digest(
                b"https://origin.example.com/old/\0origin.example.com",
            )),
            private: false,
            etag: "\"fixture-v1\"".to_owned(),
            descriptor,
            probe,
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("should bind HTTP boundary");
        let url = format!(
            "http://{}",
            listener.local_addr().expect("should read listener address")
        );
        let app = Router::new()
            .fallback(exchange)
            .with_state(Arc::clone(&boundary));
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("should serve boundary");
        });
        Self {
            boundary,
            url,
            server,
        }
    }

    fn context(&self, directory: &tempfile::TempDir) -> Context {
        let state = self.boundary.lock().expect("should lock boundary");
        let page = url::Url::parse(&state.cases[0].planned.request.page_url)
            .expect("should parse fixture page");
        let host = page.host_str().expect("should have fixture host");
        let config: WorkerConfig = toml::from_str(&format!(
            r#"
listen = "0.0.0.0:8112"
store_dir = {:?}
deployment_id = "real-probe-worker-test"
api_base_url = {:?}
edge_url = {:?}
domain = {:?}
public_host = {:?}
max_media_bytes = 1048576
max_snapshot_bytes = 1048576
max_large_media_bytes = 16777216
max_preparation_seconds = 120
[secrets]
worker_token = "TEST_WORKER"
signal_secret = "TEST_SIGNAL"
publication_secret = "TEST_PUBLICATION"
operator_secret = "TEST_OPERATOR"
"#,
            directory.path().to_str().expect("should have path"),
            self.url,
            self.url,
            host.strip_prefix("www.").unwrap_or(host),
            host
        ))
        .expect("should parse worker config");
        config
            .check()
            .expect("should admit actual fixture configuration");
        drop(state);
        let store = Arc::new(
            Store::open(
                &directory.path().join("preparation.redb"),
                Bounds::default(),
            )
            .expect("should own real durable store"),
        );
        store
            .recover_storage()
            .expect("should recover all durable source/probe/output material");
        store
            .recover_all(now())
            .expect("should fence previous process claims");
        let secrets = Secrets {
            worker_token: "cwk_real_probe_fixture".to_owned(),
            signal: "signal".to_owned(),
            publication: "publication".to_owned(),
            operator: "operator".to_owned(),
            canary: Some("creation-canary".to_owned()),
        };
        Context::new(
            store,
            Api::new(&self.url, &secrets),
            Edge::new(&self.url, &secrets, &config.deployment_id),
            Arc::new(config),
            &secrets.worker_token,
        )
    }

    fn target(&self) -> Target {
        let state = self.boundary.lock().expect("should lock boundary");
        let page =
            url::Url::parse(&state.cases[0].planned.request.page_url).expect("should parse page");
        Target {
            path: page.path().to_owned(),
            variant: String::new(),
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn answer(status: StatusCode, value: &Value) -> Response {
    (status, axum::Json(value.clone())).into_response()
}
fn denied() -> Response {
    answer(
        StatusCode::FORBIDDEN,
        &json!({"error":{"code":"CREATION_DISABLED"}}),
    )
}

async fn exchange(State(boundary): State<Arc<Mutex<Boundary>>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let headers = request.headers().clone();
    let body = to_bytes(request.into_body(), 8 * 1024 * 1024)
        .await
        .expect("should read bounded control request");
    let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut state = boundary.lock().expect("should lock HTTP boundary");
    if path.starts_with("/api/v1/sign/media/probe/") {
        assert!(headers.get("x-encypher-canary-token").is_none());
        assert!(
            headers.get("x-encypher-worker-token").is_none(),
            "public probe acquisition must not leak the worker credential"
        );
        assert_eq!(
            path,
            format!(
                "/api/v1/sign/media/probe/{}.wasm",
                state.descriptor["sha256"]
                    .as_str()
                    .expect("should have descriptor digest")
            )
        );
        state.probe_fetches += 1;
        return state.probe.clone().into_response();
    }
    if path.starts_with("/api/v1/") {
        assert_eq!(
            headers
                .get("x-encypher-worker-token")
                .expect("should authenticate API calls"),
            "cwk_real_probe_fixture"
        );
        if path.ends_with("/prepare") || path.ends_with("/finalize") {
            assert_eq!(
                headers
                    .get("x-encypher-canary-token")
                    .expect("creation should carry configured canary"),
                "creation-canary"
            );
        } else {
            assert!(
                headers.get("x-encypher-canary-token").is_none(),
                "read-only authority and recovery must not require a creation canary"
            );
        }
        let policy = state.cases[0]
            .planned
            .request
            .expected_policy_digest
            .clone();
        if path.ends_with("/policy") {
            return answer(StatusCode::OK, &json!({"policy_digest":policy}));
        }
        if path.ends_with("/capabilities") {
            state.capabilities += 1;
            if state.closed {
                return denied();
            }
            let formats: Vec<_> = state
                .cases
                .iter()
                .map(|case| case.planned.request.source.mime.clone())
                .collect();
            return answer(
                StatusCode::OK,
                &json!({"protocol":pipeline::PREPARED_PROTOCOL,"enabled":true,"policy_digest":policy,"formats":formats,"embedded_formats":formats,"contract_versions":[1,2],"maximums":{"total_size_bytes":16777216,"manifest_decoded_bytes":2097152},"probe":[state.descriptor]}),
            );
        }
        let key = headers
            .get("idempotency-key")
            .expect("should pin operation key")
            .to_str()
            .expect("should parse key")
            .to_owned();
        if path.ends_with("/prepare") {
            if state.closed {
                return denied();
            }
            let index = state
                .cases
                .iter()
                .position(|case| {
                    serde_json::to_value(&case.planned.request)
                        .expect("should serialize frozen request")
                        == value
                })
                .expect("worker must inspect exactly the exported source and frozen contract");
            if let Some(prior) = state.operations.get(&key) {
                assert_eq!(*prior, index, "operation identity cannot be rebound");
            }
            state.operations.insert(key, index);
            state.preparations += 1;
            return answer(StatusCode::OK, &state.cases[index].prepare);
        }
        if path.ends_with("/recover") {
            let Some(index) = state.operations.get(&key).copied() else {
                return answer(
                    StatusCode::NOT_FOUND,
                    &json!({"error":{"code":"NOT_FOUND"}}),
                );
            };
            assert_eq!(
                value,
                serde_json::to_value(&state.cases[index].planned.request)
                    .expect("should serialize request")
            );
            if !state.completed[index] {
                return answer(
                    StatusCode::NOT_FOUND,
                    &json!({"error":{"code":"NOT_FOUND"}}),
                );
            }
            if state.retired_receipt {
                return answer(
                    StatusCode::GONE,
                    &json!({"error":{"code":"RESULT_RETIRED"}}),
                );
            }
            if state.lose_receipt_once {
                state.lose_receipt_once = false;
                return answer(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &json!({"error":{"code":"LOST_RECEIPT_REPLY"}}),
                );
            }
            return answer(StatusCode::OK, &state.cases[index].recover);
        }
        if path.ends_with("/finalize") {
            state.finalize_attempts += 1;
            if state.closed {
                return denied();
            }
            let index = *state
                .operations
                .get(&key)
                .expect("finalize must use the durable prepare operation");
            let plan = &state.cases[index].planned;
            assert_eq!(value["plan_id"], plan.plan_id);
            assert_eq!(
                value["binding"]["digest"], plan.digest,
                "the real signed probe hash program must reproduce the native fixture digest"
            );
            assert!(
                !state.completed[index],
                "retry must recover, not finalize a completed operation"
            );
            state.completed[index] = true;
            state.finalizations += 1;
            return answer(StatusCode::OK, &state.cases[index].finalize);
        }
        panic!("worker must not use text signing or media upload in this scenario: {path}");
    }
    assert_eq!(
        headers
            .get("x-encypher-preparation-token")
            .expect("should authenticate publisher control"),
        "publication"
    );
    assert!(
        headers.get("cookie").is_none() && headers.get("authorization").is_none(),
        "snapshot/publication transport must never carry reader state"
    );
    if path == "/_encypher/preparation/snapshot" {
        let request: SnapshotRequest =
            serde_json::from_value(value).expect("should use frozen snapshot protocol");
        let first = state.active[0];
        let plan = &state.cases[first].planned;
        let mut bytes = if state.standalone {
            state.cases[first].source.clone()
        } else {
            let images: String = state
                .active
                .iter()
                .map(|index| {
                    let case = &state.cases[*index];
                    assert_eq!(
                        case.planned.request.page_url, plan.request.page_url,
                        "fixture siblings share their exact page binding"
                    );
                    format!(
                        "<img src=\"data:{};base64,{}\">",
                        case.planned.request.source.mime,
                        STANDARD.encode(&case.source)
                    )
                })
                .collect();
            format!(
                "<!doctype html><html><body><main>{images}{}</main></body></html>",
                state.extra_html
            )
            .into_bytes()
        };
        let total = bytes.len() as u64;
        let page_url = plan.request.page_url.clone();
        let origin_host = plan.request.source.origin_host.clone();
        let content_type = if state.standalone {
            plan.request.source.mime.clone()
        } else {
            "text/html".to_owned()
        };
        let reference = if state.standalone && plan.request.source.reference_kind == "origin" {
            Some(plan.request.source.reference.clone())
        } else {
            None
        };
        let mut status = 200;
        match request.read {
            SnapshotMode::Whole if state.large => {
                return answer(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &json!({"error":{"code":"snapshot_too_large"}}),
                );
            }
            SnapshotMode::Whole => {}
            SnapshotMode::Head => bytes.clear(),
            SnapshotMode::Range {
                first,
                last,
                if_match,
            } => {
                assert_eq!(if_match, state.etag);
                bytes = bytes[first as usize..=last as usize].to_vec();
                if state.short_range {
                    bytes.pop();
                }
                state.range_reads += 1;
                status = 206;
            }
        }
        state.snapshot_reads += 1;
        if state.remap_on_snapshot == Some(state.snapshot_reads) {
            state.origin_identity_sha256 = crate::hex_lower(&Sha256::digest(
                b"https://origin.example.com/new/\0origin.example.com",
            ));
        }
        let metadata = SnapshotMeta {
            protocol: PREPARATION_PROTOCOL.to_owned(),
            page_url,
            content_type,
            etag: Some(state.etag.clone()),
            length: Some(total),
            origin: SnapshotOrigin {
                status,
                private: state.private,
                no_transform: false,
                manifest_link: false,
                content_encoding: String::new(),
                origin_host,
                origin_identity_sha256: state.origin_identity_sha256.clone(),
                source_reference: reference,
            },
        };
        return (
            [
                (
                    SNAPSHOT_META_HEADER,
                    STANDARD
                        .encode(serde_json::to_vec(&metadata).expect("should encode source facts")),
                ),
                ("cache-control", "private, no-store".to_owned()),
            ],
            bytes,
        )
            .into_response();
    }
    if path.starts_with("/_encypher/preparation/results/") {
        if state.fail_publication {
            return answer(
                StatusCode::SERVICE_UNAVAILABLE,
                &json!({"error":{"code":"EDGE_UNAVAILABLE"}}),
            );
        }
        state.publications.push(
            serde_json::from_value(value).expect("should publish the actual worker envelope"),
        );
        return answer(StatusCode::OK, &json!({"stored":true}));
    }
    panic!("unexpected external boundary route: {path}");
}

fn claimed(context: &Context, target: &Target, token: u64) -> Claim {
    context
        .store
        .admit(target, JobOrigin::Warm, &[], now())
        .expect("should admit authenticated warm");
    context
        .store
        .claim(now(), token)
        .expect("should claim")
        .expect("should have durable work")
}

async fn warm(context: &Context, target: &Target, token: u64) {
    let claim = claimed(context, target, token);
    run(context, &claim)
        .await
        .expect("should finish real probe work");
    assert!(
        context
            .store
            .complete(&claim.job.key, claim.token, now())
            .expect("should acknowledge exact generation")
    );
}

#[tokio::test]
async fn real_probe_changed_only_media_preparation_reuses_siblings_and_durable_outputs() {
    let harness = Harness::start(&["png", "jpeg", "webp", "svg"]).await;
    let directory = tempfile::tempdir().expect("should create durable worker storage");
    let context = harness.context(&directory);
    harness.boundary.lock().expect("should lock").active = vec![0, 1];
    warm(&context, &harness.target(), 1).await;
    {
        let state = harness.boundary.lock().expect("should lock");
        assert_eq!(state.finalizations, 2);
        for envelope in &state.publications {
            if let EnvelopeItem::Embedded {
                planned,
                output_b64,
                ..
            } = &envelope.item
            {
                let expected = state
                    .cases
                    .iter()
                    .find(|case| {
                        case.planned.request.source.sha256 == planned.request.source.sha256
                    })
                    .expect("should identify real source");
                assert_eq!(
                    STANDARD.decode(output_b64).expect("should decode output"),
                    expected.output,
                    "assembly must match real native output"
                );
            } else {
                panic!("embedded carrier is required, never an external fallback");
            }
        }
    }
    // Change one image while the JPEG remains byte-identical on the same page.
    harness.boundary.lock().expect("should lock").active = vec![2, 1];
    warm(&context, &harness.target(), 2).await;
    {
        let state = harness.boundary.lock().expect("should lock");
        assert_eq!(state.preparations, 3, "only the changed source prepares");
        assert_eq!(state.finalizations, 3, "stable sibling is not re-signed");
        assert_eq!(
            state.probe_fetches, 1,
            "all retained plans share the exact accounted real probe"
        );
        let sha = state.descriptor["sha256"]
            .as_str()
            .expect("should have digest");
        assert_eq!(
            context
                .store
                .probe_references(sha)
                .expect("should retain release references")
                .len(),
            3
        );
        assert!(
            context.store.reserved().expect("should count hard bytes") >= state.probe.len() as u64
        );
    }
    harness.boundary.lock().expect("should lock").active = vec![3, 1];
    warm(&context, &harness.target(), 3).await;
    assert_eq!(
        harness.boundary.lock().expect("should lock").finalizations,
        4
    );
    drop(context);
    harness.boundary.lock().expect("should lock").closed = true;
    let context = harness.context(&directory);
    warm(&context, &harness.target(), 4).await;
    let state = harness.boundary.lock().expect("should lock");
    assert_eq!(
        state.finalizations, 4,
        "restart/eviction restores stored output with creation disabled"
    );
    assert_eq!(state.preparations, 4);
    assert_eq!(state.publications.len(), 8);
}

#[tokio::test]
async fn html_extraction_refusals_remain_visible_while_valid_siblings_prepare_and_reuse() {
    let harness = Harness::start(&["png"]).await;
    let directory = tempfile::tempdir().expect("should create durable storage");
    let context = harness.context(&directory);
    harness.boundary.lock().expect("should lock").extra_html =
        "<img src=\"data:image/png;base64,not-base64\">".to_owned();
    let target = harness.target();
    warm(&context, &target, 1).await;
    harness
        .boundary
        .lock()
        .expect("should lock")
        .extra_html
        .push_str("<!-- unrelated rotating nonce -->");
    warm(&context, &target, 2).await;
    {
        let state = harness.boundary.lock().expect("should lock");
        assert_eq!(state.preparations, 1);
        assert_eq!(state.finalizations, 1, "the valid sibling stays reusable");
        let EnvelopeItem::Embedded { output_b64, .. } = &state.publications[0].item else {
            panic!("the valid sibling should retain its embedded carrier");
        };
        assert_eq!(
            STANDARD.decode(output_b64).expect("should decode output"),
            state.cases[0].output
        );
    }
    drop(context);
    let context = harness.context(&directory);
    assert_eq!(
        context
            .store
            .active_outcome_counts(now())
            .expect("should read durable operator outcomes")
            .get("malformed_data_uri"),
        Some(&1),
        "unchanged refused media must not accumulate diagnostics for unrelated HTML changes"
    );
}

#[tokio::test]
async fn reader_snapshot_instability_damps_only_the_changed_item_across_restart() {
    use crate::store::Admission;
    use trusted_server_provenance_encypher::preparation::protocol::ObservedItem;

    let harness = Harness::start(&["png", "jpeg", "webp"]).await;
    let directory = tempfile::tempdir().expect("should create durable storage");
    let context = harness.context(&directory);
    let target = harness.target();
    let observed: Vec<_> = {
        let state = harness.boundary.lock().expect("should lock");
        state
            .cases
            .iter()
            .map(|case| ObservedItem {
                kind: ItemKind::Embedded,
                identity: MediaIdentity {
                    credential_context: &context.credential_context,
                    policy_digest: &case.planned.request.expected_policy_digest,
                    page_url: &case.planned.request.page_url,
                    variant: &target.variant,
                    carrier: Carrier::Embedded,
                    mime: &case.planned.request.source.mime,
                    source_sha256: &case.planned.request.source.sha256,
                }
                .digest(),
            })
            .collect()
    };
    // The reader saw PNG + JPEG; the independently acquired snapshot now has WebP + JPEG.
    harness.boundary.lock().expect("should lock").active = vec![2, 1];
    assert!(matches!(
        context
            .store
            .admit(&target, JobOrigin::Signal, &observed[..2], now())
            .expect("should admit"),
        Admission::Queued { .. }
    ));
    let claim = context
        .store
        .claim(now(), 1)
        .expect("should claim")
        .expect("should have work");
    run(&context, &claim)
        .await
        .expect("should settle the observed generation");
    assert!(
        context
            .store
            .complete(&claim.job.key, claim.token, now())
            .expect("should acknowledge")
    );
    let refusal = context
        .store
        .outcome(&target, &observed[0].identity)
        .expect("should read outcome")
        .expect("should record instability");
    assert_eq!(refusal.code, "source_unstable");
    assert_eq!(refusal.class, OutcomeClass::SourceUnstable);
    assert!(
        context
            .store
            .artifact(&observed[0].identity)
            .expect("should read")
            .is_none()
    );
    assert!(
        context
            .store
            .artifact(&observed[2].identity)
            .expect("should read")
            .is_none(),
        "the unobserved replacement must not sign"
    );
    {
        let state = harness.boundary.lock().expect("should lock");
        assert_eq!(state.preparations, 1);
        assert_eq!(state.finalizations, 1);
        assert_eq!(state.publications[0].identity, observed[1].identity);
    }
    drop(context);
    let context = harness.context(&directory);
    assert_eq!(
        context
            .store
            .admit(&target, JobOrigin::Signal, &observed[..1], now())
            .expect("should admit"),
        Admission::Damped,
        "the same unstable observation remains damped after restart"
    );
    assert!(matches!(
        context
            .store
            .admit(&target, JobOrigin::Signal, &observed[1..], now())
            .expect("should admit new observation"),
        Admission::Queued { .. }
    ));
    let claim = context
        .store
        .claim(now(), 2)
        .expect("should claim")
        .expect("should have changed work");
    run(&context, &claim)
        .await
        .expect("should prepare the newly observed replacement");
    assert!(
        context
            .store
            .complete(&claim.job.key, claim.token, now())
            .expect("should acknowledge")
    );
    let state = harness.boundary.lock().expect("should lock");
    assert_eq!(state.preparations, 2);
    assert_eq!(
        state.finalizations, 2,
        "the stable JPEG must not be re-signed"
    );
    assert!(
        state
            .publications
            .iter()
            .all(|item| item.identity != observed[0].identity)
    );
}

#[tokio::test]
async fn crash_after_finalize_recovers_pinned_geometry_and_probe_without_capabilities_or_new_sign()
{
    let harness = Harness::start(&["png"]).await;
    let directory = tempfile::tempdir().expect("should create durable storage");
    {
        let context = harness.context(&directory);
        harness
            .boundary
            .lock()
            .expect("should lock")
            .lose_receipt_once = true;
        let claim = claimed(&context, &harness.target(), 1);
        assert_eq!(
            run(&context, &claim)
                .await
                .expect_err("should lose receipt after committed finalize")
                .class,
            FailureClass::Transient
        );
        assert_eq!(context.store.counts().expect("should count").artifacts, 0);
    }
    let previous_capabilities = {
        let mut state = harness.boundary.lock().expect("should lock");
        state.closed = true;
        assert_eq!(state.finalizations, 1);
        state.capabilities
    };
    let context = harness.context(&directory);
    let page = harness.boundary.lock().expect("should lock").cases[0]
        .planned
        .request
        .page_url
        .clone();
    probe_ready(&context, &page)
        .await
        .expect("readiness must accept retained pinned releases while creation is closed");
    let claim = context
        .store
        .claim(now(), 2)
        .expect("should recover durable claim")
        .expect("should keep unfinished generation");
    assert!(matches!(
        claim.job.state,
        JobState::Processing { token: 2, .. }
    ));
    run(&context, &claim)
        .await
        .expect("should reconstruct exact completed receipt while creation is disabled");
    let state = harness.boundary.lock().expect("should lock");
    assert_eq!(state.preparations, 1);
    assert_eq!(state.finalizations, 1);
    assert_eq!(state.finalize_attempts, 1);
    assert_eq!(
        state.capabilities, previous_capabilities,
        "recovery bypasses creation capability admission"
    );
    assert_eq!(
        state.probe_fetches, 1,
        "restart retains the exact authenticated release"
    );
    assert_eq!(state.publications.len(), 1);
}

#[tokio::test]
async fn real_external_media_uses_small_and_pinned_large_lanes_without_upload() {
    for large in [false, true] {
        let harness = Harness::start(&["external-png"]).await;
        let directory = tempfile::tempdir().expect("should create durable storage");
        {
            let mut state = harness.boundary.lock().expect("should lock");
            state.standalone = true;
            state.large = large;
        }
        let context = harness.context(&directory);
        warm(&context, &harness.target(), 1).await;
        let published_identity = {
            let state = harness.boundary.lock().expect("should lock");
            assert_eq!(state.finalizations, 1);
            assert_eq!(state.publications.len(), 1);
            assert_eq!(state.range_reads > 0, large);
            assert!(matches!(&state.publications[0].item, EnvelopeItem::Large { .. }) == large);
            assert!(matches!(&state.publications[0].item, EnvelopeItem::Media { .. }) != large);
            if let EnvelopeItem::Large {
                planned,
                etag,
                origin_identity_sha256,
            } = &state.publications[0].item
            {
                assert_eq!(origin_identity_sha256, &state.origin_identity_sha256);
                assert_eq!(
                    state.publications[0].identity,
                    LargeLocator {
                        credential_context: &context.credential_context,
                        policy_digest: &planned.request.expected_policy_digest,
                        reader_url: &planned.request.page_url,
                        variant: "",
                        origin_host: &planned.request.source.origin_host,
                        origin_identity_sha256,
                        mime: &planned.request.source.mime,
                        etag,
                        length: planned.request.source.byte_length,
                    }
                    .digest()
                );
            }
            assert!(
                !directory
                    .path()
                    .join("large")
                    .read_dir()
                    .is_ok_and(|mut entries| entries.next().is_some()),
                "temporary pinned sources are removed before their reservations are released"
            );
            state.publications[0].identity.clone()
        };
        drop(context);
        harness.boundary.lock().expect("should lock").closed = true;
        let context = harness.context(&directory);
        warm(&context, &harness.target(), 2).await;
        let state = harness.boundary.lock().expect("should lock");
        assert_eq!(state.preparations, 1);
        assert_eq!(state.finalizations, 1);
        assert_eq!(state.publications.len(), 2);
        assert_eq!(state.publications[1].identity, published_identity);
    }
}

#[tokio::test]
async fn configured_origin_remap_refuses_large_acquisition_and_both_current_checks() {
    for (remap_on_snapshot, preparations, finalizations) in [(2, 0, 0), (3, 1, 0), (4, 1, 1)] {
        let harness = Harness::start(&["external-png"]).await;
        let directory = tempfile::tempdir().expect("should create durable storage");
        {
            let mut state = harness.boundary.lock().expect("should lock");
            state.standalone = true;
            state.large = true;
            state.remap_on_snapshot = Some(remap_on_snapshot);
        }
        let context = harness.context(&directory);
        let target = harness.target();
        let claim = claimed(&context, &target, 1);
        run(&context, &claim)
            .await
            .expect("should persist integrity refusal");
        let state = harness.boundary.lock().expect("should lock");
        assert_eq!(state.preparations, preparations);
        assert_eq!(state.finalizations, finalizations);
        assert_eq!(state.range_reads, 1);
        assert!(state.publications.is_empty());
        assert_eq!(context.store.counts().expect("should count").artifacts, 0);
        let request = &state.cases[0].planned.request;
        let expected_origin_identity = crate::hex_lower(&Sha256::digest(
            b"https://origin.example.com/old/\0origin.example.com",
        ));
        let identity = LargeLocator {
            credential_context: &context.credential_context,
            policy_digest: &request.expected_policy_digest,
            reader_url: &request.page_url,
            variant: "",
            origin_host: &request.source.origin_host,
            origin_identity_sha256: &expected_origin_identity,
            mime: &request.source.mime,
            etag: &state.etag,
            length: request.source.byte_length,
        }
        .digest();
        assert_eq!(
            context
                .store
                .outcome(&target, &identity)
                .expect("should read item outcome")
                .expect("should retain configured-origin refusal")
                .code,
            "source_version_changed"
        );
    }
}

#[tokio::test]
async fn invalid_configured_origin_digest_refuses_large_work_before_acquisition() {
    for invalid in ["".to_owned(), "not-a-digest".to_owned(), "A".repeat(64)] {
        let harness = Harness::start(&["external-png"]).await;
        let directory = tempfile::tempdir().expect("should create durable storage");
        {
            let mut state = harness.boundary.lock().expect("should lock");
            state.standalone = true;
            state.large = true;
            state.origin_identity_sha256 = invalid.to_owned();
        }
        let context = harness.context(&directory);
        let target = harness.target();
        warm(&context, &target, 1).await;
        let state = harness.boundary.lock().expect("should lock");
        assert_eq!(state.range_reads, 0);
        assert_eq!(state.preparations, 0);
        assert_eq!(state.finalizations, 0);
        assert!(state.publications.is_empty());
        assert_eq!(
            context
                .store
                .outcome(&target, "*")
                .expect("should read snapshot outcome")
                .expect("should retain invalid-origin refusal")
                .code,
            "source_origin_identity_invalid"
        );
    }
}

#[tokio::test]
async fn truncated_pinned_source_is_refused_before_prepare_and_cleans_its_claim_owned_file() {
    let harness = Harness::start(&["external-png"]).await;
    let directory = tempfile::tempdir().expect("should create storage");
    {
        let mut state = harness.boundary.lock().expect("should lock");
        state.standalone = true;
        state.large = true;
        state.short_range = true;
    }
    let context = harness.context(&directory);
    warm(&context, &harness.target(), 1).await;
    let state = harness.boundary.lock().expect("should lock");
    assert_eq!(state.preparations, 0);
    assert_eq!(state.finalizations, 0);
    assert!(state.publications.is_empty());
    assert!(
        context
            .store
            .reserved()
            .expect("should retain only bounded refusal metadata")
            < 4096
    );
    assert!(
        !directory
            .path()
            .join("large")
            .read_dir()
            .expect("should have source directory")
            .any(|entry| entry.is_ok())
    );
}

#[tokio::test]
async fn retired_completed_receipt_refuses_recovery_without_any_finalize_attempt() {
    let harness = Harness::start(&["png"]).await;
    let directory = tempfile::tempdir().expect("should create durable storage");
    {
        let context = harness.context(&directory);
        harness
            .boundary
            .lock()
            .expect("should lock")
            .lose_receipt_once = true;
        let claim = claimed(&context, &harness.target(), 1);
        run(&context, &claim)
            .await
            .expect_err("should simulate crash after API commit");
    }
    {
        let mut state = harness.boundary.lock().expect("should lock");
        state.closed = true;
        state.retired_receipt = true;
    }
    let context = harness.context(&directory);
    let claim = context
        .store
        .claim(now(), 2)
        .expect("should reclaim")
        .expect("should retain exact operation");
    run(&context, &claim)
        .await
        .expect("should record explicit terminal retirement refusal");
    let state = harness.boundary.lock().expect("should lock");
    assert_eq!(state.finalizations, 1);
    assert_eq!(
        state.finalize_attempts, 1,
        "HTTP410 must never fall through to a finalize call"
    );
    assert_eq!(state.preparations, 1);
    assert!(state.publications.is_empty());
    assert_eq!(context.store.counts().expect("should count").artifacts, 0);
    let request = &state.cases[0].planned.request;
    let identity = MediaIdentity {
        credential_context: &context.credential_context,
        policy_digest: &request.expected_policy_digest,
        page_url: &request.page_url,
        variant: "",
        carrier: Carrier::Embedded,
        mime: &request.source.mime,
        source_sha256: &request.source.sha256,
    }
    .digest();
    assert_eq!(
        context
            .store
            .outcome(&claim.job.target, &identity)
            .expect("should retain outcome")
            .expect("should expose retired refusal")
            .code,
        "result_retired"
    );
}

#[tokio::test]
async fn corrupt_durable_output_is_refused_without_replanning_or_signing() {
    let harness = Harness::start(&["png"]).await;
    let directory = tempfile::tempdir().expect("should create storage");
    let context = harness.context(&directory);
    warm(&context, &harness.target(), 1).await;
    let identity = harness.boundary.lock().expect("should lock").publications[0]
        .identity
        .clone();
    let path = directory
        .path()
        .join("artifacts")
        .join(format!("{identity}.json"));
    let mut bytes = std::fs::read(&path).expect("should inspect durable output");
    bytes[0] ^= 1;
    std::fs::write(path, bytes).expect("should simulate disk corruption");
    harness.boundary.lock().expect("should lock").closed = true;
    let claim = claimed(&context, &harness.target(), 2);
    assert_eq!(
        run(&context, &claim)
            .await
            .expect_err("should refuse corrupt durable material")
            .code,
        "store_unavailable"
    );
    let state = harness.boundary.lock().expect("should lock");
    assert_eq!(state.preparations, 1);
    assert_eq!(state.finalize_attempts, 1);
    assert_eq!(
        state.publications.len(),
        1,
        "corrupt bytes never reach publication"
    );
}

#[tokio::test]
async fn private_media_and_existing_real_embedded_provenance_are_never_replaced_or_uploaded() {
    for existing_provenance in [false, true] {
        let harness = Harness::start(&["png"]).await;
        let directory = tempfile::tempdir().expect("should create storage");
        {
            let mut state = harness.boundary.lock().expect("should lock");
            if existing_provenance {
                state.cases[0].source = state.cases[0].output.clone();
            } else {
                state.private = true;
            }
        }
        let context = harness.context(&directory);
        warm(&context, &harness.target(), 1).await;
        let state = harness.boundary.lock().expect("should lock");
        assert_eq!(state.preparations, 0);
        assert_eq!(state.finalize_attempts, 0);
        assert!(state.publications.is_empty());
        assert_eq!(context.store.counts().expect("should count").artifacts, 0);
    }
}
