//! Local runtime fixture exercising the production HTTP client implementation.

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
#[allow(dead_code)]
#[path = "../../src/platform.rs"]
mod platform;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use std::time::Duration;
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use trusted_server_core::platform::{PlatformHttpClient as _, PlatformHttpRequest};

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
#[spin_sdk::http_service]
async fn handle(request: spin_sdk::http::Request) -> Result<String, spin_sdk::http::Error> {
    let result = exercise(&request.uri().to_string())
        .await
        .map_err(|error| format!("{error:?}"))?;
    Ok(result)
}

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
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
    let client = platform::SpinPlatformHttpClient;
    let request = edgezero_core::http::request_builder()
        .uri(upstream)
        .body(edgezero_core::body::Body::empty())
        .expect("should construct fixture request");
    let started = web_time::Instant::now();
    let pending = client
        .send_async(PlatformHttpRequest::new(request, "fixture"))
        .await?;
    spin_sdk::time::sleep(Duration::from_millis(origin_ms)).await;
    let collected = client
        .select_until(vec![pending], started + Duration::from_millis(budget_ms))
        .await?;
    let status = match collected {
        Some(result) => match result.ready {
            Ok(response) => {
                assert_eq!(
                    response.response.body().as_bytes(),
                    Some(b"okay".as_slice()),
                    "should retain the complete nonempty upstream body"
                );
                "complete"
            }
            Err(error) if format!("{error:?}").contains("length limit exceeded") => {
                assert_eq!(
                    result.failed_backend_name.as_deref(),
                    Some("fixture"),
                    "should attribute oversized response to its provider"
                );
                "oversized"
            }
            Err(error) => return Err(error),
        },
        None => "timeout",
    };
    Ok(format!("{status}:{}", started.elapsed().as_millis()))
}
