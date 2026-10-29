Run all CI checks locally, in order. Stop and report if any step fails.

1. `cargo fmt --all -- --check`
2. `cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare && cargo clippy-cloudflare-wasm && scripts/lint-wasm-clock.sh`
3. `cargo test-fastly && cargo test-axum && cargo test-cloudflare`
4. `cargo check-cloudflare` (wasm32-unknown-unknown target check, mirrors CI)
5. `cd crates/trusted-server-js/lib && npx vitest run`
6. `cd crates/trusted-server-js/lib && npm run format`
7. `cd docs && npm run format`
8. `cargo run --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')" --bin ts -- dev lint domains --changed-vs origin/main` (use the PR's base branch in place of `main` when it targets another branch)
9. `node --test scripts/check-ci-node-pin.test.mjs && node scripts/check-ci-node-pin.mjs`

Report a summary of all results when done.
