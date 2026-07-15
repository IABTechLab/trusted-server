# Make SSAT publisher HTML assembly truly streaming and parser-safe

**Issue:** #857  
**Status:** Draft  
**Area:** Trusted Server runtime / publisher fallback / server-side ad stack

## Summary

Trusted Server's server-side ad stack (SSAT) publisher HTML path should be a
true streaming assembly path on Fastly: origin HTML bytes should flow through
Trusted Server and reach the browser before the origin response is fully read,
while bid injection remains parser-safe and privacy-preserving.

Today the code has a streaming-shaped publisher API, but the Fastly origin
response can still be materialized into a single WASM-heap allocation before
client bytes flow, and the EdgeZero publisher fallback buffers the assembled
publisher response before sending it. The current SSAT bid hold also scans raw
origin bytes for `</body`, which can false-positive inside inline scripts or
JSON.

This design makes the Fastly SSAT HTML path truly streaming, replaces raw-byte
close-body detection with parser-confirmed late binding, keeps compressed HTML
streaming through decode/rewrite/re-encode, and documents non-Fastly adapters
and full-document HTML post-processors as buffered mode for this slice.

## Goals

- Fastly SSAT HTML sends rewritten head/body bytes to the client without first
  materializing the full origin body or full assembled body.
- Fastly distinguishes publisher SSAT streaming from asset pass-through
  streaming so the SSAT assembly loop, auction lifecycle, and header finalizers
  all run before and during client streaming.
- The body-close hold is parser-context-aware and cannot trigger on `</body`
  text inside inline scripts, JSON, comments, attributes, or other non-tag
  contexts.
- Bid injection remains before the real parser-confirmed `</body>` when one is
  present.
- If no parser-confirmed body close exists, append the SSAT fallback tail at EOF
  as a best-effort fallback.
- Streaming mode supports gzip, deflate, and brotli origin HTML by decoding,
  rewriting, and re-encoding incrementally.
- Streaming mode enforces cumulative decoded-input and processed-output caps
  without full-document allocation.
- Full-document HTML post-processors, including the current Next.js
  post-processor, are explicitly routed through buffered mode for this slice.
- Axum, Cloudflare, and Spin publisher SSAT behavior is documented and tested as
  buffered mode for this slice.
- Existing SSAT privacy behavior remains unchanged:
  `Cache-Control: private, max-age=0` and no shared/runtime edge-cache headers
  on assembled per-user HTML.
- Processed HTML responses do not retain stale payload validators or range
  metadata from the origin representation.

## Non-goals

- Origin-template caching.
- Transformed-template caching.
- Dynamic HTML/RSC/API cache-key design.
- SSAT compression offload via `Accept-Encoding: identity` / `X-Compress-Hint`.
- Akamai-specific cache behavior.
- Auction backend timeout/name fixes.
- Making Next.js RSC post-processing streaming-safe in this slice.
- Making Axum, Cloudflare, or Spin publisher SSAT truly streaming in this slice.

## Current behavior

### Publisher response shape

`crates/trusted-server-core/src/publisher.rs` exposes:

- `PublisherResponse::Buffered`
- `PublisherResponse::Stream`
- `PublisherResponse::PassThrough`

`PublisherResponse::Stream` currently means "processable body with headers
separated from the body", not necessarily client-visible streaming. Its docs
already note that, on the interim path, the body may have been materialized in
WASM heap upstream.

Also, the current Fastly send boundary treats a plain `EdgeBody::Stream(_)` as
an asset pass-through stream. It does not carry the publisher-specific state
needed to run the SSAT HTML assembly loop, collect/abandon an auction, or apply
parser-safe bid late binding. True publisher streaming therefore needs an
explicit publisher-streaming envelope or equivalent dispatch path, not just a
processed response whose body happens to be `EdgeBody::Stream(_)`.

### Fastly origin body materialization

`handle_publisher_request` sends the origin request with:

