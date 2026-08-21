//! Service interfaces injected into providers.
//!
//! Trusted Server wires providers by dependency injection. A provider's
//! constructor takes the services it needs as `Arc<dyn Trait>`, and the adapter
//! (the composition root) supplies instances per request. A provider that needs
//! a service the host does not supply cannot be built, so the request stops
//! rather than silently degrading.
//!
//! These traits are the service interfaces. Request-scoped data outlives the
//! live request only when snapshotted, so an implementation owns its data where
//! needed ([`OwnedRequestInfo`] is the built-in owned snapshot).

use http::HeaderMap;

/// Read-only access to the current request's basic information.
///
/// The request data any host can supply: the normalized client IP, the
/// User-Agent, and request headers. A provider receives it by reference at call
/// time (`generate`/`detect`), reads what it needs, and does not retain it.
pub trait RequestInfo: Send + Sync + core::fmt::Debug {
    /// The normalized client IP, or `""` when the host cannot determine it.
    fn client_ip(&self) -> &str;

    /// The `User-Agent` header value, or `""` when absent.
    fn user_agent(&self) -> &str;

    /// An arbitrary request header by name (case-insensitive), or `None`.
    ///
    /// Request cookies are read through this, from the `Cookie` header (a
    /// provider that stores values in cookies parses them from it).
    fn header(&self, name: &str) -> Option<&str>;

    /// The names of all request headers present, for a provider that enumerates
    /// evidence (for example to forward client hints). The default is empty.
    fn header_names(&self) -> Vec<&str> {
        Vec::new()
    }

    /// The request path (the URL path, without the query string), or `""` when
    /// request info was built without a URL.
    ///
    /// A provider reads the request target through this together with
    /// [`query`](Self::query); `RequestInfo` is the evidence abstraction, so more
    /// request accessors can be added here (as defaulted methods) without
    /// breaking existing implementations.
    fn path(&self) -> &str {
        ""
    }

    /// The raw request query string (the part after `?`, without the leading
    /// `?`), or `""` when the request carried none.
    ///
    /// A provider reads request parameters through this, or the
    /// [`query_param`](Self::query_param) convenience. The default is empty, for
    /// request info built without a URL.
    fn query(&self) -> &str {
        ""
    }

    /// The first value of query parameter `name`, percent-decoded, or `None`
    /// when the parameter is absent.
    ///
    /// Parses [`query`](Self::query) with `application/x-www-form-urlencoded`
    /// rules, matching how the browser encodes query parameters.
    fn query_param(&self, name: &str) -> Option<String> {
        url::form_urlencoded::parse(self.query().as_bytes())
            .find_map(|(key, value)| (&*key == name).then(|| value.into_owned()))
    }
}

/// An owned [`RequestInfo`] built from a request snapshot.
///
/// Owns the client IP and a header snapshot, for a context that cannot borrow
/// the live request for the duration of the call. The request path uses
/// [`BorrowedRequestInfo`]; this owned variant serves tests and any future
/// host whose request data cannot be borrowed.
#[derive(Debug, Default, Clone)]
pub struct OwnedRequestInfo {
    client_ip: String,
    headers: HeaderMap,
    path: String,
    query: String,
}

impl OwnedRequestInfo {
    /// Builds owned request info from the client IP and a header snapshot.
    ///
    /// The request target ([`path`](RequestInfo::path) and
    /// [`query`](RequestInfo::query)) is empty; attach it with
    /// [`with_request_target`](Self::with_request_target) when the caller has the
    /// URL.
    #[must_use]
    pub fn new(client_ip: String, headers: HeaderMap) -> Self {
        Self {
            client_ip,
            headers,
            path: String::new(),
            query: String::new(),
        }
    }

    /// Attaches the request target (URL path and query string) to this snapshot,
    /// so a provider can read request parameters through
    /// [`query_param`](RequestInfo::query_param).
    #[must_use]
    pub fn with_request_target(mut self, path: String, query: String) -> Self {
        self.path = path;
        self.query = query;
        self
    }
}

impl RequestInfo for OwnedRequestInfo {
    fn client_ip(&self) -> &str {
        &self.client_ip
    }

    fn user_agent(&self) -> &str {
        self.headers
            .get(http::header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }

    fn header_names(&self) -> Vec<&str> {
        self.headers.keys().map(http::HeaderName::as_str).collect()
    }

    fn path(&self) -> &str {
        &self.path
    }

    fn query(&self) -> &str {
        &self.query
    }
}

/// A borrowed [`RequestInfo`] over the live request, with no allocation.
///
/// The composition root builds one per request from the normalized client IP and
/// an optional borrow of the request headers, then passes it to a provider by
/// shared reference at call time (`generate`/`detect`). It borrows rather than
/// owns, so it must not outlive the request. A provider reads it during the call
/// and does not retain it, so no per-request `HeaderMap` clone is needed.
#[derive(Debug)]
pub struct BorrowedRequestInfo<'a> {
    client_ip: &'a str,
    headers: Option<&'a HeaderMap>,
    path: &'a str,
    query: &'a str,
}

