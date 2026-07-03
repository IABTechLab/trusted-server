# EdgeZero Store-Registry Migration (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route trusted-server's runtime **config and secret reads** through EdgeZero's `ConfigRegistry`/`SecretRegistry` (as KV already is), reconcile all logical store ids with `edgezero.toml`, and delete the duplicated Fastly chunk resolver — without breaking the runtime **write** path (key rotation) or Fastly's custom dispatch.

**Architecture:** trusted-server core reads stores through the bespoke `PlatformConfigStore`/`PlatformSecretStore` traits (read `get`/`get_string` + write `put`/`create`/`delete`), surfaced via `RuntimeServices`. EdgeZero's `ConfigStore`/`SecretStore` are **read-only**; the per-request `ConfigRegistry`/`SecretRegistry` live in request extensions. This phase makes core reads resolve from those registries while **keeping** the write-capable path until D6 decides its fate. Every adapter must wire the registries, including Fastly's custom `oneshot` path.

**Tech Stack:** Rust 2024, `error-stack` (`Report<TrustedServerError>`), EdgeZero (`edgezero-core` git dep), Viceroy (Fastly test sim), `cargo test-{fastly,axum,cloudflare,spin}`.

**Spec:** `docs/superpowers/specs/2026-07-02-edgezero-full-migration-design.md` §5 Phase 1, decisions D5 + D6, §4a.

## Global Constraints

- Rust **2024 edition**, toolchain **1.95.0** (`rust-toolchain.toml`); WASM target `wasm32-wasip1`.
- Errors: `error-stack` `Report<E>` only (no `anyhow` outside the Spin entry point); `derive_more::Display` for error types; import `Error` from `core::error::`.
- No `unwrap()` in production; `expect("should …")`. No `println!`/`eprintln!`; use `log` macros.
- No wildcard imports (except `use super::*` in `#[cfg(test)]`). No local imports inside functions.
- Commit style: sentence case, imperative, no semantic prefixes, no `Co-Authored-By`/AI footers.
- CI gate (must pass before PR): `cargo fmt --all -- --check`; `cargo clippy-{fastly,axum,cloudflare,spin-native,spin-wasm}`; `cargo test-{fastly,axum,cloudflare,spin}`; `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`.
- **Every phase step leaves all four adapters building and green.**
- **EdgeZero `ConfigStore`/`SecretStore` are read-only.** Never assume a runtime write API on them.
- **Registry lookup is strict:** an unknown logical id yields `None`. Every id any config field names at runtime must be declared in `edgezero.toml` `[stores.config]`/`[stores.secrets]` `ids`.

---

## Task 1: Store-capability inventory + D5/D6 decision gate (no code deletion)

This task produces decisions, not deletions. Its deliverable is a written **decision record** appended to this plan (section "Task 1 Output") that Tasks 2+ depend on. Per spec review, Phase 1 must not begin with deletions.

**Files:**
- Modify (append decision record): `docs/superpowers/plans/2026-07-02-edgezero-store-registry-migration.md`
- Read-only inventory across: `crates/trusted-server-core/src/**`, `crates/trusted-server-adapter-*/src/**`, `edgezero.toml`, `trusted-server.example.toml`, `crates/trusted-server-integration-tests/fixtures/**`

**Interfaces:**
- Produces: the **store-id map** (logical id → platform name → declared-in-edgezero.toml?) and the **write-site list** (every runtime `put`/`create`/`delete` call), consumed by Tasks 2, 3, 6.

- [ ] **Step 1: Enumerate every logical store id referenced at runtime**

Run:
```bash
cd /Users/ag/projects/iab/trusted-server/.claude/worktrees/edgezero-migration-spec
rg -n 'config_store_id|secret_store_id|secret_store\s*=|config_store\s*=|StoreName::from|StoreId::from|"app_config"|"secrets"|"jwks|ts_secrets|signing_keys|"api-keys"' \
  crates/trusted-server-core crates/trusted-server-adapter-* trusted-server.example.toml \
  crates/trusted-server-integration-tests/fixtures
rg -n '\[stores\.' edgezero.toml
```
Expected: a list of ids including at least `app_config`, `secrets`, `signing_keys`, JWKS config-list store, DataDome `ts_secrets`, S3 secret store — versus `edgezero.toml` declaring only `trusted_server_config`/`trusted_server_kv`/`trusted_server_secrets`.

- [ ] **Step 2: Enumerate every runtime store WRITE call site**

Run:
```bash
rg -n '\.config_store\(\)\.(put|delete)|\.secret_store\(\)\.(create|delete)' crates/trusted-server-core
```
Expected: the `KeyRotationManager` write sites in `crates/trusted-server-core/src/request_signing/rotation.rs` (`store_private_key`, `store_public_jwk`, `deactivate_key`, `delete_key`) — the only runtime writers. Confirm no other runtime writers exist.

