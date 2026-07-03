# Trusted Server → EdgeZero — Full Migration (umbrella design)

- **Status:** Draft for review
- **Date:** 2026-07-02
- **Scope:** Move trusted-server **completely** onto EdgeZero primitives: config push, KV, secret store, config injection without an embedded `trusted-server.toml`, extractor-based handlers, and deletion of every pre-EdgeZero workaround.
- **Shape:** Umbrella roadmap. Defines the end-state, the current-state gap, and an ordered set of phases with dependencies. **Each phase gets its own implementation plan** (`writing-plans`) before code is written.
- **Companion spec:** Phase 0 (`State<T>` extractor + nested `#[secret]`) is an **edgezero-repo** change, specified separately (`…-state-and-nested-secrets-design.md`) and tracked via edgezero PR [stackpop/edgezero#305](https://github.com/stackpop/edgezero/pull/305). This umbrella depends on it but does not re-specify it.

---

## 1. End-state

trusted-server is a fully EdgeZero-native app: adapter binaries are thin entry points (`run_app::<App>` where the platform allows it, or a **documented adapter-level dispatch shim** where it does not — see Fastly below); core is platform-neutral; config, KV, and secrets flow exclusively through EdgeZero's `StoreRegistry`; app config is a signed blob published by `ts config push` and read back typed with secrets resolved from the secret store; handlers are `#[action]` functions taking `FromRequest` extractors; and no *pre-EdgeZero* shim remains in core or the adapters.

**Fastly is not `run_app::<App>` today and may not be at the end** (Blocker, verified `adapter-fastly/src/main.rs`): Fastly deliberately calls `app.router().oneshot()` directly instead of the standard dispatch helpers, because (a) the helpers convert through `fastly::Response` via `set_header`, which **drops duplicate `Set-Cookie` values** from publisher/origin responses, and (b) `run_app_*` triggers a **logger reinit** Fastly must avoid. Fastly also injects `client_info` + `device_signals` (TLS JA4 / H2 fingerprint) into request extensions from the *original* `FastlyRequest` before conversion — signals a reconstructed EdgeZero request cannot expose. This is an **EdgeZero-adapter capability gap**, not trusted-server cruft. Resolution is a **prerequisite (P0-C)**, see §4a.

Concretely, at the end of this migration:

- **No adapter/runtime app-config baking** — no `include_str!` of `*.toml` **app config** in any adapter runtime path; all four adapters load app config from the EdgeZero config store. (The `ts config init` CLI command still embeds `trusted-server.example.toml` as a scaffolding template — that is not runtime config baking and is out of scope.)
- **No app-level secrets embedded in the pushed config blob.** Secrets live in the EdgeZero secret store; the blob carries only key names, resolved at request time.
- **No bespoke `PlatformConfigStore` / `PlatformSecretStore` / `RuntimeServices`.** Core and adapters use EdgeZero `ConfigStore` / `SecretStore` / `KvStore` via `StoreRegistry`.
- **No `FastlyManagementApiClient`, no `settings_data.rs` chunk resolver, no `config`-crate env overlay, no `Redacted<T>`.**
- **Core handlers are extractor-based**; the per-adapter handler shims are gone.
- **The legacy Fastly `route_request` path, `compat.rs`, and the `edgezero_enabled` / `edgezero_rollout_pct` flags are deleted** (final phase, gated on 100% rollout).

---

## 2. Current-state gap analysis

Verified across `trusted-server-core`, the four adapters, `trusted-server-cli`, and the pinned `edgezero` dependency.

| Concern | Today | Gap to close |
|---|---|---|
| **KV** | ✅ 100% on EdgeZero (`KvStore`/`KvHandle`, re-exported as `PlatformKvStore`) | None (baseline for the pattern) |
| **Routing** | ✅ All 4 adapters route through EdgeZero `RouterService` + `Hooks` | None structurally; handler authoring changes in Phase 4 |
| **Core off `fastly::` types** | ✅ Enforced by `migration_guards.rs` | Keep the guard; extend coverage as adapters shrink |
| **Config load** | ⚠️ Fastly + Axum load the blob from the config store; **Cloudflare** reads a `TRUSTED_SERVER_CONFIG` env side-channel (native fallback `include_str!`); **Spin `include_str!` `trusted-server.example.toml`** — none of these is a boot-time config-store read | Phase 2 (P-BOOT) |
| **Config injection** | ⚠️ `TrustedServerAppConfig` wraps `Settings`, `SECRET_FIELDS = &[]` → secrets inline in blob; `#[derive(AppConfig)]`/`#[secret]` unused | Phases 2–3 |
| **Config / Secret stores** | ❌ Core uses bespoke `PlatformConfigStore`/`PlatformSecretStore` + `RuntimeServices`; 4× per-adapter `platform.rs` impls; `FastlyManagementApiClient` for writes | Phase 1 |
| **Fastly config chunking** | ❌ `settings_data.rs` re-implements EdgeZero's `chunked_config.rs` verbatim in core | Phase 1 |
| **Env overlay** | ❌ `from_toml_and_env` + `TRUSTED_SERVER__*` via the `config` crate (test-only, but keeps the dep) | Phase 2 |
| **Handlers → extractors** | ❌ Hand-written `Fn(RequestContext)` shims calling `(&Settings, &RuntimeServices, Request)`; `#[action]`/`FromRequest` unused; **no `State<T>` extractor exists upstream** | Phase 0 (upstream) → Phase 4 |
| **Legacy Fastly path** | ❌ `legacy_main`/`route_request` + `compat.rs` + rollout flags live (marked "TODO delete after Phase 5 cutover — #495") | Phase 5 |

**Key architectural constraints discovered:**

1. **No `State`/`Extension` extractor in EdgeZero.** trusted-server threads `Arc<AppState>` (`Settings`, `AuctionOrchestrator`, `IntegrationRegistry`) via closures. Extractor migration needs an upstream `State<T>` → **Phase 0**.
2. **`AppConfig<C>` re-parses + verifies + secret-walks the whole blob every request** — too costly for `Settings`. Decision: keep loading `Settings` once at startup into `Arc<Settings>`, exposed via `State<Settings>`, rather than the per-request `AppConfig` extractor (see §4, Decision D1).
3. **Full secret externalization needs nested/array `#[secret]`** because `Settings` is deeply nested → **Phase 0** (edgezero derive change).
4. **Integration proxies are a second, nested `matchit` router** with their own `IntegrationProxy::handle(&Settings, &RuntimeServices, req)` convention — orthogonal to the core route handlers (see §4, Decision D2).
5. **Fastly cannot use `run_app::<App>` today** — it bypasses standard dispatch for multi-value `Set-Cookie` preservation, to skip a logger reinit, and to capture TLS JA4 / H2 fingerprints from the raw `FastlyRequest`. Needs an EdgeZero-adapter capability (**P0-C**, §4a) or a permanent documented exception → **Phase 0 / §4a**.
6. **Config is loaded at boot, before any request context exists** (`build_state()` → `load_startup_settings()`), but EdgeZero's config-store handle is only wired *per request* (`ConfigRegistry` in request extensions). On Cloudflare, config arrives via a `TRUSTED_SERVER_CONFIG` env side-channel injected at the worker entry; on Spin it's baked example TOML. So "load config from the store at startup" needs a **boot-time store-access mechanism**, not the per-request registry → **§4a + Phase 2**.
7. **The logical app-config store id is inconsistent** — `settings_data.rs` defaults to `app_config`, `edgezero.toml` declares `trusted_server_config`, and Fastly splits rollout flags (`trusted_server_config`) from the app-config blob (`app_config`). Must be unified → **Decision D5**, before Phase 1/2 planning.

---

## 3. Phase map (ordered, foundation-first)

Each phase leaves **all four adapters building and green**. Dependencies are explicit; within a phase, work can parallelize.

```
Phase 0 (edgezero, external)  ── State<T> extractor ─────────────┐
                              └─ nested/array #[secret] ───┐     │
                                                           v     v
Phase 1 (stores) ──> Phase 2 (config) ──> Phase 3 (secrets)   Phase 4 (extractors)
                                                                     │
                                                        Phase 5 (legacy removal, gated 100% rollout)
```

- **Phase 1** depends on nothing upstream (EdgeZero store APIs already exist).
- **Phase 2** depends on Phase 1 (config store must be EdgeZero-native first).
- **Phase 3** depends on Phase 2 **and** Phase 0's nested `#[secret]`.
- **Phase 4** depends on Phase 0's `State<T>`; it can run in parallel with 1–3 once Phase 0 lands, but is cleaner after Phase 1 (so handlers pull EdgeZero stores, not `RuntimeServices`).
- **Phase 5** is last and **gated on the edgezero rollout reaching 100%** (issue #495).

---

## 4. Cross-cutting design decisions

**D1 — Config caching, not per-request extraction.** Keep the load-once model: at startup each adapter reads the blob from the config store, verifies the envelope, resolves secrets, validates, and stores `Arc<Settings>` in `AppState`. Handlers read it via `State<Settings>` (Phase 4). Rationale: `AppConfig<Settings>`'s per-request re-parse/verify/secret-walk is prohibitive for a struct this large. Trade-off: config changes require a new deploy/boot to take effect (already true today). *This diverges deliberately from the stock `AppConfig<C>` extractor; documented as such.*

**D2 — Integration proxy router stays put (for now).** Phase 4 migrates the **named/core routes** to extractors. The integration registry's nested `matchit` dispatch and `IntegrationProxy::handle` signature are internal, working, and orthogonal; migrating them is a **follow-up** (Phase 4b, optional), not silently dropped. Called out so the extractor migration isn't mistaken for "all handlers."

**D3 — Secret resolution happens at startup, not per request.** With D1, the startup config load resolves `#[secret]` fields against the secret store once (Phase 3). Adapters must therefore have a secret-store handle available **at boot**, not only per request — the same boot-time-store-access problem as config (constraint 6). Resolution is shared with §4a. On Cloudflare `env` and on Spin the host component are both available at the `run_app` entry, so a boot-time handle is constructible; the gap is that EdgeZero currently only exposes stores via the per-request registry.

**D4 — One typed `Settings` as the AppConfig root.** Replace `TrustedServerAppConfig` (wrapper with empty `SECRET_FIELDS`) by deriving `AppConfig` directly on `Settings`, with `#[secret]` on the real secret fields (Phase 3). Removes a transitional indirection.

**D5 — Reconcile ALL runtime store ids with `edgezero.toml`, kind by kind.** Not just the app-config blob, and **not just config/secrets**: EdgeZero's registry lookup is **strict** (unknown id → `None`), so every logical store id any config field or call site names at runtime must appear in `edgezero.toml` under the **correct kind** — `[stores.kv]`, `[stores.config]`, or `[stores.secrets]`. Reconciliation is kind-aware (`Settings::referenced_store_ids_by_kind()`): **KV** ids = `ec.ec_store` (`ec_identity_store`), `consent.consent_store`, `auction.creative_store` (deprecated); **config** ids = the app-config store `trusted_server_config` (key `app_config`), `jwks_store`, `datadome-ip-bypass`; **secret** ids = `signing_keys`, DataDome `ts_secrets`, the S3 secret store. (`counter_store`/`opid_store` are Fastly-adapter constants, not `Settings` ids.) Every declared id must map to a real platform store, opened **by logical id** (D7 — no `EDGEZERO__STORES__*__NAME`). The app-config-store rename and the request-signing store-id fix are detailed just below; the full inventory is the Phase 1 plan's Task 1.

**D6 — Runtime write path for request-signing key rotation.** EdgeZero `ConfigStore`/`SecretStore` are **read-only** at runtime; writes go through provisioning (author/ops time). But `KeyRotationManager` writes+deletes config (JWKS) and secrets (private keys) **at request time** via `/_ts/admin/keys/{rotate,deactivate,delete}`, backed by `management_api.rs`. Three resolutions:
- **(a) Keep a write-capable admin abstraction** — retain the trusted-server `put`/`create`/`delete` traits + `management_api.rs` for the admin write path only; EdgeZero read-only stores serve the read path. Least disruptive; leaves a non-EdgeZero write path (so "completely on EdgeZero" is not literally met for admin writes).
- **(b) Move key rotation out of runtime** — make rotate/deactivate/delete an **ops/CLI** operation (`ts keys …`) that writes via EdgeZero provisioning; the runtime only reads current keys. Most EdgeZero-native; changes the admin surface from an HTTP endpoint to an operator command — a **product/ops decision**.
- **(c) Add an EdgeZero runtime store-write/provision API** — upstream change; broadens the platform.
**Decision needed before Phase 1 deletions.** Until then `management_api.rs` stays. This is R10; recommend (a) as the interim (unblocks Phase 1 read migration) with (b) as the target end-state if ops agrees.

---

**D7 — Runtime app-config is config-store-only; no runtime environment variables (hard cutoff).** At runtime, every adapter builds `Settings` **solely** from the config-store blob: open store → verify envelope → deserialize (+ secret walk) → validate. There is **no runtime environment-variable overlay** for app config on any adapter. All env-var influence on config content happens at **push time**: `ts config push` (which runs on a host that has the env vars) reads them, applies the AppConfig env overlay, and bakes the resolved values into the signed blob. Consequences:
- Delete `Settings::from_toml_and_env` and the `TRUSTED_SERVER__*` overlay entirely (Phase 2) — not merely unused, **forbidden** at runtime.
- The AppConfig loader's `env_overlay` is a **push-time-only** option; runtime never applies it.
- **Store-name binding follows the same spirit:** a logical store id resolves to the **platform store of the same name by default** — via EdgeZero's existing `store_name` **fallback to the logical id when the env var is unset**. So we **provision no `EDGEZERO__STORES__*__NAME` values**, and every adapter opens stores by logical id. Note the mechanism, not the absolute: EdgeZero's Axum/Cloudflare/Spin builders still *call* `EnvConfig` internally — but with nothing set they fall back to the logical id, which is D7-compliant with **no EdgeZero change**. Only Fastly's *custom* dispatch path (which bypasses EdgeZero's builders) needs trusted-server-local by-id builders (Phase 1 Task 6). If a deployment ever needs a different physical name, that is a **provisioning/manifest** concern at deploy time, never a runtime env read.

---

## 4a. Prerequisites (must resolve before or during Phase 1/2)

These are not trusted-server refactors; they are EdgeZero-adapter capability gaps or up-front decisions that gate the phases.

**P0-C — EdgeZero adapter dispatch that preserves multi-value headers and skips logger reinit (Fastly).** For Fastly to reach a thin entry point, EdgeZero's Fastly adapter dispatch must: (1) preserve duplicate response headers (esp. `Set-Cookie`) instead of collapsing via `set_header`; (2) allow the app to opt out of the per-call logger reinit; and (3) provide a hook to inject request-scoped extensions (`client_info`, `device_signals`) derived from the raw `FastlyRequest` before conversion. **Two resolutions:**
- **(Recommended) Upstream to EdgeZero** as a header-preserving `run_app`/dispatch variant + a pre-dispatch extension hook. Add to the edgezero prerequisite set alongside Phase 0 (A/B). Then Fastly's `main.rs` collapses to that variant.
- **(Fallback) Permanent documented exception** — Fastly keeps a small adapter-level dispatch shim calling `app.router().oneshot()`. The end-state (§1) already allows this. This is *not* pre-EdgeZero cruft and would survive Phase 5.
Decision needed with the edgezero maintainer; feeds the same PR track as #305.

**P-BOOT — Boot-time store access for startup config + secret load.** Define, per adapter, how `build_state()` obtains a config-store (and secret-store) handle at boot, before request context. Options:
- **(a) Boot-time handle from the adapter environment** — Cloudflare builds a config-store handle from the `env` binding passed to `run_app`; Spin from the host component config; Fastly/Axum open the store eagerly (already do). Requires EdgeZero to expose a boot-time store constructor (or trusted-server constructs it from the adapter's env directly, mirroring today's `TRUSTED_SERVER_CONFIG` side-channel but reading the store instead).
- **(b) Lazy first-request load + cache** — defer the config load to the first request (where the registry exists), cache `Arc<Settings>` in a `OnceCell`. Keeps D1's load-once semantics but moves the load off the boot path. Trade-off: first request pays the cost and must handle a config-load error as a request error.
Recommendation: **(a)** where the adapter env is available at boot (Cloudflare/Spin both pass it to `run_app`), falling back to **(b)** only if an adapter genuinely cannot construct a boot-time handle. Settle in the Phase 2 plan; this is the load-bearing detail that makes "no baked TOML on Cloudflare/Spin" actually implementable.

---

## 5. Phases

### Phase 0 — EdgeZero prerequisites (external, edgezero repo)

**Owner:** edgezero. **Tracked by:** its own spec + PR [stackpop/edgezero#305](https://github.com/stackpop/edgezero/pull/305) — "add State<T> + nested #[secret] design spec".
**Delivers:** (A) `State<T>` extractor + `RouterBuilder::with_state`; (B) nested/array `#[secret]` in `#[derive(AppConfig)]` + path-aware `secret_walk`; **(C, if resolved upstream) P0-C** header-preserving Fastly dispatch + pre-dispatch extension hook (§4a).
**Blocks:** Phase 3 (B), Phase 4 (A), Phase 5/Fastly end-state (C). **This umbrella consumes it as a versioned dependency** — bump the pinned `edgezero` rev once merged.
**Note for #305:** the trusted-server secret audit (Phase 3 / §5) confirms **array secrets exist** (`ec.partners[].api_token`, `handlers[].password`) and **optional-string secrets exist** (`ts_pull_token`). So edgezero #305's `ArrayEach` and `Option<String>` support are **required**, not deferrable — this settles that PR's open question B-1.

---

### Phase 1 — Stores onto EdgeZero `StoreRegistry`

**Goal:** route all **read** store access through EdgeZero `ConfigStore` / `SecretStore` / `StoreRegistry` (KV is already there), and delete the bespoke read layer + duplicated chunk resolver. **Runtime writes and store-id reconciliation must be resolved first** (see D5, D6 below and the plan's task 1).

> **Plan ordering (per review):** the Phase 1 plan does NOT start with deletions. Task 1 is a **store-capability inventory** — enumerate every runtime store id and every read vs write call site — and a **decision on D6** (runtime writes). Deletions come only after the write path is settled.

**Changes:**
- **Reads (runtime + boot):** route `PlatformConfigStore`/`PlatformSecretStore` **reads** and the `RuntimeServices` config/secret fields through EdgeZero handles resolved from the per-request registries, matching KV — via a **composite store** (reads → EdgeZero, writes → existing management path; see D6-a). Also migrate the **boot-time** config load: Fastly `load_settings_from_config_store()` and Axum `build_state()` read `Settings` at boot through `&FastlyPlatformConfigStore` / `&AxumPlatformConfigStore` **before** request context exists, so those must move to a boot-time EdgeZero config read *before* the bespoke read impls are deleted. Migrate read consumers: `proxy.rs` (S3), `request_signing/{signing,rotation}.rs` (reads), `integrations/datadome/{protection,protection_scope}.rs`.
- **Writes (D6):** `KeyRotationManager` writes+deletes **config and secrets at request time** (`store_private_key`/`store_public_jwk`/`delete_key` for `/_ts/admin/keys/rotate` + deactivate/delete). EdgeZero `ConfigStore`/`SecretStore` are **read-only by design**. So `management_api.rs` **cannot be deleted in Phase 1 as originally written**. Resolve per D6 before touching it.
- **Store-id reconciliation (D5, expanded, kind-aware):** every runtime store id referenced by config must be declared in `edgezero.toml` under the **correct kind** — `[stores.kv]`, `[stores.config]`, or `[stores.secrets]` — or strict registry lookup returns `None`. Reconcile at least: **KV** — `ec.ec_store` (`ec_identity_store`), `consent.consent_store`, `auction.creative_store` (deprecated — creatives are inline — but still a `Settings` field, so declare it to keep strict lookup safe); **config** — the **app-config blob store** (see the store-id/key rule below), `request_signing.config_store_id` (`app_config` today), the JWKS/config-list store, and **DataDome's IP-CIDR config store** (`ProtectionIpCidrSourceConfig.config_store`, default `datadome-ip-bypass`, `protection_scope.rs`); **secrets** — `request_signing.secret_store_id` (`secrets` today), DataDome secret store (`ts_secrets`), the S3 secret store — plus all `trusted-server.example.toml` + integration/test fixtures. (`counter_store`/`opid_store` are **Fastly-adapter** constants (rate limiter / opid), **not** `Settings` logical ids, and are out of scope.)

  **App-config store-id/key rule (explicit):** the app-config blob lives in the config **store id `trusted_server_config`** (the `edgezero.toml` default) under **blob key `app_config`** (`CONFIG_BLOB_KEY`). Phase 1 changes only the **app-config** store id — `settings_data.rs::DEFAULT_CONFIG_STORE_ID` from `"app_config"` to `"trusted_server_config"` — keeping `app_config` as the blob **key**.

  **Do NOT repoint request-signing at the app-config store.** Request signing uses its **own** stores: reads use the hard-coded runtime names `JWKS_CONFIG_STORE_NAME = "jwks_store"` (config) and `SIGNING_SECRET_STORE_NAME = "signing_keys"` (secret); writes use `request_signing.config_store_id`/`secret_store_id`, which are the **management-API identifiers of those same jwks/signing stores** (Fastly separates runtime name from management id). Today `trusted-server.example.toml` sets `config_store_id = "app_config"` / `secret_store_id = "secrets"`, which points **writes at the wrong store** (writes JWKS state into app-config while reads look in `jwks_store`). Phase 1 fixes this: set `request_signing.config_store_id` to the **jwks store**'s id and `secret_store_id` to the **signing_keys** store's id, so reads and writes target the same store. Declare `jwks_store` (config) and `signing_keys` (secret) as their own logical ids in `edgezero.toml`.

  **Requirement (state explicitly):** because writes use `config_store_id`/`secret_store_id` as **Fastly management-API identifiers** while reads use the runtime **logical id**, Phase 1 requires — for the jwks/signing stores — that the **Fastly management resource id equals the runtime logical id** (`jwks_store`, `signing_keys`). Provision the Fastly stores with those exact names/ids. If a deployment cannot make them equal, it must supply an explicit management-id→logical-id mapping (out of Phase 1 scope); the plan assumes equality and the operator runbook must call it out.

  **`StoreName` semantics (D7):** `platform/types.rs::StoreName` is documented as an "edge-visible platform name". Under D7 runtime reads resolve through the registry by **logical id** (`registry.named(id)`), so for reads `StoreName` now carries the **logical store id** (== platform name by default). Phase 1 updates the `StoreName` doc and audits read call sites to pass logical ids, not physical platform names.
- **Fastly registry injection (ties to P0-C):** Fastly's custom `oneshot` path (§1) currently inserts only a `ConfigStoreHandle`, not registries via `dispatch_with_registries`. EdgeZero's `dispatch_with_registries` and its registry builders are **`pub(crate)`** (verified in the pinned checkout), so trusted-server must build the registries **locally** (from `StoresMetadata` + `EnvConfig` + the EdgeZero Fastly store open primitives) and insert them into extensions before `oneshot` — or an EdgeZero public builder must be added upstream (**R11**).
- Delete the 4× per-adapter `platform.rs` config/secret **read** impls; adapters build registries from `[stores.*]` metadata (via `dispatch_with_registries` on Axum/Cloudflare/Spin, via the Fastly-specific injection above).
- Delete `settings_data.rs`'s `FastlyChunkPointer` resolver — EdgeZero's `FastlyConfigStore` resolves chunks transparently. `get_settings_from_config_store` collapses to `ConfigStore::get` + `settings_from_config_blob`.

**Deletions (after D6/D5 resolved):** `settings_data.rs` chunk resolver, `platform/traits.rs` config/secret **read** traits, 4× `platform.rs` config/secret read impls. **`management_api.rs` deletion is conditional on D6** (may move to CLI/ops instead, or stay as a runtime write path).
**Keeps:** `RuntimeServices` as a shrinking bundle (removed in Phase 4); the runtime write path until D6 resolves it; `StoreName`/`StoreId` where writes/provisioning need the management-id split.
**Acceptance:** all adapters build; `cargo test-fastly/-axum/-cloudflare/-spin` + parity green; secret/config **reads** go through EdgeZero registries; **key rotation/delete still works** (per the D6 resolution); every declared store id resolves (no strict-lookup `None`).

---

### Phase 2 — Finish config injection (no embedded `trusted-server.toml`)

**Goal:** every adapter loads app config from the EdgeZero config store; kill compile-time config baking and the legacy env overlay.

**Changes:**
- Derive `AppConfig` on the config root (interim: still `TrustedServerAppConfig` until Phase 3 collapses it onto `Settings`) so all adapters use the same store-load path.
- **Cloudflare** (`adapter-cloudflare/src/app.rs`) and **Spin** (`adapter-spin/src/app.rs`): replace startup config sourcing (Cloudflare's `TRUSTED_SERVER_CONFIG` env side-channel + the native `include_str!` fallback; Spin's baked example TOML) with a **boot-time config-store read** per **P-BOOT (§4a)**. `build_state()` obtains a config-store handle from the adapter env passed to `run_app` (option a) or defers to a lazy first-request cached load (option b). This is the load-bearing detail — settle the mechanism in the Phase 2 plan. Seed each platform's config store (`wrangler.toml` / `runtime-config.toml` / `fastly.toml` local blocks) with the pushed blob under the D5 store id/key.
- Delete `Settings::from_toml_and_env`, `ENVIRONMENT_VARIABLE_PREFIX/SEPARATOR`, and the `config` **dev-dependency**. Per **D7 (hard cutoff)** there is **no runtime env overlay** — runtime reads the blob only. Env-var influence on config happens exclusively at **push time** via `ts config push` (AppConfig `env_overlay` applied there, then baked into the blob).

**Deletions:** both `include_str!` config paths, `from_toml_and_env`, `config` crate dep.
**Acceptance:** Cloudflare + Spin serve with store-loaded config (no baked TOML); `ts config push` blob is the single source on all four adapters; tests green.

---

### Phase 3 — Secret externalization (full)

**Goal:** no app-level secret is stored inside the config blob; secrets live in the EdgeZero secret store and resolve at startup (D3).

**Depends on:** Phase 0 (B) nested `#[secret]`, Phase 2.

**Changes:**
- Collapse `TrustedServerAppConfig` onto `Settings` (D4): `#[derive(AppConfig)]` on `Settings`, `#[secret]` / `#[secret(store_ref)]` on the real secret fields (S3 keys, request-signing key refs, DataDome server-side key, integration API keys, etc.), including the **nested** ones enabled by Phase 0.
- Delete `Redacted<T>` and its manual redaction handling; `#[secret]` + the secret store replace it.
- Operator migration: `ts` provisions secrets into the secret store (via EdgeZero provision), and a migration guide moves existing inline secrets out of `trusted-server.toml`. `reject_placeholder_secrets` becomes a check on the resolved values at boot.
- Startup load resolves `#[secret]` fields against the secret store (D1/D3), then validates.

**Secret inventory (spec artifact — verify + extend during the Phase 3 plan).** Preliminary audit of `Settings`; shapes drive the edgezero #305 requirements:

| Secret | Path | Shape | Notes |
|---|---|---|---|
| Partner API tokens | `ec.partners[].api_token` | **array element** | needs `ArrayEach` (edgezero #305) |
| Handler passwords | `handlers[].password` | **array element** | needs `ArrayEach` |
| EC passphrase | `ec.passphrase` | scalar `String` | nested |
| Pull token | `ts_pull_token` | **`Option<String>`** | needs optional-secret support (edgezero #305) |
| Publisher proxy secret | `publisher.proxy_secret` | scalar `String` | nested |
| DataDome server-side key | `integrations.datadome.*` (store-ref name+key) | store-ref | already resolves via secret-store name+key |
| S3 / proxy secret access key | `proxy.secret_access_key` (+ `proxy.secret_store`) | store-ref | already store-backed |
| Request-signing keys | `request_signing.*` (`secret_store_id`) | store-ref | already store-backed |

Two consequences: (1) edgezero #305 **must** ship `ArrayEach` + `Option<String>` (see Phase 0 note); (2) the already-store-backed secrets (DataDome, S3, request-signing) need only re-expression as `#[secret(store_ref)]`, not relocation.

**Deletions:** inline secrets in the blob, `Redacted<T>`, `SECRET_FIELDS = &[]` wrapper.
**Acceptance:** pushed blob contains only secret **key names**; boot resolves them; a config with nested **and array** secrets validates and serves; operator migration guide published; tests green.

---

### Phase 4 — Handlers → extractors

**Goal:** core route handlers become `#[action]` functions taking `FromRequest` extractors; per-adapter handler shims deleted.

**Depends on:** Phase 0 (A) `State<T>`; cleaner after Phase 1.

**Changes:**
- Introduce `State<Arc<AppState>>` (or narrower `State<Arc<Settings>>` / `State<Arc<AuctionOrchestrator>>` / `State<Arc<IntegrationRegistry>>`) wired via `RouterBuilder::with_state` in each adapter's `Hooks::routes()`. *Granularity (one `Arc<AppState>` vs per-component states) is a Phase 4 plan decision.*
- Rewrite core `handle_*` (`proxy.rs`, `publisher.rs`, `auction/endpoints.rs`, `request_signing/endpoints.rs`, `ec/*.rs`) from `(&Settings, &RuntimeServices, Request<EdgeBody>)` to `#[action]` signatures using `State<…>`, `Json`/`Query`/`Path`/`Headers`/`Host`, and the store extractors (`Kv`, `Secrets`, `Config`).
- Delete the per-adapter shims (`execute_handler`/`execute_named`/`named_route_handler` + `NamedRouteHandler` enums) and shrink/retire `RuntimeServices` (its store fields already gone in Phase 1; remaining bundle folds into `State` + extractors).
- **EC lifecycle & pre-route filters** (`build_ec_request_state`, `run_pre_route_filters`, `attach_dispatch_extensions`, `FinalizeResponseMiddleware`) are cross-cutting — keep them as **middleware**, not per-arg extractors.
- **Phase 4b (optional follow-up, D2):** migrate the integration proxy nested router / `IntegrationProxy::handle` onto `RouterService` + extractors. Deferred by default.

**Deletions:** per-adapter handler shims, `NamedRouteHandler` enums, `RuntimeServices` (final form).
**Acceptance:** every named route **that a given adapter supports** is served via an `#[action]` handler on that adapter (route sets are *not* uniform — Fastly exposes EC identity routes `/_ts/api/v1/{identify,batch-sync}`; Spin and Axum deliberately omit them to match non-Fastly adapters); middleware carries EC lifecycle, and **Fastly-only EC after-send / finalize ordering** is preserved; parity test green.

---

### Phase 5 — Delete the legacy Fastly path (gated on 100% rollout)

**Goal:** remove the pre-EdgeZero Fastly entry path once the EdgeZero rollout is complete.

**Gate:** edgezero rollout at 100% (issue #495). Do not start until confirmed.

**Changes:**
- Delete `legacy_main` / `route_request` (`adapter-fastly/src/main.rs`), `compat.rs` (fastly↔http shim), and the flag machinery (`edgezero_enabled`, `edgezero_rollout_pct`, `select_edgezero_entrypoint`, `should_route_to_edgezero`, IP-bucket hashing).
- `main()` calls the EdgeZero path unconditionally — the P0-C dispatch variant, or the documented Fastly dispatch shim (§4a), depending on how P0-C resolves.
- Retire the `trusted_server_config` rollout-flag reads (the flags, not the config store — after D5 the store may still hold app config).
- **Ancillary cleanup (easy to miss):** Fastly route tests importing legacy stores + `route_request` (`adapter-fastly/src/route_tests.rs`); generated Viceroy config rollout flags (`integration-tests/src/bin/generate-viceroy-config.rs`); `fastly.toml` local `edgezero_enabled`/`edgezero_rollout_pct` config; and the rollout runbook `docs/internal/EDGEZERO_MIGRATION.md`.

**Deletions:** `legacy_main`, `route_request`, `compat.rs`, rollout flags, `route_tests.rs` legacy imports, viceroy-config flags, `fastly.toml` flag config, `EDGEZERO_MIGRATION.md` runbook.
**Acceptance:** Fastly adapter has a single EdgeZero entry path; no rollout flags anywhere (adapter, tests, generated config, `fastly.toml`, docs); full CI gate green; production traffic unaffected (already 100% on EdgeZero by gate definition).

---

## 6. Cruft deletion ledger (rolled into phases)

| Item | File(s) | Phase | Replaced by |
|---|---|---|---|
| Fastly chunk-pointer resolver | `core/src/settings_data.rs` | 1 | EdgeZero `FastlyConfigStore` + `chunked_config.rs` |
| Bespoke config/secret store traits | `core/src/platform/traits.rs` (config+secret trait defs); `mod.rs`/`types.rs` edited, not deleted (KV re-export + shrinking `RuntimeServices` stay) | 1 | EdgeZero `ConfigStore`/`SecretStore`/`StoreRegistry` |
| 4× per-adapter store impls | `adapter-*/src/platform.rs` | 1 | per-adapter EdgeZero store impls |
| Fastly management REST client (**runtime writes**) | `adapter-fastly/src/management_api.rs` | **conditional (D6)** — 1 only if key rotation moves to ops/CLI; otherwise retained as the admin write path | EdgeZero provisioning (if writes leave runtime) — else no replacement |
| Adapter/runtime app-config baking | `adapter-{cloudflare,spin}/src/app.rs` (`include_str!` + Cloudflare `TRUSTED_SERVER_CONFIG` side-channel) | 2 | boot-time store-loaded config (P-BOOT). *`ts config init` template embed is out of scope.* |
| Legacy env overlay + `config` dep | `core/src/settings.rs` (`from_toml_and_env`, `ENVIRONMENT_VARIABLE_*`) | 2 | `EDGEZERO__*` / AppConfig env layers |
| AppConfig wrapper w/ empty `SECRET_FIELDS` | `core/src/config.rs` | 3 | `#[derive(AppConfig)]` on `Settings` |
| `Redacted<T>` | `core/src/redacted.rs` | 3 | `#[secret]` + secret store |
| Per-adapter handler shims | `adapter-*/src/app.rs` | 4 | `#[action]` + extractors |
| Legacy Fastly path + flags + compat | `adapter-fastly/src/{main.rs,compat.rs}` | 5 | single EdgeZero entry path |
| Rollout-flag ancillaries | `adapter-fastly/src/route_tests.rs` (legacy imports), `integration-tests/src/bin/generate-viceroy-config.rs` (flags), `fastly.toml` (local flag config), `docs/internal/EDGEZERO_MIGRATION.md` (runbook) | 5 | — (deleted with the rollout mechanism) |

**Explicitly NOT cruft (do not remove):** `migration_guards.rs` (intentional `fastly::` ban test), `s3_sigv4.rs` (AWS-domain canonical/hashing), `platform/image_optimizer.rs` (no EdgeZero equivalent yet), EC KV CAS wrapper (`ec/kv*.rs` — needs EdgeZero generation-CAS parity first; revisit, don't delete).

---

## 7. Risks & open questions

| ID | Question | Owner / resolution |
|----|----------|--------------------|
| R1 | Do any `Settings` secrets live inside **arrays**? | **Resolved: yes** (`ec.partners[].api_token`, `handlers[].password`) + optional (`ts_pull_token`). edgezero #305 must ship `ArrayEach` + `Option<String>` (see §5 Phase 3 inventory + Phase 0 note). |
| R7 | P0-C: upstream a header-preserving Fastly dispatch, or keep a permanent Fastly dispatch shim? | Decide with edgezero maintainer (§4a); gates the Fastly end-state and Phase 5. |
| R8 | P-BOOT: boot-time store handle (a) vs lazy cached first-request load (b), per adapter? | Phase 2 plan (§4a). |
| R9 | D5: reconcile **all** runtime store ids **by kind** with `edgezero.toml` (strict lookup fails otherwise) — KV: `ec_identity_store`, `consent_store`, `creative_store`; config: `trusted_server_config` (app-config blob, key `app_config`), `jwks_store`, `datadome-ip-bypass`; secrets: `signing_keys`, `ts_secrets`, S3. Request signing uses `jwks_store`/`signing_keys` (not the app-config store). | Phase 1 plan task 1/2. |
| R12 | Fastly `EnvConfig` reader is private / `fastly::ConfigStore` has no `iter()`. | **Resolved by D7** — runtime opens stores by logical id; no store-name env/dictionary read; no local `EnvConfig` reader needed. |
| R10 | D6: runtime write path for key rotation — keep write-capable admin abstraction (a), move to ops/CLI (b), or upstream an EdgeZero write API (c)? | **Blocks Phase 1 deletions.** Phase 1 plan locks to **(a)**; (b)/(c) → separate plan. |
| R11 | Should EdgeZero expose a **public** Fastly registry-builder helper? | Lower priority under D7 — local builders open by logical id and need only public store constructors. Decide with edgezero maintainer if convenient. |
| R2 | `StoreName` vs `StoreId` split — still needed after `management_api.rs` deletion? | Phase 1; drop if only the CLI provision path used it. |
| R3 | EC identity API + Fastly rate limiter are Fastly-only today | Out of scope here; note as a portability follow-up (not blocking). |
| R4 | Cloudflare/Spin boot-time secret-store access for D3 | Confirm in Phase 3 scoping. |
| R5 | Config-change-requires-redeploy (D1) acceptable to operators? | Already true today; confirm no regression expectation. |
| R6 | Phase 4 handler rewrite is large — split by route group? | Yes; per-implementation-plan, group by file (`proxy`, `auction`, `ec`, `request_signing`, `publisher`). |

---

## 8. Next step

Per phase, run `writing-plans` to produce an implementation plan **at phase start** (not upfront for all five) — the plan for Phase N should reflect the state left by Phase N-1. Begin with **Phase 1** once this umbrella is approved and the Phase 0 edgezero PR is merged (or Phase 1 can start immediately since it has no upstream dependency).
