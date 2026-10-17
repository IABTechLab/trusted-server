# Design Spec: A geo module is called with the request

**Status:** Proposed, 2026-10-15. Written after the code, for draft pull request IABTechLab/trusted-server#1374
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-10-12-module-context-design.md`, `2026-10-12-device-signals-design.md`, `2026-08-27-integration-provider-seam-design.md`, `2026-09-07-didomi-geo-design.md`. Every design document of the stack is in IABTechLab/trusted-server#1342.
**Related PRs:** IABTechLab/trusted-server#1374, reviewed on jwrosewell/trusted-server#28 (closed)
**Issues:** IABTechLab/trusted-server#1321
**Last updated:** 2026-10-16

> **Why this spec exists.** A geo module was handed the client address and nothing else of the request, so a vendor whose one service answers location and device from the same request could not answer both from one call.

## 1. Problem

A geo module was called as `PlatformGeo::lookup(client_ip, services)`, so the module was handed the client address and the services and nothing else of the request.

A device module is called as `DeviceModule::detect(call)`, with a module call that carries the request's view (its method, host, scheme, path and query), the request's evidence, the settings and the services. The evidence is narrow. On Fastly, the only adapter that calls `detect`, the evidence is the client address, the `User-Agent` header and, for a device module that a registration supplies, the `Cookie` header, and the evidence holds no path or query (`derive_device_signals` in `crates/trusted-server-adapter-fastly/src/lib.rs`). A device module's `advertising_attributes`, unlike `detect`, is handed every request header (`attach_device_attributes` in `crates/trusted-server-core/src/auction/formats.rs`).

A vendor whose one service answers location and device from the same request, keyed on the whole request, could not make the geo answer come from the same call as the device answer. The geo module never saw the evidence. The vendor therefore asked its service twice, or the geo answer depended on the device module having run first on the same adapter.

Six places ask for a location with the request in hand, and section 2.2 lists them. Each handed the module the address alone.

A second fault sat in the Fastly adapter. The step that writes the `x-geo-*` headers on every response built its module with `build_geo_module` alone. That function cannot see the registry, so the function answers the host's own lookup for `platform` and the module that resolves nothing for every other name. A deployment whose `[geo] module` named a module from the registry got no location on that path, and every page carried `x-geo-info-available: false`, while the same module answered the request path.

The issue that asked for this change (IABTechLab/trusted-server#1321) does not ask for the fix to this second fault. The issue asks for the call to be built where each adapter resolves geo, and says nothing of the response headers. The fix changes the `x-geo-*` headers that responses carry on a deployment whose `[geo] module` names a module from the registry, so the open questions at the end of section 2 ask whether the fix is a pull request of its own.

## 2. Design

A geo module becomes a `GeoModule`, called with a module call, the type a device module is called with. A lookup that needs the client address alone stays a `PlatformGeo` and is wrapped.

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

One function builds the call, so that a module is handed the same kinds of evidence from every caller. Six callers use that function.

| Caller                                                         | Function                                                                                         | Headers passed               | Settings passed                 | When the module fails                                                             |
| -------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ | ---------------------------- | ------------------------------- | --------------------------------------------------------------------------------- |
| The Edge Cookie state, and the permissions inspection endpoint | `EcContext::read_from_request_resolving_geo` in `crates/trusted-server-core/src/ec/mod.rs`       | The request's                | Yes                             | Logged at error level, and every permission resolves to the requires-signal floor |
| The auction endpoint                                           | `handle_auction` in `crates/trusted-server-core/src/auction/endpoints.rs`                        | The request's                | Yes                             | Logged as a warning, and the auction goes on with no location                     |
| The Didomi notice loader                                       | `DidomiIntegration::route` in `crates/cmp/didomi/src/lib.rs`                                     | The request's                | No, the route is not handed any | The loader answers with a geo failure response                                    |
| The seam probe's report                                        | `SeamProbeProxy::route` in `crates/testing/seam-probe/src/lib.rs`                                | The request's                | No                              | Logged as a warning, and the report's country is null                             |
| Fastly response headers, router path                           | `FinalizeResponseMiddleware::handle` in `crates/trusted-server-adapter-fastly/src/middleware.rs` | The request's, as it arrived | Yes                             | Logged as a warning, and the response carries `x-geo-info-available: false`       |
| Fastly response headers, entry-point path                      | `apply_entry_point_finalize_headers` in `crates/trusted-server-adapter-fastly/src/lib.rs`        | None                         | Yes                             | The same                                                                          |

Fastly reads the Edge Cookie state on the fallback (except for a closed path) and on the routes of its named route table other than config, batch sync and data. The read comes after the request preparers have run, except on the permissions route. A request preparer is a function that an integration registers with `with_request_preparer` to clean or change a request before routing, and `IntegrationRegistry::prepare_request` runs each one. Axum, Cloudflare and Spin read the state only in the handlers that need the state, being the auction, page bids, the integration proxy routes, the publisher fallback and the permissions endpoint, which reads it through `handle_permissions` in `crates/trusted-server-core/src/inspect/permissions.rs`.

**How the callers differ.** The six callers do not build the call alike.

1. The Edge Cookie state, the auction endpoint and the seam probe build the request's view with `ModuleRequest::of`, which reads the host from `Forwarded`, then `X-Forwarded-Host`, then `Host`, and the scheme from the client's TLS metadata, then `Forwarded`, then `X-Forwarded-Proto`, then `fastly-ssl`, and gives `http` when none of them says otherwise. The other three build the view by hand. The Didomi loader and the Fastly middleware read the `Host` header and take the scheme from the request's URI, or `https` when the URI has none. The Fastly entry-point step reads the `Host` header and gives `https` when the `fastly-ssl` header is present and `http` otherwise.
2. The services of the first four callers carry all the client's metadata that the entry point captured. The two Fastly finalize steps set the services' client metadata to the client address alone.
3. The two Fastly finalize steps are given a smaller services graph than the request path (`build_finalize_services` in `crates/trusted-server-adapter-fastly/src/app.rs`). The graph has the config, secret and key-value stores, the value cache, the loading caches, the backend, the HTTP client and the geo module, and each step adds the publisher's prefixes. The graph has no host signals, no device module, no Edge Cookie module, no permission signal modules and no template cache or assembler, and its auction telemetry sink records nothing. A geo module that reads any of these from the services finds nothing there on those two steps.

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

1. `FinalizeResponseMiddleware` finishes every response that passes through the router's middleware chain. The router is given `finalize_geo_module(state)` in `app.rs`, which is the module `IntegrationRegistry::geo_module()` returns, and what `build_geo_module` returns when that call returns `None`. Before the handler takes the request, `handle` keeps the request's method, headers and URI as the request arrived and builds the host and scheme by hand. After the handler, `handle` calls `locate_request` with the kept method, headers and URI, the host and scheme, the settings and the client address alone as the client's metadata. `handle` skips the call when `geo_allowed_for_response` is false, which is the case for a 401 response. The method, headers and URI are copied on every request, before the middleware knows the response's status, and the copy is made for a deployment whose `[geo] module` is unset too.
2. `apply_entry_point_finalize_headers` finishes a response that did not pass through the middleware, such as the 405 the router returns for a method no route accepts. The middleware marks the responses it finished, and the entry point runs this step for those without the mark. The step builds its module with `build_geo_module`, so the module is the host's lookup or the module that resolves nothing, and never one from the registry. The step calls `locate_request` with the request's view from the copy the entry point kept (method, host, scheme, path and query), the settings and the client address alone as the client's metadata. The step passes no headers, because the entry point keeps only the request's view.

**Why the entry-point step does not use `finalize_geo_module`.** The comment on the step says the module is the host's own lookup or the module that resolves nothing, never one from the registry, and that both read the address alone, so the call carries no headers. The comment gives no reason for leaving the registry out. The entry point holds the application state as an `Option` that is `None` when the state failed to build, and runs the step in that case too, with no registry to ask. When the state exists the entry point holds the registry, so the code could give the step the module `finalize_geo_module` returns, and does not.

**Why each step asks the module again.** The response already carries the location the request path resolved, inside `EcFinalizeState` in `app.rs`, on the responses whose handler read the Edge Cookie state, apart from two. The permissions route and the asset fallback read the state and attach no `EcFinalizeState`, so their responses carry no location. Neither step reads that location. The middleware keeps the request's headers so that a module that holds its answers by evidence gives the answer it already holds. The headers help only when the middleware's call carries what the request path's call carried, and four differences between the two calls can stop the middleware's call from carrying the same things.

1. A request preparer runs inside the handler (`prepare_request` in `execute_named` and `dispatch_fallback`), before the Edge Cookie state is read, and a preparer can change the request. The GPT diagnostics preparer (`crates/ad-tag/google/src/diagnostics.rs`) removes its reserved query parameter and cookie, and `IntegrationRegistry::prepare_request` runs every preparer whether or not its integration is switched on. The middleware keeps the headers and URI as the request arrived, before any preparer. When a preparer changes the request, the middleware's call therefore carries the unchanged request where the request path's call carries the changed one, and a module that holds its answers by the evidence it was given finds no answer held for the middleware's call.
2. The middleware builds the host and scheme by hand, and the request path's call builds them with `ModuleRequest::of` (section 2.2).
3. The middleware's call carries the client address alone as the client's metadata, and the request path's call carries what the entry point captured (section 2.2).
4. The middleware's services graph is the smaller one (section 2.2).

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
- Every request header the caller passes, which a module reads by name. The `Cookie` and `Authorization` headers are among the headers passed, and `RequestInfo::cookie` reads one cookie from the `Cookie` header. On Fastly the headers also include the ones the entry point sets from the platform's TLS metadata, when the platform supplies the metadata, being `fastly-ssl`, `x-ts-tls-protocol`, `x-ts-tls-cipher`, `x-ts-tls-ja4` and `x-ts-h2-fingerprint`. Core removes no header before the call.
- The services, which a lookup was already handed.
- The settings, when the caller has them.

Five of the six callers pass the headers of the request they are serving. `apply_entry_point_finalize_headers` passes none.

**What a device module is handed.** On Fastly, the evidence that `detect` is shown holds the client address, the `User-Agent` header and, for a device module that a registration supplies, the `Cookie` header, and holds no path or query. The Fastly device module that `[device] module = "fastly"` selects also reads the TLS and HTTP/2 fingerprints, which the adapter captures into `FastlyHostSignals` and does not pass as headers. Only `advertising_attributes`, which the auction code calls, is handed every request header. A geo module is therefore handed more of the request as evidence than `detect` is, and that includes what the device signals are made from, being the `User-Agent` header and, on Fastly, the TLS and HTTP/2 fingerprint headers.

**What a geo module is not handed.** The call carries no decoded consent, no resolved permissions, no location, no `DeviceSignals` value and no Edge Cookie identifier as values. The headers still carry what some of these values are made from, because the `Cookie` header holds the consent cookies and the Edge Cookie and a module reads each by name. On the Edge Cookie path none of these values exists yet, because the location is resolved first and then chooses which permission rules apply to the request.

**No permission check guards the call.** The evidence is handed to a geo module without a check. A device module's `detect` is also called without one, before permissions are resolved, but `detect` is handed fewer headers.

**A permission guards the answer, for other modules only.** When a geo module declares permissions, the location is passed on to another module only if that module declares every one of them and the request has been granted them. When `required_permissions` is empty, which is the default, the location is passed to every module. `ModuleContext::with_request_state` in `crates/trusted-server-core/src/module_context.rs` and the permission step of the Edge Cookie state in `crates/trusted-server-core/src/ec/mod.rs` set the requirement from `services.geo().required_permissions()`. `PlatformGeoModule` gives the wrapped lookup's declaration. No permission guards the response headers. On Fastly, `apply_finalize_headers` writes the answer's city, country, continent and coordinates, and the metro code and region when the answer has them, into the `x-geo-*` headers of every response except a 401, whatever permissions the module declares and whatever the request has been granted (`GeoInfo::set_response_headers` in `crates/trusted-server-core/src/geo.rs`).

**What the built-in lookups read.** The Fastly lookup reads the client address alone, so a deployment on `platform` or `none` sends no more to a host service than before. The Cloudflare lookup reads the platform's `cf-*` request headers when the services are built, as before. `PlatformGeoModule` hands each lookup the address and nothing else of the evidence.

**What a module does with the evidence.** Core filters nothing from the evidence, and a module that sends any of it to another service decides what to send. The operator controls what is sent by choosing the module with `[geo] module`. With the key unset or `none`, the module called uses none of the request's data and answers nothing.

**Responses that get no lookup.** On both Fastly steps, `geo_allowed_for_response` stops the finalize call for a 401 response, so no module is called a second time and the response carries `x-geo-info-available: false` in place of a location. The rule does not stop the call on the request path, and that call has run before the response exists. On Fastly, a request to `/_ts/api/v1/identify` with an invalid token and a request that the origin answers with a 401 have both already handed the module every request header, `Authorization` included, through the Edge Cookie state before the 401 exists.

### 2.8 Failure handling

A module answers with a location, with `Ok(None)` or with an error, and each caller handles the last two as the table in section 2.2 shows. `Ok(None)` means no location. An error means the lookup failed. The Edge Cookie state and the Didomi loader tell the two apart. The other callers treat both as no location.

The Edge Cookie state resolves permissions at the top node of the `permissions.yaml` rules tree when there is no location. It resolves every permission to the requires-signal floor when the lookup fails, and logs the failure at error level. The Didomi loader turns a missing location into the geo failure response `geo_unavailable`, a failed lookup into `lookup_failed`, and a location it cannot normalize into the reason the normalization gives.

- A call with no services reaches `PlatformGeoModule` and answers `Ok(None)`. It is not an error.
- `locate_request` always supplies the evidence, so a module called through it always finds the evidence. A call built by hand without evidence withholds it, and the test modules return `PlatformError::Geo` for that.
- Core adds no timeout, retry or cache around the call. Each caller calls once, waits for the answer and uses the answer, so a slow module delays the caller. On Fastly both finalize steps wait before the response headers are sent, so a slow module delays every response except a 401. A module that reaches a service applies its own timeout and holds its own answers.

### 2.9 Rollout for existing deployments

**Deployments.** No settings document and no stored data changes. A deployment on `none` or `platform` behaves as before, apart from the cost below, because the wrapped lookups read the same address and the built-in lookups ignore the added evidence.

**One difference.** On Fastly, a deployment whose `[geo] module` names a module from the registry now gets that module's location in the `x-geo-*` headers of every response the middleware finishes. Before, those responses carried `x-geo-info-available: false`. Those headers carry the module's city, coordinates, metro code and region as well as its country, and no permission guards them (section 2.7). Responses finished by the entry-point step are unchanged.

**One cost.** On Fastly, `FinalizeResponseMiddleware` copies the request's method, headers and URI before the handler runs, on every request and whatever `[geo] module` selects, and when the key is unset.

**Crates outside core** that use the items below stop compiling until they change, and `CHANGELOG.md` marks the change as breaking for them.

- `RuntimeServicesBuilder::geo`, `RuntimeServices::with_geo`, `IntegrationRegistrationBuilder::with_geo_module` and the Fastly adapter's `FinalizeResponseMiddleware::new` take an `Arc<dyn GeoModule>` where they took an `Arc<dyn PlatformGeo>`, and `build_geo_module` returns one.
- `RuntimeServices::geo()` returns `&dyn GeoModule`. A caller that called `lookup` on it calls `locate_request`.
- `IntegrationRegistry::geo_module()` returns an `Option<Arc<dyn GeoModule>>`, and the public field `IntegrationRegistration::geo_module` holds an `Option<(&'static str, Arc<dyn GeoModule>)>`, being the name `[geo] module` selects the module by and the module, where each had an `Arc<dyn PlatformGeo>` in place of the `Arc<dyn GeoModule>`.
- A crate that passed a `PlatformGeo` wraps it in `PlatformGeoModule::new(name, module)`. A crate that wants the request implements `GeoModule`.

In this repository the converted code is the six callers in section 2.2, the `ts` command line tool's local services in `crates/trusted-server-cli/src/commands/keys.rs`, the seam probe's registration, the example in the integration guide, and the test services in core, the Fastly adapter, the Axum adapter, the Prebid server crate, the 51Degrees loading cache crate and the Didomi crate.

**Open questions.** Questions 1 to 6 wait for a decision and for a code change. Items 7 to 12 are follow-ups that need an edit and no decision, each with the file and lines it has in `split/33-geo-call` at `6944f4e70`. This revision changes no code and no document outside this page.

1. Whether `FinalizeResponseMiddleware` reads the location that `EcFinalizeState` carries on the response and asks the module only for a response that carries none. Today a request that reads the Edge Cookie state asks the module a second time at the finalize step unless the response is a 401, and the second call carries the differences listed in section 2.5. Reading the carried location would also remove those differences for the responses that carry one.
2. Whether `apply_entry_point_finalize_headers` uses the module `finalize_geo_module` returns when application state was built, so that a response the router did not finish follows `[geo] module` as the middleware's responses do. The step would hand the module the request's view and no headers.
3. Whether the `x-geo-*` response headers are written only when the request has been granted the permissions the module declares, as the location handed to another module is. Today the headers carry the answer's city, coordinates, metro code and region whatever the module declares.
4. Whether the Didomi loader, the Fastly middleware and the Fastly entry-point step build the request's view the way `ModuleRequest::of` does, so that all six callers read the host and scheme by one rule.
5. Whether the middleware copies the request's method, headers and URI only when `[geo] module` selects a module from the registry, since the host's lookup and the module that resolves nothing read the address alone.
6. Whether the fix for the second fault, which changes the `x-geo-*` headers of a deployment's responses and which the issue for this change (IABTechLab/trusted-server#1321) does not ask for, is a pull request of its own.
7. `CHANGELOG.md`, line 12, says a geo module is called with the request's module call "as a device module is". Section 2.7 shows that the two calls carry different evidence, because `detect` is shown less of the request than a geo module is, so the entry should say only that the call has the same type.
8. `docs/guide/integration-guide.md`, line 416 and lines 432 to 433, say a geo module is called with the request "as a device module is", and the same correction applies.
9. The doc comment of `GeoModule` in `crates/trusted-server-core/src/platform/traits.rs`, lines 191 to 195, says no headers survive to the Fastly finalize paths and that both call the host's own lookup, never a module from the registry. Only the entry-point step does that. The middleware keeps the request's headers and calls the module `finalize_geo_module` returns, which can be the registry's (section 2.5).
10. The doc comment of `geo_allowed_for_response` in `crates/trusted-server-adapter-fastly/src/middleware.rs`, lines 178 to 180, says the function only decides, and leaves the lookup to its callers, because `PlatformGeo::lookup` is asynchronous, where both callers now call `locate_request`.
11. The doc comment of `GeoConfig` in `crates/trusted-server-core/src/settings.rs`, lines 1460 to 1462, says `[geo]` selects the module that resolves a client IP, where the module is now called with the request.
12. The `Geo / IP intelligence` row of the capability table in `AGENTS.md`, line 400, names `PlatformGeo` as the trait. `[geo] module` selects a `GeoModule`, and `PlatformGeoModule` wraps a `PlatformGeo`.

## 3. What is not in this change

- **A geo module in the stock list that reads more than the address.** The host lookups for Fastly, Axum, Cloudflare and Spin stay `PlatformGeo` and are wrapped. The only module this repository offers through the registry is the seam probe's, which wraps a lookup that answers a fixed country.
- **The Fastly entry-point step.** `apply_entry_point_finalize_headers` still builds its module with `build_geo_module` and passes no headers. When `[geo] module` names a registry module, a response that reaches that step carries `x-geo-info-available: false`.
- **Geo in the response headers of Axum, Cloudflare and Spin.** Their `x-geo-info-available` does not follow the module, as the table in section 2.5 shows.
- **Making core share one answer between the geo call and the device call.** A vendor crate whose one service answers both holds the answer itself and returns it from `locate` and from `detect`. The device call carries less evidence than the geo call (section 2.7), so a held answer can be keyed only on what both calls carry.
- **Checking permissions before the evidence is handed over.** A geo module is handed the evidence without a check, and so is a device module's `detect`, which is handed less (section 2.7).
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
- `crates/trusted-server-integration-tests/tests/documentation_snippets.rs` has `integration_guide_runtime_services_fixture_compiles`, which compiles the runtime services example in the integration guide, the example that wraps `FixtureGeo` in `PlatformGeoModule::new`.

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
12. A located answer reaching the Fastly response headers. Every finalize test in the Fastly adapter's `middleware.rs` and `lib.rs` has the module answer nothing, so none checks an `x-geo-*` header with a location in it. `proxy_route_reports_the_modules_geo_and_that_the_preparer_ran` passes through `FinalizeResponseMiddleware` with the registry's module selected, and checks the body and not the response's headers.
13. A request that a preparer changes, sent through the router with a geo module that reads its evidence. No test compares the evidence the request path's call carries with the evidence the middleware's call carries.

The pull request records the fork's CI green on all 19 checks at 1b8b43858. Its checklist leaves the manual check under `fastly compute serve` unticked.

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/33-geo-call` at `c5b04c263`. Issues found, three of them blocking. The three were the claim that a geo module is called with the request as a device module is, when a device module's `detect` is shown at most two headers and five of the six geo callers hand the module every header, the claim that the router path carries the same evidence as the request path, when a request preparer changes the request after the middleware has kept it, and the claim that no header is handed to a module for a response that did not authenticate, when the request path has already handed the module every header. This revision states what the code does in each case, retitles the page, and adds the services the finalize steps get, the response headers that no permission guards, the wait for the module, the missing breaking items and the missing tests, while the decisions the round raises wait under Open questions.
2. **Round 2, 2026-10-15.** A lighter review in a fresh session, checking the round 1 fixes against `split/33-geo-call` at `6944f4e70`. Issues found, none blocking. This revision corrects what the page says about why the entry-point step skips the registry, which responses carry `EcFinalizeState`, what the Fastly device module reads and the type of the registration field, names the permissions endpoint for Axum, Cloudflare and Spin, corrects the round 1 sentence on the code changes, and adds the changelog, guide and comment follow-ups to the open questions.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                    |
| ---------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-10-15 | First draft, written after the code.                                                                                                                                                                                                                                                                                                                                                      |
| 2026-10-15 | Fixes from review round 1. The title, what a device module and a geo module are each handed, the evidence the router path's call carries, the 401 rule, the services the finalize steps get, the response headers that no permission guards, the wait for the module, the breaking items, the missing tests, the open questions and the review log.                                       |
| 2026-10-15 | Corrections from review round 2. Why the entry-point step skips the registry, which responses carry `EcFinalizeState`, what the Fastly device module reads, the type of the registration field, the permissions endpoint in Axum, Cloudflare and Spin, the round 1 log sentence on the code changes, and the follow-ups for the changelog, the integration guide and four stale comments. |
