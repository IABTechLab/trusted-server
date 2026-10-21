# Trusted Didomi Geo Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans for central implementation
> and superpowers:requesting-code-review for independent final reviews.

**Goal:** Resolve #1151 by removing caller-supplied Didomi geography and protecting
SDK responses whose URLs do not identify trusted geo.

**Architecture:** Reuse the existing geo normalizer and response privacy helper.
Resolve SDK geo once, preserve canonical-loader behavior, and bypass caching for
every noncanonical request. No new dependencies or adapter API changes.

**Tech stack:** Rust, RuntimeServices, platform HTTP cache intent, Markdown.

## Task 1: Regression tests

File: `crates/trusted-server-core/src/integrations/didomi.rs` test module.

- [x] Add a request matrix for loader/asset, geo mode on/off, and POST loader.
      Assert trusted normalized geo wins over spoofed source/destination headers.
- [x] Add unavailable, invalid, incomplete, and failed geo cases asserting proxy
      success without derived or raw geo headers on noncanonical requests.
- [x] Assert noncanonical SDK requests bypass caching, strip all cache metadata,
      retain CORS, and exclude client conditional headers from upstream requests.
- [x] Verify API geo isolation and canonical loader cache preservation.
- [x] Run `cargo test -p trusted-server-core didomi` and confirm failures expose
      spoofed headers/default caching before changing production code.

## Task 2: Minimal implementation

File: `crates/trusted-server-core/src/integrations/didomi.rs`.

- [x] Resolve SDK geo before loader handling. Preserve loader lookup failure
      reasons; tolerate missing geo on other SDK paths by omitting headers.
- [x] Remove `copy_geo_headers`, and set derived headers only from trusted geo.
- [x] Use `canonical_loader.is_some()` to select cache eligibility; otherwise
      bypass outbound caching and enforce terminal private response privacy.
- [x] Run focused tests and `cargo fmt --all -- --check`.

## Task 3: Documentation and verification

File: `docs/guide/integrations/didomi.md`.

- [x] Document all-SDK header trust, complete-pair omission, cache policy, purge
      requirements, platform limitations, and compatibility/origin traffic impact.
- [x] Run repository CI gates and record any environment or baseline failures.
- [x] Have independent subagents review `git diff main` for correctness, security,
      regression coverage, and compatibility. Resolve findings and reverify.
- [x] Keep the new branch/worktree for user handoff; do not merge or publish.

## Review record

The independent design review approved the spec and plan without blocking gaps.
Independent security and correctness reviewers then examined the complete diff
against `main` and reported no findings requiring changes. They confirmed the
geo trust boundary, canonical cache eligibility, private fallback responses, and
migration guidance. One optional suggestion was to count API geo lookups in tests;
the production SDK-only lookup condition remains explicit.

Focused native tests demonstrated three failures before the production fix
(spoofed headers, missing-geo header fallback, and default outbound caching), then
passed all 37 Didomi-related tests after it. The JavaScript suite passed all 1,211
tests with the pinned Node 24.12.0. All required repository CI gates passed:

- Rust format; eight adapter/CLI/codegen lint aliases; native build-digest lint.
- Fastly, Fastly reusable sandbox, Axum, Cloudflare, and Spin tests.
- Adapter parity (17 tests) and native build-digest tests (10 tests).
- JavaScript build, tests, and formatting; documentation and Markdown formatting.
- Wasm clock lint and all documentation proposal script checks.

Fastly tests required access to the macOS certificate keychain, and Axum tests
required local loopback sockets. Their sandbox-only startup/bind failures passed
when rerun with those permissions. The first JavaScript test attempt selected
Node 20 in the nested worktree; the pinned Node 24.12.0 run passed.
