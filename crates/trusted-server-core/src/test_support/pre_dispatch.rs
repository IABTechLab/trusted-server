//! Exercises head-only pre-dispatch hooks through their real router boundary.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use edgezero_core::PreDispatchRequest;
use edgezero_core::body::Body;
use edgezero_core::context::RequestContext;
use edgezero_core::error::EdgeError;
use edgezero_core::http::{Method, Request, RequestParts, Response};
use edgezero_core::router::{PreDispatchHook, RouterService};

#[derive(Clone)]
struct OrdinaryResponse;

struct ObservedHook {
    hook: Arc<dyn PreDispatchHook>,
    head: Arc<Mutex<Option<RequestParts>>>,
}

#[async_trait(?Send)]
impl PreDispatchHook for ObservedHook {
    async fn handle(
        &self,
        request: &mut PreDispatchRequest,
    ) -> Result<Option<Response>, EdgeError> {
        let response = self.hook.handle(request).await;
        *self.head.lock().expect("should lock the observed head") = Some(request.parts().clone());
        response
    }
}

async fn ordinary_request(context: RequestContext) -> Result<Response, EdgeError> {
    let mut response = Response::new(context.into_request()?.into_body());
    response.extensions_mut().insert(OrdinaryResponse);
    Ok(response)
}

/// Runs a hook against protected input while retaining ordinary request bodies.
#[async_trait(?Send)]
pub(crate) trait PreDispatchHookTestExt {
    async fn handle_request(&self, request: &mut Request) -> Result<Option<Response>, EdgeError>;
}

#[async_trait(?Send)]
impl<T> PreDispatchHookTestExt for T
where
    T: PreDispatchHook + Clone + 'static,
{
    async fn handle_request(&self, request: &mut Request) -> Result<Option<Response>, EdgeError> {
        let head = Arc::new(Mutex::new(None));
        let mut router = RouterService::builder().pre_dispatch_hook_arc(Arc::new(ObservedHook {
            hook: Arc::new(self.clone()),
            head: Arc::clone(&head),
        }));
        for method in [
            Method::GET,
            Method::HEAD,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ] {
            router = router.route("/", method.clone(), ordinary_request);
            router = router.route("/{*rest}", method, ordinary_request);
        }
        let incoming = std::mem::replace(request, Request::new(Body::empty()));
        let response = router.build().oneshot(incoming).await?;
        let parts = head
            .lock()
            .expect("should lock the observed head")
            .take()
            .expect("should observe the protected pre-dispatch head");
        if response.extensions().get::<OrdinaryResponse>().is_some() {
            *request = Request::from_parts(parts, response.into_body());
            Ok(None)
        } else {
            *request = Request::from_parts(parts, Body::empty());
            Ok(Some(response))
        }
    }
}
