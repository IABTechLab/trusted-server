# Fermyon Spin deployment

The Spin adapter packages Trusted Server as a WASI component. Startup reads
the config blob from Spin's `default` key-value store and resolves credentials
through Spin variables.

## Support status

<!-- docs-parity:start adapter-support-spin -->

| Adapter | Release status | Health | Startup status | Startup health | Provider fan-out | Trusted-client-IP handling | Request normalization            |
| ------- | -------------- | ------ | -------------- | -------------- | ---------------- | -------------------------- | -------------------------------- |
| `spin`  | experimental   | real   | `503`          | yes            | single           | outermost sanitize         | innermost Spin-header derivation |

<!-- docs-parity:end adapter-support-spin -->

This generated row is the canonical status summary. Spin supports one enabled
auction provider. When startup fails, its hardened router preserves `/health`
but answers publisher routes with 503; neither response proves successful
configuration.

## Verify the complete local handoff

With the Spin CLI installed, run:

```bash
./scripts/smoke-spin.sh
```

The script creates an isolated manifest and strictly validates a fresh app
config. It maps the logical EdgeZero store with
`EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME=default`, then runs `ts
config push --adapter spin --local`. Without that mapping, the push can land in
a store the runtime never reads. Local push writes the temporary
`.spin/sqlite_key_value.db`; no repository-local Spin state is used.

Spin encodes `v_<store>_v_<key>` names. The smoke supplies these exact
variables:

- `v_trusted_x5fserver_x5fsecrets_v_handler_x5fpassword`
- `v_trusted_x5fserver_x5fsecrets_v_publisher_x5fproxy_x5fsecret`
- `v_trusted_x5fserver_x5fsecrets_v_ec_x5fpassphrase`

It first launches `spin up` before config is pushed, then relaunches it while
omitting each variable independently. Every publisher request must return 503,
and the controlled config or variable delta must identify the intended missing
input. A generic degraded response cannot satisfy the check.

The final non-health publisher request must return 200, retain the stub-origin
sentinel, rewrite its origin URL to the Spin listener, and omit the original
URL. The exit trap stops Spin and the stub origin and removes the isolated
manifest, SQLite store, response captures, and logs.

## Runtime boundaries

Use the same store mapping and encoded variables in the deployment provider;
empty variable defaults fail closed. Spin's request-time store registry is
currently unwired, so EC KV lookup, request-signing key variables, and key
rotation are unavailable. The read-only `/_ts/admin/eids` diagnostic remains
available. Forwarded client-IP headers are sanitized, and Spin derives its
runtime authority, scheme, and client-address headers in the innermost
normalization layer.

Spin is not installed in the integration CI environment. The recurring manual
smoke therefore needs a named owner, tested commit, exact tool versions, and an
expiry; those details are recorded with each run in the documentation-refresh
evidence ledger.

Compare the [Fastly](./fastly), [Cloudflare](./cloudflare), and
[Axum](./axum-dev) adapter journeys.
