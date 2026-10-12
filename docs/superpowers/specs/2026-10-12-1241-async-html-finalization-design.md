# Optional asynchronous HTML document finalization

**Date:** 2026-10-12

**Status:** Implemented on `feat/1241-async-html-finalization`; review hardening and verification recorded below

**Issue:** trusted-server #1241

**Inspected baseline:** `2a0cb0444dc335bd09105abe5952329bbbdfb621`

**Implementation plan:** [Task-by-task implementation and verification](../plans/2026-10-12-1241-async-html-finalization.md)

**Implementation review:** [Requirement coverage, verification, and runtime limitations](../reviews/2026-10-12-1241-async-html-finalization-implementation.md)

**Review hardening:** [Response metadata, range safety, telemetry clock, and CI refinements](../plans/2026-10-12-1241-review-hardening.md)

## 1. Decision

Add an opt-in asynchronous final-document phase to Trusted Server's HTML integration
pipeline. An enabled finalizer receives the completed, synchronously transformed HTML
document and may await request-time HTTP or KV services before producing replacement
HTML. Core owns both the retained document and a separate bounded replacement writer.

Keep `StreamProcessor::process_chunk` synchronous and leave `StreamProcessor` and
`StreamingPipeline::process` unchanged. The new capability is registered explicitly as
an HTML finalizer, rather than being an optional method that some generic synchronous
drivers cannot execute.

Only processable HTML documents with an enabled finalizer take this path. Existing
integrations retain their streaming behavior. Finalizers execute in their own registration
order, after all existing synchronous HTML processors. This is a terminal phase, not an
interleaved asynchronous extension to every streaming stage.

One absolute finish deadline covers the entire finalizer chain. On optional-finalizer
failure, retain the bytes entering that finalizer; on deadline expiry, also skip all
remaining finalizers. Preserve existing fatal handling for origin, parser, synchronous
processor, compression, and downstream-write failures.

For the first version, enabled HTML finalizers bypass shared-template cache reads and
writes before lookup. Their processed HTML responses use the existing synthesized HTML
`private, no-store` policy. Supporting cacheable asynchronous finalizers requires a
separate design for output identity, freshness, and invalidation.

No EdgeZero revision change is required. Trusted Server owns this document lifecycle and
its request service container. A reusable EdgeZero timer can be considered separately.

## 2. Verified problem and limits of the evidence

At the inspected baseline:

- `streaming_processor.rs::StreamProcessor` provides synchronous `process_chunk` and
  `reset` methods. It offers no future-returning operation.
- `html_processor.rs::HtmlWithStreamingProcessors` forwards each chunk and the same
  `is_last` value through the entire synchronous chain immediately.
- `IntegrationHtmlStreamProcessorFactory` creates per-document processors. Its context
  contains hosts, scheme, and document-local state, but no request services.
- `publisher.rs` awaits auction collection outside synchronous processing callbacks.
  The auction already has an application-specific asynchronous seam.
- Buffered materialized responses still use `StreamingPipeline::process` or the separate
  reader-based auction hold loop. An asynchronous stream-helper change alone would miss
  those paths.

A local probe using the actual `StreamingPipeline` verified that `now_or_never()` completes
an immediately ready lookup but discards a lookup that returns `Pending`. Awaiting the same
yielding lookup completes it. This establishes the API gap; it does not establish that
every operation yields on every adapter. A local or already available KV result can be
immediately ready.

No shipped integration at this baseline uses the problematic workaround. The reported
article-signing consumer and the contributor's unsubmitted patch, runtime timings, and
test results have not been independently validated. This is an extension capability,
not a claim that current synchronous integrations are broken.

The first demonstrated requirement is whole-document text signing: obtain a signature
after reading the article and annotate the page. Such a consumer already withholds the
document. Making its buffering an explicit, bounded core responsibility adds no new
whole-document waiting requirement to that consumer.

## 3. Goals

1. Allow a registered HTML integration to await I/O after synchronous document processing
   ends, on all four adapters.
2. Make buffer ownership, limits, cancellation, and unchanged-byte fallback enforceable
   by core rather than implicit integration conventions.
3. Preserve early streaming, chunk boundaries, processor EOF counts, and auction-seam
   timing when no finalizer is enabled.
4. Support both `EdgeBody::Once` and `EdgeBody::Stream`, with and without an auction.
5. Keep decoder validation ahead of finalization, and auction-seam replacement and encoder
   finalization after it on the opted-in path.
