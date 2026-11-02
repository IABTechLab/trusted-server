# Design Spec: One module context

**Status:** Proposed, 2026-10-12. Implemented as the pull request [IABTechLab/trusted-server#1349](https://github.com/IABTechLab/trusted-server/pull/1349), answering [IABTechLab/trusted-server#1267](https://github.com/IABTechLab/trusted-server/issues/1267). [IABTechLab/trusted-server#1342](https://github.com/IABTechLab/trusted-server/pull/1342) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** IABTechLab/trusted-server#1349, which builds on the integration seam in [IABTechLab/trusted-server#1348](https://github.com/IABTechLab/trusted-server/pull/1348) and was reviewed on [jwrosewell/trusted-server#3](https://github.com/jwrosewell/trusted-server/pull/3) (closed)
**Issues:** IABTechLab/trusted-server#1267
**Last updated:** 2026-10-16

> **Why this spec exists.** Each kind of module is called with its own parameter list.

## 1. Problem

Each kind of module is called with its own parameter list. A device module is handed `request_info` and `services`, an Edge Cookie module `request_info`, an `IdentityInput` and `services`, a permission signal module the consent and the evidence, and a route the settings and the services. Handing any of them one more value, such as the permissions resolved for the request, means changing that trait and every implementation of it, in this repository and in every vendor's crate.

Nothing in those lists says which values a module may use, so nothing can hold back a value whose use needs a permission from a module that has not declared that permission. The location, the device signals and the Edge Cookie identifier are such values, because the module that produces each of them can declare a permission for its use.

## 2. Design

**What a module is handed.** Core builds one context where it calls modules, from what the request already holds. That is the request's Edge Cookie state (the permissions, the consent, the location, the device signals and the identifier that core resolves for it), its services and its settings. Core hands each module a `ModuleCall`, which is that context together with the permissions the module declares. A module hands a function of its own to `call.inject`, and that function names what it needs as parameters. A route, and an Edge Cookie module resolving a value that a page posted, also act on one argument of their own, the request or the payload. They hand their function to `call.inject_with` together with that argument, and the function takes the argument straight after the module. The example is a route:

```rust
async fn route(
    &self,
    req: Request<EdgeBody>,
    settings: &Settings,
    geo: Option<&GeoInfo>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> { ... }

async fn handle(
    &self,
    call: ModuleCall<'_>,
    req: Request<EdgeBody>,
) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
    call.inject_with(self, req, Self::route)?.await
}
```

A parameter is what the request resolved to (method, path, query, host and scheme), the reader's evidence, the connection, the resolved permissions, the decoded consent, the settings, the services, the location, the device signals or the Edge Cookie identifier. It may be an `Option` of one, which is `None` when the value cannot be passed, or a tuple of several. A function that names a value itself, without `Option`, is not called when the value cannot be passed. Both calls return the reason as a `Withheld` error and write it to the debug log, and the module decides what a skipped call means.

**Which values are gated.** A gated value is one whose use needs a permission. Three values are gated, being the location, the device signals and the Edge Cookie identifier. Each carries the permissions that the module which produced it declares, and it is passed only to a module that declares every one of those permissions and is granted them on this request. A producer that declares no permission gates nothing, which is the default, so its value reaches every module. Of the modules in this repository, only the HMAC, host signal and `client_fixed` Edge Cookie modules declare a permission, which is storing on the device, so no geo or device module here gates anything today. The other seven values (the request, the evidence, the connection, the permissions, the consent, the settings and the services) are never gated. A device module that an adapter builds itself, as the Fastly adapter does when none is registered, is not held in the services, so its signals are gated by nothing and such a module must declare no permission. A permission signal module's declaration passes nothing yet, because the signal modules run before any permission is resolved.

| Module            | Called through                                 | What its context carries                                                                                                                 |
| ----------------- | ---------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| Device            | `DeviceModule::detect`                         | The request, the evidence, the settings and the services. No permission is resolved yet                                                  |
| Permission signal | `SignalInput::call`, `valid_signal` and `tdls` | The request, the evidence, the connection, the consent, the settings and the services, and the location when its use needs no permission |
| Edge Cookie       | `generate` and `resolve_from_client`           | Every value above                                                                                                                        |
| Route             | `IntegrationProxy::handle`                     | Every value above except the evidence, because the route holds the request itself                                                        |

**When a value cannot be passed.** A value is withheld for one of four reasons, checked in this order. The context carries none where the module runs (absent), the module does not declare a permission that the value's use needs (not declared), the value's use needs a permission and none are resolved where the module runs (unresolved), or the request does not set a permission that the value's use needs (not granted). The modules in this repository answer a skipped call quietly. A device module answers the unknown device, an Edge Cookie module creates no identifier, and a permission signal module vouches for no signal and declares no terms.

A route ends its call with `?`, and core turns the reason into the response. A value that the request's permissions withhold, being not declared or not granted, answers `403`. A value that is absent, or that needs permissions that are not resolved where the route runs, answers `500`, because core reads that as a fault in how the route was written or placed and not as the reader's. A route always has the request, the connection, the permissions, the consent, the settings and the services, so it names those without `Option`. It never has the evidence, because it holds the request itself, so a route that names the evidence answers `500` every time. The other three depend on the reader, and a route names them as `Option` unless it must not run without them.

| Value                      | Parameter        | Absent when                                                                                                                    |
| -------------------------- | ---------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| The location               | `&GeoInfo`       | No geo module is selected, which is the default, or the module finds no location for the reader's address, or the lookup fails |
| The device signals         | `&DeviceSignals` | The adapter classifies no device, which is every adapter but Fastly                                                            |
| The Edge Cookie identifier | `EdgeCookie`     | No Edge Cookie module is selected, or the reader has no identifier and none was created for this request                       |

Core answers an absent value with `500` whether no reader could have it, such as the evidence in a route, or only some readers lack it. A route that names `&GeoInfo`, `&DeviceSignals` or `EdgeCookie` without `Option` therefore answers `500` to every reader for whom the value is absent. For the location that is every reader where no geo module is selected, and for the device signals it is every reader on an adapter other than Fastly. The identifier has a second per-reader case. A reader whose cookie holds an identifier but whose request does not grant storing on the device is withheld the identifier, and a route that names the identifier without `Option` answers `403` to that reader. None of the twelve routes in this repository names the location, the device signals or the identifier, so none of them can answer `403` or `500` for a missing value today.

**What changes for a module.** These are breaking changes to the module traits. They are made once, on purpose, so that handing a module a further value later does not break the traits again.

| Trait                    | Before                                                                                             | After                                                                                                                                                  |
| ------------------------ | -------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `DeviceModule`           | `detect(&self, request_info, services)`                                                            | `detect(&self, call)`                                                                                                                                  |
| `EdgeCookieModule`       | `generate(&self, request_info, input, services)` and `resolve_from_client(&self, input, services)` | `generate(&self, call)` and `resolve_from_client(&self, call, payload)`. `IdentityInput` and `ClientResolveInput` are removed                          |
| `PermissionSignalModule` | `valid_signal(&self, consent, evidence)` and `tdls(&self, consent, evidence)`                      | `valid_signal(&self, call)` and `tdls(&self, call)`. `SignalInput` keeps its fields and gains `call()`. `required_permissions` is new, none by default |
| `IntegrationProxy`       | `handle(&self, settings, services, req)`                                                           | `handle(&self, call, req)`. `required_permissions` is new, none by default                                                                             |
| `assemble_permissions`   | `(consent, evidence, geo, modules)`                                                                | `(context, geo, modules)`                                                                                                                              |

Every module in this repository is converted: the built-in device module and the Fastly one, the HMAC, host signal and `client_fixed` Edge Cookie modules, the five permission signal crates, the seam probe's device and Edge Cookie modules, and the twelve routes (ten vendor crates, the seam probe and core's JavaScript asset proxy).

