//! Fictional standard-profile executor compiled only for automated tests.

use edgezero_core::body::Body as EdgeBody;
use error_stack::{Report, ResultExt as _};
use http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};

use super::{apply_notification_policy, extract_standard_response, unused_bidder_params_count};
use crate::auction::plan::ProviderPlan;
use crate::auction::routing::ProviderAuctionInput;
use crate::auction::types::AuctionResponse;
use crate::error::TrustedServerError;
use crate::platform::{PlatformBackend, PlatformHttpClient, PlatformHttpRequest};

const MAX_STANDARD_RESPONSE_BYTES: usize = 1024 * 1024;

/// Execute one fictional standard-profile request through a supplied test client.
///
/// The HTTP client receives exactly one request. Redirect statuses are
/// classified as the original provider error and never followed.
pub(super) async fn execute_standard_fixture(
    provider: &ProviderPlan,
    input: &ProviderAuctionInput,
    request: &trusted_server_openrtb::BidRequest,
    backend: &dyn PlatformBackend,
    http_client: &dyn PlatformHttpClient,
) -> Result<AuctionResponse, Report<TrustedServerError>> {
    let spec = provider.backend_spec();
    let predicted_name =
        backend
            .predict_name(&spec)
            .change_context(TrustedServerError::Auction {
                message: "Failed to predict fictional standard backend".to_string(),
            })?;
    let backend_name = backend
        .ensure(&spec)
        .change_context(TrustedServerError::Auction {
            message: "Failed to ensure fictional standard backend".to_string(),
        })?;
    if backend_name != predicted_name {
        return Err(Report::new(TrustedServerError::Auction {
            message: "Fictional standard backend ensure did not match prediction".to_string(),
        }));
    };
    let body = serde_json::to_vec(request).change_context(TrustedServerError::Auction {
        message: "Failed to serialize fictional standard request".to_string(),
    })?;
    let outbound = Request::builder()
        .method(Method::POST)
        .uri(provider.endpoint.as_str())
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json")
        .body(EdgeBody::from(body))
        .change_context(TrustedServerError::Auction {
            message: "Failed to build fictional standard request".to_string(),
        })?;
    let response = http_client
        .send(PlatformHttpRequest::new(outbound, backend_name))
        .await
        .change_context(TrustedServerError::Auction {
            message: "Fictional standard transport failed".to_string(),
        })?
        .response;
    let status = response.status();
    if status == StatusCode::NO_CONTENT {
        return Ok(
            AuctionResponse::no_bid(provider.id.as_str(), 0).with_metadata(
                "routing",
                json!({"unused_bidder_params_count": unused_bidder_params_count(&provider.profile, input)}),
            ),
        );
    }
    if !status.is_success() {
        return Ok(AuctionResponse::error(provider.id.as_str(), 0)
            .with_metadata("http_status", json!(status.as_u16()))
            .with_metadata(
                "routing",
                json!({"unused_bidder_params_count": unused_bidder_params_count(&provider.profile, input)}),
            ));
    }
    let body = response
        .into_body()
        .into_bytes_bounded(MAX_STANDARD_RESPONSE_BYTES)
        .await
        .change_context(TrustedServerError::Auction {
            message: "Fictional standard response exceeded its limit".to_string(),
        })?;
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return Ok(
                AuctionResponse::error(provider.id.as_str(), 0).with_metadata(
                    "routing",
                    json!({"unused_bidder_params_count": unused_bidder_params_count(&provider.profile, input)}),
                ),
            );
        }
    };
    let mut parsed = extract_standard_response(provider.id.as_str(), input, &value, 0);
    apply_notification_policy(&mut parsed.bids, &provider.notifications);
    parsed.metadata.insert(
        "routing".to_string(),
        json!({"unused_bidder_params_count": unused_bidder_params_count(&provider.profile, input)}),
    );
    Ok(parsed)
}
