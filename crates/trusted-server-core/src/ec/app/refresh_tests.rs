//! Refresh behavior through publisher authorization and shared response finalization.

use std::net::IpAddr;
use std::sync::Arc;

use edgezero_core::body::Body;
use edgezero_core::http::{Method, Response, StatusCode, header};
use edgezero_core::ingress::{InboundOrigin, OriginSource};
use error_stack::Report;
use serde_json::json;

use super::{IdentityApp, IdentityPlatform};
use crate::ec::coordinator::results::{
    DropCounts, LookupResults, LookupStatus, OptOutMemory, ProviderLookup,
};
use crate::ec::eid_types::{AgentType, EidFields, Expiry, ProviderId, SourceDomain, UidValue};
use crate::ec::kv_backend::EcKvStore as _;
use crate::ec::kv_backend::test_support::{FailingEcKv, InMemoryEcKv, WriteFailingEcKv};
use crate::ec::kv_types::{KvEid, KvEntry, KvUid};
use crate::ec::rate_limiter::NoRateLimit;
use crate::ec::refresh::{encode_marker, should_refresh};
use crate::ec::{EcContext, EcKvSnapshot, current_timestamp};
use crate::http_util::RuntimeRequestOrigin;
use crate::platform::test_support::{NoopConfigStore, NoopSecretStore, noop_services};
use crate::platform::{ClientInfo, GeoInfo, PlatformError, PlatformGeo, RuntimeServices};
use crate::response_privacy::apply_response_headers_with_cache_privacy;
use crate::settings::ProxyAssetRoute;
use crate::test_support::tests::create_test_settings;

const EC: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123";

struct FixtureGeo;
impl PlatformGeo for FixtureGeo {
    fn lookup(&self, _ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(Some(GeoInfo::country_region("US", Some("TN".to_owned()))))
    }
}

struct Fixture {
    app: Arc<IdentityApp>,
    services: RuntimeServices,
    store: Arc<InMemoryEcKv>,
}

impl Fixture {
    async fn new(enabled: bool) -> Self {
        let mut settings = create_test_settings();
        settings.ec.partners.clear();
        settings.publisher.domain = "publisher.example.com".to_owned();
        if enabled {
            let mut ec = serde_json::to_value(&settings.ec).expect("should encode EC settings");
            ec["refresh_after_days"] = json!(1);
            settings.ec = serde_json::from_value(ec).expect("should enable refresh in app config");
        }
        let store = Arc::new(InMemoryEcKv::new("refresh-fixture"));
        let app = IdentityApp::build(
            Arc::new(settings),
            IdentityPlatform {
                record_store: Some(store.clone()),
                rate_limiter: Some(Arc::new(NoRateLimit)),
                providers: None,
            },
        )
        .expect("should build identity app");
        let services = noop_services();
        let services = RuntimeServices::builder()
            .config_store(Arc::new(NoopConfigStore))
            .secret_store(Arc::new(NoopSecretStore))
            .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
            .outbound_client(services.outbound_client().clone())
            .clock(services.clock().clone())
            .geo(Arc::new(FixtureGeo))
            .client_info(ClientInfo {
                client_ip: Some("192.0.2.7".parse().expect("should parse fixture IP")),
                ..ClientInfo::default()
            })
            .build();
        Self {
            app,
            services,
            store,
        }
    }

    async fn seed(&self, live: bool) {
        let mut row = KvEntry::tombstone(current_timestamp());
        row.consent.ok = live;
        self.app
            .identity_graph()
            .expect("should have graph")
            .create(EC, &row)
            .await
            .expect("should seed record");
    }

    async fn visit(&self, case: Case) -> Response {
        let mut request = http::Request::builder()
            .method(case.method)
            .uri(case.path)
            .header(
                header::COOKIE,
                format!(
                    "{}us_privacy={}; {}",
                    case.ec
                        .map(|value| format!("ts-ec={value}; "))
                        .unwrap_or_default(),
                    if case.consent { "1YNN" } else { "1YYN" },
                    case.marker
                ),
            )
            .header(
                "sec-fetch-dest",
                if case.document { "document" } else { "empty" },
            )
            .body(Body::empty())
            .expect("should build request");
        for extra in case.extra_cookie_fields {
            request.headers_mut().append(
                header::COOKIE,
                extra.parse().expect("should encode another Cookie field"),
            );
        }
        self.respond(request, case.status, case.content_type).await
    }

