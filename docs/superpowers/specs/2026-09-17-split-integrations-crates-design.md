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

Cross-integration runtime order within the same comparable capability phase
will come exclusively from TOML declaration order. Fixed engine seams such as
immediate versus deferred loading and the diagnostics post-unified position are
named phases, not hidden integration priorities. The config push and
config-store representation will preserve order explicitly; filesystem
discovery order will never affect execution. For auction providers, that order
is also operational priority: it controls launch and response order, mediator
input order, and local equal-price tie-breaking.

The compatibility baseline is the behavior shipped on `origin/main` at
`a4e01eb55`, not either prior pull request discussed below. This design changes
only the configuration, activation, and ordering behavior called out explicitly
in this document; all other current runtime, browser, CLI, and cache behavior is
preserved.

This remains one design, but it has two merge milestones. The crate and runtime
boundary moves first without changing operator configuration. The ordered,
integration-owned configuration cuts over only after the compatibility-focused
boundary, including its explicitly listed baseline bug fixes, is running. The
milestones share one target architecture without
forcing the packaging move and configuration migration into one deployment.

## Context

On `origin/main` at `a4e01eb55`, neutral registry machinery and concrete
integrations share `crates/trusted-server-core/src/integrations`. The concrete
Rust units are:

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

The same baseline includes the parser-aware streaming Next.js processor from PR
#1135, managed Prebid User IDs and their OpenRTB EID/EC
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

## Related Pull Requests and Disposition (Non-Normative)

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

PRs #1043 through #1047 and #1094 implement parts of that alternate convention.
They cannot merge concurrently with this configuration contract. Their tests or
neutral wire-format work may be reused after independent verification, but their
inventory, discriminator, provider-naming, and crate-layout decisions are
superseded for in-tree integrations by this specification. That disposition is
coordination, not evidence for the architecture chosen here.

PR #1135 is part of the current baseline. Its parser-aware streaming Next.js
processor, test support, and cross-adapter parity case move with the integration;
the removed HTML post-processor is not recreated by this work.

Before either milestone branches for implementation, its baseline commit and
locked EdgeZero revision are recorded again and every baseline-dependent
inventory in this document is rechecked. Open or previously approved pull
requests never override a decision in this specification merely because of
their review status.

## Rejected Alternatives and Rationale

The following alternatives were considered and are deliberately not part of
the target design. Each rejection is narrow: where review exposed a valid
failure mode, the concern is accepted even when the proposed remedy is not.
The objection and the replacement decision are stated separately so an
implementation cannot quietly reintroduce the rejected mechanism.

- **One crate per integration or an external plugin ABI.** Concrete ownership
  boundaries and independent testability are required; that concern is
  accepted. **Objection:** per-vendor crates, dynamic discovery, or a public SDK
  would multiply dependency, compatibility, release, and governance surfaces
  without a current external consumer. **Decision:** use one statically linked
  Rust integrations crate, one integration-browser crate, per-integration
  directories, and one compile-checked catalog.
- **A separate top-level auction-provider inventory, including APS.** APS does
  provide auction behavior, but it also owns browser, renderer, route, and page
  behavior. **Objection:** classifying APS as only a provider would split one
  implementation's activation and ownership across unrelated top-level
  sections and would make `[integrations]` incomplete. **Decision:** APS is an
  integration that registers several capabilities. Its provider instances live
  below `[integrations.aps.auction.providers.<id>]`, as do provider instances
  owned by other integrations.
- **A config `type`, `kind`, or `implementation` discriminator.** Multiple
  configured instances must be able to reuse one implementation; that concern
  is accepted. **Objection:** an extra discriminator would duplicate the static
  integration ID already present in the TOML path, admit contradictory ID/type
  combinations, and expose internal registration types to operators.
  **Decision:** `[integrations.<id>]` selects the statically cataloged
  integration, while nested instance names identify reusable provider
  configurations. Capability types remain Rust contracts, not configuration.
- **An operator-written `[auction] provider_order` list.** Provider priority
  must represent schema-1's globally interleaved lexical order; that concern is
  accepted. **Objection:** a second operator list would duplicate every provider
  identity, permit the inventory and priority to drift, and separate priority
  from the configuration block an operator is reviewing. It would make the
  promised single integrations configuration untrue. **Decision:** schema 2
  derives priority from declaration order in the one `[integrations]`
  inventory. The neutral compatibility model carries a flat legacy sequence so
  schema 1 remains exact; that internal sequence is not a second operator
  syntax.
- **A hidden engine override that always runs `js_asset_proxy` first.** Attribute
  overlap and terminal-removal behavior must be explicit and tested; that
  concern is accepted. **Objection:** a hidden first phase would make the TOML
  order contract false and still would not reproduce baseline Prebid removal
  behavior, because Prebid precedes `js_asset_proxy` today. **Decision:** legacy
  compatibility paths reproduce the complete baseline sequence. Schema 2
  exposes chained replacement and terminal removal in declaration order and
  diagnoses known overlaps rather than silently overriding the operator's
  order.
- **Treating PR #1016, PR #1084, or earlier review statements as design
  authority.** Their code and tests can reveal compatibility constraints, and
  conflicting in-flight work needs an explicit disposition. **Objection:**
  review or merge status does not make a prior proposal correct for this design,
  and importing its architecture would silently expand this spec's scope.
  **Decision:** current behavior is evidence and prior proposals are context.
  This specification records its own ownership, ordering, activation, and
  rollout decisions and records the disposition of incompatible work above.
- **Requiring the Node package and lockfile to move to a common ancestor.** One
  Node project must reliably resolve, type-check, lint, format, test, and build
  both source roots; that concern is accepted. **Objection:** moving the package
  root is not required to meet that contract and would add unrelated
  repository-wide path and automation churn. **Decision:** keep one canonical
  project with explicit, tested resolver and tool-root configuration for the
  sibling sources. Moving the root remains a fallback only if that contract
  cannot be made reliable.
- **A new public configuration-status endpoint.** Runtime settings must be
  loaded and the expected schema must be observable during rollout; that
  concern is accepted. **Objection:** a new endpoint would add an authentication
  and public-API surface unrelated to the crate split.
  **Decision:** use adapter-native version/binding inspection, startup
  schema-and-digest logging, and an existing authenticated or
  settings-dependent probe.
- **Filesystem snapshots around EdgeZero config commands.** Config commands must
  validate and serialize the same app-config bytes; that concern is accepted.
  **Objection:** same-directory manifest copies add write
  requirements, can survive process termination, expose operator configuration,
  and require log/path rewriting without freezing every adapter manifest and
  store target. **Decision:** a narrow two-stage EdgeZero typed-config extension
  gives the app the exact source bytes and the effective typed command context,
  providing the required consistency without filesystem snapshots.
- **Hashing resolved secret values into template identity.** Current integration
  configuration changes that alter document bytes must invalidate templates;
  that concern is accepted. **Objection:** current integration secrets authorize
  upstream calls and do not form HTML variants, so hashing their values adds
  rotation churn and sensitive derived material without improving correctness.
  **Decision:** the verified stored-data hash covers secret references. A future
  secret that shapes output must declare a non-secret behavior fingerprint or
  force private output.
- **A second generic per-capability enablement system.** A simple master kill
  switch and precise optional-feature activation are both required; that
  concern is accepted. **Objection:** operator-visible capability kinds or a
  parallel browser/provider activation inventory would recreate the
  discriminator-driven configuration this design is removing and introduce two
  answers to whether an integration is active. **Decision:** `enabled` remains
  the integration master gate, existing typed fields decide which optional
  capabilities are configured, and routes to known-disabled integrations are
  pruned so `enabled = false` remains a kill switch.
- **Silently accepting unknown integrations or providers in schema 2.** Exact
  baseline acceptance must remain exact while schema 1 can still be loaded;
  that concern is accepted. **Objection:** extending that permissiveness to new
  source would turn typos into silently inactive configuration and prevent the
  static catalog from validating ownership. **Decision:** exact legacy
  acceptance belongs only to the schema-1 compatibility reader. New source and
  stored schema 2 fail on unknown IDs; only references to an explicitly
  disabled, known integration receive the kill-switch treatment defined below.

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
- Upstream EdgeZero lifecycle, host-evidence, store, or adapter changes. One
  narrow typed-config validation extension is allowed: a source-bytes check and
  a post-parse command-validation callback over the same loaded value. It does
  not change target selection, deployment, storage, or adapter behavior.
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
      integrations/
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
          rsc.rs
          rsc_placeholders.rs
          rsc_stream.rs
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

Every current flat Rust integration file becomes
`src/integrations/<id>/mod.rs`. Restricting the completeness scan to that
directory prevents helper modules from being mistaken for integrations.
Existing nested modules and fixtures stay with their owner. The current
Next.js `rsc_stream` implementation moves; the removed `html_post_process`
module does not return. `openrtb` is a built-in Rust-only, directory-backed
integration adapter that exposes configuration for the current standard
OpenRTB profile without turning the neutral OpenRTB execution engine into
concrete code. The target Rust catalog therefore has sixteen definitions:
fifteen moved implementations plus the new `openrtb` adapter.

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
             │                         │
             │                         └──→ trusted-server-openrtb
             └───────────────→ trusted-server-integrations-js

adapters and CLI ────────────→ trusted-server-integrations
adapters and CLI ────────────→ trusted-server-core
trusted-server-integration-tests ──→ trusted-server-integrations
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
- Pure source parsing, structural validation, and deploy-validation APIs used by
  the CLI at the validation level appropriate to each command.
- The public runtime entry points that load a config-store blob and return one
  composed runtime value.
- A structural `SourceConfigView` and a deploy-validated
  `ValidatedSourceConfig` used by non-runtime CLI commands.
- A generic `PartialSourceConfigView<T>` for recovery-oriented commands that
  intentionally type only one owned subtree of an otherwise invalid document.

