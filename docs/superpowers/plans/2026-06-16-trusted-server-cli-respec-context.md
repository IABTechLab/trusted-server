# Trusted Server CLI Respec Context

**Date:** 2026-06-16
**Status:** Research artifact, not a spec
**Purpose:** Capture context from the earlier Trusted Server CLI implementation, the existing Trusted Server CLI draft spec, and EdgeZero PR #269 so the new Trusted Server CLI specs can be cut cleanly.

## Sources reviewed

- Local branch `feature/ts-cli`
  - `crates/trusted-server-cli/`
  - `docs/guide/cli.md`
  - `docs/guide/fastly-provisioning.md`
  - `docs/superpowers/specs/2026-04-20-config-store-runtime-config-design.md`
- Local branch `spec/ts-cli`
  - `docs/superpowers/specs/2026-04-23-trusted-server-cli-design.md`
- EdgeZero PR #269 at head `2eeccc9748daba92b9adf6afe4df105e79269ae9`
  - PR summary and file list via GitHub API
  - `docs/superpowers/specs/2026-05-19-cli-extensions-design.md`
  - `docs/superpowers/specs/2026-06-01-spin-kv-config.md`
  - representative implementation files under `crates/edgezero-cli/`, `crates/edgezero-adapter/`, and `crates/edgezero-core/`
- Current Trusted Server branch `feature/ts-cli-next`
  - currently equal to `main`; no `trusted-server-cli` crate present
  - still uses build-time embedded config via `settings_data.rs` / `build.rs`
  - already has EdgeZero-derived core HTTP/body/platform abstractions and Fastly `PlatformConfigStore` / `PlatformSecretStore` / KV plumbing

## What the old Trusted Server CLI actually implemented

### Crate and binary

- Added `crates/trusted-server-cli`.
- Binary name: `ts`.
- `main.rs` was a thin wrapper over `trusted_server_cli::run()`.
- Used `clap`, `error-stack`, `dialoguer`, `keyring`, `reqwest::blocking`, `chromiumoxide`, `scraper`, and `tokio` for host-only CLI behavior.
- Added host-target Cargo aliases because the workspace default target is `wasm32-wasip1`.

### Command surface

```text
ts config init
ts config validate [--json]
ts audit <url>
ts dev [-a fastly]
ts auth fastly login|status|logout
ts provision fastly plan|apply
```

### Config model

- `trusted-server.toml` remained the authoring file.
- `trusted-server.example.toml` became the tracked template; `trusted-server.toml` was gitignored.
- The CLI split `[providers]` out of the source TOML before canonicalizing runtime app config.
- Runtime app config was canonical TOML stored under fixed key `ts-config` in fixed runtime alias `ts_config_store`.
- Provider config did not affect the canonical config hash.

### Runtime config-store change

`feature/ts-cli` also implemented the runtime config architecture:

- deleted `settings_data.rs` and made `build.rs` a no-op;
- added `trusted_server_core::runtime_config` for strict parse, validation, canonical TOML, and hash;
- changed Fastly startup to read `ts_config_store` / `ts-config` via `RuntimeServices.config_store()` before routing;
- made `/health` depend on successful runtime config loading.

Current `feature/ts-cli-next` does **not** have this runtime config-store behavior yet; it still embeds config at build time.

### Fastly provisioning model

The old CLI did direct Fastly API orchestration, not native CLI delegation:

- credential resolution: `FASTLY_API_KEY` first, then OS keyring via `ts auth fastly login`;
- `plan` inspected service versions, active/latest versions, stores, items, and resource links;
- `apply` created or reused stores, wrote config items/secrets, created or updated resource links, cloned locked service versions if needed, and activated when bindings changed;
- app config store was always managed;
- request signing resources were managed when enabled;
- consent KV store was managed when configured;
- apply was non-destructive, idempotent, and fail-fast;
- JSON output included completed actions and failed action on partial failure.

### Audit model

`ts audit` was Trusted-Server-specific and not covered by EdgeZero:

- launched Chrome/Chromium via `chromiumoxide`;
- collected script tags and resource timing entries;
- detected integrations by URL/inline evidence;
- wrote `js-assets.toml` and a draft `trusted-server.toml`;
- refused overwrites unless `--force`.

## Existing Trusted Server draft spec vs implementation

`spec/ts-cli` contains `2026-04-23-trusted-server-cli-design.md`. It matches the old implementation at a high level, but the implementation moved beyond it in several ways:

- Spec said `--service-id` was required for provisioning; implementation resolved service ID from CLI, `[providers.fastly].service_id`, then `fastly.toml`.
- Spec kept Fastly resource identity as an open question; implementation chose fixed runtime aliases plus configurable underlying resource names.
- Spec did not fully separate runtime config-store architecture into its own CLI-dependent implementation details; implementation did.
- Spec did not deeply specify request-signing bootstrap/runtime API token behavior; implementation did.
- Spec did not anticipate EdgeZero PR #269's manifest/app-config split or adapter registry design.

## EdgeZero PR #269 patterns worth borrowing

### CLI as a reusable library

EdgeZero turned `edgezero-cli` into a library-first crate:

- `pub mod args` exposes `*Args` structs;
- root-level `run_*` functions implement built-ins;
- default binary is a thin dispatcher;
- downstream app CLIs can reuse built-ins and wire typed config functions.

Trusted Server can borrow this if we want publisher-specific or deployment-specific wrappers later. If not, we can still borrow the thin-main / testable-runner shape.

### Adapter-owned dispatch

EdgeZero centralizes adapter discovery in `edgezero-adapter::registry::Adapter`:

- CLI dispatches `build`, `deploy`, `serve`, `auth`, `provision`, and `config push` to registered adapters;
- adapter crates own platform details;
- CLI avoids hard-coded adapter-specific branches where possible;
- adapter trait also owns validation hooks for platform-specific manifest/config constraints.

Trusted Server currently has only Fastly in-tree, but the EdgeZero migration plan expects Axum/Cloudflare later. We should decide whether the new `ts` CLI starts with a small Trusted Server adapter trait now, or keeps Fastly-specific command trees and extracts a trait when the second adapter lands.

### Manifest + typed app config split

EdgeZero uses:

- `edgezero.toml`: portable app/trigger/store/adapters manifest;
- `<app-name>.toml`: typed per-service app config;
- `EDGEZERO__STORES__<KIND>__<ID>__NAME`: runtime platform-name overlay.

The earlier Trusted Server CLI used one `trusted-server.toml` containing app config plus `[providers]` deployment config, then stripped `[providers]` before canonicalization.

This is the biggest respec decision: keep the single Trusted Server file for operator simplicity, or split runtime app config from provider/platform manifest like EdgeZero.

### Store model

EdgeZero moved to logical store IDs:

```toml
[stores.config]
ids = ["app_config"]

[stores.secrets]
ids = ["default"]
```

Rules:

- logical ids are portable;
- platform names resolve from env overlay, defaulting to the logical id;
- single-store adapters reject multiple ids for unsupported store kinds;
- legacy schema is a hard load error;
- store ids are validated for portability and env-var safety.

Trusted Server's old implementation used fixed runtime aliases (`ts_config_store`, `jwks_store`, `signing_keys`, `api-keys`) and configurable Fastly underlying resource names under `[providers.fastly]`. A respec should either retain that TS-specific alias model or translate it into logical store declarations.

### `config validate` and `config push`

EdgeZero separates:

- `config validate`: local app config + manifest validation;
- `provision`: create/bind platform resources;
- `config push`: push app config entries to config store.

Old Trusted Server combined config upload into `provision apply`. Splitting `config push` out would align with EdgeZero and reduce provisioning blast radius, but may add one more operator command.

### Spin KV follow-up

The original EdgeZero CLI spec treated Spin config as flat variables. The later `2026-06-01-spin-kv-config` plan changes Spin config to KV-backed multi-store config with local/cloud push paths. For Trusted Server, this matters mainly as a warning: avoid baking in a config-store model that assumes all adapters look like Fastly Config Store. Future adapters may need backend-specific config push behavior.

## Suggested new spec set

Instead of one giant CLI spec, cut smaller specs with explicit dependencies:

1. **Trusted Server CLI v1 substrate and UX**
   - crate/binary, command tree, output, exit codes, host-target build, thin main/testable run functions;
   - decide whether `ts` is library-extensible like EdgeZero.

2. **Runtime application config store**
   - remove build-time embed;
   - canonical TOML + hash;
   - production config store key/alias contract;
   - local development projection;
   - health/fail-closed behavior.

3. **Trusted Server config and provider manifest model**
   - decide monolithic `trusted-server.toml` + `[providers]` vs split app config + platform manifest;
   - define store logical IDs, fixed aliases, provider resource names/IDs, and env overlays.

4. **Fastly auth and provisioning**
   - credential source policy;
   - direct Fastly API vs native CLI delegation;
   - plan/apply semantics;
   - request-signing bootstrap;
   - service-version cloning/activation;
   - JSON schemas.

5. **Config push / deploy config workflow**
   - if split from provisioning: `ts config push --adapter fastly`;
   - if not split: define why `provision apply` owns config upload;
   - dry-run and idempotency behavior.

6. **Local development / serve**
   - `ts dev` vs `ts serve` naming;
   - Fastly Viceroy local config-store projection;
   - passthrough args and `--skip-build` behavior;
   - future Axum adapter path.

7. **Audit and config bootstrap**
   - browser collector scope;
   - integration detection;
   - generated files;
   - limits and future authenticated audit.

## High-priority decisions before writing the new spec

1. **File model:** keep one `trusted-server.toml` with `[providers]`, or move toward EdgeZero's manifest + app-config split?
2. **Store identity:** keep fixed runtime aliases plus provider resource names, or introduce logical store ids with platform-name env overlays?
3. **Provision vs push:** should config upload remain in `ts provision fastly apply`, or become `ts config push --adapter fastly`?
4. **Auth strategy:** keep OS keyring + direct Fastly API, or delegate to native Fastly CLI profiles like EdgeZero?
5. **Extensibility:** does `trusted-server-cli` need to be a reusable library for downstream/custom CLIs?
6. **Naming:** keep `ts dev`, rename to `ts serve`, or support both with one canonical name?
7. **Runtime health:** should `/health` require valid runtime config (old CLI branch) or stay config-independent (current branch)?
8. **Scope of v1:** runtime config-store migration and Fastly provisioning were coupled in `feature/ts-cli`; should they remain coupled or ship as separate specs/PRs?

## Working recommendation

For the next spec pass, start from Trusted Server's operator workflow, not EdgeZero's framework workflow:

- keep `ts` as the product CLI;
- preserve `trusted-server.toml` as the operator-facing app config unless we deliberately choose a split;
- borrow EdgeZero's library-first runner shape and adapter-owned validation hooks;
- split `config push` from `provision apply` unless the team strongly prefers one-step Fastly provisioning;
- keep direct Fastly API provisioning because Trusted Server needs precise resource-link, config item, secret, and key-bootstrap behavior that EdgeZero intentionally avoided by delegating to native CLIs;
- write runtime config-store as its own prerequisite spec so the CLI can reference a stable config deployment contract.
