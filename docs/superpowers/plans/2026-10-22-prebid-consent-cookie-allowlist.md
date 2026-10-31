# Prebid consent cookie allowlist implementation plan

> **For agentic workers:** Execute centrally with the superpowers debugging, test-driven-development, and verification workflows; use independent agents for the outbound audit and final PR review.

**Goal:** Resolve #763 by preventing arbitrary browser cookies from reaching Prebid Server, while preserving OpenRTB consent behavior.

**Architecture:** Rebuild the PBS Cookie header in the shared transport helper from exact consent-cookie names only. Omit Cookie entirely for openrtb_only, non-ASCII headers, and inputs without valid allowed pairs. Keep Sourcepoint's intentional partner allowlist and publisher-origin forwarding intact. No new configuration or dependencies.

**Tech Stack:** Rust, http HeaderValue, existing consent constants, native and Viceroy tests.

## Scope and decisions

The issue already specifies the desired policy. Filtering at the common PBS transport boundary covers planned provider dispatch and the legacy test wrapper. Copying then stripping more names is unsafe because unknown publisher cookies remain; the unused generic forwarding helper is removed so the old unrestricted forwarding path cannot be reused accidentally.

Consent-cookie selection uses the same CookieJar parser as extraction, with case-sensitive allowlisted names and last-duplicate selection. A safe outbound value uses RFC 6265 cookie-octets (optionally enclosed in double quotes). Invalid pairs are omitted; an unreadable header is omitted entirely. Values may contain `=`. Empty values are valid. Comma-separated decimal GPP section IDs are normalized to remove section whitespace, preserving extraction semantics. If an extracted consent cookie cannot be framed safely, cookies_only carries only the affected signal in the body instead, with the GPP string and section IDs treated as one coupled signal. No cookie values are logged. Repeated inbound headers retain the existing selected-header behavior.

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
- [x] Independently review transport, consent extraction, serialization, and outbound audit behavior.

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

Initial validation used Rust 1.98.1, the locally available Node 24.12.0 executable, and Viceroy 0.21.0; the repository pins Node 24.20.0. The initially installed Viceroy 0.17.0 could not load the current SDK imports; the pinned version was downloaded separately for verification. Axum's loopback tests and Viceroy's macOS certificate access required sandbox-enabled retries. Those retries passed. The existing proc-macro-error2 future-incompatibility notice remains. No manual publisher deployment was performed.

## Consent extraction alignment follow-up

A planned-dispatch regression reproduced loss of both `__gpp_sid=2, 6` and `us_privacy = 1YYN`: consent extraction accepted the values but transport dropped them, while cookies_only omitted the body. Transport now uses the same CookieJar parser, with last-duplicate selection and safe decimal GPP whitespace normalization. A framing failure enables the JSON-body fallback for only the affected consent signal (coupling GPP and its section IDs) without forwarding unsafe bytes or unrelated cookies. Regressions cover all modes, decoded sale opt-out, GPP section IDs, duplicate values, quoted framing, and injected delimiters.
