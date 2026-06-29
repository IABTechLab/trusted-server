# Spin Adapter Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `trusted-server-adapter-spin` crate that provides a Fermyon Spin entry point for the EdgeZero migration, parallel to the existing Cloudflare adapter.

**Architecture:** The Spin adapter should mirror `trusted-server-adapter-cloudflare` for route wiring and middleware, while using Spin-specific entrypoint, manifests, request context, stores, and outbound HTTP. EdgeZero's Spin `run_app` injects config and KV handles into `RequestContext`; Trusted Server still needs its own synchronous platform adapters for `RuntimeServices`.

**Tech Stack:** Rust 2024, EdgeZero, Fermyon `spin-sdk` 5.2, `wasm32-wasip1`, `error-stack`, `async-trait`, GitHub Actions, existing Cargo aliases.

---

## Implementation Context

This plan is intended for a stacked PR branch after the existing EdgeZero Axum and Cloudflare work. Start from the current stack tip, not from `main`, unless the maintainer explicitly says to restack.

Recommended branch name:

```text
feature/edgezero-pr19-spin-adapter
```

Use an isolated git worktree when possible. This keeps the stacked branch clean, avoids disturbing the current IDE workspace, and gives baseline verification a clean starting point.

## Background and Constraints

- Follow `CLAUDE.md` first. Present this plan before coding, keep changes minimal, use `error-stack` for Trusted Server errors, avoid `unwrap()` in production, and use `log` macros.
- Use `apply_patch` for file edits in this workspace. When a task says "copy" an existing adapter file, read the source file and create the new file with `apply_patch`; do not shell-overwrite existing tracked files.
- This plan extends Phase 4 of `docs/superpowers/specs/2026-03-19-edgezero-migration-design.md` with a new Spin target after Axum and Cloudflare.
- Do not modify Fastly, Axum, Cloudflare, or core behavior except where dependency or workspace wiring requires it.
- Adapter alignment after inspection:
  - Fastly remains the production `wasm32-wasip1` Compute path with rollout flag and legacy fallback.
  - Axum remains the native dev adapter and is useful for env-var naming precedent, but it is not the structural model for this crate.
  - Cloudflare is the structural model for Spin route wiring, middleware order, buffered publisher behavior, route tests, and conservative outbound HTTP behavior.
- The EdgeZero dependency rev update is required, not optional. The current workspace rev does not include `edgezero-adapter-spin`; adding the Spin adapter without bumping the shared EdgeZero rev will not resolve.
- The separate `crates/integration-tests` manifest also pins EdgeZero. Update it in the same dependency task to avoid duplicate/mismatched `edgezero_core` types in parity tests.
- The current EdgeZero rev in root `Cargo.toml` is outdated. Local EdgeZero checkout verified at:
  `/Users/prk-jr/Desktop/opensource/rust/work/kodejams/edgezero`
  rev `ce6bcf74b529d9066d08ba87b2971af8379eb29e`.
- EdgeZero Spin has two different manifest concepts:
  - EdgeZero application manifest: used by `edgezero_adapter_spin::run_app`.
  - Spin runtime manifest: `spin.toml`, used by `spin up` and Spin deployment.
- Do not pass Spin runtime `spin.toml` to `run_app`. Use a separate EdgeZero manifest such as `crates/trusted-server-adapter-spin/edgezero.toml`.
- EdgeZero `SpinProxyClient` implements EdgeZero's proxy trait, not Trusted Server's `PlatformHttpClient`. Implement a local `SpinPlatformHttpClient` for `RuntimeServices`.
- EdgeZero `SpinSecretStore` is async. Trusted Server's `PlatformSecretStore` is sync. Implement a local sync secret adapter around `spin_sdk::variables::get`.
- EdgeZero still injects a `SecretHandle` into request extensions when secrets are enabled, but do not bridge that handle into `PlatformSecretStore`; `SecretHandle::get_bytes` is async and would not satisfy Trusted Server's synchronous platform trait.
- Spin KV has no TTL support. Any Trusted Server path that writes KV with TTL may degrade on Spin unless the runtime/EdgeZero support changes.
- Spin variables have naming restrictions. Verify config and secret key names before claiming runtime parity for request signing success paths.
- Keep the MVP honest: this branch adds a compiling/deployable Spin entry point, route/auth smoke coverage, native host tests, wasm build coverage, and CI. Full authenticated admin-key rotation success on Spin is a follow-up unless config/secret write support and Spin variable-name mapping are solved in this branch.
- Verification must be target-matched. The original prompt's monolithic `cargo clippy --workspace --all-targets --all-features -- -D warnings` is not the right blocking gate for this mixed-target workspace unless it is proven to compile every adapter under the correct runtime SDK. Use the Fastly, Axum, Cloudflare, Spin, and integration-test clippy aliases below as the required replacement, and call this out explicitly in the PR body.

## Scope Boundaries

In scope:

- Spin entrypoint crate and manifests.
- EdgeZero dependency revision update needed to access `edgezero-adapter-spin`.
- Spin runtime service adapters for Trusted Server's existing platform traits.
- Route wiring parity and auth-gate tests.
- Native and wasm CI coverage.
- Documentation for commands and known degraded areas.

Out of scope unless explicitly pulled into this branch:

- Production Spin deployment automation.
- Authenticated admin key rotation success on Spin.
- General config/secret management write support for Spin.
- Cross-adapter parity suite expansion beyond adding Spin smoke coverage.
- Refactoring shared adapter code across Cloudflare and Spin.

## File Structure

Create:

- `crates/trusted-server-adapter-spin/Cargo.toml`
  - Spin adapter crate manifest.
  - `crate-type = ["cdylib", "rlib"]`.
  - `spin` feature forwards to `edgezero-adapter-spin/spin`.
  - Host-compilable tests; wasm-only runtime deps in target block.

- `crates/trusted-server-adapter-spin/edgezero.toml`
  - EdgeZero manifest consumed by `edgezero_adapter_spin::run_app`.
  - Model on `crates/trusted-server-adapter-cloudflare/cloudflare.toml`.
  - Include `[adapters.spin]`.
  - Use `[stores.kv.adapters.spin] name = "default"` unless product requirements need a custom label.
  - Enable secrets for Spin.

- `crates/trusted-server-adapter-spin/spin.toml`
  - Spin runtime manifest.
  - `spin_manifest_version = 2`.
  - HTTP trigger route `"/..."`.
  - Component source points to `../../target/wasm32-wasip1/release/trusted_server_adapter_spin.wasm`.
  - `key_value_stores = ["default"]`.
  - `allowed_outbound_hosts = ["https://*:*", "http://*:*"]` unless ops requires a stricter list.
  - Declare only stable local dev variables. Avoid hardcoding secrets.

- `crates/trusted-server-adapter-spin/src/lib.rs`
  - Spin `#[http_component]` entrypoint.
  - Calls `edgezero_adapter_spin::run_app::<app::TrustedServerApp>(include_str!("../edgezero.toml"), req).await`.

- `crates/trusted-server-adapter-spin/src/app.rs`
  - Copy from Cloudflare adapter app.
  - Only replace crate-level names if needed.

- `crates/trusted-server-adapter-spin/src/middleware.rs`
  - Copy from Cloudflare adapter middleware.
  - Change geo availability calculation to `false` unconditionally.

- `crates/trusted-server-adapter-spin/src/platform.rs`
  - Runtime service construction for Spin.
  - Reuse config/KV handle adapters where valid.
  - Implement Spin sync secret adapter.
  - Implement Spin outbound HTTP client.
  - Implement null geo and Spin client IP extraction.

- `crates/trusted-server-adapter-spin/tests/routes.rs`
  - Copy Cloudflare route tests and adjust crate import names.
  - Keep tests host-native with `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`.

Modify:

- `Cargo.toml`
  - Add workspace member.
  - Bump all EdgeZero git dependencies to the same verified rev.
  - Add `edgezero-adapter-spin`.
  - Add `spin-sdk = { version = "5.2", default-features = false }`.
  - Add `anyhow = "1"` only for Spin entrypoint compatibility.
  - If EdgeZero rev requires `worker = 0.8`, update Cloudflare dependency versions only if compilation demands it; do not change Cloudflare behavior.

- `Cargo.lock`
  - Updated by Cargo after the EdgeZero rev bump and new dependencies.

- `crates/integration-tests/Cargo.toml`
  - Update EdgeZero revs to the same rev as the root workspace.
  - Add `trusted-server-adapter-spin` as a path dependency for in-process parity tests.

