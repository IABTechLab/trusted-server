//! TS request policy on the single request-scoped `EdgeZero` transport.

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "outbound_native_tests.rs"]
mod native_tests;

use std::sync::Arc;
use std::task::{Context, Poll};

use async_trait::async_trait;
use edgezero_core::http::{HeaderValue, header};
use edgezero_core::outbound::{
    HttpClient, OutboundCachePolicy, OutboundCropFocus, OutboundImageFormat,
    OutboundImageOptimizerOptions, OutboundImageOptimizerRegion, OutboundPending, OutboundRequest,
    OutboundResizeFilter, OutboundSendMode, OutboundTime, ResponseDecoding, validate_for_dispatch,
};
use edgezero_core::time::Deadline;
use error_stack::{Report, ResultExt as _};
use futures::future::{Either, poll_fn, select};
use web_time::Instant;

use super::{
    AuctionTargetId, PlatformCacheIntent, PlatformError, PlatformHttpClient, PlatformHttpRequest,
    PlatformImageOptimizerOptions, PlatformPendingRequest, PlatformResponse, PlatformSelectResult,
};

/// Applies TS correlation and collection policy to EdgeZero-owned exchanges.
///
/// The incoming request supplies the transport, progress driver and paired time.
/// This type creates no transport, executor, SDK handle or independent timer.
pub struct EdgeZeroHttpClient {
    client: HttpClient,
    time: OutboundTime,
    target: AuctionTargetId,
    response_decoding: Option<ResponseDecoding>,
}

impl EdgeZeroHttpClient {
    /// Uses the incoming request's client and its existing clock/timer factory.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let client = EdgeZeroHttpClient::new(request_client, AuctionTargetId::Axum);
    /// ```
    #[must_use]
    pub fn new(client: HttpClient, target: AuctionTargetId) -> Self {
        let time = client.time();
        Self {
            client,
            time,
            target,
            response_decoding: None,
        }
    }

    /// Select the existing SDK decoding policy for an explicit adapter profile.
    ///
    /// This changes no transport, executor, body owner or deadline source.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let client = EdgeZeroHttpClient::new(request_client, AuctionTargetId::Axum)
    ///     .response_decoding(ResponseDecoding::Decode);
    /// ```
    #[must_use]
    pub fn response_decoding(mut self, policy: ResponseDecoding) -> Self {
        self.response_decoding = Some(policy);
        self
    }

    /// Uses only the incoming request's transport, with no process-wide fallback.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let client = EdgeZeroHttpClient::for_request(&context, AuctionTargetId::Axum);
    /// ```
    #[must_use]
    pub fn for_request(
        context: &edgezero_core::context::RequestContext,
        target: AuctionTargetId,
    ) -> Arc<dyn PlatformHttpClient> {
        match context.http_client() {
            Some(client) => Arc::new(Self::new(client, target)),
            None => Arc::new(super::UnavailableHttpClient),
        }
    }