```rust
services
    .http_client()
    .send(PlatformHttpRequest::new(req, backend_name))
    .await
```

On Fastly, `PlatformHttpRequest::stream_response` defaults to `false`, so
`crates/trusted-server-adapter-fastly/src/platform.rs` converts the
`fastly::Response` body through `take_body_bytes()` before returning the
platform-neutral response. That can allocate the full origin response body before
any client bytes are sent.

### EdgeZero publisher fallback buffering

`crates/trusted-server-adapter-fastly/src/app.rs` currently resolves publisher
fallback responses with `buffer_publisher_response_async`, producing a single
`Body::Once` response. `crates/trusted-server-adapter-fastly/src/main.rs` only
streams `EdgeBody::Stream(_)` bodies directly to Fastly's `StreamingBody`; SSAT
publisher HTML is therefore sent as a completed buffered response.

### Raw close-body hold

`stream_publisher_body_async` uses `BodyCloseHoldBuffer`, which searches decoded
origin bytes for the case-insensitive raw prefix `</body`. That can trigger
inside valid non-tag contexts, for example:

```html
<script>
  const marker = '</body>'
</script>
```

The actual injection is parser-aware because it happens in a `lol_html` body
end-tag handler, but the decision about when to stop streaming and collect the
auction is not parser-aware.

### Full-document HTML post-processors

`crates/trusted-server-core/src/html_processor.rs` wraps the HTML rewriter in
`HtmlWithPostProcessing`. When any `IntegrationHtmlPostProcessor` is registered,
it accumulates the rewritten document and runs post-processors only at EOF. The
current Next.js integration registers such a post-processor.

This is intentionally not stream-safe today and must be treated as buffered mode
for this issue.

## Locked decisions

1. **Missing `</body>` fallback:** append the SSAT fallback tail at EOF when no
   parser-confirmed body close is available.
2. **Post-processors:** full-document HTML post-processors are an acceptable
   buffered-mode tradeoff for this slice. Next.js streaming-safe post-processing
   is deferred.
3. **Compression:** true streaming should support gzip, deflate, and brotli by
   incrementally decoding, rewriting, and re-encoding.
4. **Adapter scope:** Fastly is the first true-streaming target. Axum,
   Cloudflare, and Spin are documented/tested as buffered mode.
5. **Caps:** streaming mode enforces cumulative decoded-input and
   processed-output caps, plus a small held-tail cap.

## Architecture

### High-level Fastly SSAT streaming flow

```text
Client request
  -> Fastly entry point
  -> publisher fallback route
  -> dispatch SSAT auction requests
  -> fetch publisher origin with streaming response body for request-level SSAT candidates
  -> inspect origin response metadata and choose the response-level route
  -> if stream-eligible, finalize response headers before client commit
  -> Fastly response.stream_to_client()
  -> incremental origin body assembly:
       encoded origin bytes
       -> decoder, if Content-Encoding is gzip/deflate/br
       -> cumulative decoded-input cap
       -> lol_html processor
       -> parser-inserted bid placeholder detection
       -> auction collect at placeholder, or EOF fallback
       -> cumulative processed-output cap
       -> encoder, if response remains compressed
       -> Fastly StreamingBody
  -> finish StreamingBody
```

### Buffered-mode flow

Buffered mode remains the compatibility path for:

- non-Fastly adapters in this slice;
- Fastly SSAT HTML when full-document HTML post-processors are registered;
- request shapes that are known before fetch not to be safe streaming candidates;
- unsupported response shapes where the stream cannot safely commit headers;
- existing unmodified buffered routes.

Buffered mode may still use the same parser-safe placeholder late-binding logic,
but its output sink is a bounded in-memory writer rather than Fastly's
`StreamingBody`.

## Parser-safe bid late binding

### Problem with raw-byte scanning

Raw `</body` scanning observes bytes before HTML parsing. It cannot know whether
those bytes are:

- a real closing body tag;
- a string literal inside `<script>`;
- JSON data inside a script block;
- text in a comment;
- attribute content;
- malformed markup that `lol_html` does not interpret as a body end tag.

