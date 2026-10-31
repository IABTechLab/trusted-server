//! Local runtime fixture exercising the production HTTP client implementation.

#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
#[path = "../../src/platform.rs"]
mod platform;

#[cfg(target_arch = "wasm32")]
use std::time::Duration;
#[cfg(target_arch = "wasm32")]
use trusted_server_core::platform::{PlatformHttpClient as _, PlatformHttpRequest};

#[cfg(target_arch = "wasm32")]
#[worker::event(fetch)]
async fn main(
    request: worker::Request,
    _: worker::Env,
    _: worker::Context,
) -> worker::Result<worker::Response> {
    let uri = request.url()?.to_string();
    let result = exercise(&uri)
        .await
        .map_err(|error| worker::Error::RustError(format!("{error:?}")))?;
    worker::Response::ok(result)
}

#[cfg(target_arch = "wasm32")]
async fn exercise(
    uri: &str,
) -> Result<String, error_stack::Report<trusted_server_core::platform::PlatformError>> {
    let parameter = |name: &str| {
        uri.split_once('?').and_then(|(_, query)| {
            query
                .split('&')
                .filter_map(|pair| pair.split_once('='))
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value)
        })
    };
    let upstream = parameter("upstream").expect("should supply local fixture upstream");
    let budget_ms: u64 = parameter("budget_ms")
        .expect("should supply budget")
        .parse()
        .expect("should parse budget");
    let origin_ms: u64 = parameter("origin_ms")
        .expect("should supply origin delay")
        .parse()
        .expect("should parse origin delay");
    let client = platform::CloudflareHttpClient;
    let request = edgezero_core::http::request_builder()
        .uri(upstream)
        .body(edgezero_core::body::Body::empty())
        .expect("should construct fixture request");
    let started = web_time::Instant::now();
    let pending = client
        .send_async(PlatformHttpRequest::new(request, "fixture"))
        .await?;
    worker::Delay::from(Duration::from_millis(origin_ms)).await;
    let collected = client
        .select_until(vec![pending], started + Duration::from_millis(budget_ms))
        .await?;
    let status = match collected {
        Some(result) => {
            let response = result.ready?;
            assert_eq!(
                response.response.body().as_bytes(),
                Some(b"okay".as_slice()),
                "should retain the complete nonempty upstream body"
            );
            "complete"
        }
        None => "timeout",
    };
    Ok(format!("{status}:{}", started.elapsed().as_millis()))
}
