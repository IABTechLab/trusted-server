//! Management routes and the real supervised housekeeping loop over HTTP/redb.
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

use axum::extract::Request;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::*;
use trusted_server_provenance_encypher::preparation::protocol::{
    MARKERS_PREFIX, MarkerEnvelope, VariantParam, VariantSchema,
};

fn worker_config(dir: &tempfile::TempDir, edge_url: &str) -> WorkerConfig {
    toml::from_str(&format!(
        r#"
listen = "0.0.0.0:8112"
store_dir = {:?}
deployment_id = "owner-http-test"
api_base_url = {:?}
edge_url = {:?}
domain = "example.com"
public_host = "www.example.com"
[secrets]
worker_token = "TEST_WORKER"
signal_secret = "TEST_SIGNAL"
publication_secret = "TEST_PUBLICATION"
operator_secret = "TEST_OPERATOR"
"#,
        dir.path().to_str().expect("should have path"),
        edge_url,
        edge_url
    ))
    .expect("should parse real worker configuration")
}

fn shared(dir: &tempfile::TempDir, bounds: Bounds, edge_url: &str) -> Arc<Shared> {
    shared_with_config(worker_config(dir, edge_url), bounds)
}

fn shared_with_config(config: WorkerConfig, bounds: Bounds) -> Arc<Shared> {
    config
        .check()
        .expect("should admit publisher configuration");
    let store = Arc::new(
        Store::open(&config.store_dir.join("preparation.redb"), bounds)
            .expect("should own durable store"),
    );
    store
        .recover_storage()
        .expect("should recover store ownership before HTTP");
    Arc::new(Shared {
        store,
        config: Arc::new(config),
        secrets: Secrets {
            worker_token: "cwk_fixture".to_owned(),
            signal: "signal".to_owned(),
            publication: "publication".to_owned(),
            operator: "operator".to_owned(),
            canary: None,
        },
        ready: AtomicBool::new(true),
    })
}

async fn serve(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("should bind owner HTTP");
    let url = format!(
        "http://{}",
        listener.local_addr().expect("should read address")
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("should serve owner HTTP");
    });
    (url, task)
}

