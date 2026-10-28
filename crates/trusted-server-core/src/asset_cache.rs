//! Where a module may answer for a file before core asks the file's origin.
//!
//! The publisher path and the asset routes each offer a `GET` for a file to
//! the asset cache module that `[asset-cache]` selects, at the one point
//! where core is about to ask the file's origin. The module answers with a
//! response, with the failure of the origin request it made, or by passing
//! the file back to core. Core keeps no copy, no key and no gate of its own,
//! so a deployment that selects no asset cache module has every file asked
//! of its origin.

use edgezero_core::body::Body as EdgeBody;
use error_stack::Report;
use http::{HeaderMap, Method, Request, Response, Uri, header};

use crate::platform::{PlatformCacheIntent, PlatformError, PlatformHttpRequest, RuntimeServices};

/// The type folder, and the section, that selects the asset cache module.
pub const MODULE_TYPE: &str = "asset-cache";

/// A module that may answer for a file before core asks the file's origin.
#[async_trait::async_trait(?Send)]
pub trait AssetCacheModule: Send + Sync {
    /// The answer for `file`, at the point core is about to ask its origin.
    async fn answer(&self, services: &RuntimeServices, file: AssetFile<'_>) -> AssetAnswer;
}

/// What core does with the module's answer.
#[derive(Debug)]
pub enum AssetAnswer {
    /// Serves this in place of the origin's response, processed as that
    /// response would be. Where the adapter does not stream responses, the
    /// body must be whole, as the origin's would be.
    Serve(Response<EdgeBody>),
    /// The module asked the origin through [`AssetFile::origin`] and could
    /// not reach it, so core answers as it does when its own origin request
    /// fails.
    OriginFailed(Report<PlatformError>),
    /// Core asks the origin itself, as it does with no module selected.
    AskOrigin,
}

/// Which of core's two paths offers a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetVia {
    /// [`handle_publisher_request`](crate::publisher::handle_publisher_request),
    /// for a file the publisher's origin sends.
    Publisher,
    /// [`handle_asset_proxy_request`](crate::proxy::handle_asset_proxy_request),
    /// for a file an asset route fetches.
    Route,
}

/// A `GET` core is about to send to a file's origin, with what core knows of
/// the reader's request. Core makes it, and a module reads its fields.
#[derive(Debug)]
#[non_exhaustive]
pub struct AssetFile<'a> {
    /// The path that offers the file.
    pub via: AssetVia,
    /// Whether the reader's request carries `Authorization`.
    pub carries_authorization: bool,
    /// Whether the reader's request carries `Cookie`.
    pub carries_cookie: bool,
    /// The host the reader asked, empty when the request names none.
    pub host: &'a str,
    /// The scheme the reader asked over.
    pub scheme: &'a str,
    /// Whether the reader's request is for a document, such as a page or a
    /// frame.
    pub is_document: bool,
    /// Whether the request, as core will send it, asks for the origin's own
    /// answer, by a range, a conditional header, or a `Cache-Control` or
    /// `Pragma` that asks for the origin. On the publisher path also when a
    /// module left state on the request or an integration marked its
    /// response personalized.
    pub asks_for_origin: bool,
    /// The URL core asks of the origin, with its query.
    pub origin_url: &'a str,
    /// What names the origin beyond the URL, so one origin's file is never
    /// taken for another's.
    pub origin_identity: &'a str,
    /// The request core would send to the origin.
    pub origin: OriginRequest,
}

/// The request core would send to the file's origin, a `GET` with no body.
#[derive(Debug, Clone)]
pub struct OriginRequest {
    uri: Uri,
    headers: HeaderMap,
    backend_name: String,
    cache_intent: PlatformCacheIntent,
    stream_response: bool,
}

impl OriginRequest {
    /// What `request` asks for, being the request core sends itself when the
    /// module passes the file back, or `None` when it is not a `GET` with no
    /// body.
    ///
    /// A request that describes a body, by `Content-Length` or
    /// `Transfer-Encoding`, is never offered, because a module's load sends
    /// it with no body, and a load the origin refuses is the answer to every
    /// request waiting on it.
    pub(crate) fn of(request: &PlatformHttpRequest) -> Option<Self> {
        let headers = request.request.headers();
        if request.request.method() != Method::GET
            || headers.contains_key(header::CONTENT_LENGTH)
            || headers.contains_key(header::TRANSFER_ENCODING)
        {
            return None;
        }
        Some(Self {
            uri: request.request.uri().clone(),
            headers: headers.clone(),
            backend_name: request.backend_name.clone(),
            cache_intent: request.cache_intent.clone(),
            stream_response: request.stream_response,
        })
    }

