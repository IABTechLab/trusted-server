//! Native lifecycle regressions with isolated conditional storage and origins.

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use edgezero_adapter_axum::key_value_store::PersistentKvStore;
use edgezero_core::body::Body;
use edgezero_core::http::{Method, Request, Response, header};
use edgezero_core::key_value_store::KvHandle;
use edgezero_core::record_store::RecordLayout;
use edgezero_core::router::RouterService;
use error_stack::Report;
use serde_json::json;
use tokio::sync::Notify;
use trusted_server_adapter_axum::app::TrustedServerApp;
use trusted_server_core::ec::kv_backend::{
    EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome,
};
use trusted_server_core::ec::record_backend::{EcRecordLayout, RecordEcKvStore};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformConfigStore, PlatformError, PlatformGeo, PlatformSecretStore,
    RuntimeServices, StoreId, StoreName, UnavailableKvStore,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::test_support::nextjs_auction::{self, NextJsAuctionOrigin};

#[cfg(feature = "celld-ec")]
#[path = "support/celld.rs"]
mod celld_fixture;

#[cfg(feature = "celld-ec")]
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires official CELLD_EC_TEST_BINARY and matching EDGEZERO_CELLD_STORE_SERVICE"]
async fn real_celld_persists_routes_withdrawal_restart_and_outage_without_fallback() {
    let mut celld = celld_fixture::CelldFixture::new();
    let store = celld.store().await;
    let app = allowed_router(store.clone());
    let issued = call(&app, request(Method::GET, "/article", None)).await;
    let ec =
        ec_cookie(&issued).expect("should issue cookie only after real record acknowledgement");
    let original = store
        .lookup(&ec)
        .await
        .expect("should read real record")
        .expect("should retain issued identity");
    assert!(
        store
            .key_exists(&ec)
            .await
            .expect("should strongly confirm exact identity")
    );
    let eids = BASE64.encode(
        json!([{"source":"partner.example.com","uids":[{"id":"celld-fixture-uid","atype":1}]}])
            .to_string(),
    );
    let cookie = format!("ts-ec={ec}; ts-eids={eids}");
    let returned = call(&app, request(Method::GET, "/article", Some(&cookie))).await;
    assert_eq!(returned.status(), 200);
    let updated = store
        .lookup(&ec)
        .await
        .expect("should read updated record")
        .expect("should retain updated identity");
    assert_ne!(updated.generation, original.generation);
    let data: serde_json::Value =
        serde_json::from_slice(&updated.body).expect("should decode unchanged graph schema");
    assert_eq!(
        data["ids"]["partner.example.com"]["uid"],
        "celld-fixture-uid"
    );
    celld.restart();
    let store = celld.store().await;
    let restored = store
        .lookup(&ec)
        .await
        .expect("should read after actual celld restart")
        .expect("should restore identity");
    assert_eq!(restored.body, updated.body);
    assert_eq!(restored.metadata, updated.metadata);
    assert_eq!(restored.generation, updated.generation);
    let regulated = router(
        store.clone(),
        Some(GeoInfo::country_region("US", Some("TN".to_owned()))),
    );
    let mut withdrawal = request(Method::GET, "/article", Some(&cookie));
    withdrawal.headers_mut().insert(
        "sec-gpc",
        "1".parse().expect("should set synthetic withdrawal signal"),
    );
    let withdrawn = call(&regulated, withdrawal).await;
    assert_eq!(ec_cookie(&withdrawn).as_deref(), Some(""));
    let tombstone = store
        .lookup(&ec)
        .await
        .expect("should read actual tombstone")
        .expect("should retain actual tombstone");
    let data: serde_json::Value =
        serde_json::from_slice(&tombstone.body).expect("should decode tombstone");
    assert_eq!(data["consent"]["ok"], false);
    assert_eq!(
        store
            .insert(
                &ec,
                EcKvWrite {
                    body: std::str::from_utf8(&updated.body).expect("should decode old entry"),
                    metadata: "{}",
                    ttl: Duration::from_secs(60),
                    mode: EcKvWriteMode::IfGenerationMatch(updated.generation)
                }
            )
            .await
            .expect("should classify stale CAS"),
        EcKvWriteOutcome::PreconditionFailed
    );
    celld.restart();
    let store = celld.store().await;
    assert_eq!(
        store
            .lookup(&ec)
            .await
            .expect("should read recovered withdrawal")
            .expect("should retain withdrawal")
            .body,
        tombstone.body
    );
    let app = allowed_router(store.clone());
    assert!(
        ec_cookie(&call(&app, request(Method::GET, "/article", Some(&cookie))).await).is_none(),
        "should not resurrect withdrawal"
    );
    trusted_server_core::test_support::record_ec::assert_record_ec_lifecycle(&store).await;
    celld.stop();
    let failed = call(&app, request(Method::GET, "/article", None)).await;
    assert!(
        ec_cookie(&failed).is_none(),
        "should not mint unsupported cookie on real storage outage"
    );
    assert!(
        store.key_exists(&ec).await.is_err(),
        "should not turn outage into false absence"
    );
    celld
        .record_handle()
        .await
        .expect_err("should fail selected-provider preparation without fallback");
}

