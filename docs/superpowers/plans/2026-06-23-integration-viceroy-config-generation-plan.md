# Integration Viceroy Config Generation Simplification Plan

**Date:** 2026-06-23
**Status:** Implemented
**Related work:** `docs/superpowers/plans/2026-06-16-edgezero-based-ts-cli-implementation-plan.md`

## Problem statement

The Trusted Server CLI blob-config cleanup made runtime settings load from the
`app_config` config store. The quickest CI fix seeded a serialized
`BlobEnvelope` into both integration Viceroy templates:

- `crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml`
- `crates/trusted-server-integration-tests/fixtures/configs/viceroy-template-edgezero.toml`

That fixed CI, but it is hard to maintain:

- The source of truth is an opaque generated JSON blob instead of readable
  Trusted Server TOML.
- The same app config appears in multiple templates, so updates can drift.
- Reviews become noisy because tiny settings changes rewrite a long single-line
  blob.
- The EdgeZero-specific template duplicates almost all of the base Viceroy
  template just to flip `trusted_server_config.edgezero_enabled`.
- The EdgeZero entry-point canary became brittle because it inferred routing path
  from runtime method behavior instead of an explicit runtime signal.

## Goals

- Keep one readable Trusted Server integration app-config fixture as the source
  of truth.
- Generate the Viceroy `app_config` blob fixture from that TOML, using the same
  Rust settings parsing and `BlobEnvelope` hashing code as production paths.
- Generate legacy and EdgeZero Viceroy configs from shared inputs instead of
  committing duplicate blob entries.
- Keep `edgezero_enabled` in `trusted_server_config`; do not move it into the
  Trusted Server app-config blob.
- Keep CI and local integration-test entry points explicit and easy to reproduce.
- Avoid adding production CLI surface area for test fixture generation.

## Non-goals

- Do not change Trusted Server runtime behavior.
- Do not change the operator-facing `ts config push` path.
- Do not introduce platform writes from the integration test harness.
- Do not rework the full integration test framework matrix.
- Do not add real customer/domain/credential data to fixtures.

## Proposed design

### Source files

Add one readable app-config fixture:

```text
crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml
```

This file should contain the same effective settings currently embedded in the
seeded blob:

- localhost/127.0.0.1 publisher origin for integration tests;
- placeholder-safe but non-default test secrets;
- the pre-seeded EC partners used by lifecycle tests;
- disabled optional integrations unless a scenario explicitly needs one;
- `proxy.certificate_check = false` for local Viceroy/origin wiring.

Keep one Viceroy base template focused on runtime resources:

```text
crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml
```

The base template should keep KV stores, secret stores, JWKS store, and any other
Viceroy-only resources. It should not carry a generated `app_config` blob.

Prefer deleting `viceroy-template-edgezero.toml` entirely. If keeping it is
safer for one PR, reduce it to a temporary compatibility fixture and remove the
blob from it; do not keep two copies of the same serialized app config.

### Fixture generator

Add a test-only Rust fixture generator under the integration-test crate, for
example:

```text
crates/trusted-server-integration-tests/src/bin/generate-viceroy-config.rs
```

Implemented CLI:

```bash
cargo run \
  --manifest-path crates/trusted-server-integration-tests/Cargo.toml \
  --target "$(rustc -vV | sed -n 's/^host: //p')" \
  --bin generate-viceroy-config -- \
  --template crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml \
  --app-config crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml \
  --output /tmp/integration-test-artifacts/configs/viceroy-legacy.toml \
  --edgezero-enabled false \
  --origin-url http://127.0.0.1:8888
```

`scripts/generate-integration-viceroy-configs.sh` wraps this and runs it twice:
once with `--edgezero-enabled false`, and once with `--edgezero-enabled true`.

Generator behavior:

1. Read the Viceroy base template.
2. Read `trusted-server.integration.toml`.
3. Parse through `trusted_server_core::settings::Settings::from_toml`.
4. Run `trusted_server_core::config::validate_settings_for_deploy` so broken
   fixtures fail before Viceroy starts.
5. Serialize settings to JSON and wrap them in
   `edgezero_core::blob_envelope::BlobEnvelope`.
