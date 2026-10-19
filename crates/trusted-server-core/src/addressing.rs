//! How a request is addressed before its configuration is chosen.
//!
//! An adapter chooses the app-config blob that serves a request before any
//! settings exist, and by default chooses it by the request's host. A
//! deployment that serves several publishers on one host, each under a path
//! of its own, supplies a [`RequestAddressing`]. It reads the request's host
//! and path and answers three things, being the selector values that name
//! the request's blob, the path the request continues with, and the path
//! prefix it removed.
//!
//! The removed prefix travels on the request as a [`RemovedPathPrefix`]
//! extension, where a module's request preparer or a handler reads it. Core
//! never writes it into a response. A deployment whose pages must carry the
//! prefix puts it back with a middleware of its own.

use std::sync::Arc;

use edgezero_core::body::Body as EdgeBody;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{HandlerFuture, Method};
use edgezero_core::router::RouterService;
use error_stack::Report;
use http::uri::PathAndQuery;
use http::{Request, Uri, header};

/// Answers how a request is addressed, before its configuration is chosen.
///
/// Asked once for each request, before the adapter chooses the app-config
/// blob and before routing. It is given the request's `Host`, or `None` when
/// the request names none, and the path without its query.
///
/// An adapter that chooses a blob for each request puts the selector values
/// in the `{host}` place of its key selector and reads the first whose blob
/// exists. An adapter that reads one app-config blob has no blob to choose,
/// so it ignores the selector values, and the path and the removed prefix
/// apply there too.
///
/// # Examples
///
/// ```
/// use trusted_server_core::addressing::{Addressing, RequestAddress, RequestAddressing};
///
/// /// Serves `/<publisher>/<path>` from the publisher's own blob.
/// struct FirstSegment;
///
/// impl RequestAddressing for FirstSegment {
///     fn address(&self, _host: Option<&str>, path: &str) -> Addressing {
///         let rest = path.strip_prefix('/').unwrap_or(path);
///         let (segment, tail) = rest.split_once('/').unwrap_or((rest, ""));
///         RequestAddress::new(
///             vec![segment.to_owned()],
///             format!("/{tail}"),
///             format!("/{segment}"),
///         )
///         .map_or(Addressing::ByHost, Addressing::Addressed)
///     }
/// }
///
/// let addressing = FirstSegment.address(None, "/publisher.example/news");
/// let Addressing::Addressed(address) = addressing else {
///     panic!("should address the request by its first segment");
/// };
/// assert_eq!(address.selector_values(), ["publisher.example"]);
/// assert_eq!(address.path(), "/news");
/// assert_eq!(address.removed_prefix(), "/publisher.example");
/// ```
pub trait RequestAddressing: Send + Sync {
    /// How the request for `host` at `path` is addressed.
    fn address(&self, host: Option<&str>, path: &str) -> Addressing;
}

/// What a [`RequestAddressing`] answers for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Addressing {
    /// Chosen by the request's host, with its path unchanged, which is how
    /// every request is addressed when no hook is supplied.
    ByHost,
    /// Chosen by the selector values the hook names, with the path it gives.
    Addressed(RequestAddress),
}

/// The selector values, the continuing path and the removed prefix a
/// [`RequestAddressing`] names for a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestAddress {
    selector_values: Vec<String>,
    path: String,
    removed_prefix: String,
}

impl RequestAddress {
    /// An address that names `selector_values` in order, continues the
    /// request at `path` and records `removed_prefix` as the prefix removed.
    ///
    /// The request's query is kept, so `path` carries none.
    ///
    /// # Errors
    ///
    /// Returns [`AddressingError::InvalidPath`] when `path` does not start
    /// with `/`, is not percent-encoded ASCII, carries a `?` or a `#`, or is
    /// not a valid URI path.
    pub fn new(
        selector_values: Vec<String>,
        path: String,
        removed_prefix: String,
    ) -> Result<Self, Report<AddressingError>> {
        if !is_continuing_path(&path) {
            return Err(Report::new(AddressingError::InvalidPath));
        }
        Ok(Self {
            selector_values,
            path,
            removed_prefix,
        })
    }

    /// The values to try in the selector's `{host}` place, in order.
    #[must_use]
    pub fn selector_values(&self) -> &[String] {
        &self.selector_values
    }

    /// The path the request continues with, without a query.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The path prefix the hook removed.
    #[must_use]
    pub fn removed_prefix(&self) -> &str {
        &self.removed_prefix
    }

