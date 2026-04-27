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
3. Run tests after every code change — two commands are required because the workspace contains both WASM-only and native-only crates:
   - `cargo test --workspace --exclude trusted-server-adapter-axum --target wasm32-wasip1` (Fastly/core crates via Viceroy)
   - `cargo test -p trusted-server-adapter-axum` (Axum dev server, native)
4. Run `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --all-features -- -D warnings`.
5. Run JS tests with `cd crates/js/lib && npx vitest run` when touching JS/TS code.
6. Use `error-stack` (`Report<E>`) for error handling — not anyhow, eyre, or thiserror.
7. Use `log` macros (not `println!`) and `expect("should ...")` (not `unwrap()`).
8. Target is `wasm32-wasip1` — no Tokio or OS-specific dependencies in core crates.