    /// The request core would send, built anew for each call.
    #[must_use]
    pub fn platform_request(&self) -> PlatformHttpRequest {
        let mut request = Request::new(EdgeBody::empty());
        *request.method_mut() = Method::GET;
        *request.uri_mut() = self.uri.clone();
        *request.headers_mut() = self.headers.clone();
        let mut platform_request = PlatformHttpRequest::new(request, self.backend_name.clone());
        platform_request.cache_intent = self.cache_intent.clone();
        platform_request.stream_response = self.stream_response;
        platform_request
    }

    /// The URI the request asks for.
    #[must_use]
    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    /// The headers the request carries, which hold the values `Vary` names.
    #[must_use]
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl<'a> AssetFile<'a> {
    /// A file offered on `via` for `origin_url`, with nothing set that keeps
    /// it from a copy, which a module's test changes field by field.
    #[must_use]
    pub fn for_test(
        via: AssetVia,
        host: &'a str,
        origin_url: &'a str,
        origin: OriginRequest,
    ) -> Self {
        Self {
            via,
            carries_authorization: false,
            carries_cookie: false,
            host,
            scheme: "https",
            is_document: false,
            asks_for_origin: false,
            origin_url,
            origin_identity: origin_url,
            origin,
        }
    }
}

#[cfg(any(test, feature = "test-utils"))]
impl OriginRequest {
    /// A `GET` of `uri` with `headers` to `backend_name`, with the platform's
    /// default cache behavior, for a module's tests.
    #[must_use]
    pub fn for_test(
        uri: Uri,
        headers: HeaderMap,
        backend_name: &str,
        stream_response: bool,
    ) -> Self {
        Self {
            uri,
            headers,
            backend_name: backend_name.to_owned(),
            cache_intent: PlatformCacheIntent::Default,
            stream_response,
        }
    }
}

/// Asset cache modules for core's own tests of the two paths.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// What a module was offered, kept beyond the call that offered it.
    #[derive(Debug, Clone)]
    pub(crate) struct Offered {
        pub(crate) via: AssetVia,
        pub(crate) carries_authorization: bool,
        pub(crate) carries_cookie: bool,
        pub(crate) host: String,
        pub(crate) scheme: String,
        pub(crate) is_document: bool,
        pub(crate) asks_for_origin: bool,
        pub(crate) origin_url: String,
        pub(crate) origin_identity: String,
        pub(crate) origin: OriginRequest,
    }

    impl Offered {
        fn of(file: &AssetFile<'_>) -> Self {
            Self {
                via: file.via,
                carries_authorization: file.carries_authorization,
                carries_cookie: file.carries_cookie,
                host: file.host.to_owned(),
                scheme: file.scheme.to_owned(),
                is_document: file.is_document,
                asks_for_origin: file.asks_for_origin,
                origin_url: file.origin_url.to_owned(),
                origin_identity: file.origin_identity.to_owned(),
                origin: file.origin.clone(),
            }
        }
    }

    /// How a test module answers every file it is offered.
    enum Answering {
        AskOrigin,
        Serve(fn() -> Response<EdgeBody>),
        OriginFailed,
    }

    /// An asset cache module that records each file it is offered and
    /// answers every one the same way.
    pub(crate) struct TestAssetCache {
        answering: Answering,
        offered: Mutex<Vec<Offered>>,
    }

    impl TestAssetCache {
        fn answering(answering: Answering) -> Arc<Self> {
            Arc::new(Self {
                answering,
                offered: Mutex::new(Vec::new()),
            })
        }

        /// A module that passes every file back to core.
        pub(crate) fn asking_the_origin() -> Arc<Self> {
            Self::answering(Answering::AskOrigin)
        }

        /// A module that serves what `response` makes for every file.
        pub(crate) fn serving(response: fn() -> Response<EdgeBody>) -> Arc<Self> {
            Self::answering(Answering::Serve(response))
        }

        /// A module whose own request to the origin fails for every file.
        pub(crate) fn failing_to_reach_the_origin() -> Arc<Self> {
            Self::answering(Answering::OriginFailed)
        }

        /// Every file offered so far, in order.
        pub(crate) fn offered(&self) -> Vec<Offered> {
            self.offered.lock().expect("should lock the offers").clone()
        }
    }

    #[async_trait::async_trait(?Send)]
    impl AssetCacheModule for TestAssetCache {
        async fn answer(&self, _services: &RuntimeServices, file: AssetFile<'_>) -> AssetAnswer {
            self.offered
                .lock()
                .expect("should lock the offers")
                .push(Offered::of(&file));
            match self.answering {
                Answering::AskOrigin => AssetAnswer::AskOrigin,
                Answering::Serve(response) => AssetAnswer::Serve(response()),
                Answering::OriginFailed => {
                    AssetAnswer::OriginFailed(Report::new(PlatformError::HttpClient))
                }
            }
        }
    }
}
