# Testing

Trusted Server spans native and WebAssembly targets. Use the repository's
target-specific commands; `cargo test --workspace` is not a valid gate because
the adapters do not share one compilation target.

## Testing absolute creative rewrite URLs

Creative rewrite tests should configure a distinct `publisher.public_origin`
and assert the parsed URL origin, not only a `/first-party/...` path substring.
Cover a deployment origin with a port and direct local development at
`http://localhost:8080`. Keep old-config deserialization and omitted-field
fallback tests separate from authority assertions. The fallback is
`https://{publisher.domain}`, not the serving page's scheme, host, or port.
Keep manual root-relative proxy and click inputs for compatibility tests;
rewritten creative output and sign/rebuild responses should be absolute. Test
the publisher SSAT/page-bids request-origin override separately. Existing TSJS
source selection and bundle hashes should remain unchanged.

## Required gates

[AGENTS.md](https://github.com/IABTechLab/trusted-server/blob/main/AGENTS.md#ci-gates)
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
cargo install viceroy --version "$(awk '$1 == "viceroy" { print $2 }' .tool-versions)" --locked --force
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

## HTML EOF runtime checks

The EOF harness builds fresh `test-utils` artifacts, creates isolated configuration
and runs disabled, successful, hung and enabled-auction controls against a loopback
origin. It uses the native Rust integration-test target and requires Rust and pinned Node. Compiler/profile
settings and an explicit `CARGO_TARGET_DIR` are preserved; operator configuration and `CARGO_BUILD_TARGET` are
not inherited because the harness selects native and adapter targets. Logs, bodies, headers and timing JSON remain in its printed temporary
directory. Owned process groups are stopped on normal exit, failure, SIGTERM and SIGINT.
The auction control deliberately uses an unreachable bidder; it checks empty-bid
script assembly after finalized output, rather than successful or delayed bids.

```bash
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --features html-eof-runtime --test html_eof_runtime
./scripts/async-html-finalization-local-test.sh axum --auction
./scripts/async-html-finalization-local-test.sh cloudflare --release --auction
./scripts/async-html-finalization-local-test.sh spin --release --auction
```

Workers requires `worker-build` and `wasm-bindgen` matching the locked SDK plus Wrangler.
Spin requires the Spin CLI. `--release` selects production-profile fixture artifacts.

The Fastly EOF timeout fixture requires **Viceroy 0.19 or newer** for backend timeout
simulation. The repository-pinned Viceroy meets this requirement and is shared with
ordinary WASI tests. Install it with the command above, then run:

```bash
./scripts/async-html-finalization-local-test.sh fastly --release --auction
```

The harness checks this prerequisite before building adapter fixtures. Fastly additionally proves
that an actual body byte arrives before a non-retaining EOF lookup completes.
CI runs Axum, native Rust helper tests and lint, and an isolated Fastly EOF job. Workers/Spin
actual EOF timeout controls remain local commands; their target builds, native tests
and shared parity controls run in CI. Hosted results are separate from local runs.

## Documentation checks

Check the documentation surfaces from the repository root:

```bash
# Lint, format-check, and build the VitePress site (fails on dead links)
cd docs && npm ci && npm run lint && npm run format && npm run build

# Compile the maintained documentation snippet against the current core
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test documentation_snippets
```

The site build also runs on every pull request through the `format-docs` job.

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
cargo test-axum test_name -- --nocapture --test-threads=1
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
