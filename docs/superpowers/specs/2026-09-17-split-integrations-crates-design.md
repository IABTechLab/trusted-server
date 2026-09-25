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

Repository discovery started from `origin/main` at `a4e01eb55`, not either
prior pull request discussed below. That commit contains the known P1 output
defects listed as prerequisites here and is not an acceptable compatibility
golden. Implementation branching is blocked until the prerequisite fixes land;
the recorded baseline is then the resulting `origin/main` commit plus captured
Next.js/GTM/RSC and browser-runtime output goldens. This design changes only the
configuration, activation, and ordering behavior called out explicitly in this
document; all other corrected-baseline runtime, browser, CLI, and cache behavior
is preserved.

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

### Open Defect and In-Flight Work Disposition

The following items are open as of 2026-09-24. They are coordination inputs,
not design authorities. “Prerequisite” means the focused fix lands on `main`
and this specification records a new baseline before extraction begins; the
crate split does not absorb that bug fix into a move commit.

| Item         | Disposition                                                                                                                                                                                                                                                                             |
| ------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| #1196        | Prerequisite. Restore cross-IIFE context/log state on the current layout, remove or debug-gate Creative's unconditional log-level bump, and add a production-artifact test. The milestone-one typed facade supersedes any transitional `Symbol.for` storage without reverting behavior. |
| #1197        | Prerequisite. Make set-valued configuration serialization deterministic before EdgeZero or template hashes depend on it.                                                                                                                                                                |
| #1198        | Prerequisite. Add the deterministic build digest defined below to the current template key before extraction; the split reuses the same contract.                                                                                                                                       |
| #1199        | Prerequisite. Replace substring DOM ownership with parsed host/path ownership on the current layout.                                                                                                                                                                                    |
| #1200        | Prerequisite. Remove the shared-`dist` partial-build race before two Rust crates consume browser outputs; milestone one then adopts owner-private outputs and the lock contract below.                                                                                                  |
| #1201        | Milestone two. New source and schema 2 reject unknown catalog IDs and fields. The temporary schema-1 reader deliberately retains baseline acceptance with a warning and is not “fixed” retroactively.                                                                                   |
| #1202        | Prerequisite. Make `ts prebid bundle` use the permission-preserving atomic writer and non-disclosing parse errors; the moved command retains that corrected behavior.                                                                                                                   |
| #1203        | Prerequisite and adopted decision. A parsed non-2xx mediator response is a mediation failure and falls back to local ranking as specified below.                                                                                                                                        |
| #1204        | Prerequisite. Every Rust CI job that can trigger a browser build installs the pinned Node toolchain and dependencies; obsolete direct Rust browser dependencies are removed.                                                                                                            |
| #1205        | Proposed remedy superseded. This specification keeps operator order and uses the pure script-source claim validation below instead of an unconditional hidden first phase. The issue must be revised to that contract or closed.                                                        |
| #1206, #1207 | Prerequisites resolved by #1208. Their output regressions are not accepted as differential-harness goldens.                                                                                                                                                                             |
| #1208        | Prerequisite. Compose script-text rewriters on the current layout and add Next.js/GTM output goldens before the baseline is captured.                                                                                                                                                   |

PR #1052 may contribute neutral renderer diagnostics, but APS-named browser
core members do not enter core; they move with APS or become generic renderer
reason fields. PR #1175 and EdgeZero PR #381 are rollout prerequisites: the
typed-config extension and Fastly store procedure build on their reviewed
environment-selector line or an equivalent merged successor, never on v0.0.8.

This specification partially supersedes the following earlier design documents
only where they conflict with the new ownership, configuration, or fingerprint
contracts:

- `2026-08-10-config-first-auction-provider-architecture-design.md`: its
  provider inventory, selection, and ordering syntax are superseded; reusable
  neutral auction-engine findings remain evidence to reverify.
- `2026-07-15-gam-ts-cohort-attribution-design.md` and
  `2026-08-19-gam-attribution-review-resolution-design.md`: attribution wire
  behavior remains, while concrete GPT code and assets move to their integration
  owner.
- `2026-09-08-1138-per-cookie-template-cache-policy-design.md`: cookie/Vary
  policy remains, while complete-settings fingerprinting is replaced by the
  composition/build digest defined here.
- `2026-07-24-prevent-duplicate-gpt-slot-requests-design.md`: duplicate-slot
  behavior remains, while concrete GPT state and browser ownership move outward.

Before either milestone branches for implementation, all prerequisite issues in
the disposition table must have landed, then the baseline commit, output
goldens, and locked EdgeZero revision are recorded again and every
baseline-dependent inventory in this document is rechecked. Open or previously
approved pull requests never override a decision in this specification merely
because of their review status.

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
- **Keeping local provider IDs as the permanent global identity.** Stable
  external values during the dual-reader rollout are required; that concern is
  accepted. **Objection:** local IDs can collide across owning integrations and
  cannot safely key one cross-integration plan. **Decision:** the plan always
  uses `QualifiedProviderId`, while an explicit external label preserves local
  schema-1 values and changes to qualified values only at the schema-2 cutover.
- **Keeping concrete browser integrations in the neutral JS crate or splitting
  them into one package per vendor.** One tested Node dependency graph is
  required. **Objection:** leaving vendor sources in the neutral crate preserves
  the ownership inversion, while per-vendor packages multiply lockfiles,
  resolution rules, and release surfaces without independent consumers.
  **Decision:** use one `trusted-server-integrations-js` Rust/source crate for
  concrete integration browser code and one canonical Node project shared with
  neutral browser core.
- **Keeping the existing mediator trait object unchanged.** One reusable
  mediator implementation and stable fallback behavior are required.
  **Objection:** the existing object couples configuration selection, request
  preparation, transport, response parsing, and provider-specific state, which
  forces concrete knowledge and downcasts across the core boundary.
  **Decision:** use a typed mediator registration, prepared request, and bound
  response parser while retaining the existing wire protocol and the corrected
  fallback policy.
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
  exposes chained replacement and terminal removal in declaration order. Pure
  per-definition script-source claims make an earlier conflicting owner a
  validation error, so the operator must place `js_asset_proxy` first for a
  deliberate per-URL override instead of receiving a silent order-dependent
  bypass.
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
- **A new configuration-status endpoint, public or authenticated.** Runtime settings must be
  loaded and the expected schema must be observable during rollout; that
  concern is accepted. **Objection:** even an authenticated endpoint would add
  a new route, authorization contract, and public support surface unrelated to
  the crate split.
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
  below an integration, such as `aps.aps-main`. Multiple instances may use the same
  integration implementation.