    /// The request extension carrying the removed prefix.
    #[must_use]
    pub fn into_removed_prefix(self) -> RemovedPathPrefix {
        RemovedPathPrefix::new(self.removed_prefix)
    }
}

/// Why a request could not be addressed as a hook named.
///
/// The messages carry nothing from the request, so they can be logged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, derive_more::Display)]
pub enum AddressingError {
    /// The continuing path is not one a request can carry.
    #[display(
        "a continuing path is percent-encoded ASCII that starts with `/` and carries no query or \
         fragment"
    )]
    InvalidPath,
    /// The request's address could not be rebuilt with the continuing path.
    #[display("the request's address could not be rebuilt with the continuing path")]
    Rewrite,
}

impl core::error::Error for AddressingError {}

/// The addressing an adapter given no hook uses, which answers
/// [`Addressing::ByHost`] for every request.
#[derive(Debug, Clone, Copy, Default)]
pub struct ByHostAddressing;

impl RequestAddressing for ByHostAddressing {
    fn address(&self, _host: Option<&str>, _path: &str) -> Addressing {
        Addressing::ByHost
    }
}

/// The path prefix a [`RequestAddressing`] removed from a request, carried
/// as a request extension.
///
/// Present only on a request a hook addressed. Putting the prefix back into
/// a response, such as into the links of a page, is the deployment's job,
/// done by a middleware of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedPathPrefix(String);

impl RemovedPathPrefix {
    /// The extension carrying `prefix`.
    #[must_use]
    pub fn new(prefix: String) -> Self {
        Self(prefix)
    }

    /// The prefix that was removed.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Asks `addressing` about `request` and applies its answer.
///
/// A request addressed by host is left as it is. Otherwise its path becomes
/// the continuing path, its query is kept, and the removed prefix is
/// inserted as a [`RemovedPathPrefix`] extension. The selector values are
/// not read here, because choosing a blob is the adapter's step and comes
/// first.
///
/// The hook is given the `Host` header, or the request's authority where
/// there is no `Host`.
///
/// # Errors
///
/// Returns [`AddressingError::Rewrite`] when the request's address cannot be
/// rebuilt with the continuing path. The request is then left as it was.
pub fn address_request(
    addressing: &dyn RequestAddressing,
    request: &mut Request<EdgeBody>,
) -> Result<(), Report<AddressingError>> {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .or_else(|| request.uri().authority().map(http::uri::Authority::as_str));
    let Addressing::Addressed(address) = addressing.address(host, request.uri().path()) else {
        return Ok(());
    };
    *request.uri_mut() = with_path(request.uri(), address.path())?;
    request
        .extensions_mut()
        .insert(address.into_removed_prefix());
    Ok(())
}

/// A router that asks `addressing` about every request, applies its answer
/// with [`address_request`], and then hands the request to `router`.
///
/// An adapter that reads one app-config blob wraps its router with this, so
/// the selector values a hook names are ignored there. It routes every path
/// for each method `router` registers and passes every request on, so a
/// path or method `router` refuses is refused by `router` with its own
/// answer. The route table it reports is its own catch-all routes, not those
/// of `router`.
#[must_use]
pub fn addressed_router(
    router: RouterService,
    addressing: Arc<dyn RequestAddressing>,
) -> RouterService {
    let mut methods: Vec<Method> = router
        .routes()
        .iter()
        .map(|route| route.method().clone())
        .collect();
    methods.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    methods.dedup();

    let handler = addressed_handler(router, addressing);
    let mut builder = RouterService::builder();
    for method in methods {
        builder = builder.route("/", method.clone(), handler.clone());
        builder = builder.route("/{*rest}", method, handler.clone());
    }
    builder.build()
}

/// Addresses the request, then routes it through `router`.
fn addressed_handler(
    router: RouterService,
    addressing: Arc<dyn RequestAddressing>,
) -> impl Fn(RequestContext) -> HandlerFuture + Clone + Send + Sync + 'static {
    move |ctx: RequestContext| {
        let router = router.clone();
        let addressing = Arc::clone(&addressing);
        Box::pin(async move {
            let mut request = ctx.into_request();
            if let Err(report) = address_request(addressing.as_ref(), &mut request) {
                log::error!("request addressing failed: {report:?}");
                return Err(EdgeError::internal(*report.current_context()));
            }
            router.oneshot(request).await
        })
    }
}

