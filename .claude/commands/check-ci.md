Run all CI checks locally, in order. Stop and report if any step fails.

1. `cargo fmt --all -- --check`
2. `cargo clippy-fastly && cargo clippy-axum`
3. `cargo test-fastly && cargo test-axum`
4. `cd crates/js/lib && npx vitest run`
5. `cd crates/js/lib && npm run format`
6. `cd docs && npm run format`

Report a summary of all results when done.
