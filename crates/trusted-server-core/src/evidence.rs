//! Request evidence passed to providers.
//!
//! A provider is constructed once, from its own configuration block or by the
//! adapter that injects it, and is handed the current request's evidence as a
//! borrowed `&dyn` view on every call, for example the `request_info` argument
//! of [`generate`](crate::ec::provider::EdgeCookieProvider::generate). Nothing
//! per-request is stored on the provider, so the same provider serves every
//! request.
//!
//! These traits are those views. Request-scoped data outlives the live request
//! only when snapshotted, so an implementation owns its data where needed.
//! [`BorrowedRequestInfo`] is the borrowed view core builds on the request
//! path, and [`OwnedRequestInfo`] is the built-in owned snapshot.
//!
//! [`RequestInfo`] carries the evidence a provider in this workspace actually
//! reads, which today is the normalized client IP. It is the seam rather than a
//! fixed parameter list, so an accessor for further evidence (the User-Agent,
//! request headers, the request target) is added as a defaulted method in the
//! change that first reads it, without breaking any existing implementation.

/// Read-only access to the current request's basic information.
///
/// A provider receives it by reference at call time (`generate`), reads what it
/// needs, and does not retain it.
pub trait RequestInfo: Send + Sync + core::fmt::Debug {
    /// The normalized client IP, or `""` when the host cannot determine it.
    fn client_ip(&self) -> &str;
}

/// An owned [`RequestInfo`] built from a request snapshot.
///
/// Owns its data, for a context that cannot borrow the live request for the
/// duration of the call. The request path uses [`BorrowedRequestInfo`]; this
/// owned variant serves tests and any future host whose request data cannot be
/// borrowed.
#[derive(Debug, Default, Clone)]
pub struct OwnedRequestInfo {
    client_ip: String,
}

impl OwnedRequestInfo {
    /// Builds owned request info from the normalized client IP.
    #[must_use]
    pub fn new(client_ip: String) -> Self {
        Self { client_ip }
    }
}

impl RequestInfo for OwnedRequestInfo {
    fn client_ip(&self) -> &str {
        &self.client_ip
    }
}

/// A borrowed [`RequestInfo`] over the live request, with no allocation.
///
/// The composition root builds one per request from the normalized client IP,
/// then passes it to a provider by shared reference at call time (`generate`).
/// It borrows rather than owns, so it must not outlive the request. A provider
/// reads it during the call and does not retain it.
#[derive(Debug)]
pub struct BorrowedRequestInfo<'a> {
    client_ip: &'a str,
}

impl<'a> BorrowedRequestInfo<'a> {
    /// Borrows request info from the normalized client IP.
    #[must_use]
    pub fn new(client_ip: &'a str) -> Self {
        Self { client_ip }
    }
}

impl RequestInfo for BorrowedRequestInfo<'_> {
    fn client_ip(&self) -> &str {
        self.client_ip
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_request_info_reports_the_client_ip_it_was_built_with() {
        let info = OwnedRequestInfo::new("203.0.113.5".to_owned());

        assert_eq!(info.client_ip(), "203.0.113.5");
    }

    #[test]
    fn owned_request_info_defaults_to_no_client_ip() {
        let info = OwnedRequestInfo::default();

        assert_eq!(
            info.client_ip(),
            "",
            "a host that cannot determine a client IP reports an empty one"
        );
    }

    #[test]
    fn borrowed_request_info_reports_the_same_client_ip() {
        let client_ip = "203.0.113.5".to_owned();
        let info = BorrowedRequestInfo::new(&client_ip);

        assert_eq!(info.client_ip(), "203.0.113.5");
    }
}
