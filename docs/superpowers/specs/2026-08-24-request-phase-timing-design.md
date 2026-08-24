# Request phase timing: Server-Timing subtimings and access telemetry

**Date:** 2026-08-24
**Status:** Approved design, pending implementation plan.
**Scope:** `trusted-server-core`, Fastly and Axum adapters, `tinybird/` schema and pipes,
performance dashboard (separate repo).

---

## 1. Problem

On 2026-08-21 a production deployment (publisher redacted, `prospect-a.com`) showed an
episodic stall: for a window of roughly 40 minutes, every request that reached the
application path carried a uniform extra ~600 ms of Fastly `time-elapsed`, and then
recovered to 20-50 ms with no deploy or config change we could observe. `/health`
(2-4 ms, short-circuits before app construction) and `/_ts/debug/ja4` (6-9 ms, settings
load only) stayed fast throughout, so the stall lived between app construction and
response send.

Attributing that window required a live probing session: route-by-route bisection,
cookie-deletion experiments, and an eight-agent code trace. The trace found no
unconditional await on the path that could cost 570 ms, and exactly two
config-conditional candidates (the pre-route request filter's synchronous verification
POST, and EC identity KV writes before send), plus one dependency shared by every
application route (two geo hostcalls per request). We could not tell which one stalled,
because nothing in the response says where server time went.

The Compute CPU budget is ~50 ms per request, so any large `time-elapsed` is wall-clock
await on a dependency by definition. Those are exactly the numbers a response can carry
about itself.

## 2. Goals

1. Every response attributes its own server time by phase, in a standard header that
   browsers expose to JavaScript (`PerformanceResourceTiming.serverTiming`), so any RUM
   tool the publisher already runs picks up the breakdown with zero integration work.
2. The same numbers flow to Tinybird so we hold p50/p95/p99 per phase, per route class,
   per PoP, per deployed version, and a future stall window self-diagnoses in one query.
3. Zero cost to TTFB: collection is a handful of monotonic clock reads; telemetry
   emission happens strictly after the last body byte.

## 3. Non-goals

- No trailer-based Server-Timing for body-phase spans (browsers do not expose trailer
  values to JavaScript).
- No per-filter naming in any emitted surface. The request-filter span is `ts-filter`
  regardless of which filter runs; vendor identity stays out of headers and telemetry.
- No Cloudflare or Spin emission wiring in v1. Core collection is adapter-neutral; those
  adapters can wire emission later without core changes.
- No rollup materialized views. The raw datasource plus one endpoint pipe is v1;
  rollups only if panel latency demands them.
- No sampling of the header. The header is all-traffic when enabled; only Tinybird rows
  sample.

## 4. Design overview

```
adapter entry (T0)
  |  RequestTimings::new() -> request extensions
  v
app construction ................ ts-appbuild   (adapter)
pre-route request filters ....... ts-filter     (adapter wrapper)
geo lookup (single, deduped) .... ts-geo        (adapter; result stashed)
EC KV before send ............... ts-kv         (core: ec/finalize.rs)
template cache lookup ........... ts-c2         (core: publisher.rs)
origin fetch to resp headers .... ts-origin     (core: publisher.rs)
  |
finalize middleware:
  append Server-Timing header ... ts-total + all recorded spans
  |
headers committed; body streams
  auction hold at seam .......... auction_wait_ms  (row only)
  stream duration, bytes ........ stream_ms, resp_bytes (row only)
  |
post-send (adapter main):
  sample gate -> one NDJSON row -> Tinybird Events API (best effort)
```

Collection is always-on and flag-free. Two independent flags gate emission: the header
(`observability.server_timing_enabled`) and the telemetry row (`tinybird.access_enabled`).

## 5. `RequestTimings` (core)

New module `crates/trusted-server-core/src/request_timing.rs`.

- `Phase`: a closed enum: `AppBuild`, `Filter`, `Geo`, `EcKv`, `Origin`, `C2Lookup`,
  `AuctionWait`, `Stream`. Header rendering covers the first six plus the derived
  total; the last two are row-only.
- Inner state: one fixed-size array of `Option<Duration>` slots indexed by phase, plus
  `t0: Instant`, plus `resp_bytes: Option<u64>`. Phases that repeat within a request
  (geo, KV) accumulate by saturating addition into the same slot.
- Sharing: `RequestTimings` is a cheap-clone handle, `Arc<Mutex<Inner>>`. It must cross
  three boundaries: request extensions (adapter entry to core handlers), the streaming
  body closure (records body-phase spans after the response object has been handed
  off), and the adapter's post-send emission read. A poisoned or contended lock must
  never fail a request: all recording methods are infallible and drop the sample on
  lock failure.
- Recording API: `timings.record(Phase::Geo, dur)` and a scope guard
  `timings.span(Phase::Origin)` that records on drop. Guards use saturating duration
  math; a non-monotonic reading records zero rather than panicking.