#[tokio::test]
async fn warm_registers_independent_inventory_and_reports_capacity_per_target() {
    let dir = tempfile::tempdir().expect("should create store");
    let shared = shared(
        &dir,
        Bounds {
            max_jobs: 1,
            reserved_jobs: 0,
            ..Bounds::default()
        },
        "http://127.0.0.1:9",
    );
    let (url, server) = serve(router(Arc::clone(&shared))).await;
    let client = reqwest::Client::new();
    let request =
        json!({"targets":[{"path":"/a","variant":""},{"path":"/b","variant":""}],"register":true});
    let answer = client
        .post(format!("{url}{WARM_PATH}"))
        .header(TOKEN_HEADER, "operator")
        .json(&request)
        .send()
        .await
        .expect("should reach owner");
    assert_eq!(answer.status(), StatusCode::OK);
    let answer: Value = answer
        .json()
        .await
        .expect("should read exact target outcomes");
    assert_eq!(answer["targets"][0]["admission"]["outcome"], "queued");
    assert_eq!(answer["targets"][1]["admission"]["outcome"], "queue_full");
    assert_eq!(
        shared.store.counts().expect("should count").inventory,
        2,
        "inventory survives failed queue admission"
    );
    let status: Value = client
        .get(format!("{url}{STATUS_PATH}"))
        .header(TOKEN_HEADER, "operator")
        .send()
        .await
        .expect("should read owner status")
        .json()
        .await
        .expect("should decode status");
    assert_eq!(status["capacity"]["jobs"], 1);
    assert_eq!(
        status["capacity"]["inventory_targets"], 1024,
        "no article-count shortcut narrows independent inventory"
    );
    assert_eq!(status["counts"]["pending"], 1);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn scopes_schema_and_body_limits_refuse_without_admitting_or_registering_work() {
    let dir = tempfile::tempdir().expect("should create store");
    let shared = shared(&dir, Bounds::default(), "http://127.0.0.1:9");
    let (url, server) = serve(router(Arc::clone(&shared))).await;
    let client = reqwest::Client::new();
    for secret in ["signal", "publication", "wrong"] {
        let answer = client
            .post(format!("{url}{WARM_PATH}"))
            .header(TOKEN_HEADER, secret)
            .json(&json!({"targets":[{"path":"/a"}],"register":true}))
            .send()
            .await
            .expect("should reach owner");
        assert_eq!(answer.status(), StatusCode::UNAUTHORIZED);
    }
    let malformed = client
        .post(format!("{url}{WARM_PATH}"))
        .header(TOKEN_HEADER, "operator")
        .json(&json!({"targets":[{"path":"/a"}],"extra":"not accepted"}))
        .send()
        .await
        .expect("should reach owner");
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    let refused: Value = client.post(format!("{url}{WARM_PATH}")).header(TOKEN_HEADER, "operator").json(&json!({"targets":[{"path":"//other.example.com/escape"},{"path":"/a","variant":"reader_cookie=private"}],"register":true})).send().await.expect("should reach owner").json().await.expect("should report every refused target");
    assert!(
        refused["targets"]
            .as_array()
            .expect("should list targets")
            .iter()
            .all(|target| target["outcome"] == "not_admitted")
    );
    let large = client
        .post(format!("{url}{SIGNALS_PATH}"))
        .header(TOKEN_HEADER, "signal")
        .body(vec![b'x'; MAX_SIGNAL_BYTES + 1])
        .send()
        .await
        .expect("should reach bounded route");
    assert_eq!(large.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(shared.store.counts().expect("should count").pending, 0);
    assert_eq!(shared.store.counts().expect("should count").inventory, 0);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn source_bytes_require_public_admission_and_last_reference_retirement_withdraws_them() {
    let dir = tempfile::tempdir().expect("should create storage");
    let shared = shared(&dir, Bounds::default(), "http://127.0.0.1:9");
    let bytes = b"admitted original source";
    let sha: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let first = "a".repeat(64);
    let second = "b".repeat(64);
    shared
        .store
        .keep_source(&sha, "image/png", bytes, &first)
        .expect("should retain first public admission");
    shared
        .store
        .keep_source(&sha, "image/png", bytes, &second)
        .expect("should retain independent second reference");
    let (url, server) = serve(router(Arc::clone(&shared))).await;
    let client = reqwest::Client::new();
    let source_url = format!("{url}{WORKER_SOURCE_PREFIX}{sha}");
    assert_eq!(
        client
            .get(&source_url)
            .header(TOKEN_HEADER, "signal")
            .send()
            .await
            .expect("should refuse signal scope")
            .status(),
        StatusCode::NOT_FOUND
    );
    let answer = client
        .get(&source_url)
        .header(TOKEN_HEADER, "publication")
        .send()
        .await
        .expect("should read admitted source");
    assert_eq!(answer.status(), StatusCode::OK);
    assert_eq!(answer.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(answer.headers()[header::CONTENT_DISPOSITION], "attachment");
    assert_eq!(
        answer.headers()[header::CONTENT_SECURITY_POLICY],
        "sandbox; default-src 'none'; base-uri 'none'; form-action 'none'"
    );
    assert_eq!(
        answer
            .bytes()
            .await
            .expect("should read exact bytes")
            .as_ref(),
        bytes
    );
    for (identity, expected) in [(&first, StatusCode::OK), (&second, StatusCode::NOT_FOUND)] {
        let answer = client
            .post(format!("{url}{RETIRE_PATH}"))
            .header(TOKEN_HEADER, "operator")
            .json(&RetireRequest {
                identity: identity.clone(),
            })
            .send()
            .await
            .expect("should retire through owner");
        assert_eq!(answer.status(), StatusCode::OK);
        assert_eq!(
            client
                .get(&source_url)
                .header(TOKEN_HEADER, "publication")
                .send()
                .await
                .expect("should check retained source authorization")
                .status(),
            expected
        );
    }
    assert!(
        !shared
            .store
            .source(&sha)
            .expect("should retain admission record")
            .expect("should retain tombstone")
            .public
    );
    assert!(
        !dir.path().join("sources").join(&sha).exists(),
        "last-reference retirement unlinks before releasing source bytes"
    );
    server.abort();
    let _ = server.await;
}

struct MarkerBoundary {
    fail: AtomicBool,
    attempts: AtomicUsize,
    markers: Mutex<Vec<MarkerEnvelope>>,
}

async fn marker_boundary(State(state): State<Arc<MarkerBoundary>>, request: Request) -> Response {
    if request.uri().path().ends_with("/capabilities") {
        return json(
            StatusCode::FORBIDDEN,
            json!({"error":{"code":"CREATION_DISABLED"}}),
        );
    }
    assert!(request.uri().path().starts_with(MARKERS_PREFIX));
    assert_eq!(
        request
            .headers()
            .get(TOKEN_HEADER)
            .expect("should use publication scope"),
        "publication"
    );
    let bytes = axum::body::to_bytes(request.into_body(), MAX_SIGNAL_BYTES)
        .await
        .expect("should read bounded marker");
    let marker: MarkerEnvelope =
        serde_json::from_slice(&bytes).expect("should use frozen marker protocol");
    state
        .markers
        .lock()
        .expect("should lock markers")
        .push(marker);
    state.attempts.fetch_add(1, Ordering::SeqCst);
    if state.fail.load(Ordering::SeqCst) {
        json(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error":{"code":"OFFLINE"}}),
        )
    } else {
        json(StatusCode::OK, json!({"stored":true}))
    }
}

async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("should reach bounded observable condition");
}

#[tokio::test]
async fn automatic_retirement_outbox_retries_after_restart_and_acknowledges_only_successful_put() {
    let boundary = Arc::new(MarkerBoundary {
        fail: AtomicBool::new(true),
        attempts: AtomicUsize::new(0),
        markers: Mutex::new(Vec::new()),
    });
    let (edge_url, edge_server) = serve(
        Router::new()
            .fallback(marker_boundary)
            .with_state(Arc::clone(&boundary)),
    )
    .await;
    let dir = tempfile::tempdir().expect("should create durable owner storage");
    let identity = "a".repeat(64);
    {
        let owner = shared(&dir, Bounds::default(), &edge_url);
        owner
            .store
            .retire(&identity, "owner-http-test")
            .expect("should commit tombstone and marker before sending");
        let task = tokio::spawn(housekeeping(
            Arc::clone(&owner),
            Edge::new(&edge_url, &owner.secrets, "owner-http-test"),
            Api::new(&edge_url, &owner.secrets),
        ));
        until(|| boundary.attempts.load(Ordering::SeqCst) == 1).await;
        assert_eq!(
            owner
                .store
                .counts()
                .expect("should report exact local status")
                .retirement_pending,
            1
        );
        task.abort();
        let _ = task.await;
    }
    boundary.fail.store(false, Ordering::SeqCst);
    let owner = shared(&dir, Bounds::default(), &edge_url);
    assert_eq!(
        owner
            .store
            .outbox()
            .expect("should persist failed send across restart")
            .len(),
        1
    );
    let task = tokio::spawn(housekeeping(
        Arc::clone(&owner),
        Edge::new(&edge_url, &owner.secrets, "owner-http-test"),
        Api::new(&edge_url, &owner.secrets),
    ));
    until(|| {
        owner
            .store
            .counts()
            .expect("should observe durable acknowledgement")
            .retirement_acknowledged
            == 1
    })
    .await;
    assert!(
        owner
            .store
            .outbox()
            .expect("should settle durable outbox")
            .is_empty()
    );
    {
        let markers = boundary
            .markers
            .lock()
            .expect("should inspect actual network payloads");
        assert_eq!(markers.len(), 2);
        assert_eq!(
            markers[0], markers[1],
            "restart retries the exact immutable retirement epoch"
        );
        assert_eq!(markers[1].identity, identity);
    }
    task.abort();
    let _ = task.await;
    edge_server.abort();
    let _ = edge_server.await;
}

#[test]
fn configuration_cadence_refuses_subminute_intervals_and_floors_partial_minutes() {
    let dir = tempfile::tempdir().expect("should create store");
    let mut config = worker_config(&dir, "http://127.0.0.1:9");
    config.scan_per_minute = 1;
    for interval in [0, 59, 901] {
        config.scan_interval_seconds = interval;
        assert!(
            config.check().is_err(),
            "interval {interval} must be refused even with empty inventory"
        );
    }
    for (interval, capacity) in [(60, 1), (61, 1), (900, 15)] {
        config.scan_interval_seconds = interval;
        config.inventory = (0..capacity)
            .map(|index| Target {
                path: format!("/article/{index:04}"),
                variant: String::new(),
            })
            .collect();
        config
            .check()
            .expect("should admit exactly the cadence capacity");
        assert_eq!(config.store_bounds().max_inventory, capacity);
        config.inventory.push(Target {
            path: "/overflow".to_owned(),
            variant: String::new(),
        });
        assert!(
            config.check().is_err(),
            "interval {interval} must refuse a distinct target above capacity {capacity}"
        );
    }
}

#[test]
fn configured_inventory_counts_distinct_path_variant_pairs_but_preserves_raw_entry_limit() {
    let dir = tempfile::tempdir().expect("should create store");
    let mut config = worker_config(&dir, "http://127.0.0.1:9");
    config.scan_per_minute = 1;
    config.scan_interval_seconds = 61;
    config.variants = VariantSchema(vec![VariantParam {
        name: "edition".to_owned(),
        values: vec!["uk".to_owned(), "us".to_owned()],
    }]);
    let target = Target {
        path: "/article".to_owned(),
        variant: "edition=uk".to_owned(),
    };
    config.inventory = vec![target.clone(); 1024];
    config
        .check()
        .expect("duplicate configured entries consume one scan slot");
    config.inventory.push(target.clone());
    assert!(
        config.check().is_err(),
        "the raw 1024-entry configuration limit still applies to duplicates"
    );
    config.inventory = vec![target.clone(), target];
    config.inventory.push(Target {
        path: "/article".to_owned(),
        variant: "edition=us".to_owned(),
    });
    assert!(
        config.check().is_err(),
        "two variants at the same path consume two scan slots"
    );
    config.scan_per_minute = 2;
    config
        .check()
        .expect("two scan slots should admit both distinct variants");
    let inventory = config.inventory.clone();
    let shared = shared_with_config(config.clone(), config.store_bounds());
    assert_eq!(
        shared
            .store
            .register(&inventory)
            .expect("runtime registration should share configured deduplication"),
        2
    );
    assert_eq!(shared.store.counts().expect("should count").inventory, 2);
    config.inventory[0].variant = "edition=private".to_owned();
    assert!(
        config.check().is_err(),
        "deduplication must not bypass configured target validation"
    );
}

#[test]
fn default_cadence_admits_1024_configured_and_registered_targets() {
    let dir = tempfile::tempdir().expect("should create store");
    let mut config = worker_config(&dir, "http://127.0.0.1:9");
    assert_eq!(config.scan_per_minute, 128);
    assert_eq!(config.scan_interval_seconds, 900);
    assert_eq!(config.store_bounds().max_inventory, 1024);
    assert_eq!(
        config.store_bounds().quota_bytes,
        config.artifact_quota_bytes
    );
    config.inventory = (0..1024)
        .map(|index| Target {
            path: format!("/article/{index:04}"),
            variant: String::new(),
        })
        .collect();
    let inventory = config.inventory.clone();
    let shared = shared_with_config(config.clone(), config.store_bounds());
    assert_eq!(
        shared
            .store
            .register(&inventory)
            .expect("default cadence should retain full inventory capacity"),
        1024
    );
    assert_eq!(shared.store.counts().expect("should count").inventory, 1024);
}

#[tokio::test]
async fn warm_registration_enforces_configured_capacity_with_duplicates_and_atomic_overflow() {
    for (interval, capacity) in [(61, 1), (900, 15)] {
        let dir = tempfile::tempdir().expect("should create store");
        let mut config = worker_config(&dir, "http://127.0.0.1:9");
        config.scan_per_minute = 1;
        config.scan_interval_seconds = interval;
        let shared = shared_with_config(config.clone(), config.store_bounds());
        let (url, server) = serve(router(Arc::clone(&shared))).await;
        let client = reqwest::Client::new();
        for index in 0..capacity - 1 {
            let answer = client
                .post(format!("{url}{WARM_PATH}"))
                .header(TOKEN_HEADER, "operator")
                .json(&json!({
                    "targets": [{"path": format!("/article/{index:04}")}],
                    "register": true
                }))
                .send()
                .await
                .expect("should reach owner");
            assert_eq!(answer.status(), StatusCode::OK);
        }
        let last = json!({"path": format!("/article/{:04}", capacity - 1)});
        let overflow = json!({"path": "/overflow"});
        let answer = client
            .post(format!("{url}{WARM_PATH}"))
            .header(TOKEN_HEADER, "operator")
            .json(&json!({"targets": [last.clone(), overflow.clone()], "register": true}))
            .send()
            .await
            .expect("should reach owner");
        assert_eq!(answer.status(), StatusCode::CONFLICT);
        let answer: Value = answer.json().await.expect("should decode refusal");
        assert_eq!(answer["error"]["code"], "inventory_full");
        assert_eq!(
            shared.store.counts().expect("should count").inventory,
            capacity - 1,
            "a rejected batch cannot partially register its first new target"
        );
        for targets in [vec![last.clone()], vec![last.clone(), last.clone()]] {
            let answer = client
                .post(format!("{url}{WARM_PATH}"))
                .header(TOKEN_HEADER, "operator")
                .json(&json!({"targets": targets, "register": true}))
                .send()
                .await
                .expect("should reach owner");
            assert_eq!(answer.status(), StatusCode::OK);
        }
        let answer = client
            .post(format!("{url}{WARM_PATH}"))
            .header(TOKEN_HEADER, "operator")
            .json(&json!({"targets": [last, overflow], "register": true}))
            .send()
            .await
            .expect("should reach owner");
        assert_eq!(answer.status(), StatusCode::CONFLICT);
        let answer: Value = answer.json().await.expect("should decode refusal");
        assert_eq!(answer["error"]["code"], "inventory_full");
        let counts = shared.store.counts().expect("should count");
        assert_eq!(counts.inventory, capacity);
        assert_eq!(counts.pending, capacity);
        let status = client
            .get(format!("{url}{STATUS_PATH}"))
            .header(TOKEN_HEADER, "operator")
            .send()
            .await
            .expect("should read owner status");
        assert_eq!(status.status(), StatusCode::OK);
        let status: Value = status.json().await.expect("should decode status");
        assert_eq!(status["capacity"]["inventory_targets"], capacity);
        assert_eq!(status["counts"]["inventory"], capacity);

        let targets: Vec<_> = (0..1024)
            .map(|index| json!({"path": format!("/warm-only/{index:04}")}))
            .collect();
        let answer = client
            .post(format!("{url}{WARM_PATH}"))
            .header(TOKEN_HEADER, "operator")
            .json(&json!({"targets": targets, "register": false}))
            .send()
            .await
            .expect("should reach owner");
        assert_eq!(
            answer.status(),
            StatusCode::OK,
            "the warm request limit remains 1024 independently of registration capacity"
        );
        let answer: Value = answer.json().await.expect("should decode warm outcomes");
        assert_eq!(
            answer["targets"]
                .as_array()
                .expect("should list outcomes")
                .len(),
            1024
        );
        assert_eq!(
            shared.store.counts().expect("should count").inventory,
            capacity
        );
        server.abort();
        let _ = server.await;
    }
}

#[tokio::test]
async fn concurrent_http_registrations_cannot_exceed_configured_inventory_capacity() {
    let dir = tempfile::tempdir().expect("should create store");
    let mut config = worker_config(&dir, "http://127.0.0.1:9");
    config.scan_per_minute = 1;
    config.scan_interval_seconds = 900;
    let shared = shared_with_config(config.clone(), config.store_bounds());
    let (url, server) = serve(router(Arc::clone(&shared))).await;
    let client = reqwest::Client::new();
    let barrier = Arc::new(tokio::sync::Barrier::new(25));
    let mut requests = Vec::new();
    for index in 0..24 {
        let client = client.clone();
        let url = url.clone();
        let barrier = Arc::clone(&barrier);
        requests.push(tokio::spawn(async move {
            barrier.wait().await;
            let answer = client
                .post(format!("{url}{WARM_PATH}"))
                .header(TOKEN_HEADER, "operator")
                .json(&json!({
                    "targets": [{"path": format!("/article/{index:04}")}],
                    "register": true
                }))
                .send()
                .await
                .expect("should reach owner");
            let status = answer.status();
            let body: Value = answer.json().await.expect("should decode registration");
            (status, body)
        }));
    }
    barrier.wait().await;
    let mut accepted = 0;
    let mut refused = 0;
    for request in requests {
        let (status, body) = request.await.expect("should join registration");
        match status {
            StatusCode::OK => {
                assert_eq!(body["targets"][0]["admission"]["outcome"], "queued");
                accepted += 1;
            }
            StatusCode::CONFLICT => {
                assert_eq!(body["error"]["code"], "inventory_full");
                refused += 1;
            }
            _ => panic!("unexpected registration response: {status} {body}"),
        }
    }
    assert_eq!(accepted, 15);
    assert_eq!(refused, 9);
    let counts = shared.store.counts().expect("should count");
    assert_eq!(counts.inventory, 15);
    assert_eq!(counts.pending, 15);
    let status: Value = client
        .get(format!("{url}{STATUS_PATH}"))
        .header(TOKEN_HEADER, "operator")
        .send()
        .await
        .expect("should read owner status")
        .json()
        .await
        .expect("should decode status");
    assert_eq!(status["capacity"]["inventory_targets"], 15);
    assert_eq!(status["counts"]["inventory"], 15);
    server.abort();
    let _ = server.await;
}
