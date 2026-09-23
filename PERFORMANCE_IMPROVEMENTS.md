# TTFB Performance Improvements

Investigation and fixes for time-to-first-byte on the Fastly adapter, driven
by profiling (`perf`) rather than guesswork. All changes verified against the
full CI gate: `cargo fmt`, all 8 `clippy-*` targets, all four adapter test
suites, the cross-adapter parity suite, and the real-Viceroy EC lifecycle
integration test.

## Improvements made

1. **Defer EC-finalize KV write past response send**
   (`ec/finalize.rs`, `main.rs`, `app.rs`)
   Moves the identity-graph KV write (new-visitor creation, consent-withdrawal
   tombstone) from before `send_edgezero_response` to after, mirroring the
   existing pull-sync post-send pattern. Anything the response headers
   actually need (the cookie value) is still decided pre-send, from state
   already proven correct — only the KV persistence moves.

2. **Dedupe JS-bundling work per request**
   (`integrations/registry.rs`, `http_util.rs`, `publisher.rs`)
   - Memoized `js_module_ids_immediate()` / `js_module_ids_deferred()` via
     `OnceLock`, eliminating 2-3x redundant recomputation per registry
     instance.
   - Removed a redundant second SHA-256 pass: `serve_static_with_etag` now
     reuses the hash `concatenated_hash` already computed for the tsjs
     bundle instead of re-hashing the same bytes again for the ETag.

3. **Eliminate duplicate geo lookup** (`app.rs`, `middleware.rs`)
   `FinalizeResponseMiddleware` now reuses the geo lookup already performed
   during EC setup instead of looking up the same client IP a second time.

4. **`strip_cookies` fast path** (`cookies.rs`)
   Skips the full split/filter/rejoin pass and its allocations when none of
   the target cookie names are present in the header — the common case for
   most requests forwarded to Prebid.

5. **Cross-request app-state cache (`AppCache`)** (`app.rs`) — the largest change
   Caches the compiled `Settings` / `AuctionPlan` / `IntegrationRegistry` /
   `RouterService` across requests that land on the same warm Fastly Wasm
   instance, instead of rebuilding all of it from scratch on every request.
   Invalidation is two-layered: a content hash of the config store's raw
   entry invalidates immediately on any config change (no propagation
   delay), and a 60-second TTL backstop bounds staleness from secret
   rotations, which don't change the config blob's raw content and so
   wouldn't otherwise be caught by the content hash.

6. **Cache `template_fingerprint`**
   (`publisher.rs`, `platform/types.rs`, `app.rs`)
   `template_fingerprint(settings)` serializes the *entire* settings struct
   to JSON and SHA-256-hashes it — this was recomputed on every eligible
   publisher request. It's now computed once per `AppState` build and
   threaded through `RuntimeServices` as an optional field (defaults to
   `None`, so Axum/Cloudflare/Spin — untouched by this change — keep
   recomputing it exactly as before).

## Behavior regressions

None found in testing. Two narrow, intentional behavior changes are worth
knowing about — both are deliberate tradeoffs, not bugs:

- **EC-finalize cookie/KV race (#1).** Previously, if the EID-merge KV
  read-back raced and observed a stale `Missing`/`Failed` state immediately
  after a row was written, the response would withhold the cookie. Now the
  cookie is set from the already-proven `Present` snapshot and is not
  retroactively withdrawn if the now-deferred enrichment write races.
  Covered by the existing test
  `finalize_generated_ec_does_not_emit_cookie_for_authoritative_missing_row`,
  which still passes under the new logic.
- **`AppCache` secret staleness window (#5).** A secret rotation (e.g. a
  proxy signing key) can be served from cache for up to 60 seconds after
  rotation, since secrets are resolved separately from the config blob and
  don't change its content hash. This is an explicit, bounded tradeoff (the
  TTL backstop), not unbounded staleness — tune `APP_CACHE_TTL` in `app.rs`
  if a tighter window is wanted.

## Performance: before → after

### I/O deferrals (#1, #3) — structurally correct, not visible under Viceroy

| Fix | Viceroy TTFB before → after |
|---|---|
| Defer EC-finalize KV write | 2.43ms → 2.41ms (flat) |
| Dedupe geo lookup | 2.45ms → 2.44ms (flat) |

Both remove a network round-trip to Fastly's KV/geo services. Viceroy's local
KV/geo backends are in-memory with near-zero latency, so there's no real
round-trip to save locally — the fixes are correct (full test coverage,
verified request-path exercise) but their real win only shows up against
production network-latency backends.

### CPU-bound work removed (#2, #5, #6) — measurable, real wins

| Fix | Before | After | Delta |
|---|---|---|---|
| Memoized `js_module_ids` (same-run comparison) | 84-89 ns/call | 12 ns/call | **~6-7x faster** |
| `AppCache`, settings/registry-dominated endpoint | ~4,800-8,100 perf samples / 400 reqs | ~0 samples / 400 reqs | Near-total elimination |
| `AppCache` + `template_fingerprint`, full page render (`perf stat`, precise cycle count) | 6,470,830,488 cycles / 400 reqs | 6,384,503,694 cycles / 400 reqs | **~1.3% fewer cycles** |

The full-page-render number is smaller because HTML rewriting, origin fetch,
and auction dispatch dominate a real page render's total cost — the same
absolute savings is a smaller slice of a bigger pie than on a route that does
almost nothing but settings/registry work. Wall-clock TTFB for the full
render stayed ~2.3-2.4ms either way; the CPU saving is too small relative to
total request latency to show up through `curl`'s own timing noise.
