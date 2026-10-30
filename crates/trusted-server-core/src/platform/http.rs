use std::any::Any;
use std::fmt;

use edgezero_core::http::{Method, Request as EdgeRequest, Response as EdgeResponse};
use error_stack::Report;
use web_time::Instant;

use super::PlatformError;
use super::image_optimizer::PlatformImageOptimizerOptions;

/// What the caller wants the platform's intermediary cache to do with this request.
///
/// One enum rather than independent flags because on Fastly they are *not* independent:
/// `set_surrogate_key` and `set_ttl` each "override any previous `Request::set_pass` call"
/// (`fastly-0.12.1/src/http/request.rs:2462`, `:2381`). A pair of booleans could express
/// "bypass the cache, and tag it for purge", which on Fastly silently means "do not
/// bypass" — the opposite of how it reads. This type makes that combination unspellable.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PlatformCacheIntent {
    /// Let the platform apply its default behavior, honoring origin freshness.
    #[default]
    Default,
    /// Do not use the intermediary cache for this request.
    Bypass,
    /// Allow caching, tagged with a surrogate key so it can be purged.
    Shared {
        /// Key attached to the stored object.
        surrogate_key: String,
    },
}

impl PlatformCacheIntent {
    /// Whether this intent asks the platform to skip its cache entirely.
    #[must_use]
    pub fn is_bypass(&self) -> bool {
        matches!(self, Self::Bypass)
    }

    /// The surrogate key to tag the stored object with, when there is one.
    ///
    /// `None` for both [`Self::Default`] and [`Self::Bypass`]: attaching a key to a
    /// bypassed request would reverse the bypass on Fastly.
    #[must_use]
    pub fn surrogate_key(&self) -> Option<&str> {
        match self {
            Self::Shared { surrogate_key } => Some(surrogate_key),
            Self::Default | Self::Bypass => None,
        }
    }
}

/// Outbound HTTP request paired with a pre-resolved backend name.
///
/// Uses `EdgeZero`'s neutral [`EdgeRequest`] type so adapters share one
/// HTTP/body model while still preserving backend correlation for Fastly-style
/// fan-out.
#[derive(Debug)]
pub struct PlatformHttpRequest {
    /// Platform-neutral request to send upstream.
    pub request: EdgeRequest,
    /// Backend name resolved ahead of time via `PlatformBackend`.
    pub backend_name: String,
    /// Optional Image Optimizer metadata for platforms that support it.
    ///
    /// Adapters that cannot attach this metadata to their send path should
    /// return an error rather than silently dropping transformations.
    pub image_optimizer: Option<PlatformImageOptimizerOptions>,
    /// What the platform's intermediary response cache should do with this request.
    ///
    /// Adapters without an intermediary outbound cache may ignore it. Defaults to
    /// [`PlatformCacheIntent::Default`] so existing call sites preserve their behavior.
    pub cache_intent: PlatformCacheIntent,
    /// Whether the response body should stay streaming in the platform response.
    ///
    /// Adapters that cannot preserve streaming response bodies should return an
    /// error rather than silently buffering large asset responses.
    pub stream_response: bool,
}

impl PlatformHttpRequest {
    /// Create a new outbound request wrapper.
    #[must_use]
    pub fn new(request: EdgeRequest, backend_name: impl Into<String>) -> Self {
        Self {
            request,
            backend_name: backend_name.into(),
            image_optimizer: None,
            cache_intent: PlatformCacheIntent::Default,
            stream_response: false,
        }
    }

    /// Attach Image Optimizer metadata to the request.
    ///
    /// The current Fastly adapter supports this on [`PlatformHttpClient::send`]
    /// and rejects it on [`PlatformHttpClient::send_async`] because Fastly IO is
    /// not available through the fan-out helper path.
    #[must_use]
    pub fn with_image_optimizer(mut self, options: PlatformImageOptimizerOptions) -> Self {
        self.image_optimizer = Some(options);
        self
    }

    /// Bypass the platform's intermediary response cache for this request.
    #[must_use]
    pub fn with_cache_bypass(mut self) -> Self {
        self.cache_intent = PlatformCacheIntent::Bypass;
        self
    }

    /// Allow the platform to cache this response, tagged for purge.
    ///
    /// Replaces any prior bypass rather than combining with it, because the two cannot
    /// both hold: see [`PlatformCacheIntent`].
    #[must_use]
    pub fn with_shared_cache(mut self, surrogate_key: impl Into<String>) -> Self {
        self.cache_intent = PlatformCacheIntent::Shared {
            surrogate_key: surrogate_key.into(),
        };
        self
    }

