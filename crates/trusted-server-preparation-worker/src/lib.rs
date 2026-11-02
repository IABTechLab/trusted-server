//! The publisher-operated native preparation worker for Encypher's
//! asynchronous prepared signing.
//!
//! One long-lived process owns one durable store. It admits reader hints,
//! publication events and operator warms into bounded durable jobs,
//! claims them with renewable fenced leases on a fixed number of
//! supervised tasks, prepares and signs content outside reader delivery,
//! and publishes results the delivery instance verifies through the API.
//! Reconciliation re-reads the registered inventory on a persisted cursor,
//! and retirement markers leave through a durable outbox until the
//! delivery instance acknowledges them.
//!
//! The process is ready only after it proves store ownership, the delivery
//! instance's acceptance of its publication credential and the API's
//! policy answer; until then `/v1/preparation/ready` answers `503`.

pub mod client;
pub mod prepare;
pub mod store;

#[cfg(test)]
#[path = "http_tests.rs"]
mod http_tests;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

use trusted_server_provenance_encypher::preparation::pipeline;
use trusted_server_provenance_encypher::preparation::protocol::{
    MAX_SIGNAL_BYTES, PREPARATION_PROTOCOL, READY_PATH, RETIRE_PATH, SIGNALS_PATH, STATUS_PATH,
    SignalRequest, TOKEN_HEADER, Target, WARM_PATH, WORKER_SOURCE_PREFIX, is_digest, token_matches,
};

use client::{Api, Edge, Secrets, WorkerConfig};
use prepare::{Context, now};
use store::{Admission, Bounds, JobOrigin, Store, StoreError};

/// Shared state of the HTTP routes and tasks.
struct Shared {
    store: Arc<Store>,
    config: Arc<WorkerConfig>,
    secrets: Secrets,
    ready: AtomicBool,
}

/// An operator warm: targets to prepare now and, optionally, to register
/// in the reconciliation inventory.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WarmRequest {
    /// The targets.
    pub targets: Vec<Target>,
    /// Register them for periodic reconciliation.
    #[serde(default)]
    pub register: bool,
    /// Clear damped outcomes of these targets: an explicit operator retry.
    #[serde(default)]
    pub retry: bool,
}

/// An operator retirement.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RetireRequest {
    /// The identity retired.
    pub identity: String,
}

pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn authorized(headers: &HeaderMap, secret: &str) -> bool {
    headers
        .get(TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|presented| token_matches(secret, presented))
}

fn json(status: StatusCode, value: serde_json::Value) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        axum::Json(value),
    )
        .into_response()
}

fn store_error(error: &StoreError) -> Response {
    log::error!("preparation: {error}");
    let (status, code) = match error {
        StoreError::InventoryFull => (StatusCode::CONFLICT, "inventory_full"),
        StoreError::Quota => (StatusCode::SERVICE_UNAVAILABLE, "quota_exhausted"),
        _ => (StatusCode::SERVICE_UNAVAILABLE, "store_unavailable"),
    };
    json(status, serde_json::json!({"error": {"code": code}}))
}

fn admitted(target: &Target, config: &WorkerConfig) -> bool {
    target.is_well_formed()
        && config.variants.admit(&target.variant).as_deref() == Some(target.variant.as_str())
}

async fn signals(State(shared): State<Arc<Shared>>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&headers, &shared.secrets.signal) {
        return json(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({"error": {"code": "unauthorized"}}),
        );
    }
    if body.len() > MAX_SIGNAL_BYTES {
        return json(
            StatusCode::PAYLOAD_TOO_LARGE,
            serde_json::json!({"error": {"code": "too_large"}}),
        );
    }
    let Ok(signal) = serde_json::from_slice::<SignalRequest>(&body) else {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": {"code": "malformed"}}),
        );
    };
    if signal.protocol != PREPARATION_PROTOCOL
        || signal.deployment_id != shared.config.deployment_id
        || !admitted(&signal.target, &shared.config)
        || !signal.items.iter().all(|item| is_digest(&item.identity))
    {
        return json(
            StatusCode::FORBIDDEN,
            serde_json::json!({"error": {"code": "not_admitted"}}),
        );
    }
    match shared
        .store
        .admit(&signal.target, JobOrigin::Signal, &signal.items, now())
    {
        Ok(admission @ (Admission::Queued { .. } | Admission::Coalesced { .. })) => {
            json(StatusCode::ACCEPTED, serde_json::json!(admission))
        }
        Ok(Admission::QueueFull) => json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!(Admission::QueueFull),
        ),
        Ok(admission) => json(StatusCode::OK, serde_json::json!(admission)),
        Err(error) => store_error(&error),
    }
}

