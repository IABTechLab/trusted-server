# EdgeZero P0-C + P0-D — Fastly `run_app` dispatch fidelity + app-state injection

- **Status:** Draft for edgezero maintainer
- **Date:** 2026-07-03
- **Target repo:** `github.com/stackpop/edgezero` (`edgezero-adapter-fastly`, `edgezero-core`, `edgezero-macros`)
- **Consumed by:** trusted-server "full convergence" migration — the decision that every adapter binary becomes the one-line `run_app::<App>` with `#[action]` handlers. These two capabilities are the remaining gaps that block Fastly (P0-C) and macro-based app state (P0-D). Independent of the earlier Phase 0 spec (State<T> + nested `#[secret]`, PR #306).
- **Verified against:** pinned commit `6ebc29a5` (branch `worktree-state-nested-secrets-spec-review`).

---

## Why

trusted-server is converging on the canonical `app-demo` wiring: `run_app::<App>` on every adapter, `#[action]` handlers, `State<Arc<AppState>>`. Two things stop that today:

1. **Fastly `run_app` loses fidelity** that trusted-server's hand-written custom dispatch preserves: multi-value `Set-Cookie` headers, an opt-out from the per-call logger reinit, and a pre-dispatch hook to capture Fastly-only request signals (TLS JA4 / H2 fingerprint, client IP) from the raw `fastly::Request` before it is converted to the neutral core request. → **P0-C.**
2. **Macro/`run_app` apps can't inject app-owned state.** `State<T>` + `RouterBuilder::with_state` exist (PR #306) and the router injects registered state at dispatch — but the `app!` macro generates the router and never calls `with_state`, and `run_app` doesn't inject app state. So `State<Arc<AppState>>` can't reach handlers in a macro app. → **P0-D.**

**P0-D is optional** (see §4): if a downstream keeps a hand-written `Hooks::routes()` that calls `RouterBuilder::with_state`, the existing dispatch-time injection already delivers `State<T>` under `run_app` — no edgezero change needed. P0-D is required only to support app-owned state **through the `app!` macro**. It is specified here so the maintainer can choose to support the fully-macro path.

---

## P0-C — Fastly `run_app` dispatch fidelity

Three independent sub-changes in `edgezero-adapter-fastly`. Each is small and separately testable.

### C1 — Preserve multi-value response headers (`Set-Cookie`)

**Current (bug):** `crates/edgezero-adapter-fastly/src/response.rs` builds the `fastly::Response` by looping over the core response's `HeaderMap` and calling `set_header`, which **replaces** — so N `Set-Cookie` values collapse to the last one:

```rust
// response.rs (~line 28)
for (name, value) in &parts.headers {
    fastly_response.set_header(name.as_str(), value.as_bytes());
}
```

`http::HeaderMap`'s iterator yields **one entry per value** (duplicates included), and the `fastly::Response` starts empty (`FastlyResponse::from_status(...)`). So the fix is to **append** instead of set:

```rust
for (name, value) in &parts.headers {
    fastly_response.append_header(name.as_str(), value.as_bytes());
}
```

`append_header` adds without clobbering, so all `Set-Cookie` (and any other multi-value header) survive. This is unconditionally correct given a fresh response; no per-header special-casing needed.

**Same defect on the outbound proxy path:** `crates/edgezero-adapter-fastly/src/proxy.rs:53` uses `set_header` when building the upstream `fastly::Request` — audit whether request-side multi-value headers (rare, but `Cookie` folding differs) need the same treatment; at minimum document why request-side `set_header` is acceptable.

**Test:** a handler returns a `Response` with two `Set-Cookie` values; assert the converted `fastly::Response` (via `get_header_all("set-cookie")`) contains both.

### C2 — Let the app opt out of the `run_app` logger init

**Current:** `run_app` (`lib.rs:113`) initializes the Fastly logger unconditionally when `use_fastly_logger`:

```rust
let logging = logging_from_env(&env);
if logging.use_fastly_logger {
    init_logger(endpoint, logging.level, logging.echo_stdout)?;
}
```

An app that already owns `log`/`log-fastly` initialization (trusted-server does) cannot use `run_app` without a double-init conflict. Provide an opt-out. **Preferred:** a `Hooks` flag consulted by every adapter's `run_app`, so it is platform-neutral:

```rust
// edgezero-core/src/app.rs — Hooks
/// When `true`, the adapter's `run_app` skips its own logger
/// initialization; the app is responsible for installing a `log` backend.
/// Default `false` (adapter initializes logging as today).
fn owns_logging() -> bool { false }
```

`run_app` becomes `if logging.use_fastly_logger && !A::owns_logging() { init_logger(...)?; }`. (Alternative: a `run_app_without_logger::<A>` variant — but the `Hooks` flag composes with the `app!` macro and applies uniformly across adapters, so prefer it.)

**Test:** an app with `owns_logging() == true` runs `run_app` twice / after the app initialized its own logger without the init error.

### C3 — Pre-dispatch hook for raw-request signals (JA4 / H2 / client IP)

**Current:** `run_app` → `dispatch_with_registries` → `dispatch_with_handles` converts the `fastly::Request` into the neutral core request and inserts the store registries into its extensions. There is **no hook** to read the *original* `fastly::Request` (whose `get_tls_ja4()`, `get_client_h2_fingerprint()`, `client_ip` are only available pre-conversion) and stash derived values into the core request's extensions. trusted-server's custom path does exactly this before dispatch.

**Proposed:** a Fastly-adapter `run_app` variant that accepts a pre-dispatch closure which populates extensions from the raw request:

```rust
// edgezero-adapter-fastly/src/lib.rs
pub fn run_app_with_request_extensions<A, F>(
    req: fastly::Request,
    extend: F,
) -> Result<fastly::Response, fastly::Error>
where
    A: Hooks,
    F: FnOnce(&fastly::Request, &mut http::Extensions),
{ /* same as run_app, but call `extend(&req, core_req.extensions_mut())`
     inside dispatch, after conversion and after registry insertion,
     before the router runs */ }
```

The closure runs once per request, receives the raw `fastly::Request` and the core request's `Extensions`, and inserts whatever typed values the app needs (trusted-server inserts its `ClientInfo` + `DeviceSignals`). `run_app` stays as the no-hook convenience wrapper (`run_app_with_request_extensions::<A>(req, |_, _| {})`).

This requires threading the closure from `run_app_with_request_extensions` → `dispatch_with_registries` → `dispatch_with_handles` (add a generic `extend: F` parameter, or an `Option<&mut dyn FnMut(&FastlyRequest, &mut Extensions)>`). Keep the existing `dispatch_with_registries` signature working (the no-op closure).

**Test:** a handler reads a value from extensions that only the pre-dispatch closure could have set (e.g. a synthetic `Ja4` newtype); assert it is present.

### P0-C acceptance

- Multi-value `Set-Cookie` round-trips through `run_app` (C1).
- An app that owns logging runs under `run_app` without a logger-init error (C2).
- A pre-dispatch closure can populate core-request extensions from the raw `fastly::Request` (C3).
- `app-demo` still builds/serves; existing Fastly tests green; `run_app` (no-hook) behavior unchanged for apps that don't opt in.

---

## P0-D — App-state injection for macro / `run_app` apps

### The gap

`State<T>` (`extractor.rs:550`) reads from request extensions; `RouterBuilder::with_state` (`router.rs`) registers a value that the router's `state_inserters` clone into each request at dispatch (`router.rs` ~line 256). That works when the app **hand-builds** its router. But the `app!` macro generates `Hooks::routes()` and never calls `with_state`, and `run_app` doesn't inject app state — so a macro app has no way to provide `State<Arc<AppState>>`.

### Design — symmetric with registry injection

Registries are injected **per request** by each adapter's `run_app` (in `dispatch_with_handles`), not baked into the router. Mirror that for app state:

1. **`edgezero-core/src/app.rs` — new `Hooks` method** (default: no state):
```rust
/// App-owned state inserted into every request's extensions before dispatch,
/// making it available to the `State<T>` extractor in macro-based apps.
/// Returns type-erased inserters (same shape as RouterBuilder's state layer).
/// Default: none.
fn app_state() -> AppState { AppState::default() }
```
where `AppState` is a small type-erased carrier (reuse the `StateInserter = Arc<dyn Fn(&mut Extensions) + Send + Sync>` shape already in `router.rs`, exposed as a public builder, e.g. `AppState::default().with(value)`).

2. **Each adapter's `run_app` applies `A::app_state()` to every request's extensions** — in the same spot it inserts the store registries (Fastly `dispatch_with_handles`; Axum `EdgeZeroAxumService`; Cloudflare/Spin equivalents). Precedence note: app-state inserts should not overwrite the store registries (distinct `TypeId`s; document last-writer-wins if an app registers a colliding type).

3. **`edgezero-macros` — `app!` gains an optional `state` argument** so macro apps can wire it:
```rust
edgezero_core::app!("edgezero.toml", state = crate::app_state);
// expands to `fn app_state() -> AppState { crate::app_state() }` in the generated Hooks impl
```
Without the argument the generated `app_state()` uses the default (no state), preserving current behavior.

### Alternative that needs NO P0-D (document in the guide)

A downstream that keeps a **hand-written `Hooks::routes()`** can call `RouterBuilder::with_state(app_state)` there; the existing dispatch-time `state_inserters` then inject it under `run_app` with zero further change. The trade-off is routes are built in Rust rather than declared in `edgezero.toml`. trusted-server may take this path to avoid P0-D — but P0-D is what makes app state work for the **fully macro-driven** shape `app-demo` models.

### P0-D acceptance

- A macro app declaring `app!("...", state = f)` can extract `State<T>` (where `T` is what `f` returns) in an `#[action]` handler on all four adapters.
- An app that provides no state is unaffected (`State<T>` for an unregistered `T` returns the existing "no state registered" 500).
- `app-demo` gains a small example using `app!(..., state = ...)` + a `State<T>` handler.

---

## Sequencing & interaction with trusted-server Phase 1

- **P0-C is required** for trusted-server Phase 4 (Fastly `run_app`). Until it lands, trusted-server's Phase 1 keeps interim Fastly local registry builders + custom `oneshot`; those are deleted in Phase 4 once P0-C exists. Landing P0-C early lets Phase 1 skip that throwaway scaffolding.
- **P0-D is required only for the `app!`-macro path.** If trusted-server keeps hand-built `routes()` + `with_state`, P0-C alone suffices for full `run_app` convergence. Decide this before Phase 4.
- Both are independent of the nested-`#[secret]` work already in #306.

## Files to touch (edgezero)

**P0-C**
- `crates/edgezero-adapter-fastly/src/response.rs` — `set_header` → `append_header` (C1)
- `crates/edgezero-adapter-fastly/src/proxy.rs` — audit request-side `set_header` (C1)
- `crates/edgezero-core/src/app.rs` — `Hooks::owns_logging()` (C2)
- `crates/edgezero-adapter-fastly/src/lib.rs` — consult `owns_logging()`; add `run_app_with_request_extensions` (C2, C3)
- `crates/edgezero-adapter-fastly/src/request.rs` — thread the pre-dispatch closure through `dispatch_with_registries`/`dispatch_with_handles` (C3)

**P0-D**
- `crates/edgezero-core/src/app.rs` — `Hooks::app_state()` + `AppState` carrier
- `crates/edgezero-core/src/router.rs` — expose the `StateInserter`/state-carrier type publicly (reuse existing)
- `crates/edgezero-adapter-{fastly,axum,cloudflare,spin}/src/…` — apply `A::app_state()` per request alongside registries
- `crates/edgezero-macros/src/app.rs` — optional `state = <path>` argument
