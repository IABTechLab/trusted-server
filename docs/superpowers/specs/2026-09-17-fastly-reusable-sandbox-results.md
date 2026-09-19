# Fastly reusable sandbox — local A/B/C results

Issue: [#856](https://github.com/IABTechLab/trusted-server/issues/856)
Design: [2026-09-17-fastly-reusable-sandbox-design.md](./2026-09-17-fastly-reusable-sandbox-design.md)

Local measurement only. No deployment was performed.

## What was measured

| Arm | Commit      | Build                         | Meaning                                |
| --- | ----------- | ----------------------------- | -------------------------------------- |
| A   | `ca7e7fa7a` | default features              | Feature off, one request per sandbox   |
| B   | `00c4e4278` | `--features reusable-sandbox` | Reuse, application rebuilt per request |
| C   | `ca7e7fa7a` | `--features reusable-sandbox` | Reuse, application retained            |

Arm A is built from the same commit as C with the feature off, which is the
shipped configuration. Arm B is the commit immediately before retention, so
A→B isolates the serving loop and B→C isolates retention.

## Runtime

- Viceroy **0.17.0**, invoked as `viceroy serve` directly against each arm's
  `.wasm`, so the binary under test is exactly the commit named above.
  `fastly compute serve` was not used, because it rebuilds from `fastly.toml`
  and would not honour per-arm feature flags.
- Fastly SDK **0.12.1** (`Cargo.lock`).
- Service id `0000000000000000000000` (Viceroy's fixed local value).
- Viceroy implements `next_request`, `next_request_wait`, and
  `next_request_abandon`, which is what makes reuse observable at all.
- Host counters `get_vcpu_ms` and `get_heap_mib` are both implemented.

## Configuration

Sandbox bounds, written into the local `edgezero_runtime_env` config store:

```toml
EDGEZERO__SERVICES__0000000000000000000000__TS__SANDBOX__MAX_REQUESTS   = "20"
EDGEZERO__SERVICES__0000000000000000000000__TS__SANDBOX__MAX_LIFETIME_MS = "60000"
EDGEZERO__SERVICES__0000000000000000000000__TS__SANDBOX__TIMEOUT_MS      = "2000"
```

`debug.sandbox_metrics_enabled = true` in `trusted-server.toml`, pushed with
`ts config push --adapter fastly --local --yes`.

Local fixtures required to make the operator config resolve: 21 placeholder
entries in the `ts_secrets` secret store (partner API tokens, DataDome keys,
S3 keys, handler password). All are local-only dummy values.

### Commands

```bash
# per arm
cargo build --bin trusted-server-adapter-fastly --release --target wasm32-wasip1 \
  [--features reusable-sandbox]
viceroy serve -C fastly.toml --addr 127.0.0.1:7676 <arm>.wasm

# measurement
ts dev sandbox-probe --path /.well-known/trusted-server.json --requests 8
```

### Resource-measurement provenance

The reuse, build-count and latency figures come from the commits in the table
above. The vCPU and heap figures do **not**: those headers were added later, in
`f51ba6f51`, so they were collected from a separate build of that revision with
`--features reusable-sandbox`.

`ts dev sandbox-probe` does not report the resource headers. They were read
directly:

```bash
curl -s -o /dev/null -D - http://127.0.0.1:7676/<path> \
  | grep -iE 'x-ts-sandbox-(vcpu-ms|heap-mib|ordinal|builds|instance)'
```

The latency experiment was not rerun on `f51ba6f51`; adding two header writes
is not expected to move it, but that is an assumption, not a measurement.

## Reuse

Reuse was observed in B and C. Each sandbox served **6 requests** before a new
one appeared, repeatedly, across every run. Arm A produced a distinct instance
for every request with ordinal always 1.

Six per sandbox is a **repeated local observation under Viceroy 0.17.0**, not a
demonstrated universal runtime ceiling. It is consistent with the previously
reported six-request limit.

Retirement summaries, logged by `serve_loop`:

```
arm B          sandbox retiring after 6 request(s), 6 build(s)
arm C          sandbox retiring after 4 request(s), 1 build(s)   <- see note
arm C, failing sandbox retiring after 5 request(s), 5 build(s)
```

Note on the arm C line: that run issued only 4 requests (the streaming check),
so the sandbox retired partially filled when the probe stopped. It is not a
different reuse depth. The 8-request probe runs show C reaching ordinal 6.

## Builds per sandbox

| Arm | Requests per sandbox | Builds per sandbox |
| --- | -------------------- | ------------------ |
| A   | 1                    | 1                  |
| B   | 6                    | 6                  |
| C   | 6                    | **1**              |

This is the primary result. Retention removes the repeated application build;
the serving loop alone does not.

## Latency

Route `/.well-known/trusted-server.json`, an edge-only route with no origin
fetch. 24 samples per arm, three runs of eight. Milliseconds.

| Arm                      | min   | p50       | p90   | max    | mean  |
| ------------------------ | ----- | --------- | ----- | ------ | ----- |
| A feature off            | 3.401 | **3.546** | 3.860 | 10.712 | 3.872 |
| B reuse, rebuild per req | 3.033 | **3.162** | 3.725 | 6.587  | 3.391 |
| C reuse, retained        | 0.277 | **0.308** | 3.868 | 4.333  | 0.934 |

Arm C split by whether the request built the application:

| Subset             | n   | p50       | mean  |
| ------------------ | --- | --------- | ----- |
| C, build requests  | 4   | **3.883** | 3.991 |
| C, reused requests | 20  | **0.302** | 0.322 |

Derived figures, each stated as what it actually is:

- **Observed cold-versus-warm difference: ~3.58 ms** (C build p50 minus C
  reused p50). This is wall-clock difference between a request that builds and
  one that does not. It is **not** isolated initialization CPU cost: it also
  contains whatever else differs on a first request in a sandbox.
- **Warm C versus A: 11.7×** (A p50 3.546 / C reused p50 0.302), a saving of
  ~3.24 ms at p50. This compares a _reused_ request against the baseline and is
  not the whole-workload improvement.
- **Observed aggregate: ~4.15×** (A mean 3.872 / C mean 0.934). Both are sample
  means over the same 24 observations per arm, so this is a like-for-like
  aggregate rather than a mix of statistics. The sample is small and local;
  treat the ratio as indicative.
- **Modelled amortization, stated separately:** across a full six-request
  sandbox where one request in six pays the build, C would average
  (3.883 + 5 × 0.302) / 6 ≈ 0.90 ms. This is a model built from the two C
  subsets, not an observed aggregate, and it is not the figure above.

### Route selection matters

The first attempt used `/`, and all three arms measured ~260–290 ms with no
distinguishable difference. That path reaches the real publisher origin, whose
bot wall returns 403, plus a Tinybird telemetry call. A live network round trip
buries a ~3.6 ms effect entirely. The effect is only visible on an edge-only
route. Any repeat of this measurement must choose the route deliberately.

## CPU and memory

Both counters are supported under Viceroy and were read per request from one
retained sandbox.

**These are pre-send cumulative samples, not complete per-request costs.** Both
counters are read as the counters are attached to the response, which is before
headers commit, therefore before streaming and before post-send work such as
pull sync. Consequently:

- The first sample excludes the remainder of the first request's work.
- A difference between consecutive samples contains the previous request's tail
  plus the current request's work up to commitment. It is not that request's
  cost.
- `heap_memory_snapshot_mib` reports the guest's linear memory including some
  host-managed buffering, rounded to MiB. It is not strictly Rust heap usage.

The sampling is still useful as a shape, with those caveats:

| Request (ordinal) | cumulative vCPU ms (pre-send) | heap MiB (pre-send) |
| ----------------- | ----------------------------- | ------------------- |
| 1 (builds)        | 17                            | 4                   |
| 2                 | 18                            | 4                   |
| 3                 | 21                            | 4                   |
| 4                 | 23                            | 4                   |

vCPU is cumulative per sandbox: ~17 ms measured at the first request's
commitment point, which includes construction, then ~1–3 ms of additional
cumulative time per reused request measured at the same point. Heap read 4 MiB
at every sample.

Four requests is far too short a span to say anything about memory stability.
Long-lived memory behaviour remains **unverified**.

## Correctness, on observed reused instances

### Progressive delivery — verified

An origin that emits its first chunk immediately and then stalls 2 s before the
tail (first byte 1.5 ms, completes 2.007 s direct). Through the edge, on one
retained sandbox:

| Request (ordinal) | first byte | total   |
| ----------------- | ---------- | ------- |
| 1                 | 0.308 s    | 2.312 s |
| 2                 | 0.169 s    | 2.171 s |
| 3                 | 0.170 s    | 2.175 s |
| 4                 | 0.158 s    | 2.162 s |

The first chunk reaches the client roughly two seconds before the origin
finishes, on every request including reused ones. A buffering path would show
first byte ≈ total ≈ 2 s.

An earlier check that only confirmed `transfer-encoding: chunked` and matching
body hashes was insufficient: that establishes framing and content integrity,
not progressive delivery.

### Duplicate `Set-Cookie` — verified

Origin emits two distinct cookies. Four requests on one retained sandbox
(`builds=1` throughout) each returned both `origin_first` and `origin_second`.

The real publisher origin cannot be used for this: its own bot wall 403s before
EC finalization, and the DataDome test-bypass credential does not help because
the 403 originates upstream, not in our integration.

### Request isolation — verified

Four requests on one retained sandbox, each with a deliberately distinct
cookie, `User-Agent`, custom header, `Accept-Language`, and query string,
against an origin that echoes those values into the body. Each response
contained its own marker exactly once and **none** of the other three requests'
markers, across 4 requests × 5 markers × 3 other responses.

The interrupted-document case — a document cut off mid-accumulation followed by
a second document through the same registry — is covered by the regression
tests added in `00c4e4278` for both the Google Tag Manager and Next.js script
rewriters. Both fail against the previous code with the first document's
content prepended to the second's response.

### Failed build not retained — verified; recovery verified only at unit level

With a deliberately broken secret-store key, one sandbox served five requests
and performed **five builds** (`sandbox retiring after 5 request(s), 5
build(s)`). The failure is therefore not retained and construction is retried
on every request.

This does **not** demonstrate recovery end to end. Recovery additionally
requires a _successful_ build after a failure in the same sandbox, which is not
locally reproducible: Viceroy's config and secret stores are fixed for a
guest's lifetime, so a build that fails once fails for that whole sandbox.

Recovery is covered at unit level by
`sandbox::tests::a_failed_build_is_retried_and_a_later_success_is_retained`,
which drives the production decision (`Sandbox::resolve_app`) with an injected
builder: fail, then succeed, then a third request that fails the test if the
builder is called again. An earlier version of this test drove the `Sandbox`
API by hand and would have passed even if the production decision were broken;
it was replaced. Both the reuse and recovery tests were checked against
deliberate mutations of `resolve_app` and fail as intended.

Note: counters are absent on the failure path, because they are gated on
settings and settings are what failed. Evidence there comes from the log.

## Limitations

- **Long-lived memory behaviour: unverified.** Four to six requests per sandbox
  is far too short, and Viceroy's limit prevents extending it locally.
- **Deployed eviction frequency, resource accounting, endpoint-handle validity,
  and production latency: unverified.** All evidence here is local.
- **Six requests per sandbox is a local observation** under Viceroy 0.17.0,
  repeated across runs. It is not established as a universal ceiling.
- Correctness results used **mock origins** on `127.0.0.1`, because the real
  publisher origin's bot wall blocks the paths under test. Behaviour against
  the real origin is unverified.
- The ~3.58 ms figure is a **cold-versus-warm wall-clock difference**, not
  isolated initialization CPU cost.
- Reuse is never guaranteed: any request may start a fresh sandbox, so
  correctness must hold for a cold sandbox on every request.
- Named-endpoint log delivery was not separated from echoed stdout in these
  runs.

---

# Re-verification against EdgeZero `277544c4`

The workspace repinned EdgeZero from `v0.0.8` to
`277544c431c1ab9bafa14a45d5f35975b5587e97` on `feat/reusable-app-lifecycle`.
Everything below was observed **fresh against that revision**. None of the
latency numbers earlier in this document are carried over as evidence for it.

## Dependency diff

All six workspace entries moved `tag = "v0.0.8"` →
`rev = "277544c431c1ab9bafa14a45d5f35975b5587e97"`:
`edgezero-adapter-{axum,cloudflare,fastly,spin}`, `edgezero-cli`,
`edgezero-core`.

`Cargo.lock`: 16 changed lines, all of them the `source =` field of the eight
edgezero packages. **Zero unrelated dependency changes.** Exactly one distinct
edgezero source resolves, and no `v0.0.8` reference survives in either file.

### Why repin at all

Not for the `Serve` re-export — the contract explicitly says not to, and
`Serve` comes from the already-pinned `fastly 0.12.1` SDK. The repin is for:

- `edgezero-cli`: push/diff validation scoped to the selected adapter.
- `edgezero-adapter-spin`: secret references redacted from diagnostics.
- `edgezero-adapter-cloudflare`: duplicate response headers preserved.

Every change to `edgezero-core` across the range (`app.rs`, `app_config.rs`,
`router.rs`) is **test-only**, and the `edgezero-adapter-fastly` change is
purely additive. No runtime behaviour we depend on moved.

### Local code removed: none

The contract permits removing local code only where a public EdgeZero API now
provides equivalent behaviour. At this revision
`service_scoped_runtime_env_key` is still private and `runtime_env_keys` is
still a closed allowlist, so `sandbox::scoped_key` and
`sandbox::read_raw_limits` remain necessary. Swapping the `Serve` import for
the re-export would be an import change, not a removal, and the contract
advises against it. Nothing qualified.

## CLI verification (synthetic values only)

Using the synthetic secret reference `Bad-Ref-01`, which violates Spin's
naming rule. No real `.env` value was used, printed, or copied.

| Check                                                | Result                                                         |
| ---------------------------------------------------- | -------------------------------------------------------------- |
| Fastly push not blocked by unrelated Spin validation | **pass** — `config push --adapter fastly --local` exits 0      |
| Standalone validation still checks declared adapters | **pass** — `config validate` exits 2 and reports the violation |
| Spin error omits original and normalized values      | **pass** — neither `Bad-Ref-01` nor `bad-ref-01` appears       |

## Runtime compatibility, observed fresh

Viceroy 0.17.0, Fastly SDK 0.12.1, service id `0000000000000000000000`.
Artifacts: arm A `sha256:041e0ca413e01fe5aa4bed06…`, arm C
`sha256:fa6af70c6e01643182f251fe…`.

| Contract requirement                                     | Result                                                                                                                                                 |
| -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Default stays single-request, reuse opt-in               | **pass** — feature off gave 6 distinct instances, ordinal 1 each, _with limits configured in the store_                                                |
| Health/debug probes bypass construction                  | **pass** — `/health`, `/_ts/debug/sandbox`, `/_ts/debug/ja4` served at ordinals 1–3; the first workload request at ordinal 4 still reported `builds=1` |
| Observed reuse retains one successful build              | **pass** — 6 requests on one instance, `builds=1`; ~6.3 ms cold vs ~0.27–0.38 ms reused                                                                |
| Failed initialization stays retryable                    | **pass** — 5 requests, 5 build attempts, never retained                                                                                                |
| Fail → success → reuse through the production decision   | **unit level only** (see gaps)                                                                                                                         |
| Request/document state isolated                          | **pass** — 4 requests × 4 distinct markers, no marker appeared in any other response                                                                   |
| Delayed chunks reach the client before origin completion | **pass** — first byte 0.168–0.186 s against 2.17–2.19 s total, on ordinals 1–4 of one sandbox                                                          |
| Duplicate `Set-Cookie` and finalization survive          | **pass** — both cookies on every request of a reused sandbox, finalization headers present                                                             |
| Post-commit failure never causes a second response       | **pass** — see below                                                                                                                                   |

### Post-commit failure, in detail

Against a raw-socket origin that commits headers, sends one chunk, then resets
(`SO_LINGER` 0): three consecutive aborted requests on **one** sandbox each
produced exactly one status line (`200`, 1034 bytes) with no second response
appended, and each logged with full attribution:

```
ERROR EdgeZero streaming failed [instance=…0000 ordinal=1 request=…0000]
ERROR EdgeZero streaming failed [instance=…0000 ordinal=2 request=…0001]
ERROR EdgeZero streaming failed [instance=…0000 ordinal=3 request=…0002]
```

A normal request afterwards on that same sandbox succeeded at ordinal 4 with
`builds=1`, so a post-commitment failure neither double-responds nor discards
the retained application.

An earlier attempt using a graceful close did **not** exercise this path: the
guest blocked on an origin that never terminated, and each request landed on a
fresh sandbox. Only a true reset reaches the error branch.

## EdgeZero's own compatibility suite

The contract requires running it against the adopted checkout:

```sh
./scripts/smoke_test_reusable_app.sh --adapter fastly --suite smoke --require-runtime
```

**Exit 0** at `277544c4`, including the `custom-*` arms that exercise the same
custom-lifecycle shape this adapter uses. Its `custom-initialization` scenario
shows one guest (`18d6143588bb14d8-e548-1`) reaching `construction_rounds: 0`
and continuing to serve ordinals 4 and 5 — the in-guest recovery our own
harness cannot reproduce.

## Remaining gaps

- **Fail → success → reuse in one guest is unit-level only for this
  application.** Viceroy's config and secret stores are fixed for a guest's
  lifetime, so an initialization failure persists for that whole sandbox.
  EdgeZero's fixture reproduces it because its fixture app injects the failure
  itself. Ours is covered by
  `sandbox::tests::a_failed_build_is_retried_and_a_later_success_is_retained`,
  driving the production `resolve_app` decision.
- **Linux CI verification is separate.** Everything here is macOS. Cross-
  compiling to `x86_64-unknown-linux-gnu` locally fails because `aws-lc-sys`
  needs a Linux C toolchain that is not installed, so Linux remains CI-only.
- **Long-lived memory behaviour and deployed eviction: still unverified.**
- **Six requests per sandbox remains a local Viceroy observation**, not a
  demonstrated universal ceiling.
- Correctness used mock origins on `127.0.0.1`; the real publisher origin's
  bot wall blocks the paths under test.
- **The pin is a branch revision, not a release tag.** `277544c4` lives on
  `feat/reusable-app-lifecycle`, which is unmerged. It is correctly pinned by
  SHA so the build is reproducible, but the workspace should move to whichever
  released tag contains this revision once that branch merges, rather than
  sitting on an unmerged feature branch.
- No latency A/B/C was rerun at this revision. Build-count and correctness
  were re-observed; comparative latency was not, and the earlier figures are
  not offered as evidence for this revision.

---

# Adoption of EdgeZero's lifecycle module (`76c59b44`)

The pin moved from `277544c4` to
`76c59b440fb35d1317dcb3fa8c1172161e3f5309` on `feat/reusable-app-lifecycle`,
which adds `edgezero_adapter_fastly::lifecycle`. This is an adoption, not a
repin: the local lifecycle mechanics were deleted and replaced.

Everything below was observed **fresh at this revision**. No earlier latency
figure is offered as evidence for it, and no comparative A/B/C was rerun.

## Local implementations removed

| Removed                                                                                              | Replaced by                                                  |
| ---------------------------------------------------------------------------------------------------- | ------------------------------------------------------------ |
| local `struct Sandbox` (`logger_installed`, `requests`, `builds`, `pending_diagnostics`, `retained`) | `lifecycle::Sandbox<RetainedApp>`                            |
| `resolve_app`, `retain_app`, `retained_app`                                                          | `Sandbox::initialize`                                        |
| `ensure_logger` + `logger_installed` guard                                                           | `Sandbox::setup_once`                                        |
| `begin_request` / `record_build` / `requests` / `builds`                                             | `Sandbox::requests()` / `Sandbox::initialization_attempts()` |
| `Serve::new()…run_with_context(…)` glue                                                              | `lifecycle::serve_custom` and `lifecycle::run_custom`        |

`logging::init_logger` now returns `Result<(), String>` instead of panicking on
the install path, so `setup_once` marks setup complete only after a successful
install and a failed install stays eligible for retry.

`serve_app` is **not** used: its response conversion buffers streams.

## Still application-owned

Feature gate, kill switch, limit parsing and safe fallback; settings retention
and refresh policy; health, JA4 and metrics routing; fresh per-request
metadata, handles, services, extensions, bodies and correlation ids; raw
request conversion and router dispatch; response-extension finalization;
progressive streaming; duplicate `Set-Cookie`; post-send work; per-document
rewrite-buffer isolation.

One new local type: `StartupDiagnostics`, holding messages produced by limit
resolution before any logger exists. It cannot live in the framework `Sandbox`,
which `serve_custom` owns and which has no slot for application state other
than the retained payload, so it is threaded through the callback.

## Runtime observations at `76c59b44`

Viceroy 0.17.0, Fastly SDK 0.12.1, service id `0000000000000000000000`.
Artifacts: arm A `sha256:4bfa7e6007c1dc1deef1…`, arm C
`sha256:2c6319b7833a80a9f33d…`.

| Check                                                    | Result                                                                                                                                                                                                                                                                        |
| -------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Feature off stays single-request, with limits configured | **pass** — 6 requests, 6 distinct instances, ordinal 1 each                                                                                                                                                                                                                   |
| Lazy start, build once, reuse                            | **pass** — 6 requests on one instance, `builds=1`                                                                                                                                                                                                                             |
| Probes do not construct                                  | **pass** — `/health`, `/_ts/debug/sandbox`, `/_ts/debug/ja4` served at ordinals 1–3; the first workload request at ordinal 4 still reported `builds=1`. The framework counts probe callbacks as requests, which is why the ordinal advances while the attempt count does not. |
| Failed initialization retried, never retained            | **pass** — 5 requests, 5 build attempts                                                                                                                                                                                                                                       |
| Request isolation                                        | **pass** — 4 requests × 4 distinct markers, none appeared in another response                                                                                                                                                                                                 |
| Duplicate `Set-Cookie` and finalization                  | **pass** — both cookies and the finalized cache header on every request of a reused sandbox                                                                                                                                                                                   |
| Progressive delivery                                     | **pass** — first byte 0.155–0.167 s against ~2.16 s total                                                                                                                                                                                                                     |
| Post-commit failure, then a successful request           | **pass** — two aborted requests each produced exactly one status line, then ordinal 3 succeeded with `builds=1` on the same sandbox                                                                                                                                           |

Post-commit failures logged with full attribution, distinct per request:

```
streaming failed [instance=…0000 ordinal=1 request=…0000]
streaming failed [instance=…0000 ordinal=2 request=…0001]
```

## EdgeZero's compatibility suite at this revision

`./scripts/smoke_test_reusable_app.sh --adapter fastly --suite smoke --require-runtime`
→ **exit 0** at `76c59b4`, including the `custom-*` arms and
`custom-initialization`.

## Limitations

- **Same-sandbox config-store recovery stays unit-level.** Viceroy's config and
  secret stores are fixed for a guest's lifetime, so an initialization failure
  persists for that whole sandbox and fail → success → reuse cannot be driven
  end to end locally. It is covered by
  `sandbox::tests::a_failed_build_is_retried_and_a_later_success_is_retained`,
  which drives `Sandbox::initialize` with an injected builder and asserts the
  third call does not rebuild.
- **macOS only.** Linux is CI-only; cross-compiling locally fails because
  `aws-lc-sys` needs a Linux C toolchain that is not installed. No claim is
  made that Linux CI passed.
- Long-lived memory behaviour and deployed eviction remain unverified.
- Six requests per sandbox remains a local Viceroy observation.
- Correctness used mock origins on `127.0.0.1`.
- No comparative latency was rerun at this revision.

## Follow-ups

- **`sandbox::scoped_key` duplicates EdgeZero's private key format.**
  `service_scoped_runtime_env_key` is private at `76c59b44`, and
  `runtime_env_keys` is a closed allowlist that drops any key outside it, so
  the adapter builds the service-scoped key itself. A public EdgeZero
  key-construction or lookup helper would remove the duplication. The working
  implementation stays until such an API exists; the `TS__SANDBOX__*` suffixes
  and the limit-validation policy remain application-owned regardless.
- **The pin is an unmerged branch revision.** Move to a release tag once one
  contains `76c59b44`.