6. Prevent warm shared-cache hits or downstream response caches from bypassing required
   per-request finalization.
7. Specify observable, deterministic failure behavior and tests before implementation.

## 4. Non-goals

- Implementing an article-signing integration, an external API protocol, signing-key
  management, or a new KV binding.
- Making `lol_html` callbacks await, making every chunk method asynchronous, or inserting
  asynchronous processors between arbitrary synchronous processors.
- Adding another framework HTML parser or reparsing the finalizer's output.
- Changing the standalone RSC Flight processor, asset rewriting, EC behavior, consent
  gates, or auction admission and transport guarantees.
- Guaranteeing interruption of synchronous Fastly host calls or CPU-heavy callback work.
- Guaranteeing cancellation or rollback of remote HTTP/KV side effects.
- Allowing finalizers to edit HTTP headers or alter the assembly protocol.
- Supporting shared-template storage of asynchronous output in the first version.
- Adding deployment manifests, a general task scheduler, background retries, persistent
  queues, or a new platform clock abstraction.

## 5. Alternatives and tradeoffs

| Approach                                         | Benefit                                                                                                                     | Cost or limitation                                                                                                                             | Decision                                             |
| ------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------- |
| Optional async finish on every `StreamProcessor` | Supports asynchronous completion inside a mixed streaming chain                                                             | Generic synchronous drivers cannot await it; downstream EOF propagation, wrappers, holding behavior, and services become broader contracts     | Defer until an actual consumer requires interleaving |
| Make `process_chunk` asynchronous                | Supports waits at every chunk boundary                                                                                      | Changes every processor and driver for an EOF-only need; boxed async trait methods can allocate per chunk; parser callbacks still cannot await | Reject                                               |
| Explicit asynchronous HTML terminal phase        | Matches the known whole-document consumer; leaves existing streaming traits intact; core can own and bound fallback buffers | Enabled pages wait for EOF; cannot interleave with synchronous stages                                                                          | Select                                               |
| Perform integration I/O before HTML rewriting    | Produces metadata before transformations                                                                                    | Still requires buffering enabled pages and delays parsing; lacks the completed transformed document                                            | Reject for this requirement                          |

Opt-in buffering does not mean buffering every page. A registry without finalizers keeps
the current path. Within an opted-in page, all existing synchronous processors run first,
in their existing order, and receive exactly one EOF notification. Finalizers then run in
their own order. Registration order does not imply interleaving between these two phases.

The framework can provide completed HTML and existing document-local capture state without
adding a parser. The consumer must separately explain how it extracts text and preserves
HTML semantics; this specification does not validate its extraction implementation.

## 6. Ownership and registration

### 6.1 Integration registration

Add `IntegrationHtmlFinalizerFactory` and `IntegrationHtmlFinalizer` contracts alongside
the existing integration factory contracts. Add an empty-by-default finalizer collection
to `IntegrationRegistration` and `IntegrationRegistry`, plus the builder method
`with_html_finalizer`. Preserve factory registration order; do not sort finalizers by ID
or deduplicate registrations silently.

A factory is `Send + Sync` and can live in the retained application registry. A factory
creates a mutable, single-use finalizer instance for each processed document using the
existing owned `IntegrationHtmlStreamContext`, including its `IntegrationDocumentState`.
The finalizer instance and its returned future need not be `Send`.

The same document-state instance must be available to parser hooks, synchronous streaming
processors, and finalizers for that document. It must not be recreated between phases or
retained across requests, including reusable Fastly sandboxes.

Currently `create_html_processor` constructs that state internally. The implementation
must expose the same state through a request-local construction helper or an explicit
construction input; creating a second state for finalizers would disconnect their capture
data from the parser. Preserve the current public helper's synchronous behavior.

Finalizer presence must be discoverable from enabled registration metadata before shared
template lookup. It must not depend on polling a future, inspecting completed HTML, or
creating a finalizer instance. A factory may later return `Unchanged` when no relevant
article is present, but that cannot retroactively authorize a shared-cache lookup.

Disabled integrations contribute no finalizers and cause no finalizer-related buffering,
privacy changes, timer creation, or cache bypass. No global setting enables this feature
without an integration explicitly registering a finalizer.

### 6.2 Callback contract

Use a regular, object-safe method returning `LocalBoxFuture`, with an explicit lifetime
covering mutable session state and borrowed inputs. Do not add native `async fn` to a trait
used behind `dyn` without an object-safety strategy, and do not require `Send` futures.

