# Design Spec: The Integration Seam

**Status:** Proposed, 2026-08-27, revised 2026-08-28, 2026-10-06 and 2026-10-07. This PR adds design
documents only and targets `main` directly. Following the review of #1043
(27 August) the seam it defines is a precondition for the module series
rather than a follow-up to it, so the order is now this spec, then its
implementation in a seventh PR against `main` (51Degrees), then PRs #1043
to #1047 reworked onto it. It reads alongside the series' specs, which this
PR now carries too, so the whole normative set is reviewable before any of
the code lands.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-07-30-pluggable-providers-design.md`,
`2026-07-30-provider-migration-rollout-design.md`,
`provider-code-registry.md`
**Related PRs:** #986, #1043, #1044, #1045, #1046, #1047, #1054
**Last updated:** 2026-10-06

> **Why this spec exists.** PRs #1043 to #1047 open the identity, device and
> geo seams, so a vendor can ship an Edge Cookie module in its own crate
> and an adapter injects it. The nine vendor integrations already inside
> `trusted-server-core` do not sit behind those seams. They hang off the
> integration registry, which is a private table in core, so none of them
> can move out until that table is opened. This spec defines the one core
> change that opens it, so the migration of every existing vendor is a
> single defined piece of work rather than an open question repeated once
> per vendor.

> **Relationship to #986 and the #1043 review.** The pluggable-providers
> spec in #986 (31 July) defines identity, device and geo as modules
> selected by `[ec] module`, `[device] module` and `[geo] module` and
> wired by each adapter through a composition root. #1043 and #1044
> implement that. The review of #1043 on 27 August asks instead that a
> vendor's identity module be a capability declared on its integration
> registration, because a vendor ships its browser JavaScript and its
> identity function together. This revision adopts that end state (§3.6)
> and applies its rule consistently, so geo and device modules attach the
> same way. The lifecycle contract, the identifier envelope, the permission
> gating and the validation rules in #986 are unchanged. What changes is
> only where a vendor's module is constructed and selected from. The
> registration shape needs a registry a vendor crate can register with,
> which is what §3.1 opens, so this spec precedes #1043 rather than
> following it.

## 1. The problem, with the code that causes it

Every claim here was read from `main` at b7fcb5d4c (28 August), which the
seventh PR targets, and the five series PRs do not touch these files.

1. **The registry is closed.** `IntegrationRegistry::new` takes only
   `&Settings` and iterates a fixed table
   (`crates/trusted-server-core/src/integrations/registry.rs:792`, table at
   `crates/trusted-server-core/src/integrations/mod.rs:290`). Both
   `IntegrationBuilder` and `builders()` are `pub(crate)` with private
   fields, so no adapter and no external crate can add to the list. The
   payload type `IntegrationRegistration`
   (`registry.rs:586`) is already public, so an outside crate can build a
   registration but has nowhere to hand it.
2. **Browser JavaScript is fixed at build time.** `trusted-server-js`
   discovers `lib/src/integrations/*/index.ts`, builds one file per
   integration, and `build.rs` writes a fixed array of `include_str!`
   entries consumed by `bundle.rs`. `IntegrationRegistry::js_module_ids`
   only serves a module when
   `trusted_server_js::module_bundle(id).is_some()`
   (`registry.rs:1155`), so an integration outside that compile-time map
   gets no script however it registers.
3. **Startup validation names every vendor.** `validate_enabled_integrations`
   imports and calls each vendor's config type by name
   (`crates/trusted-server-core/src/config.rs:136` to `:166`).
4. **Demand providers are a second closed table.**
   The list of Prebid, APS and the ad server mock is at
   `crates/trusted-server-core/src/auction/mod.rs:51` to `:53`, inside
   `provider_builders()` at `:49`. `main` has since replaced that table with
   a compiled auction plan, in PR #1016, and §3.4 follows the plan.
5. **Two vendors reach further into core.** DataDome drives cache privacy
   and the origin fetch decision through a marker type
   (`html_processor.rs:303`, `publisher.rs:4369` to `:4381`,
   `publisher.rs:2653`), and GPT diagnostics is called by name from all four
   adapters (for example
   `crates/trusted-server-adapter-fastly/src/app.rs:564`).

The result is that the project carries nine vendors as core code (ten
registered integrations, since GPT registers a proxy and a diagnostics
integration). Tech Lab engineering time is spent on named commercial vendors, and
every new vendor is another core change, as PR #1054 shows.

## 2. Principle

A vendor integration is a module like any other. Core owns the seam and
owns nothing behind it. Concretely:

- Core defines the registration contract and the request pipeline. It names
  no vendor.
- An integration builder is the one way an implementation of any module
  type reaches operator configuration. A builder that supplies only an
  implementation is not a page integration, ships no browser JavaScript,
  and no section selects it.
- A vendor integration ships as its own crate with its Rust, its browser
  JavaScript, its configuration type, its startup validation and its tests.
- An adapter composes the deployment by injecting the registrations it was
  built with, exactly as it already injects the geo and device modules.
- Tech Lab engineering assesses and reviews vendor crates. It does not
  maintain them. A vendor crate pins the dependency versions its module
  needs, because Cargo links different major versions of one crate into one
  binary, and a vendor release needs no pull request to this repository. One
  crate for every vendor would mean one release train and one person choosing
  versions for everyone, which does not scale with the number of modules.
- What an operator selects to supply a capability is a module, and every
  type selects one the same way, with `module` where one runs and `modules`
  where several run: `[ec] module`, `[geo] module`, `[device] module`,
  `[permission-signal] modules`, `[demand] modules`, `[ad-server] module`
  and, for a page integration, the section of its type. The word provider
  has left the configuration, and the identifiers this series added say
  module.
- A module is named by its crate folder, not by an id written in its code.
  The name is the crate's path below `crates/`, with `.` between the parts,
  taken from `CARGO_MANIFEST_DIR` when the crate is built, so
  `crates/permission-signal/gpp` is `permission-signal.gpp` and cannot drift
  from its folder, as the hand-written ids this series started with had
  (that crate called itself `gpp_sale_opt_out`). A section is named exactly
  as its type folder, apart from `[ec]`, whose folder is `edgecookie`, and a
  name written in a section may leave the section's own type folder off, so
  `[permission-signal] modules = ["gpp"]` and `["permission-signal.gpp"]`
  select the same module. Core's own modules, such as `hmac` and `builtin`,
  take bare names. An integration that still lives in core carries the name
  its crate will have, held as a constant, so its section and table do not
  change when it moves out. A section that selects several modules uses
  `modules`, a list, and one that selects one uses `module`.
- Page integrations are selected from the section of their type, such as
  `[cmp]`, `[bot-protection]`, `[framework]`, `[tag]`, `[ad-tag]`,
  `[identity]`, `[audience]` and `[testing]`, each the folder under
  `crates/` that a crate of that type lives in, with `[auction]` and
  `[proxy]` selecting the modules they run beside their own settings, so
  there is no `[integration]` section, and a table its
  section does not select, a section that selects nothing and a name no
  module in the deployment supplies are each refused at startup.

## 3. Design

### 3.1 Opening the registry

Make the builder contract public and give the registry a second input.

- `IntegrationBuilder` becomes `pub` with a public constructor. The built-in
  set is discovered at build time from the integration directories, in the
  way `trusted-server-js/build.rs` already discovers browser modules, so
  `builders()` is generated and core keeps no hand-written list of vendors.
  That removes the drift the 31 August review found in `migration_guards.rs`,
  where one integration had no entry. An external crate still arrives
  through the registrations an adapter supplies, which is the only route for
  code outside this repository.
- `IntegrationRegistry::with_plan`, the constructor every adapter calls
  since PR #1016, gains a companion,
  `IntegrationRegistry::with_plan_and_registrations(settings, plan, extra)`,
  where `extra` is a slice of externally supplied builders. `with_plan` keeps
  its signature and calls the companion with an empty slice, so no existing
  caller changes behavior.
- Duplicate integration ids are a startup error, naming both sources, so a
  vendor crate cannot silently shadow a built-in. There is no such check
  today, only a per-route conflict check and a debug-only assertion, so the
  builder carries a source label and the registry gets the check. Prebid
  Server and APS are demand implementations rather than integrations,
  selected by `[demand] modules` and by no section, and their names are
  reserved by the same check so an integration cannot take one.

### 3.2 Carrying browser JavaScript on the registration

Add one optional field to `IntegrationRegistration`, next to `js_deferred`
and `js_disabled`: the module source and its hash, both `&'static str`, so a
crate can `include_str!` its own built bundle.

`js_module_ids` keeps serving built-in ids from the compile-time map and
serves a carried module from the registration. The composition of the
served script moves from `trusted-server-js` into core, because every hop
after `js_module_ids` today re-enters `trusted-server-js` by id and silently
drops an id it does not know (`bundle.rs`, `concatenated_module_ids` and
`visit_concatenated_module_parts`), and the hash memo is keyed on the id
list alone. Core composes body and hash from (id, source, hash) triples
drawn from both sources, keeping the exact byte rule of today (core first,
`;\n` separator) so every existing `?v=` hash is unchanged. Three consumers
follow the registry rather than the compile-time list: the standalone
module route `parse_single_module_filename` (`publisher.rs`), the
`GPT_DIAGNOSTICS_INTEGRATION_ID` standalone special case, which becomes a
registration property, and `template_fingerprint`, which must cover carried
modules so a vendor crate rebuild invalidates the server-side template
cache. The registry verifies a carried module's declared hash against its source
when it is built, so a stale literal is a startup error rather than a
stale script served under a valid-looking URL. Covering carried modules in
the template cache hash means the publisher entry point needs the
registry, so it takes the configuration and the registry as one argument
rather than two. The served script keeps its cache rule, being the
`?v=<hash>` query matched at serve time (there is no integrity attribute on the tag today, and
this change adds none).

### 3.3 Startup validation on the registration

Replace the named list in `config.rs` with a validation hook on the
registration, so a vendor validates its own configuration and a missing
vendor cannot silently stop being validated. The existing test that asserts
every registered integration is covered by deploy validation
(`config.rs:688` on `main`) is rewritten against the hook, so the guarantee
survives in a vendor-neutral form. Two details the map of `main` adds. The
enumeration the test needs is independent of which integrations a
configuration selects, so the registry exposes the full set of registrations
it was built from, not only the ones the sections select. And
Prebid Server, APS and `ad-server.mock` are demand and ad server
implementations rather than integrations, so their `[demand.<name>]` and
`[ad-server.<name>]` tables validate through the same reject-what-you-do-not-
know rule the registration hook gives every other module, and a test
plants a setting each of them must reject.

### 3.4 Demand and ad server providers

This change does not open the auction to demand implementations from outside
core. An earlier revision of this section gave `AuctionOrchestrator` a public
provider-builder type and a second input. `main` has since replaced the
provider table that revision addressed with a compiled auction plan, in PR
#1016, and the orchestrator and the integration registry share that one
compiled plan. This change follows that design and adds no auction provider
builder.

Demand and the ad server are two provider types in their own right, and
neither is an integration. A demand provider is a source bids are requested
from, and several run, so `[demand] modules` takes a list. An ad server
decides what is shown, and one runs, so `[ad-server] module` takes a string.
An implementation is named by its module path. A demand table names its
implementation on an `implementation` line, because `demand` is not the
type an implementation is named under, which is how two Prebid Servers run
side by side under different names, and the ad server's name resolves
within its own type, so `mock` is `ad-server.mock`:

```toml
[demand]
modules = ["pbs_main", "pbs_eu", "aps"]

[demand.pbs_main]
implementation = "auction.prebid-server"
endpoint = "https://pbs.example.com/openrtb2/auction"

[demand.pbs_eu]
implementation = "auction.prebid-server"
endpoint = "https://pbs-eu.example.com/openrtb2/auction"

[demand.aps]
implementation = "auction.aps"
endpoint = "https://aax.amazon-adsystem.com/e/dtb/bid"
rendering_mode = "aps_sdk"

[ad-server]
module = "mock"

[auction]
enabled = true
timeout_ms = 1000

[auction.bidders.example_bidder]
module = "pbs_main"
```

The demand implementations in this repository are `auction-protocol.openrtb`,
`auction.prebid-server` and `auction.aps`, and the one ad server
implementation is `ad-server.mock`. None of
them is an integration, so no section selects them, and a configuration
that selects one refuses startup. APS in particular
stopped being an integration, and its `rendering_mode` now sits in its
`[demand.<name>]` table rather than in a vendor integration table. Every
demand and ad server endpoint must be HTTPS, or HTTP to a loopback host only
(`127.0.0.1`, `::1`, `localhost`).

`[auction]` is not a module type of its own. It keeps `enabled`,
`timeout_ms`, the creative settings and `allowed_context_keys`, its
`modules` list selects the page modules the auction runs, such as `prebid`
(§2), and `[auction.bidders.<code>] module = "<demand name>"` maps a
bidder code a page asks for onto one of the declared demand providers.

The bid renderer contract is still generalized in this change. Before it,
`BidRenderer` was an enum with one variant, `Aps(ApsRendererV1)`
(`crates/trusted-server-core/src/auction/types.rs:216`), serialized into the
OpenRTB response extension under a `type` tag
(`crates/trusted-server-core/src/auction/formats.rs:377`,
`crates/trusted-server-core/src/openrtb.rs:183`). It becomes an open
descriptor, a type tag and a payload the demand provider supplies, with
the same serialized form, so the response a page receives does not change
and the APS renderer type moves out of the shared auction types into
`crates/trusted-server-core/src/integrations/aps.rs`, which is where the APS
demand implementation lives on `main`. The ad server mock uses the neutral
form.

The plan keeps three things closed, read from `main` at 066ea3c69, and they
are recorded here rather than solved.

- The set of demand implementations is fixed in core, being the profiles
  `standard`, `prebid-server` and `aps`
  (`crates/trusted-server-core/src/auction/profile.rs:170`), and the
  compiled form is a closed enum (`profile.rs:63`) whose Prebid and APS
  behavior is imported from those modules (`profile.rs:11` and `:12`).
  Any number of demand providers may run the same implementation
  (`compiler_supports_two_instances_of_the_same_profile` in
  `crates/trusted-server-core/src/auction/plan.rs`), which is what
  `implementation` expresses, but a vendor cannot add an implementation
  without changing core.
- The plan compiler treats `prebid-server` and `aps` specially by name
  (`plan.rs:307`, `:585` and `:595`).
- The only ad server implementation is `adserver_mock` (`plan.rs:20` and
  `:556`), and the orchestrator builds it by calling that module directly
  (`crates/trusted-server-core/src/auction/mod.rs:90`).

Moving an auction-side vendor out of core therefore needs a change to the
auction plan, which is outside this stack.

### 3.5 The two neutral hooks

- **Response shaping.** DataDome's marker becomes a neutral request
  extension meaning "this response is personalized to the request, do not
  share it", set by any integration. Core keeps the behavior, being the full
  body buffer and private cache, and stops naming a vendor.
- **Request prepare and finalize.** The direct GPT diagnostics calls move
  behind hooks on the registration, so an adapter runs whatever its
  registrations declare. Those calls reach beyond the adapter edge. On
  `main` nine production `prepare_request` call sites sit in the four
  adapters (two in Fastly, two in Axum, two in Cloudflare and three in
  Spin) and a tenth sits in core's `handle_publisher_request`
  (`publisher.rs:4050`). `finalize_response` has one production call site
  and it is in core rather than in any adapter (`publisher.rs:4783`), so
  the finalize hook has to run on the core response path and not only at
  the adapter edge.

### 3.6 Identity, geo and device as registration capabilities

The rule. Things the host supplies are platform services, being the KV store, the HTTP client, and the host TLS and HTTP/2 signals. A host geo lookup is host data that a selected geo module may consume. Geo itself is a module a deployer selects, never a platform service. The transport evidence types, being the client IP and the TLS, JA4 and HTTP/2 signals, are candidates to migrate behind an EdgeZero evidence contract when one exists. Things a vendor supplies are capabilities of that vendor's module.
An identity module, a geo module and a device module are supplied by
vendors, with or without any host involved, so all three are module
capabilities, and the same registration carries them alongside the module's
JavaScript and hooks. A registration does not have to carry JavaScript at
all, so a crate may supply an implementation and nothing else, which is how
core ends up naming no vendor while still shipping a default for each
capability.

- The registration builder gains three optional capabilities, at most one
  of each per registration:
  `.with_ec_module(name, Arc<dyn EdgeCookieModule>)`,
  `.with_geo_module(name, Arc<dyn PlatformGeo>)` and
  `.with_device_module(name, Arc<dyn DeviceModule>)`. Each is declared
  under a name, which is the path under `crates/` of the crate the module
  lives in, so one registration can supply a module of each type, each
  under the name of its own crate. An Edge Cookie module is declared under
  the name its own `id` returns. The traits are the ones #1043 and #1044
  define, unchanged.
- Selection keeps the select-exactly-one semantics of #986. `[ec] module`,
  `[geo] module` and `[device] module` each name one implementation by the
  name it was declared under, read the way a section reads a name, as
  written or with the type folder (`edgecookie`, `geo` or `device`) in
  front, and every implementation reaches those selectors the same way,
  through an integration builder's registration. Core names none of them.
  A `[geo] module` or `[device] module` that names a module which supplies
  no module of that type, or a name no running module supplies, is a
  startup error, and the message lists the modules of that type the
  deployment runs. A registration that declares a capability no selector
  names is inert for that capability and its other hooks still run, and
  startup logs a warning naming the unused module, so an operator can see
  a module shipping script for a module that is not selected.
- No module is built into core. Everything goes through one method, so
  the HMAC identity module from #1043 and the User-Agent-only device
  module from #1044 become Tech Lab-owned crates under
  `crates/integrations/`, registered by an integration builder, selected by
  `[ec] module` and `[device] module`, and validated through §3.3 like
  any other registration. Neither ships browser JavaScript and no section
  selects either, because a builder that supplies an implementation does
  not have to be a page integration. Each takes an
  `[ec.<name>]` or `[device.<name>]` table only where it has a setting to
  carry, which `hmac` does and the User-Agent-only classifier does not, and
  the adapters register both by default. Core keeps only the seam and the
  `none` state for each capability (no identity, no location, unknown device
  signals). A deployment that selects no identity implementation is
  stateless, as #986's `module = "none"` already means.
- Composition. The composition root resolves the selected module for
  each capability from the registry once at startup and places it in the
  per-request services, so the request path is unchanged from #1043 and
  #1044. Adapters stop injecting vendor modules directly (the Edge Cookie
  module an adapter holds for itself and the injected closures in
  `build_device_module` and `build_geo_module` go). Host
  defaults are still supplied by the adapter as platform services and are
  consumed by whichever implementation is selected, through the request
  evidence and host signal abstractions, exactly as now. A module that needs a host signal
  the running adapter does not expose is rejected at startup, as #986
  requires.
- A module that declares all three capabilities may share one backend call
  per request across them, which is the shared-backend principle in
  `CLAUDE.md`, and is the case that a split between a registry-attached
  identity module and platform-attached geo and device modules would
  have made impossible.
- The host-signal device module that #1044 ships as a separate crate is a
  module built on platform signals, so it registers as a module too. The
  signals it reads stay platform.

Effect on the series. #1043 and #1044 rework their construction and
selection path onto this section, move the HMAC and User-Agent-only
modules into module crates, and keep everything else. #1045, #1046
and #1047 are unaffected beyond the rebase.

### 3.7 Page changes as middleware

A page change is one middleware, a capability an integration registers, run
only where an ordered entry in the settings names it. The middleware contract
replaces the four page hook traits, `IntegrationAttributeRewriter`,
`IntegrationScriptRewriter`, `IntegrationHtmlStreamProcessorFactory` and
`IntegrationHeadInjector`, and their four context types. Proxies and request
filters are not page changes and keep their traits. A script-source claim
stays a declared transition, and the trusted attributes an integration adds
to the unified script tag stay with its browser assets.

**The contract.** A registered middleware is `Send + Sync` and holds no
request state. For each document core calls its factory once, with one
context, and gets back an action. The context carries the request host and
scheme, the origin host, the per-document state and the script buffering
limit, which the four old contexts carried between them. The element name,
the attribute name and the last-chunk flag arrive with each matched element
or text chunk. An action combines any of these parts, and an empty action
leaves the document unchanged:

| Action part      | Replaces                                | Decision it carries                                                                               |
| ---------------- | --------------------------------------- | ------------------------------------------------------------------------------------------------- |
| Head inserts     | `IntegrationHeadInjector`               | Markup to write at the start of `<head>`.                                                         |
| Element handlers | `IntegrationAttributeRewriter`          | For each element a selector matches, keep it, replace one attribute value, or remove the element. |
| Text handlers    | `IntegrationScriptRewriter`             | For the text inside each element a selector matches, keep it, replace it, or remove the node.     |
| Stream processor | `IntegrationHtmlStreamProcessorFactory` | One processor over the document the HTML rewriter produced.                                       |

The parts carry exactly the decisions the four traits carry today, so
converting a hook repackages it and must not change what it does. A
middleware a crate supplies is named by its integration and a local name,
`<integration>.<local-middleware>`, in the way a qualified module ID is
written.

**Where it runs.** Each phase has its own ordered list of entries, `[[fetch]]`
and `[[serve]]`. An entry covers one media type, optionally only the requests
under a path prefix, and names the middleware to run in order. A response
takes the first entry that covers it. Each middleware is handed the document
as the ones before it left it. A middleware reads its settings from
`[middleware.<name>]`.

```toml
[[fetch]]
media_type = "text/html"
path = "/news/"
middleware = ["prebid.strip", "gpt"]

[[fetch]]
media_type = "text/html"
middleware = ["prebid.strip"]

[[serve]]
media_type = "text/html"
middleware = ["datadome"]
```

Entries carry no settings and activate nothing. Whether an integration runs
is decided where it is today, and an entry only places and orders the page
changes of the integrations that run. A middleware named in no entry is a
warning at startup, and a name no registration supplies is a startup error.
Page changes therefore take their order from the entries rather than from
the integration's one position, so a publisher can run a change on part of a
site, or place two changes of one integration apart, without touching the
auction's priority.

**The two phases.** Fetch is obtaining the document from the origin. A fetch
middleware is handed nothing about the reader, so its output cannot depend on
who asked, and what it leaves is what a shared template stores for every
reader. Serve is preparing the document for the response. A serve middleware
runs per reader, on that reader's copy, whether the page came from the store
or the origin, and nothing it writes is stored. The rules that today keep one
reader's bytes out of a shared template by declaration, being a hook raising
`RequestProcessingRequirements` for request-dependent output, the rule
against copying request-private state into a template, and the rule that a
fingerprinted head insert is configuration-rendered, become structural for
page changes. The requirements type stays as a contract about the request,
raised by the request filter and the preparation hook.

**What converts.** On `main` the four traits have nineteen implementations in
twelve of the fifteen integrations: nine attribute rewriters (`datadome`,
`google_tag_manager`, `gpt`, `js_asset_proxy`, `lockr`, `permutive`,
`prebid`, `sourcepoint`, `testlight`), three script rewriters
(`google_tag_manager` and two in `nextjs`), one stream processor (`nextjs`)
and six head injectors (`aps`, `datadome`, `didomi`, `gpt`, `prebid`,
`sourcepoint`). Eighteen become middleware. The APS head injector returns no
markup and only sets a trusted tag attribute, so it becomes that attribute
and registers no middleware. One implementation reads a request-scoped
decision, DataDome's head injector leaving out its client tag for a request
its filter marked, so it becomes a serve middleware and its tag moves to the
start of `<head>`, which is a visible change and is stated as one. The other
eighteen run in the fetch phase, where they run today. Each integration
converts in a change of its own, checked against the differential harness,
and the four traits, their contexts and the registry's four hook lists are
deleted when the last implementation converts.

## 4. Migration of the nine existing vendors

One vendor per PR, after this change lands. Each migration PR gives its vendor crate a visible maintainers declaration, the way Prebid.js requires of every adapter, and per-crate code ownership, so the boundary carries a named owner from its first day. Each moves its Rust, its
TypeScript, its config type and its tests into
`crates/integrations/<vendor>`, and the adapter that wants it depends on it.

| Vendor                                                           | What it needs                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| ---------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Didomi, Google Tag Manager, Lockr, Osano, Permutive, Sourcepoint | Move as they are. Coupled only through the builder table, deploy validation and the JS map.                                                                                                                                                                                                                                                                                                                                                                        |
| APS                                                              | A demand implementation rather than a page integration, so it is configured under `[demand.<name>]`, carries its `rendering_mode` there, and no section selects it. Needs the generalized renderer contract in §3.4, which this change delivers. It also needs a change to the auction plan so an auction-side vendor can live outside core (§3.4), and browser-side work, because core TypeScript imports APS directly (§8 item 8). This change delivers neither. |
| GPT (the `gpt` proxy and `gpt_diagnostics`)                      | The proxy moves as it is. The diagnostics half needs the prepare and finalize hooks in §3.5.                                                                                                                                                                                                                                                                                                                                                                       |
| DataDome                                                         | Needs the neutral response-shaping hook in §3.5, and about forty test literals move with it.                                                                                                                                                                                                                                                                                                                                                                       |

A vendor's `[<type>.<name>]` table needs no change when the vendor moves,
because the sections are read into `TypeSections` in
`crates/trusted-server-core/src/settings.rs`, which accepts a name core
does not know. Which vendors run is the section of each vendor's type, and
a table its section does not select refuses startup like any other stray
module table.

Two more places every move must touch, found by mapping `main`:

- `crates/trusted-server-core/src/migration_guards.rs` embeds core source
  files by relative path with `include_str!`, so a vendor move that leaves
  an embedded entry behind breaks the build rather than a test. On `main`
  the directory `crates/trusted-server-core/src/integrations/` holds 23
  `.rs` files, being 2 infrastructure files (`mod.rs` and `registry.rs`),
  6 files in the `nextjs/` subdirectory, 2 in the `datadome/` submodule and
  13 top-level integration modules. The guard embeds 20 of those 23, and 9
  of the 20 are files of the nine vendors in the table above. The three it
  does not embed are `osano.rs` and the two `datadome/` files, so the guard
  is already incomplete and the Osano move has no guard entry to delete.
  Separately, `builders()` registers 13 integrations, which is not the same
  13 as the file count, because `adserver_mock` is a file with no
  registration while `nextjs` is a registration held in a subdirectory. The
  guard cannot derive its list from the registrations, because
  `include_str!` paths are fixed at compile time, so this change drops the
  vendors' files from the guard instead, because a module crate is outside
  the core neutrality guarantee, and a move then deletes nothing there.
- The `ts audit` command carries its own vendor table (detection patterns
  and configuration section names in
  `crates/trusted-server-cli/src/commands/audit/analyzer.rs` and
  `commands/audit/mod.rs`). It is outside the registry and outside this
  change. Each vendor move takes its `ts audit` rows with it, and how the
  CLI learns a vendor's detection pattern from a crate is a follow-up this
  spec records but does not solve.

## 5. What does not change

The request pipeline, the served script format and its hash, every
module's settings table, the permission model,
and the identity lifecycle, envelope and validation contracts from #986 as
implemented in PRs #1043 to #1046. No integration changes behavior. A
deployment that lists the same integrations gets the same responses.

## 6. Acceptance

1. **A round trip with a non-default implementation, on Fastly.** A test
   integration defined outside `trusted-server-core`, carrying its own
   JavaScript, registers through an adapter, appears in the served bundle
   with the right hash, runs its hooks in the right order, and is rejected
   on a duplicate id. A seam is only proven by an implementation that is
   not the built-in one. The round trip has to run on the Fastly adapter,
   which is the primary deployment target, and not only on the Axum dev
   server, because a seam proven on the dev server alone is not a seam any
   production deployment can use. The Fastly adapter carries only
   `src/main.rs` and so has no library target for an external crate to
   register through (§8 item 7), so meeting this criterion means giving
   that adapter a composition entry point a vendor crate can reach.
2. **Capabilities round trip, on the same adapter.** The same test
   integration declares an identity, a geo and a device module. With the
   three selectors naming it, a request is served by all three (the created
   identifier carries its code, the resolved country and the device signals
   are its). With a selector naming a module that lacks the capability,
   startup fails with an error that names the module and the capability.
3. **Parity.** The existing integration and parity suites pass unchanged,
   because the built-in set still registers through the same path.
4. **No vendor left behind.** The rewritten deploy-validation test shows
   every registered integration validates its configuration.
5. **Page changes through the contract.** The test integration registers a
   middleware. It runs only where a `[[fetch]]` or `[[serve]]` entry names it,
   a fetch middleware's output reaches a second reader from the shared
   template, a serve middleware's change never reaches the stored copy, and
   each of the nineteen converted implementations reproduces its recorded
   output.
6. All CI gates in `CLAUDE.md`, on all four adapters.

## 7. Risk

The change is wide but shallow. It touches the registry, the served script
path, deploy validation and four adapter entry points, and it changes no
integration's behavior. The largest risk is the served script, where a
mistake shows up as a wrong hash or a missing module, so §6's round trip
covers both, and the existing hash round-trip tests in `bundle.rs`,
`publisher.rs` and `tsjs.rs` pin every current `?v=` value. The second risk
is the renderer contract, where `BidRenderer::as_aps` is an exhaustive
single-arm match with eight test sites constructing the variant directly,
and the wire shape `{"type":"aps", ...}` must survive byte for byte. Doing this once is what removes the per-vendor core change
that the project pays for today, most recently in PR #1054.

## 8. What implementing this found

A probe integration built outside `trusted-server-core` and registered
through an adapter exercised every seam end to end. Eight things surfaced
that reading the code did not, and a ninth came from reading the settings
loader for what a vendor with a secret would need. They are recorded here
rather than left for each vendor to rediscover.

1. **A vendor's own deploy rules do not run through the operator CLI.**
   `ts config validate` and `ts config push` reach validation through
   `TrustedServerAppConfig`, which supplies no builders, so a vendor's
   `[<type>.<name>]` rules are skipped on exactly the path an operator
   uses. The validation hook in §3.3 is only real once that path can carry
   the builders a deployment was composed with. This needs a decision:
   either the CLI is built per deployment with its vendor crates, or the
   adapter validates at startup and the CLI checks only what core owns.
2. **A carried module's hash is hand-maintained and line-ending fragile.**
   Core's own modules get their hashes generated at build time. A vendor
   crate keeps a literal beside its `include_str!`, and on a checkout that
   rewrites line endings the embedded file changes and the literal stops
   matching, which fails startup on that machine only. A generated helper
   or a documented build-script recipe removes the trap, and the probe pins the
   file's line endings and tests the literal, which every vendor would
   otherwise have to reinvent.
3. **A module is resolved more than once per request.** On `main` this is
   core against core rather than a proxy against the request path. Every
   adapter resolves geo once to build the EC context (`build_ec_context` at
   `crates/trusted-server-adapter-axum/src/app.rs:162`,
   `crates/trusted-server-adapter-cloudflare/src/app.rs:142` and
   `crates/trusted-server-adapter-spin/src/app.rs:346`, and
   `build_ec_request_state` at
   `crates/trusted-server-adapter-fastly/src/app.rs:394`). On
   `POST /auction` the same request then reaches `handle_auction`, which
   looks the same client IP up a second time to fill in the auction's
   device info (`crates/trusted-server-core/src/auction/endpoints.rs:262`).
   Those two are the only production geo call sites in the tree, so the
   auction route is where the duplication shows. `CLAUDE.md`'s principle
   that a vendor sharing one backend makes a single call per request needs
   a per-request module context to hang that on, which this change does
   not introduce.
4. **One core reader still reaches into a vendor's payload.** The
   `hb_adid` fallback in the publisher reads the APS renderer's fields, so
   the APS migration needs a neutral answer for it rather than only the
   renderer contract in §3.4.

5. **Request preparation covers different routes on each host.** Every
   adapter runs preparers before routing, but not on the same set of
   routes. Cloudflare covers everything it registers, because every route
   binding goes through one wrapper (`make_handler`) and the adapter has no
   health route at all. Axum covers every route but `GET /health`, which is
   bound to an inline closure. Fastly covers every route but `GET /health`,
   which short-circuits before the app is built, plus the S2S batch sync
   and the two admin lookup routes, which return deliberately before the
   preparer runs. Spin is the widest gap, covering only `POST /auction`,
   the two page-bids GET routes and the publisher fallback, so its
   `/health`, its discovery and signature-verification routes, its inline
   admin stubs and all six of its first-party bindings (`proxy`, `click`,
   `sign` on GET and POST, and `proxy-rebuild` on GET and POST) run with no
   preparer at all. A module that strips its own reserved query or cookie
   is therefore protected on a different set of routes depending on the
   host it is deployed to. Making that uniform means routing each adapter's
   hand-written handlers through one wrapper, which is worth doing before a
   vendor depends on it.

6. **A module's validate function runs nowhere in a real deployment.**
   Building the registry calls only a builder's build function, and the
   operator CLI calls deploy validation without the builders, so a vendor
   whose checks live in `validate` has them enforced on no path at all. The
   probe works around it by repeating its check inside its build function,
   which every vendor would have to copy. Either the registry runs
   `validate` when it builds, or the operator path carries the builders,
   and the second is item 1. This is the same gap as item 1 seen from the
   other side, and together they mean §3.3 is not yet delivered in
   practice even though the hook exists.
7. **The Fastly adapter cannot take a vendor crate at all.** Its
   `build_state_with_registrations` is crate-private and it has no library
   target, so composing a module into a Fastly deployment means editing the
   adapter. The other three adapters expose both entry points. Fastly is
   the primary deployment target, so this one decides whether the seam is
   usable in production or only in the dev server.
8. **Core TypeScript imports APS directly, so generalizing the Rust
   renderer alone does not move APS out.** On `main` at d516a9e94,
   `crates/trusted-server-js/lib/src/core/auction.ts:5` imports
   `parseApsRendererDescriptor` from `../integrations/aps/render` and calls
   it at line 139, `crates/trusted-server-js/lib/src/core/request.ts:2`
   imports `dispatchApsRendering` and `renderApsCreative` from the same
   module and calls both at lines 56 to 59, and
   `crates/trusted-server-js/lib/src/core/types.ts:69` fixes the shared
   renderer type with `export type AuctionBidRenderer = ApsRendererV1`.
   That coupling is pre-existing on `main` and is introduced by no PR in
   this stack. §3.4 does not reach it either, because §3.4 generalizes the
   Rust `BidRenderer` enum and the serialized descriptor, not the browser
   code that consumes them. Moving APS therefore needs the browser side
   generalized too, so that core TypeScript names no vendor. Designing
   that, whether as a browser-side renderer registry or in some other
   shape, is out of scope for this stack and belongs with the APS
   migration in §4.
9. **A vendor crate cannot declare a secret setting, and core's settings
   code names DataDome.** A secret reference is resolved only at the paths
   `TrustedServerAppConfig::secret_fields` lists, which is a fixed list in
   core, and DataDome's two secret settings are on it by their path.
   `remove_inactive_secret_references` decides by name whether those two
   are live, and deploy validation checks their key references by reading
   DataDome's settings type. All three are on `main`, and no pull request
   in this stack changes them. A vendor crate with a secret of its own
   therefore has to read it from the secret store itself at request time,
   and moving DataDome out of core means changing all three. A declaration
   on the builder closes this, being the secret leaves a module reads,
   resolved only when a section selects the module and refused when one
   points outside the module's own table. The DataDome move in §4 needs it
   first.

Items 1, 6 and 7 are the ones a vendor meets on its first day, item 9
joins them for a vendor with a secret, and item 7 decides whether any of
this is reachable on the platform most deployments use. Item 5 is the one
that produces a bug report nobody can reproduce, because whether it appears
depends on which host the reporter runs.

Taken together these say the seam is proven but not yet finished. A vendor
can register a module, ship its browser code, declare a geo module and
serve a route, all from its own crate and proven end to end. It cannot yet
do that on Fastly, and its own configuration rules are not enforced
anywhere. Both are small changes against what this document already
defines, and both should land before the first vendor is asked to use it.

## 9. Sign-off

| #   | Decision                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           | Status               |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------- |
| 1   | Vendor integrations belong outside core, behind the registration contract                                                                                                                                                                                                                                                                                                                                                                                                                                                          | Proposed             |
| 2   | Tech Lab engineering reviews vendor crates, and does not maintain them                                                                                                                                                                                                                                                                                                                                                                                                                                                             | Proposed, governance |
| 3   | A registration may carry its own browser JavaScript                                                                                                                                                                                                                                                                                                                                                                                                                                                                                | Proposed             |
| 4   | Deploy validation moves onto the registration                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | Proposed             |
| 5   | The nine existing integrations migrate one PR each, on the schedule in §4                                                                                                                                                                                                                                                                                                                                                                                                                                                          | Proposed             |
| 6   | This change completes the Rust side for a page integration that holds no secret, so after it moving one needs no Rust core change. An auction-side vendor still needs a change to the auction plan (§3.4). A vendor with a secret setting still has its path in core's secret list (§8 item 9), so a DataDome move needs a secret declaration on the builder first. The browser side is not complete, because TSJS core still imports the APS renderer directly (§8 item 8), so an APS move also needs a browser renderer contract | Proposed             |
| 7   | Identity, geo and device modules are capabilities of a module registration (§3.6), the #1043 review's rule applied to all three                                                                                                                                                                                                                                                                                                                                                                                                    | Proposed             |
| 8   | No module is built into core, because HMAC and the User-Agent-only device module are Tech Lab-owned crates registered by an integration builder and selected by `[ec] module` and `[device] module`, neither being a page integration, and core keeps only `none`                                                                                                                                                                                                                                                                  | Proposed             |
| 9   | This spec and its core implementation precede #1043, so 51Degrees implements the core seam and the nine vendor moves in §4 stay one PR each                                                                                                                                                                                                                                                                                                                                                                                        | Proposed             |
| 10  | What an operator selects is a module, selected with `module` or `modules` in every type's table (§2)                                                                                                                                                                                                                                                                                                                                                                                                                               | Proposed             |
| 11  | The built-in integrations are discovered at build time, and an external crate registers through the adapter (§3.1)                                                                                                                                                                                                                                                                                                                                                                                                                 | Proposed             |
| 12  | A page change is one middleware, run only where an ordered `[[fetch]]` or `[[serve]]` entry names it, in two phases (§3.7)                                                                                                                                                                                                                                                                                                                                                                                                         | Proposed             |
| 13  | A vendor crate pins its own dependency versions and releases without a pull request here, and Tech Lab reviews it (§2)                                                                                                                                                                                                                                                                                                                                                                                                             | Proposed             |
| 14  | A module is named by its crate folder, and a page integration is selected from the section of its type, so there is no `[integration]` section (§2)                                                                                                                                                                                                                                                                                                                                                                                | Proposed             |

## Revision record

- 2026-10-06. Merged `main` at 7a0ecb4. Added the module name (§2), build-time
  discovery of the built-in set (§3.1), the reasons for the maintainers
  convention (§2), and page changes as middleware in ordered phase entries
  with a fetch and a serve phase (§3.7, acceptance 5, sign-off rows 10 to
  13). The hook traits leave "What does not change". The seven specs say
  module for what an operator selects, with provider kept for the demand and
  ad server selectors (pluggable spec §2.1).
- 2026-10-07. A module is named by its crate folder, and a page integration
  is selected from the section of its type, so there is no `[integration]`
  section (§2, sign-off row 14). Every type selects with `module` or
  `modules`, which takes the word provider out of the configuration, so the
  auction's tables read `[demand] modules` and `[ad-server] module` and name
  their implementations by module path (§3.4, pluggable spec §2.1). The
  profile ids quoted from `main` in §3.4 are corrected to `standard` and
  `prebid-server`. The response header design names DataDome's settings
  under `[bot-protection.datadome]`. A registration declares its Edge
  Cookie, geo and device modules under a name, and the three selectors read
  that name the way a section does (§3.6). The device trait is
  `DeviceModule`, as the code names it (pluggable spec). §8 gains item 9,
  that a vendor crate cannot declare a secret setting and core's settings
  code names DataDome, and sign-off row 6 says what that means for the
  DataDome move.

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-08-27 | First draft, written against `split/5-response-hook-docs`.                                                                                                                                                                                                                                                                                                                                                                                  |
| 2026-08-27 | Brought the bid renderer contract into scope (§3.4), so that after this change no vendor move needs a Rust core change (§8 row 6). The browser side is recorded as outstanding in §8 item 8.                                                                                                                                                                                                                                                |
| 2026-08-28 | Corrected line references to `main` at b7fcb5d4c and added what mapping `main` found: composition of the served script moves into core (§3.2), registration enumeration and the auction-only `adserver_mock` case (§3.3), the duplicate-id gap (§3.1), the source-file guard and the `ts audit` vendor table (§4), the renderer risk (§7).                                                                                                  |
| 2026-08-28 | Recorded what implementing the seam found (§8): the operator CLI skips a vendor's deploy rules, a carried module's hash literal is fragile, modules resolve more than once per request, and one core reader still reads an APS payload. Recorded the construction-time hash check in §3.2.                                                                                                                                                  |
| 2026-08-28 | Adopted the #1043 review's registration shape for identity and applied its rule to geo and device, with no module built into core (§3.6, §6 item 2, §8 rows 7 to 9). Recorded the relationship to #986 and reordered the series so this spec and its implementation come first.                                                                                                                                                             |
| 2026-08-30 | Moved the five series design specs and the module-code registry into this PR from PRs #1043 to #1047, so every normative document is reviewed before the code that implements it. Document content is unchanged, and only this status line and this row are new.                                                                                                                                                                            |
| 2026-08-31 | Corrected the counts and line references the review found, against `main` at d516a9e94: the source-file guard counts (§4), the prepare and finalize call-site counts (§3.5), the real double geo resolution on `POST /auction` (§8 item 3), the per-adapter preparer coverage (§8 item 5), and the `settings.rs`, `auction/mod.rs` and `publisher.rs` line references. §6 now requires the round trip on Fastly rather than on any adapter. |
| 2026-09-01 | Answered the review finding that generalizing the Rust `BidRenderer` does not move APS out. Recorded the pre-existing browser-side coupling in core TypeScript as §8 item 8, and corrected the APS migration row in §4 to name the browser-side work. Designing a browser-side renderer contract stays out of scope for this stack.                                                                                                         |