    fn prepare(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<OutboundRequest, Report<PlatformError>> {
        if request.stream_response && !self.supports_streaming_responses() {
            return Err(http_error(
                "this target does not support streaming responses",
            ));
        }
        let hosts = request.request.headers().get_all(header::HOST);
        let mut hosts = hosts.iter();
        let authority = hosts.next().cloned();
        if hosts.next().is_some() {
            return Err(http_error("outbound request has multiple Host values"));
        }
        let target = request.request.uri().clone();
        let mut outbound = OutboundRequest::from_request(request.request, target)
            .change_context(PlatformError::HttpClient)?
            .max_request_body_bytes(request.max_request_body_bytes);
        if let Some(authority) = authority {
            outbound = outbound
                .host_authority_override(
                    authority
                        .to_str()
                        .change_context(PlatformError::HttpClient)?,
                )
                .change_context(PlatformError::HttpClient)?;
        }
        if let Some(deadline) = request.deadline {
            outbound = outbound.deadline(deadline);
        }
        if let Some(timeout) = request.timeout {
            outbound = outbound.timeout(timeout);
        }
        let buffered_limit = if request.stream_response {
            None
        } else {
            match self.target {
                AuctionTargetId::Fastly | AuctionTargetId::Cloudflare => Some(10 * 1024 * 1024),
                AuctionTargetId::Spin => Some(8 * 1024 * 1024),
                AuctionTargetId::Axum => None,
            }
        };
        let response_limit = match (request.max_response_body_bytes, buffered_limit) {
            (Some(explicit), Some(target)) => Some(explicit.min(target)),
            (explicit, target) => explicit.or(target),
        };
        if let Some(limit) = response_limit {
            outbound = outbound
                .max_response_bytes(limit)
                .max_encoded_response_bytes(limit)
                .max_decoded_response_bytes(limit);
        }
        outbound = match self.target {
            AuctionTargetId::Fastly => outbound.connect_timeout(std::time::Duration::from_secs(1)),
            AuctionTargetId::Axum => outbound.connect_timeout(std::time::Duration::from_secs(5)),
            AuctionTargetId::Cloudflare | AuctionTargetId::Spin => outbound,
        };
        let cache_policy = match request.cache_intent {
            PlatformCacheIntent::Default => OutboundCachePolicy::PlatformDefault,
            PlatformCacheIntent::Bypass => OutboundCachePolicy::Bypass,
            PlatformCacheIntent::Shared { surrogate_key } => {
                match HeaderValue::from_str(&surrogate_key) {
                    Ok(value) => OutboundCachePolicy::PlatformDefaultWithSurrogateKey(value),
                    Err(_) => {
                        log::warn!(
                            "outbound cache tags are invalid; bypass the intermediary cache"
                        );
                        OutboundCachePolicy::Bypass
                    }
                }
            }
        };
        outbound = outbound.cache_policy(cache_policy).response_decoding(
            self.response_decoding.unwrap_or(match self.target {
                AuctionTargetId::Fastly | AuctionTargetId::Axum => ResponseDecoding::Original,
                AuctionTargetId::Cloudflare | AuctionTargetId::Spin => ResponseDecoding::Decode,
            }),
        );
        if let Some(options) = request.image_optimizer {
            outbound = outbound.image_optimizer(image_options(options)?);
        }
        if request.stream_response {
            outbound = outbound.stream_response();
        }
        Ok(outbound)
    }

    async fn prepare_pending(
        &self,
        mut request: PlatformHttpRequest,
    ) -> Result<OutboundRequest, Report<PlatformError>> {
        // Begin requires a buffered upload. Preserve an explicit caller budget
        // across preparation, without making the SDK's default a TS setting.
        // The original guarded input remains responsible for ingress constraints.
        let timeout = request
            .timeout
            .map(|timeout| self.time.clock().deadline_after(timeout));
        let deadline = match (request.deadline, timeout) {
            (Some(deadline), Some(timeout)) => Some(Deadline::at_instant(
                deadline.instant().min(timeout.instant()),
            )),
            (deadline, timeout) => deadline.or(timeout),
        };
        request.deadline = deadline;
        let outbound = self.prepare(request)?;
        // Validate before reading input so response-owned bodies cannot be
        // laundered into fresh upload bytes. Begin's upload check runs after
        // bounded materialization; its optimizer restriction remains pre-read.
        let mode = if outbound.image_optimizer_options().is_some() {
            OutboundSendMode::Begin
        } else {
            OutboundSendMode::Send
        };
        validate_for_dispatch(&outbound, mode).change_context(PlatformError::HttpClient)?;
        let mut parts = outbound.into_parts();
        let buffered = Box::pin(async move {
            let maximum = usize::try_from(parts.max_request_body_bytes)
                .change_context(PlatformError::HttpClient)?;
            let body = std::mem::replace(&mut parts.body, edgezero_core::body::Body::empty());
            let bytes = body
                .into_bytes_bounded(maximum)
                .await
                .change_context(PlatformError::HttpClient)?;
            parts.body = edgezero_core::body::Body::from_bytes(bytes);
            OutboundRequest::from_parts(parts).change_context(PlatformError::HttpClient)
        });
        let Some(deadline) = deadline else {
            return buffered.await;
        };
        match select(self.time.wait_until(deadline), buffered).await {
            Either::Left((result, _)) => {
                result.change_context(PlatformError::HttpClient)?;
                Err(Report::new(PlatformError::DeadlineExceeded))
            }
            Either::Right((request, _)) => request,
        }
    }

    fn collection_handles(
        &self,
        requests: Vec<PlatformPendingRequest>,
    ) -> Result<Vec<Pending>, Report<PlatformError>> {
        if requests.len() > 1 && !self.supports_concurrent_fanout() {
            return Err(
                http_error("this target does not support multi-provider fanout")
                    .attach(format!("{} providers submitted", requests.len())),
            );
        }
        if requests.is_empty() || requests.iter().any(PlatformPendingRequest::stream_response) {
            return Err(http_error(
                "fanout collection requires pending buffered exchanges; use wait for streamed responses",
            ));
        }
        recover(requests)
    }
}

struct Pending {
    backend_name: String,
    pending: OutboundPending,
}

fn http_error(message: &'static str) -> Report<PlatformError> {
    Report::new(PlatformError::HttpClient).attach(message)
}

fn recover(requests: Vec<PlatformPendingRequest>) -> Result<Vec<Pending>, Report<PlatformError>> {
    requests
        .into_iter()
        .map(|request| {
            request.downcast::<Pending>().map_err(|request| {
                http_error("pending exchange belongs to another client")
                    .attach(format!("{request:?}"))
            })
        })
        .collect()
}

fn poll_ready(handles: &mut [Pending], context: &mut Context<'_>) -> Poll<usize> {
    for (index, handle) in handles.iter_mut().enumerate() {
        if handle.pending.poll_ready(context).is_ready() {
            return Poll::Ready(index);
        }
    }
    Poll::Pending
}

async fn completed(mut handles: Vec<Pending>, index: usize) -> PlatformSelectResult {
    let selected = handles.swap_remove(index);
    let backend_name = selected.backend_name;
    let ready = selected
        .pending
        .collect()
        .await
        .change_context(PlatformError::HttpClient)
        .and_then(|response| {
            response
                .into_response()
                .change_context(PlatformError::HttpClient)
        })
        .map(|response| PlatformResponse::new(response).with_backend_name(backend_name.clone()));
    let failed_backend_name = ready.as_ref().err().map(|_| backend_name);
    let remaining = handles
        .into_iter()
        .map(|handle| {
            let backend = handle.backend_name.clone();
            PlatformPendingRequest::new(handle).with_backend_name(backend)
        })
        .collect();
    PlatformSelectResult {
        ready,
        remaining,
        failed_backend_name,
    }
}

#[async_trait(?Send)]
impl PlatformHttpClient for EdgeZeroHttpClient {
    async fn send(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        let backend_name = request.backend_name.clone();
        let response = self
            .client
            .send(self.prepare(request)?)
            .await
            .change_context(PlatformError::HttpClient)?
            .into_response()
            .change_context(PlatformError::HttpClient)?;
        Ok(PlatformResponse::new(response).with_backend_name(backend_name))
    }

