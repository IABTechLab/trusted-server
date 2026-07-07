# EdgeZero Store-Registry Migration (Phase 1, D6-a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route trusted-server's runtime **and boot-time** config/secret **reads** through EdgeZero stores/registries, add **named/non-default KV** selection (default KV is already EdgeZero; named KV / `consent_store` selection is **not** — see Step 2c), reconcile every logical store id (kv/config/secrets) with `edgezero.toml`, and delete the duplicated Fastly chunk resolver — while **keeping** the runtime **write** path (key rotation) intact via a composite store (decision **D6-a**).

**Architecture:** trusted-server core reads/writes stores through the bespoke `PlatformConfigStore`/`PlatformSecretStore` traits (each mixes read `get`/`get_string` + write `put`/`create`/`delete`), surfaced via `RuntimeServices` (one trait object per kind). EdgeZero's `ConfigStore`/`SecretStore` are **read-only**; per-request `ConfigRegistry`/`SecretRegistry` live in request extensions. This phase introduces a **composite store** whose *reads* resolve from EdgeZero and whose *writes* delegate to the existing management-API-backed impl, migrates the Fastly/Axum **boot** config read to EdgeZero, and adds **local** registry builders for Fastly's custom `oneshot` dispatch (EdgeZero's builders are `pub(crate)`).

**Tech Stack:** Rust (mixed edition — core/fastly 2021, axum 2024; follow each crate), toolchain 1.95.0, `error-stack` `Report<TrustedServerError>`, EdgeZero (`edgezero-core`/`edgezero-adapter-fastly` git dep, run `--locked`), Viceroy, `cargo test-{fastly,axum,cloudflare,spin}`.

**Spec:** `docs/superpowers/specs/2026-07-02-edgezero-full-migration-design.md` §5 Phase 1, D5, D6, §4a.

## Pinned dependency (verified)

This plan targets the EdgeZero commit pinned in `Cargo.lock`: **`branch = worktree-state-nested-secrets-spec-review` @ `d8f71a4a`** (PR [stackpop/edgezero#306](https://github.com/stackpop/edgezero/pull/306), now including the merged P0 State<T> + nested/array `#[secret]` work). That commit **has** every API this plan uses — re-verified against the pinned checkout: `store_registry.rs` (`StoreRegistry`/`ConfigRegistry`/`SecretRegistry`/`from_parts`), `StoresMetadata` + `Hooks::stores()` (`app.rs`), `dispatch_with_registries` + `build_*_registry` (adapter-fastly), `AxumDevServer::{with_config,with_kv,with_secret}_registry`, the `[stores.*]` `ids`/`default` manifest schema (`manifest.rs`, `deny_unknown_fields`), CloudflareConfigStore backed by **KV namespaces**, AxumConfigStore backed by **`.edgezero/local-config-<id>.json`**. **P0 caveat:** the nested-secret work reshaped `AppConfigMeta` from a `const SECRET_FIELDS` to `fn secret_fields() -> Vec<SecretField>`; `TrustedServerAppConfig`'s impl was updated to match (empty until Phase 3) and `cargo check` is green on core + all four adapters + CLI at this pin. Older cargo-cache checkouts (`6ebc29a5`, `ce6bcf7`, `7ec2ad1`, …) predate this pin — **confirm any API check against `d8f71a4a`**, not an older checkout dir.

> **Lockfile guard (execution):** `Cargo.toml` uses a **mutable branch** dependency (`branch = "worktree-state-nested-secrets-spec-review"`) and the upstream branch has **already advanced past `d8f71a4a`**. Execute every task with the committed `Cargo.lock` — build/test **`--locked`** and **do not `cargo update` the edgezero crates** unless the EdgeZero API is re-reviewed and this pin note re-verified. A silent bump could pull an unreviewed commit mid-migration.

## Global Constraints

- **Mixed Rust edition — follow each crate's `Cargo.toml`** (not global 2024): `trusted-server-core` and `trusted-server-adapter-fastly` are **2021**; `trusted-server-adapter-axum` is **2024**. Toolchain **1.95.0**; WASM target `wasm32-wasip1`.
- **Run every cargo command `--locked`.** The `Run:` snippets in the tasks below **omit `--locked` for brevity** — the executor MUST append it to **every** cargo invocation (each command in an `&&` chain too): `cargo test-fastly --locked`, `cargo check-cloudflare --locked`, etc. The edgezero dep is a **mutable branch** whose upstream head has advanced past the pinned `d8f71a4a`; `--locked` prevents a silent bump to an unreviewed commit (see the pin note). *(Safest: `export CARGO_NET_OFFLINE=true` for the session after the preflight fetch, so no command can update the lock.)*
- **Preflight before Task 1:** materialize + verify the pinned edgezero object locally so nobody re-checks APIs against a stale cache checkout — `cargo fetch --locked` then `cargo check -p trusted-server-core --locked`, and confirm the resolved rev is `d8f71a4a` (`grep 'edgezero-core' Cargo.lock`). The checkout dir is `…/git/checkouts/edgezero-efe7ff47d5367787/<short-rev>/` — but **do not assume it is materialized**: on a given machine the cache may only hold an *older* checkout (e.g. `7ec2ad1`), so `cargo fetch --locked` first, then verify the resolved rev is `d8f71a4a` before checking APIs against any checkout dir.
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
Expected (verified): **KV** ids = `ec.ec_store` (`ec_identity_store`, `settings.rs:452`), `consent.consent_store` (`consent_config.rs:80`), and `auction.creative_store` (`auction_config_types.rs:28`, default `"creative_store"`, **deprecated** — creatives are delivered inline); **config** ids = the app-config blob store (**store id `app_config`**, see D5 rule below), `request_signing.config_store_id`, the JWKS store (`JWKS_CONFIG_STORE_NAME`), and **DataDome's IP-CIDR config store** (`ProtectionIpCidrSourceConfig.config_store`, default `datadome-ip-bypass`, `protection_scope.rs:165`); **secret** ids = `secrets` (`request_signing.secret_store_id`), DataDome `ts_secrets`, the S3 secret store, `signing_keys` (`SIGNING_SECRET_STORE_NAME`) — versus `edgezero.toml` declaring only one id per kind. NOTE: `counter_store` (`RATE_COUNTER_NAME` in the Fastly `rate_limiter.rs`) and `opid_store` are **Fastly-only** platform stores, not `Settings` logical ids — out of scope for D5. `creative_store` **is** a `Settings` id: declare it in `[stores.kv]` (deprecated) so strict lookup can't fail, and flag it for removal in a later phase.

  **D5 app-config store-id/key decision (operator-confirmed — KEEP `app_config`):** the app-config blob stays in config **store id `app_config`**, blob **key `app_config`** (`CONFIG_BLOB_KEY` and `DEFAULT_CONFIG_STORE_ID` **unchanged**). We resolve the `settings_data.rs` (`app_config`) vs `edgezero.toml` (was `trusted_server_config`) inconsistency by **declaring `app_config` in `edgezero.toml`** (config `default = "app_config"`), **not** by renaming the code/config/tests. This avoids the entire rename cascade (config_payload, settings_data, example, integration fixtures, Viceroy generator, test envs, Cloudflare side-channel). Key == id == `app_config`, so `ts config push`'s default key and the boot read already agree with no env/`--key`. *(The earlier "rename to `trusted_server_config`" plan was reversed by operator decision on 2026-07-07.)*

  **Request-signing store ids (do NOT point at app-config):** request signing reads use hard-coded `JWKS_CONFIG_STORE_NAME = "jwks_store"` (config) + `SIGNING_SECRET_STORE_NAME = "signing_keys"` (secret); writes use `request_signing.config_store_id`/`secret_store_id`. Today the example sets these to `"app_config"`/`"secrets"` — which sends **writes to a different store than reads**. Fix: set `request_signing.config_store_id = "jwks_store"` and `secret_store_id = "signing_keys"` in `trusted-server.example.toml` + fixtures, and declare `jwks_store` (config) + `signing_keys` (secret) as logical ids in `edgezero.toml`. (Under the composite, reads resolve `registry.named("jwks_store")`; writes go to the same store via the writer/management id.)

- [ ] **Step 2: Enumerate runtime WRITE sites**

Run:
```bash
rg -n '\.config_store\(\)\.(put|delete)|\.secret_store\(\)\.(create|delete)' crates/trusted-server-core
```
Expected: only `KeyRotationManager` in `crates/trusted-server-core/src/request_signing/rotation.rs` (`store_private_key`, `store_public_jwk`, `deactivate_key`, `delete_key`). Confirm no other runtime writers.

- [ ] **Step 3: Record the kind-partitioned D5 map**

Append a table to "Task 1 Output": for each `{kv|config|secrets}` id → resolution (declare in `edgezero.toml`, or collapse onto the kind's default) → the concrete platform resource per adapter. **Under D7 there is no `EDGEZERO__STORES__*__NAME` mapping** — the logical id opens the same-named platform store, so the table records the *platform resource per adapter* (Fastly local/prod store, CF KV namespace / flat secret keys, Spin KV label / variable), **not** an env var. Spec default: app-config blob → config id **`app_config`** key `app_config` (kept, not renamed); JWKS → its own config id `jwks_store`; `ec_identity_store` → kv id; declare `signing_keys`/DataDome `ts_secrets`/S3 `s3-auth` as distinct secret ids.

- [ ] **Step 4: Confirm D6-a (or STOP)**

Confirm this phase keeps the write-capable composite (D6-a). Record it. **If the team instead chooses D6-b/c, stop here** and open a separate plan (`…-key-rotation-ops-migration.md`); do not proceed to Task 2.

- [ ] **Step 5: Commit the record**

```bash
git add docs/superpowers/plans/2026-07-02-edgezero-store-registry-migration.md
git commit -m "Record Phase 1 kind-aware store-id map and confirm D6-a"
```

---

## Task 2: Declare all store ids (kv/config/secrets) in `edgezero.toml` + reconcile fields/fixtures

**Files (exact) — app-config store KEPT as `app_config` (no rename cascade):**
- Modify: `edgezero.toml` (`[stores.kv]`/`[stores.config]`/`[stores.secrets]` `ids` + set config `default = "app_config"`)
- Modify: `trusted-server.example.toml` + `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml` — **only** the request-signing 2-line fix (`config_store_id = "jwks_store"`, `secret_store_id = "signing_keys"`)
- Create: `crates/trusted-server-core/src/testdata/all-store-refs.toml`
- Modify (platform manifests): `fastly.toml`, `crates/trusted-server-adapter-cloudflare/wrangler.toml`, `crates/trusted-server-adapter-cloudflare/cloudflare.toml` (reconcile/delete stale schema, Step 6), `crates/trusted-server-adapter-spin/spin.toml` (labels + drop stale `v_…` comment); **Create** `crates/trusted-server-adapter-spin/runtime-config.toml`. The Spin **serve command** (`spin up … --runtime-config-file …`) lives in `edgezero.toml` (~L95) and `CLAUDE.md` (~L87) — update both, not `spin.toml`.
- **Create** `docs/internal/store-provisioning.md` (operator runbook: Fastly mgmt-id==logical-id + store create/link, Cloudflare `wrangler secret put`, Spin runtime-config backends). **Modify** `crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml` (declare the new local Fastly stores for parity).
- Create: `crates/trusted-server-core/src/stores.rs` (shared `stores_metadata()`); Modify: each `crates/trusted-server-adapter-{fastly,axum,cloudflare,spin}/src/app.rs` (`impl Hooks` → add `fn stores()`)
- Test: `crates/trusted-server-core/src/settings.rs` (`#[cfg(test)]`)
- **NOT touched (would only change under the abandoned rename):** `config_payload.rs` (`CONFIG_BLOB_KEY` stays `app_config`), `settings_data.rs` (`DEFAULT_CONFIG_STORE_ID` stays `app_config`), `generate-viceroy-config.rs`, `tests/common/config.rs`, `tests/environments/{axum,cloudflare}.rs`, Cloudflare `app.rs` side-channel key.

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
Expected: FAIL — `ec_identity_store`/`consent_store`/`creative_store` (kv), `jwks_store`/`datadome-ip-bypass`/`app_config` (config), `signing_keys`/`ts_secrets` (secrets) referenced but not declared.

- [ ] **Step 3: Implement `referenced_store_ids_by_kind()` + manifest helper**

Add the `ReferencedStoreIds` struct + method returning **KV** ids (`ec.ec_store`, `consent.consent_store`, `auction.creative_store`), **config** ids (`request_signing.config_store_id`, the app-config store id, and **every `ProtectionIpCidrSourceConfig.config_store`** from DataDome scopes — default `datadome-ip-bypass`), **secret** ids (`request_signing.secret_store_id`, DataDome `ts_secrets`, S3). Do **not** include `counter_store`/`opid_store`. Add test-only `declared_store_ids_by_kind_from_manifest()` parsing `edgezero.toml`.

**D5 (operator decision — keep `app_config`):** do **NOT** rename the app-config store. `config_payload.rs::CONFIG_BLOB_KEY` and `settings_data.rs::DEFAULT_CONFIG_STORE_ID` **stay `"app_config"`** — we declare `app_config` in `edgezero.toml` instead (Step 4), avoiding the rename cascade. The **only** example/fixture edit in Task 2 is the request-signing fix: set `request_signing.config_store_id = "jwks_store"` and `secret_store_id = "signing_keys"` in `trusted-server.example.toml` + the integration fixture (they must match the read constants `JWKS_CONFIG_STORE_NAME`/`SIGNING_SECRET_STORE_NAME`, not the dormant `app_config`/`secrets`). Do **not** touch the Viceroy generator, `tests/common/config.rs`, `tests/environments/{axum,cloudflare}.rs`, or the Cloudflare side-channel key — those only needed changing under the abandoned rename.

- [ ] **Step 4: Declare every id in `edgezero.toml`** — `[stores.kv]` ids = `trusted_server_kv`, `ec_identity_store`, `consent_store`, `creative_store` (default stays `trusted_server_kv`); `[stores.config]` ids = **`app_config`**, `jwks_store`, `datadome-ip-bypass` with **`default = "app_config"`** (declare the existing app-config store name — no rename); `[stores.secrets]` ids = `trusted_server_secrets`, `signing_keys`, `ts_secrets`, `s3-auth` (default stays `trusted_server_secrets`). (Names double as the platform store names under D7.) `CONFIG_BLOB_KEY` stays `app_config`; blob key == store id == `app_config`, so `ts config push`'s default key and the boot read already agree with no env/`--key`. *(Confirm during the run whether `trusted_server_kv`/`trusted_server_secrets` are real code-referenced defaults or just edgezero.toml placeholders; if nothing references them, set the kv/secret defaults to a real referenced id instead.)*

- [ ] **Step 5: Wire `Hooks::stores()` on all four adapters (Blocker — metadata is not wired today).** Each `impl Hooks for TrustedServerApp` currently overrides only `routes()`; the default `stores()` returns **empty** `StoresMetadata`, so no registries can be built from it. Add `fn stores() -> StoresMetadata` returning the `[stores.*]` metadata, generated once from `edgezero.toml`. Prefer a single shared fn in `trusted-server-core` (`pub fn stores_metadata() -> StoresMetadata`) that all four adapters return, so the ids live in one place. Verify against `edgezero_core::app::StoresMetadata`/`StoreMetadata` shape.

- [ ] **Step 5b: Anti-drift test — `stores_metadata()` and every adapter's `Hooks::stores()` must equal `edgezero.toml`.** Registries are built from `TrustedServerApp::stores()`, **not** from the `edgezero.toml` that Step 1's test validates — so a stale/incomplete `stores_metadata()` would pass Step 1 while runtime registries silently miss ids. Add a test that parses `edgezero.toml`'s `[stores.*]` ids/default and asserts they equal `trusted_server_core::stores_metadata()` **and** each `<adapter>::TrustedServerApp::stores()` (per kind, ids as sets + default). Put the core half in `trusted-server-core` and one assertion in each adapter's test module (so a future adapter that forgets to return `stores_metadata()` fails).

- [ ] **Step 6: Declare the stores in every PLATFORM manifest (Blocker — local resources missing), per each adapter's real mapping.** D7 requires each logical id to be openable as a real platform store. The adapters map kinds to concrete resources differently — declare exactly:
  - **Fastly** — `fastly.toml` holds **only local Viceroy resources** under `[local_server]`: add each id as `[[local_server.kv_stores.<id>]]` / `[local_server.config_stores.<id>]` / `[local_server.secret_stores.<id>]`. The **production** Fastly KV/config/secret stores + their service links are **not** in `fastly.toml` — they are an **operator/provisioning step** (create via `fastly kv-store`/`config-store`/`secret-store` + link to the service, or the EdgeZero provision path). Document those commands in the operator runbook; do not try to express them in `fastly.toml`.
  - **Cloudflare manifest `cloudflare.toml` — reconcile the STALE schema (Medium blocker).** `crates/trusted-server-adapter-cloudflare/cloudflare.toml` still uses the pre-rewrite manifest schema (`[stores.kv].name = …`, `[stores.kv.adapters.cloudflare].name = …`), which the pinned EdgeZero manifest parser (`manifest.rs`, `deny_unknown_fields`) **rejects** in favor of `[stores.*]` `ids`/`default`. Either migrate it to the `ids`/`default` schema (matching `edgezero.toml`) or **delete it if `edgezero.toml` is the single source** and nothing loads `cloudflare.toml`. Do not leave a stale-schema manifest that a tool/test could load.
  - **Cloudflare** (`wrangler.toml`): EdgeZero backs **config stores by a KV namespace binding** (`config_store.rs`) — so each **config** id (`app_config`, `jwks_store`, `datadome-ip-bypass`) gets a `[[kv_namespaces]]` binding (as does each KV id). **Secrets use a FLAT namespace — `CloudflareSecretStore::get_bytes` ignores `store_name` and reads `env.secret(key)`.** So do **not** `wrangler secret put signing_keys` (that provisions the wrong name). Provision the concrete secret **keys the code reads**: the signing KIDs written by `KeyRotationManager`, the DataDome `server_side_key_secret_name`, and the S3 `access_key_id` / `secret_access_key` / optional session-token keys. Document the exact `wrangler secret put <key>` commands in the operator runbook; `store_name`/store-id is irrelevant on Cloudflare.
  - **Spin** (`spin.toml` **and** `runtime-config.toml`): config **and** KV ids open **KV-store labels** (`request.rs:282`). `spin.toml:41` currently declares only `key_value_stores = ["default"]` — extend it to list **every** kv+config logical id label (`trusted_server_kv`, `consent_store`, `creative_store`, `app_config`, `jwks_store`, `datadome-ip-bypass`). Each declared label **also needs a backend** in `runtime-config.toml` (`[key_value_store.<label>]`) — **create `crates/trusted-server-adapter-spin/runtime-config.toml`** (none exists today) mapping each label to a store backend (e.g. `spin` default, or a file backend for tests). **Also remove the stale doc comment** at `spin.toml:7` describing the old `v_…` component-variable encoding for config/secret stores (superseded by KV-label config + flat secrets). And **document the launch command** — EdgeZero loads the backends only when Spin is run with the runtime config, so the local-run instruction becomes `spin up --from crates/trusted-server-adapter-spin --runtime-config-file crates/trusted-server-adapter-spin/runtime-config.toml` (update `edgezero.toml`'s Spin serve command / CLAUDE.md's Spin smoke line accordingly). Secrets are a **flat** namespace (`SpinSecretStore` ignores `store_name`) mapped to Spin variables — provision the concrete secret **keys** (as for Cloudflare), lowercased per Spin's variable rules, not the store id.
  - **Viceroy integration template** (`crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml`): it declares `[local_server.kv_stores]` with only placeholder stores (`counter_store`). Once Fastly opens **every** declared KV id at runtime, missing local stores (`trusted_server_kv`, `consent_store`, `creative_store`) fail the parity suite. Add each declared kv/config/secret id to the template's `[local_server.*_stores]` blocks.
  - **Operator provisioning runbook** (**create `docs/internal/store-provisioning.md`**): the production store creation/link steps that don't live in any manifest — Fastly `kv-store`/`config-store`/`secret-store` create + service link (and the **requirement that the Fastly management resource id equals the runtime logical id** for `jwks_store`/`signing_keys`, per spec D5); Cloudflare `wrangler secret put <key>` for the concrete secret keys; Spin runtime-config backend setup. The spec mandates documenting this; give it a real file so it's committed, not just prose in the plan.

- [ ] **Step 6b: (REMOVED — no app-config rename).** The operator decision keeps the app-config store named `app_config`, so **none** of the `app_config → trusted_server_config` surface edits are needed: the Viceroy generator, `tests/common/config.rs`, `tests/environments/{axum,cloudflare}.rs`, the Cloudflare `settings_from_cloudflare_config_json` side-channel key, and the `wrangler.toml` placeholder JSON **stay as-is** (they already use `app_config`, which is now the declared store id). Only the request-signing `config_store_id`/`secret_store_id` 2-line fix (Step 3) touches `example.toml` + the fixture. Cross-check each manifest — some ids (`jwks_store`, `signing_keys`) may already be partially declared (`request_signing/mod.rs` doc references `fastly.toml`); add only the missing ones. (Axum dev-only `.edgezero/local-config-<id>.json` seeding stays documented, not committed.)

- [ ] **Step 7: Run to verify pass + full adapter suites + wasm checks**

Run: `cargo test-fastly every_referenced_store_id_is_declared_by_kind`
Then: `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin && cargo check-cloudflare && cargo check-spin`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
# NOTE: no app-config rename (keep `app_config`). Only the request-signing 2-line
# fix touches example.toml/fixture; the Viceroy generator, tests/common/config.rs,
# tests/environments/*, config_payload.rs, settings_data.rs are NOT changed in Task 2.
git add edgezero.toml fastly.toml trusted-server.example.toml \
  crates/trusted-server-adapter-cloudflare/wrangler.toml \
  crates/trusted-server-adapter-cloudflare/cloudflare.toml \
  crates/trusted-server-adapter-spin/spin.toml \
  crates/trusted-server-adapter-spin/runtime-config.toml \
  crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml \
  CLAUDE.md \
  docs/internal/store-provisioning.md \
  crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml \
  crates/trusted-server-core/src/settings.rs \
  crates/trusted-server-core/src/stores.rs \
  crates/trusted-server-core/src/lib.rs \
  crates/trusted-server-core/src/testdata/all-store-refs.toml \
  crates/trusted-server-adapter-fastly/src/app.rs \
  crates/trusted-server-adapter-axum/src/app.rs \
  crates/trusted-server-adapter-cloudflare/src/app.rs \
  crates/trusted-server-adapter-spin/src/app.rs
git commit -m "Declare store ids in edgezero.toml, platform manifests, Hooks::stores(); fix request-signing store ids (keep app_config)"
```

---

## Task 3: Registry-backed composite store (reads → EdgeZero registry by store_name, writes → management path)

Concrete D6-a mechanism. The bespoke traits read **by `StoreName`** and callers use **multiple** store ids (`app_config`, `jwks_store`, `datadome-ip-bypass`, `s3-auth`, `ts_secrets`, `ec_identity_store` for KV). So the composite must hold the **whole `ConfigRegistry`/`SecretRegistry`** (not a single handle) and resolve `named(store_name)` on each read; writes (`put`/`create`/`delete`) delegate to the existing management-API-backed writer. Preserves `KeyRotationManager` writes with zero call-site changes.

**Files:**
- Modify: `crates/trusted-server-core/src/platform/traits.rs` (split write-only traits)
- Create: `crates/trusted-server-core/src/platform/composite.rs` (`CompositeConfigStore`, `CompositeSecretStore`)
- Modify: `crates/trusted-server-core/src/platform/mod.rs` (export composite + writer traits)
- Modify: `crates/trusted-server-core/src/platform/types.rs` (`StoreName` doc → logical read id, Step 5)
- Test: `crates/trusted-server-core/src/platform/composite.rs` (`#[cfg(test)]`)

**Interfaces:**
- Consumes: `edgezero_core::store_registry::{ConfigRegistry, SecretRegistry, ConfigStoreBinding, BoundSecretStore}`, write-only `Arc<dyn PlatformConfigWriter>`/`Arc<dyn PlatformSecretWriter>`.
- Produces:
  - New **write-only** traits `PlatformConfigWriter { put; delete }` and `PlatformSecretWriter { create; delete }` (extracted from the read+write `PlatformConfigStore`/`PlatformSecretStore`). This is what lets Task 8 delete the per-adapter **read** impls while keeping the writer object — the writer no longer needs `get`/`get_bytes`.
  - `CompositeConfigStore::new(reader: Option<ConfigRegistry>, writer: Arc<dyn PlatformConfigWriter>) -> Self` implementing the full read+write `PlatformConfigStore`. The reader is `Option` because **an empty `StoreRegistry` cannot be constructed** (fields are private; `from_parts` returns `None` on empty `by_id`) — so the "absent registry" case is `None`, not an empty registry. `get(store_name, key)` = `let reg = self.reader.as_ref().ok_or(PlatformError::ConfigStore)?;` then (since **`ConfigRegistry::named(id)` returns `Option<ConfigStoreBinding>`, not a handle**) `let binding = reg.named(store_name.as_str()).ok_or(PlatformError::ConfigStore)?;` then `block_on(binding.handle.get(key))`. EdgeZero `ConfigStore::get` returns `Result<Option<String>, ConfigStoreError>`; the bespoke `get` returns `Result<String, PlatformError>`, so map `Ok(None)`/`Err(ConfigStoreError::*)` → `PlatformError::ConfigStore`. `put`/`delete` → `writer`.
  - `CompositeSecretStore::new(reader: Option<SecretRegistry>, writer: Arc<dyn PlatformSecretWriter>) -> Self` implementing `PlatformSecretStore`: `get_bytes(store_name, key)` = `self.reader.as_ref().ok_or(PlatformError::SecretStore)?.named(store_name.as_str()).ok_or(PlatformError::SecretStore)?` → `block_on(bound.get_bytes(key))`; map `Ok(None)`/`Err` → `PlatformError::SecretStore`. `create`/`delete` → `writer`. Both an **absent registry** (`None`) and a store_name not in the registry are hard errors (strict), never a silent fallback.

- [ ] **Step 0: Define the write-only traits (core only) — the per-adapter impls live in Task 5/6, not here.** In `traits.rs`, define `pub trait PlatformConfigWriter: Send + Sync { put; delete }` and `pub trait PlatformSecretWriter: Send + Sync { create; delete }` (matching the parent traits' `Send + Sync`). Keep `PlatformConfigStore`/`PlatformSecretStore` as the read+write surface `RuntimeServices` exposes. **Orphan-rule note:** the `PlatformConfigWriter` impls for adapter-owned types (`FastlyPlatformConfigStore` in `adapter-fastly`, `AxumPlatformConfigStore` in `adapter-axum`, Cloudflare/Spin) **cannot be written in core** (core can't depend on the adapter crates) — and a blanket `impl<T: PlatformConfigStore> PlatformConfigWriter for T` would coherence-conflict when Task 8 makes some impls writer-only. So each adapter impls the writer trait for its **own** store type **as the first step of its composite wiring** (Task 5 for Axum/Cloudflare/Spin; Task 6 for Fastly), forwarding to the store's existing `put`/`create`/`delete`. Task 3 here defines the traits and the composite that *consumes* `Arc<dyn PlatformConfigWriter>`; the writers are supplied per adapter. Run `cargo check -p trusted-server-core` to confirm the trait definitions + composite compile.

- [ ] **Step 1: Write the failing test — reads resolve the NAMED store; unknown store errors; writes delegate**

```rust
#[test]
fn composite_config_reads_named_store_and_writes_delegate() {
    // Arrange: a ConfigRegistry with TWO ids (default `app_config`, non-default `jwks_store`).
    let reader = config_registry(&[
        ("app_config", "current-kid", "kid-1"),
        ("jwks_store", "kid-1", "{\"kty\":\"OKP\"}"),
    ], "app_config");
    let writer = Arc::new(RecordingConfigWriter::default());
    let composite = CompositeConfigStore::new(Some(reader), writer.clone());
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

- [ ] **Step 1b: Write the failing SECRET-composite test too** (not just config — Cloudflare/Spin secrets are a **flat** namespace, so route tests won't prove store-id binding; this unit test does). Mirror the config test for `CompositeSecretStore`:
```rust
#[test]
fn composite_secret_reads_named_store_and_writes_delegate() {
    // SecretRegistry with default + a non-default `ts_secrets` id.
    let reader = secret_registry(&[
        ("trusted_server_secrets", "API_KEY", b"default-key"),
        ("ts_secrets", "server-side-key", b"dd-secret"),
    ], "trusted_server_secrets");
    let writer = Arc::new(RecordingSecretWriter::default());
    let composite = CompositeSecretStore::new(Some(reader), writer.clone());
    // Non-default store resolves.
    let v = composite.get_bytes(&StoreName::from("ts_secrets"), "server-side-key").expect("read");
    assert_eq!(v, b"dd-secret");
    // Unknown store id is a strict error.
    assert!(matches!(
        composite.get_bytes(&StoreName::from("nope"), "x").expect_err("should error on unknown secret store").current_context(),
        PlatformError::SecretStore,
    ));
    // create/delete delegate with the target StoreId preserved.
    composite.create(&StoreId::from("ts_secrets"), "new", "val").expect("create delegates");
    assert_eq!(
        writer.creates.lock().expect("should acquire writer lock").as_slice(),
        &[("ts_secrets".to_owned(), "new".to_owned(), "val".to_owned())],
        "create must delegate with the SAME StoreId",
    );
}
```

- [ ] **Step 2: Run to verify both fail**

Run: `cargo test-fastly composite_` (one shared-prefix filter runs both `composite_config_…` and `composite_secret_…`; `cargo test` takes a single filter).
Expected: FAIL (module does not exist).

- [ ] **Step 3: Implement `composite.rs`**

`get` resolves `reader.named(store_name)` → `ConfigStoreBinding`, then `block_on(binding.handle.get(key))`; `get_bytes` resolves `reader.named(store_name)` → `BoundSecretStore`, then `block_on(bound.get_bytes(key))` (mirror `storage/kv_store.rs`). Strict: `named` returning `None` → `PlatformError`; EdgeZero `Ok(None)`/`Err` → `PlatformError`. `put`/`create`/`delete` forward to `writer`. Add `config_registry(entries, default)` / `secret_registry(...)` test helpers that build a real `StoreRegistry` from in-memory EdgeZero stores (config entries wrapped as `ConfigStoreBinding { handle, default_key }`), and a `RecordingConfigWriter` (impl `PlatformConfigWriter`) that records **`(StoreId, key, value)`** tuples so tests assert the target StoreId is preserved on delegation.

- [ ] **Step 4: Run to verify it passes** — the trait split + writer impls (Step 0) touch **core**, so compile/test **all four** targets, not just Fastly.

Run: `cargo test-fastly composite_` (runs both config + secret composite tests), then confirm the core trait split compiles on every target incl. the **wasm** surfaces (repo rule): `cargo check -p trusted-server-adapter-axum && cargo check-cloudflare && cargo check-spin`
Expected: PASS.

- [ ] **Step 5: Reconcile `StoreName` semantics (D7).** `platform/types.rs::StoreName` is documented as an "edge-visible **platform** name". The composite now resolves `registry.named(store_name.as_str())` by **logical id**, so `StoreName` for reads must carry the **logical store id**. Update the `StoreName` doc comment to say "logical runtime store id" for reads, and audit read call sites (`request_signing/{signing,rotation}.rs`, `proxy.rs`, `integrations/datadome/{protection,protection_scope}.rs`) to confirm they pass **logical ids** (`app_config`, `jwks_store`, `ts_secrets`, `datadome-ip-bypass`, …), not physical platform names. No functional change if ids already equal names (D7 convention), but the doc + audit prevent implementers from passing physical names into logical registries.

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

- [ ] **Step 2: Re-type `get_settings_from_config_store`** to `(&ConfigStoreHandle, key: &str)`, called with **store id `app_config`, key `app_config`** (`CONFIG_BLOB_KEY`, unchanged). In Fastly `load_settings_from_config_store()` open the EdgeZero `FastlyConfigStore` for `app_config` at boot and wrap in a `ConfigStoreHandle`. In Axum `build_state()` open the EdgeZero Axum config store, which reads **`.edgezero/local-config-app_config.json`** (`edgezero-adapter-axum/src/config_store.rs` — id-scoped local file); do **not** apply any env-key override (D7). The adapter-level boot wiring is exercised by each adapter's existing `build_state` test path (no new Viceroy test needed — the core test above covers the parse logic).

- [ ] **Step 3: Run to verify pass** (core test + adapter boot suites)

Run: `cargo test-fastly get_settings_reads_blob_via_edgezero_handle`
Expected: PASS.
Then, since `get_settings_from_config_store`'s re-typing is a **core** change, confirm **all four** adapters + wasm surfaces still build/pass (the "all four green" rule):
Run: `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin && cargo check-cloudflare && cargo check-spin`
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
- **Modify (core KV surface, Step 2c):** `crates/trusted-server-core/src/platform/types.rs` (`RuntimeServices::kv_handle_named` + `kv_registry` field/builder), `crates/trusted-server-core/src/consent/mod.rs` + `storage/kv_store.rs` (consent KV surface → `KvHandle`), `crates/trusted-server-core/src/publisher.rs` (interim call site keeps `kv_handle()`; the named `consent_store` flip is **Task 6**)
- Modify: `crates/trusted-server-adapter-{axum,cloudflare,spin}/src/platform.rs` (`build_runtime_services` → composite + `kv_registry` from extensions)
- **Fastly named-KV + composite + `runtime_services_for_consent_route` removal → Task 6** (Fastly injects registries only in Task 6). Task 5 leaves Fastly compiling: the core `kv_handle_named` is additive and returns `None` on Fastly until Task 6 wires `kv_registry` (consent falls back safely).
- Test-support (Step 2d): a shared `test_context_with_registries(...)` helper; migrate existing direct-context tests in the **`adapter-{axum,cloudflare,spin}`** test modules (Fastly's is Task 6, per Step 2d)
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
Add `build_*_registry_axum(&StoresMetadata)` in `adapter-axum/src/registries.rs` mirroring Task 6's Fastly by-id builders but opening the EdgeZero **Axum** store primitives (`.edgezero/*` local backends). **KV-file path decision (make it explicit):** EdgeZero's Axum KV backend uses a **private** on-disk path scheme (`.edgezero/kv-<slug>-<hash>.redb`). If trusted-server reimplements the open by hand it risks pointing at a **different file** than EdgeZero's dev server, silently diverging local KV. Executable rule (pick during Task 1, record the choice):
  - **If EdgeZero exposes a public by-logical-id Axum KV constructor** at the pin (`grep -rn 'pub fn' …/edgezero-adapter-axum/src/key_value_store.rs`) → **use it**.
  - **Else** → copy the exact `.edgezero/kv-<slug>-<hash>.redb` path algorithm verbatim **plus a parity test** asserting the generated path byte-for-byte matches EdgeZero's for a known id, **and** file a separate "expose public Axum KV constructor" upstream ask (R11) so the copy is removed later. Do **not** ship a hand-written path without that parity test. This preserves PORT/axum.toml exactly and wires registries. **Bounded smoke (do not block the executor):** the dev server is long-lived, so don't run it bare — either `timeout 8 cargo run -p trusted-server-adapter-axum 2>&1 | grep -q 'Listening on'` (assert the bind log, non-zero timeout exit is expected/ignored), or run it in the background, `curl -fsS localhost:$PORT/health`, then kill it. Prefer an Axum route test over booting the server where possible.

**Interfaces:**
- Consumes: Task 3 composite; `ConfigRegistry`/`SecretRegistry` from request extensions.
- Produces: `RuntimeServices` whose reads flow through EdgeZero, writes through the composite writer.

- [ ] **Step 1: Write failing Axum tests covering the default AND a non-default config id AND a non-default secret id** (in the Axum app test module):
  - `discovery_reads_jwks_from_nondefault_config_store` — `GET /.well-known/trusted-server.json`: seed the Axum `ConfigRegistry` with two ids (`app_config` default + the JWKS store id); assert `200` + the JWKS `kid` in the body (proves non-default **config** id resolution).
  - `datadome_reads_secret_from_nondefault_secret_store` — a request to a **protected non-integration** route (the DataDome server-side key is read during protected-request *filtering* in `protection.rs`, which **skips** the `/integrations/datadome/*` routes — see `protection.rs:110`), so drive a publisher/first-party route in DataDome's protection scope. Seed the `SecretRegistry` with two ids (default + `ts_secrets`) and the server-side key under `ts_secrets`; assert the filter reads it (proves non-default **secret** id resolution).
  - `first_party_proxy_reads_s3_secret` — `GET /first-party/proxy` for an S3-auth asset route: seed the S3 secret id; assert the SigV4 path obtains the secret (proves the S3 secret read).

Run each (one filter per `cargo test` invocation):
```bash
cargo test-axum discovery_reads_jwks_from_nondefault_config_store
cargo test-axum datadome_reads_secret_from_nondefault_secret_store
cargo test-axum first_party_proxy_reads_s3_secret
```
Expected: FAIL (all three).

- [ ] **Step 2: Build `RuntimeServices` via the composite** in each adapter's `build_runtime_services(ctx: &RequestContext)`. **Extract the whole registry from request extensions** — `ctx.request().extensions().get::<ConfigRegistry>().cloned()` / `get::<SecretRegistry>()` — the same way EdgeZero's `Config`/`Secrets` extractors do. Do **not** use `ctx.config_store_default()`/`config_store(id)` (those return a single bound handle and would wire only the default store). **First, impl the writer traits for these adapters' own stores** (Task 3 Step 0 defers them here for the orphan rule): in each of `adapter-{axum,cloudflare,spin}/src/platform.rs`, `impl PlatformConfigWriter for <AdapterConfigStore>` / `impl PlatformSecretWriter for <AdapterSecretStore>` forwarding to their existing `put`/`create`/`delete`. Then pass the cloned registry **`Option`** as the composite reader (Task 3) and that write impl (as `Arc<dyn PlatformConfigWriter>`) as the writer. **Absent-registry policy (concrete):** `build_runtime_services` returns `RuntimeServices` (not `Result`) on all adapters, so don't add a fallible signature. The composite reader is `Option<ConfigRegistry>` / `Option<SecretRegistry>` (an empty `StoreRegistry` is unconstructable — private fields; `from_parts` → `None` on empty). So pass `ctx.request().extensions().get::<ConfigRegistry>().cloned()` (already an `Option`) straight into `CompositeConfigStore::new(...)`; when it's `None`, the composite's `get`/`get_bytes` **error** (`PlatformError`) on first read rather than silently reading a default store. A missing registry surfaces as a read error at the call site — no builder signature change, no empty-registry construction.

- [ ] **Step 2b: Non-default coverage on Cloudflare AND Spin (not just Axum).** Cloudflare/Spin platform mappings differ (config = KV-namespace/label backed; secrets = flat namespace), so default-only assertions are insufficient. Add, in each of the Cloudflare and Spin test modules, tests proving **non-default** resolution: a `jwks_store` **config** read and a `ts_secrets` / S3 **secret**-key read resolve through the composite (route tests if cheap, else small `build_runtime_services` + composite-read tests seeding a 2-id registry).

- [ ] **Step 2c: Named-KV resolution — a CORE surface change, not adapter-only (DECIDED — registry-backed KV now).** The core `RuntimeServices` exposes only `kv_store(&self) -> &dyn PlatformKvStore` — a **single** handle — and consumers that have a store id today **drop it**: `publisher.rs:626` does `settings.consent.consent_store.as_deref().map(|_| services.kv_store())`. So adapter-only changes cannot make `consent_store` resolve. This step is cross-cutting:
  - **Type decision — resolve named KV as `KvHandle`, and migrate consent onto `KvHandle`.** `KvRegistry::named(id)` yields a `KvHandle` (a wrapper `{ store: Arc<dyn KvStore> }`), **not** a `&dyn PlatformKvStore` — and `ConsentPipelineInput.kv_store` is currently `Option<&dyn PlatformKvStore>` (`consent/mod.rs:89`), so a `KvHandle` does not fit directly. Do **not** wrap; **migrate the consent KV surface to `KvHandle`** (the idiomatic edgezero handle — `RuntimeServices` already exposes `kv_handle()` for the default). Specifically:
    - **Core (`platform/types.rs`):** add `RuntimeServices::kv_handle_named(&self, id: &str) -> Option<KvHandle>` (mirroring the existing `kv_handle()`), resolving from a `KvRegistry` carried on `RuntimeServices` (add a `kv_registry` field + builder setter, populated by adapters from `ctx.request().extensions().get::<KvRegistry>()`).
    - **Consent type migration (Task 5) vs behavioral flip (Task 6) — avoid an interim Fastly regression.** Change `ConsentPipelineInput.kv_store` from `Option<&dyn PlatformKvStore>` to `Option<KvHandle>` and update the persistence fns (`load_consent_from_kv`/`save_consent_to_kv`/`delete_consent_from_kv`) to take `&KvHandle` (they already `block_on`) — **in Task 5**. But **do NOT flip the `publisher.rs:626` call site to `kv_handle_named` in Task 5**: Fastly has no `KvRegistry` until Task 6, so `kv_handle_named("consent_store")` would return `None` there and consent persistence would **silently skip on Fastly** between Task 5 and Task 6. Today Fastly consent works because `runtime_services_for_consent_route` (`app.rs:205`) reopens the consent store and **swaps the default** kv via `with_kv_store`. So in **Task 5**, the call site keeps today's behavior: pass `services.kv_handle()` (the default handle — which on Fastly is the swapped consent store; on the others matches current behavior). The **named-lookup flip** — `settings.consent.consent_store.as_deref().and_then(|id| services.kv_handle_named(id))` — and removal of the Fastly swap happen **atomically in Task 6**, once all four adapters (Fastly included) inject a `KvRegistry`. The behavioral test (below) therefore lands in **Task 6**.
    - Audit other KV consumers (`ec/*`) for the same "id dropped" pattern; they already use `kv_handle()` so are lower-risk.
  - **Adapters — Axum/Cloudflare/Spin here; Fastly in Task 6 (sequencing).** Populate `RuntimeServices.kv_registry` from extensions in `build_runtime_services` for **Axum/Cloudflare/Spin** (they inject registries via EdgeZero `run_app`/`dispatch_with_registries`). **Fastly's** named-KV wiring belongs in **Task 6**, because Fastly only injects registries into extensions in Task 6 (its custom `oneshot`); its active per-request services are built in `app.rs:238` (`build_per_request_services`), not `platform.rs`, and its consent special-casing (`app.rs:205`, `runtime_services_for_consent_route`, used at `app.rs:588/735`) is removed **there** once the `kv_registry` is present. Doing Fastly named-KV in Task 5 would populate from a registry not yet in extensions.
  - **Files (Task 5):** `crates/trusted-server-core/src/platform/types.rs`, `crates/trusted-server-core/src/consent/mod.rs` (`ConsentPipelineInput.kv_store` type), `crates/trusted-server-core/src/storage/kv_store.rs` (consent persistence fns → `&KvHandle`), `crates/trusted-server-core/src/publisher.rs` (interim call site, kept on `kv_handle()`), `crates/trusted-server-adapter-{axum,cloudflare,spin}/src/platform.rs`, adapter test helpers (Step 2d). **Fastly `platform.rs`/`app.rs` (populate `kv_registry`, remove consent special-case) + the `publisher.rs` named-lookup flip are Task 6**, not here.
  - **Test (Task 5):** the `kv_handle_named` surface resolves — per adapter, `kv_handle_named("consent_store")` returns a handle distinct from the default; unknown id → `None`. (The **behavioral** consent test — that `consent_store` is actually selected and the default is left untouched — lands in **Task 6** with the call-site flip; see Task 6.)

- [ ] **Step 2d: Test-support — registry-populated `RequestContext` helper + migrate existing direct-context tests.** Strict registries make a missing registry a wiring bug, but existing adapter tests call `build_runtime_services(&ctx)` / `build_per_request_services(&ctx)` on **hand-built** `RequestContext`s with no registries inserted (e.g. `adapter-axum/src/app.rs:130`, `adapter-cloudflare/src/app.rs:151,314`, `adapter-spin/src/app.rs:440`, `adapter-cloudflare/src/platform.rs:729`). Those will now fail (composite → `registry.named()` → `None`). Add a shared test helper (e.g. `test_context_with_registries(config: &[…], kv: &[…], secrets: &[…]) -> RequestContext`) that inserts `ConfigRegistry`/`KvRegistry`/`SecretRegistry` into the context, and migrate the **Axum/Cloudflare/Spin** direct-context tests to use it here. **Fastly's direct-context test migration (`route_tests.rs`, `app.rs`) belongs in Task 6**, since Fastly's composite/registry wiring lands there — doing it in Task 5 would edit files this task doesn't commit. Enumerate them (`rg 'build_(runtime|per_request)_services'` in the non-Fastly adapter test modules for Task 5; Fastly in Task 6).

Also convert the **existing core consent tests** that construct `ConsentPipelineInput` with a `&dyn PlatformKvStore` store — `crates/trusted-server-core/src/consent/mod.rs` has `kv_store: Some(&store)` call sites at ~lines 1450 / 1481 / 1492 — to the new `Option<KvHandle>` shape (build a `KvHandle` over an in-memory `KvStore` test double). These fail to compile the moment `ConsentPipelineInput.kv_store` changes type, so migrate them in the same step (`rg 'kv_store: Some\(' crates/trusted-server-core/src/consent` to find them all).

- [ ] **Step 3: Run to verify pass** — this task changes **core** (`platform/types.rs`, consent type, `publisher.rs` interim call site) and **Axum/CF/Spin** platform wiring. **Fastly is NOT modified here** (its named-KV wiring + consent flip are Task 6) — but run **all four** anyway to confirm Fastly still compiles/passes with the core changes (the `kv_handle()` interim call site preserves Fastly's swap behavior), per the "all four green" rule.

Run: `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin && cargo check-cloudflare && cargo check-spin`
Expected: PASS.

- [ ] **Step 4: Commit** (core + Axum/CF/Spin — **not** Fastly; Fastly is committed in Task 6)

```bash
git add crates/trusted-server-core/src/platform/types.rs \
  crates/trusted-server-core/src/consent/mod.rs \
  crates/trusted-server-core/src/storage/kv_store.rs \
  crates/trusted-server-core/src/publisher.rs \
  crates/trusted-server-adapter-axum \
  crates/trusted-server-adapter-cloudflare \
  crates/trusted-server-adapter-spin
git commit -m "Add named-KV surface + composite reads on Axum/Cloudflare/Spin; migrate consent to KvHandle"
```

---

## Task 6: Local Fastly registry builders + injection into the custom `oneshot` path

EdgeZero's Fastly `dispatch_with_registries` and its registry builders are `pub(crate)` (verified in the pinned checkout), so trusted-server must build the registries **locally** and insert them into the request extensions before `app.router().oneshot()`. (Alternative: an upstream EdgeZero public builder — tracked as **R11**; not assumed here.)

**Files:**
- Create: `crates/trusted-server-adapter-fastly/src/registries.rs` (`build_config_registry`, `build_secret_registry`, `build_kv_registry`)
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs:477` (the `oneshot` dispatch block)
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs:238` (`build_per_request_services` → build from composite; remove `runtime_services_for_consent_route` + its call sites at `app.rs:584/735`, Step 4b)
- Modify: `crates/trusted-server-adapter-fastly/src/platform.rs` (`impl PlatformConfigWriter`/`PlatformSecretWriter` for the Fastly stores, Step 4b)
- Modify (Step 5b, core consent flip): `crates/trusted-server-core/src/consent/mod.rs` (`resolve_consent_kv`), `crates/trusted-server-core/src/publisher.rs` (both call sites), `crates/trusted-server-core/src/auction/endpoints.rs` (auction fail-closed)
- Test: `crates/trusted-server-adapter-fastly/src/registries.rs` (`#[cfg(test)]`) + route tests in `route_tests.rs` (incl. the Fastly consent/503 gates); + core consent/auction tests; + migrate Fastly direct-context tests (deferred from Task 5 Step 2d)

**Interfaces:**
- Consumes: `StoresMetadata` (from `Hooks::stores()`), EdgeZero `FastlyConfigStore`/`FastlyKvStore`/`FastlySecretStore` open primitives, `StoreRegistry::from_parts` (which returns **`Option<Self>`** — `None` when the default id is absent from `by_id`).
- Produces (signatures mirror EdgeZero's own private Fastly builders):
  - `build_kv_registry(&StoresMetadata) -> Result<Option<KvRegistry>, FastlyError>` — KV store `open` can fail (→ `Err`); a metadata with no KV stores or a missing default → `Ok(None)`.
  - `build_config_registry(&StoresMetadata) -> Option<ConfigRegistry>` and `build_secret_registry(&StoresMetadata) -> Option<SecretRegistry>` — `None` when the kind is undeclared or the default id can't be assembled.
  - **Failure policy:** each opens every declared id **by logical id** (D7). If a *declared* store fails to open (KV `Err`) propagate it to the request as an error; if the *default* id is missing, `from_parts` yields `None` → the registry is not inserted → the strict extractor later returns `None` → the handler surfaces a 500 (no silent fallback). This matches EdgeZero's `dispatch_with_registries` behavior for the other adapters.

- [ ] **Step 1: (D7) No runtime env reader.** Per D7 the runtime does **not** read `EDGEZERO__STORES__*__NAME` — stores are opened by **logical id**. This deletes the need for a Fastly runtime-dictionary `EnvConfig` reader (and sidesteps that `fastly::ConfigStore` has no `iter()` and EdgeZero's reader is private). If a deployment ever needs to remap a physical store name, that is handled at provisioning time, not here. No code in this step; it records the design constraint the builders follow.

- [ ] **Step 2: Write a failing builder test** — `build_config_registry` opens each declared id by name and yields a registry whose `default()` resolves and whose declared non-default id (`jwks_store`) resolves; an id **not** in `StoresMetadata` is absent (`named("nope").is_none()`). Name: `build_config_registry_resolves_declared_ids`.

Run: `cargo test-fastly build_config_registry_resolves_declared_ids` → Expected: FAIL.

- [ ] **Step 3: Implement the three builders** in `registries.rs` with the signatures above — the three kinds construct **differently** (mirror EdgeZero's own private Fastly builders, `request.rs`):
  - **KV** (`build_kv_registry -> Result<Option<KvRegistry>, FastlyError>`): `FastlyKvStore::open(id)` returns `Result<FastlyKvStore, KvError>` (not a `KvHandle`), so for each id `let store = FastlyKvStore::open(id)?;` then wrap `KvHandle::new(Arc::new(store))` (map `KvError` → `FastlyError`); collect into `BTreeMap<String, KvHandle>`; `StoreRegistry::from_parts(by_id, default_id)`.
  - **Config** (`-> Option<ConfigRegistry>`): for each id, build a `ConfigStoreHandle` over `FastlyConfigStore` for that id, paired as `ConfigStoreBinding { handle, default_key: id.to_owned() }` — under **D7** the `default_key` is the **logical id** (no `EnvConfig::store_key` lookup); `from_parts`.
  - **Secret** (`-> Option<SecretRegistry>`): **do NOT open per id.** Create **one** `SecretHandle::new(Arc::new(FastlySecretStore))` (the provider is stateless — `FastlySecretStore::get_bytes(store_name, key)` opens the named store per call), then bind each id via `BoundSecretStore::new(handle.clone(), store_name)` where `store_name` = the logical id (D7); `from_parts`.
  - All open **by logical id** (D7 — no `EnvConfig`/runtime dictionary). `from_parts` yields `None` if a kind is undeclared or the default id is absent.

- [ ] **Step 4: Insert registries in the oneshot block** — replace the lone `core_req.extensions_mut().insert(config_store)` at `main.rs:477`: build the three registries via Step 3 (propagate `build_kv_registry`'s `FastlyError` into the dispatch's `Result`), and `if let Some(reg) = ...` insert each into `core_req.extensions_mut()`, preserving the existing `client_info`/`device_signals` inserts.

- [ ] **Step 4b: Build Fastly `RuntimeServices` from the composite + wire named KV + remove consent special-casing (Fastly half of Step 2c, sequenced here).** Fastly's `build_per_request_services` (`adapter-fastly/src/app.rs:238`) currently does `RuntimeServices::builder().config_store(Arc::new(FastlyPlatformConfigStore))…` — reading directly, ignoring the registries. Now that Step 4 injects `Config`/`Secret`/`Kv` registries into extensions, change it to:
  - first `impl PlatformConfigWriter for FastlyPlatformConfigStore` / `impl PlatformSecretWriter for FastlyPlatformSecretStore` in `adapter-fastly/src/platform.rs` (forwarding to their `put`/`create`/`delete`) — the Fastly half of Task 3 Step 0's deferred writer impls;
  - extract the registries from extensions (`ctx.request().extensions().get::<ConfigRegistry>().cloned()` / `SecretRegistry` / `KvRegistry`) and build `CompositeConfigStore`/`CompositeSecretStore` (reader = registry; writer = the Fastly write impl above; the read impl stays for legacy per Task 8) — as Task 5 does for the other adapters;
  - populate `RuntimeServices.kv_registry` from the `KvRegistry` (Step 2c core surface), so `kv_handle_named("consent_store")` works on Fastly;
  - **remove `runtime_services_for_consent_route` (`app.rs:205`) and its call sites (`app.rs:588/735`)** — consent now selects its store via `kv_handle_named`, so the special reopening is redundant.
  Without this, Step 4 wires registries nothing reads, and named consent KV stays Fastly-special.

- [ ] **Step 5: Write a failing Fastly route test** — `GET /.well-known/trusted-server.json` via the EdgeZero `oneshot` path returns the JWKS doc read through the injected `ConfigRegistry` (built with default + `jwks_store` ids). Name: `oneshot_discovery_reads_jwks_via_registry` (mirror the `StubJwksConfigStore`/`JWKS_CONFIG_STORE_NAME` pattern in `route_tests.rs`, but drive the EdgeZero path, not `route_request`).

Run: `cargo test-fastly oneshot_discovery_reads_jwks_via_registry` → Expected: FAIL, then PASS only after Steps 3, 4, **and 4b** (the test reads through `RuntimeServices`, which is composite-backed only after 4b — without 4b the injected registries are unused and the read still hits the old direct store).

- [ ] **Step 5b: Flip the consent call site to named KV — FAIL CLOSED — + behavioral test (the atomic cutover; all four adapters now inject a `KvRegistry`).** Now that Fastly (Step 4b) and Axum/CF/Spin (Task 5) all inject a `KvRegistry`, resolve the consent store by id. **Do NOT use `.and_then(|id| kv_handle_named(id))`** — that silently yields `None` (= "no persistence") when a store **is** configured but unresolved, regressing today's **fail-closed** behavior (Fastly returns **503** on consent-dependent routes for a missing consent store — see the existing tests `dispatch_auction_with_missing_consent_store_returns_503` and `edgezero_missing_consent_store_breaks_only_consent_routes` in `adapter-fastly/src/app.rs`). Instead, at `publisher.rs:626`:
```rust
let consent_kv = match settings.consent.consent_store.as_deref() {
    Some(id) => Some(services.kv_handle_named(id).ok_or_else(|| Report::new(
        TrustedServerError::KvStore { store_name: id.to_owned(), message: "consent store not resolved".into() }))?),
    None => None, // consent persistence intentionally disabled
};
```
So **configured-but-unresolved → error (→ 503 on consent-dependent routes)**; **unconfigured → `None` (persistence off)**. Integration routes stay unaffected (they don't require consent KV). Extract this into a **shared core helper** `resolve_consent_kv(settings, services) -> Result<Option<KvHandle>, Report<TrustedServerError>>` so the fail-closed logic lives in one place. **Also fix the revocation delete path (`publisher.rs:885`)**, which currently does `if consent_store.is_some() { delete_consent_from_kv(services.kv_store(), …) }` using the **default** KV — route it through `resolve_consent_kv` so revocation deletes from `consent_store`, not the default.

  **Cover the auction route too (not just publisher) — this is where the removed Fastly wrapper's guard must land.** The Fastly auction route (`adapter-fastly/src/app.rs:584`) got its fail-closed from `runtime_services_for_consent_route` (removed in Step 4b, *"auction reads consent data … fail closed with 503"*). **Correction on mechanism:** `handle_auction` (`auction/endpoints.rs`) does **not** build its consent from KV — auction consent comes from `ec_context.consent()` (`endpoints.rs:113`), and `KvIdentityGraph` (`endpoints.rs:50`) is the separate EC-identity-graph input. So the guard is **not** about the EC graph: `handle_auction` must call `resolve_consent_kv(settings, services)?` **purely as a fail-closed guard** (it returns `Err → 503` when `consent_store` is configured but unresolved) — **without changing EC-graph semantics** — replacing the adapter-level wrapper with core-level fail-closed that works on **all four** adapters. **The existing Fastly tests `dispatch_auction_with_missing_consent_store_returns_503` and `edgezero_missing_consent_store_breaks_only_consent_routes` are mandatory gates — they must still pass** after the wrapper is removed.
Add the **behavioral** core test (failing first): with `consent_store = "consent_store"` + a registry holding a **default** KV + a distinct `consent_store` KV, a consent round-trip (load/save/delete) reads/writes the **`consent_store`** handle and leaves the **default** store **untouched**; add a second test that a **configured-but-unresolved** consent store **errors** (not silently skips). Confirm the existing Fastly 503 tests still pass. Files: `crates/trusted-server-core/src/consent/mod.rs` (`resolve_consent_kv` helper), `crates/trusted-server-core/src/publisher.rs` (both call sites), `crates/trusted-server-core/src/auction/endpoints.rs` (auction fail-closed), + core consent/auction tests.

- [ ] **Step 5c: Fastly named-KV / consent route test.** With `runtime_services_for_consent_route` removed (4b), add a Fastly test proving `consent_store` resolves via the **injected `KvRegistry`** — a consent-persisting route (or a `build_per_request_services`-level test) writes/reads through the `consent_store` handle, not the default. This guards the special-case removal.

- [ ] **Step 6: Fastly suite + parity + commit** (core consent flip is committed here with the Fastly work, since the flip is only safe once Fastly injects registries)

Run: `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin && cargo check-cloudflare && cargo check-spin && cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`
```bash
git add crates/trusted-server-adapter-fastly \
  crates/trusted-server-core/src/publisher.rs \
  crates/trusted-server-core/src/consent/mod.rs \
  crates/trusted-server-core/src/auction/endpoints.rs
git commit -m "Inject Fastly registries; flip consent to named KV (fail-closed, incl. auction); remove consent special-casing"
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
git commit -m "Retire non-Fastly per-adapter config/secret read impls; reads via EdgeZero, writes via composite (Fastly reads kept until Phase 5)"
```

---

## Task 1 Output (recorded 2026-07-06; pin `d8f71a4a`, `--locked` preflight green)

**Preflight:** `cargo fetch --locked` OK; `d8f71a4a` checkout materialized; `cargo check -p trusted-server-core --locked` green. All registry APIs present at the pin (per the "Pinned dependency" note).

**D6 decision: D6-a (confirmed by operator).** Runtime key-rotation writes stay via the composite writer + `management_api.rs`; reads move to EdgeZero. Verified the **only** runtime write sites are `KeyRotationManager` (`request_signing/rotation.rs`): `.put(&config_store_id, …)` (JWKS `current-kid`/`active-kids`/per-kid) at L196/209/224, `.create(&secret_store_id, …)` at L176, `.delete(&{config,secret}_store_id, …)` at L96/116/124/285/292. No other runtime writer exists. `management_api.rs` is **retained**.

**D5 store-id map (kind-partitioned; declare all in `edgezero.toml`):**

| Kind | Logical id | Source | Reconcile |
|---|---|---|---|
| KV | `ec_identity_store` | `ec.ec_store` (`settings.rs:452`, example L16) | declare |
| KV | `consent_store` | `consent.consent_store` (`consent_config.rs:80`) | declare |
| KV | `creative_store` | `auction.creative_store` (`auction_config_types.rs:30`, **deprecated**) | declare (keep strict lookup safe) |
| config | **`app_config`** (KEEP — operator decision) | app-config blob; `CONFIG_BLOB_KEY`/`DEFAULT_CONFIG_STORE_ID` **stay `app_config`** | **declare `app_config` in `edgezero.toml`** (set `[stores.config]` default to `app_config`) — NO rename cascade; do **not** touch `config_payload.rs`/`settings_data.rs`/the Viceroy generator/test envs/Cloudflare side-channel for the app-config store |
| config | `jwks_store` | `JWKS_CONFIG_STORE_NAME` (`request_signing/mod.rs:40`); `request_signing.config_store_id` | set example/fixtures `config_store_id = "jwks_store"` (today wrongly `app_config`) |
| config | `datadome-ip-bypass` | `default_ip_cidr_source_store` (`protection_scope.rs:164`) | declare |
| secret | `signing_keys` | `SIGNING_SECRET_STORE_NAME` (`mod.rs:46`); `request_signing.secret_store_id` | set example/fixtures `secret_store_id = "signing_keys"` (today wrongly `secrets`) |
| secret | `ts_secrets` | `default_server_side_key_secret_store` (`datadome.rs:242`) | declare |
| secret | `s3-auth` | `default_s3_secret_store` (`settings.rs:654`) | declare |

`counter_store`/`opid_store` are Fastly-adapter constants (rate limiter / opid), **not** `Settings` ids — out of scope. Requirement (spec D5): Fastly management resource id **==** runtime logical id for `jwks_store`/`signing_keys` (operator runbook).

**Axum local-KV path (deferred to Task 5 impl):** first `grep 'pub fn' …/edgezero-adapter-axum/src/key_value_store.rs` at the pin — use a public by-logical-id constructor if present; else copy the `.edgezero/kv-<slug>-<hash>.redb` algorithm **with a parity test** + file the upstream ask.

**No plan/spec conflicts surfaced in pre-flight.** Proceed to Task 2.

---

## Scope, gating, and follow-ups

- **D6-a locked.** Runtime key-rotation writes stay on the management path via the composite. If Task 1 selects D6-b/c, this plan **stops after Task 1**; a separate `key-rotation-ops-migration` plan handles the admin-surface change.
- **R11 (open):** whether EdgeZero should expose a **public** registry-builder helper (so Fastly need not maintain local builders, Task 6). Decide with the edgezero maintainer; not assumed here.
- **Not in this phase:** `RuntimeServices` removal (Phase 4); Cloudflare/Spin `include_str!`/side-channel config removal (Phase 2); `from_toml_and_env` + `config` dep (Phase 2); `Redacted<T>` / secret externalization (Phase 3); `management_api.rs` deletion (only under a future D6-b).
- **No dependency on edgezero #305** — Phase 1 uses shipped EdgeZero store APIs only.