The following is the intended API shape, not implementation code:

```rust
pub trait IntegrationHtmlFinalizer {
    fn finalize<'a>(
        &'a mut self,
        input: HtmlFinalizationInput<'a>,
    ) -> LocalBoxFuture<'a, Result<HtmlFinalization, Report<HtmlFinalizationError>>>;
}

pub struct HtmlFinalizationInput<'a> {
    pub response_status: StatusCode,
    pub html: &'a [u8],
    pub replacement: &'a mut HtmlReplacementWriter,
    pub services: &'a RuntimeServices,
    pub deadline: &'a HtmlFinalizationDeadline,
}

pub enum HtmlFinalization {
    Unchanged,
    Replace,
}
```

`Unchanged` emits the driver-owned input without cloning it. `Replace` commits only bytes
successfully written into `HtmlReplacementWriter`. Neither the original buffer nor an
unbounded replacement `Vec` is transferred into the future.

The method must return its future promptly and perform I/O inside that future. Factory
creation is synchronous, request-local setup; it must not perform an outside lookup or
hide whole-document buffering. Public implementations must follow repository documentation
and error conventions; `HtmlFinalizationError` is a concrete error type with `Report`.

The replacement writer is a core-owned sink, separate from `html`. Its append operations
are fallible and enforce the byte cap before extending storage. A failed append poisons
that attempt: a later `Replace` cannot commit truncated output even if the callback ignored
the writer error. Empty replacement output is valid only when no required seam token would
be lost; `Unchanged` is the explicit no-replacement result.

The framework does not make arbitrary integration allocations safe. The cap covers its
retained HTML and replacement storage. Consumer extraction buffers and any external
response buffering must have their own bounds and be reviewed.

## 7. Processing lifecycle

### 7.1 No finalizers

```text
origin -> existing decoder -> existing HTML parser and synchronous processors
       -> existing auction seam controller -> existing encoder -> client
```

Do not collect a whole document, allocate a replacement writer or boxed finish future,
create a timer, or move auction collection to EOF. Keep existing synchronous helper
semantics. A cheap registration-presence check is acceptable.

### 7.2 Enabled finalizers

```text
origin -> existing decoder -> existing HTML parser and synchronous processors
       -> bounded retained HTML
       -> ordered asynchronous HTML finalizers
       -> existing auction seam controller -> existing encoder -> client
```

The driver performs these steps:

1. Create normal document-local parser/processor state and finalizer sessions.
2. Pull and decode origin chunks, run synchronous transformations, and retain the resulting
   HTML under the cap. No finalizer-visible HTML is emitted before this phase completes.
3. Drain and validate decoder tails, then finish the existing synchronous chain exactly
   once. Include its final output in the retained HTML.
4. Start one absolute finalization deadline and, on supporting adapters, one timer.
5. For each finalizer, lend the current retained HTML immutably and a fresh bounded
   replacement writer mutably. Race its future against the same remaining deadline.
6. For an accepted `Replace`, validate the candidate and move it into current retained
   storage. Drop the previous stage's input. For `Unchanged` or a soft failure, keep the
   current input and discard the candidate. Finalizer B sees the accepted output of A.
7. On deadline expiry, drop the active future and candidate, skip the remaining finalizers,
   and retain the current input. Earlier accepted replacements remain accepted.
8. Feed the accepted HTML into the existing auction seam controller in bounded chunks.
   Emit its available prefix before collecting bids. Preserve no-seam fallback and terminal
   auction telemetry. Then emit the encoder trailer and finish the response.

Already completed synchronous processors are never re-entered with replacement output.
This prevents duplicate parsing, script injection, Next.js processing, and EOF calls.

Callbacks observe post-rewrite HTML before reader-specific auction insertion. They cannot
claim to sign the final assembled response or content added later by the auction. A text
signing consumer must document its article scope, exclusions, and canonicalization so
framework scripts, placeholders, and later ad content do not invalidate its claims.

### 7.3 Auction behavior

Auction dispatch and its existing deadline remain unchanged. They are not restarted or
extended for finalization. Full-document withholding can make an opted-in page's auction
older by the time its seam reaches collection; an expired auction follows today's
empty-bid behavior. This is an explicit latency/monetization tradeoff for enabled pages.

When no finalizer is enabled, retain the existing ability to collect at the structural
body seam before origin EOF. The terminal phase must not become a universal EOF barrier.
Missing structural body ends, bot/prefetch/consent suppression, non-HTML bodies, and
abandonment remain governed by existing auction policy.

