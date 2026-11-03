//! Worker behavior over real redb and local HTTP boundaries. The API fixture
//! persists exact text receipts; it never stands in for the worker or probe.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use trusted_server_preparation_worker::client::{Api, Edge, Secrets, WorkerConfig};
use trusted_server_preparation_worker::prepare::{Context, now, run};
use trusted_server_preparation_worker::store::{
    Admission, Artifact, Bounds, Claim, JobOrigin, OutcomeClass, Store, target_key,
};
use trusted_server_provenance_encypher::preparation::pipeline::FailureClass;
use trusted_server_provenance_encypher::preparation::protocol::{
    ItemKind, ObservedItem, PREPARATION_PROTOCOL, ResultEnvelope, SNAPSHOT_META_HEADER,
    SnapshotMeta, SnapshotMode, SnapshotOrigin, SnapshotRequest, Target,
};

#[derive(Default)]
struct Boundary {
    documents: HashMap<String, String>,
    receipts: HashMap<String, (Value, Value)>,
    signs: usize,
    sign_requests: usize,
    recovers: usize,
    capabilities: usize,
    publications: Vec<ResultEnvelope>,
    creation_closed: bool,
    lose_sign_reply: bool,
    reject_publication: bool,
    private: bool,
    ranges: Vec<(u64, u64)>,
    wrong_range_length: bool,
}

struct Fixture {
    state: Arc<Mutex<Boundary>>,
    url: String,
    server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn start() -> Self {
        let state = Arc::new(Mutex::new(Boundary::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("should bind boundary");
        let url = format!(
            "http://{}",
            listener.local_addr().expect("should read address")
        );
        let app = Router::new()
            .fallback(boundary)
            .with_state(Arc::clone(&state));
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("should serve boundary");
        });
        Self { state, url, server }
    }

    fn document(&self, path: &str, text: &str) {
        self.state.lock().expect("should lock boundary").documents.insert(path.to_owned(), format!("<html><head><title>Harbor</title></head><body><article>{text}</article></body></html>"));
    }