- **Local provider ID:** the provider name within one integration, such as
  `aps-main`.
- **Qualified provider ID:** the strong, globally unique pair of an integration
  ID and local provider ID, such as `aps.aps-main`, serialized as
  `<integration>.<local-provider>`.
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

The public-API snapshot classifies exports as stable neutral contract,
integration-owned facade, or milestone-one transitional delegation. A
transitional export must name its replacement and owning move step; adding one
requires review, and the class must be empty before milestone 1 exits.

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
push have one selected adapter and fail when its target validation fails. The
existing EdgeZero `config validate --strict` meaning is unchanged: it enforces
the manifest-completeness and handler-path checks already owned by EdgeZero.
Trusted Server target-plan validation is exposed separately as a repeatable
`ts config validate --target <adapter>` option. With no `--target`, the command
runs target-neutral Trusted Server checks plus existing EdgeZero validation;
with one or more targets, every selected target is checked and any target
failure makes the command fail. Auction-testing documentation and smoke
commands use this same target spelling. A fixture rejected by runtime
composition for a secret-independent reason must produce the same
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
  → application-schema dispatch
  → schema-2 sidecar/map bijection or schema-1 normalization
  → core-owned neutral/global inactive-secret preprocessing
  → catalog-owned integration inactive-secret preprocessing
  → aggregated core + integration secret resolution
  → resolved settings-only neutral view
  → catalog-aware resolved-value validation
  → core AuctionPlan compilation
  → target validation
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
contract. Building on EdgeZero PR #381 and Trusted Server PR #1175, EdgeZero
therefore gains one narrow typed-config extension with two default no-op
stages. Target resolution occurs before either hook. One exact source read is
shared by the source pre-pass, environment overlay, secret checks, typed
callback, diff, and serialization; a callback cannot reopen the path. Hook
context contains the command kind, the one selected target for diff/push or
the explicitly requested target set for validate, app environment-variable
prefix, `--no-env`, EdgeZero's existing `strict` flag, raw path and bytes, and a
borrow of the overlay-applied typed value. EdgeZero invokes command validation
and serializes that same value. Validate, diff, and push therefore cannot
validate one app-config read or typed value and serialize another.

All workspace `edgezero-*` dependencies are then repinned together from v0.0.8
to one immutable, reviewed tag or commit containing this extension. The
extension does not alter manifest parsing, target selection,
environment-overlay mechanics, logging, storage, or adapter behavior. It
replaces the filesystem snapshot wrapper entirely: config commands do not write
temporary operator or manifest copies, require a writable checkout, rewrite
logged paths, or rely on destructor cleanup around `process::exit` and signals.
If the extension cannot land and the workspace cannot repin to its reviewed
revision, milestone 1 is blocked rather than restoring the snapshot design.
Push-time selected-target validation is an explicit milestone-one CLI delta
needed to place all deploy validation behind the new composition root; it does
not enable schema 2 or configuration-order semantics.

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

The migration updates ad-template lint/help text and `--explain` rule counts in
the same commit so neither describes `[auction.providers]` or reports a stale
number of checks. Documentation-snippet and exact-message tests pin the new
integration-owned paths and counts.

`ts prebid bundle` obtains typed bidder, User ID, analytics, and managed-module
requirements through an integration-owned
`PartialSourceConfigView<PrebidConfig>` rather than a duplicate CLI schema. It
intentionally does not require unrelated app
configuration or an `external_bundle_url` to be deploy-valid: `bundle.modules`,
`external_bundle_sha256`, and `external_bundle_sri` are inert staging metadata,
while `external_bundle_url` activates the browser capability. The command
retains its current ability to build first, patch hash/SRI metadata through the
permission-preserving `write_file_atomically` path, and tell the operator to
upload and set the URL. It validates the structural
pre-pass and affected Prebid subtree before writing; full app validation remains
the contract of config validate/push. Because parent position is runtime order,
the command no longer invents or appends a missing `[integrations.prebid]`
parent. It requires an existing parent with explicit `enabled`, or exits with a
placement example; it may create only owned descendants beneath that parent.
Provider diagnostics display qualified providers in configuration order rather
than alphabetizing a detached map. TOML parse failures report only the path and
line/column; diagnostics never echo source lines or configuration values. The
command canonicalizes both the Node package root and every integration-owned
source input and rejects realpaths that do not belong to the same workspace and
worktree checkout.

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
  exports-aware resolver. Runtime state is externalized behind the facade; the
  build does not depend on bundler deduplication of stateful entry points.
- TypeScript includes both source roots and supplies exact package paths where
  Node's ancestor walk cannot reach the canonical `node_modules`.
- Vitest names the sibling test directory and includes its runtime tests. A
  separate canary command intentionally typechecks a fixture with one expected
  error and asserts that exact diagnostic, so an empty glob or disabled source
  checking cannot pass silently. The canonical package provides a real
  `npm run typecheck` script for both roots.
- ESLint is invoked from their common ancestor with explicit source globs; it
  does not require moving `package.json`. Prettier always
  receives the canonical `--config` path for sibling files.
- Generated external-Prebid entries live under the canonical project or use an
  explicit resolver that preserves the `prebid.js` package `exports` map; a
  directory alias that bypasses package exports is forbidden.
- Because the current `gpt_bootstrap.js` fails the canonical formatter and
  legacy browser lint rules, its move preserves output behind one exact-path
  Prettier ignore and one documented, exact-path ESLint legacy override. The
  shared rules are not weakened; modernizing that file is separate work.

Separate build targets emit neutral and integration artifacts directly into
private owner-specific directories below their Cargo `$OUT_DIR` and validate
per-target manifests before embedding them. They never clean, discover, or copy
from one shared `dist` directory. The dependency installer takes an exclusive
cross-process lock; Vite, Vitest, TypeScript, ESLint, Prettier, both embed
builds, and the CLI Prebid builder take a shared lock for their whole process,
so `npm ci` cannot remove `node_modules` beneath a reader. Owner manifests
record a digest of their complete source inputs and
reject stale output. This removes stale/partial discovery races while allowing
parallel readers. Every Rust and Node participant uses the same canonical lock
path and compatible shared/exclusive protocol; per-crate locks are invalid.

The integration build discovers immediate directories containing `index.ts`
and emits one IIFE per entry point. An IIFE may call the external versioned
browser runtime facade and therefore is not described as self-contained. Its Cargo build embeds each
output and its SHA-256 hash. CI, browser integration scripts, and the CLI
Prebid builder use the single workspace root rather than maintaining a second
dependency graph. Dependabot's existing browser entry continues to watch this
one lockfile; the existing docs entry is unchanged.

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
path constant. The facade returns repository-relative paths, never absolute
compile-checkout paths. The CLI resolves both the package root and inputs under
one selected runtime repository/worktree root, canonicalizes them, and rejects
any realpath that escapes or crosses that checkout. A two-worktree test proves
it cannot fall back to a compile-time `CARGO_MANIFEST_DIR` in the other checkout.