// Fault controls remain at the test request boundary. All stored rows and
// conditional writes now use the shared adapter over real persistent records.
#[derive(Clone)]

struct Store {
    inner: RecordEcKvStore,
    handle: KvHandle,
    fail_writes: Arc<AtomicBool>,
    pause_writes: Arc<AtomicBool>,
    started: Arc<Notify>,
    resume: Arc<Notify>,
    _directory: Arc<tempfile::TempDir>,
}

impl Default for Store {
    fn default() -> Self {
        let directory = Arc::new(tempfile::tempdir().expect("should create record fixture"));
        let layout = RecordLayout::new(EcRecordLayout).expect("should declare EC layout");
        let provider = PersistentKvStore::new_records(directory.path().join("ec.redb"), layout)
            .expect("should open persistent EC records");
        let handle = KvHandle::new(Arc::new(provider));
        let inner = RecordEcKvStore::new("native-ec-fixture".to_owned(), &handle)
            .expect("should acquire shared EC record adapter");
        Self {
            inner,
            handle,
            fail_writes: Arc::default(),
            pause_writes: Arc::default(),
            started: Arc::default(),
            resume: Arc::default(),
            _directory: directory,
        }
    }
}

impl Store {
    async fn is_empty(&self) -> bool {
        self.handle
            .list_keys_page("", None, 1)
            .await
            .expect("should inspect persistent fixture rows")
            .keys
            .is_empty()
    }
}

#[async_trait(?Send)]
impl EcKvStore for Store {
    fn store_name(&self) -> &str {
        self.inner.store_name()
    }
    async fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
        self.inner.lookup(key).await
    }
    async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
        self.inner.key_exists(key).await
    }
    async fn insert(
        &self,
        key: &str,
        write: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
        if self.pause_writes.load(Ordering::SeqCst) {
            self.started.notify_one();
            self.resume.notified().await;
        }
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err(Report::new(TrustedServerError::KvStore {
                store_name: self.store_name().to_owned(),
                message: "isolated uncertain write".to_owned(),
            }));
        }
        self.inner.insert(key, write).await
    }
    async fn list_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<String>, Report<TrustedServerError>> {
        self.inner.list_keys_with_prefix(prefix, limit).await
    }
    async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
        self.inner.delete(key).await
    }
}

struct UnusedStores;
#[async_trait(?Send)]
impl PlatformConfigStore for UnusedStores {
    fn get(&self, _: &StoreName, _: &str) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore))
    }
    async fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore))
    }
    async fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::ConfigStore))
    }
}
#[async_trait(?Send)]
impl PlatformSecretStore for UnusedStores {
    fn get_bytes(&self, _: &StoreName, _: &str) -> Result<Vec<u8>, Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore))
    }
    async fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore))
    }
    async fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore))
    }
}
struct Geo(Option<GeoInfo>);
impl PlatformGeo for Geo {
    fn lookup(&self, _: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(self.0.clone())
    }
}

fn settings() -> Settings {
    let mut settings = nextjs_auction::settings();
    settings.auction.enabled = false;
    settings.auction.providers.clear();
    settings.creative_opportunities = None;
    settings.ec.partners = serde_json::from_value(json!([{
        "name": "Fixture partner", "source_domain": "partner.example.com",
        "bidstream_enabled": true, "api_token": "native-fixture-partner-token-32-bytes-minimum"
    }]))
    .expect("should configure isolated partner");
    settings
}