/// Whether `path` is one a request can continue with, which is a
/// percent-encoded ASCII path as a request carries it, so every adapter
/// routes the same path.
fn is_continuing_path(path: &str) -> bool {
    path.starts_with('/')
        && path.is_ascii()
        && !path.contains(['?', '#'])
        && PathAndQuery::try_from(path).is_ok_and(|parsed| parsed.as_str() == path)
}

/// `uri` with its path replaced by `path` and its query kept.
fn with_path(uri: &Uri, path: &str) -> Result<Uri, Report<AddressingError>> {
    let path_and_query = match uri.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    };
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(
        PathAndQuery::try_from(path_and_query)
            .map_err(|_| Report::new(AddressingError::Rewrite))?,
    );
    Uri::from_parts(parts).map_err(|_| Report::new(AddressingError::Rewrite))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use edgezero_core::http::{Response, StatusCode, request_builder};

    use super::*;

    /// A hook that serves `/<segment>/<rest>` from the blob `segment` names
    /// and records what it was asked.
    #[derive(Default)]
    struct FirstSegment {
        asked: Mutex<Vec<(Option<String>, String)>>,
    }

    impl RequestAddressing for FirstSegment {
        fn address(&self, host: Option<&str>, path: &str) -> Addressing {
            self.asked
                .lock()
                .expect("should lock what the hook was asked")
                .push((host.map(str::to_owned), path.to_owned()));
            let rest = path.strip_prefix('/').unwrap_or(path);
            let (segment, tail) = rest.split_once('/').unwrap_or((rest, ""));
            let address = RequestAddress::new(
                vec![segment.to_owned()],
                format!("/{tail}"),
                format!("/{segment}"),
            )
            .expect("should name a valid continuing path");
            Addressing::Addressed(address)
        }
    }

    fn request(uri: &str, host: Option<&str>) -> Request<EdgeBody> {
        let mut builder = request_builder().method("GET").uri(uri);
        if let Some(host) = host {
            builder = builder.header(header::HOST, host);
        }
        builder
            .body(EdgeBody::empty())
            .expect("should build the request")
    }

    /// A router that answers with the path, the query and the removed
    /// prefix the request reached it with.
    fn echo_router() -> RouterService {
        let echo = |ctx: RequestContext| async move {
            let request = ctx.request();
            let prefix = request
                .extensions()
                .get::<RemovedPathPrefix>()
                .map_or("none", RemovedPathPrefix::as_str);
            let body = format!(
                "{} {} {prefix}",
                request.uri().path(),
                request.uri().query().unwrap_or("-")
            );
            Ok::<Response, EdgeError>(Response::new(EdgeBody::from(body)))
        };
        RouterService::builder()
            .route("/", Method::GET, echo)
            .route("/{*rest}", Method::GET, echo)
            .build()
    }

    fn body_of(response: Response) -> String {
        String::from_utf8(
            response
                .into_body()
                .into_bytes()
                .unwrap_or_default()
                .to_vec(),
        )
        .expect("should answer in UTF-8")
    }

    fn route(router: &RouterService, request: Request<EdgeBody>) -> Response {
        futures::executor::block_on(router.oneshot(request)).expect("should route the request")
    }

    #[test]
    fn the_default_addressing_answers_by_host() {
        for (host, path) in [
            (None, "/"),
            (Some("publisher.example"), "/news"),
            (Some("publisher.example"), "/other.example/news"),
        ] {
            assert_eq!(
                ByHostAddressing.address(host, path),
                Addressing::ByHost,
                "should address {host:?} {path} by its host"
            );
        }
    }

    #[test]
    fn a_continuing_path_starts_with_a_slash_and_carries_no_query() {
        for path in ["/", "/news", "/news/today.html", "/a%20b", "//double"] {
            assert!(
                RequestAddress::new(Vec::new(), path.to_owned(), String::new()).is_ok(),
                "should accept `{path}`"
            );
        }
        for path in ["", "news", "/news?page=2", "/news#top", "/a b", "/\u{e9}"] {
            let error = RequestAddress::new(Vec::new(), path.to_owned(), String::new())
                .expect_err("should refuse a path a request cannot continue with");
            assert_eq!(
                *error.current_context(),
                AddressingError::InvalidPath,
                "should refuse `{path}` as an invalid path"
            );
        }
    }

    #[test]
    fn the_error_carries_nothing_from_the_request() {
        let error = RequestAddress::new(Vec::new(), "/private?id=reader".to_owned(), String::new())
            .expect_err("should refuse a path with a query");

        assert!(
            !error.to_string().contains("reader"),
            "a logged refusal should not carry what the request named"
        );
    }

    #[test]
    fn a_request_addressed_by_host_is_left_as_it_is() {
        let mut addressed = request(
            "https://publisher.example/other.example/news?page=2",
            Some("publisher.example"),
        );
        let before = addressed.uri().clone();

        address_request(&ByHostAddressing, &mut addressed)
            .expect("should address the request by its host");

        assert_eq!(
            addressed.uri(),
            &before,
            "should leave the address unchanged"
        );
        assert!(
            addressed.extensions().get::<RemovedPathPrefix>().is_none(),
            "should carry no removed prefix when nothing was removed"
        );
    }

    #[test]
    fn a_rewriting_hook_continues_the_request_at_its_path_and_keeps_the_query() {
        let hook = FirstSegment::default();
        let mut addressed = request(
            "https://front.example/publisher.example/news/today?page=2&sort=new",
            Some("front.example"),
        );

        address_request(&hook, &mut addressed).expect("should address the request");

        assert_eq!(
            addressed.uri().path(),
            "/news/today",
            "should continue at the path"
        );
        assert_eq!(
            addressed.uri().query(),
            Some("page=2&sort=new"),
            "should keep the query"
        );
        assert_eq!(
            addressed.uri().host(),
            Some("front.example"),
            "should keep the authority"
        );
        assert_eq!(
            addressed
                .extensions()
                .get::<RemovedPathPrefix>()
                .map(RemovedPathPrefix::as_str),
            Some("/publisher.example"),
            "should carry the prefix the hook removed"
        );
        assert_eq!(
            *hook
                .asked
                .lock()
                .expect("should lock what the hook was asked"),
            [(
                Some("front.example".to_owned()),
                "/publisher.example/news/today".to_owned()
            )],
            "the hook should be given the host and the path without its query"
        );
    }

    #[test]
    fn a_request_with_no_host_header_offers_its_authority() {
        let hook = FirstSegment::default();
        let mut addressed = request("https://front.example:8443/publisher.example/", None);

        address_request(&hook, &mut addressed).expect("should address the request");

        assert_eq!(
            hook.asked
                .lock()
                .expect("should lock what the hook was asked")[0]
                .0,
            Some("front.example:8443".to_owned()),
            "should offer the authority where no Host header is sent"
        );
        assert_eq!(addressed.uri().path(), "/", "should continue at the root");

        let hook = FirstSegment::default();
        let mut addressed = request("/publisher.example/news", None);
        address_request(&hook, &mut addressed).expect("should address the request");
        assert_eq!(
            hook.asked
                .lock()
                .expect("should lock what the hook was asked")[0]
                .0,
            None,
            "should offer no host when the request names none"
        );
    }

    #[test]
    fn an_addressed_router_routes_the_continuing_path_with_the_prefix() {
        let router = addressed_router(echo_router(), Arc::new(FirstSegment::default()));

        let response = route(
            &router,
            request(
                "https://front.example/publisher.example/news?page=2",
                Some("front.example"),
            ),
        );

        assert_eq!(
            response.status(),
            StatusCode::OK,
            "should route the request"
        );
        assert_eq!(
            body_of(response),
            "/news page=2 /publisher.example",
            "the router should see the continuing path, the query and the prefix"
        );
    }

    #[test]
    fn a_by_host_hook_leaves_routing_unchanged() {
        let by_host = addressed_router(echo_router(), Arc::new(ByHostAddressing));

        for uri in ["/", "/news?page=2", "/publisher.example/news"] {
            let plain = body_of(route(&echo_router(), request(uri, Some("front.example"))));
            assert_eq!(
                body_of(route(&by_host, request(uri, Some("front.example")))),
                plain,
                "`{uri}` should route as it does with no hook"
            );
        }
    }

    #[test]
    fn a_method_the_router_refuses_is_still_refused() {
        let plain = route(
            &echo_router(),
            request_builder()
                .method("POST")
                .uri("/news")
                .body(EdgeBody::empty())
                .expect("should build the request"),
        );
        let addressed = route(
            &addressed_router(echo_router(), Arc::new(FirstSegment::default())),
            request_builder()
                .method("POST")
                .uri("/publisher.example/news")
                .body(EdgeBody::empty())
                .expect("should build the request"),
        );

        assert_eq!(
            plain.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "the plain router should refuse a method it does not route"
        );
        assert_eq!(
            addressed.status(),
            plain.status(),
            "the addressed router should refuse it the same way"
        );
    }
}
