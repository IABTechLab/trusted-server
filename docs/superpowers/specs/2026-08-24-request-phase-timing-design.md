# Request phase timing: Server-Timing subtimings and access telemetry

**Date:** 2026-08-24
**Status:** Approved design, revised for review rounds 1 and 2, pending implementation
plan.
**Scope:** `trusted-server-core`, Fastly and Axum adapters, `tinybird/` schema,
performance dashboard (separate repo).

---

## 1. Problem

On 2026-08-21 a production deployment (publisher redacted, `prospect-a.example`) showed
an episodic stall: for a window of roughly 40 minutes, every request that reached the
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

The Compute CPU budget is ~50 ms per request, so a large `time-elapsed` strongly
suggests wall-clock time outside active guest CPU: dependency awaits are the leading
explanation, with platform scheduling and hostcall queueing as the residual ones. The
comparison figure here is the fronting delivery layer's `time-elapsed` Server-Timing
entry, observed at its deliver phase. Either way, these are exactly the numbers a
response can carry about itself.

## 2. Goals

1. Every normal application response attributes its own server time by phase in a
   standard header. Browsers expose the values to same-origin JavaScript via
   `PerformanceResourceTiming.serverTiming`, so RUM tooling that reads that API can
   surface the breakdown. Whether a given vendor or the publisher's own monitoring
   extension actually collects it is verified separately in rollout; the publisher
   extension needs a small change to render it.
2. The same numbers flow to Tinybird so we hold p50/p95/p99 per phase, per route class,
   per PoP, per deployed version, and a future stall window self-diagnoses in one query.
3. No additional awaited I/O before first byte. The pre-send cost is a handful of
   monotonic clock reads, one small allocation at entry, and rendering one header;
   telemetry emission happens strictly after the last body byte.

Scope note: phases cover the application lifecycle after T0. The `/health` and
`/_ts/debug/ja4` short-circuits, config-store open failures, and request-conversion
failures bypass the lifecycle and emit nothing. Requests served entirely by the
fronting cache never reach the guest and produce neither header entries nor rows.

## 3. Non-goals

- No trailer-based Server-Timing for body-phase spans (browsers do not expose trailer
  values to JavaScript).
- No per-filter naming in any emitted surface. The request-filter span is `ts-filter`
  regardless of which filter runs; vendor identity stays out of headers and telemetry.
- No Cloudflare or Spin emission wiring in v1. Core collection is adapter-neutral; those
  adapters can wire emission later without core changes.
- No Tinybird endpoint pipe and no rollup materialized views in v1. Grafana queries the
  datasource through the ClickHouse connector, matching the auction dashboards; rollups
  only if panel latency demands them.
- No sampling of the header. The header is all-traffic when enabled; only Tinybird rows
  sample.
- No cross-request circuit breaker for telemetry emission. Compute runs one isolate per
  request; there is no shared mutable state to hold breaker state. The controls are the
  bounded per-request cost and the `access_sample_rate` lever (section 10).

## 4. Design overview

```
adapter entry (T0)
  |  RequestTimings::new() -> shared handle
  v
app construction ................ ts-appbuild        (adapter)
pre-route request filters ....... ts-filter          (adapter wrapper)
geo lookup (single, deduped) .... ts-geo             (adapter; result carried forward)
template cache lookup ........... ts-template-cache  (core: publisher.rs)
origin fetch to resp headers .... ts-origin          (core: publisher.rs)
EC identity KV, pre-send ........ ts-kv              (core: KV abstraction)
auction wait, buffered mode ..... auction_wait_ms    (row only; pre-header in this mode)
  |
send_edgezero_response, immediately before into_parts():
  mark_headers_ready() snapshot (unconditional)
  build AccessTelemetrySnapshot (unconditional)
  append Server-Timing header (flag-gated, only on conclusively private responses)
  |
headers committed; body streams
  auction hold at seam .......... auction_wait_ms    (row only; in-stream in this mode)
  stream duration, bytes ........ stream_ms, resp_bytes (row only)
  |
post-send (adapter main):
  request_elapsed snapshot, then existing pull-sync, then:
  sample gate -> one NDJSON row -> Tinybird Events API
  bounded response await, 2xx validated
```

