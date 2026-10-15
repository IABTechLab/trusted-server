# Design Spec: A geo module is called with the request, as a device module is

**Status:** Proposed, 2026-10-15. Written after the code, for draft pull request IABTechLab/trusted-server#1374
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-12-module-context-design.md`, `2026-10-12-device-signals-design.md`, `2026-08-27-integration-provider-seam-design.md`, `2026-09-07-didomi-geo-design.md`. Every design document of the stack is in IABTechLab/trusted-server#1342.
**Related PRs:** IABTechLab/trusted-server#1374, reviewed on jwrosewell/trusted-server#28
**Issues:** IABTechLab/trusted-server#1321
**Last updated:** 2026-10-15

> **Why this spec exists.** A geo module was handed the client address and nothing else of the request, so a vendor whose one service answers location and device from the same request could not answer both from one call.

## 1. Problem

A geo module was called as `PlatformGeo::lookup(client_ip, services)`, so the module was handed the client address and the services and nothing else of the request. A device module is called as `DeviceModule::detect(call)`, with a module call that carries the request's evidence (its headers, path and query), the settings and the services.

A vendor whose one service answers location and device from the same request, keyed on the whole request, could not make the geo answer come from the same call as the device answer. The geo module never saw the evidence. The vendor therefore asked its service twice, or the geo answer depended on the device module having run first on the same adapter.

Six places ask for a location with the request in hand, and section 2.2 lists them. Each handed the module the address alone.

A second fault sat in the Fastly adapter. The step that writes the `x-geo-*` headers on every response built its module with `build_geo_module` alone. That function cannot see the registry, so the function answers the host's own lookup for `platform` and the module that resolves nothing for every other name. A deployment whose `[geo] module` named a module from the registry got no location on that path, and every page carried `x-geo-info-available: false`, while the same module answered the request path.

## 2. Design

A geo module becomes a `GeoModule`, called with the same module call as a device module. A lookup that needs the client address alone stays a `PlatformGeo` and is wrapped.

### 2.1 The interface

`GeoModule` is in `crates/trusted-server-core/src/platform/traits.rs`.

```rust
#[async_trait::async_trait(?Send)]
pub trait GeoModule: Send + Sync {
    fn id(&self) -> &'static str;

    async fn locate(
        &self,
        call: ModuleCall<'_>,
    ) -> Result<Option<GeoInfo>, Report<PlatformError>>;

    fn required_permissions(&self) -> PermissionSet {
        PermissionSet::none()
    }
}
```

**The answer.** `Ok(Some(info))` is a location. `Ok(None)` means the module has no data for the request. An error is an unexpected failure and should be `PlatformError::Geo`. The country is an ISO 3166-1 alpha-2 code and the region is the ISO 3166-2 subdivision code without the country prefix, as `PlatformGeo::lookup` gives them, so the permission rules keyed on those codes read a module's answer unchanged.

**The call.** A module takes what it needs from the call by type, as a device module does. A module that answers from the evidence reads it like this.

```rust
let evidence = <&dyn RequestInfo>::from_module_context(&call)
    .map_err(|_| Report::new(PlatformError::Geo))?;
