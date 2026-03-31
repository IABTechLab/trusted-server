# PR 8 — Content Rewriting Trait (or Verification) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Verify that the content rewriting pipeline (HTML injection, attribute rewriting, compression) is already platform-agnostic, and document this finding so future adapter authors (PR 16/17) know no `PlatformContentRewriter` implementation is required.

**Architecture:** This is a verification-and-documentation PR. The rewriting pipeline modules (`html_processor.rs`, `streaming_processor.rs`, `streaming_replacer.rs`, `rsc_flight.rs`) have zero Fastly imports and are fully platform-agnostic. The `publisher.rs` handler module uses `fastly::Body`/`Request`/`Response`/`header` throughout its handler layer — including in `process_response_streaming`, which both accepts `fastly::Body` by value and returns a new `fastly::Body` via `Body::from(output)`. The handler layer is platform-coupled, but this is an HTTP-type concern to be addressed in Phase 2 (PR 11), not a content-rewriting concern. The `StreamingPipeline::process` generic (`R: Read + W: Write`) means the pipeline itself is already platform-neutral. No `PlatformContentRewriter` trait is needed.

**Tech Stack:** Rust 1.91.1, `lol_html`, `flate2`, `brotli`, `error-stack`. All tests run via `cargo test --workspace`.

---

## Audit Findings (Pre-Work Research)

| File                                                    | Fastly imports?                    | Notes                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| ------------------------------------------------------- | ---------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/html_processor.rs`      | **None**                           | Uses `lol_html`, `std`, `crate::integrations`, `crate::streaming_processor` only                                                                                                                                                                                                                                                                                                                                                                                  |
| `crates/trusted-server-core/src/streaming_processor.rs` | **None**                           | `StreamingPipeline::process` is generic over `R: Read + W: Write`                                                                                                                                                                                                                                                                                                                                                                                                 |
| `crates/trusted-server-core/src/streaming_replacer.rs`  | **None**                           | URL text replacer, standard Rust only                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `crates/trusted-server-core/src/rsc_flight.rs`          | **None**                           | RSC Flight URL rewriter, standard Rust only                                                                                                                                                                                                                                                                                                                                                                                                                       |
| `crates/trusted-server-core/src/publisher.rs`           | **Yes — handler layer throughout** | `fastly::http::{header, StatusCode}` and `fastly::{Body, Request, Response}` used in handler logic and at function signatures. `process_response_streaming` takes `Body` by value **and** returns `Body::from(output)` as output. The entire module is platform-coupled at its handler layer. Only the internal calls into `StreamingPipeline::process` are platform-agnostic. This is an HTTP-type concern for Phase 2 (PR 11), not a content-rewriting concern. |

**Outcome: PLATFORM-AGNOSTIC (pipeline layer).** The decompress-rewrite-recompress pipeline itself uses no Fastly-specific APIs. No `PlatformContentRewriter` trait is needed.

---

## File Map

| File                                                    | Change                                                        |
| ------------------------------------------------------- | ------------------------------------------------------------- |
| `crates/trusted-server-core/src/platform/mod.rs`        | Extend module doc with "Platform-Agnostic Components" section |
| `crates/trusted-server-core/src/html_processor.rs`      | Add `# Platform notes` section to module doc                  |
| `crates/trusted-server-core/src/streaming_processor.rs` | Add `# Platform notes` section to module doc                  |

`streaming_replacer.rs` and `rsc_flight.rs` are internal helpers called only from `publisher.rs` — they are not called directly from adapter entry points and are not public API surfaces that adapter authors need to navigate. They are documented via the `publisher.rs` audit note above and the `platform/mod.rs` summary. No separate module-doc task is needed for them.

---

## Task 1: Add Platform-Agnostic Note to `platform/mod.rs`

**Files:**

- Modify: `crates/trusted-server-core/src/platform/mod.rs:1-15`

- [ ] **Step 1: Read lines 1–16 of `platform/mod.rs` to confirm the exact current text**

  Open `crates/trusted-server-core/src/platform/mod.rs`. The current doc block
  should end with the `PlatformGeo` entry and be immediately followed by a blank
  `//!` line or the `mod error;` declaration. Confirm the exact content before
  editing. The expected existing block (lines 1–15) is:

  ```
  //! Platform abstraction layer for `trusted-server-core`.
  //!
  //! This module defines platform-neutral service contracts and request-scoped
  //! runtime state. Concrete implementations live in adapter crates such as
  //! `trusted-server-adapter-fastly`.
  //!
  //! ## Traits
  //!
  //! - [`PlatformConfigStore`] — key-value config store access
  //! - [`PlatformSecretStore`] — encrypted secret store access
  //! - [`PlatformKvStore`] — (re-exported from `edgezero_core`)
  //! - [`PlatformBackend`] — dynamic backend registration
  //! - [`PlatformHttpClient`] — outbound HTTP client
  //! - [`PlatformGeo`] — geographic information lookup
  //!
  ```

  Followed immediately by `mod error;` at line 17 (or similar). Confirm before
  proceeding.

