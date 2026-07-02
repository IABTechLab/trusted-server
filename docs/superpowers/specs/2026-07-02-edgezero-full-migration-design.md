# Trusted Server → EdgeZero — Full Migration (umbrella design)

- **Status:** Draft for review
- **Date:** 2026-07-02
- **Scope:** Move trusted-server **completely** onto EdgeZero primitives: config push, KV, secret store, config injection without an embedded `trusted-server.toml`, extractor-based handlers, and deletion of every pre-EdgeZero workaround.
- **Shape:** Umbrella roadmap. Defines the end-state, the current-state gap, and an ordered set of phases with dependencies. **Each phase gets its own implementation plan** (`writing-plans`) before code is written.
- **Companion spec:** Phase 0 (`State<T>` extractor + nested `#[secret]`) is an **edgezero-repo** change, specified separately (`…-state-and-nested-secrets-design.md`) and tracked via edgezero PR [stackpop/edgezero#305](https://github.com/stackpop/edgezero/pull/305). This umbrella depends on it but does not re-specify it.

---

## 1. End-state

trusted-server is a fully EdgeZero-native app: adapter binaries call `run_app::<App>`; core is platform-neutral; config, KV, and secrets flow exclusively through EdgeZero's `StoreRegistry`; app config is a signed blob published by `ts config push` and read back typed at request time with secrets resolved from the secret store; handlers are `#[action]` functions taking `FromRequest` extractors; and no Fastly-specific or pre-EdgeZero shim remains in core or the adapters.

Concretely, at the end of this migration:

- **No `include_str!` of any `*.toml` config** in any adapter. All four adapters load app config from the EdgeZero config store.
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
| **Config load** | ⚠️ Fastly + Axum load the blob from the config store; **Cloudflare + Spin `include_str!` `trusted-server.example.toml`** | Phase 2 |
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

**D3 — Secret resolution happens at startup, not per request.** With D1, the startup config load resolves `#[secret]` fields against the secret store once (Phase 3). Adapters must therefore have a secret-store handle available at boot, not only per request. Fastly/Axum already open stores eagerly; Cloudflare/Spin resolve from bindings — confirm boot-time access in Phase 3 scoping.

**D4 — One typed `Settings` as the AppConfig root.** Replace `TrustedServerAppConfig` (wrapper with empty `SECRET_FIELDS`) by deriving `AppConfig` directly on `Settings`, with `#[secret]` on the real secret fields (Phase 3). Removes a transitional indirection.

---

## 5. Phases

### Phase 0 — EdgeZero prerequisites (external, edgezero repo)

**Owner:** edgezero. **Tracked by:** its own spec + PR [stackpop/edgezero#305](https://github.com/stackpop/edgezero/pull/305) — "add State<T> + nested #[secret] design spec".
**Delivers:** (A) `State<T>` extractor + `RouterBuilder::with_state`; (B) nested/array `#[secret]` in `#[derive(AppConfig)]` + path-aware `secret_walk`.
**Blocks:** Phase 3 (B), Phase 4 (A). **This umbrella consumes it as a versioned dependency** — bump the pinned `edgezero` rev once merged.

---

### Phase 1 — Stores onto EdgeZero `StoreRegistry`

**Goal:** delete trusted-server's bespoke config/secret store layer; route all store access through EdgeZero `ConfigStore` / `SecretStore` / `StoreRegistry` (KV is already there).

**Changes:**
- Replace `PlatformConfigStore` / `PlatformSecretStore` (`platform/traits.rs`, `types.rs`) and the `RuntimeServices` config/secret fields with EdgeZero `ConfigStoreHandle` / `BoundSecretStore` resolved from the per-request registries (`ConfigRegistry` / `SecretRegistry`), matching how KV already works.
- Migrate core secret consumers to `secrets.named(id)?.require_str(key)` / config consumers to the config binding: `proxy.rs` (S3), `request_signing/{signing,rotation}.rs`, `integrations/datadome/{protection,protection_scope}.rs`.
- Delete the 4× per-adapter `platform.rs` config/secret store impls (`FastlyPlatformConfigStore`, `AxumPlatformConfigStore`, `NoopConfigStore`, `Cloudflare…`, and secret equivalents); adapters instead build `ConfigRegistry`/`SecretRegistry` via `dispatch_with_registries` from `[stores.*]` metadata.
- Delete `FastlyManagementApiClient` (`management_api.rs`) — store writes/provisioning move to the EdgeZero CLI provision path.
- Delete `settings_data.rs`'s `FastlyChunkPointer` resolver — EdgeZero's `FastlyConfigStore` resolves chunks transparently. `get_settings_from_config_store` collapses to `ConfigStore::get` + `settings_from_config_blob`.

**Deletions:** `management_api.rs`, `settings_data.rs` chunk resolver, `platform/traits.rs` config/secret traits, 4× `platform.rs` config/secret impls.
**Keeps:** `RuntimeServices` as a shrinking bundle for the still-explicit-arg handlers (removed in Phase 4); `StoreName`/`StoreId` only where the CLI provisioning still needs the management-id split (revisit).
**Acceptance:** all adapters build; `cargo test-fastly/-axum/-cloudflare/-spin` green; secret/config reads exercised in tests go through EdgeZero registries; parity test passes.

---

### Phase 2 — Finish config injection (no embedded `trusted-server.toml`)

**Goal:** every adapter loads app config from the EdgeZero config store; kill compile-time config baking and the legacy env overlay.

**Changes:**
- Derive `AppConfig` on the config root (interim: still `TrustedServerAppConfig` until Phase 3 collapses it onto `Settings`) so all adapters use the same store-load path.
- **Cloudflare** (`adapter-cloudflare/src/app.rs`) and **Spin** (`adapter-spin/src/app.rs`): replace `Settings::from_toml(include_str!(".../trusted-server.example.toml"))` with `get_settings_from_config_store(...)` (now the EdgeZero `ConfigStore` path from Phase 1). Seed each platform's config store (`wrangler.toml` / `runtime-config.toml` / `fastly.toml` local blocks) with the pushed blob.
- Delete `Settings::from_toml_and_env`, `ENVIRONMENT_VARIABLE_PREFIX/SEPARATOR`, and the `config` **dev-dependency**. Any remaining env overlay uses EdgeZero's `EDGEZERO__*` / AppConfig `<APP>__…` layers.

**Deletions:** both `include_str!` config paths, `from_toml_and_env`, `config` crate dep.
**Acceptance:** Cloudflare + Spin serve with store-loaded config (no baked TOML); `ts config push` blob is the single source on all four adapters; tests green.

---

### Phase 3 — Secret externalization (full)

**Goal:** no app-level secret is stored inside the config blob; secrets live in the EdgeZero secret store and resolve at startup (D3).

**Depends on:** Phase 0 (B) nested `#[secret]`, Phase 2.

**Changes:**
- Collapse `TrustedServerAppConfig` onto `Settings` (D4): `#[derive(AppConfig)]` on `Settings`, `#[secret]` / `#[secret(store_ref)]` on the real secret fields (S3 keys, request-signing key refs, DataDome server-side key, integration API keys, etc.), including the **nested** ones enabled by Phase 0.
- Audit `Settings` for the secret inventory **before implementation** — this settles Phase 0's open question B-1 (are any secrets inside arrays?). Feed the answer back to the edgezero PR.
- Delete `Redacted<T>` and its manual redaction handling; `#[secret]` + the secret store replace it.
- Operator migration: `ts` provisions secrets into the secret store (via EdgeZero provision), and a migration guide moves existing inline secrets out of `trusted-server.toml`. `reject_placeholder_secrets` becomes a check on the resolved values at boot.
- Startup load resolves `#[secret]` fields against the secret store (D1/D3), then validates.

**Deletions:** inline secrets in the blob, `Redacted<T>`, `SECRET_FIELDS = &[]` wrapper.
**Acceptance:** pushed blob contains only secret **key names**; boot resolves them; a config with a nested secret validates and serves; operator migration guide published; tests green.

---

### Phase 4 — Handlers → extractors

**Goal:** core route handlers become `#[action]` functions taking `FromRequest` extractors; per-adapter handler shims deleted.

**Depends on:** Phase 0 (A) `State<T>`; cleaner after Phase 1.

**Changes:**
- Introduce `State<Arc<AppState>>` (or narrower `State<Arc<Settings>>` / `State<Arc<AuctionOrchestrator>>` / `State<Arc<IntegrationRegistry>>`) wired via `RouterBuilder::with_state` in each adapter's `Hooks::routes()`.
- Rewrite core `handle_*` (`proxy.rs`, `publisher.rs`, `auction/endpoints.rs`, `request_signing/endpoints.rs`, `ec/*.rs`) from `(&Settings, &RuntimeServices, Request<EdgeBody>)` to `#[action]` signatures using `State<…>`, `Json`/`Query`/`Path`/`Headers`/`Host`, and the store extractors (`Kv`, `Secrets`, `Config`).
- Delete the per-adapter shims (`execute_handler`/`execute_named`/`named_route_handler` + `NamedRouteHandler` enums) and shrink/retire `RuntimeServices` (its store fields already gone in Phase 1; remaining bundle folds into `State` + extractors).
- **EC lifecycle & pre-route filters** (`build_ec_request_state`, `run_pre_route_filters`, `attach_dispatch_extensions`, `FinalizeResponseMiddleware`) are cross-cutting — keep them as **middleware**, not per-arg extractors.
- **Phase 4b (optional follow-up, D2):** migrate the integration proxy nested router / `IntegrationProxy::handle` onto `RouterService` + extractors. Deferred by default.

**Deletions:** per-adapter handler shims, `NamedRouteHandler` enums, `RuntimeServices` (final form).
**Acceptance:** all named routes served via `#[action]` handlers on all adapters; middleware carries EC lifecycle; parity test green.

---

### Phase 5 — Delete the legacy Fastly path (gated on 100% rollout)

**Goal:** remove the pre-EdgeZero Fastly entry path once the EdgeZero rollout is complete.

**Gate:** edgezero rollout at 100% (issue #495). Do not start until confirmed.

**Changes:**
- Delete `legacy_main` / `route_request` (`adapter-fastly/src/main.rs`), `compat.rs` (fastly↔http shim), and the flag machinery (`edgezero_enabled`, `edgezero_rollout_pct`, `select_edgezero_entrypoint`, `should_route_to_edgezero`, IP-bucket hashing).
- `main()` calls the EdgeZero path unconditionally (`run_app::<TrustedServerApp>` shape).
- Retire the `trusted_server_config` rollout-flag reads.

**Deletions:** `legacy_main`, `route_request`, `compat.rs`, rollout flags.
**Acceptance:** Fastly adapter has a single EdgeZero entry path; no rollout flags; full CI gate green; production traffic unaffected (already 100% on EdgeZero by gate definition).

---

## 6. Cruft deletion ledger (rolled into phases)

| Item | File(s) | Phase | Replaced by |
|---|---|---|---|
| Fastly chunk-pointer resolver | `core/src/settings_data.rs` | 1 | EdgeZero `FastlyConfigStore` + `chunked_config.rs` |
| Bespoke config/secret store traits | `core/src/platform/traits.rs` (config+secret trait defs); `mod.rs`/`types.rs` edited, not deleted (KV re-export + shrinking `RuntimeServices` stay) | 1 | EdgeZero `ConfigStore`/`SecretStore`/`StoreRegistry` |
| 4× per-adapter store impls | `adapter-*/src/platform.rs` | 1 | per-adapter EdgeZero store impls |
| Fastly management REST client | `adapter-fastly/src/management_api.rs` | 1 | EdgeZero CLI provision |
| `include_str!` config baking | `adapter-{cloudflare,spin}/src/app.rs` | 2 | store-loaded config |
| Legacy env overlay + `config` dep | `core/src/settings.rs` (`from_toml_and_env`, `ENVIRONMENT_VARIABLE_*`) | 2 | `EDGEZERO__*` / AppConfig env layers |
| AppConfig wrapper w/ empty `SECRET_FIELDS` | `core/src/config.rs` | 3 | `#[derive(AppConfig)]` on `Settings` |
| `Redacted<T>` | `core/src/redacted.rs` | 3 | `#[secret]` + secret store |
| Per-adapter handler shims | `adapter-*/src/app.rs` | 4 | `#[action]` + extractors |
| Legacy Fastly path + flags + compat | `adapter-fastly/src/{main.rs,compat.rs}` | 5 | single EdgeZero entry path |

**Explicitly NOT cruft (do not remove):** `migration_guards.rs` (intentional `fastly::` ban test), `s3_sigv4.rs` (AWS-domain canonical/hashing), `platform/image_optimizer.rs` (no EdgeZero equivalent yet), EC KV CAS wrapper (`ec/kv*.rs` — needs EdgeZero generation-CAS parity first; revisit, don't delete).

---

## 7. Risks & open questions

| ID | Question | Owner / resolution |
|----|----------|--------------------|
| R1 | Do any `Settings` secrets live inside **arrays**? | Phase 3 audit; feeds edgezero Phase 0 B-1. |
| R2 | `StoreName` vs `StoreId` split — still needed after `management_api.rs` deletion? | Phase 1; drop if only the CLI provision path used it. |
| R3 | EC identity API + Fastly rate limiter are Fastly-only today | Out of scope here; note as a portability follow-up (not blocking). |
| R4 | Cloudflare/Spin boot-time secret-store access for D3 | Confirm in Phase 3 scoping. |
| R5 | Config-change-requires-redeploy (D1) acceptable to operators? | Already true today; confirm no regression expectation. |
| R6 | Phase 4 handler rewrite is large — split by route group? | Yes; per-implementation-plan, group by file (`proxy`, `auction`, `ec`, `request_signing`, `publisher`). |

---

## 8. Next step

Per phase, run `writing-plans` to produce an implementation plan **at phase start** (not upfront for all five) — the plan for Phase N should reflect the state left by Phase N-1. Begin with **Phase 1** once this umbrella is approved and the Phase 0 edgezero PR is merged (or Phase 1 can start immediately since it has no upstream dependency).
