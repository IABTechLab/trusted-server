# Cross-adapter health and startup-failure implementation plan

**Goal:** Give every adapter separate liveness and initialization-readiness
probes, with generic public startup-failure responses and observable diagnostics.

**Architecture:** A shared core pre-dispatch hook handles probes before router
lookup, authentication, response overrides, or trace capture. Healthy adapters
delegate other requests to their existing trace hook; failed adapters use the
same response builder for ordinary startup errors. Fastly keeps its native
liveness fast path and uses its existing initialization result for readiness.

**Tech stack:** Rust 2024, error-stack, log, EdgeZero `v0.0.11`, existing adapter
SDKs, Viceroy, Wrangler, Spin, shell smoke scripts, and Prettier.

**Specification:** [Health and startup-failure contract](../specs/2026-11-16-1142-health-startup-contract-design.md).
Baseline: `1810c763a`. Implemented and verified locally on
`fix/1142-health-startup-contract`. The execution record describes local
verification completed before publication, separately from hosted CI. Follow
repository signing and publication requirements for delivery.

## File responsibilities

| Files                                                                                 | Responsibility                                                                                        |
| ------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| New `crates/trusted-server-core/src/health.rs`; existing `src/lib.rs` in that crate   | Probe classification, fixed response policy, terminal response marker, and composed pre-dispatch hook |
| `crates/trusted-server-adapter-{axum,cloudflare,spin}/src/app.rs`                     | Supply startup outcome and wrap the current trace hook; use generic failure catchalls                 |
| `crates/trusted-server-adapter-{cloudflare,spin}/src/lib.rs`                          | Enable existing diagnostic sinks in production before startup                                         |
| `crates/trusted-server-adapter-fastly/src/{app,main}.rs`                              | Shared contract in router and native entrypoint; preserve initialization/retry/reuse                  |
| Adapter unit/route tests; `crates/trusted-server-integration-tests/tests/parity.rs`   | Healthy/failure matrix, methods, authentication, response headers, and preservation                   |
| `scripts/smoke-{common,fastly,axum,cloudflare,spin}.sh`                               | Real production startup/configuration and publisher controls                                          |
| Integration readiness consumers and trace-boundary assertions listed in Task 4        | Use the new reserved probes deliberately                                                              |
| New `.github/workflows/health-startup-contract.yml`; `.tool-versions`                 | Repeatable production-artifact smoke matrix and the missing Wrangler pin                              |
| API reference, adapter guides, integration guide, `scripts/README.md`, `CHANGELOG.md` | Normative matrix, timing limits, verification, and migration                                          |

## Task 1: Shared operational response policy

Files: create `crates/trusted-server-core/src/health.rs`; modify
`crates/trusted-server-core/src/lib.rs`.

- [x] Add focused tests in the new module for exact paths, query tolerance,
      GET/HEAD, near misses, non-probe methods, success/failure bytes, protected
      headers, terminal markers, and hook delegation. Use a counting delegate
      that returns a distinctive response: probes must bypass it, ordinary
      requests must invoke it once without path/method mutation.
- [x] Establish the missing-contract baseline through public adapter regressions:
      anonymous health received 401 under catch-all auth, operator headers
      changed cache policy, and Axum startup returned 500 instead of 503. After
      implementation, run `cargo test -p trusted-server-core --lib health::tests`.
- [x] Implement the small API below, with required public documentation and
      examples. Use EdgeZero's `Request`, `Response`, and `Body`, static header
      values, and explicit empty HEAD bodies. No settings, clocks, stores, or
      runtime services belong in this module.

```rust
// Implemented API surface; public items have documentation in health.rs.
pub enum StartupState {
    Ready,
    Unavailable,
}

pub enum Probe {
    Liveness,
    Readiness,
}

pub struct OperationalResponse;

pub fn classify_probe(method: &Method, path: &str) -> Option<Probe>;
pub fn probe_response(method: &Method, probe: Probe, state: StartupState) -> Response;
pub fn startup_unavailable_response(method: &Method) -> Response;

pub struct ProbePreDispatchHook {
    state: StartupState,
    delegate: Option<BoxPreDispatchHook>,
}
```