- `crates/integration-tests/Cargo.lock`
  - Updated after integration-test dependency changes.

- `crates/integration-tests/tests/parity.rs`
  - Add Spin as a third in-process adapter beside Axum and Cloudflare for route/auth/header parity.

- `.cargo/config.toml`
  - Exclude Spin from Fastly wasm test/clippy aliases.
  - Add `test-spin`, `check-spin`, `clippy-spin-native`, and `clippy-spin-wasm`.

- `.github/workflows/test.yml`
  - Add Spin native check, wasm32-wasip1 check, and native route tests.

- `.github/workflows/format.yml`
  - Add Spin clippy job or step.

- `.github/workflows/integration-tests.yml`
  - Review only. No Spin runtime job is required in this branch.
  - The existing integration-test job will still compile the integration-test crate after Spin is added as a path dependency, so keep Spin host compilation clean.

- `.github/workflows/codeql.yml`
  - Review only. No change expected because the Rust CodeQL entry uses `build-mode: none`.

- `CLAUDE.md`
  - Add Spin build/check/test commands if this branch is intended to update contributor workflow docs.
  - Update lint guidance so Spin uses target-matched clippy commands instead of the old mixed-target monolithic workspace clippy command.
  - Keep this change small and command-focused.

## Task 0: Branch, Worktree, and Baseline Setup

**Files:**

- Maybe modify: `.gitignore` if a project-local worktree directory is chosen and is not ignored
- No production source changes

- [ ] **Step 1: Confirm the stack base**

Run:

```bash
git status --short
git branch --show-current
git log --oneline -5
```

Expected: current branch is the latest stacked EdgeZero branch that already contains the Axum, Cloudflare, and verification-gate work. If the current branch is already `feature/edgezero-pr19-spin-adapter`, treat the branch-creation step below as already satisfied and only use a worktree if the maintainer explicitly wants one. If the current branch is not the intended stack tip, stop and ask which branch to base Spin on.

- [ ] **Step 2: Preserve this plan before creating a worktree**

Run:

```bash
git status --short docs/superpowers/plans/2026-05-22-spin-adapter-integration.md
```

Expected: the plan is tracked or deliberately copied into the implementation worktree. Because untracked files do not appear in a new `git worktree`, prefer committing this plan first:

```bash
git add docs/superpowers/plans/2026-05-22-spin-adapter-integration.md
git commit -m "Add Spin adapter integration plan"
```

If the maintainer does not want the plan committed yet, copy this file into the new worktree immediately after creating it and before starting Task 1.

- [ ] **Step 3: Check whether a worktree directory already exists**

Run:

```bash
ls -ld .worktrees worktrees 2>/dev/null
grep -i "worktree.*director" CLAUDE.md 2>/dev/null
```

Expected: use `.worktrees/` if present, otherwise `worktrees/` if present, otherwise follow any `CLAUDE.md` preference. If none exists and no preference is documented, ask where to create worktrees. Recommended fallback is:

```text
~/.config/superpowers/worktrees/trusted-server/feature-edgezero-pr19-spin-adapter
```

- [ ] **Step 4: Verify project-local worktree directories are ignored**

Only if using `.worktrees/` or `worktrees/`, run:

```bash
git check-ignore -q .worktrees || git check-ignore -q worktrees
```

Expected: PASS. If it fails, add the chosen directory to `.gitignore`, commit that `.gitignore` change separately, then continue.

- [ ] **Step 5: Create or reuse the isolated branch**

From the current stack tip, run one of the following.

Preferred worktree path:

```bash
mkdir -p ~/.config/superpowers/worktrees/trusted-server
git worktree add ~/.config/superpowers/worktrees/trusted-server/feature-edgezero-pr19-spin-adapter -b feature/edgezero-pr19-spin-adapter
cd ~/.config/superpowers/worktrees/trusted-server/feature-edgezero-pr19-spin-adapter
```

Project-local alternative:

```bash
mkdir -p .worktrees
git worktree add .worktrees/feature-edgezero-pr19-spin-adapter -b feature/edgezero-pr19-spin-adapter
cd .worktrees/feature-edgezero-pr19-spin-adapter
```

Fallback if the maintainer asks not to use a worktree:

```bash
git switch feature/edgezero-pr19-spin-adapter 2>/dev/null || git switch -c feature/edgezero-pr19-spin-adapter
```

If the branch name already exists, do not delete it. Use `git worktree list` and either reuse the existing worktree, stay on the current branch if it is already checked out, or ask for a new branch suffix.

- [ ] **Step 6: Capture a clean baseline**

Run:

```bash
cargo check
cargo check -p trusted-server-adapter-fastly --target wasm32-wasip1
cargo test-axum
cargo test-cloudflare
cargo check -p trusted-server-adapter-cloudflare --target wasm32-unknown-unknown --features cloudflare
cargo test --manifest-path crates/integration-tests/Cargo.toml --test parity
cargo fmt --all -- --check
```

Expected: PASS. If any baseline command fails before Spin work begins, record the failure and ask whether to fix baseline first or proceed with a known failing base.

## Task 1: Dependency Preflight and EdgeZero Rev Bump

**Files:**

- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/integration-tests/Cargo.toml`
- Modify: `crates/integration-tests/Cargo.lock`
- Maybe modify: `crates/trusted-server-adapter-cloudflare/Cargo.toml`
- Test: existing workspace check commands

- [ ] **Step 1: Inspect current EdgeZero dependency block**

Run:

```bash
sed -n '55,75p' Cargo.toml
```

Expected: current EdgeZero dependencies all use the same old rev.

- [ ] **Step 2: Audit the local EdgeZero Spin API before editing dependencies**

Run:

```bash
git -C /Users/prk-jr/Desktop/opensource/rust/work/kodejams/edgezero rev-parse HEAD
sed -n '1,220p' /Users/prk-jr/Desktop/opensource/rust/work/kodejams/edgezero/crates/edgezero-adapter-spin/src/lib.rs
sed -n '1,260p' /Users/prk-jr/Desktop/opensource/rust/work/kodejams/edgezero/crates/edgezero-adapter-spin/src/request.rs
sed -n '1,180p' /Users/prk-jr/Desktop/opensource/rust/work/kodejams/edgezero/crates/edgezero-adapter-spin/src/context.rs
sed -n '1,220p' /Users/prk-jr/Desktop/opensource/rust/work/kodejams/edgezero/crates/edgezero-adapter-spin/src/key_value_store.rs
sed -n '1,180p' /Users/prk-jr/Desktop/opensource/rust/work/kodejams/edgezero/crates/edgezero-adapter-spin/src/secret_store.rs
sed -n '1,180p' /Users/prk-jr/Desktop/opensource/rust/work/kodejams/edgezero/crates/edgezero-adapter-spin/src/proxy.rs
sed -n '1,120p' /Users/prk-jr/Desktop/opensource/rust/work/kodejams/edgezero/Cargo.toml
```

Expected:

- `run_app` accepts EdgeZero manifest contents plus `IncomingRequest`.
- `request::dispatch_with_store_settings` injects `ConfigStoreHandle`, `KvHandle`, and `SecretHandle` into request extensions.
- `SpinRequestContext::get` is public and exposes `client_addr`.
- `parse_client_addr` is crate-private; do not use it from Trusted Server.
- `SpinKvStore`, `SpinSecretStore`, and `SpinProxyClient` are all gated behind `#[cfg(all(feature = "spin", target_arch = "wasm32"))]`.
- `spin-sdk` is pinned at `5.2`.
- `worker` is pinned at `0.8`, so Cloudflare dependency drift is likely.

- [ ] **Step 3: Verify the target EdgeZero rev exists on the remote**

Run:

```bash
git -C /Users/prk-jr/Desktop/opensource/rust/work/kodejams/edgezero remote -v
git -C /Users/prk-jr/Desktop/opensource/rust/work/kodejams/edgezero branch --contains ce6bcf74b529d9066d08ba87b2971af8379eb29e
git ls-remote https://github.com/stackpop/edgezero ce6bcf74b529d9066d08ba87b2971af8379eb29e
```

Expected: the SHA is present on the upstream GitHub remote. If `git ls-remote` returns nothing, stop: do not pin Trusted Server to a local-only EdgeZero commit. Use the actual merged upstream SHA instead.

- [ ] **Step 4: Update root workspace dependencies**

Change the EdgeZero dependencies together:

```toml
edgezero-adapter-axum = { git = "https://github.com/stackpop/edgezero", rev = "ce6bcf74b529d9066d08ba87b2971af8379eb29e", default-features = false }
edgezero-adapter-cloudflare = { git = "https://github.com/stackpop/edgezero", rev = "ce6bcf74b529d9066d08ba87b2971af8379eb29e", default-features = false }
edgezero-adapter-fastly = { git = "https://github.com/stackpop/edgezero", rev = "ce6bcf74b529d9066d08ba87b2971af8379eb29e", default-features = false }
edgezero-adapter-spin = { git = "https://github.com/stackpop/edgezero", rev = "ce6bcf74b529d9066d08ba87b2971af8379eb29e", default-features = false }
edgezero-core = { git = "https://github.com/stackpop/edgezero", rev = "ce6bcf74b529d9066d08ba87b2971af8379eb29e", default-features = false }
```

Add:

```toml
anyhow = "1"
spin-sdk = { version = "5.2", default-features = false }
```

Verify the existing workspace dependency block already includes `brotli` and `flate2`. They are required by the Spin adapter's EdgeZero-compatible outbound response decompression policy. If either is missing in the target branch, add it to `[workspace.dependencies]` using the same versions already used by `trusted-server-core`.

- [ ] **Step 5: Update integration-test EdgeZero pins**

In `crates/integration-tests/Cargo.toml`, update both EdgeZero dev dependencies to the same rev:

```toml
edgezero-adapter-axum = { git = "https://github.com/stackpop/edgezero", rev = "ce6bcf74b529d9066d08ba87b2971af8379eb29e", features = ["axum"] }
edgezero-core = { git = "https://github.com/stackpop/edgezero", rev = "ce6bcf74b529d9066d08ba87b2971af8379eb29e" }
```

Do this in the same dependency task as the root update so in-process parity tests do not link two different `edgezero_core` versions.

- [ ] **Step 6: Run dependency-only checks before adding Spin crate**

Run:

```bash
cargo check -p trusted-server-adapter-cloudflare
cargo check -p trusted-server-adapter-cloudflare --target wasm32-unknown-unknown --features cloudflare
cargo check -p trusted-server-adapter-axum
cargo check -p trusted-server-adapter-fastly --target wasm32-wasip1
cargo check
```

Expected: PASS, or fail only on mechanical dependency API drift.

If Cargo needs to fetch the new EdgeZero revision and network access is blocked, rerun the same command with approved network permissions rather than changing dependency strategy.

- [ ] **Step 7: Handle Cloudflare worker version drift**

EdgeZero now pins `worker = 0.8`. Because `trusted-server-adapter-cloudflare` passes `worker` types into `edgezero_adapter_cloudflare::run_app`, the local crate will likely need the same `worker` version. Update only the `worker` version lines in `crates/trusted-server-adapter-cloudflare/Cargo.toml` from `0.7` to `0.8` if the wasm check fails or Cargo resolves duplicate incompatible Worker types.

Do not change Cloudflare adapter source behavior in this task.

- [ ] **Step 8: Refresh lockfiles**

Run:

```bash
cargo check
cargo test --manifest-path crates/integration-tests/Cargo.toml --test parity --no-run
```

Expected: `Cargo.lock` and `crates/integration-tests/Cargo.lock` are updated as needed.

If Cargo keeps the old git source in either lockfile, force the git dependency update instead of hand-editing lockfiles:

```bash
cargo update -p edgezero-core
cargo update -p edgezero-adapter-axum
cargo update -p edgezero-adapter-cloudflare
cargo update -p edgezero-adapter-fastly
cargo update --manifest-path crates/integration-tests/Cargo.toml -p edgezero-core
cargo update --manifest-path crates/integration-tests/Cargo.toml -p edgezero-adapter-axum
```

Do not run `cargo update -p edgezero-adapter-spin` in this task unless a package already depends on it. A root `[workspace.dependencies]` entry alone may not place `edgezero-adapter-spin` in `Cargo.lock`; it becomes resolvable after `crates/trusted-server-adapter-spin/Cargo.toml` exists and uses it.

- [ ] **Step 9: Commit dependency preflight**

Run:

```bash
git add Cargo.toml Cargo.lock crates/integration-tests/Cargo.toml crates/integration-tests/Cargo.lock crates/trusted-server-adapter-cloudflare/Cargo.toml
git commit -m "Update EdgeZero dependency revision"
```

Skip the commit if the implementation workflow intentionally batches commits, but keep this as the first review boundary.

## Task 2: Scaffold Spin Adapter Crate and Manifests

**Files:**

- Create: `crates/trusted-server-adapter-spin/Cargo.toml`
- Create: `crates/trusted-server-adapter-spin/edgezero.toml`
- Create: `crates/trusted-server-adapter-spin/spin.toml`
- Modify: `Cargo.toml`

- [ ] **Step 1: Add workspace member**

Add to root `[workspace].members`:

```toml
"crates/trusted-server-adapter-spin",
```

Do not add it to `default-members`.

Update the nearby workspace comments so the adapter matrix lists Spin as:

```text
trusted-server-adapter-spin     → wasm32-wasip1         (Fermyon Spin)
```

Keep the `default-members = ["crates/trusted-server-adapter-fastly"]` block unchanged; Viceroy depends on Fastly remaining the sole default member.

- [ ] **Step 2: Create crate manifest**

Create `crates/trusted-server-adapter-spin/Cargo.toml`:

```toml
[package]
name = "trusted-server-adapter-spin"
version = "0.1.0"
edition = "2024"
publish = false

[lints]
workspace = true

[lib]
name = "trusted_server_adapter_spin"
path = "src/lib.rs"
crate-type = ["cdylib", "rlib"]

[features]
default = []
spin = ["edgezero-adapter-spin/spin"]

[dependencies]
anyhow = { workspace = true }
async-trait = { workspace = true }
brotli = { workspace = true }
bytes = { workspace = true }
edgezero-adapter-spin = { workspace = true }
edgezero-core = { workspace = true }
error-stack = { workspace = true }
flate2 = { workspace = true }
log = { workspace = true }
trusted-server-core = { path = "../trusted-server-core" }
trusted-server-js = { path = "../js" }

[target.'cfg(target_arch = "wasm32")'.dependencies]
spin-sdk = { workspace = true }

[dev-dependencies]
base64 = { workspace = true }
edgezero-core = { workspace = true }
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }
```

Keep `spin-sdk` in the wasm target dependency block, not in the unconditional dependency list. Native route tests must compile without the Spin runtime SDK.

- [ ] **Step 3: Create EdgeZero manifest**

Create `crates/trusted-server-adapter-spin/edgezero.toml`:

```toml
[app]
name = "trusted-server"
version = "0.1.0"
kind = "http"

[adapters.spin]

[stores.kv]
name = "trusted_server_kv"

[stores.kv.adapters.spin]
name = "default"

[stores.config]
name = "trusted_server_config"

[stores.secrets]
name = "trusted_server_secrets"

[stores.secrets.adapters.spin]
enabled = true
```

- [ ] **Step 4: Create Spin runtime manifest**

Create `crates/trusted-server-adapter-spin/spin.toml`:

```toml
spin_manifest_version = 2

[application]
name = "trusted-server-adapter-spin"
version = "0.1.0"

[variables]

[[trigger.http]]
route = "/..."
component = "trusted-server"

[component.trusted-server]
source = "../../target/wasm32-wasip1/release/trusted_server_adapter_spin.wasm"
allowed_outbound_hosts = ["https://*:*", "http://*:*"]
key_value_stores = ["default"]

[component.trusted-server.variables]

[component.trusted-server.build]
command = "cargo build --target wasm32-wasip1 --release -p trusted-server-adapter-spin --features spin"
watch = ["src/**/*.rs", "Cargo.toml", "edgezero.toml", "spin.toml"]
```

This runtime manifest intentionally declares no request-signing config values or secrets. The optional runtime smoke in Task 12 must therefore only prove that Spin starts, routes requests, and enforces unauthenticated admin auth. If a stricter local smoke later needs config or secret values, add only safe lowercase Spin variables using the two-part Spin pattern:

```toml
[variables]
example_config = { default = "" }

[component.trusted-server.variables]
example_config = "{{ example_config }}"
```

Do not add dotted, hyphenated, uppercase, or fake secret placeholders just to make request-signing success paths pass. EdgeZero's current `SpinConfigStore` reads the requested key directly from `spin_sdk::variables::get(key)`, while Spin component variable names are constrained.

- [ ] **Step 5: Check crate discovery**

Run:

```bash
cargo check -p trusted-server-adapter-spin
```

