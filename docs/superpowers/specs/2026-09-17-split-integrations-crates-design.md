# Split Integrations into Dedicated Crates and One Ordered Configuration

**Date:** 2026-09-17

**Status:** Proposed

**Scope:** Move all concrete Rust and browser integrations into two statically
compiled workspace crates, introduce typed multi-capability registration, and
make ordered `[integrations]` configuration the single inventory for concrete
integrations and their auction providers.

## Summary

Trusted Server will separate concrete integrations from its neutral Rust and
browser runtimes.

The workspace gains two crates:

- `trusted-server-integrations`, containing all concrete Rust integrations.
- `trusted-server-integrations-js`, containing all integration-specific
  TypeScript, JavaScript assets, fixtures, tests, and generated bundles.

`trusted-server-core` will retain neutral integration contracts and execution
engines. `trusted-server-js` will retain the neutral browser runtime. The
adapters and CLI will use `trusted-server-integrations` as the statically linked
application composition root.

Each Rust integration will expose one definition that may register multiple
typed capabilities. APS, for example, may register proxy, head-injection,
JavaScript, and OpenRTB-profile capabilities. Capabilities are subsystem-owned
types and traits, not a single `IntegrationType` enum and not a discriminator
in operator configuration.

`[integrations]` will become the only operator inventory for concrete
integrations. An integration will own all of its settings, including any named
auction provider instances. Global `[auction]` settings will continue to own
cross-integration orchestration such as the auction timeout, bidder routing,
creative policy, and mediator selection.

Runtime order will come exclusively from TOML declaration order. The config
push and config-store representation will preserve that order explicitly;
filesystem discovery order will never affect execution. For auction providers,
that order is also operational priority: it controls launch and response order,
mediator input order, and local equal-price tie-breaking.

This design intentionally changes the auction configuration introduced by PR
#1016 while preserving that work's compiled-plan and runtime guarantees. It
adopts only the typed-registration portion of PR #1084 and excludes that PR's
external provider ecosystem and unrelated provider systems.

## Context

On `main` at `6cae7f5da`, neutral registry machinery and concrete integrations
share `crates/trusted-server-core/src/integrations`. The concrete Rust units
are:

- `adserver_mock`
- `aps`
- `datadome`
- `didomi`
- `google_tag_manager`
- `gpt`
- `gpt_diagnostics`
- `js_asset_proxy`
- `lockr`
- `nextjs`
- `osano`
- `permutive`
- `prebid`
- `sourcepoint`
- `testlight`

The ordinary builder table registers twelve units. APS and Prebid are also
constructed from the compiled auction plan, and `adserver_mock` supplies the
current mediator. Core additionally imports concrete APS and Prebid code from
the OpenRTB profile and provider paths, concrete DataDome response state, and
GPT diagnostics lifecycle functions.

Integration browser code shares a Node project with browser core under
`crates/trusted-server-js/lib/src/integrations`. The build discovers
directories containing `index.ts`, emits an IIFE for each entry point, and
embeds bundles and hashes into the `trusted-server-js` Rust crate. APS renderer
code is imported directly by browser core even though APS does not currently
have its own `index.ts`.

Configuration is also split by implementation detail. Browser/page settings
use `[integrations.<id>]`, while server auction providers use
`[auction.providers.<instance>]` plus `profile = "aps"` or
`profile = "prebid-server"`. One logical APS integration is therefore
configured in two inventories and may be activated implicitly by an auction
plan. `IntegrationSettings` currently uses `HashMap`, so integration
declaration order is discarded before registry construction.

These conditions produce five related problems:

1. Core owns both neutral contracts and concrete implementations.
2. Rust registration, auction profiles, deploy validation, and migration guards
   maintain separate concrete inventories.
3. Browser core imports integration-specific code.
4. One logical integration can be configured and activated through unrelated
   locations.
5. Runtime integration order is not a stable property of the operator
   configuration.

## Relationship to Existing Work

### PR #1016

PR #1016 made auction providers configuration-driven and introduced a single
validated, immutable auction plan shared by adapter backend construction,
runtime dispatch, browser demand, routing, and telemetry. It also separated a
configured provider instance from the OpenRTB profile implementation it uses.

This design preserves those runtime guarantees:

- Multiple configured instances may use one integration implementation.
- One validated provider identity remains shared by bidder routing, backend
  correlation, diagnostics, and telemetry; this design changes its serialized
  value from a local ID to a qualified ID.
- One validated plan remains authoritative across every adapter and runtime
  consumer.
- The generic OpenRTB transport remains shared.
- Existing routing, timeout, notification, response admission, mediation, and
  telemetry attribution behavior remains unchanged except where provider order
  is observable.

This design changes the configuration location and identity spelling. Provider
instances move below their owning integration, and cross-integration references
use a qualified `<integration>.<instance>` identifier. It also intentionally
changes deterministic provider priority from lexical provider-ID order to
operator declaration order. The pricing algorithm is unchanged, but the first
configured provider retains an equal-price tie and later providers receive the
remaining shared auction budget after earlier providers launch.

### PR #1084