`TrustedServerAppConfig` contains neutral core configuration plus the ordered
integration-owned source configuration. Concrete integration configuration is
not added to core's `Settings`. Composition consumes the integration portion
into capabilities and returns a neutral runtime `Settings` value containing
only state that core execution engines understand.

The public API has four explicit levels:

1. `SourceConfigView` performs the TOML pre-pass, typed parse, catalog
   resolution, and structural validation. It does not run deploy validation.
   Read-only diagnostics that already require a complete typed root use this
   level so unrelated deployment checks do not become new failures.
2. `PartialSourceConfigView<T>` retains the source document plus one typed,
   command-owned subtree. Recovery-oriented mutators and generators use it when
   their current contract tolerates an invalid unrelated root. It cannot be
   converted into a storage DTO or runtime composition, and each command names
   the only source paths it may read or write.
3. `ValidatedSourceConfig` applies the selected environment overlay, aggregates
   secret metadata, runs every secret-independent integration and
   cross-integration check, serializes the storage DTO, and validates its order
   sidecars. It invokes the same pure auction-plan compiler used at runtime,
   including provider routing, browser/server bidder ownership, mediator
   capability and enablement, and duplicate routes, then discards the
   validation-only plan. A separate pure `validate_for_targets` operation runs
   the resulting plan against the command's resolved target set. Neither path
   creates executable capabilities or attempts to use unresolved secret values.
4. Runtime composition begins after envelope verification, inactive-secret
   preprocessing, and secret resolution. It repeats the shared pure validation
   kernel against resolved values, compiles the authoritative plan, constructs
   plan-dependent capabilities, and returns the final composition.

This distinction makes "runtime-only construction" precise: it does not move
any currently push-time, secret-independent plan failure to startup. Diff and
push have one selected adapter and fail when its target validation fails.
`config validate` has no adapter selector: it runs target validation for every
supported adapter declared by the manifest and prints a named result per target.
Ordinary validate fails on target-neutral errors and reports target-specific
failures as warnings so a valid Fastly configuration is not rejected merely
because the same multi-provider plan cannot run on Cloudflare or Spin;
`config validate --strict` fails if any declared target fails. A fixture rejected
by runtime composition for a secret-independent reason must produce the same
target-neutral error or the same named target result in the CLI.

The complete source and validated views retain the typed operator configuration
and expose only the data their CLI consumers need: neutral global settings,
ordered integration and qualified-provider metadata, and integration-owned read
models. A partial view retains only its owned typed subtree and the source
document needed for a bounded edit. None is a runtime registry or contains
resolved secret values or executable capability objects.

The runtime value, conceptually `TrustedServerComposition`, contains the
validated neutral `Settings`, one `Arc<AuctionPlan>`, the plan-backed auction
orchestrator including the selected mediator, one `IntegrationRegistry`, the
composed `BrowserDocumentAssets`, the validated deployment target, and a lazy
canonical composition digest. Adapters consume this value; they do not
separately compile the auction plan, construct the orchestrator, rebuild the
integration registry, enumerate browser bundles, or reconstruct the digest.

The composition is immutable and may live for a request, a Fastly sandbox, or a
bounded adapter cache without changing semantics. Stored capabilities are
`Send + Sync` and request-stateless. Script text buffers, Next.js stream state,
document observations, and other mutable transformation state are created by
per-document factories and live in request/processor state, never in a reused
registry object. Composition work is O(configuration); exact asset hashes are
build-time inputs and template identity is lazy/memoized as described below.

Composition also exposes a narrow settings-only result before capability
construction. Fastly retains its current JA4 gate and failed-startup
finalization paths through that view; a registry or orchestrator failure must
not erase settings those degraded paths already use.

Core retains neutral config-store access, Fastly chunk reconstruction, blob
envelope verification, preprocessing for inactive neutral/global secret
references, secret-resolution primitives, global settings types, and
auction-plan compilation. Today the neutral preprocessor removes inactive
Tinybird token references and disabled EC-partner pull-token references; that
behavior remains in core. These helpers accept or return neutral data and never
call the concrete catalog. Adapter-specific readers produce one verified
envelope/data value; Fastly chunk reconstruction remains a core loader helper,
while Cloudflare and Spin adapt their existing binding/KV inputs to the same
verified-data boundary. The integration crate composes it in this order:

```text
config-store bytes
  → core chunk reconstruction and envelope verification
  → settings-only neutral view
  → core-owned neutral/global inactive-secret preprocessing
  → catalog-owned integration inactive-secret preprocessing
  → aggregated core + integration secret resolution
  → catalog-aware config validation
  → core AuctionPlan compilation
  → plan-dependent capability construction
  → core IntegrationRegistry and orchestrator construction
  → browser asset composition and document fingerprint
  → TrustedServerComposition
```

The CLI imports `TrustedServerAppConfig`, the complete, partial, and validated
source views, and pure validation APIs from `trusted-server-integrations`. The
host-only CLI continues to own command dispatch and the direct `edgezero-cli`
dependency;
`trusted-server-integrations`, which is linked into every WASM adapter, never
depends on `edgezero-cli`.

The current locked EdgeZero revision does not expose enough context for this
contract. EdgeZero therefore gains one narrow typed-config extension with two
default no-op stages. The source stage receives the selected app-config path and
exact raw bytes before deserialization. The effective-config stage receives a
borrow of the overlay-applied typed value plus the command kind and target
context EdgeZero already resolved: one adapter for diff/push and the manifest's
declared supported adapter set for validate. It does not select or mutate a
target. EdgeZero reads the file once, runs the source-aware pre-pass, constructs
one typed value, invokes command validation on that value, and serializes that
same value. Validate, diff, and push therefore cannot validate one app-config
read or typed value and serialize another.

All workspace `edgezero-*` dependencies are then repinned together from v0.0.8
to one immutable, reviewed tag or commit containing this extension. The
extension does not alter manifest parsing, target selection,
environment-overlay mechanics, logging, storage, or adapter behavior. It
replaces the filesystem snapshot wrapper entirely: config commands do not write
temporary operator or manifest copies, require a writable checkout, rewrite
logged paths, or rely on destructor cleanup around `process::exit` and signals.
If the extension cannot land and the workspace cannot repin to its reviewed
revision, milestone 2 is blocked rather than restoring the snapshot design.

Read-only `config ad-templates` and `audit ad-templates` commands load the
effective `SourceConfigView` with the existing optional environment overlay when
they currently require the complete typed root. Mutating or generator commands
load file bytes without the overlay, so environment-only values are never
persisted. Recovery-oriented ad-template generation uses an explicitly bounded
`PartialSourceConfigView<AdTemplateConfig>` against an otherwise invalid
baseline, preserving its current warning and non-disclosure behavior.
"Structural-only" means valid TOML, explicit parent/descendant structure, ID
grammar, and typed parsing of the subtree the command reads or writes; it
excludes unrelated required values, plan compilation, target validation, active
secret values, and publisher-domain deploy checks. Candidate and baseline use
the same selected validation level: a valid candidate is written; an invalid
candidate is refused when the baseline was valid; and an invalid candidate over
an already-invalid baseline retains the current warning and atomic-write escape
hatch without disclosing source values. Final validate, diff, and push always
use the complete `ValidatedSourceConfig` path.

`ts prebid bundle` obtains typed bidder, User ID, analytics, and managed-module
requirements through an integration-owned
`PartialSourceConfigView<PrebidConfig>` rather than a duplicate CLI schema. It
intentionally does not require unrelated app
configuration or an `external_bundle_url` to be deploy-valid: `bundle.modules`,
`external_bundle_sha256`, and `external_bundle_sri` are inert staging metadata,
while `external_bundle_url` activates the browser capability. The command
retains its current ability to build first, atomically patch hash/SRI metadata,
and tell the operator to upload and set the URL. It validates the structural
pre-pass and affected Prebid subtree before writing; full app validation remains
the contract of config validate/push. Because parent position is runtime order,
the command no longer invents or appends a missing `[integrations.prebid]`
parent. It requires an existing parent with explicit `enabled`, or exits with a
placement example; it may create only owned descendants beneath that parent.
Provider diagnostics display qualified providers in configuration order rather
than alphabetizing a detached map.

`ts audit generate` may retain detector and edit metadata for concrete
integrations under this design's audit-reorganization non-goal. That metadata is
not a second runtime schema: audit candidates and final writes are parsed and
validated through `SourceConfigView`/`ValidatedSourceConfig`, and the audit code
does not define activation, defaults, secret metadata, or capability
construction independently.

## Static Rust Catalog and Browser Discovery

### Rust

`trusted-server-integrations/src/lib.rs` declares a small, explicit static
catalog. Each entry names an `IntegrationId` and a crate-private `definition`
function from a same-named directory. Rust compilation checks every listed
module and definition signature. A host-target completeness test enumerates
immediate `src/integrations/<id>/mod.rs` directories and fails if a valid
directory is missing from the catalog or a catalog ID has no directory. IDs
must parse as the `IntegrationId` defined by this design and must be unique.

There is no directory-local numeric order. Runtime order belongs to
configuration.

This intentionally avoids a Rust build script and generated module
declarations for a sixteen-entry table. The catalog-completeness test provides
the missing-registration guard without making filesystem discovery part of
compilation.

### JavaScript

`trusted-server-js` and `trusted-server-integrations-js` use one browser Node
toolchain project, one browser lockfile, and one coordinated set of Vitest,
ESLint, Prettier, Vite, and Prebid resolution rules. The canonical project root remains
`crates/trusted-server-js/lib`; `trusted-server-integrations-js` does not add a
second `package.json` or lockfile. Neutral browser sources remain under
`trusted-server-js`; integration sources, owned unit/artifact fixtures, and
owned unit/artifact tests live under `trusted-server-integrations-js`. The
shared configuration does more than include that sibling source root:

- Vite and Vitest resolve bare packages through the canonical project's
  exports-aware resolver and deduplicate stateful browser-core entry points.
- TypeScript includes both source roots and supplies exact package paths where
  Node's ancestor walk cannot reach the canonical `node_modules`.
- Vitest names the sibling test directory, includes its runtime and type tests,
  and has a deliberately failing typecheck canary so `ignoreSourceErrors` or an
  empty glob cannot make the gate pass silently.
- ESLint uses an explicit common base path and source globs. Prettier always
  receives the canonical `--config` path for sibling files.
- Generated external-Prebid entries live under the canonical project or use an
  explicit resolver that preserves the `prebid.js` package `exports` map; a
  directory alias that bypasses package exports is forbidden.

Separate build targets emit neutral and integration artifacts directly into
private owner-specific directories below their Cargo `$OUT_DIR` and validate
per-target manifests before embedding them. They never clean, discover, or copy
from one shared `dist` directory. A cross-process lock covers dependency
installation or mutation only; completed dependency trees may be read by
parallel owner builds. This removes stale/partial discovery races without
serializing every adapter build.

The integration build discovers immediate directories containing `index.ts`
and emits one IIFE per entry point. An IIFE may call the external versioned
browser runtime facade and therefore is not described as self-contained. Its Cargo build embeds each
output and its SHA-256 hash. CI, browser integration scripts, and the CLI
Prebid builder use the single workspace root rather than maintaining a second
dependency graph; Dependabot continues to watch the one browser lockfile in
addition to the repository's unrelated Node projects.

Canonical npm build, typecheck, lint, format, and test commands include both
source roots explicitly, and CI invokes those commands rather than core-only
paths. A resolution test imports `vitest`, `prebid.js`, and one exported Prebid
module from a sibling integration file. Both Rust build scripts emit complete
`rerun-if-changed` coverage for their own manifest, configuration, and source
inputs, including the sibling integration root. Clean, incremental, and
concurrent build tests change one integration source and prove the integration
manifest is regenerated without spuriously changing the neutral manifest or
observing another build's partial output.

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
  HTML-stream-processor, and head-injector traits and contexts.
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
3. Preparing a provider returns `Exchange(PreparedOpenRtbExchange)` or
   `Skip { metadata }`. `Skip` preserves the current no-impressions/no-transport
   outcome and any profile-specific diagnostic metadata. An exchange contains
   the finalized outbound request plus a boxed, object-safe response parser
   bound to that exact provider and request. The parser owns any request-local
   APS, Prebid, or standard parsing state and consumes itself when parsing the
   response.

The prepared exchange lets the core engine retain shared backend registration,
transport, deadlines, notification policy, normalized response handling, and
telemetry. The profile owns request specialization, profile-specific headers,
debug capture, response parsing, diagnostics, and renderer descriptors.

The neutral preparation input explicitly carries the current request facts:
DNT, raw Cookie, User-Agent, Referer, Accept-Language, attested client IP,
zone, consent-filtered identity/EIDs, routed demand, logical budget, transport
budget, and optional signer. Core constructs that snapshot, stamps the
`QualifiedProviderId`, and owns the generic request/response envelope. A
profile owns only its current branch points and bound parser state; moving code
does not duplicate the shared OpenRTB engine inside each integration.

Renderer-bearing bids use a neutral, versioned descriptor with
`renderer_type`, opaque payload, and the handler-supplied targeting bid ID. Its
serde representation is byte-for-byte compatible with the existing tagged
`BidRenderer::Aps` JSON. Core may carry and serialize the descriptor, but only
the registered renderer handler interprets its payload or supplies APS/GPT
browser metadata.

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
existing outcome policy. Dispatch or parse errors warn and fall back to local
ranking. A successfully parsed non-2xx `adserver_mock` error response is a
mediator response with zero winners and does not fall back; that current
distinction is covered by a contract test. The current mediator endpoint policy,
including an allowed HTTP endpoint where supported today, is preserved. Its
response correlation remains the current mediator integration identity rather
than being silently rewritten as an auction-provider identity.
`adserver_mock` owns only request construction and response interpretation. The
old mediator use of `Arc<dyn AuctionProvider>` is deleted when its last caller
migrates.

No unused generic `AuctionProviderFactory` extension is added. A provider that
cannot use the current OpenRTB engine will define that additional seam in a
future design.

## Static Catalog and Activation

The explicit static catalog establishes what the binary supports.
Configuration establishes what runs.

Source validation and runtime startup both resolve IDs through the same catalog.
Source validation runs the validation-only plan path described above. At
runtime, after secrets are resolved:

1. Parse `[integrations]` into an ordered sequence.
2. Resolve each ID against the static definition catalog.
3. Ask each owning definition to parse and validate its complete configuration
   and expose catalog-level profile/mediator definitions.
4. Collect enabled integration-owned provider instances into the independent
   flat provider sequence and ask core to compile the single canonical auction
   plan.
5. Construct plan-independent capabilities, then construct plan-dependent
   capabilities such as Prebid bidder ownership, head injection, and mediator
   selection with a read-only `Arc<AuctionPlan>`.
6. Resolve typed browser modules and construct the neutral integration registry
   and orchestrator in configured order.

An absent integration is inactive. Every explicit parent integration table must
contain `enabled = true` or `enabled = false`; there is no integration-specific
default. `enabled` is the integration's master gate, not an assertion that every
optional capability is configured. An explicitly disabled integration may
retain its settings and provider instances but contributes no runtime
capabilities or providers. Configuration cannot activate APS or Prebid through
an auction plan while omitting its parent. References to an absent or unknown
parent fail validation. References to an explicitly disabled, known integration
remain in source as a kill switch: they are omitted from the compiled route or
mediator plan with one aggregated warning and ordered diagnostics, and become
subject to full validation again when the parent is enabled. No unknown ID is
silently treated this way.

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
startup validation already uses. The closed browser-only source-field set is
`account_id`, `debug`, `script_patterns`, `client_side_bidders`,
`excluded_gam_ad_unit_path_suffixes`, and `managed_user_ids`. Any explicitly
configured or effectively non-default value in that set without
`external_bundle_url` fails validation rather than activating a partial browser
path; an implicit default script-pattern list alone does not activate or fail.
CLI-only `bundle.modules`, `external_bundle_sha256`, and
`external_bundle_sri` may be staged without a URL and never activate runtime
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

Internal qualified identity and externally serialized identity cut over at
different times. The schema-1 compatibility reader constructs a qualified
internal key but retains the legacy local provider ID for backend names,
mediator wire correlation, public `/auction` provider details, ts-debug, and
telemetry. Schema 2 uses the qualified value on those external surfaces. Thus
deploying the dual reader over an unchanged schema-1 blob does not split metrics
or mediator correlation; the documented rename occurs only when schema 2 is
pushed. The rollout updates exact-match dashboards, alerts, Tinybird filters,
mediator expectations, and the local template-cache harness before that push.

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
6. Schema-2 plan input flattens providers first by owning integration and then
   by local provider declaration. The neutral plan input nevertheless carries
   one independent flat provider sequence so legacy compatibility can represent
   globally interleaved provider IDs without regrouping them.
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
   each back-to-back launch, so every later launch observes a smaller logical
   budget and may also cross an adapter timeout bucket; order is budget-visible
   even when the deadline does not expire.
10. Shared browser dispatchers execute handlers by the owning integration's
    configured ordinal and then definition-local registration order. Wall-clock
    registration timing, numeric priority, and lexical handler ID are not
    alternate ordering mechanisms.

Ordered runtime introspection carries the configured ordinal with each
integration and provider. Lookup indexes may use maps, but iterating a
`BTreeMap`, `HashMap`, or alphabetized metadata view never defines or displays
execution priority. CLI diagnostics and registry metadata that show order use
the ordered plan/registration view.

The stale comment that `js_asset_proxy` is globally first is retired. The
effective baseline registration sequence is, when each entry is active:

```text
prebid, aps, js_asset_proxy, testlight, nextjs, permutive, lockr, didomi,
sourcepoint, osano, google_tag_manager, datadome, gpt, gpt_diagnostics
```

Milestone-one normalization and schema-one decoding use that complete sequence;
they do not move `js_asset_proxy` ahead of Prebid. Schema 2 instead chains
attribute rewriters in operator-visible integration order: a replacement becomes
the next rewriter's input and a removal is terminal. Operators are told that an
earlier native rewrite can prevent a later exact-original-URL proxy rule from
matching, and that moving `js_asset_proxy` ahead of Prebid can prevent the
publisher-Prebid remover from recognizing a rewritten URL. Overlap tests cover
GPT, Google Tag Manager, DataDome, Sourcepoint, Permutive, Lockr, Testlight, and
Prebid in both legacy-compatible and deliberately reordered configurations. No
engine-only override is hidden from TOML.

Script-text rewriters also receive the current text produced by the preceding
rewriter rather than having independent `lol_html` handlers repeatedly replace
the original chunk. DOM-insertion guards claim canonical parsed host/path
ownership, not substring coincidences in a query string. Built-in ownership
sets are disjoint and tested with exactly one claimant for every canonical URL;
configured order applies to genuinely composable handlers, not to select the
winner of an ownership bug.

Browser load mode remains a lifecycle phase, not a second operator priority:
immediate code necessarily evaluates before deferred code. Config order is
preserved within each phase and remains the stored ordinal used by any shared
dispatcher after a module registers. A hook that must arbitrate during initial
document mutation, including a DOM-insertion guard, is immediate-only;
composition rejects it on a deferred asset. Diagnostics display each module's
fixed load mode so this phase boundary is visible rather than inferred from
timing.

All recovery paths obey the same provider ordinals. Existing stable plan-index
sorts remain. The abandon/failure path that can emit providers from `HashMap`
iteration is changed to plan-order recovery before the new ordering contract is
enabled; no no-op lexical-sort change is claimed as a compatibility break.

