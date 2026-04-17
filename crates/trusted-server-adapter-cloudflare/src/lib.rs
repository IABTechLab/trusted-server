pub mod app;
pub mod platform;

#[cfg(all(feature = "cloudflare", target_arch = "wasm32"))]
use worker::*;

#[cfg(all(feature = "cloudflare", target_arch = "wasm32"))]
#[event(fetch)]
pub async fn main(
    req: Request,
    env: Env,
    ctx: Context,
) -> Result<Response> {
    edgezero_adapter_cloudflare::run_app::<app::TrustedServerApp>(req, env, ctx).await
}