The classifier recognizes only GET/HEAD and exact `/health` or `/ready` paths.
The caller supplies `request.uri().path()`, so query strings do not affect the
result. Constructors give successful probes `200 text/plain` and `ok` or `ready`;
unavailable responses use `503 text/plain; charset=utf-8` and
`Service Unavailable\n`. HEAD uses the same status and headers but no body.
Every constructed response carries no-store, the optional compiled git version,
and `OperationalResponse`. It never adds cookies or authentication challenges.

The hook's constructor takes `StartupState` and `Option<BoxPreDispatchHook>`.
Implement `PreDispatchHook` with `#[async_trait(?Send)]` and this decision order:

```text
classify the unchanged request method and URI path
if probe: return Some(shared probe response)
if delegate exists: await delegate.handle(request)
otherwise: return None
```

- [x] Export the module; rerun focused tests, then
      `cargo clippy -p trusted-server-core --lib -- -D warnings`.
      Keep core's normal `TrustedServerError` conversion unchanged.

## Task 2: Axum, Cloudflare, and Spin integration

Files: the three adapters' `src/app.rs`, their existing `tests/routes.rs` where
present, `crates/trusted-server-integration-tests/tests/parity.rs`, and
Cloudflare/Spin `src/lib.rs`.

- [x] Add healthy-router regressions for GET/HEAD on both probes with a
      catch-all Basic-auth handler and configured response headers that attempt
      to set cache-control, content-type, set-cookie, and www-authenticate.
      Assert the exact probe contract and an ordinary protected-route `401`.
      Add a trace request control proving existing trace handling still runs.
- [x] Add private failure-router tests in each `app.rs`: use Configuration,
      BadRequest, and a normally-503 error with a fictional sentinel in its
      message/attachments. Assert identical generic failures on `/` and a
      nested path for all seven supported methods, alongside health/readiness.
      Check HEAD is empty and the sentinel never appears in responses.
- [x] Run `cargo test-axum`, `cargo test-cloudflare`, and `cargo test-spin`;
      retain meaningful baseline failures for the new cases.
- [x] Wrap each healthy router's current `TracePreDispatchHook` in the shared
      hook with `Ready`. Install the wrapper as the sole SDK hook. Install
      `Unavailable` without a delegate in failed routers. Remove redundant old
      GET health handlers so there is only one probe response policy.
- [x] Change failure catchalls to `startup_unavailable_response(ctx.request().method())`.
      Preserve the existing supported-method lists and `/` plus `/{*rest}`
      matching. Remove failure-router default-settings finalization; startup
      responses must not depend on it. Retain the diagnostic log at the existing
      startup catch boundary. Healthy middleware ordering stays unchanged.
- [x] Make the existing Cloudflare/Spin logger types and installation callable
      in production, before `run_app`. Rename fixture-specific names to describe
      the adapter sink, keep installation idempotent, and preserve the current
      warning/error filtering. Keep fixture-only panic hooks under `test-utils`.
      Do not add a logger dependency or change the general logging format.
- [x] Extend native parity with full response/body assertions for healthy
      probes and authentication/header controls. Exercise private failure
      routers in adapter tests rather than exposing new production test APIs.
      Use counting/failing runtime services for already initialized routers to
      show probes do not poll dependencies, even when those services would fail.
- [x] Rerun the three adapter suites and
      `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`.
      Run `cargo clippy-axum`, `cargo clippy-cloudflare`,
      `cargo clippy-cloudflare-wasm`, `cargo clippy-spin-native`, and
      `cargo clippy-spin-wasm`. All new matrix cases must pass.

## Task 3: Fastly native and router boundaries

Files: `crates/trusted-server-adapter-fastly/src/app.rs`, `src/main.rs`, and
existing sandbox tests in `src/sandbox.rs` if additional controls are needed.

- [x] Add regressions around `health_response` for GET/HEAD, exact path/query,
      no-store, compiled version, and absent Fastly-runtime headers. Add router
      tests for ready/unavailable and ordinary startup errors, including the
      current BadRequest fixture whose expected status changes from 400 to 503.
- [x] Add entrypoint/finalization tests using a hostile settings fixture and
      `OperationalResponse`. Assert operational responses bypass settings
      reload, EC finalization, geo, request-filter effects, cookie changes, and
      sandbox counter headers. Keep existing trace terminal-response tests.
