# Trusted Didomi Geo Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans for central implementation
> and superpowers:requesting-code-review for independent final reviews.

**Goal:** Resolve #1151 by removing caller-supplied Didomi geography and protecting
SDK responses whose URLs do not identify trusted geo.

**Architecture:** Reuse the existing geo normalizer and response privacy helper.
Resolve SDK geo once, preserve canonical-loader behavior, and bypass caching for
noncanonical loaders and SDK POSTs. Other SDK GETs follow origin cache policy.
No new dependencies or adapter API changes.

**Tech stack:** Rust, RuntimeServices, platform HTTP cache intent, Markdown.

## Task 1: Regression tests

File: `crates/trusted-server-core/src/integrations/didomi.rs` test module.

- [x] Add a request matrix for loader/asset, geo mode on/off, and POST loader.
      Assert trusted normalized geo wins over spoofed source/destination headers.
- [x] Add unavailable, invalid, incomplete, and failed geo cases asserting proxy
      success without derived or raw geo headers on noncanonical requests.
- [x] Assert noncanonical loaders and SDK POSTs bypass caching, strip all cache
      metadata, retain CORS, and exclude client conditional headers upstream.
- [x] Verify API geo isolation and canonical loader cache preservation.
- [x] Run `cargo test -p trusted-server-core didomi` and confirm failures expose
      spoofed headers/default caching before changing production code.

## Task 2: Minimal implementation

File: `crates/trusted-server-core/src/integrations/didomi.rs`.

- [x] Resolve SDK geo before loader handling. Preserve loader lookup failure
      reasons; tolerate missing geo on other SDK paths by omitting headers.
- [x] Remove `copy_geo_headers`, and set derived headers only from trusted geo.
- [x] Retain origin caching for canonical loaders and other SDK GETs except paths
      ending `/loader.js`; bypass other requests and enforce terminal privacy.
- [x] Run focused tests and `cargo fmt --all -- --check`.

## Task 3: Documentation and verification

File: `docs/guide/integrations/didomi.md`.

- [x] Document all-SDK header trust, complete-pair omission, cache policy, purge
      requirements, platform limitations, and compatibility/origin traffic impact.
- [x] Run repository CI gates and record any environment or baseline failures.
- [x] Have independent subagents review `git diff main` for correctness, security,
      regression coverage, and compatibility. Resolve findings and reverify.

## PR review follow-up

The reviews on PR #1414 identified excessive cache bypass for static SDK assets.
Keep the loader URL contract and bypass policy; document the remaining throttling
risk and recommend canonical loaders on Fastly after validating geo coverage.

- [x] Add regression coverage for static core/UI/text GETs preserving all origin
      cache metadata with geo enabled/disabled and trusted geo present/absent.
      Run `cargo test -p trusted-server-core didomi` and observe the new failure.
- [x] Separate bypass cases from the geo-header matrix, including root and nested
      loader paths and SDK POSTs. Keep spoofed-header coverage for static assets.
- [x] Restore default outbound caching and origin response metadata for SDK GETs
      except `/loader.js` suffixes; preserve canonical-loader and API behavior.
- [x] Represent SDK geo as `Some(Ok | Err)` and API geo as `None`, eliminating the
      unreachable success-without-geo arm. Log omitted SDK geo at debug level.
- [x] Rename stale tests, update the guide/spec, add the security release note,
      link trusted client-IP setup, and remove inaccessible chat/session notes.
- [x] Run focused tests, target-matched adapter tests and lint, and repository CI
      gates. Review the complete PR diff before replying to and resolving threads.

## Verification results

The static SDK cache regression failed with `Bypass` before the cache-policy fix;
all 38 focused Didomi tests pass afterward. Independent review of the complete PR
diff found two guide inconsistencies (cacheable SDK GETs in the rollout checklist
and disabling SDK path rewriting for direct delivery); both were corrected. The
final review found no further confirmed runtime or documentation issue.

Passed checks:

- Rust format and all eight adapter/CLI/codegen clippy aliases.
- Fastly, reusable-sandbox, Axum, Cloudflare, and Spin tests.
- Adapter parity (17 tests), native build-digest tests (10 tests), and its lint.
- Host CLI tests, including the enabled browser fixtures in `scripts/test-cli.sh`.
- JS build (13 modules), 1,211 tests across 50 files, JS/docs/Markdown formatting.
- WASM clock lint and documentation proposal shellcheck, Node tests, and harness.
- Added-line domain lint; the optional full-repository audit reports pre-existing
  violations and this follow-up adds none.

No live Fastly deployment or CDN smoke test was performed.