PR #1084 proposes a much broader compile-time provider ecosystem: public
registration for external vendor crates, provider systems for identity, geo,
device, permission signals, demand and ad servers, a permission/jurisdiction
model, client-cycle EC resolution, provider-code governance, adapter and
EdgeZero composition work, and independent vendor ownership expectations.

This design shares one idea with that proposal: one integration may register
multiple typed capabilities. It does not create the external ecosystem. The
new contracts serve the integrations compiled in this workspace; they are not
a stable third-party SDK or independent release boundary.

## Goals

1. Move every concrete Rust integration implementation out of core and into one
   `trusted-server-integrations` crate.
2. Move all integration-specific browser sources and artifacts into one
   `trusted-server-integrations-js` crate.
3. Give each concrete integration one Rust directory and, when applicable, one
   same-named browser directory.
4. Discover Rust and browser integration inventories from directories at build
   time.
5. Let one integration register multiple typed capabilities without a global
   integration-kind enum.
6. Remove concrete integration construction, auction-profile, validation, and
   lifecycle imports from core.
7. Make `[integrations]` the single ordered inventory for concrete integration
   configuration, including auction providers.
8. Preserve the runtime behavior and compiled-plan guarantees of PR #1016,
   except that deterministic provider priority moves from lexical ID order to
   configuration order.
9. Make core compile without depending on either integrations crate.
10. Keep all integrations statically linked; no runtime loading is introduced.

## Non-Goals

This design does not introduce:

- One Cargo crate per vendor.
- External vendor crate injection or adapter-supplied registration.
- Runtime-loaded integrations, dynamic linking, or an ABI.
- A stable public plugin or integration SDK.
- Independent vendor release, compatibility, security-response, or governance
  policies.
- Identity, EC, geo, device, or permission-signal provider systems.
- A jurisdiction or permission-policy redesign.
- Client-cycle EC resolution or provider-code allocation.
- Upstream EdgeZero lifecycle, host-evidence, store, or adapter changes.
- New auction protocols, pricing algorithms, notification policies, or
  telemetry schemas. Configuration order intentionally replaces lexical
  provider-ID order wherever deterministic provider priority is observable.
- A reorganization of CLI audit detection that is unrelated to configuration
  validation and composition.

The design adds only capabilities needed to move current implementations. A
future non-OpenRTB provider, external crate, or new provider family requires a
separate design with a real consumer.

## Terms

The following terms are distinct:

- **Integration definition:** the statically discovered code definition for a
  stable integration ID such as `aps`.
- **Capability registration:** one typed contribution made by an integration,
  such as a proxy, HTML rewriter, JavaScript module, OpenRTB profile, or
  mediator.
- **Integration configuration:** the single ordered operator block at
  `[integrations.<id>]` that activates and configures the definition.
- **Auction provider instance:** one named endpoint and policy configuration
  below an integration, such as `aps.main`. Multiple instances may use the same
  integration implementation.
- **Local provider ID:** the provider name within one integration, such as
  `main`.
- **Qualified provider ID:** the strong, globally unique pair of an integration
  ID and local provider ID, serialized as `<integration>.<local-provider>`.
- **Integration registry:** core runtime state containing the enabled page,
  request, response, and browser capabilities in configuration order.
- **Auction plan:** core runtime state containing the validated configured
  provider instances, routes, and orchestration policy.

An integration is therefore a container for capabilities; it is not itself a
single capability type.

## Target Workspace Layout

```text
crates/
  trusted-server-core/
    src/
      integration/
        mod.rs
        registry.rs

  trusted-server-js/
    lib/
      src/core/
    src/

  trusted-server-integrations/
    build.rs
    Cargo.toml
    src/
      lib.rs
      adserver_mock/
        mod.rs
      aps/
        mod.rs
      datadome/
        mod.rs
        protection.rs
        protection_scope.rs
      didomi/
        mod.rs
      ...
      nextjs/
        mod.rs
        html_post_process.rs
        rsc.rs
        rsc_placeholders.rs
        script_rewriter.rs
        shared.rs
        fixtures/
      openrtb/
        mod.rs

  trusted-server-integrations-js/
    build.rs
    Cargo.toml
    lib/
      package.json
      src/integrations/
        aps/
          index.ts
          render.ts
        creative/
          index.ts
        datadome/
          index.ts
        ...
      test/
        integrations/
        fixtures/
    src/
      lib.rs
```

Every current flat Rust integration file becomes `<id>/mod.rs`. Existing
nested modules and fixtures stay with their owner. `openrtb` is a built-in
Rust-only integration that exposes configuration for the current standard
OpenRTB profile without turning the neutral OpenRTB execution engine into a
concrete integration.

JavaScript-only `creative` remains valid without a Rust directory. Rust-only
integrations remain valid without a browser directory.

`creative` is the sole fixed, non-configurable browser prelude in this design;
it is runtime support rather than an operator integration. Directory discovery
may build other JavaScript-only assets, but `[integrations]` cannot activate one
unless a Rust definition with that ID registers its browser-module capability.

## Dependency Direction

The Cargo dependency graph is one-way:

