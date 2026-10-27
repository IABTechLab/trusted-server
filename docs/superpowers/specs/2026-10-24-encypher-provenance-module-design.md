# Encypher text and routed-media provenance as a module

Status: implemented scope on `encypher/all-media-provenance`, extending the
text-provenance module described below. Local Axum, real HTTPS API and browser
verification covers the routed subset (section 4). Full repository landing
gates remain separate. This document claims neither complete page-media
coverage nor a production deployment.

## 1. Problem

A publisher who wants proof of authorship carried inside article text needs a
signing step at the edge, after the page is assembled and before it reaches the
reader. Encypher's earlier contribution did this as an integration inside core,
with core changes of its own: an HTML stream processor factory, a request
context type, an asynchronous finish for stream processors, a platform timer
and runtime KV on each adapter. The stack moves every vendor out of core and
lets a module change a page through a middleware alone, so the contribution has
to move onto those interfaces, and the core seams it needs have to be neutral.

## 2. Design

### 2.1 What core gains, for any module

1. **An awaited end of document.** `StreamProcessor::async_finish` lets a
   processor finish its last output asynchronously, with a deadline.
   `StreamProcessor::finish` ends a processor once; a processor made of others
   ends each in order, so each is handed what the one before it finished with.
   Every publisher path that can await ends its processor this way: the
   no-hold pipeline, both auction-hold loops, the serve pass on a shared
   template and the lazy stream. A body read whole takes the same chunk
   pipeline as a streamed one, and the synchronous `stream_publisher_body`
   path is removed, so no path silently skips a finish. A finish that outlives
   its deadline on a runtime with a timer is dropped and the held bytes go on.
2. **`PlatformTimer`**, optional on `RuntimeServices`. The Cloudflare, Spin and
   Axum adapters provide one. Fastly provides none: its backend setting bounds
   the first byte and each between-byte gap, not total elapsed work. A peer can
   keep a request open beyond `max_hold_ms` by sending each byte before the gap
   timeout, and Fastly KV calls have no module deadline.
3. **Response facts for middleware.** `MiddlewareContext` carries the
   response `status` and `origin_private`, whether the origin marked its
   response as one reader's by a cookie or a `Cache-Control` that forbids
   shared storage, read before core writes its own cache policy.
4. **Runtime KV where the stack left it unavailable.** Fastly opens the store
   linked as `trusted_server_kv`; Axum opens a persistent store at
   `TRUSTED_SERVER_KV_PATH`, running each operation on the blocking pool so a
   timer raced against it can fire. Cloudflare and Spin already open their
   declared store on the stack and are unchanged.
5. **An optional outbound response-body bound.** A caller can ask every
   adapter to reject a response before buffering or yielding more than a
   chosen byte count. Axum reads reqwest chunks, Cloudflare reads its
   `ByteStream`, Spin reads `IncomingBodyExt::stream`, and Fastly reads
   `fastly::Body`. Axum, Cloudflare and Spin can inspect a transport or
   host-created chunk only after that one chunk exists, but none collects the
   complete response before enforcing the bound. Spin applies the same limit
   to gzip and Brotli output.
   Fastly preserves the limit on a direct pending wait and refuses bounded
   handles through `select`, whose host API does not preserve wrapper metadata.
6. **Original-source protection**, default off on `Middleware`. The applicable
   fetch and serve chains supply classifiers through `source_protection`; core
   knows only the exact byte ranges they return. Before any HTML transformation,
   a selected policy holds the complete original under the publisher body limit.
   Opaque comment markers replace protected slices while core and middleware
   work. The pass checks marker collision, presence, order and uniqueness, then
   restores the original slices before any byte leaves the pass or enters the
   shared-template cache. Cached reader copies take the same protection around
   serve transformations. Invalid ranges, marker damage and limit excess refuse
   the pass. A policy applies even when the module's later request gate declines
   signing; it is selected by the actual path/media-type chains, not by rehosting.
   This bounds memory and delays the first transformed byte until the full
   document has been classified and restored.
   The transformation view contains opaque comments, not the protected source.
   Raw whole-HTML signing/digest middleware must not bind that intermediate
   view and assume restoration preserves its binding. Encypher's article plan
   key and signing payload use canonical text, which ignores SVGs and comments;
   the random marker therefore enters neither that text nor its cache identity.
   This contract does not establish compatibility for arbitrary future
   middleware that binds the complete HTML byte sequence.

### 2.2 The module

`crates/provenance/encypher`, `provenance.encypher`, selected by
`[provenance] module = "encypher"` and placed by a `[[serve]]` entry.

- **Preparation** applies the gates with no I/O: `GET`, status `200`, an
  origin response that is not one reader's, and a host equal to `domain` with
  or without `www.`. API, feed, admin, well-known and asset paths are left out
  of the chain by `handles_path`.