Expected initially: FAIL because source files do not exist yet. This confirms Cargo resolves the package.

## Task 3: Add Route Tests First

**Files:**

- Create: `crates/trusted-server-adapter-spin/tests/routes.rs`

- [ ] **Step 1: Copy Cloudflare route tests**

Copy `crates/trusted-server-adapter-cloudflare/tests/routes.rs` to:

```text
crates/trusted-server-adapter-spin/tests/routes.rs
```

Change only:

```rust
use trusted_server_adapter_cloudflare::app::TrustedServerApp;
```

to:

```rust
use trusted_server_adapter_spin::app::TrustedServerApp;
```

Update file-level comments from Cloudflare to Spin.

- [ ] **Step 2: Add EdgeZero manifest validation test**

Add this test to `tests/routes.rs` so `edgezero.toml` is validated on the native host target. Route tests alone do not parse the manifest; `run_app` only parses it at Spin runtime.

```rust
#[test]
fn edgezero_manifest_loads_and_resolves_spin_stores() {
    let loader =
        edgezero_core::manifest::ManifestLoader::load_from_str(include_str!("../edgezero.toml"));
    let manifest = loader.manifest();

    assert!(
        manifest.stores.config.is_some(),
        "Spin EdgeZero manifest must enable config store injection"
    );
    assert_eq!(
        manifest.kv_store_name(edgezero_core::app::SPIN_ADAPTER),
        "default",
        "Spin KV label must match spin.toml key_value_stores"
    );
    assert!(
        manifest.secret_store_enabled(edgezero_core::app::SPIN_ADAPTER),
        "Spin EdgeZero manifest must enable secret handle injection"
    );
}
```

Do not add `[stores.config.adapters.spin]` to `edgezero.toml`; EdgeZero validates that as an unsupported config-store adapter key because Spin config values come from component variables.

- [ ] **Step 3: Run the tests and verify failure**

Run:

```bash
cargo test -p trusted-server-adapter-spin --test routes
```

Expected: FAIL because `trusted_server_adapter_spin::app` does not exist yet.

## Task 4: Add Entrypoint, App, and Middleware

**Files:**

- Create: `crates/trusted-server-adapter-spin/src/lib.rs`
- Create: `crates/trusted-server-adapter-spin/src/app.rs`
- Create: `crates/trusted-server-adapter-spin/src/middleware.rs`

- [ ] **Step 1: Create Spin entrypoint**

Create `src/lib.rs`:

```rust
pub mod app;
pub mod middleware;
pub mod platform;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http::{IncomingRequest, IntoResponse};
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
use spin_sdk::http_component;

#[cfg(all(feature = "spin", target_arch = "wasm32"))]
#[http_component]
async fn handle(req: IncomingRequest) -> anyhow::Result<impl IntoResponse> {
    edgezero_adapter_spin::run_app::<app::TrustedServerApp>(include_str!("../edgezero.toml"), req)
        .await
}
```

Use the `all(feature = "spin", target_arch = "wasm32")` gate here as well as in `platform.rs`. This keeps native host tests free of Spin runtime SDK requirements and matches EdgeZero's own Spin adapter gates.

- [ ] **Step 2: Copy app wiring**

Copy:

```text
crates/trusted-server-adapter-cloudflare/src/app.rs
```

to:

```text
crates/trusted-server-adapter-spin/src/app.rs
```

Do not restructure route registration. Keep handler behavior identical.

Update comments that say "Workers" to "Spin" only where they describe buffering/runtime behavior. Do not rename handlers, state, routes, or middleware ordering.

- [ ] **Step 3: Copy middleware and apply Spin geo change**

Copy:

```text
crates/trusted-server-adapter-cloudflare/src/middleware.rs
```

to:

```text
crates/trusted-server-adapter-spin/src/middleware.rs
```

Change `FinalizeResponseMiddleware::handle` to:

```rust
async fn handle(&self, ctx: RequestContext, next: Next<'_>) -> Result<Response, EdgeError> {
    let geo_available = false;

    let mut response = next.run(ctx).await?;
    apply_finalize_headers(&self.settings, geo_available, &mut response);
    Ok(response)
}
```

Update the doc comment to say Spin has no geo headers.

Review the copied middleware unit tests. Keep helper-level tests for `apply_finalize_headers`, but rename or remove any test title that implies Spin reads `cf-ipcountry`.

- [ ] **Step 4: Run route tests and verify next failure**

Run:

```bash
cargo test -p trusted-server-adapter-spin --test routes
```

Expected: FAIL because `platform::build_runtime_services` does not exist yet.

## Task 5: Implement Spin Platform Runtime Services

**Files:**

- Create: `crates/trusted-server-adapter-spin/src/platform.rs`
- Test: `crates/trusted-server-adapter-spin/src/platform.rs` test module

- [ ] **Step 1: Start from Cloudflare platform**

Copy `crates/trusted-server-adapter-cloudflare/src/platform.rs` to Spin platform and then make only Spin-required substitutions.

Remove all `worker`, `js_sys`, and Cloudflare header-specific logic. Keep generic no-op stores, no-op backend, config handle adapter, KV handle adapter, and the buffered HTTP-client shape.

Before keeping copied test helpers, clean up Cloudflare test-only `unwrap()` calls. The new Spin crate must pass `cargo clippy-spin-native`, which runs all targets with workspace lints and denies `unwrap_used`. Replace copied test unwraps with `expect("should ...")`, `expect_err("should ...")`, or explicit assertions before adding Spin-specific tests.

- [ ] **Step 2: Keep shared store handle adapters**

Keep these adapters from Cloudflare mostly verbatim:

```rust
struct ConfigStoreHandleAdapter(ConfigStoreHandle);
struct KvHandleAdapter(KvHandle);
```

Rationale: EdgeZero Spin `run_app` resolves store settings and injects `ConfigStoreHandle`, `KvHandle`, and `SecretHandle` into request extensions before routing.

Do not build `SpinConfigStore` or `SpinKvStore` directly unless EdgeZero's audited API changed. Direct construction is not needed with current `run_app`.

- [ ] **Step 3: Apply exact cfg gates for Spin-only types**

Every import, struct, and impl that touches `spin_sdk`, `SpinKvStore`, `SpinSecretStore`, or Spin outbound HTTP must use:

```rust
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
```

The native fallback must use:

```rust
#[cfg(not(all(feature = "spin", target_arch = "wasm32")))]
```

This is stricter than Cloudflare's `#[cfg(target_arch = "wasm32")]` pattern because EdgeZero Spin gates its runtime APIs by both feature and target.

- [ ] **Step 4: Implement null geo**

Replace Cloudflare geo with:

```rust
struct NullGeo;

impl PlatformGeo for NullGeo {
    fn lookup(&self, _client_ip: Option<IpAddr>) -> Result<Option<GeoInfo>, Report<PlatformError>> {
        Ok(None)
    }
}
```

- [ ] **Step 5: Extract client IP from Spin request context**

Use EdgeZero's already-parsed context:

```rust
fn extract_client_ip(ctx: &edgezero_core::context::RequestContext) -> Option<IpAddr> {
    edgezero_adapter_spin::SpinRequestContext::get(ctx.request()).and_then(|c| c.client_addr)
}
```

Do not call `parse_client_addr`; it is crate-private in EdgeZero.

- [ ] **Step 6: Add tests for client IP extraction and null geo**

Add tests using `RequestContext::new` and `SpinRequestContext::insert`:

```rust
#[test]
fn extract_client_ip_reads_spin_request_context() {
    let mut req = request_builder()
        .method("GET")
        .uri("https://example.com/")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    edgezero_adapter_spin::SpinRequestContext::insert(
        &mut req,
        edgezero_adapter_spin::SpinRequestContext {
            client_addr: Some("203.0.113.42".parse().expect("should parse test IP")),
            full_url: None,
        },
    );
    let ctx = RequestContext::new(req, PathParams::default());

    assert_eq!(
        extract_client_ip(&ctx),
        Some("203.0.113.42".parse().expect("should parse test IP")),
        "should read Spin client IP from request context"
    );
}
```

- [ ] **Step 7: Implement sync Spin secret adapter**

Under `#[cfg(all(feature = "spin", target_arch = "wasm32"))]`:

```rust
struct SpinSecretStoreAdapter;

impl PlatformSecretStore for SpinSecretStoreAdapter {
    fn get_bytes(
        &self,
        _store_name: &StoreName,
        key: &str,
    ) -> Result<Vec<u8>, Report<PlatformError>> {
        let variable_name = key.to_ascii_lowercase();
        match spin_sdk::variables::get(&variable_name) {
            Ok(value) => Ok(value.into_bytes()),
            Err(error) => Err(Report::new(PlatformError::SecretStore).attach(format!(
                "secret lookup failed for key `{key}` as Spin variable `{variable_name}`: {error}"
            ))),
        }
    }

    fn create(&self, _: &StoreId, _: &str, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore)
            .attach("secret store writes are not supported on Spin"))
    }

    fn delete(&self, _: &StoreId, _: &str) -> Result<(), Report<PlatformError>> {
        Err(Report::new(PlatformError::SecretStore)
            .attach("secret store deletes are not supported on Spin"))
    }
}
```

Do not use EdgeZero's `SpinSecretStore` or `ctx.secret_handle()` directly for `PlatformSecretStore`; their trait path is async while Trusted Server's platform secret trait is sync. Match EdgeZero's Spin convention by ignoring `store_name` and lowercasing the key before calling `spin_sdk::variables::get`. Do not prefix the variable with the Trusted Server store id unless a later product requirement defines an explicit mapping.

This adapter is read-only. `create` and `delete` must return `PlatformError::SecretStore` errors instead of silently succeeding.

- [ ] **Step 8: Add build_runtime_services**

Use this shape:

```rust
pub fn build_runtime_services(ctx: &edgezero_core::context::RequestContext) -> RuntimeServices {
    let client_ip = extract_client_ip(ctx);

    #[cfg(all(feature = "spin", target_arch = "wasm32"))]
    let http_client: Arc<dyn PlatformHttpClient> = Arc::new(SpinPlatformHttpClient);
    #[cfg(not(all(feature = "spin", target_arch = "wasm32")))]
    let http_client: Arc<dyn PlatformHttpClient> = Arc::new(UnavailableHttpClient);

    let config_store: Arc<dyn PlatformConfigStore> = ctx
        .config_store()
        .map(|h| Arc::new(ConfigStoreHandleAdapter(h)) as Arc<dyn PlatformConfigStore>)
        .unwrap_or_else(|| Arc::new(NoopConfigStore));

    let kv_store: Arc<dyn PlatformKvStore> = ctx
        .kv_handle()
        .map(|h| Arc::new(KvHandleAdapter(h)) as Arc<dyn PlatformKvStore>)
        .unwrap_or_else(|| Arc::new(UnavailableKvStore));

    #[cfg(all(feature = "spin", target_arch = "wasm32"))]
    let secret_store: Arc<dyn PlatformSecretStore> = Arc::new(SpinSecretStoreAdapter);
    #[cfg(not(all(feature = "spin", target_arch = "wasm32")))]
    let secret_store: Arc<dyn PlatformSecretStore> = Arc::new(NoopSecretStore);

    RuntimeServices::builder()
        .config_store(config_store)
        .secret_store(secret_store)
        .kv_store(kv_store)
        .backend(Arc::new(NoopBackend))
        .http_client(http_client)
        .geo(Arc::new(NullGeo))
        .client_info(ClientInfo {
            client_ip,
            tls_protocol: None,
            tls_cipher: None,
        })
        .build()
}
```

- [ ] **Step 9: Add native tests for service construction**

Add a host-target test that calls `build_runtime_services` with a plain `RequestContext` and asserts:

- `client_info.client_ip` is `None`.
- `geo.lookup(None)` returns `Ok(None)`.
- `config_store().get(...)` returns an error instead of panicking when no handle is injected.
- `kv_store()` is unavailable instead of panicking when no handle is injected.

- [ ] **Step 10: Run platform tests**

Run:

```bash
cargo test -p trusted-server-adapter-spin platform
```

Expected: PASS on host.

## Task 6: Implement SpinPlatformHttpClient

**Files:**

- Modify: `crates/trusted-server-adapter-spin/src/platform.rs`
- Test: wasm build; native route tests exercise fallback only

- [ ] **Step 1: Implement wasm-only buffered HTTP client**

Under `#[cfg(all(feature = "spin", target_arch = "wasm32"))]`, implement `SpinPlatformHttpClient` using `spin_sdk::http::send`.

Mirror Cloudflare limitations:

- `send` executes and buffers the response.
- `send_async` eagerly executes and stores response parts in `SpinPendingResponse`.
- `select` rejects `pending_requests.len() >= 2` with a clear `PlatformError::HttpClient`.

Do not use `edgezero_adapter_spin::SpinProxyClient` here. That type is for EdgeZero's proxy abstraction, while Trusted Server routes integrations and auctions through `trusted_server_core::platform::PlatformHttpClient`.

Match `SpinProxyClient`'s response policy, though:

- If upstream returns `content-encoding: gzip`, decompress with `flate2::read::GzDecoder`.
- If upstream returns `content-encoding: br`, decompress with `brotli::Decompressor`.
- Cap decompressed bodies at 64 MiB, matching EdgeZero's `MAX_DECOMPRESSED_SIZE`.
- After successful gzip/br decompression, strip `content-encoding` and `content-length` before constructing the `PlatformResponse`.
- For unsupported or absent encodings, preserve the body bytes and encoding headers unchanged.
- If gzip/br decompression fails or exceeds the cap, return `Report::new(PlatformError::HttpClient)` with an attached message that names the failed encoding.

Implementation details:

- Convert `edgezero_core::http::Method` to `spin_sdk::http::Method`.
- Convert the URI with `request.request.uri().to_string()`.
- Forward only UTF-8 header values; log and drop non-UTF-8 headers because Spin's WASI HTTP request builder expects string header values.
- Buffer `Body::Once` directly.
- Return a typed `PlatformError::HttpClient` for unsupported `Body::Stream` unless a local helper safely buffers it.
- Build the EdgeZero response from Spin status, sanitized headers, and decompressed-or-raw body bytes according to the policy above.
- Sanitize hop-by-hop response headers using the Axum helper pattern: strip `connection`, `proxy-authenticate`, `proxy-authorization`, `te`, `trailer`, `transfer-encoding`, `upgrade`, `keep-alive`, and any header named by `Connection`.
- Document the compression policy in a short comment above the response conversion helper. This is a deliberate adapter-level match with EdgeZero Spin proxy behavior, not an open-ended runtime audit.
- Preserve `backend_name` on both immediate and pending responses.

Required response wrapper:

```rust
#[cfg(all(feature = "spin", target_arch = "wasm32"))]
struct SpinPendingResponse {
    backend_name: String,
    status: u16,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
}
```

- [ ] **Step 2: Preserve backend correlation**

Every successful response should use:

```rust
PlatformResponse::new(edge_resp).with_backend_name(request.backend_name)
```

Every pending request should use:

```rust
PlatformPendingRequest::new(pending).with_backend_name(backend_name)
```

- [ ] **Step 3: Handle streaming request bodies conservatively**

If `edgezero_core::body::Body::Stream(_)` cannot be safely buffered on Spin, return:

```rust
Err(Report::new(PlatformError::HttpClient)
    .attach("streaming request bodies are not supported on Spin outbound HTTP"))
```

If buffering is straightforward with existing EdgeZero helpers, use the local established pattern and keep it wasm-only.

- [ ] **Step 4: Add conversion-focused tests**

Extract the response-header sanitization and gzip/br decompression policy into target-neutral helpers wherever possible. Add native unit tests for:

- `transfer-encoding` is stripped.
- a header named in `Connection` is stripped.
- ordinary response headers are preserved.
- gzip body is decoded and `content-encoding` / `content-length` are stripped.
- brotli body is decoded and `content-encoding` / `content-length` are stripped.
- unsupported encodings preserve raw body bytes and encoding headers.
- decompression failures return a typed `PlatformError::HttpClient`.
- decompressed output over 64 MiB returns a typed `PlatformError::HttpClient`.

Do not rely only on `cargo check-spin` for this behavior; compilation does not prove response semantics. If a portion must stay wasm-only because it directly touches Spin SDK types, keep the pure header/body policy in a target-neutral helper and test that helper. If this is impossible without changing production code shape too much, document the missing runtime validation as an explicit follow-up in the PR body.

- [ ] **Step 5: Run wasm check**

Run:

```bash
cargo check -p trusted-server-adapter-spin --target wasm32-wasip1 --features spin
```

Expected: PASS.

## Task 7: Route Tests and Native Host Compilation

**Files:**

- Modify: `crates/trusted-server-adapter-spin/tests/routes.rs`
- Maybe modify: `crates/trusted-server-adapter-spin/src/middleware.rs`
- Maybe modify: `crates/trusted-server-adapter-spin/src/app.rs`

