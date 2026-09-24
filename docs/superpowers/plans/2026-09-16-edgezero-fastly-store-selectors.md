# EdgeZero Fastly Store Selector Alignment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore Trusted Server staging deployments by consuming EdgeZero PR 381's canonical Fastly store-selector behavior and removing the superseded service-scoped configuration.

**Architecture:** Pin every EdgeZero workspace dependency to the upstream PR branch so the CLI and adapters share one revision. Keep Trusted Server's strict runtime secret resolution, while moving the Fastly entry point, local Viceroy configuration, its regression test, and operator guidance to the logical-ID store model: EdgeZero links each physical store selected by a canonical `EDGEZERO__STORES__...` deployment selector under its logical ID, and the runtime opens stores by that ID.

**Tech Stack:** Rust 1.95, Cargo Git dependencies, Fastly Compute/Viceroy TOML configuration, Markdown documentation, GitHub CLI.

---

## File Map

- `Cargo.toml`: select the EdgeZero PR 381 branch for all EdgeZero crates.
- `Cargo.lock`: lock all EdgeZero packages to the branch revision.
- `crates/trusted-server-adapter-fastly/src/main.rs`, `crates/trusted-server-adapter-fastly/src/app.rs`: open stores and the config entry by logical ID instead of reading `edgezero_runtime_env`.
- `fastly.toml`: expose the local Viceroy secret store under its logical ID and drop the runtime selector store.
- `crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml`, `scripts/template-cache-local-test.sh`: keep generated Viceroy configurations aligned with the root Fastly configuration.
- `crates/trusted-server-integration-tests/tests/common/config.rs`: regress the logical-ID store exposure in both Viceroy configurations.
- `docs/guide/fastly.md`: explain deploy-time canonical selector handling and physical store linking.
- `docs/guide/cli.md`: explain staging Config Store selection under the restored EdgeZero deployment model.
- `docs/superpowers/specs/2026-09-16-edgezero-fastly-store-selectors-design.md`: approved design, already committed.
- `docs/superpowers/plans/2026-09-16-edgezero-fastly-store-selectors.md`: this execution plan.

### Task 1: Create the Tracking Issue

**Files:**

- No repository files.

- [ ] **Step 1: Draft the issue**

Use a bug title such as `Align Fastly staging store selectors with EdgeZero PR 381` and include:

- the observed `publisher.proxy_secret` resolution failure;
- the impact that staging cannot build application state;
- the root cause that EdgeZero `v0.0.8` reads service-scoped selectors while the selected GitHub Environment supplies canonical selectors;
- the expected canonical key `EDGEZERO__STORES__SECRETS__TRUSTED_SERVER_SECRETS__NAME=ts_secrets`;
- the dependency on `https://github.com/stackpop/edgezero/pull/381`;
- acceptance criteria covering the branch dependency, Viceroy fixtures, documentation, and validation.

- [ ] **Step 2: Create the issue and record its number**

Run:

```bash
gh issue create --repo IABTechLab/trusted-server --title "Align Fastly staging store selectors with EdgeZero PR 381" --label bug --body-file <temporary-body-file>
```

Expected: GitHub prints the new issue URL. Save its issue number for the pull request body.

### Task 2: Expose Viceroy Stores Under Their Logical IDs

**Files:**

- Modify: `crates/trusted-server-integration-tests/tests/common/config.rs:48`
- Modify: `fastly.toml:56`
- Modify: `crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml:69`
- Modify: `scripts/template-cache-local-test.sh:372`

- [ ] **Step 1: Change the regression test first**

Replace `local_fastly_secret_store_mapping_is_service_scoped` with
`local_fastly_secret_store_is_exposed_under_its_logical_id`. Assert that both
parsed configurations define `[[local_server.secret_stores.trusted_server_secrets]]`
and define no `edgezero_runtime_env` Config Store. Point the starter secret
reference test at the logical ID as well.

- [ ] **Step 2: Run the focused test and verify it fails**

Run from `crates/trusted-server-integration-tests` using the host target:

```bash
cargo test --test integration local_fastly --target aarch64-apple-darwin
```

Expected: FAIL because both TOML files still name the store `ts_secrets` and
still define `edgezero_runtime_env`.

- [ ] **Step 3: Update the Viceroy configurations**

In `fastly.toml`, the Viceroy template, and the template-cache script rename
`[[local_server.secret_stores.ts_secrets]]` to
`[[local_server.secret_stores.trusted_server_secrets]]`. Remove the
`[local_server.config_stores.edgezero_runtime_env]` table and its contents from
`fastly.toml` and the template. Adjust the nearby root `fastly.toml` comment to
say Viceroy exposes the store under its logical ID.

- [ ] **Step 4: Run the focused test and verify it passes**

Run the same focused `cargo test` command.

Expected: PASS.

- [ ] **Step 5: Commit the regression and fixtures**

```bash
git add fastly.toml scripts/template-cache-local-test.sh crates/trusted-server-integration-tests/fixtures/configs/viceroy-template.toml crates/trusted-server-integration-tests/tests/common/config.rs
git commit -m "Expose Viceroy stores under their logical IDs"
```

### Task 3: Consume the EdgeZero PR Branch

**Files:**

- Modify: `Cargo.toml:59`
- Modify: `Cargo.lock`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs:92`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs:158`

- [ ] **Step 1: Select the upstream branch consistently**

Replace `tag = "v0.0.8"` on all six `edgezero-*` workspace dependencies with:

```toml
branch = "fix/fastly-environment-store-selectors"
```

