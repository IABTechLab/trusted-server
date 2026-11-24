//! EC route provider eligibility through the complete shared application.

use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use base64::Engine as _;
use edgezero_core::body::Body;
use edgezero_core::http::{HeaderValue, Method, StatusCode, header, request_builder};
use edgezero_core::ingress::{InboundOrigin, OriginSource};
use edgezero_core::ja4::Ja4Source;
use error_stack::Report;
use serde_json::json;

use super::{EC_PATH, IdentityApp, IdentityPlatform, IdentityProviders};
use crate::ec::coordinator::{ProviderCoordinator, RegisteredProvider};
use crate::ec::eid_types::SourceDomain;
use crate::ec::kv_backend::EcKvStore;
use crate::ec::kv_backend::test_support::{FailingEcKv, InMemoryEcKv};
use crate::ec::kv_types::KvEntry;
use crate::ec::provider::{
    EidLevelFields, EidShape, IdentityProvider, LookupInput, LookupOutcome, ProviderCapabilities,
    ProviderError, ProviderRequest, ProviderResponse, SourceCapability, SourceInput,
};
use crate::ec::rate_limiter::NoRateLimit;
use crate::ec::registry::PartnerRegistry;
use crate::http_util::RuntimeRequestOrigin;
use crate::platform::test_support::{NoopConfigStore, NoopSecretStore, noop_services};
use crate::platform::{ClientInfo, GeoInfo, PlatformError, PlatformGeo, RuntimeServices};
use crate::test_support::ja4::{
    CHROME_USER_AGENT, CHROMIUM_JA4, PYTHON_JA4, captured_ja4, insert_ja4,
};
use crate::test_support::tests::create_test_settings;

const EXISTING_EC: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.ABC123";

struct CountingProvider {
    capabilities: ProviderCapabilities,
    calls: Arc<AtomicUsize>,
    sends_ec_id: bool,
}

impl IdentityProvider for CountingProvider {
    fn capabilities(&self) -> &ProviderCapabilities {
        &self.capabilities
    }

    fn build_request(
        &self,
        _lookup: &LookupInput<'_>,
    ) -> Result<ProviderRequest, Report<ProviderError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // Count the attempted provider lookup without starting network traffic.
        Err(Report::new(ProviderError::Request))
    }

    fn parse_response<'body>(
        &self,
        _response: ProviderResponse<'body>,
    ) -> Result<LookupOutcome<'body>, Report<ProviderError>> {
        Ok(LookupOutcome::NoMatch)
    }

    // Without the EC ID the provider is eligible on the request that mints
    // the EC, so a fresh grant is a positive control for the refused requests.
    fn sends_ec_id(&self) -> bool {
        self.sends_ec_id
    }
}

struct KnownGeo;

impl PlatformGeo for KnownGeo {
    fn lookup(&self, _ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(Some(GeoInfo::country_region("AU", None)))
    }
}

/// A complete identity app with one counting page provider and its record store.
fn counting_app(
    calls: &Arc<AtomicUsize>,
    sends_ec_id: bool,
) -> (Arc<IdentityApp>, Arc<InMemoryEcKv>) {
    let store = Arc::new(InMemoryEcKv::new("route-provider-gate"));
    (
        counting_app_with_store(calls, sends_ec_id, store.clone()),
        store,
    )
}