A module outside this repository moves the same way. It takes the `ModuleCall` where its trait now asks for one. It writes a function that names, as parameters, what its old parameter list carried, and hands that function to `call.inject`, or to `call.inject_with` with the request or the payload as the argument. A device module names `&dyn RequestInfo` and `&RuntimeServices`, a route names `&Settings` and `&RuntimeServices`, and a permission signal module names `&ConsentContext` and `&dyn RequestInfo`. An Edge Cookie module that read an `IdentityInput` names `&PermissionState` and `&ConsentContext` in its place. The integration guide (`docs/guide/integration-guide.md`, "What a module is handed") shows a route.

**What it costs a request.**

- Permission assembly borrows the request, so it allocates nothing more. `http_util` reads the host and the scheme borrowed for that, and `RequestInfo::from_request` owns copies as before.
- The Edge Cookie state's snapshot of the request, taken only when a module is configured and the request carries no identifier or is a navigation, holds the method, the host and the scheme beside the path and query it already held. The snapshot still copies the request headers, as before.
- A module route takes the request, so the registry copies what the request resolved to (method, path, query, host and scheme) for the context. The resolve endpoint (`POST /_ts/api/v1/ec/resolve`, which only the Fastly adapter routes) makes the same copy once it reaches the module. Neither copies the headers.
- On Axum, Cloudflare and Spin, every module route now reads the Edge Cookie state, as the publisher path does there and as Fastly does for every request. The route did none of the following before:
  - a geo lookup,
  - a parse of the `Cookie` header,
  - the resolution of the selected Edge Cookie module, which Axum repeats on each request for a built-in module, and Cloudflare and Spin repeat on every request because they build their state for each one,
  - a decode of the consent signals, and
  - a run of every permission signal module over the permissions.

