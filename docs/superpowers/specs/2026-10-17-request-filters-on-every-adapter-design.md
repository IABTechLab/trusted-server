# Design Spec: Request filters and device detection run on every adapter

**Status:** Proposed, 2026-10-17. A pull request above `ts config migrate`, not yet raised, for Tech Lab review.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-14-51degrees-client-hints-design.md`, and the design documents of the stack in IABTechLab/trusted-server#1342
**Related PRs:** none yet
**Issues:** none
**Last updated:** 2026-10-17

> **Why this spec exists.** Only the Fastly adapter runs the integration request filters and the device module `[device]` selects. On Axum, Cloudflare and Spin a module's request filter never runs, so the 51Degrees client hints middleware writes nothing and DataDome's protection never asks its service, and no module call carries a device classification. This change runs both on those three adapters where the Fastly adapter runs them.

## 1. Problem

The Fastly adapter runs three steps that no other adapter runs.

1. **Request filters.** `run_pre_route_filters` in `crates/trusted-server-adapter-fastly/src/app.rs` calls `IntegrationRegistry::filter_request` for every named route except the inspection endpoints and batch sync, and for the publisher fallback. It runs after the modules' preparers and once the request's Edge Cookie context is read, and it hands every filter the location and the permissions resolved for that context. A filter that answers stands in for the route. A filter that lets the request continue has its request headers applied before the route, and its response headers are applied by the entry point after the response is finalized (`apply_terminal_response_effects` in `src/lib.rs`).
2. **Device detection.** `derive_device_signals` in `src/lib.rs` calls `build_device_module` and `DeviceModule::detect` at the entry point, and `build_ec_request_state` sets the result on the Edge Cookie context. Every module call made from that context carries the classification (`ModuleContext::with_request_state` in core).
3. **robots.txt first.** `dispatch_fallback` answers robots.txt once the preparers have run and before the Edge Cookie steps and the filters, with an empty Edge Cookie context, so no filter's answer stands in for the file and none of the crawler's cookies are read.

No code in `crates/trusted-server-adapter-axum`, `-cloudflare` or `-spin` calls `filter_request` or `DeviceModule::detect`. So on those adapters the 51Degrees client hints middleware asks its service nothing and writes no header or tag, DataDome's protection filter never runs, `[device] module` changes nothing, and robots.txt is answered through the publisher path with the crawler's own Edge Cookie context. On Spin the discovery, signature verification and first-party routes and the page-bids preflight also skip the modules' preparers, which every other adapter runs for them.

## 2. Design

**One request cycle per adapter, in the order the Fastly adapter runs it.** Each of the three adapters gains the steps the Fastly adapter has, under the Fastly adapter's names, in `src/app.rs`.

| Step                  | Function                                   | What it does                                                                                                                                                        |
| --------------------- | ------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Preparers             | `execute_named`, `execute_fallback`        | `IntegrationRegistry::prepare_request`, once for each request, as before                                                                                            |
| robots.txt            | `asks_for_robots_txt`, `answer_robots_txt` | Answers a `GET` or `HEAD` of `/robots.txt`, when `[robots-txt]` is present, through the publisher path with `EcContext::default()` and no auction                   |
| Device classification | `derive_device_signals`                    | Runs the selected device module, or the built-in one, with the request's module context                                                                             |
| Edge Cookie context   | `build_ec_context`                         | `EcContext::read_from_request_resolving_geo`, then `set_device_signals`. An error answers the request                                                               |
| Request filters       | `run_pre_route_filters`                    | `IntegrationRegistry::filter_request` with the context's location and permissions. An answer, or an error, stands in for the route                                  |
| Route                 | `run_named_route`, `dispatch_fallback`     | The handler, given the context the filters saw                                                                                                                      |
| Response effects      | `attach_request_filter_effects`            | Leaves the filters' response headers on the response for the finalize middleware                                                                                    |
| Finalize              | `FinalizeResponseMiddleware`               | Finalizes the response, then `apply_terminal_response_effects` writes the filters' response headers and restores the privacy guards, as the Fastly entry point does |

**Which routes run the cycle.** The routes the Fastly adapter runs it for, which are every named route but the inspection endpoints, and the publisher fallback, being robots.txt, tsjs, the module routes and the publisher's origin. The inspection endpoints are read only and stay ahead of the Edge Cookie context and the filters, as on Fastly.

| Adapter    | Named routes in the cycle                                                                                                     | Inspection endpoints                             | Registration                                                                                                                                |
| ---------- | ----------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------- |
| Axum       | Every row of `named_routes()` but `/_ts/permissions`, `/_ts/permissions.json`, `/_ts/config`, `/_ts/config.json`, `/_ts/data` | `execute_named` answers them after the preparers | Unchanged. `named_route_handler` calls `execute_named` and `fallback_handler` calls `execute_fallback`, where both called `execute_handler` |
| Cloudflare | Discovery, `/verify-signature`, `/auction`, both page-bids paths (`GET` and `OPTIONS`), the four first-party paths            | `make_handler`, after the preparers, as before   | The same paths and methods, each handled by `named_route_handler`                                                                           |
| Spin       | The same routes as Cloudflare                                                                                                 | Their own handlers, with no preparers, as before | The same paths and methods, each handled by `named_route_handler`                                                                           |

On Spin the discovery, signature verification and first-party routes and the page-bids preflight now run the preparers, because the cycle starts with them. Before this change they ran none.

**Device classification.** `derive_device_signals` follows the Fastly adapter's function of the same name. A device module a registration supplies is shown the `User-Agent` and the `Cookie` header, and the built-in module the `User-Agent` alone, each in a module context carrying the request, that evidence, the settings and the services. Three things differ from the Fastly adapter.

1. It runs where the adapter builds the Edge Cookie context, after the preparers, where the Fastly adapter runs it at the entry point before routing. A preparer that removes a reserved cookie therefore removes it before a device module sees it.
2. The registered module is the one the request's services carry (`RuntimeServices::device_module`), which `services_for_request` sets from the registry, and the module is handed those same services, so it reads the platform's key-value store when the services carry one. The Fastly entry point builds services of its own for the call.
3. These adapters supply no TLS or HTTP/2 signals, so `[device] module = "platform"` builds the built-in module, which classifies from the `User-Agent`, because `platform` adds the host's signals only where the platform supplies them.

**Response effects.** A filter that lets the request continue can name response headers. The route's response carries them in its extensions as `RequestFilterEffects`, and `FinalizeResponseMiddleware` applies them after it writes the finalize headers, so a filter's header is written after an operator's `[response_headers]`, as on Fastly. `apply_terminal_response_effects` then enforces `no-store, private` on a response core marked `TerminalPrivateResponse` and runs `enforce_set_cookie_cache_privacy` and `enforce_uncacheable_cache_privacy`, the steps the Fastly entry point runs before it sends every response.

**Configuration.** None. The filters a deployment's modules register and the module `[device]` selects now run on every adapter.

**Consent and privacy.** Every filter is handed the permissions resolved for the request, as on Fastly. The 51Degrees filter sends the reader's `User-Agent` and `Sec-CH-UA` hints to the 51Degrees service for each page navigation an entry naming its middleware covers, whatever the permissions say, which is the behavior and the open question that `2026-10-14-51degrees-client-hints-design.md` describes for Fastly. DataDome's protection filter removes `x-ts-datadome-bypass` from every request it runs for, before its other checks. A device module a registration supplies sees the reader's cookies, as on Fastly. robots.txt is answered with an empty Edge Cookie context, so a crawler's cookies and consent signals are no longer read on these adapters.

**Failure handling.** A filter that fails answers the request with the error, logged as `Failed to run integration request filters`, as on Fastly. An Edge Cookie context that cannot be read answers the request with the error before the filters run, which now covers every route in the cycle, including tsjs and the discovery, signature verification and first-party routes. The errors are a selected Edge Cookie module the request cannot build and a `Cookie` header that is not UTF-8, and startup refuses a selection the adapter can never build. A filter's outbound call is bounded only by the adapter's HTTP client, which applies no module's own timeout on these adapters. Axum's client allows 5 seconds to connect and 30 in all, Cloudflare leaves the call to the Workers runtime's limit, and Spin to the Spin runtime's default (`src/platform.rs` of each adapter).

**Rollout.** A deployment whose modules register no filter, and that selects no device module, answers as before. It now reads the Edge Cookie context, with its geo lookup where `[geo]` selects a module, and classifies the device with the built-in module, on every route in the cycle, where it read the context for the auction, page-bids, module routes and the publisher alone. A deployment with `[shared.51degrees]` and a `[[serve]]` entry naming `51degrees.client_hints` now has the headers and the tag written on the pages the entry covers, and such a page keeps to the origin path and is sent `private, no-store`, because the filter leaves the hint names in the request's state. A deployment with DataDome's protection on now has its pages checked, and a challenge answers in place of the page.

**Tests.** In `src/app/request_cycle_tests.rs` of each of the three adapters, through the adapter's router built by its composition root, with a stub origin:

- `a_filter_that_answers_stands_in_for_the_page`: the answer and its effect header are the response, and the origin is not asked.
- `a_filter_s_request_header_reaches_the_route_and_its_effect_is_written_last`: a module route sees the request header a filter added, and the filter's response header replaces the operator's header of the same name.
- `the_device_module_s_classification_reaches_the_module_call`: the module `[device]` selects classifies the request, shown its cookies, and a module route's call carries that classification.
- `robots_txt_is_answered_before_the_request_filters`: with a filter that answers every request, robots.txt is still the file, and the filter's header is absent.

**Open questions.** None.

## 3. What is not in this change

- Creating and writing the Edge Cookie, and the resolve route, on these adapters, which the next pull request of the stack covers.
- Asset routes on these adapters, which the pull request after that covers.
- A timeout for a filter's outbound call on Axum, Cloudflare and Spin, which their HTTP clients do not offer.
- Moving the inspection endpoints ahead of the preparers on Axum and Cloudflare, where the Fastly adapter answers two of them before the preparers.

## 4. Acceptance

The four tests pass on each of the three adapters, and the existing tests of each adapter and the cross-adapter parity tests pass unchanged.

## Revision record

| Date       | Change       |
| ---------- | ------------ |
| 2026-10-17 | First draft. |
