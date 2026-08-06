// The `cloudflare` feature activates the `worker` crate which requires
// wasm-bindgen and only compiles for `wasm32-unknown-unknown`. Enabling it on
// a native target produces cryptic linker errors — catch it early instead.
#[cfg(all(feature = "cloudflare", not(target_arch = "wasm32")))]
compile_error!(
    "The `cloudflare` feature requires `--target wasm32-unknown-unknown`. \
     Run: cargo check -p trusted-server-adapter-cloudflare \
     --features cloudflare --target wasm32-unknown-unknown"
);

pub mod app;
pub mod middleware;
pub mod platform;

#[cfg(target_arch = "wasm32")]
use worker::{Context, Env, Request, Response, Result, event};

#[cfg(all(
    feature = "aps-runner-proxy-integration-test",
    any(target_arch = "wasm32", test)
))]
fn preserved_reserved_method(value: &str) -> Option<edgezero_core::http::Method> {
    edgezero_core::http::Method::from_bytes(value.as_bytes()).ok()
}

#[cfg(target_arch = "wasm32")]
#[event(fetch)]
/// Dispatches an incoming Cloudflare Worker fetch event.
///
/// # Errors
///
/// Returns a Workers runtime error when the fallback error response cannot be
/// constructed.
pub async fn main(req: Request, env: Env, ctx: Context) -> Result<Response> {
    if let Ok(config) = env.var("TRUSTED_SERVER_CONFIG") {
        app::set_cloudflare_config_json(config.to_string());
    }
    app::set_cloudflare_env(env.clone());

    #[cfg(feature = "aps-runner-proxy-integration-test")]
    let is_reserved = req
        .url()
        .is_ok_and(|url| trusted_server_core::integrations::aps::is_aps_family_path(url.path()));
    #[cfg(feature = "aps-runner-proxy-integration-test")]
    if is_reserved {
        // workers-rs maps unknown methods to GET; the underlying Fetch request
        // preserves the original method token, so capture it before conversion.
        let method = preserved_reserved_method(&req.inner().method()).ok_or_else(|| {
            worker::Error::RustError("reserved APS request method is invalid".to_string())
        })?;
        let mut request = edgezero_adapter_cloudflare::request::into_core_request(req, env, ctx)
            .await
            .map_err(|error| worker::Error::RustError(error.to_string()))?;
        *request.method_mut() = method;
        let response = app::dispatch_reserved(request)
            .await
            .map_err(|error| worker::Error::RustError(error.to_string()))?
            .ok_or_else(|| {
                worker::Error::RustError(
                    "reserved APS path has no coordinated-cutover handler".to_string(),
                )
            })?;
        return edgezero_adapter_cloudflare::response::from_core_response(response)
            .map_err(|error| worker::Error::RustError(error.to_string()));
    }

    match edgezero_adapter_cloudflare::run_app::<app::TrustedServerApp>(req, env, ctx).await {
        Ok(resp) => Ok(resp),
        Err(e) => {
            log::error!("worker dispatch error: {e:?}");
            Response::error("internal server error", 500)
        }
    }
}

#[cfg(all(test, feature = "aps-runner-proxy-integration-test"))]
mod tests {
    use super::preserved_reserved_method;

    #[test]
    fn reserved_method_parser_preserves_extension_methods() {
        let method = preserved_reserved_method("PROPFIND")
            .expect("should preserve a syntactically valid extension method");

        assert_eq!(method.as_str(), "PROPFIND");
    }
}
