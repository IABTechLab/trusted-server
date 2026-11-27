//! EC route provider eligibility through the complete shared application.

use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use edgezero_core::body::Body;
use edgezero_core::http::{HeaderValue, Method, StatusCode, header, request_builder};
use edgezero_core::ingress::{InboundOrigin, OriginSource};
use edgezero_core::ja4::Ja4Source;
use error_stack::Report;
use serde_json::json;

use super::{EC_PATH, IdentityApp, IdentityPlatform, IdentityProviders};
use crate::ec::coordinator::{ProviderCoordinator, RegisteredProvider};
use crate::ec::eid_types::SourceDomain;
use crate::ec::kv_backend::EcKvStore as _;
use crate::ec::kv_backend::test_support::InMemoryEcKv;
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
}

struct KnownGeo;

impl PlatformGeo for KnownGeo {
    fn lookup(&self, _ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(Some(GeoInfo::country_region("AU", None)))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ec_route_refused_client_with_existing_identity_never_calls_page_provider() {
    for allowed in [true, false] {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut settings = create_test_settings();
        settings.publisher.domain = "publisher.example.com".to_owned();
        settings.ec.partners = serde_json::from_value(json!([{
            "name": "Example provider", "source_domain": "ids.example.com", "bidstream_enabled": true
        }]))
        .expect("should configure fictional partner");
        let partners = Arc::new(
            PartnerRegistry::from_ec(&settings.ec).expect("should build partner registry"),
        );
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
            calls: Arc::clone(&calls),
        });
        let page = Arc::new(
            ProviderCoordinator::new(
                vec![RegisteredProvider::new(config, provider)],
                Arc::clone(&partners),
                Duration::from_millis(100),
            )
            .expect("should build enabled page coordinator"),
        );
        let store = Arc::new(InMemoryEcKv::new("route-provider-gate"));
        let app = IdentityApp::build(
            Arc::new(settings),
            IdentityPlatform {
                record_store: Some(store.clone()),
                rate_limiter: Some(Arc::new(NoRateLimit)),
                providers: Some(IdentityProviders {
                    partners,
                    page: Some(page),
                    after_response: None,
                }),
            },
        )
        .expect("should build complete identity application");
        let graph = app.identity_graph().expect("should have shared graph");
        let mut row = KvEntry::tombstone(crate::ec::current_timestamp());
        row.consent.ok = true;
        graph
            .create(EXISTING_EC, &row)
            .await
            .expect("should seed returning identity");
        let existing = noop_services();
        let services = RuntimeServices::builder()
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
            .build();
        let mut request = request_builder()
            .method(Method::POST)
            .uri(format!("https://publisher.example.com{EC_PATH}"))
            .header(header::ORIGIN, "https://publisher.example.com")
            .header(header::COOKIE, format!("ts-ec={EXISTING_EC}"))
            .body(Body::from(json!({}).to_string()))
            .expect("should build same-origin returning EC request");
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
            captured_ja4(
                if allowed { CHROMIUM_JA4 } else { PYTHON_JA4 },
                Ja4Source::RuntimeTls,
            ),
        );

        let response = app.ec_route(&services, request).await;

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should handle returning EC request"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            usize::from(allowed),
            "should call the provider only for accepted trusted signals"
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
