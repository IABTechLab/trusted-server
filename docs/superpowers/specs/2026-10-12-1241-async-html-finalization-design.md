# Ordered asynchronous HTML processor completion

**Issue:** #1241. **Status:** implemented; shared-template caching for async output is deferred to #1397.

The [implementation plan](../plans/2026-10-12-1241-async-html-finalization.md) maps this contract to implementation and verification. Fresh executed evidence is recorded in PR #1290.

## 1. Requirement and scope

HTML integrations receive synchronous `process_chunk(bytes, is_last)` calls but have
no supported opportunity to await HTTP or KV after document EOF. Provide optional
asynchronous processor completion. Keep chunk processing synchronous. A processor's
completed output must reach downstream processors before their own EOF. Article
signing is an example, not an API requirement or a framework responsibility.

Trusted Server owns processor composition, publisher driving, auction assembly,
cache admission and request services. No EdgeZero repin is needed for EOF completion.
Changing buffered adapters into streaming adapters is separate work.

This revision does not implement signing, verification endpoints, queues, key
management, background jobs, publisher CMS changes, async parser callbacks,
per-chunk I/O, a second parser, or insertion into already emitted HTML positions.

## 2. One processor lifecycle

`StreamProcessor` gains an object-safe optional `async_finish` returning a
`LocalBoxFuture`. The default returns `None` without allocating a future. Its input
borrows driver-owned synchronous EOF output, a separate bounded replacement writer,
actual response status, request `RuntimeServices`, and a shared absolute deadline.
It may borrow processor state and need not be `Send`. A capability method lets
synchronous drivers reject unsupported async processing before effects.

`StreamFinishInput.output` is un-emitted output eligible for replacement. It is a
streaming processor's EOF tail or an explicitly buffered processor's retained
output; it is not universally the complete document. Outcomes are `Unchanged` and
`Replace`. Empty replacement is intentional. Return the future promptly and perform
I/O inside it. Bytes already delivered cannot be replaced or rolled back.

Use the existing `IntegrationHtmlStreamProcessorFactory` and one registration
sequence. Factory metadata declares async EOF capability and optional retention of
its synchronous output, both false by default. Reject retention without async
capability. Metadata and the created session's capability must agree; undeclared
async work must never silently disappear through a synchronous driver.

Use one production processor lifecycle without a separate finalizer factory,
session trait or registration list. Tests use ordinary `StreamProcessor` fixtures
for this lifecycle.

Factory metadata is available before template lookup, without creating a session,
reading HTML or polling a future. Every session uses the existing request-local
`IntegrationDocumentState`; state must not survive requests or reusable sandboxes.

## 3. Operator acceptance of latency

Add `publisher.allow_async_html_finish`, default `false`. An enabled integration
requiring async EOF without this permission produces an actionable configuration
error identifying the integration and setting. Do not silently omit required work.
A true setting with no async factories leaves the old processing path intact.

Check permission at registry construction and defend direct publisher entry points
using manually constructed registries before consuming a body or committing
processed output. Factory retention metadata specifies whether core withholds output;
permission accepts the possible delivery cost of async EOF, including on buffered
adapters. It is not a guarantee of background execution after request completion.

## 4. Ordered EOF driver

For no-hook pages, preserve existing composition, chunk boundaries, streaming and
auction seam timing. Create no future, collector or timer.

For opted-in HTML:

1. Decode and validate origin bytes under existing limits. Run parser and processor
   chunks synchronously in their existing order.
2. Complete decoder/parser EOF. Feed their remaining output downstream with
   `is_last = false` and release ready bytes before optional I/O.
3. Finish the next integration processor exactly once with `is_last = true`.
   Keep its synchronous EOF output outside its optional borrowing future.
4. Await optional finish under the remaining shared deadline. Commit an accepted
   bounded candidate or retain the synchronous fallback.
5. Feed the selected output through later processors as ordinary input. Release
   ready bytes before finishing the next stage. Repeat steps 3–5.
6. After all processors complete, assemble the auction and finish the encoder.

Use a stepped concrete HTML chain in the async publisher driver. Do not await one
composite future that holds all EOF output. Race only a leaf callback, never the
whole chain: timeout must still complete downstream synchronous EOF.

