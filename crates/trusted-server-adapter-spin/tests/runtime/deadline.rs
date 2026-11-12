//! Local fixture using the production request-local EdgeZero client and driver.
#![cfg(all(feature = "spin", target_arch = "wasm32"))]

use std::time::Duration;

use edgezero_core::app::Hooks;
use edgezero_core::body::Body;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::Response;
use edgezero_core::router::RouterService;
use trusted_server_core::platform::{
    AuctionTargetId, EdgeZeroHttpClient, PlatformError, PlatformHttpRequest,
};

struct DeadlineFixture;

impl Hooks for DeadlineFixture {
    fn routes() -> Result<RouterService, EdgeError> {
        Ok(RouterService::builder()
            .get("/", |context: RequestContext| async move {
                exercise(context)
                    .await
                    .map(|result| Response::new(Body::from(result)))
                    .map_err(|_| EdgeError::bad_gateway("deadline fixture exchange fails"))
            })
            .build())
    }
}

#[spin_sdk::http_service]
async fn handle(
    request: spin_sdk::http::Request,
) -> Result<edgezero_adapter_spin::SpinResponse, spin_sdk::http::Error> {
    edgezero_adapter_spin::run_app::<DeadlineFixture>(request)
        .await
        .map_err(|_| spin_sdk::http::Error::from("deadline fixture dispatch fails"))
}

async fn exercise(context: RequestContext) -> Result<String, error_stack::Report<PlatformError>> {
    let uri = context.uri().to_string();
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
    let client = EdgeZeroHttpClient::for_request(&context, AuctionTargetId::Spin);
    let request = edgezero_core::http::request_builder()
        .uri(upstream)
        .body(Body::empty())
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
            Err(error)
                if matches!(
                    error.downcast_ref::<EdgeError>(),
                    Some(EdgeError::ResponseTooLarge { .. })
                ) =>
            {
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
