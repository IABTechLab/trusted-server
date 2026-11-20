//! Portable identity scenarios through the real Axum router and finalizer.

use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use edgezero_core::body::Body;
use edgezero_core::http::{Method, Request, StatusCode, header, request_builder};
use edgezero_core::ingress::{InboundOrigin, OriginSource};
use error_stack::Report;
use futures::executor::block_on;
use trusted_server_core::http_util::RuntimeRequestOrigin;
use trusted_server_core::platform::{
    ClientInfo, GeoInfo, PlatformConfigStore, PlatformError, PlatformGeo, PlatformSecretStore,
    RuntimeServices, StoreId, StoreName, UnavailableKvStore,
};
use trusted_server_core::settings::Settings;
use trusted_server_core::test_support::identity_conformance::{
    Adapter, ApiObservation, ApiRequest, Consent, Geo, Navigation, Observation, RecordingStore,
    Scenario, configure_http_fixture, tcf_consent_cookie, verify_failed_ec_creation,
    verify_http_contract,
};
use trusted_server_core::test_support::scripted_client::{
    ScriptedHttpClient, ScriptedReply, TestClock,
};

use crate::app::TrustedServerApp;

const DOCUMENT: &str = "<html><head></head><body>Example article</body></html>";
const ORIGIN: &str = "https://origin.example.com/article";
const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/146.0.0.0 Safari/537.36";

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
fn settings() -> Settings {
    let mut settings = Settings::from_toml(
        r#"
        [[handlers]]
        path = "^/_ts/admin"
        username = "example-admin"
        password = "example-password"
        [publisher]
        domain = "publisher.example.com"
        cookie_domain = ".publisher.example.com"
        origin_url = "https://origin.example.com"
        proxy_secret = "example-proxy-secret"
        [ec]
        passphrase = "example-secret-at-least-32-bytes-long"
        [request_signing]
        enabled = false
        config_store_id = "example-config"
        secret_store_id = "example-secrets"
        "#,
    )
    .expect("should parse conformance settings");
    configure_http_fixture(&mut settings);
    settings
}

struct FixtureGeo(Geo);

impl PlatformGeo for FixtureGeo {
    fn lookup(&self, _: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        match self.0 {
            Geo::Gdpr => Ok(Some(GeoInfo::country_region("DE", None))),
            Geo::UsCalifornia => Ok(Some(GeoInfo::country_region("US", Some("CA".to_owned())))),
            Geo::Missing => Ok(None),
            Geo::Failed => Err(Report::new(PlatformError::Geo)),
        }
    }
}

struct AxumAdapter {
    store: RecordingStore,
}

impl Adapter for AxumAdapter {
    fn navigate(&mut self, navigation: &Navigation) -> Observation {
        self.execute(navigation, None).response
    }

    fn exchange(&mut self, request: &ApiRequest) -> ApiObservation {
        self.execute(
            &Navigation {
                geo: request.geo,
                ..Navigation::default()
            },
            Some(request),
        )
    }
}

