Run all CI checks locally, in order. Stop and report if any step fails.

1. `cargo fmt --all -- --check`
2. `cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare`
3. `cargo test-fastly && cargo test-axum && cargo test-cloudflare`
4. `cargo check-cloudflare` (wasm32-unknown-unknown target check, mirrors CI)
5. `cd crates/js/lib && npx vitest run`
6. `cd crates/js/lib && npm run format`
7. `cd docs && npm run format`

Report a summary of all results when done.
