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

Cross-integration runtime order will come exclusively from TOML declaration
order. The config push and config-store representation will preserve that order
explicitly; filesystem discovery order will never affect execution. For
auction providers, that order is also operational priority: it controls launch
and response order, mediator input order, and local equal-price tie-breaking.

This design intentionally changes the auction configuration introduced by PR
#1016 while preserving that work's compiled-plan and runtime guarantees. It
adopts only the typed-registration portion of PR #1084 and excludes that PR's
external provider ecosystem and unrelated provider systems.

This remains one design, but it has two merge milestones. The crate and runtime
boundary moves first without changing operator configuration. The ordered,
integration-owned configuration cuts over only after the behavior-preserving
boundary is running. The milestones share one target architecture without
forcing the packaging move and configuration migration into one deployment.

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

The current PR #1084 configuration convention is mutually exclusive with this
design. PR #1084 selects implementations through top-level `[integration]`,
`[demand]`, and `[adserver]` provider selectors, removes `enabled` from
integration blocks, and does not treat APS as an integration. This design
deliberately chooses one ordered `[integrations]` inventory, explicit
`enabled`, and APS as a multi-capability integration. The two configurations
must not merge as parallel conventions. If this design is accepted, the
conflicting configuration and auction sections of PR #1084 must be superseded
or revised before that broader provider work proceeds.

## Goals

1. Move every concrete Rust integration implementation out of core and into one
   `trusted-server-integrations` crate.
2. Move all integration-specific browser sources and artifacts into one
   `trusted-server-integrations-js` crate.
3. Give each concrete integration one Rust directory and, when applicable, one
   same-named browser directory.
4. Keep one explicit compile-checked Rust catalog and discover browser modules
   from integration directories at build time.
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

- **Integration definition:** the statically cataloged code definition for a
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
      package.json
      package-lock.json
      build-browser.mjs
      src/core/
      test/core/
    src/

  trusted-server-integrations/
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
Rust-only, directory-backed integration adapter that exposes configuration for
the current standard OpenRTB profile without turning the neutral OpenRTB
execution engine into concrete code. The target Rust catalog therefore has
sixteen definitions: fifteen moved implementations plus the new `openrtb`
adapter.

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
5. Every adapter and the CLI uses the same source-validation entry points from
   `trusted-server-integrations`; every adapter uses its single runtime
   composition entry point.
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
- Public source-validation wrappers used by the CLI before EdgeZero's typed
  validate, diff, and push mechanics.
- The public runtime entry points that load a config-store blob and return one
  composed runtime value.

`TrustedServerAppConfig` contains neutral core configuration plus the ordered
integration-owned source configuration. Concrete integration configuration is
not added to core's `Settings`. Composition consumes the integration portion
into capabilities and returns a neutral runtime `Settings` value containing
only state that core execution engines understand.

The public API has two explicit phases. The source phase performs the pre-pass,
catalog resolution, integration-owned structural and deploy validation, secret
metadata aggregation, and serialization into the storage DTO. The CLI stops at
that phase and never constructs runtime capability objects from unresolved
secret key names. The runtime phase begins only after envelope verification,
inactive-secret preprocessing, and secret resolution; it validates the resolved
values and constructs the plan, registry, and browser assets. Both phases use
the same static catalog and integration schemas, but only adapters receive the
final `TrustedServerComposition`.

That runtime value, conceptually `TrustedServerComposition`, contains the
validated neutral `Settings`, one `Arc<AuctionPlan>`, and one
`IntegrationRegistry`, plus the composed `BrowserDocumentAssets`. Adapters
consume this value; they do not separately compile the auction plan, rebuild
the integration registry, or enumerate browser bundles.

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
  → browser asset composition and document fingerprint
  → TrustedServerComposition
