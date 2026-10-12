# Design Spec: One module context

**Status:** Proposed, 2026-10-12. Implemented on the fork as jwrosewell/trusted-server#3, ready for review, answering IABTechLab/trusted-server#1267. This pull request (#1084) carries every design document of the stack in one place.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-08-27-integration-provider-seam-design.md`
**Related PRs:** #1094, jwrosewell/trusted-server#3
**Last updated:** 2026-10-12

> **Why this spec exists.** Each kind of module is called with its own parameter list.

## 1. Problem

Each kind of module is called with its own parameter list. A device module is handed `request_info` and `services`, an Edge Cookie module `request_info`, an `IdentityInput` and `services`, a permission signal module the consent and the evidence, and a route the settings and the services. Handing any of them one more value, such as the permissions resolved for the request, means changing that trait and every implementation of it, in this repository and in every vendor's crate.

Nothing in those lists says which values a module may use. The location, the device signals and the Edge Cookie identifier each need a permission to use, and a module is handed them whether or not it declares that permission.

## 2. Design

**What a module is handed.** A module hands a function of its own to `call.inject`, and that function names what it needs as parameters:

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

A parameter is the request (method, path, query, host and scheme), the reader's evidence, the connection, the resolved permissions, the decoded consent, the settings, the services, the location, the device signals or the Edge Cookie identifier. It may be an `Option` of one, which is `None` when the value cannot be passed, or a tuple of several. A function that names a value itself is not called when the value cannot be passed, and the reason goes to the debug log.

The location, the device signals and the Edge Cookie identifier each carry the permissions that the module which produced them declares. One of them is passed only to a module that declares every one of those permissions and is granted them on this request.

| Module            | Called through                                 | What its context carries                                                                                                                 |
| ----------------- | ---------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| Device            | `DeviceModule::detect`                         | The request, the evidence, the settings and the services. No permission is resolved yet                                                  |
| Permission signal | `SignalInput::call`, `valid_signal` and `tdls` | The request, the evidence, the connection, the consent, the settings and the services, and the location when its use needs no permission |
| Edge Cookie       | `generate` and `resolve_from_client`           | Every value above                                                                                                                        |
| Route             | `IntegrationProxy::handle`                     | Every value above except the evidence, because the route holds the request itself                                                        |

**What changes for a module.** These are breaking changes to the module traits. They are made once, on purpose, so that handing a module a further value later does not break the traits again.

| Trait                    | Before                                                                                             | After                                                                                                                                                  |
| ------------------------ | -------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `DeviceModule`           | `detect(&self, request_info, services)`                                                            | `detect(&self, call)`                                                                                                                                  |
| `EdgeCookieModule`       | `generate(&self, request_info, input, services)` and `resolve_from_client(&self, input, services)` | `generate(&self, call)` and `resolve_from_client(&self, call, payload)`. `IdentityInput` and `ClientResolveInput` are removed                          |
| `PermissionSignalModule` | `valid_signal(&self, consent, evidence)` and `tdls(&self, consent, evidence)`                      | `valid_signal(&self, call)` and `tdls(&self, call)`. `SignalInput` keeps its fields and gains `call()`. `required_permissions` is new, none by default |
| `IntegrationProxy`       | `handle(&self, settings, services, req)`                                                           | `handle(&self, call, req)`. `required_permissions` is new, none by default                                                                             |
| `assemble_permissions`   | `(consent, evidence, geo, modules)`                                                                | `(context, geo, modules)`                                                                                                                              |

Every module in this repository is converted: the built-in device module and the Fastly one, the HMAC, host signal and `client_fixed` Edge Cookie modules, the five permission signal crates, and the twelve routes (ten vendor crates, the seam probe and core's JavaScript asset proxy).

**What is not handed the context.** A request preparer and a response finalizer stay plain functions on the builder. A preparer changes the request before anything about it is resolved, so there is nothing resolved to hand it and it holds the request mutably, which a borrowed context cannot share. A finalizer is handed only what its own module left for the request.

**What it costs a request.** - Permission assembly borrows the request, so it allocates nothing more. `http_util` reads the host and the scheme borrowed for that, and `RequestInfo::from_request` owns copies as before.

- The Edge Cookie state's snapshot of the request, taken only when a module is configured and the request carries no identifier or is a navigation, holds the host and the scheme beside the path and query it already held.
- A module route takes the request, so the registry copies what the request resolved to (method, address, host and scheme) for the context. It does not copy the headers.
- The Axum, Cloudflare and Spin adapters now resolve the request's Edge Cookie state for a module route, as their publisher path does and as Fastly does for every request. Before, they dispatched a module route with an empty state, which carried no permissions at all, so a route there could not have been told what the request allowed.

## 3. What is not in this change

Nothing beyond the design above. A deployment whose settings do not select the change behaves as before.

## 4. Acceptance

On e8c7ef5f5, locally on Windows, the whole check: `cargo fmt --all -- --check`, six Clippy runs with warnings denied (Fastly, Axum, Cloudflare native and wasm, Spin native and wasm), `cargo test -p trusted-server-core` (2,573 passed), the Fastly tests under Viceroy (3,581 passed), the Axum alias with every module crate's tests (897 passed), Cloudflare (56) and Spin (90), the seam probe (11), the parity tests (17), the Viceroy configuration generator's tests (8), the integration tests that need no container (29), the permissions inspector's wasm build, and the docs format, lint and build.

New tests: thirteen for the context and its adapter (twelve after the last code commit, which removes one with the value it tested), a device module with no evidence answering the unknown device, a signal module answering for the host of the request it is handed, and a route handed its request's permissions and identifier, where one that declares nothing sees the permissions and not the identifier. The existing tests of the evidence an Edge Cookie module reads and of the resolve round trip also check that the request reaches the module.

CI on this fork passed on the head, 19 of 19 checks, and on each of the five commits pushed before it. On 8d18b40b2 one job first failed in `collects_gpt_slot_from_local_fixture`, a test of the `ts` tool that launches a browser and timed out doing so on the runner. It passed when the job was run again, and this change does not touch it.

The pull request's description records how it was checked, with CI green on all 19 checks.

## Revision record

| Date       | Change                                                          |
| ---------- | --------------------------------------------------------------- |
| 2026-10-12 | First draft, from the pull request and its issue as they stand. |
