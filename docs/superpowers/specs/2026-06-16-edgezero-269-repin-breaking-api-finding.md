# Finding: edgezero #269 repin — breaking-API surface for trusted-server

- **Date:** 2026-06-16 (build-verified 2026-06-17, §10)
- **Author:** Prakash (HTTP port); Christian owns the CLI port (see §7)
- **Upstream:** [stackpop/edgezero#269](https://github.com/stackpop/edgezero/pull/269)
  "EdgeZero CLI Extensions" — head `feature/extensible-cli`, base `main`,
  **OPEN / unmerged** as of 2026-06-16. Sibling adaptation precedent:
  [stackpop/mocktioneer#110](https://github.com/stackpop/mocktioneer/pull/110)
  (design + plan only, docs-only).
- **Companion doc:** extends
  [2026-03-19-edgezero-migration-design.md](./2026-03-19-edgezero-migration-design.md)
  (the original Fastly→EdgeZero migration, PRs 1–17; PR13 merged, branch now on
  PR19 canary cutover).
- **Method:** diffed the exact pin `170b74b` (current) against
  `feature/extensible-cli` HEAD (`git diff 170b74b..HEAD`, 257 commits,
  +28,969 / −5,754 across 143 files), then cross-referenced every changed
  public symbol against trusted-server's actual consumption sites. All
  signatures below are quoted verbatim from the two refs; call sites are
  `file:line` in this repo.

---

## 0. TL;DR

1. **trusted-server currently pins edgezero at `170b74b`** (March 2026, "unified
   key-value store abstraction #165"). `feature/extensible-cli` is **257 commits
   ahead** of that exact commit — and `170b74b` is a clean ancestor, so a repin
   to post-#269 swallows the **entire** delta, not just #269's own commits.

2. **Almost none of #269's headline breaks reach trusted-server.** The original
   migration deliberately wrapped edgezero behind trusted-server's own
   `platform/` trait layer (`RuntimeServices`, `PlatformConfigStore`,
   `PlatformSecretStore`, `PlatformKvStore`, `PlatformHttpClient`). As a result
   trusted-server uses **none** of: `run_app`, the `app!` macro,
   `RequestContext`, edgezero extractors, typed `AppConfig`, or `[stores.*]`
   manifest tables. Every one of those is where #269's breakage lives.

3. **The actual code-level break is a single method:**
   `edgezero_core::body::Body::into_bytes()` now returns `Option<Bytes>` instead
   of panicking. (`as_bytes()` changed the same way but has **no** trusted-server
   call site — §2.) **Compiler-enumerated** (not rg-guessed): **18 sink bindings
   — 8 production + 10 test-only** (§2/§10). Mechanical fix.

4. **`KvError` going `#[non_exhaustive]` + two new variants does _not_ break us**
   — trusted-server only _constructs_ `KvError::Unavailable` and never
   exhaustively matches the enum.

5. **Strategic question for the HTTP port:** #269 matures edgezero's _own_
   first-class multi-store registry, async `ConfigStore`/`SecretStore`,
   `Config`/`Secrets`/`Kv` extractors, and typed `AppConfig`. These now overlap
   heavily with trusted-server's bespoke `platform/` layer. The HTTP port can be
   a **minimal repin** (keep the bespoke layer) or a **convergence** onto
   edgezero's surface. See §6.

6. **The stack already walks edgezero forward** — pins are _not_ frozen at
   `170b74b`: PR1–13 = `170b74b`, PR14–18 = `38198f9`, PR19–20 = `ce6bcf7`. The
   #269 repin is the next step of a bump the team already does. PR14 is where
   trusted-server _starts_ consuming edgezero's high-level surface
   (`RequestContext`/`EdgeError`/middleware/router) — yet a real build (§10)
   proves even that base breaks on **nothing but `Body`**. See §11.

7. **Plan (agreed):** do the upgrade on a **dedicated branch off PR14** (not on
   main, not in-place on any reviewed PR), then **merge up** the stack; **re-pin
   to edgezero `main` after #269 merges**. Full-adaptation roadmap (store
   convergence + typed config + entry-point) is a _separate, optional_ track —
   **not** forced by the repin (§11).

8. **Superseded for Fastly by `feature/ts-cli-next` (§12).** Christian's "CLI"
   branch already implements the end-to-end Fastly config-store migration — same
   #269 pin, the `Body` fixes (graceful `ok_or_else`, not `.expect()`), the store
   ids, the `config_payload` flatten/hash contract, **and runtime
   Settings-from-store load** (`get_settings_from_services`). So our minimal-repin
   (#771) is largely redundant for Fastly. **Revised: build on his branch**; our
   real HTTP-layer deliverable is the **runtime-config-store spec** his CLI doc
   references but never wrote. See §12.

---

## 1. What trusted-server actually consumes from edgezero

Verified by `rg` across `crates/`. The dependency is a **thin, low-level slice**
of `edgezero-core` plus one type from `edgezero-adapter-fastly`.

| edgezero symbol                                                                                            | trusted-server usage                                                                                                                                                              | Reaches #269 break?                                                              |
| ---------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| `edgezero_core::body::Body` (alias `EdgeBody`)                                                             | pervasive — bodies on every request/response, integrations, publisher, auction                                                                                                    | **YES** (`into_bytes` → `Option`; `as_bytes` changed too but has **no** TS sink) |
| `edgezero_core::http::{Request, Response, request_builder, response_builder, HeaderValue, …}`              | request/response construction in `platform.rs`, `proxy.rs`, `http_util.rs`, tests                                                                                                 | No (stable; alias names unchanged)                                               |
| `edgezero_core::key_value_store::{KvStore (as `PlatformKvStore`), KvError, KvHandle, KvPage, NoopKvStore}` | KV trait impls, EC identity graph, `UnavailableKvStore` stub                                                                                                                      | No (trait sigs unchanged; `KvError` change is inert for us — §3.2)               |
| `edgezero_adapter_fastly::key_value_store::FastlyKvStore`                                                  | `platform.rs:13` — only symbol used from the fastly adapter                                                                                                                       | No (`open()` sig unchanged)                                                      |
| `edgezero-adapter-axum`, `edgezero-adapter-cloudflare` (workspace deps)                                    | **declared in root `[workspace.dependencies]` only; no member crate references them — `cargo tree -i edgezero-adapter-axum` / `-cloudflare` return "did not match any packages"** | No (absent from the dependency graph — not compiled at all; see §9 Q4 → drop)    |
| `edgezero-adapter-spin`                                                                                    | **not a dependency** — `trusted-server-adapter-spin` is an in-repo stub                                                                                                           | No (edgezero's Spin SDK6/wasip2 churn never reaches us)                          |

**Not used at all** (and therefore immune to #269): `run_app`, `app!`,
`RequestContext`, `FromRequest`/extractors, `EdgeError`, `IntoResponse`,
`ProxyClient`/edgezero `proxy`, typed `AppConfig`, manifest `[stores.*]` /
`[adapters.*]` tables. trusted-server's manifest is `trusted-server.toml` (a
bespoke `Settings` struct in `settings.rs`), **not** an edgezero `edgezero.toml`.
(Baseline as of the current pin / pre-`ts-cli-next`. Christian's branch adds an
`edgezero.toml` and deletes `trusted-server.toml` — but still reads config through
the **bespoke `PlatformConfigStore`**, not edgezero's first-class store/extractor,
so this "uses none of …" list stays true even there. See §12.)

---

## 2. The one break that reaches trusted-server: `Body` → `Option`

`crates/edgezero-core/src/body.rs`. The `Body` enum shape is **unchanged** —
`Body::Once(Bytes)` / `Body::Stream(LocalBoxStream<…>)` — so trusted-server's
pattern matches in `platform.rs` survive. Two accessor return types changed:

| Item               | BASE (`170b74b`)                                                     | HEAD (`feature/extensible-cli`)                                              | Break       |
| ------------------ | -------------------------------------------------------------------- | ---------------------------------------------------------------------------- | ----------- |
| `Body::as_bytes`   | `pub fn as_bytes(&self) -> &[u8]` (panics on `Stream`) `body.rs:48`  | `pub fn as_bytes(&self) -> Option<&[u8]>` (`None` on `Stream`) `body.rs:24`  | return type |
| `Body::into_bytes` | `pub fn into_bytes(self) -> Bytes` (panics on `Stream`) `body.rs:55` | `pub fn into_bytes(self) -> Option<Bytes>` (`None` on `Stream`) `body.rs:62` | return type |

Everything else on `Body` is intact: `empty()`, `from_bytes()`, `from_stream()`,
`into_stream()`, `is_stream()`, `text()`, `json()`, `to_json()`, and `stream()`
(the earlier subagent claim that `stream()` was removed is **wrong** — it moved
in source order only). `From` impls unchanged.

**Behavior** (not just signatures) verified for the accessor neighbours: `Body::to_json`
is byte-identical BASE↔HEAD and matches on the `Once`/`Stream` enum directly —
it never calls `as_bytes`, so the `Option`-return change cannot leak into JSON
deserialization. `text()`/`json()`/`to_json()` are unused in trusted-server
regardless.

### Affected call sites — compiler-enumerated (authoritative)

> **Source of truth = the compiler, not `rg`.** An earlier hand-built `rg` list
> was wrong (missed production sinks `proxy.rs:38` and `auction/endpoints.rs:81`;
> mis-tagged tests as production). The list below is the exhaustive set from
> `cargo build --workspace --all-targets` on the repinned spike (§10): **27
> compiler errors collapsing to 18 distinct `into_bytes()` sink bindings.** All
> are `Body` (`EdgeBody`); **no `as_bytes` site exists** in trusted-server.

Line numbers are **PR14-base**; they shift per branch as the stack rewrites these
files — re-derive from the compiler on whatever branch you repin (the §8 gate does
this). One binding often produces several errors (`.len()`, `.to_vec()`,
`from_slice(&…)`, `from_utf8(&…)` on the now-`Option`).

**Production (8) — fail plain `cargo build` (lib + bin):**

| Binding site (PR14)                  | Shape                                                               |
| ------------------------------------ | ------------------------------------------------------------------- |
| `proxy.rs:38` (`body_as_reader`)     | `Cursor::new(body.into_bytes())`                                    |
| `publisher.rs:46` (`body_as_reader`) | `Cursor::new(body.into_bytes())`                                    |
| `auction/endpoints.rs:81`            | `let b = body.into_bytes(); b.len(); from_slice(&b)`                |
| `proxy.rs:1550`                      | `let b = req.into_body().into_bytes(); enforce(&b); from_utf8(&b)`  |
| `proxy.rs:1665`                      | same shape (rebuild path)                                           |
| `request_signing/endpoints.rs:103`   | `let b = req.into_body().into_bytes(); enforce(&b); from_slice(&b)` |
| `request_signing/endpoints.rs:246`   | same (rotate; also `b.is_empty()`)                                  |
| `request_signing/endpoints.rs:365`   | same (deactivate)                                                   |

**Test-only (10) — fail only `cargo test` / `--all-targets`, invisible to plain
`cargo build`:**

`auction/formats.rs:444`, `integrations/prebid.rs:2067`,
`integrations/testlight.rs:461`, `proxy.rs:2034`, `proxy.rs:2795`,
`proxy.rs:2851`, `publisher.rs:748`, `publisher.rs:1079`, `publisher.rs:1562`,
`request_signing/endpoints.rs:464`.

**NOT a sink:** `http_util.rs:456` appears in errors as the _expected_ side — it's
the `enforce_max_body_size(bytes: &[u8], …)` signature. No edit; `&Bytes` derefs
to `&[u8]` once the caller unwraps.

**False positives — leave untouched** (receiver is `str`/`String`/`FromUtf8Error`,
not `Body`; confirmed by source):
`http_util.rs:286,320` (`str::as_bytes`), `request_signing/endpoints.rs:23`
(`String::into_bytes`), `request_signing/endpoints.rs:452` (test, `str::as_bytes`),
`sourcepoint.rs:571` / `datadome.rs:323` (`rewrite_script_content() -> String`),
`sourcepoint.rs:822` (`FromUtf8Error::into_bytes`).

### Fix — style (updated per `feature/ts-cli-next`)

> **Revised guidance.** Christian's branch already fixed these sinks and chose
> **`into_bytes().ok_or_else(|| <error>)?`** (graceful error, no panic) for
> production request/response handlers, `unwrap_or_default()` for
> compression/test paths, and reserved `.expect()` for genuinely-unreachable
> spots. That is **better than a blanket `.expect()`** — a streaming/empty body
> must not panic the worker. **Adopt his approach:** propagate an error at
> production handler sinks; only `.expect("should …")` where a buffered body is
> truly invariant (and never `unwrap_or_default()` where an empty body would
> silently corrupt behavior). Align with his exact per-sink choices when we
> converge (§12).

For a production handler sink, prefer (error variant illustrative — match the
existing one at each call site, not necessarily `BadRequest`):

```rust
let bytes = req.into_body().into_bytes().ok_or_else(|| {
    Report::new(TrustedServerError::BadRequest { message: "request body should be buffered".into() })
})?;
```

The three mechanical shapes below still apply (substitute `ok_or_else(…)?` for
`.expect(…)` at production sinks):

```rust
// Shape A — value consumed directly (e.g. proxy.rs:38, publisher.rs:46)
let body = resp.into_body().into_bytes()
    .expect("should have a buffered body");

// Shape B — chained .to_vec() (e.g. prebid.rs:2067, proxy.rs:2034)
String::from_utf8(
    resp.into_body().into_bytes()
        .expect("should have a buffered body")
        .to_vec(),
)

// Shape C — bound, then borrowed into &[u8] / &Bytes (e.g. proxy.rs:1550,
// auction/endpoints.rs:81, request_signing/endpoints.rs:*)
let b = req.into_body().into_bytes()
    .expect("should have a buffered request body");
enforce_max_body_size(&b, …)?;        // &Bytes → &[u8]
serde_json::from_slice(&b)?;          // borrow the unwrapped Bytes
```

No signature changes propagate to callers (the locals are consumed in place).

---

## 3. Low-level surface that changed upstream but is INERT for trusted-server

These changed across the 257-commit delta but do **not** break our build,
verified against actual usage. Documented so the repin reviewer doesn't chase
ghosts.

### 3.1 `KvStore` trait — reordered, not re-signed

`key_value_store.rs`. All methods keep identical signatures
(`get_bytes`/`put_bytes`/`put_bytes_with_ttl`/`delete`/`list_keys_page`/`exists`,
all `async`, same params/returns). Only source ordering changed (clippy
`arbitrary_source_item_ordering`). trusted-server's `UnavailableKvStore` /
`NoopKvStore` impls and `KvIdentityGraph` calls are unaffected.

### 3.2 `KvError` — `#[non_exhaustive]` + new variants (inert here)

|          | BASE                                 | HEAD                                                                        |
| -------- | ------------------------------------ | --------------------------------------------------------------------------- |
| attr     | (none)                               | `#[non_exhaustive]` `key_value_store.rs:302`                                |
| variants | `NotFound { key }`, `Unavailable`, … | adds `LimitExceeded { message }` `:311`, `Unsupported { operation }` `:328` |

Inert for us: trusted-server only **constructs** `KvError::Unavailable` (a unit
variant — still constructible downstream under enum-level `#[non_exhaustive]`) in
`platform/kv.rs`, and never writes an exhaustive `match` on `KvError`. No catch-arm
needed.

### 3.3 `KvPage`, `KvHandle`, `NoopKvStore`, `FastlyKvStore::open`, http builders

All present and signature-stable:

- `KvPage` — same fields (`keys`, `cursor`/etc.), alphabetized only.
- `KvHandle` `key_value_store.rs:354`, `NoopKvStore` `:818` — exist.
- `edgezero_core::http::request_builder()` / `response_builder()` — same
  signatures; `RequestBuilder`/`ResponseBuilder` are still exported alias names
  (now aliasing `HttpRequestBuilder`/`HttpResponseBuilder` internally, transparent
  to us); `Request`/`Response`/`Method`/`StatusCode`/`HeaderValue`/… aliases
  unchanged.
- `edgezero_adapter_fastly::key_value_store::FastlyKvStore::open(name: &str) ->
Result<Self, KvError>` — unchanged.

---

## 4. Full #269 breaking-API catalog (does NOT reach trusted-server today)

Recorded for completeness — these are the framework-level breaks that bite
_consumers who use edgezero's high-level surface_ (e.g. mocktioneer). They matter
to us only **if** the HTTP port chooses convergence (§6) or when Christian's CLI
port (§7) adopts typed config. Grouped by subsystem.

### 4.1 Adapter entrypoints — `run_app` dropped the manifest arg

| Adapter    | BASE                                                                 | HEAD                                               |
| ---------- | -------------------------------------------------------------------- | -------------------------------------------------- |
| axum       | `pub fn run_app<A: Hooks>(manifest_src: &str) -> anyhow::Result<()>` | `pub fn run_app<A: Hooks>() -> anyhow::Result<()>` |
| cloudflare | `run_app<A>(manifest_src: &str, req, env, ctx)`                      | `run_app<A>(req, env, ctx)`                        |
| fastly     | `run_app<A>(manifest_src: &str, req)`                                | `run_app<A>(req)`                                  |

Manifest/store config now flows from `A::stores()` (macro-baked) + `EDGEZERO__*`
env vars instead of an `include_str!` manifest string. **Not used by us** (we have
a manual `fn main()` event loop, not `run_app`).

### 4.2 `Hooks::stores()` + `StoresMetadata`

New `fn stores() -> StoresMetadata` on the `Hooks` trait (default impl returns
empty). The `app!` macro auto-emits it from `[stores.*]`. **Not used by us** (no
`app!`, no `Hooks` impl).

### 4.3 Manifest `[stores.*]` hard rewrite

Old per-adapter shape is now a **hard load error**:

```toml
# BASE (now rejected)
[stores.kv]
name = "MY_KV"
[stores.kv.adapters.cloudflare]
name = "CF_BINDING"

# HEAD (portable; names move to env)
[stores.kv]
ids = ["default"]
default = "default"
```

`[adapters.<name>.stores.*]` and unknown `[adapters.<name>.*]` subtables now
fail `manifest.validate()`. `Manifest::kv_store_name()` removed → runtime
`EnvConfig::store_name(kind, id)` (`EDGEZERO__STORES__<KIND>__<ID>__NAME`).
**Not used by us** (no edgezero manifest).

### 4.4 Multi-store registry + async `ConfigStore`/`SecretStore`

New `store_registry.rs`: `StoreRegistry<H>` with `default()`/`named(id)`, aliases
`KvRegistry`/`ConfigRegistry`/`SecretRegistry`, and `BoundSecretStore` (binds
platform store name per logical id). New async read traits
`ConfigStore::get(&self, key) -> Result<Option<String>, ConfigStoreError>` and
`SecretStore::get_bytes(&self, store_name, key) -> Result<Option<Bytes>, …>`.
**Overlaps directly with our `PlatformConfigStore`/`PlatformSecretStore`** — see
§6. Not consumed today.

### 4.5 `RequestContext` store accessors

`kv_handle()` removed; replaced by `kv_store(id)`/`kv_store_default()` (+ config
& secret variants) returning `Option<BoundXStore>`. **Not used by us** (no
`RequestContext`).

### 4.6 Extractor overhaul

`Kv(KvHandle)` → `Kv(KvRegistry)` with `.default()`/`.named(id)`; new
`Config`/`Secrets` extractors. **Not used by us.**

### 4.7 `#[derive(AppConfig)]` + `#[secret]` (entirely new)

New derive macro (`edgezero-macros/src/app_config.rs`) emitting `AppConfigMeta`
with `SECRET_FIELDS`. `#[secret]` / `#[secret(store_ref)]` only on scalar
`String` fields; rejects `Option<String>`, `Cow`, non-scalars, `serde(rename)`,
container `rename_all`, duplicate/`=`/unknown-arg forms (compile-fail UI tests).
Pairs with CLI `config validate`/`config push`. **This is Christian's CLI-port
territory** (§7); net-new adoption, not a break.

### 4.8 `EdgeError` / `IntoResponse` / `ProxyResponse`

`EdgeError` now `#[non_exhaustive]`, gains `NotImplemented`, `source()`→`inner()`.
`IntoResponse::into_response` now returns `Result<Response, EdgeError>`.
`ProxyResponse::into_response` returns `Result`. **None used by us** (we use
`error_stack::Report<TrustedServerError>` and never touch `EdgeError`/edgezero
`IntoResponse`/edgezero `proxy`).

### 4.9 CLI surface (new) — inventory only

New `edgezero-cli` commands: `auth` (login/logout/status), `provision`,
`config validate`, `config push`, `demo` (replaces `dev`). Generated `<name>-cli`
crate per app. Typed entrypoints a consumer crate calls:
`run_config_validate_typed::<C>()`, `run_config_push_typed::<C>()`, plus
`run_{auth,build,deploy,provision,serve}`. **Christian's port** (§7).

### 4.10 Spin adapter — SDK 6.0 / wasip2

edgezero's Spin adapter moved to `spin-sdk ~6.0`, `wasm32-wasip1`→`wasip2`,
`#[http_component]`→`#[http_service]`, `IncomingRequest`→`Request`, async stores.
**Does not reach us** — trusted-server does not depend on `edgezero-adapter-spin`;
our `trusted-server-adapter-spin` is an in-repo stub.

---

## 5. Net repin work for the HTTP port (minimal path)

If the HTTP port is a **straight repin** (keep the bespoke `platform/` layer):

1. Bump the four `edgezero-*` git deps in root `Cargo.toml` from `rev = "170b74b"`
   to the #269 branch (then to `main` post-merge), regenerate root `Cargo.lock`.
2. **Reconcile `crates/integration-tests/Cargo.lock`** (it has its own lock; so
   does `crates/openrtb-codegen/`). CI enforces that shared direct deps match
   between the root and integration-tests lockfiles, and the 257-commit edgezero
   delta will drag shared transitive deps (bytes/http/serde/…). Fix with targeted
   `cargo update -p <crate> --precise <ver>` in the integration-tests workspace —
   **never** a blanket `cargo update`.
3. Fix the **18 `Body::into_bytes()` sink bindings** (§2) — 8 production + 10
   test — with explicit `.expect("should …")` / `None` handling.
4. Run the full gate (§8): host + Fastly `wasm32-wasip1`, **`--all-targets`**,
   clippy `-D warnings`, `cargo test --workspace`, `cargo fmt --check`.

**Status:** compilation is now **verified** (host, lib + tests — §10): the forced
code delta is the `Body` sinks and nothing else. **Still unverified:**
`wasm32-wasip1`, clippy, full test pass, and lockfile reconciliation (step 2).
Source-API diffing cannot see transitive breaks (dep bumps, MSRV, feature
unification, `spin-sdk ~6.0` lock entries); the §8 matrix is the proof, not this
document. "Does not reach us" (§4.10) is true at _source_ level but does not by
itself prove the lock graph resolves or that wasm builds.

### 5.1 Risks & assumptions

| Risk / assumption                                                                                                                    | Mitigation                                                                                                                                                             |
| ------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Pinning to an **OPEN, unmerged, force-pushable** ref (`feature/extensible-cli`)                                                      | Pin to the branch only if we must move pre-merge; re-pin to edgezero `main` after #269 merges (mirrors mocktioneer #110).                                              |
| **Transitive build breakage** unseen by source diffing (lock re-resolution, MSRV, features, spin-sdk 6)                              | Verification gate above — actually build on a scratch branch before sign-off.                                                                                          |
| Branch rebased / dep destabilizes after we pin                                                                                       | **Rollback = single-commit revert.** `170b74b` stays recoverable; the repin is one `Cargo.toml`/`Cargo.lock` commit — `git revert` it to return to the known-good pin. |
| Assumption: all 18 `Body` sinks are buffered (`Once`) bodies                                                                         | True today (compiler-confirmed receivers); if a future sink is genuinely streaming, use `into_stream()` / branch on `None` instead of `.expect()`.                     |
| **integration-tests lockfile drift** — CI fails if shared direct deps diverge between root and `crates/integration-tests/Cargo.lock` | Reconcile with targeted `cargo update -p --precise` (§5 step 2); never blanket-update.                                                                                 |
| **Test-only sinks slip through** a `cargo build`-only check                                                                          | Gate runs `--all-targets` + `cargo test` (§8) — 10 of 18 sinks are test-only (§10).                                                                                    |

---

## 6. Strategic divergence (decision for the HTTP port)

The original migration built a trusted-server-owned abstraction:
`RuntimeServices { config_store, secret_store, kv_store, backend, http_client,
geo, client_info }` with `PlatformConfigStore`/`PlatformSecretStore`(full CRUD)/
`PlatformKvStore`/`PlatformHttpClient` traits. #269 ships edgezero's _own_
first-class equivalents: async `ConfigStore`/`SecretStore` read traits, the
multi-store `StoreRegistry`, `Config`/`Secrets`/`Kv` extractors, env-var store
binding, and typed `AppConfig`.

So two parallel abstractions now exist for the same job. The original design doc
already flagged this risk (PR2: "these must not coexist as parallel abstractions
… file an EdgeZero issue to generalize `ProxyClient` into `HttpClient`"). #269 is
edgezero answering that — on the store/config axis.

| Option                 | What it means                                                                                                                                     | Cost     | Pull                                                                                                                                                              |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- | -------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **A. Minimal repin**   | Keep `platform/` layer; only fix `Body`.                                                                                                          | 18 sinks | Fastest; preserves CRUD writes (edgezero stores are read-only) and the `select()` fan-out we depend on.                                                           |
| **B. Converge stores** | Map `PlatformConfigStore`/`SecretStore` reads onto edgezero's `ConfigStore`/`SecretStore` + `StoreRegistry`; keep our write-CRUD as an extension. | medium   | Aligns with framework direction; less bespoke code. But edgezero read traits don't cover our management writes (key rotation), so the layer can't fully dissolve. |
| **C. Full adoption**   | Also take `run_app`/`app!`/`RequestContext`/extractors/typed `AppConfig`.                                                                         | large    | Matches mocktioneer; big rewrite of our manual dispatch + `Settings`.                                                                                             |

**Recommendation:** ship **A** as the repin (unblocks everything, tiny diff),
then evaluate **B** as a separate follow-up once #269 lands on `main`. **C** is a
roadmap question, not a repin question — and it's where the HTTP/CLI split
actually pays off: typed `AppConfig` (C/§4.7) is Christian's CLI surface, and our
read-store convergence (B) is the HTTP surface. The shared contract between the
two of you is **the typed config struct + its `[stores.config]` declaration** —
agree its shape and store id before either port starts.

---

## 7. HTTP-port vs CLI-port split

|                   | HTTP port (Prakash)                               | CLI port (Christian)                                                                                                              |
| ----------------- | ------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| #269 axis         | runtime / stores / adapters (§4.1–4.6, 4.8, 4.10) | CLI + typed config (§4.7, 4.9)                                                                                                    |
| repin-forced work | `Body` fix (§2)                                   | none (trusted-server has no edgezero CLI today)                                                                                   |
| net-new adoption  | optional store convergence (§6 B)                 | `<app>-cli` crate, `auth`/`provision`/`config validate`/`config push`, `#[derive(AppConfig)]`, CI `config validate --strict` gate |
| shared seam       | **reads** the typed config from the bound store   | **defines/validates/pushes** the typed config                                                                                     |

Note: because trusted-server uses a bespoke `Settings` + `trusted-server.toml`
(not edgezero config), Christian's CLI port is largely a **net-new adoption**
(mirroring mocktioneer #110 §3.5–3.9), not a break-fix. Sequence the typed-config
struct contract first; both ports depend on it.

---

## 8. Verification commands

```bash
# reproduce the upstream diff base (full clone; 170b74b is not in a shallow fetch)
cd /tmp && rm -rf ez && git clone https://github.com/stackpop/edgezero ez && cd ez
git fetch origin feature/extensible-cli
git diff 170b74b..origin/feature/extensible-cli -- crates/edgezero-core/src/body.rs

# after repin, in this repo — the full gate:
cargo build --workspace --all-targets        # CRITICAL: --all-targets, else 10 test-only sinks hide
cargo build --package trusted-server-adapter-fastly --target wasm32-wasip1
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check

# integration-tests lockfile gate (separate workspace lock):
( cd crates/integration-tests && cargo build --workspace )   # must resolve against root deps
```

Expected pre-fix failures: type errors at the 18 `Body` sinks (§2) — `Option<Bytes>`
has no `.len()`/`.to_vec()`, can't pass where `Bytes`/`&[u8]` expected. **8 surface
under plain `cargo build`; the other 10 only under `--all-targets`/`cargo test`.**

Compilation green is **verified** (§10). The remaining legs — wasm, clippy, full
test, lockfile reconciliation — are the proof of "done"; transitive lock/MSRV/
feature breaks surface only here, not in source-API diffing. **Re-run this gate on
every branch as the pin advances** (§11), since the sink set and line numbers
shift per layer.

---

## 9. Decisions & open questions

**Decided (this review cycle):**

- **Repin target:** pin the upgrade branch to `feature/extensible-cli` (or its
  HEAD sha) to start now; **re-pin to edgezero `main` after #269 merges.**
- **Where:** dedicated branch **off PR14** — _not_ main, _not_ in-place on any
  reviewed PR — then **merge up** the stack (merge, not rebase). See §11.
- **Scope:** the _forced_ repin work is minimal (the 18 `Body` sinks, §2/§10). The
  full A+B+C adaptation (store convergence onto edgezero's `ConfigStore`/
  `SecretStore`/`StoreRegistry`; two-tier typed `AppConfig`; entry-point
  convergence) is a **separate, optional roadmap** — see §6 and the companion
  full-adaptation design — and is **not** required by the repin.
- **Platform layer:** _hybrid_ — converge stores onto edgezero (+ a thin
  write-CRUD extension for rotation), keep `PlatformHttpClient`/`PlatformBackend`/
  `Geo`/`ClientInfo` (edgezero gaps). Roadmap, not repin.
- **`edgezero-adapter-axum`/`cloudflare`:** drop — absent from the dependency
  graph (`cargo tree -i` matches no package, §1); not compiled.

**Still open:**

1. **Convergence with `feature/ts-cli-next` (§12).** Christian's branch already
   implements the end-to-end Fastly config-store migration (repin + Body fix +
   runtime Settings-from-store), so our minimal-repin (#771) is largely subsumed
   for Fastly. Decide: rebase our HTTP work onto his config system vs keep the
   PR14-stack repin. **Recommend: build on his.**
2. **Body-fix style conflict** — his `ok_or_else` (graceful) vs our former
   `.expect()`. Resolved in §2 (adopt his); confirm per-sink alignment on merge.
3. **Secret-write conflict** — he punts secret-store writes (key rotation) until
   edgezero exposes write primitives; our original migration design kept
   write-CRUD in TS. Decide which holds.
4. **Shared config contract — now CONCRETE, not "to agree":** store ids
   `app_config` / `secrets` / `ec_identity_store` (his `edgezero.toml`) and the
   `config_payload` flatten/hash rules (his core module). The remaining gap is
   the **runtime-config-store spec** his doc references but never wrote — that is
   our HTTP-layer deliverable (§12).
5. wasm32-wasip1 + clippy + test legs of the verification gate (§10 covered host
   build only).

---

## 10. Verified build result (spike)

The §5 "prediction, not a result" hedge is **discharged for compilation** (host).
A throwaway branch `spike/edgezero-269-upgrade` was cut **off PR14** (base pin
`38198f9`), repinned to #269 HEAD (`2eeccc9`), and built twice:

```
cargo build --workspace                →  exit 101, 15 errors   (lib + bin only)
cargo build --workspace --all-targets  →  exit 101, 27 errors   (adds tests)
```

**Every error is downstream of `Body::into_bytes` → `Option`, all in
`trusted-server-core`. Zero errors from `RequestContext`, `EdgeError`, middleware,
router, or any other #269 churn — even though PR14 imports all of those.** This
empirically confirms the central thesis: the repin's forced code change is the
`Body` break and nothing else.

The two runs differ by design and this gap is the lesson:

- **Plain `cargo build`** compiles lib + bin → the **8 production** sinks (§2).
- **`--all-targets`** also compiles tests → **+10 test-only** sinks. These are
  **invisible to plain `cargo build`** and only fail under `cargo test` /
  `--all-targets`. A repin that greens `cargo build` but skips `--all-targets`
  would ship a red test suite. **The gate (§8) must include `--all-targets`.**

27 raw errors collapse to **18 distinct `into_bytes()` bindings** (one binding →
several errors via `.len()`/`.to_vec()`/`from_slice(&…)`/`from_utf8(&…)`). Full
enumeration with the production/test split is §2 — that list is now the
compiler's, superseding the earlier rg attempt (which missed `proxy.rs:38` and
`auction/endpoints.rs:81`).

**Not yet run** (remaining gate legs): `wasm32-wasip1` build, `cargo clippy -D
warnings`, `cargo test --workspace`, and the integration-tests lockfile
reconciliation (§5.1). Compilation-green ≠ gate-green.

> Evidence branch kept (repin only; trial code edits reverted) so the failing
> build is reproducible. No real branch was modified.

---

## 11. Impact on the in-flight stacked migration branches

The stack is `PR1 → … → PR20`, partially linear / partially diverged
(PR15/PR16/PR19 are not clean descendants of their predecessor — merges
happened). Pins climb in steps:

| Branches    | edgezero pin | date   |
| ----------- | ------------ | ------ |
| PR1–13      | `170b74b`    | Mar 18 |
| PR14–18     | `38198f9`    | Apr 9  |
| PR19–20     | `ce6bcf7`    | May 21 |
| (#269 HEAD) | `2eeccc9`    | Jun 12 |

**Key facts:**

- `Body::into_bytes`→`Option` landed in `7ec2ad1` ("strict clippy #257", **Jun
  12**) — _after every current stack pin_ (latest is May 21). So **no existing
  branch has absorbed the `Body` break**; whichever branch first bumps to a
  rev ≥ Jun-12 eats all 18 sinks (§2).
- **PR14 is the inflection**: it introduces `run_app`/`RequestContext`/
  `EdgeError`/`middleware`/`router` consumption (PR13 has none). That is the
  high-level surface #269 churned — _but the §10 build proves it does not break_.
- main's "only `Body`" story is therefore true **for the whole stack**, not just
  main. The earlier worry that PR14+ would drag in context/error/router breakage
  is **disproven by build**.

**Why not repin at the bottom (main / `upgrade-edgezero-http-layer`):** that base
predates the consuming code, so it can't exercise PR14+ at all; the real build
signal only exists from PR14 up.

**Why a dedicated branch off PR14, not in-place on PR14:** PR14 is still under
review; folding a version bump into it conflates two review concerns and forces
rework when review feedback rewrites the migrated code. A branch _on top of_ PR14
gets the same build signal without disturbing any open review.

**Propagation:** land the repin + `Body` fix once on the dedicated branch, then
**merge up** PR14→15→16→17→18→19→20 (merge, not rebase, per team preference).
Conflicts will cluster in the files every layer rewrites — `publisher.rs`,
`proxy.rs`, `request_signing/endpoints.rs`, `auction/endpoints.rs`. Run the §10
gate (host + wasm + clippy + test) **per branch as the pin advances**, not once.

---

## 12. Convergence with `feature/ts-cli-next` (Christian's branch)

Inspected 2026-06-18. The "CLI" branch is **not just CLI** — it is an
**end-to-end config-store migration for the Fastly adapter**, off `main`, pinned
to the **same #269 HEAD** (`2eeccc9`) we used. It already carries the repin, the
`Body` fixes, the CLI crate, _and_ runtime Settings-loading from the config store.
This materially changes our plan.

### 12.1 What it already implements (overlaps our work)

| Area                  | His branch                                                                                                                                   | Effect on us                                                 |
| --------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| #269 repin            | deps pinned `2eeccc9`                                                                                                                        | our repin (#771) duplicated for Fastly                       |
| `Body` sink fixes     | `into_bytes().ok_or_else(…)?` (prod), `unwrap_or_default()` (compress/test)                                                                  | supersedes our `.expect()` style (§2)                        |
| Store ids             | `edgezero.toml`: config `app_config`, secrets `secrets`, kv `ec_identity_store`                                                              | the shared seam, concrete                                    |
| Config contract       | `trusted-server-core/src/config_payload.rs` — flatten all of `Settings` → entries + `sha256` (`ts-config-hash`/`ts-config-keys`), reversible | shared core module; CLI pushes, runtime reads                |
| **Runtime load**      | adapter `main.rs`: `build_runtime_services()` → `get_settings_from_services()` reads `app_config`, rebuilds `Settings`                       | **this is the HTTP-layer pattern, already wired for Fastly** |
| `trusted-server.toml` | **deleted**; replaced by `trusted-server.example.toml`                                                                                       | config now lives in the store, seeded via `ts config push`   |

### 12.2 The HTTP-layer pattern, demonstrated

`crates/trusted-server-adapter-fastly/src/main.rs` (his branch):

```rust
let runtime_services = build_runtime_services(&req, kv_store); // config store available first
let settings = match get_settings_from_services(&runtime_services) { … }; // load Settings FROM store
```

`get_settings_from_services` → resolves the store name via
`env_config.store_name("config", DEFAULT_CONFIG_STORE_ID)` (`= "app_config"`) →
reads `ts-config-keys` / `ts-config-hash` / each entry →
`settings_from_config_entries` (verifies hash) → `Settings`. **That entry-point
sequence is exactly what "our HTTP layer" was meant to build.**

**Crucial: he reads through the bespoke `PlatformConfigStore`, not edgezero's
store surface.** `services.config_store()` returns `&dyn PlatformConfigStore`
(impl `FastlyPlatformConfigStore`); he uses **none** of edgezero's #269
`ConfigStore`/`StoreRegistry`/`Config` extractor/`RequestContext` (grep-confirmed:
zero in his `main.rs`/`settings_data.rs`). So he converged the config **source**
(toml → store) while **keeping `RuntimeServices` + the `platform/` layer** — i.e.
he took our §6 **hybrid** path, not full edgezero adoption. This also keeps §1's
"uses none of …" list accurate on his branch, and validates that our HTTP layer
should bind the store via `PlatformConfigStore`, not edgezero's extractor.

### 12.3 Runtime contract (new, important)

A **missing key is a hard error** — there is **no `trusted-server.toml`
fallback** anymore. The settings-error arm in `main.rs` **does serve a response**
(`to_error_response(&e).send_to_client(); return;`) — so an unseeded store yields
a **generic 500 on every route** (not a silent no-response), and that 500 is
**indistinguishable from a real config bug**. Net: the worker **cannot serve real
routes until the store is seeded** (`ts config push`). This is a new
deploy-ordering requirement (seed-before-serve) and an operational risk — the HTTP
layer should make the unseeded case **actionable** (clear message) and **correctly
classified** (retryable 503, not 500). See the plan's Phase 2.

### 12.4 Conflicts to resolve (see §9)

1. **Body-fix style** — adopt his `ok_or_else` (done in §2); align per-sink.
2. **Secret writes** — he punts key-rotation secret writes until edgezero adds
   write primitives; our original migration design kept write-CRUD in TS. Decide.
3. **Base branch** — his off `main`; ours off the PR14 stack. His already carries
   repin+Body, so for Fastly our minimal-repin is redundant.
4. **Whole-`Settings` vs two-tier** — he flattens _all_ of `Settings` into the
   store (not our two-tier small `AppConfig`). One source of truth; bigger blast.

### 12.5 Revised recommendation

- **Build on his branch, don't run a parallel repin.** Our #771 minimal-repin is
  superseded for Fastly; keep it only as the verified breaking-API reference.
- **Our HTTP-layer deliverable = the "runtime-config-store spec" his CLI doc
  references but never wrote** — document `get_settings_from_services`, the
  flatten/reconstruct rules (shared `config_payload`), the seed-before-serve
  contract, empty/malformed-store behavior, and non-Fastly adapter wiring.
- **Keep our compiler-verified `Body` enumeration (§2/§10)** as the authoritative
  sink reference when merging his ad-hoc fixes up the stack.