### 7.4 Cancellation

The driver must own current HTML outside the pending finalizer future. Keep a losing
future's lifetime separate from the retained buffer's ownership; explicitly drop it before
moving that buffer or inspecting the borrowed replacement writer.

Reusing a single deadline across finalizers must not put the only fallback buffer inside
a chain future that is destroyed on timeout. The driver remains the owner at every await.

A client disconnect drops active work and request-local buffers. It does not invoke another
finalizer, persist partial output, or initiate a background retry. Preserve the existing
dispatched-auction guard so terminal abandonment is recorded exactly once on supported
drop paths. Dropping local work is not a guarantee that a remote write did not occur.

## 8. Memory and output limits

Use `publisher.max_buffered_body_bytes` as the cap for both retained transformed HTML and
each replacement writer. Its existing default is 16 MiB. Do not add a second independently
configured page-size limit in this change.

Check lengths with overflow-safe arithmetic before append or reserve. Avoid capacity
growth beyond the configured cap, including `Vec` geometric reserve growth. Allocate
incrementally; do not reserve the entire cap for every small page.

At a finalizer await, core may hold one current HTML buffer and one replacement buffer,
each capped at that limit. It must not retain previous successful candidates or clone the
input for rollback. Codec state, pending chunks, consumer-owned state, and platform response
buffers are additional memory; this is not a claim that total request heap use is 16 MiB.

If collection for the optional terminal phase would exceed the cap:

- Disable all finalizers for that document before starting I/O.
- On a streaming sink, release the retained prefix, the chunk that crossed the limit, and
  subsequent synchronous output through the normal auction/encoding path in order. Avoid
  duplicate bytes and a fallback copy of the retained prefix.
- On a buffered sink, preserve the existing outer response-size cap. Optional-finalizer
  bypass does not authorize an oversized buffered response.
- Existing raw-input, decoder, parser, or response-output limit failures remain fatal.
  This fallback applies only when the new optional collection layer reaches its own cap.

A replacement-writer overflow is a soft failure of that finalizer. Discard the candidate
and continue with its input, subject to the same remaining finish budget. Never raise the
cap to accept an integration's output.

## 9. Deadline and adapter services

### 9.1 Configuration

Add `publisher.html_finalization_timeout_ms`, defaulting to `5000`. It covers only the
terminal phase after decoded/synchronously processed EOF, across all finalizers together.
It is not an origin-body timeout, auction timeout, or end-to-end response deadline.

Accept values from `1` through `60000` inclusive and reject invalid settings at startup.
The upper bound keeps the adapter timer duration comfortably within its representable
range and gives the new optional phase a finite operator-controlled budget. Match TOML
deserialization and programmatic `Publisher::default()` behavior. The field has no runtime
effect when no finalizer is registered.

The five-second default is a proposed product default based on the issue's example, not a
claim that all adapter execution budgets permit or guarantee five seconds. Operators must
also respect their platform's invocation limits.

### 9.2 Timer contract

Add an optional `PlatformTimer` service to `RuntimeServices`, defaulting to absent. The
service object is `Send + Sync`; its delay future is local and need not be `Send`. A regular
object-safe method can return `LocalBoxFuture<'static, ()>` for a `Duration`.

| Adapter    | Timer implementation    | Finish deadline semantics                                                           |
| ---------- | ----------------------- | ----------------------------------------------------------------------------------- |
| Cloudflare | `worker::Delay`         | Cooperative race against JS-backed I/O                                              |
| Spin       | `spin_sdk::time::sleep` | Cooperative race against runtime-backed I/O                                         |
| Axum       | `tokio::time::sleep`    | Cooperative race on the existing Tokio executor                                     |
| Fastly     | No timer                | Check elapsed budget at control boundaries; bound host calls within the integration |

Keep SDK imports and concrete timer implementations in Trusted Server adapters. These
primitives exist in the pinned dependencies; do not add a Tokio dependency to core or
repin EdgeZero to obtain a timer.

Use `web_time::Instant` for absolute budget accounting and elapsed observations in core.
Do not introduce `std::time::Instant::now()` or `SystemTime::now()` on the Cloudflare path.
The review hardening exception corrects the traced auction telemetry clock using
that existing abstraction. Broader clock sites reported in #1240 remain separate
work; one successful auction smoke does not verify all of them.

### 9.3 Race and fallback rules