impl AxumAdapter {
    fn execute(&mut self, navigation: &Navigation, api: Option<&ApiRequest>) -> ApiObservation {
        let clock = TestClock::new();
        let origin = ScriptedHttpClient::new(&clock);
        origin.respond_to(
            api.map_or(Method::GET, |request| request.method.clone()),
            &api.map_or_else(
                || ORIGIN.to_owned(),
                |request| format!("https://origin.example.com{}", request.path),
            ),
            {
                let reply = ScriptedReply::status(
                    api.map_or(StatusCode::OK, |request| request.origin_status),
                );
                let mime = api.map_or("text/html", |request| request.origin_content_type);
                let reply = if mime.is_empty() {
                    reply
                } else {
                    reply.header("content-type", mime)
                };
                reply.body(DOCUMENT)
            },
        );
        let services = RuntimeServices::builder()
            .config_store(Arc::new(UnusedStores))
            .secret_store(Arc::new(UnusedStores))
            .kv_store(Arc::new(UnavailableKvStore))
            .outbound_client(origin.http_client())
            .clock(clock.monotonic_clock())
            .geo(Arc::new(FixtureGeo(navigation.geo)))
            .client_info(ClientInfo {
                client_ip: Some("192.0.2.7".parse().expect("should parse fixture IP")),
                ..ClientInfo::default()
            })
            .build();
        let mut configured = settings();
        if let Some(api) = api {
            configured.auction.enabled = api.auction_enabled;
        }
        let router = if api.is_some_and(|request| !request.store_enabled) {
            TrustedServerApp::routes_with_settings_and_services(configured, services)
        } else {
            TrustedServerApp::routes_with_settings_services_and_ec_store(
                configured,
                services,
                self.store.clone(),
            )
        }
        .expect("should build the Axum conformance router");
        let mut cookies = Vec::new();
        match navigation.consent {
            Consent::Granted => cookies.push(tcf_consent_cookie(true)),
            Consent::Denied => cookies.push(tcf_consent_cookie(false)),
            Consent::Missing => {}
        }
        if let Some(identity) = &navigation.ec_cookie {
            cookies.push(format!("ts-ec={identity}"));
        }
        let document = navigation.destination == "document";
        // Native has no trusted JA4. Every scenario exercises the shared
        // missing-JA4 policy with a trusted IP and real parsed consent.
        let request: Request = request_builder()
            .method(Method::GET)
            .uri("https://publisher.example.com/article")
            .header("sec-fetch-dest", navigation.destination)
            .header(
                "sec-fetch-mode",
                if document { "navigate" } else { "no-cors" },
            )
            .header(header::ACCEPT, if document { "text/html" } else { "*/*" })
            .header(header::USER_AGENT, USER_AGENT)
            .header(header::COOKIE, cookies.join("; "))
            .body(Body::empty())
            .expect("should build conformance navigation");
        let mut request = if let Some(api) = api {
            let mut builder = request_builder()
                .method(api.method.clone())
                .uri(format!("https://publisher.example.com{}", api.path))
                .header(header::USER_AGENT, USER_AGENT);
            for (name, value) in &api.headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            builder
                .body(Body::from(api.body.clone()))
                .expect("should build API conformance request")
        } else {
            request
        };
        // This router seam bypasses native ingress; supply only its validated
        // public origin, without inventing TLS or JA4 fingerprints.
        request.extensions_mut().insert(RuntimeRequestOrigin::new(
            InboundOrigin::parse("https", "publisher.example.com", OriginSource::RuntimeUri)
                .expect("should validate fixture public origin"),
        ));
        let response = block_on(router.oneshot(request)).expect("should dispatch the real router");
        let cache_control = response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        self.store.response_released();
        let status = response.status();
        let cookies = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| {
                value
                    .to_str()
                    .expect("should decode response cookie")
                    .to_owned()
            })
            .collect();
        let body = block_on(response.into_body().into_bytes_bounded(1024 * 1024))
            .expect("should consume the complete publisher response")
            .to_vec();
        ApiObservation {
            response: Observation {
                status,
                cookies,
                body,
            },
            origin_requests: origin.requests().len(),
            cache_control,
        }
    }
}

#[test]
fn identity_conformance_axum() {
    for scenario in Scenario::NAVIGATION_SCENARIOS {
        let store = RecordingStore::new(scenario == Scenario::WriteFailure);
        let mut adapter = AxumAdapter {
            store: store.clone(),
        };
        log::info!("Running Axum identity scenario {scenario:?}");
        scenario.verify(&mut adapter, &store);
    }
    let store = RecordingStore::new(false);
    let mut adapter = AxumAdapter {
        store: store.clone(),
    };
    verify_http_contract(&mut adapter, &store);
    let store = RecordingStore::new(true);
    let mut adapter = AxumAdapter {
        store: store.clone(),
    };
    verify_failed_ec_creation(&mut adapter, &store);
}
