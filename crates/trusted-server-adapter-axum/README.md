# trusted-server-adapter-axum

Native adapter for Trusted Server. It runs the shared application through Axum
on the host, without an edge simulator, for local development and as the
production entry point. The shared core identity app uses an EdgeZero-prepared
record service for Edge Cookies, partner APIs and body EID persistence.

The crate owns application route registration, middleware and immutable
config/secret snapshots. EdgeZero owns native hosting, prepared providers,
outbound HTTP, admission and shutdown. Generic platform KV operations remain
unavailable; live identity records use the separate prepared record capability.
The logical record store is declared unconditionally, so its local provider
requires a writable `EDGEZERO__ADAPTER__DATA_DIR`.

Before listening, the native initializer reads the declared prepared stores,
validates one bounded config envelope and its active secret references, and
installs the real router. Missing or invalid inputs stop the process with a
non-zero exit. Local development uses an isolated EdgeZero config-store map and
`EDGEZERO__SECRETS__*` values. Legacy `TRUSTED_SERVER_CONFIG_*` and
`TRUSTED_SERVER_SECRET_*` variables cannot rescue missing prepared inputs.
See [Axum production startup](../../docs/guide/native-rc-demo.md) for the
AWS providers and their checks.

The binary runs through EdgeZero's Axum production startup,
`run_production_app_with_options_and_initializer`, which installs the logger and binds the
listener; the adapter installs no logger of its own. The log level comes from
`EDGEZERO__LOGGING__LEVEL` (default `info`) and the listener address from
`EDGEZERO__ADAPTER__HOST` and `EDGEZERO__ADAPTER__PORT` (default
`127.0.0.1:8787`); an invalid value stops startup. The listener's ingress
limits come from `EDGEZERO__ADAPTER__INGRESS__*` environment variables, and an
invalid value also stops startup. The
[Axum development guide](../../docs/guide/axum-dev.md#ingress-limits) lists
them.

Build and test from the repository root:

```bash
cargo build-axum
cargo test-axum
```

Run the isolated first-success check with `./scripts/smoke-axum.sh`. See the
[Axum development guide](../../docs/guide/axum-dev.md) for its config and
secret handoff, negative cases, success oracle, and cleanup behavior. Shared
runtime behavior belongs in
[`trusted-server-core`](../trusted-server-core/README.md), not this adapter.
