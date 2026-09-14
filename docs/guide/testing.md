# Testing

Trusted Server spans native and WebAssembly targets. Use the repository's
target-specific commands; `cargo test --workspace` is not a valid gate because
the adapters do not share one compilation target.

## Required gates

[CLAUDE.md](https://github.com/IABTechLab/trusted-server/blob/main/CLAUDE.md#ci-gates)
is the single source of truth for the complete local and CI gate matrix. Run
the checks that match the files you change. Pull-request CI covers the full
supported operating-system and target matrix.

## Adapter test targets

The aliases in `.cargo/config.toml` select the correct target and feature set:

| Adapter    | Test command            | Execution target                                         |
| ---------- | ----------------------- | -------------------------------------------------------- |
| Fastly     | `cargo test-fastly`     | `wasm32-wasip1` under Viceroy                            |
| Axum       | `cargo test-axum`       | Native host                                              |
| Cloudflare | `cargo test-cloudflare` | Native host tests; production builds use WASM            |
| Spin       | `cargo test-spin`       | Native host tests; production builds use `wasm32-wasip1` |

Fastly tests require the Viceroy version recorded in `.tool-versions`:

```bash
cargo install viceroy --version 0.17.0 --locked --force
rustup target add wasm32-wasip1 wasm32-unknown-unknown
```

Pass a test-name filter after an alias to narrow a run:

```bash
cargo test-fastly test_generate_ec_id
cargo test-axum sets_geo_unavailable_header
```

Tests live beside their implementations in `#[cfg(test)]` modules. Read the
source test instead of copying it into another document; for example, EC ID
tests are in `crates/trusted-server-core/src/edge_cookie.rs`, creative rewrite
tests are in `crates/trusted-server-core/src/creative.rs`, and proxy tests are
in `crates/trusted-server-core/src/proxy.rs`.

## Documentation checks

Run the documentation-specific aggregate from the repository root:

```bash
./scripts/check-documentation.sh
```

It installs the locked Node dependencies, lints and builds the VitePress site,
lints the exported TypeScript library, compiles the maintained documentation
snippet, runs core doctests, and builds rustdoc with warnings denied for the
core, adapters, CLI, and code generator. It does not rewrite tracked files.

Maintainers can run the same command in GitHub from
**Actions → Documentation checks → Run workflow**. The workflow is manual and
is not a required core or adapter check.

## Cross-adapter parity and snippets

The native integration-test crate compares shared router behavior and compiles
the maintained integration-guide fixture:

```bash
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test documentation_snippets
```

The snippet gate covers the marked fixture in the integration guide. Other
Rust examples must either be rustdoc tests or link to their source tests; prose
pages must not present copied implementation fragments as compiled examples.

## End-to-end publisher tests

The end-to-end script builds the Fastly and Axum adapters, creates generated
Viceroy configuration under `target/integration-test-artifacts`, builds the
WordPress and Next.js Docker fixtures, and runs the ignored integration suite
serially:

```bash
./scripts/integration-tests.sh
```

Pass a test filter to run one path:

```bash
./scripts/integration-tests.sh test_wordpress_axum
./scripts/integration-tests.sh test_nextjs_fastly
```

Browser coverage uses Playwright and Chromium against both fixtures:

```bash
./scripts/integration-tests-browser.sh
```

These suites require Docker, the pinned Node.js version, Viceroy, and the
`wasm32-wasip1` target. See the
[integration-test crate README](https://github.com/IABTechLab/trusted-server/blob/main/crates/trusted-server-integration-tests/README.md)
for the exact surfaces and retained build artifacts.

## Adapter runtime smokes

Each smoke script starts a real local runtime in temporary state. It verifies
missing configuration, each required secret, and a successful publisher proxy
request with URL rewriting:

```bash
./scripts/smoke-axum.sh
./scripts/smoke-fastly.sh
./scripts/smoke-cloudflare.sh
./scripts/smoke-spin.sh
```

The scripts build a missing adapter artifact when necessary. They require the
corresponding runtime CLI: Fastly CLI for Fastly, the pinned Wrangler release
for Cloudflare, and Spin CLI for Spin. Default ports can be overridden through
the environment variables documented in `scripts/README.md`.

## Debugging a failure

Show captured output and serialize a focused test when ordering or shared test
state matters:

```bash
RUST_LOG=debug cargo test-axum test_name -- --nocapture --test-threads=1
cargo test-fastly test_name -- --nocapture --test-threads=1
```

Viceroy reports the guest test output but may not provide a native-style stack
trace. Narrow the test filter first, then add temporary `log` diagnostics near
the failing boundary. Do not use `println!` in production code.

## Related guides

- [Auction testing](/guide/auction-testing)
- [Architecture](/guide/architecture)
- [Configuration](/guide/configuration)
- [Request signing](/guide/request-signing)