Inline-table and dotted-key shorthand may not define an integration parent or
provider parent. Requiring ordinary table headers makes activation, ownership,
and order visible in one form and lets the pre-pass produce targeted errors.

The integrations crate directly enables the `preserve_order` feature on the
workspace's single resolved `toml` package; it does not rely on an unrelated
dependency to activate that feature through Cargo unification. EdgeZero's
`toml::Value` then sees the same ordered map implementation. A test asserts that
typed integration/provider order equals the independent `toml_edit` pre-pass
order. The integration-owned source model uses ordered map types with explicit
iteration semantics, never `HashMap` or `BTreeMap`.

Before typed deserialization, the host-only `source-config` module of
`trusted-server-integrations` parses the source with `toml_edit`. The dependency
and code are feature-gated out of every WASM adapter graph. This source-aware
pre-pass rejects descendant-before-parent declarations, missing explicit parent
tables, and missing `enabled` fields. It then permits the existing EdgeZero
scalar environment overlay; overlays may
replace values but may not create, remove, or reorder integration or provider
tables. Validate, diff, and push inspect app-prefixed overlay variable names
without reading their values into diagnostics and fail when a name targets a
retired auction-provider or integration path; silently ignoring an old endpoint
override is not permitted.

The direct `toml_edit` dependency is pinned to the same TOML 1.1 parser
generation as the workspace `toml` package and EdgeZero's typed parser. Parser
parity fixtures cover otherwise-unrelated valid and invalid TOML syntax so the
pre-pass cannot accept a document the typed path rejects, or reject one merely
because it used an older TOML grammar.

Every entry point that accepts TOML uses this pre-pass, including local loading,
CLI validate/diff/push, ad-template diagnostics and candidate validation, and
the Prebid bundle command. The integration-test `generate-viceroy-config`
binary also uses the same source parser and schema-2 storage serializer rather
than retaining a direct `toml::from_str`/legacy-envelope path. Recovery mutators
may request the structural-only mode described above. Ad-template generation
applies the comparative candidate/baseline rule, and the Prebid builder
validates only its owned partial view; neither recovery path is silently
tightened into unconditional full-app validation. EdgeZero's typed mechanics
remain responsible for overlay, validation invocation, diff, envelope
construction, consent, and store writes after the pre-pass succeeds.

The EdgeZero extension uses EdgeZero's existing path and target resolution: an
explicit `--app-config` wins; otherwise the path is
`<manifest-dir>/<app.name>.toml`; diff/push provide their resolved adapter; and
validate provides the supported adapter set declared by the manifest. Parity
tests cover exact-byte validation and serialization, command/target context,
explicit and default paths, manifest paths with and without parent directories,
concurrent app-config replacement, and `--no-env`. The environment overlay can
replace only scalar leaves already present in TOML; it cannot create an omitted
`enabled` field, integration, provider, table, or array. Operator templates must
contain every leaf intended for overlay.

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
sidecars above. `Validate` checks the sidecar/map bijection even for commands
such as `config validate` that do not serialize. Serialization defensively
checks it again, and runtime loading performs the same check before constructing
ordered settings. Missing, duplicate, extra, or mismatched names fail at all
three boundaries.

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

The schema-2 sidecars are sufficient to derive the neutral flat provider
sequence: iterate `integration_order`, then each present `provider_order`.
Schema 1 instead supplies the independent sequence directly in global lexical
local-ID order before local IDs are mapped to qualified internal keys. The
neutral compiler never reconstructs legacy order by grouping providers under
their owners.

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

Provider tables do not contain secret-reference fields in this design because
the locked EdgeZero secret metadata cannot address arbitrary map keys. Adding a
provider secret requires a separate EdgeZero metadata prerequisite or a fixed,
integration-owned non-map path; implementations may not add an unresolvable
map-keyed secret ad hoc.

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

Source validation treats secret leaves as key/store references only. It does
not call constructors such as DataDome `try_new` with an unresolved key name;
format and semantic checks on the resolved secret value run only after runtime
resolution. References retained under disabled integrations still receive the
locked EdgeZero name, store, collision, and adapter checks described below.

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
- A bidder route to an unknown or incompatible provider.
- A selected mediator whose integration is absent, unknown, or lacks the
  mediator capability when enabled.
- Duplicate routes or renderer types.
- A referenced browser module absent from the generated browser catalog.
- An unsupported capability combination.

A disabled integration may retain structurally valid provider configuration;
those providers are not added to the plan. A globally disabled auction may
likewise retain otherwise valid enabled integration and provider configuration
so operators can prepare configuration before enabling the auction. References
from bidder routing or mediator selection to an explicitly disabled integration
are retained but omitted from the compiled plan with one aggregated warning.
This makes `enabled = false` a one-line integration kill switch without making
typos or absent definitions valid. The global auction switch remains the
cross-integration emergency stop. The design does not add a second generic
capability-specific enablement system; server-only Prebid remains an enabled
parent without browser-activating fields.

## Configuration Migration

This is a deliberate operator-source migration. Ordinary config commands in the
new CLI accept only the new inventory and never define precedence between old
and new source fields. `ts config migrate` is the sole legacy-source entry
point; it transforms one complete old source into a candidate and never treats
that source as deployable input. The runtime temporarily supports two stored
application schema versions only to make deployment safe; it normalizes exactly
one complete stored shape and never merges inventories.

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

Milestone 2 includes `ts config migrate [--dry-run]`. The command is a
comment-preserving `toml_edit` transformation, never a push. It:

- rejects mixed old/new inventories and operates on one complete legacy source;
- maps every profile to its owning integration, flattens `profile_config`, drops
  the fixed protocol, qualifies cross-integration references, and creates each
  required parent before its descendants;
- emits retained/required integration parents in the deterministic migration
  sequence `prebid, aps, js_asset_proxy, testlight, nextjs, permutive, lockr,
didomi, sourcepoint, osano, google_tag_manager, datadome, gpt,
gpt_diagnostics, openrtb, adserver_mock`, filtered to configured entries. The
  first fourteen reproduce the complete baseline hook-registration sequence;
  `openrtb` and the mediator have no competing page-hook position. This does not
  preserve the previously cosmetic order of legacy `[integrations.*]` tables or
  assume `js_asset_proxy` was globally first;
- writes explicit `enabled` using a frozen table of the baseline defaults rather
  than guessing one value for every integration; the baseline-true set is
  Prebid, GPT, Didomi, Lockr, and Permutive, and retained blocks for other
  integrations remain false unless legacy server activation requires an
  enabled parent;
- handles server-only Prebid and implicit APS activation explicitly, and warns
  before enabling a retained disabled APS block whose `rendering_mode` would
  change renderer-route behavior;
- emits providers in the new integration/provider declaration order, reports
  every legacy globally interleaved priority that schema 2 cannot retain, and
  shows the resulting qualified sequence before writing;
- prints old-to-new environment-overlay variable paths and fails when an
  app-prefixed environment variable targets a retired path;
- preserves file permissions and uses the existing permission-preserving atomic
  writer.

Missing-`enabled` diagnostics name the exact old default even when operators
migrate by hand. The shipped example, `config init`, audit drafts, operator
guides, and environment-overlay examples contain a complete migrated shape.

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

One shared legacy DTO-to-neutral converter serves both the milestone-one source
normalizer and the schema-one stored-blob reader. It accepts exactly what the
baseline typed loader plus runtime validation accepts: unknown integration IDs
are ignored with a rate-limited warning, integration-specific omitted
`enabled` fields retain their baseline defaults, implicit APS activation
remains, and providers enter the independent flat sequence in global lexical
local-ID order. New schema-2 strictness is never applied retroactively to an
unchanged schema-one blob. The application-schema marker remains at the data
root, and every rollback binary is proven to reject an unknown root marker.
New CLI writes schema 2 only.

Rollout is adapter-specific:

| Adapter    | Forward cutover                                                                                                                                                                                                                                                                                                                                      | Rollback isolation and evidence                                                                                                                                                                                                                                                                       |
| ---------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Fastly     | Export and reconstruct the complete schema-one envelope, including every referenced chunk. Create a new physical Config Store, seed it with schema one, and link it as `trusted_server_config` only in the service version containing the dual reader. After settings-dependent probes pass at representative POPs, push schema 2 to that new store. | The prior service version remains linked to the untouched schema-one store, so reactivation cannot observe schema two. Config GC is forbidden for either rollback generation until the rollback window closes. Control-plane confirmation alone is insufficient because store visibility is eventual. |
| Cloudflare | Create a new Worker version whose `TRUSTED_SERVER_CONFIG` variable contains schema 2; `ts config push` to KV is not treated as a runtime cutover.                                                                                                                                                                                                    | Worker rollback restores the previous code and schema-one binding together. Verify the bound version, startup schema/digest log, and a settings-dependent route.                                                                                                                                      |
| Spin       | Use a named versioned KV key/store only when the deployment platform can export, restore, and select it atomically with the component version.                                                                                                                                                                                                       | The default remote Spin path is blocked from schema-2 rollout until a concrete control-plane export/restore drill exists; liveness alone is not evidence because the startup-error router stays healthy.                                                                                              |
| Axum       | Use the migrated local file/environment as a development-only cutover.                                                                                                                                                                                                                                                                               | Retain the archived schema-one file and restart the matching binary/config pair.                                                                                                                                                                                                                      |

The common release order is: archive and verify schema one; deploy the
dual-reader code against schema one; verify a settings-dependent route and
startup log reporting application schema and non-secret composition digest;
freeze writes; run `ts config migrate --dry-run`; cut over through the
adapter-specific mechanism; then verify registry order, provider order, browser
asset hashes, external provider IDs, and auction behavior. Existing
authenticated diagnostics may expose the same evidence, but no new public
status route is introduced.