    /// Preserve the upstream response body as a stream when the adapter supports it.
    ///
    /// Asset routes use this to avoid materializing large image/static responses
    /// into WASM memory before returning them to the client.
    #[must_use]
    pub fn with_stream_response(mut self) -> Self {
        self.stream_response = true;
        self
    }
}

/// Outbound HTTP response with optional backend correlation metadata.
#[derive(Debug)]
pub struct PlatformResponse {
    /// Platform-neutral HTTP response.
    pub response: EdgeResponse,
    /// Backend that produced the response, when known.
    pub backend_name: Option<String>,
}

impl PlatformResponse {
    /// Create a response wrapper without backend metadata.
    #[must_use]
    pub fn new(response: EdgeResponse) -> Self {
        Self {
            response,
            backend_name: None,
        }
    }

    /// Attach backend correlation metadata to the response.
    #[must_use]
    pub fn with_backend_name(mut self, backend_name: impl Into<String>) -> Self {
        self.backend_name = Some(backend_name.into());
        self
    }
}

/// Opaque handle for an in-flight outbound request.
///
/// The core stores this as an opaque support type. Adapter implementations can
/// recover their concrete runtime handle through [`Self::downcast`].
///
/// # `!Send` design
///
/// `inner` is `Box<dyn Any>` (not `Box<dyn Any + Send>`) because all async
/// operations in this platform layer use `#[async_trait(?Send)]`. The `?Send`
/// bound exists because [`edgezero_core::body::Body`] wraps a
/// `LocalBoxStream` that is intentionally `!Send` for wasm32 compatibility —
/// wasm32 targets are single-threaded and cannot use `Send` futures.
/// Adapter crates targeting a multi-threaded runtime (e.g. Axum with tokio)
/// would need to wrap state in `Arc` rather than relying on `Send` here.
///
/// See [`PlatformHttpClient`] for the trait-level `?Send` design rationale,
/// including why `Send + Sync` bounds on the trait type are compatible with
/// `?Send` futures.
pub struct PlatformPendingRequest {
    inner: Box<dyn Any>,
    backend_name: Option<String>,
    stream_response: bool,
    request_method: Option<Method>,
}

impl PlatformPendingRequest {
    /// Wrap an adapter-specific pending request handle.
    #[must_use]
    pub fn new<T>(inner: T) -> Self
    where
        T: Any,
    {
        Self {
            inner: Box::new(inner),
            backend_name: None,
            stream_response: false,
            request_method: None,
        }
    }

    /// Attach backend correlation metadata to the pending request.
    #[must_use]
    pub fn with_backend_name(mut self, backend_name: impl Into<String>) -> Self {
        self.backend_name = Some(backend_name.into());
        self
    }

    /// Return the correlated backend name when it is known before completion.
    #[must_use]
    pub fn backend_name(&self) -> Option<&str> {
        self.backend_name.as_deref()
    }

    /// Retain response-conversion metadata for direct single-handle waits.
    #[must_use]
    pub fn with_response_handling(mut self, stream_response: bool, request_method: Method) -> Self {
        self.stream_response = stream_response;
        self.request_method = Some(request_method);
        self
    }

    /// Whether completion must preserve the response body as a stream.
    #[must_use]
    pub fn stream_response(&self) -> bool {
        self.stream_response
    }

    /// Method of the originating request, when retained by the adapter.
    #[must_use]
    pub fn request_method(&self) -> Option<&Method> {
        self.request_method.as_ref()
    }

    /// Recover the adapter-specific pending request type.
    ///
    /// # Errors
    ///
    /// Returns `Err(self)` — the original wrapper with its backend metadata
    /// preserved — when `T` does not match the stored type.
    pub fn downcast<T>(self) -> Result<T, Self>
    where
        T: Any,
    {
        let Self {
            inner,
            backend_name,
            stream_response,
            request_method,
        } = self;

        match inner.downcast::<T>() {
            Ok(inner) => Ok(*inner),
            Err(inner) => Err(Self {
                inner,
                backend_name,
                stream_response,
                request_method,
            }),
        }
    }
}

impl fmt::Debug for PlatformPendingRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlatformPendingRequest")
            .field("backend_name", &self.backend_name)
            .field("stream_response", &self.stream_response)
            .field("request_method", &self.request_method)
            .finish()
    }
}