/// [`counting_app`] over `store`, with the provider's partner mirrored.
fn counting_app_with_store(
    calls: &Arc<AtomicUsize>,
    sends_ec_id: bool,
    store: Arc<dyn EcKvStore + Send + Sync>,
) -> Arc<IdentityApp> {
    let mut settings = create_test_settings();
    settings.publisher.domain = "publisher.example.com".to_owned();
    settings.ec.partners = serde_json::from_value(json!([{
        "name": "Example provider", "source_domain": "ids.example.com", "bidstream_enabled": true,
        "mirror_enabled": true
    }]))
    .expect("should configure fictional partner");
    let partners =
        Arc::new(PartnerRegistry::from_ec(&settings.ec).expect("should build partner registry"));
    let config = serde_json::from_value(json!({
        "id": "example-provider", "kind": "counter", "sources": ["ids.example.com"],
        "endpoint": "https://ids.example.com/lookup", "allowed_domains": ["ids.example.com"]
    }))
    .expect("should configure counting provider");
    let provider: Box<dyn IdentityProvider> = Box::new(CountingProvider {
        capabilities: ProviderCapabilities {
            sources: Box::new([SourceCapability {
                source: SourceDomain::try_from("ids.example.com")
                    .expect("should parse provider source"),
                input: SourceInput::Anonymous,
            }]),
            shape: EidShape {
                max_eids: NonZeroUsize::new(1).expect("should have one source"),
                max_uids_per_eid: NonZeroUsize::new(1).expect("should have one UID"),
                eid_level_fields: EidLevelFields::Ignored,
            },
        },
        calls: Arc::clone(calls),
        sends_ec_id,
    });
    let page = Arc::new(
        ProviderCoordinator::new(
            vec![RegisteredProvider::new(config, provider)],
            Arc::clone(&partners),
            Duration::from_millis(100),
        )
        .expect("should build enabled page coordinator"),
    );
    IdentityApp::build(
        Arc::new(settings),
        IdentityPlatform {
            record_store: Some(store),
            rate_limiter: Some(Arc::new(NoRateLimit)),
            providers: Some(IdentityProviders {
                partners,
                page: Some(page),
                after_response: None,
            }),
        },
    )
    .expect("should build complete identity application")
}

/// Services with a known location and a routable client IP.
fn located_services() -> RuntimeServices {
    let existing = noop_services();
    RuntimeServices::builder()
        .config_store(Arc::new(NoopConfigStore))
        .secret_store(Arc::new(NoopSecretStore))
        .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
        .outbound_client(existing.outbound_client().clone())
        .clock(existing.clock().clone())
        .geo(Arc::new(KnownGeo))
        .client_info(ClientInfo {
            client_ip: Some("192.0.2.7".parse().expect("should parse fixture IP")),
            ..ClientInfo::default()
        })
        .build()
}

/// An EC route request from a trusted public origin with the given browser verdict.
fn route_request(
    method: Method,
    origin: Option<&str>,
    cookie: Option<&str>,
    ja4: &str,
) -> edgezero_core::http::Request {
    let mut builder = request_builder()
        .method(method)
        .uri(format!("https://publisher.example.com{EC_PATH}"));
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let mut request = builder
        .body(Body::from(json!({}).to_string()))
        .expect("should build EC route request");
    request.extensions_mut().insert(RuntimeRequestOrigin::new(
        InboundOrigin::parse("https", "publisher.example.com", OriginSource::RuntimeUri)
            .expect("should supply trusted fixture ingress"),
    ));
    // A trusted Python JA4 behind a Chrome User-Agent is one ID-7 refuses.
    request.headers_mut().insert(
        header::USER_AGENT,
        HeaderValue::from_static(CHROME_USER_AGENT),
    );
    insert_ja4(
        request.extensions_mut(),
        captured_ja4(ja4, Ja4Source::RuntimeTls),
    );
    request
}

