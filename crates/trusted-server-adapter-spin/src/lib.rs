//! Fermyon Spin adapter for Trusted Server.

pub mod app;
pub mod middleware;
pub mod platform;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http::{IntoResponse, Request};
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http_service;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
#[http_service]
// FORCED: edgezero_adapter_spin::run_app returns anyhow::Result — EdgeZero SDK constraint, not a project choice.
async fn handle(req: Request) -> anyhow::Result<impl IntoResponse> {
    if trusted_server_core::integrations::aps::is_aps_family_path(req.uri().path()) {
        let request = edgezero_adapter_spin::request::into_core_request(req).await?;
        let response = app::dispatch_reserved(request)
            .await
            .map_err(|error| anyhow::anyhow!("{error:?}"))?
            .expect("reserved APS path should dispatch before RouterService");
        return edgezero_adapter_spin::response::from_core_response(response)
            .await
            .map_err(Into::into);
    }
    edgezero_adapter_spin::run_app::<app::TrustedServerApp>(req).await
}