/// Result of waiting for one in-flight request to complete.
#[derive(Debug)]
pub struct PlatformSelectResult {
    /// Completed response, or the error returned by the ready request.
    pub ready: Result<PlatformResponse, Report<PlatformError>>,
    /// Requests still in flight after the ready result is removed.
    ///
    /// Note: entries in this list may have `backend_name == None` on some
    /// adapters (e.g., Fastly), because the adapter cannot preserve input
    /// identifiers across the platform `select()` call. Orchestrators must
    /// not rely on `backend_name()` being set on remaining entries.
    pub remaining: Vec<PlatformPendingRequest>,
    /// Backend name of the request that became ready with an error, when
    /// `ready` is `Err`. `None` on the success path and when the adapter
    /// cannot identify the failed backend.
    pub failed_backend_name: Option<String>,
}

/// A [`PlatformHttpClient`] stand-in used when outbound HTTP is not available
/// on the current platform (e.g. Cloudflare Workers, where the proxy client is
/// managed by the edgezero dispatch layer instead).
///
/// Every method returns [`PlatformError::HttpClient`], ensuring that code paths
/// that reach this stub receive a typed error. Adapter crates should use this
/// type rather than defining their own stub so the fallback behaviour is
/// consistent across all platform implementations.
pub struct UnavailableHttpClient;

#[async_trait::async_trait(?Send)]
impl PlatformHttpClient for UnavailableHttpClient {
    async fn send(
        &self,
        _request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        Err(Report::new(PlatformError::HttpClient)
            .attach("HTTP client is unavailable on this platform"))
    }