When a browser build is required, a missing `npm` or failed dependency setup is
a hard error. `TSJS_SKIP_BUILD=1` is a local-development escape hatch only when
every expected owner manifest and artifact exists and its recorded input digest
matches; CI never sets it. Build scripts emit `rerun-if-env-changed` for
`TSJS_SKIP_BUILD`, `TSJS_TEST`, and every tool environment variable that changes
output, in addition to complete `rerun-if-changed` input coverage. Every Rust CI
job that can build either browser crate installs the repository-pinned Node
version and dependencies, satisfying prerequisite #1204.

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

Before constructing executable capabilities, each definition produces a pure
`IntegrationDeclaration` from its validated, unresolved-secret source view. It
contains provider/profile facts, mediator presence, browser modules, reserved
inputs, exact or pattern-based route claims, and ordered script-source claims.
These declarations are the shared input to CLI and runtime validation; they do
not allocate clients, read secrets, or execute a rewriter.

A script-source claim is a pure
`claims_script_src(url) -> Replace | Remove | None` predicate with its owning
integration ID and source field. Native integration claims use the same parsed
URL/pattern semantics as their executable attribute rewriter. During schema-2
validation, every `js_asset_proxy` asset URL is evaluated against claims from
earlier enabled integrations. Any earlier `Replace` or `Remove` conflicts with
the asset's `enabled` or `blocked` policy and fails validate, diff, push, and
startup, naming both integration tables and the URL. Placing `js_asset_proxy`
before the native owner is the explicit, visible per-URL override; terminal
blocking or replacement then follows the ordinary chain. Schema-1 compatibility
uses the frozen legacy sequence and does not apply new schema-2 overlap
strictness. `ts config migrate` evaluates its schema-2 candidate and refuses to
write a conflicting result until the operator reorders the parent or removes
the contradictory asset.

Route claims likewise carry method plus canonical path/pattern ownership and
are derived from pure configuration, including Prebid `script_patterns` and
`js_asset_proxy` routes. Duplicate routes therefore fail in
`ValidatedSourceConfig` as well as runtime composition. A future integration
whose route shape depends on a resolved secret needs a separate contract; it
may not defer an otherwise source-visible route collision to startup.

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
corrected prerequisite #1203 outcome policy. Dispatch errors, parse errors, and
all non-2xx responses are mediation failures: they warn and fall back to local
ranking. Non-2xx telemetry records stable reason `http_status`, the numeric
status, and `fallback_used = true`; it never fabricates a mediator winner, and
the original ordered provider outcomes and locally selected winner remain
attributed to their providers. The current mediator endpoint policy, including
an allowed HTTP endpoint where supported today, is preserved. Its response
correlation remains the current mediator integration identity rather than being
silently rewritten as an auction-provider identity.
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
mediator plan with one aggregated warning and ordered diagnostics. Their IDs,
references, and structure are still fully validated; only active-value
semantics resume when the parent is enabled. No unknown ID is silently treated
this way.

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
`account_id`, `debug`, `timeout_ms`, `script_patterns`, `client_side_bidders`,
`excluded_gam_ad_unit_path_suffixes`, and `managed_user_ids`. Any defaulted
typed value in that set that differs from its declared default without
`external_bundle_url` fails validation rather than activating a partial browser
path. Validation is value-based, not source-presence-based: explicitly spelling
the default is equivalent to omitting it, and an implicit default script-pattern
list alone does not activate or fail.
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

- `IntegrationId` matches `^[a-z][a-z0-9]*(?:_[a-z0-9]+)*$`, with a maximum of
  63 ASCII bytes. This permits existing IDs such as `adserver_mock` while
  forbidding dots, leading/trailing underscores, and repeated underscores.
- `LocalProviderId` retains the current
  `^[a-z][a-z0-9-]{0,62}$` grammar; dots are forbidden. Its maximum length is
  63 ASCII bytes.
- `QualifiedProviderId` stores an `IntegrationId` and `LocalProviderId`, parses
  exactly one dot separator, and has a maximum serialized length of 127 ASCII
  bytes.

`QualifiedProviderId` is the internal identity used by bidder routes, provider
plans, and lookup indexes. Its canonical `Display` and serde representation is
`<integration>.<local>`. `ExternalProviderLabel` is the separate validated
string used on backend discriminators, auction responses, diagnostics,
telemetry, mediator correlation, and other externally keyed maps: it contains
the legacy local ID for schema 1 and the qualified ID for schema 2. Every such
map is built from an explicit `ExternalProviderLabel -> plan ordinal` index;
label collisions fail validation and no lookup uses `unwrap_or(usize::MAX)` or
map iteration as a fallback ordering rule.

No consumer reconstructs either type with ad hoc string concatenation,
truncates it, or treats a local provider ID as globally unique. Adapter target
validation predicts and rejects backend-name collisions using the complete
qualified identity. A platform backend name is not itself the provider
identity. Where an adapter's normalization is lossy, as with Axum mapping dots,
hyphens, and underscores to the same character, its correlation name includes
a stable digest of the full qualified ID. Target validation rejects any
remaining final name collision. Tests cover aliases such as `a_b.c` and
`a.b-c`.

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
   each back-to-back launch. A later provider receives
   `min(remaining_deadline, provider_timeout)`; elapsed time reduces the shared
   remainder but may not change the logical budget when the provider's own
   shorter timeout still caps it. Order remains launch-, response-, mediator-,
   and tie-visible even when budget values happen to match.
10. Shared browser dispatchers execute immediate handlers by IIFE registration
    sequence and then definition-local registration order. Because immediate
    IIFEs are concatenated in configuration order and must register
    synchronously during evaluation, this sequence is the configured order
    without a second ordinal channel. Numeric priority, lexical handler ID, and
    later wall-clock callbacks are not alternate ordering mechanisms.

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
the next rewriter's input and a removal is terminal. The pure claim validation
defined above prevents an earlier native owner from silently bypassing a later
`js_asset_proxy` asset policy. Moving `js_asset_proxy` before that owner is the
only supported explicit override; the TOML diff then shows the precedence
change. Overlap tests cover blocked and enabled assets against GPT, Google Tag
Manager, DataDome, Sourcepoint, Permutive, Lockr, Testlight, and Prebid in both
legacy-compatible and deliberately reordered configurations. No engine-only
override is hidden from TOML.

