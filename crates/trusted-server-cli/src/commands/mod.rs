pub(crate) mod audit;
pub(crate) mod config;
pub(crate) mod pbs;
// `dev` is `pub` so the macOS-gated `tests/proxy_e2e.rs` suite can reach
// `commands::dev::proxy`, `origin` is `pub` so `tests/origin_probe.rs` can drive the
// shareability probe against a local fixture, and `cache` and `ec` are `pub` so
// `tests/cache_purge.rs` and `tests/ec_admin.rs` can drive their admin calls against one;
// the other command modules are crate-internal.
pub mod cache;
pub mod dev;
pub mod ec;
pub mod origin;
