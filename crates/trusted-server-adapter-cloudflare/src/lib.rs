pub mod app;
pub mod middleware;
pub mod platform;

#[cfg(target_arch = "wasm32")]
use worker::*;

#[cfg(target_arch = "wasm32")]
#[event(fetch)]
pub async fn main(req: Request, env: Env, ctx: Context) -> Result<Response> {
    match edgezero_adapter_cloudflare::run_app::<app::TrustedServerApp>(
        include_str!("../cloudflare.toml"),
        req,
        env,
        ctx,
    )
    .await
    {
        Ok(resp) => Ok(resp),
        Err(e) => Response::error(format!("worker dispatch error: {e}"), 500),
    }
}
