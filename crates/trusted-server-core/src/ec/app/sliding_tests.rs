//! Sliding EC lifetime behavior through shared request and response processing.

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use edgezero_core::body::Body;
use edgezero_core::http::{Method, Request, Response, StatusCode, header};
use edgezero_core::ingress::{InboundOrigin, OriginSource};
use error_stack::Report;
use serde_json::json;

use super::{IdentityApp, IdentityPlatform};
use crate::cookies::template_cache_policy::{validate_cookie_names, validate_sharedid_key_cookie};
use crate::ec::coordinator::results::{
    DropCounts, LookupResults, LookupStatus, OptOutMemory, ProviderLookup,
};
use crate::ec::eid_types::{AgentType, EidFields, Expiry, ProviderId, SourceDomain, UidValue};
use crate::ec::kv_backend::test_support::{FailingEcKv, InMemoryEcKv, WriteFailingEcKv};
use crate::ec::kv_backend::{EcKvLookup, EcKvStore, EcKvWrite, EcKvWriteOutcome};
use crate::ec::kv_types::{KvEid, KvEntry, KvMetadata, KvUid};
use crate::ec::rate_limiter::NoRateLimit;
use crate::ec::{EcContext, EcKvSnapshot, current_timestamp};
use crate::error::TrustedServerError;
use crate::http_util::RuntimeRequestOrigin;
use crate::platform::test_support::{NoopConfigStore, NoopSecretStore, noop_services};
use crate::platform::{ClientInfo, GeoInfo, PlatformError, PlatformGeo, RuntimeServices};
use crate::response_privacy::apply_response_headers_with_cache_privacy;
use crate::settings::ProxyAssetRoute;
use crate::test_support::tests::create_test_settings;

const EC: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123";
const CUSTOM_MAX_AGE: &str = "Max-Age=2592000";

struct ObservedStore {
    inner: Arc<dyn EcKvStore + Send + Sync>,
    reads: AtomicUsize,
    writes: AtomicUsize,
}

#[async_trait::async_trait(?Send)]
impl EcKvStore for ObservedStore {
    fn store_name(&self) -> &str {
        self.inner.store_name()
    }
    async fn lookup(&self, key: &str) -> Result<Option<EcKvLookup>, Report<TrustedServerError>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.lookup(key).await
    }
    async fn key_exists(&self, key: &str) -> Result<bool, Report<TrustedServerError>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.key_exists(key).await
    }
    async fn insert(
        &self,
        key: &str,
        write: EcKvWrite<'_>,
    ) -> Result<EcKvWriteOutcome, Report<TrustedServerError>> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.insert(key, write).await
    }
    async fn list_keys_with_prefix(
        &self,
        prefix: &str,
        limit: u32,
    ) -> Result<Vec<String>, Report<TrustedServerError>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.list_keys_with_prefix(prefix, limit).await
    }
    async fn delete(&self, key: &str) -> Result<(), Report<TrustedServerError>> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.delete(key).await
    }
}

struct FixtureGeo;
impl PlatformGeo for FixtureGeo {
    fn lookup(&self, _ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(Some(GeoInfo::country_region("US", Some("TN".to_owned()))))
    }
}

struct Fixture {
    app: Arc<IdentityApp>,
    services: RuntimeServices,
    store: Option<Arc<ObservedStore>>,
}

impl Fixture {
    fn new(enabled: bool, store_available: bool) -> Self {
        Self::with_store(
            enabled,
            store_available.then(|| {
                Arc::new(InMemoryEcKv::new("sliding-fixture")) as Arc<dyn EcKvStore + Send + Sync>
            }),
        )
    }

    fn with_store(enabled: bool, inner: Option<Arc<dyn EcKvStore + Send + Sync>>) -> Self {
        Self::with_options(enabled, Some(30), inner)
    }

    fn defaults(store_available: bool) -> Self {
        Self::with_options(
            false,
            None,
            store_available.then(|| {
                Arc::new(InMemoryEcKv::new("default-fixture")) as Arc<dyn EcKvStore + Send + Sync>
            }),
        )
    }