async fn warm(State(shared): State<Arc<Shared>>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&headers, &shared.secrets.operator) {
        return json(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({"error": {"code": "unauthorized"}}),
        );
    }
    let Ok(request) = serde_json::from_slice::<WarmRequest>(&body) else {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": {"code": "malformed"}}),
        );
    };
    if request.targets.len() > Bounds::default().max_inventory {
        return json(
            StatusCode::PAYLOAD_TOO_LARGE,
            serde_json::json!({"error": {"code": "too_many_targets"}}),
        );
    }
    if request.register {
        let admitted_targets: Vec<_> = request
            .targets
            .iter()
            .filter(|target| admitted(target, &shared.config))
            .cloned()
            .collect();
        if let Err(error) = shared.store.register(&admitted_targets) {
            return store_error(&error);
        }
    }
    let mut outcomes = Vec::with_capacity(request.targets.len());
    for target in &request.targets {
        if !admitted(target, &shared.config) {
            outcomes.push(serde_json::json!({"target": target, "outcome": "not_admitted"}));
            continue;
        }
        if request.retry
            && let Err(error) = shared.store.clear_outcomes(target, now())
        {
            return store_error(&error);
        }
        match shared.store.admit(target, JobOrigin::Warm, &[], now()) {
            Ok(admission) => {
                outcomes.push(serde_json::json!({"target": target, "admission": admission}))
            }
            Err(error) => return store_error(&error),
        }
    }
    json(
        StatusCode::OK,
        serde_json::json!({"protocol": PREPARATION_PROTOCOL, "targets": outcomes}),
    )
}

async fn retire(State(shared): State<Arc<Shared>>, headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&headers, &shared.secrets.operator) {
        return json(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({"error": {"code": "unauthorized"}}),
        );
    }
    let Ok(request) = serde_json::from_slice::<RetireRequest>(&body) else {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": {"code": "malformed"}}),
        );
    };
    if !is_digest(&request.identity) {
        return json(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": {"code": "identity_invalid"}}),
        );
    }
    match shared
        .store
        .retire(&request.identity, &shared.config.deployment_id)
    {
        // Retired locally; marker publication is pending until the
        // delivery instance acknowledges it, see status.
        Ok(epoch) => {
            let acknowledged = match shared.store.retirement(&request.identity) {
                Ok(Some(retirement)) => retirement.acknowledged,
                Ok(None) => false,
                Err(error) => return store_error(&error),
            };
            json(
                StatusCode::OK,
                serde_json::json!({
                    "identity": request.identity, "epoch": epoch,
                    "marker": if acknowledged { "acknowledged" } else { "pending" },
                }),
            )
        }
        Err(error) => store_error(&error),
    }
}

async fn status(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    if !authorized(&headers, &shared.secrets.operator) {
        return json(
            StatusCode::UNAUTHORIZED,
            serde_json::json!({"error": {"code": "unauthorized"}}),
        );
    }
    let refusals = match shared.store.active_outcome_counts(now()) {
        Ok(refusals) => refusals,
        Err(error) => return store_error(&error),
    };
    match shared.store.counts() {
        Ok(counts) => json(
            StatusCode::OK,
            serde_json::json!({
                "protocol": PREPARATION_PROTOCOL,
                "ready": shared.ready.load(Ordering::Relaxed),
                "counts": counts,
                "active_refusals_by_code": refusals,
                "capacity": {
                    "jobs": shared.store.bounds().max_jobs,
                    "reader_jobs": shared.store.bounds().max_jobs - shared.store.bounds().reserved_jobs,
                    "queue_bytes": shared.store.bounds().max_queue_bytes,
                    "reader_queue_bytes": shared.store.bounds().max_queue_bytes - shared.store.bounds().reserved_bytes,
                    "inventory_targets": shared.store.bounds().max_inventory,
                    "artifact_bytes": shared.store.bounds().quota_bytes,
                },
            }),
        ),
        Err(error) => store_error(&error),
    }
}

async fn ready(State(shared): State<Arc<Shared>>) -> Response {
    if shared.ready.load(Ordering::Relaxed) {
        json(StatusCode::OK, serde_json::json!({"ready": true}))
    } else {
        json(
            StatusCode::SERVICE_UNAVAILABLE,
            serde_json::json!({"ready": false}),
        )
    }
}

