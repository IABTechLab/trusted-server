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
3. Run `cargo test --workspace` after every code change.
4. Run `cargo fmt --all -- --check` and `cargo clippy --all-targets --all-features -- -D warnings`.
5. Run JS tests with `cd crates/js/lib && npx vitest run` when touching JS/TS code.
6. Use `error-stack` (`Report<E>`) for error handling — not anyhow, eyre, or thiserror.
7. Use `tracing` macros (not `println!`) and `expect("should ...")` (not `unwrap()`).
8. Target is `wasm32-wasip1` — no Tokio or OS-specific dependencies in core crates.