fn router<S: EcKvStore + Clone + Send + Sync + 'static>(
    store: S,
    geo: Option<GeoInfo>,
) -> RouterService {
    let origin = NextJsAuctionOrigin::default();
    let scripted = nextjs_auction::services(&origin);
    let services = RuntimeServices::builder()
        .config_store(Arc::new(UnusedStores))
        .secret_store(Arc::new(UnusedStores))
        .kv_store(Arc::new(UnavailableKvStore))
        .outbound_client(scripted.outbound_client().clone())
        .clock(scripted.clock().clone())
        .geo(Arc::new(Geo(geo)))
        .client_info(ClientInfo {
            client_ip: Some("198.51.100.42".parse().expect("should parse test IP")),
            ..ClientInfo::default()
        })
        .build();
    TrustedServerApp::routes_with_settings_services_and_ec_store(settings(), services, store)
        .expect("should construct native EC fixture router")
}
fn allowed_router<S: EcKvStore + Clone + Send + Sync + 'static>(store: S) -> RouterService {
    router(store, Some(GeoInfo::country_region("AU", None)))
}
fn request(method: Method, path: &str, cookie: Option<&str>) -> Request {
    let mut req = axum::http::Request::builder()
        .method(method)
        .uri(format!("https://test-publisher.example.com{path}"))
        .header(
            header::USER_AGENT,
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/146.0.0.0 Safari/537.36",
        )
        .header("sec-fetch-dest", "document")
        .header(header::ACCEPT, "text/html");
    if let Some(cookie) = cookie {
        req = req.header(header::COOKIE, cookie);
    }
    req.body(Body::empty())
        .expect("should construct fixture request")
}
fn ec_cookie(response: &Response) -> Option<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            value.strip_prefix("ts-ec=").map(|value| {
                value
                    .split(';')
                    .next()
                    .expect("should contain cookie value")
                    .to_owned()
            })
        })
}
async fn call(router: &RouterService, request: Request) -> Response {
    router
        .oneshot(request)
        .await
        .expect("should dispatch native fixture request")
}

#[tokio::test(flavor = "current_thread")]
async fn persists_before_cookie_reuses_across_app_recreation_and_withdraws() {
    let store = Store::default();
    let app = allowed_router(store.clone());
    let response = call(&app, request(Method::GET, "/article", None)).await;
    assert_eq!(response.status(), 200);
    let ec = ec_cookie(&response).expect("should issue EC after confirmed persistence");
    assert!(
        store
            .key_exists(&ec)
            .await
            .expect("should confirm authoritative row")
    );
    assert_eq!(
        response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter(|value| value
                .to_str()
                .is_ok_and(|value| value.starts_with("ts-ec=")))
            .count(),
        1,
        "should finalize exactly once"
    );
    assert!(
        response.headers()[header::CACHE_CONTROL]
            .to_str()
            .expect("should read cache policy")
            .contains("private")
    );
    let eids = BASE64.encode(
        json!([{"source":"partner.example.com","uids":[{"id":"fixture-uid","atype":1}]}])
            .to_string(),
    );
    let cookie = format!("ts-ec={ec}; ts-eids={eids}");
    let recreated = allowed_router(store.clone());
    let returning = call(&recreated, request(Method::GET, "/article", Some(&cookie))).await;
    assert_eq!(returning.status(), 200);
    assert!(
        ec_cookie(&returning).is_none(),
        "should not refresh an unchanged returning cookie"
    );
    let row: serde_json::Value = serde_json::from_slice(
        &store
            .lookup(&ec)
            .await
            .expect("should read row")
            .expect("should retain row")
            .body,
    )
    .expect("should decode persisted entry");
    assert_eq!(row["ids"]["partner.example.com"]["uid"], "fixture-uid");
    let mut identify = request(Method::GET, "/_ts/api/v1/identify", Some(&cookie));
    identify.headers_mut().insert(
        header::AUTHORIZATION,
        "Bearer native-fixture-partner-token-32-bytes-minimum"
            .parse()
            .expect("should set fixture partner token"),
    );
    let identified = call(&recreated, identify).await;
    assert_eq!(identified.status(), 200);
    assert!(
        ec_cookie(&identified).is_none(),
        "identify must not emit an identity cookie"
    );
    let data: serde_json::Value = serde_json::from_slice(
        &identified
            .into_body()
            .into_bytes_bounded(64 * 1024)
            .await
            .expect("should read identify JSON"),
    )
    .expect("should decode identify JSON");
    assert_eq!(
        data["uid"], "fixture-uid",
        "native identify must use the live graph and partner registry"
    );
    let mut withdrawal = request(Method::GET, "/article", Some(&cookie));
    withdrawal.headers_mut().insert(
        "sec-gpc",
        "1".parse().expect("should set genuine withdrawal input"),
    );
    // Revocation remains writable even when the user agent no longer passes
    // the ordinary browser heuristic.
    withdrawal.headers_mut().insert(
        header::USER_AGENT,
        "privacy-client".parse().expect("should set hardened UA"),
    );
    let regulated = router(
        store.clone(),
        Some(GeoInfo::country_region("US", Some("TN".to_owned()))),
    );
    let withdrawn = call(&regulated, withdrawal).await;
    assert_eq!(ec_cookie(&withdrawn).as_deref(), Some(""));
    let row: serde_json::Value = serde_json::from_slice(
        &store
            .lookup(&ec)
            .await
            .expect("should read tombstone")
            .expect("should retain tombstone")
            .body,
    )
    .expect("should decode tombstone");
    assert_eq!(row["consent"]["ok"], false);
    assert!(
        row.get("ids")
            .is_none_or(|ids| ids.as_object().is_some_and(serde_json::Map::is_empty)),
        "should clear all withdrawn partner IDs"
    );
    let retry = call(&recreated, request(Method::GET, "/article", Some(&cookie))).await;
    assert!(
        ec_cookie(&retry).is_none(),
        "should not resurrect a revoked identity"
    );
    let row: serde_json::Value = serde_json::from_slice(
        &store
            .lookup(&ec)
            .await
            .expect("should read tombstone after retry")
            .expect("should retain tombstone")
            .body,
    )
    .expect("should decode retained tombstone");
    assert_eq!(row["consent"]["ok"], false);
}