Before either crate move, prerequisite #1208 makes script-text rewriters receive
the current text produced by the preceding rewriter rather than having
independent `lol_html` handlers repeatedly replace the original chunk. That
composition applies to schema-1 and schema-2 blobs; milestone two changes only
which integration order supplies the already-composed chain. Prerequisite #1199
similarly makes DOM-insertion guards claim canonical parsed host/path ownership,
not substring coincidences in a query string. Built-in ownership sets are
disjoint and tested with exactly one claimant for every canonical URL;
configured order applies to genuinely composable handlers, not to select the
winner of an ownership bug.

Browser load mode remains a lifecycle phase, not a second operator priority:
immediate code necessarily evaluates before deferred code. Config order is
preserved within each phase. A browser capability that declares a DOM-insertion
handler has `requires_initial_dom_dispatch = true`; composition accepts that
capability only with `load_mode = immediate`, and the IIFE must register the
handler synchronously before returning. Diagnostics display each module's fixed
load mode so this phase boundary is visible rather than inferred from timing.

All recovery paths obey the same provider ordinals. Existing stable plan-index
sorts remain. The abandon/failure path that can emit providers from `HashMap`
iteration is changed to plan-order recovery before the new ordering contract is
enabled; no no-op lexical-sort change is claimed as a compatibility break.

Inline-table and dotted-key shorthand may not define an integration parent or
provider parent. Requiring ordinary table headers makes activation, ownership,
and order visible in one form and lets the pre-pass produce targeted errors.

The workspace already receives `toml/preserve_order` transitively through
`handlebars`; this design does not pretend that feature is absent or depend on
Cargo feature toggling for correctness. A test asserts that typed
integration/provider order equals the independent `toml_edit` pre-pass order.
The integration-owned source model uses ordered map types with explicit
iteration semantics, never `HashMap` or `BTreeMap`, and stored runtime order
comes only from sidecars.

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
the Trusted Server validate hook receives only the repeatable `--target`
selections, possibly empty. EdgeZero separately retains the manifest-declared
target set for its existing validation and `--strict` checks. Parity
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
than deserializing it back into the operator type. The DTO stores normalized
typed values after defaults, not source-field presence; the retained
`toml_edit` view alone preserves comments and editing intent. `integration_order`
is always present. `provider_order` is required whenever the corresponding
`providers` object is present, including when both are empty; both are absent
when an integration has no provider collection.

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
object-shaped integration entry by ID and removes inactive references; the
shared resolver later writes active resolved values back to the same entry.
Neither searches an `{ id, config }` sequence. End-to-end tests
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

A disabled integration may retain provider configuration, but every retained
child ID and reference is resolved and structurally validated before pruning;
a typo or missing child fails even while disabled. Active-only semantic checks
remain skipped. A globally disabled auction may likewise retain otherwise
valid enabled integration and provider configuration so operators can prepare
configuration before enabling the auction.

References to an explicitly disabled known integration are retained in a
validated suppression set and omitted from executable capabilities with one
aggregated warning. A pruned bidder route produces the distinct diagnostic
outcome `disabled_by_integration`, not `unroutable`, and its bidder stays
suppressed from Prebid client-side fallback. A selected disabled mediator
intentionally falls back to local ranking with a warning instead of the
baseline startup error; this is an explicit behavior change. This makes
`enabled = false` a one-line integration kill switch without making typos or
absent definitions valid. The global auction switch remains the
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
- first derives the complete legacy flat provider sequence in lexical local-ID
  order. Provider-owning parents with active providers are promoted to a prefix
  before `js_asset_proxy`, ordered by the first occurrence of one of their
  providers in that sequence; each parent's provider tables retain their legacy
  local order. If each owner's entries were contiguous, this exactly preserves
  legacy priority. If owners were interleaved, grouping is unavoidable: the
  report prints every old and new ordinal and every reordered pair, then asks
  for confirmation before writing. Remaining parents follow the frozen legacy
  hook sequence `prebid, aps, js_asset_proxy, testlight, nextjs, permutive,
lockr, didomi, sourcepoint, osano, google_tag_manager, datadome, gpt,
gpt_diagnostics, openrtb, adserver_mock`, excluding promoted entries and
  filtering to configured entries. This does not preserve the previously
  cosmetic order of legacy `[integrations.*]` tables or assume
  `js_asset_proxy` was globally first;
- writes explicit `enabled` using a frozen table of the baseline defaults rather
  than guessing one value for every integration; the baseline-true set is
  Prebid, GPT, Didomi, Lockr, and Permutive, and retained blocks for other
  integrations remain false unless legacy server activation requires an
  enabled parent;
- handles server-only Prebid and implicit APS activation explicitly, and warns
  before enabling a retained disabled APS block whose `rendering_mode` would
  change renderer-route behavior;
- for a server-only Prebid migration, enables the parent, comments out rather
  than activates inactive legacy browser-only non-default fields including
  `debug`, `client_side_bidders`, and `timeout_ms`, and reports each edit. It
  preserves inert `bundle.modules`, `external_bundle_sha256`, and
  `external_bundle_sri` staging fields. The emitted candidate must pass the new
  value-based activation validation;
- prints old-to-new environment-overlay variable paths. Migration warns and
  maps stale names when possible; `--no-env` skips even the environment-name
  scan and reports that it did so. Ordinary validate, diff, and push fail any
  app-prefixed variable that still targets a retired path;
- preserves file permissions and uses the existing permission-preserving atomic
  writer.

Missing-`enabled` diagnostics name the exact old default even when operators
migrate by hand. An exact golden test migrates the shipped example and proves
that its previously inactive Prebid browser-only values stay commented/inert,
its bundle staging metadata is preserved, and the result validates. The shipped
example, `config init`, audit drafts, operator guides, and environment-overlay
examples contain a complete migrated shape.

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

