# Fastly review resolution implementation plan

**Goal:** Resolve the reusable-sandbox review findings without changing public response caching.

**Approved scope:** Gate workload counters on existing private/no-store policy; require a positive memory limit; report logger installation failures; cover config-bounded CIDR caching; correct retention and measurement documentation; prepare a current PR description.

**Architecture:** Keep policy and lifecycle changes in the Fastly adapter. Exercise the existing DataDome cache without changing its implementation. Memory limits are checked between requests by the SDK, not enforced as an allocation ceiling during a request. Missing heap support retires the sandbox conservatively.

**Tech stack:** Rust, Fastly Compute, EdgeZero, Viceroy.

- [x] Add counter privacy regressions in `crates/trusted-server-adapter-fastly/src/main.rs`; run `cargo test-fastly-reuse --locked sandbox_counters` before and after the fix. Cover public/absent/quoted policies and terminal-private responses.
- [x] Add memory-limit validation regressions in `crates/trusted-server-adapter-fastly/src/sandbox.rs`; require `TS__SANDBOX__MAX_MEMORY_MIB` to fit a positive `u32`, and pass it to `Serve::with_max_memory`. Test missing, zero, malformed, overflowing and valid values.
- [x] Report failed logger installation through the approved, narrowly scoped stderr fallback; preserve successful-only setup and diagnostic flushing. Run the existing setup retry regression and inspect the direct stderr branch; avoid adding test-only indirection around a single output statement.
- [x] Extend the existing CIDR source test in `crates/trusted-server-core/src/integrations/datadome/protection_scope.rs` to assert configured cache keys remain unchanged across varied traffic and refreshes.
- [x] Correct `AppState` lifetime docs, configuration guidance and measurement limitations. Preserve provenance of historical measurements.
- [x] Prepare an updated PR body with the actual EdgeZero pin and missing commits; distinguish previous validation from checks run for these fixes.
- [x] Run target-matched regression tests, then the full CI gate list in `AGENTS.md`; report any environment blockers accurately. Review the final diff before handoff.

## Verification outcome

All `AGENTS.md` CI gates passed. Additional host CLI tests passed (532 passed,
18 ignored). After the final CIDR assertion adjustment, the targeted WASM test,
59 host DataDome tests, Fastly clippy and Rust formatting passed again. Independent
code review found no remaining issues.

The counter-privacy and missing-memory regressions failed before their fixes.
Deployed memory behavior and historical performance measurements were not rerun.
