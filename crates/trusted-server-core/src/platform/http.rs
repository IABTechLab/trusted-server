use std::any::Any;
use std::fmt;

use edgezero_core::http::{Request as EdgeRequest, Response as EdgeResponse};
use error_stack::Report;

use super::PlatformError;

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
}

impl PlatformHttpRequest {
    /// Create a new outbound request wrapper.
    #[must_use]
    pub fn new(request: EdgeRequest, backend_name: impl Into<String>) -> Self {
        Self {
            request,
            backend_name: backend_name.into(),
        }
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
        } = self;

        match inner.downcast::<T>() {
            Ok(inner) => Ok(*inner),
            Err(inner) => Err(Self {
                inner,
                backend_name,
            }),
        }
    }
}

impl fmt::Debug for PlatformPendingRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlatformPendingRequest")
            .field("backend_name", &self.backend_name)
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

    /// Start an upstream request without waiting for it to complete.
    ///
    /// # Errors
    ///
    /// Returns `PlatformError::HttpClient` when the request cannot be
    /// started.
    async fn send_async(
        &self,
        request: PlatformHttpRequest,
    ) -> Result<PlatformPendingRequest, Report<PlatformError>>;

    /// Whether [`send_async`](Self::send_async) defers execution so multiple
    /// pending requests progress concurrently and [`select`](Self::select)
    /// races them.
    ///
    /// Platforms where `send_async` executes each request eagerly before
    /// returning (e.g. Cloudflare Workers) return `false`. On such platforms
    /// multi-request fan-out runs sequentially and accrues the sum of the
    /// individual latencies, so callers with a latency budget (the auction
    /// orchestrator) must check this before launching more than one request.
    fn supports_concurrent_fanout(&self) -> bool {
        true
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
    use super::*;

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
