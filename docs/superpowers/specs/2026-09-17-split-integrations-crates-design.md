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
filesystem discovery order will never affect execution.

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
- Qualified provider IDs remain the stable identity shared by bidder routing,
  backend correlation, diagnostics, and telemetry.
- One validated plan remains authoritative across every adapter and runtime
  consumer.
- The generic OpenRTB transport remains shared.
- Existing routing, timeout, notification, response admission, mediation, and
  telemetry attribution behavior remains unchanged.

This design changes the configuration location and identity spelling. Provider
instances move below their owning integration, and cross-integration references
use a qualified `<integration>.<instance>` identifier.

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
8. Preserve the runtime behavior and compiled-plan guarantees of PR #1016.
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
- EdgeZero lifecycle, host-evidence, store, or adapter changes.
- New auction protocols, bidding behavior, ranking, notification behavior, or
  telemetry semantics.
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
   core never imports a concrete integration.
5. Every adapter and the CLI uses the same composition and validation entry
   points from `trusted-server-integrations`.
6. No adapter reconstructs a concrete catalog or imports `aps`, `prebid`, or
   another integration module directly.

## Directory Discovery

### Rust

`trusted-server-integrations/build.rs` scans immediate directories under
`src/`. A directory containing `mod.rs` is a concrete integration whose stable
ID is the directory name. IDs must use the existing integration ID grammar and
must be unique.

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

The OpenRTB profile contract replaces the closed
`CompiledOpenRtbProfile::{Aps, PrebidServer, ...}` dependency. It supplies the
profile-owned operations required by the existing generic engine, including
typed configuration compilation, request specialization, response parsing,
diagnostics, and renderer information. Core invokes the contract without
matching on vendor variants or importing integration types.

Compilation returns an `Arc`-backed trait object representing one immutable
compiled profile. The auction plan stores that object beside the common
provider settings, and the generic OpenRTB engine calls its typed methods. No
`Any` downcast or vendor-keyed side table is used.

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

An absent integration is inactive. An explicitly disabled integration is
validated but contributes no runtime capabilities. Configuration cannot
activate APS through an auction plan while omitting `[integrations.aps]`.

The four adapters and CLI share this path. Runtime and deploy validation cannot
use different catalogs or integration schemas.

## One Integration Configuration

### Operator-facing shape

`[integrations]` is the single concrete integration inventory. No `type` field
is added; the table key resolves the statically compiled definition.

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

1. Integration order is the first explicit declaration order of parent
   `[integrations.<id>]` tables.
2. Each integration must have an explicit parent table; a nested auction table
   cannot implicitly create or position it.
3. Provider order is declaration order under that integration's
   `auction.providers` map.
4. Disabled integrations contribute nothing; remaining integrations keep their
   relative order.
5. The flattened auction plan orders providers first by owning integration and
   then by local provider declaration.
6. Hook and immediate/deferred JavaScript lists retain integration order.
7. Browser output is neutral browser core first, the existing fixed
   JavaScript-only `creative` prelude second, and configured integration modules
   afterward.

The TOML parser must capture order directly. `IntegrationSettings` may not use
`HashMap` or another unordered representation.

### Config-store representation

JSON object member order is not an ordering contract. Config push therefore
converts the operator tables into an explicit ordered sequence in the signed
blob envelope. Nested provider maps are likewise encoded with explicit
sequence order. Runtime loading reconstructs ordered settings from those
sequences and never infers order from JSON object iteration.

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

The exact private Rust types may differ, but the serialized order must be
explicit and covered by compatibility tests across:

```text
trusted-server.toml
  → typed CLI configuration
  → signed blob envelope
  → config store
  → runtime Settings
  → registry, JavaScript lists, and AuctionPlan
```

## Configuration Ownership and Validation

Core retains global settings and auction-orchestration validation. Each
integration owns the typed schema and validation for its full configuration,
including its provider instances.