6. Use a fixed `generated_at`, for example `2026-06-23T00:00:00Z`, so generated
   config files are deterministic.
7. Inject into the Viceroy template:

   ```toml
   [local_server.config_stores.app_config]
       format = "inline-toml"
   [local_server.config_stores.app_config.contents]
       app_config = '''{...BlobEnvelope JSON...}'''
   ```

8. Inject or update the rollout config store separately:

   ```toml
   [local_server.config_stores.trusted_server_config]
       format = "inline-toml"
   [local_server.config_stores.trusted_server_config.contents]
       edgezero_enabled = "true" # or "false"
   ```

9. Write the generated Viceroy config to the requested output path.

Keep the injector simple and deterministic. A practical implementation is to add
a marker comment to the template, such as:

```toml
    [local_server.config_stores]
        # GENERATED_TRUSTED_SERVER_CONFIG_STORES
```

Then replace only that marker with generated `app_config` and
`trusted_server_config` blocks. This avoids TOML round-tripping and preserves the
human-authored template formatting.

### CI flow

Generate Viceroy configs once in `prepare integration artifacts`, upload them
with the existing integration artifact bundle, then reuse them in downstream
jobs.

Proposed artifact layout:

```text
/tmp/integration-test-artifacts/
  wasm/trusted-server-adapter-fastly.wasm
  docker/test-images.tar
  configs/viceroy-legacy.toml
  configs/viceroy-edgezero.toml
```

Workflow changes:

1. In `prepare-artifacts`, after building the WASM binary, run the generator
   twice:
   - `--edgezero-enabled false` to produce `configs/viceroy-legacy.toml`;
   - `--edgezero-enabled true` to produce `configs/viceroy-edgezero.toml`.
2. Include `configs/**` in the `integration-test-artifacts` upload.
3. In the standard integration-test job, set:

   ```bash
   VICEROY_CONFIG_PATH=$ARTIFACTS_DIR/configs/viceroy-legacy.toml
   ```

4. In the EdgeZero integration-test job, set:

   ```bash
   VICEROY_CONFIG_PATH=$ARTIFACTS_DIR/configs/viceroy-edgezero.toml
   EXPECT_EDGEZERO_ENTRY_POINT=true
   ```

5. In the browser integration-test job, set `VICEROY_CONFIG_PATH` to the legacy
   generated config unless that job is intentionally exercising EdgeZero.

This keeps TypeScript/Playwright global setup unchanged except for consuming the
generated config path already provided by the workflow.

### Local developer flow

Update `scripts/integration-tests.sh` or the relevant local integration runner to
mirror CI:

1. Build the WASM binary.
2. Generate `target/integration-test-artifacts/configs/viceroy-legacy.toml`.
3. Generate `target/integration-test-artifacts/configs/viceroy-edgezero.toml`.
4. Run Rust and browser integration tests with the appropriate
   `VICEROY_CONFIG_PATH`.

If no local runner currently exists for a specific path, document the commands in
`crates/trusted-server-integration-tests/README.md`.

## Implementation stages

### Stage 1 — Extract readable app config fixture

1. Decode the currently committed blob only to confirm the intended settings.
2. Create `trusted-server.integration.toml` with those settings in readable TOML.
3. Verify locally that `Settings::from_toml` accepts it and
   `validate_settings_for_deploy` passes.
4. Keep all values fictional/test-only and localhost-oriented.

### Stage 2 — Add deterministic generator

1. Add the integration-test binary `generate-viceroy-config`.
2. Reuse production/core parsing and `BlobEnvelope`; do not duplicate hashing in
   shell, Python, or TypeScript.
3. Implement marker-based injection into the Viceroy template.
4. Add generator tests for:
   - generated config contains `app_config.app_config`;
   - generated config contains `edgezero_enabled = "true"` when requested;
   - generated config contains `edgezero_enabled = "false"` when requested;
   - generated blob verifies with `settings_from_config_blob`;
   - invalid app config fails fast with a useful error.

### Stage 3 — Simplify fixtures

1. Remove committed generated blob blocks from Viceroy templates.
2. Add the marker comment to the base Viceroy template.
3. Delete `viceroy-template-edgezero.toml`, or leave it as a temporary thin
   compatibility file only if removing it in one PR creates too much churn.