    fn with_options(
        enabled: bool,
        lifetime: Option<u16>,
        inner: Option<Arc<dyn EcKvStore + Send + Sync>>,
    ) -> Self {
        let mut settings = create_test_settings();
        settings.ec.partners.clear();
        settings.publisher.domain = "publisher.example.com".to_owned();
        let mut ec = serde_json::to_value(&settings.ec).expect("should encode EC settings");
        if let Some(days) = lifetime {
            ec["max_age_days"] = json!(days);
        }
        if enabled {
            ec["sliding_refresh"] = json!(true);
        }
        settings.ec = serde_json::from_value(ec).expect("should configure EC lifetime");
        settings.proxy.asset_routes.push(ProxyAssetRoute::new(
            "/assets/",
            "https://assets.example.com",
        ));
        let store = inner.map(|inner| {
            Arc::new(ObservedStore {
                inner,
                reads: AtomicUsize::new(0),
                writes: AtomicUsize::new(0),
            })
        });
        let app = IdentityApp::build(
            Arc::new(settings),
            IdentityPlatform {
                record_store: store
                    .as_ref()
                    .map(|store| Arc::clone(store) as Arc<dyn EcKvStore + Send + Sync>),
                rate_limiter: Some(Arc::new(NoRateLimit)),
                providers: None,
            },
        )
        .expect("should build identity app");
        let base = noop_services();
        let services = RuntimeServices::builder()
            .config_store(Arc::new(NoopConfigStore))
            .secret_store(Arc::new(NoopSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .outbound_client(base.outbound_client().clone())
            .clock(base.clock().clone())
            .geo(Arc::new(FixtureGeo))
            .client_info(ClientInfo {
                client_ip: Some("192.0.2.7".parse().expect("should parse example IP")),
                ..ClientInfo::default()
            })
            .build();
        Self {
            app,
            services,
            store,
        }
    }

    fn assert_no_store_io(&self) {
        if let Some(store) = &self.store {
            assert_eq!(
                store.reads.load(Ordering::SeqCst),
                0,
                "should add no identity record reads"
            );
            assert_eq!(
                store.writes.load(Ordering::SeqCst),
                0,
                "should add no identity record writes"
            );
        }
    }

    async fn visit(&self, case: Case, update: fn(&mut EcContext)) -> (Response, EcKvSnapshot) {
        let request = case.request();
        let mut identity = self
            .app
            .read_request(&self.services, &request)
            .expect("should read identity request");
        self.app
            .prepare_navigation(&mut identity.context, &request, case.allow_creation)
            .await;
        update(&mut identity.context);
        let mut response = http::Response::builder()
            .status(case.status)
            .header(header::CONTENT_TYPE, case.content_type)
            .header(header::CACHE_CONTROL, "public, max-age=120")
            .header("surrogate-control", "max-age=120")
            .body(Body::from("example-response"))
            .expect("should build origin response");
        self.app
            .finalize(
                &mut identity.context,
                identity.eids_cookie.as_deref(),
                &identity.sharedid,
                true,
                &mut response,
            )
            .await;
        apply_response_headers_with_cache_privacy(self.app.settings(), &mut response);
        (response, identity.context.kv_snapshot().clone())
    }
}

struct Case {
    method: Method,
    path: &'static str,
    status: StatusCode,
    content_type: &'static str,
    document: bool,
    consent: Option<bool>,
    ec: Option<&'static str>,
    allow_creation: bool,
    conflicting_header: bool,
}
impl Default for Case {
    fn default() -> Self {
        Self {
            method: Method::GET,
            path: "/article",
            status: StatusCode::OK,
            content_type: "text/html",
            document: true,
            consent: Some(true),
            ec: Some(EC),
            allow_creation: false,
            conflicting_header: false,
        }
    }
}
impl Case {
    fn request(&self) -> Request {
        let mut cookie = self
            .ec
            .map(|ec| format!("ts-ec={ec}; "))
            .unwrap_or_default();
        if let Some(consent) = self.consent {
            cookie.push_str(if consent {
                "us_privacy=1YNN"
            } else {
                "us_privacy=1YYN"
            });
        }
        let mut request = http::Request::builder()
            .method(self.method.clone())
            .uri(self.path)
            .header(header::COOKIE, cookie)
            .header(
                "sec-fetch-dest",
                if self.document { "document" } else { "empty" },
            )
            .body(Body::empty())
            .expect("should build publisher request");
        if self.conflicting_header {
            request.headers_mut().insert(
                "x-ts-ec",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.XYZ789"
                    .parse()
                    .expect("should encode ignored header"),
            );
        }
        request
    }
}

fn cookies(response: &Response) -> Vec<&str> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().expect("should decode response cookie"))
        .collect()
}
fn assert_same_ec_renewed(response: &Response) {
    let set = cookies(response);
    assert_eq!(set.len(), 1, "should renew only the EC cookie: {set:?}");
    assert!(
        set[0].starts_with(&format!("ts-ec={EC};")),
        "should preserve the complete EC value"
    );
    assert!(
        set[0].contains(CUSTOM_MAX_AGE),
        "should apply the full configured thirty-day lifetime: {set:?}"
    );
    assert!(
        !response.headers().contains_key("x-ts-ec"),
        "should not expose a generated identity header"
    );
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, max-age=0",
        "should guard renewed identity against public caching"
    );
    assert!(
        !response.headers().contains_key("surrogate-control"),
        "should remove edge caching on renewed identity"
    );
}
fn present(live: bool) -> EcKvSnapshot {
    let mut entry = KvEntry::tombstone(current_timestamp());
    entry.consent.ok = live;
    EcKvSnapshot::Present {
        ec_id: EC.to_owned(),
        entry: Box::new(entry),
        generation: Some(1),
    }
}
fn known_missing(context: &mut EcContext) {
    context.set_kv_snapshot(EcKvSnapshot::Missing {
        ec_id: EC.to_owned(),
    });
}
fn known_revoked(context: &mut EcContext) {
    context.set_kv_snapshot(present(false));
}
fn known_failed(context: &mut EcContext) {
    context.set_kv_snapshot(EcKvSnapshot::Failed {
        ec_id: EC.to_owned(),
    });
}
fn known_live(context: &mut EcContext) {
    context.set_kv_snapshot(present(true));
}