Store/version isolation makes downgrade structurally safe where the platform
supports it. The operational old-CLI fence remains defense in depth, not the
only protection: an old writer has no release credentials or destination
mapping for the new Fastly store/version or Cloudflare binding. Schema-one
reappearance after cutover is an explicit rollback event. The compatibility
reader is removed only in a later release after every supported deployment has
completed and drilled its rollback window.

## Required Neutral Lifecycle Boundaries

### Request processing and sharing annotations

DataDome currently communicates a concrete request marker to core before
template lookup. Core uses it to bypass a shared template, require an
unconditional complete origin representation for HTML documents by stripping
conditional and range headers, and make synthesized body-carrying HTML private.
It does not select buffered body processing. Replacing it with either a late
cache-only flag or a blanket buffering requirement would change behavior.

Core therefore owns a small monotonic `RequestProcessingRequirements` value
with three independent axes:

- origin freshness and sharing: ordinary, bypass shared template/readthrough,
  or require an unconditional full HTML representation;
- response privacy scope: unchanged, synthesized body-carrying HTML only, or
  every response;
- fixed browser placement requirements, including a request-conditional
  synchronous asset immediately after the unified tag.

These requirements do not choose `Stream` versus `Buffered` routing and cannot
turn a DataDome-suppressed document into a size-limited full-body buffer.

Every hook may only make a requirement more restrictive. The registry combines
requirements before template-cache, origin-readthrough, and origin-selection
decisions, carries the result through HTML processing, and enforces privacy at
the declared response scope. This is a
processing contract, not a general policy, permission, or vendor-state system.

Registry requirements are an additional monotonic veto, never an alternate
cache authorization path. Shared-template eligibility remains the conjunction
of all current method, request-cache, diagnostics, key-cookie, bypass-cookie,
unlisted-cookie, cookie-independent-origin, assembly-mode, and origin-response
checks plus the registry requirement. The same combined request decision still
governs both warm lookup and cold-store authorization. A registry hook can make
an otherwise shareable request private or origin-bound; it cannot make a
request shareable when any existing cookie or cache gate rejected it.

The verification matrix covers key-cookie, bypass-cookie, malformed, unlisted,
absent, and empty cookie cases plus cookie-independent origins. Integration
requirements may only restrict those existing decisions. DataDome request
filtering and template-cache effects remain Fastly-only where they are
Fastly-only on the baseline; this split does not invent equivalent adapter
behavior.

DataDome sets these requirements inside the request-filter hook at the point
where it already decides client-tag suppression. Its tag-suppression detail
remains opaque integration-owned request/document state. Core sees only the
neutral requirements and an opaque token returned to later hooks; it has no
DataDome type, field, or JSON path.

### Request preparation and response finalization

GPT diagnostics currently has direct preparation calls in adapters and core
and a direct finalization call in core. Add neutral typed hooks for those two
existing lifecycle points. The registry owns opaque request-scoped state
between them. Separately, the static catalog exposes reserved-input normalizers
for every compiled definition whether that integration is absent, disabled, or
enabled.

The GPT-diagnostics reserved-input normalizer preserves the baseline behavior
that is independent of activation: it captures then removes `ts_console`,
removes `__Host-ts-console`, merges retained Cookie fields, and drops Cookie
fields that cannot be represented as visible ASCII. It runs before EC setup,
generic cookie parsing/classification, template-key construction, and origin
forwarding at every existing adapter boundary, with the direct publisher call
remaining an idempotent safety net. This static normalization contract is not
an enabled runtime capability. Therefore an absent GPT-diagnostics block cannot
reintroduce HTTP 400 responses for non-ASCII Cookie fields or leak reserved
inputs upstream.

Preparation returns the opaque per-integration decision plus its declared
`RequestProcessingRequirements`. The neutral requirements are available before
the existing template-cache/private decision; under ESI, request-private opaque
state is never copied into a shared template. Registry preparation remains at
both existing locations: adapter boundaries and the idempotent core publisher
safety-net boundary used by direct core callers. Request extensions make a
second preparation a no-op. Core invokes finalization on the existing response
path. GPT diagnostics may request the fixed synchronous post-unified asset phase
and all-response privacy; ordinary head injectors cannot approximate that
position. The current auction-correlation decision is deployment-scoped, not
request-activation-scoped, and becomes a neutral enabled-capability query so
SPA and non-navigation auctions retain correlation without a concrete GPT
import.

## Browser Composition and APS Renderer

`trusted-server-js` builds only the neutral core IIFE. Before extraction, the
implementation audits every production value import from an integration or the
fixed `creative` prelude into today's `core/` and `shared/` trees. Each import
is classified rather than copied blindly:

- Stateful facilities use one versioned `TrustedServerBrowserRuntime` facade.
  Browser core owns its API and adoption rules, while bootstrap code that must
  run before the unified bundle may create the backing `window.tsjs` object and
  selected state first. The initial surface
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

The GPT bootstrap still runs before the unified bundle, creates or adopts
`window.tsjs`, and may install the first-impression state and GPT lifecycle
listeners. The core IIFE must adopt that exact object identity, preserve
pre-bundle claims and listener markers, validate the runtime-facade version, and
initialize only missing registries. It never replaces bootstrap state. Immediate
GPT and deferred Prebid likewise adopt the facade and keep listener installation
idempotent. Integration bundles consume it through an external runtime shim and
type-only browser-core declarations; their bundler must not inline the stateful
registry implementation. Artifact tests load bootstrap, core, immediate GPT,
and deferred Prebid in production order and prove that first-impression claims,
Permutive context, logging configuration, renderer registrations, and APS frame
supersession use the same shared state.

Every integration IIFE declares the facade version it requires. A mismatched
deferred or stale asset fails closed for that integration with a diagnostic
rather than creating a second registry or mutating an incompatible shape. This
is the runtime version-skew contract for static routes that historically serve
current bytes even when a stale `?v=` value is requested.

`trusted-server-integrations-js` builds integration IIFEs. Immediate modules
are concatenated in the ordering contract above. Deferred and standalone
modules remain separate assets but retain their configuration-relative order
and typed identities.

The browser DOM-insertion dispatcher follows the same ordering rule as Rust
hooks. Its current numeric priority and ID-lexical sort are removed. Immediate
IIFEs evaluate in configured concatenation order, so their registration
sequence already is the configured sequence; no separate ordinal mapping or
inline mapping script is emitted. A handler registers under its owning
integration ID and executes by registration sequence, then by that
integration's local registration sequence. Handler IDs remain diagnostic
identities only, and DOM-insertion handlers are immediate-only so an absent
deferred handler cannot observe mutations too late. Built-in guards must have
disjoint parsed URL ownership as specified above; configuration order is not a
repair mechanism for overlapping substring matchers.

The Rust registration does not carry only a string module ID. Core owns an
immutable `CompiledBrowserAsset` containing the typed module ID, embedded bytes,
SHA-256 hash, and load mode. Composition binds configuration-dependent trusted
script-tag attributes separately and produces a `BrowserDocumentAssets` value
containing:

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

Publisher template invalidation is broader than the browser document, but it
does not invent another canonical serializer or hash resolved secret values.
The composition digest is:

```text
SHA-256(
  UTF8("ts-composition-v1\0") ||
  HEX_DECODE_32(verified_envelope.data_sha256) ||
  BrowserDocumentAssets.document_fingerprint_raw_32 ||
  U32_BE(application_schema) ||
  U32_BE(LEN_UTF8(build_id)) || UTF8(build_id)
)
```

The two digests are raw 32-byte values inside this framing; the EdgeZero digest
is decoded from its validated canonical 64-character lowercase hexadecimal
representation. `application_schema` is an unsigned 32-bit integer. `build_id`
is UTF-8 with a four-byte big-endian byte-length prefix. These fixed widths and
the one explicit length prefix make the input unambiguous; implementations do
not hash the displayed formula, hexadecimal text, or delimiter-free strings.

`data_sha256` is EdgeZero's already-verified canonical hash of stored data
before secret resolution. It covers schema 2 order sidecars, enabled and
disabled source, neutral settings, overlay results, and secret references
without exposing secret values. Raw resolved secrets never enter a template
key. If a future resolved secret changes document bytes rather than only
authorizing an upstream call, its owner must contribute a reviewed non-secret
behavior fingerprint or make the output request-private; it may not hash the
secret itself. Semantically set-valued fields serialize in deterministic order
before EdgeZero hashes them.

The digest is computed lazily and memoized on the immutable composition so
non-document routes and ineligible template-cache requests pay no repeated
full-config hash cost. Normal request execution does not force it merely for
logging. During a rollout, each adapter's configuration-activation path forces
it once for the activated snapshot and emits one rate-limited schema/digest log;
subsequent template use reads the memoized value. An adapter that cannot retain
the composition still deduplicates this rollout log per schema/data hash and
build ID rather than logging per request. The composition digest is distinct
from the exact concatenated unified-bundle hash, whose existing byte-content
semantics remain unchanged.

Core uses that configuration digest directly as the template fingerprint's
configuration-and-document contribution; it does not append
`BrowserDocumentAssets.document_fingerprint` a second time because the digest
already covers it. This replaces only the old complete-`Settings` plus
global-bundle digest; it does not replace any other `TemplateCacheKey`
dimension. Full URL, request host and scheme, origin identity, assembly mode,
ordered `Vary` values, selected cookie values, and `TEMPLATE_SCHEMA_VERSION`
remain independent key inputs. Transform-shape changes still bump
`TEMPLATE_SCHEMA_VERSION`. Tests prove neutral settings, non-head integration
rewriter settings, external and inline assets, and cookie policy changes
invalidate templates without exposing raw configuration.

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
- The direct `/auction` renderer-bid path in browser core also dispatches
  through this registry; it is not a third APS-aware implementation. Renderer
  failure reasons cross the neutral boundary as stable reason codes for the GPT
  bridge without adding an APS enum to core.

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
response, or beacon side effect is newly created before the selected handler
accepts the payload. This rule does not remove the baseline APS supersession
bookkeeping or GPT's fail-closed event propagation, which may occur while
deciding whether a payload is admissible.
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
initializers; Sourcepoint response patches and `_sp_` property trap; and
GPT-diagnostics activation/history bootstrap move byte-for-byte with their
owners. Core-owned `build_bids_script`, `build_seam_script`, and
`build_ad_slots_script` remain core-owned neutral document programs.