- [ ] **Step 2: Refresh the lockfile through a targeted CLI check**

Run:

```bash
cargo check --package trusted-server-cli --target aarch64-apple-darwin
```

Expected: Cargo fetches the branch, locks every EdgeZero package to the same Git
revision, and the Trusted Server CLI compiles. If upstream API changes cause a
compiler failure, make only the call-site changes required by that error and add
a focused parsing or behavior test before the production edit.

The branch head removes `edgezero_adapter_fastly::runtime_env_config`. Replace
`RuntimeStoreConfig::from_env` with `RuntimeStoreConfig::logical()`: open
`trusted_server_config` and `trusted_server_secrets` by logical ID and read
the config entry under `trusted_server_config` for every target. Cover the
logical IDs and key with a unit test in `app.rs`.

- [ ] **Step 3: Verify the lockfile revision is consistent**

Run:

```bash
rg -n -A3 'name = "edgezero-' Cargo.lock
```

Expected: all Git-sourced EdgeZero packages use the branch URL and the same
commit fragment; no EdgeZero entry remains on `tag=v0.0.8`.

- [ ] **Step 4: Run the CLI test suite**

Run:

```bash
./scripts/test-cli.sh
```

Expected: all non-ignored tests pass.

- [ ] **Step 5: Commit the dependency update**

```bash
git add Cargo.toml Cargo.lock crates/trusted-server-adapter-fastly crates/trusted-server-cli
git commit -m "Consume canonical EdgeZero deployment selectors"
```

Only stage `crates/trusted-server-cli` if compiler-driven compatibility changes
were necessary.

### Task 4: Align Operator Documentation

**Files:**

- Modify: `docs/guide/fastly.md:264`
- Modify: `docs/guide/cli.md:115`

- [ ] **Step 1: Update Fastly store-selector guidance**

Keep the canonical export example. Replace the statements that provisioning
persists a service-scoped key and that unscoped keys are ignored with the PR 381
contract:

- provisioning creates/reuses the physical store;
- a managed deployment reads canonical selectors from its selected environment
  and links each selected physical store to the target version under its
  logical ID;
- the runtime opens stores by logical ID; nothing is stored in
  `edgezero_runtime_env`;
- staging links its selected physical stores into only the staged version;
- canonical environment-variable names contain no service ID;
- the selected physical resources must already exist.

- [ ] **Step 2: Update staging CLI guidance**

Clarify that `ts config push --staging` writes the logical key into the
physical Config Store selected by the staging environment, and that the staged
binary reads the same `<logical-store-id>` key there. State that production and
staging may select the same or different physical stores.

- [ ] **Step 3: Check for stale service-scoped guidance**

Run:

```bash
rg -n 'EDGEZERO__SERVICES__|service-scoped mapping|ignored unscoped|edgezero_runtime_env' fastly.toml docs crates/trusted-server-integration-tests --glob '!docs/superpowers/**'
```

Expected: no matches outside archived design material.

- [ ] **Step 4: Format documentation**

Run:

```bash
cd docs && npm run format
```

Expected: formatter exits successfully without unrelated changes.

- [ ] **Step 5: Commit documentation**

```bash
git add docs/guide/fastly.md docs/guide/cli.md
git commit -m "Document deploy-time Fastly store selection"
```

### Task 5: Verify the Complete Change

**Files:**

- Verify all modified files.

- [ ] **Step 1: Run formatting checks**

```bash
cargo fmt --all -- --check
cd docs && npm run format
```

Expected: both commands exit successfully.

- [ ] **Step 2: Run target-matched Rust checks**

```bash
cargo check-fastly
cargo check-axum
cargo check-cloudflare
cargo check-spin
```

Expected: all adapters compile with the selected EdgeZero branch.

- [ ] **Step 3: Run target-matched tests**

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
./scripts/test-cli.sh
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test integration local_fastly
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
```

Expected: all tests pass. Run socket-binding tests with loopback access if the
sandbox blocks them.

- [ ] **Step 4: Run target-matched Clippy**

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

Expected: every command exits successfully with warnings denied by the aliases.

- [ ] **Step 5: Run JavaScript and documentation gates**

```bash
(cd crates/trusted-server-js/lib && node build-all.mjs && npx vitest run && npm run format)
(cd docs && npm run format)
```

Expected: the JavaScript build and tests pass, and both formatters exit
successfully without unrelated changes.

- [ ] **Step 6: Review the final diff**

```bash
git diff origin/main...HEAD --check
git diff origin/main...HEAD --stat
git status --short
```

Expected: no whitespace errors, only planned files changed, and the worktree is
clean after final commits.

### Task 6: Publish the Pull Request

**Files:**

- No additional repository files.

- [ ] **Step 1: Push the isolated branch**

```bash
git push -u origin fix/align-edgezero-pr-381
```

- [ ] **Step 2: Draft the PR from the repository template**

Use a title such as `Align Fastly staging store selectors with EdgeZero PR 381`.
Lead with the staging failure and resulting canonical store resolution. Include:

- the EdgeZero branch dependency and upstream PR link;
- the canonical Viceroy fixtures and regression test;
- documentation changes;
- exact validation results;
- `Closes #<issue-number>`;
- a note that the branch pin will be replaced by the release tag after upstream
  merges.

- [ ] **Step 3: Create the pull request**

```bash
gh pr create --repo IABTechLab/trusted-server --base main --head fix/align-edgezero-pr-381 --title "Align Fastly staging store selectors with EdgeZero PR 381" --body-file <temporary-body-file>
```

Expected: GitHub prints the new pull request URL.