#[tokio::test(flavor = "current_thread")]
async fn unread_identity_renews_without_record_io() {
    let fixture = Fixture::with_store(true, Some(Arc::new(FailingEcKv::new("unread-store"))));
    let (response, snapshot) = fixture.visit(Case::default(), |_| {}).await;
    assert_same_ec_renewed(&response);
    assert_eq!(
        snapshot,
        EcKvSnapshot::NotRead,
        "should preserve the unread snapshot"
    );
    fixture.assert_no_store_io();
}

#[tokio::test(flavor = "current_thread")]
async fn unavailable_record_store_still_renews_held_ec() {
    let fixture = Fixture::new(true, false);
    let (response, snapshot) = fixture.visit(Case::default(), |_| {}).await;
    assert_same_ec_renewed(&response);
    assert_eq!(
        snapshot,
        EcKvSnapshot::NotRead,
        "should not invent a failed record read"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn known_missing_record_suppresses_renewal_without_recovery() {
    let fixture = Fixture::new(true, true);
    let (response, _) = fixture.visit(Case::default(), known_missing).await;
    assert!(
        cookies(&response).is_empty(),
        "should not renew a known absent identity"
    );
    fixture.assert_no_store_io();
}

#[tokio::test(flavor = "current_thread")]
async fn known_revoked_record_suppresses_renewal() {
    let fixture = Fixture::new(true, true);
    let (response, _) = fixture.visit(Case::default(), known_revoked).await;
    assert!(
        cookies(&response).is_empty(),
        "should not renew a revoked identity"
    );
    fixture.assert_no_store_io();
}

#[tokio::test(flavor = "current_thread")]
async fn failed_record_snapshot_suppresses_renewal() {
    let fixture = Fixture::new(true, true);
    let (response, _) = fixture.visit(Case::default(), known_failed).await;
    assert!(
        cookies(&response).is_empty(),
        "should not renew after an actual record read failure"
    );
    fixture.assert_no_store_io();
}

#[tokio::test(flavor = "current_thread")]
async fn known_live_record_renews_without_reading_or_writing_it_again() {
    let fixture = Fixture::new(true, true);
    let (response, snapshot) = fixture.visit(Case::default(), known_live).await;
    assert_same_ec_renewed(&response);
    assert!(
        snapshot.entry_for(EC).is_some_and(|entry| entry.consent.ok),
        "should preserve the live snapshot"
    );
    fixture.assert_no_store_io();
}

fn add_lookup(context: &mut EcContext) {
    known_live(context);
    let uid = KvUid::new(
        UidValue::try_from("example-provider-uid").expect("should build UID"),
        AgentType::try_from(1).expect("should build agent type"),
        None,
        Some(ProviderId::new("example-provider")),
        Expiry::Never,
    );
    context.set_lookup_results(LookupResults::new(
        EC,
        vec![ProviderLookup {
            provider: ProviderId::new("example-provider"),
            status: LookupStatus::Found {
                kept: 1,
                dropped: DropCounts::default(),
            },
            entries: vec![(
                SourceDomain::try_from("provider.example.com").expect("should build source"),
                vec![KvEid::new(EidFields::default(), vec![uid])],
            )],
            cleared: Vec::new(),
            opt_out: OptOutMemory::Unchanged,
        }],
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn failed_page_load_write_suppresses_renewal() {
    let failing = Arc::new(WriteFailingEcKv::new());
    let mut entry = KvEntry::tombstone(current_timestamp());
    entry.consent.ok = true;
    failing
        .seed(
            EC,
            &serde_json::to_string(&entry).expect("should encode row"),
            &serde_json::to_string(&KvMetadata::from_entry(&entry))
                .expect("should encode metadata"),
        )
        .await;
    let fixture = Fixture::with_store(true, Some(failing));
    let (response, _) = fixture.visit(Case::default(), add_lookup).await;
    assert!(
        cookies(&response).is_empty(),
        "should suppress renewal after planned write failure"
    );
    assert!(
        fixture
            .store
            .as_ref()
            .expect("should have observed store")
            .writes
            .load(Ordering::SeqCst)
            > 0,
        "should actually attempt the failing page-load write"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn renewal_obeys_existing_document_status_method_and_asset_gates() {
    for (method, path, status, content_type, document, expected) in [
        (
            Method::GET,
            "/article",
            StatusCode::OK,
            "text/html",
            true,
            true,
        ),
        (
            Method::POST,
            "/article",
            StatusCode::OK,
            "application/xhtml+xml",
            true,
            true,
        ),
        (
            Method::HEAD,
            "/article",
            StatusCode::OK,
            "text/html; charset=utf-8",
            true,
            true,
        ),
        (
            Method::PATCH,
            "/article",
            StatusCode::INTERNAL_SERVER_ERROR,
            "text/html",
            true,
            true,
        ),
        (
            Method::GET,
            "/article",
            StatusCode::NOT_MODIFIED,
            "application/json",
            true,
            true,
        ),
        (
            Method::GET,
            "/article",
            StatusCode::FOUND,
            "text/html",
            true,
            false,
        ),
        (
            Method::GET,
            "/article",
            StatusCode::TEMPORARY_REDIRECT,
            "text/html",
            true,
            false,
        ),
        (
            Method::GET,
            "/article",
            StatusCode::OK,
            "application/json",
            true,
            false,
        ),
        (
            Method::GET,
            "/article",
            StatusCode::OK,
            "text/html",
            false,
            false,
        ),
        (
            Method::GET,
            "/assets/image",
            StatusCode::OK,
            "text/html",
            true,
            false,
        ),
        (
            Method::HEAD,
            "/assets/image",
            StatusCode::OK,
            "text/html",
            true,
            false,
        ),
        (
            Method::POST,
            "/assets/image",
            StatusCode::OK,
            "text/html",
            true,
            true,
        ),
    ] {
        let fixture = Fixture::new(true, true);
        let (response, _) = fixture
            .visit(
                Case {
                    method,
                    path,
                    status,
                    content_type,
                    document,
                    ..Case::default()
                },
                |_| {},
            )
            .await;
        if expected {
            assert_same_ec_renewed(&response);
        } else {
            assert!(
                cookies(&response).is_empty(),
                "should keep ineligible document or asset response unchanged at {status} {content_type}"
            );
        }
        fixture.assert_no_store_io();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ignored_identity_header_does_not_block_cookie_renewal() {
    let fixture = Fixture::new(true, true);
    let (response, _) = fixture
        .visit(
            Case {
                conflicting_header: true,
                ..Case::default()
            },
            |_| {},
        )
        .await;
    assert_same_ec_renewed(&response);
    fixture.assert_no_store_io();
}

#[tokio::test(flavor = "current_thread")]
async fn unknown_consent_and_invalid_cookie_never_renew() {
    for case in [
        Case {
            consent: None,
            ..Case::default()
        },
        Case {
            ec: Some("invalid"),
            ..Case::default()
        },
    ] {
        let fixture = Fixture::new(true, true);
        let (response, _) = fixture.visit(case, |_| {}).await;
        assert!(
            cookies(&response).is_empty(),
            "should require a valid incoming EC and positive consent"
        );
        fixture.assert_no_store_io();
    }
}

fn ec_post(ec: Option<&str>) -> Request {
    let mut request = http::Request::builder()
        .method(Method::POST)
        .uri("https://publisher.example.com/_ts/api/v1/ec")
        .header(header::ORIGIN, "https://publisher.example.com")
        .header("sec-fetch-dest", "document")
        .header(
            header::COOKIE,
            format!(
                "{}us_privacy=1YNN",
                ec.map(|value| format!("ts-ec={value}; "))
                    .unwrap_or_default()
            ),
        )
        .body(Body::from(json!({}).to_string()))
        .expect("should build EC POST");
    request.extensions_mut().insert(RuntimeRequestOrigin::new(
        InboundOrigin::parse("https", "publisher.example.com", OriginSource::RuntimeUri)
            .expect("should provide publisher ingress"),
    ));
    request
}

#[tokio::test(flavor = "current_thread")]
async fn returning_ec_post_never_renews_even_with_document_headers() {
    let fixture = Fixture::new(true, true);
    let response = fixture
        .app
        .ec_route(&fixture.services, ec_post(Some(EC)))
        .await;
    assert_eq!(response.status(), StatusCode::OK, "should admit EC POST");
    assert!(
        cookies(&response).is_empty(),
        "should exclude API responses from sliding renewal"
    );
    fixture.assert_no_store_io();
}

#[tokio::test(flavor = "current_thread")]
async fn disabled_sliding_retains_existing_response_matrix() {
    for store_available in [true, false] {
        for (method, path, status, content_type, document) in [
            (Method::GET, "/article", StatusCode::OK, "text/html", true),
            (
                Method::POST,
                "/article",
                StatusCode::OK,
                "application/xhtml+xml",
                true,
            ),
            (
                Method::HEAD,
                "/article",
                StatusCode::NOT_MODIFIED,
                "application/json",
                true,
            ),
            (
                Method::GET,
                "/article",
                StatusCode::FOUND,
                "text/html",
                true,
            ),
            (
                Method::GET,
                "/article",
                StatusCode::OK,
                "application/json",
                true,
            ),
            (Method::GET, "/article", StatusCode::OK, "text/html", false),
            (
                Method::GET,
                "/assets/image",
                StatusCode::OK,
                "text/html",
                true,
            ),
        ] {
            let fixture = Fixture::defaults(store_available);
            let (response, snapshot) = fixture
                .visit(
                    Case {
                        method,
                        path,
                        status,
                        content_type,
                        document,
                        ..Case::default()
                    },
                    |_| {},
                )
                .await;
            let expected = http::Response::builder()
                .header(header::CONTENT_TYPE, content_type)
                .header(header::CACHE_CONTROL, "public, max-age=120")
                .header("surrogate-control", "max-age=120")
                .body(Body::empty())
                .expect("should build baseline header fixture");
            assert_eq!(
                response.headers(),
                expected.headers(),
                "should retain every baseline response header"
            );
            assert!(
                cookies(&response).is_empty(),
                "should preserve disabled response cookies"
            );
            assert_eq!(response.status(), status, "should preserve status");
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "public, max-age=120",
                "should preserve cache policy"
            );
            assert_eq!(
                response.headers()["surrogate-control"],
                "max-age=120",
                "should preserve edge policy"
            );
            assert_eq!(
                response
                    .into_body()
                    .into_bytes()
                    .expect("should keep buffered body"),
                b"example-response".as_slice(),
                "should preserve origin body"
            );
            assert_eq!(
                snapshot,
                EcKvSnapshot::NotRead,
                "should preserve disabled unread behavior"
            );
            fixture.assert_no_store_io();
        }
        let fixture = Fixture::defaults(store_available);
        let response = fixture
            .app
            .ec_route(&fixture.services, ec_post(Some(EC)))
            .await;
        assert_eq!(
            response.status(),
            if store_available {
                StatusCode::OK
            } else {
                StatusCode::NOT_IMPLEMENTED
            },
            "should preserve existing API status"
        );
        assert!(
            cookies(&response).is_empty(),
            "should preserve disabled API cookies"
        );
        fixture.assert_no_store_io();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn withdrawal_preserves_zero_lifetime_with_sliding_on_or_off() {
    for enabled in [false, true] {
        let fixture = Fixture::new(enabled, false);
        let (response, _) = fixture
            .visit(
                Case {
                    consent: Some(false),
                    ..Case::default()
                },
                |_| {},
            )
            .await;
        let set = cookies(&response);
        assert_eq!(set.len(), 1, "should expire only held EC");
        assert!(
            set[0].starts_with("ts-ec=;") && set[0].contains("Max-Age=0"),
            "should preserve withdrawal deletion"
        );
    }
}

fn assert_new_ec_lifetime(response: &Response) {
    let set = cookies(response);
    let ec = set
        .iter()
        .find(|value| value.starts_with("ts-ec="))
        .expect("should issue EC cookie");
    assert!(
        !ec.starts_with(&format!("ts-ec={EC};")),
        "should issue a fresh identity"
    );
    assert!(
        ec.contains(CUSTOM_MAX_AGE),
        "should apply configured lifetime to new identity"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn publisher_creation_and_ec_post_creation_use_configured_lifetime() {
    for enabled in [false, true] {
        let fixture = Fixture::new(enabled, true);
        let (response, _) = fixture
            .visit(
                Case {
                    ec: None,
                    allow_creation: true,
                    ..Case::default()
                },
                |_| {},
            )
            .await;
        assert_new_ec_lifetime(&response);
        let fixture = Fixture::new(enabled, true);
        let response = fixture.app.ec_route(&fixture.services, ec_post(None)).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should admit creation POST"
        );
        assert_new_ec_lifetime(&response);
    }
}

fn orphan(context: &mut EcContext) {
    known_missing(context);
    context.set_recovery_eligible(true);
}

#[tokio::test(flavor = "current_thread")]
async fn proven_orphan_rotation_uses_configured_lifetime() {
    let fixture = Fixture::new(true, true);
    let (response, _) = fixture.visit(Case::default(), orphan).await;
    assert_new_ec_lifetime(&response);
}

#[test]
fn identity_and_configured_sharedid_cookies_remain_forbidden_template_keys() {
    assert!(
        validate_cookie_names(&["ts-ec".to_owned()], &[]).is_err(),
        "should reject EC as shared template key"
    );
    assert!(
        validate_sharedid_key_cookie(&["publisherIdentity".to_owned()], Some("publisherIdentity"))
            .is_err(),
        "should reject the configured SharedID name as shared template key"
    );
}
