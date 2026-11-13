//! Local runtime fixture exercising `EdgeZero`'s Spin HTTP client.
//!
//! `scripts/test-worker-auction-deadlines.mjs` calls it with a delayed
//! upstream. It starts one buffered request in a batch, sleeps for the
//! simulated publisher work, then collects the batch at the auction cutoff.

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use edgezero_adapter_spin::outbound::SpinOutboundClient;
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use edgezero_core::error::EdgeError;
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use edgezero_core::http::Method;
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use edgezero_core::outbound::{
    HttpClient, OutboundBatchTermination, OutboundCachePolicy, OutboundCollectedSlot,
    ResponseDecoding,
};
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use edgezero_core::time::Deadline;
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use trusted_server_core::outbound::{BUFFERED_TIMEOUT, buffered, outbound_request};

/// Response cap the script's oversized upstream exceeds by one byte.
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
const RESPONSE_CAP_BYTES: u64 = 8 * 1024 * 1024;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
#[spin_sdk::http_service]
async fn handle(request: spin_sdk::http::Request) -> Result<String, spin_sdk::http::Error> {
    let result = exercise(&request.uri().to_string())
        .await
        .map_err(|error| format!("{error:?}"))?;
    Ok(result)
}

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
async fn exercise(uri: &str) -> Result<String, EdgeError> {
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
    let client = HttpClient::with_client(SpinOutboundClient::new());
    let request = buffered(
        outbound_request(Method::GET, upstream)?,
        BUFFERED_TIMEOUT,
        RESPONSE_CAP_BYTES,
        ResponseDecoding::Decode,
    )
    .cache_policy(OutboundCachePolicy::Bypass);
    let started = web_time::Instant::now();
    let batch = client.start_batch_until(
        vec![request],
        Deadline::after(Duration::from_millis(budget_ms)),
    );
    spin_sdk::time::sleep(Duration::from_millis(origin_ms)).await;
    let (slots, termination) = batch
        .collect()
        .await
        .map_err(|failure| failure.into_parts().0)?
        .into_parts();
    let status = match (slots.into_iter().next(), termination) {
        (Some(OutboundCollectedSlot::Resolved(result)), _) => match result.outcome {
            Ok(response) => {
                assert_eq!(
                    response.body().as_bytes(),
                    Some(b"okay".as_slice()),
                    "should retain the complete nonempty upstream body"
                );
                "complete"
            }
            Err(EdgeError::ResponseTooLarge { .. }) => "oversized",
            Err(error) => return Err(error),
        },
        (Some(OutboundCollectedSlot::Unresolved), OutboundBatchTermination::Cutoff) => "timeout",
        (slot, termination) => {
            return Err(EdgeError::internal(std::io::Error::other(format!(
                "unexpected batch outcome {slot:?} with {termination:?}"
            ))));
        }
    };
    Ok(format!("{status}:{}", started.elapsed().as_millis()))
}
