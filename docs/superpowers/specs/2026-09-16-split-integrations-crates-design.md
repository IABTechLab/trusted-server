# Split Integrations into Dedicated Rust and JavaScript Crates

**Date:** 2026-09-16
**Status:** Proposed
**Scope:** Move every concrete integration implementation out of
`trusted-server-core` and `trusted-server-js` into two statically compiled,
directory-discovered workspace crates

## Summary

Trusted Server will separate its concrete integrations from its neutral server
and browser runtimes.

The workspace gains two crates:

- `trusted-server-integrations`, containing every concrete Rust integration.
- `trusted-server-integrations-js`, containing every integration-specific
  browser module, browser asset, and JavaScript test.

Each crate discovers integrations from its own directories at build time. The
Rust and JavaScript inventories are independent because the repository has
Rust-only and JavaScript-only integrations. A Rust integration may declare that
it requires a same-named JavaScript module, and that reference is checked at
build time.

`trusted-server-core` keeps only the neutral contracts and runtime machinery
needed to execute registrations. `trusted-server-js` keeps only the neutral
browser runtime. Adapters and the CLI use the statically linked
`trusted-server-integrations` crate as the composition root.

This is a packaging and dependency-direction change. It is not the provider,
permission, configuration, or externally loaded integration system proposed by
PR #1084.

## Context

On `main` at `6cae7f5da`, concrete integrations live inside
`crates/trusted-server-core/src/integrations`. That directory contains the
neutral registry alongside fifteen concrete implementation units:

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

The ordinary builder table registers twelve of those units. APS and Prebid
register separately from the compiled auction plan, while `adserver_mock`
registers the current mock mediator. That difference in construction does not
justify leaving those implementations in core.

Integration browser code lives beside browser core under
`crates/trusted-server-js/lib/src/integrations`. The current build discovers
directories containing `index.ts`, emits one IIFE per discovered entry point,
and embeds those bundles and hashes into the `trusted-server-js` Rust crate.
The directory also contains APS renderer code that browser core imports
directly even though APS has no current `index.ts` entry point.

This layout has four problems:

1. Core contains both the integration contract and every implementation.
2. Rust registration, deploy validation, and migration guards maintain
   separate concrete inventories.
3. Browser core and integration code have dependencies in both directions.
4. Adding an in-tree integration requires editing central hand-maintained
   lists instead of adding a self-contained directory.

PR #1084 identifies these problems but combines their solution with external
vendor crates, runtime registration injection, provider capabilities,
configuration changes, permission work, and adapter composition changes. This
specification takes only the shared-crate and build-time discovery decisions.

## Goals

1. Move all fifteen concrete Rust implementation units into one
   `trusted-server-integrations` crate.
2. Move all integration-specific TypeScript, JavaScript assets, fixtures, and
   JavaScript tests into one `trusted-server-integrations-js` crate.
3. Give every concrete Rust implementation its own directory.
4. Discover the Rust and JavaScript inventories at build time.
5. Remove concrete integration construction and validation tables from core.
6. Preserve current configuration, routes, ordering, auction behavior, wire
   formats, and request/response behavior.
7. Make core compile without depending on either integrations crate.
8. Keep the two new crates statically linked workspace components.

## Meaning of “Concrete Integration”

For this change, a concrete integration is an implementation currently under
`trusted-server-core/src/integrations`, integration-specific browser code under
`trusted-server-js/lib/src/integrations`, or direct construction and lifecycle
plumbing that imports one of those implementations.

The extraction does not require renaming or relocating every existing domain
type, compatibility field, comment, or wire type that mentions APS, GPT,
Prebid, or another integration. For example, a serialized auction renderer
descriptor may remain in core when it is part of the existing core wire model.
Such types move only when leaving them in place would create a dependency from
core to a concrete implementation.

This boundary keeps the requested extraction complete without turning it into
an auction-domain or configuration-model rewrite.

## Non-Goals

This specification does not introduce:

- External integration crates.
- Runtime-loaded integrations or dynamic registration.
- Per-vendor ownership or independent release lifecycles.
- A public plugin SDK.
- Identity, geo, device, permission, demand, or ad-server provider systems.
- Configuration key, schema, or file-format changes.
- New integrations or new integration capabilities.
- A redesign of the auction plan, ranking, mediation, or renderer wire format.
- EdgeZero lifecycle, evidence, store, or adapter changes.
- A general-purpose hook or capability language.
- A reorganization of the CLI audit analyzer or other integration-related code
  that is not part of the implementation, validation, or registration paths
  being extracted.

