# trusted-server-adapter-axum

Native development adapter for Trusted Server. It runs the shared application
through Axum on the host, without an edge simulator, and is not a production
deployment target.

The crate owns application route registration, middleware and immutable
config/secret snapshots. EdgeZero owns native hosting, prepared providers,
outbound HTTP, admission and shutdown. Generic platform KV operations remain
unavailable; config/secret snapshots are not live identity storage.

Before listening, the native initializer reads the declared prepared stores,
validates one bounded config envelope and its active secret references, and
installs the real router. Missing or invalid inputs stop the process with a
non-zero exit. Local development uses an isolated EdgeZero config-store map and
`EDGEZERO__SECRETS__*` values. Legacy `TRUSTED_SERVER_CONFIG_*` and
`TRUSTED_SERVER_SECRET_*` variables cannot rescue missing prepared inputs.
See the [native RC guide](../../docs/guide/native-rc-demo.md) for the
production-style hosting and provider checks.

The listener's ingress limits come from `EDGEZERO__ADAPTER__INGRESS__*`
environment variables, and an invalid value also stops startup. The
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
