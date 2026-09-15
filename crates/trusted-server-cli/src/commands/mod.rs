pub(crate) mod audit;
pub(crate) mod config;
// `dev` is `pub` so the macOS-gated `tests/proxy_e2e.rs` suite can reach
// `commands::dev::proxy`, and `origin` is `pub` so `tests/origin_probe.rs` can drive the
// shareability probe against a local fixture; the other command modules are
// crate-internal.
pub mod dev;
pub mod origin;
