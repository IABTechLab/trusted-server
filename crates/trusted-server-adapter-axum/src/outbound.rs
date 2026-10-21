//! Compatibility bridge from TS HTTP operations to the request-scoped RC client.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use edgezero_core::body::{Body, BodyContent};
use edgezero_core::http::header;
use edgezero_core::outbound::{
    HttpClient, OutboundBatch, OutboundBatchTermination, OutboundCachePolicy, OutboundRequest,
    OutboundResponse,
};
use edgezero_core::time::{Deadline, MonotonicClock};
use error_stack::{Report, ResultExt as _};
use futures::StreamExt as _;
use futures::future::{FutureExt as _, LocalBoxFuture, select_all};
use trusted_server_core::platform::{
    PlatformError, PlatformHttpClient, PlatformHttpRequest, PlatformPendingRequest,
    PlatformResponse, PlatformSelectResult,
};

/// TS HTTP client backed exclusively by the RC client's transport and policies.
///
/// RC owns decoding and updates representation headers to match its decoded body.
/// Core must not decode it again. Redirects and origin authorization remain in TS
/// core; the RC native transport does not automatically follow redirects.
/// Remove this bridge when TS adopts the RC outbound contract directly.
pub struct AxumPlatformHttpClient {
    client: HttpClient,
    clock: MonotonicClock,
}

impl AxumPlatformHttpClient {
    /// Build the bridge using the incoming request's client and application clock.
    #[must_use]
    pub fn new(client: HttpClient, clock: MonotonicClock) -> Self {
        Self { client, clock }
    }

    fn prepare(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<(OutboundRequest, String, u64, Deadline), Report<PlatformError>> {
        if request.image_optimizer.is_some() {
            return Err(http_error("native Image Optimizer is unavailable"));
        }
        let timeout_deadline = self
            .clock
            .deadline_after(request.timeout.unwrap_or(Duration::from_secs(30)));
        let deadline = request.deadline.map_or(timeout_deadline, |caller| {
            Deadline::at_instant(caller.instant().min(timeout_deadline.instant()))
        });
        // TS has already constructed its outbound head, including any configured
        // publisher virtual-host override. RC from_request drops Host, so carry
        // that explicit outbound value through RC's checked authority API.
        let mut hosts = request.request.headers().get_all(header::HOST).iter();
        let host = hosts
            .next()
            .map(|value| value.to_str().map(str::to_owned))
            .transpose()
            .change_context(PlatformError::HttpClient)?;
        if hosts.next().is_some() {
            return Err(http_error(
                "native outbound request has multiple Host values",
            ));
        }
        let target = request.request.uri().clone();
        let mut outbound = OutboundRequest::from_request(request.request, target)
            .change_context(PlatformError::HttpClient)?
            .deadline(deadline)
            .max_request_body_bytes(request.max_request_body_bytes)
            .max_encoded_response_bytes(request.max_response_body_bytes)
            .max_decoded_response_bytes(request.max_response_body_bytes)
            .max_response_bytes(request.max_response_body_bytes)
            .max_response_header_bytes(64 * 1024)
            .max_response_header_count(128);
        if let Some(host) = host {
            outbound = outbound
                .host_authority_override(&host)
                .change_context(PlatformError::HttpClient)?;
        }
        if request.cache_intent.is_bypass() {
            outbound = outbound.cache_policy(OutboundCachePolicy::Bypass);
        }
        if request.stream_response {
            outbound = outbound.stream_response();
        }
        Ok((
            outbound,
            request.backend_name,
            request.max_request_body_bytes,
            deadline,
        ))
    }
}

fn http_error(message: impl Into<String>) -> Report<PlatformError> {
    Report::new(PlatformError::HttpClient).attach(message.into())
}

fn platform_response(
    response: OutboundResponse,
    backend_name: String,
) -> Result<PlatformResponse, Report<PlatformError>> {
    // Use the final RC framing pass, including HEAD/204/304 handling, rather
    // than rebuilding headers or losing duplicate Set-Cookie fields.
    let response = response
        .into_response()
        .change_context(PlatformError::HttpClient)?;
    Ok(PlatformResponse::new(response).with_backend_name(backend_name))
}

/// Finish the one-slot batch, including the driver's final EOF validation.
///
/// TS permits independently dropping/reordering pending handles and starting new
/// ones between selects. RC batches are closed at creation and cancel as a unit.
/// Each TS handle therefore owns one RC batch, whose original input index is 0.
/// We never reinterpret a TS handle position as an RC index. Waiting for collect
/// also catches trailing driver failures after the sole terminal item.
async fn finish_batch(batch: OutboundBatch) -> Result<OutboundResponse, Report<PlatformError>> {
    let mut results = batch.collect().await.map_err(|failure| {
        http_error(format!(
            "native outbound batch driver failed: {}",
            failure.error
        ))
    })?;
    if results.slots.len() != 1 {
        return Err(http_error(
            "native pending batch must have exactly one input slot",
        ));
    }
    let slot = results.slots.remove(0);
    match (results.termination, slot) {
        (OutboundBatchTermination::Completed, Some(slot)) => {
            slot.outcome.change_context(PlatformError::HttpClient)
        }
        (OutboundBatchTermination::Cutoff, _) => {
            Err(http_error("native outbound observation cutoff expired"))
        }
        _ => Err(http_error(
            "native outbound batch has no completed original slot 0",
        )),
    }
}

struct NativePendingHandle {
    backend_name: String,
    // This future owns the RC batch. Dropping it drops adapter transport work;
    // there is no detached Tokio task and no second cancellation owner.
    completion: LocalBoxFuture<'static, Result<OutboundResponse, Report<PlatformError>>>,
}

impl Future for NativePendingHandle {
    type Output = (String, Result<OutboundResponse, Report<PlatformError>>);

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        match self.completion.as_mut().poll(context) {
            Poll::Ready(result) => Poll::Ready((std::mem::take(&mut self.backend_name), result)),
            Poll::Pending => Poll::Pending,
        }
    }
}