The geo lookup is the selected geo module's own call. With `[geo] module` unset, which is the default, the module resolves nothing and the lookup costs nothing. No geo module in this repository reads a store, because the Axum and Spin lookups resolve nothing, the Cloudflare lookup answers from the `cf-*` request headers and the Fastly lookup is the host's own. A geo module that a deployment adds may call a backend or read a store, and a module route then pays that on every request as well. On Fastly a module route pays only the copies above.

## 3. What changes for a deployment

This change adds no setting, so no setting selects it. Every deployment gets it and none can turn it off. It adds no startup check either.

On Fastly a module route behaves as before, because the Fastly adapter already built the Edge Cookie state for every request. On Axum, Cloudflare and Spin a module route is now called with that state, built as the publisher path builds it. Before, those adapters called a module route with an empty state that carried no permissions at all. Three things follow on those adapters.

1. **A module route pays for the state.** The costs are listed in section 2.
2. **A module route can fail before it runs.** A request whose `Cookie` header is not valid UTF-8 now answers `400`, and a selected Edge Cookie module that cannot be built for the request now answers `500`. A module route answered neither before. A bad consent string and a failed geo lookup do not fail the request.
3. **A navigation to a module route can create an identifier that nothing keeps.** When an Edge Cookie module is selected, the reader has no identifier and the reader's permissions allow one, the registry's `handle_proxy` asks the module for an identifier before it calls the route. It does so on a document navigation, being a `Sec-Fetch-Dest` of `document` or, where the request has no `Sec-Fetch-Dest`, an `Accept` header naming `text/html`. No other request on these adapters creates an identifier, because only the Fastly adapter creates one for a page. These adapters pass no identity graph and finalize no response, so the identifier lasts for that request. It is not stored, no cookie is set, and the module is asked again on the next navigation. The route is handed the identifier when it declares the permissions that the module declares.

| Adapter    | Reads the Edge Cookie state for a module route | Can hand a route the device signals | Stores a created identifier and sets its cookie                                                                               |
| ---------- | ---------------------------------------------- | ----------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| Fastly     | For every request, as before                   | Yes, classified at the entry point  | Yes for a client it recognizes as a browser, when `ec.ec_store` is set, through the identity graph and the response finalizer |
| Axum       | Now. Before, an empty state                    | No, it classifies no device         | No, it passes no identity graph and finalizes no response                                                                     |
| Cloudflare | Now. Before, an empty state                    | No, it classifies no device         | No, it passes no identity graph and finalizes no response                                                                     |
| Spin       | Now. Before, an empty state                    | No, it classifies no device         | No, it passes no identity graph and finalizes no response                                                                     |

## 4. What is not in this change

These kinds of module are not handed the context, and each keeps the parameters it had.

- A request preparer and a response finalizer stay plain functions on the builder. A preparer changes the request before anything about it is resolved, so there is nothing resolved to hand it, and it holds the request mutably, which a borrowed context cannot share. A finalizer is handed only what its own module left for the request.
- A request filter is handed the settings, the services, the request, the location and the permissions in one input. The location reaches it whatever the geo module declares, because the context's check is not applied there. The DataDome filter reads the network number of the reader's address from it.
- The page rewriters and injectors stay as they were, being the attribute rewriter, the script rewriter, the HTML stream processor factory and the head injector.
- The geo module is still called as `PlatformGeo::lookup(client_ip, services)`. A later change in the stack, described in `2026-10-15-geo-module-request-design.md`, hands it a module call.
- The auction providers and the demand sources stay as they were.