async fn source(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    UrlPath(sha256): UrlPath<String>,
) -> Response {
    if !authorized(&headers, &shared.secrets.publication) || !is_digest(&sha256) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(Some(record)) = shared.store.source(&sha256) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // A digest held privately is never public authorization.
    if !record.public || record.references.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    match shared.store.blob("source", &sha256) {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, record.mime),
                (header::CACHE_CONTROL, "private, no-store".to_owned()),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
                (header::CONTENT_DISPOSITION, "attachment".to_owned()),
                (
                    header::CONTENT_SECURITY_POLICY,
                    "sandbox; default-src 'none'; base-uri 'none'; form-action 'none'".to_owned(),
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// The worker's routes over `store`.
fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route(
            SIGNALS_PATH,
            post(signals).layer(axum::extract::DefaultBodyLimit::max(MAX_SIGNAL_BYTES)),
        )
        .route(WARM_PATH, post(warm))
        .route(RETIRE_PATH, post(retire))
        .route(STATUS_PATH, get(status))
        .route(READY_PATH, get(ready))
        .route(&format!("{WORKER_SOURCE_PREFIX}{{sha256}}"), get(source))
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_SIGNAL_BYTES * 1024,
        ))
        .with_state(shared)
}

/// Runs one preparation task on its own thread until `stop` is set: claim,
/// run with lease renewal, settle. No work outlives its claim.
fn task(context: Arc<Context>, stop: Arc<AtomicBool>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("should build a task runtime");
    runtime.block_on(async move {
        let lease = Duration::from_secs(context.store.bounds().lease_seconds / 3);
        while !stop.load(Ordering::Relaxed) {
            let token = rand::random::<u64>();
            let claim = match context.store.claim(now(), token) {
                Ok(Some(claim)) => claim,
                Ok(None) => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
                Err(error) => {
                    log::error!("preparation: claim failed: {error}");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            let key = claim.job.key.clone();
            let work = prepare::run(&context, &claim);
            tokio::pin!(work);
            let mut renew = tokio::time::interval(lease);
            renew.tick().await;
            let result = loop {
                tokio::select! {
                    result = &mut work => break result,
                    _ = renew.tick() => {
                        match context.store.renew(&key, token, now()) {
                            Ok(true) => {}
                            // Another owner holds the claim now: this
                            // attempt is abandoned, never settled.
                            _ => break Err(pipeline::Failure::new(
                                pipeline::FailureClass::Transient,
                                "lease_lost",
                            )),
                        }
                    }
                }
            };
            let settled = match result {
                Ok(()) => context.store.complete(&key, token, now()),
                Err(failure) => {
                    log::info!("preparation: attempt failed code={}", failure.code);
                    context.store.retry(&key, token, now(), &failure.code)
                }
            };
            if let Err(error) = settled {
                log::error!("preparation: settle failed: {error}");
            }
        }
    });
}

/// Publishes queued retirement markers until each is acknowledged, and
/// admits due inventory targets for reconciliation, once a minute.
async fn housekeeping(shared: Arc<Shared>, edge: Edge, api: Api) {
    let context = Context::new(
        Arc::clone(&shared.store),
        api,
        edge.clone(),
        Arc::clone(&shared.config),
        &shared.secrets.worker_token,
    );
    let page = format!("https://{}/", shared.config.public_host);
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if let Err(error) = shared.store.prune_expired_outcomes(now()) {
            log::error!("preparation: expired outcome pruning failed: {error}");
        }
        if let Err(error) = shared.store.recover_expired(now()) {
            log::error!("preparation: expired claim recovery failed: {error}");
        }
        match shared.store.due_inventory(
            now(),
            shared.config.scan_interval_seconds,
            shared.config.scan_per_minute,
        ) {
            Ok(due) => {
                for target in due {
                    if let Err(error) =
                        shared
                            .store
                            .admit(&target, JobOrigin::Reconcile, &[], now())
                    {
                        log::error!("preparation: reconciliation admission failed: {error}");
                    }
                }
            }
            Err(error) => log::error!("preparation: inventory scan failed: {error}"),
        }
        // A signed release change can make a previously unsupported item eligible.
        // This is metadata selection only: no probe execution or API creation.
        match tokio::time::timeout(
            Duration::from_secs(2),
            prepare::refresh_probe_context(&context, &page),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(failure)) => log::debug!(
                "preparation: probe observation deferred code={}",
                failure.code
            ),
            Err(_) => log::debug!("preparation: probe observation deferred code=deadline"),
        }
        if let Ok(markers) = shared.store.outbox_batch(16) {
            for marker in markers {
                match tokio::time::timeout(Duration::from_secs(2), edge.retire(&marker)).await {
                    Ok(Ok(())) => {
                        if let Err(error) = shared
                            .store
                            .acknowledge_marker(&marker.identity, marker.epoch)
                        {
                            log::error!("preparation: marker acknowledgement failed: {error}");
                        }
                    }
                    Ok(Err(failure)) => log::info!(
                        "preparation: marker publication pending code={}",
                        failure.code
                    ),
                    Err(_) => log::info!("preparation: marker publication pending code=deadline"),
                }
            }
        }
    }
}

