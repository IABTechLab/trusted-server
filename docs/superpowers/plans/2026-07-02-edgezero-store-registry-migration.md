# EdgeZero Store-Registry Migration (Phase 1, D6-a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route trusted-server's runtime **and boot-time** config/secret **reads** through EdgeZero stores/registries (as KV already is), reconcile every logical store id (kv/config/secrets) with `edgezero.toml`, and delete the duplicated Fastly chunk resolver — while **keeping** the runtime **write** path (key rotation) intact via a composite store (decision **D6-a**).

**Architecture:** trusted-server core reads/writes stores through the bespoke `PlatformConfigStore`/`PlatformSecretStore` traits (each mixes read `get`/`get_string` + write `put`/`create`/`delete`), surfaced via `RuntimeServices` (one trait object per kind). EdgeZero's `ConfigStore`/`SecretStore` are **read-only**; per-request `ConfigRegistry`/`SecretRegistry` live in request extensions. This phase introduces a **composite store** whose *reads* resolve from EdgeZero and whose *writes* delegate to the existing management-API-backed impl, migrates the Fastly/Axum **boot** config read to EdgeZero, and adds **local** registry builders for Fastly's custom `oneshot` dispatch (EdgeZero's builders are `pub(crate)`).

**Tech Stack:** Rust 2024, toolchain 1.95.0, `error-stack` `Report<TrustedServerError>`, EdgeZero (`edgezero-core`/`edgezero-adapter-fastly` git dep), Viceroy, `cargo test-{fastly,axum,cloudflare,spin}`.

**Spec:** `docs/superpowers/specs/2026-07-02-edgezero-full-migration-design.md` §5 Phase 1, D5, D6, §4a.

## Pinned dependency (verified)

This plan targets the EdgeZero commit pinned in `Cargo.lock`: **`branch = worktree-state-nested-secrets-spec-review` @ `6ebc29a5`** (PR [stackpop/edgezero#306](https://github.com/stackpop/edgezero/pull/306)). That commit **has** every API this plan uses — verified by inspecting the pinned checkout: `store_registry.rs` (`StoreRegistry`/`ConfigRegistry`/`SecretRegistry`/`from_parts`), `StoresMetadata` + `Hooks::stores()` (`app.rs`), `dispatch_with_registries` + `build_*_registry` (adapter-fastly), `AxumDevServer::{with_config,with_kv,with_secret}_registry`, the `[stores.*]` `ids`/`default` manifest schema (`manifest.rs`, `StoreDeclaration`, `deny_unknown_fields`), CloudflareConfigStore backed by **KV namespaces**, and AxumConfigStore backed by **`.edgezero/local-config-<id>.json`**. Older cargo-cache checkouts (e.g. `ce6bcf7`, "Add store support for Spin adapter #253") **predate** the registry refactor and lack these — do not review against them; confirm any API check against `6ebc29a5`.

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

  **D5 app-config store-id/key decision (record in Task 1 Output):** the app-config blob → config **store id `trusted_server_config`**, blob **key `trusted_server_config`** (key == store id, D7-consistent — see below). This changes `settings_data.rs::DEFAULT_CONFIG_STORE_ID` from `"app_config"` to `"trusted_server_config"` (`settings_data.rs:11`) **and** `config_payload.rs::CONFIG_BLOB_KEY` from `"app_config"` to `"trusted_server_config"`. Rationale: `default_config_key()` falls back to the id when no `…__KEY` env is set, and D7 forbids that runtime env — so the key must equal the id, or `ts config push` would need `--key`. The `app_config` name is fully retired.

  **Request-signing store ids (do NOT point at app-config):** request signing reads use hard-coded `JWKS_CONFIG_STORE_NAME = "jwks_store"` (config) + `SIGNING_SECRET_STORE_NAME = "signing_keys"` (secret); writes use `request_signing.config_store_id`/`secret_store_id`. Today the example sets these to `"app_config"`/`"secrets"` — which sends **writes to a different store than reads**. Fix: set `request_signing.config_store_id = "jwks_store"` and `secret_store_id = "signing_keys"` in `trusted-server.example.toml` + fixtures, and declare `jwks_store` (config) + `signing_keys` (secret) as logical ids in `edgezero.toml`. (Under the composite, reads resolve `registry.named("jwks_store")`; writes go to the same store via the writer/management id.)

- [ ] **Step 2: Enumerate runtime WRITE sites**

Run:
```bash
rg -n '\.config_store\(\)\.(put|delete)|\.secret_store\(\)\.(create|delete)' crates/trusted-server-core
```
Expected: only `KeyRotationManager` in `crates/trusted-server-core/src/request_signing/rotation.rs` (`store_private_key`, `store_public_jwk`, `deactivate_key`, `delete_key`). Confirm no other runtime writers.

- [ ] **Step 3: Record the kind-partitioned D5 map**

Append a table to "Task 1 Output": for each `{kv|config|secrets}` id → resolution (declare in `edgezero.toml`, or collapse onto the kind's default) → the concrete platform resource per adapter. **Under D7 there is no `EDGEZERO__STORES__*__NAME` mapping** — the logical id opens the same-named platform store, so the table records the *platform resource per adapter* (Fastly local/prod store, CF KV namespace / flat secret keys, Spin KV label / variable), **not** an env var. Spec default: app-config blob → config id `trusted_server_config` key `trusted_server_config` (key == id); JWKS → its own config id; `ec_identity_store` → kv id; collapse `secrets`→`trusted_server_secrets` where identical; declare DataDome/S3/signing as distinct secret ids.

- [ ] **Step 4: Confirm D6-a (or STOP)**

Confirm this phase keeps the write-capable composite (D6-a). Record it. **If the team instead chooses D6-b/c, stop here** and open a separate plan (`…-key-rotation-ops-migration.md`); do not proceed to Task 2.

- [ ] **Step 5: Commit the record**

```bash
git add docs/superpowers/plans/2026-07-02-edgezero-store-registry-migration.md
git commit -m "Record Phase 1 kind-aware store-id map and confirm D6-a"
```

---

## Task 2: Declare all store ids (kv/config/secrets) in `edgezero.toml` + reconcile fields/fixtures

**Files (exact):**
- Modify: `edgezero.toml` (`[stores.kv]`/`[stores.config]`/`[stores.secrets]` `ids`)
- Modify: `crates/trusted-server-core/src/settings_data.rs` (`DEFAULT_CONFIG_STORE_ID`)
- Modify: `crates/trusted-server-core/src/config_payload.rs` (`CONFIG_BLOB_KEY` → `"trusted_server_config"`)
- Modify: `trusted-server.example.toml`, `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml`
- Create: `crates/trusted-server-core/src/testdata/all-store-refs.toml`
- **Modify (integration test surfaces that hard-code `app_config` — break under the rename):** `crates/trusted-server-integration-tests/src/bin/generate-viceroy-config.rs` (`[local_server.config_stores.app_config]` + `app_config = '''…'''` + the generator test asserting them → `trusted_server_config`), `crates/trusted-server-integration-tests/tests/common/config.rs` (`{"app_config": envelope}` → `{"trusted_server_config": …}`), `crates/trusted-server-integration-tests/tests/environments/axum.rs` (env `TRUSTED_SERVER_CONFIG_APP_CONFIG_APP_CONFIG` → the trusted_server_config-keyed name)
- Modify (platform manifests): `fastly.toml`, `crates/trusted-server-adapter-cloudflare/wrangler.toml`, `crates/trusted-server-adapter-spin/spin.toml` (Axum uses `.edgezero/local-config-*.json` local dev files — document but do not commit machine-local state)
- Modify: `crates/trusted-server-adapter-cloudflare/src/app.rs` (`settings_from_cloudflare_config_json` side-channel key `app_config` → `CONFIG_BLOB_KEY`)
- Create: `crates/trusted-server-core/src/stores.rs` (shared `stores_metadata()`); Modify: each `crates/trusted-server-adapter-{fastly,axum,cloudflare,spin}/src/app.rs` (`impl Hooks` → add `fn stores()`)
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
    assert_all_ids_declared(include_str!("testdata/all-store-refs.toml"), "all-store-refs");
}
```
(Create `crates/trusted-server-core/src/testdata/all-store-refs.toml` populating every store-id field with a declared id. `settings.rs` lives in `src/`, so the `include_str!` path relative to it is `testdata/all-store-refs.toml` — **not** `../testdata/…`.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test-fastly every_referenced_store_id_is_declared_by_kind`
Expected: FAIL — `ec_identity_store`/`consent_store`/`creative_store` (kv), `jwks_store`/`datadome-ip-bypass`/`trusted_server_config` (config), `signing_keys`/`ts_secrets` (secrets) referenced but not declared.

- [ ] **Step 3: Implement `referenced_store_ids_by_kind()` + manifest helper**

Add the `ReferencedStoreIds` struct + method returning **KV** ids (`ec.ec_store`, `consent.consent_store`, `auction.creative_store`), **config** ids (`request_signing.config_store_id`, the app-config store id, and **every `ProtectionIpCidrSourceConfig.config_store`** from DataDome scopes — default `datadome-ip-bypass`), **secret** ids (`request_signing.secret_store_id`, DataDome `ts_secrets`, S3). Do **not** include `counter_store`/`opid_store`. Add test-only `declared_store_ids_by_kind_from_manifest()` parsing `edgezero.toml`.

Apply the **D5 renames**: set `settings_data.rs::DEFAULT_CONFIG_STORE_ID = "trusted_server_config"`; set `request_signing.config_store_id = "jwks_store"` and `secret_store_id = "signing_keys"` in `trusted-server.example.toml` + fixtures (they must match the read constants — **not** `app_config`/`secrets`).

- [ ] **Step 4: Declare every id in `edgezero.toml`** — `[stores.kv]` = `trusted_server_kv`, `ec_identity_store`, `consent_store`, `creative_store`; `[stores.config]` = `trusted_server_config`, `jwks_store`, `datadome-ip-bypass`; `[stores.secrets]` = `trusted_server_secrets`, `signing_keys`, `ts_secrets`, `s3-auth`. (Names double as the platform store names under D7.) Also set `config_payload.rs::CONFIG_BLOB_KEY = "trusted_server_config"` (blob key == store id, per the D5 rule) so `ts config push`'s default key and the boot read agree with no env/`--key`.

- [ ] **Step 5: Wire `Hooks::stores()` on all four adapters (Blocker — metadata is not wired today).** Each `impl Hooks for TrustedServerApp` currently overrides only `routes()`; the default `stores()` returns **empty** `StoresMetadata`, so no registries can be built from it. Add `fn stores() -> StoresMetadata` returning the `[stores.*]` metadata, generated once from `edgezero.toml`. Prefer a single shared fn in `trusted-server-core` (`pub fn stores_metadata() -> StoresMetadata`) that all four adapters return, so the ids live in one place. Verify against `edgezero_core::app::StoresMetadata`/`StoreMetadata` shape.

- [ ] **Step 5b: Anti-drift test — `stores_metadata()` and every adapter's `Hooks::stores()` must equal `edgezero.toml`.** Registries are built from `TrustedServerApp::stores()`, **not** from the `edgezero.toml` that Step 1's test validates — so a stale/incomplete `stores_metadata()` would pass Step 1 while runtime registries silently miss ids. Add a test that parses `edgezero.toml`'s `[stores.*]` ids/default and asserts they equal `trusted_server_core::stores_metadata()` **and** each `<adapter>::TrustedServerApp::stores()` (per kind, ids as sets + default). Put the core half in `trusted-server-core` and one assertion in each adapter's test module (so a future adapter that forgets to return `stores_metadata()` fails).

- [ ] **Step 6: Declare the stores in every PLATFORM manifest (Blocker — local resources missing), per each adapter's real mapping.** D7 requires each logical id to be openable as a real platform store. The adapters map kinds to concrete resources differently — declare exactly:
  - **Fastly** (`fastly.toml`): KV ids → `[[local_server.kv_stores.<id>]]`; config ids → `[local_server.config_stores.<id>]`; secret ids → `[local_server.secret_stores.<id>]`. Also add the production-service store bindings for each id.
  - **Cloudflare manifest `cloudflare.toml` — reconcile the STALE schema (Medium blocker).** `crates/trusted-server-adapter-cloudflare/cloudflare.toml` still uses the pre-rewrite manifest schema (`[stores.kv].name = …`, `[stores.kv.adapters.cloudflare].name = …`), which the pinned EdgeZero manifest parser (`manifest.rs`, `deny_unknown_fields`) **rejects** in favor of `[stores.*]` `ids`/`default`. Either migrate it to the `ids`/`default` schema (matching `edgezero.toml`) or **delete it if `edgezero.toml` is the single source** and nothing loads `cloudflare.toml`. Do not leave a stale-schema manifest that a tool/test could load.
  - **Cloudflare** (`wrangler.toml`): EdgeZero backs **config stores by a KV namespace binding** (`config_store.rs`) — so each **config** id (`trusted_server_config`, `jwks_store`, `datadome-ip-bypass`) gets a `[[kv_namespaces]]` binding (as does each KV id). **Secrets use a FLAT namespace — `CloudflareSecretStore::get_bytes` ignores `store_name` and reads `env.secret(key)`.** So do **not** `wrangler secret put signing_keys` (that provisions the wrong name). Provision the concrete secret **keys the code reads**: the signing KIDs written by `KeyRotationManager`, the DataDome `server_side_key_secret_name`, and the S3 `access_key_id` / `secret_access_key` / optional session-token keys. Document the exact `wrangler secret put <key>` commands in the operator runbook; `store_name`/store-id is irrelevant on Cloudflare.
  - **Spin** (`spin.toml`): config **and** KV ids open **KV-store labels** (`request.rs:282`) — declare each under the component's `key_value_stores = [...]`. Secrets are likewise a **flat** namespace (`SpinSecretStore` ignores `store_name`) mapped to Spin variables — provision the concrete secret **keys** (as for Cloudflare), lowercased per Spin's variable rules, not the store id.

- [ ] **Step 6b: Update every surface that hard-codes the old `app_config` store/key** (they run in the adapter suites / boot paths, so the rename breaks them):
  - **`generate-viceroy-config.rs` — MERGE, don't duplicate.** The generator already emits `[local_server.config_stores.trusted_server_config]` holding the **rollout flags** (`edgezero_enabled`, `edgezero_rollout_pct`), separate from the `app_config` store holding the envelope blob. After the rename both live in ONE store `trusted_server_config`, so **merge the envelope entry into the existing `trusted_server_config.contents` table under key `trusted_server_config`** (alongside the flag keys) — do **not** emit a second `[local_server.config_stores.trusted_server_config]` block (duplicate table). Update the generator's assertion test accordingly.
  - **`tests/common/config.rs`:** `{"app_config": envelope}` → `{"trusted_server_config": envelope}`.
  - **`tests/environments/axum.rs`:** rename the `TRUSTED_SERVER_CONFIG_APP_CONFIG_APP_CONFIG` env var to the `trusted_server_config`-keyed name the Axum config store expects.
  - **`crates/trusted-server-adapter-cloudflare/src/app.rs` (`settings_from_cloudflare_config_json`):** it reads the literal `value.get("app_config")` from the `TRUSTED_SERVER_CONFIG` side-channel (Cloudflare stays on the side-channel until Phase 2). Change this literal to `CONFIG_BLOB_KEY` (now `"trusted_server_config"`) so Cloudflare boot doesn't break under the rename. (Key-string update only; the Phase 2 store migration is separate.)
  - **`crates/trusted-server-integration-tests/tests/environments/cloudflare.rs`:** the `inject_cloudflare_config` tests assert against `{"app_config":"blob"}` / `TRUSTED_SERVER_CONFIG = '''{"app_config":…}'''` literals (lines ~206/210/221/232) — update to the `trusted_server_config` key.
  - **`crates/trusted-server-adapter-cloudflare/wrangler.toml`:** the placeholder `TRUSTED_SERVER_CONFIG = '{"app_config":""}'` (line ~23) + its `app_config` comment (line ~21) — update to `trusted_server_config`.
  - **Axum**: dev-only local files `.edgezero/local-config-<id>.json` (config) and the redb KV default; document how to seed them, do not commit machine-local state.
  Cross-check each existing manifest — some ids (`jwks_store`, `signing_keys`) are already partially declared (`request_signing/mod.rs` doc references `fastly.toml`); add only the missing ones.

- [ ] **Step 7: Run to verify pass + full adapter suites + wasm checks**

Run: `cargo test-fastly every_referenced_store_id_is_declared_by_kind`
Then: `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin && cargo check-cloudflare && cargo check-spin`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add edgezero.toml fastly.toml trusted-server.example.toml \
  crates/trusted-server-adapter-cloudflare/wrangler.toml \
  crates/trusted-server-adapter-cloudflare/cloudflare.toml \
  crates/trusted-server-adapter-spin/spin.toml \
  crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml \
  crates/trusted-server-integration-tests/src/bin/generate-viceroy-config.rs \
  crates/trusted-server-integration-tests/tests/common/config.rs \
  crates/trusted-server-integration-tests/tests/environments/axum.rs \
  crates/trusted-server-integration-tests/tests/environments/cloudflare.rs \
  crates/trusted-server-adapter-cloudflare/src/app.rs \
  crates/trusted-server-core/src/settings.rs \
  crates/trusted-server-core/src/settings_data.rs \
  crates/trusted-server-core/src/config_payload.rs \
  crates/trusted-server-core/src/stores.rs \
  crates/trusted-server-core/src/lib.rs \
  crates/trusted-server-core/src/testdata/all-store-refs.toml \
  crates/trusted-server-adapter-fastly/src/app.rs \
  crates/trusted-server-adapter-axum/src/app.rs \
  crates/trusted-server-adapter-cloudflare/src/app.rs \
  crates/trusted-server-adapter-spin/src/app.rs
git commit -m "Declare store ids in edgezero.toml, manifests, Hooks::stores(); rename app-config store/key to trusted_server_config"
```

---

## Task 3: Registry-backed composite store (reads → EdgeZero registry by store_name, writes → management path)

Concrete D6-a mechanism. The bespoke traits read **by `StoreName`** and callers use **multiple** store ids (`trusted_server_config`, `jwks_store`, `datadome-ip-bypass`, `s3-auth`, `ts_secrets`, `ec_identity_store` for KV). So the composite must hold the **whole `ConfigRegistry`/`SecretRegistry`** (not a single handle) and resolve `named(store_name)` on each read; writes (`put`/`create`/`delete`) delegate to the existing management-API-backed writer. Preserves `KeyRotationManager` writes with zero call-site changes.

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

- [ ] **Step 0: Split write-only traits.** In `traits.rs`, define `pub trait PlatformConfigWriter: Send + Sync { put; delete }` and `pub trait PlatformSecretWriter: Send + Sync { create; delete }` (matching the parent traits' `Send + Sync` bounds, since they're held as `Arc<dyn …>`). Keep `PlatformConfigStore`/`PlatformSecretStore` as the read+write surface `RuntimeServices` exposes. This split is the prerequisite that makes Task 8's "delete reads, keep writes" compile. Run `cargo check-axum` to confirm the split compiles before proceeding.

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
    let err = composite.get(&StoreName::from("nope"), "kid-1").expect_err("should error on unknown store id");
    assert!(matches!(err.current_context(), PlatformError::ConfigStore), "unknown id -> ConfigStore error");
    // Write delegates to the management-path writer, PRESERVING the target StoreId (core D6-a risk).
    composite
        .put(&StoreId::from("jwks_store"), "current-kid", "kid-2")
        .expect("should delegate write");
    assert_eq!(
        writer.puts.lock().expect("should acquire writer lock").as_slice(),
        &[("jwks_store".to_owned(), "current-kid".to_owned(), "kid-2".to_owned())],
        "write must delegate to the writer with the SAME StoreId, key, and value",
    );
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test-fastly composite_config_reads_named_store_and_writes_delegate`
Expected: FAIL (module does not exist).

- [ ] **Step 3: Implement `composite.rs`**

`get` resolves `reader.named(store_name)` → `ConfigStoreBinding`, then `block_on(binding.handle.get(key))`; `get_bytes` resolves `reader.named(store_name)` → `BoundSecretStore`, then `block_on(bound.get_bytes(key))` (mirror `storage/kv_store.rs`). Strict: `named` returning `None` → `PlatformError`; EdgeZero `Ok(None)`/`Err` → `PlatformError`. `put`/`create`/`delete` forward to `writer`. Add `config_registry(entries, default)` / `secret_registry(...)` test helpers that build a real `StoreRegistry` from in-memory EdgeZero stores (config entries wrapped as `ConfigStoreBinding { handle, default_key }`), and a `RecordingConfigWriter` (impl `PlatformConfigWriter`) that records **`(StoreId, key, value)`** tuples so tests assert the target StoreId is preserved on delegation.

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
    let handle = ConfigStoreHandle::new(Arc::new(InMemoryConfigStore::with(&[("trusted_server_config", &blob)])));
    // Act
    let settings = get_settings_from_config_store(&handle, "trusted_server_config")
        .expect("should parse settings from the EdgeZero-read blob");
    // Assert
    assert!(settings.ec.ec_store.is_some(), "should deserialize the example config");
}
```
(`InMemoryConfigStore` is a local test double implementing `edgezero_core::config_store::ConfigStore`; `blob_envelope_json` wraps the TOML→JSON in a `BlobEnvelope`. Add both to the test module.)

Run: `cargo test-fastly get_settings_reads_blob_via_edgezero_handle` → Expected: FAIL.

- [ ] **Step 2: Re-type `get_settings_from_config_store`** to `(&ConfigStoreHandle, key: &str)`, called with **store id `trusted_server_config`, key `trusted_server_config`** (D5 — key == store id; `CONFIG_BLOB_KEY` is set to `trusted_server_config` in Task 2). In Fastly `load_settings_from_config_store()` open the EdgeZero `FastlyConfigStore` for `trusted_server_config` at boot and wrap in a `ConfigStoreHandle`. In Axum `build_state()` open the EdgeZero Axum config store, which reads **`.edgezero/local-config-trusted_server_config.json`** (`edgezero-adapter-axum/src/config_store.rs` — id-scoped local file); do **not** apply any env-key override (D7). The adapter-level boot wiring is exercised by each adapter's existing `build_state` test path (no new Viceroy test needed — the core test above covers the parse logic).

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

## Task 5: Preserve `AxumDevServer::with_config` + add registries; RuntimeServices via composite (Axum/Cloudflare/Spin)

**Blocker addressed:** Axum today calls `TrustedServerApp::routes()` + `AxumDevServer::with_config(...)` (`adapter-axum/src/main.rs:23`) — which never builds registries. We **keep** `AxumDevServer::with_config` (to preserve the custom `PORT`/`axum.toml` behavior) and add registries via its `.with_config_registry()/.with_kv_registry()/.with_secret_registry()` builder methods (Step 0). Cloudflare and Spin already dispatch via EdgeZero `run_app`, which builds registries from `Hooks::stores()` (wired in Task 2 Step 5) — confirm they do once `stores()` exists. Then build `RuntimeServices` config/secret from `CompositeConfigStore`/`CompositeSecretStore` (reader = the whole request registry from extensions; writer = the per-adapter write-only impl). Store-name binding uses EdgeZero's `EnvConfig` fallback-to-logical-id (D7 — we set no `EDGEZERO__STORES__*__NAME`).

**Files:**
- Modify: `crates/trusted-server-adapter-axum/src/main.rs` (keep `AxumDevServer::with_config`, chain registry setters)
- Create: `crates/trusted-server-adapter-axum/src/registries.rs` (`build_{config,kv,secret}_registry_axum(&StoresMetadata)`)
- **Modify (core KV surface, Step 2c):** `crates/trusted-server-core/src/platform/types.rs` (`RuntimeServices::kv_store_named` + `kv_registry` field/builder), `crates/trusted-server-core/src/publisher.rs` (consent call site passes `consent_store`), + any other id-dropping KV consumers
- Modify: `crates/trusted-server-adapter-{axum,cloudflare,spin}/src/platform.rs` (`build_runtime_services` → composite + `kv_registry` from extensions)
- Modify: `crates/trusted-server-adapter-fastly/src/{platform.rs,app.rs}` (populate `kv_registry`; remove consent-store special-casing)
- Test-support (Step 2d): a shared `test_context_with_registries(...)` helper; migrate existing direct-context tests (`adapter-{axum,cloudflare,spin,fastly}` test modules)
- Test: `crates/trusted-server-adapter-axum/src/app.rs` route tests (+ cloudflare/spin equivalents; non-default KV test per adapter)

- [ ] **Step 0: Wire registries into Axum while keeping the custom PORT behavior.** Do **not** call `dev_server::run_app` — it reads bind config only from `EDGEZERO__ADAPTER__HOST/PORT` and would drop trusted-server's `PORT`/`axum.toml` handling (`main.rs:11`, `port_from_env`). Instead keep the current `AxumDevServer::with_config(router, config)` and chain the builder's registry setters (verified present: `AxumDevServer::{with_config_registry, with_kv_registry, with_secret_registry}`):
```rust
// adapter-axum/src/main.rs
let router = TrustedServerApp::routes();
let stores = trusted_server_core::stores_metadata(); // Task 2 Step 5
let mut server = AxumDevServer::with_config(router, config);
if let Some(reg) = build_config_registry_axum(&stores) { server = server.with_config_registry(reg); }
if let Some(reg) = build_kv_registry_axum(&stores)     { server = server.with_kv_registry(reg); }
if let Some(reg) = build_secret_registry_axum(&stores) { server = server.with_secret_registry(reg); }
server.run()?;
```
Add `build_*_registry_axum(&StoresMetadata)` in `adapter-axum/src/registries.rs` mirroring Task 6's Fastly by-id builders but opening the EdgeZero **Axum** store primitives (`.edgezero/*` local backends). This preserves PORT/axum.toml exactly and wires registries. Run `cargo run -p trusted-server-adapter-axum` to confirm it boots on the configured port.

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

- [ ] **Step 2: Build `RuntimeServices` via the composite** in each adapter's `build_runtime_services(ctx: &RequestContext)`. **Extract the whole registry from request extensions** — `ctx.request().extensions().get::<ConfigRegistry>().cloned()` / `get::<SecretRegistry>()` — the same way EdgeZero's `Config`/`Secrets` extractors do. Do **not** use `ctx.config_store_default()`/`config_store(id)` (those return a single bound handle and would wire only the default store). Pass the cloned registry as the composite reader (Task 3) and the per-adapter **write-only** impl (`PlatformConfigWriter`/`PlatformSecretWriter`) as the writer. If a registry is absent from extensions, that is a wiring bug (Step 0 / EdgeZero dispatch) — surface it, don't silently fall back.

- [ ] **Step 2b: Non-default coverage on Cloudflare AND Spin (not just Axum).** Cloudflare/Spin platform mappings differ (config = KV-namespace/label backed; secrets = flat namespace), so default-only assertions are insufficient. Add, in each of the Cloudflare and Spin test modules, tests proving **non-default** resolution: a `jwks_store` **config** read and a `ts_secrets` / S3 **secret**-key read resolve through the composite (route tests if cheap, else small `build_runtime_services` + composite-read tests seeding a 2-id registry).

- [ ] **Step 2c: Named-KV resolution — a CORE surface change, not adapter-only (DECIDED — registry-backed KV now).** The core `RuntimeServices` exposes only `kv_store(&self) -> &dyn PlatformKvStore` — a **single** handle — and consumers that have a store id today **drop it**: `publisher.rs:626` does `settings.consent.consent_store.as_deref().map(|_| services.kv_store())`. So adapter-only changes cannot make `consent_store` resolve. This step is cross-cutting:
  - **Type decision — resolve named KV as `KvHandle`, and migrate consent onto `KvHandle`.** `KvRegistry::named(id)` yields a `KvHandle` (a wrapper `{ store: Arc<dyn KvStore> }`), **not** a `&dyn PlatformKvStore` — and `ConsentPipelineInput.kv_store` is currently `Option<&dyn PlatformKvStore>` (`consent/mod.rs:89`), so a `KvHandle` does not fit directly. Do **not** wrap; **migrate the consent KV surface to `KvHandle`** (the idiomatic edgezero handle — `RuntimeServices` already exposes `kv_handle()` for the default). Specifically:
    - **Core (`platform/types.rs`):** add `RuntimeServices::kv_handle_named(&self, id: &str) -> Option<KvHandle>` (mirroring the existing `kv_handle()`), resolving from a `KvRegistry` carried on `RuntimeServices` (add a `kv_registry` field + builder setter, populated by adapters from `ctx.request().extensions().get::<KvRegistry>()`).
    - **Consent (`consent/mod.rs`, `storage/kv_store.rs`, `publisher.rs`):** change `ConsentPipelineInput.kv_store` from `Option<&dyn PlatformKvStore>` to `Option<KvHandle>`; update the consent persistence fns (`load_consent_from_kv`/`save_consent_to_kv`/`delete_consent_from_kv`) to take a `&KvHandle` and use its async methods (they already `block_on`). At the call site (`publisher.rs:626`) pass `settings.consent.consent_store.as_deref().and_then(|id| services.kv_handle_named(id))`.
    - Audit other KV consumers (`ec/*`) for the same "id dropped" pattern; they already use `kv_handle()` so are lower-risk.
  - **Adapters (all four `platform.rs`):** populate `RuntimeServices.kv_registry` from extensions in `build_runtime_services`. Remove Fastly's special consent-store reopening (`app.rs:205`, `runtime_services_for_consent_route`) — now redundant.
  - **Files:** `crates/trusted-server-core/src/platform/types.rs`, `crates/trusted-server-core/src/consent/mod.rs` (`ConsentPipelineInput.kv_store` type), `crates/trusted-server-core/src/storage/kv_store.rs` (consent persistence fns → `&KvHandle`), `crates/trusted-server-core/src/publisher.rs` (call site), `crates/trusted-server-adapter-{fastly,axum,cloudflare,spin}/src/platform.rs`, `crates/trusted-server-adapter-fastly/src/app.rs` (remove consent special-case), adapter test helpers (Step 2d).
  - **Test:** per adapter, a non-default KV id (`consent_store`) resolves via `kv_handle_named` and is **distinct** from the default handle; an unknown id → `None`.

- [ ] **Step 2d: Test-support — registry-populated `RequestContext` helper + migrate existing direct-context tests.** Strict registries make a missing registry a wiring bug, but existing adapter tests call `build_runtime_services(&ctx)` / `build_per_request_services(&ctx)` on **hand-built** `RequestContext`s with no registries inserted (e.g. `adapter-axum/src/app.rs:130`, `adapter-cloudflare/src/app.rs:151,314`, `adapter-spin/src/app.rs:440`, `adapter-cloudflare/src/platform.rs:729`). Those will now fail (composite → `registry.named()` → `None`). Add a shared test helper (e.g. `test_context_with_registries(config: &[…], kv: &[…], secrets: &[…]) -> RequestContext`) that inserts `ConfigRegistry`/`KvRegistry`/`SecretRegistry` into the context, and **migrate every existing direct-context test** to use it. Enumerate them during the run (`rg 'build_(runtime|per_request)_services'` in adapter test modules).

- [ ] **Step 3: Run to verify pass** — this task touches **core** (`platform/types.rs`, consent, `publisher.rs`) and **Fastly** (`platform.rs`/`app.rs`) as well as Axum/CF/Spin, so run **all four** adapters + wasm checks (per the global "all four green" rule), incl. the non-default config/secret/KV tests from 2b/2c.

Run: `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin && cargo check-cloudflare && cargo check-spin`
Expected: PASS.

- [ ] **Step 4: Commit** (include the core + Fastly changes, not just the three adapters)

```bash
git add crates/trusted-server-core/src/platform/types.rs \
  crates/trusted-server-core/src/consent/mod.rs \
  crates/trusted-server-core/src/storage/kv_store.rs \
  crates/trusted-server-core/src/publisher.rs \
  crates/trusted-server-adapter-fastly \
  crates/trusted-server-adapter-axum \
  crates/trusted-server-adapter-cloudflare \
  crates/trusted-server-adapter-spin
git commit -m "Wire RuntimeServices via composite + registry-backed named KV across all adapters"
```

---

## Task 6: Local Fastly registry builders + injection into the custom `oneshot` path

EdgeZero's Fastly `dispatch_with_registries` and its registry builders are `pub(crate)` (verified in the pinned checkout), so trusted-server must build the registries **locally** and insert them into the request extensions before `app.router().oneshot()`. (Alternative: an upstream EdgeZero public builder — tracked as **R11**; not assumed here.)

**Files:**
- Create: `crates/trusted-server-adapter-fastly/src/registries.rs` (`build_config_registry`, `build_secret_registry`, `build_kv_registry`)
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs:477` (the `oneshot` dispatch block)
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs:238` (`build_per_request_services` → build from composite, Step 4b)
- Test: `crates/trusted-server-adapter-fastly/src/registries.rs` (`#[cfg(test)]`) + a route test in `route_tests.rs`

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

- [ ] **Step 4b: Build Fastly `RuntimeServices` from the composite (else the injected registries are unused).** Fastly's `build_per_request_services` (`adapter-fastly/src/app.rs:238`) currently does `RuntimeServices::builder().config_store(Arc::new(FastlyPlatformConfigStore))…` — reading directly, ignoring the registries. Change it to extract the registries from extensions (`ctx.request().extensions().get::<ConfigRegistry>().cloned()` / `SecretRegistry`) and build `CompositeConfigStore`/`CompositeSecretStore` (reader = registry; writer = the Fastly write-only impl), exactly as Task 5 does for the other adapters. Without this, Steps 1–4 wire registries nothing reads.

- [ ] **Step 5: Write a failing Fastly route test** — `GET /.well-known/trusted-server.json` via the EdgeZero `oneshot` path returns the JWKS doc read through the injected `ConfigRegistry` (built with default + `jwks_store` ids). Name: `oneshot_discovery_reads_jwks_via_registry` (mirror the `StubJwksConfigStore`/`JWKS_CONFIG_STORE_NAME` pattern in `route_tests.rs`, but drive the EdgeZero path, not `route_request`).

Run: `cargo test-fastly oneshot_discovery_reads_jwks_via_registry` → Expected: FAIL, then PASS only after Steps 3, 4, **and 4b** (the test reads through `RuntimeServices`, which is composite-backed only after 4b — without 4b the injected registries are unused and the read still hits the old direct store).

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

- [ ] **Step 3: Run tests + wasm checks** — `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin && cargo check-cloudflare && cargo check-spin` → PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/trusted-server-core/src/settings_data.rs
git commit -m "Delete duplicated Fastly config-chunk resolver; rely on EdgeZero FastlyConfigStore"
```

---

## Task 8: Retire per-adapter config/secret READ impls — EXCEPT Fastly's, which the live legacy path still needs (D6-a)

Now that all reads (boot + request) flow through EdgeZero **on the edgezero path**, convert the per-adapter management impls to **write-only** (`PlatformConfigWriter`/`PlatformSecretWriter` from Task 3 Step 0) + `management_api.rs` (D6-a).

**⚠️ Phase 1 / Phase 5 boundary (Blocker fix):** Fastly's `legacy_main` (`adapter-fastly/src/main.rs:726`) is **still live** until Phase 5 (gated on 100% rollout, issue #495). It builds `RuntimeServices` via `build_runtime_services` (`adapter-fastly/src/platform.rs:578`), which wires `FastlyPlatformConfigStore` / `FastlyPlatformSecretStore` **for reads**. So **Fastly's read impls must NOT become write-only in Phase 1** — doing so breaks (or fails to compile) the legacy path before it is deleted. Therefore:
- **Axum / Cloudflare / Spin** read impls → **write-only** now (they have no legacy path).
- **Fastly** `FastlyPlatformConfigStore`/`FastlyPlatformSecretStore` stay **read+write** (full `PlatformConfigStore`/`PlatformSecretStore`) until Phase 5. The edgezero path on Fastly reads via the composite (Task 3–6); `legacy_main` reads via the direct impl. Both coexist. Fastly's read impls are deleted / narrowed to write-only in **Phase 5**, together with `legacy_main`.

**Files:**
- Modify: `crates/trusted-server-adapter-{axum,cloudflare,spin}/src/platform.rs` (→ write-only)
- Leave: `crates/trusted-server-adapter-fastly/src/platform.rs` config/secret **read** impls in place (write-only conversion deferred to Phase 5)
- Modify: `crates/trusted-server-adapter-fastly/src/route_tests.rs` (update stubs to the composite/registry shape)

- [ ] **Step 1: Convert the NON-Fastly config/secret impls to write-only.** For **Axum / Cloudflare / Spin**, re-implement the old read+write impls (`AxumPlatformConfigStore`, `NoopConfigStore`, Cloudflare/Spin equivalents, secret impls) as `PlatformConfigWriter`/`PlatformSecretWriter` (drop `get`/`get_bytes`; keep `put`/`create`/`delete`). This compiles only because Task 3 Step 0 split the traits. **Leave `FastlyPlatformConfigStore`/`FastlyPlatformSecretStore` as full read+write** — `legacy_main` still reads through them until Phase 5 (see the boundary note above). Keep `management_api.rs`.

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