```text
trusted-server-integrations ──→ trusted-server-core ──→ trusted-server-js
             │
             └───────────────→ trusted-server-integrations-js

adapters and CLI ────────────→ trusted-server-integrations
adapters and CLI ────────────→ trusted-server-core
```

The rules are:

1. Core never depends on either integrations crate.
2. Concrete Rust integrations use public neutral core contracts and domain
   types.
3. `trusted-server-integrations` links Rust definitions with generated browser
   modules from `trusted-server-integrations-js`.
4. Integration TypeScript may use the explicit browser-core API, but browser
   core never imports a concrete integration and integration bundles never
   embed a private copy of stateful browser-core modules.
5. Every adapter and the CLI uses the same composition and validation entry
   points from `trusted-server-integrations`.
6. No adapter reconstructs a concrete catalog or imports `aps`, `prebid`, or
   another integration module directly.

### Application composition ownership

`trusted-server-integrations` is the application composition root, not only a
directory of implementations. It owns:

- `TrustedServerAppConfig`, the typed operator-facing app-config root used by
  the CLI.
- Source-aware TOML structure validation and ordered integration
  deserialization.
- Aggregation of core and integration secret metadata.
- Integration-owned preprocessing for conditionally active secrets.
- Catalog-aware validation and capability construction.
- The public runtime entry points that load a config-store blob and return one
  composed runtime value.

That runtime value, conceptually `TrustedServerComposition`, contains the
validated neutral `Settings`, one `Arc<AuctionPlan>`, and one
`IntegrationRegistry`. Adapters consume this value; they do not separately
compile the auction plan or rebuild the integration registry.

Core retains neutral config-store access, Fastly chunk reconstruction, blob
envelope verification, secret-resolution primitives, global settings types,
and auction-plan compilation. Those helpers accept or return neutral data and
never call the concrete catalog. The integration crate calls them in this
order:

```text
config-store bytes
  → core chunk reconstruction and envelope verification
  → integration-owned inactive-secret preprocessing
  → aggregated core + integration secret resolution
  → catalog-aware config validation
  → core AuctionPlan compilation
  → core IntegrationRegistry construction from typed registrations
  → TrustedServerComposition
```

The CLI imports `TrustedServerAppConfig` and its config command wrappers from
`trusted-server-integrations`. Each wrapper performs the source-aware pre-pass
before delegating storage and diff mechanics to EdgeZero's typed CLI functions.
No EdgeZero source change or new host service is required.

## Directory Discovery

### Rust

`trusted-server-integrations/build.rs` scans immediate directories under
`src/`. A directory containing `mod.rs` is a concrete integration whose stable
ID is the directory name. IDs must parse as the `IntegrationId` defined by this
design and must be unique.

The build script generates module declarations and a definition catalog. Its
lexical sorting makes generated source reproducible but has no runtime ordering
meaning. Each module must expose the expected crate-private `definition`
function; failure to do so is a compile error.

There is no directory-local numeric order. Runtime order belongs to
configuration.

The build fails for malformed IDs, duplicate normalized IDs, unreadable
directories, or an empty catalog. It emits `cargo:rerun-if-changed` directives
for the discovered directories.

### JavaScript

`trusted-server-integrations-js` owns its Node project, integration tests,
build pipeline, generated Rust module catalog, and browser assets. Its build
discovers immediate directories containing `index.ts` and emits one
self-contained IIFE per entry point. Its Cargo build embeds each output and its
SHA-256 hash.

The generated Rust API exposes typed module identifiers rather than accepting
unchecked strings. A Rust registration referencing a missing browser module
therefore fails compilation. JavaScript-only modules are valid and need no Rust
definition.

Rust and JavaScript discovery are independent; neither filesystem inventory is
treated as the canonical list for the other.

## Neutral Core Contracts

The neutral contents of `integrations/registry.rs` move to singular
`trusted_server_core::integration`. Core continues to own:

- `IntegrationDefinition` and `IntegrationRegistration` contracts.
- `IntegrationRegistry` and registry execution.
- Proxy, request-filter, attribute-rewriter, script-rewriter,
  HTML-post-processor, and head-injector traits and contexts.
- Neutral request-preparation and response-finalization hooks.
- Neutral response-sharing/private-cache annotations.
- Neutral JavaScript module metadata and load modes.
- OpenRTB profile registration contracts consumed by the generic plan and
  transport engines.
- Mediator registration contracts consumed by auction orchestration.
- Duplicate route, ID, renderer-type, and capability detection.
- Empty and stub registrations for core tests.

Core does not own a global `IntegrationType` enum. The builder has typed methods
for each supported contribution, and one definition may supply any compatible
combination. The initial methods correspond only to behavior present in the
repository.

An APS definition conceptually registers:

```text
APS
├── proxy capability
├── head-injection capability
├── JavaScript renderer module
└── OpenRTB profile capability
```

Capability multiplicity is explicit. Collection capabilities such as routes
and rewriters may register multiple entries. Single-valued capabilities such as
an OpenRTB profile or mediator may appear at most once per integration
definition; duplicate registration fails composition. Because provider tables
have no profile discriminator, an integration that owns
`auction.providers` must register exactly one OpenRTB profile capability.

The OpenRTB profile boundary has three stages:

1. `OpenRtbProfileDefinition` is the catalog-level capability. It supplies the
   stable profile ID, default timeout policy, typed configuration compiler,
   endpoint canonicalization and validation, and supported routing policy.
2. `CompiledOpenRtbProfile` is an object-safe, `Send + Sync` immutable profile
   stored as an `Arc` in each provider plan. It exposes only neutral routing
   facts and prepares one provider exchange from neutral auction input.
3. `PreparedOpenRtbExchange` contains the finalized outbound request plus a
   boxed, object-safe response parser bound to that exact provider and request.
   The parser owns any request-local APS, Prebid, or standard parsing state and
   consumes itself when parsing the response.

The prepared exchange lets the core engine retain shared backend registration,
transport, deadlines, notification policy, normalized response handling, and
telemetry. The profile owns request specialization, profile-specific headers,
debug capture, response parsing, diagnostics, and renderer descriptors.

Neutral routing policy replaces checks such as `is_prebid_server`. It expresses
only behaviors the generic router needs, including whether `all_eligible` is
allowed, whether trusted stored-request demand is recognized, and how bidder
parameters are admitted. Endpoint policy likewise replaces string comparisons
against profile IDs.

No stage returns `Any`, requires a downcast, or indexes a vendor-keyed side
table. Because the response parser is created by the same profile object that
prepares the request, state from one provider instance cannot be supplied to
another accidentally.

The standard OpenRTB implementation is registered by the built-in `openrtb`
integration. APS and Prebid register their implementations from their own
directories. `adserver_mock` registers the existing mediator capability.

No unused generic `AuctionProviderFactory` extension is added. A provider that
cannot use the current OpenRTB engine will define that additional seam in a
future design.

## Static Catalog and Activation

Directory discovery establishes what the binary supports. Configuration
establishes what runs.

At startup or deploy validation:

1. Parse `[integrations]` into an ordered sequence.
2. Resolve each ID against the generated definition catalog.
3. Ask the owning definition to parse and validate its complete configuration.
4. For each enabled integration, construct its typed capability registrations
   in TOML order.
5. Collect integration-owned auction provider instances and profiles.
6. Ask core to compile the single canonical auction plan.
7. Resolve typed browser modules and construct the neutral integration
   registry.

An absent integration is inactive. Every explicit parent integration table must
contain `enabled = true` or `enabled = false`; there is no integration-specific
default. An explicitly disabled integration may retain its settings and provider
instances but contributes no runtime capabilities or providers. Configuration
cannot activate APS through an auction plan while omitting
`[integrations.aps]`, and bidder or mediator references to a disabled
integration fail validation.

Disabled configuration still receives structural validation: unknown fields,
wrong types, duplicate IDs, and invalid values that are present fail. Missing
active-only required values and inactive secret references do not fail until
the integration is enabled. This permits operators to turn off an integration
without deleting prepared configuration while preventing disabled behavior from
leaking into the runtime plan.

The four adapters and CLI share this path. Runtime and deploy validation cannot
use different catalogs or integration schemas.

## One Integration Configuration

### Operator-facing shape

`[integrations]` is the single concrete integration inventory. No `type` field
is added; the table key resolves the statically compiled definition.
The `enabled` field is mandatory on every parent integration table, including
the built-in `openrtb` integration.

```toml
[integrations.prebid]
enabled = true
client_side_bidders = ["example-browser"]

[integrations.prebid.auction.providers.pbs-main]
endpoint = "https://prebid.example.com/openrtb2/auction"
timeout_ms = 1000
routing = "explicit"
debug = false
test_mode = false
consent_forwarding = "both"

[integrations.prebid.auction.providers.pbs-main.notifications]
suppress_all = false
suppress_seats = []

[integrations.aps]
enabled = true
rendering_mode = "trusted_server"

[integrations.aps.auction.providers.aps-main]
endpoint = "https://aps.example.com/e/pb/bid"
timeout_ms = 800
routing = "all_eligible"
account_id = "example-account"
debug = false
allow_script_creatives = false
```

The parent integration determines the implementation. Integration-owned
providers therefore do not accept `profile`, `implementation`, or
`profile_config`. Common provider fields and integration-specific profile
fields form one typed provider schema owned by that integration. Common
notification settings may remain in the nested `notifications` table.

Multiple named provider instances are supported beneath one integration.

The standard generic path uses the same inventory:

```toml
[integrations.openrtb]
enabled = true

[integrations.openrtb.auction.providers.example-direct]
endpoint = "https://bidder.example.com/openrtb"
timeout_ms = 1000
routing = "explicit"
```

### Global auction configuration

`[auction]` retains settings that coordinate integrations:

```toml
[auction]
enabled = true
timeout_ms = 2000
rewrite_creatives = true
sanitize_creatives = false
mediator = "adserver_mock"

[auction.bidders.example-bidder]
provider = "prebid.pbs-main"
```

Provider references use the canonical `<integration>.<local-provider>` form.
The qualified value is the provider identity used by the compiled plan,
backend correlation, diagnostics, and telemetry. The local provider name may
repeat under different integrations without collision.

Provider identity uses three strong types rather than broadening the existing
local identifier:

- `IntegrationId` matches `^[a-z][a-z0-9_]{0,62}$`. The underscore permits the
  existing Rust module IDs such as `adserver_mock`; dots are forbidden.
- `LocalProviderId` retains the current
  `^[a-z][a-z0-9-]{0,62}$` grammar; dots are forbidden.
- `QualifiedProviderId` stores an `IntegrationId` and `LocalProviderId`, parses
  exactly one dot separator, and has a maximum serialized length of 127 ASCII
  bytes.

`QualifiedProviderId` is the type used by bidder routes, provider plans,
backend discriminators, auction responses, diagnostics, and telemetry. Its
canonical `Display` and serde representation is `<integration>.<local>`. No
consumer reconstructs it with string concatenation, truncates it, or treats a
local provider ID as globally unique. Adapter target validation continues to
predict and reject backend-name collisions using the complete qualified
identity.

The mediator selects an enabled integration that registered a mediator
capability. Its settings remain under that integration:

```toml
[integrations.adserver_mock]
enabled = true
endpoint = "https://adserver.example.com/mediate"
timeout_ms = 500
```

## Ordering Contract

No manifest field, filename, alphabetic sort, hash-map iteration, or numeric
priority controls runtime order.

The contract is:

1. Integration order is the explicit declaration order of parent
   `[integrations.<id>]` tables.
2. Each parent integration table must appear before any descendant table. A
   nested auction or provider table cannot implicitly create or position an
   integration.
3. Provider order is the explicit declaration order of
   `[integrations.<id>.auction.providers.<provider>]` tables. Each provider
   parent must appear before descendant tables such as `notifications`.
4. Every parent integration table contains an explicit `enabled` value.
5. Disabled integrations contribute nothing; remaining integrations keep their
   relative order.
6. The flattened auction plan orders providers first by owning integration and
   then by local provider declaration.
7. Hook and immediate/deferred JavaScript lists retain integration order.
8. Browser output is neutral browser core first, the existing fixed
   JavaScript-only `creative` prelude second, and configured integration modules
   afterward.
9. Auction provider launch, response, and mediator-input order follows the
   flattened plan. With the existing strict-greater-than price comparison, the
   first configured provider retains an equal-price tie during local winner
   selection. Providers later in the sequence receive the remaining shared
   auction budget after earlier launches.

Inline-table and dotted-key shorthand may not define an integration parent or
provider parent. Requiring ordinary table headers makes activation, ownership,
and order visible in one form and lets the pre-pass produce targeted errors.

The workspace enables the `preserve_order` feature on its single resolved
`toml` package, so Cargo feature unification makes EdgeZero's `toml::Value`
maps order-preserving too. `IntegrationSettings` and provider collections use
ordered sequence-backed types, never `HashMap` or `BTreeMap`.

Before typed deserialization, `trusted-server-integrations` parses the source
with `toml_edit`. This source-aware pre-pass rejects descendant-before-parent
declarations, missing explicit parent tables, and missing `enabled` fields. It
then permits the existing EdgeZero scalar environment overlay; overlays may
replace values but may not create, remove, or reorder integration or provider
tables.

Every entry point that accepts TOML uses this pre-pass, including local loading
and the Trusted Server wrappers around CLI validate, diff, and push. EdgeZero's
typed mechanics remain responsible for overlay, validation invocation, diff,
envelope construction, consent, and store writes after the pre-pass succeeds.

### Config-store representation

JSON object member order is not an ordering contract. Config push therefore
converts the operator tables into an explicit ordered sequence in the
hash-verified blob envelope. Nested provider maps are likewise encoded with
explicit sequence order. Runtime loading reconstructs ordered settings from
those sequences and never infers order from JSON object iteration.

Conceptually, the stored representation carries:

```json
{
  "integrations": [
    {
      "id": "prebid",
      "config": {
        "enabled": true,
        "auction": {
          "providers": [
            {
              "id": "pbs-main",
              "config": {
                "endpoint": "https://prebid.example.com/openrtb2/auction"
              }
            }
          ]
        }
      }
    },
    { "id": "aps", "config": { "enabled": true } }
  ]
}
```

`TrustedServerAppConfig` uses custom serde at this boundary: deserialization
accepts the operator TOML table shape after the source pre-pass, while
serialization emits the explicit integration and provider sequences above for
the blob envelope. Runtime loading accepts only the new stored sequence shape;
an old blob containing an integration object map fails with migration guidance
rather than relying on JSON member order.

The private Rust type names may differ, but the serialized order must be
explicit and covered by compatibility tests across:

```text
trusted-server.toml
  → typed CLI configuration
  → hash-verified blob envelope
  → config store
  → runtime Settings
  → registry, JavaScript lists, and AuctionPlan
```

## Configuration Ownership and Validation

Core retains global settings and auction-orchestration validation. Each
integration owns the typed schema and validation for its full configuration,
including its provider instances.

`TrustedServerAppConfig` and the generated definition catalog live in
`trusted-server-integrations`. The catalog supplies integration parsing,
validation, secret metadata, pre-resolution handling for conditionally active
secrets, and capability construction to both runtime startup and the CLI. Core
exposes its non-integration secret metadata through a neutral helper; the
composition root combines it with catalog metadata.