    fn context(&self, dir: &tempfile::TempDir) -> Context {
        let config: WorkerConfig = toml::from_str(&format!(
            r#"
listen = "0.0.0.0:8112"
store_dir = {:?}
deployment_id = "fixture"
api_base_url = {:?}
domain = "example.com"
public_host = "www.example.com"
edge_url = {:?}
min_text_length = 1
article_selector = "article"
max_large_media_bytes = 1048576
max_snapshot_bytes = 1048576
max_media_bytes = 1048576
[secrets]
worker_token = "FIXTURE_WORKER"
signal_secret = "FIXTURE_SIGNAL"
publication_secret = "FIXTURE_PUBLICATION"
operator_secret = "FIXTURE_OPERATOR"
"#,
            dir.path().to_str().expect("should have path"),
            self.url,
            self.url
        ))
        .expect("should parse worker configuration");
        let store = Arc::new(
            Store::open(&dir.path().join("preparation.redb"), Bounds::default())
                .expect("should open real store"),
        );
        store.recover_storage().expect("should recover real files");
        store.recover_all(now()).expect("should recover claims");
        let secrets = Secrets {
            worker_token: "cwk_fixture".to_owned(),
            signal: "signal".to_owned(),
            publication: "publication".to_owned(),
            operator: "operator".to_owned(),
            canary: None,
        };
        Context::new(
            store,
            Api::new(&self.url, &secrets),
            Edge::new(&self.url, &secrets, "fixture"),
            Arc::new(config),
            &secrets.worker_token,
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn response(status: StatusCode, value: Value) -> Response {
    (status, axum::Json(value)).into_response()
}

async fn boundary(State(state): State<Arc<Mutex<Boundary>>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let headers = request.headers().clone();
    let bytes = to_bytes(request.into_body(), 8 * 1024 * 1024)
        .await
        .expect("should read bounded request");
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let mut state = state.lock().expect("should lock boundary");
    if path.ends_with("/policy") {
        return response(StatusCode::OK, json!({"policy_digest": "a".repeat(64)}));
    }
    if path.ends_with("/capabilities") {
        state.capabilities += 1;
        return response(
            StatusCode::FORBIDDEN,
            json!({"error":{"code":"CREATION_DISABLED"}}),
        );
    }
    if path == "/api/v1/public/cdn/sign" || path == "/api/v1/public/cdn/sign/recover" {
        assert_eq!(
            value["cache_scope"], "exact_request",
            "both signing and recovery must use the exact-request API cache partition"
        );
        assert_eq!(
            headers
                .get("x-encypher-worker-token")
                .expect("should authenticate API"),
            "cwk_fixture"
        );
        let key = headers
            .get("idempotency-key")
            .expect("should carry operation identity")
            .to_str()
            .expect("should be text")
            .to_owned();
        if path.ends_with("/recover") {
            state.recovers += 1;
            return match state.receipts.get(&key) {
                Some((frozen, answer)) if frozen == &value => {
                    response(StatusCode::OK, answer.clone())
                }
                Some(_) => response(StatusCode::CONFLICT, json!({"error":{"code":"CONFLICT"}})),
                None => response(StatusCode::NOT_FOUND, json!({"error":{"code":"MISSING"}})),
            };
        }
        state.sign_requests += 1;
        if state.creation_closed {
            return response(
                StatusCode::FORBIDDEN,
                json!({"error":{"code":"CREATION_DISABLED"}}),
            );
        }
        assert!(
            !state.receipts.contains_key(&key),
            "the worker must recover a completed operation, not call sign as replay"
        );
        state.signs += 1;
        let answer = json!({"success":true,"policy_digest":"a".repeat(64),"embedding_plan":{"index_unit":"codepoint","operations":[{"insert_after_index":0,"marker":"\u{fe00}"}]},"manifest_url":"https://manifest.example.com/receipt"});
        state.receipts.insert(key, (value, answer.clone()));
        if state.lose_sign_reply {
            state.lose_sign_reply = false;
            return response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error":{"code":"LOST_REPLY"}}),
            );
        }
        return response(StatusCode::OK, answer);
    }
    assert_eq!(
        headers
            .get("x-encypher-preparation-token")
            .expect("should authenticate edge"),
        "publication"
    );
    if path == "/_encypher/preparation/snapshot" {
        let request: SnapshotRequest =
            serde_json::from_value(value).expect("should send snapshot request");
        let Some(document) = state.documents.get(&request.target.path) else {
            return response(
                StatusCode::FORBIDDEN,
                json!({"error":{"code":"target_not_configured"}}),
            );
        };
        let mut body = document.as_bytes().to_vec();
        let mut origin_status = 200;
        let length = body.len() as u64;
        match request.read {
            SnapshotMode::Whole => {}
            SnapshotMode::Head => body.clear(),
            SnapshotMode::Range { first, last, .. } => {
                body = body[first as usize..=last as usize].to_vec();
                if state.wrong_range_length {
                    body.pop();
                }
                state.ranges.push((first, last));
                origin_status = 206;
            }
        }
        let meta = SnapshotMeta {
            protocol: PREPARATION_PROTOCOL.to_owned(),
            page_url: format!("https://www.example.com{}", request.target.path),
            content_type: "text/html".to_owned(),
            etag: Some("\"stable\"".to_owned()),
            length: Some(length),
            origin: SnapshotOrigin {
                status: origin_status,
                private: state.private,
                no_transform: false,
                manifest_link: false,
                content_encoding: String::new(),
                origin_host: "origin.example.com".to_owned(),
                origin_identity_sha256: String::new(),
                source_reference: Some(format!(
                    "https://origin.example.com{}",
                    request.target.path
                )),
            },
        };
        let encoded = STANDARD.encode(serde_json::to_vec(&meta).expect("should encode metadata"));
        return (
            [
                (SNAPSHOT_META_HEADER, encoded),
                ("cache-control", "private, no-store".to_owned()),
            ],
            body,
        )
            .into_response();
    }
    if path.starts_with("/_encypher/preparation/results/") {
        if state.reject_publication {
            return response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error":{"code":"UNAVAILABLE"}}),
            );
        }
        state
            .publications
            .push(serde_json::from_value(value).expect("should publish a real envelope"));
        return response(StatusCode::OK, json!({"stored":true}));
    }
    if path.starts_with("/_encypher/preparation/markers/") {
        return response(StatusCode::OK, json!({"stored":true}));
    }
    response(
        StatusCode::NOT_FOUND,
        json!({"error":{"code":"unexpected_route"}}),
    )
}

fn target(path: &str) -> Target {
    Target {
        path: path.to_owned(),
        variant: String::new(),
    }
}

fn claim(
    context: &Context,
    target: &Target,
    origin: JobOrigin,
    items: &[ObservedItem],
    token: u64,
) -> Claim {
    assert!(matches!(
        context
            .store
            .admit(target, origin, items, now())
            .expect("should admit"),
        Admission::Queued { .. } | Admission::Coalesced { .. }
    ));
    context
        .store
        .claim(now(), token)
        .expect("should claim")
        .expect("should have work")
}

async fn warm(context: &Context, target: &Target, token: u64) {
    let claim = claim(context, target, JobOrigin::Warm, &[], token);
    run(context, &claim).await.expect("should finish warm");
    assert!(
        context
            .store
            .complete(&claim.job.key, claim.token, now())
            .expect("should settle")
    );
}

#[tokio::test]
async fn changed_only_preparation_reuses_stable_siblings_and_restores_evicted_delivery_records() {
    let fixture = Fixture::start().await;
    let dir = tempfile::tempdir().expect("should create store");
    fixture.document("/a", "The harbor vote passed.");
    fixture.document("/b", "The ferry leaves at noon.");
    let context = fixture.context(&dir);
    warm(&context, &target("/a"), 1).await;
    warm(&context, &target("/b"), 2).await;
    assert_eq!(fixture.state.lock().expect("should lock").signs, 2);
    let original = fixture
        .state
        .lock()
        .expect("should lock")
        .publications
        .clone();
    fixture.document("/a", "The harbor vote was amended.");
    warm(&context, &target("/a"), 3).await;
    warm(&context, &target("/b"), 4).await;
    let state = fixture.state.lock().expect("should lock");
    assert_eq!(state.signs, 3, "only the changed exact identity signs");
    assert_ne!(state.publications[2].identity, original[0].identity);
    assert_eq!(
        state.publications[3], original[1],
        "delivery eviction restores exactly the retained envelope"
    );
    assert_eq!(context.store.counts().expect("should count").artifacts, 3);
}

#[tokio::test]
async fn title_and_mirror_url_changes_take_new_exact_operations_but_unchanged_text_reuses() {
    let fixture = Fixture::start().await;
    let dir = tempfile::tempdir().expect("should create store");
    fixture.document("/a", "The harbor vote passed.");
    let context = fixture.context(&dir);
    warm(&context, &target("/a"), 1).await;
    {
        let mut state = fixture.state.lock().expect("should lock boundary");
        let original = state.documents["/a"].clone();
        state.documents.insert(
            "/a".to_owned(),
            original.replace(
                "<title>Harbor</title>",
                "<title>Revised harbor title</title>",
            ),
        );
    }
    warm(&context, &target("/a"), 2).await;
    {
        let mut state = fixture.state.lock().expect("should lock boundary");
        let revised = state.documents["/a"].clone();
        state.documents.insert("/mirror".to_owned(), revised);
    }
    warm(&context, &target("/mirror"), 3).await;
    warm(&context, &target("/mirror"), 4).await;
    let state = fixture.state.lock().expect("should lock boundary");
    assert_eq!(state.signs, 3, "only new exact text requests should sign");
    assert_eq!(
        state.receipts.len(),
        3,
        "each metadata context has its own operation"
    );
    assert_ne!(
        state.publications[0].identity,
        state.publications[1].identity
    );
    assert_ne!(
        state.publications[1].identity,
        state.publications[2].identity
    );
    assert_eq!(state.publications[2], state.publications[3]);
    let requests: Vec<&Value> = state
        .receipts
        .values()
        .map(|(request, _)| request)
        .collect();
    assert!(
        requests
            .iter()
            .all(|request| request["text"] == "The harbor vote passed.")
    );
    assert!(
        requests
            .iter()
            .any(|request| request["document_title"] == "Harbor")
    );
    assert!(requests.iter().any(|request| {
        request["document_title"] == "Revised harbor title"
            && request["page_url"] == "https://www.example.com/a"
    }));
    assert!(
        requests
            .iter()
            .any(|request| request["page_url"] == "https://www.example.com/mirror")
    );
}

#[tokio::test]
async fn six_hour_store_maintenance_and_restart_keep_exact_artifact_reusable_with_creation_closed()
{
    let fixture = Fixture::start().await;
    let dir = tempfile::tempdir().expect("should create store");
    let page = target("/a");
    fixture.document(
        "/a",
        "Unchanged editorial content needs no hourly signing renewal.",
    );
    let context = fixture.context(&dir);
    warm(&context, &page, 1).await;
    let original = fixture.state.lock().expect("should lock").publications[0].clone();
    let retained = context
        .store
        .artifact(&original.identity)
        .expect("should read")
        .expect("should retain durable artifact");
    let charge = context
        .store
        .reserved()
        .expect("should charge artifact and reconstruction");
    let maintenance_start = now();
    fixture.state.lock().expect("should lock").creation_closed = true;
    // Store maintenance accepts explicit Unix seconds. This advances those
    // controls only; preparation's SystemTime clock does not advance.
    for hours in [1, 3, 6] {
        let at = maintenance_start + hours * 3600;
        assert_eq!(
            context
                .store
                .recover_expired(at)
                .expect("should sweep expired claims"),
            0
        );
        assert_eq!(
            context
                .store
                .prune_expired_outcomes(at)
                .expect("should sweep expired outcomes"),
            0
        );
        assert_eq!(
            context
                .store
                .artifact(&original.identity)
                .expect("should read retained result"),
            Some(retained.clone()),
            "multi-hour maintenance must not time-expire an unchanged artifact"
        );
        assert_eq!(
            context
                .store
                .reserved()
                .expect("should preserve quota charge"),
            charge
        );
    }
    drop(context);
    let context = fixture.context(&dir);
    assert_eq!(
        context
            .store
            .artifact(&original.identity)
            .expect("should read after recovery"),
        Some(retained)
    );
    assert_eq!(
        context
            .store
            .reserved()
            .expect("should retain charged reconstruction after restart"),
        charge
    );
    let recovers_before = fixture.state.lock().expect("should lock").recovers;
    for token in 2..=4 {
        warm(&context, &page, token).await;
    }
    let state = fixture.state.lock().expect("should lock");
    assert_eq!(state.signs, 1);
    assert_eq!(
        state.sign_requests, 1,
        "unchanged reuse must not even attempt a denied signing request"
    );
    assert_eq!(
        state.recovers, recovers_before,
        "a retained artifact needs no worker-side API replay or renewal"
    );
    assert_eq!(state.capabilities, 0);
    assert_eq!(state.publications.len(), 4);
    assert!(
        state
            .publications
            .iter()
            .all(|envelope| envelope == &original),
        "delivery eviction is repaired by the same immutable artifact and operation"
    );
    assert_eq!(
        context
            .store
            .counts()
            .expect("should retain one version")
            .artifacts,
        1
    );
}

#[tokio::test]
async fn crash_after_api_commit_recovers_with_creation_closed_and_never_calls_sign_again() {
    let fixture = Fixture::start().await;
    let dir = tempfile::tempdir().expect("should create store");
    fixture.document("/a", "A durable exact text receipt.");
    {
        let context = fixture.context(&dir);
        fixture.state.lock().expect("should lock").lose_sign_reply = true;
        let claim = claim(&context, &target("/a"), JobOrigin::Warm, &[], 1);
        assert_eq!(
            run(&context, &claim)
                .await
                .expect_err("should lose API reply")
                .class,
            FailureClass::Transient
        );
        assert_eq!(fixture.state.lock().expect("should lock").signs, 1);
        // Simulate process death without settling: redb and attempted operation survive.
    }
    fixture.state.lock().expect("should lock").creation_closed = true;
    let context = fixture.context(&dir);
    let claim = context
        .store
        .claim(now(), 2)
        .expect("should reclaim")
        .expect("should recover unfinished job");
    run(&context, &claim)
        .await
        .expect("should recover with signing disabled");
    let state = fixture.state.lock().expect("should lock");
    assert_eq!(state.signs, 1);
    assert_eq!(state.publications.len(), 1);
    assert!(state.recovers >= 2);
    assert_eq!(
        state.capabilities, 0,
        "text recovery never depends on creation capability"
    );
}

#[tokio::test]
async fn durable_artifact_survives_lost_publication_and_restart_without_api_creation() {
    let fixture = Fixture::start().await;
    let dir = tempfile::tempdir().expect("should create store");
    fixture.document("/a", "Retained output survives restart.");
    {
        let context = fixture.context(&dir);
        fixture
            .state
            .lock()
            .expect("should lock")
            .reject_publication = true;
        let claim = claim(&context, &target("/a"), JobOrigin::Warm, &[], 1);
        run(&context, &claim)
            .await
            .expect_err("should retain before failed push");
        assert_eq!(context.store.counts().expect("should count").artifacts, 1);
    }
    {
        let mut state = fixture.state.lock().expect("should lock");
        state.reject_publication = false;
        state.creation_closed = true;
    }
    let context = fixture.context(&dir);
    let claim = context
        .store
        .claim(now(), 2)
        .expect("should reclaim")
        .expect("should have work");
    run(&context, &claim)
        .await
        .expect("should republish durable artifact");
    let state = fixture.state.lock().expect("should lock");
    assert_eq!(state.signs, 1);
    assert_eq!(state.publications.len(), 1);
    assert!(
        context
            .store
            .artifact(&state.publications[0].identity)
            .expect("should read")
            .expect("should retain")
            .acknowledged
    );
}

#[tokio::test]
async fn private_snapshot_refuses_before_policy_or_signing_and_is_damped_per_observation() {
    let fixture = Fixture::start().await;
    let dir = tempfile::tempdir().expect("should create store");
    fixture.document("/a", "Private source must never be admitted.");
    fixture.state.lock().expect("should lock").private = true;
    let context = fixture.context(&dir);
    let observed = ObservedItem {
        identity: "b".repeat(64),
        kind: ItemKind::Text,
    };
    let claim = claim(
        &context,
        &target("/a"),
        JobOrigin::Signal,
        std::slice::from_ref(&observed),
        1,
    );
    run(&context, &claim)
        .await
        .expect("should record privacy refusal");
    let outcome = context
        .store
        .outcome(&target("/a"), &observed.identity)
        .expect("should read")
        .expect("should damp");
    assert_eq!(outcome.class, OutcomeClass::Terminal);
    assert_eq!(outcome.code, "source_private");
    assert_eq!(fixture.state.lock().expect("should lock").signs, 0);
}

#[tokio::test]
async fn unstable_child_does_not_suppress_stable_text_or_create_signing_churn() {
    let fixture = Fixture::start().await;
    let dir = tempfile::tempdir().expect("should create store");
    fixture.document("/a", "Stable editorial text alongside a rotating child.");
    let context = fixture.context(&dir);
    warm(&context, &target("/a"), 1).await;
    let identity = fixture.state.lock().expect("should lock").publications[0]
        .identity
        .clone();
    let rotating = ObservedItem {
        identity: "c".repeat(64),
        kind: ItemKind::Embedded,
    };
    let stable = ObservedItem {
        identity: identity.clone(),
        kind: ItemKind::Text,
    };
    let claim = claim(
        &context,
        &target("/a"),
        JobOrigin::Signal,
        &[rotating.clone(), stable],
        2,
    );
    run(&context, &claim)
        .await
        .expect("should reuse stable sibling");
    assert_eq!(
        context
            .store
            .outcome(&target("/a"), &rotating.identity)
            .expect("should read")
            .expect("should damp")
            .class,
        OutcomeClass::SourceUnstable
    );
    let state = fixture.state.lock().expect("should lock");
    assert_eq!(state.signs, 1);
    assert_eq!(state.publications.len(), 2);
    assert_eq!(state.publications[1].identity, identity);
}

#[tokio::test]
async fn independent_inventory_requires_consecutive_stability_and_survives_restart() {
    let fixture = Fixture::start().await;
    let dir = tempfile::tempdir().expect("should create store");
    fixture.document(
        "/registered",
        "An unseen page with independent publisher inventory.",
    );
    {
        let context = fixture.context(&dir);
        context
            .store
            .register(&[target("/registered")])
            .expect("should register without reader hint");
        let due = context
            .store
            .due_inventory(1000, 900, 128)
            .expect("should scan");
        assert_eq!(due, vec![target("/registered")]);
        let claim = claim(&context, &due[0], JobOrigin::Reconcile, &[], 1);
        run(&context, &claim).await.expect("should observe only");
        context
            .store
            .complete(&claim.job.key, claim.token, now())
            .expect("should settle");
        assert_eq!(fixture.state.lock().expect("should lock").signs, 0);
    }
    let context = fixture.context(&dir);
    let claim = claim(
        &context,
        &target("/registered"),
        JobOrigin::Reconcile,
        &[],
        2,
    );
    run(&context, &claim)
        .await
        .expect("should prepare consecutive stable snapshot");
    assert_eq!(fixture.state.lock().expect("should lock").signs, 1);
}

#[tokio::test]
async fn retired_result_cannot_be_recovered_by_warm_or_late_claim_across_restart() {
    let fixture = Fixture::start().await;
    let dir = tempfile::tempdir().expect("should create store");
    let page = target("/a");
    fixture.document("/a", "An explicitly retired identity stays spent.");
    let (artifact, late, epoch) = {
        let context = fixture.context(&dir);
        warm(&context, &page, 1).await;
        let envelope = fixture.state.lock().expect("should lock").publications[0].clone();
        let artifact = Artifact {
            envelope,
            target: page.clone(),
            acknowledged: true,
        };
        let late = claim(&context, &page, JobOrigin::Warm, &[], 2);
        let epoch = context
            .store
            .retire(&artifact.envelope.identity, "fixture")
            .expect("should retire");
        let counts = context
            .store
            .counts()
            .expect("should report local retirement separately");
        assert_eq!(counts.artifacts, 0);
        assert_eq!(counts.retirement_pending, 1);
        assert_eq!(counts.retirement_acknowledged, 0);
        assert_eq!(
            context
                .store
                .outbox()
                .expect("should persist marker before delivery")
                .len(),
            1
        );
        (artifact, late, epoch)
    };
    fixture.state.lock().expect("should lock").creation_closed = true;
    {
        let context = fixture.context(&dir);
        let identity = &artifact.envelope.identity;
        let marker = context
            .store
            .outbox()
            .expect("should retain delayed marker across restart")
            .pop()
            .expect("should have marker");
        assert_eq!(marker.identity, *identity);
        assert_eq!(marker.epoch, epoch);
        assert_eq!(
            context
                .store
                .retire(identity, "fixture")
                .expect("should preserve existing retirement"),
            epoch
        );
        assert!(
            !context
                .store
                .record_artifact(&artifact)
                .expect("should reject delayed old artifact")
        );
        assert!(
            context
                .store
                .artifact(identity)
                .expect("should refuse retired result")
                .is_none()
        );
        assert_eq!(
            run(&context, &late)
                .await
                .expect_err("should fence pre-restart worker")
                .code,
            "lease_lost"
        );
        let recovered = context
            .store
            .claim(now(), 3)
            .expect("should claim unfinished warm")
            .expect("should retain unfinished target");
        run(&context, &recovered)
            .await
            .expect("should refuse retired identity without signing");
        assert!(
            context
                .store
                .complete(&recovered.job.key, recovered.token, now())
                .expect("should settle retired work")
        );
        warm(&context, &page, 4).await;
        let observed = ObservedItem {
            identity: identity.clone(),
            kind: ItemKind::Text,
        };
        assert_eq!(
            context
                .store
                .admit(&page, JobOrigin::Signal, &[observed], now())
                .expect("should refuse reader resurrection"),
            Admission::Damped
        );
        context
            .store
            .acknowledge_marker(identity, epoch + 1)
            .expect("should ignore wrong acknowledgement");
        assert_eq!(
            context
                .store
                .outbox()
                .expect("should keep pending exact epoch"),
            vec![marker]
        );
        assert_eq!(
            context
                .store
                .counts()
                .expect("should report still pending")
                .retirement_pending,
            1
        );
        context
            .store
            .acknowledge_marker(identity, epoch)
            .expect("should acknowledge exact marker");
        assert!(
            context
                .store
                .outbox()
                .expect("should settle outbox")
                .is_empty()
        );
        let counts = context
            .store
            .counts()
            .expect("should distinguish acknowledged retirement");
        assert_eq!(counts.retirement_pending, 0);
        assert_eq!(counts.retirement_acknowledged, 1);
    }
    let context = fixture.context(&dir);
    assert!(
        context
            .store
            .retirement(&artifact.envelope.identity)
            .expect("should read tombstone after second restart")
            .expect("should remain retired")
            .acknowledged
    );
    assert!(
        context
            .store
            .outbox()
            .expect("should retain acknowledged state")
            .is_empty()
    );
    assert!(
        !context
            .store
            .record_artifact(&artifact)
            .expect("should reject late result even after marker acknowledgement")
    );
    warm(&context, &page, 5).await;
    assert!(
        context
            .store
            .job(&target_key(&page))
            .expect("should read")
            .is_none()
    );
    let state = fixture.state.lock().expect("should lock");
    assert_eq!(state.signs, 1);
    assert_eq!(state.sign_requests, 1);
    assert_eq!(
        state.publications.len(),
        1,
        "no late claim, warm or restart can publish retired bytes again"
    );
}