- [x] Run `cargo test-fastly` and `cargo test-fastly-reuse` to verify the native
      probe, startup response, and initialization lifecycle regressions.
- [x] Replace native liveness response construction with the shared policy
      converted through the existing Fastly response conversion. It remains
      before logging, settings, and `sandbox.initialize`; support HEAD explicitly.
- [x] Wrap the healthy Fastly router's trace hook with `Ready`; add the shared
      unavailable hook and fixed failure catchalls to `startup_error_router`.
      This also makes in-process router behavior match the native contract.
- [x] In `edgezero_main`, retain the original method and probe classification
      before consuming the native request. After the existing config-store-open
      and `sandbox.initialize` outcome, answer `/ready` from that result before
      building ordinary per-request services, sanitation, or metadata.
      Do not call `build_state` separately or retain failed initialization.
- [x] For config-store-open failure, log the error and send the shared generic
      unavailable response for ordinary supported routes and readiness. Liveness
      has already returned. Leave JA4/counter diagnostic behavior and unrelated
      dispatch/conversion errors unchanged; identify this distinction in tests.
- [x] When a failed build has returned its fallback app, let ordinary traffic
      reach its fixed unavailable response. Recognize `OperationalResponse`
      immediately after router dispatch, before the existing native finalizers,
      and send it directly. Preserve the separate trace marker path. Keep the
      defensive terminal-effects helper from mutating operational responses too.
- [x] Exercise normal initialization, repeated readiness, and failed-then-valid
      initialization through the existing sandbox test seam: liveness performs
      zero builds; readiness initializes once and reuses success in reusable
      mode; failed construction is retried. Do not infer shared sandbox identity
      by polling different network requests.
- [x] Rerun both Fastly suites, then `cargo clippy-fastly`. Verify unrelated
      runtime error statuses, publisher methods, trace privacy, and streaming
      finalization remain covered by their existing tests.

## Task 4: Real startup consumers and production-artifact evidence

Files: `scripts/smoke-{common,fastly,axum,cloudflare,spin}.sh`,
`crates/trusted-server-integration-tests/tests/environments/{axum,cloudflare,fastly}.rs`,
`tests/common/trace_boundary.rs` and `tests/html_eof_runtime.rs` within that
integration crate; its `browser/helpers/infra.ts`;
`crates/trusted-server-cli/src/commands/dev/sandbox_probe.rs`; new `.github/workflows/health-startup-contract.yml`;
`.tool-versions`.

- [x] Update positive-startup waits to use GET/HEAD `/ready` and require 200.
      Remove successful-startup acceptance of arbitrary root status or a
      discovery/root fallback. Inspect all `health_check_path` implementations
      and explicit waits; keep framework-origin health checks unchanged.
      Negative configuration fixtures must launch without waiting for readiness:
      wait for `/health` or transport, then assert `/ready` is unavailable.
- [x] Move the browser Viceroy startup wait to `/ready`. Reject `/ready` in
      the sandbox-probe CLI before connecting, because operational responses
      intentionally carry no workload counters.
- [x] Update `html_eof_runtime.rs`'s HEAD `/health` application-usability wait
      to HEAD `/ready`. Update the Cloudflare `/health` fallback expectation in
      `trace_boundary.rs`: it is now a local probe. Keep near-miss publisher
      routing and trace privacy/authentication assertions.
- [x] Extend the shared smoke assertions to check exact GET bodies, empty HEAD
      bodies on the wire, content type, no-store, and absence of cookie/auth
      headers for both probes. Do this while each process is alive, before its
      existing shutdown. Keep the final publisher request and sentinel/rewrite
      assertions in place; preserve its status separately so probe calls do not
      overwrite `SMOKE_HTTP_STATUS` used by existing checks.
- [x] Keep existing missing-config and individual missing-secret cases; change
      Fastly/Axum/Cloudflare ordinary expectations from 500 to 503. Add separate
      malformed-envelope and syntactically valid but invalid-settings cases.
      Keep all unrelated required secrets available in each controlled negative.
      Use a fresh process/isolate/component fixture for each case to avoid
      Cloudflare's OnceCell retaining a prior configuration.
