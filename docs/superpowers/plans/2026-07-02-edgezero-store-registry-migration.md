# EdgeZero Store-Registry Migration (Phase 1, D6-a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route trusted-server's runtime **and boot-time** config/secret **reads** through EdgeZero stores/registries (as KV already is), reconcile every logical store id (kv/config/secrets) with `edgezero.toml`, and delete the duplicated Fastly chunk resolver — while **keeping** the runtime **write** path (key rotation) intact via a composite store (decision **D6-a**).

**Architecture:** trusted-server core reads/writes stores through the bespoke `PlatformConfigStore`/`PlatformSecretStore` traits (each mixes read `get`/`get_string` + write `put`/`create`/`delete`), surfaced via `RuntimeServices` (one trait object per kind). EdgeZero's `ConfigStore`/`SecretStore` are **read-only**; per-request `ConfigRegistry`/`SecretRegistry` live in request extensions. This phase introduces a **composite store** whose *reads* resolve from EdgeZero and whose *writes* delegate to the existing management-API-backed impl, migrates the Fastly/Axum **boot** config read to EdgeZero, and adds **local** registry builders for Fastly's custom `oneshot` dispatch (EdgeZero's builders are `pub(crate)`).

**Tech Stack:** Rust 2024, toolchain 1.95.0, `error-stack` `Report<TrustedServerError>`, EdgeZero (`edgezero-core`/`edgezero-adapter-fastly` git dep), Viceroy, `cargo test-{fastly,axum,cloudflare,spin}`.

**Spec:** `docs/superpowers/specs/2026-07-02-edgezero-full-migration-design.md` §5 Phase 1, D5, D6, §4a.

## Global Constraints

- Rust **2024 edition**, toolchain **1.95.0**; WASM target `wasm32-wasip1`.
- Errors: `error-stack` `Report<E>` only (no `anyhow` outside the Spin entry point); `derive_more::Display`; import `Error` from `core::error::`.
- No `unwrap()` in production (`expect("should …")`); no `println!`/`eprintln!` (use `log`).
- No wildcard imports (except `use super::*` in `#[cfg(test)]`); no imports inside functions.
- Commits: sentence case, imperative, no semantic prefixes, no `Co-Authored-By`/AI footers.
- CI gate before PR: `cargo fmt --all -- --check`; `cargo clippy-{fastly,axum,cloudflare,spin-native,spin-wasm}`; `cargo test-{fastly,axum,cloudflare,spin}`; `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`.
- **Every task leaves all four adapters building and green.**
- **EdgeZero `ConfigStore`/`SecretStore` are read-only.** Runtime writes stay on the management path (D6-a).
- **Registry lookup is strict:** an unknown logical id yields `None`. Every id any config field names — in **any** kind (kv/config/secrets) — must be declared in `edgezero.toml`.
- **This plan is D6-a-locked.** If Task 1 selects D6-b (move key rotation to ops/CLI) or D6-c (upstream write API), **stop after Task 1** and write a separate plan — those change the admin API surface and are out of Phase 1 scope.

---

## Task 1: Kind-aware store inventory + confirm D6-a (decision gate, no deletions)

Deliverable: a **decision record** appended to "Task 1 Output" that Tasks 2+ consume. No code is deleted here.

**Files:**
- Modify (append record): this plan file.
- Read-only inventory: `crates/trusted-server-core/src/**`, `crates/trusted-server-adapter-*/src/**`, `edgezero.toml`, `trusted-server.example.toml`, `crates/trusted-server-integration-tests/fixtures/**`.

**Interfaces:**
- Produces: the **kind-partitioned store-id map** (`{kv, config, secrets}` → each logical id → platform name → declared?) and the **write-site list**, consumed by Tasks 2, 3, 8.

- [ ] **Step 1: Enumerate store ids by kind**

Run:
```bash
cd /Users/ag/projects/iab/trusted-server/.claude/worktrees/edgezero-migration-spec
# KV ids
rg -n 'ec_store|consent_store|creative_store|ec_identity_store|counter_store|opid_store' crates/trusted-server-core/src/settings.rs trusted-server.example.toml
# config ids
rg -n 'config_store_id|jwks|JWKS_CONFIG_STORE_NAME|"app_config"|config_store\s*=' crates/trusted-server-core trusted-server.example.toml
# secret ids
rg -n 'secret_store_id|secret_store\s*=|"secrets"|ts_secrets|signing_keys|SIGNING_SECRET_STORE_NAME' crates/trusted-server-core trusted-server.example.toml
rg -n '\[stores\.' edgezero.toml
```
Expected: KV ids include `ec_identity_store` (from `ec.ec_store`), consent/creative/counter/opid stores; config ids include `app_config` + the JWKS store (`JWKS_CONFIG_STORE_NAME`); secret ids include `secrets`, `signing_keys`, DataDome `ts_secrets`, the S3 secret store — versus `edgezero.toml` declaring only `trusted_server_kv`/`trusted_server_config`/`trusted_server_secrets`.

- [ ] **Step 2: Enumerate runtime WRITE sites**

Run:
```bash
rg -n '\.config_store\(\)\.(put|delete)|\.secret_store\(\)\.(create|delete)' crates/trusted-server-core
```
Expected: only `KeyRotationManager` in `crates/trusted-server-core/src/request_signing/rotation.rs` (`store_private_key`, `store_public_jwk`, `deactivate_key`, `delete_key`). Confirm no other runtime writers.

- [ ] **Step 3: Record the kind-partitioned D5 map**

Append a table to "Task 1 Output": for each `{kv|config|secrets}` id → resolution (declare in `edgezero.toml`, or collapse onto the kind's default) → `EDGEZERO__STORES__<KIND>__<ID>__NAME`. Spec default: app-config blob → config id `trusted_server_config` key `app_config`; JWKS → its own config id; `ec_identity_store` → kv id; collapse `secrets`→`trusted_server_secrets` where identical; declare DataDome/S3/signing as distinct secret ids.

- [ ] **Step 4: Confirm D6-a (or STOP)**

Confirm this phase keeps the write-capable composite (D6-a). Record it. **If the team instead chooses D6-b/c, stop here** and open a separate plan (`…-key-rotation-ops-migration.md`); do not proceed to Task 2.

- [ ] **Step 5: Commit the record**

```bash
git add docs/superpowers/plans/2026-07-02-edgezero-store-registry-migration.md
git commit -m "Record Phase 1 kind-aware store-id map and confirm D6-a"
```

---

## Task 2: Declare all store ids (kv/config/secrets) in `edgezero.toml` + reconcile fields/fixtures

**Files:**
- Modify: `edgezero.toml` (`[stores.kv]`, `[stores.config]`, `[stores.secrets]` `ids`)
- Modify: `trusted-server.example.toml`, `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml`
- Test: `crates/trusted-server-core/src/settings.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: Task 1 map.
- Produces: `Settings::referenced_store_ids_by_kind() -> ReferencedStoreIds { kv: BTreeSet<String>, config: BTreeSet<String>, secrets: BTreeSet<String> }`; an `edgezero.toml` whose per-kind `ids` are supersets.

- [ ] **Step 1: Write the failing test**

Add to `settings.rs` under `#[cfg(test)]`:
```rust
#[test]
fn every_referenced_store_id_is_declared_by_kind() {
    let settings = Settings::from_toml(include_str!("../../../trusted-server.example.toml"))
        .expect("should parse example config");
    let referenced = settings.referenced_store_ids_by_kind();
    let declared = declared_store_ids_by_kind_from_manifest(); // reads edgezero.toml
    for (kind, ids) in [
        ("kv", &referenced.kv),
        ("config", &referenced.config),
        ("secrets", &referenced.secrets),
    ] {
        let declared_for_kind = declared.for_kind(kind);
        for id in ids {
            assert!(
                declared_for_kind.contains(id),
                "{kind} store id `{id}` referenced by Settings is not declared in edgezero.toml",
            );
        }
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test-fastly every_referenced_store_id_is_declared_by_kind`
Expected: FAIL — `ec_identity_store` (kv), `app_config`/JWKS (config), `secrets`/`ts_secrets` (secrets) referenced but not declared.

- [ ] **Step 3: Implement `referenced_store_ids_by_kind()` + manifest helper**

Add the `ReferencedStoreIds` struct + method returning KV ids (`ec.ec_store`, consent/creative/counter/opid), config ids (`request_signing.config_store_id`, `JWKS_CONFIG_STORE_NAME`, app-config), secret ids (`request_signing.secret_store_id`, DataDome, S3, `SIGNING_SECRET_STORE_NAME`). Add test-only `declared_store_ids_by_kind_from_manifest()` parsing `edgezero.toml`.

- [ ] **Step 4: Update `edgezero.toml` + config fields/fixtures per the Task 1 map**

- [ ] **Step 5: Run to verify pass**

Run: `cargo test-fastly every_referenced_store_id_is_declared_by_kind`
Expected: PASS.

- [ ] **Step 6: Full adapter tests + commit**

Run: `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin`
```bash
git add edgezero.toml trusted-server.example.toml crates/trusted-server-integration-tests/fixtures crates/trusted-server-core/src/settings.rs
git commit -m "Declare kv/config/secret store ids in edgezero.toml and reconcile config fields"
```

---

## Task 3: Composite read/write store bridge (reads → EdgeZero, writes → management path)

Concrete D6-a mechanism. Introduce a composite that implements `PlatformConfigStore`/`PlatformSecretStore` by routing **reads** to an EdgeZero-backed handle and **writes** (`put`/`create`/`delete`) to the existing management-API-backed impl (`inner_writer`). This preserves `KeyRotationManager` writes with zero call-site changes.

**Files:**
- Create: `crates/trusted-server-core/src/platform/composite.rs` (`CompositeConfigStore`, `CompositeSecretStore`)
- Modify: `crates/trusted-server-core/src/platform/mod.rs` (export composite)
- Test: `crates/trusted-server-core/src/platform/composite.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `edgezero_core::config_store::ConfigStoreHandle`, `edgezero_core::store_registry::BoundSecretStore`, an `Arc<dyn PlatformConfigStore>`/`Arc<dyn PlatformSecretStore>` writer.
- Produces:
  - `CompositeConfigStore::new(reader: ConfigStoreHandle, writer: Arc<dyn PlatformConfigStore>) -> Self` implementing `PlatformConfigStore` (get→reader, put/delete→writer).
  - `CompositeSecretStore::new(reader: BoundSecretStore, writer: Arc<dyn PlatformSecretStore>) -> Self` implementing `PlatformSecretStore` (get_bytes→reader, create/delete→writer).

- [ ] **Step 1: Write the failing test — read via EdgeZero, write delegates to writer**

```rust
#[test]
fn composite_config_reads_edgezero_and_writes_delegate() {
    // Arrange: an EdgeZero reader returning "hi"; a recording writer.
    let reader = fixed_config_handle("greeting", "hi");
    let writer = Arc::new(RecordingConfigWriter::default());
    let composite = CompositeConfigStore::new(reader, writer.clone());
    // Act: read + write.
    let read = composite
        .get(&StoreName::from("trusted_server_config"), "greeting")
        .expect("should read via EdgeZero reader");
    composite
        .put(&StoreId::from("trusted_server_config"), "current-kid", "kid-1")
        .expect("should delegate write");
    // Assert
    assert_eq!(read, "hi", "read should come from the EdgeZero reader");
    assert_eq!(
        writer.puts.lock().expect("lock").as_slice(),
        &[("current-kid".to_owned(), "kid-1".to_owned())],
        "write should delegate to the management-path writer",
    );
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test-fastly composite_config_reads_edgezero_and_writes_delegate`
Expected: FAIL (module does not exist).

- [ ] **Step 3: Implement `composite.rs`**

Reads call the EdgeZero handle via `futures::executor::block_on` (mirror `storage/kv_store.rs`), mapping `edgezero_core` errors → `PlatformError`. Writes forward to `writer`. Repeat for `CompositeSecretStore`. Add `RecordingConfigWriter`/`fixed_config_handle` test helpers.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test-fastly composite_config_reads_edgezero_and_writes_delegate`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/platform/
git commit -m "Add composite config/secret store: EdgeZero reads, management-path writes"
```

---

## Task 4: Migrate Fastly + Axum BOOT config read to EdgeZero (before deleting bespoke impls)

`build_state()` loads `Settings` at boot via `get_settings_from_config_store(&FastlyPlatformConfigStore, …)` / `&AxumPlatformConfigStore` — **before** any request context. Migrate the boot read to an EdgeZero-backed boot reader so the bespoke impls can be deleted later (Task 8) without breaking boot. (P-BOOT option a for Fastly/Axum: `ConfigStore` opens at boot.)

**Files:**
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs:161` (`load_settings_from_config_store`)
- Modify: `crates/trusted-server-adapter-axum/src/app.rs:54` (`build_state`)
- Modify: `crates/trusted-server-core/src/settings_data.rs` (accept an EdgeZero `ConfigStoreHandle` reader)
- Test: `crates/trusted-server-adapter-fastly/src/app.rs` (`#[cfg(test)]`), Axum equivalent

**Interfaces:**
- Consumes: `edgezero_core` Fastly/Axum `ConfigStore` open primitives; Task 3 nothing (boot read is direct).
- Produces: `get_settings_from_config_store` taking `&ConfigStoreHandle` (EdgeZero) instead of `&dyn PlatformConfigStore`.

- [ ] **Step 1: Write a failing boot test (Fastly)** asserting `load_settings_from_config_store()` returns parsed `Settings` when the EdgeZero Fastly config store holds the blob (use the EdgeZero test store / a seeded local config store).

Run: `cargo test-fastly boot_config_loads_via_edgezero` → Expected: FAIL.

- [ ] **Step 2: Re-type `get_settings_from_config_store`** to take an EdgeZero `ConfigStoreHandle`; open the EdgeZero `FastlyConfigStore` at boot in `load_settings_from_config_store`, and the EdgeZero Axum config store in Axum `build_state`.

- [ ] **Step 3: Run to verify pass** (Fastly + Axum)

Run: `cargo test-fastly boot_config_loads_via_edgezero && cargo test-axum boot_config_loads_via_edgezero`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-adapter-axum/src/app.rs crates/trusted-server-core/src/settings_data.rs
git commit -m "Load boot config via EdgeZero config store on Fastly and Axum"
```

---

## Task 5: Wire request registries in Axum, Cloudflare, Spin; RuntimeServices uses the composite

These adapters use EdgeZero `dispatch_with_registries` (registries already inserted into extensions). Build `RuntimeServices` config/secret from `CompositeConfigStore`/`CompositeSecretStore` (reader from the request registry; writer = the existing per-adapter write impl).

**Files:**
- Modify: `crates/trusted-server-adapter-{axum,cloudflare,spin}/src/platform.rs` (`build_runtime_services`)
- Test: `crates/trusted-server-adapter-axum/src/app.rs` route tests (+ cloudflare/spin equivalents)

**Interfaces:**
- Consumes: Task 3 composite; `ConfigRegistry`/`SecretRegistry` from request extensions.
- Produces: `RuntimeServices` whose reads flow through EdgeZero, writes through the composite writer.

- [ ] **Step 1: Write a failing Axum route test** — `GET /.well-known/trusted-server.json` returns the JWKS/discovery document read from the config store. Name: `discovery_reads_jwks_from_edgezero_config_store` in the Axum app test module. Seed the Axum config registry with a JWKS entry fixture; assert `200` + the JWKS `kid` in the body.

Run: `cargo test-axum discovery_reads_jwks_from_edgezero_config_store` → Expected: FAIL.

- [ ] **Step 2: Build `RuntimeServices` via the composite** in each adapter's `build_runtime_services`, resolving the reader from the request `ConfigRegistry`/`SecretRegistry` and keeping the existing writer.

- [ ] **Step 3: Run to verify pass** (all three)

Run: `cargo test-axum && cargo test-cloudflare && cargo test-spin`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-adapter-axum crates/trusted-server-adapter-cloudflare crates/trusted-server-adapter-spin
git commit -m "Build RuntimeServices via composite store in Axum, Cloudflare, and Spin"
```

---

## Task 6: Local Fastly registry builders + injection into the custom `oneshot` path

EdgeZero's Fastly `dispatch_with_registries` and its registry builders are `pub(crate)` (verified in the pinned checkout), so trusted-server must build the registries **locally** and insert them into the request extensions before `app.router().oneshot()`. (Alternative: an upstream EdgeZero public builder — tracked as **R11**; not assumed here.)

**Files:**
- Create: `crates/trusted-server-adapter-fastly/src/registries.rs` (`build_config_registry`, `build_secret_registry`, `build_kv_registry`)
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs:477` (the `oneshot` dispatch block)
- Test: `crates/trusted-server-adapter-fastly/src/registries.rs` (`#[cfg(test)]`) + a route test

**Interfaces:**
- Consumes: `StoresMetadata` (from `Hooks::stores()`), `EnvConfig`, EdgeZero `FastlyConfigStore`/`FastlyKvStore`/`FastlySecretStore` open primitives, `StoreRegistry::from_parts`.
- Produces: `build_config_registry(&StoresMetadata, &EnvConfig) -> ConfigRegistry` (+ `_secret_/_kv_` variants) matching EdgeZero's per-id name resolution (`EDGEZERO__STORES__<KIND>__<ID>__NAME`).

- [ ] **Step 1: Write a failing builder test** — `build_config_registry` yields a registry whose `default()` resolves and whose declared non-default ids resolve; unknown id → `None`.

Run: `cargo test-fastly build_config_registry_resolves_declared_ids` → Expected: FAIL.

- [ ] **Step 2: Implement the three builders** in `registries.rs` (iterate `StoreMetadata.ids`, resolve platform name via `EnvConfig::store_name(kind, id)`, open the EdgeZero store, assemble `StoreRegistry::from_parts`).

- [ ] **Step 3: Insert registries in the oneshot block** — replace the lone `core_req.extensions_mut().insert(config_store)` at `main.rs:477` with inserts of `ConfigRegistry`/`SecretRegistry`/`KvRegistry` (built via Step 2), preserving the existing `client_info`/`device_signals` inserts.

- [ ] **Step 4: Write a failing Fastly route test** — `GET /.well-known/trusted-server.json` via the EdgeZero `oneshot` path returns the JWKS doc read through the injected `ConfigRegistry`. Name: `oneshot_discovery_reads_jwks_via_registry` (mirror the `StubJwksConfigStore`/`JWKS_CONFIG_STORE_NAME` pattern in `route_tests.rs`, but drive the EdgeZero path, not `route_request`).

Run: `cargo test-fastly oneshot_discovery_reads_jwks_via_registry` → Expected: FAIL then PASS after Steps 2–3.

- [ ] **Step 5: Fastly suite + parity + commit**

Run: `cargo test-fastly && cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`
```bash
git add crates/trusted-server-adapter-fastly
git commit -m "Add local Fastly registry builders and inject them into the oneshot dispatch"
```

---

## Task 7: Delete the duplicated Fastly chunk resolver

With reads via EdgeZero (`FastlyConfigStore` reassembles chunks transparently), collapse `get_settings_from_config_store` and drop the local resolver.

**Files:**
- Modify: `crates/trusted-server-core/src/settings_data.rs`
- Test: `crates/trusted-server-core/src/settings_data.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Rewrite/keep the settings_data test** to assert the blob is read + parsed (not locally chunk-reassembled) — EdgeZero owns reassembly now.

- [ ] **Step 2: Delete `FastlyChunkPointer`, `FastlyChunkRef`, `resolve_fastly_chunk_pointer`, `sha256_hex`, and the chunk constants;** collapse `get_settings_from_config_store` to `ConfigStore::get` + `settings_from_config_blob`.

- [ ] **Step 3: Run tests** — `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin` → PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-core/src/settings_data.rs
git commit -m "Delete duplicated Fastly config-chunk resolver; rely on EdgeZero FastlyConfigStore"
```

---

## Task 8: Retire per-adapter config/secret READ impls; keep the write path (D6-a)

Now that all reads (boot + request, all adapters) flow through EdgeZero, delete the config/secret **read** implementations. Keep the **write** methods + `management_api.rs` (D6-a). Update the legacy `route_tests.rs` stubs that construct `RuntimeServices` from bespoke read stores.

**Files:**
- Modify: `crates/trusted-server-adapter-{fastly,axum,cloudflare,spin}/src/platform.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/route_tests.rs` (update stubs to the composite/registry shape)

- [ ] **Step 1: Delete the config/secret read impls** now unused after Tasks 4–6 (`FastlyPlatformConfigStore::get`, `AxumPlatformConfigStore`, `NoopConfigStore`, Cloudflare/Spin equivalents, secret read impls). Keep the write impls + `management_api.rs`.

- [ ] **Step 2: Update `route_tests.rs`** — the stub stores (`StubJwksConfigStore`, etc.) and `RuntimeServices` construction move to the composite/registry shape (reader = a fixed EdgeZero handle, writer = a recording stub). Keep coverage of the write path (`put`/`create`/`delete`) so key-rotation delegation stays tested.

- [ ] **Step 3: Full CI gate**

Run: `cargo fmt --all -- --check && cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare && cargo clippy-spin-native && cargo clippy-spin-wasm && cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin && cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`
Expected: PASS. **Key rotation/delete still works** (composite writer path).

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-adapter-*
git commit -m "Retire per-adapter config/secret read impls; reads via EdgeZero, writes via composite"
```

---

## Task 1 Output (filled in during execution)

_Kind-partitioned D5 map and the confirmed D6-a decision are recorded here by Task 1 before Tasks 2+ run._

---

## Scope, gating, and follow-ups

- **D6-a locked.** Runtime key-rotation writes stay on the management path via the composite. If Task 1 selects D6-b/c, this plan **stops after Task 1**; a separate `key-rotation-ops-migration` plan handles the admin-surface change.
- **R11 (open):** whether EdgeZero should expose a **public** registry-builder helper (so Fastly need not maintain local builders, Task 6). Decide with the edgezero maintainer; not assumed here.
- **Not in this phase:** `RuntimeServices` removal (Phase 4); Cloudflare/Spin `include_str!`/side-channel config removal (Phase 2); `from_toml_and_env` + `config` dep (Phase 2); `Redacted<T>` / secret externalization (Phase 3); `management_api.rs` deletion (only under a future D6-b).
- **No dependency on edgezero #305** — Phase 1 uses shipped EdgeZero store APIs only.