- Rendering: `server_timing_value(&self) -> Option<String>` produces
  `ts-total;dur=41.2, ts-appbuild;dur=18.4, ts-filter;dur=9.1` with durations in
  milliseconds at one decimal. Phases never recorded are omitted. `ts-total` is
  elapsed-since-`t0` at render time, which by construction is the last mutation before
  send. Returns `None` when nothing was recorded (defensive; `t0` always exists).

`Instant` is already used freely in the guest (`publisher.rs`, `auction/telemetry.rs`),
so no new clock abstraction is needed.

## 6. Span taxonomy and recording sites

| Entry         | Measures                                                                  | Site                                                                                    |
| ------------- | ------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `ts-total`    | adapter entry (post health short-circuit) to header emission              | derived at render                                                                       |
| `ts-appbuild` | config-store open, Settings parse, orchestrator + registry + router build | Fastly `main.rs` around `open_trusted_server_config_store()` + `build_app_with_state()` |
| `ts-filter`   | pre-route request filters, end to end (backend ensure, secret read, POST) | around `run_pre_route_filters` (`app.rs:751`)                                           |
| `ts-geo`      | geo hostcall (single after dedupe; accumulates if any path still repeats) | `build_ec_request_state` (`app.rs:410`)                                                 |
| `ts-kv`       | EC identity KV operations before send                                     | `ec/finalize.rs` ingest path, EC-route KV lookups                                       |
| `ts-origin`   | publisher backend send to response headers available (C1 hit or miss)     | `publisher.rs` around the origin `send` (~line 4496)                                    |
| `ts-c2`       | template cache `lookup_or_reserve`, including the hit-path full-body read | `publisher.rs` around the lookup (~line 4391)                                           |

Row-only fields:

| Field             | Measures                                                 | Site                                              |
| ----------------- | -------------------------------------------------------- | ------------------------------------------------- |
| `auction_wait_ms` | hold at the `</body>` seam waiting on dispatched auction | `collect_stream_auction` / seam wait in publisher |
| `stream_ms`       | headers committed to last body byte                      | adapter around the body-stream drive              |
| `resp_bytes`      | bytes written to the client body                         | same                                              |

## 7. Header emission

The last step of `apply_finalize_headers` (`middleware.rs:197`) appends one
`Server-Timing` header from `server_timing_value()`, gated on
`observability.server_timing_enabled`. Append semantics, never insert: an
origin-supplied Server-Timing survives, and the fronting delivery layer's own entries
(`time-elapsed`, `hit-state`) are additive per the header's list semantics.

Emission runs after the finalize geo resolution and after every configurable response
mutation, so the header reflects everything that happened before send. Both finalize
sites (the router middleware and the adapter entry-point fallback for streaming
responses) emit through the same helper; the `HEADER_X_TS_FINALIZED` marker already
prevents double-finalization, which also prevents a doubled header.

## 8. Geo lookup dedupe (rider)

Today every dispatched request pays two geo hostcalls for one answer: request-phase in
`build_ec_request_state` (`app.rs:410`) and response-phase in
`FinalizeResponseMiddleware` (`middleware.rs:83`, alternate site `main.rs:285`). The
first lookup stashes the resolved `GeoInfo` in request extensions; the finalize path
reads the stashed value and only falls back to a live lookup when the request-phase
never ran. This halves exposure to a degraded geo subsystem, which is one of the three
candidate dependencies for the observed stall window. The 401 short-circuit behavior
(`resolve_geo_for_response` skips lookup for unauthorized responses) is preserved.

## 9. Access telemetry row

Extends the reserved `tinybird/datasources/access_logs_raw.datasource` (in-repo only,
no deployed pipes over it, so this is an edit rather than a migration).

Kept columns: `event_ts`, `method`, `path`, `status`, `time_elapsed_ms` (now defined as
`ts-total`), `cache_state`, `country`, `sample_rate`, `event_date`, 30-day TTL.

Added columns:

```
`route_class`     LowCardinality(String),      -- publisher_html | tsjs | integration_proxy | ec | auction_api | other
`appbuild_ms`     Nullable(UInt32),
`filter_ms`       Nullable(UInt32),
`geo_ms`          Nullable(UInt32),
`kv_ms`           Nullable(UInt32),
`origin_ms`       Nullable(UInt32),
`c2_ms`           Nullable(UInt32),
`auction_wait_ms` Nullable(UInt32),
`stream_ms`       Nullable(UInt32),
`resp_bytes`      Nullable(UInt64),
`c2_state`        LowCardinality(Nullable(String)),  -- x-ts-c2-cache value
`ts_version`      LowCardinality(String),
`pop`             LowCardinality(Nullable(String))   -- FASTLY_POP
```

Null means the phase did not run. `route_class` is assigned by the adapter at handler
selection (the named-route table knows which handler it picked), not by path regex.
Percentile analysis groups by `route_class`; `path` stays for drill-down only.

## 10. Emission mechanics

- Point: Fastly `main.rs`, strictly after `send_edgezero_response` returns (the body
  has fully streamed). The row can never affect TTFB or block delivery.
- Sampling: uniform per-request decision against `tinybird.access_sample_rate`
  (validation `0.0..=1.0` already exists). No client stickiness.