```

The CLI imports `TrustedServerAppConfig` and its config command wrappers from
`trusted-server-integrations`. Each wrapper performs the source-aware pre-pass
and source-phase catalog validation before delegating storage and diff mechanics
to EdgeZero's typed CLI functions. No EdgeZero source change or new host service
is required.

## Static Rust Catalog and Browser Discovery

### Rust

`trusted-server-integrations/src/lib.rs` declares a small, explicit static
catalog. Each entry names an `IntegrationId` and a crate-private `definition`
function from a same-named directory. Rust compilation checks every listed
module and definition signature. A host-target completeness test enumerates
immediate `src/<id>/mod.rs` directories and fails if a valid directory is
missing from the catalog or a catalog ID has no directory. IDs must parse as
the `IntegrationId` defined by this design and must be unique.

There is no directory-local numeric order. Runtime order belongs to
configuration.

This intentionally avoids a Rust build script and generated module
declarations for a sixteen-entry table. The catalog-completeness test provides
the missing-registration guard without making filesystem discovery part of
compilation.

### JavaScript

`trusted-server-js` and `trusted-server-integrations-js` use one Node toolchain
project, one lockfile, and one set of Vitest, ESLint, Prettier, Vite, and Prebid
aliases. The canonical project root remains
`crates/trusted-server-js/lib`; `trusted-server-integrations-js` does not add a
second `package.json` or lockfile. Neutral browser sources remain under
`trusted-server-js`; integration sources, fixtures, and tests live under
`trusted-server-integrations-js`. Separate build targets emit neutral and
integration artifacts into their owning Rust crates. The build helpers
coordinate dependency installation and output generation so parallel Cargo
build scripts cannot race or consume stale artifacts.

The integration build discovers immediate directories containing `index.ts`
and emits one self-contained IIFE per entry point. Its Cargo build embeds each
output and its SHA-256 hash. CI, Dependabot, browser integration scripts, and
the CLI Prebid builder use the single workspace root rather than maintaining a
second dependency graph.

The generated Rust API exposes typed module identifiers rather than accepting
unchecked strings. A Rust registration referencing a missing browser module
therefore fails compilation. JavaScript-only modules are valid and need no Rust
definition.

The explicit Rust catalog and generated browser catalog are independent;
neither inventory is treated as the canonical list for the other.

## Neutral Core Contracts

The neutral contents of `integrations/registry.rs` move to singular
`trusted_server_core::integration`. Core continues to own:

- `IntegrationDefinition` and `IntegrationRegistration` contracts.
- `IntegrationRegistry` and registry execution.
- Proxy, request-filter, attribute-rewriter, script-rewriter,
  HTML-post-processor, and head-injector traits and contexts.
- Neutral request-preparation and response-finalization hooks.
- Neutral request-processing and response-sharing annotations.
- Neutral browser-asset metadata, byte/hash access, load modes, and composed
  document fingerprints.
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

The mediator does not reuse or expose the legacy `AuctionProvider` trait. Its
neutral boundary has three parts:

1. `MediatorDefinition` compiles one integration's mediator configuration.
2. `CompiledMediator` receives the original auction request, the already
   ordered provider responses, and the remaining bounded runtime context. It
   returns a `PreparedMediatorExchange`.
3. `PreparedMediatorExchange` contains the finalized outbound request and a
   bound response parser that consumes the upstream response into the existing
   normalized mediation result.

Core retains mediator transport, deadline enforcement, telemetry, and the
existing warning-and-local-ranking fallback policy. `adserver_mock` owns only
request construction and response interpretation. The old mediator use of
`Arc<dyn AuctionProvider>` is deleted when its last caller migrates.

No unused generic `AuctionProviderFactory` extension is added. A provider that
cannot use the current OpenRTB engine will define that additional seam in a
future design.

## Static Catalog and Activation

The explicit static catalog establishes what the binary supports.
Configuration establishes what runs.

Source validation and runtime startup both resolve IDs through the same catalog.
Source validation stops after validating the unresolved, storage-safe model. At
runtime, after secrets are resolved:

1. Parse `[integrations]` into an ordered sequence.
2. Resolve each ID against the static definition catalog.
3. Ask the owning definition to parse and validate its complete configuration.
4. For each enabled integration, construct its typed capability registrations
   in the validated integration order restored from the storage sidecar.
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

The APS rule is an intentional activation break from PR #1016. Today an APS
auction profile can activate server-side rendering support without an enabled
`[integrations.aps]` browser block. After cutover, every APS provider requires
an explicit `[integrations.aps]` parent with `enabled = true`; that one parent
activates APS's server, page, and browser capabilities together.

Disabled configuration still receives structural validation: unknown fields,
wrong types, duplicate IDs, and invalid values that are present fail. Missing
active-only required values and inactive secret references do not fail until
the integration is enabled. This permits operators to turn off an integration
without deleting prepared configuration while preventing disabled behavior from
leaking into the runtime plan.

The four adapters share the runtime composition path. The CLI and adapters
share the source/catalog validation rules; runtime and deploy validation cannot
use different catalogs, integration schemas, or plan-input validation.

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
  existing Rust module IDs such as `adserver_mock`; dots are forbidden. Its
  maximum length is 63 ASCII bytes.
- `LocalProviderId` retains the current
  `^[a-z][a-z0-9-]{0,62}$` grammar; dots are forbidden. Its maximum length is
  63 ASCII bytes.
- `QualifiedProviderId` stores an `IntegrationId` and `LocalProviderId`, parses
  exactly one dot separator, and has a maximum serialized length of 127 ASCII
  bytes.

`QualifiedProviderId` is the type used by bidder routes, provider plans,
backend discriminators, auction responses, diagnostics, and telemetry. Its
canonical `Display` and serde representation is `<integration>.<local>`. No
consumer reconstructs it with string concatenation, truncates it, or treats a
local provider ID as globally unique. Adapter target validation continues to
predict and reject backend-name collisions using the complete qualified
identity. A platform backend name is not itself the provider identity. Where an
adapter's normalization is lossy, as with Axum mapping dots, hyphens, and
underscores to the same character, its correlation name includes a stable
digest of the full qualified ID. Target validation rejects any remaining final
name collision. Tests cover aliases such as `a_b.c` and `a.b-c`.

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
7. Hook and immediate/deferred JavaScript lists retain integration order. When
   one integration registers multiple hooks of the same capability, their
   relative order is the explicit order returned by that integration's
   definition; the integration remains one operator-visible position and does
   not expose a second priority mechanism.
8. Browser output is neutral browser core first, the existing fixed
   JavaScript-only `creative` prelude second, and configured integration modules
   afterward.
9. Auction provider launch, response, and mediator-input order follows the
   flattened plan. With the existing strict-greater-than price comparison, the
   first configured provider retains an equal-price tie during local winner
   selection. Dispatch checks the remaining shared deadline immediately before
   each back-to-back launch; configuration order becomes budget-observable only
   if the deadline expires or adapter timeout canonicalization reaches zero
   during that launch loop.

The current hard-coded requirement that `js_asset_proxy` remain the first
rewriter is retired. Rewriter chaining follows the same operator-visible
integration order as every other hook. Migration guidance places
`js_asset_proxy` first in migrated examples and procedures so the old behavior
is preserved by default, while an operator may deliberately choose a different
order. No engine-only priority is hidden from the configuration.

All recovery paths obey the same provider ordinals. In particular, the two
current transport-failure branches that sort provider IDs lexically are
replaced with plan-order recovery before the new ordering contract is enabled.

Inline-table and dotted-key shorthand may not define an integration parent or
provider parent. Requiring ordinary table headers makes activation, ownership,
and order visible in one form and lets the pre-pass produce targeted errors.

The workspace enables the `preserve_order` feature on its single resolved
`toml` package, so Cargo feature unification makes EdgeZero's `toml::Value`
maps order-preserving too. The integration-owned source model's integration and
provider collections use ordered map types with explicit iteration semantics,
never `HashMap` or `BTreeMap`.

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

EdgeZero does not currently expose a pre-parse hook. The Trusted Server wrappers
therefore deliberately duplicate its app-config path rule: an explicit
`--app-config` wins; otherwise the path is `<manifest-dir>/<app.name>.toml`.
The wrapper reads that source for structural validation and EdgeZero reads it
again for typed processing. Parity tests cover explicit and default paths,
manifest paths with and without parent directories, and `--no-env`. The
environment overlay can replace only scalar leaves already present in TOML; it
cannot create an omitted `enabled` field, integration, provider, table, or
array. Operator templates must contain every leaf intended for overlay.

### Config-store representation

JSON object member order is not an ordering contract. Config push therefore
keeps integration and provider configuration in object maps but adds explicit
order sidecars to the hash-verified blob envelope. This preserves the existing
object paths used by secret metadata while making runtime order independent of
JSON map iteration. `serde_json/preserve_order` is not required and does not
become a workspace-wide behavior change.

Conceptually, the stored representation carries:

```json
{
  "trusted_server_schema": 2,
  "integration_order": ["prebid", "aps"],
  "integrations": {
    "prebid": {
      "enabled": true,
      "auction": {
        "provider_order": ["pbs-main"],
        "providers": {
          "pbs-main": {
            "endpoint": "https://prebid.example.com/openrtb2/auction"
          }
        }
      }
    },
    "aps": { "enabled": true }
  }
}
```

`TrustedServerAppConfig` uses custom serialization at this boundary.
Deserialization accepts the operator TOML table shape after the source pre-pass;
serialization emits object-shaped configuration plus the schema and order
sidecars above. Serialization fails if a sidecar omits, duplicates, or names a
different key than its corresponding object map. Runtime loading validates the
same bijection before constructing ordered settings.

This is an explicit asymmetric serde boundary. EdgeZero's typed CLI
deserializes and validates `TrustedServerAppConfig`, then its manual `Serialize`
implementation emits the schema-2 storage DTO. Runtime reads that DTO rather
than deserializing it back into the operator type. `integration_order` is always
present. `provider_order` is required whenever the corresponding `providers`
object is present, including when both are empty; both are absent when an
integration has no provider collection.

`trusted_server_schema`, `integration_order`, and `provider_order` are reserved
storage fields. They are generated by serialization and rejected if supplied in
operator TOML.

Object-shaped storage keeps secret paths such as
`integrations.datadome.server_side_key_secret_name` valid. Core and integration
secret metadata traverse that resolution view before ordered settings are
constructed. Integration-owned inactive-secret preprocessing also receives the
object-shaped integration entry by ID and writes any resolved value back to the
same entry; it never searches an `{ id, config }` sequence. End-to-end tests
prove both active DataDome secret fields are presence-checked, resolved, and
removed when inactive.

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

`TrustedServerAppConfig` and the static definition catalog live in
`trusted-server-integrations`. The catalog supplies integration parsing,
validation, secret metadata, pre-resolution handling for conditionally active
secrets, and capability construction to both runtime startup and the CLI. Core
exposes its non-integration secret metadata through a neutral helper; the
composition root combines it with catalog metadata.

For example, DataDome's inactive secret references are filtered by its
definition before the shared secret resolver runs; core's config-payload code
does not retain a DataDome-specific JSON path. Config-store loading, `config
validate`, `config diff`, and `config push` use the same catalog resolution,
schemas, and pure validation functions as the adapters. Only config-store
loading proceeds through resolved-value validation and runtime capability
composition.

The DataDome move accounts for every current production coupling rather than
only its response marker: direct HTML-processor state, publisher
template/body/privacy decisions, startup and deploy validation, legacy settings
cleanup, secret metadata and resolved-value validation, and inactive-secret
preprocessing. Integration-owned code moves outward; the neutral processing
requirements and object-shaped secret-resolution view replace the two places
where core genuinely coordinates behavior.

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

This is a deliberate operator-source migration. The new CLI accepts only the
new inventory and never defines precedence between old and new source fields.
The runtime temporarily supports two stored application schema versions only
to make deployment safe; it normalizes exactly one complete stored shape and
never merges inventories.

Representative mappings are:

| Previous configuration                                          | New configuration                                         |
| --------------------------------------------------------------- | --------------------------------------------------------- |
| `[auction.providers.pbs-main]` with `profile = "prebid-server"` | `[integrations.prebid.auction.providers.pbs-main]`        |
| `[auction.providers.aps-main]` with `profile = "aps"`           | `[integrations.aps.auction.providers.aps-main]`           |
| APS provider with no `[integrations.aps]` parent                | Add `[integrations.aps]` with `enabled = true`            |
| A standard profile provider named `example-direct`              | `[integrations.openrtb.auction.providers.example-direct]` |
| `[auction.providers.<id>.profile_config]`                       | Flattened into the owning integration's provider table    |
| Bidder route `provider = "pbs-main"`                            | `provider = "prebid.pbs-main"`                            |

Old `[auction.providers]`, `profile`, and `profile_config` fields fail with an
actionable message naming the new integration-owned location. A mixed old/new
operator configuration also fails. There is no deprecation interval for
operator TOML. The temporary old-blob reader is not an accepted source format
and does not make old fields valid in the new CLI.

Examples, integration fixtures, environment-overlay tests, CLI documentation,
and operator guides migrate in the same change.

### Stored schema and rollout

The EdgeZero `BlobEnvelope` version describes EdgeZero's envelope and canonical
hash rules; it is not a Trusted Server application-schema version. Both old and
new data therefore remain in envelope version 1. New data carries
`trusted_server_schema = 2` inside `data`; absence of that field identifies the
existing stored shape during the transition. Unknown application schema values
fail before secret resolution.

One release of the new binary contains a read-only compatibility decoder for
the existing stored object shape and `[auction.providers]`. It converts that
complete legacy value into the same neutral ordered composition used by schema
2, retaining the current fixed integration-builder and special-registration
order, the old lexical provider order, and the existing implicit APS activation
behavior for that legacy blob only. It emits an operator warning to push the
migrated configuration. New CLI writes schema 2 only.

Rollout is ordered:

1. Archive the current envelope bytes and record the adapter, store, and key.
   EdgeZero has no config-pull command, so this uses the adapter's native
   config-store read/export facility and is an explicit release artifact.
2. Deploy the dual-reader binary while the schema-1 blob remains active.
3. Complete health checks on every deployed instance.
4. Push the migrated schema-2 configuration with the new CLI.
5. Verify registry order, provider order, browser asset hashes, and auction
   health before declaring the cutover complete.

An old binary must never serve a schema-2 blob. Rolling back after step 4 first
restores the archived schema-1 envelope, verifies that restoration, and only
then rolls the binary back. If the platform cannot coordinate those operations,
the release is paused rather than accepting an outage window. The compatibility
decoder is removed only in a later release after every supported deployment has
completed the schema-2 cutover.

## Required Neutral Lifecycle Boundaries

### Request processing and sharing annotations

DataDome currently communicates a concrete request marker to core before
template lookup. Core uses it to bypass a shared template, require the origin
and a full HTML body, and stamp the final response private. Replacing it with a
late cache-only flag would change behavior.

Core therefore owns a small monotonic `RequestProcessingRequirements` value
with three independent axes:

- shared-template eligibility: eligible or bypass;
- body processing: streaming allowed or full body required;
- response sharing: shared allowed or request-private.

Every hook may only make a requirement more restrictive. The registry combines
requirements before template-cache and origin-selection decisions, carries the
result through HTML processing, and enforces final response privacy. This is a
processing contract, not a general policy, permission, or vendor-state system.

DataDome sets these requirements inside the request-filter hook at the point
where it already decides client-tag suppression. Its tag-suppression detail
remains opaque integration-owned request/document state. Core sees only the
neutral requirements and an opaque token returned to later hooks; it has no
DataDome type, field, or JSON path.

### Request preparation and response finalization

GPT diagnostics currently has direct preparation calls in adapters and core
and a direct finalization call in core. Add neutral typed hooks for those two
existing lifecycle points. The registry owns opaque request-scoped state
between them.

Preparation returns the opaque per-integration decision plus its declared
`RequestProcessingRequirements`. The neutral requirements are available before
the existing template-cache/private decision; under ESI, request-private opaque
state is never copied into a shared template. Adapters invoke registry
preparation at the existing boundaries and core invokes finalization on the
existing response path. No additional lifecycle call site is introduced.

## Browser Composition and APS Renderer

`trusted-server-js` builds only the neutral core IIFE. Its browser API exposes
the existing shared facilities integrations actually use: logging, slot lookup
and render helpers, normalized auction-response access, first-impression state,
and renderer registration and dispatch. Integration bundles import only
type-only declarations and call the installed browser API; Rollup/Vite treats
the runtime shim as external.

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

The Rust registration does not carry only a string module ID. Core owns a
neutral immutable `BrowserAsset` contract containing the typed module ID,
embedded bytes, SHA-256 hash, load mode, and trusted script-tag attributes.
Composition produces a `BrowserDocumentAssets` value containing:

- the exact ordered byte parts and concatenated hash for the unified asset;
- the ordered deferred and permitted standalone assets with their bytes and
  hashes;
- the fixed creative prelude;
- a deterministic document fingerprint covering injected asset IDs, hashes,
  order, attributes, immutable inline-asset bytes, and stable configuration
  fingerprints for generated inline head output.

Trusted attributes from immediate assets are merged onto the unified script
tag. Duplicate names with different values fail composition; equal duplicates
collapse to one attribute.

Static serving, cache-busting URLs, immutable-cache validation, and publisher
template keys consume `BrowserDocumentAssets`; core no longer performs a
crate-global `all_module_ids()` lookup. The fingerprint includes only assets
that can affect the composed document and includes GPT bootstrap bytes. It is
recomputed during composition whenever configuration or embedded bytes change.
Each head injector whose output varies with integration configuration supplies
a deterministic fingerprint contribution from the exact fields that affect its
output. Request-dependent head variation is permitted only when its neutral
`RequestProcessingRequirements` bypass shared-template reuse; request data is
never folded into the composition-wide fingerprint.

Browser core currently imports APS renderer logic directly. Replace that
reverse dependency with one neutral renderer registration mechanism:

- Core parses the existing renderer envelope far enough to identify its type
  and retain its payload.
- The APS browser module registers validation and dispatch for the existing APS
  renderer type.
- Core dispatches through the registered renderer.
- GPT and Prebid call the neutral dispatcher and never import APS source.
- No APS source is inlined into the core, GPT, or Prebid IIFE.

An enabled APS integration whose provider can emit APS renderer descriptors
includes its immediate APS browser module. The core IIFE and fixed creative
prelude load first, so APS registration is complete before a bid can render.
APS renderer registration is an immediate-only capability; composition rejects
a deferred APS renderer. Its current rendering mode continues to be read while
the synchronous unified script tag is executing, and the APS-owned trusted
attribute remains on that tag. A future switch to a standalone or deferred APS
asset requires replacing `document.currentScript` configuration first.

Renderer failure is scoped to the owning renderer-bearing bid or message. A
missing or rejecting renderer suppresses that bid with no generic-creative or
native-Prebid fallback; unrelated bids and the page continue. A duplicate type
fails Rust composition, while a defensive browser-side duplicate poisons that
type rather than using last-registration-wins. No renderer route, DOM, message,
or beacon side effect occurs before the selected handler accepts the payload.

The serialized descriptor, validation, sandbox flags, message authentication,
timeouts, and render results do not change.

`gpt_bootstrap.js` moves with GPT into `trusted-server-integrations-js` and is
exported as a hashed integration-owned inline asset. GPT's Rust head injector
consumes that exported asset; core never embeds it directly. Tests resolve the
asset through the owning package instead of a cross-crate relative path. APS
golden renderer fixtures likewise move to an integration-owned shared fixture
location used by both Rust and browser tests.

## Error Handling

Failures occur as early as the available information allows, with policy
defined per capability rather than one blanket rule.

Build-time failures include an invalid static catalog, missing typed browser
modules, JavaScript compilation failures, and missing generated bundles.

CLI or startup failures include retired or mixed configuration shapes,
unknown definitions, invalid integration settings, unresolved qualified
provider references, duplicate routes, incompatible capabilities, invalid
auction plans, and unavailable assets.

Runtime hooks retain their current `Report<TrustedServerError>` context and HTTP
behavior. Request filters and HTML hooks preserve their existing propagation;
provider failures remain materialized provider outcomes; mediator launch or
parse failure continues to warn and fall back to local ranking; renderer
failure follows the per-bid rule above. Moving a concrete call behind a registry
must not change that hook's policy or introduce a panic.

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

The intentional compatibility breaks are:

- Auction providers move from `[auction.providers]` beneath their owning
  integration and references become qualified.
- APS providers no longer activate without an enabled `[integrations.aps]`
  parent.
- `js_asset_proxy` is no longer implicitly first; migration examples and
  procedures place it first to retain existing behavior unless the operator
  reorders it.
- Provider failure responses and mediator input stop using the two remaining
  lexical recovery sorts and use configuration order everywhere.
- Conflicting trusted attributes for the unified script tag fail composition
  instead of warning and keeping the first value; identical duplicates still
  collapse.
- Stored data gains the application schema and explicit order sidecars. The
  rollout decoder, not the steady-state schema, provides temporary old-blob
  compatibility.

This specification does not preserve or coexist with PR #1084's current
`[integration]`, `[demand]`, and `[adserver]` configuration convention. That
conflict is resolved in favor of this single `[integrations]` design rather
than hidden behind aliases or precedence rules.

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
and ordering changes.

The first merge milestone contains workstreams 1 through 3 and retains the
current operator schema and provider activation semantics through a temporary
normalization adapter. It delivers the crate boundary without an operator
cutover. The second milestone contains workstream 4, introduces stored schema
2 and the rollout decoder, and atomically replaces the temporary source
normalizer with the new source parser.
Intermediate commits may add unused neutral contracts or new crates, but no
merged state may have two active catalogs, two simultaneously interpreted
provider inventories, or adapter-specific composition paths. This scope does
not include the external plugin ecosystem proposed by PR #1084.

The milestone-one normalizer is a compatibility boundary, not a second
catalog. It accepts only the current operator and stored shape, resolves current
profile strings through the new static definitions, preserves the current fixed
integration-builder order, lexical provider priority, and implicit APS
activation, and emits the one neutral model consumed by composition. Milestone
two atomically replaces that source parser with the new `[integrations]` parser;
it does not accept both operator inventories. Only the read-only stored-blob
decoder continues to accept the complete legacy shape during rollout.

## Migration Sequence

Implementation may use small commits, but the merged workspace must never have
two active integration or provider inventories.

Steps 3 through 5 form the first atomic merge milestone. Preparatory commits may
compile and parity-test copied code in an unused new crate while the old catalog
remains authoritative, but the final milestone switch rewires every consumer
and deletes the old concrete sources together. No deployable revision selects
some integrations or browser assets from each catalog.

1. Add neutral capability, processing-requirement, browser-asset, OpenRTB
   profile/exchange, and mediator contracts to core. Extend the `test-utils`
   feature with only neutral stubs required by extracted integration tests.
2. Replace the closed APS and Prebid profile variants and the mediator's legacy
   `AuctionProvider` use while implementations are still in core. Convert GPT
   diagnostics and DataDome call sites to the neutral lifecycle contracts.
3. Create `trusted-server-integrations-js`, move all integration browser
   sources, tests, GPT bootstrap, and shared fixtures, remove every browser-core
   APS import, and make the composed asset set authoritative for bytes and
   hashes.
4. Create `trusted-server-integrations` with the explicit static catalog. Move
   ordinary integrations first, then move DataDome and GPT diagnostics after
   their lifecycle seams, APS and Prebid after the profile seam, and
   `adserver_mock` after the mediator seam. Add the new `openrtb` adapter.
5. Move `TrustedServerAppConfig`, all integration-specific configuration,
   validation, inactive-secret preprocessing, and secret metadata into the
   integrations crate. Rewire the CLI to the source-validation entry point and
   all adapters to the single runtime composition entry point while retaining
   the existing operator schema through the temporary normalizer. This is the
   behavior-preserving crate-split milestone.
6. Add the TOML source pre-pass, ordered in-memory maps, application schema 2,
   object-shaped storage with explicit order sidecars, strong qualified
   provider IDs, the dual stored-schema reader, and the breaking
   integration-owned provider schema. Atomically replace and remove the
   milestone-one old-source normalizer so this CLI accepts only the new source
   inventory.
7. Replace every lexical provider recovery sort with compiled-plan ordinals,
   update adapter backend correlation naming, and activate configuration-order
   semantics only after their parity and failure-path tests pass.
8. Update examples, fixtures, operator documentation, migration diagnostics,
   CI aliases, browser scripts, and the rollout runbook. Deploy the dual reader,
   then push schema 2 according to the rollout section.
9. Remove the read-only schema-1 blob decoder only in the later release defined
   by the rollout contract.

When files leave core, the Fastly-SDK migration guard is not weakened. Its
integration `include_str!` entries move to an equivalent guard owned by
`trusted-server-integrations`; neutral core entries remain in core. Integration
tests move integration-owned fixtures and helpers outward. Only platform-neutral
stubs become public under `test-utils`; production APIs are not widened merely
to preserve `cfg(test)` imports.

## Testing and Verification

### Discovery and dependency tests

- The static Rust catalog contains exactly the sixteen expected definitions,
  and every valid `src/<id>/mod.rs` directory appears exactly once.
- Invalid, missing, or duplicate catalog IDs fail tests or compilation.
- JavaScript-only and Rust-only directories are accepted.
- A typed Rust reference to an absent browser module fails compilation.
- Embedded bundle hashes match built bytes.
- Core has no dependency on either integrations crate.
- Browser core imports no integration source.
- Adapters and CLI import no concrete integration module.

### Configuration and ordering tests

- TOML parent-table order becomes the integration-owned source-model order.
- Nested provider declaration order is retained.
- A parent integration or provider table declared after one of its descendants
  fails before typed deserialization.
- Missing `enabled` fails; omitted integration tables remain inactive.
- TOML-to-envelope-to-runtime round trips preserve both order sidecars exactly,
  independent of JSON object-member order.
- Order sidecars reject missing, duplicate, extra, or mismatched map keys.
- Schema-1 stored blobs normalize through the transition reader; unknown schema
  values fail before secrets are resolved.
- Config-store loading produces the same registry, JavaScript, and provider
  order that the CLI validated.
- CLI validation never constructs runtime capabilities from unresolved secret
  key names; the post-resolution runtime phase rejects unresolved values.
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
- Transport-failure recovery paths retain plan order and never fall back to
  lexical provider sorting.
- APS, Prebid, and `openrtb` fixtures cover enabled parents, disabled-parent
  retention, missing-enabled rejection, and the nested provider migration.
- Qualified IDs that alias under Axum's legacy normalization receive distinct
  correlation names or fail target validation before deployment.

### Capability and behavior parity tests

- The static catalog contains all current integration IDs plus `openrtb`.
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
- Active DataDome secrets are presence-checked and resolved through unchanged
  object paths; inactive protection and bypass secrets are removed before the
  shared resolver.
- Request-processing requirements preserve DataDome origin bypass, full-body
  buffering, and final private caching, and prevent request-private GPT
  diagnostics state from entering ESI templates.
- The dedicated mediator capability preserves request construction, ordered
  response input, bounded transport, parsing, and local-ranking fallback without
  exposing the legacy `AuctionProvider` trait.

### Browser tests

- Output order is core, creative prelude, and configured integrations.
- Immediate and deferred lists preserve configuration-relative order.
- APS is absent from browser core and registers its renderer from its own IIFE.
- Core, GPT, and Prebid artifacts contain no private copy of APS renderer state.
- Existing APS validation, sandbox, messaging, timeout, and rendering tests pass
  through neutral dispatch.
- APS remains immediate and reads its rendering mode from the synchronous
  unified tag; a deferred APS renderer is rejected during composition.
- Equal duplicate trusted script attributes collapse, while conflicting values
  fail composition before HTML is served.
- Missing or rejecting renderers drop only the renderer-bearing bid with no
  fallback or pre-acceptance side effect; duplicate registration poisons the
  type or fails composition.
- Unified, deferred, standalone, and inline assets expose bytes and hashes that
  match the emitted document fingerprint and static responses.
- Changing any integration setting that affects generated head output changes
  the document fingerprint; request-dependent head variation bypasses shared
  template reuse through processing requirements.
- GPT bootstrap and APS shared fixtures resolve from their integration-owned
  package locations.

### Repository gates

Before handoff, run every gate required by `AGENTS.md`, including Rust format,
all target-matched clippy aliases, Fastly/Axum/Cloudflare/Spin tests, CLI and
cross-adapter parity tests, required native and WASM builds, JavaScript builds
and Vitest suites for both browser source roots, JavaScript formatting, and
documentation formatting. The explicit package lists in every Fastly
build/check/clippy/test alias include both new Rust crates where applicable;
host-target tests still run catalog-completeness and integration test support.
The migrated Fastly-SDK guard continues to scan the moved integration sources.

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

Mitigation: use ordered in-memory types, object-shaped stored configuration,
and explicit order sidecars in the hash-verified blob. Enable
`toml/preserve_order`, reject descendant-before-parent source declarations with
the `toml_edit` pre-pass, and test the complete push/store/load path rather than
only the TOML parser.

### Binary and stored-config cutover drift

An old binary cannot consume application schema 2, and a binary rollback after
config cutover would otherwise fail at startup.

Mitigation: deploy the dual reader before pushing schema 2, archive the prior
envelope, require restoration before binary rollback, and remove the legacy
reader only in a later release.

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

Mitigation: both call the same catalog-backed source-validation API, and only
adapter startup continues through the post-secret runtime composition API. No
secondary validation inventory is allowed. Adapters receive the already
composed settings, plan, registry, and browser assets rather than reconstructing
any of them.

### Stale or incorrectly ordered browser artifacts

Splitting Rust ownership while sharing one Node workspace can embed previous
output, race build scripts, or load APS too late.

Mitigation: coordinate the one workspace's build/install lock, retain
stale-output refusal, hash built bytes, load core and the creative prelude
first, reject deferred APS composition, and run artifact-level renderer,
fingerprint, and ordering tests.

## Acceptance Criteria

The change is complete when:

1. Both new crates are workspace members and statically linked by the CLI and
   every adapter where required.
2. All fifteen current concrete Rust implementation units live under
   `trusted-server-integrations/src/<id>/`.
3. The standard provider configuration is supplied by the built-in Rust-only
   `openrtb` integration, making sixteen static definitions in total.
4. All integration browser sources, assets, fixtures, and tests live under
   `trusted-server-integrations-js`.
5. Rust definitions use one explicit compile-checked catalog with a directory
   completeness test; browser modules remain directory-discovered.
6. Core owns only neutral contracts and execution engines and imports no
   concrete integration.
7. One integration can register multiple typed capabilities; APS, Prebid, and
   `adserver_mock` are not special construction paths.
8. `[integrations]` is the only concrete integration and auction-provider
   inventory.
9. Configuration and provider ordering survive config push and runtime loading
   through validated order sidecars and define the documented auction priority.
10. The old `[auction.providers]` schema is rejected with targeted migration
    guidance.
11. The compiled auction plan retains PR #1016 behavior after normalization,
    except for the explicit change from lexical to configuration-order provider
    priority.
12. Browser core imports no concrete integration, and APS rendering works
    through one immediate registration without private copies in core, GPT, or
    Prebid bundles.
13. `TrustedServerAppConfig`, integration secret handling, and final runtime
    composition are owned by `trusted-server-integrations`; core has no concrete
    config or loader dependency.
14. The CLI and adapters use the same catalog-backed source validation, and all
    adapters receive one post-secret-resolution
    settings/plan/registry/browser-assets composition.
15. OpenRTB request-local state crosses the transport boundary through a
    prepared response parser without `Any` or vendor enum variants in core.
16. Explicit `enabled`, parent-before-descendant, disabled-retention, local-ID,
    and qualified-ID rules have end-to-end tests.
17. Schema-1 blobs remain readable for the documented rollout release, schema-2
    blobs preserve existing secret paths, and binary rollback requires verified
    restoration of the archived schema-1 envelope.
18. Browser assets carry bytes and hashes through composition, publisher
    template fingerprints vary with every composition-time external or inline
    asset change, and request-dependent head variants bypass shared reuse.
19. Core test support, the Fastly-SDK migration guard, Cargo aliases, CI,
    Dependabot, browser scripts, and the CLI Prebid builder cover the new crate
    boundaries.
20. The full repository verification gates pass.

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
