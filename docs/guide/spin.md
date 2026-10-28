# Fermyon Spin deployment

The Spin adapter packages Trusted Server as a WASI component. Startup reads
the config blob from Spin's `default` key-value store and resolves credentials
through Spin variables.

## Support status

| Adapter | Release status | Health | Startup status | Startup health | Provider fan-out | Trusted-client-IP handling | Request normalization            |
| ------- | -------------- | ------ | -------------- | -------------- | ---------------- | -------------------------- | -------------------------------- |
| `spin`  | experimental   | real   | `500`          | yes            | single           | outermost sanitize         | innermost Spin-header derivation |

This row is the canonical status summary. Spin supports one enabled
auction provider. When startup fails, its hardened router preserves `/health`
but answers publisher routes with the status of the startup error's kind,
500 for a configuration error; neither response proves successful
configuration.

## Prerequisites

Spin 4.1.0 or newer, pinned as `spin` in `.tool-versions`. `spin-sdk` 7 imports
`wasi:http/types@0.3.0`, which Spin 4.0.x does not provide, so an older runtime
cannot start the component. Install or upgrade from the
[Spin install guide](https://spinframework.dev/install).

asdf has no first-party Spin plugin, so `asdf install` does not provide this
one; install it directly, or add a community plugin if you want asdf to manage
it. The pin records the required version and is what the smoke checks against.

Nothing in the build catches this. The component compiles, `cargo
test-spin` passes, and only `spin up` reports:

```
Error: component imports instance `wasi:http/types@0.3.0`, but a
matching implementation was not found in the linker
```

`./scripts/smoke-spin.sh` checks the runtime version before it starts the app,
so it fails with a clear message rather than that linker error.

## Verify the complete local handoff

With Spin 4.1.0 or newer installed, run:

```bash
./scripts/smoke-spin.sh
```

The script creates an isolated manifest and strictly validates a fresh app
config, which names the repository's sample permission policy. It maps the
logical EdgeZero store with
`EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME=default`, then runs `ts
config push --adapter spin --local` with the sample through `--permissions`.
Without that mapping, the push can land in a store the runtime never reads.
Local push writes the temporary `.spin/sqlite_key_value.db`; no
repository-local Spin state is used.

Spin encodes `v_<store>_v_<key>` names. The smoke supplies these exact
variables:

- `v_trusted_x5fserver_x5fsecrets_v_publisher_x5fproxy_x5fsecret`
- `v_trusted_x5fserver_x5fsecrets_v_ec_x5fpassphrase`

It first launches `spin up` before config is pushed, then relaunches it while
omitting each variable independently. Every publisher request must return 500.
The component installs no logger, so the smoke checks these cases by their
status alone.

The final non-health publisher request must return 200, retain the stub-origin
sentinel, rewrite its origin URL to the Spin listener, and omit the original
URL. The exit trap stops Spin and the stub origin and removes the isolated
manifest, SQLite store, response captures, and logs.

## Runtime boundaries

Use the same store mapping and encoded variables in the deployment provider;
empty variable defaults fail closed. The adapter declares Spin's `default`
key-value store, the store startup reads the settings from, so EdgeZero opens it
for every request, and a loading cache module that a deployment links keeps its
copies there between requests, while a copy in memory lasts one request because
Spin builds the state for each request. EdgeZero treats a declared store as
required, so a component that cannot open `default` fails every request before
any route runs, `/health` included. No request-time config store is declared,
so request signing cannot read its key metadata, and EC KV lookup is not
available. Forwarded client-IP
headers are sanitized, and Spin derives its
runtime authority, scheme, and client-address headers in the innermost
normalization layer. Spin has no geo lookup, so `[geo] module = "platform"` is
refused at startup.

Spin sends no [auction telemetry](./telemetry#adapter-support), and warns at
startup when `[analytics]` selects a module.

Spin is not installed in the integration CI environment. The recurring manual
smoke therefore needs a named owner, tested commit, exact tool versions, and an
expiry; those details are recorded with each run in the documentation-refresh
evidence ledger.

Compare the [Fastly](./fastly), [Cloudflare](./cloudflare), and
[Axum](./axum-dev) adapter journeys.
