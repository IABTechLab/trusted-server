//! Native lifecycle regressions with isolated conditional storage and origins.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use edgezero_core::body::Body;
use edgezero_core::http::{Method, Request, Response, header};
use edgezero_core::router::RouterService;
use error_stack::Report;
use serde_json::json;
use tokio::sync::Notify;
use trusted_server_adapter_axum::app::TrustedServerApp;
use trusted_server_core::ec::kv_backend::{
    EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteMode, EcKvWriteOutcome,
};
use trusted_server_core::error::TrustedServerError;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformConfigStore, PlatformError, PlatformGeo, PlatformSecretStore,
    RuntimeServices, StoreId, StoreName, UnavailableKvStore,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::test_support::nextjs_auction::{self, NextJsAuctionOrigin};

#[derive(Clone, Default)]
struct Store {
    rows: Arc<Mutex<BTreeMap<String, EcKvLookup>>>,
    generation: Arc<AtomicU64>,
    fail_writes: Arc<AtomicBool>,
    pause_writes: Arc<AtomicBool>,
    started: Arc<Notify>,
    resume: Arc<Notify>,
}

#[async_trait(?Send)]
impl EcKvStore for Store {
    fn store_name(&self) -> &str {
        "native-ec-fixture"
    }
    async fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
        Ok(self
            .rows
            .lock()
            .expect("should lock fixture rows")
            .get(key)
            .cloned())
    }
    async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
        Ok(self.lookup(key).await?.is_some())
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
        let mut rows = self.rows.lock().expect("should lock fixture rows");
        match write.mode {
            EcKvWriteMode::Add if rows.contains_key(key) => {
                return Ok(EcKvWriteOutcome::PreconditionFailed);
            }
            EcKvWriteMode::IfGenerationMatch(expected)
                if rows.get(key).map(|row| row.generation) != Some(expected) =>
            {
                return Ok(EcKvWriteOutcome::PreconditionFailed);
            }
            _ => {}
        }
        rows.insert(
            key.to_owned(),
            EcKvLookup {
                body: write.body.as_bytes().to_vec(),
                metadata: Some(write.metadata.as_bytes().to_vec()),
                generation: self.generation.fetch_add(1, Ordering::SeqCst) + 1,
            },
        );
        Ok(EcKvWriteOutcome::Written)
    }
    async fn list_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<String>, Report<TrustedServerError>> {
        Ok(self
            .rows
            .lock()
            .expect("should lock fixture rows")
            .keys()
            .filter(|key| key.starts_with(prefix))
            .take(limit as usize)
            .cloned()
            .collect())
    }
    async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
        self.rows
            .lock()
            .expect("should lock fixture rows")
            .remove(key);
        Ok(())
    }
}

struct UnusedStores;
#[async_trait(?Send)]
impl PlatformConfigStore for UnusedStores {
    fn get(&self, _: &StoreName, _: &str) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
    async fn put(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
    async fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}
#[async_trait(?Send)]
impl PlatformSecretStore for UnusedStores {
    fn get_bytes(&self, _: &StoreName, _: &str) -> Result<Vec<u8>, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
    async fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
    async fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
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

fn router(store: Store, geo: Option<GeoInfo>) -> RouterService {
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
fn allowed_router(store: Store) -> RouterService {
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
    let mut bot = request(Method::GET, "/article", None);
    bot.headers_mut().insert(
        header::USER_AGENT,
        "curl/8.0".parse().expect("should set bot UA"),
    );
    assert!(ec_cookie(&call(&app, bot).await).is_none());
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
        store
            .rows
            .lock()
            .expect("should read fixture rows")
            .is_empty()
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
        store
            .rows
            .lock()
            .expect("should inspect paused rows")
            .is_empty()
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
        store
            .rows
            .lock()
            .expect("should read fixture rows")
            .is_empty()
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
