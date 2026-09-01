# AGENTS.md

**Before doing anything else, read `CLAUDE.md` in this repository root.** It
contains all project conventions, coding standards, build commands, workflow
rules, and CI requirements. Everything in `CLAUDE.md` applies to you.

This file exists because Codex looks for `AGENTS.md` by convention. All shared
rules are maintained in `CLAUDE.md` to avoid duplication and drift. If you
cannot access `CLAUDE.md`, the critical rules are summarized below as a
fallback.

---

## Fallback Summary

If you cannot read `CLAUDE.md`, follow these rules:

1. Present a plan and get approval before coding.
2. Keep changes minimal — do not refactor unrelated code.

<!-- BEGIN GENERATED CI GATES: source CLAUDE.md#ci-gates -->

Every PR must pass:

1. `cargo fmt --all -- --check`
2. `cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare && cargo clippy-cloudflare-wasm && cargo clippy-spin-native && cargo clippy-spin-wasm`
3. `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin`
4. `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`
5. JS build and test (`cd crates/trusted-server-js/lib && npx vitest run`)
6. JS format (`cd crates/trusted-server-js/lib && npm run format`)
7. Docs format (`cd docs && npm run format`)
<!-- END GENERATED CI GATES -->

8. Use `error-stack` (`Report<E>`) for error handling — not anyhow, eyre, or thiserror.
9. Use `log` macros (not `println!`) and `expect("should ...")` (not `unwrap()`).
10. Target is `wasm32-wasip1` — no Tokio or OS-specific dependencies in core crates.
