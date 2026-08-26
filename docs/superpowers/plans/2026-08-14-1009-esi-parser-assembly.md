# #1009 ESI Parser Assembly Implementation Plan

> **Execution note:** Implemented inline in the current checkout, without a worktree or
> subagents, as requested. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Use the repaired ESI parser on authorized cold template-cache misses without changing the existing warm-hit streaming behavior.

**Architecture:** The template cache retains the inert schema-v4 seam. Core delegates cold assembly through a platform trait; Fastly converts the seam to one synthetic ESI include and resolves it from the already-collected per-reader script. Parser failure falls back to core's validated byte split, while warm hits continue to stream by byte seam.

**Tech Stack:** Rust 1.95, `wasm32-wasip1`, Fastly Compute/Viceroy, `stackpop/esi` pinned by Git revision, `error-stack`.

---

### Task 1: Restore a platform assembly boundary

**Files:**

- Create: `crates/trusted-server-core/src/platform/template_assembly.rs`
- Modify: `crates/trusted-server-core/src/platform/mod.rs`
- Modify: `crates/trusted-server-core/src/platform/types.rs`
- Test: `crates/trusted-server-core/src/platform/template_assembly.rs`

- [x] Add a failing object-safety/default-behavior test for `PlatformTemplateAssembler`.
- [x] Run the focused core test and confirm it fails because the boundary is absent.
- [x] Add the trait, error type, unavailable default, runtime service field, builder method,
      accessor, and test support.
- [x] Run the focused tests and confirm they pass.

### Task 2: Delegate only cold-miss assembly

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs`
- Test: `crates/trusted-server-core/src/publisher.rs`

- [x] Add a recording assembler to the template-cache end-to-end tests.
- [x] Add a test asserting one platform call on a cold miss and no additional call on the
      subsequent warm hit.
- [x] Add a test asserting platform failure returns a complete byte-seam response.
- [x] Add tests for `x-ts-assembly` values on parser, fallback, and warm paths.
- [x] Run each test first and confirm the expected failure.
- [x] Change `assemble_if_shared` to call the platform assembler after storage, fall back
      to the validated byte split on error, and return the assembly method.
- [x] Set `x-ts-assembly` without changing `x-ts-template-cache` or privacy headers.
- [x] Re-run the template-cache end-to-end test module.

### Task 3: Add the repaired Fastly ESI adapter

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/Cargo.toml`
- Modify: `Cargo.lock`
- Create: `crates/trusted-server-adapter-fastly/src/esi_assembly.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/app.rs`
- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`
- Test: `crates/trusted-server-adapter-fastly/src/esi_assembly.rs`

- [x] Add failing adapter tests for a large Next.js script followed by the seam, an
      unexpected publisher ESI directive, an unexpected dispatcher URL, and verbatim
      fragment content.
- [x] Run the focused Fastly test filter and confirm the missing module/implementation
      fails.
- [x] Pin `https://github.com/stackpop/esi.git` at
      `4c53feab4d22ad9a84641b4c46f3f63bc6d197e2`.
- [x] Implement the explicit no-cache/no-DCA ESI configuration and synthetic completed
      fragment dispatcher.
- [x] Register `FastlyTemplateAssembler` in per-request runtime services.
- [x] Run the focused Fastly tests and confirm they pass.

### Task 4: Preserve cache schema and documentation truth

**Files:**

- Modify: `docs/superpowers/specs/2026-08-11-1009-streaming-assembly-architecture.md`
- Modify: `docs/guide/configuration.md`
- Modify: `scripts/template-cache-local-test.sh`
- Test: `crates/trusted-server-core/src/platform/template_cache.rs`

- [x] Add/adjust tests proving schema version 4 and the inert stored marker remain
      unchanged.
- [x] Extend the local harness to require `esi-parser` on the miss and `byte-seam` on the
      hit.
- [x] Update architecture and operator documentation to describe the hybrid path and
      pinned fork accurately.
- [x] Run formatting and the harness's static checks.

### Task 5: Full verification and signed commit

**Files:**

- Review every modified file.

- [x] Run `cargo fmt --all -- --check`.
- [x] Run every target-matched Clippy alias from `CLAUDE.md`.
- [x] Run `cargo test-fastly`, `cargo test-axum`, `cargo test-cloudflare`, and
      `cargo test-spin`.
- [x] Run the integration parity test.
- [x] Run JS tests/build/format and docs format.
- [x] Run the template-cache local harness when Viceroy and its certificate environment are
      available; otherwise report that environmental gap explicitly.
- [x] Run `git diff --check`, inspect staged scope, and confirm no operator configuration
      or secrets are staged.
- [x] Create one SSH-signed commit only after every required gate is green.
- [x] Verify the commit signature locally and report the exact commit ID and test counts.
