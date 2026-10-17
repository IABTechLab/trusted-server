# Design Spec: The Edge Cookie is created and written on every adapter

**Status:** Proposed, 2026-10-17. A pull request above the one that runs the request filters on every adapter, not yet raised, for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-17-request-filters-on-every-adapter-design.md`, `2026-10-15-cookies-by-path-design.md`, and the design documents of the stack in IABTechLab/trusted-server#1342
**Related PRs:** none yet
**Issues:** none
**Last updated:** 2026-10-18

> **Why this spec exists.** Only the Fastly adapter creates and writes the Edge Cookie and serves the resolve route. On Axum, Cloudflare and Spin an Edge Cookie module reads the cookie a request carries, but no page view creates one and no response writes one, so the stack's cookies-by-path change is never reached there. This change runs the Fastly adapter's Edge Cookie steps on those three adapters, which open no identity graph, and core writes a created Edge Cookie with or without a graph, so the cookie is written on every host, the graph is optional, and without one the graph's syncs are skipped.

## 1. Problem

The Fastly adapter runs the Edge Cookie lifecycle in four places (`crates/trusted-server-adapter-fastly/src/app.rs` and `src/lib.rs`).

1. `build_ec_request_state` reads the request's Edge Cookie context with the device classification, takes the bot gate's answer from the classification (`DeviceSignals::looks_like_browser`), and keeps the identity graph, when `[ec] ec_store` names one, for a request that passes the gate.
2. `dispatch_fallback` creates an Edge Cookie for a real browser's page navigation (`EcContext::generate_if_needed`) before the publisher path runs. The integration proxy creates one for a navigation to a module route (`IntegrationRegistry::handle_proxy` in core).
3. Every response a route in the request cycle makes carries an `EcFinalizeState`, and the entry point runs `ec_finalize_response` on it once the response is finalized. That writes a created Edge Cookie, expires one on a withdrawal and reconciles the markers.
4. `POST /_ts/api/v1/ec/resolve` is a row of `NAMED_ROUTES`, answered by `handle_ec_resolve`.

None of the four exists on Axum, Cloudflare or Spin. Spin's `named_fallback_paths` says the resolve route is left out because the identity graph is not wired there.

Core writes a created Edge Cookie only with its identity-graph row. `ec_finalize_response` in `crates/trusted-server-core/src/ec/finalize.rs` skips the write when it is given no graph, so that on a deployment with a graph no browser carries an identifier the graph does not know. The three adapters open no graph, because the graph needs strongly consistent reads and conditional writes (`EcKvStore::key_exists` and `EcKvWriteMode::IfGenerationMatch` in `ec/kv_backend.rs`), which the platform key-value store these adapters can open does not offer (`get_bytes`, `put_bytes`, `put_bytes_with_ttl`, `delete` and `list_keys_page` in `edgezero_core::key_value_store`). Porting the Fastly steps alone would therefore create an Edge Cookie on these adapters and never write it. A Fastly service whose `[ec] ec_store` names no store writes none either, and the resolve route answers `204` with no cookie wherever it has no graph.

## 2. Design

**The Edge Cookie request state.** Each adapter's `build_ec_request_state` reads the context as before, with the device classification, and keeps the bot gate's answer beside it in an `EcRequestState`. A request the gate stops is logged as on Fastly. No identity graph is kept, because none is opened.

**Creating the Edge Cookie.** `dispatch_fallback` calls `EcContext::generate_if_needed` for a real browser's page navigation (`is_navigation_request`) before the publisher path runs, with no graph, as the Fastly adapter does when it has none. The integration proxy creates one for a navigation as before, in core.

**Writing it.** Every response a route in the request cycle makes carries an `EcFinalizeState`, holding the context and the request's services. These are the responses the Fastly adapter attaches it to, being the named routes but the inspection endpoints, the fallback's tsjs, module route and publisher answers, and a filter's answer. robots.txt carries none, as on Fastly. `FinalizeResponseMiddleware` takes the state off the response once it has written the finalize headers and runs `ec_finalize_response` with no graph, which applies a module's response headers, writes an Edge Cookie the request created, expires the cookie and its markers on a withdrawal, and reconciles the pull-sync marker. The request filters' response effects are applied after it, as on Fastly. The `sharedId` cookie the Fastly adapter carries to finalizing is read only by the graph's identifier writes, so it is not carried.

**Core writes the cookie on every host.** `ec_finalize_response` writes a created Edge Cookie for a request that passed the bot gate and whose permissions allow the Edge Cookie (`EcContext::ec_allowed`), with or without an identity graph. With one, it records the request's identifiers on the cookie's row and writes the cookie once that row is authoritative, as before. Without one, it skips the graph steps and writes the cookie through `set_ec_cookie_on_response`, which takes the request's public prefix and, on a request addressed by path, the removed prefix as its `Path`, as core writes every cookie it owns. Core reads the bot gate from the device signals the adapter sets on the context (`EcContext::passed_bot_gate`, which a request with no device signals does not pass), because a missing graph no longer stops the cookie and so can no longer stand in for the gate. The Fastly adapter still opens no graph for a request the gate stops, so no graph row is written for it.

| Step                                 | Fastly                                                                                            | Axum, Cloudflare and Spin                                |
| ------------------------------------ | ------------------------------------------------------------------------------------------------- | -------------------------------------------------------- |
| Bot gate                             | `build_ec_request_state`                                                                          | `build_ec_request_state`                                 |
| Identity graph for the request       | The `[ec] ec_store` graph, for a real browser                                                     | None                                                     |
| Creating on a page navigation        | `dispatch_fallback`, for a real browser                                                           | `dispatch_fallback`, for a real browser                  |
| Finalizing                           | The entry point, after the finalize headers                                                       | `FinalizeResponseMiddleware`, after the finalize headers |
| Writing a created Edge Cookie        | `ec_finalize_response`, once its graph row exists, or with no row where `ec_store` names no store | `ec_finalize_response`, with no row                      |
| Pull sync after the response is sent | When the graph and the partners allow it                                                          | Skipped                                                  |
| `POST /_ts/api/v1/ec/resolve`        | A row of `NAMED_ROUTES`                                                                           | A named route on each adapter, in the request cycle      |

**The resolve route.** Each adapter registers `POST /_ts/api/v1/ec/resolve` as a named route the way it registers its other named routes, so the request passes the preparers, the Edge Cookie context and the request filters, as on Fastly. It is answered by `handle_ec_resolve` with no graph, which answers as it does with one, so its refusals are the Fastly route's, being `403` for a missing or foreign `Origin` and `415` for another content type among them, and a client-side module's created Edge Cookie is set with its marker and no graph row. A request that did not pass the bot gate gets `204` with no cookie, as on Fastly.

**One startup warning.** Where `[ec] ec_store` names an identity graph store, each adapter logs once, as it builds its state, that it opens no identity graph, so the Edge Cookie is still written but the data the graph would keep with it, such as partner IDs, is not. Cloudflare and Spin build their state for every request, so a flag keeps the line to one for each instance.

**Configuration.** None. `[ec] ec_store` is accepted on these adapters as before, and logged once.

**Consent and privacy.** A created Edge Cookie is written only where core's permission gate allows the Edge Cookie module and the request passed the bot gate, on every host, with a graph or without. A withdrawal expires the cookie and its markers, as on Fastly, and there is no graph row to tombstone. Without a graph no partner identifier is written against the Edge Cookie, and no pull sync, batch sync or identify call reads one, so the Edge Cookie stays a first-party identifier in the browser.

**Failure handling.** A failure to create the Edge Cookie is logged and the page is served without one, as on Fastly. A partner registry that cannot be built from `[ec] partners` skips the finalize step with a logged error, as on Fastly.

**Rollout.** A deployment that selects no Edge Cookie module is unchanged. One that selects a server-side module, such as `hmac`, now sets the Edge Cookie on a real browser's first page navigation where the permissions allow it, on these adapters and on a Fastly service with no `ec_store`, and that response is kept out of shared caches because it sets a cookie (`enforce_set_cookie_cache_privacy`). A returning browser keeps its cookie. A client-side module, such as `client_fixed`, gets its cookie from the resolve route on those hosts too. A crate outside core that finalizes the Edge Cookie or answers the resolve route sets the device signals on the context, or no created cookie is written.

**Tests.** In `src/app/request_cycle_tests.rs` of each of the three adapters, through the adapter's router, with a stub origin and a reader whose services place them in the US, where the sample permission rules grant the Edge Cookie with no signal, and `[geo] module = "platform"`, which leaves that location standing:

- `a_first_page_view_gets_the_edge_cookie_under_the_public_prefix_and_path`: a page view addressed by path gets `ts-ec` with `Path=/publisher.example`, and the origin is asked at the continuing path.
- `a_returning_page_view_keeps_its_edge_cookie`: a page view addressed by host gets `ts-ec` under `/`, and the same page view carrying that cookie gets none.
- `a_bot_s_page_view_gets_no_edge_cookie`: a page view whose `User-Agent` names a crawler gets none.
- `the_resolve_route_answers`: a foreign `Origin` gets `403` with `Cache-Control: no-store` and the publisher's gets `204` with no cookie, the answer of a server-side module, which creates nothing from a posted value, and neither reaches the origin.
- `startup_warns_that_this_adapter_opens_no_identity_graph`: building the state with `[ec] ec_store` set logs the warning.

The tests' `edge_cookie` helper also fails a response that writes the Edge Cookie more than once, so the first page view shows core alone writes it.

In core:

- `finalize_generated_ec_without_kv_writes_the_cookie_and_skips_the_graph` in `ec/finalize.rs`: a created Edge Cookie finalized with no graph gets the headers the same cookie gets with one, and no graph step runs, while with a graph the request's partner ID is recorded on the row.
- `a_generated_ec_is_written_only_for_a_request_that_passed_the_bot_gate` in `ec/finalize.rs`: a browser's created Edge Cookie is written, and a crawler's or that of a request with no device signals is not, with a graph and without.
- `resolve_answers_as_it_does_with_a_graph_when_there_is_none` in `ec/resolve.rs`: with no graph the route answers `200` with the Edge Cookie, its marker and `Cache-Control: no-store`, the headers it gives with a graph.
- `resolve_creates_nothing_for_a_request_that_did_not_pass_the_bot_gate` in `ec/resolve.rs`: a crawler, or a request with no device signals, gets `204` with no cookie and no row, with a graph and without.
- `the_bot_gate_passes_a_browser_and_stops_a_crawler_or_a_request_with_no_signals` in `ec/mod.rs`.

In the Fastly adapter, under Viceroy, `with_no_ec_store_the_entry_point_writes_a_created_edge_cookie` in `src/app.rs`: with no `[ec] ec_store`, a browser's page view builds no graph, creates an Edge Cookie, and the entry point's finalize step writes it once.

**Decisions.** Two questions this design left open, whether the resolve route should write a client-side module's cookie on an adapter that opens no graph and whether a Fastly service with no `ec_store` should write a created Edge Cookie, are both answered yes. The Edge Cookie is the identifier, and the identity graph is one store of the data associated with it. The permission state comes from the permission model, whose sources may be values the graph holds, signals the client sends or any other source, so a missing graph does not stop the cookie.

So the Edge Cookie is the identifier, the identity graph is one optional store of the data associated with it, the partner IDs among it, and the permission state comes from the permission model whatever its sources. A missing graph does not stop the cookie.

## 3. What is not in this change

- An identity graph on Axum, Cloudflare or Spin, which needs a strongly consistent store with conditional writes.
- `/_ts/api/v1/identify`, `/_ts/api/v1/batch-sync` and pull sync on those adapters, which need the graph, and the tester routes, which no other adapter registers.
- Asset routes on these adapters, which the next pull request of the stack covers.

## 4. Acceptance

The five tests pass on each of the three adapters, the core and Fastly tests above pass, and the existing tests of each adapter, core's Edge Cookie tests and the cross-adapter parity tests pass.

## Revision record

| Date       | Change                                                                                                                                                                                                                           |
| ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-17 | First draft.                                                                                                                                                                                                                     |
| 2026-10-18 | Core writes a created Edge Cookie with or without an identity graph, behind the bot gate it reads from the device signals, and the resolve route answers without a graph as it does with one, which answers both open questions. |
