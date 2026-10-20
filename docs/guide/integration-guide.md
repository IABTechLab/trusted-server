# Integration Development

Trusted Server integrations are platform-neutral registrations assembled by
`IntegrationRegistry`. Adapter crates provide I/O through
`RuntimeServices`; integration code must not import Fastly, Cloudflare,
Axum, or Spin SDK types.

## Choose the narrowest hook

- `IntegrationProxy` owns explicit method/path endpoints and receives
  `Settings`, `RuntimeServices`, and an EdgeZero-neutral request.
- `IntegrationAttributeRewriter` inspects selected HTML attributes.
- `IntegrationScriptRewriter` handles one declared selector.
- `IntegrationHeadInjector` inserts deterministic head markup.
- `IntegrationHtmlPostProcessor` is for bounded whole-document work that
  cannot be performed during streaming.
- `IntegrationRequestFilter` makes an early request decision.

Build one `IntegrationRegistration` with only the hooks the feature needs.
Use `with_deferred_js()` only for a separately loaded integration module and
`without_js()` when another asset path owns delivery. Proxy routes should be
namespaced and bounded; do not introduce a general outbound proxy.

## Compiling core-neutral fixture

The fixture below registers an attribute rewriter and constructs every
required `RuntimeServices` service without an adapter dependency. The
documentation test extracts this exact fence and compiles it as an isolated
crate.

<!-- documentation-snippet:runtime-services:start -->

```rust
use std::net::IpAddr;
use std::sync::Arc;

use error_stack::Report;
use trusted_server_core::integrations::{
    AttributeRewriteAction, IntegrationAttributeContext,
    IntegrationAttributeRewriter, IntegrationRegistration,
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

struct AssetRewriter;

impl IntegrationAttributeRewriter for AssetRewriter {
    fn integration_id(&self) -> &'static str {
        "example"
    }

    fn handles_attribute(&self, attribute: &str) -> bool {
        matches!(attribute, "src" | "href")
    }

    fn rewrite(
        &self,
        _attribute: &str,
        value: &str,
        _context: &IntegrationAttributeContext<'_>,
    ) -> AttributeRewriteAction {
        value
            .strip_prefix("https://assets.example/")
            .map(|path| AttributeRewriteAction::replace(format!("/assets/{path}")))
            .unwrap_or_else(AttributeRewriteAction::keep)
    }
}

pub fn registration() -> IntegrationRegistration {
    IntegrationRegistration::builder("example")
        .with_attribute_rewriter(Arc::new(AssetRewriter))
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

An `IntegrationProxy::handle` implementation receives the complete runtime
service graph. Register or predict backends through `services.backend()`,
send through `services.http_client()`, bound request and response bodies, and
return `Report<TrustedServerError>` with integration context. The registry
strips internal identity headers before dispatch; an integration must opt into
any explicit forwarding behavior.

Streaming support is an adapter capability. Request
`PlatformHttpRequest::with_stream_response()` only when the caller has
checked `supports_streaming_responses()`; unsupported adapters must reject
the request instead of silently buffering it.

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
| Id                                            | Names the integration in its routes under `/integrations/<id>/`, in its browser module and in diagnostics                                                                                                 |
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

| Declaration                                           | What it does                                                                        |
| ----------------------------------------------------- | ----------------------------------------------------------------------------------- |
| `.with_proxy(...)`                                    | Routes the paths the proxy declares                                                 |
| `.with_head_injector(...)`                            | Emits markup at the start of `<head>` and, if it chooses, after the script bundle   |
| `.with_attribute_rewriter(...)`                       | Rewrites attribute values in publisher HTML                                         |
| `.with_script_rewriter(...)`                          | Rewrites inline script contents                                                     |
| `.with_html_stream_processor(...)`                    | Works on the document as it streams                                                 |
| `.with_request_filter(...)`                           | Inspects a request and can turn it back before it reaches the origin                |
| `.with_js_module(CarriedJsModule { source, sha256 })` | Carries the integration's own browser script, built outside `trusted-server-js`     |
| `.with_deferred_js()`                                 | Serves the script as its own `<script defer>` tag instead of in the main bundle     |
| `.with_standalone_js()`                               | Serves the script only on its own path, for an integration that injects its own tag |
| `.without_js()`                                       | Ships no browser script                                                             |
| `.with_ec_module(name, ...)`                          | Offers an Edge Cookie module that `[ec] module` may select by that name             |
| `.with_geo_module(name, ...)`                         | Offers a geo module that `[geo] module` may select by that name                     |
| `.with_device_module(name, ...)`                      | Offers a device module that `[device] module` may select by that name               |

The three script delivery choices are exclusive and the last call wins.

Each of the last three takes the module's name, which is the path under
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

### Acting on one request

A module that decides something about one request and acts on it later
carries the decision in the request's `IntegrationRequestState`. A request
preparer or a request filter leaves a value under the integration's id.

```rust
IntegrationRequestState::insert(request, EXAMPLE_ID, ExampleDecision::default());
```

When the request produces an HTML document, each value is copied into the
document's state before parsing starts, so the head injector, the rewriters
and the stream processors read it with
`ctx.document_state.get::<ExampleDecision>(EXAMPLE_ID)`. The same values are
handed to the response finalizer the builder declared, which runs on the
response the page path returns.

A request that carries any value gets a document made for it alone. The
document is fetched from the origin, never read from a shared template and
never stored as one, and an HTML response with a body is sent
`private, no-store`. So a module leaves a value only for a request it will
act on, and an ordinary request stays on the shared path.

A head injector has two places to write. `head_inserts` writes at the start
of `<head>`, before the script bundle, and `after_bundle_inserts` writes
straight after the bundle, for a script that needs the bundle to have run and
has to run before the page's own scripts.

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
crates of their own, in the order their hooks run. Every adapter builds its
state with that list followed by the builders a deployment added, and loads
its settings against the same list. The `ts` tool registers it for deploy
validation. A module joins a stock build by adding its crate and its builder
to that list, in the place its hooks should run. Offering a module does not
run it, because the registry builds a module only when a section of the
settings selects it.

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
