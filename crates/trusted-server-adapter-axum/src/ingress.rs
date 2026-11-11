//! Ingress capture for the application-owned plain HTTP listener.
//!
//! EdgeZero's HTTP transport marker is private to its dev-server helper. Our
//! timed listener cannot install it, and SDK request conversion replaces any
//! preinstalled `RequestIngress`. Carry a separate private extension through
//! conversion and restore it before trace preflight instead.

use async_trait::async_trait;
use axum::body::Body as AxumBody;
use axum::http::Request as AxumRequest;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{Request, Response, header};
use edgezero_core::ingress::{
    CapturedTarget, HeaderFidelity, InboundOrigin, OriginSource, Preservation, RequestIngress,
    TargetSource,
};
use edgezero_core::router::PreDispatchHook;

#[derive(Clone)]
struct HttpIngress(RequestIngress);

/// Capture ingress at the application-owned **plain HTTP** listener only.
///
/// This must not be used for TLS listeners or arbitrary in-process requests.
/// Forwarded headers never establish the transport scheme or authority.
///
/// # Examples
///
/// ```ignore
/// capture_http_ingress(&mut request);
/// let response = service.oneshot(request).await;
/// ```
pub fn capture_http_ingress(request: &mut AxumRequest<AxumBody>) {
    let uri = request.uri();
    let mut hosts = request.headers().get_all(header::HOST).iter();
    let origin = hosts.next().and_then(|host| {
        if hosts.next().is_some()
            || uri
                .scheme_str()
                .is_some_and(|scheme| !scheme.eq_ignore_ascii_case("http"))
        {
            return None;
        }
        let origin =
            InboundOrigin::parse("http", host.to_str().ok()?, OriginSource::TransportBinding)
                .ok()?;
        if let Some(authority) = uri.authority() {
            let supplied =
                InboundOrigin::parse("http", authority.as_str(), OriginSource::TransportBinding)
                    .ok()?;
            if !origin.same_authority(&supplied) {
                return None;
            }
        }
        Some(origin)
    });
    let source = if uri.scheme().is_some() {
        TargetSource::RuntimeUrl
    } else {
        TargetSource::RuntimePathAndQuery
    };
    let ingress = RequestIngress::new(
        CapturedTarget::capture(&uri.to_string(), source, Preservation::Unknown),
        origin,
        HeaderFidelity::RUNTIME_HEADER_MAP,
    );
    request.extensions_mut().insert(HttpIngress(ingress));
}

/// Restore only facts captured by our listener, before the shared trace policy.
pub(crate) struct HttpIngressHook<H>(pub(crate) H);

#[async_trait(?Send)]
impl<H: PreDispatchHook> PreDispatchHook for HttpIngressHook<H> {
    async fn handle(&self, request: &mut Request) -> Result<Option<Response>, EdgeError> {
        if let Some(ingress) = request.extensions_mut().remove::<HttpIngress>() {
            request.extensions_mut().insert(ingress.0);
        }
        self.0.handle(request).await
    }
}
