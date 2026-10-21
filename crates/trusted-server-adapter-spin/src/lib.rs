//! Fermyon Spin adapter for Trusted Server.

pub mod app;
pub mod middleware;
pub mod platform;
mod startup;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http::Request;
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http_service;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
#[http_service]
// EdgeZero requires anyhow only at this FFI boundary.
async fn handle(req: Request) -> anyhow::Result<edgezero_adapter_spin::SpinResponse> {
    // Bootstrap errors are pre-admission exceptions, not routed readiness pages.
    let app = startup::initialize().await.map_err(|error| {
        anyhow::anyhow!(
            "Spin bootstrap failed before dispatch: {}",
            error.current_context()
        )
    })?;
    edgezero_adapter_spin::dispatch_app::<app::TrustedServerApp>(&app, req).await
}