| Adapter    | Forward cutover                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      | Rollback isolation and evidence                                                                                                                                                                                                                                                                                                                                    |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Fastly     | A native Fastly entry copy, not `ts` reserialization, reconstructs the complete source root and every referenced chunk. It writes identical chunk keys/values to a new physical Config Store first, reads and reconstructs them there, verifies the archived `BlobEnvelope.sha256`, and writes the pointer/root entry last. Production and staging stores/selectors are copied and verified separately under PR #1175/EdgeZero PR #381. The dual-reader service version alone links the new store; representative POPs must observe the expected setting and a settings-dependent route before schema 2 is pushed. Missing or not-yet-propagated chunks block cutover; milestone 1 explicitly maps this condition to the adapter's transient startup/503 path instead of today's configuration/500, and partial decode is forbidden. | Rollback is only reactivation of the prior service version, which remains linked to the untouched schema-one store. Never redeploy an old release or bind an old binary to the new store. Config GC is forbidden for either rollback generation until the rollback window closes; control-plane confirmation alone is insufficient because visibility is eventual. |
| Cloudflare | Create a new Worker version whose `TRUSTED_SERVER_CONFIG` variable contains schema 2; `ts config push` to KV is not treated as a runtime cutover.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    | Worker rollback restores the previous code and schema-one binding together. Verify the bound version, startup schema/digest log, and a settings-dependent route.                                                                                                                                                                                                   |
| Spin       | Use a named versioned KV key/store only when the deployment platform can export, restore, and select it atomically with the component version.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | The default remote Spin path is blocked from schema-2 rollout until a concrete control-plane export/restore drill exists; liveness alone is not evidence because the startup-error router stays healthy.                                                                                                                                                           |
| Axum       | Use the migrated local file/environment as a development-only cutover.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               | Retain the archived schema-one file and restart the matching binary/config pair.                                                                                                                                                                                                                                                                                   |

Before rollout, the candidate dual reader decodes an archived production
schema-one envelope and compares normalized settings, plan, integration and
provider order, and every `ExternalProviderLabel` with the old binary's output.
The common release order is: archive and verify schema one; deploy the
dual-reader code against schema one; verify a settings-dependent route and
startup log reporting application schema and non-secret composition digest;
freeze writes; run `ts config migrate --dry-run`; cut over through the
adapter-specific mechanism; then verify registry order, provider order, browser
asset hashes, external provider IDs, and auction behavior. Existing
authenticated diagnostics may expose the same evidence, but no new public
status route is introduced.

Store/version isolation makes downgrade structurally safe where the platform
supports it. The operational old-writer fence is defense in depth, not a
credential guarantee or the only protection. The dual reader still accepts a
schema-one write on the new generation but records one structured
schema-regression/rollback event. Schema-one reappearance after cutover is an
explicit rollback event. The compatibility reader is removed only in a later
release after every production adapter, including Spin, has completed and
drilled its rollback window; a blocked Spin cutover blocks reader cleanup.

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
that is independent of activation: it captures console activation from the
`ts_console` query and `__Host-ts-console` cookie into neutral
`ReservedInputState`, then removes those reserved inputs, merges retained Cookie
fields, and drops Cookie fields that cannot be represented as visible ASCII.
The integrations composition boundary exposes one request wrapper that runs
this catalog normalization before EC setup, template-key construction, origin
forwarding, or generic classification. All four adapters and direct core test
entry points use that wrapper. Core's generic Cookie parser remains a lenient,
idempotent safety net, but cannot call the concrete catalog because dependency
direction forbids it. This static normalization contract is not an enabled
runtime capability. Therefore an absent GPT-diagnostics block cannot
reintroduce HTTP 400 responses for non-ASCII Cookie fields or leak reserved
inputs upstream.

Preparation returns the opaque per-integration decision plus its declared
`RequestProcessingRequirements`. The neutral requirements are available before
the existing template-cache/private decision; under ESI, request-private opaque
state is never copied into a shared template. Registry preparation remains at
the integrations-owned wrapper; request extensions make a repeated neutral
preparation a no-op where an adapter boundary can be re-entered. Core invokes
finalization on the existing response path. GPT diagnostics may request the
fixed synchronous post-unified asset phase
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
already-installed namespace. Facade version 1 is exact, not a compatible range;
each install callback resolves `getRuntimeV1()` lazily when it runs instead of
capturing an eager imported object. A mismatch logs one diagnostic, skips that
integration, and leaves its installed/shim flag unset so a failed Prebid load
cannot look successful. The integration build fails on any undeclared
cross-root value import. This covers current imports such as Permutive context
registration, Prebid auction helpers, Testlight queue installation, GPT slot
resolution, and the shared script/beacon guards; it is not limited to the APS
renderer examples.

The GPT bootstrap still runs before the unified bundle, creates or adopts
`window.tsjs`, and may install the first-impression state and GPT lifecycle
listeners. The core IIFE must adopt that exact object identity, preserve
pre-bundle claims and listener markers, validate the runtime-facade version, and
initialize only missing registries. The legacy bootstrap state has no version,
so adoption shape-validates it. GPT may intentionally replace legacy
`adInit`/`scheduleInitialAdInit` functions as the facade is installed; object and
first-impression state identity are preserved, not function identity. Immediate
GPT and deferred Prebid likewise adopt the facade and keep listener installation
idempotent. Integration bundles consume it through an external runtime shim and
type-only browser-core declarations; their bundler must not inline the stateful
registry implementation. Artifact tests load bootstrap, core, immediate GPT,
and deferred Prebid in production order and prove that first-impression claims,
Permutive context, logging configuration, renderer registrations, and APS frame
supersession use the same shared state.

Prerequisite #1196 removes or debug-gates Creative's unconditional log-level
bump before extraction. The default remains `warn`, and an explicit publisher
`warn` setting is never overwritten; a production-artifact test pins both.
Restoring shared context makes Permutive context visible to core collection,
which is an intentional bug fix with a consent caveat: server-side
`allowed_context_keys` remains only a key allowlist, not a browser consent gate.
This extraction adds no new browser consent check and does not redesign consent
policy.

Browser core publishes the queue API without draining preloaded `requestAds()`
calls. Composition appends one neutral finalizer after every immediate
integration IIFE; only that finalizer drains the preloaded queue, after
Permutive and all other synchronous registrations are complete. A production
order artifact test queues a call before loading the bundle and proves its
context providers are present. For one deprecation window, the facade also
backs the compatibility alias `window.tsjs.apsPrebidRenderers` used by bundles
predating #967.

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
  order, attributes, and the exact static or configuration-rendered inline
  bytes.

Trusted attributes from immediate assets are merged onto the unified script
tag through validated name/value constructors. Invalid names or values and
duplicate names with different values fail release-mode composition; equal
duplicates collapse to one attribute. None of these checks is only a debug
assertion.

Static serving, cache-busting URLs, and immutable-cache validation consume
`BrowserDocumentAssets`; core no longer performs a crate-global
`all_module_ids()` lookup. Its document fingerprint includes only assets that
can affect the composed document and includes GPT bootstrap bytes. Each head
injector whose generated inline output varies with integration configuration
supplies the exact immutable bytes for that output. The verified EdgeZero
envelope hash already covers configuration, so injectors do not invent
additional configuration fingerprints. Request-dependent head variation is
permitted only when its neutral
`RequestProcessingRequirements` bypass shared-template reuse; request data is
never folded into a composition-wide fingerprint.

