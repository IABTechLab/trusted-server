//! The actual application handler path, with only outbound HTTP and KV stubbed.

use std::time::Duration;

use bytes::Bytes;
use edgezero_core::body::Body;
use edgezero_core::http::request_builder;
use edgezero_core::key_value_store::{KvError, KvPage};
use trusted_server_core::platform::PlatformKvStore;
use trusted_server_core::test_support::nextjs_auction;

use super::*;
use crate::lab_public_origin::LabPublicOrigin;

struct EmptyKv;

#[async_trait(?Send)]
impl PlatformKvStore for EmptyKv {
    async fn get_bytes(&self, _key: &str) -> Result<Option<Bytes>, KvError> {
        Ok(None)
    }

    async fn put_bytes(&self, _key: &str, _value: Bytes) -> Result<(), KvError> {
        Err(KvError::Unavailable)
    }

    async fn put_bytes_with_ttl(
        &self,
        _key: &str,
        _value: Bytes,
        _ttl: Duration,
    ) -> Result<(), KvError> {
        Err(KvError::Unavailable)
    }

    async fn delete(&self, _key: &str) -> Result<(), KvError> {
        Err(KvError::Unavailable)
    }

    async fn list_keys_page(
        &self,
        _prefix: &str,
        _cursor: Option<&str>,
        _limit: usize,
    ) -> Result<KvPage, KvError> {
        Err(KvError::Unavailable)
    }
}

fn router(fixed_origin: Option<&str>) -> RouterService {
    let mut settings = nextjs_auction::settings();
    settings.auction.enabled = false;
    settings.creative_opportunities = None;
    let services =
        nextjs_auction::services(Arc::new(nextjs_auction::NextJsAuctionOrigin::default()))
            .with_kv_store(Arc::new(EmptyKv));
    let mut state = build_state_with_services(settings, Some(services))
        .expect("should build application state");
    let state_mut = Arc::get_mut(&mut state).expect("should uniquely own test state");
    state_mut.public_origin =
        LabPublicOrigin::from_value(fixed_origin, &state_mut.settings.publisher.domain)
            .expect("should validate startup origin");
    state_mut.config_sha256 = Some("fixture-adopted-checksum".to_owned());
    build_router(Some(state))
}

async fn text(response: Response) -> String {
    assert_eq!(response.status(), StatusCode::OK);
    String::from_utf8(
        response
            .into_body()
            .into_bytes_bounded(8 * 1024 * 1024)
            .await
            .expect("should read application response")
            .to_vec(),
    )
    .expect("should return UTF-8")
}

#[tokio::test]
async fn fixed_origin_reaches_real_publisher_rewriting_and_same_diagnostic_facts() {
    let router = router(Some("https://test-publisher.example.com"));
    let request = request_builder()
        .uri("/article")
        .header("host", nextjs_auction::PUBLISHER_HOST)
        .header("forwarded", "host=evil.example.com;proto=http")
        .header("x-forwarded-host", "evil.example.com")
        .header("x-forwarded-proto", "http")
        .body(Body::empty())
        .expect("should build publisher request");
    let body = text(
        router
            .oneshot(request)
            .await
            .expect("should handle publisher request"),
    )
    .await;
    assert!(
        body.contains("https://test-publisher.example.com"),
        "{body}"
    );
    assert!(
        body.contains("1:T41,"),
        "should adjust rewritten RSC length"
    );
    assert!(!body.contains("http://test-publisher.example.com"));
    assert!(!body.contains("evil.example.com"));

    let request = request_builder()
        .uri(crate::lab_store_check::PATH)
        .header("host", nextjs_auction::PUBLISHER_HOST)
        .header("authorization", "Basic YWRtaW46YWRtaW4tcGFzcw==")
        .header("forwarded", "host=evil.example.com;proto=http")
        .body(Body::empty())
        .expect("should build diagnostic request");
    let body = text(
        router
            .oneshot(request)
            .await
            .expect("should handle diagnostic"),
    )
    .await;
    let diagnostic: serde_json::Value =
        serde_json::from_str(&body).expect("should parse diagnostic");
    assert_eq!(diagnostic["scheme"], "https");
    assert_eq!(diagnostic["host"], nextjs_auction::PUBLISHER_HOST);
    assert_eq!(diagnostic["config_sha256"], "fixture-adopted-checksum");
}

#[tokio::test]
async fn absent_fixed_origin_preserves_existing_http_rewriting() {
    let request = request_builder()
        .uri("/article")
        .header("host", nextjs_auction::PUBLISHER_HOST)
        .body(Body::empty())
        .expect("should build default request");
    let body = text(
        router(None)
            .oneshot(request)
            .await
            .expect("should handle default request"),
    )
    .await;
    assert!(body.contains("http://test-publisher.example.com"), "{body}");
    assert!(body.contains("1:T40,"), "should retain existing RSC length");
}

#[tokio::test]
async fn internal_health_host_is_exempt_but_application_authorities_are_not() {
    let router = router(Some("https://test-publisher.example.com"));
    for (path, expected) in [
        ("/health", StatusCode::OK),
        ("/article", StatusCode::BAD_REQUEST),
        ("/static/tsjs=tsjs-unified.min.js", StatusCode::BAD_REQUEST),
    ] {
        let request = request_builder()
            .uri(path)
            .header("host", "127.0.0.1:8787")
            .body(Body::empty())
            .expect("should build internal-host request");
        let response = router
            .oneshot(request)
            .await
            .expect("should handle internal host");
        assert_eq!(
            response.status(),
            expected,
            "should scope the exemption to health"
        );
    }
}

#[tokio::test]
async fn hostile_authorities_are_rejected_in_actual_handler_path() {
    let router = router(Some("https://test-publisher.example.com"));
    for (host, target) in [
        ("evil.example.com", "/article"),
        (
            nextjs_auction::PUBLISHER_HOST,
            "https://evil.example.com/article",
        ),
        (
            "evil.example.com",
            "https://test-publisher.example.com/article",
        ),
    ] {
        let request = request_builder()
            .uri(target)
            .header("host", host)
            .header("x-forwarded-host", nextjs_auction::PUBLISHER_HOST)
            .header("x-forwarded-proto", "https")
            .body(Body::empty())
            .expect("should build hostile request");
        let response = router
            .oneshot(request)
            .await
            .expect("should reject hostile authority locally");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