#[tokio::test(flavor = "current_thread")]
async fn ec_route_refused_requests_without_identity_mint_nothing_and_call_no_provider() {
    let same_origin = Some("https://publisher.example.com");
    // Fixture TCF strings are age-checked elsewhere; a non-regulated location
    // lets the control mint without one.
    for (label, method, origin, ja4, mints) in [
        (
            "same-origin control",
            Method::POST,
            same_origin,
            CHROMIUM_JA4,
            true,
        ),
        (
            "cross-site Origin",
            Method::POST,
            Some("https://cross-site.example.com"),
            CHROMIUM_JA4,
            false,
        ),
        ("missing Origin", Method::POST, None, CHROMIUM_JA4, false),
        ("pixel GET", Method::GET, same_origin, CHROMIUM_JA4, false),
        (
            "ID-7 refused client",
            Method::POST,
            same_origin,
            PYTHON_JA4,
            false,
        ),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let (app, store) = counting_app(&calls, false);

        let response = app
            .ec_route(
                &located_services(),
                route_request(method, origin, None, ja4),
            )
            .await;

        let keys = store
            .list_keys_with_prefix("", 10)
            .await
            .expect("should inspect authoritative keys");
        let minted = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|value| {
                value.as_bytes().starts_with(b"ts-ec=") && !value.as_bytes().starts_with(b"ts-ec=;")
            });
        assert_eq!(
            (calls.load(Ordering::SeqCst), keys.len(), minted),
            (usize::from(mints), usize::from(mints), mints),
            "{label}: should call the provider, create a record and set ts-ec only when allowed"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ec_route_refused_client_with_existing_identity_never_calls_page_provider() {
    // A returning identity makes both a provider that sends the EC ID and one
    // that does not eligible, so the refusal must hold for each.
    for (allowed, sends_ec_id) in [(true, false), (false, false), (true, true), (false, true)] {
        let calls = Arc::new(AtomicUsize::new(0));
        let (app, store) = counting_app(&calls, sends_ec_id);
        let graph = app.identity_graph().expect("should have shared graph");
        let mut row = KvEntry::tombstone(crate::ec::current_timestamp());
        row.consent.ok = true;
        graph
            .create(EXISTING_EC, &row)
            .await
            .expect("should seed returning identity");
        let cookie = format!("ts-ec={EXISTING_EC}");
        let request = route_request(
            Method::POST,
            Some("https://publisher.example.com"),
            Some(&cookie),
            if allowed { CHROMIUM_JA4 } else { PYTHON_JA4 },
        );

        let response = app.ec_route(&located_services(), request).await;

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should handle returning EC request"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            usize::from(allowed),
            "should call the provider only for accepted trusted signals \
             (sends_ec_id = {sends_ec_id})"
        );
        assert!(
            !response.headers().contains_key(header::SET_COOKIE),
            "should not issue cookies for an unchanged returning identity"
        );
        assert_eq!(
            store
                .list_keys_with_prefix("", 10)
                .await
                .expect("should inspect authoritative keys"),
            vec![EXISTING_EC.to_owned()],
            "should never create another identity"
        );
        assert_eq!(
            graph
                .get(EXISTING_EC)
                .await
                .expect("should inspect returning row")
                .expect("should retain existing identity")
                .0,
            row,
            "should preserve the seeded identity when the provider returns no data"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ec_route_lists_no_eids_when_it_leaves_a_carried_mirror_untouched() {
    // Arrange: the record cannot be read, so finalize cannot rebuild the
    // mirror and leaves the carried one untouched. The carried value is a
    // well-formed mirror of the mirrored partner, so echoing it would list it.
    let calls = Arc::new(AtomicUsize::new(0));
    let app = counting_app_with_store(&calls, false, Arc::new(FailingEcKv::new("unreadable")));
    let carried = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        json!({ "v": 1, "eids": [{
            "source": "ids.example.com",
            "uids": [{ "id": "carried", "atype": 1, "exp": 1_767_225_600 }]
        }] })
        .to_string(),
    );
    let cookie = format!("ts-ec={EXISTING_EC}; __Host-ts-eids-mirror={carried}");
    let request = route_request(
        Method::POST,
        Some("https://publisher.example.com"),
        Some(&cookie),
        CHROMIUM_JA4,
    );

    // Act
    let response = app.ec_route(&located_services(), request).await;

    // Assert
    let mirror_cookies = response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter(|value| value.as_bytes().starts_with(b"__Host-ts-eids-mirror="))
        .count();
    assert_eq!(
        mirror_cookies, 0,
        "should leave the carried mirror untouched when the record cannot be read"
    );
    let body: serde_json::Value = serde_json::from_slice(
        &response
            .into_body()
            .into_bytes()
            .expect("should hold a buffered body"),
    )
    .expect("should answer with JSON");
    assert_eq!(
        body,
        json!({ "v": 1, "eids": [] }),
        "should list no EIDs rather than echo a mirror it could not rebuild"
    );
}
