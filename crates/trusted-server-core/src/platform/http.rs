use std::any::Any;
use std::fmt;

use edgezero_core::http::{Request as EdgeRequest, Response as EdgeResponse};
use error_stack::Report;

use super::PlatformError;

/// Platform-neutral Image Optimizer options for an upstream request.
///
/// Core code stores only a closed transformation set here. The Fastly adapter is
/// responsible for translating these values to SDK-specific
/// `ImageOptimizerOptions`, while non-Fastly adapters can reject or ignore the
/// metadata according to their platform capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformImageOptimizerOptions {
    /// Image Optimizer processing region understood by the target adapter.
    pub region: String,
    /// Whether non-IO query parameters should be preserved on the origin request.
    pub preserve_query_string_on_origin_request: bool,
    /// Transformation parameters to apply.
    pub params: PlatformImageOptimizerParams,
}

impl PlatformImageOptimizerOptions {
    /// Create Image Optimizer options for the given region and params.
    #[must_use]
    pub fn new(region: impl Into<String>, params: PlatformImageOptimizerParams) -> Self {
        Self {
            region: region.into(),
            preserve_query_string_on_origin_request: false,
            params,
        }
    }

    /// Preserve non-IO query parameters on the origin request.
    ///
    /// Asset routes with profile-table IO reject arbitrary query preservation by
    /// default because client query parameters can otherwise become additional
    /// Image Optimizer inputs.
    #[must_use]
    pub fn with_preserve_query_string_on_origin_request(mut self, preserve: bool) -> Self {
        self.preserve_query_string_on_origin_request = preserve;
        self
    }
}

/// Platform-neutral subset of image transformation parameters.
///
/// This intentionally mirrors only the parameters accepted by asset-route
/// profile tables: format, quality, resize filter, dimensions, and crop. Client
/// query strings are converted into this closed set before the request reaches a
/// platform adapter.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PlatformImageOptimizerParams {
    /// Output format such as `auto` or `webp`.
    pub format: Option<String>,
    /// Output quality from 0 to 100.
    pub quality: Option<u32>,
    /// Resize filter such as `bicubic`.
    pub resize_filter: Option<String>,
    /// Output width in pixels.
    pub width: Option<u32>,
    /// Output height in pixels.
    pub height: Option<u32>,
    /// Crop transformation.
    pub crop: Option<PlatformImageOptimizerCrop>,
}

impl PlatformImageOptimizerParams {
    /// Merge another param set into this one, with `other` taking precedence.
    pub fn merge_from(&mut self, other: Self) {
        if other.format.is_some() {
            self.format = other.format;
        }
        if other.quality.is_some() {
            self.quality = other.quality;
        }
        if other.resize_filter.is_some() {
            self.resize_filter = other.resize_filter;
        }
        if other.width.is_some() {
            self.width = other.width;
        }
        if other.height.is_some() {
            self.height = other.height;
        }
        if other.crop.is_some() {
            self.crop = other.crop;
        }
    }
}

/// Platform-neutral crop transformation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformImageOptimizerCrop {
    /// Aspect-ratio width component.
    pub width: u32,
    /// Aspect-ratio height component.
    pub height: u32,
    /// Optional crop focus mode.
    pub mode: Option<PlatformImageOptimizerCropMode>,
    /// Optional x-axis crop offset bucket.
    pub offset_x: Option<u32>,
    /// Optional y-axis crop offset bucket.
    pub offset_y: Option<u32>,
}

impl PlatformImageOptimizerCrop {
    /// Create a bare aspect-ratio crop.
    #[must_use]
    pub fn aspect_ratio(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            mode: None,
            offset_x: None,
            offset_y: None,
        }
    }

    /// Returns true when no focus mode or explicit offsets have been applied.
    #[must_use]
    pub fn is_bare_aspect_ratio(&self) -> bool {
        self.mode.is_none() && self.offset_x.is_none() && self.offset_y.is_none()
    }
}

/// Platform-neutral crop focus mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformImageOptimizerCropMode {
    /// Use Fastly IO smart crop mode.
    Smart,
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
}

impl PlatformHttpRequest {
    /// Create a new outbound request wrapper.
    #[must_use]
    pub fn new(request: EdgeRequest, backend_name: impl Into<String>) -> Self {
        Self {
            request,
            backend_name: backend_name.into(),
            image_optimizer: None,
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
    pub remaining: Vec<PlatformPendingRequest>,
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
