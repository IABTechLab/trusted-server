# Design Spec: The Integration Seam

**Status:** Proposed, 2026-08-27, revised 2026-08-28, 2026-10-06, 2026-10-07, 2026-10-08, 2026-10-12, 2026-10-15 and 2026-10-16.
PR #1342, row 1 of the stack in #777, carries this spec with every design
document of the stack and the URL host linter's allowlist for the modules the
stack ships, and targets `main` directly, so the whole normative set is
reviewable before any of the code lands. The module series follows it in
PRs #1343 to #1347 (rows 2 to 6), then the seam's implementation in PR #1348
(row 7), each PR based on the one before it.
**Author:** 51Degrees (contributed), for Tech Lab review
**Related specs:** `2026-07-30-pluggable-providers-design.md`,
`2026-07-30-provider-migration-rollout-design.md`,
`provider-code-registry.md`
**Related PRs:** #1342 (row 1), #1343 to #1347 (rows 2 to 6), #1348 (row 7), #986 (closed), #1043 to #1047 (closed), #1054 (merged)
**Issues:** #777 (row 1), #784 and #1100 (row 7)
**Last updated:** 2026-10-16

> **Why this spec exists.** PRs #1043 to #1047 open the identity, device and
> geo seams, so a vendor can ship an Edge Cookie module in its own crate
> and an adapter injects it. The nine vendor integrations already inside
> `trusted-server-core` do not sit behind those seams. They hang off the
> integration registry, which is a private table in core, so none of them
> can move out until that table is opened. This spec defines the one core
> change that opens it, so the migration of every existing vendor is a
> single defined piece of work rather than an open question repeated once
> per vendor. The implementation (#1094) has since moved every one of them
> out of core, which §4 records.

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

## What row 7 delivers and what is left

The stack's rows are the numbered branches `split/<n>-...`. Row 7,
`split/7-integration-seam-impl`, is the implementation of this spec (#1094),
and this revision was checked against it at `d3bbc32a5`. A section that
describes something row 7 does not build says which row builds it, or says
that no row does. Sign-off rows are the rows of the table in section 9, and
are named as such.

| Piece                                                                                                                                                        | At row 7                                                                                               | Later rows                                                                                           |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------- |
| The open registry, the duplicate id check, carried browser JavaScript, validation on the builder, the open auction plan and the neutral hooks (§3.1 to §3.5) | Built                                                                                                  |                                                                                                      |
| Every integration module moved out of core (§4) and the settings migration that came with it (§5)                                                            | Built. The settings change is breaking                                                                 |                                                                                                      |
| Edge Cookie, geo and device modules as capabilities of a registration (§3.6)                                                                                 | The three builder methods are built, and the seam probe uses them                                      |                                                                                                      |
| No module built into core, with HMAC and the User-Agent-only device module as crates (§3.6)                                                                  | Not built. Core builds them in, and the adapters still inject the Fastly geo and device modules        | No row through row 50 (`split/50-cookies-by-path`), the latest, moves them                           |
| The stock list of modules discovered at build time (§3.1)                                                                                                    | Not built. The list is written by hand in `crates/trusted-server-modules`                              | No row builds it. Row 43 (`split/43-no-modules-crate`) moves the hand-written list into each adapter |
| Page changes as middleware (§3.7)                                                                                                                            | Not built. The four page hook traits remain, and the settings have no `[[fetch]]` or `[[serve]]` entry | Row 10 (`split/10-page-middleware`)                                                                  |

## 1. The problem, with the code that causes it

Every claim here was read from `main` at b7fcb5d4c (28 August), before the
seventh PR changed these files, so each item describes `main` as it then
was. Line numbers and counts in later sections come from other snapshots of
`main`, which those sections name, and §3.5 and §8 item 3 give the
locations in the seventh PR.

1. **The registry was closed.** `IntegrationRegistry::new` took only
   `&Settings` and iterated a fixed table
   (`crates/trusted-server-core/src/integrations/registry.rs:792`, table at
   `crates/trusted-server-core/src/integrations/mod.rs:290`). Both
   `IntegrationBuilder` and `builders()` were `pub(crate)` with private
   fields, so no adapter and no external crate could add to the list. The
   payload type `IntegrationRegistration`
   (`registry.rs:586`) was already public, so an outside crate could build a
   registration but had nowhere to hand it.
2. **Browser JavaScript was fixed at build time.** `trusted-server-js`
   discovers `lib/src/integrations/*/index.ts`, builds one file per
   integration, and `build.rs` writes a fixed array of `include_str!`
   entries consumed by `bundle.rs`. `IntegrationRegistry::js_module_ids`
   only served a module when
   `trusted_server_js::module_bundle(id).is_some()`
   (`registry.rs:1155`), so an integration outside that compile-time map
   got no script however it registered.
3. **Startup validation named every vendor.** `validate_enabled_integrations`
   imported and called each vendor's config type by name
   (`crates/trusted-server-core/src/config.rs:136` to `:166`).
4. **Demand providers were a second closed table.**
   The list of Prebid, APS and the ad server mock was at
   `crates/trusted-server-core/src/auction/mod.rs:51` to `:53`, inside
   `provider_builders()` at `:49`. `main` has since replaced that table with
   a compiled auction plan, in PR #1016, and §3.4 follows the plan.
5. **Two vendors reached further into core.** DataDome drove cache privacy
   and the origin fetch decision through a marker type
   (`html_processor.rs:303`, `publisher.rs:4369` to `:4381`,
   `publisher.rs:2653`), and GPT diagnostics was called by name from all four
   adapters (for example
   `crates/trusted-server-adapter-fastly/src/app.rs:564`).

The result was that the project carried nine vendors as core code (ten
registered integrations, since GPT registers a proxy and a diagnostics
integration). Tech Lab engineering time was spent on named commercial vendors, and
every new vendor was another core change, as PR #1054 shows.

## 2. Principle

A vendor integration is a module like any other. Core owns the seam and
owns nothing behind it. Concretely:

- Core defines the registration contract and the request pipeline. It names
  no vendor.
- An integration builder is the way an implementation of any module type
  from a crate reaches operator configuration, and core supplies a few
  modules of its own (§3.6). A builder that supplies only an
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
  At row 7 the vendors have not yet taken the crates over. Each of the 23
  crates under `crates/<type>/<name>` names an owner and one of three
  statuses in its manifest, and a test in `trusted-server-modules` requires
  both. Eighteen name `Trusted Server maintainers` with the status
  `seeking vendor owner`, because no vendor has adopted them yet, four name
  them with the status `project owned`, and `permission-signal.mtm` names
  51Degrees with the status `vendor owned`.
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
  `modules`, a list, and one that selects one uses `module`. A crate built
  outside a `crates` directory, such as a published package, has no folder
  to name it, so `module_name!` gives it its package name, and a builder
  can instead state the name its module answers to with
  `with_module_name`.
- Page integrations are selected from the section of their type, such as
  `[cmp]`, `[bot-protection]`, `[framework]`, `[tag]`, `[ad-tag]`,
  `[identity]`, `[audience]` and `[testing]`, each the folder under
  `crates/` that a crate of that type lives in, with `[auction]` and
  `[proxy]` selecting the modules they run beside their own settings, so
  there is no `[integration]` section, and a table its
  section does not select, a section that selects nothing and a name no
  module in the deployment supplies are each refused at startup. A builder
  can name a section other than its folder with `selected_in`, which
  Testlight does, so `testing.testlight` is selected in `[auction]` and not
  in `[testing]`.
- What a selector means, as the code at row 7 reads it.
  - `module` is a string in `[ec]`, `[geo]`, `[device]` and `[ad-server]`,
    and `modules` is a list in `[demand]` and `[permission-signal]`. Every
    other section, `[auction]` and `[proxy]` included, accepts either key
    and at most one of the two.
  - `[ec] module` unset or `none` leaves the deployment stateless. Any
    other name is looked up among core's own modules (`hmac`,
    `host_signals` and, in a build with the `client-fixed-demo` cargo
    feature, `client_fixed`) and then among the modules a registration
    supplies.
  - `[geo] module` unset or `none` resolves no location and makes no host
    geo call, so the permission baseline is the top of the
    `permissions.yaml` rules tree, and a deployment that runs an Edge Cookie
    module without a geo module must also set
    `[geo] assume_single_jurisdiction = true`. `platform` selects the
    adapter's own host lookup, and any other name is looked up among the
    modules a registration supplies.
  - `[device] module` unset or `builtin` classifies a request from the
    User-Agent alone with core's module. `fastly` selects the Fastly
    adapter's host module, and any other name is looked up among the
    modules a registration supplies.

## 3. Design

### 3.1 Opening the registry

Make the builder contract public and give the registry a second input.

- `IntegrationBuilder` becomes `pub` with a public constructor. Core keeps no
  list of vendors. Its own list, `BUILT_IN_BUILDERS` in
  `crates/trusted-server-core/src/integrations/mod.rs`, is written by hand
  and holds only the JavaScript asset proxy outside tests. The modules a
  stock build ships are listed, also by hand and in the order their hooks
  run, by `builders()` in `crates/trusted-server-modules/src/lib.rs`, and
  every adapter and the `ts` tool take that list. The first draft discovered
  the built-in set at build time from the integration directories, in the
  way `trusted-server-js/build.rs` discovers browser modules, so that
  `builders()` would be generated. No row of the stack builds that (§10),
  and row 43 (`split/43-no-modules-crate`) moves the hand-written list into
  each adapter. The list that `build.rs` does generate is the source-file
  guard's, which removes the drift the 31 August review found in
  `migration_guards.rs`, where one integration had no entry (§4). An
  external crate arrives through the registrations an adapter supplies,
  which is the only route for code outside this repository.
- `IntegrationRegistry::with_plan`, the constructor every adapter calls
  since PR #1016, gains a companion,
  `IntegrationRegistry::with_plan_and_registrations(settings, plan, extra)`,
  where `extra` is a slice of externally supplied builders. `with_plan` keeps
  its signature and calls the companion with an empty slice, so no existing
  caller changes behavior.
- Duplicate integration ids are a startup error, naming both sources, so a
  vendor crate cannot silently shadow a built-in. On `main` there was no
  such check, only a per-route conflict check and a debug-only assertion, so
  the builder carries a source label and the registry has the check. Prebid
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
stale script served under a valid-looking URL. The check costs one SHA-256 of
each carried module each time the registry is built. The Axum dev server
builds the registry once, whereas a comment in
`IntegrationRegistry::with_plan_and_registrations` records that Fastly,
Cloudflare and Spin build it on every request. Covering carried modules in
the template cache hash means the publisher entry point needs the
registry, so it takes the configuration and the registry as one argument
rather than two. The served script keeps its cache rule, being the
`?v=<hash>` query matched at serve time (there is no integrity attribute on the tag today, and
this change adds none).

### 3.3 Startup validation on the builder

Replace the named list in `config.rs` with a validation function on the
builder, so a vendor validates its own configuration and a missing vendor
cannot silently stop being validated. The function sits on the builder and
not on the registration, and deploy validation runs it for every builder a
deployment hands over, whether or not a section selects the builder's module
(`validate_integration_blocks` in `config.rs`). The existing test that
asserts every registered integration is covered by deploy validation
(`config.rs:688` on `main`) is rewritten against the builders, so the
guarantee survives in a vendor-neutral form. Two details the map of `main`
adds. The enumeration the test needs is independent of which integrations a
configuration selects, so the registry exposes every builder id it was built
from, not only the ones the sections select (`registered_builder_ids`). And
Prebid Server, APS and `ad-server.mock` are demand and ad server
implementations rather than integrations, so their `[demand.<name>]` and
`[ad-server.<name>]` tables validate through the same
reject-what-you-do-not-know rule the builder's function gives every other
module. A test plants a setting that Prebid Server must reject
(`settings_reject_a_key_the_implementation_does_not_know`) and one that APS
must reject (`deploy_validation_rejects_a_setting_aps_does_not_know`).
`ad-server.mock` has no such test of its own, and neither does
`auction-protocol.openrtb`. Core's `an_ad_server_is_built_from_its_own_table`
plants a setting in a stand-in ad server (`plan.rs`).

### 3.4 Demand and ad server providers

This change opens the auction to demand and ad server implementations from
outside core, through the builder a page integration registers with. An
earlier revision of this section gave `AuctionOrchestrator` a public
provider-builder type and a second input, and a later one recorded the
auction as closed, because `main` had replaced the provider table with a
compiled auction plan in PR #1016. The implementation follows that plan. A
builder supplies an implementation with `with_demand` or `with_adserver`, the
plan compiler resolves each `implementation` line against what the
deployment's builders supply, and the orchestrator and the integration
registry share that one compiled plan.

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
endpoint = "https://aps.example.com/e/pb/bid"
routing = "all_eligible"
account_id = "example-account"
rendering_mode = "publisher_native"

[ad-server]
module = "mock"

[ad-server.mock]
endpoint = "https://adserver.example.com/decide"

[auction]
enabled = true
timeout_ms = 1000

[auction.bidders.example_bidder]
module = "pbs_main"

[auction.bidders.example_eu_bidder]
module = "pbs_eu"
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

The APS and ad server settings in the example are the ones those
implementations insist on. APS needs an `account_id`, takes a
`rendering_mode` of `trusted_server` (the default) or `publisher_native`,
and refuses an endpoint whose path ends in `/e/dtb/bid`, the legacy APS
path, which is why the endpoint ends in `/e/pb/bid`. The mock ad server
needs an `endpoint` in `[ad-server.mock]`, and an `[ad-server]` that selects
`mock` without that table is refused for lacking it. Cloudflare and Spin
refuse more than one demand source while `[auction] enabled` is true
(`AuctionPlan::validate_for_target`,
`crates/trusted-server-core/src/auction/plan.rs:523`), so a deployment on
either lists one source, and the example, which lists three, is for Fastly
and Axum.

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
and the APS renderer type moves out of the shared auction types into APS's
own crate, `crates/auction/aps`. The ad server mock uses the neutral form.

On `main` at 066ea3c69 the plan kept three things closed. The implementation
opens each, because no auction-side vendor could leave core otherwise.

- The set of demand implementations was fixed in core, being the profiles
  `standard`, `prebid-server` and `aps`
  (`crates/trusted-server-core/src/auction/profile.rs:170`), compiled into a
  closed enum (`profile.rs:63`) whose Prebid and APS behavior was imported
  from those modules (`profile.rs:11` and `:12`). It is now whatever the
  deployment's builders supply. Each implementation is a
  `DemandImplementation` its crate declares, and core's own list holds none,
  so `auction-protocol.openrtb`, `auction.prebid-server` and `auction.aps`
  are three crates. Any number of demand providers may still run the same
  implementation, which is what `implementation` expresses.
- The plan compiler treated `prebid-server` and `aps` specially by name
  (`plan.rs:307`, `:585` and `:595`). It names no implementation now. What
  an implementation decides is in its own field policy, endpoint rule and
  request extensions, and a builder that needs the compiled plan registers
  and validates against it through a hook (§8 item 14).
- The only ad server implementation was `adserver_mock` (`plan.rs:20` and
  `:556`), which the orchestrator built by calling that module directly
  (`crates/trusted-server-core/src/auction/mod.rs:90`). An ad server is an
  `AdServerImplementation` a builder supplies, and the mock is the crate
  `ad-server.mock`.

Core's auction engine still builds and reads `OpenRTB` itself. `OpenRTB` as
a translation at the edge of the auction is outside this stack.

### 3.5 The two neutral hooks

- **Response shaping.** DataDome's marker becomes a neutral request
  extension meaning "this response is personalized to the request, do not
  share it", set by any integration. Core keeps the behavior, being the full
  body buffer and private cache, and stops naming a vendor.
- **Request prepare and finalize.** The direct GPT diagnostics calls move
  behind two functions on the builder, a request preparer
  (`with_request_preparer`) and a response finalizer
  (`with_response_finalizer`), so an adapter runs whatever its builders
  declare. The registry collects both from every builder a deployment hands
  over before it checks which modules a section selects, so a preparer runs
  on every request and a finalizer on every response the publisher path
  returns, whether or not a section selects the module. A preparer's error
  ends the request, and each adapter answers with the error's response and
  routes nothing. Those calls reach beyond the adapter edge. On `main` there
  were nine production `prepare_request` call sites in the four adapters
  (two in Fastly, two in Axum, two in Cloudflare and three in Spin) and a
  tenth in core's `handle_publisher_request`. At row 7 there are eight in
  the adapters, because Axum has one, in `execute_handler`, which every
  route but `/health` goes through, and one in core (`publisher.rs:4524`).
  The registry prepares a request once however many times it is asked.
  `finalize_response` has one production call site and it is in core rather
  than in any adapter (`publisher.rs:5472`), so the finalize hook has to run
  on the core response path and not only at the adapter edge.

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
  front. A module a registration supplies reaches those selectors through
  its integration builder's registration. Core also answers to names of its
  own, being `none`, `hmac`, `host_signals` and `client_fixed` for `[ec]`,
  `none` and `platform` for `[geo]`, and `builtin` and `fastly` for
  `[device]`, and §2 says what each means. A `[geo] module` or
  `[device] module` that names a module which supplies no module of that
  type, or a name no running module supplies, is a startup error, and the
  message lists the modules of that type the deployment runs. A
  registration that declares a capability no selector
  names is inert for that capability and its other hooks still run, and
  startup logs a warning naming the unused module, so an operator can see
  a module shipping script for a module that is not selected.
- **No module built into core is the aim, and row 7 does not reach it.**
  Everything would go through one method, so the HMAC identity module from
  #1043 and the User-Agent-only device module from #1044 would be Tech
  Lab-owned crates under `crates/edgecookie/` and `crates/device/`,
  registered by an integration builder, selected by `[ec] module` and
  `[device] module`, and validated through §3.3 like any other
  registration. Neither would ship browser JavaScript and no section would
  select either, because a builder that supplies an implementation does not
  have to be a page integration. Each would take an `[ec.<name>]` or
  `[device.<name>]` table only where it has a setting to carry, which `hmac`
  does and the User-Agent-only classifier does not, and core would keep only
  the seam and the `none` state for each capability. The code at row 7 is
  different, and no row of the stack through row 50
  (`split/50-cookies-by-path`) changes it.
  - Core builds the HMAC module (`hmac`), the host-signal module
    (`host_signals`) and, with the `client-fixed-demo` cargo feature, the
    `client_fixed` demonstration module (`resolve_named_module` in
    `crates/trusted-server-core/src/ec/module.rs`), and the User-Agent-only
    device module (`BuiltinDeviceModule` in
    `crates/trusted-server-core/src/ec/device.rs`). `hmac` and
    `host_signals` each take a `passphrase` in `[ec.<name>]`.
  - `crates/edgecookie/` holds only a README, which says the HMAC module
    ships in core. `crates/device/fastly` and `crates/geo/fastly` are crates
    with no builder, and the READMEs of `crates/device` and `crates/geo` say
    the Fastly adapter injects them.
  - Every adapter still calls `build_geo_module`, which returns the
    adapter's own geo lookup for `[geo] module = "platform"` and no location
    otherwise. The Fastly entry point still calls `build_device_module`,
    which builds the Fastly device module from a closure for
    `[device] module = "fastly"` and returns the User-Agent-only module
    otherwise. Only the Fastly adapter classifies a request (§8 item 11).
  - A deployment that selects no identity implementation is stateless, as
    #986's `module = "none"` already means.
- Composition. The composition root resolves the selected module for
  each capability from the registry once at startup and places it in the
  per-request services, so the request path is unchanged from #1043 and
  #1044. Adapters hand core the Edge Cookie module the registry resolved,
  when the selector names one, and supply none of their own. The first
  draft also had the injected closures in `build_device_module` and
  `build_geo_module` go, and they have not gone (see above), so the host
  defaults are supplied by the adapter as platform services and are consumed
  by whichever implementation is selected, through the request evidence and
  host signal abstractions, exactly as now. A module that needs a host
  signal the running adapter does not expose is rejected at startup, as #986
  requires.
- Consent and privacy at row 7. A preparer of a module no section selects
  still runs on every request (§3.5). The Fastly entry point shows a device
  module a registration supplies the `User-Agent` and `Cookie` headers,
  which carry what a page gathers in the browser, and shows the two modules
  it builds itself no header but the `User-Agent`
  (`derive_device_signals` in
  `crates/trusted-server-adapter-fastly/src/lib.rs`). A device module that
  the selector names and that declares required permissions is refused when
  the registry is built, because device classification runs before the
  request's permissions are known and no per-request gate exists to enforce
  the declaration (`resolve_device_module` in
  `crates/trusted-server-core/src/integrations/registry.rs`).
- A module that declares all three capabilities may share one backend call
  per request across them, which is the shared-backend principle in
  `CLAUDE.md`, and is the case that a split between a registry-attached
  identity module and platform-attached geo and device modules would
  have made impossible.
- The host-signal device module that #1044 ships as a separate crate is a
  module built on platform signals, so the design registers it as a module
  too. At row 7 it does not register, because `crates/device/fastly` has no
  builder. The signals it reads stay platform.

Effect on the series. #1043 and #1044 rework their construction and
selection path onto this section and keep everything else. Moving the HMAC
and User-Agent-only modules into module crates, which this section first
asked of them, is not in the code at row 7 (§10). #1045, #1046 and #1047
are unaffected beyond the rebase.

### 3.7 Page changes as middleware

Row 7 builds none of this section. The first draft specified page middleware
in full, and the code that builds it belongs to a later row.

- At row 7 the four page hook traits, `IntegrationAttributeRewriter`,
  `IntegrationScriptRewriter`, `IntegrationHtmlStreamProcessorFactory` and
  `IntegrationHeadInjector`, are still defined in
  `crates/trusted-server-core/src/integrations/registry.rs` and exported by
  `integrations/mod.rs`. Every selected module's hooks run on every page, in
  the one order the modules registered in, and the settings have no
  `[[fetch]]` or `[[serve]]` entry.
- Row 10 (`split/10-page-middleware`) builds the replacement and removes the
  four traits. A page change becomes one middleware that a registration
  supplies, run only where an ordered `[[fetch]]` or `[[serve]]` entry names
  it. Fetch is obtaining the document from the origin, and its output is what
  a shared template stores for every reader. Serve is preparing the document
  for one reader's response, and nothing it writes is stored.
- `2026-10-12-page-middleware-design.md` specifies the contract, the entries
  and the conversion of each module. It differs from the first draft of this
  section in five places, being the middleware's name, where its settings
  sit, a fifth part of the contract, the count of implementations converted
  and which of them are serve middleware, and where serve markup goes. This
  section therefore no longer repeats the contract, and that document is the
  one to read.

## 4. Migration of the existing vendors

The implementation (#1094) moves every integration module out of core, one module per commit, so each move can be read and reverted on its own. A module's Rust, its settings type, its deploy rules and its tests move into `crates/<type>/<vendor>`, and the module is named by that folder (§2). Each crate carries a visible maintainers declaration, the way Prebid.js requires of every adapter, so the boundary has a named owner from its first day. `crates/trusted-server-modules` lists the modules a stock build ships, in the order their hooks run, and every adapter and the `ts` tool take that list (row 43, `split/43-no-modules-crate`, later removes that crate and gives each adapter its own list). A vendor's browser script stays in `crates/trusted-server-js` for now (§8 item 8).

| Module                                                                               | What its move needed                                                                                                                                                                                                                                                                                                                                                                                            |
| ------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Didomi, Google Tag Manager, Lockr, Osano, Permutive, Sourcepoint, Testlight, Next.js | Moved as they are. Coupled only through the builder table, deploy validation and the JS map.                                                                                                                                                                                                                                                                                                                    |
| Google Publisher Tags (the `gpt` proxy and its diagnostics)                          | The proxy moved as it is. The diagnostics half needed a module to act on one request without core naming it (§3.5, §8 item 12).                                                                                                                                                                                                                                                                                 |
| DataDome                                                                             | Needed the same request state (§3.5, §8 item 12), and a module's own declaration of its secret settings (§8 items 9 and 13).                                                                                                                                                                                                                                                                                    |
| APS                                                                                  | A demand implementation rather than a page integration, so it is configured under `[demand.<name>]`, carries its `rendering_mode` there, and no section selects it. Needed the open renderer descriptor and the open auction plan of §3.4, and a registration from the compiled plan for the page support its renderer needs (§8 item 14). Core TypeScript still imports the APS renderer directly (§8 item 8). |
| Prebid and Prebid Server                                                             | One file on `main`. It was divided inside core into the page integration and the demand implementation, which share nothing, and the two moved as two crates. The page integration registers and validates against the plan as APS does (§8 item 14).                                                                                                                                                           |
| The mock ad server                                                                   | An ad server implementation a builder supplies (§3.4).                                                                                                                                                                                                                                                                                                                                                          |
| The plain `OpenRTB` demand                                                           | Moved as it is. The reader of an ordinary `OpenRTB` response stays in core's driver, for any implementation to use.                                                                                                                                                                                                                                                                                             |

A vendor's `[<type>.<name>]` table needs no change when the vendor moves,
because the sections are read into `TypeSections` in
`crates/trusted-server-core/src/settings.rs`, which accepts a name core
does not know. Which vendors run is the section of each vendor's type, and
a table its section does not select refuses startup like any other stray
module table. The move of every table out of `[integrations]` is separate
from a vendor leaving core, and §5 records it.

Two more places every move must touch, found by mapping `main`:

- `crates/trusted-server-core/src/migration_guards.rs` embeds core source
  files by relative path with `include_str!`, so a vendor move that leaves
  an embedded entry behind breaks the build rather than a test. On `main`
  the directory `crates/trusted-server-core/src/integrations/` holds 23
  `.rs` files, being 2 infrastructure files (`mod.rs` and `registry.rs`),
  6 files in the `nextjs/` subdirectory, 2 in the `datadome/` submodule and
  13 top-level integration modules. The guard embeds 20 of those 23, and 9
  of the 20 are files of the nine vendors `main` holds. The three it
  does not embed are `osano.rs` and the two `datadome/` files, so the guard
  is already incomplete and the Osano move has no guard entry to delete.
  Separately, `builders()` registers 13 integrations, which is not the same
  13 as the file count, because `adserver_mock` is a file with no
  registration while `nextjs` is a registration held in a subdirectory. The
  guard cannot derive its list from the registrations, because
  `include_str!` paths are fixed at compile time, so the implementation
  generates the list in `build.rs` from every `.rs` file under `src`. A file
  that leaves core then leaves the guard with no edit, and a module crate is
  outside the core neutrality guarantee.
- The `ts audit` command carries its own vendor table (detection patterns
  and configuration section names in
  `crates/trusted-server-cli/src/commands/audit/analyzer.rs` and
  `commands/audit/mod.rs`). It is outside the registry and outside this
  change. The detection patterns stay in the CLI. What the audit writes for
  a module it detects, being the section that selects the module and its
  name there, comes from the list of stock modules, so the CLI names no
  module's section. How the CLI learns a vendor's detection pattern from a
  crate is a follow-up this spec records but does not solve.

## 5. What does not change, and what does

**Does not change.** The request pipeline, the served script format and its
hash, the permission model, and the identity lifecycle, envelope and
validation contracts from #986 as implemented in PRs #1043 to #1046. No
integration changes behavior once its settings are written in the new
shape.

**What changes is the settings.** The seventh PR is a breaking change to the
settings, recorded as Breaking in `CHANGELOG.md`, which also holds the full
migration table. A configuration still written the old way is refused at
startup and by `ts config push`, with a message that says where the setting
went. That covers:

- `[integrations]` and `[integration]`. Each page integration is a module
  selected in the section of its type, as `[<type>] module = "<name>"` or
  `modules = ["<name>", ...]`, with its settings in `[<type>.<name>]`. So
  `[integrations.gpt]` is `[ad-tag.google]` and `[integrations.prebid]` is
  `[auction.prebid]`.
- `[adserver]`, now `[ad-server]`, and `[auction] mediator`, now
  `[ad-server] module`.
- `[permission_signal]`, now `[permission-signal]`, and its `sources`, now
  `modules`.
- `[auction] providers`, now `[demand] modules` with a `[demand.<name>]`
  table for each source. A name that carried a hyphen, such as `pbs-main`,
  becomes snake_case, and so does every `[auction.bidders.<code>] module`
  value that points at one.
- Environment overrides, which follow the new names, so
  `TRUSTED_SERVER__INTEGRATIONS__GPT__GAM_ATTRIBUTION_ENABLED` is
  `TRUSTED_SERVER__AD-TAG__GOOGLE__GAM_ATTRIBUTION_ENABLED`. Secret
  references move with their tables.

**Compatibility and rollback.** The old and new settings blobs are mutually
incompatible. This binary refuses the old shape, and a binary from before the
change refuses the new one, because its settings reject sections it does not
know. So the new binary and a blob in the new shape are activated together,
and a rollback restores the old binary and the old-schema blob together. On
Fastly the blob sits in a config store apart from the compiled service, so
`ts config push` and a deploy are separate steps, and between them a service
whose binary refuses the blob answers every request with its startup error
(`startup_error_router` in `crates/trusted-server-adapter-fastly/src/app.rs`).

## 6. Acceptance

1. **A round trip with a non-default implementation, on Fastly.** A test
   integration defined outside `trusted-server-core`, carrying its own
   JavaScript, registers through an adapter, appears in the served bundle
   with the right hash, runs its hooks in the right order, and is rejected
   on a duplicate id. A seam is only proven by an implementation that is
   not the built-in one. The round trip has to run on the Fastly adapter,
   which is the primary deployment target, and not only on the Axum dev
   server, because a seam proven on the dev server alone is not a seam any
   production deployment can use. The Fastly adapter is a library with a
   thin binary for that reason, and a deployment's own binary passes its
   builders to `run_with` (§8 item 7). On Fastly and on Axum the seam probe
   (`crates/testing/seam-probe`) shows its carried module in the served
   bundle under the composed hash, a request prepared exactly once on a named
   route and on the fallback, and a duplicate id refused naming both
   sources. The order two modules' hooks run in is tested in the registry
   (`prepare_request_runs_every_preparer_in_registration_order_named_or_not`
   and
   `finalize_response_runs_every_finalizer_in_registration_order_with_the_request_state`),
   and no adapter test registers two modules and checks that order.
2. **Capabilities round trip, on the same adapter.** The same test
   integration declares an identity, a geo and a device module. With the
   three selectors naming it, each resolves the module the integration
   declares, and a request to the probe's report route returns the country
   its geo module resolved. Its identity module is driven through `generate`
   and creates an identifier the HMAC module cannot, and on Fastly its device
   module is driven through `derive_device_signals`. On Axum the device
   module is resolved by name and no request is classified, because only the
   Fastly adapter classifies one (§8 item 11). No one request is served by
   all three modules, so the acceptance as first written, one request
   showing the created identifier, the resolved country and the device
   signals together, is not shown (§10). With `[geo] module` naming a
   module that lacks a geo module, startup fails with an error that names
   the module and the capability, which the adapter tests show.
3. **Parity.** The existing integration and parity suites pass against
   settings fixtures written in the new shape, because the stock set
   registers through the same path. They do not run on the fixtures they
   had, because the settings change of §5 rewrote them, among them the
   integration tests' `trusted-server.integration.toml` in
   `crates/trusted-server-integration-tests/fixtures/configs/`.
4. **No vendor left behind.** The rewritten deploy-validation test shows
   every registered integration validates its configuration.
5. **Page changes through the contract.** Not shown at row 7, which builds
   no page middleware (§3.7). Row 10 (`split/10-page-middleware`) builds it,
   and its acceptance is in `2026-10-12-page-middleware-design.md`.
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
and the wire shape `{"type":"aps", ...}` must survive byte for byte. The
third risk is the settings change (§5), which is breaking, so a binary and a
settings blob in different shapes stop a service starting, and the two are
activated together. Doing this once is what removes the per-vendor core change
that the project pays for today, most recently in PR #1054.

## 8. What implementing this found

A probe integration built outside `trusted-server-core` and registered
through an adapter exercised every seam end to end. Eight things surfaced
that reading the code did not, and three more came from reading the
settings loader and the Fastly entry point for what a vendor with a secret,
an ad server or a device module would need. Moving every module out of core
then found four more, items 12 to 15, and checking this section against the
code of the seventh PR added item 16. They are recorded here rather than left
for each vendor to rediscover.

1. **A vendor's own deploy rules do not run through the operator CLI.**
   `ts config validate` and `ts config push` reach validation through
   `TrustedServerAppConfig`, which supplies no builders, so a vendor's
   `[<type>.<name>]` rules are skipped on exactly the path an operator
   uses. The validation function in §3.3 is only real once that path can carry
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
3. **A module is resolved more than once per request.** On `main` this was
   core against core rather than a proxy against the request path, and it
   still is on the auction route. Every adapter resolves geo once to build
   the EC context, in `EcContext::read_from_request_resolving_geo`
   (`crates/trusted-server-core/src/ec/mod.rs:372`), which `build_ec_context`
   calls (the function is defined at `crates/trusted-server-adapter-axum/src/app.rs:336`,
   `crates/trusted-server-adapter-cloudflare/src/app.rs:402` and
   `crates/trusted-server-adapter-spin/src/app.rs:585`), and
   `build_ec_request_state` calls (defined at
   `crates/trusted-server-adapter-fastly/src/app.rs:614`). On
   `POST /auction` the same request then reaches `handle_auction`, which
   looks the same client IP up a second time to fill in the auction's
   device info (`crates/trusted-server-core/src/auction/endpoints.rs:334`).
   The auction route is not the only place a location is looked up twice.
   The Didomi proxy looks it up for the notice loader when
   `geo_query_parameters` is set (`crates/cmp/didomi/src/lib.rs:475`), and
   the Fastly finalize paths look it up for every response but a 401, to
   write the response headers (`crates/trusted-server-adapter-fastly/src/lib.rs:714`
   and `crates/trusted-server-adapter-fastly/src/middleware.rs:104`).
   `CLAUDE.md`'s principle that a vendor sharing one backend makes a single
   call per request needs a per-request module context to hang that on,
   which this change does not introduce.
4. **One core reader reached into a vendor's payload.** The `hb_adid`
   fallback in the publisher read the APS renderer's fields by APS's own
   names, so the APS migration needed a neutral answer for it rather than
   only the renderer contract in §3.4. A renderer descriptor now carries the
   name of the field that holds its bid id, stated by the implementation
   that builds it, and the publisher asks the descriptor for that id.

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

6. **A module's validate function runs only where a deployment hands the
   settings load its builders.** Building the registry calls only a
   builder's build function, and the operator CLI calls deploy validation
   without the builders. Validation at startup runs while the settings
   load, and the implementation (#1094) gives the load a form that takes a
   deployment's builders, so a vendor's `validate` runs at startup for a
   deployment that loads its settings with them. The probe still repeats
   its check inside its build function, which covers a deployment that does
   not. The operator path is item 1, and until it carries the builders §3.3
   is delivered at startup and not when an operator pushes.
7. **The Fastly adapter takes a vendor crate through `run_with`.** It was
   a binary only and its `build_state_with_registrations` was
   crate-private, so composing a module into a Fastly deployment meant
   editing the adapter. The implementation (#1094) makes it a library with
   a thin binary. `run_with(Vec<IntegrationBuilder>)` records the builders
   a deployment offers before any request is served, the settings load
   validates against them, and the round trip of §6 items 1 and 2 runs on
   it under Viceroy. No test loads settings from a config store with
   registered builders on that adapter, so that one call is covered by the
   tests of the load in core and not by a test of its own.
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
   points outside the module's own table. The DataDome move in §4 needed
   it first, and item 13 is what was built.
10. **A deployment hands its builders to the settings load as well as to
    the state build.** The settings are validated as they load, and that
    validation compiles the auction plan. With the built-in implementations
    alone it refuses a `[demand]` or `[ad-server]` name one of the
    deployment's own builders supplies, before any adapter state is built.
    The implementation (#1094) carries the builders through the load and
    through deploy validation, and has the probe supply a demand source and
    an ad server, so the auction seam is reached from a crate core does not
    know. No auction is driven through either, so what an implementation
    outside core does during an auction is held by the tests of the built-in
    ones.
11. **A device module is asked on Fastly alone.** Only the Fastly adapter
    classifies a request and sets device signals, on `main` as in this
    stack, so a device module a crate supplies runs there and on no other
    adapter, where `[device] module` resolves it and nothing asks it. On
    Fastly the entry point derives the signals before the request reaches
    the application, so the implementation (#1094) hands it the module the
    registry resolved. Acceptance item 2 asks that a request's device
    signals be the test integration's, which can therefore be shown on
    Fastly and not on the dev server.
12. **A module that acts on one request needs somewhere to leave what it
    decided.** The diagnostics half of Google Publisher Tags and DataDome
    each decide something about one request before it is routed and act on
    it when the page is written. A request preparer or filter leaves a value
    on the request under its integration id. The page path carries the value
    to the module's own hooks in the document and to a response finalizer
    the builder declares. A request that carries one keeps to the origin
    path and its HTML is private, so it is never served from or stored as a
    shared template. This is the neutral form of both hooks in §3.5.
13. **A builder declares its module's secret settings.** This closes item 9. A builder lists the settings in its own table that hold the name of a
    secret, each with a rule for whether the table as written puts the
    setting to use. As the settings load, a setting in use in a selected
    module's table is looked up and the others are cleared, and deploy
    validation checks that each one in use names a key. Core's settings code
    names no vendor's secret.
14. **A module whose page support follows the auction plan registers from
    the plan.** Prebid's page integration and the page support APS's
    renderer needs are wanted when the plan selects them, whatever a section
    says. A builder can register from the compiled plan, with its hooks
    ahead of every module a section selects, and can check its settings
    against the plan when a deployment is validated and as the settings
    load. The registry named Prebid and APS before this.
15. **Every process that validates or loads settings needs the builders.**
    Items 6 and 10 found this for the settings load. Moving the demand
    implementations found it three more times, in the integration tests'
    Viceroy configuration generator, in their own configuration and in two
    of the CLI's tests, each of which validated a configuration in a process
    that had been handed no builders and so refused an implementation the
    build ships. A path that takes no builders knows core's own modules
    alone, which after the moves is the JavaScript asset proxy. The `ts`
    tool registers the modules a stock build ships before it validates, so
    their rules run when an operator validates or pushes. A deployment's own
    crate is still unknown to the stock tool, which is the decision item 1
    asks for.
16. **Cloudflare and Spin take no deployment crate in production, and no
    round trip runs on them.** Their production entries build the state from
    the stock list alone (`build_state` in each adapter's `app.rs`). Both
    have `routes_with_registrations`, which takes a deployment's builders,
    and neither has a seam probe test, so the round trip of §6 items 1 and 2
    runs on Fastly and Axum only. Both refuse more than one demand source
    while `[auction] enabled` is true, so the example in §3.4 cannot run on
    them. The registry comment that records the cost of the carried module's
    hash check (§3.2) says both build the registry on every request.

Items 1 and 6 are the ones a vendor meets on its first day, and item 9
joined them for a vendor with a secret until item 13. Item 5 is the one
that produces a bug report nobody can reproduce, because whether it appears
depends on which host the reporter runs.

Taken together these say the seam is proven but not yet finished. A vendor
can register a module, ship its browser code, declare a geo module and
serve a route, all from its own crate and proven end to end on Axum and
on Fastly. Its own configuration rules are enforced at startup only for a
deployment that loads its settings with its builders, and when an operator
pushes only for the modules a stock build ships. That is a small change
against what this document already defines, and it should land before the
first vendor outside this repository is asked to use it.

## 9. Sign-off

| #   | Decision                                                                                                                                                                                                                                                                                                                                                                                   | Status               |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------- |
| 1   | Vendor integrations belong outside core, behind the registration contract                                                                                                                                                                                                                                                                                                                  | Proposed             |
| 2   | Tech Lab engineering reviews vendor crates, and does not maintain them                                                                                                                                                                                                                                                                                                                     | Proposed, governance |
| 3   | A registration may carry its own browser JavaScript                                                                                                                                                                                                                                                                                                                                        | Proposed             |
| 4   | Deploy validation moves onto the builder                                                                                                                                                                                                                                                                                                                                                   | Proposed             |
| 5   | Every integration module in core migrates inside the implementation (#1094), one commit each, as §4 records                                                                                                                                                                                                                                                                                | Proposed             |
| 6   | This change completes the Rust side for every kind of module, so a vendor's module needs no Rust core change, whether it is a page integration, holds a secret (§8 item 13) or is an auction implementation (§3.4). The browser side is not complete, because TSJS core still imports the APS renderer directly (§8 item 8), so APS's browser code still needs a browser renderer contract | Proposed             |
| 7   | Identity, geo and device modules are capabilities of a module registration (§3.6), the #1043 review's rule applied to all three                                                                                                                                                                                                                                                            | Proposed             |
| 8   | No module built into core is the aim, with the HMAC and User-Agent-only device modules as Tech Lab-owned crates registered by an integration builder and selected by `[ec] module` and `[device] module`, neither being a page integration, and core keeping only `none`. Row 7 does not build it, and no row through row 50 does (§3.6)                                                   | Proposed             |
| 9   | This spec and its core implementation precede #1043, so 51Degrees implements the core seam and the moves in §4                                                                                                                                                                                                                                                                             | Proposed             |
| 10  | What an operator selects is a module, selected with `module` or `modules` in every type's table (§2)                                                                                                                                                                                                                                                                                       | Proposed             |
| 11  | An external crate registers through the adapter, and the stock modules are listed once, by hand and in hook order, in `crates/trusted-server-modules`. Discovery of that list at build time is not built (§3.1)                                                                                                                                                                            | Proposed             |
| 12  | A page change is one middleware, run only where an ordered `[[fetch]]` or `[[serve]]` entry names it, in two phases. Row 10 builds it, and `2026-10-12-page-middleware-design.md` specifies it (§3.7)                                                                                                                                                                                      | Proposed             |
| 13  | A vendor crate pins its own dependency versions and releases without a pull request here, and Tech Lab reviews it (§2)                                                                                                                                                                                                                                                                     | Proposed             |
| 14  | A module is named by its crate folder, and a page integration is selected from the section of its type, so there is no `[integration]` section (§2)                                                                                                                                                                                                                                        | Proposed             |

## 10. Open questions

These wait for a decision and for a code change, and this revision changes no
code.

1. Whether the HMAC, host-signal and `client_fixed` Edge Cookie modules and
   the User-Agent-only device module move out of core into crates under
   `crates/edgecookie/` and `crates/device/`, as §3.6 first asked, and in
   which row. The same design has `crates/device/fastly` and
   `crates/geo/fastly` register through builders, and the adapters stop
   calling `build_device_module` and `build_geo_module`.
2. Whether the stock list is generated at build time from the crate
   folders, as the browser modules are, or stays hand-written, as it is in
   `crates/trusted-server-modules` at row 7 and in each adapter from row 43.
3. Whether the Cloudflare and Spin production entries take a deployment's
   own builders, as the Fastly entry does through `run_with`, and whether
   they get seam probe tests of their own (§8 item 16).
4. Whether a test serves one request with the Edge Cookie, geo and device
   modules of one registration selected, which acceptance 2 asked for as
   first written.
5. Who maintains the 18 module crates that name `Trusted Server maintainers`
   with the status `seeking vendor owner` until a vendor adopts them, given
   that §2 says Tech Lab engineering does not maintain vendor crates.

## Revision record

| Date       | Change                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| ---------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2026-08-27 | First draft, written against `split/5-response-hook-docs`.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| 2026-08-27 | Brought the bid renderer contract into scope (§3.4), so that after this change no vendor move needs a Rust core change (§8 row 6). The browser side is recorded as outstanding in §8 item 8.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| 2026-08-28 | Corrected line references to `main` at b7fcb5d4c and added what mapping `main` found: composition of the served script moves into core (§3.2), registration enumeration and the auction-only `adserver_mock` case (§3.3), the duplicate-id gap (§3.1), the source-file guard and the `ts audit` vendor table (§4), the renderer risk (§7).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| 2026-08-28 | Recorded what implementing the seam found (§8): the operator CLI skips a vendor's deploy rules, a carried module's hash literal is fragile, modules resolve more than once per request, and one core reader still reads an APS payload. Recorded the construction-time hash check in §3.2.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| 2026-08-28 | Adopted the #1043 review's registration shape for identity and applied its rule to geo and device, with no module built into core (§3.6, §6 item 2, §8 rows 7 to 9). Recorded the relationship to #986 and reordered the series so this spec and its implementation come first.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| 2026-08-30 | Moved the five series design specs and the module-code registry into this PR from PRs #1043 to #1047, so every normative document is reviewed before the code that implements it. Document content is unchanged, and only this status line and this row are new.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| 2026-08-31 | Corrected the counts and line references the review found, against `main` at d516a9e94: the source-file guard counts (§4), the prepare and finalize call-site counts (§3.5), the real double geo resolution on `POST /auction` (§8 item 3), the per-adapter preparer coverage (§8 item 5), and the `settings.rs`, `auction/mod.rs` and `publisher.rs` line references. §6 now requires the round trip on Fastly rather than on any adapter.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| 2026-09-01 | Answered the review finding that generalizing the Rust `BidRenderer` does not move APS out. Recorded the pre-existing browser-side coupling in core TypeScript as §8 item 8, and corrected the APS migration row in §4 to name the browser-side work. Designing a browser-side renderer contract stays out of scope for this stack.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| 2026-10-06 | Merged `main` at 7a0ecb4. Added the module name (§2), build-time discovery of the built-in set (§3.1), the reasons for the maintainers convention (§2), and page changes as middleware in ordered phase entries with a fetch and a serve phase (§3.7, acceptance 5, sign-off rows 10 to 13). The hook traits leave "What does not change". The seven specs say module for what an operator selects, with provider kept for the demand and ad server selectors (pluggable spec §2.1).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| 2026-10-07 | A module is named by its crate folder, and a page integration is selected from the section of its type, so there is no `[integration]` section (§2, sign-off row 14). Every type selects with `module` or `modules`, which takes the word provider out of the configuration, so the auction's tables read `[demand] modules` and `[ad-server] module` and name their implementations by module path (§3.4, pluggable spec §2.1). The profile ids quoted from `main` in §3.4 are corrected to `standard` and `prebid-server`. The response header design names DataDome's settings under `[bot-protection.datadome]`. A registration declares its Edge Cookie, geo and device modules under a name, and the three selectors read that name the way a section does (§3.6). The device trait is `DeviceModule`, as the code names it (pluggable spec). §8 gains item 9, that a vendor crate cannot declare a secret setting and core's settings code names DataDome, and sign-off row 6 says what that means for the DataDome move. §8 item 6 says where a vendor's validate function now runs, and item 10 records that a deployment hands its builders to the settings load. Item 11 records that a device module is asked on Fastly alone. Acceptance item 1 and §8 item 7 say the Fastly adapter is a library that takes a deployment's builders through `run_with`, and that the round trip runs on it. |
| 2026-10-08 | The implementation moved every integration module out of core, so the passages that described the moves as work to come say what was built. Vendor crates sit under `crates/<type>/<vendor>` (§3.6, §4), where earlier revisions said `crates/integrations/<vendor>`. The auction is open to demand and ad server implementations from a crate (§3.4), where the previous revision recorded it as closed. §4 says what each module's move needed, and that the moves are one commit each inside the implementation where earlier revisions planned one pull request each after it (sign-off rows 5, 6 and 9). The source-file guard's list is generated, and the audit takes a module's section from the list of stock modules (§4). Acceptance item 3 names the stock set. §8 item 4 says how the publisher reads a renderer's bid id now. §8 gains items 12 to 15, being request state for a module, a module's own secret settings, registration from the auction plan, and the builders every validating process needs.                                                                                                                                                                                                                                                                                                                                                                               |
| 2026-10-12 | One design document for each of the stack's pull requests above the seam, the 23 files dated 2026-10-12 in this folder, so this pull request carries every design document of the stack in one place. Each names the fork pull request that implements it and the Tech Lab issue it answers, and is written from both as they stand.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| 2026-10-15 | Fixes from review round 1, checked against `split/7-integration-seam-impl` at `d3bbc32a5`. Added a table of what row 7 delivers and what is left, the owners of the stock crates, what each selector means when unset, and the open questions (§10). Said what core builds in and what the adapters still inject (§3.6, sign-off row 8), that the stock list is written by hand (§3.1, sign-off row 11), the migration, compatibility and rollback of the settings change (§5, §7), that page middleware is the work of row 10 (§3.7, acceptance 5, sign-off row 12), and what the tests show (acceptance 1 to 3). Corrected the settings example (§3.4), the validation and hook functions, which sit on the builder (§3.3, §3.5), and the locations and counts (§3.5, §8 item 3). Added the facts on consent, failure, Cloudflare and Spin (§3.5, §3.6, §8 item 16) and the review log. Wrote §1 in the past tense and put the revision record in date order as one table.                                                                                                                                                                                                                                                                                                                                                                                                                              |
| 2026-10-16 | Review round 2, approved, with the section 8 line citations described as where the builders are defined.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |

## Review log

1. **Round 1, 2026-10-15.** A review in a fresh session, against `split/7-integration-seam-impl` at `d3bbc32a5`. Issues found, five of them blocking. The five were that §3.6 and sign-off row 8 said no module is built into core when the HMAC module, the host-signal module and the built-in device module are, that §3.1 and sign-off row 11 said the built-in set is discovered at build time when two hand-written lists name it, that the §3.4 settings example failed startup four ways, that §5 listed every settings table as unchanged with no migration, compatibility or rollback text when the branch refuses the old tables, and that §3.7, acceptance 5 and sign-off row 12 described page middleware the branch lacks. This revision states what the code does in each case, names row 10 as the row that builds page middleware, corrects the settings example and adds the migration and rollback text, and leaves the move of the built-in modules into crates and the build-time discovery of the stock list under Open questions, because both need code changes.
2. **Round 2, 2026-10-16.** A lighter review in a fresh session, checking the round 1 fixes against `split/7-integration-seam-impl`. Approved, none blocking. This revision says the adapter lines cited in section 8 are where the two context builders are defined, and the device crate README's wording is left as a code follow-up.