- **Create** returns a stream processor that holds the document up to
  `max_document_bytes`, and signs in its asynchronous finish: policy digest,
  plan cache, sign call, cache writes, render. Before allocating rendered
  output, it checks the exact marker and escaped manifest expansion against
  the same bound. A shared policy failure leaves the whole document unchanged.
  Article and individual media failures are isolated: failed items stay
  unchanged while other eligible parts may still be signed.
- **Serve phase only**, so a failed signing is never stored as the shared
  template every reader receives.
- **Cache integrity** keeps positive plans and policy digests apart from
  negative results, reads a positive first, and uses versioned namespaces:
  `encypher:plan:v4:`, `encypher:plan-skip:v4:`,
  `encypher:policy:v3:` and `encypher:policy-skip:v3:`. Malformed or
  structurally invalid entries are deleted best effort. An alignment failure
  for one HTML representation does not delete a valid plan.
- **Content identity** remains the policy digest, credential context and
  canonical article-text hash, not the page URL. The matching Encypher server
  contract deliberately deduplicates equal canonical text across URL or title
  changes. A reused plan's manifest URL can therefore retain the first
  canonical source URL.
- **Original SVG preservation** uses `embedded::scan_html` and its existing
  namespace-aware provenance/equivalence classifier for both source protection
  and signing decisions. A carrier or provenance reference after an editable
  descendant protects the complete original outer SVG. Protected descendants
  never reach core URL/text rewriting or middleware element, text or signing
  handlers. Unclosed or structurally uncertain ranges remain opaque. Unsigned
  SVGs still pass through the normal normalization and signing path. No rehost
  route is required for protection.

### 2.3 Routed and embedded media

The selected module also supplies a vendor-neutral `MediaSigner` to core.
Publisher and asset routes can sign bounded public media responses recognized
by their response type and verified bytes, including extensionless paths.
Images, audio, video and fonts are candidates, not a promise that every format
is supported. Existing embedded, XMP-referenced and HTTP-linked provenance is
retained rather than replaced. Fragmented ISO BMFF init and media segments
remain unchanged; a progressive MP4 can take the normal signing path.

The HTML serve processor signs eligible `data:` URIs and inline SVG in the
source document. An unsigned inline SVG is signed only as a standalone XML
document equivalent to the browser's HTML-parsed tree. A path-scoped
`provenance.encypher.stylesheet` serve entry signs eligible embedded data in
CSS. It must cover only stylesheets the publisher knows are not loaded with
subresource integrity.

An asset route's opt-in `rehost_page_references` moves eligible source HTML
media and stylesheet references onto that route. The route must preserve the
exact upstream resource identity; moved CSS resolves its own references
against its upstream URL. Private, integrity-protected and otherwise unsafe
resources keep their existing refusal behavior. The
[asset-route guide](../../guide/asset-routes.md) defines routing, relocation,
freshness and Image Optimizer behavior.

`max_media_bytes` bounds each captured or decoded item;
`max_media_sign_calls_per_document` bounds uncached embedded-media calls
(default `8`, allowed `1..=128`). Cache hits use no call budget. Exhaustion
leaves remaining items unchanged without a negative cache entry. Document
time and output-size limits still apply.

The prepared large-media lane is opt-in (`max_large_media_bytes = 0` by
default). It hashes a version-pinned source in bounded chunks and requests
detached provenance without uploading or buffering the complete media.
Admission requires the source/version checks, matching prepared API and signed
probe, streaming support, and one enforceable wall-clock budget for hashing
and signing. Fastly supplies no such total timer, so this lane stays closed
there even when the API can sign the source. A refusal delivers an unsigned
fresh origin response; it does not weaken deadline or source-binding guards.

### 2.4 Permission contract

The module requires no permission and reads no gated value. It sends the
visible article text or eligible bounded media bytes, together with the
reader-facing address without its query and the applicable source metadata.
The prepared lane sends source identity and digest instead of the whole file.
A page whose origin set a cookie or sent a `Cache-Control` that forbids shared
storage is never sent; a test covers this. The module cannot detect a
personalized article that carries neither signal, so it does not exclude
reader data automatically. The deployer places the middleware only on public
article routes, through the `[[serve]]` entry's `path`, and has the origin mark
every personalized response private. The worker token is a module secret
setting, resolved from the secret store and never logged.

### 2.5 What the port removes

