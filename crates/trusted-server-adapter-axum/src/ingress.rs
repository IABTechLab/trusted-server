//! Ingress capture for the plain HTTP listener owned by this adapter.
//!
//! The custom listener keeps terminal timing outside `EdgeZeroAxumService`, but
//! bypasses `EdgeZero`'s dev-server-only transport marker. Capture equivalent
//! metadata here and restore it after SDK conversion, before trace preflight.

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderMap, Request, Uri, header::HOST};
use edgezero_core::error::EdgeError;
use edgezero_core::http::{Request as CoreRequest, Response};
use edgezero_core::ingress::{
    CapturedTarget, HeaderFidelity, InboundOrigin, OriginSource, Preservation, RequestIngress,
    TargetSource,
};
use edgezero_core::router::PreDispatchHook;
use trusted_server_core::trace::TracePreDispatchHook;

// Not constructible from request headers or outside this listener bridge.
#[derive(Clone)]
struct HttpListenerIngress(RequestIngress);

/// Capture ingress at the adapter-owned plain HTTP listener boundary.
///
/// Call only when serving plain HTTP, never to infer a transport scheme from
/// forwarded headers. As in `EdgeZero`'s listener, Host must be singular and agree
/// with any absolute request URI. Header and target preservation stay unknown.
pub fn capture_http_ingress(request: &mut Request<Body>) {
    let source = if request.uri().scheme().is_some() {
        TargetSource::RuntimeUrl
    } else {
        TargetSource::RuntimePathAndQuery
    };
    let ingress = RequestIngress::new(
        CapturedTarget::capture(&request.uri().to_string(), source, Preservation::Unknown),
        http_origin(request.uri(), request.headers()),
        HeaderFidelity::RUNTIME_HEADER_MAP,
    );
    request
        .extensions_mut()
        .insert(HttpListenerIngress(ingress));
}

fn http_origin(uri: &Uri, headers: &HeaderMap) -> Option<InboundOrigin> {
    let mut hosts = headers.get_all(HOST).iter();
    let host = hosts.next()?.to_str().ok()?;
    if hosts.next().is_some() {
        return None;
    }
    let origin = InboundOrigin::parse("http", host, OriginSource::TransportBinding).ok()?;
    if uri
        .scheme_str()
        .is_some_and(|scheme| !scheme.eq_ignore_ascii_case("http"))
    {
        return None;
    }
    if let Some(authority) = uri.authority() {
        let supplied =
            InboundOrigin::parse("http", authority.as_str(), OriginSource::TransportBinding)
                .ok()?;
        if !origin.same_authority(&supplied) {
            return None;
        }
    }
    Some(origin)
}

pub(crate) struct HttpTracePreDispatchHook(pub(crate) TracePreDispatchHook);

#[async_trait(?Send)]
impl PreDispatchHook for HttpTracePreDispatchHook {
    async fn handle(&self, request: &mut CoreRequest) -> Result<Option<Response>, EdgeError> {
        // SDK conversion installs its own snapshot without a transport origin.
        // Only the private listener snapshot can replace it. Bare services keep
        // the SDK's fail-closed behavior when no transport has been established.
        if let Some(HttpListenerIngress(ingress)) =
            request.extensions_mut().remove::<HttpListenerIngress>()
        {
            request.extensions_mut().insert(ingress);
        }
        self.0.handle(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use edgezero_core::ingress::{MAX_AUTHORITY_BYTES, MAX_TARGET_BYTES, TargetUnavailable};

    #[test]
    fn listener_origin_requires_one_valid_host_consistent_with_http_uri() {
        for (uri, hosts, accepted) in [
            ("/_ts/trace/enable", vec!["localhost:8787"], true),
            (
                "http://localhost:8787/_ts/trace/enable",
                vec!["localhost:8787"],
                true,
            ),
            (
                "http://LOCALHOST:80/_ts/trace/enable",
                vec!["localhost"],
                true,
            ),
            (
                "http://[::1]:8787/_ts/trace/enable",
                vec!["[::1]:8787"],
                true,
            ),
            ("/_ts/trace/enable", vec![], false),
            (
                "/_ts/trace/enable",
                vec!["localhost:8787", "localhost:8787"],
                false,
            ),
            (
                "/_ts/trace/enable",
                vec!["localhost:8787,localhost:8787"],
                false,
            ),
            ("/_ts/trace/enable", vec!["user@localhost:8787"], false),
            ("/_ts/trace/enable", vec!["localhost:invalid"], false),
            (
                "https://localhost:8787/_ts/trace/enable",
                vec!["localhost:8787"],
                false,
            ),
            (
                "http://other.example.com/_ts/trace/enable",
                vec!["localhost:8787"],
                false,
            ),
            (
                "http://localhost:8788/_ts/trace/enable",
                vec!["localhost:8787"],
                false,
            ),
            ("http://localhost:8787/_ts/trace/enable", vec![], false),
        ] {
            let mut request = Request::builder()
                .uri(uri)
                .header("forwarded", "host=localhost:8787;proto=https")
                .header("x-forwarded-host", "localhost:8787")
                .header("x-forwarded-proto", "https")
                .body(Body::empty())
                .expect("should build listener request");
            for host in hosts {
                request.headers_mut().append(
                    HOST,
                    HeaderValue::from_str(host).expect("should construct visible Host"),
                );
            }
            capture_http_ingress(&mut request);
            let ingress = &request
                .extensions()
                .get::<HttpListenerIngress>()
                .expect("should capture listener metadata")
                .0;
            assert_eq!(ingress.origin().is_some(), accepted, "URI {uri}");
            if let Some(origin) = ingress.origin() {
                assert_eq!(origin.scheme(), "http");
                assert_eq!(origin.source(), OriginSource::TransportBinding);
            }
            assert_eq!(
                ingress.header_fidelity(&HOST),
                HeaderFidelity::RUNTIME_HEADER_MAP,
                "should not claim original header preservation"
            );
            let CapturedTarget::Complete(target) = ingress.target() else {
                panic!("should capture bounded runtime target");
            };
            assert_eq!(target.value(), uri);
            assert_eq!(target.fidelity(), Preservation::Unknown);
        }
    }

    #[test]
    fn listener_capture_preserves_bounds_and_rejects_non_utf8_host() {
        let mut request = Request::builder()
            .uri(format!("/_ts/trace?{}", "x".repeat(MAX_TARGET_BYTES)))
            .body(Body::empty())
            .expect("should build oversized visible target");
        for host in [
            HeaderValue::from_bytes(b"\xff").expect("should build non-UTF8 Host"),
            HeaderValue::from_str(&"x".repeat(MAX_AUTHORITY_BYTES + 1))
                .expect("should build oversized Host"),
        ] {
            request.headers_mut().insert(HOST, host);
            capture_http_ingress(&mut request);
            let ingress = &request
                .extensions()
                .get::<HttpListenerIngress>()
                .expect("should capture listener metadata")
                .0;
            assert!(ingress.origin().is_none());
            assert!(matches!(
                ingress.target(),
                CapturedTarget::Unavailable(TargetUnavailable::TooLarge)
            ));
        }
    }
}