/// Why the worker stopped.
#[derive(Debug, Clone, PartialEq, Eq, derive_more::Display)]
#[display("preparation worker: {_0}")]
pub struct WorkerError(pub String);

impl core::error::Error for WorkerError {}

/// The asynchronous tasks one worker process supervises beside its
/// preparation threads, which report an unrequested stop on `failures`.
struct Supervised {
    readiness: tokio::task::JoinHandle<()>,
    keeper: tokio::task::JoinHandle<()>,
    failures: tokio::sync::mpsc::Receiver<()>,
}

/// Why `name`, which never ends on its own, ended before shutdown.
fn premature(name: &str, joined: &Result<(), tokio::task::JoinError>) -> String {
    match joined {
        Ok(()) => format!("{name} stopped"),
        Err(error) => format!("{name} failed: {error}"),
    }
}

/// Serves until `signal` resolves or anything supervised stops, then stops
/// everything it owns: readiness is withdrawn, `stop` is set for the
/// preparation threads, the server drains through `shutdown`, and the
/// readiness and housekeeping tasks are cancelled and joined. Each handle
/// is polled to completion exactly once. The caller joins the threads.
///
/// # Errors
///
/// A [`WorkerError`] where the signal handler, the listener or any
/// supervised task stopped without being asked to.
async fn supervise<F, S>(
    serving: F,
    shutdown: tokio::sync::oneshot::Sender<()>,
    signal: S,
    mut supervised: Supervised,
    ready: &AtomicBool,
    stop: &AtomicBool,
) -> Result<(), WorkerError>
where
    F: Future<Output = std::io::Result<()>>,
    S: Future<Output = std::io::Result<()>>,
{
    tokio::pin!(serving);
    let mut served = false;
    let mut readiness_joined = false;
    let mut keeper_joined = false;
    let mut failure = tokio::select! {
        result = &mut serving => {
            served = true;
            Some(match result {
                Ok(()) => "the listener stopped".to_owned(),
                Err(error) => format!("the listener failed: {error}"),
            })
        }
        signalled = signal => signalled
            .err()
            .map(|error| format!("the shutdown signal failed: {error}")),
        _ = supervised.failures.recv() => Some("a preparation task stopped".to_owned()),
        joined = &mut supervised.readiness => {
            readiness_joined = true;
            Some(premature("readiness", &joined))
        }
        joined = &mut supervised.keeper => {
            keeper_joined = true;
            Some(premature("housekeeping", &joined))
        }
    };
    ready.store(false, Ordering::Relaxed);
    stop.store(true, Ordering::Relaxed);
    // Resolves the shutdown future axum awaits on a task of its own; the
    // send fails only where the server has already finished.
    let _ = shutdown.send(());
    let drained = if served { Ok(()) } else { serving.await };
    if let Err(error) = drained {
        failure.get_or_insert_with(|| format!("the listener failed: {error}"));
    }
    for (name, handle, joined) in [
        ("readiness", supervised.readiness, readiness_joined),
        ("housekeeping", supervised.keeper, keeper_joined),
    ] {
        if joined {
            continue;
        }
        handle.abort();
        if let Err(error) = handle.await
            && error.is_panic()
        {
            failure.get_or_insert_with(|| format!("{name} failed: {error}"));
        }
    }
    match failure {
        Some(message) => Err(WorkerError(message)),
        None => Ok(()),
    }
}

