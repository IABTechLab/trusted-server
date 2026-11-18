# Adapter store inventory and wiring: implementation plan

Spec: [2026-11-18-adapter-store-inventory-design.md](../specs/2026-11-18-adapter-store-inventory-design.md).

Base: `spec/edgezero-pr275-migration` (#1506). This work stacks on the cutover
because #1506 rewrites every adapter `platform.rs` and `edgezero.toml`.

Each task follows red, then green, then refactor. A task's verify command must
pass before the next task starts.

## Task 1: Shared `KvHandle` bridge in core

- Add `KvHandleStore(KvHandle)` to `crates/trusted-server-core/src/platform/kv.rs`
  and export it from `platform`. Every trait method, including `exists`,
  forwards through the handle, so the handle's validation runs first.
- Tests:
  - A round trip of put, get, exists, list and delete.
  - An empty key returns `KvError::Validation`, and the backend's call count
    stays at zero. This distinguishes forwarding through the handle from
    forwarding straight to the store.
- Verify: `cargo test -p trusted-server-core --lib platform::kv`.

## Task 2: Fastly uses the declared handle

- In `build_per_request_services`, use `ctx.kv_store_default()` wrapped in
  `KvHandleStore`. Fall back to `state.default_kv_store` only when the request
  has no registry. Production always has one. Test contexts that inject
  `default_kv_store` (the trace tests) do not, so they keep observing the
  injected store, and the fallback's doc comment says so.
- `fastly.toml`: add a local `[[local_server.kv_stores.trusted_server_kv]]`
  placeholder and remove `creative_store`.
- Viceroy template: remove `creative_store`, `opid_store` and
  `ec_partner_store`.
- `integration-tests/tests/common/config.rs`: also assert that `fastly.toml`
  defines `trusted_server_kv`.
- Tests, all under Viceroy:
  1. Build a context whose `KvRegistry` is keyed by
     `TrustedServerApp::stores().kv` (default id and ids) and holds
     `FastlyKvStore::open(<that id>)`. Write through `services.kv_store()`,
     then read the value back through an independently opened
     `FastlyKvStore`. This proves the write reached the linked store.
  2. A context without a registry returns `KvError::Unavailable`.
- Verify: `cargo test-fastly` and `cargo test-fastly-reuse`.

## Task 3: Cloudflare opens `TRUSTED_SERVER_KV`

- `platform.rs`:
  - Add `pub const CLOUDFLARE_KV_BINDING: &str = "TRUSTED_SERVER_KV"`. It is
    public (`platform` is a public module) because the wasm branch is the only
    production use, and `pub(crate)` would be `dead_code` on the host.
  - On wasm, open `CloudflareKvStore::from_env(env, CLOUDFLARE_KV_BINDING)` per
    request and wrap it in `KvHandleStore`. If the open fails, log one warning
    per isolate (`AtomicBool`) and use `UnavailableKvStore`. The host build
    uses `UnavailableKvStore`.
  - Remove the private `KvHandleAdapter` and the unused
    `kv_store_default()` branch. Correct the doc comments that say `run_app`
    injects stores from `cloudflare.toml`.
- `cloudflare.toml`: add a header saying it is a reference manifest that the
  Worker runtime does not read (D6).
- Tests:
  - `tests/config_defaults.rs` asserts that both `wrangler.toml` and
    `wrangler.ci.toml` bind `CLOUDFLARE_KV_BINDING`.
  - A route test: discovery returns `500`, and `/verify-signature` with a
    well-formed body returns `200` with `verified: false`. It is a
    documentation pin.
- Verify: `cargo test-cloudflare`, `cargo clippy-cloudflare`,
  `cargo clippy-cloudflare-wasm` and `cargo check-cloudflare`.

## Task 4: Spin KV declaration and request-time config

- `app.rs`: `Hooks::stores()` declares only
  `kv: Some(StoreMetadata { default: SPIN_KV_STORE_ID, ids: &[SPIN_KV_STORE_ID] })`,
  where `SPIN_KV_STORE_ID = "trusted_server_kv"`.
- `platform.rs`:
  - Use `KvHandleStore` for `ctx.kv_store_default()`.
  - On wasm, the config store is a `ConfigStoreHandleAdapter` over
    `SpinConfigStore::for_label("default".to_owned())`; on the host it is
    `NoopConfigStore`.
  - Remove the private `KvHandleAdapter`.
  - Gate `NoopConfigStore` to non-wasm builds, and `ConfigStoreHandleAdapter`
    plus its `ConfigStoreHandle` import to `any(test, wasm+spin)`. Without
    that, each target's clippy reports dead code.
- `spin.toml` comments:
  - Request-time config and signing metadata come from `default`.
  - `trusted_server_kv` is an opt-in label, with an example.
  - The label override must never name `default`.
- `tests/routes.rs`:
  - Reword the manifest test messages that claim config and secret injection.
  - Add a test that `TrustedServerApp::stores()` declares only KV, with the id
    `trusted_server_kv`.
- Integration: refactor the Spin branch of `with_trace_runtime` into a helper
  that accepts extra `default` key-value pairs. The existing branch passes
  none.
  - Add `#[ignore]`d `spin_runtime_discovery_reads_signing_metadata_from_default`
    (the Spin CI job runs ignored `spin_runtime_` tests). It seeds
    `current-kid`, `active-kids` and one example JWK and asserts discovery
    returns `200` with that `kid`.
  - This test fails on the base branch, because `NoopConfigStore` yields
    `500`.
- Verify:
  - `cargo test-spin`, `cargo clippy-spin-native`, `cargo clippy-spin-wasm`
    and `cargo check-spin`.
  - The Spin wasm build, then the new integration test with Spin 4.1.0.

## Task 5: Axum opt-in persistent KV

- `platform.rs`:
  - `fn kv_store_from_path(path: Option<&Path>) -> Arc<dyn PlatformKvStore>`
    opens `PersistentKvStore` behind `BlockingKvStore`. Without a path, or
    when the open fails, it logs a warning and returns `UnavailableKvStore`.
  - `fn kv_store_from_env()` reads `TRUSTED_SERVER_KV_PATH` and calls it.
  - `build_runtime_services` caches the result in a `OnceLock`.
  - `BlockingKvStore` runs each call with `spawn_blocking` plus `block_on`
    when `tokio::runtime::Handle::try_current()` succeeds, and inline
    otherwise.
- Tests:
  - A temp-dir path round-trips a value.
  - Reopening the path after a drop reads the value back.
  - `None` gives `Unavailable`.
  - A path under a regular file gives `Unavailable`, not a panic.
  - `kv_store_from_env` under `temp_env` covers both the set and unset cases.
  - A round trip under `futures::executor::block_on`, outside any runtime.
  - Add `tempfile` (workspace) to the dev-dependencies.
- Verify: `cargo test-axum` and `cargo clippy-axum`.

## Task 6: Inventory and documentation

- `docs/guide/edgezero.md`: the store inventory table (spec "Inventory").
- `docs/guide/cloudflare.md`:
  - Correct the KV contract (D1).
  - Signing, EC lookup and key rotation are unsupported, with the documented
    results (D3).
  - The config-push namespace and the reserved key (D5).
- `docs/guide/spin.md`:
  - Request-time config comes from `default`, including the DataDome effect
    and key collisions (D2).
  - `ts provision` adds every `edgezero.toml` KV and config label, including
    `trusted_server_kv`, to `key_value_stores`, after which `spin up` needs
    runtime config for them.
  - Correct line 89: CI now installs Spin.
  - Signing private keys use encoded variables.
  - The opt-in KV label and the override warning (D7).
  - Correct lines 84-86.
- `docs/guide/axum-dev.md`: describe `TRUSTED_SERVER_KV_PATH`, and correct the
  reason EC lookup and key rotation are unsupported.
- `docs/guide/fastly.md`: the purpose of `trusted_server_kv`, plus the
  dedicated-store requirement (D7).
- `docs/guide/api-reference.md`: add a footnote for the Cloudflare discovery
  and `/verify-signature` cells that gives the actual results. The cells must
  not use the `unsupported` (501) disposition.
- `docs/guide/architecture.md` (lines 57 and 70) and the crate READMEs for
  Axum, Cloudflare and Spin.
- `trusted-server.example.toml`: fix the `ec_store` comment that links it to
  `trusted_server_kv`.
- Verify:
  - `cd docs && npm run format` and the Markdown prettier gate, both with
    docs' pinned prettier.
  - The domain lint on changed lines.

## Task 7: Full gate and runtime verification

1. Run the CI gate list from `CLAUDE.md` with Rust 1.98.1, Node 24.20.0 and
   Viceroy 0.21.0.
2. Runtime verification (D8): Viceroy, `wrangler dev --local`, `spin up` and
   Axum, each with and without the KV binding.
   - Use a temporary probe build, never committed, to read and write KV.
   - Also run a Spin signed auction against a stub bidder.
   - Also send a publisher request on each adapter.