impl<'a> BorrowedRequestInfo<'a> {
    /// Borrows request info from the client IP and optional request headers.
    ///
    /// Pass `None` for headers on a path that only needs the client IP. The
    /// request target ([`path`](RequestInfo::path) and
    /// [`query`](RequestInfo::query)) is empty; attach it with
    /// [`with_request_target`](Self::with_request_target) when the caller has the
    /// URL.
    #[must_use]
    pub fn new(client_ip: &'a str, headers: Option<&'a HeaderMap>) -> Self {
        Self {
            client_ip,
            headers,
            path: "",
            query: "",
        }
    }

    /// Attaches the borrowed request target (URL path and query string), so a
    /// provider can read request parameters through
    /// [`query_param`](RequestInfo::query_param).
    #[must_use]
    pub fn with_request_target(mut self, path: &'a str, query: &'a str) -> Self {
        self.path = path;
        self.query = query;
        self
    }
}

impl RequestInfo for BorrowedRequestInfo<'_> {
    fn client_ip(&self) -> &str {
        self.client_ip
    }

    fn user_agent(&self) -> &str {
        self.headers
            .and_then(|headers| headers.get(http::header::USER_AGENT))
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .and_then(|headers| headers.get(name))
            .and_then(|value| value.to_str().ok())
    }

    fn header_names(&self) -> Vec<&str> {
        self.headers
            .map(|headers| headers.keys().map(http::HeaderName::as_str).collect())
            .unwrap_or_default()
    }

    fn path(&self) -> &str {
        self.path
    }

    fn query(&self) -> &str {
        self.query
    }
}

/// Host-computed client fingerprints that are not carried in request headers.
///
/// A host that can compute them supplies an implementation (Fastly exposes the
/// TLS JA4 and HTTP/2 fingerprints). A provider that needs them takes
/// `Arc<dyn HostSignals>` in its constructor; on a host that supplies none, the
/// provider cannot be built and the request stops.
pub trait HostSignals: Send + Sync + core::fmt::Debug {
    /// The full JA4 TLS fingerprint, or `None` when unavailable.
    fn ja4(&self) -> Option<&str>;

    /// The raw HTTP/2 SETTINGS fingerprint, or `None` when unavailable.
    fn h2(&self) -> Option<&str>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_with_cookie() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "cookie",
            "client-id=abc123; ts-ec=xyz"
                .parse()
                .expect("should parse cookie header"),
        );
        headers
    }

    #[test]
    fn query_param_decodes_and_selects_the_first_value() {
        let info = OwnedRequestInfo::new(String::new(), HeaderMap::new())
            .with_request_target("/page".to_owned(), "id=a%20b&id=second&flag=1".to_owned());

        assert_eq!(
            info.query_param("id").as_deref(),
            Some("a b"),
            "should percent-decode and return the first value for a repeated key"
        );
        assert_eq!(info.query_param("flag").as_deref(), Some("1"));
        assert_eq!(
            info.query_param("missing"),
            None,
            "an absent parameter should be None"
        );
    }

    #[test]
    fn path_and_query_accessors_return_the_request_target() {
        let info = OwnedRequestInfo::new(String::new(), HeaderMap::new())
            .with_request_target("/a/b".to_owned(), "x=1".to_owned());
        assert_eq!(info.path(), "/a/b");
        assert_eq!(info.query(), "x=1");
    }

    #[test]
    fn request_info_defaults_to_an_empty_target() {
        let info = OwnedRequestInfo::new("203.0.113.5".to_owned(), HeaderMap::new());
        assert_eq!(info.path(), "", "path should default to empty");
        assert_eq!(info.query(), "", "query should default to empty");
        assert_eq!(
            info.query_param("id"),
            None,
            "query_param over an empty query should be None"
        );
    }

    #[test]
    fn a_provider_reads_cookies_from_the_header() {
        let info = OwnedRequestInfo::new("203.0.113.5".to_owned(), headers_with_cookie());
        assert_eq!(
            info.header("cookie"),
            Some("client-id=abc123; ts-ec=xyz"),
            "cookies are read through the Cookie header"
        );
    }

    #[test]
    fn borrowed_request_info_exposes_the_same_target() {
        let headers = headers_with_cookie();
        let info = BorrowedRequestInfo::new("203.0.113.5", Some(&headers))
            .with_request_target("/page", "id=abc123");

        assert_eq!(info.path(), "/page");
        assert_eq!(info.query(), "id=abc123");
        assert_eq!(info.query_param("id").as_deref(), Some("abc123"));
        assert_eq!(info.header("cookie"), Some("client-id=abc123; ts-ec=xyz"));
    }
}
