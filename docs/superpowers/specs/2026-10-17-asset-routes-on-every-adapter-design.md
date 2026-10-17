# Design Spec: Asset routes are served on every adapter

**Status:** Proposed, 2026-10-17. A pull request above the one that creates and writes the Edge Cookie on every adapter, not yet raised, for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-17-request-filters-on-every-adapter-design.md`, `2026-10-17-edge-cookie-on-every-adapter-design.md`, `2026-10-14-source-modules-design.md`, `2026-10-14-origin-signing-design.md`, `2026-10-15-asset-cache-design.md`, and the design documents of the stack in IABTechLab/trusted-server#1342
**Related PRs:** none yet
**Issues:** none
**Last updated:** 2026-10-17

> **Why this spec exists.** Only the Fastly adapter serves `[[proxy.asset_routes]]`. On Axum, Cloudflare and Spin a request under a route's prefix goes to the publisher's origin as an ordinary page request, so a route's own source, its signing and its held answers are never used there. This change dispatches asset routes on those three adapters as the Fastly adapter does.

## 1. Problem

`dispatch_fallback` in `crates/trusted-server-adapter-fastly/src/app.rs` checks a `GET` or `HEAD` against `Settings::asset_route_index_for_path` once robots.txt, the Edge Cookie context, the request filters, tsjs and the module routes have had their turn, and before the publisher's origin. `dispatch_asset_fallback` then asks the registry for the route's source (`IntegrationRegistry::route_source`) and answers with `proxy::handle_asset_proxy_request`, which locates the asset through the source module, signs the request where the source's `auth` table names a signer, looks for the answer among the copies its caches hold before it asks the origin, and keeps the origin's answer the way the publisher path keeps an asset. The response carries its `AssetProxyCachePolicy`, which the entry point applies after the finalize headers, and the request filters' effects, and no Edge Cookie state, because an asset response skips the Edge Cookie steps.

No code in the Axum, Cloudflare or Spin adapter calls `handle_asset_proxy_request`. `docs/guide/api-reference.md` lists the route as Fastly's alone, and the Cloudflare HTTP client's note says these requests are not routed to it.

## 2. Design

**Dispatch.** Each adapter's `dispatch_fallback` checks a `GET` or `HEAD` against the asset routes where the Fastly adapter does, and `dispatch_asset_fallback` follows the Fastly function of that name: the route's source, `handle_asset_proxy_request`, the response's cache policy in its extensions, and the request filters' effects. An error answers with the error and the `no-store, private` policy, as on Fastly. No Edge Cookie state is attached, so the finalize middleware writes no Edge Cookie on an asset response.

**Finalizing.** `FinalizeResponseMiddleware` takes the cache policy off the response and applies it after the finalize headers with the adapter's own shared cache header (`AssetProxyCachePolicy::apply_after_route_finalization`), before the Edge Cookie and the filters' effects, the order the Fastly entry point uses.

**Held answers and the body.** `handle_asset_proxy_request` keeps and reads its answers through the request's loading caches and value cache, as on Fastly. Cloudflare and Spin rebuild their state for every request and have no value cache, so an answer is held across requests only by a loading cache module that keeps its copies in the platform's key-value store, when the services carry one. None of the three HTTP clients streams a response (`supports_streaming_responses`), so core asks for none and the asset body is held whole, where the Fastly adapter streams it.

**Image Optimizer.** A route that enables `[proxy.asset_routes.image_optimizer]` sets Image Optimizer metadata on the request, which the Fastly adapter translates and the other three cannot. The Cloudflare and Spin HTTP clients already refuse such a request, as the `PlatformHttpRequest` contract asks of a client that cannot honor it, and the Axum client now refuses it too where it would have sent it unchanged. The request then answers with the error.

| Adapter    | Source and signing | Held answers                                           | Body       | Image Optimizer                 |
| ---------- | ------------------ | ------------------------------------------------------ | ---------- | ------------------------------- |
| Fastly     | Yes                | Loading caches and the value cache                     | Streamed   | Translated                      |
| Axum       | Yes                | The loading caches of the process                      | Held whole | Refused by the client, an error |
| Cloudflare | Yes                | A loading cache module's copies in the key-value store | Held whole | Refused by the client, an error |
| Spin       | Yes                | A loading cache module's copies in the key-value store | Held whole | Refused by the client, an error |

**Configuration.** None. `[[proxy.asset_routes]]` now takes effect on every adapter.

**Consent and privacy.** An asset route forwards only `ASSET_PROXY_FORWARD_HEADERS` to the route's origin, as on Fastly, so the reader's cookies are not sent there. Before this change the request went to the publisher's origin with the headers the publisher path forwards.

**Failure handling.** A route whose source cannot be built, an origin that cannot be reached and a request the client refuses each answer with the error and `no-store, private`, as on Fastly.

**Rollout.** A deployment with no asset routes is unchanged. One with asset routes now has their requests answered by each route's origin on Axum, Cloudflare and Spin, where the publisher's origin answered them, and a route that enables the Image Optimizer answers with an error there.

**Tests.** In `src/app/request_cycle_tests.rs` of each of the three adapters, with a stub origin and an asset route whose source signs with the stock `s3_sigv4` signer:

- `a_request_under_an_asset_route_reaches_the_route_s_origin_signed`: the route's origin is asked, at the route's address, with an `AWS4-HMAC-SHA256` `Authorization` header, and its answer is the response.
- `a_request_outside_the_asset_routes_reaches_the_publisher_s_origin`: the publisher's origin is asked, unsigned.

The Axum adapter's platform tests gain `a_request_with_image_optimizer_metadata_is_refused`.

**Open questions.** None.

## 3. What is not in this change

- An Image Optimizer on Axum, Cloudflare or Spin.
- Streaming an asset body on those adapters, whose HTTP clients hold every response whole.

## 4. Acceptance

The tests pass on each of the three adapters, and the existing tests of each adapter and the cross-adapter parity tests pass.

## Revision record

| Date       | Change       |
| ---------- | ------------ |
| 2026-10-17 | First draft. |