Therefore raw scanning is not a safe trigger for auction collection or bid-tail
holding.

### Proposed mechanism

Use `lol_html` as the only authority for detecting the real body end tag.

1. For SSAT requests, configure the HTML processor with a per-request opaque bid
   placeholder token instead of directly injecting bids from shared state in the
   `body` end-tag handler.
2. The body end-tag handler inserts that placeholder immediately before the real
   parser-confirmed `</body>`.
3. The publisher streaming loop scans **processed uncompressed output**, not raw
   origin input, for the opaque placeholder.
4. Until the placeholder is found, processed output streams through immediately,
   subject only to the placeholder scanner's overlap buffer.
5. When the placeholder is found:
   - emit bytes before the placeholder;
   - stop reading additional origin bytes after the current decoded/processed
     chunk has been handled;
   - hold the placeholder plus any suffix from the current processed chunk;
   - collect the dispatched auction;
   - build the bids script;
   - replace the placeholder with the bids script;
   - emit the held suffix;
   - resume reading and streaming origin bytes.
6. At EOF, if no placeholder was found:
   - collect the dispatched auction if it has not already been collected;
   - finalize the HTML processor;
   - if final processor output contains the placeholder, replace it normally;
   - otherwise append the SSAT fallback tail at EOF.

Pausing origin reads while the auction is collected is intentional. The
implementation should rely on the runtime's normal upstream backpressure rather
than draining the rest of the origin response into memory. The wait remains
bounded by the existing auction collection timeout/deadline; if collection fails
or times out, replace the placeholder with the empty/current bids script and
continue streaming.

The EOF fallback covers both documents that have a `<body>` without a parsed end
tag and malformed documents that never expose a body end tag. If the normal
`<head>` injection has already run, the fallback tail may be just the bids
script. If no head injection ran, the fallback tail must include the minimal SSAT
bootstrap in executable order — ad slot state, integration head config required
by the TSJS bundle, the TSJS script tag(s), and then the bids script. This is a
best-effort malformed-document path; it must still be bounded by the processed
output cap and must not leak placeholders.

The placeholder should be a per-request high-entropy token, such as an HTML
comment containing a UUID, for example:

```html
<!--__TSJS_BIDS_PLACEHOLDER_018f4b1c-0000-7000-9000-000000000000__-->
```

The exact format is an implementation detail, but it must be:

- generated per request;
- impossible for normal origin content to predict;
- valid HTML when inserted by `lol_html`;
- scanned only in processed uncompressed output;
- fully removed from the client response on success.

### Placeholder scanner requirements

The placeholder scanner must be streaming-safe:

- handle the placeholder split across processor output chunks;
- emit all bytes before the placeholder as soon as safely possible;
- keep at most `placeholder.len() - 1` bytes of overlap while searching;
- once the placeholder is found, hold only the placeholder and suffix bytes until
  auction collection completes;
- enforce a held-tail cap so the hold cannot become accidental full-document
  buffering;
- after the first successful replacement, strip any later occurrence of the same
  placeholder token from processed output rather than forwarding it to the
  client.

### Bid source behavior

If an auction was dispatched, the first parser-confirmed placeholder triggers
auction collection. The resulting winning bids are written through the existing
bid-script builder path.

If no auction was dispatched but the SSAT ad stack still needs to inject slot
state and an empty bids script, the late-binding layer should replace the
placeholder immediately with the empty/current bids script without awaiting an
auction.

If multiple body end tags exist, only the first placeholder is replaced with
bids. The preferred implementation is for the `lol_html` body end-tag handler to
insert at most one placeholder, using the existing once-only guard pattern. The
late-binding scanner must still defensively remove any later placeholders, if
any, so no raw placeholder leaks to the client and bids are not injected multiple
times.

## Compression design

Streaming SSAT HTML must support the currently supported origin content
encodings:

- identity / no `Content-Encoding`;
- `gzip`;
- `deflate`;
- `br`.

The streaming path should preserve the existing response encoding unless a
separate route explicitly strips it. Compression offload and forcing identity
origin fetches belong to #858 and are out of scope here.

### Pipeline placement

Parser-safe placeholder detection must occur after HTML rewriting and before
recompression:

```text
encoded origin chunk
  -> decoder
  -> decoded HTML bytes
  -> lol_html processor
  -> processed uncompressed bytes containing parser placeholder
  -> placeholder late binder / bid insertion
  -> encoder
  -> client writer
```

Scanning compressed output would be incorrect because the placeholder would not
be visible. Scanning raw decoded input would reintroduce the original parser
context bug.

The true-streaming implementation must drive this pipeline from origin stream
chunks. It must not convert the publisher body through `EdgeBody::into_bytes()`,
`take_body_bytes()`, or any equivalent full-body materialization before decoding
and rewriting. Buffered adapters may continue to use the bounded in-memory path.

### Decoder/encoder finalization

The existing synchronous pipeline already explicitly finalizes gzip and deflate
encoders with `finish()` and uses explicit brotli flush/finalization behavior.
The new SSAT streaming loop must preserve equivalent finalization semantics:

- gzip: call `finish()` and propagate errors;
- deflate: call `finish()` and propagate errors;
- brotli: explicitly flush/finalize through the available brotli writer API and
  propagate write errors where the API exposes them.

If compression finalization fails after headers have been committed, log the
error and abort the streaming body.

## Streaming caps

True streaming removes the full-document allocation, but it must still protect
WASM memory and CPU from unbounded input/output growth.

### Cap types

Use the existing publisher body limit initially:

```text
settings.publisher.max_buffered_body_bytes
```

Despite the current name, it should be applied to streaming SSAT as the
cumulative safety limit until/unless a separate streaming-specific setting is
introduced.

Enforce:

1. **Decoded input cap**
   - Count bytes after decompression and before HTML parsing.
   - Protects against compressed expansion bombs and excessive parser workload.
2. **Processed uncompressed output cap**
   - Count bytes emitted by the HTML processor and late binder before
     recompression.
   - Protects against output amplification from rewrites and injected scripts.
3. **Held-tail cap**
   - Count bytes retained after the parser placeholder is found and before the
     auction is collected.
   - Use a concrete default so tests and operators can reason about behavior:
     `SSAT_HELD_TAIL_CAP_BYTES = 64 * 1024` (8× the current 8 KiB stream chunk),
     with the effective cap clamped to `settings.publisher.max_buffered_body_bytes`
     when that setting is smaller.
   - If exceeded, treat it as a streaming safety violation.

### Pre-commit rejection vs mid-stream abort

When headers have not yet been committed, known oversized responses should fail
cleanly with an error response. For example, an identity response with
`Content-Length` greater than the configured cap can be rejected before
`stream_to_client()`. Compressed `Content-Length` is not a decoded-size signal and
must not be used as proof that the decoded input is under the cap; compressed
responses are checked by the cumulative decoded-input counter while streaming.

For compressed, chunked, or unknown-size responses, the true size may only be
known after streaming begins. If a cumulative cap is exceeded after headers are
committed:

- log the violation with enough context for diagnosis;
- abort/drop the streaming body;
- do not attempt to recover with a buffered fallback because the client-visible
  response has already started.

This matches standard reverse-proxy behavior for mid-stream processing failures.

## Fastly adapter design

### Origin fetch

Fastly has to make the platform request before it knows the origin response's
`Content-Type`, status, or `Content-Encoding`, so the route decision is
intentionally two-phase:

1. **Request-level candidate decision, before origin fetch.** Use a streaming
   platform response only when request metadata and configuration make true SSAT
   streaming possible: the method can carry a response body (`GET`, not `HEAD`),
   the server-side ad stack may run for this navigation, no full-document HTML
   post-processors are registered, and the Fastly send boundary can preserve the
   publisher-streaming state through response finalization.