- [ ] **Step 1: Run route tests**

Run:

```bash
cargo test -p trusted-server-adapter-spin --test routes
```

Expected: PASS.

- [ ] **Step 2: Keep exact-status assertions only where stable**

Use exact `401` assertions for admin auth-fail routes and `WWW-Authenticate`.

Before weakening any status assertion, compare against the original acceptance requirement: named routes should return expected `200`, `401`, or `405` where the result is deterministic without runtime stores. Keep exact assertions for:

- unauthenticated protected/admin routes: `401`
- `WWW-Authenticate` on `401`
- unsupported methods on explicitly registered routes: `405`, if the EdgeZero router currently returns `405`

For general route smoke tests, mirror Cloudflare's current style:

- route is not `404`
- route does not `5xx` where applicable
- finalize header is present

Do not force `200` for routes whose success depends on request-signing config, KV, secrets, or upstream HTTP. If the branch cannot satisfy a requested exact `200`/`405` without adding fake runtime configuration, document that as a deliberate acceptance-scope deviation in the PR body instead of silently weakening coverage.

- [ ] **Step 3: Add exact 405 route coverage**

Add at least one deterministic method-not-allowed assertion for an explicitly registered route. Use `PUT /verify-signature`, which is registered as a POST route in the Cloudflare structural model. Do not use `GET /verify-signature`: the app also registers GET catch-all publisher fallback routes, so a GET request may route through the fallback instead of producing router-level `405`.

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn verify_signature_put_returns_405() {
    let router = TrustedServerApp::routes();
    let req = request_builder()
        .method("PUT")
        .uri("/verify-signature")
        .body(edgezero_core::body::Body::empty())
        .expect("should build request");
    let resp = router.oneshot(req).await;

    assert_eq!(
        resp.status().as_u16(),
        405,
        "PUT /verify-signature must return method-not-allowed, not route miss"
    );
}
```

If this fails because the current EdgeZero router returns a different deterministic status, compare Axum, Cloudflare, and Spin before changing the assertion. Do not delete the `405` coverage silently; either choose another registered route that returns `405`, or document the router-level deviation in the PR body.

- [ ] **Step 4: Run native package check**

Run:

```bash
cargo check -p trusted-server-adapter-spin
```

Expected: PASS.

## Task 8: Add Spin to In-Process Parity Tests

**Files:**

- Modify: `crates/integration-tests/Cargo.toml`
- Modify: `crates/integration-tests/Cargo.lock`
- Modify: `crates/integration-tests/tests/parity.rs`

- [ ] **Step 1: Add Spin adapter to integration-test dev dependencies**

In `crates/integration-tests/Cargo.toml`, add:

```toml
trusted-server-adapter-spin = { path = "../trusted-server-adapter-spin" }
```

Keep the EdgeZero revs in this file aligned with the root workspace rev from Task 1.

- [ ] **Step 2: Add Spin app alias and helpers**

In `crates/integration-tests/tests/parity.rs`, add:

```rust
use trusted_server_adapter_spin::app::TrustedServerApp as SpinApp;
```

Add helpers matching the Cloudflare in-process helpers:

```rust
/// Send a GET request to the Spin adapter and return (status, headers, body bytes).
async fn spin_get_body(uri: &str) -> (u16, HeaderMap, bytes::Bytes) {
    let router = SpinApp::routes();
    let req = request_builder()
        .method("GET")
        .uri(uri)
        .body(edgezero_core::body::Body::empty())
        .expect("should build GET request");
    let resp = router.oneshot(req).await.expect("should respond");
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body_bytes = resp.into_body().into_bytes();
    (status, headers, body_bytes)
}

/// Send a GET request to the Spin adapter and return (status, headers).
async fn spin_get(uri: &str) -> (u16, HeaderMap) {
    let (s, h, _) = spin_get_body(uri).await;
    (s, h)
}

/// Send a POST request to the Spin adapter and return (status, headers, body bytes).
async fn spin_post(uri: &str, body: &str) -> (u16, HeaderMap, bytes::Bytes) {
    let router = SpinApp::routes();
    let req = request_builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .body(edgezero_core::body::Body::from(body.to_owned()))
        .expect("should build POST request");
    let resp = router.oneshot(req).await.expect("should respond");
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let body_bytes = resp.into_body().into_bytes();
    (status, headers, body_bytes)
}

/// Convenience wrapper for tests that don't need body.
async fn spin_post_headers(uri: &str, body: &str) -> (u16, HeaderMap) {
    let (s, h, _) = spin_post(uri, body).await;
    (s, h)
}
```

Use `spin_get_body` for the discovery JSON-parsability parity test. Do not silently downgrade that assertion to status/header-only coverage.

- [ ] **Step 3: Extend existing parity assertions to Spin**

For each existing Axum-vs-Cloudflare parity test, add Spin as a third participant where the assertion is stable:

- discovery route status
- discovery route JSON parsability
- verify-signature route status
- unauthenticated admin rotate and deactivate status/header
- `x-geo-info-available` presence on representative responses
- auction route is not challenged by admin auth
- unknown route status

Example pattern:

```rust
let (spin_status, spin_headers) = spin_post_headers("/admin/keys/rotate", "{}").await;
assert_eq!(
    spin_status, 401,
    "Spin must return 401 for unauthenticated admin route"
);
assert!(
    spin_headers.contains_key("www-authenticate"),
    "Spin 401 must include WWW-Authenticate header"
);
assert_eq!(
    cf_status, spin_status,
    "Cloudflare and Spin must return the same status for unauthenticated admin route"
);
```

Do not add runtime Spin CLI or network-backed tests here. These are host in-process parity tests only.

- [ ] **Step 4: Run parity tests**

Run:

```bash
cargo test --manifest-path crates/integration-tests/Cargo.toml --test parity
```

Expected: PASS.

## Task 9: Spin Runtime Config, KV, and Secret Semantics Audit

**Files:**

- Maybe modify: `crates/trusted-server-adapter-spin/edgezero.toml`
- Maybe modify: `crates/trusted-server-adapter-spin/spin.toml`
- Maybe modify: `docs/superpowers/specs/2026-03-19-edgezero-migration-design.md`

- [ ] **Step 1: Identify runtime store keys actually used before auth**

Run:

```bash
rg -n "config_store\\(\\)|secret_store\\(\\)|kv_store\\(\\)|kv_handle\\(\\)" crates/trusted-server-core/src
rg -n "put_bytes_with_ttl|list_keys_page|get_bytes\\(|get_string\\(|\\.put\\(|\\.delete\\(|\\.create\\(" \
  crates/trusted-server-core/src/request_signing \
  crates/trusted-server-core/src/storage \
  crates/trusted-server-core/src/publisher.rs \
  crates/trusted-server-core/src/proxy.rs \
  crates/trusted-server-core/src/auction \
  crates/trusted-server-core/src/integrations
