//! Service interfaces injected into providers.
//!
//! Trusted Server wires providers by dependency injection. A provider's
//! constructor takes the services it needs as `Arc<dyn Trait>`, and the adapter
//! (the composition root) supplies instances per request. A provider that needs
//! a service the host does not supply cannot be built, so the request stops
//! rather than silently degrading.
//!
//! These traits are the service interfaces. Concrete implementations live with
//! the host or vendor that supplies them (for example `FastlyHostSignals` in
//! `trusted-server-device-fastly`). An injected service outlives the borrow of
//! the live request, so an implementation owns its data ([`OwnedRequestInfo`] is
//! the built-in owned snapshot).

use http::HeaderMap;

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
    fn header(&self, name: &str) -> Option<&str>;

    /// The names of all request headers present, for a provider that enumerates
    /// evidence (for example to forward client hints). The default is empty.
    fn header_names(&self) -> Vec<&str> {
        Vec::new()
    }
}

/// An owned [`RequestInfo`] built from a request snapshot.
///
/// Owns the client IP and a header snapshot, for a context that cannot borrow
/// the live request for the duration of the call. The request path uses
/// [`BorrowedRequestInfo`]; this owned variant serves tests and any future
/// host whose request data cannot be borrowed.
#[derive(Debug)]
pub struct OwnedRequestInfo {
    client_ip: String,
    headers: HeaderMap,
}

impl OwnedRequestInfo {
    /// Builds owned request info from the client IP and a header snapshot.
    #[must_use]
    pub fn new(client_ip: String, headers: HeaderMap) -> Self {
        Self { client_ip, headers }
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
}

impl<'a> BorrowedRequestInfo<'a> {
    /// Borrows request info from the client IP and optional request headers.
    ///
    /// Pass `None` for headers on a path that only needs the client IP (for
    /// example the Edge Cookie generate path), so no header access is retained.
    #[must_use]
    pub fn new(client_ip: &'a str, headers: Option<&'a HeaderMap>) -> Self {
        Self { client_ip, headers }
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
}