The new cross-crate interfaces exist only to preserve the current statically
compiled application. Designing them so a future external crate could use them
is explicitly deferred until a real external consumer exists.

## Target Workspace Layout

```text
crates/
  trusted-server-core/
    src/
      integration/
        mod.rs
        registry.rs

  trusted-server-js/
    lib/src/core/
    src/

  trusted-server-integrations/
    build.rs
    Cargo.toml
    src/
      lib.rs
      adserver_mock/
        integration.toml
        mod.rs
      aps/
        integration.toml
        mod.rs
      datadome/
        integration.toml
        mod.rs
        protection.rs
        protection_scope.rs
      ...
      nextjs/
        integration.toml
        mod.rs
        html_post_process.rs
        rsc.rs
        rsc_placeholders.rs
        script_rewriter.rs
        shared.rs
        fixtures/

  trusted-server-integrations-js/
    build.rs
    Cargo.toml
    lib/
      package.json
      src/
        aps/
          index.ts
          render.ts
        creative/
          index.ts
          ...
        datadome/
          index.ts
          ...
        ...
      test/
        integrations/
        fixtures/
    src/
      lib.rs
```

Every flat Rust implementation file becomes `<id>/mod.rs`. Existing nested
modules and fixtures stay with their owning integration. The GPT bootstrap
script moves to `trusted-server-integrations-js` and is exported as an
integration asset rather than remaining beside Rust source.

JavaScript-only `creative` remains valid without a Rust directory. Rust-only
integrations remain valid without a JavaScript directory.

## Dependency Direction

The dependency graph is one-way:

```text
trusted-server-js
       ^
       |
trusted-server-integrations-js
       ^
       |
trusted-server-integrations ----> trusted-server-core ----> trusted-server-js
       ^                                  ^
       |                                  |
       +------------- adapters -----------+
       +--------------- CLI
```

The diagram shows logical dependencies. Cargo may deduplicate a shared
`trusted-server-js` dependency; no cycle is permitted.

The rules are:

1. Core never depends on `trusted-server-integrations` or
   `trusted-server-integrations-js`.
2. Concrete Rust integrations may use public core contracts and domain types.
3. Integration JavaScript may import the explicit browser-core source API.
4. Browser core must not import a concrete integration.
5. Adapters and the CLI compose core with the built-in integrations crate.

## Neutral Core Contract

The existing neutral contents of `integrations/registry.rs` move to a singular
`trusted_server_core::integration` module. The singular name distinguishes the
contract from the collection of concrete implementations.

Core continues to own:

- `IntegrationRegistration` and its builder.
- `IntegrationRegistry` and registry execution.
- Request filtering, proxy, rewriting, head injection, HTML post-processing,
  and other neutral hook traits and contexts.
- Duplicate ID and route detection.
- Neutral script-module metadata used by publishing and HTML injection.
- Test helpers for empty or stub registries.

`IntegrationRegistry` no longer constructs built-ins. Its production
constructor accepts completed registrations and the existing compiled auction
plan. The fixed `builders()` table, APS/Prebid special construction, and
concrete `IntegrationRegistry::with_plan` assembly move to
`trusted-server-integrations`.

Core tests that need only registry behavior use neutral stubs. Tests that need
the actual built-in catalog move to or depend on the integrations crate at the
outer composition layer.

## Rust Directory Discovery

`trusted-server-integrations/build.rs` scans immediate directories under
`src/`. A Rust integration directory must contain:

- `mod.rs`.
- `integration.toml`.

The directory-local manifest is deliberately small:

```toml
id = "gpt"
order = 80
javascript = true
```

The fields mean:

- `id` must equal the directory name and use Rust `snake_case`.
- `order` preserves current registration and hook order. Ordering metadata is
  owned by the integration rather than a central list.
- `javascript` states whether a same-named directory with `index.ts` must exist
  in `trusted-server-integrations-js`.

No provider, permission, configuration, dependency, or ownership metadata is
added to this manifest.

The build script generates module declarations and an ordered definition
table. It fails the build for:

- A malformed or mismatched ID.
- A missing manifest or `mod.rs`.
- Duplicate IDs or order values.
- A JavaScript requirement whose same-named `index.ts` is absent.
- Generated output that would be empty.

The script emits `cargo:rerun-if-changed` directives for the discovered Rust
directories and for JavaScript directories referenced by a Rust manifest.

Each generated module exposes one crate-private definition function. The
returned internal definition may carry only the contributions already needed
by current code:

- Deploy and startup validation.
- An ordinary page registration.
- An auction profile or transport implementation.
- The current mock mediator implementation.
- An optional JavaScript module or asset reference.

This internal definition is not exported as a plugin contract. APS, Prebid,
and `adserver_mock` use the same generated inventory as every other directory,
even though their existing contributions are different.

## JavaScript Directory Discovery

`trusted-server-integrations-js` owns its Node project, tests, build pipeline,
generated Rust module catalog, and integration assets.

Its JavaScript build discovers immediate directories containing `index.ts`,
sorts them deterministically, and emits one self-contained IIFE per directory.
Its Cargo build script embeds each bundle and its SHA-256 hash, following the
current `trusted-server-js` mechanism.

The inventories remain independent:

- A JavaScript-only directory such as `creative` is built without a Rust
  registration.
- A Rust directory with `javascript = false` does not require a browser module.
- A Rust directory with `javascript = true` requires a same-named browser entry
  point and receives that generated module at composition time.

There is no central allowlist spanning the crates.

`trusted-server-js` builds only the core IIFE and exposes neutral helpers for
combining the core bundle with supplied integration modules. The concatenation
order remains core first, followed by immediate integration modules in registry
order. Deferred and standalone modules remain separately addressable.

## Static Composition

`trusted-server-integrations` is the only built-in composition root. It uses
the generated definition table in this order:

1. Run integration-aware deploy or startup validation as requested by the
   caller.
2. Collect the existing auction profile inputs needed to compile the canonical
   auction plan.
3. Ask core to compile the plan using those inputs.
4. Build the existing mediator and integration registrations against that plan.
5. Resolve required bundles and assets from
   `trusted-server-integrations-js`.
6. Pass completed registrations to core's neutral registry constructor.

The exact Rust function names are left to the implementation plan, but the
composition path must be shared. The four adapters must not recreate the
generated catalog or call concrete integrations directly.

At runtime:

1. An adapter holds the completed core registry and auction state as it does
   today.
2. Core invokes neutral registry hooks during request and response processing.
3. Script generation starts with the core bundle and reads enabled integration
   bundles from the registry.
4. Deferred and standalone script requests resolve against registry-carried
   module metadata rather than a global concrete list in core.

No runtime directory scanning or dynamic loading occurs.

## Configuration and CLI Validation

Configuration keys and serialized shapes do not change.

Core retains global settings parsing and global validation. Concrete config
types and integration-specific validation move with their implementations.
The generated definition table supplies integration validation to both runtime
startup and the CLI.

The integration-aware typed app-config wrapper composes:

- Core secret-field metadata and global validation.
- Generated integration secret-field metadata and validation.

The CLI uses this composed wrapper for `config validate`, `config diff`, and
`config push`. This removes the concrete validation table and DataDome-specific
secret paths from core without changing the published `trusted-server.toml`
shape.

Unknown or invalid integration settings continue to fail with the current
error contexts. An integration present in configuration but absent from the
compiled catalog must fail rather than be silently ignored.

## Required Neutral Lifecycle Hooks

Moving every implementation exposes two existing reverse dependencies that
must become neutral registry behavior.

### Response sharing annotation

DataDome currently communicates a concrete request marker back into core so
core buffers the full response and applies private caching. Replace that marker
with a neutral response-sharing annotation owned by core. DataDome sets the
annotation through its registered hook; core performs the same buffering and
cache behavior without importing a DataDome type.

This annotation does not introduce a general policy system. It represents only
the existing shared-versus-request-private decision already consumed by core.

### Request preparation and response finalization

GPT diagnostics currently has direct preparation calls in every adapter and in
core, plus direct finalization in core. Add neutral registration hooks for
those two lifecycle points. The registry owns any opaque request-scoped state
between them.