For example, DataDome's inactive secret references are filtered by its
definition before the shared secret resolver runs; core's config-payload code
does not retain a DataDome-specific JSON path. Config-store loading, `config
validate`, `config diff`, and `config push` use the same catalog and composition
functions as the adapters.

Validation fails for:

- An unknown integration ID.
- A parent integration table with a missing or non-boolean `enabled` field.
- Integration configuration not accepted by its owner.
- A descendant integration or provider table declared before its explicit
  parent.
- Duplicate local provider IDs or duplicate qualified provider identities.
- A bidder route to an unknown, disabled, or incompatible provider.
- A selected mediator whose integration is absent, disabled, or lacks the
  mediator capability.
- Duplicate routes or renderer types.
- A referenced browser module absent from the generated browser catalog.
- An unsupported capability combination.

A disabled integration may retain structurally valid provider configuration;
those providers are not added to the plan. A globally disabled auction may
likewise retain otherwise valid enabled integration and provider configuration
so operators can prepare configuration before enabling the auction. References
from bidder routing or mediator selection to a disabled integration still fail,
even when the global auction is disabled.

## Configuration Migration

This is a deliberate breaking migration. The runtime and CLI do not support
both inventories or define precedence between them.

Representative mappings are:

| Previous configuration                                          | New configuration                                         |
| --------------------------------------------------------------- | --------------------------------------------------------- |
| `[auction.providers.pbs-main]` with `profile = "prebid-server"` | `[integrations.prebid.auction.providers.pbs-main]`        |
| `[auction.providers.aps-main]` with `profile = "aps"`           | `[integrations.aps.auction.providers.aps-main]`           |
| A standard profile provider named `example-direct`              | `[integrations.openrtb.auction.providers.example-direct]` |
| `[auction.providers.<id>.profile_config]`                       | Flattened into the owning integration's provider table    |
| Bidder route `provider = "pbs-main"`                            | `provider = "prebid.pbs-main"`                            |

Old `[auction.providers]`, `profile`, and `profile_config` fields fail with an
actionable message naming the new integration-owned location. A mixed old/new
configuration also fails. There is no silent translation at runtime and no
deprecation interval.

Examples, integration fixtures, environment-overlay tests, CLI documentation,
and operator guides migrate in the same change.

## Required Neutral Lifecycle Boundaries

### Response sharing annotation

DataDome currently communicates a concrete marker to core so core buffers a
full response and applies private caching. Replace that marker with a neutral
response-sharing annotation owned by core. DataDome sets it through a
registered hook; core preserves the current buffering and cache behavior.

The annotation represents only the existing shared-versus-request-private
decision. It is not a general policy or permissions system.

DataDome's tag-suppression and other integration-private request state stays
owned by DataDome and moves with the implementation. It may use neutral opaque
request/document state, but it is not folded into the response-sharing
annotation or exposed as a core vendor-specific field.

### Request preparation and response finalization

GPT diagnostics currently has direct preparation calls in adapters and core
and a direct finalization call in core. Add neutral typed hooks for those two
existing lifecycle points. The registry owns opaque request-scoped state
between them.

Adapters invoke registry preparation at the existing boundaries. Core invokes
finalization on the existing response path. No additional lifecycle stages are
introduced.

## Browser Composition and APS Renderer

`trusted-server-js` builds only the neutral core IIFE and exposes a narrow API
for integration registration and module combination.

The core IIFE initializes exactly one stateful registration object on the
Trusted Server browser namespace before any integration IIFE runs. Integration
bundles consume that object through an external runtime shim and type-only
browser-core declarations; their bundler must not inline the stateful registry
implementation. Artifact tests prove that a renderer registered by an
integration IIFE is visible to the already-loaded core IIFE.

`trusted-server-integrations-js` builds integration IIFEs. Immediate modules
are concatenated in the ordering contract above. Deferred and standalone
modules remain separate assets but retain their configuration-relative order
and typed identities.

Browser core currently imports APS renderer logic directly. Replace that
reverse dependency with one neutral renderer registration mechanism:

- Core parses the existing renderer envelope far enough to identify its type
  and retain its payload.
- The APS browser module registers the parser and dispatcher for the existing
  APS renderer type.
- Core dispatches through the registered renderer.
- Missing, duplicate, or rejecting renderers fail closed.

An enabled APS integration whose provider can emit APS renderer descriptors
includes its immediate APS browser module. The core IIFE and fixed creative
prelude load first, so APS registration is complete before a bid can render.

The serialized descriptor, validation, sandbox flags, message authentication,
timeouts, and render results do not change.

## Error Handling

Failures remain fail-closed and occur as early as the available information
allows.

Build-time failures include invalid discovered directories, generated catalog
errors, missing typed browser modules, JavaScript compilation failures, and
missing generated bundles.

CLI or startup failures include retired or mixed configuration shapes,
unknown definitions, invalid integration settings, unresolved qualified
provider references, duplicate routes, incompatible capabilities, invalid
auction plans, and unavailable assets.