Create one absolute deadline and retain one pinned timer across the entire chain. Poll
the deadline before active work at decision points. Check absolute expiry before a stage
starts and after it resolves; if completion and expiry coincide, deadline expiry wins.
Do not restart a full-duration sleep for each finalizer.

Callbacks receive a deadline handle exposing remaining time and whether a cooperative
timer is available. Bound individual network requests and external response reads to the
remaining budget where the service permits it. Successful but late candidates are
discarded even if a timer could not run while the callback was polling.

Timers cannot preempt a blocking host call or a callback that never yields. On Fastly,
consumer acceptance requires evidence that its calls are bounded independently, or that
it returns `Unchanged` without performing unsupported unbounded operations. Core cannot
promise a hard five-second release for arbitrary Fastly HTTP/KV operations.

Cancellation frees borrowed buffers and releases the publisher's wait. It does not
universally abort transport work or reverse external writes. Consumers must tolerate
duplicate requests/retries and use idempotent external operations where required.

## 10. Failure and validation contract

Fallback bytes are the synchronously transformed input of the current finalizer, including
accepted earlier finalizers. They are not necessarily the origin's original bytes. No
rollback of prior accepted HTML or remote side effects is promised.

| Event                                                                         | Document behavior                                                | Remaining finalizers            |
| ----------------------------------------------------------------------------- | ---------------------------------------------------------------- | ------------------------------- |
| `Unchanged`                                                                   | Keep current input; discard any candidate writes                 | Continue                        |
| Valid `Replace` within budget                                                 | Commit bounded candidate                                         | Continue with replacement       |
| Callback reports unavailable/rejected external work                           | Keep current input; discard candidate                            | Continue while budget remains   |
| Replacement writer overflow or invalid required marker                        | Keep current input; discard candidate                            | Continue while budget remains   |
| Overall deadline expires                                                      | Drop active future and candidate; keep current input             | Skip all remaining              |
| Optional collection limit reached                                             | Release retained synchronous output through ordinary delivery    | Skip all finalizers             |
| Origin read, decoder, parser, synchronous processor, encoder, or sink failure | Existing fatal error/truncation behavior and auction abandonment | Stop                            |
| Callback panics                                                               | Existing panic behavior                                          | No new panic-catching mechanism |

Define controlled error categories for external failure and candidate validation. Keep
`Report<TrustedServerError>` at orchestration boundaries and existing `io::Error` where
streaming I/O uses it. Do not turn unrelated processor errors into silent passthrough.

Finalizers must preserve the parser-generated control marker, when present, as opaque
bytes. They must not remove, duplicate, or relocate it into script/text content. Core
checks the active token's required cardinality before committing a replacement; missing
or duplicated tokens reject that candidate. A count check cannot prove structural
placement. This remaining invariant is a reviewed integration contract, not a claim of
automatic HTML validation without parsing.

Core does not inspect arbitrary replacement HTML for semantic validity or reconstruct
parser observations. Finalizers must preserve valid document structure, existing CSP
requirements, URL/RSC encodings, and the meaning of upstream rewrite decisions. Header
changes and transformations needing new parser-derived security observations require a
separate capability. The first consumer should add signature metadata without changing
executable markup or article text after canonicalization.

## 11. Cache, privacy, conditional requests, and framing

### 11.1 Shared-template cache

When enabled finalizer metadata is present for the request's document pipeline, disallow
shared-template lookup/reservation before the first cache read. The same decision must
exclude later storage. A factory deciding `Unchanged` at EOF does not restore eligibility.

Use the existing non-shared inline fallback consistently across slot scripts, body-seam
selection, transformation, and assembly. Do not generate a shared-template marker for a
response that has no shared-template consumer. Record the existing request-bypass outcome
where cache response-state reporting is enabled.

Warm entries may remain stored but must not be read while the finalizer is enabled. No
purge is required merely to activate it. Disabling an integration can restore ordinary
eligibility, subject to existing settings/build fingerprints and template validation.

There is no first-version `shared_safe` escape hatch. Later cache support must specify at
least reader-neutral output, external-data/signing-key identity, expiration, invalidation,
and whether failures can be cached. Cold-miss ordering alone does not settle these rules.

### 11.2 Downstream privacy and origin readthrough

Apply the existing synthesized HTML `private, no-store` policy to body-carrying HTML
responses that enter the enabled finalizer pipeline, regardless of auction eligibility or
the finalizer outcome. Strip validators and stale downstream cache directives using the
same existing helper as other synthesized HTML. Apply this before streaming headers are
committed. An unchanged or size-bypassed result remains private for that response.

