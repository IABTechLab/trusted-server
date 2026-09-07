# Parser-aware body hold and streaming-safe Next.js processing

**Date:** 2026-09-07

**Status:** Proposed

**Issue:** [IABTechLab/trusted-server#850](https://github.com/IABTechLab/trusted-server/issues/850)

## 1. Decision

Use `lol_html`'s structural `body` end-tag handler to place an internal,
request-unique control token in the transformed byte stream. Move the auction hold after
HTML parsing, recognize only that generated token, and replace it with the reader's bid
script after auction collection. Delete the pre-parser scan for the publisher-controlled
byte sequence `</body`.

Replace the full-document HTML post-processor with per-document streaming output
processors. Migrate Next.js RSC rewriting to a bounded streaming processor that emits
ordinary HTML immediately, holds only an unresolved ordered RSC group, releases the group
as soon as its T-chunks are complete, and restores the original bytes if the group cannot
be safely rewritten within its configured bound.

This is one delivery change with two coordinated parts. The parser-aware body seam removes
false auction stalls in arbitrary script, JSON, and comment data. The Next.js processor
removes the unconditional end-of-document buffer which would otherwise conceal that gain
on the issue's primary CMS.

## 2. Current behavior and correction to the issue report

The issue remains open and reproducible on `main` at `640e93d1`.

`publisher.rs` currently places `BodyCloseHoldBuffer` before the HTML processor. It scans
decoded origin bytes case-insensitively for `</body`, retains the possible six-byte prefix
across chunks, and reports the first match as the auction seam. That scanner has no HTML
context. A string such as:

```html
<script>
  self.__next_f.push([1, 'text containing </body> here'])
</script>
```

causes the publisher stream to wait for the auction before the real body end. A match split
across origin chunks has the same result.

The issue's original phrase "near-full-page hold" no longer precisely describes that
scanner in the current implementation. Once a match is found, the caller collects the
auction, processes the held tail, removes the hold, and resumes streaming. The false match
therefore causes an **early auction stall**, rather than retaining every later page byte.
The bug and its user-visible latency remain real.

Separately, `HtmlWithPostProcessing` accumulates all transformed output until EOF whenever
any `IntegrationHtmlPostProcessor` is registered. Next.js is the only current registrant.
This remains a true whole-document buffer and delays all output even when a document has no
RSC payload to rewrite.

The ESI work does not close #850. It already uses a `lol_html` body end-tag handler to place
`TEMPLATE_SEAM_PLACEHOLDER` at the structural seam, protecting cached templates from
`</body>` strings in scripts and comments. The ordinary inline auction path still uses the
raw scanner, and the Next.js post-processor still buffers to EOF. This design applies the
same structural principle to inline delivery without changing ESI cache assembly.

## 3. Goals

1. Make `lol_html` the sole authority for choosing an inline HTML body-close seam.
2. Stream the transformed prefix while the auction is pending and wait only when the
   parser-confirmed seam reaches the response controller.
3. Ensure publisher-controlled `</body>` text can never trigger the auction wait.
4. Remove unconditional whole-document buffering when Next.js is enabled.
5. Preserve Next.js URL rewriting, script order, React hydration, and RSC T-chunk length
   correction whenever a bounded group is complete.
6. Bound all Next.js deferral and degrade to byte-preserving output when safe rewriting is
   impossible.
7. Preserve compression, error, telemetry, CSP, and ESI template-cache behavior.

## 4. Non-goals

- Making authorized ESI template-cache fills stream. A complete transformed document is
  intentionally required before cache validation and insertion.
- Changing warm ESI template assembly or its stable `AD_ASSEMBLY_SEAM` format.
- Adding streaming response support to adapters that do not currently expose it.
- Expanding which Next.js attributes or URL shapes are rewritten.
- Changing the standalone `text/x-component` RSC Flight response processor.
- Adding another HTML tokenizer or parsing the document a second time.
- Guaranteeing a rewrite for malformed or over-limit RSC data. Hydration-safe unchanged
  output takes priority in those cases.

## 5. Considered approaches

### A. Parser-generated control seam plus streaming integration sessions — selected

The existing parser inserts a private token at the structural body end. A post-parser
controller recognizes the token and coordinates the asynchronous auction. Next.js uses a
per-response streaming session and defers only an unresolved RSC group.

This keeps one HTML parser, makes the async wait occur outside synchronous `lol_html`
callbacks, preserves output order, and limits changes to the core HTML/publisher pipeline
and Next.js registration.

### B. Return structural events from `StreamProcessor`

Change `StreamProcessor::process_chunk` to return bytes plus typed events and exact output
offsets. This avoids a generated byte token, but every processor, compression driver, and
caller must adopt the new result type. It still needs an ordered Next.js deferral layer.
The additional surface is not justified for the one asynchronous seam.

### C. Tokenize before the existing HTML processor

Run another HTML tokenizer over origin bytes to locate `</body>` before `lol_html` runs.
This duplicates parsing, must map an input boundary onto rewritten output, and can drift
from `lol_html` on malformed-but-renderable documents. It is rejected.

## 6. Processing architecture

For processable inline HTML on a streaming adapter, the body path becomes:

```text
origin chunks
  -> bounded decompressor
  -> lol_html structural and integration rewrites
  -> integration streaming output processors (Next.js when enabled)
  -> inline auction seam controller
  -> streaming compressor
  -> client
```

The important ordering change is `lol_html` before the auction seam controller. The current
pipeline holds origin bytes and then parses them. The new pipeline parses first, so only a
control token emitted by the parser can cause a wait.

Buffered adapters use the same processors and state transitions while writing into their
existing bounded output. They gain behavior parity and lose the redundant Next.js
whole-document accumulation, even though their platform response remains buffered.

Authorized ESI cold misses continue through their existing bounded finalizer. Their parser
emits the stable template placeholder, the completed transform is validated and stored,
and the reader-specific seam is assembled as it is today. They do not use the inline
request-unique token.

They also stop using the raw body-close hold. The stable template placeholder does not
depend on bid state, so the transform can run to completion while the auction remains in
flight. After transform and template validation, collect the auction before substituting
the current reader's seam content. The cold miss remains intentionally buffered for cache
insertion, but neither its parsing nor auction lifecycle depends on scanning for `</body>`.
An ESI response whose template-cache gate was rejected follows the ordinary inline deferred
seam path because `effective_assembly_mode` already reduces it to inline delivery.

## 7. Parser-confirmed inline auction seam

### 7.1 Control token ownership

When the response has a dispatched HTML auction and inline body injection is enabled, the
publisher creates one UUID-v4 token using the repository's existing WASM-compatible UUID
support. Its serialized form is an inert HTML comment:

```text
<!--ts-inline-body-close-<32 lowercase hex digits>-->
```

The token is request-private, never stored, and shared by exactly two components:

- `HtmlProcessorConfig`, whose body end-tag handler emits it once;
- the auction seam controller, which recognizes and removes it.

Use a distinct `BodyCloseInjection` variant for this token rather than overloading the ESI
`Marker` semantics. The ESI marker must be stable between requests; the inline token must
be unique to one response.

Select the deferred variant only when orchestration still owns a dispatched auction that
must run concurrently with body processing. Paths which have already collected the auction
retain immediate `InlineBids` insertion. Paths with neither a pending nor completed inline
auction emit no inline token. This prevents any constructor that lacks a seam controller
from producing an unresolved token.

Represent this choice explicitly at processor construction, for example as an immediate or
deferred inline seam mode. `PublisherBodyProcessor` must expose the generated deferred token
to its async driver; callers must not independently generate a second value. The exact Rust
type is an implementation-plan decision, but one construction result must own both the
configured parser and the matching controller token.

Publisher bytes cannot predict the generated value. A source document containing the
fixed prefix or another response's token is ordinary content. The implementation must
match the complete current-response token and retain at most `token.len() - 1` candidate
bytes between processed chunks.

### 7.2 Parser behavior

The existing `element!("body", ...)` handler remains the structural hook. For the inline
deferred variant, its first available end-tag handler inserts the token immediately before
the structural `</body>`. The existing single-injection guard continues to handle documents
with multiple body elements.

The handler no longer reads `ad_bids_state` for deferred inline delivery. Synchronous
parser work only identifies the seam; asynchronous auction collection and bid construction
remain in publisher orchestration.

If `lol_html` exposes no body end tag because the body is implicit, truncated, or absent,
the handler emits no token. The existing diagnostic warning remains appropriate when the
server-side ad path expected an insertion point.

### 7.3 Seam-controller state machine

The controller has three states:

1. **Searching:** stream all processed bytes except the suffix that could begin the exact
   token.
2. **Found:** return the prefix before the token to the caller. The caller must make that
   prefix available to the client before awaiting auction collection. After collection,
   emit the bid script in place of the token, then stream the remaining bytes.
3. **Released:** pass every later processed byte through without scanning or copying.

On processor EOF while still searching, release the retained candidate bytes, collect the
auction for completion and telemetry, finalize compression, and inject no bids. The token
must never be sent to the client.

This state machine replaces `BodyCloseHoldBuffer`; no production code may search origin or
transformed HTML for the literal `</body` to coordinate an auction.

### 7.4 Async driver changes

The shared sync-reader and async-stream helpers must follow the same order:

1. decode an origin chunk;
2. run `PublisherBodyProcessor`;
3. pass its output through the seam controller;
4. encode and expose the ready prefix;
5. only then collect at a reported seam;
6. encode the bid script and held suffix;
7. continue processing.

The lazy Fastly stream must yield all ready segments before entering the auction `await`.
Write-sink drivers must write and flush the ready prefix before collection. Common step and
finish helpers should remain shared so live-stream and buffered-reader behavior cannot
drift.

Authorized ESI template transforms use the no-hold chunk driver because their parser emits
reader-neutral markup. Their finalizer collects the already-dispatched auction after the
complete transform and before assembling the current reader's response. This is an EOF
collection path, not an inline body-seam event.

## 8. Streaming integration output processors

### 8.1 Contract

Replace the full-document `IntegrationHtmlPostProcessor` API with a factory for
per-document streaming sessions. Registrations remain immutable and `Send + Sync`; each
factory creates a mutable session for one HTML document using owned request origins and a
clone of `IntegrationDocumentState`.

Each session implements the existing chunk contract:

```text
process_chunk(input, is_last) -> Result<Vec<u8>, io::Error>
```

It may retain a documented, bounded subset of output between calls. It must emit all
retained output or return an error at EOF. Generated control placeholders must never reach
the caller.

`create_html_processor` constructs one `HtmlRewriterAdapter`, then wraps its output in the
registered sessions in registration order. With no streaming output processor, it returns
each `lol_html` output chunk immediately. Delete `HtmlWithPostProcessing`'s
`accumulated_output`, `decoded_input_len`, and EOF-wide post-processing branch.

The registry methods and builder terminology change from `html_post_processor` to
`html_stream_processor`. This is crate-internal API and has only the Next.js consumer in
the current tree.

All mutable script-fragment and placeholder state must live in
`IntegrationDocumentState` or the per-document session. Do not retain request-progress
buffers such as `NextJsNextDataRewriter::accumulated_text` on the registry-owned rewriter:
the registry is shared, so request interleaving could otherwise combine fragments from
different documents. Moving this state is required by the new session boundary, not a
separate integration refactor.

### 8.2 Ordering and bounds

Processors must preserve document byte order. A processor may withhold bytes after a
control placeholder when earlier content cannot yet be finalized, but it may not emit
later markup before that placeholder is resolved or restored.

Every processor owns its own semantic limit. The outer publisher decoded-input and output
bounds remain defense in depth for buffered response paths; they do not justify an
unbounded integration session on streaming paths.

## 9. Next.js RSC streaming design

### 9.1 Parser-side capture

Keep `NextJsNextDataRewriter`'s current per-script fragmentation behavior. Intermediate
`__NEXT_DATA__` text fragments are suppressed and accumulated; the complete text node is
rewritten or restored on `last_in_text_node`. Bound this per-script accumulator by
`publisher.max_buffered_body_bytes`. Before another fragment would exceed the bound,
replace the current fragment with all previously suppressed text plus the current fragment,
then pass the rest of that script through unchanged. This changes an oversized script from
"buffer then reject the whole response" to "preserve this script without rewriting" while
the surrounding document continues streaming.

Store that accumulator in the current `IntegrationDocumentState`, keyed separately from
RSC group state. The immutable `NextJsNextDataRewriter` retains only configuration and its
compiled URL matcher.

Change the RSC script rewriter to use the same discipline. For each `script` text node:

- accumulate and suppress fragments until `last_in_text_node`;
- restore non-RSC or unparseable script text exactly;
- for a recognized `self.__next_f.push([1, ...])`, store the original payload in
  request-scoped state and emit a request-scoped placeholder for that payload range.

Bound RSC per-script text accumulation by `max_combined_payload_bytes`. On overflow, restore
the suppressed prefix in the current text fragment, pass the rest of that script through,
and mark the document's RSC output session as bypassed. An incomplete payload cannot be
safely separated from later script continuations.

Placeholder names include a per-document UUID namespace plus a monotonically increasing
index. A publisher string that resembles the fixed placeholder prefix is not actionable
without the current namespace.

The factory and parser rewriter obtain the namespace from one shared per-document state
created before the first HTML chunk is processed. Namespace creation must be idempotent so
factory and handler construction order cannot produce different values.

### 9.2 Output-session state

The Next.js session owns:

- the request origin and rewrite configuration;
- the placeholder namespace;
- a FIFO of captured original payloads;
- the output held behind the first unresolved placeholder;
- the combined payload byte count and total held-output byte count;
- whether one over-limit/incomplete warning has been emitted for the current group.
- whether RSC rewriting has entered byte-preserving bypass for the rest of the document.

It scans only for its generated placeholder namespace. Non-candidate output streams with
at most `longest_placeholder_len - 1` bytes retained for chunk-boundary matching.

### 9.3 Group resolution

When the next placeholder is available in both output and captured state:

1. Append its original payload to the current group.
2. Test the combined payloads with the existing T-chunk parser.
3. If every declared T-chunk is complete, run
   `rewrite_rsc_scripts_combined_with_limit`, verify that the output count equals the input
   count, substitute every group placeholder in order, and release the entire held segment.
4. If a T-chunk is incomplete, retain the group and the following output until another RSC
   payload arrives.

A complete single-script payload therefore releases on the same processor call. A
cross-script payload releases as soon as the script containing its final declared byte has
been parsed; it does not wait for document EOF.

Content between grouped scripts must remain in the held segment. Although that may include
ordinary HTML, emitting it early would place it before an earlier executable script and
change document execution order.

### 9.4 Safe fallback

`integrations.nextjs.max_combined_payload_bytes` becomes the hard maximum for each of:

- the sum of original payload bytes in one unresolved group;
- the transformed output bytes held behind that group.

The configuration key and default remain unchanged. Its guide text must explain the
broader streaming-memory meaning.

Before either counter would exceed the limit, replace every placeholder already held with
its exact original payload and release the group unchanged. If the group was incomplete or
invalid, enter byte-preserving RSC bypass for the rest of the document: every later RSC
placeholder is replaced immediately with its original payload and is never independently
rewritten. A later payload may be the continuation of the earlier T-chunk, so rewriting it
alone could change content without correcting the header already emitted.

If a complete, internally valid group is restored only because its held-output size reached
the limit, clear that group and allow the next independent RSC group to be considered. In
either fallback mode, the processor must not repeatedly absorb an unbounded segment.

At EOF, apply the same unchanged restoration to an incomplete or invalid group. If the
rewrite function returns a different payload count or any generated placeholder remains
after substitution, restore the entire group from originals instead of emitting partial
rewrites. Emit one warning with reason, payload count, and byte counts; never include
payload text or URLs.

This fallback deliberately favors hydration over proxy URL coverage. It matches the
existing over-limit safety policy while ensuring that no publisher document is withheld
without a bound.

"Unchanged" here means unchanged relative to the first-pass `lol_html` output with RSC URL
rewriting disabled. Other intentional transformations already performed by the HTML
processor remain present; the fallback must restore the exact captured RSC payload bytes
and preserve their surrounding serialized markup.

### 9.5 Relationship to the auction seam

The Next.js session runs before the inline seam controller. Normally it resolves RSC groups
before `</body>`, allowing the body-close token to reach the auction controller at the
structural location. If an incomplete group extends to the body close, the Next.js EOF or
limit fallback restores and releases it; the token then triggers collection. No component
searches RSC content for `</body`.

## 10. Error handling and observability

- Preserve existing decoder, processor, encoder, auction-abandonment, and response error
  mappings using `Report<TrustedServerError>` at orchestration boundaries and `io::Error`
  inside `StreamProcessor` implementations.
- A `lol_html` processing error abandons an outstanding auction exactly once.
- A stream read or compression error preserves the existing terminal telemetry reason.
- Missing body close is not a parsing error; it is the no-injection EOF path described
  above.
- Next.js malformed, incomplete, count-mismatched, or over-limit data is restored unchanged
  and logged once per affected group.
- Log completed cross-script RSC groups at debug level with payload count and byte counts.
- Never log HTML, RSC payloads, rewritten URLs, bid contents, or generated control tokens.

No new metrics or response headers are required. Existing auction telemetry is sufficient
to observe completion and abandonment; streaming correctness is enforced by tests rather
than timing logs.

## 11. Compatibility and security properties

- HTML output without an auction or streaming integration remains byte-for-byte equivalent
  to the current `lol_html` path.
- `__NEXT_DATA__`, non-RSC scripts, attribute rewriting, head injection, CSP nonce handling,
  and DataDome suppression retain existing behavior.
- Per-document fragment state cannot cross-contaminate concurrent or interleaved responses.
- Inline and ESI seam tokens are distinct. An inline token is never cacheable; an ESI
  template marker is never interpreted by the inline controller.
- UUID token generation uses an existing dependency and runtime capability; no new source
  of randomness is introduced.
- A publisher cannot deliberately trigger the inline wait using a known fixed string.
- Generated tokens and placeholders are removed on success and fallback paths.
- The Next.js memory limit is checked before growing either retained buffer beyond the
  configured ceiling.
- Per-script `__NEXT_DATA__` and RSC text accumulation is bounded before complete script
  classification is available.
- Script order and text are preserved when rewriting is skipped, preventing hydration
  corruption under adversarial or malformed payloads.

## 12. Test and acceptance matrix

### 12.1 Parser-aware body seam

Add focused tests proving:

- `</body>` inside ordinary script text, `__next_f` payloads, JSON, escaped strings, and
  comments does not report a seam;
- false literals split at every token boundary do not report a seam;
- mixed-case structural `</BoDy>` and a structural close split across origin chunks emit
  exactly one generated control token through `lol_html`;
- a trailing comment containing `</body>` cannot move the seam;
- multiple body elements still create one insertion;
- a document without an explicit body end emits no token and leaks no internal bytes.

### 12.2 Auction delivery ordering

Use the existing pending-auction stream fixtures rather than wall-clock sleeps:

- poll a response whose script contains `</body>` while the auction remains pending and
  assert that output through and beyond that script is available;
- assert that polling stops only when the parser-generated seam is reached;
- complete the auction and assert that bids occur immediately before the structural body
  end;
- split the generated token across processed chunks and obtain identical output;
- verify EOF-without-seam completes telemetry and injects no bids;
- verify read, parse, and encode failures abandon the auction once.

These tests are the direct regression proof for #850. A final-output assertion alone is
insufficient because buffered and streaming implementations can produce identical bytes.

### 12.3 Next.js streaming

Add unit and pipeline tests for:

- a Next.js-enabled document with no RSC scripts emits intermediate output before EOF;
- complete single-script RSC payloads rewrite and release without EOF;
- input-chunk fragmentation within an RSC script is accumulated and rewritten once;
- multiple independent payloads release independently in document order;
- a T-chunk split across two or more scripts updates the original header length and releases
  immediately when complete;
- non-RSC scripts and interstitial markup retain exact relative order;
- the payload-byte limit restores a group byte-for-byte;
- the held-output limit restores a group before exceeding the limit;
- invalid and EOF-incomplete T-chunks restore originals;
- an incomplete over-limit group forces later continuation scripts to remain unchanged;
- oversized fragmented `__NEXT_DATA__` and RSC scripts release suppressed text and stream
  the rest unchanged;
- two interleaved processor instances never share `__NEXT_DATA__`, RSC payload, namespace,
  or bypass state;
- rewrite count mismatch and placeholder-remnant safeguards restore originals;
- no placeholder namespace appears in any final output;
- `__NEXT_DATA__` fragmentation and configured-attribute behavior remain unchanged.

### 12.4 Compression, ESI, and adapters

- Exercise identity, gzip, deflate, and Brotli through the new parse -> seam -> encode order,
  including trailer preservation and multi-member gzip coverage already present in the
  publisher suite.
- Retain and run ESI cold-miss, warm-hit, publisher-collision, script-literal, and
  trailing-comment seam tests.
- Prove an authorized ESI cold miss reaches transform EOF without waiting at either a real
  or publisher-authored `</body>` sequence, then collects once before reader assembly.
- Fastly tests must prove lazy streaming order. Axum, Cloudflare, and Spin tests must prove
  final-byte parity on their buffered response paths.

### 12.5 Required verification

Run the repository-prescribed gates:

```bash
cargo fmt --all -- --check
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
```

The JavaScript suite is not required unless implementation changes JavaScript or
TypeScript. Documentation formatting is required after updating the Next.js guide.

## 13. Documentation changes

Update `docs/guide/integrations/nextjs.md` to state:

- HTML outside unresolved RSC groups streams as it is transformed;
- cross-script T-chunks may briefly defer an ordered group;
- `max_combined_payload_bytes` bounds both combined RSC data and output held for that group;
- over-limit, invalid, or incomplete groups are emitted unchanged to preserve hydration.

Do not describe the integration as whole-document buffered after this change. Do not claim
that every RSC URL is rewritten when the documented safety fallback applies.

## 14. Completion criteria

#850 is complete when all of the following are true:

1. `BodyCloseHoldBuffer` and every auction-coordination scan for `</body` are removed.
2. Inline auction collection is triggered only by a `lol_html`-placed, request-private seam
   token or by EOF cleanup.
3. Tests prove meaningful response bytes pass a false `</body>` literal while the auction
   remains pending.
4. Enabling Next.js no longer makes every intermediate HTML processor result empty until
   EOF.
5. Cross-script RSC behavior, order, length correction, and bounded unchanged fallback are
   covered by tests.
6. No generated control token or placeholder reaches a client or shared cache.
7. Existing ESI assembly tests and all applicable CI gates pass.