- [x] Inject invalid input at the genuine runtime boundary, after the valid
      fixture's strict CLI validation: Axum config env value, Fastly local
      config-store value, Cloudflare `TRUSTED_SERVER_CONFIG`, and Spin local
      config-store value. For invalid settings, encode a valid envelope with
      a known startup-invalid setting using the existing blob format helper;
      do not confuse an envelope parser failure with settings validation.
      Require the corresponding diagnostic in logs and a fixed public body;
      use fictional sentinels to test disclosure.
      Use a literal `{` for the malformed envelope. For invalid settings, set
      `publisher.domain` to the empty string and reconstruct a valid-integrity
      `BlobEnvelope` from that data (see `config_payload.rs` test helpers).
      Retain all secrets and assert the settings-validation diagnostic. Record
      the exact mutation and observed diagnostic separately for these cases.
- [x] In a valid-config variant, add a catch-all auth rule and hostile response
      headers. Both probes must remain anonymous and fixed; an ordinary route
      without credentials must return 401. Retain a separate publisher-positive
      variant or supply valid credentials for the publisher control.
      Check local origin counters/logs before and after probes to prove they
      never forward; the later positive publisher request must reach origin.
- [x] Build current production artifacts before the smoke, without `test-utils`.
      Existing scripts can reuse files; an old artifact's existence is not
      evidence. Explicitly build the CLI, Axum, Fastly, Cloudflare bundle, and
      Spin component for this revision, and pass their artifact paths where
      supported. Use an isolated target/build directory when implementation
      shares a workspace with other builds. Verify the logs originate from the
      production sinks added in Task 2.
- [x] Add `wrangler 4.83.0` to `.tool-versions`, matching the already pinned
      runtime in `worker-auction-deadlines.yml`; the current Cloudflare smoke
      requires that entry but it is absent. Keep this addition scoped to making
      the health fixture runnable; do not float the installed CLI version.
- [x] Add a PR/push CI matrix for `[fastly, axum, cloudflare, spin]` running the
      corresponding smoke script. Set up the pinned Node before any Cargo step,
      use the existing setup-rust action, install JS dependencies, build current
      CLI and the selected production adapter, and install required local tools.
      For Fastly use pinned Fastly CLI/Viceroy; for Cloudflare use locked
      worker-build/wasm-bindgen and pinned Wrangler; for Spin reuse the pinned
      archive/version/digest setup from `worker-auction-deadlines.yml`.
      Run `scripts/smoke-${adapter}.sh`, not deadline example binaries. Provide
      curl, jq, Python 3.11+ and pgrep; scripts clean their owned temporary resources.
      The job must fail on missing runtimes or artifacts, not skip a matrix row.
      Add an opt-in smoke artifact destination for CI: before cleanup, copy
      synthetic fixture logs and response captures there, then upload them on
      failure. Keep default cleanup intact and do not upload config envelopes,
      secret fixtures, or ambient environment files.
- [x] Run `shellcheck scripts/smoke-{common,fastly,axum,cloudflare,spin}.sh` and
      `bash -n` on those scripts. Run all four smoke scripts with the freshly
      built artifacts and record actual versions/results. A missing runtime is
      an outstanding verification requirement, not a passing row.
- [x] Rerun integration parity and existing trace-boundary/runtime tests affected
      by the new reserved paths. Run the HTML EOF harness unit/lint gates because
      its readiness consumer changed; this does not change EOF runtime behavior.

## Task 5: Documentation and migration

Files: `docs/guide/api-reference.md`, `docs/guide/{fastly,axum-dev,cloudflare,spin}.md`,
`docs/guide/integrations-overview.md`, `scripts/README.md`, `CHANGELOG.md`.

- [x] Put the specification's complete response matrix in the API reference;
      make that the normative source for status, body, headers, methods, and
      disclosure policy. Update the route-availability and adapter-support tables
      to include both probes in healthy and failed-startup states.
- [x] Explain initialization readiness, absence of dependency polling, and the
      platform timing limits. Replace blanket claims that all startup errors
      are 500. Explain that runtime errors retain their existing mapping.
- [x] Link adapter summaries to the canonical matrix. Fix Spin's current claim
      of existing Axum failure-time parity and Cloudflare's no-local-health claim.
      Update smoke examples and expected statuses without rewriting historical
      specifications.