- [ ] **Step 3: Record the D5 decision (store-id reconciliation map)**

Append to "Task 1 Output" a table: each runtime logical id → chosen resolution (declare a new id in `edgezero.toml`, or collapse onto `trusted_server_config`/`trusted_server_secrets`) → the `EDGEZERO__STORES__<KIND>__<ID>__NAME` mapping. Default recommendation from the spec (D5): app-config blob in `trusted_server_config` key `app_config`; declare JWKS as its own config id; collapse `secrets`→`trusted_server_secrets`; keep DataDome/S3 as declared secret ids. Confirm or adjust against Step 1's actual list.

- [ ] **Step 4: Record the D6 decision (runtime write path)**

Append to "Task 1 Output" the chosen option: **(a)** keep a write-capable admin abstraction (`management_api.rs` + the `put`/`create`/`delete` trait methods stay for the admin path); **(b)** move rotate/deactivate/delete to an ops/CLI command; or **(c)** upstream an EdgeZero write API. Spec recommendation: **(a) as the Phase 1 interim** (unblocks the read migration without changing the admin surface), with (b) as the target end-state pending an ops decision. Record which is chosen; Tasks 3–6 assume **(a)** unless this step records otherwise.

- [ ] **Step 5: Commit the decision record**

```bash
git add docs/superpowers/plans/2026-07-02-edgezero-store-registry-migration.md
git commit -m "Record Phase 1 store-id map and runtime-write decision (D5, D6)"
```

---

## Task 2: Declare all runtime store ids in `edgezero.toml` + reconcile config fields/fixtures

Implements the D5 map from Task 1. Makes every referenced id resolvable so strict registry lookup never returns `None`.

**Files:**
- Modify: `edgezero.toml` (`[stores.config]`/`[stores.secrets]` `ids`)
- Modify: `trusted-server.example.toml` (`request_signing.config_store_id`, `secret_store_id`, and any other store-id fields to match the D5 map)
- Modify: `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml`
- Test: `crates/trusted-server-core/src/settings.rs` (`#[cfg(test)]`) — assert declared ids cover the config's referenced ids

**Interfaces:**
- Consumes: Task 1 store-id map.
- Produces: an `edgezero.toml` whose `[stores.*].ids` is a superset of every id named by `Settings`.

- [ ] **Step 1: Write the failing test**

Add to `crates/trusted-server-core/src/settings.rs` under `#[cfg(test)]`:
```rust
#[test]
fn every_referenced_store_id_is_declared() {
    // Arrange: parse the example config and the manifest's declared ids.
    let settings = Settings::from_toml(include_str!("../../../trusted-server.example.toml"))
        .expect("should parse example config");
    let declared = declared_store_ids_from_manifest(); // helper reads edgezero.toml
    // Act: collect the store ids the settings reference.
    let referenced = settings.referenced_store_ids();
    // Assert: manifest declares every referenced id.
    for id in &referenced {
        assert!(
            declared.contains(id),
            "store id `{id}` referenced by Settings is not declared in edgezero.toml",
        );
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test-fastly every_referenced_store_id_is_declared`
Expected: FAIL — `app_config`/`secrets`/JWKS ids referenced but not declared.

- [ ] **Step 3: Add `Settings::referenced_store_ids()` + the manifest helper**

In `settings.rs`, implement `referenced_store_ids(&self) -> std::collections::BTreeSet<String>` returning every `*_store_id` / `*_store` value (request-signing config+secret ids, DataDome, S3, EC, consent). Add a test-only `declared_store_ids_from_manifest()` that parses `edgezero.toml`'s `[stores.config]`/`[stores.secrets]` `ids`.

- [ ] **Step 4: Update `edgezero.toml` + config fields per the D5 map**

Edit `edgezero.toml` `[stores.config].ids` / `[stores.secrets].ids` to declare every id from the map; update `trusted-server.example.toml` and the integration fixture so their store-id fields use declared ids.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test-fastly every_referenced_store_id_is_declared`
Expected: PASS.

- [ ] **Step 6: Run all adapter tests + commit**

Run: `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin`
Expected: PASS.
```bash
git add edgezero.toml trusted-server.example.toml crates/trusted-server-integration-tests/fixtures crates/trusted-server-core/src/settings.rs
git commit -m "Declare all runtime store ids in edgezero.toml and reconcile config fields"
```

---

## Task 3: Bridge `RuntimeServices` config/secret READS to EdgeZero registries

Make `RuntimeServices::config_store()`/`secret_store()` **reads** resolve from the request's `ConfigRegistry`/`SecretRegistry` instead of the per-adapter `Platform*Store` read impls. The write methods (`put`/`create`/`delete`) stay routed to the existing path (D6 option a).

**Files:**
- Modify: `crates/trusted-server-core/src/platform/types.rs` (`RuntimeServices` build/accessors)
- Modify: `crates/trusted-server-core/src/platform/traits.rs` (split read vs write if needed)
- Test: `crates/trusted-server-core/src/platform/types.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: EdgeZero `ConfigRegistry`/`SecretRegistry` (from request extensions), `edgezero_core::config_store::ConfigStoreHandle`, `edgezero_core::store_registry::BoundSecretStore`.
- Produces: a `RuntimeServices` whose `config_store().get(name,key)` / `secret_store().get_string(name,key)` read through EdgeZero; write methods unchanged.

