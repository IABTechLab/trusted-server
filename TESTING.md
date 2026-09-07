# Testing

This file is the repository test-matrix index. Use the target-specific Cargo
aliases in `.cargo/config.toml`; a bare `cargo test --workspace` is not a valid
cross-adapter gate because the workspace contains incompatible native and WASM
targets.

## Required local gates

| Surface                | Command                                                                                                                               | Runtime or target                |
| ---------------------- | ------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------- |
| Rust formatting        | `cargo fmt --all -- --check`                                                                                                          | All workspace Rust sources       |
| Fastly and shared core | `cargo test-fastly`                                                                                                                   | `wasm32-wasip1` through Viceroy  |
| Axum adapter           | `cargo test-axum`                                                                                                                     | Native host                      |
| Cloudflare adapter     | `cargo test-cloudflare`                                                                                                               | Native host test configuration   |
| Spin adapter           | `cargo test-spin`                                                                                                                     | Native host test configuration   |
| Cross-adapter parity   | `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`                                         | Native host                      |
| Operator CLI           | `./scripts/test-cli.sh`                                                                                                               | Explicit native host target      |
| TSJS                   | `cd crates/trusted-server-js/lib && npm ci && npm run lint && npm run format && npx vitest run && npm run build`                      | Node version in `.tool-versions` |
| Documentation site     | `cd docs && npm ci && npm run lint && npm run format && npm run build`                                                                | Node version in `.tool-versions` |
| Documentation parity   | `cargo test --manifest-path tools/docs-parity/Cargo.toml` and `cargo run --manifest-path tools/docs-parity/Cargo.toml -- check --all` | Native host, offline checks      |

Run the matching clippy aliases before opening a pull request:

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

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