Collection is always-on and flag-free, including the `mark_headers_ready()` snapshot.
Two independent flags gate emission: the header (`observability.server_timing_enabled`)
and the telemetry row (`tinybird.access_enabled`).

## 5. `RequestTimings` (core)

New module `crates/trusted-server-core/src/request_timing.rs`.

- `Phase`: a closed enum: `AppBuild`, `Filter`, `Geo`, `EcKv`, `Origin`,
  `TemplateCacheLookup`, `AuctionWait`, `Stream`. Header rendering covers the first six
  plus the stored total; the last two are row-only.
- Inner state: one fixed-size array of `Option<Duration>` slots indexed by phase,
  `t0: Instant`, `headers_ready_total: Option<Duration>`,
  `auction_wait_placement: Option<AuctionWaitPlacement>` (`PreHeader` or `InStream`),
  and `resp_bytes: Option<u64>`. Phases that repeat within a request (geo, KV)
  accumulate by saturating addition into the same slot.
- `mark_headers_ready()`: stores `t0.elapsed()` once at the response-commit boundary,
  unconditionally, before either emission flag is consulted. The header renders this
  stored value as `ts-total`; the telemetry row reads the same stored value as
  `time_elapsed_ms`. The two surfaces cannot disagree, and the row stays correct when
  the header flag is off. Full request duration is captured separately as
  `request_elapsed_ms`, snapshotted immediately after the body-stream drive returns and
  before any other post-send work, so pull-sync and telemetry emission are never
  included in it.
- Sharing: `RequestTimings` is a cheap-clone handle, `Arc<Mutex<Inner>>`. It crosses
  three boundaries: adapter entry to core handlers, the streaming body closure (records
  body-phase spans after the response object has been handed off), and the adapter's
  post-send emission read. A poisoned or contended lock must never fail a request: all
  recording methods are infallible and drop the sample on lock failure.
- Recording API: `timings.record(Phase::Geo, dur)` and a scope guard
  `timings.span(Phase::Origin)` that records on drop. Guards use saturating duration
  math; a non-monotonic reading records zero rather than panicking. The auction-wait
  recorder takes the placement explicitly so the two modes cannot be conflated.
- Rendering: `server_timing_value(&self) -> Option<String>` produces
  `ts-total;dur=41.2, ts-appbuild;dur=18.4, ts-filter;dur=9.1` with durations in
  milliseconds at one decimal. Phases never recorded are omitted. Returns `None` when
  `mark_headers_ready()` has not run.

`Instant` is already used freely in the guest (`publisher.rs`, `auction/telemetry.rs`),
so no new clock abstraction is needed.

## 6. Span taxonomy and recording sites

| Entry               | Measures                                                                              | Site                                                                                    |
| ------------------- | ------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `ts-total`          | T0 to `mark_headers_ready()` at the response-commit boundary                          | stored snapshot                                                                         |
| `ts-appbuild`       | config-store open, Settings parse, orchestrator + registry + router build             | Fastly `main.rs` around `open_trusted_server_config_store()` + `build_app_with_state()` |
| `ts-filter`         | pre-route request filters, end to end (backend ensure, secret read, POST)             | around `run_pre_route_filters` (`app.rs:751`)                                           |
| `ts-geo`            | geo hostcall (single after dedupe; accumulates if any path still repeats)             | `build_ec_request_state` (`app.rs:410`) and timed finalizer fallback lookups            |
| `ts-kv`             | EC identity KV operations before response send (see enumeration below)                | the shared KV abstraction                                                               |
| `ts-origin`         | publisher backend send to response headers available (read-through cache hit or miss) | `publisher.rs` around the origin `send`                                                 |
| `ts-template-cache` | template cache `lookup_or_reserve`, including the hit-path full-body read             | `publisher.rs` around the lookup                                                        |

Naming follows the completed template-cache terminology migration (`x-ts-template-cache`
is the emitted header on `main`; `c2` naming is retired).