The generic `StreamingPipeline` remains synchronous. Its entry point, synchronous
publisher helpers and direct composite EOF reject async-capable use before reader,
parser or sink effects. Both materialized and streamed bodies reach the ordered driver.
Raw partial-response copying remains a documented exception because it executes no
processor or callback; pending auctions must still use an async entry point.

## 5. Optional bounded retention

For factories requesting retention, core owns a collector for that stage's synchronous
output. Use `publisher.max_buffered_body_bytes` (existing 16 MiB default), checked
arithmetic and incremental allocation. At EOF, move the retained output into the
fallback and lend a slice to the callback. Candidate storage has the same cap.

These are per-holder limits, not an aggregate request-memory budget. Retained stage output,
fallback/candidate, auction suffix, codec state and buffered adapter responses can coexist.
Total memory may exceed this setting; integrations also own allocations core cannot bound.
Candidates run one at a time and replay moves/slices selected output. Do not infer a fixed
peak-memory multiplier or a total heap bound from the body-byte limit.

Overflow permanently disables that stage's optional EOF I/O. Replay retained prefix,
crossing chunk and later output exactly once through downstream stages, then finish
its synchronous EOF normally. It does not authorize an oversized outer buffered
response. Core bounds its holder and candidate, not arbitrary private allocations
inside integration implementations.

After overflow, the collector is consumed into ordinary ready output. The existing
`Vec<u8>` chunk interface may coalesce the prefix and crossing chunk above this
optional retention cap; ordinary output and outer buffered-response limits still
apply. The cap bounds withheld output, not every transient streaming chunk.

Without retention, emit ordinary stage output immediately and keep only its EOF
fallback while awaiting. If fallback exceeds the optional-work cap, deliver it under
the ordinary output policy and skip that stage's optional I/O. Do not truncate it.

## 6. Deadlines, failures and cancellation

Retain `publisher.html_finalization_timeout_ms`: default 5000, valid 1–60000.
The initial default follows the 5-second budget in the issue example; 60 seconds is
a configuration ceiling. Neither value is a measured optimum or a response latency
guarantee. Operators explicitly accept and tune this budget when enabling EOF work.
One absolute budget begins after decoder/parser EOF and covers every optional finish.
Share one timer across stages, initializing its delay with the remaining budget
immediately before the first callback is polled. The deadline wins readiness ties. Check before polling
callbacks and before accepting output.

Callback errors, poisoned candidates, invalid seam-marker multiplicity and late results
discard only that candidate. For each stage, retain at most marker-length minus one
bytes of its previously emitted output and validate token counts across that boundary
plus the EOF fallback/candidate. This catches split-token creation/removal without
retaining the page. Buffered and overflow-bypassed stages use the same boundary rule.
Preserve the current fallback allocation and earlier
accepted output. On expiry, drop the leaf future, skip later optional I/O, and still
finish all later synchronous stages. Access borrowed buffers only after dropping
that future. Callback panics propagate.

Origin, parser, synchronous processor, codec and sink failures remain fatal. Client
cancellation drops local work; preserve existing auction diagnostics. No detached
futures, retries or background execution are implied.

Axum, Workers and Spin supply cooperative timers. Fastly supplies none: consumers
must bound blocking host calls independently. Timers cannot preempt blocking calls
or CPU work that never yields; do not promise universal hard deadline release.

## 7. Auction and encoding

Keep auction dispatch and its transport deadline unchanged. On enabled EOF paths,
complete processor finishes before auction collection/assembly. Release available
prefix bytes before waiting. Preserve exact-token seam handling, missing-body-end
fallback, winning bids, encoder ordering and abandonment telemetry.

A structural body-close may precede substantial trailing content. Bound the held
auction suffix using `publisher.max_buffered_body_bytes`; count every retained byte
after the marker (and an incomplete marker prefix before it). Check before allocation.
Split and release a large chunk's ready prefix before retaining its bounded suffix;
do not first copy the entire chunk into the holder. Exact-cap retention succeeds;
cap crossing is a fatal body error with abandonment, not an unbounded
allocation. No-hook auction seam timing remains unchanged.