Adapters call the registry preparation operation at the same request boundaries
used today. Core calls finalization on the same response path used today.
Integration-specific bootstrap and script decisions flow through the existing
head-injection and document-state mechanisms instead of concrete fields on
`HtmlProcessorConfig`.

No other lifecycle stages are added.

## Auction-Coupled Implementations

APS, Prebid, and `adserver_mock` move with all other concrete implementations.
Core keeps the generic auction plan, orchestration, request, response, and wire
types.

The smallest cross-crate inputs needed by the current implementations become
public neutral core APIs:

- Profile compilation registrations consumed by the existing plan compiler.
- Provider transport and response callbacks consumed by the existing generic
  provider path.
- The optional mediator passed to the existing orchestrator.

The integrations crate supplies only the current built-ins through these APIs.
This work must not change provider configuration, plan semantics, routing,
timeouts, response normalization, ranking, mediation, or telemetry.

Existing APS-specific serialized types may remain in core where they are part
of the established browser wire contract. The concrete APS parsing,
validation, transport, and rendering implementation moves.

## Browser APS Boundary

Browser core currently imports APS renderer parsing and dispatch directly.
That reverse dependency must end when APS moves.

Browser core will own one narrow bid-renderer registration mechanism:

- Core parses the existing renderer envelope only far enough to identify its
  type and retain its opaque payload.
- An integration module registers the parser and dispatcher for its renderer
  type during IIFE initialization.
- The APS browser module registers the existing `aps` renderer implementation.
- Core dispatches a renderer bid through the registered implementation.
- An absent or rejecting renderer continues to fail closed without executing
  unvalidated creative code.

The APS module is included in the immediate script set whenever the compiled
auction plan can emit an APS renderer descriptor. Core is loaded first, so the
registration exists before application code can request and render bids.

The serialized renderer descriptor, APS validation rules, sandbox flags,
message authentication, timeouts, and render results remain unchanged. This is
not a general creative-renderer redesign; it is the minimum inversion needed
to remove the core-to-APS source import.

## Error Handling

Failures remain fail-closed and occur as early as the information permits.

Build-time failures include malformed directories, invalid manifests,
duplicate discovery metadata, missing JavaScript entry points, JavaScript build
failures, and missing generated bundles.

Startup or deploy-validation failures include duplicate integration IDs,
duplicate routes, invalid integration configuration, unresolved script assets,
and incompatible auction contributions.

Runtime hook errors retain the current `Report<TrustedServerError>` contexts
and response behavior. Moving a call behind the registry must not turn an
existing error into a log-and-continue path or a panic.

Core visibility changes must be narrow. A private helper moves with its
integration when possible. When an integration genuinely needs a core helper,
the implementation exposes the smallest named API and documents it. The change
must not broadly convert core modules or fields to `pub`.

## Compatibility

The following are compatibility requirements:

- Existing `trusted-server.toml` files require no migration.
- Integration IDs and enablement rules do not change.
- Registration and hook order do not change.
- Routes and endpoint behavior do not change.
- Immediate, deferred, and standalone delivery decisions do not change, except
  that APS becomes an explicit integration module required by its plan.
- Auction requests, responses, renderer descriptors, and telemetry do not
  change.
- Cache privacy and full-buffer decisions do not change.
- All four adapters expose the same routes and behaviors as before.

Bundle hashes and cache-busting URLs may change because browser core and
integration code are rebuilt in different crates. The server must always emit
URLs matching the newly generated hashes, so old and new artifacts cannot be
confused in cache. No stable bundle hash is part of the compatibility contract.

## Migration Sequence

Implementation may use small commits, but the merged workspace must never
contain two active built-in catalogs.

1. Establish the neutral script-module and lifecycle contracts in core without
   changing behavior.
2. Create `trusted-server-integrations-js`, move integration browser sources
   and tests, and remove concrete imports from browser core.
3. Create `trusted-server-integrations`, add directory discovery, and move all
   fifteen Rust implementation units.
4. Move concrete configuration validation and secret metadata into the
   generated catalog.
5. Rewire the CLI and all adapters to the shared composition entry point.
6. Delete the old concrete directory, fixed builder table, validation list,
   and concrete migration-guard inventory from core.

The final change is atomic from an operator's perspective. There is no dual
configuration or deprecation period because the configuration does not change.

## Testing and Verification

### Discovery tests