    async fn send_async(
        &self,
        _request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>> {
        Err(Report::new(PlatformError::HttpClient)
            .attach("HTTP client is unavailable on this platform"))
    }

    async fn select(
        &self,
        _pending_requests: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>> {
        Err(Report::new(PlatformError::HttpClient)
            .attach("HTTP client is unavailable on this platform"))
    }
}

/// Outbound HTTP client abstraction.
///
/// Supports both single-request sends ([`Self::send`]) and async fan-out
/// ([`Self::send_async`] + [`Self::select`]) so adapters can drive parallel
/// upstream requests without additional abstractions.
///
/// Object safety is provided by `async_trait`, which boxes the returned
/// futures behind `dyn PlatformHttpClient`.
///
/// Uses `?Send` on all targets because [`edgezero_core::body::Body`] contains
/// a `LocalBoxStream` that is `!Send` by design for wasm32 compatibility.
#[async_trait::async_trait(?Send)]
pub trait PlatformHttpClient: Send + Sync {
    /// Send a single upstream request and wait for the response.
    ///
    /// # Errors
    ///
    /// Returns `PlatformError::HttpClient` when the request cannot be sent or
    /// the platform client fails before a response is produced.
    async fn send(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>>;

    /// Starts an upstream request without waiting for its response.
    ///
    /// Adapters retain ownership of the request operation so it can progress
    /// while the publisher origin is fetched. Dropping its pending handle
    /// cancels or abandons unfinished work according to the runtime contract.
    ///
    /// On Cloudflare and Spin the request runs as a task on the runtime
    /// executor, so [`Self::wait`] and [`Self::select_until`] must be awaited
    /// on that executor. Driving them through a blocking executor such as
    /// `futures::executor::block_on` blocks the only thread the task can run
    /// on and hangs.
    ///
    /// # Errors
    ///
    /// Returns `PlatformError::HttpClient` when the request cannot be
    /// started.
    async fn send_async(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>>;

    /// Whether multiple pending requests can progress concurrently.
    ///
    /// Fastly and Axum provide concurrent fan-out. Cloudflare and Spin retain
    /// a single-provider contract and return `false`; the auction orchestrator
    /// checks this capability before launching more than one request.
    fn supports_concurrent_fanout(&self) -> bool {
        true
    }

    /// Whether [`send`](Self::send) can preserve upstream response bodies as
    /// [`Body::Stream`](edgezero_core::body::Body::Stream) when requested via
    /// [`PlatformHttpRequest::with_stream_response`].
    ///
    /// Adapters that cannot preserve streaming response bodies must keep the
    /// default `false` so callers do not request a contract the adapter will
    /// reject or silently buffer.
    fn supports_streaming_responses(&self) -> bool {
        false
    }

    /// Whether a request started by [`send_async`](Self::send_async) can be
    /// completed directly by [`wait`](Self::wait) while preserving a streamed
    /// response body.
    ///
    /// Supporting adapters must override `wait` with direct single-handle
    /// completion. A pending request marked for streaming must never be passed
    /// to [`select`](Self::select), whose fan-out contract remains buffered.
    fn supports_pending_streaming_responses(&self) -> bool {
        false
    }

    /// Wait for one of the in-flight requests to complete.
    ///
    /// # Errors
    ///
    /// Returns `PlatformError::HttpClient` if the platform cannot poll the
    /// pending requests at all.
    async fn select(
        &self,
        pending_requests: Vec<PlatformPendingRequest>,
    ) -> Result<PlatformSelectResult, Report<PlatformError>>;

    /// Collects an available response, waiting only until an absolute deadline.
    ///
    /// On expiry, implementations take one nonblocking snapshot of complete
    /// available responses before relinquishing unfinished requests. This
    /// preserves buffered bids when a publisher origin delays collection.
    /// Subsequent calls may drain only the completed snapshot. The deadline
    /// bounds network waiting, not eligibility by an upstream arrival timestamp.
    /// Returns `Ok(None)` when no complete response remains. Both header waits
    /// and body reads must be bounded; unfinished work is canceled or abandoned.
    /// Fastly cannot cancel backend requests with the installed SDK.
    /// Stream-marked requests require the separate streaming wait contract.
    ///
    /// # Performance
    ///
    /// Complete response bodies are buffered within adapter-specific limits.
    /// Deadline expiry drops incomplete bodies and pending handles after the
    /// nonblocking snapshot; complete buffered responses remain collectible.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use trusted_server_core::platform::{PlatformHttpClient, PlatformPendingRequest, PlatformSelectResult, PlatformError};
    /// # use error_stack::Report;
    /// # async fn collect(client: &dyn PlatformHttpClient, pending: Vec<PlatformPendingRequest>) -> Result<Option<PlatformSelectResult>, Report<PlatformError>> {
    /// let deadline = web_time::Instant::now() + std::time::Duration::from_millis(100);
    /// client.select_until(pending, deadline).await
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Unsupported`] unless the adapter implements
    /// bounded collection, or an HTTP error when polling itself fails.
    async fn select_until(
        &self,
        pending_requests: Vec<PlatformPendingRequest>,
        _deadline: Instant,
    ) -> Result<Option<PlatformSelectResult>, Report<PlatformError>> {
        drop(pending_requests);
        Err(Report::new(PlatformError::Unsupported)
            .attach("absolute-deadline HTTP collection is unavailable"))
    }

    /// Waits for a complete buffered response before an absolute deadline.
    ///
    /// Uses [`Self::select_until`] so expiry also relinquishes the pending
    /// request and never falls back to an unbounded wait.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use trusted_server_core::platform::{PlatformHttpClient, PlatformPendingRequest, PlatformResponse, PlatformError};
    /// # use error_stack::Report;
    /// # async fn collect(client: &dyn PlatformHttpClient, pending: PlatformPendingRequest) -> Result<PlatformResponse, Report<PlatformError>> {
    /// let deadline = web_time::Instant::now() + std::time::Duration::from_millis(100);
    /// client.wait_until(pending, deadline).await
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::DeadlineExceeded`] on expiry, an adapter polling
    /// error, or the attributed upstream error in the selected result.
    async fn wait_until(
        &self,
        pending: PlatformPendingRequest,
        deadline: Instant,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        self.select_until(vec![pending], deadline)
            .await?
            .ok_or_else(|| Report::new(PlatformError::DeadlineExceeded))?
            .ready
    }

    /// Wait for a single in-flight request to complete.
    ///
    /// This is a convenience wrapper around [`select`](Self::select) for the
    /// common case where only one request is in flight.
    ///
    /// # Errors
    ///
    /// Returns `PlatformError::HttpClient` if the underlying `select` fails or
    /// the response itself contains an error.
    async fn wait(
        &self,
        pending: PlatformPendingRequest,
    ) -> Result<PlatformResponse, Report<PlatformError>> {
        self.select(vec![pending]).await?.ready
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    use edgezero_core::body::Body;
    use edgezero_core::http::request_builder;

    use super::*;

    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn bounded_collection_default_relinquishes_expired_handles() {
        let dropped = Arc::new(AtomicUsize::new(0));
        let response = futures::executor::block_on(UnavailableHttpClient.select_until(
            vec![PlatformPendingRequest::new(DropProbe(Arc::clone(&dropped)))],
            Instant::now(),
        ))
        .expect_err("should reject unsupported collection even after expiry");
        assert!(matches!(
            response.current_context(),
            PlatformError::Unsupported
        ));
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            1,
            "should drop abandoned state"
        );
    }

    #[test]
    fn bounded_collection_default_fails_closed_instead_of_unbounded_polling() {
        let error = futures::executor::block_on(UnavailableHttpClient.select_until(
            vec![PlatformPendingRequest::new(())],
            Instant::now() + Duration::from_secs(1),
        ))
        .expect_err("should reject unsupported bounded collection");
        assert!(
            matches!(error.current_context(), PlatformError::Unsupported),
            "should not delegate to unbounded select"
        );
    }

    #[test]
    fn unsupported_bounded_wait_reports_unsupported() {
        let error = futures::executor::block_on(
            UnavailableHttpClient.wait_until(PlatformPendingRequest::new(()), Instant::now()),
        )
        .expect_err("should reject an unsupported wait");
        assert!(
            matches!(error.current_context(), PlatformError::Unsupported),
            "should preserve the unsupported collection contract"
        );
    }

    #[test]
    fn platform_http_request_cache_bypass_defaults_to_false() {
        let request = PlatformHttpRequest::new(
            request_builder()
                .body(Body::empty())
                .expect("should build request"),
            "stub-backend",
        );

        assert_eq!(
            request.cache_intent,
            PlatformCacheIntent::Default,
            "should preserve existing cache behavior by default"
        );
    }

    #[test]
    fn platform_http_request_cache_bypass_builder_enables_bypass() {
        let request = PlatformHttpRequest::new(
            request_builder()
                .body(Body::empty())
                .expect("should build request"),
            "stub-backend",
        )
        .with_cache_bypass();

        assert_eq!(
            request.cache_intent,
            PlatformCacheIntent::Bypass,
            "should enable intermediary cache bypass"
        );
    }

    #[test]
    fn cache_intent_cannot_request_bypass_and_a_surrogate_key_at_once() {
        // The whole reason this is an enum. On Fastly a surrogate key reverses a prior
        // set_pass, so "bypass, and tag for purge" would silently mean "do not bypass".
        let bypass = PlatformCacheIntent::Bypass;
        let shared = PlatformCacheIntent::Shared {
            surrogate_key: "ts-origin".to_owned(),
        };

        assert!(bypass.is_bypass());
        assert!(bypass.surrogate_key().is_none());
        assert!(!shared.is_bypass());
        assert_eq!(shared.surrogate_key(), Some("ts-origin"));
        assert!(PlatformCacheIntent::Default.surrogate_key().is_none());
        assert!(!PlatformCacheIntent::Default.is_bypass());
    }

    #[test]
    fn the_last_cache_builder_call_wins_rather_than_combining() {
        // Builders replace rather than accumulate: a request cannot end up asking for
        // both, whichever order a caller writes them in.
        let base = || {
            PlatformHttpRequest::new(
                request_builder()
                    .body(Body::empty())
                    .expect("should build request"),
                "stub-backend",
            )
        };

        assert_eq!(
            base()
                .with_cache_bypass()
                .with_shared_cache("ts-origin")
                .cache_intent,
            PlatformCacheIntent::Shared {
                surrogate_key: "ts-origin".to_owned()
            }
        );
        assert_eq!(
            base()
                .with_shared_cache("ts-origin")
                .with_cache_bypass()
                .cache_intent,
            PlatformCacheIntent::Bypass
        );
    }

    // ---------------------------------------------------------------------------
    // Error-correlation interim scope (before EdgeZero #213)
    // ---------------------------------------------------------------------------

    #[test]
    fn platform_response_default_has_no_backend_name() {
        // On Axum/Cloudflare noop clients return PlatformResponse::new(response)
        // with no backend_name. Core logic must not panic when backend_name is None.
        let response = edgezero_core::http::response_builder()
            .status(200)
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");
        let resp = PlatformResponse::new(response);
        // PlatformResponse has a public field, not a method.
        // PlatformPendingRequest has backend_name() method; PlatformResponse does not.
        assert_eq!(
            resp.backend_name, None,
            "PlatformResponse without backend_name must have None field"
        );
    }

    #[test]
    fn platform_response_with_backend_name_is_some() {
        // On Fastly, responses carry backend_name for error correlation.
        let response = edgezero_core::http::response_builder()
            .status(200)
            .body(edgezero_core::body::Body::empty())
            .expect("should build response");
        let resp = PlatformResponse::new(response).with_backend_name("prebid-backend");
        assert_eq!(
            resp.backend_name.as_deref(),
            Some("prebid-backend"),
            "with_backend_name must set backend_name field"
        );
    }
}