- [ ] **Step 2: Append the Platform-Agnostic Components section to the module doc**

  The edit must replace the text that begins with `//! ## Traits` and ends just
  before `mod error;`. Replace the following exact block (lines 7–15 inclusive,
  the trailing `//!` blank line at the end of the doc):

  **Old text** (lines 7–15):

  ```
  //! ## Traits
  //!
  //! - [`PlatformConfigStore`] — key-value config store access
  //! - [`PlatformSecretStore`] — encrypted secret store access
  //! - [`PlatformKvStore`] — (re-exported from `edgezero_core`)
  //! - [`PlatformBackend`] — dynamic backend registration
  //! - [`PlatformHttpClient`] — outbound HTTP client
  //! - [`PlatformGeo`] — geographic information lookup
  //!
  ```

  **New text** (replace those lines with):

  ```
  //! ## Traits
  //!
  //! - [`PlatformConfigStore`] — key-value config store access
  //! - [`PlatformSecretStore`] — encrypted secret store access
  //! - [`PlatformKvStore`] — (re-exported from `edgezero_core`)
  //! - [`PlatformBackend`] — dynamic backend registration
  //! - [`PlatformHttpClient`] — outbound HTTP client
  //! - [`PlatformGeo`] — geographic information lookup
  //!
  //! ## Platform-Agnostic Components
  //!
  //! The following components were evaluated for platform-specific behavior
  //! (PR 8) and found to have a platform-agnostic rewriting pipeline. No
  //! platform trait is required; future adapters (PR 16/17) need not provide
  //! any content-rewriting implementation:
  //!
  //! - **Content rewriting** — `html_processor`, `streaming_processor`,
  //!   `streaming_replacer`, and `rsc_flight` modules use only standard Rust
  //!   (`std::io::Read`/`Write`, `lol_html`, `flate2`, `brotli`). The pipeline
  //!   is accessed via `StreamingPipeline::process<R: Read, W: Write>` which
  //!   accepts any reader, including `fastly::Body` (which implements
  //!   `std::io::Read`).
  //!
  //!   The `publisher.rs` handler module is platform-coupled at its handler
  //!   layer — it accepts and returns `fastly::Body` in function signatures
  //!   such as `process_response_streaming`. This is an HTTP-type coupling
  //!   that will be addressed in Phase 2 (PR 11) alongside all other
  //!   `fastly::Request`/`Response`/`Body` migrations. It is not a
  //!   content-rewriting concern.
  //!
  //!   No `PlatformContentRewriter` trait exists or is needed.
  //!
  ```

- [ ] **Step 3: Run `cargo check` to confirm the edit compiles**

  ```bash
  cargo check --workspace
  ```

  Expected: exit code 0, no errors.

- [ ] **Step 4: Run `cargo doc --no-deps --all-features` to confirm doc renders cleanly**

  ```bash
  cargo doc --no-deps --all-features 2>&1 | grep -E "warning|error" | head -20
  ```

  Expected: no output (no warnings or errors).

- [ ] **Step 5: Commit**

  ```bash
  git add crates/trusted-server-core/src/platform/mod.rs
  git commit -m "Document content rewriting as platform-agnostic in platform module"
  ```

---

## Task 2: Add Platform Notes to `html_processor.rs`

**Files:**

- Modify: `crates/trusted-server-core/src/html_processor.rs:1-3`

- [ ] **Step 1: Confirm exact module doc text at lines 1–3**

  Open `crates/trusted-server-core/src/html_processor.rs`. Confirm lines 1–3
  read exactly:

  ```
  //! Simplified HTML processor that combines URL replacement and integration injection
  //!
  //! This module provides a `StreamProcessor` implementation for HTML content.
  ```

  Line 4 is a blank line (no comment), followed by `use std::cell::Cell;` at
  line 5. The doc comment block ends at line 3 with no trailing `//!` blank
  line.