#[tokio::test(flavor = "current_thread")]
async fn missing_consent_unknown_geo_storage_failure_and_non_navigation_do_not_mint() {
    let store = Store::default();
    for geo in [
        None,
        Some(GeoInfo::country_region("US", Some("TN".to_owned()))),
    ] {
        let response = call(
            &router(store.clone(), geo),
            request(Method::GET, "/article", None),
        )
        .await;
        assert!(
            ec_cookie(&response).is_none(),
            "should preserve missing-consent policy"
        );
    }
    let app = allowed_router(store.clone());
    for (method, path) in [
        (Method::GET, "/health"),
        (Method::GET, "/static/tsjs=fixture"),
        (Method::OPTIONS, "/article"),
        (Method::OPTIONS, "/_ts/page-bids"),
        (Method::GET, "/_ts/admin/ec"),
        (Method::GET, "/_ts/api/v1/identify"),
        (Method::HEAD, "/article"),
        (Method::POST, "/article"),
    ] {
        let response = call(&app, request(method, path, None)).await;
        assert!(ec_cookie(&response).is_none(), "should not mint on {path}");
    }
    let mut asset = request(Method::GET, "/article", None);
    asset.headers_mut().insert(
        "sec-fetch-dest",
        "image".parse().expect("should set subresource destination"),
    );
    assert!(ec_cookie(&call(&app, asset).await).is_none());
    let mut identify = request(Method::GET, "/_ts/api/v1/identify", None);
    identify.headers_mut().insert(
        header::AUTHORIZATION,
        "Bearer native-fixture-partner-token-32-bytes-minimum"
            .parse()
            .expect("should set fixture partner token"),
    );
    let identified = call(&app, identify).await;
    assert_eq!(
        identified.status(),
        204,
        "authenticated identify without an identity stays read-only"
    );
    assert!(ec_cookie(&identified).is_none());
    assert!(
        store.is_empty().await,
        "should retain no persistent identity rows"
    );
    store.fail_writes.store(true, Ordering::SeqCst);
    assert!(
        ec_cookie(&call(&app, request(Method::GET, "/article", None)).await).is_none(),
        "uncertain writes must not issue phantom identities"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn regulated_fixture_uses_decoded_signal_and_gpc_keeps_precedence() {
    let store = Store::default();
    let app = router(
        store.clone(),
        Some(GeoInfo::country_region("US", Some("TN".to_owned()))),
    );
    // Encoded, synthetic US Privacy input exercises the real core decoder.
    // This is not evidence of consent capture from a real publisher CMP.
    let accepted = call(
        &app,
        request(Method::GET, "/article", Some("us_privacy=1YNN")),
    )
    .await;
    let ec = ec_cookie(&accepted).expect("should honor the decoded qualifying fixture signal");
    assert!(
        store
            .key_exists(&ec)
            .await
            .expect("should confirm regulated fixture row")
    );
    let mut denied = request(Method::GET, "/article", Some("us_privacy=1YNN"));
    denied
        .headers_mut()
        .insert("sec-gpc", "1".parse().expect("should set GPC input"));
    assert!(
        ec_cookie(&call(&app, denied).await).is_none(),
        "GPC must override the non-opt-out fixture signal"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn delayed_storage_yields_to_health_and_cancels_without_cookie() {
    let store = Store::default();
    store.pause_writes.store(true, Ordering::SeqCst);
    let app = allowed_router(store.clone());
    let article = call(&app, request(Method::GET, "/article", None));
    tokio::pin!(article);
    tokio::select! {
        _ = store.started.notified() => {},
        _ = &mut article => panic!("storage should remain pending"),
    }
    let health = tokio::time::timeout(
        Duration::from_millis(250),
        call(&app, request(Method::GET, "/health", None)),
    )
    .await
    .expect("health should proceed on the same current thread while storage waits");
    assert_eq!(health.status(), 200);
    assert!(ec_cookie(&health).is_none());
    assert!(
        store.is_empty().await,
        "should retain no persistent identity rows"
    );
    // Dropping the pending request models cancellation before acknowledgement.
    // No synchronous executor/worker must be joined before this runtime exits.
}

#[tokio::test(flavor = "current_thread")]
async fn orphan_recovery_rotates_only_after_an_eligible_publisher_start() {
    let store = Store::default();
    let app = allowed_router(store.clone());
    let created = call(&app, request(Method::GET, "/article", None)).await;
    let original = ec_cookie(&created).expect("should issue initial identity");
    store
        .delete(&original)
        .await
        .expect("should simulate expired identity row");
    let eids = BASE64.encode(
        json!([{"source":"partner.example.com","uids":[{"id":"fixture-uid","atype":1}]}])
            .to_string(),
    );
    let cookie = format!("ts-ec={original}; ts-eids={eids}");
    let named = call(
        &app,
        request(Method::GET, "/_ts/api/v1/identify", Some(&cookie)),
    )
    .await;
    assert!(
        ec_cookie(&named).is_none(),
        "read-only identify must not recover an orphan"
    );
    assert!(
        store.is_empty().await,
        "should retain no persistent identity rows"
    );
    let recovered = call(&app, request(Method::GET, "/article", Some(&cookie))).await;
    let replacement =
        ec_cookie(&recovered).expect("should recover on eligible publisher navigation");
    assert_ne!(replacement, original);
    assert!(
        store
            .key_exists(&replacement)
            .await
            .expect("should confirm recovered row before cookie")
    );
    assert!(
        !store
            .key_exists(&original)
            .await
            .expect("should leave orphan absent")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shared_record_backend_preserves_bytes_metadata_and_stale_tokens() {
    let store = Store::default();
    let key = "hash.ABC123";
    let write = EcKvWrite {
        body: "original",
        metadata: "{\"kind\":\"ec\"}",
        ttl: Duration::from_secs(60),
        mode: EcKvWriteMode::Add,
    };
    assert_eq!(
        store
            .insert(key, write)
            .await
            .expect("should insert record"),
        EcKvWriteOutcome::Written
    );
    let original = store
        .lookup(key)
        .await
        .expect("should read record")
        .expect("should retain record");
    assert_eq!(
        original.metadata.as_deref(),
        Some(write.metadata.as_bytes())
    );
    store
        .handle
        .put_bytes(key, vec![0_u8, 255_u8].into())
        .await
        .expect("should replace same value through ordinary KV");
    let ordinary = store
        .lookup(key)
        .await
        .expect("should read ordinary replacement")
        .expect("should retain ordinary replacement");
    assert_eq!(ordinary.body, vec![0_u8, 255_u8]);
    assert!(
        ordinary.metadata.is_none(),
        "should clear metadata on ordinary replacement"
    );
    assert_ne!(ordinary.generation, original.generation);
    let stale = EcKvWrite {
        mode: EcKvWriteMode::IfGenerationMatch(original.generation),
        ..write
    };
    assert_eq!(
        store
            .insert(key, stale)
            .await
            .expect("should reject stale replacement"),
        EcKvWriteOutcome::PreconditionFailed
    );
    store.delete(key).await.expect("should delete same value");
    assert!(
        !store
            .key_exists(key)
            .await
            .expect("should prove exact absence")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shared_record_backend_collects_native_pages_without_changing_capped_semantics() {
    let store = Store::default();
    for index in 0_u32..1001_u32 {
        store
            .handle
            .put_bytes(&format!("hash.{index:06}"), vec![1_u8].into())
            .await
            .expect("should seed paged records");
    }
    let complete = store
        .list_keys_with_prefix("hash.", 2000)
        .await
        .expect("should traverse multiple native pages");
    assert_eq!(
        complete.len(),
        1001,
        "should not treat the first native page as complete"
    );
    let capped = store
        .list_keys_with_prefix("hash.", 100)
        .await
        .expect("should retain graph's bounded count contract");
    assert_eq!(capped.len(), 100);
    assert!(
        store.list_keys_with_prefix("hash", 100).await.is_err(),
        "should reject partial partitions"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn graph_cluster_count_uses_a_complete_record_partition() {
    let store = Store::default();
    let hash = "a".repeat(64);
    for suffix in ["Ab1234", "Cd5678"] {
        store
            .handle
            .put_bytes(&format!("{hash}.{suffix}"), vec![1_u8].into())
            .await
            .expect("should seed root in one IP group");
    }
    let marker = format!("__ts_ec_withdrawal_complete__:{hash}.Ab1234:123");
    store
        .handle
        .put_bytes(&marker, vec![1_u8].into())
        .await
        .expect("should seed separate marker group");
    let graph = trusted_server_core::ec::kv::KvIdentityGraph::new(store);
    assert_eq!(
        graph
            .count_hash_prefix_keys(&hash)
            .await
            .expect("should query the complete IP partition"),
        2,
        "should exclude completion markers"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn shared_app_batch_sync_updates_the_navigation_record() {
    let store = Store::default();
    let app = allowed_router(store.clone());
    let issued = call(&app, request(Method::GET, "/article", None)).await;
    let ec = ec_cookie(&issued).expect("should create a navigation identity");
    let mut sync = request(Method::POST, "/_ts/api/v1/batch-sync", None);
    sync.headers_mut().insert(
        header::AUTHORIZATION,
        "Bearer native-fixture-partner-token-32-bytes-minimum"
            .parse()
            .expect("should set partner token"),
    );
    *sync.body_mut() = Body::from(
        serde_json::to_vec(&json!({"mappings":[{
            "ec_id":ec,"partner_uid":"batch-local-uid","timestamp":1
        }]}))
        .expect("should encode batch"),
    );
    let response = call(&app, sync).await;
    assert_eq!(
        response.status(),
        200,
        "should execute batch sync locally with the shared store"
    );
    assert!(
        ec_cookie(&response).is_none(),
        "should keep partner API cookie-free"
    );
    let stored = store
        .lookup(&ec)
        .await
        .expect("should read store")
        .expect("should retain navigation record");
    let row: serde_json::Value =
        serde_json::from_slice(&stored.body).expect("should decode record");
    assert_eq!(
        row["ids"]["partner.example.com"]["uid"], "batch-local-uid",
        "should update the same authoritative record"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn native_creation_without_ja4_does_not_require_a_browser_user_agent() {
    let store = Store::default();
    let app = allowed_router(store.clone());
    let mut navigation = request(Method::GET, "/article", None);
    navigation.headers_mut().remove(header::USER_AGENT);
    let response = call(&app, navigation).await;
    let ec = ec_cookie(&response).expect("should issue identity when trusted JA4 is absent");
    assert!(
        store
            .lookup(&ec)
            .await
            .expect("should read store")
            .is_some(),
        "should persist before issuance"
    );
}

fn ec_post(cookie: Option<&str>) -> Request {
    let mut req = request(Method::POST, "/_ts/api/v1/ec", cookie);
    req.headers_mut().insert(
        header::ORIGIN,
        "http://test-publisher.example.com"
            .parse()
            .expect("should set same origin"),
    );
    *req.body_mut() =
        Body::from(serde_json::to_vec(&json!({})).expect("should encode consent snapshot"));
    req
}

#[tokio::test(flavor = "current_thread")]
async fn ec_route_creates_record_and_rejects_cross_origin_before_writes() {
    let store = Store::default();
    let app = allowed_router(store.clone());
    let created = call(&app, ec_post(None)).await;
    assert_eq!(created.status(), 200, "should answer the EC POST locally");
    let ec = ec_cookie(&created).expect("should issue EC on its own response");
    assert!(
        store
            .lookup(&ec)
            .await
            .expect("should read store")
            .is_some(),
        "should create backing row before response"
    );
    assert_eq!(
        created
            .headers()
            .get(header::CACHE_CONTROL)
            .expect("should disable caching"),
        "no-store, private",
        "should keep EC response private"
    );
    let mut invalid = ec_post(None);
    invalid.headers_mut().insert(
        header::ORIGIN,
        "https://other.example.com"
            .parse()
            .expect("should set cross origin"),
    );
    let denied = call(&app, invalid).await;
    assert_eq!(denied.status(), 403, "should reject cross-origin minting");
    assert!(
        ec_cookie(&denied).is_none(),
        "should not issue on rejection"
    );
    let get = call(&app, request(Method::GET, "/_ts/api/v1/ec", None)).await;
    assert_eq!(get.status(), 204, "should answer GET without minting");
    assert!(ec_cookie(&get).is_none(), "should never mint on pixel GET");
}

#[tokio::test(flavor = "current_thread")]
async fn identify_withdrawal_tombstones_and_expires_browser_ec() {
    let store = Store::default();
    let created = call(
        &allowed_router(store.clone()),
        request(Method::GET, "/article", None),
    )
    .await;
    let ec = ec_cookie(&created).expect("should create live record");
    let app = router(
        store.clone(),
        Some(GeoInfo::country_region("US", Some("TN".to_owned()))),
    );
    let mut identify = request(
        Method::GET,
        "/_ts/api/v1/identify",
        Some(&format!("ts-ec={ec}")),
    );
    identify
        .headers_mut()
        .insert("sec-gpc", "1".parse().expect("should request withdrawal"));
    identify.headers_mut().insert(
        header::AUTHORIZATION,
        "Bearer native-fixture-partner-token-32-bytes-minimum"
            .parse()
            .expect("should authenticate partner"),
    );
    let denied = call(&app, identify).await;
    assert_eq!(
        denied.status(),
        403,
        "should refuse withdrawn identity lookup"
    );
    let cookies: Vec<_> = denied
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .collect();
    assert_eq!(
        cookies.len(),
        1,
        "should emit only EC withdrawal deletion on identify"
    );
    let deletion = cookies[0].to_str().expect("should decode EC deletion");
    assert!(
        deletion.starts_with("ts-ec=;") && deletion.contains("Max-Age=0"),
        "should expire browser EC on the tombstoning response"
    );
    let row: serde_json::Value = serde_json::from_slice(
        &store
            .lookup(&ec)
            .await
            .expect("should read store")
            .expect("should retain withdrawal record")
            .body,
    )
    .expect("should decode record");
    assert_eq!(
        row["consent"]["ok"], false,
        "should tombstone withdrawal on identify"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ec_withdrawal_storage_failure_is_not_acknowledged_as_success() {
    let store = Store::default();
    let created = call(
        &allowed_router(store.clone()),
        request(Method::GET, "/article", None),
    )
    .await;
    let ec = ec_cookie(&created).expect("should create live record");
    let app = router(
        store.clone(),
        Some(GeoInfo::country_region("US", Some("TN".to_owned()))),
    );
    store.fail_writes.store(true, Ordering::SeqCst);
    let mut withdrawal = ec_post(Some(&format!("ts-ec={ec}")));
    withdrawal.headers_mut().insert(
        "sec-gpc",
        "1".parse().expect("should request genuine withdrawal"),
    );
    let response = call(&app, withdrawal).await;
    assert_eq!(
        response.status(),
        503,
        "should report that authoritative withdrawal could not be persisted"
    );
    assert_eq!(
        ec_cookie(&response).as_deref(),
        Some(""),
        "should still expire the browser identity on the EC response"
    );
    let row: serde_json::Value = serde_json::from_slice(
        &store
            .lookup(&ec)
            .await
            .expect("should read store")
            .expect("should retain original record")
            .body,
    )
    .expect("should decode record");
    assert_eq!(
        row["consent"]["ok"], true,
        "should prove the failed store left a live authoritative row"
    );
}
