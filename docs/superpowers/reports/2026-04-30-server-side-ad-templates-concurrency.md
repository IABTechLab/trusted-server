# Server-Side Ad Templates Concurrency Feasibility

## Selected Primitive

Go with a Fastly-only shape:

- Fastly `Request::send_async()` starts origin and bidder/provider requests without waiting for their responses.
- Fastly `PendingRequest::poll()` advances bidder/provider requests without blocking page streaming.
- Fastly `Response::stream_to_client()` commits browser response headers as soon as origin headers are ready.
- Fastly Core Cache stores the request-ID rendezvous state for `/ts-bids`; process-global memory is not used for production bid delivery.

This means the implementation should not call the existing blocking `AuctionOrchestrator::run_auction()` on the publisher streaming path. It needs a pollable auction path that starts provider requests, polls them between streaming writes, and writes completed or empty bid results to the Core Cache-backed `BidCache`.

## Evidence

- `Cargo.lock` uses `fastly = 0.11.12`.
- Fastly SDK confirms normal request isolation: `fastly-0.11.12/src/experimental/reusable_sessions.rs:3` says each incoming HTTP request normally starts a new Compute instance. Therefore a process-global `static`/`LazyLock` `BidCache` is not a valid production rendezvous for a page request and a later `/ts-bids` request.
- Fastly SDK exposes non-blocking pending request polling: `fastly-0.11.12/src/http/request/pending.rs:39` through `:43` documents `PendingRequest::poll()` as immediately returning a `PollResult`.
- Fastly SDK preserves pending handles when not ready: `fastly-0.11.12/src/http/request/pending.rs:50` through `:52` returns a new pending request handle when a backend response is not ready.
- Fastly SDK exposes blocking `select()`, but it is not acceptable on the publisher streaming critical path: `fastly-0.11.12/src/http/request/pending.rs:94` through `:96` documents that `select` blocks until one request is ready.
- Fastly SDK `Request::send_async()` starts backend work without waiting: `fastly-0.11.12/src/http/request.rs:709` through `:719`.
- Fastly SDK `Response::stream_to_client()` commits response headers immediately: `fastly-0.11.12/src/http/response.rs:1734` through `:1745`.
- Fastly SDK `StreamingBody` writes are buffered and can be flushed to emit chunks: `fastly-0.11.12/src/http/body/streaming.rs:20` through `:22`, and `:123` through `:125`.
- Fastly Core Cache supports one-off `lookup()`/`insert()` for arbitrary cached objects: `fastly-0.11.12/src/cache/core.rs:30` through `:35`, `:143`, and `:611`.
- Fastly Core Cache replacement keeps an existing object accessible while the replacement is being written by default: `fastly-0.11.12/src/cache/core/replace.rs:15` through `:28`.

## Required Publisher-Path Shape

- Compute `A_deadline = T0 + auction_timeout_ms` once at the start of a matched, consent-allowed page request.
- Insert a pending `BidCacheEntry` into Fastly Core Cache before emitting `window.__ts_request_id`; the entry must include the original deadline as epoch milliseconds.
- Dispatch origin and provider requests immediately with `send_async()`.
- Start streaming HTML as soon as origin response headers are available.
- During streaming, only advance auction state through non-blocking `PendingRequest::poll()` calls between chunk writes.
- When all provider requests finish or the original deadline expires, replace the Core Cache entry with a completed bid map. Auction errors must replace it with an empty bid map.
- `/ts-bids` must read the Core Cache entry. Pending entries long-poll only until the persisted original deadline; completed empty maps return `{}`; unknown/expired IDs return `404`.
- Non-Fastly adapters remain unsupported/deferred for this feature until EdgeZero migration provides equivalent primitives.

## Stop/Go Decision

Go, with the plan corrected to use Fastly Core Cache for request-ID rendezvous and `PendingRequest::poll()` for non-blocking auction progress.

Do not implement a process-global production `BidCache`. Do not implement `/ts-bids`-initiated auctions. Do not call blocking `select()` or `run_auction()` before the page has started streaming.
