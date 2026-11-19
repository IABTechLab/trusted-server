# Adapter store inventory and wiring: implementation plan

Spec: [2026-11-18-adapter-store-inventory-design.md](../specs/2026-11-18-adapter-store-inventory-design.md).

Current base: `feat/1573-axum-production-serving` (#1635), merged without
rebasing. The [rework plan](./2026-11-19-adapter-store-inventory-rework.md)
supersedes the original Axum file-store task and local full-gate instruction.

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

## Task 5: Axum deliberately unsupported KV

- Preserve #1635's immutable prepared config/secret snapshots and unavailable KV.
- Remove the adapter-local file implementation and production `bytes` dependency;
  keep `tempfile` for the production startup tests and do not restore `temp-env`.
- Test unavailable reads and writes through an actual request context.
- Runtime: production binary starts from prepared stores and serves publisher
  pages while reporting KV unavailable. Support is deferred to #1587 → #1574.

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
- `docs/guide/axum-dev.md`: document unsupported KV pending #1587 → #1574,
  prepared-store startup, and unsupported EC lookup and key rotation.
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

## Task 7: Targeted and runtime verification

Follow the rework plan's targeted test and real-runtime matrix. Full CI gates
run in GitHub Actions, not locally. Preserve fixture-driven infrastructure
proof separately from production identity persistence.
