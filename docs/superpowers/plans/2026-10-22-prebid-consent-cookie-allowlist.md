# Prebid consent cookie allowlist implementation plan

> **For agentic workers:** Execute centrally with the superpowers debugging, test-driven-development, and verification workflows; use independent agents for the outbound audit and final PR review.

**Goal:** Resolve #763 by preventing arbitrary browser cookies from reaching Prebid Server, while preserving OpenRTB consent behavior.

**Architecture:** Rebuild the PBS Cookie header in the shared transport helper from exact consent-cookie names only. Omit Cookie entirely for openrtb_only, non-ASCII headers, and inputs without valid allowed pairs. Keep Sourcepoint's intentional partner allowlist and publisher-origin forwarding intact. No new configuration or dependencies.

**Tech Stack:** Rust, http HeaderValue, existing consent constants, native and Viceroy tests.

## Scope and decisions

The issue already specifies the desired policy. Filtering at the common PBS transport boundary covers planned provider dispatch and the legacy test wrapper. Copying then stripping more names is unsafe because unknown publisher cookies remain; changing the generic forwarding helper would affect a separate contract and is unnecessary because it has no production callers.

A valid allowed pair has an exact case-sensitive allowlisted name, `=`, and an RFC 6265 cookie-octet value (optionally enclosed in double quotes). Invalid pairs are omitted; an unreadable header is omitted entirely. Values may contain `=`. Empty values are valid. Comma-separated decimal GPP section IDs are retained for compatibility with existing consent extraction. No cookie values are logged. Repeated inbound headers retain the existing selected-header behavior.

The existing cookies_only exception for policy-sourced body consent is preserved.

## Tasks

- [x] Establish the current transport test baseline in the isolated worktree.
- [x] Audit all outbound integration/provider Cookie paths and record intentional exceptions.
- [x] Add a failing mixed-cookie dispatch matrix for all modes, asserting both captured headers and OpenRTB consent fields through production AuctionPlan/PlannedOpenRtbProvider dispatch as well as legacy coverage.
- [x] Add failing boundary regressions for malformed/non-ASCII input, exact-name matching, quoted values, missing/empty headers, repeated headers, and stale destination cookies; preserve attested XFF.
- [x] Implement a minimal PBS-specific allowlist filter in the shared transport helper; remove any destination Cookie before applying policy.
- [x] Run the regressions and adapter-matched tests; update stale expectations without weakening coverage.
- [x] Document operator-visible PBS behavior and outbound audit results, keeping partner-specific exceptions explicit.
- [x] Run all AGENTS.md CI gates and record command outcomes, including environmental limitations.
- [x] Commit and push; create a draft PR using .claude/agents/pr-creator.md with Closes #763.
- [x] Independently review the draft PR, fix validated findings with regressions, rerun affected checks, and push the fixes.

## Review and regression evidence

An independent plan review confirmed the common transport boundary and requested explicit production planned-provider coverage plus updated snapshot/wrapper documentation; both are included.

The original transport baseline passed. New regressions then failed on the original implementation for raw cookie leakage, malformed-byte fallback, and stale destination cookies. After the fix, all 197 cookie-focused tests passed. The production matrix covers all three modes, cookie- and policy-sourced consent, mixed and malformed headers, no allowed cookies, absent headers, repeated Cookie fields, and top-level/ext OpenRTB consent placement.

## Verification

- Native core: 2,982 unit tests passed; both native integration suites passed (10 tests each).
- Fastly: 221 adapter and 2,976 core tests passed, with six existing wasm-only skips; reusable-sandbox suite: 221 passed.
- Axum, Cloudflare, and Spin adapter tests passed; native parity: 17 passed.
- All eight target-matched Clippy aliases passed, including CLI and codegen; native build-digest lint passed.
- Rust formatting, JavaScript formatting/build and 1,211 JavaScript tests passed.
- Documentation and outside-doc Markdown formatting, wasm-clock lint, and all documentation-proposal checks passed.
- Optimized Fastly WASM build passed.

Validation used Rust 1.98.1, Node 24.12.0, and Viceroy 0.21.0. The initially installed Viceroy 0.17.0 could not load the current SDK imports; the pinned version was downloaded separately for verification. Axum's loopback tests and Viceroy's macOS certificate access required sandbox-enabled retries. Those retries passed. The existing proc-macro-error2 future-incompatibility notice remains. No manual publisher deployment was performed.

## Independent PR review

Draft PR #1429 was independently reviewed at commit `999d72394`. The reviewer examined the live issue and PR, complete diff, production dispatch, consent serialization, transport capture, and outbound audit. No actionable findings were reported, so no runtime follow-up was needed. The reviewer independently reran the two focused Prebid-cookie tests and all 197 cookie-related tests; both runs passed, and the diff passed whitespace checks.

The PR remains a draft. Manual deployment/browser smoke tests and live PBS validation remain outside this verification. The project-board status could not be read or updated because the GitHub token lacks project-read permission; the PR's closing link to #763 is verified. The optional core rustdoc build completed with 31 link/private-item warnings in untouched documentation.