    async fn send_async(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
        let backend_name = request.backend_name.clone();
        let streamed = request.stream_response;
        let method = request.request.method().clone();
        if streamed && !self.supports_pending_streaming_responses() {
            return Err(http_error(
                "this target does not support pending streamed responses",
            ));
        }
        let outbound = self.prepare_pending(request).await?;
        let pending = self
            .client
            .begin(outbound)
            .await
            .change_context(PlatformError::HttpClient)?;
        Ok(PlatformPendingRequest::new(Pending {
            backend_name: backend_name.clone(),
            pending,
        })
        .with_backend_name(backend_name)
        .with_response_handling(streamed, method))
    }

    async fn select_until(
        &self,
        requests: Vec<PlatformPendingRequest>,
        deadline: Instant,
    ) -> Result<Option<PlatformSelectResult>, Report<PlatformError>> {
        let mut handles = self.collection_handles(requests)?;
        if self.time.clock().now() < deadline {
            // Poll/register the paired timer first, before the readiness future
            // can drive Fastly progress. Timer expiry must not consume handles.
            let ready_index = {
                let ready = Box::pin(poll_fn(|context| poll_ready(&mut handles, context)));
                match select(self.time.wait_until(Deadline::at_instant(deadline)), ready).await {
                    Either::Left((result, _)) => {
                        result.change_context(PlatformError::HttpClient)?;
                        None
                    }
                    Either::Right((index, _)) => Some(index),
                }
            };
            if let Some(index) = ready_index {
                return Ok(Some(completed(handles, index).await));
            }
        }
        // One non-driving completion snapshot. Dropping unfinished handles
        // cancels their work; subsequent calls can drain only this snapshot.
        handles.retain_mut(|handle| handle.pending.is_ready());
        if handles.is_empty() {
            return Ok(None);
        }
        Ok(Some(completed(handles, 0).await))
    }