The generated definition catalog supplies integration parsing, validation,
secret metadata, and capability construction to both runtime startup and the
CLI. `config validate`, `config diff`, and `config push` must use the same
catalog as the adapters.

Validation fails for:

- An unknown integration ID.
- Integration configuration not accepted by its owner.
- A nested provider table without an explicit parent integration table.
- Auction providers on an explicitly disabled integration.
- Duplicate local provider IDs or duplicate qualified provider identities.
- A bidder route to an unknown, disabled, or incompatible provider.
- A selected mediator whose integration is absent, disabled, or lacks the
  mediator capability.
- Duplicate routes or renderer types.
- A referenced browser module absent from the generated browser catalog.
- An unsupported capability combination.

A globally disabled auction may retain otherwise valid enabled integration and
provider configuration so operators can prepare configuration before enabling
the auction.

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
blob shape. It does preserve:

- Integration IDs.
- Existing routes and endpoint behavior.
- Existing integration hook behavior, now ordered by configuration.
- Auction plan compilation semantics after configuration normalization.
- OpenRTB request, response, routing, timeout, notification, and response
  admission behavior.
- Auction ranking, mediation, renderer descriptors, and telemetry semantics.
- Existing provider identity fields, with values migrated from local IDs such
  as `pbs-main` to qualified IDs such as `prebid.pbs-main`.
- Cache privacy and full-buffer decisions.
- The route and behavioral parity of Fastly, Axum, Cloudflare, and Spin.

Bundle hashes and cache-busting URLs may change because browser sources are
rebuilt in different crates. The server must emit URLs matching the new
embedded hashes; bundle hashes are not a stable public contract.

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
   profile behavior and add the built-in `openrtb` integration.
5. Move all integration-specific configuration, validation, and secret metadata
   into integration definitions.
6. Change operator and stored configuration to the ordered integration-owned
   provider model.
7. Rewire the CLI and all adapters to the single composition entry point.
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
- TOML-to-envelope-to-runtime round trips preserve both orders byte-for-byte at
  the sequence level.
- Config-store loading produces the same registry, JavaScript, and provider
  order that the CLI validated.
- Disabled integrations are skipped without reordering enabled neighbors.
- Nested-only, unknown, disabled-with-provider, and mixed old/new configurations
  fail with actionable messages.
- Qualified provider references resolve correctly and reject missing or
  incompatible targets.
- Multiple provider instances under APS, Prebid, and standard OpenRTB compile
  with stable qualified identities.

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
  and auction results remain equivalent to PR #1016 behavior.
- Bidder routing, backend naming, notification suppression, telemetry identity,
  and mediator behavior remain equivalent.

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

Mitigation: use ordered in-memory types and explicit sequences in the signed
blob. Test the complete push/store/load path rather than only the TOML parser.

### Hidden reverse dependencies

Concrete integrations use core-private helpers and vendor-specific enum arms.

Mitigation: move owned helpers outward, replace vendor matches with the narrow
typed capability contract, and review every new core public item.

### Divergent validation paths

CLI validation and adapter startup could use different catalogs or schemas.

Mitigation: both call the same generated composition API. No secondary
validation inventory is allowed.

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
   exactly.
10. The old `[auction.providers]` schema is rejected with targeted migration
    guidance.
11. The compiled auction plan retains PR #1016 behavior after normalization.
12. Browser core imports no concrete integration, and APS rendering works
    through registration.
13. The CLI and all adapters use the same generated composition and validation
    path.
14. The full repository verification gates pass.

## Deferred Work

The following require separate designs and real consumers:

- External vendor-owned crates or adapter-supplied registrations.
- Runtime integration loading or a stable integration SDK.
- Independent integration release and compatibility policies.
- Identity, EC, geo, device, and permission-signal providers.
- Permission and jurisdiction policy changes.
- Non-OpenRTB auction provider factories.
- EdgeZero composition and host-service changes.
- Moving CLI audit detection metadata into integration directories.
