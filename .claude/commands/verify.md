Full verification: build, test, and lint the entire project.

1. `cargo build --workspace`
2. `cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1`
3. `cargo fmt --all -- --check`
4. `cargo clippy --workspace --all-targets --all-features -- -D warnings`
5. `cargo test --workspace`
6. `cd crates/trusted-server-js/lib && npx vitest run`
7. `cd crates/trusted-server-js/lib && npm run format`
8. `cd docs && npm run format`

Report results for each step. Stop and investigate if any step fails.
