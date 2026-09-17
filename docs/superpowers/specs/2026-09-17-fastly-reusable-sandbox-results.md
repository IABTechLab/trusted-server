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