2. **Response-level route decision, after origin headers arrive and before
   client commit.** Inspect status, `Content-Type`, `Content-Encoding`, and
   relevant headers to choose processed streaming, streamed unmodified
   pass-through, bodiless response handling, or a pre-commit error.

Request-level Fastly candidates should use the existing platform flag shape:

```rust
PlatformHttpRequest::new(req, backend_name).with_stream_response()
```

On Fastly, this preserves the origin response body as `EdgeBody::Stream(_)`
instead of using `take_body_bytes()`.

The platform-level body materialization cap still applies to non-streaming
requests. Streaming SSAT applies its own cumulative decoded/processed caps in
core. Because a candidate request may later prove not to be processable SSAT
HTML, the Fastly path must be able to forward a preserved origin stream
unmodified without first buffering it. If a request is known before fetch to
require buffered mode, such as post-processor-enabled HTML, do not request a
streaming origin body.

### Client commit

For true-streaming Fastly SSAT responses:

1. Build and mutate response headers in core as today.
2. Preserve the publisher streaming state across the fallback/entry-point
   boundary. Do not encode a processed publisher stream as a plain
   `EdgeBody::Stream(_)` unless the send path can distinguish it from the asset
   pass-through stream case. A dedicated `PublisherResponse` variant,
   response extension, or Fastly-specific fallback result is acceptable as long
   as it carries the response skeleton, origin stream, processing params,
   orchestrator/services access, and auction telemetry token.
3. Apply all finalization that must happen before client commit, including:
   - EC/privacy finalization;
   - request-filter response effects;
   - final cache/privacy guards;
   - transformed-response header normalization;
   - removal of `Content-Length` for processed streaming bodies.
4. Convert the response headers to a Fastly skeleton response.
5. Call `stream_to_client()`.
6. Run the SSAT streaming body assembly loop, writing to the Fastly
   `StreamingBody`.
7. Call `finish()` on success.
8. On any mid-stream error, log and drop the streaming body.

Once `stream_to_client()` is called, no response header mutation is possible.
The Fastly entry point therefore owns the boundary between final response header
mutation and body streaming.

### Route decision

A Fastly publisher response is response-level stream-eligible when all of the
following are true:

- the request/response can carry a body (`GET`, not `HEAD`, not `204`, `205`, or
  `304`);
- the response is HTML that needs SSAT assembly, not merely a text/JS/CSS/JSON
  response that the generic publisher processor could rewrite;
- the content encoding is supported (`identity`, `gzip`, `deflate`, `br`);
- no full-document HTML post-processors are registered for the active
  integration registry;
- pre-commit header checks, including identity `Content-Length` preflight, have
  not rejected the response;
- response headers can be finalized before commit;
- processor construction succeeds before commit.

If the request was fetched as a streaming candidate but the response is not
processable SSAT HTML, Fastly should stream the origin body unmodified when that
is safe, abandon any dispatched auction with telemetry, and preserve bodyless
status semantics by not attaching a body for `HEAD`, `204`, `205`, or `304`.
If the response requires buffered mode and that requirement was known before the
origin fetch, the request should not have used `with_stream_response()`.

## Non-Fastly adapters

For this issue, Axum, Cloudflare, and Spin remain buffered for publisher SSAT
HTML. Their behavior must be explicit in docs and tests:

- they may still call `buffer_publisher_response_async`;
- they must preserve parser-safe bid injection semantics;
- they must enforce the existing buffered body cap;
- they should remove or normalize headers such as `Transfer-Encoding` where the
  adapter already does so after buffering;
- they do not satisfy the Fastly true-streaming acceptance criterion and should
  not be described as true streaming in docs or comments.

Future work can add adapter-specific streaming support once each runtime has a
safe response-commit and body-streaming boundary.

## HTML post-processors

Any registered `IntegrationHtmlPostProcessor` means the HTML path requires the
full rewritten document. For this slice:

