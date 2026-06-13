//! Fermyon Spin adapter for Trusted Server.

pub mod app;
pub mod middleware;
pub mod platform;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http::{IncomingRequest, IntoResponse};
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http_component;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
#[http_component]
// FORCED: edgezero_adapter_spin::run_app returns anyhow::Result — EdgeZero SDK constraint, not a project choice.
async fn handle(req: IncomingRequest) -> anyhow::Result<impl IntoResponse> {
    edgezero_adapter_spin::run_app::<app::TrustedServerApp>(include_str!("../edgezero.toml"), req)
        .await
}
