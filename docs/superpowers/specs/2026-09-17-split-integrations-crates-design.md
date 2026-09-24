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

The compatibility baseline is the behavior shipped on `origin/main` at
`4c6d26a16`, not either prior pull request discussed below. This design changes
only the configuration, activation, and ordering behavior called out explicitly
in this document; all other current runtime, browser, CLI, and cache behavior is
preserved.

This remains one design, but it has two merge milestones. The crate and runtime
boundary moves first without changing operator configuration. The ordered,
integration-owned configuration cuts over only after the behavior-preserving
boundary is running. The milestones share one target architecture without
forcing the packaging move and configuration migration into one deployment.

## Context

On `origin/main` at `4c6d26a16`, neutral registry machinery and concrete integrations
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

The same baseline includes managed Prebid User IDs and their OpenRTB EID/EC
flow, LiveRamp configuration through that existing managed-ID facility,
analytics-adapter selection in external Prebid bundles, cookie-keyed publisher
template caching, additional CLI ad-template and audit config consumers, and
documentation-snippet verification. These are current behavior and remain in
scope for parity even though they landed after this design was first drafted.

`core/src/ec/prebid_eids.rs` is historically named after the first browser
producer, but its `ts-eids` ingestion, consent checks, EC finalization,
partner-graph ingestion, and admin diagnostics are shared identity machinery.
They remain in core under their current name for this split. Renaming the
neutral module is unrelated cleanup and is deferred. Prebid-owned configuration
and browser-module management move out; the shared EID/EC machinery does not
become a new capability family, and LiveRamp does not become another
integration definition.

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

## Historical Pull Requests (Non-Normative)

The following pull requests explain how some current code arrived in the
repository. They are not design authorities for this specification. The
normative inputs are the decisions in this document and behavior present on the
current baseline above.

### PR #1016

PR #1016 introduced the ancestor of the current compiled auction plan. The
following properties are now baseline repository behavior and are preserved
because current consumers depend on them, not because the PR is authoritative:

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

PR #1084 explores a broader external-provider and plugin ecosystem. That scope
and its alternate configuration convention are not inputs to this design. This
specification independently chooses static workspace crates, typed capabilities
needed by current implementations, one ordered `[integrations]` inventory,
explicit `enabled`, and APS as an integration. No `[integration]`, `[demand]`,
or `[adserver]` selector convention is carried forward.

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
8. Preserve behavior on the current `origin/main` baseline except for the
   explicitly documented configuration, activation, and ordering changes.
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
- New identity, EC, geo, device, or permission-signal provider systems. The
  existing neutral `ts-eids`/EC flow and managed Prebid User ID behavior remain
  supported.
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
      build-all.mjs
      build-prebid-external.mjs
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
          renderer-document.html
        creative/
          index.ts
        datadome/
          index.ts
        prebid/
          index.ts
          user_id_modules.json
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
integrations remain valid without a browser directory. Cross-adapter Playwright
and parity tests remain in `trusted-server-integration-tests`; their paths and
load-order assertions change, but system tests do not become source owned by
one integration crate.

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
5. The CLI uses the source-validation APIs from
   `trusted-server-integrations`; every adapter uses its single runtime
   composition entry point from that crate. Both phases resolve the same static
   catalog and integration schemas.
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
- Pure source parsing, structural validation, and catalog-validation APIs used
  by the CLI before EdgeZero's typed validate, diff, and push mechanics.
- The public runtime entry points that load a config-store blob and return one
  composed runtime value.
- A validated source-config view used by non-runtime CLI commands.

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

The source phase returns a conceptual `ValidatedSourceConfig`. It retains the
typed operator configuration and exposes only the views CLI consumers need:
neutral global settings, ordered integration and qualified-provider metadata,
and integration-owned read models such as Prebid external-bundle inputs. This
is not a runtime registry and contains no resolved secret values or executable
capability objects.

That runtime value, conceptually `TrustedServerComposition`, contains the
validated neutral `Settings`, one `Arc<AuctionPlan>`, one
`IntegrationRegistry`, the composed `BrowserDocumentAssets`, and a canonical
digest of the complete composition snapshot defined below. Adapters consume
this value; they do not separately compile the auction plan, rebuild the
integration registry, enumerate browser bundles, or reconstruct the digest.

Core retains neutral config-store access, Fastly chunk reconstruction, blob
envelope verification, preprocessing for inactive neutral/global secret
references, secret-resolution primitives, global settings types, and
auction-plan compilation. Today the neutral preprocessor removes inactive
Tinybird token references and disabled EC-partner pull-token references; that
behavior remains in core. These helpers accept or return neutral data and never
call the concrete catalog. The integration crate calls them in this order:

```text
config-store bytes
  → core chunk reconstruction and envelope verification
  → core-owned neutral/global inactive-secret preprocessing
  → catalog-owned integration inactive-secret preprocessing
  → aggregated core + integration secret resolution
  → catalog-aware config validation
  → core AuctionPlan compilation
  → core IntegrationRegistry construction from typed registrations
  → browser asset composition and document fingerprint
  → TrustedServerComposition
```

The CLI imports `TrustedServerAppConfig`, `ValidatedSourceConfig`, and pure
source-validation APIs from `trusted-server-integrations`. The host-only CLI
continues to own the config command wrappers and the direct `edgezero-cli`
dependency; `trusted-server-integrations`, which is linked into every WASM
adapter, never depends on `edgezero-cli`. Each CLI wrapper performs the
source-aware pre-pass and source-phase catalog validation before delegating
storage and diff mechanics to EdgeZero's typed CLI functions.

Because locked EdgeZero accepts paths rather than validated bytes, a wrapper
copies the operator config and manifest inputs to private mode-`0600`, immutable
temporary snapshots, rewrites the delegated arguments to those snapshots, and
removes them afterward. The manifest snapshot is created beside the original
manifest so all manifest-relative adapter paths retain the same base directory;
failure to create the secure snapshot aborts before remote I/O. Snapshot paths
in errors and the validate-success line are rewritten or replaced with the
original operator paths. The config and manifest bytes used to choose the
adapter, store, and app-config path are therefore the same bytes EdgeZero
processes; a concurrent edit cannot redirect the delegated operation. No
EdgeZero source change or new host service is required.