- Fastly SSAT HTML with post-processors must route to buffered mode.
- Next.js is explicitly accepted as buffered mode.
- The request-level Fastly route decision should use
  `IntegrationRegistry::has_html_post_processors()` or an equivalent presence
  check before requesting a streaming origin body.
- Tests should verify that a registry with a post-processor does not enter the
  true Fastly streaming path and does not call `with_stream_response()` for the
  publisher origin fetch.

Buffered post-processor mode must preserve the current observable ordering:
post-processors should see the rewritten HTML with the final bids script, not a
raw placeholder. The bounded buffered pipeline should therefore be:

```text
decode origin body
  -> lol_html rewrite with parser placeholder
  -> parser-safe late binding / bids replacement or EOF fallback
  -> full-document post-processors
  -> encode or buffer final body
```

No placeholder token may be visible to post-processors unless the post-processor
API explicitly opts into that in future work, and no placeholder may leak to the
client.

This does not prevent script rewriters or attribute rewriters from streaming;
those run inside `lol_html` and are distinct from full-document post-processors.

## Privacy and cache behavior

SSAT-assembled HTML can contain per-user slot state and bid data. This issue
must preserve the existing privacy contract:

```http
Cache-Control: private, max-age=0
```

and strip runtime/shared edge-cache headers, including:

```http
Surrogate-Control
Fastly-Surrogate-Control
CDN-Cache-Control
Cloudflare-CDN-Cache-Control
```

This applies before Fastly commits the streaming response. Streaming must not
weaken the final cache/privacy guard that protects responses with `Set-Cookie`
or per-user SSAT data.

### Transformed-response header normalization

Any processed HTML response has a different byte representation from the origin
response, even when `Content-Encoding` is preserved through decode/re-encode.
Before client commit, the streaming and buffered processed paths should remove
payload-derived headers unless they are recomputed for the transformed payload.
At minimum, processed publisher HTML should remove:

```http
Content-Length
Content-MD5
Digest
Repr-Digest
Content-Range
Accept-Ranges
ETag
```

`Content-Encoding` should be preserved when the response is re-encoded with the
same encoding. `Transfer-Encoding` remains adapter-owned and should continue to
be removed or normalized where the adapter already does so. Unmodified
pass-through streams keep origin payload validators and range metadata.

## Error handling

### Before client commit

Errors before `stream_to_client()` should return a normal error response where
possible:

- invalid origin configuration;
- backend registration failure;
- origin fetch failure before headers;
- unsupported route discovered before commit that cannot be safely streamed
  unmodified;
- processor construction failure;
- known oversized identity body from `Content-Length` preflight.

A streaming candidate that resolves to non-HTML, unsupported-encoding HTML, or a
bodiless status is not automatically an error: it may be streamed or returned
unmodified when headers can still be finalized safely. If an SSAT auction was
dispatched and the response cannot be processed, consume or abandon the dispatch
token explicitly so telemetry remains accurate.

### After client commit

Errors after headers are committed cannot be converted into a clean HTTP error
response. The streaming path should:

- log the error;
- emit abandonment/completion telemetry where applicable;
- drop/abort the streaming body without calling `finish()`;
- let the client observe a truncated response.

Mid-stream errors include:

- origin stream read failure;
- decompression failure;
- HTML processor failure;
- placeholder hold cap exceeded;
- decoded-input or processed-output cap exceeded;
- write failure to the client streaming body;
- compression finalization failure.

## Observability

Add low-cardinality logs or telemetry fields sufficient to understand streaming
behavior without exposing user data:

- route mode: `fastly_streaming`, `streamed_unmodified_non_html`,
  `streamed_unmodified_unsupported_encoding`, `buffered_post_processor`,
  `buffered_adapter`, `pass_through`, `buffered_unmodified`;
- request-level candidate decision and response-level route decision;
- whether bid insertion used `body_close`, `eof_fallback`, or
  `missing_head_eof_fallback`;
