# trusted-server-adapter-spin

Experimental Fermyon Spin adapter for Trusted Server. The component targets
`wasm32-wasip1` with the `spin` feature; native builds exercise route and
platform tests.

The crate maps Spin HTTP, variables, and the `default` key-value store into the
shared runtime. Startup failures install a restricted 503 router that keeps
`/health` live. The adapter permits one enabled auction provider. Request-time
KV services beyond startup config loading remain unavailable. Its entry point
uses `anyhow::Result` only because the EdgeZero Spin FFI requires that type.

Build and test from the repository root:

```bash
cargo build --package trusted-server-adapter-spin --target wasm32-wasip1 --features spin --release
cargo test-spin
```

Run `./scripts/smoke-spin.sh` for the isolated config, encoded-secret, failure,
and publisher-response checks. See the
[Spin deployment guide](../../docs/guide/spin.md) for the exact local-store
mapping and operational limitations.
