# Fermyon Spin deployment

The Spin adapter packages Trusted Server as a WASI component. Startup reads
the config blob from Spin's `default` key-value store and resolves credentials
through Spin variables.

## Support status

| Adapter | Release status | Health | Startup status | Startup health | Provider fan-out | Trusted-client-IP handling | Request normalization            |
| ------- | -------------- | ------ | -------------- | -------------- | ---------------- | -------------------------- | -------------------------------- |
| `spin`  | experimental   | real   | `503`          | yes            | single           | outermost sanitize         | innermost Spin-header derivation |

This row is the canonical status summary. Spin supports one enabled
auction provider. When startup fails, its hardened router preserves `/health`
but answers publisher routes with 503; neither response proves successful
configuration.

## Prerequisites

Spin 4.1.0 or newer, pinned as `spin` in `.tool-versions`. This branch temporarily
uses `spin-sdk` 6 to match the unreleased EdgeZero secret-provisioning APIs;
the runtime minimum remains unchanged. `spin-sdk` 7 imports
`wasi:http/types@0.3.0`, which Spin 4.0.x does not provide. Install or upgrade
from the [Spin releases](https://github.com/spinframework/spin/releases).

asdf has no first-party Spin plugin, so `asdf install` does not provide this
one; install it directly, or add a community plugin if you want asdf to manage
it. The pin records the required version and is what the smoke checks against.

With `spin-sdk` 7, the build does not catch an older runtime. The component
compiles, `cargo test-spin` passes, and only `spin up` reports:

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
config. It maps the logical EdgeZero store with
`EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME=default`, then runs `ts
config push --adapter spin --local`. Without that mapping, the push can land in
a store the runtime never reads. Local push writes the temporary
`.spin/sqlite_key_value.db`; no repository-local Spin state is used.

Generate exact names and both application/component declarations with
`ts secrets plan --adapter spin`. Follow the
[native variable walkthrough](./secrets#spin-native-variables). Merge component
entries into one table, retain required secret declarations without plaintext
defaults, and use protected provider input. `secret = true` alone does not
encrypt storage. The local smoke must use generated contexts, not old labels.

It first launches `spin up` before config is pushed, then relaunches it while
omitting each variable independently. Every publisher request must return 503,
and the controlled config or variable delta must identify the intended missing
input. A generic degraded response cannot satisfy the check.

The final non-health publisher request must return 200, retain the stub-origin
sentinel, rewrite its origin URL to the Spin listener, and omit the original
URL. The exit trap stops Spin and the stub origin and removes the isolated
manifest, SQLite store, response captures, and logs.

## Runtime boundaries

Use the same store mapping and generated declarations in the deployment provider.
Direct `set` and native presence `check` are unsupported; `check` exits nonzero.
Verify native component reads, provider/name limits, HTTPS/no-follow, and
config-writer permission separation before release. Host tests do not prove them. Spin's request-time store registry is
currently unwired, so EC KV lookup, request-signing key variables, and key
rotation are unavailable. The read-only `/_ts/admin/eids` diagnostic remains
available. Forwarded client-IP headers are sanitized, and Spin derives its
runtime authority, scheme, and client-address headers in the innermost
normalization layer.

CI installs the pinned Spin runtime for `scripts/secret-bindings-runtime.sh spin`.
This native contract check uses a `test-utils` integration to invoke the shared
credential exporter, because Spin does not currently dispatch S3 asset routes.
It verifies real variable reads, HTTPS, and no-follow behavior, not shipping
S3 routing or provider IAM. Keep the complete config-push handoff smoke and
deployment rehearsals separate; track release evidence in [#1479](https://github.com/IABTechLab/trusted-server/issues/1479).

Compare the [Fastly](./fastly), [Cloudflare](./cloudflare), and
[Axum](./axum-dev) adapter journeys.
