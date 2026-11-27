# trusted-server-adapter-axum

Native development adapter for Trusted Server. It runs the shared application
through Axum on the host, without an edge simulator, and is not a production
deployment target.

The crate owns application route registration, middleware and immutable
config/secret snapshots. EdgeZero owns native hosting, prepared providers,
outbound HTTP, admission and shutdown. Generic platform KV operations remain
unavailable; config/secret snapshots are not live identity storage. Its
`/health`, `/ready`, and startup-failure responses follow the shared
[operational probe contract](../../docs/guide/api-reference.md#operational-probe-contract).

Before listening, the native initializer reads the declared prepared stores,
validates one bounded config envelope and its active secret references, and
installs the real router. A missing or invalid config value or secret, or
settings the application cannot start from, installs the failure router
instead: the process listens, `/health` stays `200`, and `/ready` and every
supported route answer `503`. A store the selected provider cannot prepare,
such as a missing local config file, stops the process with a non-zero exit
before it listens. Local development uses an isolated EdgeZero config-store
map and `EDGEZERO__SECRETS__*` values. Legacy `TRUSTED_SERVER_CONFIG_*` and
`TRUSTED_SERVER_SECRET_*` variables cannot rescue missing prepared inputs.
See the [native RC guide](../../docs/guide/native-rc-demo.md) for the
production-style hosting and provider checks.

The binary runs through EdgeZero's Axum production startup,
`run_production_app_with_initializer`, which installs the logger and binds the
listener; the adapter installs no logger of its own. The log level comes from
`EDGEZERO__LOGGING__LEVEL` (default `info`) and the listener address from
`EDGEZERO__ADAPTER__HOST` and `EDGEZERO__ADAPTER__PORT` (default
`127.0.0.1:8787`); an invalid value stops the process with a non-zero exit
before it listens. The listener's ingress limits come from
`EDGEZERO__ADAPTER__INGRESS__*` environment variables, and an invalid value
also stops the process with a non-zero exit before it listens. The
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