`ts-kv` is instrumented at the shared identity-graph/KV abstraction
(`KvIdentityGraph` and the platform KV store wrapper), not at individual call sites, so
new callers cannot silently escape the span. Included pre-send operations: EC
generation `create_or_revive`, identify-path graph reads and evaluation, finalize-path
`ingest_eid_cookies`/`upsert_partner_ids` and withdrawal tombstones, consent-store
reads on consent routes, and batch-sync graph access when it runs before send.
Explicitly excluded: pull-sync work, which runs strictly after `send_to_client` and is
invisible to both surfaces. The timings handle reaches `ec_finalize_response` through
an existing context or a parameter object; that function already has the repository
maximum of seven arguments and does not gain an eighth.

Row-only fields:

| Field                | Measures                                                  | Site                                             |
| -------------------- | --------------------------------------------------------- | ------------------------------------------------ |
| `auction_wait_ms`    | wait on the dispatched auction (placement varies by mode) | seam hold (streaming) or buffered finalizer wait |
| `body_mode`          | `streamed` or `buffered` response assembly                | set where the response body is built             |
| `stream_ms`          | headers committed to last body byte                       | adapter around the body-stream drive             |
| `resp_bytes`         | bytes written to the client body                          | same                                             |
| `request_elapsed_ms` | T0 to immediately after the body-stream drive returns     | post-send snapshot, before pull-sync             |

Auction-wait placement is not universal. On the ordinary streaming path the wait
happens at the `</body>` seam inside the body stream and nests inside `stream_ms`. On
buffered paths (the Fastly shared-template authorized miss, which buffers the full
transform and auction before returning a response, and every Axum response) the wait
completes before headers commit. The row therefore carries `body_mode` plus
`auction_wait_placement` (`pre_header` or `in_stream`), and derivations are
conditional:

- `in_stream`: `stream_other_ms = greatest(coalesce(stream_ms, 0) - coalesce(auction_wait_ms, 0), 0)`.
- `pre_header`: `auction_wait_ms` joins the pre-header phase set, and `stream_other_ms = coalesce(stream_ms, 0)`.

`unattributed_ms = greatest(coalesce(time_elapsed_ms, 0) - (coalesce(appbuild_ms, 0) +
coalesce(filter_ms, 0) + coalesce(geo_ms, 0) + coalesce(kv_ms, 0) +
coalesce(origin_ms, 0) + coalesce(template_cache_ms, 0) + pre-header auction wait), 0)`.
Every phase column is nullable, so every query-time formula wraps each term in
`coalesce(column, 0)` and every subtraction in `greatest(..., 0)`; query tests cover
sparse phase combinations.

## 7. Freeze point and header emission

The freeze-and-emit point is `send_edgezero_response` (Fastly `main.rs`), immediately
before `response.into_parts()`. This is the single choke point every send path shares,
and it runs after everything that can still mutate the response: the router middleware,
entry-point finalize (`apply_finalize_headers`, asset-policy reapplication), EC
finalization and its KV work, and terminal filter/privacy effects.
`apply_finalize_headers` itself does not emit; the `HEADER_X_TS_FINALIZED` sentinel
marks middleware finalization, not header commitment, and must not be treated as the
timing boundary.

At the freeze point, in order: `mark_headers_ready()` (unconditional), the
`AccessTelemetrySnapshot` build (unconditional, section 10), then, gated on
`observability.server_timing_enabled`, append one `Server-Timing` header from
`server_timing_value()`. Append semantics, never insert: an origin-supplied
Server-Timing survives, and the fronting delivery layer's own entries (`time-elapsed`,
`hit-state`) are additive per the header's list semantics.

Header emission is conservative: it happens only when the response is conclusively
non-storable by any shared cache, meaning `Cache-Control` contains `private` or
`no-store` (the existing `cache_control_headers_are_private_or_no_store` predicate).
Anything else, including bare `max-age`, `s-maxage` without `private`,
heuristically-cacheable responses with no cache header at all, and anything a fronting
cache override might store, emits no header, because a stored object would replay one
request's timings for its full lifetime. The long-lived immutable `tsjs` asset route
is the concrete excluded case. The snapshot and the telemetry row are unaffected by
this skip, so excluded routes still report through Tinybird.

The Axum adapter applies the same emission rule at its terminal point before response
serialization, with adapter-specific phase semantics (section 8a).

