Full verification: build, test, and lint the entire project.

1. `cargo build --workspace`
2. `cargo build --bin trusted-server-fastly --release --target wasm32-wasip1`
3. `cargo fmt --all -- --check`
4. `cargo clippy --all-targets --all-features -- -D warnings`
5. `cargo test --workspace`
6. `cd crates/js/lib && npx vitest run`

Report results for each step. Stop and investigate if any step fails.
