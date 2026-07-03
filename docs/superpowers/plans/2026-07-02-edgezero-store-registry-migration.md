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
- CI gate before PR: `cargo fmt --all -- --check`; `cargo clippy-{fastly,axum,cloudflare,spin-native,spin-wasm}`; **`cargo check-cloudflare` + `cargo check-spin`** (wasm-target surfaces — `test-cloudflare`/`test-spin` are native and do **not** compile the wasm runtime paths); `cargo test-{fastly,axum,cloudflare,spin}`; `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`.
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
# KV ids (logical ids referenced by Settings — NOT Fastly-only platform stores)
rg -n 'ec_store|consent_store|creative_store' crates/trusted-server-core/src/settings.rs crates/trusted-server-core/src/consent_config.rs crates/trusted-server-core/src/auction_config_types.rs trusted-server.example.toml
# config ids (incl. DataDome IP-CIDR config store)
rg -n 'config_store_id|jwks|JWKS_CONFIG_STORE_NAME|"app_config"|config_store\s*=|datadome-ip-bypass|default_ip_cidr_source_store' crates/trusted-server-core trusted-server.example.toml
# secret ids
rg -n 'secret_store_id|secret_store\s*=|"secrets"|ts_secrets|signing_keys|SIGNING_SECRET_STORE_NAME' crates/trusted-server-core trusted-server.example.toml
rg -n '\[stores\.' edgezero.toml
```
Expected (verified): **KV** ids = `ec.ec_store` (`ec_identity_store`, `settings.rs:452`), `consent.consent_store` (`consent_config.rs:80`), and `auction.creative_store` (`auction_config_types.rs:28`, default `"creative_store"`, **deprecated** — creatives are delivered inline); **config** ids = the app-config blob store (**store id `trusted_server_config`**, see D5 rule below), `request_signing.config_store_id`, the JWKS store (`JWKS_CONFIG_STORE_NAME`), and **DataDome's IP-CIDR config store** (`ProtectionIpCidrSourceConfig.config_store`, default `datadome-ip-bypass`, `protection_scope.rs:165`); **secret** ids = `secrets` (`request_signing.secret_store_id`), DataDome `ts_secrets`, the S3 secret store, `signing_keys` (`SIGNING_SECRET_STORE_NAME`) — versus `edgezero.toml` declaring only one id per kind. NOTE: `counter_store` (`RATE_COUNTER_NAME` in the Fastly `rate_limiter.rs`) and `opid_store` are **Fastly-only** platform stores, not `Settings` logical ids — out of scope for D5. `creative_store` **is** a `Settings` id: declare it in `[stores.kv]` (deprecated) so strict lookup can't fail, and flag it for removal in a later phase.

  **D5 app-config store-id/key decision (record in Task 1 Output):** the app-config blob → config **store id `trusted_server_config`**, blob **key `app_config`** (`CONFIG_BLOB_KEY`). This changes only `settings_data.rs::DEFAULT_CONFIG_STORE_ID` from `"app_config"` to `"trusted_server_config"` (it is currently a *store id*, `settings_data.rs:11`); `app_config` survives only as the blob **key**.

  **Request-signing store ids (do NOT point at app-config):** request signing reads use hard-coded `JWKS_CONFIG_STORE_NAME = "jwks_store"` (config) + `SIGNING_SECRET_STORE_NAME = "signing_keys"` (secret); writes use `request_signing.config_store_id`/`secret_store_id`. Today the example sets these to `"app_config"`/`"secrets"` — which sends **writes to a different store than reads**. Fix: set `request_signing.config_store_id = "jwks_store"` and `secret_store_id = "signing_keys"` in `trusted-server.example.toml` + fixtures, and declare `jwks_store` (config) + `signing_keys` (secret) as logical ids in `edgezero.toml`. (Under the composite, reads resolve `registry.named("jwks_store")`; writes go to the same store via the writer/management id.)

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

- [ ] **Step 1: Write the failing test (parameterized over multiple configs)**

Cover the example config, the integration fixture, AND a purpose-built config that exercises every store-backed field (DataDome IP-CIDR sources, S3 auth, request-signing) so optional/targeted settings can't escape coverage. Add to `settings.rs` under `#[cfg(test)]`:
```rust
fn assert_all_ids_declared(config_toml: &str, label: &str) {
    let settings = Settings::from_toml(config_toml).unwrap_or_else(|e| panic!("{label} should parse: {e}"));
    let referenced = settings.referenced_store_ids_by_kind();
    let declared = declared_store_ids_by_kind_from_manifest(); // reads edgezero.toml
    for (kind, ids) in [("kv", &referenced.kv), ("config", &referenced.config), ("secrets", &referenced.secrets)] {
        for id in ids {
            assert!(
                declared.for_kind(kind).contains(id),
                "[{label}] {kind} store id `{id}` referenced by Settings is not declared in edgezero.toml",
            );
        }
    }
}

#[test]
fn every_referenced_store_id_is_declared_by_kind() {
    assert_all_ids_declared(include_str!("../../../trusted-server.example.toml"), "example");
    assert_all_ids_declared(
        include_str!("../../trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml"),
        "integration-fixture",
    );
    // Purpose-built config exercising DataDome IP-CIDR, S3, and request-signing store refs.
    assert_all_ids_declared(include_str!("../testdata/all-store-refs.toml"), "all-store-refs");
}
```
(Create `crates/trusted-server-core/src/testdata/all-store-refs.toml` populating every store-id field with a declared id.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test-fastly every_referenced_store_id_is_declared_by_kind`
Expected: FAIL — `ec_identity_store`/`consent_store`/`creative_store` (kv), `jwks_store`/`datadome-ip-bypass`/`trusted_server_config` (config), `signing_keys`/`ts_secrets` (secrets) referenced but not declared.

- [ ] **Step 3: Implement `referenced_store_ids_by_kind()` + manifest helper**

Add the `ReferencedStoreIds` struct + method returning **KV** ids (`ec.ec_store`, `consent.consent_store`, `auction.creative_store`), **config** ids (`request_signing.config_store_id`, the app-config store id, and **every `ProtectionIpCidrSourceConfig.config_store`** from DataDome scopes — default `datadome-ip-bypass`), **secret** ids (`request_signing.secret_store_id`, DataDome `ts_secrets`, S3). Do **not** include `counter_store`/`opid_store`. Add test-only `declared_store_ids_by_kind_from_manifest()` parsing `edgezero.toml`.

Apply the **D5 renames**: set `settings_data.rs::DEFAULT_CONFIG_STORE_ID = "trusted_server_config"`; set `request_signing.config_store_id = "jwks_store"` and `secret_store_id = "signing_keys"` in `trusted-server.example.toml` + fixtures (they must match the read constants — **not** `app_config`/`secrets`).

- [ ] **Step 4: Declare every id in `edgezero.toml`** — `[stores.kv]` = `trusted_server_kv`, `ec_identity_store`, `consent_store`, `creative_store`; `[stores.config]` = `trusted_server_config`, `jwks_store`, `datadome-ip-bypass`; `[stores.secrets]` = `trusted_server_secrets`, `signing_keys`, `ts_secrets`, and the S3 secret id. (Names double as the platform store names under D7.)

- [ ] **Step 5: Wire `Hooks::stores()` on all four adapters (Blocker — metadata is not wired today).** Each `impl Hooks for TrustedServerApp` currently overrides only `routes()`; the default `stores()` returns **empty** `StoresMetadata`, so no registries can be built from it. Add `fn stores() -> StoresMetadata` returning the `[stores.*]` metadata, generated once from `edgezero.toml`. Prefer a single shared `const`/fn in `trusted-server-core` (e.g. `pub fn stores_metadata() -> StoresMetadata`) that all four adapters return, so the ids live in one place. Verify against `edgezero_core::app::StoresMetadata`/`StoreMetadata` shape.

- [ ] **Step 6: Declare the stores in every PLATFORM manifest (Blocker — local resources missing).** D7 requires each logical id to be openable as a real platform store. Add the new ids to: `fastly.toml` (`[local_server.kv_stores]`/`[local_server.config_stores]`/`[local_server.secret_stores]` + the production service store bindings), `crates/trusted-server-adapter-cloudflare/wrangler.toml` (`[[kv_namespaces]]` + config/secret bindings), `crates/trusted-server-adapter-spin/spin.toml` (`key_value_stores` + variables/config), and the Axum local files `.edgezero/local-config-<id>.json` / KV redb defaults. Cross-check each existing manifest — some planned ids (`jwks_store`, `datadome-ip-bypass`, `signing_keys`) may already be partially declared; add the missing ones.

- [ ] **Step 7: Run to verify pass + full adapter suites + wasm checks**

Run: `cargo test-fastly every_referenced_store_id_is_declared_by_kind`
Then: `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin && cargo check-cloudflare && cargo check-spin`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add edgezero.toml fastly.toml trusted-server.example.toml crates/trusted-server-adapter-*/{wrangler.toml,spin.toml} crates/trusted-server-integration-tests/fixtures crates/trusted-server-core/src
git commit -m "Declare kv/config/secret store ids in edgezero.toml and reconcile config fields"
```

---

## Task 3: Registry-backed composite store (reads → EdgeZero registry by store_name, writes → management path)

Concrete D6-a mechanism. The bespoke traits read **by `StoreName`** and callers use **multiple** store ids (`app_config`, JWKS, DataDome, S3, `ec_identity_store` for KV). So the composite must hold the **whole `ConfigRegistry`/`SecretRegistry`** (not a single handle) and resolve `named(store_name)` on each read; writes (`put`/`create`/`delete`) delegate to the existing management-API-backed writer. Preserves `KeyRotationManager` writes with zero call-site changes.

**Files:**
- Modify: `crates/trusted-server-core/src/platform/traits.rs` (split write-only traits)
- Create: `crates/trusted-server-core/src/platform/composite.rs` (`CompositeConfigStore`, `CompositeSecretStore`)
- Modify: `crates/trusted-server-core/src/platform/mod.rs` (export composite + writer traits)
- Test: `crates/trusted-server-core/src/platform/composite.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `edgezero_core::store_registry::{ConfigRegistry, SecretRegistry, ConfigStoreBinding, BoundSecretStore}`, write-only `Arc<dyn PlatformConfigWriter>`/`Arc<dyn PlatformSecretWriter>`.
- Produces:
  - New **write-only** traits `PlatformConfigWriter { put; delete }` and `PlatformSecretWriter { create; delete }` (extracted from the read+write `PlatformConfigStore`/`PlatformSecretStore`). This is what lets Task 8 delete the per-adapter **read** impls while keeping the writer object — the writer no longer needs `get`/`get_bytes`.
  - `CompositeConfigStore::new(reader: ConfigRegistry, writer: Arc<dyn PlatformConfigWriter>) -> Self` implementing the full read+write `PlatformConfigStore`. **`ConfigRegistry::named(id)` returns `Option<ConfigStoreBinding>`, not a handle** — so `get(store_name, key)` = resolve `binding = reader.named(store_name.as_str()).ok_or(PlatformError::ConfigStore)?`, then `block_on(binding.handle.get(key))`. EdgeZero `ConfigStore::get` returns `Result<Option<String>, ConfigStoreError>`; the bespoke `get` returns `Result<String, PlatformError>`, so map `Ok(None)`/`Err(ConfigStoreError::*)` → `PlatformError::ConfigStore`. `put`/`delete` → `writer`.
  - `CompositeSecretStore::new(reader: SecretRegistry, writer: Arc<dyn PlatformSecretWriter>) -> Self` implementing `PlatformSecretStore`: `get_bytes(store_name, key)` = `reader.named(store_name.as_str()).ok_or(PlatformError::SecretStore)?` → `block_on(bound.get_bytes(key))`; map `Ok(None)`/`Err` → `PlatformError::SecretStore`. `create`/`delete` → `writer`. A store_name not in the registry is a hard error (strict), not a silent fallback.

- [ ] **Step 0: Split write-only traits.** In `traits.rs`, define `PlatformConfigWriter` (`put`, `delete`) and `PlatformSecretWriter` (`create`, `delete`). Keep `PlatformConfigStore`/`PlatformSecretStore` as the read+write surface `RuntimeServices` exposes. This split is the prerequisite that makes Task 8's "delete reads, keep writes" compile. Run `cargo check-axum` to confirm the split compiles before proceeding.

- [ ] **Step 1: Write the failing test — reads resolve the NAMED store; unknown store errors; writes delegate**

```rust
#[test]
fn composite_config_reads_named_store_and_writes_delegate() {
    // Arrange: a ConfigRegistry with TWO ids (default `trusted_server_config`, non-default `jwks_store`).
    let reader = config_registry(&[
        ("trusted_server_config", "current-kid", "kid-1"),
        ("jwks_store", "kid-1", "{\"kty\":\"OKP\"}"),
    ], "trusted_server_config");
    let writer = Arc::new(RecordingConfigWriter::default());
    let composite = CompositeConfigStore::new(reader, writer.clone());
    // Act + Assert: non-default store resolves.
    let jwk = composite
        .get(&StoreName::from("jwks_store"), "kid-1")
        .expect("should read from the non-default jwks_store");
    assert_eq!(jwk, "{\"kty\":\"OKP\"}");
    // Unknown store id is a strict error, not a fallback to default.
    let err = composite.get(&StoreName::from("nope"), "kid-1").expect_err("unknown store must error");
    assert!(matches!(err.current_context(), PlatformError::ConfigStore), "unknown id -> ConfigStore error");
    // Write delegates to the management-path writer.
    composite
        .put(&StoreId::from("jwks_store"), "current-kid", "kid-2")
        .expect("should delegate write");
    assert_eq!(
        writer.puts.lock().expect("lock").as_slice(),
        &[("current-kid".to_owned(), "kid-2".to_owned())],
        "write should delegate to the management-path writer",
    );
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test-fastly composite_config_reads_named_store_and_writes_delegate`
Expected: FAIL (module does not exist).

- [ ] **Step 3: Implement `composite.rs`**

`get` resolves `reader.named(store_name)` → `ConfigStoreBinding`, then `block_on(binding.handle.get(key))`; `get_bytes` resolves `reader.named(store_name)` → `BoundSecretStore`, then `block_on(bound.get_bytes(key))` (mirror `storage/kv_store.rs`). Strict: `named` returning `None` → `PlatformError`; EdgeZero `Ok(None)`/`Err` → `PlatformError`. `put`/`create`/`delete` forward to `writer`. Add `config_registry(entries, default)` / `secret_registry(...)` / `RecordingConfigWriter` test helpers that build a real `StoreRegistry` from in-memory EdgeZero stores (config entries wrapped as `ConfigStoreBinding { handle, default_key }`).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test-fastly composite_config_reads_named_store_and_writes_delegate`
Expected: PASS.

- [ ] **Step 5: Reconcile `StoreName` semantics (D7).** `platform/types.rs::StoreName` is documented as an "edge-visible **platform** name". The composite now resolves `registry.named(store_name.as_str())` by **logical id**, so `StoreName` for reads must carry the **logical store id**. Update the `StoreName` doc comment to say "logical runtime store id" for reads, and audit read call sites (`request_signing/{signing,rotation}.rs`, `proxy.rs`, `integrations/datadome/{protection,protection_scope}.rs`) to confirm they pass **logical ids** (`trusted_server_config`, `jwks_store`, `ts_secrets`, `datadome-ip-bypass`, …), not physical platform names. No functional change if ids already equal names (D7 convention), but the doc + audit prevent implementers from passing physical names into logical registries.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/platform/
git commit -m "Add registry-backed composite store; document StoreName as logical read id"
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

- [ ] **Step 1: Write a failing CORE-level test** for the re-typed loader (deterministic, no adapter/Viceroy). In `crates/trusted-server-core/src/settings_data.rs` `#[cfg(test)]`, build an in-memory EdgeZero store and assert the loader parses the blob:

```rust
#[test]
fn get_settings_reads_blob_via_edgezero_handle() {
    // Arrange: an EdgeZero ConfigStoreHandle over an in-memory store holding the blob envelope.
    let blob = blob_envelope_json(include_str!("../../../trusted-server.example.toml"));
    let handle = ConfigStoreHandle::new(Arc::new(InMemoryConfigStore::with(&[("app_config", &blob)])));
    // Act
    let settings = get_settings_from_config_store(&handle, "app_config")
        .expect("should parse settings from the EdgeZero-read blob");
    // Assert
    assert!(settings.ec.ec_store.is_some(), "should deserialize the example config");
}
```
(`InMemoryConfigStore` is a local test double implementing `edgezero_core::config_store::ConfigStore`; `blob_envelope_json` wraps the TOML→JSON in a `BlobEnvelope`. Add both to the test module.)

Run: `cargo test-fastly get_settings_reads_blob_via_edgezero_handle` → Expected: FAIL.

- [ ] **Step 2: Re-type `get_settings_from_config_store`** to `(&ConfigStoreHandle, key: &str)`, called with **store id `trusted_server_config`, key `app_config`** (D5). In Fastly `load_settings_from_config_store()` open the EdgeZero `FastlyConfigStore` for `trusted_server_config` at boot and wrap in a `ConfigStoreHandle`. In Axum `build_state()` open the EdgeZero Axum config store, which reads **`.edgezero/local-config-trusted_server_config.json`** (`edgezero-adapter-axum/src/config_store.rs` — id-scoped local file); do **not** apply any env-key override (D7 — runtime is config-store-only). The adapter-level boot wiring is exercised by each adapter's existing `build_state` test path (no new Viceroy test needed — the core test above covers the parse logic).

- [ ] **Step 3: Run to verify pass** (core test + adapter boot suites)

Run: `cargo test-fastly get_settings_reads_blob_via_edgezero_handle`
Expected: PASS.
Then confirm the adapter boot paths still build/pass via their existing `build_state` coverage:
Run: `cargo test-fastly && cargo test-axum`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/app.rs crates/trusted-server-adapter-axum/src/app.rs crates/trusted-server-core/src/settings_data.rs
git commit -m "Load boot config via EdgeZero config store on Fastly and Axum"
```

---

## Task 5: Switch adapters to EdgeZero's registry-aware entry; RuntimeServices uses the composite

**Blocker addressed:** Axum today calls `TrustedServerApp::routes()` + `AxumDevServer::with_config(...)` (`adapter-axum/src/main.rs:23`) — which never builds registries. This task switches Axum to EdgeZero's registry-aware `run_app::<TrustedServerApp>()` (`edgezero_adapter_axum::dev_server::run_app`, which builds registries from `Hooks::stores()` — now wired in Task 2 Step 5). Cloudflare and Spin already dispatch via EdgeZero `run_app`; confirm their `run_app` builds registries once `stores()` is wired. Then build `RuntimeServices` config/secret from `CompositeConfigStore`/`CompositeSecretStore` (reader from the request registry; writer = the per-adapter write impl). Store-name binding uses EdgeZero's `EnvConfig` fallback-to-logical-id (D7 — we set no `EDGEZERO__STORES__*__NAME`).

**Files:**
- Modify: `crates/trusted-server-adapter-axum/src/main.rs` (switch to `run_app::<TrustedServerApp>()`)
- Modify: `crates/trusted-server-adapter-{axum,cloudflare,spin}/src/platform.rs` (`build_runtime_services` → composite)
- Test: `crates/trusted-server-adapter-axum/src/app.rs` route tests (+ cloudflare/spin equivalents)

- [ ] **Step 0: Switch Axum `main.rs` to `run_app::<TrustedServerApp>()`.** Replace the `TrustedServerApp::routes()` + `AxumDevServer::with_config(router, config).run()` path with `edgezero_adapter_axum::run_app::<TrustedServerApp>()` (preserving bind-address/port behavior via the config surface `run_app` exposes, or `DevServer::run` with `.with_*` registry wiring). Confirm the dev server still binds the configured address. Run `cargo run -p trusted-server-adapter-axum` locally to sanity-check it boots.

**Interfaces:**
- Consumes: Task 3 composite; `ConfigRegistry`/`SecretRegistry` from request extensions.
- Produces: `RuntimeServices` whose reads flow through EdgeZero, writes through the composite writer.

- [ ] **Step 1: Write failing Axum tests covering the default AND a non-default config id AND a non-default secret id** (in the Axum app test module):
  - `discovery_reads_jwks_from_nondefault_config_store` — `GET /.well-known/trusted-server.json`: seed the Axum `ConfigRegistry` with two ids (`trusted_server_config` default + the JWKS store id); assert `200` + the JWKS `kid` in the body (proves non-default **config** id resolution).
  - `datadome_reads_secret_from_nondefault_secret_store` — a request to a **protected non-integration** route (the DataDome server-side key is read during protected-request *filtering* in `protection.rs`, which **skips** the `/integrations/datadome/*` routes — see `protection.rs:110`), so drive a publisher/first-party route in DataDome's protection scope. Seed the `SecretRegistry` with two ids (default + `ts_secrets`) and the server-side key under `ts_secrets`; assert the filter reads it (proves non-default **secret** id resolution).
  - `first_party_proxy_reads_s3_secret` — `GET /first-party/proxy` for an S3-auth asset route: seed the S3 secret id; assert the SigV4 path obtains the secret (proves the S3 secret read).

Run each (one filter per `cargo test` invocation):
```bash
cargo test-axum discovery_reads_jwks_from_nondefault_config_store
cargo test-axum datadome_reads_secret_from_nondefault_secret_store
cargo test-axum first_party_proxy_reads_s3_secret
```
Expected: FAIL (all three).

- [ ] **Step 2: Build `RuntimeServices` via the composite** in each adapter's `build_runtime_services`, passing the whole request `ConfigRegistry`/`SecretRegistry` as the composite reader (Task 3) and the per-adapter **write-only** impl (`PlatformConfigWriter`/`PlatformSecretWriter`, Task 3 Step 0 / Task 8) as the writer.

- [ ] **Step 3: Run to verify pass** (all three)

Run: `cargo test-axum && cargo test-cloudflare && cargo test-spin`
Expected: PASS. (Cloudflare/Spin reuse the same composite; their route tests assert the default-id read at minimum.)

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
- Consumes: `StoresMetadata` (from `Hooks::stores()`), EdgeZero `FastlyConfigStore`/`FastlyKvStore`/`FastlySecretStore` open primitives, `StoreRegistry::from_parts` (which returns **`Option<Self>`** — `None` when the default id is absent from `by_id`).
- Produces (signatures mirror EdgeZero's own private Fastly builders):
  - `build_kv_registry(&StoresMetadata) -> Result<Option<KvRegistry>, FastlyError>` — KV store `open` can fail (→ `Err`); a metadata with no KV stores or a missing default → `Ok(None)`.
  - `build_config_registry(&StoresMetadata) -> Option<ConfigRegistry>` and `build_secret_registry(&StoresMetadata) -> Option<SecretRegistry>` — `None` when the kind is undeclared or the default id can't be assembled.
  - **Failure policy:** each opens every declared id **by logical id** (D7). If a *declared* store fails to open (KV `Err`) propagate it to the request as an error; if the *default* id is missing, `from_parts` yields `None` → the registry is not inserted → the strict extractor later returns `None` → the handler surfaces a 500 (no silent fallback). This matches EdgeZero's `dispatch_with_registries` behavior for the other adapters.

- [ ] **Step 1: (D7) No runtime env reader.** Per D7 the runtime does **not** read `EDGEZERO__STORES__*__NAME` — stores are opened by **logical id**. This deletes the need for a Fastly runtime-dictionary `EnvConfig` reader (and sidesteps that `fastly::ConfigStore` has no `iter()` and EdgeZero's reader is private). If a deployment ever needs to remap a physical store name, that is handled at provisioning time, not here. No code in this step; it records the design constraint the builders follow.

- [ ] **Step 2: Write a failing builder test** — `build_config_registry` opens each declared id by name and yields a registry whose `default()` resolves and whose declared non-default id (`jwks_store`) resolves; an id **not** in `StoresMetadata` is absent (`named("nope").is_none()`). Name: `build_config_registry_resolves_declared_ids`.

Run: `cargo test-fastly build_config_registry_resolves_declared_ids` → Expected: FAIL.

- [ ] **Step 3: Implement the three builders** in `registries.rs` with the signatures above: iterate `StoreMetadata.ids`, open the EdgeZero Fastly store **by the logical id** (`FastlyConfigStore`/`FastlyKvStore`/`FastlySecretStore` open primitive), collect into a `BTreeMap<String, H>`, and `StoreRegistry::from_parts(by_id, default_id.to_owned())` (propagating the KV `open` error in `build_kv_registry`). No `EnvConfig`, no runtime dictionary.

- [ ] **Step 4: Insert registries in the oneshot block** — replace the lone `core_req.extensions_mut().insert(config_store)` at `main.rs:477`: build the three registries via Step 3 (propagate `build_kv_registry`'s `FastlyError` into the dispatch's `Result`), and `if let Some(reg) = ...` insert each into `core_req.extensions_mut()`, preserving the existing `client_info`/`device_signals` inserts.

- [ ] **Step 5: Write a failing Fastly route test** — `GET /.well-known/trusted-server.json` via the EdgeZero `oneshot` path returns the JWKS doc read through the injected `ConfigRegistry` (built with default + `jwks_store` ids). Name: `oneshot_discovery_reads_jwks_via_registry` (mirror the `StubJwksConfigStore`/`JWKS_CONFIG_STORE_NAME` pattern in `route_tests.rs`, but drive the EdgeZero path, not `route_request`).

Run: `cargo test-fastly oneshot_discovery_reads_jwks_via_registry` → Expected: FAIL then PASS after Steps 3–4.

- [ ] **Step 6: Fastly suite + parity + commit**

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

Now that all reads (boot + request, all adapters) flow through EdgeZero, delete the config/secret **read** implementations. The per-adapter management impls become **write-only** (`PlatformConfigWriter`/`PlatformSecretWriter` from Task 3 Step 0) + `management_api.rs` (D6-a). Update the legacy `route_tests.rs` stubs that construct `RuntimeServices` from bespoke read stores.

**Files:**
- Modify: `crates/trusted-server-adapter-{fastly,axum,cloudflare,spin}/src/platform.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/route_tests.rs` (update stubs to the composite/registry shape)

- [ ] **Step 1: Convert per-adapter config/secret impls to write-only.** The old impls implemented the read+write `PlatformConfigStore`/`PlatformSecretStore` (`FastlyPlatformConfigStore` with `get`+`put`+`delete`, `AxumPlatformConfigStore`, `NoopConfigStore`, Cloudflare/Spin equivalents, secret impls). Now that the composite serves reads, **re-implement them as `PlatformConfigWriter`/`PlatformSecretWriter`** (drop `get`/`get_bytes`; keep `put`/`create`/`delete`). This compiles only because Task 3 Step 0 split the traits — otherwise deleting `get` from a `PlatformConfigStore` impl is a trait-incompleteness error. Keep `management_api.rs`.

- [ ] **Step 2: Update `route_tests.rs`** — the stub stores (`StubJwksConfigStore`, etc.) and `RuntimeServices` construction move to the composite/registry shape: build the composite reader from a real `ConfigRegistry`/`SecretRegistry` with **at least two ids** (default + a non-default such as `jwks_store`/`ts_secrets`), and assert an **unknown store id resolves strictly to an error** (not a silent fallback to default). Writer = a recording stub; keep coverage of the write path (`put`/`create`/`delete`) so key-rotation delegation stays tested.

- [ ] **Step 3: Full CI gate**

Run: `cargo fmt --all -- --check && cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare && cargo clippy-spin-native && cargo clippy-spin-wasm && cargo check-cloudflare && cargo check-spin && cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin && cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`
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