Read-only `config ad-templates` and `audit ad-templates` commands load the
effective source view with the existing optional environment overlay. Mutating
or generator commands load file bytes without the overlay, so environment-only
values are never persisted. Recovery-oriented ad-template generation may run a
structural-only pre-pass against an otherwise invalid baseline, preserving its
current warning and non-disclosure behavior. Candidate and baseline then use
the same complete integration-owned validation path: a valid candidate is
written; an invalid candidate is refused when the baseline was valid; and an
invalid candidate over an already-invalid baseline retains the current warning
and atomic-write escape hatch without disclosing source values.

`ts prebid bundle` obtains typed bidder, User ID, analytics, and managed-module
requirements through an integration-owned partial source view rather than a
duplicate CLI schema. It intentionally does not require unrelated app
configuration or an `external_bundle_url` to be deploy-valid: `bundle.modules`,
`external_bundle_sha256`, and `external_bundle_sri` are inert staging metadata,
while `external_bundle_url` activates the browser capability. The command
retains its current ability to build first, atomically patch hash/SRI metadata,
and tell the operator to upload and set the URL. It validates the structural
pre-pass and affected Prebid subtree before writing; full app validation remains
the contract of config validate/push. Provider diagnostics display qualified
providers in configuration order rather than alphabetizing a detached map.

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
`trusted-server-js`; integration sources, owned unit/artifact fixtures, and
owned unit/artifact tests live under `trusted-server-integrations-js`. The
shared TypeScript, lint, format, and test configurations include that sibling
source root explicitly. Separate build targets emit neutral and integration
artifacts into owner-specific output directories and validate per-target
manifests before embedding them. One cross-process lock covers dependency
installation, output cleanup, build execution, discovery, manifest validation,
and artifact copy, so parallel Cargo build scripts cannot race or consume stale
or partially replaced output.

The integration build discovers immediate directories containing `index.ts`
and emits one self-contained IIFE per entry point. Its Cargo build embeds each
output and its SHA-256 hash. CI, browser integration scripts, and the CLI
Prebid builder use the single workspace root rather than maintaining a second
dependency graph; Dependabot continues to watch only its one lockfile.

Canonical npm build, typecheck, lint, format, and test commands include both
source roots explicitly, and CI invokes those commands rather than core-only
paths. Both Rust build scripts emit complete `rerun-if-changed` coverage for
their own manifest, configuration, and source inputs, including the sibling
integration root. Clean and incremental build tests change one integration
source and prove the integration manifest is regenerated without spuriously
changing the neutral manifest.

`build-prebid-external.mjs` and its npm command remain at the canonical Node
root as build orchestration, not browser runtime. The Prebid registry, aliases,
shims, and other integration-owned source inputs move with Prebid into
`trusted-server-integrations-js`. The launcher receives their resolved sibling
paths explicitly and has no hard-coded `src/integrations/prebid` assumption.
The CLI resolves the canonical package root for dependencies and obtains the
integration-owned input paths and typed module requirements from the
integrations facade; it does not locate a registry through its own relative
path constant.

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
4. For each enabled integration, construct the typed capability registrations
   activated by its validated settings, in integration order restored from the
   storage sidecar.
5. Collect integration-owned auction provider instances and profiles.
6. Ask core to compile the single canonical auction plan.
7. Resolve typed browser modules and construct the neutral integration
   registry.

An absent integration is inactive. Every explicit parent integration table must
contain `enabled = true` or `enabled = false`; there is no integration-specific
default. `enabled` is the integration's master gate, not an assertion that every
optional capability is configured. An explicitly disabled integration may
retain its settings and provider instances but contributes no runtime
capabilities or providers. Configuration cannot activate APS or Prebid through
an auction plan while omitting its parent, and bidder or mediator references to
a disabled integration fail validation.

Provider-owning integrations use the following explicit activation rules. They
do not add capability names or implementation discriminators to operator
configuration.

| Integration     | Enabled configuration                            | Runtime contribution                                                                                                                            |
| --------------- | ------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `aps`           | No provider instances                            | No runtime contribution; retained settings remain structurally validated.                                                                       |
| `aps`           | One or more provider instances                   | One OpenRTB provider plan per instance plus coupled APS renderer/head/browser support and the mode-dependent proxy required by those providers. |
| `prebid`        | No providers and no `external_bundle_url`        | No runtime contribution; retained settings remain structurally validated.                                                                       |
| `prebid`        | Provider instances, but no `external_bundle_url` | Server-side OpenRTB provider plans only; no proxy, rewriter, head injector, managed browser User IDs, or deferred browser module.               |
| `prebid`        | Valid `external_bundle_url` and browser settings | Existing Prebid proxy, rewriter, head injector, managed User IDs, and deferred browser module, with zero or more server providers.              |
| `openrtb`       | Zero or more provider instances                  | One standard OpenRTB provider plan per instance; zero instances is a valid staged no-op.                                                        |
| `adserver_mock` | Enabled parent                                   | The existing mediator capability, independently of provider count.                                                                              |

Thus a current server-only Prebid provider migrates beneath an enabled Prebid
parent without activating Prebid's page/browser behavior. Browser-only Prebid
continues to be selected by the same required external-bundle URL that current
startup validation already uses. Runtime browser settings such as managed User
IDs or client-side bidders without that URL fail validation rather than
activating a partial browser path. CLI-only `bundle.modules` and generated
hash/SRI metadata may be staged without a URL and never activate runtime
browser behavior. For APS, a configured provider necessarily activates its
renderer support; an enabled APS parent with no provider is a staged no-op.

Requiring the enabled parent is an intentional activation change from the
current split inventory. Today an APS auction profile can activate rendering
support without an enabled `[integrations.aps]` block. After cutover, every APS
provider requires an explicit enabled parent, and its coupled server and
renderer capabilities activate together.

Disabled configuration receives schema-safety validation only: unknown fields,
wrong types, integration/provider ID grammar, duplicates, and secret-reference
name/store-reference/collision/adapter rules still fail. Active-only required
fields and value validators—including ranges, endpoint policy, format patterns,
and cross-field rules—are deferred until the integration is enabled. This preserves current
disabled placeholders such as the example Google Tag Manager container while
preventing malformed structure or secret references from being stored. At
runtime, integration-owned preprocessing removes inactive secret paths before
value resolution, so disabled behavior does not leak into the runtime plan.

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