Runtime hook errors retain the current `Report<TrustedServerError>` context and
HTTP behavior. Moving a concrete call behind a registry must not convert an
error into a warning, ignore it, or panic.

Core visibility changes remain narrow. Helpers move with their integration
when possible. Core exposes a new public item only when a neutral cross-crate
contract requires it.

## Compatibility Contract

The change intentionally does not preserve operator configuration or config
blob shape. It also intentionally changes provider priority from lexical local
provider-ID order to qualified configuration order. It preserves:

- Integration IDs.
- Existing routes and endpoint behavior.
- Existing integration hook behavior, now ordered by configuration.
- Auction plan compilation semantics after configuration normalization, except
  for the documented provider-priority source.
- OpenRTB request, response, routing, timeout, notification, and response
  admission behavior.
- Auction price comparison, mediation protocol, renderer descriptors, and
  telemetry schema. Provider response order and a locally selected equal-price
  winner may change when configuration order differs from the old lexical
  order.
- Existing provider identity fields, with values migrated from local IDs such
  as `pbs-main` to qualified IDs such as `prebid.pbs-main`.
- Cache privacy and full-buffer decisions.
- The route and behavioral parity of Fastly, Axum, Cloudflare, and Spin.

Bundle hashes and cache-busting URLs may change because browser sources are
rebuilt in different crates. The server must emit URLs matching the new
embedded hashes; bundle hashes are not a stable public contract.

## Delivery Scope

This is one architecture design but not one undifferentiated refactor. It has
four reviewable workstreams:

1. Neutral Rust capability and lifecycle contracts.
2. Browser-core separation and `trusted-server-integrations-js`.
3. Concrete Rust extraction and application composition ownership.
4. Ordered configuration, provider identity, and auction-profile migration.

The implementation plan must give each workstream its own verification
checkpoint and keep behavior-preserving moves separate from intentional config
and ordering changes. Intermediate commits may add unused neutral contracts or
new crates, but no merged state may have two active catalogs, two provider
inventories, or adapter-specific composition paths. This scope does not include
the external plugin ecosystem proposed by PR #1084.

## Migration Sequence

Implementation may use small commits, but the merged workspace must never have
two active integration or provider inventories.

1. Add neutral typed capability, lifecycle, renderer, and script-module
   contracts to core without changing behavior.
2. Create `trusted-server-integrations-js`, move integration browser sources
   and tests, and remove the browser-core APS import.
3. Create `trusted-server-integrations`, add directory discovery, and move all
   fifteen current Rust implementation units.
4. Replace closed APS and Prebid profile variants with registered OpenRTB
   profile and prepared-exchange behavior, and add the built-in `openrtb`
   integration.
5. Move `TrustedServerAppConfig`, all integration-specific configuration,
   validation, inactive-secret preprocessing, and secret metadata into the
   integrations crate.
6. Add the TOML source pre-pass, order-preserving maps, explicit stored
   sequences, strong qualified provider IDs, and the breaking
   integration-owned provider schema.
7. Rewire the CLI and all adapters to the single composition entry point that
   returns settings, plan, and registry together.
8. Remove the old concrete directories, fixed builder/profile tables,
   validation lists, and `[auction.providers]` schema.
9. Update examples, fixtures, operator documentation, and migration errors.

## Testing and Verification

### Discovery and dependency tests

- Every valid Rust directory appears exactly once in generated output.
- Invalid or duplicate directory IDs fail generation.
- JavaScript-only and Rust-only directories are accepted.
- A typed Rust reference to an absent browser module fails compilation.
- Embedded bundle hashes match built bytes.
- Core has no dependency on either integrations crate.
- Browser core imports no integration source.
- Adapters and CLI import no concrete integration module.

### Configuration and ordering tests

- TOML parent-table order becomes `IntegrationSettings` order.
- Nested provider declaration order is retained.
- A parent integration or provider table declared after one of its descendants
  fails before typed deserialization.
- Missing `enabled` fails; omitted integration tables remain inactive.
- TOML-to-envelope-to-runtime round trips preserve both orders byte-for-byte at
  the sequence level.
- Config-store loading produces the same registry, JavaScript, and provider
  order that the CLI validated.
- Disabled integrations may retain valid provider settings, contribute no
  providers or capabilities, and do not reorder enabled neighbors.
- Bidder and mediator references to disabled integrations fail, including while
  the global auction is disabled.
- Nested-only, unknown, missing-enabled, descendant-before-parent, and mixed
  old/new configurations fail with actionable messages.
- Qualified provider references resolve correctly and reject missing or
  incompatible targets.
- Local and qualified provider IDs enforce their separate grammars and bounds.
- Multiple provider instances under APS, Prebid, and standard OpenRTB compile
  with stable qualified identities.
- Provider launch, response, mediator-input, and local equal-price tie order
  follows integration then local-provider declaration order.

### Capability and behavior parity tests

- The generated catalog contains all current integration IDs plus `openrtb`.
- APS and Prebid register page/browser and auction capabilities without core
  importing their types.
- `adserver_mock` registers and is selected through the mediator capability.
- Route tables and duplicate detection retain behavior.
- DataDome privacy and buffering behavior remains unchanged.
- GPT diagnostics preparation, bootstrap injection, finalization, and caching
  remain unchanged on every adapter path.
