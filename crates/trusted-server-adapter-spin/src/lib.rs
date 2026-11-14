//! Fermyon Spin adapter for Trusted Server.

// Unit tests may buffer bodies with `Body::into_bytes`; production code must not.
#![cfg_attr(test, allow(clippy::disallowed_methods))]

pub mod app;
pub mod middleware;
pub mod platform;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http::Request;
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http_service;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
#[http_service]
// FORCED: edgezero_adapter_spin::run_app returns anyhow::Result — EdgeZero SDK constraint, not a project choice.
async fn handle(req: Request) -> anyhow::Result<edgezero_adapter_spin::SpinResponse> {
    edgezero_adapter_spin::run_app::<app::TrustedServerApp>(req).await
}