```

Expected: produce a short note in the PR description about which routes can hit config, secret, or KV before auth and which routes only matter after auth. At minimum, classify:

- Unauthenticated admin requests: should stop at auth and not touch config, secret, or KV stores.
- Authenticated admin rotation/deactivation: touches config writes/deletes and secret create/delete; out of MVP unless Spin management writes are implemented.
- Request signing discovery/verify/signing: reads config and secret values; limited by Spin variable-name mapping.
- Publisher consent storage: uses KV and `put_bytes_with_ttl`; limited by Spin KV TTL support.
- Auction/integration routes: may use outbound HTTP through `SpinPlatformHttpClient`.

- [ ] **Step 2: Confirm admin auth rejection does not require Spin stores**

Run:

```bash
cargo test -p trusted-server-adapter-spin --test routes admin_route_without_credentials_returns_401
cargo test -p trusted-server-adapter-spin --test routes admin_rotate_key_auth_fail_returns_401
cargo test -p trusted-server-adapter-spin --test routes admin_deactivate_key_auth_fail_returns_401
```

Expected: PASS on host. These prove the required admin auth gates do not depend on Spin runtime stores.

- [ ] **Step 3: Document Spin config key limitations**

Spin component variables are not a general key/value config dictionary. Current EdgeZero `SpinConfigStore` calls `spin_sdk::variables::get(key)`, so keys containing hyphens, dots, or uppercase may not work unless normalized or explicitly mapped.

Add a short note to the plan/spec or PR body:

```markdown
Spin MVP does not claim authenticated request-signing rotation success because
Trusted Server config keys such as `current-kid` and `active-kids` do not map
cleanly to Spin component variable names without a key mapping layer.
Unauthenticated admin rejection is covered in this PR; runtime write support is
a follow-up.
```

- [ ] **Step 4: Document Spin KV TTL limitation**

Trusted Server currently uses `put_bytes_with_ttl` for consent KV storage. EdgeZero's Spin KV adapter returns validation errors for TTL writes.

Add a short PR/spec note:

```markdown
Spin KV TTL is unavailable in the current EdgeZero Spin adapter. Routes that
require expiring KV writes may degrade on Spin until a TTL strategy is defined.
```

- [ ] **Step 5: Keep manifests minimal**

Do not add fake request-signing variables or placeholder secrets to `spin.toml` just to make smoke tests pass. Only add variables that are required for a local Spin runtime smoke and have safe defaults.

If the optional Spin runtime smoke remains limited to discovery routing plus unauthenticated admin rejection, keep `[variables]` and `[component.trusted-server.variables]` empty. If the smoke expands to a route that must read config or secrets successfully, add the exact lowercase Spin variable declarations and component mappings in `spin.toml`, and explain in the PR why those safe defaults are needed.

## Task 10: Cargo Aliases and CI

**Files:**

- Modify: `.cargo/config.toml`
- Modify: `.github/workflows/test.yml`
- Modify: `.github/workflows/format.yml`

- [ ] **Step 1: Update Fastly aliases to exclude Spin**

In `.cargo/config.toml`, add `--exclude trusted-server-adapter-spin` to aliases that target Fastly `wasm32-wasip1` workspace builds:

```toml
test-fastly = ["test", "--workspace", "--exclude", "trusted-server-adapter-axum", "--exclude", "trusted-server-adapter-cloudflare", "--exclude", "trusted-server-adapter-spin", "--target", "wasm32-wasip1"]
clippy-fastly = ["clippy", "--workspace", "--exclude", "trusted-server-adapter-axum", "--exclude", "trusted-server-adapter-cloudflare", "--exclude", "trusted-server-adapter-spin", "--all-targets", "--all-features", "--target", "wasm32-wasip1", "--", "-D", "warnings"]
```

Also update the top comments in `.cargo/config.toml` so they mention Spin and explain that Fastly aliases exclude Axum, Cloudflare, and Spin because those adapters have separate target-matched checks.

- [ ] **Step 2: Add Spin aliases**

Add:

```toml
test-spin = ["test", "-p", "trusted-server-adapter-spin"]
check-spin = ["check", "-p", "trusted-server-adapter-spin", "--target", "wasm32-wasip1", "--features", "spin"]
clippy-spin-native = ["clippy", "-p", "trusted-server-adapter-spin", "--all-targets", "--no-default-features", "--", "-D", "warnings"]
clippy-spin-wasm = ["clippy", "-p", "trusted-server-adapter-spin", "--target", "wasm32-wasip1", "--features", "spin", "--lib", "--", "-D", "warnings"]
```

Do not use host-target `--all-features` for Spin unless `spin-sdk` is proven host-compilable. The wasm alias provides feature coverage for the real runtime feature.

- [ ] **Step 3: Add Spin CI test job**

In `.github/workflows/test.yml`, add a job parallel to Cloudflare:

```yaml
test-spin:
  name: cargo check/build (spin native + wasm32-wasip1)
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4

    - name: Retrieve Rust version
      id: rust-version
      run: echo "rust-version=$(grep '^rust ' .tool-versions | awk '{print $2}')" >> $GITHUB_OUTPUT
      shell: bash

    - name: Set up Rust toolchain (native + wasm32-wasip1)
      uses: actions-rust-lang/setup-rust-toolchain@v1
      with:
        toolchain: ${{ steps.rust-version.outputs.rust-version }}
        target: wasm32-wasip1
        cache-shared-key: cargo-${{ runner.os }}

    - name: Check Spin adapter (native host)
      run: cargo check -p trusted-server-adapter-spin

    - name: Check Spin adapter (wasm32-wasip1)
      run: cargo check-spin

    - name: Build Spin adapter release WASM
      run: cargo build --package trusted-server-adapter-spin --target wasm32-wasip1 --features spin --release

    - name: Run Spin adapter tests (native host)
      run: cargo test-spin
```

Do not add a separate parity job. The existing `test-parity` job should cover Spin after Task 8 modifies `crates/integration-tests/tests/parity.rs`. If that job starts failing to compile because of the new Spin path dependency, fix the Spin crate's native dependency gating rather than weakening the parity job.

- [ ] **Step 4: Add Spin clippy**

In `.github/workflows/format.yml`, add:

```yaml
- name: Run cargo clippy (Spin - native)
  run: cargo clippy-spin-native

- name: Run cargo clippy (Spin - wasm32-wasip1)
  run: cargo clippy-spin-wasm
```

If the wasm clippy job needs the `wasm32-wasip1` target in the format workflow, ensure the setup step already installs it before adding the command.

Do not bundle unrelated Cloudflare CI cleanup into this branch unless `cargo clippy-cloudflare` already passes locally and the maintainer wants CI symmetry. The required Spin CI surface is native clippy plus `wasm32-wasip1` clippy for the `spin` feature.

- [ ] **Step 5: Review integration and CodeQL workflows**

Check these files before final verification:

```bash
sed -n '1,220p' .github/workflows/integration-tests.yml
sed -n '1,120p' .github/workflows/codeql.yml
```

Expected:

- No Spin runtime integration job is needed.
- Existing integration tests continue to compile because `trusted-server-adapter-spin` is host-compilable without `--features spin`.
- No CodeQL change is needed while Rust analysis uses `build-mode: none`.

## Task 11: Documentation Update

**Files:**

- Modify: `CLAUDE.md`
- Maybe modify: `docs/superpowers/specs/2026-03-19-edgezero-migration-design.md`

- [ ] **Step 1: Add Spin commands to CLAUDE.md**

Add commands near Cloudflare:

```bash
# Check Spin adapter (native)
cargo check -p trusted-server-adapter-spin

# Check Spin adapter (WASM target)
cargo check-spin

# Test Spin adapter (native host)
cargo test-spin

# Clippy Spin adapter
cargo clippy-spin-native
cargo clippy-spin-wasm

# Production-style Spin WASM artifact used by spin.toml
cargo build --package trusted-server-adapter-spin --target wasm32-wasip1 --features spin --release

# Optional local Spin runtime smoke, if the Spin CLI is installed
spin up --from crates/trusted-server-adapter-spin
```

- [ ] **Step 2: Update CLAUDE.md lint guidance**

Update the existing `Testing & Quality` / `Lint` guidance so it does not imply the mixed-target command is the required blocking gate after Spin is added. Keep the general command available only as an optional compatibility check, and document the target-matched blocking gate instead:

```bash
# Lint by adapter target
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-spin-native
cargo clippy-spin-wasm
```

The rationale to preserve in prose: this workspace now has multiple wasm runtimes (`wasm32-wasip1` and `wasm32-unknown-unknown`) with runtime-specific SDKs, so adapter clippy must be target-matched.

- [ ] **Step 3: Update migration plan phase table**

If this branch owns planning docs, add a Phase 4 row after PR 17:

```markdown
| PR 18/19 | Spin entry point | PR 17 + EdgeZero Spin stores | Route parity + basic-auth gate tests pass; crate host-compilable; Spin wasm32-wasip1 check added; CI jobs added | TBD | edgezero, phase-4, spin | Phase 4 |
```

Use the actual PR number for the stacked branch.

## Task 12: Verification

**Files:**

- No source changes unless failures expose issues.

- [ ] **Step 1: Format**

Run:

```bash
cargo fmt --all -- --check
```

Expected: PASS.

- [ ] **Step 2: Native and adapter-specific checks**

Run:

```bash
cargo check
cargo check -p trusted-server-adapter-fastly --target wasm32-wasip1
cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
cargo check -p trusted-server-adapter-axum
cargo check -p trusted-server-adapter-cloudflare
cargo check -p trusted-server-adapter-cloudflare --target wasm32-unknown-unknown --features cloudflare
cargo check -p trusted-server-adapter-spin
cargo check-spin
cargo check --manifest-path crates/integration-tests/Cargo.toml
```

Expected: PASS.

Do not use `cargo build --workspace` as a required gate for this branch. The workspace contains the Fastly adapter, which is only linkable for `wasm32-wasip1`; host workspace builds can fail at link time even when the target-matched adapter gates are healthy.

Do not replace the target-matched clippy aliases with `cargo clippy --workspace --all-targets --all-features -- -D warnings`; this workspace contains multiple wasm targets with different runtime SDKs. If a reviewer asks for the literal monolithic command from the original prompt, run it separately, record the result, and keep the target-matched aliases as the blocking gate unless the command is proven valid for the mixed-target workspace.

- [ ] **Step 3: Tests**

Run:

```bash
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/integration-tests/Cargo.toml --test parity
cargo test --manifest-path crates/integration-tests/Cargo.toml --no-run
```

Expected: PASS.

- [ ] **Step 4: Fastly gate**

Run:

```bash
cargo test-fastly
```

Expected: PASS. If Viceroy is missing locally, document that it was not run and rely on CI.

- [ ] **Step 5: Clippy**

Run:

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo clippy --manifest-path crates/integration-tests/Cargo.toml -- -D warnings
```