This split does not introduce a general template language. Integration-owned
program bodies live in `trusted-server-integrations-js`; integration Rust may
serialize safe data and fill a narrowly generated owner-specific renderer that
preserves the baseline escaping model and output bytes. Exact rendered inline
bytes and hashes participate in the document fingerprint. A source/artifact
guard fails when a new production executable integration algorithm is
introduced directly in Rust without an explicitly reviewed data-only exception.

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

Core visibility changes are governed by a reviewed allowlist and a public-API
snapshot gate. Helpers move with their integration when possible. The
implementation plan names every module and item widened for Prebid, APS, and
the other integrations, classifies it as a neutral contract or moves it
outward, and fails CI on an unreviewed new export. Auction-engine inputs such as
transport headers and response-admission diagnostics are specified as part of
the neutral OpenRTB contract rather than exposed accidentally one helper at a
time.

Core tests do not add a dev-dependency on `trusted-server-integrations`, which
would form a cycle. Vendor-specific unit tests and fixtures move with their
owner. Core tests use neutral stubs behind `test-utils`; the implementation plan
inventories current vendor-bearing fixtures, registry constructors, and test
call sites and checks that moved tests are still selected by a native or
target-matched gate.

Some names remain in core because they are established wire/configuration
contracts rather than implementation ownership. The reviewed retained-name
allowlist includes `creative_opportunities.slot.providers.{aps,prebid}`, the
browser `tsjs.scheduleInitialAdInit` and GAM `hb_*` handshake, and the historical
`prebid_eids` neutral identity module. Their current serialized forms and CLI
editing behavior remain. Conversely, `TrustedServerError::Prebid`, concrete
Prebid request extensions, the JS-asset-proxy header constant, the
`"adserver_mock"` plan literal, DataDome-only staging input, vendor benches, and
the concrete core test fixture move outward or become a named neutral contract.
A mechanical guard rejects any additional concrete vendor token in core unless
it is added to this allowlist with a wire-compatibility reason.

## Compatibility Contract

The change intentionally does not preserve operator configuration or config
blob shape. It also intentionally changes provider priority from lexical local
provider-ID order to qualified configuration order. It preserves:

- Integration IDs.
- Existing routes and endpoint behavior.
- Existing integration hook behavior in milestone 1, except for the explicitly
  listed shared-browser-state corrections; schema 2 then changes applicable
  hook order to configuration order.
- Auction plan compilation semantics after configuration normalization, except
  for the documented provider-priority source.
- OpenRTB request, response, routing, timeout, notification, and response
  admission behavior.
- Auction price comparison, mediation protocol, renderer descriptors, and
  telemetry schema. Provider response order and a locally selected equal-price
  winner may change when configuration order differs from the old lexical
  order.
- Existing provider identity fields. Schema 1 retains local external values;
  schema 2 migrates them from values such as `pbs-main` to qualified values such
  as `prebid.pbs-main` at the documented cutover.
- Managed Prebid User ID aliases, collision checks, consent gating, opaque
  LiveRamp envelopes, OpenRTB EID production, EC partner ingestion, and admin
  diagnostics.
- External Prebid bidder, User ID, and analytics-module selection, manifests,
  hashes, SRI values, and runtime codes.
- Cache privacy, streaming/buffer decisions, origin-representation policy,
  cookie-key and bypass policy, and every
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
- A retained enabled Prebid parent with neither a provider nor browser-activating
  URL becomes a valid staged no-op; the activation matrix and migration tool
  make this explicit rather than inheriting an integration-specific default.
- Attribute and script rewriters use schema-2 configuration order instead of
  the complete legacy sequence. The migration tool shows this order and its
  overlap diagnostics; it does not falsely describe `js_asset_proxy` as the
  baseline first rewriter.
- The unordered abandon/failure provider path begins using plan order.
- Conflicting trusted attributes for the unified script tag fail composition
  instead of warning and keeping the first value; identical duplicates still
  collapse.
- Browser state that is accidentally duplicated across self-contained IIFEs is
  unified behind the runtime facade: Permutive context reaches core collection,
  integration logging follows `tsjs.setConfig`, and APS renderer/frame state is
  shared across core, GPT, and Prebid. These are explicit baseline bug fixes,
  not invisible "pure moves."
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
2 and the rollout decoder, and replaces the temporary source normalizer with
the new source parser. Neutral contracts from workstream 1 may land as
behavior-inert preparatory commits, but milestone 1 is not complete or
deployable as the new architecture until workstreams 1 through 3 all meet its
exit criteria.

Intermediate revisions keep exactly one active catalog while moving
integrations incrementally. The catalog may temporarily delegate individual
entries to explicitly allowlisted implementations still in core, and the
browser manifest may temporarily point individual entries at the legacy source
root. Each move removes one transitional entry. There are never two catalogs or
two interpreted provider inventories, and copied-but-unused duplicate source
trees are forbidden. This scope does not include an external plugin ecosystem.

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
  implicit APS activation, CLI behavior, cache policy, adapter behavior, and
  Fastly settings-only failure paths remain unchanged. The browser-state
  corrections listed in the compatibility contract and one-time artifact/hash
  invalidation are explicit milestone deltas. DOM-handler ordering and trusted
  attribute conflict policy remain at baseline until milestone 2. Every live
  consumer uses the new composition root, every concrete source has moved, all
  transitional delegations are deleted, and the full repository gates pass.
- **Milestone 2 — ordered configuration cutover:** the new operator source is
  the only writable shape; the dual stored-schema reader is deployed; qualified
  identities and order sidecars are used end to end; every normal and recovery
  path uses plan ordinals; activation rules, CLI output, examples, diagnostics,
  scripts, documentation, and the rollout runbook are updated together; and the
  full repository and rollout tests pass before schema 2 is pushed.
- **Later release — cleanup:** the schema-1 reader is removed only after the
  rollback and support conditions in the rollout contract are satisfied.

## Migration Sequence

Implementation uses reviewable commits, but the merged workspace must never
have two active integration or provider inventories. Steps 1 through 6 comprise
milestone 1; each crate/source move is independently testable under the one
transitional catalog.

1. Add neutral capability, processing-requirement, browser-asset, OpenRTB
   profile/exchange, and mediator contracts to core. Extend the `test-utils`
   feature with only neutral stubs required by extracted integration tests.
2. Replace the closed APS and Prebid profile variants and the mediator's legacy
   `AuctionProvider` use while implementations are still in core. Convert GPT
   diagnostics and DataDome call sites to the neutral lifecycle contracts.
3. Create both new crates and update Cargo aliases, native/WASM checks, JS
   discovery/typecheck/lint/format commands, Dependabot inputs, and test package
   selection in the same change. Update compiled documentation snippets and
   crate-path documentation required for that workspace state. Create the
   explicit Rust catalog as the one active catalog, initially delegating through
   a shrinking, reviewed set of transitional core exports. Create the one
   browser manifest with an equally explicit shrinking map to legacy source
   paths. No production code is copied into an unused duplicate tree.
4. Move ordinary Rust integrations one owner at a time, then DataDome and GPT
   diagnostics after their lifecycle seams, APS and Prebid after the profile
   seam, and `adserver_mock` after the mediator seam. Add `openrtb`. Each move is
   a rename-focused change that moves its unit tests/fixtures, removes its
   transitional export, runs the public-API guard, and proves the moved tests
   are selected.
5. Move browser integrations one owner at a time with their unit/artifact tests,
   GPT bootstrap, APS renderer document, integration-owned executable inline
   programs, registry inputs, and shared fixtures. Each move removes one legacy
   manifest path and updates its JS gates immediately. Complete the cross-root
   import audit, remove browser-core APS imports, adopt the shared runtime
   facade, and make composed assets authoritative. Keep baseline DOM priority
   ordering until milestone 2. Cross-adapter system tests remain in
   `trusted-server-integration-tests` and gain bootstrap/core/immediate/deferred
   load-order coverage.
6. Move `TrustedServerAppConfig`, all integration-specific configuration,
   validation, inactive-secret preprocessing, and secret metadata into the
   integrations crate. Land the EdgeZero typed-config source/command extension,
   repin all EdgeZero workspace dependencies together, rewire the CLI to the
   source-view APIs, and rewire all adapters to the single runtime composition entry
   point while retaining the existing operator schema through the shared legacy
   converter. Remove all transitional catalog/source entries.
7. Add the TOML source pre-pass, ordered in-memory maps, application schema 2,
   object-shaped storage with explicit order sidecars, strong qualified
   provider IDs, the shared schema-one reader, `ts config migrate`, and the
   integration-owned provider schema. Replace and remove only the legacy source
   entry point from ordinary config commands; the isolated migration parser may
   read legacy source only to emit a schema-2 candidate.
8. Activate configuration-order provider and hook semantics, chained script
   rewriting, disjoint DOM claims, strict trusted-attribute conflicts, external
   qualified IDs, and plan-order recovery only after focused baseline and
   failure-path tests pass.
9. Update operator examples, fixtures, migration diagnostics, guides, and the
   adapter-specific rollout runbook. Repository/CI path updates associated with
   moved code are already complete from steps 3 through 6. Deploy the dual
   reader, then cut over schema 2 according to the rollout section.
