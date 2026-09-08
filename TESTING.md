# Testing

This file is the repository test-matrix index. Use the target-specific Cargo
aliases in `.cargo/config.toml`; a bare `cargo test --workspace` is not a valid
cross-adapter gate because the workspace contains incompatible native and WASM
targets.

## Required local gates

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

Every clippy alias denies warnings. The WASM checks require the targets listed
in `.tool-versions`; Fastly tests also require the pinned Viceroy version.

## End-to-end tests

`./scripts/integration-tests.sh` builds the Fastly WASM adapter and native Axum
adapter, provisions generated Viceroy configuration, builds the WordPress and
Next.js fixtures, and runs the ignored integration suite serially. Docker,
Viceroy, and `wasm32-wasip1` are prerequisites.

Pass test filters after the script name to narrow a run:

```bash
./scripts/integration-tests.sh test_wordpress_axum
./scripts/integration-tests.sh test_nextjs_fastly
```

## Focused runbooks

- [Public testing guide](docs/guide/testing.md)
- [Auction testing](docs/guide/auction-testing.md)
- [Integration-test crate](crates/trusted-server-integration-tests/README.md)

CI remains authoritative for the complete operating-system and target matrix.