- Every valid Rust directory appears exactly once in generated output.
- Directory name, manifest ID, and generated module ID match.
- Missing `mod.rs`, missing manifests, duplicate order values, and malformed
  IDs fail generation.
- A required JavaScript directory without `index.ts` fails generation.
- JavaScript-only and Rust-only directories are accepted.
- Generated bundle hashes match embedded bytes.

### Parity tests

- The generated Rust catalog contains all fifteen current implementation IDs.
- Enabled registration IDs and order match pre-move behavior for representative
  settings.
- Immediate, deferred, standalone, and absent module selections match current
  behavior.
- Configuration validation accepts and rejects the same fixtures.
- Secret-field metadata remains equivalent.
- Route tables, hook order, and duplicate detection remain equivalent.
- DataDome response privacy and client-tag suppression remain equivalent.
- GPT diagnostics request preparation, bootstrap injection, finalization, and
  cache behavior remain equivalent on every adapter path.
- APS and Prebid plan compilation, transport, parsing, and auction results
  remain equivalent.
- The APS browser renderer passes its existing validation, sandbox, messaging,
  timeout, and rendering tests through the neutral renderer registration.

### Boundary guards

Automated source and dependency guards verify that:

- `trusted-server-core` has no dependency on either integrations crate.
- Browser core contains no import from an integration directory.
- Core has no concrete builder or deploy-validation inventory.
- Adapters and the CLI do not import concrete integration modules.
- The old `trusted-server-core/src/integrations` and
  `trusted-server-js/lib/src/integrations` directories no longer exist.

These guards target implementation coupling. They do not reject existing
domain or wire types merely because a stable type name contains `Aps`, `Gpt`,
or another integration name.

### Repository gates

Before handoff, run the full project gates from `AGENTS.md`, including:

- Rust formatting.
- All target-matched clippy aliases.
- Fastly, Axum, Cloudflare, and Spin tests.
- CLI tests and integration parity tests.
- Native and required WASM compilation for both new crates through their
  consumers.
- JavaScript builds, Vitest suites, and formatting for both browser crates.
- Documentation formatting.

## Risks and Mitigations

### Hidden reverse dependencies

Some concrete integrations use core-private helpers. Moving each module may
tempt broad visibility changes.

Mitigation: inventory each use, move integration-owned helpers outward, and
expose only the smallest unavoidable neutral core API. Treat new public surface
as a reviewed deliverable.

### Ordering drift

Filesystem iteration order must not decide runtime hook order.

Mitigation: require unique directory-local order values, sort generated output,
and pin parity with tests.

### Divergent validation paths

Runtime startup and CLI deploy validation could consume different catalogs.

Mitigation: both use the same generated integration definitions. There is no
secondary validation list.

### Stale JavaScript artifacts

Splitting the Node build can accidentally embed a previous bundle.

Mitigation: retain the current refusal to use stale output after a failed build,
generate hashes from the copied output bytes, and test every embedded hash.

### APS load-order regression

Moving APS renderer code out of browser core can leave the renderer unavailable
when an auction response arrives.

Mitigation: include APS immediately whenever its plan can emit the descriptor,
load core before integrations, and add end-to-end renderer-dispatch tests.

## Acceptance Criteria

The design is complete when all of the following are true:

1. Both new crates are workspace members and statically linked by every runtime
   adapter and the CLI where appropriate.
2. All fifteen current concrete Rust implementation units live under
   `trusted-server-integrations/src/<id>/`.
3. All integration browser sources, assets, fixtures, and tests live under
   `trusted-server-integrations-js`.
4. Rust and JavaScript directories are discovered without a central integration
   allowlist.
5. Core owns only neutral integration contracts and runtime execution.
6. Browser core imports no concrete integration.
7. APS, Prebid, and `adserver_mock` are not exceptions to the Rust move.
8. The CLI and all adapters use the shared static composition path.
9. Current configuration and runtime behavior pass parity tests.
10. The full repository verification gates pass.

## Deferred Work

The following require separate designs and real consumers:

- External vendor-owned crates.
- Runtime integration injection.
- Independent integration release and compatibility policies.
- Provider capability registration for identity, geo, device, permissions,
  demand, or ad servers.
- EdgeZero composition or host-service changes.
- Moving CLI audit detection metadata into integration directories.
