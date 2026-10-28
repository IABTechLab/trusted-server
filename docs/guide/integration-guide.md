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
    PlatformBackendSpec, PlatformConfigStore, PlatformError, PlatformGeo,
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
        .geo(Arc::new(FixtureGeo))
        .client_info(ClientInfo::default())
        .build()
}
```

<!-- documentation-snippet:runtime-services:end -->

Production adapters replace every unavailable or fixture service with the
target implementation. The builder deliberately panics when a required service
is omitted, so adapter startup must construct the complete service graph.

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

`crates/testing/seam-probe` is the worked example. It is a test fixture rather
than something to deploy, and it exercises every part of the seam from a
vendor crate's position. The round-trip tests in
`crates/trusted-server-adapter-axum/tests/seam_probe.rs` drive each part
through the Axum adapter, and the Fastly adapter carries the same round trip.

### What the crate provides

The crate hands out an `IntegrationBuilder`, which names the integration and
points at the functions that do the work.

| Part                                          | Purpose                                                                                                                                                                                                   |
| --------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Id                                            | Names the integration in its browser module, in diagnostics and in the prefix `/integrations/<id>/` of its proxy routes, unless a route takes [a path of its own](#serving-an-endpoint-from-a-module)     |
| Source                                        | The crate or package name, reported when two builders claim one id, so an operator can tell which crates collided                                                                                         |
| Build function                                | Reads `Settings` and returns the registration. It is called only when a section selects the module                                                                                                        |
| Validate function                             | The integration's own deploy rules. It runs for every builder, selected or not                                                                                                                            |
| `.with_module_name("<type>.<name>")`          | The name a section selects the module by, which is the crate's path under `crates/` with `.` between the parts. `module_name!()` derives it from the crate's folder                                       |
| `.with_request_preparer(...)`                 | Optional. Runs before routing, once for a request, whether or not the module is selected                                                                                                                  |
| `.with_response_finalizer(...)`               | Optional. Finishes the response the page path returns, from what the module's request hooks left for the request                                                                                          |
| `.with_auction_token()`                       | Optional. Declares that the module's browser script reads the token an auction publishes with its winning bids, which is made only when a selected module reads one                                       |
| `.with_secret_settings(...)`                  | Optional. Lists the settings in the module's own table that hold the name of a secret, so each is looked up as the settings load                                                                          |
| `.with_plan_registration(...)`                | Optional. Registers the module from the compiled auction plan, for a module whose page support follows what the plan selects. It runs whether or not a section selects the module, and its hooks go first |
| `.with_plan_validator(...)`                   | Optional. Checks the module's settings against the compiled auction plan, when a deployment is validated and as the settings load                                                                         |
| `.with_demand(...)` and `.with_adserver(...)` | Optional. Registers an auction implementation that `[demand]` or `[ad-server]` can name. A crate that supplies nothing else starts from `IntegrationBuilder::implementations(...)`                        |

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
test module. A test in `crates/trusted-server-modules` fails for a crate in
this repository that leaves the declaration out.

### What a registration can declare

The build function returns an `IntegrationRegistration`, built with the
builder every integration uses.

| Declaration                                           | What it does                                                                                    |
| ----------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| `.with_proxy(...)`                                    | Routes the paths the proxy declares                                                             |
| `.with_request_filter(...)`                           | Inspects a request and can turn it back before it reaches the origin                            |
| `.with_middleware(...)`                               | Offers a page change that an entry may place by the middleware's name                           |
| `.with_bundle_tag_attribute(name, value)`             | Puts an attribute on the script bundle's tag, for a browser module that reads a setting from it |
| `.with_js_module(CarriedJsModule { source, sha256 })` | Carries the integration's own browser script, built outside `trusted-server-js`                 |
| `.with_deferred_js()`                                 | Serves the script as its own `<script defer>` tag instead of in the main bundle                 |
| `.with_standalone_js()`                               | Serves the script only on its own path, for an integration that injects its own tag             |
| `.without_js()`                                       | Ships no browser script                                                                         |
| `.with_ec_module(name, ...)`                          | Offers an Edge Cookie module that `[ec] module` may select by that name                         |
| `.with_geo_module(name, ...)`                         | Offers a geo module that `[geo] module` may select by that name                                 |
| `.with_device_module(name, ...)`                      | Offers a device module that `[device] module` may select by that name                           |
| `.with_robots_txt_contributor(name, ...)`             | Offers rules for `/robots.txt` that `[robots-txt] modules` may select by that name              |

The three script delivery choices are exclusive and the last call wins.

Each of the last four takes the module's name, which is the path under
`crates/` of the crate the module lives in, as `module_name!()` gives it.
`[ec] module`, `[geo] module` and `[device] module` read a written name the
way a section does, as written or with the type folder (`edgecookie`, `geo`
or `device`) in front, so a geo module from `crates/geo/example` is selected
with `[geo] module = "example"`. One registration can supply a module of each
type, each under the name of its own crate, which is how a vendor makes one
backend call serve all three.

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
is run with `[testing] modules = ["seam-probe"]` and its geo module is
selected with `[geo] module = "testing.seam-probe"`. A `[geo] module` or
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

| Field                  | What it does                                                                                             |
| ---------------------- | -------------------------------------------------------------------------------------------------------- |
| `head_inserts`         | Writes markup in `<head>`, ahead of the script bundle                                                    |
| `after_bundle_inserts` | Writes markup straight after the script bundle, for a script that needs the bundle to have run           |
| `element_handlers`     | For the elements a CSS selector matches, keeps the element, replaces one attribute's value or removes it |
| `text_handlers`        | For the text inside the elements a CSS selector matches, keeps a chunk, replaces it or removes it        |
| `stream`               | Runs a stream processor over the document the handlers left                                              |

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

| Module            | Called through                                         | What its context carries                                                                                                                                                            |
| ----------------- | ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Device            | `DeviceModule::detect`                                 | The request, the evidence, the settings and the services. No permission is resolved yet, so nothing whose use needs one                                                             |
| Permission signal | `SignalInput::call`, `valid_signal` and `tdls`         | The request, the evidence, the connection, the consent, the settings and the services, and the location when its use needs no permission. The permissions are what the run produces |
| Edge Cookie       | `EdgeCookieModule::generate` and `resolve_from_client` | Every row of the table above                                                                                                                                                        |
| Route             | `IntegrationProxy::handle`                             | Every row of the table above except the evidence, because the route holds the request itself                                                                                        |

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

A serve middleware has two places to write markup. `head_inserts` writes
straight before the script bundle, and `after_bundle_inserts` writes straight
after it, for a script that needs the bundle to have run and has to run
before the page's own scripts.

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

### The modules a stock build ships

`crates/trusted-server-modules` lists the modules a stock build ships from
crates of their own, in the order their request hooks run. Every adapter
builds its state with that list followed by the builders a deployment added,
and loads its settings against the same list. The `ts` tool registers it for
deploy validation. A module joins a stock build by adding its crate and its
builder to that list, in the place its request hooks should run. The order of
the list does not order page changes, which run in the order an entry names
them. Offering a module does not run it, because the registry builds a module
only when a section of the settings selects it.

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
