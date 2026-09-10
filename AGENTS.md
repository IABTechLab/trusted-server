# AGENTS.md

**Before doing anything else, read `CLAUDE.md` in this repository root.** It
contains all project conventions, coding standards, build commands, workflow
rules, and CI requirements. Everything in `CLAUDE.md` applies to you.

This file exists because Codex looks for `AGENTS.md` by convention. All shared
rules are maintained in `CLAUDE.md` to avoid duplication and drift. If you
cannot access `CLAUDE.md`, the critical rules are summarized below as a
fallback.

---

## CI Gates

<!-- docs-parity:gates:start -->

### Documentation parity

<!-- docs-parity:owner:documentation-maintainers -->

- `cargo fmt --manifest-path tools/docs-parity/Cargo.toml -- --check`
- `cargo clippy --manifest-path tools/docs-parity/Cargo.toml --all-targets --all-features -- -D warnings`
- `cargo test --manifest-path tools/docs-parity/Cargo.toml`
- `cargo run --manifest-path tools/docs-parity/Cargo.toml -- check --all`
- `cargo run --manifest-path tools/docs-parity/Cargo.toml -- generate --check`

### JavaScript and documentation site

<!-- docs-parity:owner:documentation-maintainers -->

- `cd crates/trusted-server-js/lib && npm ci && npm run lint && npx vitest run && npm run format && npm run build`
- `cd docs && npm ci && npm run lint && npm run format && npm run build`

### Rust formatting and linting

<!-- docs-parity:owner:rust-maintainers -->

- `cargo fmt --all -- --check`
- `cargo clippy-fastly`
- `cargo clippy-axum`
- `cargo clippy-cloudflare`
- `cargo clippy-cloudflare-wasm`
- `cargo clippy-spin-native`
- `cargo clippy-spin-wasm`
- `cargo clippy --package trusted-server-cli --target $(rustc -vV | sed -n 's/host: //p') --all-targets --all-features -- -D warnings`
- `cargo clippy --package trusted-server-openrtb-codegen --target $(rustc -vV | sed -n 's/host: //p') --all-targets -- -D warnings`
- `cargo fmt --manifest-path crates/trusted-server-integration-tests/Cargo.toml -- --check`
- `cargo clippy --manifest-path crates/trusted-server-integration-tests/Cargo.toml --all-targets -- -D warnings`

### Rust tests and release builds

<!-- docs-parity:owner:rust-maintainers -->

- `cargo test-fastly`
- `cargo test-axum`
- `cargo test-cloudflare`
- `cargo test-spin`
- `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`
- `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test documentation_snippets`
- `./scripts/test-cli.sh`
- `cargo test --package trusted-server-openrtb-codegen --target $(rustc -vV | sed -n 's/host: //p')`
- `cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1`
- `cargo build --package trusted-server-adapter-spin --target wasm32-wasip1 --features spin --release`

### Rust API documentation

<!-- docs-parity:owner:documentation-maintainers -->

- `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features -p trusted-server-core -p trusted-server-js -p trusted-server-openrtb --target wasm32-wasip1`
- `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps -p trusted-server-adapter-fastly --target wasm32-wasip1`
- `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps -p trusted-server-adapter-cloudflare --target wasm32-unknown-unknown --features cloudflare`
- `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps -p trusted-server-adapter-spin --target wasm32-wasip1 --features spin`
- `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features -p trusted-server-adapter-axum`
- `RUSTDOCFLAGS='-D warnings' cargo doc --no-deps --all-features -p trusted-server-cli -p trusted-server-openrtb-codegen --target $(rustc -vV | sed -n 's/host: //p')`
- `cargo test --doc -p trusted-server-core`

<!-- docs-parity:gates:end -->

## Fallback Summary

If you cannot read `CLAUDE.md`, follow these rules:

1. Present a plan and get approval before coding.
2. Keep changes minimal — do not refactor unrelated code.
3. Use `error-stack` (`Report<E>`) for error handling — not anyhow, eyre, or thiserror.
4. Use `log` macros (not `println!`) and `expect("should ...")` (not `unwrap()`).
5. Target is `wasm32-wasip1` — no Tokio or OS-specific dependencies in core crates.