- decoded input bytes;
- processed output bytes;
- held-tail bytes;
- whether the response was compressed and which encoding was used;
- auction collect wait duration at the body-close hold;
- cap violation reason, if any.

Do not log bid payloads, EC IDs, cookies, consent strings, or full URLs with
sensitive query parameters.

## Testing strategy

### Core parser-safety tests

- Inline script contains `"</body>"`; auction collection is not triggered until
  the real parser-confirmed body close.
- JSON/script data contains escaped or literal `</body` text; no early hold.
- Real `</body>` split across origin chunks still produces a parser placeholder
  and bid injection before the close tag.
- Placeholder token split across processor output chunks is detected and
  replaced exactly once.
- Multiple body close tags do not inject bids multiple times and do not leak
  placeholders.
- Missing `</body>` appends bids or the SSAT fallback tail at EOF.
- Missing `<head>` plus missing `</body>` appends the minimal SSAT fallback tail
  at EOF without leaking placeholders.
- Normal bid injection still places bids before `</body>` when the close tag is
  present.

### Streaming cap tests

- Large identity HTML below the cap streams successfully.
- Decoded input exceeding the cap fails/aborts without allocating the full body.
- Processed output exceeding the cap fails/aborts.
- Highly compressible gzip/deflate/br HTML that expands over the decoded cap is
  rejected during streaming.
- Held-tail cap violation aborts rather than growing an unbounded hold buffer.
- A streaming-body test proves the true Fastly path consumes chunks incrementally
  and does not call `EdgeBody::into_bytes()` or an equivalent full-body materializer.

### Compression tests

For gzip, deflate, and brotli:

- compressed origin HTML streams through decode/rewrite/re-encode;
- decompressed output contains injected bids at the correct location;
- final compressed response can be decoded by a client;
- encoder finalization errors, where testable, are propagated as stream errors.

### Fastly adapter tests

- Fastly SSAT stream-eligible HTML preserves the origin body as
  `EdgeBody::Stream(_)` before client send.
- Fastly SSAT stream-eligible HTML does not call the buffered publisher response
  resolver.
- Fastly streaming uses an explicit publisher-streaming dispatch path and is not
  mistaken for an asset pass-through `EdgeBody::Stream(_)`.
- Fastly streaming removes `Content-Length` and other payload-derived headers
  for processed bodies.
- Fastly response headers are finalized before the streaming body is opened.
- Fastly post-processor-enabled HTML routes to buffered mode and does not request
  `with_stream_response()` from the publisher origin.
- Fastly streaming candidates that resolve to non-HTML or unsupported-encoding
  HTML stream unmodified safely and abandon any dispatched auction.
- Fastly `HEAD`, 204, 205, and 304 responses do not attach a streaming processed
  body.

### Non-Fastly adapter tests

- Axum publisher SSAT is explicitly buffered in this slice.
- Cloudflare publisher SSAT is explicitly buffered in this slice.
- Spin publisher SSAT is explicitly buffered in this slice.
- Buffered non-Fastly paths preserve parser-safe bid injection and EOF fallback.
- Buffered post-processor paths run parser-safe late binding before
  post-processors and never expose placeholders to post-processors or clients.

### Privacy regression tests

- SSAT HTML still emits `Cache-Control: private, max-age=0`.
- Shared/runtime cache headers are stripped from SSAT HTML.
- Streaming Fastly SSAT responses with cookies or per-user data do not regain
  shared cacheability after finalization.
- Processed streaming HTML strips stale entity validators/range headers while
  unmodified pass-through streams preserve them.

## Implementation phases

### Phase 1: Parser-safe late-binding in core

- Add per-request bid placeholder support to the HTML processor configuration.
- Add a streaming placeholder late-binder that scans processed uncompressed
  output and replaces the parser-inserted placeholder with bids.
- Replace raw `BodyCloseHoldBuffer` usage for SSAT collection with
  placeholder-triggered collection.