- APS and Prebid request construction, transport, parsing, response admission,
  and auction results remain equivalent to PR #1016 behavior except for the
  documented provider-priority change.
- Prepared response parsers consume profile-owned request state without `Any`,
  downcasts, vendor enums, or cross-provider state reuse.
- Bidder routing, backend naming, notification suppression, telemetry identity,
  and mediator behavior remain equivalent apart from documented ordering.

### Browser tests

- Output order is core, creative prelude, and configured integrations.
- Immediate and deferred lists preserve configuration-relative order.
- APS is absent from browser core and registers its renderer from its own IIFE.
- Existing APS validation, sandbox, messaging, timeout, and rendering tests pass
  through neutral dispatch.
- Missing or duplicate renderer registrations fail closed.

### Repository gates

Before handoff, run every gate required by `AGENTS.md`, including Rust format,
all target-matched clippy aliases, Fastly/Axum/Cloudflare/Spin tests, CLI and
cross-adapter parity tests, required native and WASM builds, JavaScript builds
and Vitest suites for both browser crates, JavaScript formatting, and
documentation formatting.

## Risks and Mitigations

### Scope expansion through generic extension points

Moving concrete implementations can invite abstractions for hypothetical
providers.

Mitigation: add only capability contracts exercised by current code. External
providers, non-OpenRTB factories, and additional lifecycle stages remain
separate designs.

### Configuration migration obscures the crate boundary

Combining packaging and configuration work increases the number of affected
files.

Mitigation: keep the behavioral invariant explicit: configuration is
normalized into the same core auction plan and registries. Use focused commits
and parity tests around each boundary.

### Ordering loss across serialization

TOML order can be lost through unordered Rust maps or JSON objects.

Mitigation: use ordered in-memory types and explicit sequences in the
hash-verified blob, enable `toml/preserve_order`, and reject
descendant-before-parent source declarations with the `toml_edit` pre-pass.
Test the complete push/store/load path rather than only the TOML parser.

### Configuration order silently changes auction priority

Provider order affects launch budget, mediator input, response order, and equal
price ties. Treating it as cosmetic would make operator edits surprising.

Mitigation: define configuration order as operational priority, document the
change from PR #1016's lexical order, and test each observable consequence.

### Hidden reverse dependencies

Concrete integrations use core-private helpers and vendor-specific enum arms.

Mitigation: move owned helpers outward, replace vendor matches with the narrow
typed capability and prepared-exchange contracts, and review every new core
public item.

### Divergent validation paths

CLI validation and adapter startup could use different catalogs or schemas.

Mitigation: both call the same generated composition API. No secondary
validation inventory is allowed. Adapters receive the already composed
settings, plan, and registry rather than reconstructing any of them.

### Stale or incorrectly ordered browser artifacts

Splitting the Node build can embed previous output or load APS too late.

Mitigation: retain stale-output refusal, hash built bytes, load core and the
creative prelude first, and run end-to-end renderer and ordering tests.

## Acceptance Criteria

The change is complete when:

1. Both new crates are workspace members and statically linked by the CLI and
   every adapter where required.
2. All fifteen current concrete Rust implementation units live under
   `trusted-server-integrations/src/<id>/`.
3. The standard provider configuration is supplied by the built-in Rust-only
   `openrtb` integration.
4. All integration browser sources, assets, fixtures, and tests live under
   `trusted-server-integrations-js`.
5. Rust and JavaScript inventories are independently directory-discovered.
6. Core owns only neutral contracts and execution engines and imports no
   concrete integration.
7. One integration can register multiple typed capabilities; APS, Prebid, and
   `adserver_mock` are not special construction paths.
8. `[integrations]` is the only concrete integration and auction-provider
   inventory.
9. Configuration and provider ordering survive config push and runtime loading
   exactly and define the documented auction priority.
10. The old `[auction.providers]` schema is rejected with targeted migration
    guidance.
11. The compiled auction plan retains PR #1016 behavior after normalization,
    except for the explicit change from lexical to configuration-order provider
    priority.
12. Browser core imports no concrete integration, and APS rendering works
    through registration.
13. `TrustedServerAppConfig`, integration secret handling, and final runtime
    composition are owned by `trusted-server-integrations`; core has no concrete
    config or loader dependency.
14. The CLI and all adapters use the same generated composition and validation
    path and receive one settings/plan/registry composition.
15. OpenRTB request-local state crosses the transport boundary through a
    prepared response parser without `Any` or vendor enum variants in core.
16. Explicit `enabled`, parent-before-descendant, disabled-retention, local-ID,
    and qualified-ID rules have end-to-end tests.
17. The full repository verification gates pass.

## Deferred Work

The following require separate designs and real consumers:

- External vendor-owned crates or adapter-supplied registrations.
- Runtime integration loading or a stable integration SDK.
- Independent integration release and compatibility policies.
- Identity, EC, geo, device, and permission-signal providers.
- Permission and jurisdiction policy changes.
- Non-OpenRTB auction provider factories.
- Upstream EdgeZero composition and host-service changes.
- Moving CLI audit detection metadata into integration directories.
