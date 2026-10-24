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
   Axum adapters provide one; a runtime whose host calls block the guest
   provides none, and work there is bounded by transport timeouts.
3. **Response facts for middleware.** `MiddlewareContext` carries the
   response `status` and `origin_private`, whether the origin marked its
   response as one reader's by a cookie or a `Cache-Control` that forbids
   shared storage, read before core writes its own cache policy.
4. **Runtime KV where the stack left it unavailable.** Fastly opens the store
   linked as `trusted_server_kv`; Axum opens a persistent store at
   `TRUSTED_SERVER_KV_PATH`, running each operation on the blocking pool so a
   timer raced against it can fire. Cloudflare and Spin already open their
   declared store on the stack and are unchanged.

### 2.2 The module

`crates/provenance/encypher`, `provenance.encypher`, selected by
`[provenance] module = "encypher"` and placed by a `[[serve]]` entry.

- **Preparation** applies the gates with no I/O: `GET`, status `200`, an
  origin response that is not one reader's, and a host equal to `domain` with
  or without `www.`. API, feed, admin, well-known and asset paths are left out
  of the chain by `handles_path`.
- **Create** returns a stream processor that holds the document up to
  `max_document_bytes`, and signs in its asynchronous finish: policy digest,
  plan cache, sign call, cache writes, render. Every failure emits the held
  document unchanged.
- **Serve phase only**, so a failed signing is never stored as the shared
  template every reader receives.

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
- Module: the request gates, the cache, policy and sign flows and their
  failure codes (ported), configuration bounds, the missing-entry refusal, the
  path exclusions, and the publisher path signing an article through both the
  buffered and the streaming finalizer, against a stub origin and a stub API,
  while sending the address without its query, and never sending a page the
  origin marked private.
- Adapters: the Fastly store opens when linked; Axum KV persists and the
  timer fires while other work is pending.

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

The API address must be `https`, and the dev server's HTTP client trusts the
public certificate authorities built into it, so the runtime smoke cannot
reach a local signing stub. Signing a page is proven by the in-process
publisher-path tests above, not against the live API.

## 5. Open questions

1. Spin's declared KV store is `default`, which also holds the settings. The
   module's keys are namespaced (`encypher:v3:`, `encypher:policy:v2:`), but a
   dedicated label would separate them.
2. Holding the page delays its first byte by the signing time on a cache miss.
   `max_hold_ms` bounds it where a timer exists; on a runtime without one,
   `timeout_ms` bounds each call.
3. The module skips pages the origin marks private, but cannot detect other
   personalized pages. A deployment places it on public article routes.