- Add EOF fallback bid append, including the missing-head minimal SSAT fallback
  tail when normal head injection never ran.
- Keep existing buffered adapters working through the new parser-safe path.
- Ensure buffered post-processor mode performs late binding before post-processing
  so post-processors see final HTML rather than raw placeholders.

### Phase 2: Streaming caps

- Add decoded-input and processed-output cumulative counters to the SSAT
  streaming loop.
- Add the concrete 64 KiB held-tail cap, clamped by the publisher body limit.
- Map cap violations to the existing proxy error type.
- Add tests for identity and compressed expansion cases.

### Phase 3: Fastly origin streaming and client streaming

- Add the two-phase Fastly route decision: request-level streaming candidates
  before fetch and response-level stream eligibility after origin headers.
- Request streaming origin responses only for Fastly request-level SSAT
  candidates.
- Preserve `EdgeBody::Stream(_)` through an explicit publisher-streaming dispatch
  path rather than the asset pass-through stream path.
- Add a Fastly entry-point path that finalizes headers, opens
  `stream_to_client()`, and drives the SSAT streaming loop into the
  `StreamingBody`.
- Stream non-HTML or unsupported-encoding candidate responses unmodified when
  safe, with auction abandonment telemetry.
- Ensure dispatched auctions are collected or abandoned on every exit path.

### Phase 4: Buffered-mode documentation and route guards

- Route full-document post-processor HTML to buffered mode before requesting a
  streaming origin response.
- Normalize transformed-response headers on both buffered and streaming
  processed HTML paths.
- Document Next.js as buffered mode for this slice.
- Document Axum, Cloudflare, and Spin publisher SSAT as buffered mode.
- Add adapter tests so future changes do not accidentally claim streaming parity.

### Phase 5: Verification

Minimum targeted verification for touched Rust code:

```bash
cargo fmt --all -- --check
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

If implementation touches JS/TS, also run:

```bash
cd crates/trusted-server-js/lib && npx vitest run
cd crates/trusted-server-js/lib && npm run format
```

## Acceptance criteria mapping

| Issue acceptance criterion                                                                                      | Design coverage                                                                                                                                                                                                      |
| --------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Fastly SSAT HTML path no longer requires full origin body materialization before sending client bytes.          | Two-phase Fastly route decision uses streaming origin bodies for request-level candidates, preserves publisher-streaming state explicitly, avoids `into_bytes()`/`take_body_bytes()`, and writes to `StreamingBody`. |
| Streaming path enforces a cumulative body cap without requiring a single full-body allocation.                  | Decoded-input and processed-output cumulative caps plus concrete held-tail cap.                                                                                                                                      |
| Body-close hold is parser-context-aware and does not trigger on `</body` literals inside inline scripts/JSON.   | `lol_html` inserts an opaque placeholder only at parser-confirmed body end tags; streaming loop scans processed output for that placeholder.                                                                         |
| EdgeZero/non-Fastly adapter behavior is either streaming-safe or explicitly documented/tested as buffered mode. | Fastly EdgeZero is true streaming; Axum, Cloudflare, and Spin are documented/tested buffered mode.                                                                                                                   |
| Tests cover large HTML, inline-script `</body` literals, missing body close tags, and normal bid injection.     | Core parser-safety and streaming cap test matrix, including missing-head EOF fallback.                                                                                                                               |
| Existing `private, max-age=0` SSAT privacy behavior remains unchanged.                                          | Privacy/cache section, transformed-header normalization, and regression tests.                                                                                                                                       |

## Follow-up work

- Make Next.js/RSC post-processing streaming-safe, if performance data justifies
  it.
- Add true publisher SSAT streaming for Axum, Cloudflare, and Spin where runtime
  response APIs allow safe header commit plus streaming body writes.
- Add SSAT compression offload (#858): request identity from origin and let the
  edge runtime compress client-facing HTML where beneficial.
- Add origin-template and transformed-template caching (#859) once streaming and
  parser-safe bid late-binding are stable.