## 8. Geo lookup dedupe (rider)

Today every dispatched request pays two geo hostcalls for one answer: request-phase in
`build_ec_request_state` (`app.rs:410`) and response-phase in
`FinalizeResponseMiddleware` (`middleware.rs:83`, alternate site `main.rs:285`).

Plain request extensions cannot carry the result out: the middleware moves the request
context into `next.run(ctx)` and holds only the response afterward. The resolved geo
travels on a dedicated `GeoLookupState` response extension, attached on every exit
path that attempted a lookup, including the asset fallback, which runs
`build_ec_request_state` and then returns without `EcFinalizeState` (which is why
`EcFinalizeState` is not an acceptable carrier). States: `NotAttempted`,
`Attempted(None)` (lookup ran and failed, do not retry), and `Resolved(GeoInfo)`. The
finalize path consumes the carried value and performs a live lookup only in the
`NotAttempted` state; those legitimate fallback lookups (admin, batch, error paths)
are themselves timed into `ts-geo` so degraded geo cannot hide inside
`unattributed_ms`. The 401 rule (`resolve_geo_for_response` skips lookup for
unauthorized responses) is preserved.

## 8a. Adapter phase semantics

The Fastly adapter is the reference implementation of the taxonomy. Axum differs
structurally and its emissions are defined accordingly rather than pretending parity:

- `ts-appbuild` is absent: Axum builds application state once at startup.
- `body_mode` is always `buffered`: the Axum HTTP client buffers upstream bodies, so
  `stream_ms` measures buffered-body write-out and `auction_wait_placement` is always
  `pre_header`.
- The freeze point is an Axum-terminal middleware layer that runs after all response
  mutation, covering router-generated 404/405 responses; Axum's `/health` sits inside
  the middleware stack and is excluded explicitly by route match.
- Axum emits the header only; no Tinybird rows in v1 (unchanged).

Cloudflare and Spin: collection compiles, no emission wiring in v1 (unchanged).

## 9. Access telemetry row

Extends the reserved `tinybird/datasources/access_logs_raw.datasource`.

Kept columns: `event_ts`, `method`, `status`, `time_elapsed_ms` (defined as the
`mark_headers_ready()` snapshot), `sample_rate`, `event_date`, 30-day TTL.

Removed: raw `path`. Route identifiers like `/_ts/admin/ec/{id}` would otherwise put
EC identifiers into a 30-day dataset, and publisher paths carry unbounded cardinality
and user-generated content (search terms, usernames, emails in slugs). Replaced by
`route_template`:

- Named routes: the matched route-table pattern verbatim, parameters left as
  placeholders.
- Publisher fallback: a coarse fixed template, `/` plus the first path segment
  restricted to a bounded allowlisted charset, plus `/*` when deeper (for example
  `/news/*`). The auction-telemetry normalizer is explicitly not sufficient here: it
  redacts long tokens but preserves short identifiers and arbitrary slugs.
- Tests are adversarial, not just the happy path: a literal EC identifier on the admin
  route, an email address in a path segment, search-term-shaped segments, and
  overlong segments must all normalize to bounded, content-free templates.

Added columns (all dimension columns non-nullable with an `unknown` sentinel, because
ClickHouse sorting keys cannot contain nullable columns):

```
`service_id`            LowCardinality(String),      -- FASTLY_SERVICE_ID; immutable deployment identity
`publisher_domain`      LowCardinality(String),      -- matches auction schema
`env`                   LowCardinality(String),      -- from typed settings state, never from a response header
`route_class`           LowCardinality(String),      -- publisher_html | tsjs | integration_proxy | ec | auction_api | other
`route_template`        String,                      -- bounded, normalized; replaces path
`body_mode`             LowCardinality(String),      -- streamed | buffered
`auction_wait_placement` LowCardinality(String),     -- pre_header | in_stream | none
`appbuild_ms`           Nullable(UInt32),
`filter_ms`             Nullable(UInt32),
`geo_ms`                Nullable(UInt32),
`kv_ms`                 Nullable(UInt32),
`origin_ms`             Nullable(UInt32),
`template_cache_ms`     Nullable(UInt32),
`auction_wait_ms`       Nullable(UInt32),
`stream_ms`             Nullable(UInt32),
`request_elapsed_ms`    Nullable(UInt32),
`resp_bytes`            Nullable(UInt64),
`template_cache_state`  LowCardinality(String),      -- from the typed response extension, not the public header
`country`               LowCardinality(String),
`ts_version`            LowCardinality(String),
`pop`                   LowCardinality(String)       -- FASTLY_POP, 'unknown' when absent
```

