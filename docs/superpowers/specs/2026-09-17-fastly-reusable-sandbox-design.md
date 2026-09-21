# Fastly reusable sandbox adoption

Issue: [#856](https://github.com/IABTechLab/trusted-server/issues/856)

## Problem

Every Fastly Compute request currently starts a fresh Wasm sandbox, so the
entry point repeats the whole initialization sequence: read the runtime env
config store, open the application config store, load and parse settings,
resolve secrets, compile the auction plan, build the orchestrator, build the
integration registry, and construct the telemetry sink. The `OnceLock` regex
caches in `settings.rs` are initialized and discarded within a single request.

Fastly SDK 0.12.1 exposes `fastly::http::serve::Serve`, which lets one sandbox
handle several requests. This design adopts it so that initialization can be
amortized, while keeping reuse opt-in and leaving the default deployment
behaviour unchanged.

## Scope

In scope: the Fastly adapter entry point and the state it owns, plus one
`trusted-server-core` change — moving the script rewriters' accumulation
buffers out of registry-lifetime objects, without which retention is unsafe.
See [Retained rewrite buffers](#retained-rewrite-buffers-blocking).

Out of scope: Spin, Cloudflare, and Axum adapters; any other change to routing,
auction, EC, or integration behaviour. Memory-bounded retirement is included
following review; it is a between-request SDK check, not an allocation ceiling.

## Current entry point

The adapter does not use `#[fastly::main]`. It owns its own `main`, converts
the raw request itself, dispatches straight into the router, and sends the
response explicitly. That shape is load-bearing and must survive the change.

| Concern                                               | Location                                                          |
| ----------------------------------------------------- | ----------------------------------------------------------------- |
| Raw `FastlyRequest::from_client()`                    | `main.rs:78-89`                                                   |
| Health probe ahead of all initialization              | `main.rs:65-72`, called at `main.rs:81`                           |
| Global logger install                                 | `main.rs:87` → `logging.rs:82-106`                                |
| Runtime env read                                      | `main.rs:93-94`                                                   |
| Native config-store handle → request extension        | opened `main.rs:58-63`, `main.rs:117-127`; inserted `main.rs:178` |
| Application build                                     | `main.rs:129` → `app.rs:1278-1297`                                |
| Direct router dispatch (keeps duplicate `Set-Cookie`) | `main.rs:170-180`                                                 |
| Progressive streaming via `stream_to_client`          | `main.rs:338-368`                                                 |
| Synchronous post-send pull sync                       | `main.rs:450-466`                                                 |

Two properties of this path block naive reuse.

1. `logging.rs:105` ends in `.apply().expect("should initialize logger")`.
   A second call panics, because a global logger is already installed.
2. `app.rs:1288-1296` returns `startup_error_router` with `state: None` when
   settings fail to load. Retaining that result would pin a sandbox into
   permanent error mode for every subsequent request it serves.

## Dependencies

The workspace pins EdgeZero at `c4841b609ec366ebabd3489416e2cb8c1359f61d` on
`feat/reusable-app-lifecycle`. (It briefly sat at `277544c4` on the same
branch; that revision carried the CLI and Cloudflare fixes but no lifecycle
module.)

This includes `edgezero_adapter_fastly::lifecycle`, introduced at `76c59b44`, which this adapter
now uses instead of its own equivalents. The framework owns lazy
successful-only retention, the callback count, the initialization-attempt
count, the one-time setup guard, and the serving wrappers. The sections below
describe the design as originally implemented; where they name a local
mechanism such as `logger_installed`, `resolve_app` or a hand-rolled
`Serve::run_with_context`, that mechanism has since been replaced by
`Sandbox::setup_once`, `Sandbox::initialize`, and
`lifecycle::serve_custom` / `run_custom`. What stays application-owned is
unchanged.

The pin is **not** for the `Serve` re-export: that type comes from the
already-pinned `fastly 0.12.1` SDK, and EdgeZero's own contract says not to
repin merely to swap the import. It is for the CLI fixes at that revision
(push/diff validation scoped to the selected adapter, and secret-reference
redaction in Spin diagnostics) and the Cloudflare duplicate-header fix. Every
`edgezero-core` and `edgezero-core::router` change across the range is test
only, so no runtime behaviour we depend on moved.

`Serve`, `ServeSummary`, and `HandlerResult` all come from `fastly 0.12.1`,
which is already resolved in `Cargo.lock`. EdgeZero PR #379 re-exports only
`Serve` and `ServeSummary` (not `HandlerResult`) and adds `serve_app` /
`serve_app_with_request_extensions`.

Those helpers are unusable here, but not because of header handling: EdgeZero's
`to_fastly_response` uses `append_header`
(`crates/edgezero-adapter-fastly/src/response.rs`), so duplicate `Set-Cookie`
values survive that conversion. The disqualifying part is that the same
function drains `Body::Stream` into a buffered `fastly::Body` before sending,
which ends progressive streaming, and that the helper path leaves no place for
the response-extension finalization and post-send work this adapter performs.
That holds regardless of whether PR #379 merges, so this work takes no
dependency on it.

`HandlerResult` is implemented for `()`, for handlers that have already called
`send_to_client` or `stream_to_client`. That is exactly the current handler
shape, so the existing send logic is reused unchanged.

## Design

### Three execution modes

Compatibility is layered, and the layers are not equivalent. Only the first
reproduces today's execution.

```text
feature off                              → original entry path
feature on, effective max_requests <= 1  → single-request handler path
feature on, validated reuse limits       → Serve loop
```

The feature-on single-request path is **not** byte-identical to the feature-off
path: it performs the startup mode lookup, which opens a config store and reads
four keys before the first request. It does not construct a `Serve` or enter
its loop — it calls the handler once directly, as the code below shows. It is
described as _single-request operation_, not as identical execution.

The startup mode lookup must not be able to break the health probe. If the
lookup fails for any reason, the adapter falls back to single-request operation
and the health probe continues to answer.

### Commit 1 — lifecycle, no retention

Add a `reusable-sandbox` Cargo feature to `trusted-server-adapter-fastly`, off
by default. `fastly.toml`'s build command does not pass it, so production
builds are unaffected.

Split `main` into a loop owner and a handler:

```rust
fn main() {
    let mut sandbox = Sandbox::default();
    match serve_mode() {
        ServeMode::Single => { handle_request(FastlyRequest::from_client(), &mut sandbox); }
        ServeMode::Reuse(serve) => { let _ = serve.run_with_context(handle_request, &mut sandbox); }
    }
}

fn handle_request(req: FastlyRequest, sandbox: &mut Sandbox) {
    // today's `main` body: health probe, logger, edgezero_main
}
```

`handle_request` returns `()`. It keeps sending its own response, so streaming,
duplicate `Set-Cookie`, response extensions, and post-send pull sync are
untouched.

In this commit `Sandbox` holds no application state. It holds:

- `logger_installed: bool`, which fixes the `logging.rs:105` double-`apply()`
  panic.
- The measurement bookkeeping from
  [Ownership of the measurement surface](#ownership-of-the-measurement-surface):
  the validated guest-instance identifier, the request ordinal within that
  instance, and the application build counter.

The build counter lives here from commit 1 even though nothing increments it
past one until commit 3 — that is what makes arm B's "one build per request"
baseline measurable rather than assumed.

Counters are attached to the response **before headers are committed**. On the
streaming path that is before `stream_to_client`, which means they describe the
request up to commitment and cannot report its eventual outcome. A failure
after commitment is therefore recorded separately, in logs, and is reconciled
with the counters during analysis rather than being expected to appear in
them.

Every non-panicking path through `handle_request` must send exactly once. This
is already true of the current code (each early return sends before returning),
but it becomes a correctness requirement of the loop rather than an incidental
property of a process that is about to exit, so it is asserted by test.

The loop owner introduces one new failure mode. `run_with_context` panics
(`serve.rs:320`) if `RequestPromise::new` fails mid-loop, which cannot happen in
today's one-shot `main`. This is accepted rather than worked around: it fires
only between requests, after the current response has been sent, and the SDK
treats it as a sandbox failure — the same outcome as any other guest panic. It
is recorded here so it is not mistaken for an application fault during
validation.

### Commit 2 — per-document rewrite buffers

A `trusted-server-core` prerequisite for retention, specified under
[Retained rewrite buffers](#retained-rewrite-buffers-blocking). No adapter
change and no behavioural change under today's one-request-per-sandbox model.

### Commit 3 — retention

`Sandbox` gains `app: Option<RetainedApp>`, where `RetainedApp` owns the built
`App` and its `Arc<AppState>`. It is constructed lazily on the first request
that is not one of the three pre-build short-circuits — the health probe, the
JA4 debug probe, and the counters endpoint from
[Ownership of the measurement surface](#ownership-of-the-measurement-surface) —
so none of those paths pays for construction.

A failed build is never stored. `router_with_state` already returns
`(startup_error_router, None)` on failure; that result serves the current
request and is dropped, so a transient config-store failure cannot poison the
sandbox.

### Ownership

| Retained in `Sandbox`                                                                                                 | Rebuilt every request                                               |
| --------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------- |
| `logger_installed`                                                                                                    | `ConfigStoreHandle` (native handle)                                 |
| `App` + `Arc<AppState>`: settings, auction plan, orchestrator, integration registry, telemetry sink, default KV store | `EnvConfig` / `RuntimeStoreConfig`                                  |
|                                                                                                                       | `ClientInfo`, `DeviceSignals`, TLS/JA4 metadata, resolved client IP |
|                                                                                                                       | `RuntimeServices` (`app.rs:285`)                                    |
|                                                                                                                       | Correlation id from `get_client_request_id()`                       |
|                                                                                                                       | `EcFinalizeState`, `RequestFilterEffects`, auth results, bodies     |

Native store handles are never retained alongside the app.

Correlation is request-local. `get_client_request_id()` supplies it;
`FASTLY_TRACE_ID` identifies the sandbox, not the request, and is never used as
a request id. Correlation values are never written into app state or into
global logger configuration.

### Nested-state audit

Retaining `AppState` is only safe if everything reachable from it is
config-derived and free of request state and native handles.

- `AuctionOrchestrator` (`orchestrator.rs:307-317`): `enabled`, `plan_backed`,
  `plan`, `planned_providers`, `mediator`. All config-derived. The per-auction
  `PlannedLaunchState` is a local, not a field.
- `IntegrationRegistry` (`registry.rs:780-783`): `Arc<IntegrationRegistryInner>`
  plus an optional plan. **Not fully config-derived — see
  [Retained rewrite buffers](#retained-rewrite-buffers-blocking).**
- `FastlyTinybirdAuctionTelemetrySink` (`tinybird.rs:36-48`): owned strings and
  a backend spec. No handles.
- `default_kv_store` is `UnavailableKvStore`, inert.

Two process-wide statics outlive a request for the first time under reuse.
Neither is reachable from `AppState`, but both change behaviour:

- `IP_CIDR_SOURCE_CACHE` (`protection_scope.rs:200`), covered under
  [Behavioural tests](#behavioural-tests).
- `MISSING_GEO_WARNING_LOGGED` (`consent/mod.rs:64`), an `AtomicBool` that
  becomes log-once-per-sandbox instead of log-once-per-request. Benign and
  arguably the intent, but recorded here so a reduced warning count during
  validation is not mistaken for a dropped warning.

A sweep of the core and adapter crates for `static` interior mutability finds
only these two; everything else is immutable `LazyLock` regexes and sets.

### Retained rewrite buffers (blocking)

The registry is **not** safe to retain as it stands. Two script rewriters hold
request content in interior-mutable state on the rewriter object itself:

- `GoogleTagManagerIntegration.accumulated_text: Mutex<String>`
  (`google_tag_manager.rs:366`, used at `:1031`)
- `NextJsNextDataRewriter.accumulated_text: Mutex<String>`
  (`nextjs/script_rewriter.rs:24`, used at `:77`)

Both accumulate fragments of an inline `<script>` across `lol_html` callbacks
and drain only via `std::mem::take` when `ctx.is_last_in_text_node` arrives.
The registry stores them as `Arc<dyn IntegrationScriptRewriter>`
(`registry.rs:710`), registered once at build time (`registry.rs:644`), so a
retained registry shares one buffer across every request the sandbox serves.

If a document stream ends before the final text fragment — client disconnect,
origin error, truncated body — the buffer keeps one request's partial script
content. The next request through the same sandbox prepends that residue to its
own accumulation, which can corrupt its response or disclose the previous
request's content. Today each sandbox serves one request, so the buffer is
destroyed before it can be observed; retention is what makes it reachable.

**This lands as its own commit, between the lifecycle and retention commits.**
It is a `trusted-server-core` change with its own regression test, it is
correct and worth having independently of sandbox reuse, and isolating it keeps
the retention commit revertable without also reverting a core fix. The
accumulation
belongs to a document, not to a registry-lifetime object. `IntegrationDocumentState`
(`html_processor.rs:57`, constructed per document at `html_processor.rs:302`)
already exists for exactly this purpose and is the intended home; the
alternative is constructing fresh stateful rewriters per document. Either way
the rewriter objects in the registry must become stateless.

This expands scope beyond adapter-owned state into `trusted-server-core`. It is
accepted deliberately: retention is unsafe without it.

Regression test: interrupt one HTML stream mid-text-node, then process a second
document through the same retained app, and assert the second document's output
contains no fragment of the first.

### What retention does not amortize

Retaining `AppState` does not remove signing-key parsing.
`orchestrator.rs:400` calls `RequestSigner::from_services(context.services)`,
and `context.services` is the per-request `RuntimeServices` built at
`app.rs:285`. The Ed25519 parse at `signing.rs:151` therefore runs per auction
per request, inside the retained orchestrator, reading the secret store through
request-scoped services. It is unaffected by this change and must be measured
separately rather than assumed away.

The same caution applies to any other construction reached from a handler
rather than from `build_state`. The measurement plan counts builds directly
instead of inferring them.

### Freshness and rotation

There is no in-sandbox refresh in v1. Retained settings carry resolved secrets,
so their age is bounded only by how long the sandbox lives.

Sandbox limits reduce snapshot age but **do not enforce a strict freshness
deadline**. The SDK checks `max_lifetime` after a callback returns and before it
waits for the next request; it does not recheck after that wait resolves. A
request that arrives after a long wait is served with state older than the
configured lifetime, and a long-running request can continue past expiration.
Request-count limits bound the number of requests served from one snapshot;
they do not bound elapsed age at all.

If a strict maximum age is later required, it must be implemented in this
repository by checking snapshot age before dispatching each request and
rebuilding expired state — rejecting the request if the rebuild fails. That is
deliberately not in v1.

### Configuration

The feature gates the code; the config store tunes the bounds. Keys are read
once at startup from the existing `edgezero_runtime_env` store, before
`Request::from_client()`.

```text
EDGEZERO__SERVICES__<sid>__TS__SANDBOX__MAX_REQUESTS
EDGEZERO__SERVICES__<sid>__TS__SANDBOX__MAX_LIFETIME_MS
EDGEZERO__SERVICES__<sid>__TS__SANDBOX__TIMEOUT_MS
EDGEZERO__SERVICES__<sid>__TS__SANDBOX__MAX_MEMORY_MIB
```

**These keys cannot be read through `runtime_env_config`.** That function
resolves a closed allowlist built by `runtime_env_keys`
(`crates/edgezero-adapter-fastly/src/lib.rs`, still closed at the pinned
revision): `EDGEZERO__ADAPTER__HOST`
and `__PORT`, four `EDGEZERO__LOGGING__*` keys, and per-store
`EDGEZERO__STORES__{CONFIG,KV,SECRETS}__<ID>__NAME` / `__KEY`. Every other key
is dropped, so an `EnvConfig` lookup for a sandbox key always returns `None`.
Routing these keys through the existing call at `main.rs:93` would therefore
resolve to nothing, silently fall back to single-request operation on every
path, and pass every test listed below while never enabling reuse.

The adapter opens the store itself instead:

- Open `edgezero_adapter_fastly::RUNTIME_ENV_STORE_NAME` (a public const,
  in `edgezero-adapter-fastly`) directly with `fastly::ConfigStore::try_open`.
- Build the scoped key from `fastly::compute_runtime::service_id()`, because
  edgezero's `service_scoped_runtime_env_key` is private, at the pinned
  revision as well as at `v0.0.8`.
- If the store is absent, or the open fails, or `service_id()` returns an empty
  string: log once and use single-request operation. (`service_id()` returns
  `&'static str` and cannot itself fail, so empty is the only reachable
  degenerate case.)

Local runs must tolerate Viceroy reporting a service id of twenty-two zeros.
That is a valid id for key construction, so local A/B/C fixtures write their
keys under that scope rather than assuming a deployed service id.

Resolution rules:

- Missing, malformed, unavailable, or overflowing values fall back to
  single-request operation.
- An application-level value of `0` normalizes to `1`. This matters because the
  SDK reads `with_max_requests(0)` as _unlimited_; the application value is
  never passed through unmapped.
- Reuse is enabled only when a finite request limit is configured **and** finite
  lifetime, wait-timeout, and memory values are supplied. Memory is in MiB and
  must be between `1` and `u32::MAX - 1`, inclusive; `0` is unlimited in the SDK,
  and `u32::MAX` is its unsupported-snapshot sentinel. Omitted SDK limits default
  to effectively unbounded (`Duration::MAX` or zero), which is not an acceptable
  production posture, so a partial configuration resolves to single-request
  operation rather than to an unbounded loop.

Existing three-key configurations must add `MAX_MEMORY_MIB` before reuse can
engage. `Serve::with_max_memory` checks heap usage after each callback and
retires the sandbox before accepting another request if usage exceeds the
bound. It cannot prevent the current request from exceeding the bound. An
unsupported heap snapshot conservatively retires the sandbox; deployed snapshot
support and memory retirement remain unverified.

No default limit values are invented here. An operator who enables the feature
without setting keys gets single-request operation.

### Rollback

```text
config key -> 1   ends reuse for sandboxes that load the new value
feature off       redeploy; restores the original entry path
revert commit 3   removes retention, keeps the lifecycle and the core fix
revert commit 2   removes the per-document buffers (only safe with 3 reverted)
revert commit 1   removes the lifecycle entirely
```

No rung of this ladder is immediate. Limits are read at sandbox startup, so a
sandbox already in its loop never observes a changed key; it retires on the
limits it started with, and only sandboxes created after the push see the new
value. A redeploy is not instantaneous either — it must build, publish, and
roll out, and sandboxes running the previous version drain on their own limits
meanwhile.

Rollback applies as configuration or deployment changes propagate and existing
sandboxes retire. Configured limits encourage retirement but do not establish a
strict wall-clock rollback deadline: as
[Freshness and rotation](#freshness-and-rotation) sets out, `max_lifetime` is
checked only between callbacks, so it interrupts neither a sandbox waiting for
its next request nor a handler already running.

## Validation

### Build and test matrix

`clippy-fastly` passes `--all-features`, so the feature-on code is linted
already. `test-fastly` does **not** pass `--all-features`, and CI invokes that
alias directly (`.github/workflows/test.yml:60`), so feature-on tests would not
run. Clippy compiling the code is not the same as executing its tests.

The change therefore adds an explicit feature-enabled test invocation alongside
the existing feature-off run, and adds it to the CI gate list. Feature-off
coverage is retained, not replaced.

### A/B/C comparison

- **A** — current single-request behaviour, feature off.
- **B** — sandbox reuse with per-request initialization (commit 1).
- **C** — sandbox reuse with retained initialization (commit 3).

Arm A turns the feature off, which would also remove the instrumentation and
leave the baseline unmeasurable. The instrumentation is therefore enabled
independently of `reusable-sandbox`, by its own
`sandbox_metrics_enabled` settings flag, so all three arms report the same
fields through the same channel. The feature flag decides whether the _Serve
loop_ exists; the settings flag decides whether _counters are emitted_.

Workload headers are emitted only after terminal cache guards, and only when
`Cache-Control` contains both `private` and `no-store`. Cacheable routes retain
their existing policy and omit counters; the probe reports those observations
as unverified. Quoted extension values containing those words do not qualify.
This applies equally to feature-off and feature-on builds.

Quantities, and how each is obtained:

| Quantity             | Source                                                                     |
| -------------------- | -------------------------------------------------------------------------- |
| Requests per sandbox | Workload-request instance id + ordinal                                     |
| Builds per sandbox   | Workload-request build counter                                             |
| Latency              | Driver-side wall clock per request, reported as a distribution, not a mean |
| Supported CPU        | SDK observation where available, reported as an observation only           |
| Memory               | `heap_memory_snapshot_mib()` where the guest supports it                   |

Any of these that the runtime does not support is reported as **unverified**
rather than omitted or estimated.

Measured per request: application build count, a validated guest-instance
identifier, a monotonically increasing request ordinal within that instance, and
the request correlation id.

**These are attached to each workload request, not read from a separate probe.**
A counters request identifies the sandbox that served _the probe_, and
keep-alive does not establish that it is the sandbox that served the preceding
application request. Polling also burns request-limit budget and reloads
settings even though it skips app construction, so a probe-only design changes
the thing it measures. The counters therefore ride on the workload responses
themselves, and the endpoint remains only as an optional point-in-time snapshot.

Reuse is claimed only when several _workload_ requests report the same instance
id with an increasing ordinal. A single observation, or agreement between a
workload request and a later probe, does not count as observed reuse.

`FASTLY_TRACE_ID` is treated as sandbox metadata only; its availability and
uniqueness are verified in whichever runtime is actually used, and the fixture
carries its own identity fallback so a missing or repeated identifier fails the
measurement instead of silently passing.

#### Ownership of the measurement surface

Both pieces land in **commit 1**, because the arm-B measurement needs them
before retention exists.

The counters are exposed at `GET /_ts/debug/sandbox`, **gated twice**: compiled
only under the `reusable-sandbox` feature, and answering only when a settings
flag allows it, mirroring how `/_ts/debug/ja4` is gated at `main.rs:96-115`. It
is permanent surface under the feature, not scaffolding removed before merge, so
the same counters remain available for deployed validation. It exposes only the
four counters above — no settings, no secrets, no request content.

There is no general `settings.debug` boolean to reuse. `DebugConfig`
(`settings.rs:2506-2540`) has only `ja4_endpoint_enabled`,
`auction_html_comment`, `auction_html_comment_options`, and
`inject_adm_for_testing`. Reusing `ja4_endpoint_enabled` would be wrong
semantics — enabling a TLS probe to read sandbox counters — so the work adds a
new flag, `sandbox_metrics_enabled`, defaulting to `false`.

Adding it is constrained. `DebugConfig` is `#[serde(deny_unknown_fields)]`, and
the struct already documents that an older binary rejects a config blob carrying
an unknown field during a mixed-version deployment or rollback. The new flag
must therefore be skipped during serialization while false, the way
`auction_html_comment_options` already is, so a default blob stays readable by a
rolled-back binary. A blob with the flag explicitly enabled will require
restoring a compatible blob before rolling back, which is the same trade the
existing option table makes.

**The counters endpoint must short-circuit ahead of application construction**,
alongside the health and JA4 probes, resolving its gate through
`load_settings_from_config_store` rather than a built app. Otherwise polling it
would itself trigger the build it is trying to count, perturbing the
builds-per-sandbox number it reports.

The driver is a subcommand of the existing `ts` operator CLI in
`crates/trusted-server-cli`, not a second `[[bin]]` target, so it inherits that
crate's existing clippy and test coverage. It issues keep-alive request
sequences against a local `fastly compute serve` and collects the counters. It
is host-target only and is not part of the Wasm build.

Several behavioural tests below ("health probe never triggers an application
build", "a failed build is not retained") are observable only through these
counters, which is why they are commit-1 deliverables rather than validation
scaffolding.

### Behavioural tests

- Duplicate `Set-Cookie` preserved across a reused sandbox.
- Progressive streaming still streams on the second and later requests.
- Stream failure after response commitment logs and stops, with no second
  response attempt.
- Health probe never triggers an application build.
- JA4 probe never triggers an application build.
- The counters endpoint never triggers an application build, so polling it does
  not perturb the builds-per-sandbox figure it reports.
- The counters endpoint returns 404 when `sandbox_metrics_enabled` is false, and
  does not exist at all with the feature off.
- A default config blob still deserializes under a binary built without
  `sandbox_metrics_enabled`, confirming the `deny_unknown_fields` rollback path.
- A failed build is not retained; the following request retries construction.
- Request-scoped extensions do not leak between loop iterations.
- An HTML stream interrupted mid-text-node leaves no residue in the script
  rewriters: a second document processed through the same retained app contains
  no fragment of the first. Covers both `accumulated_text` buffers.
- Logger installs exactly once across a reused sandbox.
- Every non-panicking handler path sends exactly once.
- `IP_CIDR_SOURCE_CACHE` (`protection_scope.rs:200`) key space stays bounded by
  configuration. It is an unbounded `HashMap` with no eviction sweep, keyed by
  `{config_store, key}`; entries carry `expires_at` but are never removed. Under
  reuse it outlives a single request for the first time, so the test asserts the
  key space is config-derived and not traffic-derived.

### Evidence limits

These are stated in the results, not discovered afterwards.

- The installed Viceroy is 0.17.0, which admits roughly six requests per guest
  regardless of the configured SDK limit. Long-lived memory behaviour cannot be
  established locally, and raising the SDK limit does not change that.
- The runtime actually selected by `fastly compute serve` is recorded; it is not
  assumed to match standalone Viceroy.
- Local reuse results do not establish deployed eviction frequency, endpoint
  handle validity, resource accounting, or production latency.
- Named-endpoint log delivery is verified separately from echoed stdout.
- Local, injected, and deployed evidence are reported separately. No claim of
  guaranteed reuse is made: any request may start a fresh sandbox, so
  correctness must hold for a cold sandbox on every request.
- Supported CPU observations are reported as observations, not as cross-run
  benchmarks.
- Some fault modes have no supported local injection mechanism; those are listed
  as untested rather than as passing.

## Deferred

- Strict snapshot-age enforcement (see [Freshness and rotation](#freshness-and-rotation)).
- Amortizing the per-request Ed25519 signing-key parse, which needs its own
  measurement and its own rotation decision.
