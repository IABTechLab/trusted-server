# EdgeZero-Based Trusted Server CLI Implementation Plan

**Date:** 2026-06-16
**Status:** Revised for blob app-config
**Spec:** `docs/superpowers/specs/2026-06-16-edgezero-based-ts-cli-design.md`

## Decisions locked for this plan

- Trusted Server app config is pushed as a **single blob envelope**, not as
  flattened per-setting entries. Fastly config-store entry/value limits make the
  flattened model unsafe for full Trusted Server configs.
- Platform lifecycle and platform writes stay inside EdgeZero. Trusted Server may
  validate and initialize app config, but it must not implement Fastly/Wrangler/
  Spin writes or adapter resolution.
- Literal secrets that still live in `Settings` are allowed to be included in the
  blob envelope for v1. Secret-store write primitives are a future EdgeZero
  coordination item.
- EdgeZero app-config env overlays stay enabled by default. Operators can pass
  `--no-env` for file-only validation/push.
- `edgezero_enabled` rollout behavior stays as it was before this PR: the flag
  remains in the existing Fastly `trusted_server_config` config store and is not
  part of the `app_config` blob.
- `ts config init` remains Trusted Server-owned because it copies the
  product-specific example template.

## Definition of done

- `ts` binary exists and implements the spec command surface.
- Lifecycle commands are thin direct calls to EdgeZero CLI library APIs.
- `ts config init` copies `trusted-server.example.toml` and is tested.
- `ts config validate` and `ts config push` call EdgeZero typed blob APIs with a
  Trusted Server-owned app-config wrapper.
- Trusted Server deploy-time validation is centralized in core.
- Runtime loading verifies `BlobEnvelope` integrity before constructing
  `Settings`.
- `trusted-server.toml` is operator-owned and ignored.
- No Trusted Server CLI code performs direct platform provisioning, adapter
  registry lookup, config-store writes, or shell command construction.
- Repository docs and verification commands are updated.

## Stage 1 — EdgeZero blob baseline

1. Keep the repository pinned to the EdgeZero revision that provides:
   - typed downstream CLI args;
   - `run_config_validate_typed::<C>`;
   - `run_config_push_typed::<C>`;
   - `BlobEnvelope` app-config model;
   - adapter-owned Fastly chunking for large config values.
2. Confirm `edgezero.toml` declares `app_config` as the default config store.
3. Confirm `trusted-server.toml` is ignored and `trusted-server.example.toml` is
   source-controlled.

## Stage 2 — Core app-config wrapper

1. Add `crates/trusted-server-core/src/config.rs`.
2. Define `TrustedServerAppConfig` as a wrapper around `Settings` that:
   - deserializes from the same top-level TOML shape;
   - serializes to the same JSON shape;
   - implements EdgeZero app-config metadata;
   - implements `validator::Validate` by running Trusted Server deploy-time
     validation.
3. Move CLI-only validation into core:
   - placeholder/default secret rejection;
   - enabled integration startup checks;
   - auction provider reference checks;
   - EC partner registry checks.
4. Keep `Settings` runtime preparation/finalization shared so EdgeZero's typed
   loader and the runtime loader do not drift.
5. Add tests for wrapper serialization/deserialization and deploy validation.

## Stage 3 — Thin CLI structure

1. Replace custom Trusted Server lifecycle args and dispatch with EdgeZero args:
   - `AuthArgs`;
   - `BuildArgs`;
   - `DeployArgs`;
   - `ProvisionArgs`;
   - `ServeArgs`;
   - `ConfigValidateArgs`;
   - `ConfigPushArgs`.
2. Delete Trusted Server-owned adapter/push plumbing:
   - custom manifest loading;
   - `edgezero_adapter::registry` imports;
   - `AdapterPushContext` construction;
   - direct `push_config_entries` calls;
   - shell command construction/escaping.
3. Keep only a small `config init` module with:
   - `--app-config <path>`;
   - `--config <path>` compatibility alias;
   - `--force`.
4. Route commands directly:

```rust
edgezero_cli::run_auth(&args)
edgezero_cli::run_build(&args)
edgezero_cli::run_config_validate_typed::<TrustedServerAppConfig>(&args)
edgezero_cli::run_config_push_typed::<TrustedServerAppConfig>(&args)
edgezero_cli::run_deploy(&args)
edgezero_cli::run_provision(&args)
edgezero_cli::run_serve(&args)
```

## Stage 4 — Runtime blob loading

1. Keep runtime loading focused on:
   - read logical blob entry;
   - reconstruct adapter chunk pointer when applicable;
   - verify `BlobEnvelope`;
   - deserialize `Settings`;
   - reject placeholders.
2. Avoid adding any config-store write behavior to Trusted Server runtime code.
3. Preserve legacy-vs-EdgeZero rollout behavior:
   - `edgezero_enabled` stays in `trusted_server_config`;
   - `app_config` stores the Trusted Server settings blob.

## Stage 5 — Documentation

1. Update the spec from flattened entries to blob envelope.
2. Update CLI docs:
   - `--app-config` is the config path flag for validate/push;
   - `--config` remains an init alias;
   - env overlays are enabled unless `--no-env` is passed;
   - config push writes a blob envelope.
3. Update `CLAUDE.md` and guide pages if command names or verification commands
   change.

## Stage 6 — Verification

Run at minimum:

```bash
cargo fmt --all -- --check
cargo test --package trusted-server-cli --target $(rustc -vV | sed -n 's/^host: //p')
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
```

If docs change:

```bash
cd docs && npm run format
```

## Risks and watch points

- `TrustedServerAppConfig` must preserve the exact `Settings` JSON shape so
  runtime reconstruction remains straightforward.
- EdgeZero env overlays can affect pushed blob hashes. This is accepted, but
  docs must mention `--no-env` for file-only operation.
- `edgezero_enabled` must not accidentally move into `app_config`; that would
  expand the PR scope.
- Fastly chunk pointer handling should remain read-only runtime behavior and not
  grow into Trusted Server-owned platform write logic.