Publisher template invalidation is broader than the browser document, but it
does not invent another canonical serializer or hash resolved secret values.
The composition digest is:

```text
SHA-256(
  UTF8("ts-composition-v1\0") ||
  HEX_DECODE_32(verified_envelope.sha256) ||
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

`BlobEnvelope.sha256` is EdgeZero's already-verified canonical hash of stored data
before secret resolution. It covers schema 2 order sidecars, enabled and
disabled source, neutral settings, overlay results, and secret references
without exposing secret values. Raw resolved secrets never enter a template
key. If a future resolved secret changes document bytes rather than only
authorizing an upstream call, its owner must contribute a reviewed non-secret
behavior fingerprint or make the output request-private; it may not hash the
secret itself. Semantically set-valued fields serialize in deterministic order
before EdgeZero hashes them. When `trusted_server_schema` is absent, the
application schema contribution is exactly `1`.

`build_id` is a deterministic build-time digest:

```text
SHA-256("ts-build-v1\0" || LENGTH_FRAMED_SORTED(PATH || RAW_BYTES))
```

Inputs include core and integrations Rust sources plus both neutral
`trusted-server-js` and `trusted-server-integrations-js` Rust sources, build
scripts, and manifests; neutral and integration browser sources and their
configuration/build scripts; embedded assets and templates; root `Cargo.toml`,
`Cargo.lock`, and pinned toolchain/version files; the canonical `package.json`
and `package-lock.json`; and any target or feature value that can alter emitted
output. Every file uses a normalized workspace-relative UTF-8 path label;
non-file inputs use stable labels such as `target-triple` and
`cargo-features`. Labels and byte values each have an explicit big-endian
length, and records are sorted bytewise by label. One canonical build-ID
generator supplies the value embedded by every composition consumer; generated
output and other self-referential derived artifacts are excluded from its input
set. The build
scripts emit matching
`rerun-if-changed`/`rerun-if-env-changed` lines and generate the digest into the
artifact. Individual asset hashes are build-time inputs; the unified
concatenated hash is a composition-time output. Each deployed artifact forces a
cold template namespace once when this `build_id` changes.

The digest is computed lazily and memoized on the immutable composition so
non-document routes and ineligible template-cache requests pay no repeated
full-config hash cost. Normal request execution does not force it merely for
logging. During a rollout, each adapter's configuration-activation path forces
it once for the activated snapshot and emits one rate-limited schema/digest log.
A process/adapter `CompositionObservationCache`, keyed only by application
schema, envelope hash, and build ID, retains at most 32 entries with LRU
eviction and deduplicates observations without retaining configuration values;
adapters that cannot retain the composition use the same cache instead of
logging per request. The once-per-snapshot structured activation log includes
ordered active integration IDs and ordered browser asset IDs/hashes. Runtime
provider-order evidence comes from existing ordered `/auction`
`ext.orchestrator.provider_details`. Together these make rollout registry/order
verification executable without a new endpoint. The composition digest is distinct
from the exact concatenated unified-bundle hash, whose existing byte-content
semantics remain unchanged.

Core uses that configuration digest directly as the template fingerprint's
configuration-and-document contribution; it does not append
`BrowserDocumentAssets.document_fingerprint` a second time because the digest
already covers it. This replaces only the old complete-`Settings` plus
global-bundle digest; it does not replace any other `TemplateCacheKey`
dimension. Full URL, request host and scheme, origin identity, assembly mode,
ordered `Vary` values, selected cookie values, and `TEMPLATE_SCHEMA_VERSION`
remain independent key inputs. `TEMPLATE_SCHEMA_VERSION` changes only when the
cache-entry serialization or interpretation format changes; transform/output
code changes are already covered by `build_id`. Tests prove neutral settings, non-head integration
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
  through this registry; it is not a third APS-aware implementation. Its
  neutral handler receives the validated `impid` targeting/slot ID, width,
  height, and creative ID. The parser preserves the exact current fallback
  order: numeric `bid.w`/`bid.h`, then descriptor dimensions, then `300x250`;
  string `bid.crid`, then descriptor creative ID, then `<seat>-<impid>`. A
  malformed descriptor is treated as absent and an otherwise valid `adm`
  follows the ordinary generic-creative path, matching the baseline. Only after
  a descriptor is valid and renderer dispatch is selected does a missing or
  rejecting handler suppress the bid without generic fallback. Provider
  admission/drop classification remains unchanged and failures expose stable
  neutral reason codes.
  Renderer failure reasons cross the neutral boundary as stable reason codes
  for the GPT bridge without adding an APS enum to core.

An enabled APS integration whose provider can emit APS renderer descriptors
includes its immediate APS browser module. The core IIFE and fixed creative
prelude load first, so APS registration is complete before a bid can render.
APS module activation is explicit in the activation matrix above. APS renderer
registration is an immediate-only capability; composition rejects
a deferred APS renderer. Its current rendering mode continues to be read while
the synchronous unified script tag is executing, and the APS-owned trusted
attribute remains on that tag. A future switch to a standalone or deferred APS
asset requires replacing `document.currentScript` configuration first.

GPT is also immediate. Its current GPT module reads `document.currentScript`
during module evaluation, so `data-ts-gam-attribution` remains on the unified
synchronous tag and an artifact-level test proves the value is available at
evaluation time. Moving source ownership must not silently make GPT deferred or
move that attribute to a later tag.

Renderer failure is scoped to the owning valid renderer-bearing bid or message.
A missing or rejecting renderer suppresses that bid with no generic-creative
or native-Prebid fallback; malformed direct-auction descriptors retain the
ordinary `adm` behavior above, and unrelated bids and the page continue. A duplicate type
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
initializers, Sourcepoint `_sp_` property trap, and GPT-diagnostics
activation/history bootstrap move with their owners. Sourcepoint response
patches are Rust regex rewriting behavior rather than standalone browser
programs, so that patch logic stays with the Rust Sourcepoint integration while
only executable fragments/assets move. Core-owned `build_bids_script`,
`build_seam_script`, and `build_ad_slots_script` remain core-owned neutral
document programs.

This split does not introduce a general template language. Integration-owned
program bodies live in `trusted-server-integrations-js`; integration Rust may
serialize safe data and fill a narrowly generated owner-specific renderer that
preserves the baseline escaping model. This permits the existing narrowly
scoped safe substitutions but does not expose generic templating. Static and
configuration-rendered inline bytes and hashes participate in the document
fingerprint. Request-dependent GPT-diagnostics bytes bypass shared-template
reuse and do not enter the composition-wide document fingerprint. Canonical
formatting may change generated bytes during the move, so behavior and reviewed
goldens—not a blanket byte-for-byte promise—are the acceptance contract. A source/artifact
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

The integrations crate exposes test-only
`routes_with_source_config`/`routes_with_composition_and_services` helpers (or
an equivalent `TestCompositionFactory` under `test-utils`) for adapter route
tests and cross-adapter parity. Core unit tests use only neutral stub
registrations. The #1135 end-to-end case remains in
`trusted-server-integration-tests/tests/parity.rs`; only integration-owned unit
fixtures move outward. This keeps test construction on the production
composition path without creating a core-to-integrations dev-dependency cycle.

Some names remain in core because they are established wire/configuration
contracts rather than implementation ownership. The reviewed retained-name
allowlist includes `creative_opportunities.slot.providers.{aps,prebid}`, the
browser `tsjs.scheduleInitialAdInit` and `trustedServer` namespace, the GAM
`hb_*` handshake and slot fields, `nextjs-static`, `TrustedServerError::Gam`,
`rsc_flight`, APS/Prebid slot parameter types, and the historical `prebid_eids`
neutral identity module. Their current serialized forms and CLI editing
behavior remain. Conversely, `TrustedServerError::Prebid`, concrete Prebid
request extensions, the JS-asset-proxy header constant, the `"adserver_mock"`
plan literal, DataDome-only staging input, vendor benches, and the concrete core
test fixture move outward or become a named neutral contract. A mechanical
guard scans production Rust/JS beneath core, excluding tests, fixtures,
generated output, and exact allowlisted paths/tokens. It rejects any additional
concrete vendor token unless added with a wire-compatibility reason; it is not a
raw substring ban across the repository.

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
  provider telemetry schema, apart from the explicit #1203 mediator-failure
  fields below. Provider response order and a locally selected equal-price
  winner may change when configuration order differs from the old lexical
  order.
- Managed Prebid User ID aliases, collision checks, consent gating, opaque
  LiveRamp envelopes, OpenRTB EID production, EC partner ingestion, and admin
  diagnostics.
- External Prebid bidder, User ID, and analytics-module selection, manifests,
  hashes, SRI values, and runtime codes.
- Cache privacy, streaming/buffer decisions, origin-representation policy,
  cookie-key and bypass policy, and every
  existing publisher template-key dimension.
- Current CLI ad-template diagnostics, audit/generator recovery behavior, and
  corrected prerequisite #1202 Prebid bundle mutation behavior.
- The route and behavioral parity of Fastly, Axum, Cloudflare, and Spin, except
  for the explicitly listed Fastly missing-chunk error-class correction.

The intentional compatibility breaks are:

- Auction providers move from `[auction.providers]` beneath their owning
  integration and references become qualified.
- External provider labels change at the schema-2 cutover from local values
  such as `pbs-main` to qualified values such as `prebid.pbs-main`; schema 1
  retains the legacy labels during the dual-reader window.
- The corrected #1203 mediator-failure telemetry adds `reason = "http_status"`,
  numeric `http_status`, and `fallback_used = true` on the structured mediator
  outcome/log surface for non-2xx fallback; provider outcome telemetry remains
  unchanged.
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
- References to a known disabled integration are pruned as a kill switch:
  bidder routes report `disabled_by_integration` and stay suppressed from
  client-side fallback, while a disabled selected mediator warns and uses local
  ranking instead of the baseline startup error. Absent and unknown targets
  still fail.
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
  not invisible "pure moves." Permutive's newly restored context can now be
  emitted for allowlisted keys without a new browser consent gate;
  `allowed_context_keys` is not represented as consent enforcement, and a
  consent-policy redesign is explicitly out of scope.
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
  implicit APS activation, operator source semantics, cache policy, adapter behavior, and
  Fastly settings-only failure paths remain unchanged except that a missing or
  not-yet-propagated config chunk becomes the documented transient startup/503
  outcome instead of configuration/500. The EdgeZero typed hook,
  `config validate --target`, and selected-target diff/push validation are the
  explicit milestone-one CLI deltas. The browser-state
  corrections listed in the compatibility contract and one-time artifact/hash
  invalidation are explicit milestone deltas. Corrected script chaining,
  disjoint parsed DOM ownership, and release-mode trusted-attribute conflict
  validation are prerequisites and apply to schema 1 as well as schema 2; only
  their ordering source changes later. Every live consumer uses the new
  composition root, every concrete source has moved, the public-API snapshot's
  explicitly classified transitional-export set is empty, and the full
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
   crate-path documentation required for that workspace state. Move
   `TrustedServerAppConfig`, integration validation/secret metadata, and the
   composition facade to the integrations crate; rewire every adapter and the
   CLI to that one composition root while retaining the legacy schema through
   the shared converter. Create the explicit Rust catalog as the one active
   catalog, initially delegating through a shrinking, reviewed class of
   transitional core exports, and delete core's old `builders()` and concrete
   `with_plan` composition paths. Create the one browser manifest with an
   equally explicit shrinking map to legacy source paths. No production code is
   copied into an unused duplicate tree. The EdgeZero extension and selected-
   target push validation land here so config consumers cannot bypass the new
   root.
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
   facade, and make composed assets authoritative. Under schema 1, immediate
   concatenation reproduces the corrected frozen legacy order through the new
   registration-sequence dispatcher; schema 2 later changes only the order
   source. Cross-adapter system tests remain in
   `trusted-server-integration-tests` and gain bootstrap/core/immediate/deferred
   load-order coverage.
6. Remove the final Rust and browser transitional exports/path mappings, prove
   the public-API snapshot's transitional class is empty, and run the complete
   schema-one differential and repository gates. This is cleanup and
   verification of the composition switch made in step 3, not a second switch.
7. Add the TOML source pre-pass, ordered in-memory maps, application schema 2,
   object-shaped storage with explicit order sidecars, strong qualified
   provider IDs, the shared schema-one reader, `ts config migrate`, and the
   integration-owned provider schema. Replace and remove only the legacy source
   entry point from ordinary config commands; the isolated migration parser may
   read legacy source only to emit a schema-2 candidate.
8. Switch the already-composed hook and provider pipelines from frozen legacy
   order to schema-2 configuration order, and activate schema-2 external
   qualified labels and plan-order recovery only after focused baseline and
   failure-path tests pass. Script chaining, disjoint DOM ownership, and trusted
   attribute validation do not wait for this step.
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
- A dependency-graph gate records the existing unified `toml/preserve_order`
  feature while adapter graphs consume only explicit stored sidecars and never
  depend on TOML or JSON object iteration order. Storage tests permute JSON
  object members and prove identical runtime order; they do not claim to toggle
  a unified Cargo feature within one graph.
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
  names. Diff/push fail selected-target validation. Validate with no `--target`
  runs target-neutral Trusted Server checks plus unchanged EdgeZero validation;
  each explicit repeatable `--target` is reported and any selected-target
  rejection fails. EdgeZero `--strict` retains its existing manifest meaning.
  Every secret-independent runtime rejection has a matching target-neutral
  error or named-target CLI fixture.
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
  order. Tests preserve the original file mode and prove parse diagnostics
  contain path and line/column but no source content.
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
  auction is disabled; absent and unknown targets still fail. Pruned routes
  report `disabled_by_integration` and remain suppressed from Prebid client-side
  fallback, while a disabled selected mediator warns and uses local ranking.
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
  server-only Prebid and implicit APS, preserves exact legacy priority when
  provider owners are contiguous, reports every old/new ordinal and reordered
  pair when grouping cannot preserve an interleaving, rejects mixed input, and
  never performs a remote write. The shipped-example golden validates.
- Ordering diagnostics cover provider details, ts-debug, telemetry `is_win`,
  mediator `ext.bidder_responses`, backend names, and CLI provider listings in
  addition to launch and response order.

### Capability and behavior parity tests

- After every prerequisite lands, a focused differential harness records the
  exact baseline commit and captures actual output goldens for a matrix covering
  all integrations, APS/Prebid/standard providers including globally interleaved
  IDs, deferred Prebid, GPT diagnostics, Next.js/GTM script rewriting, RSC
  streaming, and the #1135 path. It compares hook order, rendered output,
  module ID lists, head inserts, trusted tag attributes, route tables,
  plan/provider order, mediator input order, and relevant CLI views. Rebuilt
  artifact hashes are recorded separately from behavior deltas.
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
  response records `http_status` and falls back to local ranking without a
  fabricated mediator winner, as fixed by prerequisite #1203.
- Fastly's JA4 gate and failed-startup finalization still receive the
  settings-only view when full composition fails; reusable capability objects
  are immutable and request-stateless, with document buffers created per HTML
  processor. A Fastly adapter test proves a missing referenced config chunk
  takes the milestone-one transient startup/503 path and never partially
  decodes the envelope.

### Browser tests

- Output order is core, creative prelude, and configured integrations.
- Immediate and deferred lists preserve configuration-relative order.
- Artifact/import-graph checks reject undeclared cross-root value imports and
  duplicate state-owner signatures. Permutive registration through its IIFE is
  visible to core context collection; Prebid auction helpers and Testlight
  queue behavior still use the single installed runtime.
- Built-in DOM-insertion guards claim disjoint parsed host/path sets with
  exactly one claimant per canonical URL. In milestone 1, composable handlers
  run in immediate IIFE registration order sourced from the corrected frozen
  legacy sequence; in milestone 2, the same dispatcher receives schema-2
  configuration order. Numeric priority and lexical handler ID cannot affect
  either result, query-string substrings cannot steal ownership, and a deferred
  DOM handler fails composition.
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
- Equal duplicate trusted script attributes collapse, while invalid names,
  invalid values, and conflicting duplicates fail release-mode composition
  before HTML is served.
- Missing or rejecting renderers drop only the renderer-bearing bid with no
  fallback or newly created route/DOM/message/response/beacon side effect;
  baseline supersession and fail-closed event handling remain, and duplicate
  registration poisons the type or fails composition.
- Direct-auction tests pin `w`/`h`/descriptor/default dimension precedence,
  `crid`/descriptor/seat-plus-impid creative-ID precedence, malformed-descriptor
  generic-`adm` fallback, and valid-descriptor fail-closed dispatch.
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
- Build-ID generator vectors are deterministic across repeated builds and
  independently perturb core Rust, integration Rust, neutral browser source,
  integration browser source, `Cargo.lock`, target triple, and feature inputs;
  each relevant change alters the ID while file ordering does not.
- Changing any integration setting that affects generated head output changes
  the document fingerprint; request-dependent head variation bypasses shared
  template reuse through processing requirements.
- GPT bootstrap, APS renderer document, Sourcepoint trap, GPT-diagnostics
  bootstrap, and DataDome/Didomi/GPT/Prebid/Sourcepoint executable inline
  programs resolve from their integration-owned package locations and pass
  reviewed behavior/output goldens. Sourcepoint's Rust regex response patches
  stay with its Rust owner. A guard rejects handwritten production integration
  browser algorithms in Rust string literals without introducing a general
  template framework.
- External Prebid artifacts preserve bidder, User ID, and analytics category
  selection, manifest/hash/SRI generation, managed-name alias and collision
  checks, `identityLinkIdSystem` requirements, consent behavior, and runtime
  codes after registry and shim paths move.
- Owner-specific private `$OUT_DIR` directories and manifests reject stale or
  partial output; installation holds the exclusive dependency lock while every
  Vite, Vitest, TypeScript, ESLint, Prettier, Cargo browser build, and CLI
  Prebid reader holds the shared lock for its entire process.
- Clean and incremental Cargo builds prove that changing a sibling integration
  source reruns the integration embed build and changes its manifest/hash while
  leaving an unrelated neutral artifact unchanged.
- Cross-adapter Playwright tests remain in
  `trusted-server-integration-tests` and verify core, creative, APS, GPT, and
  Prebid load order using the moved assets.
- An artifact test loads GPT bootstrap, core, immediate GPT, and deferred Prebid
  in production order, proves object identity/listener idempotence and
  first-impression preservation, rejects an incompatible facade version without
  setting the Prebid shim flag, and verifies the temporary
  `apsPrebidRenderers` compatibility alias. A queued pre-load `requestAds()` is
  drained only by the post-immediate finalizer and observes Permutive context.
- Production logging artifacts prove the default remains `warn`, an explicit
  publisher `warn` is not overwritten, and Creative does not raise the shared
  level.
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
known path joins, not only exact static paths. Dependabot's existing browser and
docs entries remain; this split adds no second browser entry. The
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

Mitigation: write each build directly into a private owner `$OUT_DIR`, use an
exclusive dependency-mutation lock and full-process shared locks for every
browser/tooling reader, configure every sibling-root resolver/tool explicitly,
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
    ownership is disjoint, composable handler registration obeys frozen legacy
    order for schema 1 and configuration order for schema 2, and GPT retains its
    synchronous-tag bootstrap contract.
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
    settings-dependent probe without adding any new status endpoint.

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
