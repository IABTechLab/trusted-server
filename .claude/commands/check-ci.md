Run all CI checks locally, in order. Stop and report if any step fails.

1. `cargo fmt --all -- --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test --workspace`
4. `cd crates/js/lib && npx vitest run`

Report a summary of all results when done.