This prevents a browser or downstream cache from treating a per-request result as the
origin's public representation. Origin readthrough remains a separate raw-byte cache: it
may still provide a complete shareable origin body under existing policy, because every
request subsequently runs transformation/finalization. Do not cache assembled output as
an origin body or modify EC-cookie privacy rules.

For document GETs requiring finalization, reuse
`strip_conditional_and_range_headers` so client revalidation or range requests cannot
make the origin return a bodyless `304` or partial `206` instead of the full document.
That helper removes `If-None-Match`, `If-Modified-Since`, `Range`, and `If-Range`.
Do not discard mutation preconditions such as `If-Match` or `If-Unmodified-Since`,
or broaden GET behavior to other methods. Unexpected bodyless responses retain existing
status/error handling and invoke no finalizer, including the existing invalid-origin-304
guard when the ad stack is active. This feature does not turn an existing 502 into a 304.

### 11.3 Response selection and framing

Run finalizers only on complete HTML that the existing routing rules already process, including
processable non-2xx HTML. Expose the actual response status so consumers can choose
which complete documents to process. With enabled finalizers, HTML with status 206
or any `Content-Range` bypasses all body transformations and terminal I/O. Preserve
encoded bytes, range/framing metadata, and origin policy; independent cookie and
diagnostics privacy still applies. This covers range-associated error responses
such as 416 as well as partial representations. The handler and both response
wrappers must make this decision before body-dependent policy or framing mutation;
direct body helpers carry the same private metadata. Abandon dispatched bids once.
Keep non-document range forwarding intact and feature-off behavior unchanged.

Do not rewrite unsupported encodings, unmodified pass-through
responses, assets, or standalone `text/x-component` bodies.

HEAD and `204`, `205`, or `304` responses invoke no finalizer or finalizer I/O. Preserve
their existing framing and any dispatched-auction abandonment. Preserve method/status
handling for other requests instead of introducing a GET-only body transformer silently.

Finalizers do not change the selected content encoding. Strip stale `Content-Length` as
the existing transformed-body path requires; stream encoders finalize only after accepted
HTML and auction output are emitted. Preserve repeated headers, cookies, and CSP policy.

## 12. Integration into publisher paths

The first implementation must deliberately cover these existing branches:

| Source and sink                                 | Auction | Required integration                                                                  |
| ----------------------------------------------- | ------- | ------------------------------------------------------------------------------------- |
| Lazy response stream, `Once` or `Stream` source | None    | Retain opted-in synchronous HTML output, await finalizers, then encode                |
| Lazy response stream, `Once` or `Stream` source | Pending | Same phase before existing seam collection; retain auction guard                      |
| Buffered response, streamed source              | None    | Same terminal phase before completing bounded response output                         |
| Buffered response, streamed source              | Pending | Same phase before seam/encoder finish                                                 |
| Buffered response, materialized source          | None    | Opted-in path cannot fall through `stream_publisher_body` and skip finalization       |
| Buffered response, materialized source          | Pending | Reader-based hold path must route opted-in HTML through the same terminal coordinator |

Introduce one private HTML terminal coordinator shared by the enabled paths. Feed it the
same synchronous transformation semantics on materialized and streamed inputs. Keep
ordinary no-finalizer branches intact; avoid replacing the entire generic compression
architecture to obtain this capability.

Do not add finalizers to a generic synchronous helper that cannot await them. Public
synchronous helpers continue to implement synchronous transformations only; document that
HTML finalizers require the supported publisher finalization entry points. Adapters must
never select a synchronous-only path for a registered finalizer silently.

Thread document state and finalizer metadata explicitly through the relevant publisher
parameters. Do not reconstruct a separate registry/session inside a lazy stream or copy
request services into global state. Reuse existing services ownership for request-lifetime
and lazy response futures.

## 13. Observability

Use existing `log` instrumentation with integration ID, controlled outcome, elapsed time,
and bounded byte counts. Log ordinary completion/unchanged results at debug level and soft
failure, timeout, or optional-collection bypass at warn level. Emit at most one outcome
per attempted finalizer and one terminal collection/deadline bypass for the document.

Never log HTML, article text, signatures, external response bodies, control tokens, client
identifiers, secrets, URLs containing request data, or arbitrary error-report attachments.
Log controlled categories instead of formatting an integration-supplied report wholesale.