Typed sources only: `env` comes from settings state (production configs may set no
header), `template_cache_state` from the typed response extension rather than the
`x-ts-template-cache` header (operator-configured response headers can override
managed headers), `service_id` and `pop` from the Fastly environment. `cache_state`
from the reserved schema is dropped: the guest cannot observe the fronting cache, and
guest-visible cache behavior is already carried by `template_cache_state` and
`origin_ms`. Rows exist only for guest-handled requests; fronting-cache hits are
invisible by construction and the dashboard documentation says so.

Sorting key: `(event_date, service_id, publisher_domain, env, route_class, pop,
status)`. Grafana time filtering uses `$__timeFilter(event_ts)` and every panel query
also carries an `event_date` predicate so the primary index prunes; rollout validates
the panel queries with `EXPLAIN` before the dashboard is committed. This replaces the
reserved key `(event_date, path, status, method)`. Rollout step 4 verifies whether the
reserved datasource was ever deployed to the remote workspace; if it was, this schema
ships as a versioned replacement datasource with a cutover, not an in-place edit.

## 10. Emission mechanics

- `AccessTelemetrySnapshot`: built unconditionally at the freeze point, before
  `into_parts()` consumes the response. It captures method, status, route metadata
  (`route_class`, `route_template`), typed dimension states (`env`,
  `template_cache_state`, geo country), and the transport context needed to emit
  (backend spec, secret store name, dataset, token secret). It exists because nothing
  else survives to post-send on every path: the request is consumed by dispatch, the
  response by `into_parts()`, and `EcFinalizeState` is absent on asset, admin, and
  error paths.
- `send_edgezero_response` returns the delivery outcome (bytes written and
  success/partial/error) instead of `()`; the outcome feeds `resp_bytes` and lets a
  partial delivery be recorded rather than silently averaged in.
- Ordering after the body-stream drive returns: snapshot `request_elapsed_ms` first,
  run the existing pull-sync dispatch unchanged, then telemetry emission last, so
  pull-sync is never delayed behind the ingest await and never included in
  `request_elapsed_ms`.
- Sampling: uniform per-request decision against `tinybird.access_sample_rate`. No
  client stickiness. Sampled-out requests are silent; every other drop (row build
  failure, send failure, non-2xx) logs one warning naming the reason. There is no
  cross-request warning suppression (per-request isolates hold no shared state); the
  overload controls are the 2 s bounded await, the single-warning-per-request cap, and
  `access_sample_rate` pushed down by config as the operational abort lever. Ingest
  health is monitored from the Tinybird side via ingestion freshness on the
  datasource, which catches quarantine and schema rejection that per-request warnings
  cannot.
- Transport: one NDJSON row to the Tinybird Events API: same `api_host`, reserved
  `access_dataset` and `access_token_secret`, 2 s first-byte and between-bytes
  timeouts, `max_body_bytes` guard, no retry.
- Delivery confirmation: unlike the auction sink, which starts `send_async` and drops
  the pending response (it runs before delivery completes and cannot afford to wait),
  the access emitter runs after the client has the full response and therefore awaits
  the bounded ingest response and validates 2xx. A non-2xx or timeout logs a warning
  with the status.
- Budget: at `access_sample_rate = 1.0` this adds one backend request per request to
  the service, after delivery; during a Tinybird outage each such request holds its
  sandbox for up to the bounded timeout. The sample rate is the budget control; 1.0 is
  a diagnosis setting, not a steady state, and rollout treats sustained emission
  warnings as the signal to dial it down.
- Axum adapter: emits the header only; no Tinybird rows in v1.

## 11. Dashboard and query model