async fn buffer_upload(
    body: Body,
    maximum: u64,
    deadline: Deadline,
    clock: &MonotonicClock,
) -> Result<Body, Report<PlatformError>> {
    match body.into_content() {
        BodyContent::Once(bytes) => {
            if bytes.len() as u64 > maximum {
                return Err(http_error("native outbound upload exceeded caller limit"));
            }
            if deadline.is_expired_at(clock.now()) {
                return Err(http_error(
                    "native outbound deadline expired collecting upload",
                ));
            }
            Ok(Body::from(bytes))
        }
        BodyContent::Stream(mut stream) => {
            let mut bytes = Vec::new();
            loop {
                let remaining = deadline.remaining_at(clock.now()).ok_or_else(|| {
                    http_error("native outbound deadline expired collecting upload")
                })?;
                let item = tokio::time::timeout(remaining, stream.next())
                    .await
                    .map_err(|_| http_error("native outbound upload timed out"))?;
                if deadline.is_expired_at(clock.now()) {
                    return Err(http_error(
                        "native outbound deadline expired collecting upload",
                    ));
                }
                let Some(item) = item else {
                    return Ok(Body::from(bytes));
                };
                let chunk = item.change_context(PlatformError::HttpClient)?;
                let length = (bytes.len() as u64)
                    .checked_add(chunk.len() as u64)
                    .ok_or_else(|| http_error("native upload size accounting overflow"))?;
                if length > maximum {
                    return Err(http_error("native outbound upload exceeded caller limit"));
                }
                bytes.extend_from_slice(&chunk);
            }
        }
    }
}

#[async_trait(?Send)]
impl PlatformHttpClient for AxumPlatformHttpClient {
    async fn send(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        let (outbound, backend, _, _) = self.prepare(request)?;
        let response = self
            .client
            .send(outbound)
            .await
            .change_context(PlatformError::HttpClient)?;
        platform_response(response, backend)
    }

    async fn send_async(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
        if request.stream_response {
            return Err(http_error(
                "native pending streaming responses require RC send, not batches",
            ));
        }
        let (outbound, backend_name, max_request_bytes, deadline) = self.prepare(request)?;
        // RC batches require buffered uploads. Do bounded collection locally in
        // the existing runtime before starting a batch, under the same deadline.
        let mut parts = outbound.into_parts();
        parts.body = buffer_upload(parts.body, max_request_bytes, deadline, &self.clock).await?;
        let outbound =
            OutboundRequest::from_parts(parts).change_context(PlatformError::HttpClient)?;
        let batch = self.client.start_batch_until(vec![outbound], deadline);
        let handle = NativePendingHandle {
            backend_name: backend_name.clone(),
            completion: finish_batch(batch).boxed_local(),
        };
        Ok(PlatformPendingRequest::new(handle).with_backend_name(backend_name))
    }

    async fn select(
        &self,
        pending_requests: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>> {
        if pending_requests.is_empty() {
            return Err(http_error("select called without pending requests"));
        }
        let handles = pending_requests
            .into_iter()
            .map(|pending| {
                pending
                    .downcast::<NativePendingHandle>()
                    .map_err(|_| http_error("pending request belongs to a different HTTP bridge"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let ((backend, outcome), _, remaining) = select_all(handles).await;
        let ready = outcome.and_then(|response| platform_response(response, backend.clone()));
        let failed_backend_name = ready.as_ref().err().map(|_| backend);
        let remaining = remaining
            .into_iter()
            .map(|handle| {
                let backend = handle.backend_name.clone();
                PlatformPendingRequest::new(handle).with_backend_name(backend)
            })
            .collect();
        Ok(PlatformSelectResult {
            ready,
            remaining,
            failed_backend_name,
        })
    }

    fn supports_streaming_responses(&self) -> bool {
        true
    }

    fn has_enforceable_total_request_deadline(&self) -> bool {
        true
    }
}
