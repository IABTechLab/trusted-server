# trusted-server-adapter-fastly

Production Fastly Compute adapter for Trusted Server, targeting
`wasm32-wasip1`.

This crate owns the Fastly entry point, dynamic backend construction, edge
cache behavior, config and secret-store access, EC KV operations, rate limits,
streaming template assembly, and Fastly-only Tinybird auction emission. Its
`/health` response is served before application construction, so liveness does
not prove that configuration loaded. Platform-neutral routing, settings,
auctions, integrations, and rewrites remain in `trusted-server-core`.

Build and test from the repository root:

```bash
cargo build-fastly
cargo test-fastly
```

The test alias uses Viceroy for the WASM target. Run
`./scripts/smoke-fastly.sh` for the isolated config-store, secret-store, and
publisher-response handoff. See the
[Fastly deployment guide](../../docs/guide/fastly.md) for provisioning and
runtime boundaries and [Auction Telemetry](../../docs/guide/telemetry.md) for
the Fastly-only telemetry path.