let hint = evidence.header("x-example-hint");
```

The test module `EvidenceReadingGeo`, in `crates/trusted-server-core/src/platform/geo.rs`, reads its evidence in this way and answers from one of its headers.

**Name and permissions.** `id` names the module for the call and the logs. `required_permissions` is empty by default. Section 2.7 says what it guards.

### 2.2 The call, built in one place

`locate_request` is a method on `dyn GeoModule`, in `crates/trusted-server-core/src/platform/geo.rs`.

```rust
pub async fn locate_request(
    &self,
    request: ModuleRequest<'_>,
    headers: Option<&HeaderMap>,
    settings: Option<&Settings>,
    services: &RuntimeServices,
) -> Result<Option<GeoInfo>, Report<PlatformError>>
```

The method builds the call and calls `locate` with the module's own `id()` and `required_permissions()`. The call carries five things.

1. The request's view, being its method, host, scheme, path and query.
2. The evidence, made of the client address as text (empty when the address is unknown), the `headers` when the caller passes them, and the request's path and query.
3. The services.
4. The client's metadata (`ClientInfo`), taken from `services.client_info()`.
5. The settings, when the caller passes them.

With `headers` as `None`, the evidence still carries the address, the path and the query, and has no headers.

One function builds the call so that a module is handed the same kinds of evidence wherever core asks for a location. Six callers use it.

| Caller                                                                                                   | Function                                                                                         | Headers passed                                           | Settings passed                 | When the module fails                                                             |
| -------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ | -------------------------------------------------------- | ------------------------------- | --------------------------------------------------------------------------------- |
| The Edge Cookie state, which every adapter reads before routing, and the permissions inspection endpoint | `EcContext::read_from_request_resolving_geo` in `crates/trusted-server-core/src/ec/mod.rs`       | The request's                                            | Yes                             | Logged at error level, and every permission resolves to the requires-signal floor |
| The auction endpoint                                                                                     | `handle_auction` in `crates/trusted-server-core/src/auction/endpoints.rs`                        | The request's                                            | Yes                             | Logged as a warning, and the auction goes on with no location                     |
| The Didomi notice loader                                                                                 | `DidomiIntegration::route` in `crates/cmp/didomi/src/lib.rs`                                     | The request's                                            | No, the route is not handed any | The loader answers with a geo failure response                                    |
| The seam probe's report                                                                                  | `SeamProbeProxy::route` in `crates/testing/seam-probe/src/lib.rs`                                | The request's                                            | No                              | Logged as a warning, and the report's country is null                             |
| Fastly response headers, router path                                                                     | `FinalizeResponseMiddleware::handle` in `crates/trusted-server-adapter-fastly/src/middleware.rs` | The request's, kept before the handler takes the request | Yes                             | Logged as a warning, and the response carries `x-geo-info-available: false`       |
| Fastly response headers, entry-point path                                                                | `apply_entry_point_finalize_headers` in `crates/trusted-server-adapter-fastly/src/lib.rs`        | None                                                     | Yes                             | The same                                                                          |

### 2.3 A lookup that needs only the address

`PlatformGeo` is unchanged. `PlatformGeoModule::new(name, platform)`, in `crates/trusted-server-core/src/platform/geo.rs`, calls one as a `GeoModule`.

- `id` returns `name`.
- `locate` takes the services from the call. A call that carries no services answers `Ok(None)` and asks the lookup nothing.
- `locate` takes the client address from the call's client metadata, and passes no address when the call carries no client or the client has no address. `locate` then calls `platform.lookup(address, services)` and returns the answer as it is.
- `required_permissions` returns the wrapped lookup's declaration.

No blanket implementation makes every `PlatformGeo` a `GeoModule`. A type that implements `PlatformGeo` is passed through `PlatformGeoModule::new` wherever a `GeoModule` is wanted.

### 2.4 Where core holds the module

`RuntimeServices::geo()` returns `&dyn GeoModule`. `RuntimeServicesBuilder::geo` and `RuntimeServices::with_geo` take an `Arc<dyn GeoModule>`. All three are in `crates/trusted-server-core/src/platform/types.rs`.

`build_geo_module(settings, host_default)`, in `crates/trusted-server-core/src/platform/mod.rs`, returns an `Arc<dyn GeoModule>`. When `[geo] module` is `platform`, it returns `PlatformGeoModule::new("platform", host_default)`. For every other value, including none, it returns `PlatformGeoModule::new("none", Arc::new(DisabledGeo))`. The function cannot see the registry, so for a name from the registry it returns the module that resolves nothing, as the base the adapter replaces.

The registry, in `crates/trusted-server-core/src/integrations/registry.rs`, holds `Arc<dyn GeoModule>`. `IntegrationRegistrationBuilder::with_geo_module(name, module)` offers one. `IntegrationRegistry::geo_module()` returns the module that `[geo] module` resolved to, which is:

- the `none` module, wrapping `DisabledGeo`, when the selector is unset or `none`
- nothing when the selector is `platform`, so the adapter's own lookup stands
- the offered module of that name otherwise

### 2.5 Each adapter

Every adapter builds the services for a request with `build_geo_module`, wrapping the adapter's own lookup, and then replaces that module with the registry's when `IntegrationRegistry::geo_module()` returns one.

| Adapter    | What `platform` selects                                                                                                                                                                | `x-geo-info-available`                                                  |
| ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------- |
| Fastly     | `FastlyPlatformGeo` in `crates/geo/fastly`, which resolves the client address with Fastly's geo lookup                                                                                 | From the module's answer, on the two steps described below              |
| Axum       | `AxumPlatformGeo`, which resolves nothing                                                                                                                                              | Always `false`                                                          |
| Cloudflare | `CloudflareGeo`, built from the request's `cf-ipcountry`, `cf-ipcity`, `cf-ipcontinent`, `cf-iplatitude`, `cf-iplongitude` and `cf-region-code` headers and ignores the client address | `true` when `cf-ipcountry` is present and not `XX`, with no module call |
| Spin       | `NullGeo`, which resolves nothing                                                                                                                                                      | Always `false`                                                          |

**Fastly.** `build_per_request_services` in `crates/trusted-server-adapter-fastly/src/app.rs` builds the services with `build_geo_module(&state.settings, Arc::new(FastlyPlatformGeo))` and replaces the module with `state.registry.geo_module()` when that returns one. The Edge Cookie state, the auction endpoint and every module that calls `services.geo()` use that module.

The adapter writes the `x-geo-*` headers on every response, from the location a module resolves for the request. Two steps do this.

1. `FinalizeResponseMiddleware` finishes every response that passes through the router's middleware chain. The router is given `finalize_geo_module(state)` in `app.rs`, which is the registry's module when `[geo] module` names one and otherwise what `build_geo_module` returns. Before the handler takes the request, `handle` keeps the request's method, headers, URI, scheme and host. After the handler, `handle` calls `locate_request` with those and the settings. `handle` skips the call when `geo_allowed_for_response` is false, which is the case for a 401 response. The headers are copied on every request, before the middleware knows whether the response will be looked up.
2. `apply_entry_point_finalize_headers` finishes a response that did not pass through the middleware, such as the 405 the router returns for a method no route accepts. The middleware marks the responses it finished, and the entry point runs this step for those without the mark. The step builds its module with `build_geo_module`, so the module is the host's lookup or the module that resolves nothing, and never one from the registry. The step calls `locate_request` with the request's view from the copy the entry point kept (method, host, scheme, path and query) and with the settings. The step passes no headers, because the entry point keeps only the request's view.

Core does not carry the location from the request path to either step, so each step calls the module again. On the router path the evidence is the same as on the request path, so a module that holds its answers by evidence gives the answer it already holds. On the entry-point path the evidence has no headers, so such a module sees different evidence.

**Axum, Cloudflare and Spin.** Each builds the request's services in `build_runtime_services` in its `platform.rs`, and `services_for_request` in its `app.rs` replaces the module with the registry's. This series changes no non-test line in these three adapters, because `build_geo_module` and `RuntimeServices::with_geo` carry the new type. None of the three adapters calls the geo module when writing response headers, so a registry module reaches them only through the core callers in section 2.2.

### 2.6 Configuration

No setting is added or changed. The `[geo]` section keeps its two keys.

| Key                          | Type and default | Meaning                                                                                                                                                                                                                                                                                      |
| ---------------------------- | ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `module`                     | Text, unset      | Unset and `none` select the module that resolves nothing and make no host geo call. `platform` selects the adapter's own lookup. Any other value names a module that a linked crate offers with `with_geo_module`. The environment variable `TRUSTED_SERVER__GEO__MODULE` overrides the key. |
| `assume_single_jurisdiction` | Boolean, `false` | Acknowledges that with no geo module every request is treated as coming from the top of the `permissions.yaml` rules tree.                                                                                                                                                                   |

**Validation.**

- A value that no linked module offers is refused at startup, with a configuration error that names the geo modules the deployment runs (`named_module` in `registry.rs`).
- Two registrations that offer a geo module under one name are refused at startup, and the error names both.
- An offered module that the selector does not choose is logged as a warning when the registry is built.
- `assume_single_jurisdiction` must be `true` when an Edge Cookie module is configured and `[geo] module` is unset or `none` (`GeoConfig::validate_jurisdiction_acknowledgment` in `settings.rs`).

**Defaults.** The defaults are unchanged. A deployment with no `[geo] module` resolves no location and makes no host geo call.

**Documents.** `docs/guide/configuration.md` says in the introduction to `[geo]` that the module is called with the request. `docs/guide/integration-guide.md` adds `PlatformGeoModule` to the example runtime services and describes `GeoModule` in the declarations table and in a paragraph below it. `CHANGELOG.md` records the change as breaking for crates outside core.

### 2.7 Consent and privacy

**What a geo module is handed.** Before, a geo module was handed the client address and the services. Now `locate_request` hands it these.

- The client address and the client's metadata.
- The request's method, host, scheme, path and query.
- Every request header the caller passes, which a module reads by name. The `Cookie` header is among them, and `RequestInfo::cookie` reads one cookie from it. Core removes no header before the call.
- The services, which a lookup was already handed.
- The settings, when the caller has them.

Each caller passes the headers of the request it is serving, except `apply_entry_point_finalize_headers`, which passes none.

**What a geo module is not handed.** The call carries no decoded consent, no resolved permissions, no location, no device signals and no Edge Cookie identifier. On the Edge Cookie path none of these exists yet, because the location is resolved first and then chooses which permission rules apply to the request.

**No permission check guards the call.** The evidence is handed to a geo module without a check, as it is handed to a device module.

**A permission guards the answer.** When a geo module declares permissions, the location is passed on to another module only if that module declares every one of them and the request has been granted them. When `required_permissions` is empty, which is the default, the location is passed to every module. `ModuleContext::with_request_state` in `crates/trusted-server-core/src/module_context.rs` and the permission step of the Edge Cookie state in `crates/trusted-server-core/src/ec/mod.rs` set the requirement from `services.geo().required_permissions()`. `PlatformGeoModule` gives the wrapped lookup's declaration.

**What the built-in lookups read.** The Fastly lookup reads the client address alone, so a deployment on `platform` or `none` sends no more to a host service than before. The Cloudflare lookup reads the platform's `cf-*` request headers when the services are built, as before. `PlatformGeoModule` hands each lookup the address and nothing else of the evidence.

**What a module does with the evidence.** Core filters nothing from the evidence, and a module that sends any of it to another service decides what to send. The operator controls what is sent by choosing the module with `[geo] module`. With the key unset or `none`, the module called uses none of the request's data and answers nothing.

**Responses that get no lookup.** On both Fastly steps, `geo_allowed_for_response` stops the call for a 401 response, so no module is called and no header is handed to one for a response that did not authenticate.

### 2.8 Failure handling

A module answers with a location, with `Ok(None)` or with an error, and each caller handles the last two as the table in section 2.2 shows. `Ok(None)` means no location. An error means the lookup failed. The Edge Cookie state and the Didomi loader tell the two apart. The other callers treat both as no location.

The Edge Cookie state resolves permissions at the top node of the `permissions.yaml` rules tree when there is no location. It resolves every permission to the requires-signal floor when the lookup fails, and logs the failure at error level. The Didomi loader turns a missing location into the geo failure response `geo_unavailable`, a failed lookup into `lookup_failed`, and a location it cannot normalize into the reason the normalization gives.

- A call with no services reaches `PlatformGeoModule` and answers `Ok(None)`. It is not an error.
- `locate_request` always supplies the evidence, so a module called through it always finds the evidence. A call built by hand without evidence withholds it, and the test modules return `PlatformError::Geo` for that.
- Core adds no timeout, retry or cache around the call. Each caller calls once and uses the answer. A module that reaches a service applies its own timeout and holds its own answers.

### 2.9 Rollout for existing deployments

**Deployments.** No settings document and no stored data changes. A deployment on `none` or `platform` behaves as before, because the wrapped lookups read the same address and the built-in lookups ignore the added evidence.

**One difference.** On Fastly, a deployment whose `[geo] module` names a module from the registry now gets that module's location in the `x-geo-*` headers of every response the middleware finishes. Before, those responses carried `x-geo-info-available: false`. Responses finished by the entry-point step are unchanged.

**Crates outside core** that use the items below stop compiling until they change, and `CHANGELOG.md` marks the change as breaking for them.

- `RuntimeServicesBuilder::geo`, `RuntimeServices::with_geo`, `IntegrationRegistrationBuilder::with_geo_module` and the Fastly adapter's `FinalizeResponseMiddleware::new` take an `Arc<dyn GeoModule>` where they took an `Arc<dyn PlatformGeo>`, and `build_geo_module` returns one.
- `RuntimeServices::geo()` returns `&dyn GeoModule`. A caller that called `lookup` on it calls `locate_request`.
- A crate that passed a `PlatformGeo` wraps it in `PlatformGeoModule::new(name, module)`. A crate that wants the request implements `GeoModule`.

In this repository the converted code is the six callers in section 2.2, the `ts` command line tool's local services in `crates/trusted-server-cli/src/commands/keys.rs`, the seam probe's registration, the example in the integration guide, and the test services in core, the Fastly adapter, the Axum adapter, the Prebid server crate, the 51Degrees loading cache crate and the Didomi crate.

## 3. What is not in this change

- **A geo module in the stock list that reads more than the address.** The host lookups for Fastly, Axum, Cloudflare and Spin stay `PlatformGeo` and are wrapped. The only module this repository offers through the registry is the seam probe's, which wraps a lookup that answers a fixed country.
- **The Fastly entry-point step.** `apply_entry_point_finalize_headers` still builds its module with `build_geo_module` and passes no headers. When `[geo] module` names a registry module, a response that reaches that step carries `x-geo-info-available: false`.
- **Geo in the response headers of Axum, Cloudflare and Spin.** Their `x-geo-info-available` does not follow the module, as the table in section 2.5 shows.
- **Making core share one answer between the geo call and the device call.** A vendor crate whose one service answers both holds the answer itself and returns it from `locate` and from `detect`.
- **Checking permissions before the evidence is handed over.** A geo module is handed the evidence without a check, as a device module is.
- **Any change to `PlatformGeo`, `GeoInfo`, the permission rules or the headers written.**

## 4. Acceptance

**Tests that exist.**

- `crates/trusted-server-core/src/platform/geo.rs` has `the_platform_lookup_is_given_the_address_the_call_carries`, `a_call_without_services_asks_the_platform_nothing`, `locate_request_hands_the_module_the_headers_and_the_client_address` and `locate_request_without_headers_still_carries_the_address`. The last two use a module that reads one header and the address from its evidence.
- `crates/trusted-server-core/src/platform/mod.rs` has `runtime_services_geo_lookup_returns_none_for_no_ip`, which calls the services' module with a call. The same file has `build_geo_module_defaults_to_no_geo`, `build_geo_module_none_selects_no_geo_explicitly` and `build_geo_module_uses_host_geo_when_platform_is_selected`, which check the module's name. The first of the three also checks that a call answers nothing.
- `crates/trusted-server-core/src/ec/mod.rs` has `a_geo_module_is_called_with_the_request_as_evidence`, where a module answers `FR` only if it reads a header from the call's evidence. The same file has `a_geo_module_reads_a_platform_service_through_the_services_it_is_given` and `a_geo_module_failure_resolves_permissions_at_the_requires_signal_floor`, which run through the wrapper.
- `crates/trusted-server-core/src/integrations/registry.rs` has `geo_selector_resolves_the_geo_module_the_selected_module_declares`, `geo_module_none_resolves_no_location` and `an_unset_geo_selector_resolves_the_disabled_module_and_platform_opts_in`. The same file has the refusals `geo_selector_rejects_a_module_that_declares_no_geo_module`, `geo_selector_rejects_a_module_no_section_selects`, `geo_selector_rejects_a_name_no_module_supplies` and `two_registrations_declaring_one_geo_module_name_are_refused`.
- `crates/trusted-server-adapter-fastly/src/middleware.rs` has `finalize_handle_hands_the_geo_module_the_request_headers`, where a module records one header of its evidence. The existing finalize tests in that file now run through the wrapper.
- `crates/trusted-server-adapter-fastly/src/app/seam_probe_tests.rs` and `crates/trusted-server-adapter-axum/tests/seam_probe.rs` each have `proxy_route_reports_the_modules_geo_and_that_the_preparer_ran`, where a route reaches the registry's module selected by `[geo] module` and gets its country back. Each also has `geo_selector_naming_a_module_without_a_geo_module_fails_at_startup`.
- `crates/trusted-server-adapter-spin/src/platform.rs` has `build_runtime_services_uses_noop_native_stores_without_handles`, which checks that the default module is named `none`.

The Cloudflare adapter's tests are unchanged and call its lookup directly.

**Joins with no test.**

1. The auction endpoint handing the module the request's headers and the settings.
2. The Didomi loader handing the module the request's headers. The Didomi tests run a stub lookup behind the wrapper, which reads only the address.
3. The seam probe handing the module the request's headers. The seam probe tests show that the registry's module is reached and answers, and its wrapped lookup ignores the evidence.
4. The settings reaching a module through `locate_request`. The tests that call it pass `None`, and no test module reads the settings from the call.
5. `FinalizeResponseMiddleware` handing the module the request's method, host, scheme, path, query and the settings. The new test checks one header only.
6. `finalize_geo_module` choosing the registry's module over the host's. The new Fastly test builds the middleware with a module passed in directly.
7. `apply_entry_point_finalize_headers` calling a module at all. The existing entry-point test covers `geo_allowed_for_response` and `apply_finalize_headers`.
8. `build_geo_module` wrapping the host lookup it is given. The earlier checks compared the two pointers, which the wrapper makes impossible, and the new checks read the module's name.
9. `PlatformGeoModule::required_permissions` returning the lookup's declaration, and the gate on the location that follows from it. The only test of a declaration is for `DisabledGeo`, which declares none.
10. A registry geo module answering a request on Cloudflare or Spin. The seam probe tests that do this run on Fastly and Axum only.
11. The wrapped Axum, Cloudflare and Spin lookups reaching a request through `build_runtime_services`. Their tests call the lookup directly, and the Spin test checks the name of the default module only.

The pull request records the fork's CI green on all 19 checks at 1b8b43858. Its checklist leaves the manual check under `fastly compute serve` unticked.

## Review log

No review rounds yet.

## Revision record

| Date       | Change                               |
| ---------- | ------------------------------------ |
| 2026-10-15 | First draft, written after the code. |