10. Remove the read-only schema-1 blob decoder only in the later release defined
    by the rollout contract.

Steps 7 through 9 are one deployable milestone-two cutover. They may be
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
  and every valid `src/integrations/<id>/mod.rs` directory appears exactly once.
- Invalid, missing, or duplicate catalog IDs fail tests or compilation.
- JavaScript-only and Rust-only directories are accepted.
- A typed Rust reference to an absent browser module fails compilation.
- Embedded bundle hashes match built bytes.
- Core has no dependency on either integrations crate.
- `trusted-server-integrations` has no `edgezero-cli` dependency and compiles in
  every native and WASM adapter graph; the `toml_edit` source module is absent
  from WASM graphs and host command dispatch remains in `trusted-server-cli`.
- Browser core imports no integration source.
- Adapters and CLI import no concrete integration module.
- A public-API snapshot rejects exports not present in the reviewed extraction
  allowlist, and core tests introduce no dev-dependency cycle.
- A dedicated native test/clippy gate executes the integrations catalog and
  host-only completeness tests; relying on adapter dependency builds is not
  sufficient to run them.

### Configuration and ordering tests

- TOML parent-table order becomes the integration-owned source-model order.
- The `toml_edit` pre-pass and typed `toml`/EdgeZero parser use the same TOML
  language generation and agree on parity fixtures outside `[integrations]`.
- A dependency-graph gate proves the host CLI graph enables
  `toml/preserve_order` through the integrations crate's direct feature request,
  while adapter graphs consume only explicit stored sidecars and never depend
  on TOML or JSON object iteration order. Running the storage round trip with
  and without `serde_json/preserve_order` yields identical runtime order.
- Nested provider declaration order is retained.
- A parent integration or provider table declared after one of its descendants
  fails before typed deserialization.
- Missing `enabled` fails; omitted integration tables remain inactive.
- TOML-to-envelope-to-runtime round trips preserve both order sidecars exactly,
  independent of JSON object-member order.
- Order sidecars reject missing, duplicate, extra, or mismatched map keys.
- Schema-1 stored blobs normalize through the transition reader; schema N+1 and
  every other unknown schema value fail before secrets are resolved. Golden
  rollback tests prove every binary retained for the rollback window rejects a
  schema-2 root marker and cannot mistake it for schema 1.
- The shared schema-one converter preserves omitted integration defaults,
  ignores baseline-accepted unknown integration IDs with one rate-limited
  warning, retains local external IDs, and reproduces globally interleaved
  lexical provider priority before qualification.
- Config-store loading produces the same registry, JavaScript, and provider
  order that the CLI validated.
- Registry metadata and CLI provider diagnostics report configured ordinals;
  lookup-map or alphabetic iteration cannot masquerade as execution order.
- CLI validation compiles and discards the complete secret-independent plan,
  including routes, bidder ownership, mediator capability, and duplicate
  claims, without constructing runtime capabilities from unresolved secret key
  names. Diff/push fail selected-target validation. Validate reports a result
  for every supported target declared in the manifest, and `--strict` fails on
  any target rejection. Every secret-independent runtime rejection has a
  matching target-neutral error or named-target CLI fixture.
- Read-only CLI consumers see the effective overlay through
  `SourceConfigView` without deploy validation; deploy commands use
  `ValidatedSourceConfig` after the overlay. Mutators and generators operate on
  file-only bytes, retain invalid-baseline recovery where currently supported,
  and never persist overlay values. Invalid candidate plus valid baseline
  refuses; two invalid values retain the current non-disclosing warning and
  write escape hatch.
- Validate/diff/push use the EdgeZero typed-config extension, proving the
  pre-pass, command/target validation, and typed serialization consume one exact
  read and one effective typed value even under concurrent source replacement.
  No command creates config or manifest snapshots.
- Prebid bundle selection and managed-module requirements come from the
  integration-owned `PartialSourceConfigView<PrebidConfig>`, not a CLI-local
  schema or hard-coded registry path. Bundle modules and generated hash/SRI
  metadata can be staged without an external URL, remain runtime-inert, and are
  patched atomically over an otherwise-invalid unrelated baseline. The command
  fails with a placement example when the explicit `[integrations.prebid]`
  parent is absent and never appends a parent that silently chooses runtime
  order.
- Disabled integrations may retain structurally valid provider settings, contribute no
  providers or capabilities, and do not reorder enabled neighbors.
- Disabled placeholder values that current examples rely on, including the
  Google Tag Manager placeholder container, deserialize safely and defer their
  active-only format validation until enabled.
- Retained secret references in disabled source still pass EdgeZero name,
  store-reference, collision, and adapter validation; omitted active-only
  references are accepted, and inactive paths are not value-resolved at
  runtime.
- Bidder and mediator references to explicitly disabled known integrations are
  omitted from the compiled plan with one warning, including while the global
  auction is disabled; absent and unknown targets still fail.
- Nested-only, unknown, missing-enabled, descendant-before-parent, and mixed
  old/new configurations fail with actionable messages.
- Qualified provider references resolve correctly and reject missing or
  incompatible targets.
- Local and qualified provider IDs enforce their separate grammars and bounds.
- Multiple provider instances under APS, Prebid, and standard OpenRTB compile
  with stable qualified identities.
- Provider launch, response, mediator-input, and local equal-price tie order
  follows integration then local-provider declaration order.
- Every later launch receives its actual remaining logical budget, and tests
  cover adapter timeout bucket boundaries even without deadline expiration.
- Transport-failure recovery paths retain plan order and never fall back to
  lexical provider sorting.
- APS, Prebid, and `openrtb` fixtures cover enabled parents, disabled-parent
  retention, missing-enabled rejection, and the nested provider migration.
- Activation-matrix tests cover APS and Prebid with zero and multiple
  providers, server-only Prebid without browser activation, browser-only Prebid,
  standard OpenRTB, and the `adserver_mock` mediator.
- Qualified IDs that alias under Axum's legacy normalization receive distinct
  correlation names or fail target validation before deployment.
- `ts config migrate --dry-run` preserves comments and permissions, emits
  frozen explicit defaults and environment-variable path mappings, handles
  server-only Prebid and implicit APS, reports priority changes, rejects mixed
  input, and never performs a remote write.
- Ordering diagnostics cover provider details, ts-debug, telemetry `is_win`,
  mediator `ext.bidder_responses`, backend names, and CLI provider listings in
  addition to launch and response order.

### Capability and behavior parity tests

- A focused differential harness captures the `a4e01eb55` baseline for a matrix
  covering all integrations, APS/Prebid/standard providers including globally
  interleaved IDs, deferred Prebid, GPT diagnostics, and the #1135 Next.js
  streaming path. It compares hook order, module ID lists, head inserts, trusted
  tag attributes, route tables, plan/provider order, mediator input order, and
  relevant CLI views. It compares semantic IDs/order rather than rebuilt bundle
  bytes and records the explicit milestone deltas separately.
- The static catalog contains all current integration IDs plus `openrtb`.
- APS and Prebid register page/browser and auction capabilities without core
  importing their types.
- `adserver_mock` registers and is selected through the mediator capability.
- Route tables and duplicate detection retain behavior.
- DataDome preserves unconditional HTML origin-representation handling without
  selecting buffering, and scopes privacy to body-carrying synthesized HTML.
- GPT diagnostics preparation, bootstrap injection, finalization, and caching
  remain unchanged on every adapter path.
- GPT-diagnostics reserved query/cookie normalization runs and remains
  idempotent when the integration is absent, disabled, or enabled. All four
  adapters cover multiple Cookie fields and non-ASCII bytes before generic
  cookie handling. Activation-dependent preparation/finalization stays gated,
  while auction correlation uses deployment capability rather than request
  activation.
- APS and Prebid request construction, transport, parsing, response admission,
  and auction results remain equivalent to current `origin/main` behavior
  except for the documented provider-priority, external-identity, and shared
  browser-state corrections.
- Prepared response parsers consume profile-owned request state without `Any`,
  downcasts, vendor enums, or cross-provider state reuse.
- Prepared outcomes cover both a bound exchange and no-impressions `Skip`, and
  renderer descriptor JSON plus targeting bid identity remains wire-compatible.
- Bidder routing, backend naming, notification suppression, telemetry identity,
  and mediator behavior remain equivalent apart from documented ordering and
  schema-2 qualified-identity changes.
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
  representation, streaming/buffering selection, scoped private caching, and
  origin-readthrough vetoes, and prevent request-private GPT diagnostics state
  from entering ESI templates.
- Warm-hit and cold-store matrices combine DataDome/GPT requirements with key,
  bypass, malformed, unlisted, absent, and empty cookies; integration privacy
  can only restrict the existing cookie/cache decision.
- The dedicated mediator capability preserves request construction, ordered
  response input, bounded transport, parsing, and launch/parse-error fallback
  without exposing the legacy `AuctionProvider` trait. A parsed non-2xx error
  response retains its current zero-winner/no-fallback behavior.
- Fastly's JA4 gate and failed-startup finalization still receive the
  settings-only view when full composition fails; reusable capability objects
  are immutable and request-stateless, with document buffers created per HTML
  processor.

### Browser tests

- Output order is core, creative prelude, and configured integrations.
- Immediate and deferred lists preserve configuration-relative order.
- Artifact/import-graph checks reject undeclared cross-root value imports and
  duplicate state-owner signatures. Permutive registration through its IIFE is
  visible to core context collection; Prebid auction helpers and Testlight
  queue behavior still use the single installed runtime.
- Built-in DOM-insertion guards claim disjoint parsed host/path sets with
  exactly one claimant per canonical URL. Composable handlers run in immediate
  IIFE registration order; numeric priority and lexical handler ID cannot affect
  the result, query-string substrings cannot steal ownership, and a deferred DOM
  handler fails composition.
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
  fallback or newly created route/DOM/message/response/beacon side effect;
  baseline supersession and fail-closed event handling remain, and duplicate
  registration poisons the type or fails composition.