- [ ] **Step 1: Write the failing test** (config read resolves via an EdgeZero-backed registry)

```rust
#[test]
fn runtime_services_config_read_resolves_via_edgezero_registry() {
    // Arrange: a RuntimeServices built from an EdgeZero ConfigRegistry with a fixed value.
    let services = runtime_services_with_config_registry("trusted_server_config", "greeting", "hi");
    // Act
    let value = services
        .config_store()
        .get(&StoreName::from("trusted_server_config"), "greeting")
        .expect("should read via edgezero registry");
    // Assert
    assert_eq!(value, "hi", "should return the EdgeZero-backed value");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test-fastly runtime_services_config_read_resolves_via_edgezero_registry`
Expected: FAIL (no such constructor / still uses the old read path).

- [ ] **Step 3: Implement the read bridge**

Add an EdgeZero-backed `PlatformConfigStore`/`PlatformSecretStore` read adapter in `platform/` that wraps a `ConfigStoreHandle`/`BoundSecretStore` resolved from the registry, mapping `edgezero_core` errors → `PlatformError`. Route `RuntimeServices::config_store()/secret_store()` reads through it; keep `put`/`create`/`delete` delegating to the existing write impl (per D6-a). Reads use `block_on` on the async EdgeZero handle (mirrors `storage/kv_store.rs`).

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test-fastly runtime_services_config_read_resolves_via_edgezero_registry`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/platform/
git commit -m "Resolve RuntimeServices config/secret reads via EdgeZero registries"
```

---

## Task 4: Wire registries in the standard adapters (Axum, Cloudflare, Spin)

These adapters use `dispatch_with_registries`; ensure `Config`/`Secret` registries are built from `[stores.*]` metadata and reach `build_runtime_services`.

**Files:**
- Modify: `crates/trusted-server-adapter-axum/src/platform.rs`, `.../adapter-cloudflare/src/platform.rs`, `.../adapter-spin/src/platform.rs`
- Test: each adapter's route tests

**Interfaces:**
- Consumes: Task 3's read bridge; `StoresMetadata` from `Hooks::stores()`.
- Produces: `RuntimeServices` on these adapters whose reads flow through EdgeZero registries.

- [ ] **Step 1: Write a failing route test (per adapter) that reads a config/secret value through a handler**