Marker multiplicity protects cardinality, not HTML structure. Async output passes
through later stream processors but does not pass through the HTML parser again.
Consumers preserve markers, CSP and application semantics. EOF append does not
promise insertion into an earlier head or before an already emitted body end.

## 8. Cache, privacy and response eligibility

Use active async factory metadata before the first processed-template lookup to skip
shared-template reads, reservations and stores. Raw-origin readthrough is independent.
This intentionally defers the issue's shared-template cold-miss storage behavior.
Maintainers accepted this scope for #1241; cacheable async output is tracked in
#1397.
Processed opted-in HTML uses `Cache-Control: no-store, private` even when work fails,
returns unchanged, or is bypassed after overflow. Cacheable async output needs a
separate identity, freshness, invalidation and hit-execution contract.

Preserve `AuctionDispatch.integration_registry` and same-registry dispatch/finish.
Keep early synthesized document-GET stripping of conditional/range headers; preserve
non-document fetch semantics.

Opted-in HTML with status 206 or any `Content-Range` bypasses all rewriting and EOF
I/O. Preserve encoded bytes, framing, validators and ordinary origin policy.
Independent cookie/diagnostics privacy remains. Async helpers abandon unneeded
auctions. HEAD, bodiless, non-HTML and unsupported encodings run no callbacks.
Complete error HTML remains eligible and exposes its actual status.

## 9. TTFB contract

| Path                                      | Contract                                                 |
| ----------------------------------------- | -------------------------------------------------------- |
| No async factories                        | Existing delivery and auction timing; no new EOF wait    |
| Permission enabled, no async factories    | Same as default                                          |
| Async stage without retention on Fastly   | Ready bytes released before EOF I/O; completion can wait |
| Explicit retention                        | Initial delivery waits until EOF or overflow release     |
| Async stage on buffered Axum/Workers/Spin | EOF wait can add to TTFB before response return          |

Do not claim zero CPU overhead or enabled-hook TTFB neutrality on buffered adapters.
Their streaming support, or reliable post-response work, is separate runtime work.

## 10. Acceptance criteria

- Default permission is false; disabled integrations need no permission; enabled
  async integrations fail configuration without explicit permission.
- Absent callbacks allocate no future and preserve the no-hook paths.
- Yielding non-`Send` callbacks work with materialized and streamed input.
- Upstream async output reaches downstream normal chunks before downstream EOF.
- Every parser/processor receives EOF once, including after timeout.
- Ready decoder/parser/stage prefixes are released while later EOF I/O is pending.
- Buffered overflow replays prefix/crossing/suffix once and starts no optional I/O.
- Cancellation, ties, callback errors, poisoned candidates and late results preserve
  driver-owned fallback without a rollback copy.
- Synchronous helpers reject async use before effects.
- Codecs, ranges, actual statuses, privacy and early cache admission retain previous
  regression protections; corrupt input starts no optional work.
- Auction assembly follows processor finishes; held suffix overflow abandons safely.
- All target-matched CI gates and actual runtime fixture controls pass on final source.

The fictional fixture demonstrates generic EOF HTTP I/O, not a production signing
integration. Verification records identify checked source, real runtimes, hosted CI,
expected skips and tooling limits separately. Historical evidence does not prove this
revised implementation.

## 11. Native Rust verification tooling

Keep EOF runtime verification in the existing Rust integration-test crate. A
feature-gated native test target owns its loopback origin, request-scoped monotonic
events and external runtime process groups. An ignored test runs the selected
adapter; normal helper tests need no adapter tools. The shell entry point selects
the adapter/profile and invokes Cargo with the explicit native target.

Preserve disabled/success/hung/auction controls on all four adapters and actual
early-body delivery on Fastly. The runtime auction control uses a deliberately
unreachable bidder and verifies empty-bid script assembly after finalized output;
it does not establish successful bid collection or slow-bidder timing. Validate typed, isolated configuration before use,
retain diagnostics, preserve compiler/profile settings, and clean process groups
on normal return, failure, SIGTERM and SIGINT. Use existing workspace dependencies
only for tests. No added verification path requires Python.