    async fn respond(
        &self,
        request: http::Request<Body>,
        status: StatusCode,
        content_type: &str,
    ) -> Response {
        self.respond_with(request, status, content_type, |_| {})
            .await
    }

    async fn respond_with(
        &self,
        request: http::Request<Body>,
        status: StatusCode,
        content_type: &str,
        update: fn(&mut EcContext),
    ) -> Response {
        let mut identity = self
            .app
            .read_request(&self.services, &request)
            .expect("should read identity");
        self.app
            .prepare_navigation(&mut identity.context, &request, true)
            .await;
        update(&mut identity.context);
        let mut response = http::Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CACHE_CONTROL, "public, max-age=120")
            .header("surrogate-control", "max-age=120")
            .body(Body::empty())
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
        response
    }
}

struct Case {
    ec: Option<&'static str>,
    path: &'static str,
    extra_cookie_fields: Vec<String>,
    marker: String,
    document: bool,
    consent: bool,
    method: Method,
    status: StatusCode,
    content_type: &'static str,
}
impl Default for Case {
    fn default() -> Self {
        Self {
            ec: Some(EC),
            path: "/article",
            extra_cookie_fields: Vec::new(),
            marker: String::new(),
            document: true,
            consent: true,
            method: Method::GET,
            status: StatusCode::OK,
            content_type: "text/html",
        }
    }
}
fn cookies(response: &Response) -> Vec<&str> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().expect("should have cookie text"))
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn enabled_refresh_bootstraps_same_ec_and_marker_then_immediate_visit_is_unchanged() {
    let fixture = Fixture::new(true).await;
    fixture.seed(true).await;
    let before = fixture
        .store
        .lookup(EC)
        .await
        .expect("should read row")
        .expect("should have row");
    let response = fixture.visit(Case::default()).await;
    let set = cookies(&response);
    assert_eq!(set.len(), 2, "should bootstrap both cookies: {set:?}");
    assert!(
        set.iter()
            .any(|v| v.starts_with(&format!("ts-ec={EC};")) && v.contains("Max-Age=31536000")),
        "should refresh same EC with full lifetime"
    );
    let marker = set
        .iter()
        .find(|v| v.starts_with("ts-ec-issued-at="))
        .expect("should set issuance marker")
        .split(';')
        .next()
        .expect("should have pair")
        .to_string();
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, max-age=0",
        "should protect refresh response"
    );
    assert!(
        !response.headers().contains_key("surrogate-control"),
        "should strip edge policy"
    );
    assert!(
        !response.headers().contains_key("x-ts-ec"),
        "should not expose a new identity header"
    );
    let response = fixture
        .visit(Case {
            marker,
            ..Case::default()
        })
        .await;
    assert!(
        cookies(&response).is_empty(),
        "should not immediately refresh again"
    );
    let after = fixture
        .store
        .lookup(EC)
        .await
        .expect("should read row")
        .expect("should keep row");
    assert_eq!(
        after.generation, before.generation,
        "should not add a refresh record write"
    );
    assert_eq!(
        after.body, before.body,
        "should preserve backing record bytes and retention"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn absent_setting_ignores_carried_marker_and_adds_no_record_reads() {
    let fixture = Fixture::new(false).await;
    let failing = Arc::new(FailingEcKv::new("disabled-no-read"));
    // There is no row and reads would fail; the disabled branch must not try renewal.
    let app = IdentityApp::build(
        Arc::clone(fixture.app.settings()),
        IdentityPlatform {
            record_store: Some(failing),
            rate_limiter: Some(Arc::new(NoRateLimit)),
            providers: None,
        },
    )
    .expect("should build disabled app");
    for marker in ["", "ts-ec-issued-at=invalid"] {
        let request = http::Request::builder()
            .uri("/article")
            .header("sec-fetch-dest", "document")
            .header(
                header::COOKIE,
                format!("ts-ec={EC}; us_privacy=1YNN; {marker}"),
            )
            .body(Body::empty())
            .expect("should create request");
        let mut identity = app
            .read_request(&fixture.services, &request)
            .expect("should read identity");
        app.prepare_navigation(&mut identity.context, &request, true)
            .await;
        let mut response = http::Response::builder()
            .header(header::CONTENT_TYPE, "text/html")
            .body(Body::empty())
            .expect("should create response");
        app.finalize(
            &mut identity.context,
            None,
            &identity.sharedid,
            true,
            &mut response,
        )
        .await;
        assert!(
            cookies(&response).is_empty(),
            "should not refresh when setting is absent"
        );
        assert!(
            matches!(
                identity.context.kv_snapshot(),
                crate::ec::EcKvSnapshot::NotRead
            ),
            "should preserve disabled no-read behavior"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn refresh_obeys_document_status_method_and_record_gates() {
    for (status, content_type, document, method, expected) in [
        (StatusCode::OK, "text/html", true, Method::GET, true),
        (
            StatusCode::OK,
            "application/xhtml+xml",
            true,
            Method::POST,
            true,
        ),
        (StatusCode::OK, "text/html", true, Method::HEAD, true),
        (
            StatusCode::NOT_MODIFIED,
            "application/json",
            true,
            Method::GET,
            true,
        ),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "text/html",
            true,
            Method::GET,
            true,
        ),
        (StatusCode::FOUND, "text/html", true, Method::GET, false),
        (
            StatusCode::TEMPORARY_REDIRECT,
            "text/html",
            true,
            Method::GET,
            false,
        ),
        (StatusCode::OK, "application/json", true, Method::GET, false),
        (StatusCode::OK, "text/html", false, Method::GET, false),
    ] {
        let fixture = Fixture::new(true).await;
        fixture.seed(true).await;
        let response = fixture
            .visit(Case {
                status,
                content_type,
                document,
                method,
                ..Case::default()
            })
            .await;
        assert_eq!(
            cookies(&response).len(),
            if expected { 2 } else { 0 },
            "should match document eligibility at {status} {content_type} document={document}"
        );
    }
    for live in [false, true] {
        let fixture = Fixture::new(true).await;
        if !live {
            fixture.seed(false).await;
        }
        let response = fixture.visit(Case::default()).await;
        assert!(
            cookies(&response).is_empty(),
            "should not renew a missing or revoked row"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn withdrawal_expires_companion_even_on_non_document_without_ec() {
    let fixture = Fixture::new(true).await;
    let request = http::Request::builder()
        .uri("/asset")
        .header("sec-fetch-dest", "empty")
        .header(header::COOKIE, "us_privacy=1YYN; ts-ec-issued-at=old")
        .body(Body::empty())
        .expect("should create withdrawal");
    let mut identity = fixture
        .app
        .read_request(&fixture.services, &request)
        .expect("should read identity");
    let mut response = http::Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::empty())
        .expect("should create response");
    fixture
        .app
        .finalize(
            &mut identity.context,
            None,
            &identity.sharedid,
            false,
            &mut response,
        )
        .await;
    assert!(
        cookies(&response)
            .iter()
            .any(|v| v.starts_with("ts-ec-issued-at=;") && v.contains("Max-Age=0")),
        "should delete carried marker even without EC or store"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn enabled_refresh_never_renews_ec_api_even_with_document_headers() {
    let fixture = Fixture::new(true).await;
    fixture.seed(true).await;
    let mut request = http::Request::builder()
        .method(Method::POST)
        .uri("https://publisher.example.com/_ts/api/v1/ec")
        .header(header::ORIGIN, "https://publisher.example.com")
        .header("sec-fetch-dest", "document")
        .header(
            header::COOKIE,
            format!("ts-ec={EC}; us_privacy=1YNN; ts-ec-issued-at=invalid"),
        )
        .body(Body::from(json!({}).to_string()))
        .expect("should build EC API request");
    request.extensions_mut().insert(RuntimeRequestOrigin::new(
        InboundOrigin::parse("https", "publisher.example.com", OriginSource::RuntimeUri)
            .expect("should supply fixture ingress"),
    ));
    let response = fixture.app.ec_route(&fixture.services, request).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "should reach admitted EC API"
    );
    assert!(
        cookies(&response).is_empty(),
        "should not renew returning EC on API"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn enabled_refresh_without_consent_or_readable_store_never_renews() {
    let fixture = Fixture::new(true).await;
    fixture.seed(true).await;
    let request = http::Request::builder()
        .uri("/article")
        .header("sec-fetch-dest", "document")
        .header(
            header::COOKIE,
            format!("ts-ec={EC}; ts-ec-issued-at=invalid"),
        )
        .body(Body::empty())
        .expect("should build no-consent request");
    let mut identity = fixture
        .app
        .read_request(&fixture.services, &request)
        .expect("should read identity");
    fixture
        .app
        .prepare_navigation(&mut identity.context, &request, true)
        .await;
    let mut response = http::Response::builder()
        .header(header::CONTENT_TYPE, "text/html")
        .body(Body::empty())
        .expect("should create response");
    fixture
        .app
        .finalize(
            &mut identity.context,
            None,
            &identity.sharedid,
            true,
            &mut response,
        )
        .await;
    assert!(
        cookies(&response).is_empty(),
        "should leave cookies alone without qualifying consent"
    );
    let app = IdentityApp::build(
        Arc::clone(fixture.app.settings()),
        IdentityPlatform {
            record_store: Some(Arc::new(FailingEcKv::new("enabled-read-failure"))),
            rate_limiter: Some(Arc::new(NoRateLimit)),
            providers: None,
        },
    )
    .expect("should build unavailable app");
    let request = http::Request::builder()
        .uri("/article")
        .header("sec-fetch-dest", "document")
        .header(
            header::COOKIE,
            format!("ts-ec={EC}; us_privacy=1YNN; ts-ec-issued-at=invalid"),
        )
        .body(Body::empty())
        .expect("should create request");
    let mut identity = app
        .read_request(&fixture.services, &request)
        .expect("should read identity");
    app.prepare_navigation(&mut identity.context, &request, true)
        .await;
    let mut response = http::Response::builder()
        .header(header::CONTENT_TYPE, "text/html")
        .body(Body::empty())
        .expect("should create response");
    app.finalize(
        &mut identity.context,
        None,
        &identity.sharedid,
        true,
        &mut response,
    )
    .await;
    assert!(
        cookies(&response).is_empty(),
        "should not refresh after a record read failure"
    );
}

fn marker_at(fixture: &Fixture, issued_at: u64) -> String {
    let settings = fixture.app.settings();
    let value = encode_marker(
        settings.ec.passphrase.expose(),
        &settings.publisher.domain,
        EC,
        issued_at,
    )
    .expect("should sign fixture marker");
    format!("ts-ec-issued-at={value}")
}

#[tokio::test(flavor = "current_thread")]
async fn signed_overdue_marker_renews_and_recent_marker_preserves_public_response() {
    let fixture = Fixture::new(true).await;
    fixture.seed(true).await;
    let now = current_timestamp();
    for (issued_at, expected) in [(now - 86_401, 2), (now - 60, 0)] {
        let response = fixture
            .visit(Case {
                marker: marker_at(&fixture, issued_at),
                ..Case::default()
            })
            .await;
        assert_eq!(
            cookies(&response).len(),
            expected,
            "should honor authenticated issuance age"
        );
        if expected == 0 {
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "public, max-age=120",
                "should preserve cache policy when refresh is not due"
            );
            assert!(
                response.headers().contains_key("surrogate-control"),
                "should preserve edge caching when cookies stay unchanged"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn marker_in_later_cookie_field_is_read_and_duplicate_markers_bootstrap() {
    let fixture = Fixture::new(true).await;
    fixture.seed(true).await;
    let marker = marker_at(&fixture, current_timestamp());
    for (first, later, expected) in [
        (String::new(), vec![marker.clone()], 0),
        (format!("{marker}; {marker}"), Vec::new(), 2),
        (marker.clone(), vec![marker.clone()], 2),
        ("ts-ec-issued-at=invalid".to_owned(), vec![marker], 2),
    ] {
        let response = fixture
            .visit(Case {
                marker: first,
                extra_cookie_fields: later,
                ..Case::default()
            })
            .await;
        assert_eq!(
            cookies(&response).len(),
            expected,
            "should suppress only one unambiguous authenticated marker"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn disabled_refresh_withdrawal_preserves_companion_cookie() {
    let fixture = Fixture::new(false).await;
    fixture.seed(true).await;
    let response = fixture
        .visit(Case {
            consent: false,
            marker: "ts-ec-issued-at=carried".to_owned(),
            ..Case::default()
        })
        .await;
    let set = cookies(&response);
    assert!(
        set.iter()
            .any(|value| value.starts_with("ts-ec=;") && value.contains("Max-Age=0")),
        "should retain existing EC withdrawal"
    );
    assert!(
        !set.iter()
            .any(|value| value.starts_with("ts-ec-issued-at=")),
        "should ignore companion cookies entirely while refresh is disabled"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn configured_asset_get_never_refreshes_even_with_document_html_signals() {
    let mut fixture = Fixture::new(true).await;
    fixture.seed(true).await;
    let mut settings = fixture.app.settings().as_ref().clone();
    settings.proxy.asset_routes.push(ProxyAssetRoute::new(
        "/assets/",
        "https://assets.example.com",
    ));
    fixture.app = IdentityApp::build(
        Arc::new(settings),
        IdentityPlatform {
            record_store: Some(fixture.store.clone()),
            rate_limiter: Some(Arc::new(NoRateLimit)),
            providers: None,
        },
    )
    .expect("should build asset route fixture");
    let response = fixture
        .visit(Case {
            path: "/assets/article",
            marker: "ts-ec-issued-at=invalid".to_owned(),
            ..Case::default()
        })
        .await;
    assert!(
        cookies(&response).is_empty(),
        "should exclude configured asset routes before refresh permission"
    );
}

fn publisher_request(ec: Option<&str>) -> http::Request<Body> {
    http::Request::builder()
        .uri("/article")
        .header("sec-fetch-dest", "document")
        .header(
            header::COOKIE,
            format!(
                "{}us_privacy=1YNN",
                ec.map(|value| format!("ts-ec={value}; "))
                    .unwrap_or_default()
            ),
        )
        .body(Body::empty())
        .expect("should create publisher request")
}

fn authorize_orphan_recovery(context: &mut EcContext) {
    context.set_recovery_eligible(true);
    context.set_kv_snapshot(EcKvSnapshot::Missing {
        ec_id: EC.to_owned(),
    });
}

#[tokio::test(flavor = "current_thread")]
async fn newly_created_and_rotated_ec_cookies_have_bound_issuance_markers() {
    for incoming in [None, Some(EC)] {
        let fixture = Fixture::new(true).await;
        let response = fixture
            .respond_with(
                publisher_request(incoming),
                StatusCode::OK,
                "text/html",
                if incoming.is_some() {
                    authorize_orphan_recovery
                } else {
                    |_| {}
                },
            )
            .await;
        let set = cookies(&response);
        let ec = set
            .iter()
            .find_map(|value| value.strip_prefix("ts-ec="))
            .expect("should issue new backed EC")
            .split(';')
            .next()
            .expect("should extract EC value");
        let marker = set
            .iter()
            .find_map(|value| value.strip_prefix("ts-ec-issued-at="))
            .expect("should stamp issuance marker")
            .split(';')
            .next()
            .expect("should extract marker value");
        assert_ne!(
            ec, EC,
            "should issue a fresh identity for creation or orphan rotation"
        );
        let settings = fixture.app.settings();
        assert!(
            !should_refresh(
                settings.ec.passphrase.expose(),
                &settings.publisher.domain,
                ec,
                Some(marker),
                current_timestamp(),
                86_400
            ),
            "should authenticate the marker against the issued EC"
        );
        assert!(
            fixture
                .store
                .lookup(ec)
                .await
                .expect("should read issued row")
                .is_some(),
            "should back issuance before releasing cookies"
        );
    }
}

fn add_page_load_lookup(context: &mut EcContext) {
    let uid = KvUid::new(
        UidValue::try_from("example-provider-uid").expect("should build synthetic UID"),
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
                SourceDomain::try_from("provider.example.com")
                    .expect("should build provider source"),
                vec![KvEid::new(EidFields::default(), vec![uid])],
            )],
            cleared: Vec::new(),
            opt_out: OptOutMemory::Unchanged,
        }],
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn failed_page_load_write_suppresses_same_ec_refresh() {
    let mut fixture = Fixture::new(true).await;
    fixture.seed(true).await;
    let original = fixture
        .store
        .lookup(EC)
        .await
        .expect("should read seed")
        .expect("should have seed");
    let failing = Arc::new(WriteFailingEcKv::new());
    failing
        .seed(
            EC,
            std::str::from_utf8(&original.body).expect("should decode row"),
            std::str::from_utf8(original.metadata.as_deref().expect("should have metadata"))
                .expect("should decode metadata"),
        )
        .await;
    fixture.app = IdentityApp::build(
        Arc::clone(fixture.app.settings()),
        IdentityPlatform {
            record_store: Some(failing),
            rate_limiter: Some(Arc::new(NoRateLimit)),
            providers: None,
        },
    )
    .expect("should build failing write app");
    let response = fixture
        .respond_with(
            publisher_request(Some(EC)),
            StatusCode::OK,
            "text/html",
            add_page_load_lookup,
        )
        .await;
    assert!(
        cookies(&response).is_empty(),
        "should not renew EC or issuance marker when planned page-load write fails"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn mismatching_identity_header_prevents_cookie_refresh() {
    let fixture = Fixture::new(true).await;
    fixture.seed(true).await;
    let mut request = publisher_request(Some(EC));
    request.headers_mut().insert(
        "x-ts-ec",
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.XYZ789"
            .parse()
            .expect("should encode conflicting identity"),
    );
    let response = fixture.respond(request, StatusCode::OK, "text/html").await;
    assert!(
        cookies(&response).is_empty(),
        "should not renew when header identity disagrees with cookie identity"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ec_get_withdrawal_expires_marker_even_without_record_store() {
    let mut missing_deletions = Vec::new();
    for with_store in [true, false] {
        let mut fixture = Fixture::new(true).await;
        if !with_store {
            fixture.app = IdentityApp::build(
                Arc::clone(fixture.app.settings()),
                IdentityPlatform::default(),
            )
            .expect("should build app without store");
        }
        let mut request = http::Request::builder()
            .uri("https://publisher.example.com/_ts/api/v1/ec")
            .header(header::COOKIE, "us_privacy=1YYN; ts-ec-issued-at=carried")
            .body(Body::empty())
            .expect("should create EC GET withdrawal");
        request.extensions_mut().insert(RuntimeRequestOrigin::new(
            InboundOrigin::parse("https", "publisher.example.com", OriginSource::RuntimeUri)
                .expect("should provide trusted ingress"),
        ));
        let response = fixture.app.ec_route(&fixture.services, request).await;
        if !cookies(&response)
            .iter()
            .any(|value| value.starts_with("ts-ec-issued-at=;") && value.contains("Max-Age=0"))
        {
            missing_deletions.push(with_store);
        }
    }
    assert!(
        missing_deletions.is_empty(),
        "should expire companion on EC GET withdrawal for both storage configurations; missing with_store={missing_deletions:?}"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn refresh_record_read_preserves_unrelated_carried_mirror_cookie() {
    let fixture = Fixture::new(true).await;
    fixture.seed(true).await;
    let mut request = publisher_request(Some(EC));
    request.headers_mut().append(
        header::COOKIE,
        "__Host-ts-eids-mirror=carried"
            .parse()
            .expect("should encode carried mirror"),
    );
    let response = fixture.respond(request, StatusCode::OK, "text/html").await;
    let set = cookies(&response);
    assert!(
        set.iter().any(|value| value.starts_with("ts-ec=")),
        "should bootstrap EC refresh"
    );
    assert!(
        set.iter()
            .any(|value| value.starts_with("ts-ec-issued-at=")),
        "should bootstrap companion marker"
    );
    assert!(
        !set.iter()
            .any(|value| value.starts_with("__Host-ts-eids-mirror=")),
        "should preserve pre-existing unread-snapshot mirror behavior"
    );
}