No endpoint pipe in v1. Grafana queries `access_logs_raw` directly through the
ClickHouse connector with `$__timeFilter(event_ts)` plus an `event_date` predicate,
matching the auction dashboards.

Dashboard: a new standalone `grafana/dashboards/edge-performance.json` in the
telemetry repo (`trusted-server-tinybird`), performance only, no panels shared with
the revenue and auction dashboards. Panels:

- Phase percentiles (p50/p95/p99) by `route_class`, per phase column.
- Stacked phase breakdown over time using the non-overlapping set: `appbuild_ms`,
  `filter_ms`, `geo_ms`, `kv_ms`, `origin_ms`, `template_cache_ms`, pre-header
  auction wait (where `auction_wait_placement = 'pre_header'`), and derived
  `unattributed_ms`. In-stream auction wait and derived `stream_other_ms` chart in a
  separate body-phase panel and never stack with pre-header phases.
- PoP split, `ts_version` overlay, template-cache state rates.
- Stall panel: rows with `request_elapsed_ms > 500` (post-body total, so body-only
  stalls are caught) grouped by dominant phase, where `unattributed_ms` competes as a
  phase so the panel cannot confidently blame a small measured span while most time is
  uninstrumented.

All derivations use the `coalesce`/`greatest` forms from section 6; query tests cover
sparse phase combinations and both `auction_wait_placement` modes.

Sampling semantics for every aggregate: `sample_rate` must be operationally stable
within any queried window. Quantile panels filter strictly to a single `sample_rate`
value. Volume panels weight each row by `1.0 / sample_rate` (the inverse-probability
estimator is `sum(1.0 / sample_rate)` over emitted rows; `count() / rate` is valid
only when the query is already filtered to one rate). Pooled unweighted quantiles
across a rate change are documented as invalid.

## 12. Config surface

```toml
[observability]
# Append TS phase timings to the Server-Timing response header.
server_timing_enabled = false  # example default
```

New `ObservabilitySettings` struct with the single boolean, default off, standard
environment override (`TRUSTED_SERVER__OBSERVABILITY__SERVER_TIMING_ENABLED`).
Collection has no flag: the flags gate the two emission surfaces independently.

Tinybird flag structure: `tinybird.enabled` today arms the auction sink by itself, so
"enable Tinybird for access telemetry" would silently enable auction emission too. The
master flag is demoted to transport-only (host, store, credentials), and each emitter
gets its own switch: a new `tinybird.auction_enabled` defaulting to `true` (preserving
current behavior for existing configs) and the reserved `tinybird.access_enabled`
defaulting to `false`. A settings test locks the decoupling in both directions.

Validation when `access_enabled = true`: `tinybird.enabled`, non-empty `api_host`,
non-empty `secret_store`, `access_dataset`, and `access_token_secret`, a positive
`max_body_bytes`, and `access_sample_rate > 0`. An armed-but-silent configuration
(`access_enabled = true`, `access_sample_rate = 0`) is a configuration error, not a
valid state; disabling is done with the flag, not the rate.

Rollback and compatibility, because `Settings` is `deny_unknown_fields`:

- Deployment order is binary first, config second. Rollback order is config first
  (remove the `[observability]` table and any new tinybird keys), binary second. A
  config containing the new fields must never be pushed while a pre-observability
  binary can still run.
- Config serialization omits the table when it equals the default, so round-tripping a
  config through tooling does not inject a field an older binary rejects. A
  compatibility test asserts the serialized default config parses under the previous
  schema.
- The environment-variable overlay cannot create a missing leaf, so the key ships
  present-but-false in the base operator TOML (the same pattern the GPT integration
  documents in `trusted-server.example.toml`) and is flipped by config push.

## 13. Error handling

- Recording is infallible: saturating math, lock-failure drops the sample, no panics.
- Header rendering failure (defensive `HeaderValue::from_str` error) logs and skips
  the header.
- Row emission failure logs one warning naming the reason and drops the row. The
  response has already been delivered; there is nothing to degrade.

## 14. Testing

- Core unit tests: phase accumulation, saturating math, `mark_headers_ready()`
  idempotence and both-surface consistency, render format (one decimal, omission of
  unrecorded phases), row serialization shape, auction-wait placement recording.