/// Runs the worker described by the configuration at `config_path` until
/// interrupted.
///
/// # Errors
///
/// A [`WorkerError`] where the configuration, store or listener cannot be
/// set up, including a store another process owns, or where a supervised
/// task, thread or the listener stops without being asked to.
pub fn run(config_path: &Path) -> Result<(), WorkerError> {
    let (config, secrets) =
        WorkerConfig::load(config_path).map_err(|error| WorkerError(error.to_string()))?;
    std::fs::create_dir_all(config.store_dir.join("probes"))
        .map_err(|error| WorkerError(error.to_string()))?;
    let store = Arc::new(
        Store::open(
            &config.store_dir.join("preparation.redb"),
            config.store_bounds(),
        )
        .map_err(|error| WorkerError(error.to_string()))?,
    );
    store
        .recover_storage()
        .map_err(|error| WorkerError(error.to_string()))?;
    store
        .refresh_context("config", &config.damping_context(&secrets.worker_token))
        .map_err(|error| WorkerError(error.to_string()))?;
    log::info!("preparation: store owned and filesystem accounting reconciled");
    store
        .register(&config.inventory)
        .map_err(|error| WorkerError(error.to_string()))?;
    let config = Arc::new(config);
    let api = Api::new(&config.api_base_url, &secrets);
    let edge = Edge::new(&config.edge_url, &secrets, &config.deployment_id);
    let shared = Arc::new(Shared {
        store: Arc::clone(&store),
        config: Arc::clone(&config),
        secrets: secrets.clone(),
        ready: AtomicBool::new(false),
    });
    let address: SocketAddr = config
        .listen
        .parse()
        .map_err(|_| WorkerError(format!("listen address {} is invalid", config.listen)))?;
    let stop = Arc::new(AtomicBool::new(false));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| WorkerError(error.to_string()))?;
    // Every preparation thread started inside the runtime is stopped and
    // joined below, on every path out of it, startup errors included.
    let mut threads = Vec::with_capacity(config.workers);
    let supervised = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(|error| WorkerError(error.to_string()))?;
        log::info!("preparation: listening on {address}");
        // Each thread reports at most once, so a send never finds it full.
        let (failed, failures) = tokio::sync::mpsc::channel(config.workers);
        for index in 0..config.workers {
            let context = Arc::new(Context::new(
                Arc::clone(&store),
                api.clone(),
                edge.clone(),
                Arc::clone(&config),
                &secrets.worker_token,
            ));
            let stop = Arc::clone(&stop);
            let shared = Arc::clone(&shared);
            let failed = failed.clone();
            let thread = std::thread::Builder::new()
                .name(format!("preparation-{index}"))
                .spawn(move || {
                    while !shared.ready.load(Ordering::Relaxed) && !stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(500));
                    }
                    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        task(context, Arc::clone(&stop));
                    }));
                    // A task ends cleanly only when it was told to stop.
                    let clean = ran.is_ok() && stop.load(Ordering::Relaxed);
                    if !clean {
                        shared.ready.store(false, Ordering::Relaxed);
                        stop.store(true, Ordering::Relaxed);
                        let _ = failed.try_send(());
                    }
                    clean
                })
                .map_err(|error| {
                    WorkerError(format!("a preparation task cannot start: {error}"))
                })?;
            threads.push(thread);
        }
        // Only the threads hold senders now, so a closed channel means
        // every thread has ended.
        drop(failed);
        // Ready only once the delivery instance accepts the publication
        // credential and the API answers this credential's policy.
        let readiness = {
            let shared = Arc::clone(&shared);
            let edge = edge.clone();
            let api = api.clone();
            let page = format!("https://{}/", config.public_host);
            let context = Context::new(
                Arc::clone(&store),
                api.clone(),
                edge.clone(),
                Arc::clone(&config),
                &secrets.worker_token,
            );
            tokio::spawn(async move {
                loop {
                    let recovered = shared.store.recover_all(now());
                    if let Ok(count) = &recovered
                        && *count > 0
                    {
                        log::info!("preparation: {count} unfinished jobs recovered");
                    }
                    let edge_ok = edge.check_authorization().await;
                    let api_ok = pipeline::policy_digest(&api, &page).await;
                    let probe_ok = prepare::probe_ready(&context, &page).await;
                    match (&edge_ok, &api_ok, &probe_ok, &recovered) {
                        (Ok(()), Ok(_), Ok(()), Ok(_)) => {
                            shared.ready.store(true, Ordering::Relaxed);
                            log::info!("preparation: ready");
                            std::future::pending::<()>().await;
                            return;
                        }
                        _ => log::warn!(
                            "preparation: not ready edge={:?} api={:?} probe={:?} recovery={:?}",
                            edge_ok.err().map(|failure| failure.code),
                            api_ok.err().map(|failure| failure.code),
                            probe_ok.err().map(|failure| failure.code),
                            recovered.err()
                        ),
                    }
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            })
        };
        let keeper = tokio::spawn(housekeeping(Arc::clone(&shared), edge.clone(), api.clone()));
        let (shutdown, shutdown_requested) = tokio::sync::oneshot::channel();
        let serving = axum::serve(listener, router(Arc::clone(&shared)))
            .with_graceful_shutdown(async move {
                // A dropped sender shuts the server down as well.
                let _ = shutdown_requested.await;
            })
            .into_future();
        supervise(
            serving,
            shutdown,
            tokio::signal::ctrl_c(),
            Supervised {
                readiness,
                keeper,
                failures,
            },
            &shared.ready,
            &stop,
        )
        .await
    });
    shared.ready.store(false, Ordering::Relaxed);
    stop.store(true, Ordering::Relaxed);
    let mut threads_failed = false;
    for thread in threads {
        match thread.join() {
            Ok(true) => {}
            Ok(false) => threads_failed = true,
            Err(_) => {
                log::error!("preparation: a task panicked");
                threads_failed = true;
            }
        }
    }
    log::info!("preparation: stopped");
    match supervised {
        Ok(()) if threads_failed => Err(WorkerError(
            "a supervised preparation task stopped".to_owned(),
        )),
        result => result,
    }
}