- Transport: one NDJSON row to the Tinybird Events API, mirroring the auction sink
  (`tinybird.rs`): same `api_host`, reserved `access_dataset` and
  `access_token_secret`, 2 s first-byte and between-bytes timeouts, `max_body_bytes`
  guard, best-effort with a warn log on failure, no retry.
- Settings: the guard that rejects `tinybird.access_enabled = true` "until an emitter
  is wired" (`settings.rs:4123` test) flips into a wiring test in the same change, so
  the flag and the emitter land together. `access_enabled = true` requires
  `tinybird.enabled` and a non-empty `api_host`.
- Axum adapter: emits the header via the same core helper; does not emit Tinybird rows
  in v1 (local dev has no Tinybird backend; the sink stays behind the Fastly adapter).

## 11. Pipe and dashboard

- `tinybird/pipes/access_phase_stats.pipe`: endpoint returning p50/p95/p99 of each
  phase column grouped by `route_class` and hour, filterable by `pop` and `ts_version`.
- Dashboard: a new standalone `grafana/dashboards/edge-performance.json` in the
  telemetry repo (`trusted-server-tinybird`), performance only, no panels shared with
  the revenue and auction dashboards. Panels: phase percentiles by route class, stacked
  phase breakdown over time, PoP split, version overlay, C2 hit-rate, and a stall
  panel: rows with `time_elapsed_ms > 500` grouped by dominant phase. Queries hit the
  raw datasource with `$__timeFilter(event_ts)`.

## 12. Config surface

```toml
[observability]
# Append TS phase timings to the Server-Timing response header.
server_timing_enabled = false  # example default

[tinybird]
access_enabled = false         # requires tinybird.enabled and api_host when true
access_sample_rate = 0.0       # 1.0 during active diagnosis, dial down after
```

New `ObservabilitySettings` struct with the single boolean, default off, standard
environment override (`TRUSTED_SERVER__OBSERVABILITY__SERVER_TIMING_ENABLED`).
Collection has no flag: the flags gate the two emission surfaces independently.

## 13. Error handling

- Recording is infallible: saturating math, lock-failure drops the sample, no panics.
- Header rendering failure (invalid header value cannot occur with the fixed format;
  defensive `HeaderValue::from_str` error) logs and skips the header.
- Row emission failure logs a warning and drops the row. The response has already been
  delivered; there is nothing to degrade.

## 14. Testing

- Core unit tests: phase accumulation, saturating math, render format (one decimal,
  omission of unrecorded phases, `ts-total` presence), row serialization shape.
- Adapter tests (Fastly via Viceroy, Axum native): header present and well-formed on a
  publisher route and on the tsjs route with the flag on; absent with the flag off;
  exactly one header after both finalize paths; append preserves a pre-existing
  Server-Timing value.
- Geo dedupe: finalize consumes the stashed request-phase `GeoInfo`; live-lookup
  fallback fires when the request phase never ran; 401 skip preserved.
- Settings: `access_enabled` validation matrix (requires `tinybird.enabled`,
  `api_host`); the former rejection test becomes the wiring test.
- Sink tests: mirror the auction sink's `RecordingHttpClient` pattern; assert URI,
  NDJSON body shape, token header, and that emission is skipped when sampled out.

## 15. Rollout and verification

1. Land collection + header emission behind the flag, off everywhere. Full CI gate.
2. Staging deploy with the flag on. Then the one deployment unknown, verified with a
   single request through the production route: the fronting VCL delivery layer must
   pass the appended Server-Timing through rather than overwrite it. Fallback if it
   clobbers: a one-line VCL change on the delivery service, or mirroring the value to
   `x-ts-timing` while that lands.
3. Production flag on. Confirm
   `performance.getEntriesByType('navigation')[0].serverTiming` shows `ts-*` entries
   in a real browser session; from that moment any RUM tooling on the page contains
   the breakdown.
4. Land the row schema, sink, settings unwiring; sample at 1.0 during stall diagnosis;
   then the pipe and dashboard.
5. Success criterion: the next stall window is attributable from one response header
   or one dashboard query, with no live probing session.

## 16. Overhead

Roughly ten monotonic clock reads and one ~130-byte header per request; one sampled
HTTP POST after the response has fully streamed. No allocation in the hot path beyond
the one `Arc` at entry and the rendered header string at finalize.

## 17. Risks and open questions

- The fronting delivery layer's Server-Timing handling is unverified until the first
  staging deploy (step 2 above). This is the only known external dependency.
- `time_elapsed_ms` semantics change from "unspecified" to "ts-total" on a datasource
  that has never had a deployed writer; acceptable without rename.
- Body-phase capture requires threading the timings handle into the streaming closure
  in `publisher.rs`; the exact seam is an implementation-plan detail, with the
  constraint that a dropped handle (error paths, early client disconnect) must still
  yield a valid row with null body-phase fields.
- The stall window itself remains unattributed until this ships. If it recurs first,
  the bisection runbook from 2026-08-21 (cookie-free curl UA request, `.js` path
  versus HTML path) is the fallback.