- Adapter tests (Fastly via Viceroy, Axum native): header present and well-formed on a
  conclusively-private publisher route with the flag on; absent with the flag off;
  absent on the shared-cacheable tsjs route and on a bare `max-age` response with the
  flag on; exactly one TS-owned metric set (a single `ts-total`) with every
  pre-existing Server-Timing value preserved, across all send paths; `ts-kv` captures
  EC finalize work (proving the freeze point sits after it).
- Body-mode tests: ordinary streaming (in-stream wait nested in `stream_ms`),
  Fastly shared-template authorized miss (buffered, pre-header wait), and Axum
  (always buffered), each asserting placement and non-negative derivations.
- Geo dedupe: finalize consumes `Resolved`; no retry on `Attempted(None)`; live
  lookup only on `NotAttempted`; fallback lookups timed into `ts-geo`; asset-fallback
  path carries `GeoLookupState` without `EcFinalizeState`; 401 skip preserved.
- Route template: adversarial normalization tests (literal EC identifier on the admin
  route, email address in a segment, search-term segments, overlong segments) all
  producing bounded content-free templates.
- Settings: the access validation matrix including the armed-but-silent rejection;
  auction/access flag decoupling in both directions; the former rejection test becomes
  the wiring test; the serialized-default-config compatibility test against the
  previous schema.
- Sink tests: `RecordingHttpClient` pattern; assert URI, NDJSON body shape, token
  header, 2xx validation and warning on non-2xx, skip when sampled out, ordering after
  pull-sync.
- Query tests: derivation formulas against sparse rows and both placements.

## 15. Rollout and verification

1. Land collection + freeze point + header emission behind the flag, off everywhere.
   Full CI gate.
2. Staging deploy with the flag on. Delivery-layer verification is two-sided: a
   pass-through request confirming the appended Server-Timing survives the fronting
   VCL, and a MISS-then-HIT replay against a cacheable route confirming no stale
   timing header is ever served from cache. Fallback if the VCL clobbers the header: a
   one-line VCL change on the delivery service, or mirroring the value to
   `x-ts-timing` while that lands.
3. Production flag on. Confirm
   `performance.getEntriesByType('navigation')[0].serverTiming` shows `ts-*` entries
   in a real browser session, and separately confirm what the publisher's RUM tooling
   actually collects; the publisher monitoring extension renders it only after a small
   change on their side.
4. Verify whether `access_logs_raw` exists in the remote Tinybird workspace. If yes,
   ship the schema as a versioned replacement with cutover; if no, edit in place.
   Validate the dashboard panel queries with `EXPLAIN` against the sorting key. Then
   land the row schema, sink, and settings changes; sample at 1.0 during stall
   diagnosis with ingestion-freshness monitoring on the datasource; then the
   dashboard.
5. Success criterion: the next stall window is attributable from one response header
   or one dashboard query, with no live probing session.

## 16. Overhead

Roughly ten monotonic clock reads, two stored snapshots, and one ~130-byte header per
request; one sampled HTTP POST with a bounded await after the response has fully
streamed. No allocation in the hot path beyond the one `Arc` at entry, the
`AccessTelemetrySnapshot` at the freeze point, and the rendered header string.

## 17. Decisions and open questions

- **Public exposure is a decision, not an open question.** The header is all-traffic
  when enabled. Rationale: values are durations only; the delivery layer already
  exposes `hit-state` and `time-elapsed` publicly on every response; filter vendor
  identity is masked; emission is restricted to conclusively-private responses so no
  cache can replay stale timings. Revisit (quantization or gating) only if a concrete
  abuse surfaces.
- The fronting delivery layer's Server-Timing pass-through is unverified until the
  first staging deploy (step 2). This is the only known external dependency.
- Body-phase capture threads the timings handle into the streaming closure in
  `publisher.rs`; the exact seam is an implementation-plan detail, with the constraint
  that a dropped handle (error paths, early client disconnect) must still yield a
  valid row with null body-phase fields and a recorded delivery outcome.
- The stall window itself remains unattributed until this ships. If it recurs first,
  the bisection runbook from 2026-08-21 (cookie-free curl UA request, static-asset
  path versus HTML path) is the fallback.