- Both server-bid and Prebid-`adId` renderer paths cover carrier scrubbing,
  failed admission/registration, bounded TTL and capacity, authenticated source
  and slot binding, atomic consume, replay rejection, renderer-owned Universal
  Creative response metadata, and `markWinningBidAsUsed` preservation.
- Unified, deferred, standalone, and inline assets expose bytes and hashes that
  match the emitted document fingerprint and static responses.
- Composition-digest vectors combine the verified EdgeZero data hash,
  document-assets fingerprint, schema, and build ID using the specified raw-byte
  and length framing; reject hex-text or ambiguous concatenation variants;
  change when configured integration/provider order changes; never include
  resolved secret values; and are lazily memoized. Rollout activation forces
  and logs the digest once per snapshot, while ordinary non-document requests do
  not. Set-valued source fields serialize deterministically.
- Changing any integration setting that affects generated head output changes
  the document fingerprint; request-dependent head variation bypasses shared
  template reuse through processing requirements.
- GPT bootstrap, APS renderer document, Sourcepoint trap, GPT-diagnostics
  bootstrap, Sourcepoint response patches, and
  DataDome/Didomi/GPT/Prebid/Sourcepoint inline programs resolve byte-for-byte
  from their integration-owned package locations. A guard rejects handwritten
  production integration browser algorithms in Rust string literals without
  introducing a general template framework.
- External Prebid artifacts preserve bidder, User ID, and analytics category
  selection, manifest/hash/SRI generation, managed-name alias and collision
  checks, `identityLinkIdSystem` requirements, consent behavior, and runtime
  codes after registry and shim paths move.
- Owner-specific private `$OUT_DIR` directories and manifests reject stale or
  partial output; concurrent neutral/integration Cargo builds share only a lock
  around dependency mutation and cannot discover one another's output.
- Clean and incremental Cargo builds prove that changing a sibling integration
  source reruns the integration embed build and changes its manifest/hash while
  leaving an unrelated neutral artifact unchanged.
- Cross-adapter Playwright tests remain in
  `trusted-server-integration-tests` and verify core, creative, APS, GPT, and
  Prebid load order using the moved assets.
- An artifact test loads GPT bootstrap, core, immediate GPT, and deferred Prebid
  in production order, proves object identity/listener idempotence and
  first-impression preservation, and rejects an incompatible facade version.
- Sibling-root gates resolve `vitest`, `prebid.js`, and an exported Prebid module
  through the canonical project; a failing typecheck canary proves moved type
  tests are actually selected, and Prettier/ESLint use explicit canonical roots.

### Repository gates

Before handoff, run every gate required by `AGENTS.md`, including Rust format,
all target-matched clippy aliases, Fastly/Axum/Cloudflare/Spin tests, CLI and
cross-adapter parity tests, required native and WASM builds, JavaScript builds
and Vitest suites for both browser source roots, JavaScript formatting, and
documentation formatting. The explicit package lists in every Fastly
build/check/clippy/test alias include both new Rust crates where applicable;
the CLI and codegen host lint gates remain intact; and a dedicated host gate runs
catalog completeness and integration test support. The migrated Fastly-SDK
guard walks the complete moved integration directory and checks the dependency
graph rather than copying the baseline's incomplete handwritten file list.
Each crate/source move updates these selectors in the same change; a coverage
gate proves moved Rust and JavaScript tests were discovered.

The path migration covers repository automation as well as compiled code and
paths assembled dynamically at runtime:
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
allowlisted transitional or historical reference. It checks string literals and
known path joins, not only exact static paths. Dependabot keeps one entry for
the browser lockfile; unrelated Node projects retain their own entries. The
Cargo graph also records the direct `trusted-server-openrtb`
and integration-test dependencies explicitly and removes unused direct browser
crate dependencies from adapters.

## Baseline Defects and Scope Containment

The review found baseline defects that this design must not accidentally encode
as desired architecture. They land as focused prerequisite commits or as the
explicit milestone corrections named above, not as unrelated additions to the
crate-move pull requests:

- replace nondeterministic serialization of
  `auction.allowed_context_keys` with a deterministic set before using the
  EdgeZero data hash for deployment or template identity;
- make generic Cookie handling robust without depending solely on an enabled
  diagnostics integration, while retaining reserved-input removal and Cookie
  merge semantics in the catalog normalizer;
- unify the browser context/logging/renderer state behind the adopted runtime
  facade and pin the intentional behavior changes with artifact tests;
- preserve baseline acceptance of unknown integration IDs only in schema one,
  while schema 2 rejects them;
- chain GTM/Next.js script rewriting over current text and replace Lockr's
  substring claim with parsed URL ownership before configuration order becomes
  authoritative.

Other baseline issues discovered during review—Prebid bundle file mode, stale
Fastly guard lists, shared `dist` races, unused dependencies, and mediator
non-2xx test coverage—receive focused fixes or migration-gate updates. They are
not reasons to add a plugin system, a second configuration inventory, or an
unrelated public API to this design.

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

Mitigation: use the adapter-specific version/store isolation table, archive a
complete schema-one generation, deploy the dual reader before schema 2, fence
old writers, prove the active binding and settings-dependent behavior, and
remove the legacy reader only after every platform's rollback drill passes.

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

Mitigation: structural read-only commands use `SourceConfigView`; deploy
commands and adapters share the complete secret-independent validation kernel;
only adapter startup continues through resolved-secret construction. No
secondary validation inventory is allowed. Adapters receive the settings,
plan, orchestrator, registry, browser assets, target result, and digest rather
than reconstructing them.

### Stale or incorrectly ordered browser artifacts

Splitting Rust ownership while sharing one Node workspace can embed previous
output, race build scripts, or load APS too late.

Mitigation: write each build directly into a private owner `$OUT_DIR`, lock only
dependency mutation, configure every sibling-root resolver/tool explicitly,
retain stale-output refusal, hash built bytes, load core and the creative
prelude first, reject deferred APS composition, and run artifact-level renderer,
fingerprint, version-skew, and ordering tests.

## Acceptance Criteria

The change is complete when:

1. Both new crates are workspace members and statically linked by the CLI and
   every adapter where required.
2. All fifteen current concrete Rust implementation units live under
   `trusted-server-integrations/src/integrations/<id>/`.
3. The standard provider configuration is supplied by the built-in Rust-only
   `openrtb` integration, making sixteen static definitions in total.
4. All integration-owned browser sources, assets, unit/artifact fixtures, and
   unit/artifact tests, including production executable inline programs, live
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
    schema-one normalization through an independent flat provider sequence;
    schema 2 makes the explicit change from lexical to configuration-order
    provider priority.
12. Browser core imports no concrete integration, and APS rendering works
    through one immediate registration without private copies in core, GPT, or
    Prebid bundles; the shared-state facade adopts GPT bootstrap state, DOM
    ownership is disjoint, composable handler registration obeys configuration
    order, and GPT retains its synchronous-tag bootstrap contract.
13. `TrustedServerAppConfig`, `SourceConfigView`,
    `PartialSourceConfigView<T>`, `ValidatedSourceConfig`, integration secret
    handling, and final runtime composition are owned by
    `trusted-server-integrations`; core has no concrete config or loader
    dependency, the CLI has no duplicate integration schema, and the host-only
    TOML pre-pass is feature-gated out of WASM graphs. EdgeZero's typed-config
    source/command extension replaces filesystem snapshots and all EdgeZero
    dependencies share its reviewed immutable revision.
14. The CLI source phase and adapter runtime phase use the same catalog-backed
    schemas and pure validation definitions, and all adapters receive one
    post-secret-resolution
    settings/plan/orchestrator/registry/browser-assets/target/digest
    composition plus the preserved settings-only degraded view.
15. OpenRTB request-local state crosses the transport boundary through a
    prepared exchange-or-skip outcome and bound response parser without `Any`
    or vendor enum variants in core; renderer descriptors retain their wire
    shape through a neutral type.
16. Explicit `enabled`, parent-before-descendant, disabled-retention, activation
    matrix, local-ID, and qualified-ID rules have end-to-end tests, including a
    server-only Prebid migration that does not activate browser behavior and
    known-disabled reference pruning that acts as a kill switch.
17. Schema-1 blobs remain readable for the documented rollout release, schema-2
    blobs preserve existing secret paths, and binary rollback uses drill-tested
    adapter-specific version/store isolation or verified restoration where
    isolation is unavailable; old schema-1 CLI writers are fenced after cutover
    and stored-schema regression is monitored as a rollback event.
18. Browser assets carry bytes and hashes through composition; the versioned,
    unambiguously framed composition digest already includes the exact
    document-assets fingerprint and is included once in publisher template
    fingerprints; all existing URL, host, scheme, origin, assembly, Vary,
    cookie, and schema-version key dimensions remain; and request-dependent
    variants bypass shared reuse.
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
23. `ts config migrate --dry-run` produces a validated schema-2 candidate,
    reports defaults, environment-overlay renames and priority changes, and
    never writes remotely.
24. Reserved GPT-diagnostics inputs and malformed Cookie fields are normalized
    on baseline routes even when diagnostics is absent or disabled.
25. The adapter-specific rollout proves schema/binding state through logs and a
    settings-dependent probe without adding a public status endpoint.

## Deferred Work

The following require separate designs and real consumers:

- External vendor-owned crates or adapter-supplied registrations.
- Runtime integration loading or a stable integration SDK.
- Independent integration release and compatibility policies.
- New identity, EC, geo, device, and permission-signal provider systems.
- Permission and jurisdiction policy changes.
- Non-OpenRTB auction provider factories.
- Upstream EdgeZero composition and host-service changes beyond the narrow
  typed-config source/command extension required above.
- Moving CLI audit detection metadata into integration directories.
