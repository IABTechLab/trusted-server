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

#[cfg(target_arch = "wasm32")]
#[event(fetch)]
pub async fn main(req: Request, env: Env, ctx: Context) -> Result<Response> {
    match edgezero_adapter_cloudflare::run_app::<app::TrustedServerApp>(req, env, ctx).await {
        Ok(resp) => Ok(resp),
        Err(e) => {
            log::error!("worker dispatch error: {e:?}");
            Response::error("internal server error", 500)
        }
    }
}
