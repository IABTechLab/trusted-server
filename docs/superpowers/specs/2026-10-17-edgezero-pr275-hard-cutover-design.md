# EdgeZero PR 275 Hard Cutover

**Date:** 2026-10-17

**Status:** Draft, in agent review

**EdgeZero target:** `IABTechLab/edgezero` PR 275, head
`f6c708ccb5be19ba9fa92575db3c08c0f17e0d72` (branch `docs/outbound-http-spec`),
called **F** below

**Depends on:** #1123 (Rust 1.98.1 and the dependency majors EdgeZero v0.0.10
requires)

**Follow-ups:** see [Follow-up specs](#follow-up-specs)

Citations use Trusted Server `main` at `a9037770a` for repository paths and
`edgezero:<path>:<line>` for EdgeZero at F.

## Problem

Trusted Server pins all six EdgeZero crates to `tag = "v0.0.9"`
(`Cargo.toml:59-64`). EdgeZero PR 275 adds portable outbound HTTP and request
lifecycle contracts across core and the four adapters, and it removes the APIs
they supersede. F contains EdgeZero `v0.0.10` and EdgeZero `main` at
`41f47872`, about 400 commits over `v0.0.9`. EdgeZero ships no compatibility
layer, so Trusted Server has to move every adapter in one change.

A compile probe of `main` with #1123's changes, repinned to F, found 96 error
sites: 73 in library code and 23 in tests. That count stops at the first failing
library in each crate, so the Axum route tests and the parity tests, which name
the now-private `EdgeZeroAxumService`, are additional. Most fixes are mechanical.

The larger risk compiles cleanly. Every adapter now hands handlers a lazy
streamed request body, even for a bodiless `GET`. Trusted Server's Fastly,
Cloudflare, and Spin outbound clients reject streamed bodies, and
`POST /auction` reads a streamed body as empty. Without a fix, publisher pages
fail on three targets and auctions return 400 on all four. Unit and parity
tests stay green because their fixtures build buffered bodies. The CI
integration suite would catch part of it (publisher pages on Fastly, Cloudflare,
and Axum, and `POST /auction` on workerd). It does not cover Spin, publisher
`POST` bodies, or `POST /auction` on Fastly and Axum.

## Goals

- Move all six EdgeZero crates to F in one pull request, with no shim that
  keeps `v0.0.9` behavior alive.
- Keep every adapter (Fastly, Axum, Cloudflare, Spin) compiling, passing the CI
  gates, and passing its runtime smoke on F.
- Keep request handling on Fastly, the production target, the same as today:
  same routes, same response status and headers, same auction semantics. The
  only intended Fastly differences are listed in
  [Fastly differences](#fastly-differences).
- Make the inbound body contract explicit. Every Trusted Server handler
  receives a buffered body under one known cap.
- Close the coverage gaps above with tests that drive real inbound bodies on
  every adapter.
- Delete Trusted Server code that the cut leaves dead.

## Non-goals

- Do not adopt EdgeZero's outbound `HttpClient` or batch API in this cut.
  Trusted Server keeps its own `PlatformHttpClient` and `PlatformBackend`
  layer. See [section 9](#9-outbound-http-stays-trusted-server-owned).
- Do not move Fastly onto EdgeZero's canonical lifecycle
  (`send_request_with_registries_and_hooks`).
- Do not port EdgeZero PR #403 (pre-dispatch hooks, `RequestIngress`) or PR #389
  (request timing) onto PR 275. Those are EdgeZero-side work.
- Do not change the toolchain, the build targets, or the dependency majors.
  #1123 owns them.
- Do not adopt optional F capabilities: ingress admission policies, egress
  observers, error-response renderers, route classes, retained app instances on
  Cloudflare and Spin, or real loggers on Cloudflare and Spin.
- Do not route Fastly config reads through EdgeZero's `FastlyConfigStore`.
  Trusted Server's chunk reader stays (it remains byte-compatible).
- Do not change consent, identity, auction, or HTML rewriting behavior.

## Decisions

| #   | Decision                                                                                                                          | Owner                    |
| --- | --------------------------------------------------------------------------------------------------------------------------------- | ------------------------ |
| D1  | Hard cutoff: one PR, no dual pin, no feature flag, no compatibility shim                                                          | User                     |
| D2  | Land on top of #1123, do not absorb it                                                                                            | Spec author              |
| D3  | Pin `rev = "f6c708cc…"` on the `IABTechLab/edgezero` URL; merge gating is open question Q1                                        | User target, gating open |
| D4  | Outbound HTTP stays Trusted Server-owned in this cut                                                                              | Spec author, open as Q2  |
| D5  | Fastly keeps its custom lifecycle entry point in this cut                                                                         | Spec author, open as Q3  |
| D6  | Inbound bodies are buffered at the adapter boundary under a 16 MiB cap; overflow is 413                                           | Spec author              |
| D7  | The Cloudflare outbound client strips `Trailer`, like the Axum and Spin clients already do                                        | Spec author              |
| D8  | `Trailer` and `Content-Length` in operator or integration response headers: rejected at deploy, skipped with a warning at runtime | Spec author              |
| D9  | The Axum dev server reads `EDGEZERO__ADAPTER__INGRESS__*` from the environment                                                    | Spec author              |
| D10 | Axum route tests and every parity leg use `RouterService::oneshot`; real-ingress coverage lives in the integration suite          | Spec author              |
| D11 | A failed `spin up` blocks the cut; moving Spin to `wasm32-wasip2` would be its own prerequisite PR                                | Spec author              |

The rationale for each call sits in the design section that implements it.

## Design

### 1. Pin and lockfile

After #1123 the six EdgeZero crates read `tag = "v0.0.10"`. This change moves
them to `rev = "f6c708ccb5be19ba9fa92575db3c08c0f17e0d72"` on
`https://github.com/IABTechLab/edgezero`. Nothing else in `Cargo.toml` changes.

F pins 25 crates with exact `=` requirements inside EdgeZero's own crates
(v0.0.10 had none), and those pins bind Trusted Server's whole lockfile.
Starting from #1123's lock, the resolver moves five entries:

| Crate                 | #1123 lock | F      | Effect    |
| --------------------- | ---------- | ------ | --------- |
| `async-compression`   | 0.4.42     | 0.4.43 | upgrade   |
| `brotli-decompressor` | 5.0.3      | 5.0.1  | downgrade |
| `bytes`               | 1.12.0     | 1.12.1 | upgrade   |
| `hyper`               | 1.10.1     | 1.12.0 | upgrade   |
| `want`                | 0.3.1      | 0.3.2  | upgrade   |

A scoped `cargo update -p edgezero-core` also rewrote the `colored` and
`winapi-util` edges from `windows-sys 0.61.2` to `0.48.0` in the probe. Restore
those by hand in their `[[package]]` blocks, diff the lockfile for any other
edge the pin did not require, and prove the result with `--locked` builds.

`fastly`, `log-fastly`, and `spin-sdk` keep #1123's range form (`0.13`,
`~7.0`). Resolution forces `0.13.1` and `7.0.0` either way, and the lockfile
records the exact versions. The `esi` fork patch from #1123 stays valid,
because `^0.13` accepts `=0.13.1`.

All 25 exact pins become version ceilings until EdgeZero moves them. They
include `fastly`, `fastly-shared`, `fastly-sys` and `log-fastly` (`=0.13.1`),
`spin-sdk` (`=7.0.0`), `worker` (`=0.8.5`), `serde_json` (`=1.0.150`), `url`,
`bytes`, `http`, `hyper`, `reqwest` (`=0.13.4`), `wasm-streams`, and the
compression crates. Dependabot cargo updates for any of them will fail
resolution. That noise is accepted here and handled as a follow-up.

### 2. Inbound body ownership

At `v0.0.9`, Fastly, Cloudflare, and Spin read the client body into a buffered
`Body::Once` before routing, and Axum buffered JSON only. At F, all four adapters
hand the router a lazy `Body::Stream`, deadline-wrapped on Axum, Cloudflare, and
Spin (`edgezero:crates/edgezero-adapter-fastly/src/request.rs:702-706`,
`edgezero:crates/edgezero-adapter-cloudflare/src/request.rs:342`,
`edgezero:crates/edgezero-adapter-spin/src/request.rs:574-579`,
`edgezero:crates/edgezero-adapter-axum/src/request.rs:43-48`).
`RequestContext::into_request` passes an undrained body through unchanged
(`edgezero:crates/edgezero-core/src/context.rs:381-394`), and
`Body::into_bytes` returns `None` for a stream.

What breaks in Trusted Server if nothing changes:

- **Publisher fallback, every method, on Fastly, Cloudflare, and Spin.** The
  client request becomes the origin request with its body intact
  (`crates/trusted-server-core/src/publisher.rs:4810-4813`). The outbound
  converters reject `Body::Stream`
  (`crates/trusted-server-adapter-fastly/src/platform.rs:313-342`,
  `crates/trusted-server-adapter-cloudflare/src/platform.rs:333-340`,
  `crates/trusted-server-adapter-spin/src/platform.rs:499-507`). Every
  publisher page fails.
- **`POST /auction`, all four adapters.** `body.into_bytes().unwrap_or_default()`
  yields an empty body, and the JSON parse returns 400
  (`crates/trusted-server-core/src/auction/endpoints.rs:147-163`).
- **Buffered-only endpoints return 400.** `request_body_bytes`
  (`crates/trusted-server-core/src/proxy.rs:47-58`) rejects streams for
  `/first-party/sign` and `/first-party/proxy-rebuild`, and the request-signing
  endpoints do the same for verify, rotate, and deactivate
  (`crates/trusted-server-core/src/request_signing/endpoints.rs:27-38`). Cache
  purge has the same check (`crates/trusted-server-core/src/cache_purge.rs:135-141`).
- **EC batch sync silently reads an empty body**
  (`crates/trusted-server-core/src/ec/batch_sync.rs:152`).

**Design.** Trusted Server buffers the inbound body once, at the adapter
boundary, before any handler sees the request. This restores the `v0.0.9`
shape on Fastly, Cloudflare, and Spin, adds a cap that `v0.0.9` lacked, and
gives Axum the same contract as the other three.

- `crates/trusted-server-core/src/http_util.rs` gains
  `MAX_INBOUND_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024`. It matches the
  `publisher.max_buffered_body_bytes` default
  (`crates/trusted-server-core/src/settings.rs:83-85`) and sits well inside
  Fastly's 128 MB per-execution memory.
- The same module gains one async helper, `into_buffered_request(ctx, limit)`.
  It consumes a `RequestContext` and returns a `Request` with a `Body::Once`
  body, or a `Report<TrustedServerError>`:
  - A declared `Content-Length` over `limit` returns
    `TrustedServerError::RequestTooLarge` (413) without reading the body.
  - Otherwise it takes the body with `ctx.take_body()` and collects it with its
    own bounded loop. A stream that grows past `limit` returns `RequestTooLarge`.
    EdgeZero's `body_bytes` would report that overflow as a generic 400, which
    is why the helper owns the loop. The integration collector in
    `crates/trusted-server-core/src/integrations/mod.rs:182-215` is not reused,
    because it maps every read error to a 502 integration error.
  - A stream item that is an EdgeZero request timeout (status 408) returns a new
    `TrustedServerError::RequestTimeout` variant mapped to 408. Any other stream
    error returns `TrustedServerError::BadRequest` (400).
  - Client-caused 408 and 413 log at `warn`, not `error`.
- Every Trusted Server call of `RequestContext::into_request` in adapter code
  becomes `into_buffered_request(ctx, MAX_INBOUND_REQUEST_BODY_BYTES).await`.
  The probe found 16 such calls: Axum `src/app.rs:163`, Cloudflare
  `src/app.rs:279` and `:515`, Fastly `src/app.rs:548`, `:768`, and test
  `:3000`, and ten in Spin `src/app.rs` (`:619` through `:799`).
- `clippy.toml` gains `disallowed-methods` entries for
  `edgezero_core::context::RequestContext::into_request` and
  `edgezero_core::context::RequestContext::take_body`, with a reason that names
  the helper. The helper carries the only `#[allow]`. A scratch probe on clippy
  1.98.1 confirmed the lint fires on an external crate's inherent method for
  method and path calls and honors `#[allow]`.
- Endpoint caps stay as they are and still apply after the boundary cap:
  auction 256 KiB, sign and rebuild 64 KiB, verify and admin 4 KiB, purge
  4 KiB, batch sync 2 MB. Their stream checks become unreachable from the
  adapters but stay as guards on the core API. One cap for every route keeps
  the helper simple. Per-route caps would not lower worst-case memory, because
  the publisher fallback accepts a 16 MiB body on any path.

**Behavior change.** A request body over 16 MiB now gets 413 on every adapter,
including production Fastly. At `v0.0.9`, Fastly, Cloudflare, and Spin buffered
without a cap (bounded only by platform memory), and Axum buffered without a cap
for outbound sends. An uncapped buffer of a client-controlled stream is a memory
risk, so the cut takes the cap rather than reproduce that. It matters only for
uploads over 16 MiB proxied to the publisher origin.

### 3. `RequestContext` accessors

`RequestContext::request()` and `request_mut()` are gone. Their replacements are
`parts()`, `parts_mut()`, `headers()`, `headers_mut()`, `extensions()`,
`extensions_mut()`, `uri()`, and `method()`
(`edgezero:crates/edgezero-core/src/context.rs:336-368`, `:432`, `:483-490`,
`:600`). The probe found 29 sites across the four adapters' `middleware.rs`,
`platform.rs`, and `app.rs`. Typed adapter context moves to
`ctx.extensions().get::<FastlyRequestContext>()` and the Axum, Cloudflare, and
Spin equivalents.

Three shared helpers take `&mut Request` only to reach headers, the URI, and
extensions. They move to `&mut http::request::Parts`, and middleware passes
`ctx.parts_mut()`:

- `enforce_basic_auth` (`crates/trusted-server-core/src/auth.rs:79-82`), with
  `extract_credentials` (`:99`, `:137`)
- `sanitize_trusted_client_ip_headers`
  (`crates/trusted-server-core/src/http_util.rs:57-60`)
- `normalize_spin_request` (`crates/trusted-server-adapter-spin/src/app.rs:294`),
  with `sanitize_forwarded_headers` (`:295`) and the call into
  `SpinRequestContext::insert`, which still takes `&mut Request`
  (`edgezero:crates/edgezero-adapter-spin/src/context.rs:30`)

`http::Request` has no `&mut Parts` accessor, so tests that call these helpers
on a whole request split it with `into_parts` and rejoin it. That touches about
24 `enforce_basic_auth` tests (`crates/trusted-server-core/src/auth.rs:213-601`),
`crates/trusted-server-core/src/http_util.rs:707`, `:735`, `:771`,
`crates/trusted-server-core/src/publisher.rs:11281`, and five Spin tests
(`crates/trusted-server-adapter-spin/src/app.rs:1134-1318`).

### 4. Streamed response bodies

`Body::from_stream` now requires `Result<Bytes, EdgeError>` items. The old
behavior is `Body::from_external_stream`, which wraps foreign errors as
`EdgeError::Internal` (`edgezero:crates/edgezero-core/src/body.rs:48-69`).
`EdgeError::Internal` displays as `internal error: {source}`
(`edgezero:crates/edgezero-core/src/error.rs:153`), so log lines that format a
stream error keep their detail. Rename the calls:

- production: `crates/trusted-server-core/src/publisher.rs:2532` and `:2742`,
  `crates/trusted-server-adapter-fastly/src/platform.rs:366`
- tests: `crates/trusted-server-core/src/proxy.rs:5939` and `:5993`,
  `crates/trusted-server-core/src/publisher.rs:19624`, `:20219`, `:20558`,
  `crates/trusted-server-adapter-fastly/src/platform.rs:1449` and `:1524`

### 5. Config store

- **Cloudflare and Spin `ConfigStoreHandleAdapter::get`**
  (`crates/trusted-server-adapter-cloudflare/src/platform.rs:95-105`,
  `crates/trusted-server-adapter-spin/src/platform.rs:112-122`). `get` now
  returns `ConfigValue` (`edgezero:crates/edgezero-core/src/config_store.rs:310-364`).
  Convert to `String` at the boundary. Nothing above the adapter changes.
- **Spin test `InMemoryConfigStore`**
  (`crates/trusted-server-adapter-spin/src/platform.rs:806-812`). Return
  `ConfigValue` and implement the new required `get_bounded` by delegating to
  `finish_bounded_config_read`.
- **`SpinConfigStore::open(..).await` is removed**
  (`crates/trusted-server-adapter-spin/src/app.rs:88-95`). Use
  `SpinConfigStore::for_label`. An open failure now surfaces at the first `get`
  through the existing "failed to read Spin Trusted Server app-config blob"
  branch (`app.rs:98-104`), which `scripts/smoke-spin.sh` already expects.
- **Fastly config-store handle.** Delete `open_trusted_server_config_store` and
  the `ConfigStoreHandle` request-extension insert
  (`crates/trusted-server-adapter-fastly/src/main.rs:64-69`, `:371-381`,
  `:464`). Nothing reads that extension, `try_open` now returns a redacted
  `ConfigStoreError`, and the insert costs a host call per request. The failure
  mode changes. Today an unopenable store returns a plain-text 500 before the
  app is built. After the deletion it surfaces from settings loading through the
  startup-error router (`crates/trusted-server-adapter-fastly/src/app.rs:1042-1065`)
  as a 500, with one build attempt per request.

The app-config envelope, the canonical data hash, and the Fastly chunk pointer
format are byte-compatible between `v0.0.9` and F. No re-push is needed.

### 6. App assembly and entry points

**Fastly.**

- The `run_custom` and `serve_custom` callbacks must return `Result<(), E>`
  (`edgezero:crates/edgezero-adapter-fastly/src/lifecycle.rs:129-157`). Trusted
  Server returns `Ok::<(), Infallible>(())` from both
  (`crates/trusted-server-adapter-fastly/src/main.rs:110-142`). An `Err` would
  end the reusable-sandbox loop, and today a post-commit delivery failure does
  not retire the sandbox.
- `Hooks::configure` now returns `Result<(), EdgeError>`, and
  `Self::configure(&mut app)` at
  `crates/trusted-server-adapter-fastly/src/app.rs:1279-1280` drops a
  `#[must_use]` result. Build with
  `App::with_name_and_platform(router, name, FASTLY_PLATFORM)` and send a
  configure failure down the existing state-build failure path.
- Fastly stays on `into_core_request`, `app.router().oneshot()`, and Trusted
  Server's own `send_edgezero_response` (`main.rs:462-471`, `:668-724`). F's
  admission limits, deadlines, and response framing therefore do not apply on
  Fastly. That is intentional for this cut. It keeps production behavior fixed
  while the dependency moves, and EdgeZero has not yet run the canonical Fastly
  composition end to end under Viceroy
  (`edgezero:docs/guide/capabilities.md:284-288`).
- Error rendering does change on Fastly, because `oneshot` renders routing and
  handler errors through `EdgeError::into_response`
  (`edgezero:crates/edgezero-core/src/router.rs:562-568`). At F that redacts
  `EdgeError::Internal` messages and adds an `Allow` header to 405 responses
  (`edgezero:crates/edgezero-core/src/error.rs:405-417`, `:555-575`). Trusted
  Server registers seven methods (`crates/trusted-server-adapter-fastly/src/app.rs:291-300`),
  so `TRACE` and `CONNECT` now get 405 with `Allow`.
- F initializes the Fastly ABI inside `run_custom` and `serve_custom`, after
  Trusted Server has already evaluated `FastlyRequest::from_client()` as the
  argument (`main.rs:110-111`). In SDK 0.13.1 that init is only the ABI version
  handshake, so the order is low risk. The Viceroy runs confirm it.

**Axum.**

- `AxumDevServerConfig` gains a required `ingress` field
  (`edgezero:crates/edgezero-adapter-axum/src/dev_server.rs:53-59`). Set it from
  `AxumIngressConfig::from_env(&EnvConfig::from_env())` and fail startup on an
  invalid value (`crates/trusted-server-adapter-axum/src/main.rs:32-35`). Without
  this, the connection and parser limits cannot be raised for benchmarks or
  parallel integration runs. See [Configuration](#configuration) for the
  variables.
- Axum now serves every connection on one current-thread runtime
  (`edgezero:crates/edgezero-adapter-axum/src/dev_server.rs:328-422`). CPU-heavy
  rewriting serializes across connections. A response with more than
  `max(100, header-count limit)` header fields or more than 64 KiB of headers
  closes the connection with no response. Axum is the development and
  integration-test server, so both are accepted.
- The Axum request-target and header-byte limits cannot be raised.
  `AxumDevServer::with_config` wraps `App::new(router)` and never calls
  `Hooks::configure` (`edgezero:crates/edgezero-adapter-axum/src/dev_server.rs:134`),
  and the service enforces the fixed 8192-byte target
  (`edgezero:crates/edgezero-adapter-axum/src/service.rs:225`). Accepted for the
  dev server. A dev-server constructor that takes an `App` is an EdgeZero ask.
- `EdgeZeroAxumService` is now private, with no public replacement
  (`edgezero:crates/edgezero-adapter-axum/src/lib.rs:30`). The Axum route tests
  (`crates/trusted-server-adapter-axum/tests/routes.rs`) and the Axum leg of the
  parity tests (`crates/trusted-server-integration-tests/tests/parity.rs`) move
  to `RouterService::oneshot` over
  `TrustedServerApp::routes_with_settings` (or `routes_with_settings_and_services`),
  the seam the Cloudflare and Spin legs already use
  (`crates/trusted-server-integration-tests/tests/parity.rs:126-309`). Every
  parity leg then compares the same seam. `App::build::<TrustedServerApp>` is not
  usable in host tests, because it calls `routes()`, which returns the
  startup-error router outside a real deployment
  (`crates/trusted-server-adapter-axum/src/app.rs:618-636`). Real adapter
  ingress is covered by the integration suite instead.

**Cloudflare.** `run_app::<TrustedServerApp>` keeps its signature and now builds
the app with `App::build::<A>(CLOUDFLARE_PLATFORM)` on every fetch
(`edgezero:crates/edgezero-adapter-cloudflare/src/lib.rs:172-183`). No entry
point change. `run_app` now fails only for build, manifest, required-store, or
abort errors, so the fallback in
`crates/trusted-server-adapter-cloudflare/src/lib.rs:33-37` is close to
unreachable. Keep it.

**Spin.** `run_app` returns `SpinResponse`, which the existing
`impl IntoResponse` return still accepts. With `spin-sdk` 7 from #1123, the
component exports final `wasi:http@0.3.0` and needs a Spin 4.1 runtime. Trusted
Server builds Spin for `wasm32-wasip1`, while EdgeZero validates
`wasm32-wasip2`. F's Spin adapter also uses `spin_sdk::wasip3` timer and spawn
imports that #1123 does not exercise. Only `spin up` proves the component
instantiates. If it fails ([D11](#decisions)), the cut stops, and moving Trusted
Server's Spin build to `wasm32-wasip2` becomes its own prerequisite pull
request, since it touches `rust-toolchain.toml` targets, the `.cargo/config.toml`
aliases, `edgezero.toml`, and `spin.toml`.

### 7. Response framing on Axum, Cloudflare, and Spin

Axum, Cloudflare, and Spin now run EdgeZero's `prepare_response_egress` on every
response (`edgezero:crates/edgezero-core/src/response_egress_framing.rs:76-145`).
Fastly does not, because of [D5](#decisions).

- **`Trailer` becomes a 500.** Any `Trailer` header fails framing, on every
  status and method, before hop-by-hop stripping (`:106-108`). Trusted Server
  keeps an origin `Trailer` on HEAD and 304 publisher responses
  (`crates/trusted-server-core/src/publisher.rs:2801-2841`), but on Axum and
  Spin the outbound clients already strip `trailer` from origin responses
  (`crates/trusted-server-adapter-axum/src/platform.rs:221-226`,
  `crates/trusted-server-adapter-spin/src/platform.rs:265-274`). Cloudflare's
  list omits it (`crates/trusted-server-adapter-cloudflare/src/platform.rs:229-240`),
  so [D7](#decisions) adds it. `make_response_bodiless` stays as it is, which
  keeps Fastly's HEAD and 304 responses unchanged. The remaining sources are
  operator `response_headers` and integration-supplied response headers, which
  [D8](#configuration) handles.
- **Strict `Content-Length`.** A buffered body whose length differs from
  `Content-Length` becomes a 500 before commit, and a stream that runs long or
  short is aborted after commit (`:115-135`). Trusted Server sets the length from
  final bytes on its rewrite paths. The same two remaining sources apply, also
  handled by D8.
- **Silently stripped fields.** `Connection` and the fields it nominates,
  `Transfer-Encoding`, `Keep-Alive`, `TE`, `Upgrade`, and `Proxy-Connection` are
  removed without error (`:281-311`). Only an unparseable `Connection` token is
  a 500.
- **Framework errors bypass Trusted Server middleware.** Admission refusals
  (400 framing, 405 with `Allow`, 408, 414, 431) and egress fallbacks render as
  EdgeZero JSON without `X-TS-Version`, geo headers, or operator
  `response_headers`. Accepted for this cut on these three adapters.
- **Head limits.** A request target over 8192 bytes returns 414, and more than
  100 header values or 64 KiB of headers returns 431
  (`edgezero:crates/edgezero-core/src/ingress.rs:21-23`, `:840-890`). The
  adapters enforce these before admission, and on Cloudflare the target is the
  absolute URL. Keep the defaults and test that realistic traffic fits (new test
  7). If it does not, raise the limits with `App::set_ingress_head_limits` in
  `Hooks::configure` on Cloudflare and Spin. Axum cannot be raised (section 6).
- **Deadlines.** Inbound reads and response writes each get 30 s
  (`edgezero:crates/edgezero-core/src/ingress.rs:20`,
  `edgezero:crates/edgezero-core/src/response_egress.rs:14`). Cloudflare and
  Spin buffer publisher responses before returning them
  (`crates/trusted-server-adapter-cloudflare/src/app.rs:290-320`), so the write
  deadline covers delivery only. Accepted.
- **Cloudflare wire framing.** Buffered bodies without `Content-Length` now go
  out as unsized streams, untyped bodies lose the default `Content-Type`, and a
  header value that Workers rejects fails the whole response into the 500
  fallback. Accepted, and covered by the workerd integration tests.

### 8. Manifest

`edgezero.toml` gains:

```toml
[capabilities.outbound]
hosts = ["*"]
```

`ts build`, `ts serve`, and `ts deploy --adapter spin` now compare Spin's
`allowed_outbound_hosts` with this list
(`edgezero:crates/edgezero-adapter/src/cli_support.rs:133-236`). An absent list
means `https://*:*` only, while
`crates/trusted-server-adapter-spin/spin.toml:43` also allows plain HTTP, so all
three commands would fail. `"*"` expands to both schemes and matches today's
`spin.toml`. Declare no `required` or `optional` capabilities: Fastly reports
`outbound-http` as best effort, and requiring it would block Fastly deploys.

The manifest now rejects unknown top-level keys
(`edgezero:crates/edgezero-core/src/manifest.rs:330-363`). Trusted Server's
manifest has none. `build.rs` exits on a manifest load error, so a future typo
fails the build.

The Cloudflare `enable_weak_ref` and `enable_request_signal` compatibility flags
are not added. EdgeZero needs them only for bounded config-KV reads and egress
cancellation, and Trusted Server uses neither.

### 9. Outbound HTTP stays Trusted Server-owned

Trusted Server calls no EdgeZero outbound API today. Every outbound request goes
through its own `PlatformHttpClient` and `PlatformBackend` traits
(`crates/trusted-server-core/src/platform/http.rs:338-433`,
`crates/trusted-server-core/src/platform/traits.rs:96-137`) over the raw SDKs.
Removing EdgeZero's old `proxy` module breaks nothing in Trusted Server, so the
cut does not force adoption.

Adopting F's outbound client now would change auction and proxy behavior in
ways that each need measurement or an upstream change:

- Bids that complete after the batch cutoff are dropped. Today they are kept.
- Fastly backend names carry a per-client namespace and the exact millisecond
  budget, so connections are not reused across requests. Expect a TCP and TLS
  handshake to every bidder on every auction.
- Fastly phase timers become connect = budget/4 and first byte = 3/4 of the
  budget. Today connect is a fixed 1 s and first byte gets the full budget.
- Batches take buffered responses only, so the publisher origin fetch can no
  longer start before bid construction.
- Responses are always decoded with no opt-out, so reader-facing HTML and
  proxied scripts go out uncompressed. Buffered responses default to a 1 MiB
  decoded cap, and a request with no timeout gets a 30 s total budget that also
  bounds streamed bodies.
- `Host` headers are dropped, Spin rejects a host override with 400,
  `certificate_check = false` has no equivalent, and neither do surrogate-key
  cache intent or the Fastly Image Optimizer hook.
- Every outbound response gains an `x-edgezero-proxy` header, which Trusted
  Server would pass through to browsers.

So the cut touches Trusted Server's own clients in two places only: the
`from_external_stream` rename (section 4) and the buffered inbound body that the
publisher forward now receives (section 2). The Fastly SDK 0.13.1 and Spin SDK 7
moves come from #1123. Adoption is the Phase 2 follow-up.

One side effect: F's Axum dev server builds an EdgeZero `reqwest` transport that
Trusted Server does not use. If that transport fails to build, every admitted
request returns 500. Accepted.

### 10. Documentation

- `docs/guide/cli.md:222-224` says build, serve, and deploy load `edgezero.toml`
  from the working directory. F walks up from the working directory and then
  picks the nearest manifest in the workspace
  (`edgezero:crates/edgezero-cli/src/manifest_source.rs:116-410`). Update the
  rule.
- Update any guide page that names the EdgeZero version or describes the request
  body contract.
- Document the 16 MiB inbound body cap where the publisher proxy is described,
  the `response_headers` rule from D8, and the Axum ingress variables.

## Adapter impact

| Adapter    | Compile changes                                                                                           | Runtime behavior changes                                                                                                                          |
| ---------- | --------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| Fastly     | About 30 sites: accessors, `into_request`, `from_external_stream`, lifecycle `Result`, `configure` result | The [Fastly differences](#fastly-differences) only                                                                                                |
| Axum       | 7 sites, the `ingress` config field, plus the route-test and parity harness port                          | All of section 7; single-threaded serving; connection and parser caps (env-tunable); fixed target limit; 16 MiB inbound cap; no JSON special case |
| Cloudflare | 19 sites: accessors, `into_request`, `ConfigValue`                                                        | All of section 7; 16 MiB inbound cap; unsized wire framing for buffered bodies; origin `Trailer` stripped                                         |
| Spin       | 31 sites: accessors, `into_request`, `ConfigValue`, `get_bounded`, `SpinConfigStore::for_label`           | All of section 7; 16 MiB inbound cap; outbound-host manifest check; config open failure surfaces at first read                                    |

Core has 9 sites (`from_stream`), and the CLI compiles unchanged against F with
all of its tests passing.

### Fastly differences

These are the only intended changes to Fastly responses:

- Request bodies over 16 MiB get 413 (section 2).
- `TRACE` and `CONNECT` get 405 with an `Allow` header, and internal error
  messages rendered by EdgeZero are redacted (section 6).
- An unopenable config store returns the startup-error 500 instead of a
  plain-text 500 (section 5).
- `Trailer` and `Content-Length` entries in operator `response_headers` or in
  integration response headers are skipped with a warning (D8).

## Configuration

**No new Trusted Server configuration keys and no changed defaults.**

**Inbound body cap.** `MAX_INBOUND_REQUEST_BODY_BYTES` is a 16 MiB constant, not
configuration. It applies on every adapter. The rationale is in section 2.

**Response header rule ([D8](#decisions)).** One core predicate names the
response fields Trusted Server never copies from configuration or third parties:
`trailer` and `content-length`. Those are the two that EdgeZero framing turns
into 500s; F strips the other hop-by-hop fields silently.

- `validate_settings_for_deploy` (`crates/trusted-server-core/src/config.rs:240-255`)
  rejects an operator `response_headers` entry that names either field, so
  `ts config validate` and `ts config push` fail with the header name. The check
  goes there, not in the `response_headers` loop in `Settings::prepare_runtime`
  (`crates/trusted-server-core/src/settings.rs:3156-3167`), because that loop
  also runs on every runtime settings load.
- At runtime, `crates/trusted-server-core/src/response_privacy.rs:167`, which
  applies operator `response_headers`, and
  `crates/trusted-server-core/src/integrations/registry.rs:520`, which applies
  integration response-header mutations, skip those fields and log a warning.
  A live config that already contains one keeps loading. DataDome forwards
  response headers named by a third-party server
  (`crates/trusted-server-core/src/integrations/datadome/protection.rs:669-690`),
  so this is the only guard on that path.

**Axum ingress variables ([D9](#decisions)).** The Axum dev server reads
EdgeZero's ingress variables. Values must be nonzero digits, and an invalid
value fails startup
(`edgezero:crates/edgezero-adapter-axum/src/ingress_config.rs:28-60`, `:186-200`).

| Variable                                           | Default |
| -------------------------------------------------- | ------- |
| `EDGEZERO__ADAPTER__INGRESS__MAX_CONNECTIONS`      | 256     |
| `EDGEZERO__ADAPTER__INGRESS__MAX_RAW_HEAD_BYTES`   | 65536   |
| `EDGEZERO__ADAPTER__INGRESS__MAX_HEADER_COUNT`     | 100     |
| `EDGEZERO__ADAPTER__INGRESS__HEAD_READ_TIMEOUT_MS` | 10000   |

**Unchanged for operators.** No re-push, store relink, deploy flag, or secret
move. The envelope version, canonical hash bytes, Fastly chunk pointer format,
store links, and secret layout are unchanged.

## Consent and privacy

Consent extraction and enforcement, EC generation and cookies, and what Trusted
Server sends to bidders are unchanged. Specific effects:

- Framework error responses on Axum, Cloudflare, and Spin skip Trusted Server
  middleware. They never set cookies or EC identifiers, and EdgeZero renders
  category-only messages, so no URL, credential, or provider detail reaches the
  client.
- Requests that fail head limits are rejected before consent processing. They
  fail closed, with no partial handling.
- The inbound body helper never logs body bytes.
- Outbound requests stay on Trusted Server's clients, so browsers see no new
  `x-edgezero-proxy` header, and bidder requests carry the same headers as
  today.

## Failure handling

| Failure                                                      | Behavior                                                                          |
| ------------------------------------------------------------ | --------------------------------------------------------------------------------- |
| Inbound body over 16 MiB (declared or streamed)              | 413 from `TrustedServerError::RequestTooLarge`; logged at `warn`                  |
| Inbound read deadline (Axum, Cloudflare, Spin; 30 s)         | 408 from the new `TrustedServerError::RequestTimeout`; logged at `warn`           |
| Other inbound read error                                     | 400 from `TrustedServerError::BadRequest`                                         |
| Fastly `configure` returns `Err`                             | Existing state-build failure path                                                 |
| Fastly config store cannot open                              | Startup-error router 500, one build attempt per request                           |
| Fastly post-commit delivery error                            | Logged; callback still returns `Ok`, so the sandbox keeps serving as today        |
| Invalid `EDGEZERO__ADAPTER__INGRESS__*` value on Axum        | Startup fails with the parse error                                                |
| Spin config store cannot open                                | Existing "failed to read Spin Trusted Server app-config blob" error at first read |
| Framing violation on Axum, Cloudflare, Spin                  | EdgeZero 500 fallback; unreachable from Trusted Server sources after D7 and D8    |
| Operator `response_headers` names `trailer`/`content-length` | Rejected by `ts config validate`/`push`; skipped with a warning at runtime        |
| Integration response header names `trailer`/`content-length` | Skipped with a warning                                                            |
| Spin manifest outbound hosts disagree with `spin.toml`       | `ts build`, `serve`, `deploy --adapter spin` fail with "Spin outbound host drift" |
| Spin runtime older than 4.1, or the wasip1 component fails   | Component fails to instantiate; caught by `scripts/smoke-spin.sh`; blocks the cut |

## Compatibility and rollout

**Hard cutoff.** One pull request moves all six crates and every adapter. No
feature flag, no dual pin, no shim. In-flight branches on older EdgeZero pins
rebase onto the new APIs. Nothing keeps the old ones alive for them.

**Before deploy.** Run the new `ts config validate` against every environment's
live config. A failure names a `response_headers` entry to remove. Runtime keeps
working either way, because the runtime path skips the field instead of failing.

**Rollback.** Revert the pull request and redeploy the previous build. Config,
stores, and secrets are unchanged, so rollback needs no operator action.

**Operator-visible.** Viceroy 0.21.0, Fastly CLI 16.0.0, and Spin 4.1 arrive
with #1123. The 16 MiB inbound body cap is new on every adapter. The
template-cache build digest hashes `Cargo.lock` and `edgezero.toml`
(`crates/trusted-server-core/build.rs:26`), so the first deploy after the cut
misses every shared template-cache entry once.

**Pin lifecycle.** F is a snapshot of a draft PR with a standing change request.
Expect more commits and a squash merge, after which F is not an ancestor of the
merged commit (the `v0.0.9` release followed the same pattern). F stays
reachable only through the PR branch and `refs/pull/275/head`. A force-push or a
further commit moves both, and an unreachable commit can be garbage-collected,
which would break `--locked` builds. Repinning to the merge commit or a release
tag re-diffs the public API between F and the new rev. How the cut gates on PR
275's merge is open question Q1.

**In-flight Trusted Server work.**

| Work                                                   | EdgeZero dependency                                                   | What it must do after the cut                                                                                            |
| ------------------------------------------------------ | --------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------ |
| #1107, #1251, rc/202610 (#1228), #1310                 | EdgeZero PR #403 APIs (`RequestIngress`, `PreDispatchHook`), not in F | Wait for #403 rebased onto PR 275, or redesign on F's admission policy (sync, read-only) plus routed middleware. See Q4. |
| #1261                                                  | EdgeZero PR #389 (request timing), not in F                           | Same, against #389                                                                                                       |
| #1342 to #1409 (integration-seam stack), #1290, #839   | `v0.0.9` or `v0.0.8` APIs                                             | Rebase and port to the new APIs                                                                                          |
| Other open PRs with older pins and no EdgeZero API use | none                                                                  | Rebase only                                                                                                              |

#1107, #1251, #1261, #1290, #1310, #839, and rc/202610 use the old
`stackpop/edgezero` git URL. Cargo treats a different URL as a different source,
so any merge that mixes URLs produces duplicate EdgeZero crates even at the same
rev. Each must switch to the `IABTechLab/edgezero` URL when it rebases.

## Verification

### Static

Every CI gate in `AGENTS.md`, plus `--locked` builds after the lockfile review
in section 1.

### New tests

1. **Real ingress, integration suite.** In
   `crates/trusted-server-integration-tests/tests/integration.rs`, which CI runs
   with `--include-ignored` against Viceroy, workerd, and the Axum binary: add
   `POST /auction` with a JSON body on Fastly and Axum (workerd already has
   one), and a publisher `POST` with a body on Fastly, Cloudflare, and Axum that
   reaches the origin stub byte for byte.
2. **Spin smoke.** `scripts/smoke-spin.sh` gains a `POST /auction` case and a
   publisher `POST` case. Today it issues only a publisher `GET`
   (`scripts/smoke-common.sh:114-129`).
3. **Handler-level streamed bodies.** Per adapter, `RouterService::oneshot` over
   `routes_with_settings` with `Body::Stream` requests: `POST /auction`, a
   publisher `POST`, and `/first-party/sign`. These are red without the helper
   on any pin, because the old buffering lived in EdgeZero's ingress, which
   `oneshot` skips.
4. **Inbound cap and errors.** A declared `Content-Length` over 16 MiB returns
   413 without polling the body. A chunked body that crosses 16 MiB returns 413.
   A stream item carrying an EdgeZero request timeout returns 408, and any other
   stream error returns 400.
5. **Response header rule.** `validate_settings_for_deploy` rejects
   `response_headers` entries for `trailer` and `content-length`. The runtime
   paths skip both from operator config and from an integration mutation, and
   log a warning. The Cloudflare outbound client strips an origin `Trailer`.
6. **Bodiless framing on the canonical adapters.** Through the Axum binary and
   workerd, a publisher `HEAD` and a conditional `GET` returning 304, from an
   origin that sends `Trailer`, return their real status, not the 500 fallback.
7. **Head limits.** Through workerd and the Axum binary, a signed
   `/first-party/proxy` URL wrapping a 4 KiB creative URL, sent with 40 headers
   of 1 KiB each, is admitted. For Spin, the same request passes
   `edgezero_core::ingress::validate_normalized_ingress_parts` with the app's
   `ingress_head_limits()`, using the absolute URL as the target.
8. **Axum ingress variables.** A variable raises its limit, and an invalid value
   fails startup.
9. **Disallowed methods.** `cargo clippy-*` fails when adapter code calls
   `RequestContext::into_request` or `take_body` outside the helper. Prove it
   once with a scratch call, not a committed test.

Ported tests: the Axum route tests and the Axum parity leg (section 6), the
helper tests from section 3, and every test touched by the accessor and
`from_external_stream` changes.

### Runtime

- `cargo test-fastly` and `cargo test-fastly-reuse` on Viceroy 0.21.0.
- `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml -- --include-ignored`
  as CI runs it, which covers `integration` and `parity`.
- `./scripts/integration-tests-browser.sh`.
- `scripts/smoke-spin.sh` on Spin 4.1, which runs `spin up` on the
  `wasm32-wasip1` artifact. CI does not install the Spin CLI, so this is a
  manual pre-merge gate, and the pull request description records its output.
  It is the only check that catches a component and host mismatch.
- `scripts/smoke-cloudflare.sh`. It needs the Wrangler pin that is missing from
  `.tool-versions` today; add it if the smoke cannot run otherwise.
- **Fastly response diff.** A shell script under `scripts/` sends a fixed
  request list to a #1123 build and to the branch build, both under Viceroy, and
  diffs status, headers, and a body hash. The list covers a publisher page,
  `HEAD` and a conditional `GET` on a page, `POST /auction`, a first-party proxy
  URL, a tsjs bundle, an admin endpoint, and a `TRACE` request. It normalizes
  `X-TS-Version`, the sandbox counter headers, dates, and generated IDs. The
  expected differences are exactly the [Fastly differences](#fastly-differences)
  the list exercises: `Allow` on the `TRACE` 405. Reviewers can rerun it.
- A Fastly staging deploy before production.

## Commit sequence

1. On the #1123 base, add integration-suite cases from new test 1. They pass on
   `v0.0.10`, which buffers bodies in EdgeZero, and record the baseline.
2. Repin to F, update and review the lockfile, add `[capabilities.outbound]`,
   make the mechanical compile fixes (sections 3 through 6) with a plain
   `into_request()?`, and port the Axum route tests and the parity Axum leg to
   `oneshot`. Every crate and test target compiles. The step 1 cases and the
   existing publisher-page and workerd auction tests are red.
3. Add `into_buffered_request`, `TrustedServerError::RequestTimeout`, the
   `clippy.toml` guard, and new tests 3 and 4. Everything from step 2 is green.
4. Add the D7 Cloudflare hop-by-hop change and the D8 response header rule, with
   new tests 5 and 6.
5. Add the Spin smoke cases (new test 2), the head-limit tests (new test 7), and
   the Axum ingress variables (new test 8).
6. Delete the Fastly config-store handle.
7. Update documentation and add the response-diff script.

## Follow-up specs

- **Phase 2: adopt EdgeZero outbound HTTP.** Replace `PlatformHttpClient`,
  `PlatformBackend`, and backend naming with `HttpClient` and
  `start_batch_until`. Upstream asks first: a decoding opt-out, a stable Fastly
  backend namespace or budget bucketing for connection reuse, a late-completion
  grace policy, a certificate-check override, surrogate-key cache intent, an
  Image Optimizer hook, an opt-out for `x-edgezero-proxy`, a Spin host-authority
  override, and streamed responses in batches so the origin fetch can overlap
  the auction.
- **Phase 3: Fastly on the canonical lifecycle.** Move to
  `send_request_with_registries_and_hooks` once EdgeZero runs that composition
  under Viceroy end to end. That brings Fastly's admission and framing in line
  with the other adapters.
- **Repin to the merged PR 275** commit or release tag, with an API re-diff.
- **EdgeZero asks.** A public Axum test harness and a dev-server constructor
  that takes an `App`, so head limits and `Hooks::configure` apply on Axum.
- **Smaller items.** Retained app instances on Cloudflare and Spin, real loggers
  on both, an error-response renderer that restores Trusted Server headers on
  framework errors, Dependabot ignore rules for EdgeZero-pinned crates, a Spin
  CI job, and the Wrangler pin.

## Open questions

- **Q1. Merge gating.** Does the cut merge to `main` while PR 275 is still open,
  pinned to F, or wait for PR 275 to merge and pin the merge commit? The
  recommendation is to develop and review against F, and merge only after PR 275
  merges, with the pin moved to the merged rev. A `main` pinned to a draft-PR
  commit breaks `--locked` builds once that commit becomes unreachable.
- **Q2. Outbound scope.** Keep Trusted Server's outbound layer in this cut
  (recommended, section 9), or adopt EdgeZero's outbound and batch API now
  despite the auction and proxy behavior changes listed there?
- **Q3. Fastly lifecycle.** Keep Fastly's custom entry point (recommended,
  section 6), or move Fastly onto EdgeZero's canonical lifecycle in the same
  cut?
- **Q4. Ordering against #1107 and rc/202610.** Does the cut land before #1107,
  which then waits for EdgeZero #403 rebased onto PR 275, or after #1107 lands
  on an EdgeZero rev that contains both? The answer also decides whether the
  October release candidate can carry the cut.

## Expected files

- `Cargo.toml`, `Cargo.lock`
- `edgezero.toml`
- `clippy.toml`
- `.tool-versions` (Wrangler pin, only if the Cloudflare smoke needs it)
- `crates/trusted-server-core/src/http_util.rs` (inbound body helper and
  constant), `error.rs` (`RequestTimeout`), `config.rs`, `response_privacy.rs`,
  `integrations/registry.rs`, `auth.rs`, `publisher.rs`, `proxy.rs`
- `crates/trusted-server-adapter-fastly/src/main.rs`, `app.rs`, `middleware.rs`,
  `platform.rs`
- `crates/trusted-server-adapter-axum/src/main.rs`, `app.rs`, `middleware.rs`,
  `platform.rs`, `tests/routes.rs`
- `crates/trusted-server-adapter-cloudflare/src/app.rs`, `middleware.rs`,
  `platform.rs`
- `crates/trusted-server-adapter-spin/src/app.rs`, `middleware.rs`,
  `platform.rs`
- `crates/trusted-server-integration-tests/tests/parity.rs`, `integration.rs`,
  and environment helpers
- `scripts/smoke-spin.sh` or `scripts/smoke-common.sh`, and the new
  response-diff script
- `docs/guide/cli.md` and the guide pages named in section 10

## Review log

- Builder check (Claude Code, Opus): asked how the cut orders against #1107 and
  rc/202610 (kept as Q4), whether Q2 and Q3 are settled (kept open), whether the
  `response_headers` rule rejects at runtime (now deploy-time rejection with a
  runtime skip, extended to integration headers after it pointed at DataDome's
  forwarded headers), how merging pinned to F works (Q1 now states the
  reachability risk), and what a failed `spin up` means (D11).
- Round 1 (Claude Code, Opus, fresh session, verified against both trees and a
  scratch compile): four blocking findings, all fixed. `App::build` cannot inject
  test settings and `App::dispatch_ingress` skips head limits and framing, so D10
  now uses `oneshot` for every parity leg and real ingress moved to the
  integration suite. The commit sequence had a red "baseline" and a step that
  could not compile; it was reordered. Runtime rejection of `response_headers`
  could take a live service to 500; the rule is now narrowed to `trailer` and
  `content-length`, rejected at deploy and skipped at runtime. Should-fix items
  adopted: the 408 variant and error mapping, the Axum fixed target limit, the
  Configuration section for the cap and Axum variables, dropping the
  Fastly-only `make_response_bodiless` change, the Fastly error-rendering and
  config-store failure-mode differences, the response-diff baseline and
  normalization, the section 3 cascade, the narrowed CI blind-spot claim, and a
  single parity seam. Nits adopted: lockfile entries, the full ceiling list, SDK
  attribution to #1123, resolved `Display` conditional, `take_body` in the guard,
  `warn` logging, the wider `stackpop` URL list, and a concrete head-limit test.