#[cfg(test)]
mod supervision_tests {
    use super::*;

    use std::sync::atomic::Ordering::SeqCst;

    use tokio::sync::{mpsc, oneshot};

    /// Records that the future holding it was dropped.
    struct Dropped(Arc<AtomicBool>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, SeqCst);
        }
    }

    /// A task that never ends on its own and records its cancellation.
    fn forever(cancelled: &Arc<AtomicBool>) -> tokio::task::JoinHandle<()> {
        let guard = Dropped(Arc::clone(cancelled));
        tokio::spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        })
    }

    /// A server that serves until asked to shut down, then drains.
    fn serving(
        requested: oneshot::Receiver<()>,
        drained: &Arc<AtomicBool>,
    ) -> impl Future<Output = std::io::Result<()>> {
        let drained = Arc::clone(drained);
        async move {
            let _ = requested.await;
            drained.store(true, SeqCst);
            Ok(())
        }
    }

    struct Observed {
        result: Result<(), WorkerError>,
        ready: bool,
        stop: bool,
    }

    async fn observe<F, S>(
        serving: F,
        shutdown: oneshot::Sender<()>,
        signal: S,
        supervised: Supervised,
    ) -> Observed
    where
        F: Future<Output = std::io::Result<()>>,
        S: Future<Output = std::io::Result<()>>,
    {
        let ready = AtomicBool::new(true);
        let stop = AtomicBool::new(false);
        let result = supervise(serving, shutdown, signal, supervised, &ready, &stop).await;
        Observed {
            result,
            ready: ready.load(SeqCst),
            stop: stop.load(SeqCst),
        }
    }

    #[tokio::test]
    async fn signal_drains_the_server_and_cancels_every_task_cleanly() {
        let (drained, readiness, keeper) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (_failed, failures) = mpsc::channel(1);
        let (shutdown, requested) = oneshot::channel();
        let supervised = Supervised {
            readiness: forever(&readiness),
            keeper: forever(&keeper),
            failures,
        };

        let observed = observe(
            serving(requested, &drained),
            shutdown,
            async { Ok(()) },
            supervised,
        )
        .await;

        assert_eq!(observed.result, Ok(()), "should stop cleanly on a signal");
        assert!(
            drained.load(SeqCst),
            "should drain the server before returning"
        );
        assert!(readiness.load(SeqCst), "should cancel and join readiness");
        assert!(keeper.load(SeqCst), "should cancel and join housekeeping");
        assert!(observed.stop, "should tell preparation threads to stop");
        assert!(!observed.ready, "should withdraw readiness");
    }

    #[tokio::test]
    async fn premature_housekeeping_exit_fails_and_stops_everything_else() {
        let (drained, readiness) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (_failed, failures) = mpsc::channel(1);
        let (shutdown, requested) = oneshot::channel();
        // Already finished: joining it a second time would panic.
        let supervised = Supervised {
            readiness: forever(&readiness),
            keeper: tokio::spawn(async {}),
            failures,
        };

        let observed = observe(
            serving(requested, &drained),
            shutdown,
            std::future::pending(),
            supervised,
        )
        .await;

        assert_eq!(
            observed.result,
            Err(WorkerError("housekeeping stopped".to_owned())),
            "should report the housekeeping exit"
        );
        assert!(drained.load(SeqCst), "should drain the server");
        assert!(readiness.load(SeqCst), "should cancel and join readiness");
        assert!(
            observed.stop && !observed.ready,
            "should stop threads and withdraw readiness"
        );
    }

    #[tokio::test]
    async fn panicking_readiness_fails_and_cancels_housekeeping() {
        let (drained, keeper) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (_failed, failures) = mpsc::channel(1);
        let (shutdown, requested) = oneshot::channel();
        let supervised = Supervised {
            readiness: tokio::spawn(async { panic!("should surface as a readiness failure") }),
            keeper: forever(&keeper),
            failures,
        };

        let observed = observe(
            serving(requested, &drained),
            shutdown,
            std::future::pending(),
            supervised,
        )
        .await;

        let error = observed
            .result
            .expect_err("should fail when readiness panics");
        assert!(
            error.0.starts_with("readiness failed"),
            "should name readiness, got {error}"
        );
        assert!(drained.load(SeqCst), "should drain the server");
        assert!(keeper.load(SeqCst), "should cancel and join housekeeping");
    }

    #[tokio::test]
    async fn a_thread_failure_report_stops_the_worker() {
        let (drained, readiness, keeper) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (failed, failures) = mpsc::channel(1);
        failed.try_send(()).expect("should report a thread failure");
        let (shutdown, requested) = oneshot::channel();
        let supervised = Supervised {
            readiness: forever(&readiness),
            keeper: forever(&keeper),
            failures,
        };

        let observed = observe(
            serving(requested, &drained),
            shutdown,
            std::future::pending(),
            supervised,
        )
        .await;

        assert_eq!(
            observed.result,
            Err(WorkerError("a preparation task stopped".to_owned())),
            "should report the thread failure"
        );
        assert!(drained.load(SeqCst), "should drain the server");
        assert!(
            readiness.load(SeqCst) && keeper.load(SeqCst),
            "should cancel and join both tasks"
        );
    }

    #[tokio::test]
    async fn a_failing_listener_cancels_every_task_without_polling_it_again() {
        let (readiness, keeper) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (_failed, failures) = mpsc::channel(1);
        let (shutdown, _requested) = oneshot::channel();
        let supervised = Supervised {
            readiness: forever(&readiness),
            keeper: forever(&keeper),
            failures,
        };
        let mut polls = 0_u32;
        let failing = std::future::poll_fn(move |_| {
            polls += 1;
            assert_eq!(polls, 1, "should never poll a finished listener again");
            std::task::Poll::Ready(Err(std::io::Error::other("accept failed")))
        });

        let observed = observe(failing, shutdown, std::future::pending(), supervised).await;

        assert_eq!(
            observed.result,
            Err(WorkerError("the listener failed: accept failed".to_owned())),
            "should report the serve error"
        );
        assert!(
            readiness.load(SeqCst) && keeper.load(SeqCst),
            "should cancel and join both tasks"
        );
        assert!(
            observed.stop && !observed.ready,
            "should stop threads and withdraw readiness"
        );
    }

    #[tokio::test]
    async fn a_failed_signal_handler_stops_the_worker_as_a_failure() {
        let (drained, readiness, keeper) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicBool::new(false)),
        );
        let (_failed, failures) = mpsc::channel(1);
        let (shutdown, requested) = oneshot::channel();
        let supervised = Supervised {
            readiness: forever(&readiness),
            keeper: forever(&keeper),
            failures,
        };

        let observed = observe(
            serving(requested, &drained),
            shutdown,
            async { Err(std::io::Error::other("no handler")) },
            supervised,
        )
        .await;

        assert_eq!(
            observed.result,
            Err(WorkerError(
                "the shutdown signal failed: no handler".to_owned()
            )),
            "should report the signal handler failure"
        );
        assert!(drained.load(SeqCst), "should drain the server");
        assert!(
            readiness.load(SeqCst) && keeper.load(SeqCst),
            "should cancel and join both tasks"
        );
    }
}