    async fn select(
        &self,
        requests: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>> {
        let mut handles = self.collection_handles(requests)?;
        let index = poll_fn(|context| poll_ready(&mut handles, context)).await;
        Ok(completed(handles, index).await)
    }

    async fn wait(
        &self,
        request: PlatformPendingRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        let mut handles = recover(vec![request])?;
        poll_fn(|context| poll_ready(&mut handles, context)).await;
        completed(handles, 0).await.ready
    }

    fn supports_concurrent_fanout(&self) -> bool {
        matches!(self.target, AuctionTargetId::Fastly | AuctionTargetId::Axum)
    }

    fn supports_streaming_responses(&self) -> bool {
        matches!(self.target, AuctionTargetId::Fastly | AuctionTargetId::Axum)
    }

    fn supports_pending_streaming_responses(&self) -> bool {
        self.target == AuctionTargetId::Fastly
    }
}

fn image_options(
    options: PlatformImageOptimizerOptions,
) -> Result<OutboundImageOptimizerOptions, Report<PlatformError>> {
    let mut output = OutboundImageOptimizerOptions::new()
        .region(
            match super::PlatformImageOptimizerRegion::parse(&options.region) {
                Some(super::PlatformImageOptimizerRegion::UsEast) => {
                    OutboundImageOptimizerRegion::UsEast
                }
                Some(super::PlatformImageOptimizerRegion::UsCentral) => {
                    OutboundImageOptimizerRegion::UsCentral
                }
                Some(super::PlatformImageOptimizerRegion::UsWest) => {
                    OutboundImageOptimizerRegion::UsWest
                }
                Some(super::PlatformImageOptimizerRegion::EuCentral) => {
                    OutboundImageOptimizerRegion::EuCentral
                }
                Some(super::PlatformImageOptimizerRegion::EuWest) => {
                    OutboundImageOptimizerRegion::EuWest
                }
                Some(super::PlatformImageOptimizerRegion::Asia) => {
                    OutboundImageOptimizerRegion::Asia
                }
                Some(super::PlatformImageOptimizerRegion::Australia) => {
                    OutboundImageOptimizerRegion::Australia
                }
                None => return Err(http_error("unsupported image optimizer region")),
            },
        )
        .preserve_origin_query(options.preserve_query_string_on_origin_request);
    let params = options.params;
    if let Some(format) = params.format {
        output = output.format(match format.trim().to_ascii_lowercase().as_str() {
            "auto" => OutboundImageFormat::Auto,
            "avif" => OutboundImageFormat::Avif,
            "gif" => OutboundImageFormat::Gif,
            "jpg" | "jpeg" => OutboundImageFormat::Jpeg,
            "jxl" | "jpegxl" => OutboundImageFormat::JpegXl,
            "mp4" => OutboundImageFormat::Mp4,
            "png" => OutboundImageFormat::Png,
            "webp" => OutboundImageFormat::WebP,
            _ => return Err(http_error("unsupported image format")),
        });
    }
    if let Some(quality) = params.quality {
        output = output
            .quality(u8::try_from(quality).change_context(PlatformError::HttpClient)?)
            .change_context(PlatformError::HttpClient)?;
    }
    if let Some(filter) = params.resize_filter {
        output = output.resize_filter(match filter.trim().to_ascii_lowercase().as_str() {
            "bicubic" => OutboundResizeFilter::Bicubic,
            "bilinear" => OutboundResizeFilter::Bilinear,
            "lanczos2" => OutboundResizeFilter::Lanczos2,
            "lanczos3" => OutboundResizeFilter::Lanczos3,
            "nearest" => OutboundResizeFilter::Nearest,
            _ => return Err(http_error("unsupported image resize filter")),
        });
    }
    if let Some(width) = params.width {
        output = output
            .width(width)
            .change_context(PlatformError::HttpClient)?;
    }
    if let Some(height) = params.height {
        output = output
            .height(height)
            .change_context(PlatformError::HttpClient)?;
    }
    if let Some(crop) = params.crop {
        let focus = match (crop.mode, crop.offset_x, crop.offset_y) {
            (Some(_), None, None) => OutboundCropFocus::Smart,
            (None, None, None) => OutboundCropFocus::Default,
            (None, Some(x), Some(y)) => OutboundCropFocus::Percent {
                x: u8::try_from(x).change_context(PlatformError::HttpClient)?,
                y: u8::try_from(y).change_context(PlatformError::HttpClient)?,
            },
            _ => {
                return Err(http_error(
                    "image crop offsets must be paired and cannot accompany smart crop",
                ));
            }
        };
        output = output
            .crop(crop.width, crop.height, focus)
            .change_context(PlatformError::HttpClient)?;
    }
    Ok(output)
}