Expected: PASS.

- [ ] **Step 6: Spin release build**

Run:

```bash
cargo build --package trusted-server-adapter-spin --target wasm32-wasip1 --features spin --release
```

Expected: PASS and artifact exists at:

```text
target/wasm32-wasip1/release/trusted_server_adapter_spin.wasm
```

- [ ] **Step 7: Optional local Spin runtime smoke**

Only if `spin` CLI is installed:

```bash
spin up --from crates/trusted-server-adapter-spin
```

Then in another shell:

```bash
curl -i http://127.0.0.1:3000/.well-known/trusted-server.json
curl -i -X POST http://127.0.0.1:3000/admin/keys/rotate -H 'content-type: application/json' -d '{"keyId":"test-key"}'
```

Expected:

- Discovery route returns a routed response, not a runtime trap.
- Admin route without credentials returns `401`.
- `WWW-Authenticate` header is present.

If the discovery route returns an application-level config/secret error because `spin.toml` intentionally has no request-signing variables, record that as expected MVP scope. The required runtime smoke assertion is that Spin starts, dispatches into Trusted Server, and admin auth rejection works without touching runtime stores.

## Task 13: Review, Stack Hygiene, and PR Handoff

**Files:**

- No source changes unless review finds issues

- [ ] **Step 1: Inspect final diff**

Run:

```bash
git status --short
git diff --stat
git diff --check
```

Expected:

- No unrelated files changed.
- No whitespace errors.
- Diff is limited to Spin adapter, workspace dependency/wiring, CI aliases/jobs, and small docs updates.

- [ ] **Step 2: Confirm no forbidden cross-adapter behavior edits**

Run:

```bash
git diff -- crates/trusted-server-adapter-fastly crates/trusted-server-adapter-axum crates/trusted-server-core
git diff -- crates/trusted-server-adapter-cloudflare
```

Expected:

- Fastly, Axum, and core diffs are empty unless the maintainer explicitly expanded scope.
- Cloudflare diff is empty or limited to dependency version compatibility in `Cargo.toml`.

- [ ] **Step 3: Prepare PR body notes**

Include:

```markdown
## Summary

- Adds `trusted-server-adapter-spin` as a host-compilable + wasm32-wasip1 adapter.
- Adds separate EdgeZero and Spin runtime manifests.
- Adds route/auth smoke tests and Spin CI gates.
- Updates EdgeZero dependency revision for Spin adapter support.
- Matches EdgeZero Spin proxy gzip/br response decompression behavior for Spin outbound HTTP.

## Known limitations

- Spin KV TTL is unavailable in current EdgeZero Spin KV.
- Authenticated admin key rotation success is not claimed for Spin in this PR.
- Spin component variable naming does not directly support all Trusted Server config keys.
- Spin outbound HTTP initially rejects multi-provider fan-out, matching Cloudflare's conservative behavior.
- Target-matched clippy aliases are used instead of the monolithic mixed-target workspace clippy command.

## Verification

- `cargo fmt --all -- --check`
- `cargo check`
- `cargo check -p trusted-server-adapter-fastly --target wasm32-wasip1`
- `cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1`
- `cargo check -p trusted-server-adapter-axum`
- `cargo check -p trusted-server-adapter-cloudflare`
- `cargo check -p trusted-server-adapter-cloudflare --target wasm32-unknown-unknown --features cloudflare`
- `cargo check -p trusted-server-adapter-spin`
- `cargo check-spin`
- `cargo check --manifest-path crates/integration-tests/Cargo.toml`
- `cargo test-axum`
- `cargo test-cloudflare`
- `cargo test-spin`
- `cargo test -p trusted-server-adapter-spin edgezero_manifest_loads_and_resolves_spin_stores`
- `cargo test --manifest-path crates/integration-tests/Cargo.toml --test parity`
- `cargo test --manifest-path crates/integration-tests/Cargo.toml --no-run`
- `cargo test-fastly`
- `cargo clippy-fastly`
- `cargo clippy-axum`
- `cargo clippy-cloudflare`
- `cargo clippy-spin-native`
- `cargo clippy-spin-wasm`
- `cargo clippy --manifest-path crates/integration-tests/Cargo.toml -- -D warnings`
- `cargo build --package trusted-server-adapter-spin --target wasm32-wasip1 --features spin --release`
- Spin runtime smoke: `spin up --from crates/trusted-server-adapter-spin` plus discovery/admin curl checks, or `not run: Spin CLI unavailable`
```

- [ ] **Step 4: Confirm stacked PR base**

Before opening the PR, confirm it targets the previous stack branch, not `main`, unless the maintainer asks otherwise.

Run:

```bash
git branch --show-current
gh pr list --head "$(git branch --show-current)" --json number,title,headRefName,baseRefName
```

Expected: If no PR exists yet, create it against the immediate predecessor branch in the EdgeZero stack.

## Known Risks and Follow-ups

- EdgeZero rev bump may require `worker = 0.8` in the Cloudflare adapter manifest. Keep this as dependency maintenance only.
- Root `Cargo.lock` and `crates/integration-tests/Cargo.lock` must both move to the same EdgeZero git revision. A mismatch can compile two incompatible `edgezero_core` copies in parity tests.
- `edgezero.toml` must be validated by a native test. Host route/parity tests do not exercise `edgezero_adapter_spin::run_app`, so they can pass even if the runtime manifest would panic at Spin request time.
- Spin config variables may not support all Trusted Server config key names, especially request-signing keys such as `current-kid`, `active-kids`, and key IDs. The MVP only guarantees route/auth smoke and wasm build unless runtime config key mapping is verified.
- Spin KV TTL is unsupported in EdgeZero's current Spin KV adapter. Consent KV paths that require TTL may degrade on Spin.
- Spin outbound HTTP fan-out should initially match Cloudflare's limitation: single-provider supported, multi-provider fan-out rejected loudly.
- Spin outbound HTTP response decompression and header stripping must stay covered by target-neutral tests because wasm build checks do not prove response semantics.
- Management writes for config and secrets are unsupported on Spin in this plan. Admin key routes must reject unauthenticated requests, but authenticated key rotation success is not in MVP scope unless product requirements change.
- The Spin runtime smoke is optional because the Spin CLI may not be installed in every development environment. CI must still cover native tests and wasm compilation.

## Completion Criteria

- `trusted-server-adapter-spin` exists as a workspace member, but not a default member.
- Native host check and route tests pass.
- Native manifest validation test proves `edgezero.toml` resolves the Spin KV label, config store, and secret enablement.
- Native conversion tests cover Spin outbound response sanitization, gzip/br decompression, unsupported encoding preservation, decode failures, and decompressed-size limits.
- `wasm32-wasip1` Spin check/build passes with `--features spin`.
- Existing Fastly, Axum, and Cloudflare gates still pass.
- `crates/integration-tests` parity tests include Spin and pass with aligned EdgeZero dependency revs.
- Root and integration-test lockfiles agree on the same EdgeZero git revision.
- CI has a Spin job with native check, wasm check, release wasm build, native tests, and Spin clippy coverage.
- Existing integration-test and CodeQL workflows were reviewed; no unintentional Spin runtime dependency was introduced there.
- EdgeZero manifest and Spin runtime manifest are separate and correctly wired.
- Route coverage includes exact unauthenticated admin `401`, `WWW-Authenticate`, and at least one deterministic unsupported-method `405` assertion or a documented router-level deviation.
- Spin outbound HTTP response header/compression policy is covered by native helper tests where feasible, or the PR documents the remaining runtime validation gap.
- No Cloudflare, Axum, Fastly, or core behavior changes were introduced beyond required dependency compatibility.
