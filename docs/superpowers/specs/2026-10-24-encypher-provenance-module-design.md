# Encypher text provenance as a module

Status: implemented on `encypher/v2-provenance-module`, based on row 55 of the
stack tracked in #777 (`feature/55-scripts-in-crates` at 5be114773), with the
repository's CI gates run locally (section 4). Signing against the live
Encypher API has not been exercised (section 4.2).

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
  the same bound. Every failure emits the held document unchanged.
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

### 2.3 Permission contract

The module requires no permission and reads no gated value. It sends the
visible text of the page's article and the page address without its query.
A page whose origin set a cookie or sent a `Cache-Control` that forbids shared
storage is never sent; a test covers this. The module cannot detect a
personalized article that carries neither signal, so it does not exclude
reader data automatically. The deployer places the middleware only on public
article routes, through the `[[serve]]` entry's `path`, and has the origin mark
every personalized response private. The worker token is a module secret
setting, resolved from the secret store and never logged.

### 2.4 What the port removes

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
- Adapters: the Fastly store opens when linked; Axum KV persists and the
  timer fires while other work is pending. Core's test client rejects an
  over-limit body on direct and asynchronous sends. All four adapter
  collectors exercise exact-limit and over-limit bodies; Axum uses a chunked
  TCP response, Cloudflare and Spin use multi-chunk host-stream equivalents,
  and Fastly covers buffered reads and a crossing chunk yielded by a stream.

### 4.1 Runtime smoke

The Axum dev server ran with the module selected, a `[[serve]]` entry on
`/news/`, runtime KV on and a local origin serving a public and a private
article. Both pages returned `200` with the article intact. The public page
was held and passed through with `reason=http_error`, because the configured
API address had no reachable service. The private page was not held
(`gate=origin_private`) and nothing was sent for it. The configuration sets
`[geo] assume_single_jurisdiction = true`, which the stack requires once
`[ec] module` is set.

### 4.2 Limits

The runtime smoke pointed the module at a signing endpoint that was not
available, to exercise fail-open. Successful signing is tested in process
only, by the publisher-path tests above against a stub API. The live Encypher
API was not exercised.

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