For Axum, add a test hitting a route whose handler reads a known config value; assert 200 + expected body. (Mirror existing `adapter-axum` route tests.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test-axum <test_name>`
Expected: FAIL.

- [ ] **Step 3: Build `Config`/`Secret` registries in each adapter's `build_runtime_services`**

Use the EdgeZero registries the adapter's `dispatch_with_registries` already inserts into request extensions; construct `RuntimeServices` via Task 3's bridge instead of the old `Platform*Store` read impls.

- [ ] **Step 4: Run per-adapter tests to verify pass**

Run: `cargo test-axum && cargo test-cloudflare && cargo test-spin`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-adapter-axum crates/trusted-server-adapter-cloudflare crates/trusted-server-adapter-spin
git commit -m "Wire EdgeZero config/secret registries in Axum, Cloudflare, and Spin adapters"
```

---

## Task 5: Fastly-specific registry injection into the custom `oneshot` path

Fastly bypasses `dispatch_with_registries` (inserts only a `ConfigStoreHandle` before `app.router().oneshot()`). Add explicit `Config`/`Secret`/`Kv` registry construction + insertion compatible with that path.

**Files:**
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs` (the `oneshot` dispatch block, ~L470–490)
- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs` (registry builders)
- Test: `crates/trusted-server-adapter-fastly/src/route_tests.rs`

**Interfaces:**
- Consumes: Task 3 bridge; the same registry builders the standard path uses.
- Produces: Fastly requests whose extensions carry `ConfigRegistry`/`SecretRegistry`/`KvRegistry`, resolvable by `RuntimeServices`.

- [ ] **Step 1: Write a failing Fastly route test** that exercises a handler reading a config value and asserts the EdgeZero-backed value is returned.

Run: `cargo test-fastly <test_name>` → Expected: FAIL.

- [ ] **Step 2: Build + insert the registries before `oneshot`**

In `main.rs`, replace the lone `core_req.extensions_mut().insert(config_store)` with construction of `ConfigRegistry`/`SecretRegistry`/`KvRegistry` (from `[stores.*]` metadata + `EnvConfig`) and insert each into `core_req.extensions_mut()`, preserving the existing `client_info`/`device_signals` inserts.

- [ ] **Step 3: Run to verify pass**

Run: `cargo test-fastly <test_name>` → Expected: PASS.

- [ ] **Step 4: Full Fastly suite + parity + commit**

Run: `cargo test-fastly && cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`
Expected: PASS.
```bash
git add crates/trusted-server-adapter-fastly
git commit -m "Inject EdgeZero registries into the Fastly custom oneshot dispatch path"
```

---

## Task 6: Delete the duplicated Fastly chunk resolver

`settings_data.rs`'s `FastlyChunkPointer` resolver duplicates EdgeZero's `FastlyConfigStore` chunk handling. With reads flowing through EdgeZero (Task 3–5), collapse `get_settings_from_config_store` to a plain `ConfigStore::get` + `settings_from_config_blob`.

**Files:**
- Modify: `crates/trusted-server-core/src/settings_data.rs`
- Test: `crates/trusted-server-core/src/settings_data.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: EdgeZero-backed config read (Task 3).
- Produces: a chunk-free `get_settings_from_config_store`.

- [ ] **Step 1: Confirm the existing multi-chunk test now passes against the EdgeZero-resolved value** (EdgeZero's `FastlyConfigStore` reassembles chunks). If a `settings_data` test asserts the local resolver's behavior, rewrite it to assert the blob is read + parsed, not chunk-reassembled.

- [ ] **Step 2: Delete `FastlyChunkPointer`, `FastlyChunkRef`, `resolve_fastly_chunk_pointer`, `sha256_hex`, and the chunk constants**; collapse `get_settings_from_config_store` to `ConfigStore::get` + `settings_from_config_blob`.

- [ ] **Step 3: Run tests to verify pass**

Run: `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-core/src/settings_data.rs
git commit -m "Delete duplicated Fastly config-chunk resolver; read via EdgeZero FastlyConfigStore"
```

---

## Task 7: Retire the per-adapter config/secret READ impls; keep the write path (D6-a)

Delete the four `platform.rs` config/secret **read** implementations now that reads flow through EdgeZero. Keep the write-capable path (`management_api.rs` + `put`/`create`/`delete`) per D6-a, or execute D6-b/c if Task 1 chose it.

**Files:**
- Modify: `crates/trusted-server-adapter-{fastly,axum,cloudflare,spin}/src/platform.rs`
- Modify (only if D6-b chosen): `crates/trusted-server-adapter-fastly/src/management_api.rs`, request-signing endpoints

**Interfaces:**
- Consumes: Tasks 3–5.
- Produces: adapters with no bespoke config/secret **read** impls; write path intact per D6.

- [ ] **Step 1: Delete the config/secret read impls** (`FastlyPlatformConfigStore::get`, `AxumPlatformConfigStore`, `NoopConfigStore`, Cloudflare/Spin equivalents, and secret read impls) that are now unused after Tasks 4–5. Keep the write impls (D6-a).

- [ ] **Step 2: If Task 1 chose D6-b** (move rotation to ops/CLI): delete `management_api.rs` and the `put`/`create`/`delete` trait methods; move `KeyRotationManager` writes behind a `ts keys` CLI command using EdgeZero provisioning; make the runtime endpoints return `501`/redirect per the ops decision. **Otherwise skip this step.**

- [ ] **Step 3: Run the full CI gate**

Run: `cargo fmt --all -- --check && cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare && cargo clippy-spin-native && cargo clippy-spin-wasm && cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin && cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`
Expected: PASS. Key rotation/delete still works (per D6 resolution).

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-adapter-*
git commit -m "Retire per-adapter config/secret read impls; reads flow through EdgeZero registries"
```

---

## Task 1 Output (filled in during execution)

_D5 store-id map and D6 decision are recorded here by Task 1 before Tasks 2+ run._

---

## Notes on scope and gating

- **Blocked-until-decided:** Tasks 3–7 assume D6-a (keep the write path). If Task 1 selects D6-b, Task 7 Step 2 activates and the `management_api.rs` deletion (spec ledger, conditional) proceeds; if D6-c, add an upstream-EdgeZero prerequisite task before Task 7.
- **Not in this phase:** `RuntimeServices` full removal (Phase 4), `include_str!` config removal on Cloudflare/Spin (Phase 2), `from_toml_and_env`/`config`-dep removal (Phase 2), `Redacted<T>` / secret externalization (Phase 3).
- **No dependency on edgezero #305** — Phase 1 uses only the already-shipped EdgeZero store APIs.