No new public response headers, persistent telemetry store, or metrics subsystem is needed.
Existing auction terminal telemetry retains its meanings and exactly-once behavior.

## 14. Verification requirements

The checks below are acceptance requirements for the implementation, not claims that the
unsubmitted feature has already passed them.

### 14.1 API, registration, and state

- Default registration has no finalizers; a disabled integration contributes none.
- Factory and finalizer ordering is deterministic, including multiple factories for one ID.
- Sessions and document capture state are isolated between requests and reused sandboxes.
- A non-`Send` future borrowing input, replacement writer, and session state compiles and
  executes through each supported publisher entry point.
- Existing processors retain chunk behavior and receive EOF once before any finalizer.
- Synchronous helpers retain their documented scope; no adapter silently skips a finalizer.

### 14.2 Real yielding, fallback, and deadlines

- A stub that returns `Pending` at least once completes when awaited and changes output.
- Finalizer A's accepted output is exactly B's input; B never receives partial candidates.
- A timeout after partial candidate writes drops the future, emits exact input bytes, and
  invokes no later finalizer. Use a drop observer and allocation/buffer identity assertions
  to prove no fallback document clone.
- `Unchanged`, callback error, invalid marker, replacement overflow, and a late result
  preserve exact stage input and discard partial candidate state.
- Two or more delayed finalizers share one deadline; the budget does not multiply.
- Deadline/completion ties follow the documented timeout-wins rule. Use an injectable
  deterministic timer/clock in tests rather than flaky wall-clock sleeps.
- Client cancellation drops active work and retains exactly-once auction abandonment.
- Fastly tests demonstrate no timer dependency, budget checks, and bounded-call consumer
  behavior; they do not pretend a synchronous call can be preempted.

### 14.3 Bounds and byte ordering

- Test cap minus one, exact cap, cap plus one, and length-overflow rejection.
- Test multiple chunks and one large chunk crossing the optional collection cap. Prefix,
  crossing chunk, and suffix emerge once in order on the streaming bypass path.
- Buffered output caps and existing decoder/input caps retain their existing failures.
- Replacement sink overflow cannot commit even when a callback ignores its write error.
- Input and replacement capacities remain bounded; no full-cap allocation for tiny pages.
- Accepted prior stages are released as ownership advances; timeout does not restore the
  origin or undo an earlier successful finalizer.

### 14.4 Publisher and codec matrix

Exercise `Once` and `Stream` sources, lazy and buffered sinks, auction present/absent,
and identity, gzip, deflate, and Brotli encoding. Include concatenated gzip members,
decoder tails released only at EOF, truncated/corrupt streams, empty HTML, split UTF-8,
missing body ends, and malformed-but-renderable HTML.

Verify decoded/synchronous EOF precedes finalizer I/O, candidates preserve private marker
cardinality, ready prefixes precede auction collection, and encoder trailers follow all
accepted bytes. Finalizer failure must not leave a raw private marker in client output.

Retain the current tests for early compressed output, parser-confirmed Next.js seams,
placeholder substitution, body-source cancellation, and auction abandonment. Those tests
must continue to use the no-finalizer path and protect its existing streaming behavior.

### 14.5 Cache, headers, and eligibility

- Populate a template entry with finalization disabled, enable a finalizer, and prove the
  request bypasses the warm entry and runs its finalizer. Assert no lookup reservation or
  store is made; cover ESI configuration falling back to inline.
- Disable the finalizer and verify ordinary cache miss/store/hit behavior is restored.
- Responses remain `private, no-store` for success, `Unchanged`, timeout, external failure,
  and collection bypass, including HTML without an auction or matched ad slot.
- Raw origin readthrough hits still execute finalizers per request; no assembled output
  enters the raw origin cache.
- Test synthesized GET revalidation/range handling and preserved mutation preconditions,
  including unexpected origin 304 behavior with and without an active ad stack.
- Verify non-2xx HTML is processed, while HEAD, bodiless statuses, non-HTML, unsupported
  encodings, and unmodified responses make no finalizer call. Preserve framing and cookies.
- Existing shared-template validation, assembly, CSP nonce, and cache-privacy tests pass
  unchanged with no finalizer enabled.

### 14.6 Actual adapters and repository gates

Native Cloudflare/Spin tests cannot establish JS or WASI timer behavior. Add fictional
fixtures and run a bounded delayed/hung lookup on Viceroy, local Workers, Spin, and Axum.
Record the exact setup, SDK/runtime versions, observed behavior, and limitations. Do not
claim a universal exact wall-clock release; cooperative scheduling adds tolerance.

