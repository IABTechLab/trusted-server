# Integration Development

Trusted Server integrations are platform-neutral registrations assembled by
`IntegrationRegistry`. Adapter crates provide I/O through
`RuntimeServices`; integration code must not import Fastly, Cloudflare,
Axum, or Spin SDK types.

## Choose the narrowest hook

- `IntegrationProxy` owns explicit method/path endpoints and receives an
  EdgeZero-neutral request and its call into the request's module context,
  from which it names the `Settings`, the `RuntimeServices` and whatever
  else it needs.
- `IntegrationRequestFilter` makes an early request decision.
- `Middleware` changes a page, on the pages where the operator's settings
  place it, and is the one way a module changes a page. See
  [Changing a page](#changing-a-page).

Build one `IntegrationRegistration` with only the hooks the feature needs.
Use `with_deferred_js()` only for a separately loaded integration module and
`without_js()` when another asset path owns delivery. Proxy routes should be
bounded, and sit beneath the integration's prefix unless the module serves an
endpoint of its own, as
[Serving an endpoint from a module](#serving-an-endpoint-from-a-module)
describes. Do not introduce a general outbound proxy.

## Compiling core-neutral fixture

The fixture below registers a middleware and constructs every
required `RuntimeServices` service without an adapter dependency. The
documentation test extracts this exact fence and compiles it as an isolated
crate.

<!-- documentation-snippet:runtime-services:start -->

```rust
use std::net::IpAddr;
use std::rc::Rc;
use std::sync::Arc;

use error_stack::Report;
use trusted_server_core::integrations::{
    AttributeRewriteAction, IntegrationRegistration,
};
use trusted_server_core::middleware::{
    AttributeRewrite, AttributeRewriteFn, Middleware, MiddlewareAction,
    MiddlewareContext, MiddlewarePhase,
};
use trusted_server_core::platform::{
    BackendNamingPolicy, ClientInfo, GeoInfo, PlatformBackend,
    PlatformBackendSpec, PlatformConfigStore, PlatformError, PlatformGeo, PlatformGeoModule,
    PlatformSecretStore, RuntimeServices, StoreId, StoreName,
    UnavailableHttpClient, UnavailableKvStore,
};

struct ReadOnlyStore;

impl PlatformConfigStore for ReadOnlyStore {
    fn get(
        &self,
        _store: &StoreName,
        _key: &str,
    ) -> Result<String, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn put(
        &self,
        _store: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn delete(
        &self,
        _store: &StoreId,
        _key: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

impl PlatformSecretStore for ReadOnlyStore {
    fn get_bytes(
        &self,
        _store: &StoreName,
        _key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn create(
        &self,
        _store: &StoreId,
        _key: &str,
        _value: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }

    fn delete(
        &self,
        _store: &StoreId,
        _key: &str,
    ) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::Unsupported))
    }
}

struct FixtureBackend;

impl PlatformBackend for FixtureBackend {
    fn naming_policy(&self) -> BackendNamingPolicy {
        BackendNamingPolicy::Axum
    }

    fn predict_name(
        &self,
        spec: &PlatformBackendSpec,
    ) -> Result<String, Report<PlatformError>> {
        if spec.host.is_empty() {
            Err(Report::new(PlatformError::Backend))
        } else {
            Ok("fixture-origin".to_owned())
        }
    }

    fn ensure(
        &self,
        spec: &PlatformBackendSpec,
    ) -> Result<String, Report<PlatformError>> {
        self.predict_name(spec)
    }
}

struct FixtureGeo;

#[async_trait::async_trait(?Send)]
impl PlatformGeo for FixtureGeo {
    async fn lookup(
        &self,
        _client_ip: Option<IpAddr>,
        _services: &RuntimeServices,
    ) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
    }
}

struct AssetAddress;

impl Middleware for AssetAddress {
    fn middleware_id(&self) -> &'static str {
        "testing.example"
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Fetch]
    }

    fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
        let decide: Rc<AttributeRewriteFn> = Rc::new(|matched| {
            matched
                .value
                .strip_prefix("https://assets.example/")
                .map(|path| AttributeRewriteAction::replace(format!("/assets/{path}")))
                .unwrap_or_else(AttributeRewriteAction::keep)
        });
        MiddlewareAction {
            element_handlers: AttributeRewrite::each(&["src", "href"], &decide),
            ..MiddlewareAction::pass()
        }
    }
}

pub fn registration() -> IntegrationRegistration {
    IntegrationRegistration::builder("example")
        .with_middleware(Arc::new(AssetAddress))
        .build()
}

pub fn runtime_services() -> RuntimeServices {
    let store = Arc::new(ReadOnlyStore);
    RuntimeServices::builder()
        .config_store(store.clone())
        .secret_store(store)
        .kv_store(Arc::new(UnavailableKvStore))
        .backend(Arc::new(FixtureBackend))
        .http_client(Arc::new(UnavailableHttpClient))
        .geo(Arc::new(PlatformGeoModule::new("fixture", Arc::new(FixtureGeo))))
        .client_info(ClientInfo::default())
        .build()
}
```

<!-- documentation-snippet:runtime-services:end -->

Production adapters replace every unavailable or fixture service with the
target implementation. The builder deliberately panics when a required service
is omitted, so adapter startup must construct the complete service graph.

### A middleware for a media type other than HTML

A middleware works on `text/html` unless it says otherwise, with
`handles_media_type`, and an entry naming it under a media type it does not
handle is refused when the registry is built. On a media type other than
HTML an action may carry a stream processor alone, which is handed the body
after core has moved the origin's addresses in it, chunk by chunk, with the
last chunk marked, and only for a `200`, whose body is the whole file. Head
markup and the element and text handlers are decisions about HTML, and an
action that makes one on another media type is refused by name when the
chain is planned.

A middleware named in an entry with a path, for a media type other than
HTML, also runs where the origin answers `404 Not Found` for that path
itself, on an empty document of the entry's media type, and what it writes
is the file. A middleware that adds to a file therefore writes the same on an
empty one, with nothing before its first line.

An entry's `path` is a prefix, so a middleware made for one file says which
path is the file with `handles_path`, and is left out everywhere else the
entry covers. One that writes the whole file, whatever the origin sent, says
so with `writes_whole_document`, and the file is then made in place of any
answer it would not be handed as a `200` of the entry's media type. One that
needs the origin's whole body says so with `needs_whole_body`, and the origin
is then asked without the reader's conditional and range headers. Core's
`robots_txt` middleware says all three.

```rust
struct Sellers;

impl Middleware for Sellers {
    fn middleware_id(&self) -> &'static str {
        "example.sellers"
    }

    fn handles_media_type(&self, media_type: &str) -> bool {
        media_type == "text/plain"
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Fetch]
    }

    fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
        MiddlewareAction {
            stream: Some(Box::new(AppendLines::new(["example-ssp.com, 1, DIRECT"]))),
            ..MiddlewareAction::pass()
        }
    }
}
```

The entry that places it names the media type and, where the change is for
one file, its path. See "Entries for a media type other than HTML" in the
[configuration guide](/guide/configuration#entries-for-a-media-type-other-than-html).

### A middleware that prepares, and one that refuses

A middleware whose decision needs the request's services, or has to wait on
the key-value store or the network, answers `prepare` with a future. Core
runs it in the request's handler, before the document is processed, as the
module the middleware belongs to and with the permissions it declares in
`required_permissions`. What the future answers is kept under the phase and
the middleware's name in the context's `prepared`, and `create`, which stays
synchronous, takes it from there as the type it prepared. A middleware that
prepares nothing leaves `prepare` at its default and is asked `create` as
before.

```rust
impl Middleware for Sellers {
    fn prepare<'a>(
        &'a self,
        call: ModuleCall<'a>,
        context: &'a MiddlewareContext<'a>,
    ) -> Option<Preparation<'a>> {
        Some(Box::pin(async move {
            let lines = self.lines_for(call, context.request_host).await?;
            Ok(Box::new(lines) as Box<dyn Any + Send>)
        }))
    }

    fn create(&self, context: &MiddlewareContext<'_>) -> MiddlewareAction {
        let lines: Vec<String> = context.take_prepared(MODULE).unwrap_or_default();
        MiddlewareAction {
            stream: Some(Box::new(AppendLines::new(lines))),
            ..MiddlewareAction::pass()
        }
    }
}
```

The future may answer a `Refusal` instead, with a status, an optional
`Retry-After` and a message. The request is then answered with that status
and the message as plain text, no document is processed, and nothing is
stored or served from the store, so a middleware that cannot make the
document it is responsible for fails closed rather than letting an empty or
unchanged one through. `Refusal::service_unavailable` is the one for a
source that cannot answer now. The preparations of one document run in the
order the entry names the middleware, and the first refusal ends them. Core
logs a refusal as a warning naming the middleware, and answers a status that
is not a client or server error with `500`.

On a document that may be stored the fetch phase's preparations are handed
the request's services with no client IP, TLS details or client signals, and
nothing else of the reader, because what they answer is stored with the
document. The serve phase is prepared with the reader's state on every
document, the one fetched for this reader as much as a copy read from the
store, so what a serve middleware finds prepared never depends on whether its
reader was the first. On a document that is not stored both phases are
prepared with the reader's state.

## Proxy implementation rules

An `IntegrationProxy::handle` implementation receives the request and its
`ModuleCall`. It hands a function of its own to `call.inject_with`, with the
request as that function's own argument, and names what else it needs as
parameters, such as `&Settings` and `&RuntimeServices`, which is the complete
runtime service graph. Register or predict backends through
`services.backend()`, send through `services.http_client()`, bound request and
response bodies, and return `Report<TrustedServerError>` with the route's
context. The registry strips internal identity headers before dispatch, and a
route must opt into any explicit forwarding behavior.

A route that names a value whose use needs a permission, such as the Edge
Cookie identifier, declares that permission in `required_permissions` and is
passed the value only on a request that grants it. Nothing gates the route
itself, so a route that must not run without a permission reads the
`&PermissionState` and refuses.

Streaming support is an adapter capability. Request
`PlatformHttpRequest::with_stream_response()` only when the caller has
checked `supports_streaming_responses()`; unsupported adapters must reject
the request instead of silently buffering it.

## Serving an endpoint from a module

A module can answer a path of its own, outside `/integrations/`, with nothing
added to core. `IntegrationProxy::routes()` may declare any path.
`IntegrationEndpoint::new` and `IntegrationEndpoint::get`, `post`, `put`,
`delete` and `patch` keep the path as written, while the proxy's own `get`,
`post`, `put`, `delete` and `patch` put `proxy_prefix()` in front of it. A
path ending in `/*` covers every path beneath it.

```rust
fn routes(&self) -> Vec<IntegrationEndpoint> {
    vec![
        // Beneath the prefix, at /integrations/example/status.
        self.get("/status"),
        // As written.
        IntegrationEndpoint::get("/example/status"),
        IntegrationEndpoint::new(Method::HEAD, "/example/status"),
    ]
}
```

A route answers only the method it declares. A `GET` route answers neither
`HEAD` nor `OPTIONS`, so a module that answers either declares a route for
that method too, and a request whose method has no module route on its path
is handled as if the module declared nothing there. Nothing in core, the
adapters or EdgeZero strips the body from an answer to `HEAD`, so the
handler of a `HEAD` route sends no body itself.

Two routes on one method and path are refused when the registry is built,
whether one module declares both or two modules declare one each. The
message names the method and the path, as in
`Integration route registration failed for GET /example/status`.

A module route is reached only when the adapter answers no route of its own
for that method and path. Every adapter answers `GET /_ts/config` itself, so
a module's `GET` route there never runs, while a `POST` of that path reaches
a module route as any other request does. The registry does not know the
adapter's routes, so it accepts a module route that can never run, and
nothing reports that it never runs.

The steps that run before a module route are the adapter's request cycle,
the same steps a request for one of the publisher's own pages goes through.
A step that answers a request, such as a request filter turning it back,
answers in place of the route.

## Browser and script guards

Add a browser module only when browser state is required. Immediate modules
join the hashed unified bundle; deferred and standalone delivery are explicit
registry decisions. Dynamic script interception must register with the shared
DOM insertion dispatcher, remain idempotent, and leave unmatched elements
untouched. See [Trusted Server JavaScript](/guide/tsjs) and
[GPT's guarded handoff](/guide/integrations/gpt#server-slot-handoff).

## Integrations that ship outside core

Every integration ships in a crate of its own, which a deployment composes in
at startup, and the hooks described above are what that crate implements.
The vendor owns the code, the release cycle and the integration's own rules,
and core never names the vendor. The crates in this repository sit under
`crates/<type>/<vendor>/`, and a vendor's crate can equally live in a
repository of its own.

`crates/testing/iabtl` is the worked example. It is a test fixture rather
than something to deploy, and it exercises every part of the seam from a
vendor crate's position. The round-trip tests in
`crates/trusted-server-adapter-axum/tests/seam_probe.rs` drive each part
through the Axum adapter, and the Fastly adapter carries the same round trip.

### What the crate provides

The crate hands out an `IntegrationBuilder`, which names the integration and
points at the functions that do the work.

| Part                                          | Purpose                                                                                                                                                                                                                                                              |
| --------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Id                                            | Names the integration in its browser module, in diagnostics and in the prefix `/integrations/<id>/` of its proxy routes, unless a route takes [a path of its own](#serving-an-endpoint-from-a-module)                                                                |
| Source                                        | The crate or package name, reported when two builders claim one id, so an operator can tell which crates collided                                                                                                                                                    |
| Build function                                | Reads `Settings` and returns the registration. It is called only when a section selects the module                                                                                                                                                                   |
| Validate function                             | The integration's own deploy rules. It runs for every builder, selected or not                                                                                                                                                                                       |
| `.with_module_name("<type>.<name>")`          | The name a section selects the module by, which is the crate's path under `crates/` with `.` between the parts. `module_name!()` derives it from the crate's folder                                                                                                  |
| `.with_request_preparer(...)`                 | Optional. Runs before routing, once for a request, whether or not the module is selected                                                                                                                                                                             |
| `.with_response_finalizer(...)`               | Optional. Finishes the response the page path returns, from what the module's request hooks left for the request                                                                                                                                                     |
| `.with_auction_token()`                       | Optional. Declares that the module's browser script reads the token an auction publishes with its winning bids, which is made only when a selected module reads one                                                                                                  |
| `.with_secret_settings(...)`                  | Optional. Lists the settings in the module's own table that hold the name of a secret, so each is looked up as the settings load                                                                                                                                     |
| `.with_middleware(...)`                       | Optional. Describes the page changes the registration supplies, each with the phase and the media type an entry names it for, so `ts audit` places them without reading the module. The registry refuses a registration whose middleware contradicts the description |
| `.with_plan_registration(...)`                | Optional. Registers the module from the compiled auction plan, for a module whose page support follows what the plan selects. It runs whether or not a section selects the module, and its hooks go first                                                            |
| `.with_plan_validator(...)`                   | Optional. Checks the module's settings against the compiled auction plan, when a deployment is validated and as the settings load                                                                                                                                    |
| `.with_demand(...)` and `.with_adserver(...)` | Optional. Registers an auction implementation that `[demand]` or `[ad-server]` can name. A crate that supplies nothing else starts from `IntegrationBuilder::implementations(...)`                                                                                   |
| `.with_loading_cache(...)`                    | Optional. Offers the loading cache the crate supplies, which a feature selects by the module's name in its `[cache.<name>]` table. An adapter takes the selections with `loading_caches_from`                                                                        |
| `.with_cache_features(...)`                   | Optional. Declares the caches the module loads through, by name, each selected in its own `[cache.<name>]` table as `[cache.robots-txt]` is. A name core or another builder declares is refused                                                                      |
| `.with_shared_settings(...)`                  | Optional. Runs the builder when its vendor's `[shared.<vendor>]` table is present, for a vendor whose modules all read that one table. A builder the table runs is selected by no section                                                                            |

```rust
pub fn module_name() -> &'static str {
    trusted_server_core::module_name!()
}

pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::new(EXAMPLE_ID, EXAMPLE_SOURCE, register, validate)
        .with_module_name(module_name())
        .with_request_preparer(prepare_request)
}
```

A crate at `crates/cmp/example` is the module `cmp.example`, so a deployment
runs it with `[cmp] module = "example"` and gives it settings in
`[cmp.example]`.

### Who maintains the crate

Every crate outside core names its maintainers in its manifest, the way
Prebid.js requires a named maintainer of every adapter.

```toml
[package.metadata.maintainers]
owner = "Example Vendor"
status = "vendor owned"
```

`status` is `vendor owned` once the vendor has adopted the crate,
`seeking vendor owner` while the Trusted Server maintainers hold it for a
vendor, and `project owned` for a crate that is the project's own, such as a
test module. A test of the Axum adapter fails for a crate in this repository
that leaves the declaration out.

### What a registration can declare

The build function returns an `IntegrationRegistration`, built with the
builder every integration uses.

| Declaration                                           | What it does                                                                                                    |
| ----------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| `.with_proxy(...)`                                    | Routes the paths the proxy declares                                                                             |
| `.with_request_filter(...)`                           | Inspects a request and can turn it back before it reaches the origin                                            |
| `.with_identifier_module(...)`                        | Hands core the identifiers the module learned, asked at each Edge Cookie read when `[ec] module` names a module |
| `.with_middleware(...)`                               | Offers a page change that an entry may place by the middleware's name                                           |
| `.with_bundle_tag_attribute(name, value)`             | Puts an attribute on the script bundle's tag, for a browser module that reads a setting from it                 |
| `.with_js_module(CarriedJsModule { source, sha256 })` | Carries the integration's own browser script, built outside `trusted-server-js`                                 |
| `.with_deferred_js()`                                 | Serves the script as its own `<script defer>` tag instead of in the main bundle                                 |
| `.with_standalone_js()`                               | Serves the script only on its own path, for an integration that injects its own tag                             |
| `.without_js()`                                       | Ships no browser script                                                                                         |
| `.with_ec_module(name, ...)`                          | Offers an Edge Cookie module that `[ec] module` may select by that name                                         |
| `.with_geo_module(name, ...)`                         | Offers a geo module, called with the request as a device module is, that `[geo] module` may select by that name |
| `.with_device_module(name, ...)`                      | Offers a device module that `[device] module` may select by that name                                           |
| `.with_prefix_module(name, ...)`                      | Offers a prefix module that `[prefix] module` may select by that name                                           |
| `.with_robots_txt_contributor(name, ...)`             | Offers rules for `/robots.txt` that `[robots-txt] modules` may select by that name                              |
| `.with_asset_cache_module(name, ...)`                 | Offers an asset cache module, asked for a file before its origin, when `[asset-cache]` selects the builder      |

The three script delivery choices are exclusive and the last call wins.

Each of the last four takes the module's name, which is the path under
`crates/` of the crate the module lives in, as `module_name!()` gives it.
`[ec] module`, `[geo] module` and `[device] module` read a written name the
way a section does, as written or with the type folder (`edgecookie`, `geo`
or `device`) in front, so a geo module from `crates/geo/example` is selected
with `[geo] module = "example"`. One registration can supply a module of each
type, each under the name of its own crate, which is how a vendor makes one
backend call serve all three.

A geo module is a `GeoModule`, called with the request's module call as a
device module is, so it sees the request's evidence, the settings and the
services, and a module whose one service answers location and device from
the same request can answer both from one call. A module that resolves from
the client address alone implements `PlatformGeo` and is offered through
`PlatformGeoModule::new(name, module)`, which hands the platform lookup the
address the call carries, as the example above does.

An Edge Cookie module is declared under the name its own `id` returns.
Startup refuses a registration where the two differ, and two registrations
that supply a module of one type under one name.

A robots.txt contributor implements `RobotsTxtContributor`. It returns groups
of records and never bytes, so it cannot produce a malformed file. Trusted
Server asks it for the request without its query, on the host the request was
addressed to and with no client information or host signals in its services,
holds the answer for that host until the contributor's `refresh` is due, and
writes the file from every contributor `[robots-txt] modules` names, in that
order. `fingerprint` names the settings an answer was given for, so an answer
held for other settings is never served. Trusted Server hashes it into the key
the answer is held under, so nothing of it is written to the store.

The Fastly adapter is the one adapter that classifies a request, so a device
module is asked there and on no other adapter. It is shown the User-Agent and
the request's cookies.

The registry builds only the modules a section selects, so the module that
supplies one of these has to be selected in its section as well. The probe
is run with `[testing] modules = ["iabtl"]` and its geo module is
selected with `[geo] module = "testing.iabtl"`. A `[geo] module` or
`[device] module` naming something no running module supplies refuses
startup. The message lists the modules of that type the deployment runs, and
says when the name is a module no section selects or one that supplies no
module of that type.

### Changing a page

A module changes a page through a middleware, which the operator's settings
place. The module registers each one with `.with_middleware(...)`, and it
runs only on the pages where an entry names it, in the order that entry
gives. See [Placing page changes](/guide/configuration#placing-page-changes).

The fixture below loads a vendor's tag from a first-party path and marks the
head. The documentation test extracts this exact fence and compiles it as an
isolated crate.

<!-- documentation-snippet:middleware:start -->

```rust
use std::rc::Rc;
use std::sync::Arc;

use trusted_server_core::integrations::{AttributeRewriteAction, IntegrationRegistration};
use trusted_server_core::middleware::{
    AttributeRewrite, Middleware, MiddlewareAction, MiddlewareContext, MiddlewarePhase,
};

/// The tag the vendor serves, and the first-party path it is loaded from
/// instead.
const VENDOR_TAG: &str = "https://cdn.vendor.example/tag.js";
const FIRST_PARTY_TAG: &str = "/integrations/example_tag/tag.js";

/// Loads the vendor's tag from a first-party path, and says so in the head.
pub struct ExampleTag;

impl Middleware for ExampleTag {
    fn middleware_id(&self) -> &'static str {
        "tag.example"
    }

    fn phases(&self) -> &[MiddlewarePhase] {
        &[MiddlewarePhase::Fetch]
    }

    fn create(&self, _context: &MiddlewareContext<'_>) -> MiddlewareAction {
        MiddlewareAction {
            head_inserts: vec!["<meta name=\"example-tag\" content=\"first-party\">".to_owned()],
            element_handlers: vec![Box::new(AttributeRewrite::matching(
                "script[src]",
                "src",
                Rc::new(|matched| {
                    if matched.value == VENDOR_TAG {
                        AttributeRewriteAction::replace(FIRST_PARTY_TAG)
                    } else {
                        AttributeRewriteAction::keep()
                    }
                }),
            ))],
            ..MiddlewareAction::pass()
        }
    }
}

/// What the module's build function returns.
pub fn registration() -> IntegrationRegistration {
    IntegrationRegistration::builder("example_tag")
        .without_js()
        .with_middleware(Arc::new(ExampleTag))
        .build()
}
```

<!-- documentation-snippet:middleware:end -->

A middleware says which phase it runs in, and an entry of the other phase
that names it refuses startup.

| Phase                    | Runs                                                                               | `document_state` holds                      |
| ------------------------ | ---------------------------------------------------------------------------------- | ------------------------------------------- |
| `MiddlewarePhase::Fetch` | On the page as the origin sent it. What it leaves is what a shared template stores | Nothing a request left                      |
| `MiddlewarePhase::Serve` | On each reader's copy, whether the page came from the store or from the origin     | What the module's request hooks left for it |

A stored page is keyed by the settings and core's build, so a fetch
middleware that writes markup or an address compiled into its own crate
returns that text from `template_key`, and a change to it stores pages
again.

A middleware is asked once for each document, by `create`, and what it
answers is used for that document alone, so a handler may hold state between
the chunks of one script. The answer is a `MiddlewareAction`, which may set
any of five things.

| Field                  | What it does                                                                                                              |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| `head_inserts`         | Writes markup in `<head>`, ahead of the script bundle                                                                     |
| `after_bundle_inserts` | Writes markup straight after the script bundle, for a script that needs the bundle to have run                            |
| `body_open_inserts`    | Writes markup immediately after the opening body tag, once per document, for an installation whose second part goes there |
| `element_handlers`     | For the elements a CSS selector matches, keeps the element, replaces one attribute's value or removes it                  |
| `text_handlers`        | For the text inside the elements a CSS selector matches, keeps a chunk, replaces it or removes it                         |
| `stream`               | Runs a stream processor over the document the handlers left                                                               |

A middleware can rely on these rules.

- The middleware of one entry run in the order the entry names them. Two
  handlers that match one element, or one script, are both asked, and each is
  handed what the one before it left.
- An element handler is asked after core has moved the origin's address in
  the attribute to the reader's host, and an element an earlier handler
  removed is not asked about again.
- A script's text arrives in chunks. A handler that needs the whole script
  removes each chunk while keeping a copy, and writes back what it kept with
  the last chunk. A replacement is written as it is, except that a `<` that
  would end the script, or begin `<script` after `<!--`, is written as
  `\u003c`.
- A fetch middleware is told nothing about the reader, because what it leaves
  is what a shared template stores. `MiddlewareContext::document_state` is
  shared by the middleware working on one document in one phase.
- A serve middleware works on the page as core and the fetch middleware left
  it, the script bundle's tag included. What a request preparer or a request
  filter left with `IntegrationRequestState::insert` is in its
  `document_state`, and a request that carries any is never answered from a
  shared template. It is told the same origin host for a page from the store
  as for one fetched for that request.
- A selector that does not parse fails the response with a `500` and a
  message naming the middleware. A middleware chooses its selectors for each
  document, so they cannot be checked when the service starts.

A middleware's name is the module's own name, as `module_name!()` gives it,
with a part of its own after it when the module supplies several, as
`module_name!("cleanup")` gives `tag.example.cleanup`. Startup refuses two
registrations that supply one name, and a name an entry could not write. A
middleware reads its settings from the module's own table, which the build
function is handed.

### What a module is handed

Core builds one module context for a request and hands each module it calls
a `ModuleCall`, which is that context together with the permissions the
module declares. The module hands a function of its own to `call.inject`,
and that function names what it needs as parameters. Each is passed in from
the context, which borrows what the request already holds.

```rust
impl ExampleProxy {
    /// Answers with the country the request came from, when the location
    /// may be passed to this route.
    async fn route(
        &self,
        _req: Request<EdgeBody>,
        settings: &Settings,
        geo: Option<&GeoInfo>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        let country = geo.map_or("unknown", |geo| geo.country.as_str());
        let body = format!("{} {country}", settings.publisher.domain);
        Ok(Response::new(EdgeBody::from(body)))
    }
}

#[async_trait(?Send)]
impl IntegrationProxy for ExampleProxy {
    fn integration_name(&self) -> &'static str {
        "example"
    }

    fn routes(&self) -> Vec<IntegrationEndpoint> {
        vec![self.get("/country")]
    }

    async fn handle(
        &self,
        call: ModuleCall<'_>,
        req: Request<EdgeBody>,
    ) -> Result<Response<EdgeBody>, Report<TrustedServerError>> {
        call.inject_with(self, req, Self::route)?.await
    }
}
```

The values a function may name, and where each is absent, are listed in the
documentation of `trusted_server_core::module_context`. A parameter may be an
`Option` of one of them, which is `None` where the value is absent or
withheld, or a tuple of several. A function that names a value itself is not
called when the value cannot be passed, and the reason goes to the debug log.
The module decides what that means. A device module answers the unknown
device, an Edge Cookie module creates no identifier, and a route's `?` answers
`403` for a value the request's permissions withhold and `500` for one absent
where the route runs. `call.inject_with` passes one argument of the caller's
own after the module, which is how a route takes its request and how an Edge
Cookie module takes the value a page posted.

The location, the device signals and the Edge Cookie identifier each carry
the permissions that the module which produced them declares. One of them is
passed only to a module that declares every one of those permissions in its
`required_permissions` and is granted them on this request. A value whose
producer declares no permission is passed to every module.

What a context carries depends on where the module is called.

| Module            | Called through                                         | What its context carries                                                                                                                                                                                                                                                                                                                                                                                                      |
| ----------------- | ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Device            | `DeviceModule::detect`                                 | The request, the evidence, the settings and the services. No permission is resolved yet, so nothing whose use needs one                                                                                                                                                                                                                                                                                                       |
| Device            | `DeviceModule::advertising_attributes`                 | The request, the evidence, the permissions, the settings and the services, for what a bidder is told about the device, being its type, make, model, operating system and screen. The default answers `None`, as a module does when the reader has not given a permission its use needs or it resolves nothing. A backend it registers waits no longer than the auction's timeout, and core drops a value a bidder cannot read |
| Permission signal | `SignalInput::call`, `valid_signal` and `tdls`         | The request, the evidence, the connection, the consent, the settings and the services, and the location when its use needs no permission. The permissions are what the run produces                                                                                                                                                                                                                                           |
| Edge Cookie       | `EdgeCookieModule::generate` and `resolve_from_client` | Every row of the table above                                                                                                                                                                                                                                                                                                                                                                                                  |
| Route             | `IntegrationProxy::handle`                             | Every row of the table above except the evidence, because the route holds the request itself                                                                                                                                                                                                                                                                                                                                  |

A request preparer and a response finalizer are plain functions on the
builder and are not handed the context. A preparer changes the request before
anything about it is resolved, and a finalizer is handed only what its own
module left for the request.

### Acting on one request

A module that decides something about one request and acts on it later
carries the decision in the request's `IntegrationRequestState`. A request
preparer or a request filter leaves a value under the integration's id.

```rust
IntegrationRequestState::insert(request, EXAMPLE_ID, ExampleDecision::default());
```

Where the request's reader is served an HTML document, each value is copied
into the state the `[[serve]]` entry's middleware share, so the module's
serve middleware reads it with
`context.document_state.get::<ExampleDecision>(EXAMPLE_ID)`. A fetch
middleware is handed none of it. The same values are handed to the response
finalizer the builder declared, which runs on the response the page path
returns.

A request that carries any value gets a document made for it alone. The
document is fetched from the origin, never read from a shared template and
never stored as one, and an HTML response with a body is sent
`private, no-store`. So a module leaves a value only for a request it will
act on, and an ordinary request stays on the shared path.

A serve middleware has three places to write markup. `head_inserts` writes
straight before the script bundle, `after_bundle_inserts` writes straight
after it, for a script that needs the bundle to have run and has to run
before the page's own scripts, and `body_open_inserts` writes immediately
after the opening body tag, once per document, for an installation whose
second part goes there and nowhere else.

### A module's own secrets

A setting that holds a secret holds the name of its key in the configuration
a deployment pushes, and the secret itself once the settings have loaded. A
module says on its builder which settings in its own table are of that kind,
and when its table puts each to use.

```rust
const SECRETS: &[ModuleSecretSetting] = &[ModuleSecretSetting {
    path: &["api_key_secret_name"],
    in_use: calls_the_api,
}];

fn calls_the_api(table: &serde_json::Map<String, serde_json::Value>) -> bool {
    table.get("call_api").and_then(serde_json::Value::as_bool) == Some(true)
}

pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::new(EXAMPLE_ID, EXAMPLE_SOURCE, register, validate)
        .with_module_name(module_name())
        .with_secret_settings(SECRETS)
}
```

A setting in use is looked up in the default secret store as the settings
load, and deploy validation refuses a configuration in which it names no key.
A setting not in use is cleared and never looked up, and so is every setting
of a module no section selects, so a key that does not exist yet cannot stop
a deployment that does not use it. The load finds the declarations through
the builders it is given, which is one more reason a deployment loads its
settings with the builders it builds its state with.

### How an adapter composes it in

The Axum, Cloudflare and Spin adapters take the builders as an argument, so no
adapter names a vendor.

```rust
let router = TrustedServerApp::routes_with_registrations(
    settings,
    &[example_integration::builder()],
)?;
```

`build_state_with_registrations` takes the same list and returns the
application state, for a host that builds its own router around it. Two
builders claiming one id are refused at startup with a message naming the id
and both sources.

The settings are validated as they load, which is before any state is built,
so a deployment loads them with the same builders. A `[demand]` or
`[ad-server]` name that one of the builders supplies is otherwise refused by
the load as an implementation the build does not have, and each builder's
validate function runs as part of the load.

```rust
let builders = [example_integration::builder()];
let settings = get_settings_from_config_store_with(
    &config_store,
    &secret_store,
    &store_name,
    &config_key,
    &default_secret_store_name(),
    &builders,
)?;
let router = TrustedServerApp::routes_with_registrations(settings, &builders)?;
```

`settings_from_config_blob_with` does the same for a host that reads the
configuration envelope itself, and `validate_settings_for_deploy_with` is the
deploy check with the same builders.

The Fastly adapter is a library with a thin binary over it, so a Fastly
deployment that ships a vendor crate has a binary of its own. `run_with`
records the builders before any request is served, and they are handed to
the settings load and composed with the modules a stock build ships when the
application state is built.

```rust
fn main() {
    trusted_server_adapter_fastly::run_with(vec![example_integration::builder()]);
}
```

The round-trip tests in
`crates/trusted-server-adapter-fastly/src/app/seam_probe_tests.rs` drive the
probe through the Fastly adapter under Viceroy.

### The modules an adapter links

Each adapter lists the module builders it links in its own `app.rs`, in the
order their request hooks run, the way it lists the permission signal schemes
it links. It builds its state with that list followed by the builders a
deployment added, and loads its settings against the same list. The `ts` tool
lists the builders it links the same way, for deploy validation and for the
configuration `ts audit` writes. A module joins a build by adding its crate
and its builder to the list of each adapter that links it, in the place its
request hooks should run, and a deployment that wants a different set links a
different set. The order of the list does not order page changes, which run
in the order an entry names them. Offering a module does not run it, because
the registry builds a module only when a section of the settings selects it.

A module describes the page changes it supplies on its builder, with
`.with_middleware(...)`, each as a `SuppliedMiddleware` naming the middleware,
the phase an entry names it in and the media type the entry covers. A
middleware that runs in either phase is described in the one a generated
configuration places it in. `ts audit` asks
`trusted_server_core::integrations::supplied_middleware` for what the builders
it links describe, in the order the registry registers them, and writes the
entries from that, so a module's page changes are placed without a table
anywhere else. The registry refuses a registration whose middleware
contradicts the description.

```rust
const SUPPLIED_MIDDLEWARE: &[SuppliedMiddleware] = &[
    SuppliedMiddleware::on_html(MODULE, MiddlewarePhase::Fetch),
    SuppliedMiddleware::on_html(TAG_MIDDLEWARE, MiddlewarePhase::Serve),
];

pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::new(ID, env!("CARGO_PKG_NAME"), register, validate)
        .with_module_name(MODULE)
        .with_middleware(SUPPLIED_MIDDLEWARE)
}
```

### Giving the deployment its prefixes

A crate may give the start of every name the browser holds for a publisher,
being cookies and first-party storage keys, and of every key the publisher's
entries take in a store publishers share, by declaring a `PrefixModule` with
`.with_prefix_module(name, ...)`. `[prefix] module` selects it by that name,
and core's own module, which gives `ts` and `ts.<publisher.domain>`, runs when
nothing is selected. The module is handed a module call carrying the request
and the settings, and nothing else, and the registry asks it once when it is
built, so a module naming a value its call never carries is refused at startup.

Every request's services carry what the module gives, as
`services.public_prefix()` and `services.server_key_prefix()`. A crate names a
cookie with `public_prefix().cookie_name(COOKIE_EC)` rather than writing
`ts-ec`, keys a shared store with the server key prefix, and reads the prefix
in the browser from the bundle tag's `data-ts-public-prefix` attribute rather
than holding a copy. A public prefix's parts are letters, digits, `_`, `-` and
`.`, and the first starts with a letter or a single `_`, so every form is a
valid cookie name and no cookie name starts with `__`.

### Handing core the identifiers a request carries

A crate whose page script persists identifiers in the browser, or that learns
them for a request another way, hands them to core by declaring an
`IdentifierModule` with `.with_identifier_module(...)`. No section selects
one. Core asks every running registration's module once per request, when it
reads the Edge Cookie state and `[ec] module` names a module, through a module
call carrying the request's evidence, settings, services, consent, client
information and resolved permissions, and its geo when the location was found,
and the module names what it reads with `call.inject`. What it returns is a
list of OpenRTB-style `Eid` entries, in the order it offers them, so that
where two carry the same partner the later one is the candidate.

Core syncs what every module learned to the identity graph when it finalizes
the response, after the request body's own EIDs, and forwards it in the
auction when the body carries none. The cookie a module reads is the module's
own, named through `services.public_prefix().cookie_name(...)`, and core
names it nowhere. The Prebid module, `crates/auction/prebid`, is the module
that reads the `ts-eids` cookie its script writes.

### A vendor's shared table

What every module of one vendor needs, such as an endpoint and a license key,
goes in one `[shared.<vendor>]` table. Core reads none of it. The vendor's
crates read it as their own typed settings with
`Settings::shared_config::<T>(vendor)`, which refuses a table that cannot be
read as `T` or fails its validation, naming the table and the setting at fault
and never the setting's value. The vendor's builder marks itself with
`.with_shared_settings(vendor)`, and the registry builds it when the table is
present and refuses a table that no builder reads. A secret in the table is
declared with `.with_secret_settings(...)` at its path inside the table, as a
module's own secrets are, is checked at push to name a key, and is resolved
from the secret store when the table is present.

### Supplying a loading cache

Some answers cost a call to make and are the same for many requests, such as
each robots.txt module's answer. Core loads such an answer through
`PlatformLoadingCache`, in `trusted_server_core::platform`, which loads a
missing value once however many requests ask for it at the same moment, gives
every request waiting on that load the value or the error, and holds nothing
that failed. Core declares the interface and `NoLoadingCache`, which holds
nothing and runs every load, and the implementation comes from a crate outside
core.

A crate supplies one by implementing `PlatformLoadingCache` and offering a
factory on its builder with `.with_loading_cache(...)`. The offer is not a
selection. Each feature that holds answers selects the cache that holds them
by naming the module in its own `[cache.<name>]` table, so a crate that
supplies nothing else starts from `IntegrationBuilder::implementations(...)`
and is named by its id. An adapter takes the selections from the settings and
the builders it is given with `loading_caches_from` when it builds its state,
which refuses a name no builder offers a cache from, and hands them to every
request's services as `LoadingCaches`. A feature whose table names no module
takes the one loading cache module the build links, and loads through
`NoLoadingCache` when the build links none or several. No stock list carries a
loading cache module, so a stock build loads every value through
`NoLoadingCache`. A deployment that wants copies kept hands a loading cache
module's builder to its adapter, through `run_with` on Fastly or
`routes_with_registrations` on Axum, Cloudflare and Spin, as it does for any
crate of its own. `crates/loading-cache/51degrees` is one such module.

```rust
pub fn builder() -> IntegrationBuilder {
    IntegrationBuilder::implementations(MODULE, env!("CARGO_PKG_NAME"))
        .with_loading_cache(factory)
}

pub fn factory() -> Arc<dyn PlatformLoadingCache> {
    Arc::new(ExampleCaches::new())
}
```

An implementation keeps its copies in the stores `RuntimeServices` hands each
call, being the host's value cache from `services.value_cache()` where the
host supplies one and its key-value store otherwise, under the `StoreKey` the
caller made, so a copy is shared by every instance that reads the same store.

Code in core, or in a module, loads through the cache with
`RuntimeServices::load_cached`. It names its cache with a `CacheSpec`, being
the cache's name, the lifetimes of the memory and store copies and how many
values the memory copy holds. Its key is a `StoreKey`, which starts with the
publisher's `ServerKeyPrefix` from `services.server_key_prefix()`, so two
publishers sharing a store never share a copy. The `Loader` it hands over
owns everything it uses, and answers a `LoadedValue`, with a lifetime of its
own where the source gives one. Its value serves every reader, so any
services it needs come from `services.clone().without_connection()`, never
the request's own, and `load_cached` hands the cache the same.

```rust
let key = StoreKey::new(services.server_key_prefix(), CACHE, digest);
let bytes = services
    .load_cached(
        &cache_spec(settings),
        &key,
        Box::new(move || {
            Box::pin(async move {
                let answer = ask(&owned)
                    .await
                    .map_err(|error| LoadError::new(error.to_string()))?;
                Ok(LoadedValue::new(answer).with_lifetime(Duration::from_secs(3600)))
            })
        }),
    )
    .await?;
```

A caller whose load pays off only when its value is kept asks
`services.loading_cache_for(name).keeps_values()` first, which the cache of a build
that links none answers with false, and `remove` drops a copy that turned out
to be unusable. The lifetimes come from the settings, as `[cache.robots-txt]`
does through `LoadingCacheConfig::lifetimes`, so an operator tunes them
without a code change.

### Answering for a file before its origin

A crate may answer for a file that is not a page, whether the publisher's
origin sends it or an asset route fetches it, by declaring an
`AssetCacheModule`, in `trusted_server_core::asset_cache`, with
`.with_asset_cache_module(name, ...)`. Core offers each `GET` for such a file
to the module `[asset-cache]` selects, at the point it is about to ask the
file's origin, as an `AssetFile` that carries what core knows of the reader's
request and the `OriginRequest` core would send. A `GET` that describes a
body, by `Content-Length` or `Transfer-Encoding`, is not offered, because the
module sends the request with no body. The module answers `Serve` with a
response, which core processes as it would the origin's, and whose body is
whole where the adapter does not stream responses, or `OriginFailed` when its
own request to the origin failed, which core answers as a failed origin
request, or `AskOrigin`, after which core asks the origin itself.

The registry takes the module only from the registration of the builder
`[asset-cache]` selects. It refuses one from any other builder, and one in
what any builder registers for the auction plan, which runs whether or not a
section selects the builder. It refuses an `[asset-cache]` that selects more
than one module, and one that selects a builder that supplies no asset cache
module, naming that builder. A deployment that selects none has every file
asked of its origin. A module's tests make an `AssetFile` with
`AssetFile::for_test` and an `OriginRequest` with `OriginRequest::for_test`,
both behind core's `test-utils` feature.

### Two traps a vendor will hit

**The carried script's hash literal must match the file's bytes.** A
registration that carries a browser script states the script's SHA-256 next to
it. The registry hashes the source when it is built and refuses to start on a
disagreement, so a stale literal is a startup error rather than a stale script
reaching browsers. The usual cause is line endings rewritten on checkout,
because a Windows clone with `core.autocrlf` on rewrites the script's newlines
and the hash moves with them. The probe crate ships a `.gitattributes` marking
its script `text eol=lf` and a unit test comparing the literal with the file's
bytes, so the failure names the cause. Copy both into a vendor crate.

**A module a deployment adds is not known to the stock CLI.** `ts config
validate` and `ts config push` validate against the modules a stock build
ships, so each of those modules' own rules runs there. A module from a crate
the deployment added is not on that list. Its validate function runs when the
deployment loads its settings with that module's builder, and when something
calls `validate_settings_for_deploy_with` with it, which means the
deployment's own code or its tests. An operator can therefore push a
configuration that module rejects when the server starts. Run that module's
validation from the deployment's own build or test step, and do not read a
clean `ts config validate` as that module having agreed.

## Registration checklist

1. Add typed settings with validation. An integration runs only when a
   section selects its module, so it takes no enabled flag.
2. Register the exact capability predicates and route methods.
3. Add source and behavior parity records.
4. Add positive, negative, body-bound, header, and adapter-capability tests.
5. Document configuration, failure behavior, and runtime limitations.
6. Name the crate's maintainers in its manifest.
7. Run the target aliases from [Testing](/guide/testing).