The `enabled` flag (selection runs the module), `path_prefixes` (the entry's
`path` covers it), the `[integrations.encypher]` table, the HTML stream
processor factory and request context, the shared-template refusal (the serve
phase runs on each reader's assembled copy), and the synchronous publisher
body path.

## 3. Configuration

```toml
[provenance]
module = "encypher"

[provenance.encypher]
api_base_url = "https://api.example.com"
domain = "publisher.example.com"
worker_token = "encypher_worker_token"

[[serve]]
media_type = "text/html"
path = "/news/"
middleware = ["provenance.encypher"]
```

The settings below belong to `[provenance.encypher]`. Unknown keys are refused.

| Setting                             | Default   | Validation and effect                                                                                                                                                        |
| ----------------------------------- | --------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `api_base_url`                      | Required  | HTTPS URL with a host, no query or fragment                                                                                                                                  |
| `domain`                            | Required  | Bare lowercase hostname without `www.`                                                                                                                                       |
| `worker_token`                      | Required  | Secret-store entry name, resolved when settings load                                                                                                                         |
| `article_selector`                  | Empty     | At most 91 characters; simple `.class`, `[attr=value]` or tag selector; malformed syntax is refused. Empty or valid-but-absent overrides retain the normal boundary fallback |
| `min_text_length`                   | `50`      | `50..=50000` Unicode scalars                                                                                                                                                 |
| `max_document_bytes`                | `5000000` | At least `1024`; effective bound also capped by `publisher.max_buffered_body_bytes`                                                                                          |
| `timeout_ms`                        | `3000`    | `250..=10000`; API first-byte timeout                                                                                                                                        |
| `max_hold_ms`                       | `5000`    | `500..=15000`, strictly greater than `timeout_ms`; total document hold where a timer exists                                                                                  |
| `plan_ttl_seconds`                  | `604800`  | `60..=31536000`                                                                                                                                                              |
| `policy_ttl_seconds`                | `300`     | `60..=300`                                                                                                                                                                   |
| `negative_ttl_seconds`              | `300`     | `60..=86400`                                                                                                                                                                 |
| `max_media_bytes`                   | `5000000` | `1024..=8000000`; bounded binary or decoded embedded item                                                                                                                    |
| `max_media_sign_calls_per_document` | `8`       | `1..=128`; uncached embedded calls per HTML/CSS pass                                                                                                                         |
| `max_large_media_bytes`             | `0`       | Disabled at `0`; otherwise `16777216..=9007199246352383` bytes                                                                                                               |
| `max_large_media_ms`                | `60000`   | `5000..=600000`; one budget for namespace resolution, cache lookup and newly launched hash/sign work                                                                         |

### Upgrade and rollout

An existing deployment selecting the Encypher module begins sending eligible
bounded media to the signing API after this upgrade, using the defaults above.
Embedded media on admitted HTML is included. There is no separate bounded-media
disable setting: deselecting the module also disables its article signing.
The large-media lane remains default-off.

Deploy the matching API first. Before upgrading an enabled edge, check media
upload approval, API capacity, route scope, document/output limits and cold-page
latency. Verify terminal media bytes and rendering under the deployed adapter.
Rehosting and stylesheet processing require their explicit route/serve entries;
the upgrade does not intercept direct third-party or script-generated media.

## 4. Tests

- Core: `finish` ends each processor after the one before it has finished; a
  finish past its deadline passes the held bytes on; a streamed body without
  an auction takes the chunk pipeline; the parser-confirmed auction hold
  injects bids before `</body>` into a document a middleware processor held
  and finished asynchronously.
- Module: the request gates, policy and sign flows, credential-bound cache
  bypass, positive/negative cache races, malformed-entry recovery, output
  expansion at the buffered publisher limit, configuration bounds, the
  missing-entry refusal, path exclusions, and publisher-path signing through
  both the buffered and streaming finalizer against a stub origin and API. The
  tests also verify the address without its query and the private-page gate.
  Original-source regressions cover origin-host attributes and text inside a
  protected SVG, late carriers/references, nested embedded media, surrounding
  URL changes, self-closing protected SVG beside unsigned normalized/signable
  SVG, bounded refusal, and real cold/warm shared-template publication through
  both finalizers. Core tests cover fetch and serve descendant handlers plus
  marker loss, duplication, reordering, alteration, invalid ranges and bounds.
- Adapters: the Fastly store opens when linked; Axum KV persists and the
  timer fires while other work is pending. Core's test client rejects an
  over-limit body on direct and asynchronous sends. All four adapter
  collectors exercise exact-limit and over-limit bodies; Axum uses a chunked
  TCP response, Cloudflare and Spin use multi-chunk host-stream equivalents,
  and Fastly covers buffered reads and a crossing chunk yielded by a stream.
- Media and routing regressions cover binary signing and existing-provenance
  preservation, embedded data and SVG equivalence, prepared-source version and
  deadline refusal, rehosting identity and HTML base resolution, CSS relocation
  and freshness, and prefix-aware reader addresses.

### 4.1 Runtime smoke

Recorded local verification on 2026-10-27 used Axum, the real HTTPS signing API
and Chromium:

- All 148 edge-served browser media responses in the captured replay verified;
  107 sequential network responses, 138 source-inline SVGs and 186 data URIs
  also verified in their respective checks.
- Exact-source first/repeat checks and four concurrent API probes verified.
  The API recorded 214 media requests and 280 policy requests, all `200`, with
  no `500` or `429` responses.
- Six desktop/mobile browser flows covering control, generic-host and
  prefix-addressed serving rendered equivalently, with no broken images.

Native core and module runs reported 3,149 and 210 passing tests respectively.
Native/WASM adapter builds, clippy and Rust formatting checks were also
recorded. These are scoped receipts, not proof that every command in the
current [AGENTS.md CI gates](../../../AGENTS.md#ci-gates) has run against the
final landing tree. Final gate execution belongs to the landing operator.

### 4.2 Verification limits

The browser run still loaded 76 direct external responses and two
script-created SVGs unsigned. These requests and nodes never entered the
configured signing paths. No new integration, exclusion or interception of
those resources is part of this scope.

Prepared large-media API signing has separate proof, but Fastly cannot
enforce the complete hash-and-sign deadline and therefore refuses that lane.
Neither the local Axum run nor Viceroy proves deployed Fastly Image Optimizer
behavior. Verification using a locally supplied detached manifest does not
prove that the emitted manifest URL is publicly retrievable. Those are
distinct deployment checks, not results established by this run.

## 5. Limits and operator obligations

1. Spin's declared KV store is `default`, which also holds the settings.
   Encypher entries have the four exact namespaces above. Spin KV supplies no
   TTL, so `expires_at` prevents stale reads and on-read deletion removes
   encountered stale entries, but abandoned content keys need the
   namespace-retirement procedure in the Spin deployment guide.
2. Holding the page delays its first byte by the signing time on a cache miss.
   `max_hold_ms` is a total module bound on Cloudflare, Spin and Axum. Fastly
   has only first-byte and between-byte transport bounds, as described above.
3. The module skips pages the origin marks private, but cannot detect other
   personalized pages. A deployment places it on public article routes.
4. Routes cover only bytes that pass through them. Direct third-party frame or
   script requests and SVG created later by scripts require separate evidence
   and are not covered by this implementation.
5. Enabling a large-media size limit does not make the lane available without
   an enforceable total deadline. Fastly remains closed; do not substitute its
   first-byte or between-byte timeout for that deadline.
6. A detached-format deployment must prove public retrieval of its emitted
   manifest URL. A local sidecar backchannel proves neither public routing nor
   deployment availability.

## 6. Landing evidence checklist

- [x] Implement routed binary signing, source HTML embedded-media signing and
      opt-in rehosting with existing privacy, integrity and byte-identity guards.
- [x] Record local real-API and browser results for the implemented subset.
- [x] Keep direct external and script-generated media outside the coverage claim.
- [x] Keep Fastly's large-media deadline refusal in force.
- [x] Complete the full current repository gate set: all 12 `AGENTS.md` gates
      passed on 2026-10-27 after the deadline and selector repairs, including
      all eight target-matched clippy aliases, five adapter test aliases,
      cross-adapter parity, JS, formatting and operator-script checks.
- [ ] Establish public detached-manifest retrieval and deployed Image Optimizer
      behavior before claiming either deployment property.

## Review log

- 2026-10-27 (GPT-6.1-Sol): reconciled the design, operator contract and
  recorded verification with the implemented routed subset. No tests or
  runtime checks were executed during this documentation pass. The existing
  spec contained no historical review log; this entry does not establish the
  two independent agent review rounds required by `CONTRIBUTING.md`.

- Round 1 (2026-10-27, independent GPT-5.5): checked the spec against adapter,
  module, source-preservation, cache, privacy and routing code. Found namespace
  resolution outside the large-media deadline, malformed selector admission,
  inaccurate atomic-failure wording, incomplete configuration bounds and an
  omitted existing-deployment rollout account. The documentation now records
  partial success, defaults/bounds and automatic bounded-media activation.
  Parent reproduced the deadline and selector defects with failing regressions.
  Source repairs now share the remaining budget and validate selectors through
  the matching parser.

- Round 2 (2026-10-27, independent GPT-6 Astra): verified the repaired normal
  large-media path, selector admission/fallback and corrected specification.
  Found the same deadline and no-timer guards missing on origin `304`
  revalidation. Parent reproduced all three added revalidation regressions
  before repair. The revised path shares one remaining budget across namespace
  and cache lookup, refuses before I/O without a timer or positive budget, and
  preserves the original `304` on refusal. Independent reinspection found no
  remaining material premerge defect in that review scope. Parent then passed
  all 27 large-media tests, including warm-cache and origin-manifest behavior.
  The reviewers did not run checks; the final repository gate receipt is
  tracked separately above.