## 5. Acceptance

**Checks.** The pull request description records the local check on `359e30a32`, the last of this change's own commits: format, Clippy with warnings denied on the six adapter targets, the `trusted-server-core` tests (2,553 passed), the Fastly tests under Viceroy (3,582), the Axum alias with every module crate's tests (897), and the Cloudflare and Spin tests. CI on the fork passed 19 of 19 checks on that commit and on the head, `bc28b3e7f`. The checks include the cross-adapter parity tests, the permissions inspector's wasm build, the integration tests and the docs format check. Two merges of the branches below this one in the stack separate the two commits, and they bring in upstream changes that leave the module context and the calling of module routes untouched.

**Tests this change adds.** Thirteen in `crates/trusted-server-core/src/module_context/tests.rs`, for the context and for calling a module's function with it. They cover what each value does when it is carried, absent, withheld or ungated, the argument of the caller's own, an `async` function, a tuple, the state a request carries and the `403` and `500` answers. Three more sit with the modules. A device module with no evidence answers the unknown device, a signal module answers for the host of the request it is handed, and a route is handed its request's permissions and identifier, where a route that declares nothing sees the permissions and not the identifier. The existing tests of the evidence an Edge Cookie module reads and of the resolve round trip also check that the request reaches the module.

**What no test covers.**

1. Of the Axum, Cloudflare and Spin adapters, only Axum has tests that send a request to a module route, being the APS renderer route and the seam probe's report route. Neither route names a value from the context beyond the services. So no Axum, Cloudflare or Spin test shows a route receiving the permissions, the location or the identifier, the new failure on a bad `Cookie` header, or the identifier created on a navigation. The one test of a route receiving resolved state builds the state by hand, in `crates/trusted-server-core/src/integrations/registry.rs`.
2. No test sends a request through a route that names a value the request withholds or lacks, so no test shows the `403` or `500` a route answers. `a_route_answers_forbidden_only_for_a_value_its_permissions_withhold` converts each reason to a status without calling a route.
3. The vendor crates' tests call a route's function directly, so they skip `handle` and the context.

## 6. Open questions

These are for the reviewer. This revision records what the code does and decides none of them.

1. Should a navigation to a module route create an identifier on Axum, Cloudflare and Spin, where nothing stores it or sets the cookie?
2. Should a module route on those adapters fail when the Edge Cookie state cannot be built, as every request does on Fastly, or be called with an empty state as it was before?
3. Should a route that names the location, the device signals or the identifier without `Option` answer `500` for a reader who simply has none, when `500` is also the answer for a value no reader could have, such as the evidence in a route?
4. Should a request filter be handed the location only when the geo module's declared permissions allow it, as the module context does?
5. Should a permission signal module be able to declare permissions at all, when no permission is resolved while the signal modules run?

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                         |
| ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand.                                                                                                                                                                                                                                                                                                                                                |
| 2026-10-15 | Fixes from review round 1. The header, the failure path and the identifier on three adapters in place of the claim that settings select the change, the costs of a module route, the answers for a missing value, the gating defaults, what keeps its own parameter list, the move of a module outside this repository, the acceptance on the head, the untested paths, the open questions and the review log. |
| 2026-10-16 | Review round 2, approved, with the module resolution on Cloudflare and Spin stated per request.                                                                                                                                                                                                                                                                                                                |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/8-module-context` at `bc28b3e7f`. Issues found, three of them blocking. The blocking issues were that section 3 said settings select the change when none does and left out what the Axum, Cloudflare and Spin adapters now do for every module route, that the costs left out the geo lookup and the other work each of those routes now does, and that nothing said what a route answers when a value is withheld or absent. This revision states what the code does for each, adds the adapter table, the answers for a missing value and the untested paths, corrects the partly true claims and the header, and leaves the code questions in section 6 and the missing tests in section 5 for code changes.
2. **Round 2, 2026-10-16.** A lighter review in a fresh session, checking the round 1 fixes against `split/8-module-context` at `bc28b3e7f`. Approved, none blocking. This revision says Cloudflare and Spin resolve the selected Edge Cookie module on every request, as they build their state for each one, and the fork's check runs confirm 19 of 19 on both commits the spec names.
