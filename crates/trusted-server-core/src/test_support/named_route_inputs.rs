//! Shared route-input regressions for adapter test suites.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use edgezero_core::body::Body;
use edgezero_core::http::{StatusCode, request_builder};
use edgezero_core::router::RouterService;
use futures::StreamExt as _;

use crate::proxy::SIGN_MAX_BODY_BYTES;
use crate::settings::Settings;

/// Returns fictional settings for the local first-party signing route.
///
/// # Panics
/// Panics if the fixed test configuration becomes invalid.
///
/// # Examples
/// ```ignore
/// let settings = sign_settings();
/// ```
#[must_use]
pub fn sign_settings() -> Settings {
    Settings::from_toml(
        r#"
        [[handlers]]
        path = "^/_ts/admin"
        username = "example-user"
        password = "example-password"
        [publisher]
        domain = "publisher.example.com"
        cookie_domain = ".publisher.example.com"
        origin_url = "https://origin.example.com"
        proxy_secret = "fictional-proxy-signing-secret-0123456789"
        [ec]
        passphrase = "fictional-ec-passphrase-32-bytes-minimum"
        [rewrite]
        include_domains = ["asset.example.com"]
        [proxy]
        allowed_domains = ["asset.example.com"]
        [integrations.nextjs]
        enabled = false
    "#,
    )
    .expect("should parse fictional signing settings")
}

/// Asserts streamed signing input is buffered once under its existing byte cap.
///
/// # Panics
/// Panics if routing fails, valid JSON is rejected, oversized JSON is accepted,
/// or an overflowing frame causes another input frame to be read.
///
/// # Examples
/// ```ignore
/// assert_sign_inputs(&router).await;
/// ```
pub async fn assert_sign_inputs(router: &RouterService) {
    let valid = Body::stream(futures::stream::iter([
        Bytes::from_static(b"{\"url\":\"https://asset."),
        Bytes::from_static(b"example.com/image.png\"}"),
    ]));
    let request = request_builder()
        .method("POST")
        .uri("https://publisher.example.com/first-party/sign")
        .header("content-type", "application/json")
        .body(valid)
        .expect("should construct a streamed signing request");
    let response = router
        .oneshot(request)
        .await
        .expect("should route streamed signing input");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "should parse valid JSON from the admitted guarded input"
    );

    let oversized = Bytes::from(format!(
        "{{\"url\":\"https://asset.example.com/{}\"}}",
        "a".repeat(SIGN_MAX_BODY_BYTES)
    ));
    let count = Arc::new(AtomicUsize::new(0));
    let reads = Arc::clone(&count);
    let body = Body::stream(
        futures::stream::iter([oversized, Bytes::from_static(b" ")]).inspect(move |_| {
            reads.fetch_add(1, Ordering::SeqCst);
        }),
    );
    let request = request_builder()
        .method("POST")
        .uri("https://publisher.example.com/first-party/sign")
        .header("content-type", "application/json")
        .body(body)
        .expect("should construct oversized streamed JSON");
    let response = router
        .oneshot(request)
        .await
        .expect("should route the bounded rejection");
    assert_eq!(
        response.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "should enforce the existing signing cap before JSON handling"
    );
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "should not read another frame after crossing the cap"
    );
}
