# Runtime Config Store Architecture for `trusted-server.toml`

> **Status:** Proposal
> **Scope:** Config store architecture only
> **Date:** April 2026

## Summary

This document specifies the application configuration changes required to move
Trusted Server from **build-time embedded configuration** to **runtime-loaded
configuration**.

Today, Trusted Server reads `trusted-server.toml` during the build, merges
`TRUSTED_SERVER__*` environment variable overrides, writes a generated TOML
file to `target/trusted-server-out.toml`, and embeds that generated file into
the WASM binary with `include_bytes!`.

This proposal replaces that model with the following target architecture:

- **Production** loads application config from a platform config store at runtime
- **Development** uses a local TOML-authored workflow rooted at the repo root by default
- The platform config store uses a **fixed key**: `ts-config`
- The stored payload is **canonical TOML** representing the application config
- The config schema remains substantially the same as the existing
  `trusted-server.toml` schema
- Build-time embedding and build-time environment-variable merging for
  application settings are removed

The goal is to make `trusted-server.toml` remain the single application config
source of truth while changing **how it is deployed and loaded**.

This document is intentionally limited to config-store behavior and does not
specify attestation signatures, discovery endpoints, or a full CLI design.

## Scope

### In scope

- Production runtime loading of application config from a platform config store
- Development loading of application config from a local TOML file
- Bootstrap contract for locating the platform config store in production
- Deterministic validation and canonicalization of application config
- Config hashing based on canonical TOML bytes
- Repository file ownership changes related to `trusted-server.toml`
- Minimal tooling responsibilities required to deploy config to the store
- Explicit removal of the current build-time merge-and-embed model

### Out of scope

- Config signatures, DSSE, or signature verification
- Runtime attestation endpoints or statement formats
- Full CLI command design or operator UX
- Hot reload, file watching, or runtime config refresh
- Runtime mutation of application config
- Broad config schema redesign
- Migration rollout sequencing or temporary dual-source compatibility
- Platform-specific implementation details beyond the abstract store contract

## Current state

Trusted Server currently treats application config as a **build input**.

The current flow is:

1. Read root `trusted-server.toml`
2. Merge `TRUSTED_SERVER__*` environment variable overrides at build time
3. Serialize the merged config to `target/trusted-server-out.toml`
4. Embed that generated file into the WASM binary with `include_bytes!`
5. Parse the embedded TOML at runtime into `Settings`

In the current codebase, this behavior is centered in:

- `crates/trusted-server-core/build.rs`
- `crates/trusted-server-core/src/settings_data.rs`

This means the deployed binary contains both:

- application code
- publisher/operator configuration

## Problems with the current state

### Code and config are tightly coupled

A config-only change produces a different binary artifact. That makes it harder
to reason about code provenance separately from config changes.

### Config changes require a rebuild

Operators cannot update configuration independently from the WASM build.
Changing runtime behavior requires regenerating and redeploying the binary.

### Build-time environment merging weakens the source-of-truth model

The current model combines a TOML file with build-time environment variables.
That makes the effective config less obvious and weakens the idea of a single,
authoritative application config document.

### Repository ownership is wrong for operator config

A tracked root `trusted-server.toml` suggests application config should live in
source control as a committed repository artifact. In practice, application
config is operator-owned and deployment-specific.

### Runtime behavior depends on build tooling decisions

Because config is preprocessed during the build, the runtime is not the
authoritative point where config loading and validity are determined.

## Goals

- Keep **one application config document**: `trusted-server.toml`
- Make **production** load application config from a platform config store
- Make **development** load application config from a local TOML file
- Remove build-time embedding of application settings into the WASM binary
- Remove build-time `TRUSTED_SERVER__*` application-setting merges
- Define a deterministic canonical TOML representation suitable for hashing
- Preserve the existing application config schema as much as possible
- Keep the architecture generic enough to work with a future cross-platform
  config store abstraction

## Non-goals

- Designing config signing or signature verification
- Designing a runtime attestation document
- Supporting multiple application config sources simultaneously in production
- Supporting hidden fallback from one source to another after source selection
- Supporting runtime writeback of application config
- Preserving comments or operator formatting in stored canonical payloads
- Supporting unknown fields in application config
- Introducing broad schema cleanup unrelated to config-store loading

## Target architecture

### High-level model

`trusted-server.toml` remains the single application configuration document.
What changes is how that document is sourced:

- **Production:** platform config store
- **Development:** local file at repo root by default

The authoritative production payload is stored in the platform config store under
key `ts-config`.

The value stored under `ts-config` is canonical TOML representing the application
config document.

### Production behavior

In production, Trusted Server:

1. Obtains a platform config store reference from deployment/bootstrap wiring
2. Reads the fixed key `ts-config`
3. Parses the TOML payload using the existing application config schema
4. Rejects unknown fields
5. Validates semantic config rules
6. Produces a valid immutable `Settings` snapshot for request handling
7. Derives canonical TOML bytes and a config hash from that valid config

Production application behavior must treat the platform config store payload as
authoritative.

### Development behavior

In development, `trusted-server.toml` remains the default local authoring file
at the repository root.

A flag may be used to choose a different TOML file path.

The local TOML file is the development authoring source of truth. Runtime
consumption may happen either directly from that file or via a
platform-specific projection step, depending on platform constraints.

The development pipeline is:

1. Load `trusted-server.toml` from the repository root by default, or the
   explicitly selected TOML file
2. Parse the TOML payload using the existing application config schema
3. Reject unknown fields according to the rules defined in this document
4. Validate semantic config rules
5. Derive canonical TOML bytes and a config hash from that valid config
6. Project the canonical TOML into the local development runtime in the
   platform-appropriate way when needed
7. Produce a valid immutable `Settings` snapshot for request handling

On platforms such as Fastly/Viceroy, the preferred local-development approach is
to populate the local simulated config store with the canonical payload under
`ts-config` before request handling, rather than relying on direct host-file
reads from within the WASM guest.

When running in development, tooling or runtime logs should identify which local
TOML file path was loaded.

Development loading does **not** automatically rewrite the source file into
canonical form.

### Request-level semantics

Each request must be handled against **one valid, internally consistent,
immutable `Settings` snapshot**.

This document intentionally does **not** require a specific fetch or caching
strategy. An implementation may fetch fresh config for each request or reuse
previously loaded state, as long as:

- each request sees one coherent snapshot
- invalid or partially loaded config is never used
- correctness does not depend on cross-request in-memory persistence

Because platform lifecycle behavior varies, the architecture must not assume that
in-memory state survives across requests.

## Bootstrap contract

Production requires a minimal bootstrap mechanism to locate the platform config
store. That bootstrap mechanism is **not** part of the application config schema.

Bootstrap is deployment plumbing only.

### Bootstrap responsibilities

Production bootstrap must provide:

- the platform config store reference needed to open/read the store

Production bootstrap must **not**:

- inject application settings values
- override application settings values
- provide an alternate key name for the application config payload
- create a secondary source of truth for runtime application behavior

### Fixed key name

The application config payload key is fixed globally:

- `ts-config`

The runtime always reads `ts-config` once the production store reference has been
resolved.

The key name is not configurable.

## Config loading and canonicalization pipeline

This section defines the semantic pipeline for both production and development.

### Step 1: Select the source by environment

- **Production** uses the platform config store
- **Development** uses a local TOML-authored workflow rooted at
  `trusted-server.toml` in the repository root by default, with a flag to
  choose a different file

This document does not define a separate first-class runtime `config_source`
mode. It defines source behavior in terms of production vs. development.

### Step 2: Load the payload

The selected source must yield a UTF-8 TOML payload.

- Production loads the payload from store key `ts-config`
- Development loads the payload from the selected local TOML file path and may
  then project the canonicalized result into a local platform-simulated config
  store before runtime consumption

### Step 3: Parse strictly using the existing schema

The payload is parsed as the existing application configuration schema used to
produce `Settings`.

This spec is schema-preserving by default. It does not redesign the
`trusted-server.toml` structure beyond what is necessary to support the new
loading model.

### Step 4: Reject unknown fields

Unknown fields are rejected.

The system must not silently preserve, ignore, or drop unsupported keys.

This applies to strongly typed configuration sections in the application config
schema. The `integrations` section continues to follow the existing integration
configuration model, where integration IDs are discovered dynamically and each
integration's typed validation rules govern the contents of its own config.

### Step 5: Validate semantic constraints

After parsing, the config must satisfy existing semantic validation rules.

Examples include:

- required fields must be present
- invalid field combinations must fail
- invalid regexes, store identifiers, or route coverage rules must fail

### Step 6: Define the canonical TOML representation

Valid config has a deterministic canonical TOML representation.

Canonicalization is part of the config pipeline semantics, not merely a tooling
implementation detail.

Canonicalization is defined as a dedicated transformation over valid config. It
must not rely on whatever output happens to fall out of naive derived
serialization alone. Implementations may need explicit canonicalization logic to
ensure the required output properties.

Canonicalization is defined as:

- parse valid config through the typed config model
- serialize it in a deterministic TOML form
- include explicitly declared settings only
- do **not** expand the config into a full dump of all effective defaulted runtime values
- define stable ordering for map-like structures so identical semantic config
  produces identical canonical bytes

As a consequence:

- comments are not preserved in canonical form
- original formatting is not preserved in canonical form
- canonical stored payloads are intended to be tight and deterministic
- additional implementation work may be required beyond current derived
  `Serialize` behavior to satisfy these guarantees

### Step 7: Compute the config hash

The config hash is computed over the canonical TOML bytes.

This provides a stable hashable representation of application config suitable
for observability and future attestation work.

### Step 8: Produce the runtime snapshot

The runtime uses the validated config to produce the `Settings` snapshot used by
request handling.

Implementations may materialize canonical bytes eagerly or lazily, but the
canonical form is part of the defined semantics.

## Failure behavior

The selected config source must produce one valid `Settings` snapshot.

If loading, parsing, or validation fails, the runtime must **fail closed**.

That includes failures such as:

- missing store/bootstrap reference in production
- inability to read the selected store or file
- missing `ts-config` key in production
- invalid UTF-8 payloads
- malformed TOML
- unknown fields
- missing required fields
- semantic validation failures

### No fallback after source selection

Once the source has been selected by environment, the runtime must not fall back
to another source.

Examples of disallowed behavior:

- production store mode falling back to a local file
- development file mode falling back to embedded config
- falling back to a previously cached last-known-good config
- loading partial config and continuing with defaults beyond normal schema behavior

### Availability behavior

A config failure means the service is not healthy for serving application
traffic.

This spec does not define a special "healthy but unusable" mode for config
failure.

## Repository and file ownership changes

The repository layout should change to reflect the new ownership model.

### Required changes

- Remove tracked `trusted-server.toml` from source control
- Add `trusted-server.toml` to `.gitignore`
- Add or retain `trusted-server.example.toml` as a tracked template file

### File roles

#### `trusted-server.toml`

- operator-owned local/deployment artifact
- default local authoring file for development
- not tracked in git

#### `trusted-server.example.toml`

- tracked template file
- kept in sync with currently supported configuration features
- intended to help operators create a real `trusted-server.toml`

## Minimal tooling contract

This document does not define a full CLI specification.

It does define the minimum tooling responsibilities required by the target
architecture.

Tooling responsible for publishing production config must be able to:

1. Load a local TOML file
2. Parse it using the application config schema
3. Reject unknown fields
4. Validate semantic config rules
5. Produce canonical TOML
6. Compute a hash over canonical TOML bytes
7. Write the canonical TOML payload to the platform config store under `ts-config`

For local platform simulators such as `fastly compute serve`, tooling may also
materialize that canonical payload into the simulator's local config-store input
before starting the runtime.

Tooling may support additional commands later, such as:

- pull
- diff
- inspect
- dry-run deployment

Those capabilities are explicitly out of scope for this document.

## Hashing

Config hashing is part of this architecture because it depends on deterministic
canonicalization.

### Hash source

The config hash is computed over the canonical TOML bytes.

Because the hash is derived from canonical bytes, canonicalization must produce
stable field and map ordering for semantically identical config.

### Purpose

The config hash exists to support:

- observability
- config comparison
- deterministic deployment artifacts
- future attestation and provenance work

### Out of scope

This document does not define:

- signature formats
- signed envelopes
- signature verification behavior
- runtime signature enforcement

## Explicit removals from the current design

This proposal explicitly removes the current application-config build pipeline.

### Removed behaviors

- build-time embedding of application config into the WASM binary
- build-time generation of `target/trusted-server-out.toml` as the runtime app-config source
- build-time merging of `TRUSTED_SERVER__*` application-setting overrides
- production dependence on a repository-tracked `trusted-server.toml`
- runtime mutation of application config

### Resulting source-of-truth model

After this change:

- `trusted-server.toml` remains the canonical application config document
- in production, the authoritative deployed copy is the platform-store payload under `ts-config`
- in development, the authoritative copy is the selected local TOML file

## Implementation notes

These notes are informative, not additional scope.

- The concrete platform config-store API may still be evolving while this is
  implemented
- Existing generic config-store abstractions in the codebase may be reused as
  they mature
- Runtime caching strategy is intentionally unspecified by this document
- The settings schema should remain substantially unchanged except where minor
  adjustments are necessary to support strict parsing or canonicalization

## Future work enabled by this design

This design is intended to enable, but not itself specify:

- config attestation based on canonical payload hashes
- runtime reporting of config hash metadata
- richer deployment tooling around validation, diffing, and inspection
- broader cross-platform config-store support behind a generic API

## Recommended next step

After agreeing on this architecture, a follow-up spec should define the concrete
operator tooling used to:

- validate local config
- canonicalize it
- compute hashes
- publish canonical TOML to the platform config store
- support development ergonomics around local file selection
- project local authored config into platform-specific local runtime inputs when
  direct runtime file access is not available