- [ ] **Step 2: Replace lines 1–3 with the extended module doc**

  Replace the three-line block (lines 1–3 only):

  **Old text:**

  ```
  //! Simplified HTML processor that combines URL replacement and integration injection
  //!
  //! This module provides a `StreamProcessor` implementation for HTML content.
  ```

  **New text:**

  ```
  //! Simplified HTML processor that combines URL replacement and integration injection.
  //!
  //! This module provides a [`StreamProcessor`] implementation for HTML content.
  //! It handles `<script>` tag injection at `<head>`, attribute URL rewriting
  //! (`href`, `src`, `action`, `srcset`, `imagesrcset`), and post-processing
  //! hooks for enabled integrations.
  //!
  //! # Platform notes
  //!
  //! This module is **platform-agnostic** (verified in PR 8). It has zero
  //! `fastly` imports and depends only on `lol_html`, `std`, and crate-internal
  //! types. [`create_html_processor`] returns an `impl `[`StreamProcessor`]
  //! whose `process_chunk` method operates on `&[u8]` slices with no
  //! platform body type involved.
  //!
  //! Future adapters (PR 16/17) do not need to implement any content-rewriting
  //! interface. See `crate::platform` module doc for the authoritative note.
  ```

- [ ] **Step 3: Run `cargo test --workspace`**

  ```bash
  cargo test --workspace
  ```

  Expected: all tests pass.

- [ ] **Step 4: Commit**

  ```bash
  git add crates/trusted-server-core/src/html_processor.rs
  git commit -m "Document html_processor as platform-agnostic"
  ```

---

## Task 3: Add Platform Notes to `streaming_processor.rs`

**Files:**

- Modify: `crates/trusted-server-core/src/streaming_processor.rs:1-7`

- [ ] **Step 1: Confirm exact module doc text at lines 1–8**

  Open `crates/trusted-server-core/src/streaming_processor.rs`. Confirm lines
  1–8 read exactly:

  ```
  //! Unified streaming processor architecture for handling compressed and uncompressed content.
  //!
  //! This module provides a flexible pipeline for processing content streams with:
  //! - Automatic compression/decompression handling
  //! - Pluggable content processors (text replacement, HTML rewriting, etc.)
  //! - Memory-efficient streaming
  //! - UTF-8 boundary handling
  ```

  Line 8 has no trailing `//!` blank line. Line 9 begins with `use error_stack`.
  The edit target is lines 1–7 only (the last line is line 7 — confirm exact
  count; adjust if the actual file differs).

- [ ] **Step 2: Replace lines 1–7 (the full doc block) with the extended module doc**

  **Old text** (lines 1–7, no trailing blank comment line):

  ```
  //! Unified streaming processor architecture for handling compressed and uncompressed content.
  //!
  //! This module provides a flexible pipeline for processing content streams with:
  //! - Automatic compression/decompression handling
  //! - Pluggable content processors (text replacement, HTML rewriting, etc.)
  //! - Memory-efficient streaming
  //! - UTF-8 boundary handling
  ```

  **New text:**

  ```
  //! Unified streaming processor architecture for handling compressed and uncompressed content.
  //!
  //! This module provides a flexible pipeline for processing content streams with:
  //! - Automatic compression/decompression handling
  //! - Pluggable content processors (text replacement, HTML rewriting, etc.)
  //! - Memory-efficient streaming
  //! - UTF-8 boundary handling
  //!
  //! # Platform notes
  //!
  //! This module is **platform-agnostic** (verified in PR 8). It has zero
  //! `fastly` imports. [`StreamingPipeline::process`] is generic over
  //! `R: Read + W: Write` — any reader or writer works, including
  //! `fastly::Body` (which implements `std::io::Read`) or standard
  //! `std::io::Cursor<&[u8]>`.
  //!
  //! Future adapters (PR 16/17) do not need to implement any compression or
  //! streaming interface. See `crate::platform` module doc for the
  //! authoritative note.
  ```

- [ ] **Step 3: Run `cargo test --workspace`**

  ```bash
  cargo test --workspace
  ```

  Expected: all tests pass.

- [ ] **Step 4: Commit**

  ```bash
  git add crates/trusted-server-core/src/streaming_processor.rs
  git commit -m "Document streaming_processor as platform-agnostic"
  ```

---

## Task 4: Run All CI Gates

**Files:** None (verification only)

- [ ] **Step 1: Run `cargo fmt --all -- --check`**

  ```bash
  cargo fmt --all -- --check
  ```

  Expected: exit code 0. If it fails, run `cargo fmt --all` then re-run.

- [ ] **Step 2: Run `cargo clippy`**

  ```bash
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  ```

  Expected: no warnings or errors.

- [ ] **Step 3: Run `cargo test --workspace`**

  ```bash
  cargo test --workspace
  ```

  Expected: all tests pass.

- [ ] **Step 4: Run `cargo doc --no-deps --all-features`**

  ```bash
  cargo doc --no-deps --all-features 2>&1 | grep -E "warning|error" | head -20
  ```

  Expected: no output (no broken intra-doc link warnings).

- [ ] **Step 5: Commit format fixes if needed**

  Only commit if `cargo fmt` produced changes. Otherwise skip.

  ```bash
  git add -p
  git commit -m "Apply rustfmt to documentation changes"
  ```