Review hardening permits one targeted correction under #1240: auction telemetry
uses `web_time::Instant`, consistent with the orchestrator, because the captured
Workers stack names that unsupported standard-clock call. Verify the actual
Workers auction control after the correction. Broader clock paths remain separate;
do not claim all of #1240 resolved from one successful fixture. Keep KV wiring under
#1139 separate; HTTP-backed and unavailable-KV fixtures can verify this lifecycle without
requiring new mandatory store bindings.

Before implementation PR handoff, run every CI gate in [AGENTS.md](../../../AGENTS.md#ci-gates),
including target-matched adapter checks/tests, parity, JS/docs formatting and tests, and
native build-digest checks. Run existing template-cache ESI/inline harnesses as appropriate.
Use only fictional fixture hosts, text, configuration, and credentials.

## 15. Implementation scope and delivery

Expected production touch points:

- Core integration registration: terminal factory/session contracts and presence metadata.
- Core HTML/publisher processing: shared document state, bounded collection/replacement,
  terminal coordinator, path selection, cache/conditional/privacy integration, and errors.
- Core platform services: optional timer and deadline helper using existing clock support.
- Core settings: the finish timeout default/validation and canonical configuration docs.
- Adapter service construction: Cloudflare, Spin, and Axum timer implementations; Fastly
  explicitly retains no cooperative timer.
- Tests, integration fixtures, example configuration, and canonical integration/adapter
  guides documenting opt-in behavior and limitations.

Keep the generic streaming trait, synchronous pipeline, JS runtime, platform manifests,
and EdgeZero pin unchanged. Do not bundle the real signing consumer, KV-store wiring,
broader Cloudflare clock fixes, or unrelated publisher refactoring into this feature.
The narrowly traced auction telemetry clock correction above is the reviewed exception.

The implementation plan should separate contract/registration work, terminal coordination,
adapter timers, cache/privacy behavior, and verification into small reviewable changes,
while not shipping an intermediate state in which enabled finalizers can be skipped.

This document authorizes no deployment, external API use, or publication. After maintainer
review, write the implementation plan separately. The current branch contains design work
only.

## 16. Acceptance criteria

The feature is ready only when:

1. A yielding finalizer works through every supported publisher response path.
2. No-finalizer output, streaming timing, auction sequencing, and error handling retain
   existing behavior under their regression tests.
3. Fallback is exact at the failed stage, candidate writes are bounded and isolated, and
   timeout drops the active future without cloning the retained document.
4. The deadline covers the entire terminal chain, with Fastly's non-preemption limitation
   explicitly tested and documented.
5. Shared-template reads and writes cannot bypass or persist the new per-request phase;
   downstream privacy and origin readthrough remain correctly separated.
6. No unsupported body, method/status, encoding, or synchronous-only adapter path silently
   invokes or skips the new capability.
7. Real runtime evidence and all required repository gates are recorded with honest limits.

## 17. References and investigation record

- Issue #1241: requested asynchronous completion for HTML stream processors.
- PR #1135 and the [parser-aware body hold design](2026-09-07-850-parser-aware-body-hold-nextjs-streaming-design.md):
  synchronous stream processors and parser-generated auction seam.
- Issues #1240 and #1139: related clock and runtime KV findings; independent scope.
- [Streaming processor](../../../crates/trusted-server-core/src/streaming_processor.rs),
  [HTML processor](../../../crates/trusted-server-core/src/html_processor.rs), and
  [integration registry](../../../crates/trusted-server-core/src/integrations/registry.rs).
- [Publisher](../../../crates/trusted-server-core/src/publisher.rs),
  [runtime services](../../../crates/trusted-server-core/src/platform/types.rs),
  [template cache contract](../../../crates/trusted-server-core/src/platform/template_cache.rs),
  and [publisher settings](../../../crates/trusted-server-core/src/settings.rs).

During investigation at the baseline, the following native core test filters passed:

```text
cargo test -p trusted-server-core --lib streaming_processor::tests --offline
34 passed
cargo test -p trusted-server-core --lib html_processor::tests --offline
32 passed
cargo test -p trusted-server-core --lib publisher::tests --offline
369 passed
```

Two temporary probe tests also passed: the current synchronous pipeline discards a
yielding one-poll lookup, and awaiting the same lookup completes it. These are investigation
evidence, not implementation regression tests or proof of the contributor's runtime claims.