- [x] Add migration notes: reserve GET/HEAD probe paths, move load-balancer
      admission to `/ready`, note auth exemptions and 503 startup status, and
      warn operators whose publishers currently own those probe paths.
- [x] Format changed docs and run the docs format command with Node 24.20.0.
      Run the domain linter on added lines using the host triple and actual base
      branch, as specified in `AGENTS.md`.

## Task 6: Final verification and review

- [x] Run `cargo fmt --all -- --check` and the full required CI gate list from
      the current `AGENTS.md` before PR handoff. Record every gate's command,
      target/features, tested revision, actual tool versions, result, and blocker.
      Do not substitute bare workspace build/test for the adapter aliases.
- [x] Include the targeted native core health tests and Clippy, both Fastly
      modes, native adapter tests, Cloudflare/Spin WASM Clippy, parity, and all
      four production smoke rows. New CI configuration must pass the repository
      Node pin guard and changed-domain checks too.
- [x] For the changed EOF wait, run
      `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --features html-eof-runtime --test html_eof_runtime`
      and the matching `cargo clippy ... -- -D warnings` command. If verification
      changes touch publisher/HTML finalization, also run the conditional actual
      Axum/Fastly EOF/auction controls from `AGENTS.md`.
- [x] Request independent review of the final implementation against the spec,
      including native Fastly exits, production logger behavior, real input
      reach, middleware/trace preservation, and test-consumer migration. Resolve
      material findings and rerun checks affected by corrections.
- [x] Report the implemented behavior, compatibility changes, exact tested
      content, local and hosted evidence separately, and any missing runtime
      qualification. Prepare PR text only within the authorized delivery scope;
      approval of these planning documents is not authorization to publish.

## Acceptance checklist

- [x] One documented matrix matches all four actual adapters.
- [x] Missing, malformed, invalid, and valid startup inputs reach genuine loaders.
- [x] GET and HEAD probes have fixed auth-independent, uncacheable responses.
- [x] Failed ordinary startup responses are generic 503 without disclosure.
- [x] Production failure diagnostics are observable without `test-utils`.
- [x] Probes perform no runtime dependency polling or publisher forwarding.
- [x] Fastly initialization retry/reuse and ordinary trace/auth/route behavior survive.
- [x] Consumers use readiness where they require successful application startup.
- [x] Required tests/checks and four runtime rows have evidence or explicit blockers.

## Execution record: 2026-11-16

- Shared policy, four adapter integrations, production diagnostic sinks,
  initialization retry/reuse, consumer migration, and documentation are complete.
- Independent reviews of the production paths and tooling found no remaining
  material issues. Review corrections included injected-service controls,
  Fastly probe lifecycle coverage, the browser readiness wait, sandbox-probe
  exclusion, and the Python 3.11 prerequisite.
- Every production adapter passed eight startup smoke cases: missing config,
  each of three missing secrets, malformed envelope, invalid settings,
  hostile auth/response headers, and successful publisher traffic. These used
  production artifacts without `test-utils`, with GET and raw HTTP HEAD checks.
- All five adapter test commands passed, including both Fastly sandbox modes.
  Native parity passed 31 tests. All eight target-matched Clippy aliases passed.
- CLI tests, JS tests/build/format, documentation and Markdown format, native
  digest test/lint, clock lint, Node pin guards, release/proposal checks, HTML EOF
  harness test/lint, and Fastly deadline fixtures passed with Rust 1.98.1 and
  Node 24.20.0. Added-line domain lint also passed against the staged changes;
  committed-state diff mode alone does not cover an uncommitted implementation.
- Cloudflare and Spin deadline examples separately passed build, target-matched
  Clippy, and actual-runtime controls. Used Fastly CLI 16.0.0, Viceroy 0.21.0,
  Wrangler 4.83.0, Spin 4.1.0, worker-build 0.8.5, and wasm-bindgen 0.2.126.
- Core documentation generation completed with existing warnings in unchanged
  modules; the new health module introduced no documentation warning.
- Hosted CI results are reported separately on the pull request. Existing ignored
  tests remain ignored except where explicitly exercised by the repository CLI
  test helper; no skipped case is counted as a passing runtime smoke.