4. Ensure there is exactly one readable Trusted Server app-config fixture.

### Stage 4 — Wire CI artifact generation

1. Update `.github/workflows/integration-tests.yml` so `prepare-artifacts`
   generates both Viceroy configs.
2. Upload generated configs with the existing artifact bundle.
3. Update integration jobs to point at generated config artifact paths instead of
   source-controlled Viceroy templates.
4. Keep `EXPECT_EDGEZERO_ENTRY_POINT=true` only on the EdgeZero job.

### Stage 5 — Wire local scripts and docs

1. Update local integration-test scripts to call the generator.
2. Update `crates/trusted-server-integration-tests/README.md` with:
   - how to generate configs;
   - which generated config to use for legacy vs EdgeZero;
   - why the app-config blob is generated rather than committed.
3. Add a short comment in the Viceroy template at the marker explaining that the
   `app_config` and rollout stores are generated.

### Stage 6 — Revisit the EdgeZero probe

Short-term:

- Keep the current non-fatal probe if it is still useful diagnostic output.
- Do not rely on method-routing behavior as a required assertion.

Better follow-up:

- Add an explicit EdgeZero-only observable signal, such as a response extension
  surfaced as a debug header in integration mode, or a dedicated test-only route
  compiled only for integration builds.
- Once an explicit signal exists, make the EdgeZero CI job assert that signal and
  remove the heuristic probe.

## Definition of done

- The committed Viceroy templates no longer contain the large generated
  `BlobEnvelope` JSON blob.
- There is one readable Trusted Server integration app-config TOML fixture.
- CI generates and uploads both legacy and EdgeZero Viceroy configs.
- Rust integration tests, EdgeZero integration tests, and browser tests consume
  generated configs.
- Local integration-test docs/scripts can reproduce the generated configs.
- The generator is deterministic: repeated runs with unchanged inputs produce
  byte-identical outputs.
- `edgezero_enabled` remains in `trusted_server_config`.
- The standard CI checks pass.

## Verification checklist

Run locally before opening the cleanup PR:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
./scripts/check-integration-dependency-versions.sh
cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
```

Generate configs manually:

```bash
ARTIFACTS_DIR=/tmp/integration-test-artifacts \
INTEGRATION_ORIGIN_PORT=8888 \
./scripts/generate-integration-viceroy-configs.sh
```

Then run representative integration checks with the generated configs:

```bash
VICEROY_CONFIG_PATH=/tmp/integration-test-artifacts/configs/viceroy-legacy.toml \
WASM_BINARY_PATH=target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm \
INTEGRATION_ORIGIN_PORT=8888 \
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml \
  --target $(rustc -vV | sed -n 's/^host: //p') \
  test_ec_lifecycle_fastly -- --include-ignored --test-threads=1

VICEROY_CONFIG_PATH=/tmp/integration-test-artifacts/configs/viceroy-edgezero.toml \
EXPECT_EDGEZERO_ENTRY_POINT=true \
WASM_BINARY_PATH=target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm \
INTEGRATION_ORIGIN_PORT=8888 \
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml \
  --target $(rustc -vV | sed -n 's/^host: //p') \
  test_ec_lifecycle_fastly -- --include-ignored --test-threads=1
```

Finally, push and watch GitHub checks until all integration jobs pass.

## Risks and mitigations

- **Generator becomes another custom config path.** Keep it test-only under the
  integration-test crate; do not expose it through `ts`.
- **Generated config is not available to browser tests.** Generate configs in the
  shared prepare-artifacts job and upload them alongside WASM/Docker artifacts.
- **Blob hash drift from timestamps.** Use a fixed `generated_at` for fixtures.
- **Template injection accidentally corrupts TOML.** Use a single explicit marker
  and unit-test the generated TOML by parsing it.
- **Settings fixture drifts from runtime needs.** Parse with core `Settings` and
  run the same validation used by runtime/CLI paths.
- **EdgeZero rollout flag moves into app config by accident.** Keep generation
  code paths separate: `app_config` blob for settings, `trusted_server_config`
  block for rollout.