The legacy `protocol = "openrtb-2.6"` field is also retired and rejected with
migration guidance. Every provider capability in this design uses the existing
OpenRTB 2.6 engine, so repeating its one accepted protocol value adds no choice.
A future non-OpenRTB engine requires a separate capability design rather than a
string switch in this schema.

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
7. Every hook list and each immediate/deferred JavaScript list retains
   integration order. When one integration registers multiple hooks of the same
   capability, their relative order is the explicit order returned by that
   integration's definition; the integration remains one operator-visible
   position and does not expose a second priority mechanism.
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
10. Shared browser dispatchers execute handlers by the owning integration's
    configured ordinal and then definition-local registration order. Wall-clock
    registration timing, numeric priority, and lexical handler ID are not
    alternate ordering mechanisms.

Ordered runtime introspection carries the configured ordinal with each
integration and provider. Lookup indexes may use maps, but iterating a
`BTreeMap`, `HashMap`, or alphabetized metadata view never defines or displays
execution priority. CLI diagnostics and registry metadata that show order use
the ordered plan/registration view.

The current hard-coded requirement that `js_asset_proxy` remain the first
rewriter is retired. Rewriter chaining follows the same operator-visible
integration order as every other hook. Migration guidance places
`js_asset_proxy` first in migrated examples and procedures so the old behavior
is preserved by default, while an operator may deliberately choose a different
order. No engine-only priority is hidden from the configuration.

Browser load mode remains a lifecycle phase, not a second operator priority:
immediate code necessarily evaluates before deferred code. Config order is
preserved within each phase and remains the stored ordinal used by any shared
dispatcher after a module registers. A hook that must arbitrate during initial
document mutation, including a DOM-insertion guard, is immediate-only;
composition rejects it on a deferred asset. Diagnostics display each module's
fixed load mode so this phase boundary is visible rather than inferred from
timing.

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

The direct `toml_edit` dependency is pinned to the same TOML 1.1 parser
generation as the workspace `toml` package and EdgeZero's typed parser. Parser
parity fixtures cover otherwise-unrelated valid and invalid TOML syntax so the
pre-pass cannot accept a document the typed path rejects, or reject one merely
because it used an older TOML grammar.

Every entry point that accepts TOML uses this pre-pass, including local loading,
CLI validate/diff/push, ad-template diagnostics and candidate validation, and
the Prebid bundle command. Recovery mutators may request the structural-only
mode described above. Ad-template generation applies the comparative
candidate/baseline rule, and the Prebid builder validates only its owned partial
view; neither recovery path is silently tightened into unconditional full-app
validation. EdgeZero's typed mechanics remain responsible for overlay,
validation invocation, diff, envelope construction, consent, and store writes
after the pre-pass succeeds.

EdgeZero does not currently expose a pre-parse hook. The host-only CLI wrappers
therefore duplicate its app-config path rule: an explicit `--app-config` wins;
otherwise the path is `<manifest-dir>/<app.name>.toml`. A wrapper reads the
manifest and source once, performs the pre-pass, and delegates typed processing
against private immutable snapshots of those exact bytes; it does not validate
one read and allow EdgeZero to reopen a concurrently changed operator or
manifest file. Parity tests cover exact-byte delegation, diagnostic path
rewriting, explicit and default paths, manifest paths with and without parent
directories, and `--no-env`. The environment overlay can replace only scalar
leaves already present in TOML; it cannot create an omitted `enabled` field,
integration, provider, table, or array. Operator templates must contain every
leaf intended for overlay.

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
removed when inactive. Before storage, EdgeZero's static secret metadata still
validates every reference actually present in source, including a reference
retained under a disabled integration; runtime filtering prevents inactive
value resolution, not source syntax or adapter validation.

The private Rust type names may differ, but the serialized order must be
explicit and covered by compatibility tests across:

```text
trusted-server.toml
  → typed CLI configuration
  → hash-verified blob envelope
  → config store
  → runtime composition
  → Settings, registry, JavaScript lists, configuration digest, and AuctionPlan
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
composition root combines it with catalog metadata. Core also retains its
neutral/global inactive-secret preprocessor for Tinybird and EC partners; the
composition root runs both core and catalog preprocessors before one shared
resolution pass.

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
- Integration configuration not accepted by its owner under the enabled or
  disabled validation phase defined above.
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

| Previous configuration                                          | New configuration                                                                                     |
| --------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| `[auction.providers.pbs-main]` with `profile = "prebid-server"` | `[integrations.prebid.auction.providers.pbs-main]`                                                    |
| `[auction.providers.aps-main]` with `profile = "aps"`           | `[integrations.aps.auction.providers.aps-main]`                                                       |
| APS provider with no `[integrations.aps]` parent                | Add `[integrations.aps]` with `enabled = true`                                                        |
| Server-only Prebid provider with no Prebid browser block        | Add `[integrations.prebid]` with `enabled = true`; omit `external_bundle_url` and browser-only fields |
| Existing browser Prebid block with `enabled = true`             | Retain its browser fields and nest any server providers beneath the same enabled parent               |
| Any retained integration parent that relied on a default        | Add an explicit `enabled = true` or `enabled = false`                                                 |
| A standard profile provider named `example-direct`              | `[integrations.openrtb.auction.providers.example-direct]`                                             |
| `[auction.providers.<id>.profile_config]`                       | Flattened into the owning integration's provider table                                                |
| `protocol = "openrtb-2.6"`                                      | Remove it; the registered provider capability fixes the protocol                                      |
| Bidder route `provider = "pbs-main"`                            | `provider = "prebid.pbs-main"`                                                                        |

Old `[auction.providers]`, `profile`, `profile_config`, and `protocol` fields
fail with an actionable message naming the new integration-owned location or
instructing the operator to remove the fixed protocol. A mixed old/new operator
configuration also fails. There is no deprecation interval for operator TOML.
The temporary old-blob reader is not an accepted source format and does not
make old fields valid in the new CLI.

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
4. Freeze configuration writes and fence old CLI artifacts from the deployment
   credentials or release path; only the schema-2 CLI may write after this
   point.
5. Push the migrated schema-2 configuration with the new CLI and read back or
   otherwise assert `trusted_server_schema = 2` from the stored envelope.
6. Verify registry order, provider order, browser asset hashes, and auction
   health before declaring the cutover complete.

The rollout runbook must name and drill a concrete export and restore mechanism
for every deployed adapter before milestone 2. This is an external release
precondition, not an assumed CLI feature. In particular, the current default
remote Spin deployment path cannot read deployed config through the locked
EdgeZero CLI; it must use a verified platform/control-plane export and restore
facility or the schema-2 rollout for that target is blocked.

An old binary must never serve a schema-2 blob. Rolling back after step 5 first
restores the archived schema-1 envelope, verifies that restoration, and only
then rolls the binary back. If the platform cannot coordinate those operations,
the release is paused rather than accepting an outage window. The compatibility
decoder is removed only in a later release after every supported deployment has
completed the schema-2 cutover.

The write fence remains until old CLI credentials/artifacts can no longer push.
Because the dual reader intentionally accepts schema 1, schema-1 reappearance
after cutover is an explicit rollback event, never a tolerated ordinary write;
release monitoring alerts on it and the runbook requires either immediate
schema-2 restoration with the new CLI or the complete binary-rollback sequence.

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

Registry requirements are an additional monotonic veto, never an alternate
cache authorization path. Shared-template eligibility remains the conjunction
of all current method, request-cache, diagnostics, key-cookie, bypass-cookie,
unlisted-cookie, cookie-independent-origin, assembly-mode, and origin-response
checks plus the registry requirement. The same combined request decision still
governs both warm lookup and cold-store authorization. A registry hook can make
an otherwise shareable request private or origin-bound; it cannot make a
request shareable when any existing cookie or cache gate rejected it.

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
state is never copied into a shared template. Registry preparation remains at
both existing locations: adapter boundaries and the idempotent core publisher
safety-net boundary used by direct core callers. Request extensions make a
second preparation a no-op. Core invokes finalization on the existing response
path. The current GPT-enabled auction-correlation check becomes a neutral
registry/request-state query, so core retains neither a concrete GPT import nor
a new lifecycle call site.

## Browser Composition and APS Renderer

`trusted-server-js` builds only the neutral core IIFE. Before extraction, the
implementation audits every production value import from an integration or the
fixed `creative` prelude into today's `core/` and `shared/` trees. Each import
is classified rather than copied blindly:

- Stateful facilities remain owned once by browser core and are exposed through
  one versioned `TrustedServerBrowserRuntime` namespace. The initial surface
  includes logging, context-provider registration and context collection,
  auction request construction and normalized response parsing, queue
  installation, slot lookup and rendering helpers, first-impression state, DOM
  insertion-handler registration, and renderer registration/dispatch/lifecycle
  operations.
- Pure stateless helpers may move to an integration-owned shared source module
  and be bundled into the consuming IIFE. They must not close over or initialize
  browser-core state.
- Types are imported from declaration-only entry points. There are no runtime
  relative imports across the two crate source roots.

Runtime calls use a stateless accessor mapped by the integration build to the
already-installed namespace. The integration build fails on any undeclared
cross-root value import. This covers current imports such as Permutive context
registration, Prebid auction helpers, Testlight queue installation, GPT slot
resolution, and the shared script/beacon guards; it is not limited to the APS
renderer examples.

The core IIFE initializes exactly one stateful registration object on the
Trusted Server browser namespace before any integration IIFE runs. Integration
bundles consume that object through an external runtime shim and type-only
browser-core declarations; their bundler must not inline the stateful registry
implementation. Artifact tests prove that state registered by an integration
IIFE is visible to the already-loaded core IIFE, including a Permutive context
provider observed by core collection and a renderer observed by GPT and Prebid.

`trusted-server-integrations-js` builds integration IIFEs. Immediate modules
are concatenated in the ordering contract above. Deferred and standalone
modules remain separate assets but retain their configuration-relative order
and typed identities.

The browser DOM-insertion dispatcher follows the same simple ordering rule as
Rust hooks. Its current numeric priority and ID-lexical sort are removed.
Composition installs the immutable integration ID-to-ordinal mapping before any
IIFE executes. A handler registers under its owning integration ID and executes
by configured ordinal, then by that integration's local registration sequence.
Handler IDs remain diagnostic identities only, and DOM-insertion handlers are
immediate-only so an absent deferred handler cannot observe mutations too late.
Reversing two configured integrations therefore reverses the winner when their
script guards both claim the same candidate; no hidden browser priority or
bundle timing can override TOML order.

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

Static serving, cache-busting URLs, and immutable-cache validation consume
`BrowserDocumentAssets`; core no longer performs a crate-global
`all_module_ids()` lookup. Its document fingerprint includes only assets that
can affect the composed document and includes GPT bootstrap bytes. Each head
injector whose generated inline output varies with integration configuration
supplies the exact immutable bytes or a deterministic contribution for that
output. Request-dependent head variation is permitted only when its neutral
`RequestProcessingRequirements` bypass shared-template reuse; request data is
never folded into a composition-wide fingerprint.

Publisher template invalidation is broader than the browser document. During
composition, `trusted-server-integrations` hashes a canonical serialization of
the complete composition snapshot: resolved neutral `Settings`, resolved
enabled-integration configuration, and structurally validated retained source
configuration for disabled integrations, all in configured integration and
provider order. Active secret values enter the hash after resolution; inactive
secret references remain unresolved retained source values. Hashing the
complete model deliberately over-invalidates so a future rewriter,
postprocessor, proxy mapping, cookie policy, or other HTML-shaping field cannot
be omitted from a hand-maintained allowlist. Input bytes and resolved secret
values are fed directly to the digest and are never logged, returned, or used
as cache-key plaintext.

“Canonical” is a versioned encoding contract, not ordinary `Serialize` output.
It preserves the explicitly ordered integration/provider sequences, sorts keys
of semantically unordered maps and elements of semantically unordered sets,
uses length-delimited domain-separated fields, and has golden vectors. Tests
build equivalent `HashMap`/`HashSet` values in different insertion orders and
require the same digest, while reversing an operator-ordered sequence must
change it.

Core computes the existing template fingerprint from that configuration digest
and `BrowserDocumentAssets.document_fingerprint`. This composite replaces only
the old complete-`Settings` plus global-bundle digest; it does not replace any
other `TemplateCacheKey` dimension. Full URL, request host and scheme, origin
identity, assembly mode, ordered `Vary` values, selected cookie values, and
`TEMPLATE_SCHEMA_VERSION` remain independent key inputs. Transform-shape
changes still bump `TEMPLATE_SCHEMA_VERSION`. Tests prove neutral settings,
non-head integration rewriter settings, external and inline assets, and cookie
policy changes invalidate templates without exposing raw configuration.

Browser core currently imports APS renderer logic directly. Replace that
reverse dependency with one neutral renderer registration mechanism. A
renderer-type handler owns descriptor validation, render dispatch, any
renderer-specific bounded capability state, and the renderer metadata needed
to construct a Prebid Universal Creative response. Browser core owns only the
type-keyed handler registry and neutral calls into it:

- Core parses the existing renderer envelope far enough to identify its type
  and retain its opaque payload.
- The APS IIFE registers the one APS handler. GPT and Prebid call the neutral
  registry and never import APS source; no APS source or private APS state is
  inlined into core, GPT, or Prebid.
- Prebid validates a descriptor before bid admission, carries it only until
  Prebid assigns an `adId`, and scrubs the carrier from both the normalized bid
  and metadata. On `bidAccepted`, with the current `bidResponse` fallback, it
  registers a bounded-TTL capability containing the `adId`, ad-unit binding,
  validated descriptor, and `markWinningBidAsUsed` callback. Registration
  failure demotes the bid to the current negative-CPM failure state.
- GPT authenticates the requesting iframe against the expected slot or ad unit
  before it consumes a capability. Consumption is compare-and-consume atomic,
  source-bound, TTL-bounded, and replay-safe. Only the selected renderer handler
  supplies its renderer source, version, URL, payload, and dimensions for the
  Universal Creative response; GPT does not know APS constants.
- Server-originated renderer descriptors use the same registered validation and
  dispatch handler while retaining the current slot-scoped replay protection.
  The Prebid `adId` path and server-bid path remain distinct neutral entry
  points so one cannot consume the other's authority accidentally.

An enabled APS integration whose provider can emit APS renderer descriptors
includes its immediate APS browser module. The core IIFE and fixed creative
prelude load first, so APS registration is complete before a bid can render.
APS renderer registration is an immediate-only capability; composition rejects
a deferred APS renderer. Its current rendering mode continues to be read while
the synchronous unified script tag is executing, and the APS-owned trusted
attribute remains on that tag. A future switch to a standalone or deferred APS
asset requires replacing `document.currentScript` configuration first.

GPT is also immediate. Its current bootstrap reads `document.currentScript`
during module evaluation, so `data-ts-gam-attribution` remains on the unified
synchronous tag and an artifact-level test proves the value is available at
evaluation time. Moving source ownership must not silently make GPT deferred or
move that attribute to a later tag.

Renderer failure is scoped to the owning renderer-bearing bid or message. A
missing or rejecting renderer suppresses that bid with no generic-creative or
native-Prebid fallback; unrelated bids and the page continue. A duplicate type
fails Rust composition, while a defensive browser-side duplicate poisons that
type rather than using last-registration-wins. No renderer route, DOM, message,
or beacon side effect occurs before the selected handler accepts the payload.
Carrier scrubbing, failed admission, bounded capacity and TTL, source binding,
atomic consumption, replay rejection, and `markWinningBidAsUsed` are part of
the compatibility contract rather than APS-private implementation details that
may disappear during extraction.

The serialized descriptor, validation, sandbox flags, message authentication,
timeouts, and render results do not change.

The production `APS_RENDERER_DOCUMENT`, including its inline browser
JavaScript, moves from the APS Rust source into
`trusted-server-integrations-js/lib/src/integrations/aps/renderer-document.html`.
The browser crate exports its immutable bytes and hash; APS Rust serves those
exact bytes with the existing content type, CSP, and other response headers.
The document's nonce binding, sandbox, message authentication, runner load, and
failure tests move with the asset. No production APS renderer program remains
as a Rust string literal.

`gpt_bootstrap.js` moves with GPT into `trusted-server-integrations-js` and is
exported as a hashed integration-owned inline asset. GPT's Rust head injector
consumes that exported asset; core never embeds it directly. Tests resolve the
asset through the owning package instead of a cross-crate relative path. APS
golden renderer fixtures likewise move to an integration-owned shared fixture
location used by both Rust and browser tests.

The move includes a repository-wide inventory of production executable browser
programs assembled by integration Rust, not only files that already end in
`.js`. The current DataDome, Didomi, GPT, Prebid, and Sourcepoint config
initializers; Sourcepoint `_sp_` property trap; and GPT-diagnostics
activation/history bootstrap all have integration-owned static program bodies
or typed templates in `trusted-server-integrations-js`. Integration Rust may
serialize safe data, invoke the generated typed template renderer, and assemble
script tags; it does not retain handwritten browser algorithms in Rust string
literals. Exact rendered inline bytes and hashes participate in the document
fingerprint. A source/artifact guard fails when a new production executable
integration script is introduced directly in Rust without an explicitly
reviewed data-only exception.

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
- Managed Prebid User ID aliases, collision checks, consent gating, opaque
  LiveRamp envelopes, OpenRTB EID production, EC partner ingestion, and admin
  diagnostics.
- External Prebid bidder, User ID, and analytics-module selection, manifests,
  hashes, SRI values, and runtime codes.
- Cache privacy, full-buffer decisions, cookie-key and bypass policy, and every
  existing publisher template-key dimension.
- Current CLI ad-template diagnostics, audit/generator recovery behavior, and
  Prebid bundle mutation behavior.
- The route and behavioral parity of Fastly, Axum, Cloudflare, and Spin.

The intentional compatibility breaks are:

- Auction providers move from `[auction.providers]` beneath their owning
  integration and references become qualified.
- The fixed legacy `protocol = "openrtb-2.6"` field is removed from operator
  source rather than copied into each integration-owned provider.
- Every retained integration parent requires an explicit `enabled` value,
  including parents whose current schema supplies a default.
- Disabled retained blocks must deserialize against the new structural schema:
  unknown fields, wrong types, malformed IDs, and invalid retained secret
  references now fail even though active-only value checks remain deferred.
- Integration and provider parents must use ordinary table headers in
  parent-before-descendant order; dotted-key or inline-table parent shorthand
  is rejected.
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

No alternate `[integration]`, `[demand]`, or `[adserver]` selector convention
is supported alongside the single `[integrations]` design. There are no aliases
or precedence rules between competing inventories.

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
normalizer with the new source parser. Neutral contracts from workstream 1 may
land as behavior-inert preparatory commits, but milestone 1 is not complete or
deployable as the new architecture until workstreams 1 through 3 all meet its
exit criteria.
Intermediate commits may add unused neutral contracts or new crates, but no
merged state may have two active catalogs, two simultaneously interpreted
provider inventories, or adapter-specific composition paths. This scope does
not include an external plugin ecosystem.

The milestone-one normalizer is a compatibility boundary, not a second
catalog. It accepts only the current operator and stored shape, resolves current
profile strings through the new static definitions, preserves the current fixed
integration-builder order, lexical provider priority, and implicit APS
activation, and emits the one neutral model consumed by composition. Milestone
two atomically replaces that source parser with the new `[integrations]` parser;
it does not accept both operator inventories. Only the read-only stored-blob
decoder continues to accept the complete legacy shape during rollout.

Milestone exit criteria are independent:

- **Milestone 1 — crate boundary:** only the current operator and stored schema
  are accepted; fixed integration-builder order, lexical provider priority,
  implicit APS activation, and all current browser, CLI, cache, and adapter
  behavior remain unchanged. Every live consumer uses the new composition root,
  old concrete sources and the old catalog are deleted together, and the full
  repository gates pass.
- **Milestone 2 — ordered configuration cutover:** the new operator source is
  the only writable shape; the dual stored-schema reader is deployed; qualified
  identities and order sidecars are used end to end; every normal and recovery
  path uses plan ordinals; activation rules, CLI output, examples, diagnostics,
  scripts, documentation, and the rollout runbook are updated together; and the
  full repository and rollout tests pass before schema 2 is pushed.
- **Later release — cleanup:** the schema-1 reader is removed only after the
  rollback and support conditions in the rollout contract are satisfied.

## Migration Sequence

Implementation may use small commits, but the merged workspace must never have
two active integration or provider inventories.

Steps 1 through 5 comprise milestone 1. Steps 1 and 2 are independently
mergeable, behavior-inert preparation; within that milestone, steps 3 through 5
form the atomic ownership cutover. Preparatory commits may compile and
parity-test copied code in an unused new crate while the old catalog remains
authoritative, but the final milestone switch rewires every consumer and
deletes the old concrete sources together. No deployable revision selects some
integrations or browser assets from each catalog.

1. Add neutral capability, processing-requirement, browser-asset, OpenRTB
   profile/exchange, and mediator contracts to core. Extend the `test-utils`
   feature with only neutral stubs required by extracted integration tests.
2. Replace the closed APS and Prebid profile variants and the mediator's legacy
   `AuctionProvider` use while implementations are still in core. Convert GPT
   diagnostics and DataDome call sites to the neutral lifecycle contracts.
3. Create `trusted-server-integrations-js`, move all integration-owned browser
   sources, unit/artifact tests, GPT bootstrap, the APS renderer document,
   every integration-owned executable inline template, registry inputs, and
   shared fixtures; complete the cross-root import audit; remove every
   browser-core APS import; retire DOM-handler priority sorting; and make the
   composed asset set authoritative for bytes and hashes. Retain cross-adapter
   system tests in `trusted-server-integration-tests` and update their paths and
   load order.
4. Create `trusted-server-integrations` with the explicit static catalog. Move
   ordinary integrations first, then move DataDome and GPT diagnostics after
   their lifecycle seams, APS and Prebid after the profile seam, and
   `adserver_mock` after the mediator seam. Add the new `openrtb` adapter.
5. Move `TrustedServerAppConfig`, all integration-specific configuration,
   validation, inactive-secret preprocessing, and secret metadata into the
   integrations crate. Rewire the CLI to the pure source-validation entry point
   while keeping host-only EdgeZero command wrappers in the CLI, and rewire all
   adapters to the single runtime composition entry point while retaining the
   existing operator schema through the temporary normalizer. This is the
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

Steps 6 through 8 are one deployable milestone-two cutover. They may be
implemented as separately reviewed commits, but schema 2 must not merge or
deploy while lexical recovery ordering, CLI consumers, or operator guidance
still implement the old contract.

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
- `trusted-server-integrations` has no `edgezero-cli` dependency and compiles in
  every native and WASM adapter graph; host-only path/snapshot/delegation code
  remains in `trusted-server-cli`.
- Browser core imports no integration source.
- Adapters and CLI import no concrete integration module.
- A dedicated native test/clippy gate executes the integrations catalog and
  host-only completeness tests; relying on adapter dependency builds is not
  sufficient to run them.

### Configuration and ordering tests

- TOML parent-table order becomes the integration-owned source-model order.
- The `toml_edit` pre-pass and typed `toml`/EdgeZero parser use the same TOML
  language generation and agree on parity fixtures outside `[integrations]`.
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
- Registry metadata and CLI provider diagnostics report configured ordinals;
  lookup-map or alphabetic iteration cannot masquerade as execution order.
- CLI validation never constructs runtime capabilities from unresolved secret
  key names; the post-resolution runtime phase rejects unresolved values.
- Read-only CLI consumers see the effective overlay through
  `ValidatedSourceConfig`; mutators and generators operate on file-only bytes,
  retain invalid-baseline recovery where currently supported, apply the same
  complete validation to baseline and candidate, and never persist overlay
  values. Invalid candidate plus valid baseline refuses; two invalid values
  retain the current non-disclosing warning and write escape hatch.
- Validate/diff/push delegate exact private snapshots of both the config and
  manifest bytes used by the pre-pass; concurrent edits cannot substitute
  different pushed bytes or change the adapter/store target. Snapshot paths are
  absent from user-facing success and error output.
- Prebid bundle selection and managed-module requirements come from the
  integration-owned partial source view, not a CLI-local schema or hard-coded
  registry path. Bundle modules and generated hash/SRI metadata can be staged
  without an external URL, remain runtime-inert, and are patched atomically
  over an otherwise-invalid unrelated baseline.
- Disabled integrations may retain structurally valid provider settings, contribute no
  providers or capabilities, and do not reorder enabled neighbors.
- Disabled placeholder values that current examples rely on, including the
  Google Tag Manager placeholder container, deserialize safely and defer their
  active-only format validation until enabled.
- Retained secret references in disabled source still pass EdgeZero name,
  store-reference, collision, and adapter validation; omitted active-only
  references are accepted, and inactive paths are not value-resolved at
  runtime.
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
- Activation-matrix tests cover APS and Prebid with zero and multiple
  providers, server-only Prebid without browser activation, browser-only Prebid,
  standard OpenRTB, and the `adserver_mock` mediator.
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
- GPT diagnostics preparation is idempotent across adapter preparation and the
  core publisher safety net, and auction correlation uses neutral registry
  request state rather than a concrete GPT import.
- APS and Prebid request construction, transport, parsing, response admission,
  and auction results remain equivalent to current `origin/main` behavior
  except for the documented provider-priority change.
- Prepared response parsers consume profile-owned request state without `Any`,
  downcasts, vendor enums, or cross-provider state reuse.
- Bidder routing, backend naming, notification suppression, telemetry identity,
  and mediator behavior remain equivalent apart from documented ordering.
- Active DataDome secrets are presence-checked and resolved through unchanged
  object paths; inactive protection and bypass secrets are removed before the
  shared resolver.
- Core-owned preprocessing continues to remove inactive Tinybird token and
  disabled EC-partner pull-token references before the shared resolver.
- Neutral `ts-eids` ingestion retains managed User ID aliases and collision
  checks, opaque LiveRamp envelopes, consent gating, OpenRTB EID production, EC
  partner ingestion, and admin diagnostics without adding a LiveRamp catalog
  definition or an identity-provider capability system.
- Request-processing requirements preserve DataDome origin bypass, full-body
  buffering, and final private caching, and prevent request-private GPT
  diagnostics state from entering ESI templates.
- Warm-hit and cold-store matrices combine DataDome/GPT requirements with key,
  bypass, malformed, unlisted, absent, and empty cookies; integration privacy
  can only restrict the existing cookie/cache decision.
- The dedicated mediator capability preserves request construction, ordered
  response input, bounded transport, parsing, and local-ranking fallback without
  exposing the legacy `AuctionProvider` trait.

### Browser tests

- Output order is core, creative prelude, and configured integrations.
- Immediate and deferred lists preserve configuration-relative order.
- Artifact/import-graph checks reject undeclared cross-root value imports and
  duplicate state-owner signatures. Permutive registration through its IIFE is
  visible to core context collection; Prebid auction helpers and Testlight
  queue behavior still use the single installed runtime.
- Colliding DOM-insertion handlers run in configured order, and reversing two
  integration tables reverses their winner. Numeric priority and lexical
  handler ID cannot affect the result; a deferred DOM handler fails composition.
- APS is absent from browser core and registers its renderer from its own IIFE.
- Core, GPT, and Prebid artifacts contain no private copy of APS renderer state.
- Existing APS validation, sandbox, messaging, timeout, and rendering tests pass
  through neutral dispatch.
- APS Rust serves the exported integration-owned renderer-document bytes with
  unchanged CSP and response headers; no production APS browser program remains
  embedded as a Rust literal.
- APS remains immediate and reads its rendering mode from the synchronous
  unified tag; a deferred APS renderer is rejected during composition.
- GPT remains immediate and reads `data-ts-gam-attribution` from the unified
  tag through `document.currentScript` at evaluation time.
- Equal duplicate trusted script attributes collapse, while conflicting values
  fail composition before HTML is served.
- Missing or rejecting renderers drop only the renderer-bearing bid with no
  fallback or pre-acceptance side effect; duplicate registration poisons the
  type or fails composition.
- Both server-bid and Prebid-`adId` renderer paths cover carrier scrubbing,
  failed admission/registration, bounded TTL and capacity, authenticated source
  and slot binding, atomic consume, replay rejection, renderer-owned Universal
  Creative response metadata, and `markWinningBidAsUsed` preservation.
- Unified, deferred, standalone, and inline assets expose bytes and hashes that
  match the emitted document fingerprint and static responses.
- Canonical composition-digest golden vectors are stable across construction
  order for unordered maps/sets, change when configured integration/provider
  order changes, and distinguish active resolved secrets from retained inactive
  references without exposing either input.
- Changing any integration setting that affects generated head output changes
  the document fingerprint; request-dependent head variation bypasses shared
  template reuse through processing requirements.
- GPT bootstrap, APS renderer document, Sourcepoint trap, GPT-diagnostics
  bootstrap, and DataDome/Didomi/GPT/Prebid/Sourcepoint inline templates resolve
  from their integration-owned package locations. A guard rejects handwritten
  production integration browser algorithms in Rust string literals.
- External Prebid artifacts preserve bidder, User ID, and analytics category
  selection, manifest/hash/SRI generation, managed-name alias and collision
  checks, `identityLinkIdSystem` requirements, consent behavior, and runtime
  codes after registry and shim paths move.
- Owner-specific output directories and manifests reject stale or partial
  output, and concurrent neutral/integration Cargo builds exercise the one
  cross-process toolchain lock.
- Clean and incremental Cargo builds prove that changing a sibling integration
  source reruns the integration embed build and changes its manifest/hash while
  leaving an unrelated neutral artifact unchanged.
- Cross-adapter Playwright tests remain in
  `trusted-server-integration-tests` and verify core, creative, APS, GPT, and
  Prebid load order using the moved assets.

### Repository gates

Before handoff, run every gate required by `AGENTS.md`, including Rust format,
all target-matched clippy aliases, Fastly/Axum/Cloudflare/Spin tests, CLI and
cross-adapter parity tests, required native and WASM builds, JavaScript builds
and Vitest suites for both browser source roots, JavaScript formatting, and
documentation formatting. The explicit package lists in every Fastly
build/check/clippy/test alias include both new Rust crates where applicable;
the CLI and codegen host lint gates remain intact; and a dedicated host gate runs
catalog completeness and integration test support. The migrated Fastly-SDK
guard continues to scan the moved integration sources.

The path migration covers repository automation as well as compiled code:
GitHub workflows and PR templates, Dependabot, `AGENTS.md`, `.claude` agents and
commands, the CLI Prebid builder, browser and template-cache smoke scripts,
TypeScript/Vitest/format/lint configuration, crate READMEs, reader-facing docs,
`trusted-server.example.toml`, and documentation-snippet tests. In particular,
the GPT bootstrap fixture path, APS Rust fixture includes, browser integration
build script, and local template-cache harness must resolve the new owners.
VitePress lint/build and `documentation_snippets` remain gates. Historical
archived specs are not rewritten as though they described the new layout.

A repository path guard rejects active code or tooling that still points to
`trusted-server-js/lib/src/integrations` or concrete
`trusted-server-core/src/integrations/<vendor>` paths, except for an explicitly
allowlisted historical reference. Dependabot remains rooted at the one lockfile.

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
envelope, fence schema-1 CLI writers during and after cutover, assert the stored
schema after the push, require restoration before binary rollback, and remove
the legacy reader only in a later release.

### Configuration order silently changes auction priority

Provider order affects launch budget, mediator input, response order, and equal
price ties. Treating it as cosmetic would make operator edits surprising.

Mitigation: define configuration order as operational priority, document the
change from the baseline's lexical order, and test each observable consequence.

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
composed settings, plan, registry, browser assets, and configuration digest
rather than reconstructing any of them. Non-runtime CLI commands consume the
validated source view rather than deserializing integration fragments locally.

### Stale or incorrectly ordered browser artifacts

Splitting Rust ownership while sharing one Node workspace can embed previous
output, race build scripts, or load APS too late.

Mitigation: hold one cross-process lock across install, cleanup, build,
discovery, manifest verification, and copy; use owner-specific output
directories; retain stale-output refusal; hash built bytes; load core and the
creative prelude first; reject deferred APS composition; and run artifact-level
renderer, fingerprint, and ordering tests.

## Acceptance Criteria

The change is complete when:

1. Both new crates are workspace members and statically linked by the CLI and
   every adapter where required.
2. All fifteen current concrete Rust implementation units live under
   `trusted-server-integrations/src/<id>/`.
3. The standard provider configuration is supplied by the built-in Rust-only
   `openrtb` integration, making sixteen static definitions in total.
4. All integration-owned browser sources, assets, unit/artifact fixtures, and
   unit/artifact tests, including production executable inline templates, live
   under `trusted-server-integrations-js`; cross-adapter system tests remain in
   the integration-test crate.
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
10. The old `[auction.providers]`, `profile`, `profile_config`, and fixed
    `protocol` fields are rejected with targeted migration guidance.
11. The compiled auction plan retains current-baseline behavior after
    normalization, except for the explicit change from lexical to
    configuration-order provider priority.
12. Browser core imports no concrete integration, and APS rendering works
    through one immediate registration without private copies in core, GPT, or
    Prebid bundles; the complete shared-state facade and DOM dispatcher obey
    configuration order; and GPT retains its synchronous-tag bootstrap
    contract.
13. `TrustedServerAppConfig`, `ValidatedSourceConfig`, integration secret
    handling, and final runtime composition are owned by
    `trusted-server-integrations`; core has no concrete config or loader
    dependency, the CLI has no duplicate integration schema, and host-only
    EdgeZero wrappers remain in the CLI rather than the WASM-linked crate.
14. The CLI source phase and adapter runtime phase use the same catalog-backed
    schemas and pure validation definitions, and all adapters receive one
    post-secret-resolution
    settings/plan/registry/browser-assets/configuration-digest composition.
15. OpenRTB request-local state crosses the transport boundary through a
    prepared response parser without `Any` or vendor enum variants in core.
16. Explicit `enabled`, parent-before-descendant, disabled-retention, activation
    matrix, local-ID, and qualified-ID rules have end-to-end tests, including a
    server-only Prebid migration that does not activate browser behavior.
17. Schema-1 blobs remain readable for the documented rollout release, schema-2
    blobs preserve existing secret paths, and binary rollback requires verified
    restoration of the archived schema-1 envelope through a drill-tested
    adapter-specific mechanism; old schema-1 CLI writers are fenced after
    cutover and stored-schema regression is monitored as a rollback event.
18. Browser assets carry bytes and hashes through composition; publisher
    template fingerprints combine the versioned canonical composition digest
    with the exact document-assets fingerprint; all existing URL, host, scheme,
    origin, assembly, Vary, cookie, and schema-version key dimensions remain;
    and request-dependent variants bypass shared reuse.
19. Core test support, the Fastly-SDK migration guard, Cargo aliases, CI,
    Dependabot, repository automation, browser and cache smoke scripts,
    documentation and snippet tests, and the CLI Prebid builder cover the new
    crate boundaries, with a stale-path guard and a dedicated native
    integrations-crate gate.
20. Managed Prebid User IDs and external bundle bidder/User-ID/analytics
    selection retain their current alias, collision, consent, manifest, hash,
    SRI, and runtime-code behavior.
21. Neutral OpenRTB-EID/EC ingestion, including opaque LiveRamp envelopes,
    partner ingestion, and admin diagnostics, remains in core without creating
    another integration definition or provider framework.
22. Both milestone exit criteria and the full repository verification gates
    pass.

## Deferred Work

The following require separate designs and real consumers:

- External vendor-owned crates or adapter-supplied registrations.
- Runtime integration loading or a stable integration SDK.
- Independent integration release and compatibility policies.
- New identity, EC, geo, device, and permission-signal provider systems.
- Permission and jurisdiction policy changes.
- Non-OpenRTB auction provider factories.
- Upstream EdgeZero composition and host-service changes.
- Moving CLI audit detection metadata into integration directories.
