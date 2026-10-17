# Design Spec: The Edge Cookie is created and written on every adapter

**Status:** Proposed, 2026-10-17. A pull request above the one that runs the request filters on every adapter, not yet raised, for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-17-request-filters-on-every-adapter-design.md`, `2026-10-15-cookies-by-path-design.md`, and the design documents of the stack in IABTechLab/trusted-server#1342
**Related PRs:** none yet
**Issues:** none
**Last updated:** 2026-10-17

> **Why this spec exists.** Only the Fastly adapter creates and writes the Edge Cookie and serves the resolve route. On Axum, Cloudflare and Spin an Edge Cookie module reads the cookie a request carries, but no page view creates one and no response writes one, so the stack's cookies-by-path change is never reached there. This change runs the Fastly adapter's Edge Cookie steps on those three adapters, which open no identity graph, so the cookie is created and written there without a graph row and the graph's syncs are skipped.

## 1. Problem

The Fastly adapter runs the Edge Cookie lifecycle in four places (`crates/trusted-server-adapter-fastly/src/app.rs` and `src/lib.rs`).

1. `build_ec_request_state` reads the request's Edge Cookie context with the device classification, takes the bot gate's answer from the classification (`DeviceSignals::looks_like_browser`), and keeps the identity graph, when `[ec] ec_store` names one, for a request that passes the gate.
2. `dispatch_fallback` creates an Edge Cookie for a real browser's page navigation (`EcContext::generate_if_needed`) before the publisher path runs. The integration proxy creates one for a navigation to a module route (`IntegrationRegistry::handle_proxy` in core).
3. Every response a route in the request cycle makes carries an `EcFinalizeState`, and the entry point runs `ec_finalize_response` on it once the response is finalized. That writes a created Edge Cookie, expires one on a withdrawal and reconciles the markers.
4. `POST /_ts/api/v1/ec/resolve` is a row of `NAMED_ROUTES`, answered by `handle_ec_resolve`.

None of the four exists on Axum, Cloudflare or Spin. Spin's `named_fallback_paths` says the resolve route is left out because the identity graph is not wired there.

Core writes a created Edge Cookie only with its identity-graph row. `ec_finalize_response` in `crates/trusted-server-core/src/ec/finalize.rs` skips the write when it is given no graph, so that on a deployment with a graph no browser carries an identifier the graph does not know. The three adapters open no graph, because the graph needs strongly consistent reads and conditional writes (`EcKvStore::key_exists` and `EcKvWriteMode::IfGenerationMatch` in `ec/kv_backend.rs`), which the platform key-value store these adapters can open does not offer (`get_bytes`, `put_bytes`, `put_bytes_with_ttl`, `delete` and `list_keys_page` in `edgezero_core::key_value_store`). Porting the Fastly steps alone would therefore create an Edge Cookie on these adapters and never write it.

## 2. Design

**The Edge Cookie request state.** Each adapter's `build_ec_request_state` reads the context as before, with the device classification, and keeps the bot gate's answer beside it in an `EcRequestState`. A request the gate stops is logged as on Fastly. No identity graph is kept, because none is opened.

**Creating the Edge Cookie.** `dispatch_fallback` calls `EcContext::generate_if_needed` for a real browser's page navigation (`is_navigation_request`) before the publisher path runs, with no graph, as the Fastly adapter does when it has none. The integration proxy creates one for a navigation as before, in core.

**Writing it.** Every response a route in the request cycle makes carries an `EcFinalizeState`, holding the context, the bot gate's answer and the request's services. These are the responses the Fastly adapter attaches it to, being the named routes but the inspection endpoints, the fallback's tsjs, module route and publisher answers, and a filter's answer. robots.txt carries none, as on Fastly. `FinalizeResponseMiddleware` takes the state off the response once it has written the finalize headers and runs `ec_finalize_response` with no graph, which applies a module's response headers, expires the cookie and its markers on a withdrawal, and reconciles the pull-sync marker. It then writes an Edge Cookie the request created, through core's `set_ec_cookie_on_response`, when the request passed the bot gate and its permissions allow the Edge Cookie (`EcContext::ec_allowed`). That write is the one core withholds without a graph, and the adapter makes it because it has no graph to withhold it for. The cookie takes the request's public prefix and, on a request addressed by path, the removed prefix as its `Path`, as core writes every cookie it owns. The request filters' response effects are applied after it, as on Fastly. The `sharedId` cookie the Fastly adapter carries to finalizing is read only by the graph's identifier writes, so it is not carried.

| Step                                 | Fastly                                            | Axum, Cloudflare and Spin                                     |
| ------------------------------------ | ------------------------------------------------- | ------------------------------------------------------------- |
| Bot gate                             | `build_ec_request_state`                          | `build_ec_request_state`                                      |
| Identity graph for the request       | The `[ec] ec_store` graph, for a real browser     | None                                                          |
| Creating on a page navigation        | `dispatch_fallback`, for a real browser           | `dispatch_fallback`, for a real browser                       |
| Finalizing                           | The entry point, after the finalize headers       | `FinalizeResponseMiddleware`, after the finalize headers      |
| Writing a created Edge Cookie        | `ec_finalize_response`, once its graph row exists | The middleware, for a real browser whose permissions allow it |
| Pull sync after the response is sent | When the graph and the partners allow it          | Skipped                                                       |
| `POST /_ts/api/v1/ec/resolve`        | A row of `NAMED_ROUTES`                           | A named route on each adapter, in the request cycle           |

**The resolve route.** Each adapter registers `POST /_ts/api/v1/ec/resolve` as a named route the way it registers its other named routes, so the request passes the preparers, the Edge Cookie context and the request filters, as on Fastly. It is answered by `handle_ec_resolve` with no graph, so its refusals are the Fastly route's, being `403` for a missing or foreign `Origin` and `415` for another content type among them, and a request it would answer `200` gets `204` with no cookie, because the route writes the graph row before the cookie and there is no graph.

**One startup warning.** Where `[ec] ec_store` names an identity graph store, each adapter logs once, as it builds its state, that it opens no identity graph, so an Edge Cookie is created with no graph row and the graph's syncs are skipped. Cloudflare and Spin build their state for every request, so a flag keeps the line to one for each instance.

**Configuration.** None. `[ec] ec_store` is accepted on these adapters as before, and logged once.

**Consent and privacy.** A created Edge Cookie is written only where core's permission gate allows the Edge Cookie module and the request passed the bot gate, which are the gates the Fastly adapter writes behind. A withdrawal expires the cookie and its markers, as on Fastly, and there is no graph row to tombstone. Without a graph no partner identifier is written against the Edge Cookie, and no pull sync, batch sync or identify call reads one, so the Edge Cookie stays a first-party identifier in the browser.

**Failure handling.** A failure to create the Edge Cookie is logged and the page is served without one, as on Fastly. A partner registry that cannot be built from `[ec] partners` skips the finalize step with a logged error, as on Fastly.

**Rollout.** A deployment that selects no Edge Cookie module is unchanged. One that selects a server-side module, such as `hmac`, now sets the Edge Cookie on a real browser's first page navigation where the permissions allow it, and that response is kept out of shared caches because it sets a cookie (`enforce_set_cookie_cache_privacy`). A returning browser keeps its cookie. This differs from a Fastly service with no `ec_store`, where core writes no created Edge Cookie because no graph row backs it. A client-side module, such as `client_fixed`, creates nothing on these adapters, because the resolve route writes the graph row first.

**Tests.** In `src/app/request_cycle_tests.rs` of each of the three adapters, through the adapter's router, with a stub origin and a reader whose services place them in the US, where the sample permission rules grant the Edge Cookie with no signal, and `[geo] module = "platform"`, which leaves that location standing:

- `a_first_page_view_gets_the_edge_cookie_under_the_public_prefix_and_path`: a page view addressed by path gets `ts-ec` with `Path=/publisher.example`, and the origin is asked at the continuing path.
- `a_returning_page_view_keeps_its_edge_cookie`: a page view addressed by host gets `ts-ec` under `/`, and the same page view carrying that cookie gets none.
- `a_bot_s_page_view_gets_no_edge_cookie`: a page view whose `User-Agent` names a crawler gets none.
- `the_resolve_route_answers`: a foreign `Origin` gets `403` with `Cache-Control: no-store` and the publisher's gets `204` with no cookie, the answer of a server-side module, which creates nothing from a posted value, and neither reaches the origin.
- `startup_warns_that_this_adapter_opens_no_identity_graph`: building the state with `[ec] ec_store` set logs the warning.

**Open questions.**

1. Whether the resolve route should write a client-side module's cookie on an adapter that opens no graph, as a page navigation now does there. Core's route writes the graph row first, so today a client-side module creates no Edge Cookie on these adapters.
2. Whether a Fastly service with no `ec_store` should write a created Edge Cookie, as these adapters now do. Core skips the write there.

## 3. What is not in this change

- An identity graph on Axum, Cloudflare or Spin, which needs a strongly consistent store with conditional writes.
- `/_ts/api/v1/identify`, `/_ts/api/v1/batch-sync` and pull sync on those adapters, which need the graph, and the tester routes, which no other adapter registers.
- Asset routes on these adapters, which the next pull request of the stack covers.

## 4. Acceptance

The five tests pass on each of the three adapters, and the existing tests of each adapter and the cross-adapter parity tests pass.

## Revision record

| Date       | Change       |
| ---------- | ------------ |
| 2026-10-17 | First draft. |
