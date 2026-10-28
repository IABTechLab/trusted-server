# Cloudflare Workers deployment

The Cloudflare adapter runs the shared Trusted Server router in a Worker. For
each request the Worker reads its settings from its config store, the Workers
KV namespace that `ts config push --adapter cloudflare` writes, and builds the
router from them.

## Support status

| Adapter      | Release status | Health | Startup status | Startup health | Provider fan-out | Trusted-client-IP handling | Request normalization |
| ------------ | -------------- | ------ | -------------- | -------------- | ---------------- | -------------------------- | --------------------- |
| `cloudflare` | development    | absent | `500`          | no             | single           | outermost sanitize         | none                  |

This row is the canonical status summary. Cloudflare has no
`/health` route, supports one enabled auction provider, and returns 500 on
every route when startup fails.

## The config store

The config store is a KV namespace of its own, separate from the
`TRUSTED_SERVER_KV` namespace. `wrangler.toml` binds it as
`trusted_server_config`, the store's default platform name. To bind another
name, set `EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME` both where
`ts config push` runs and in the Worker's variables, so the push and the
Worker open the same namespace. `ts provision --adapter cloudflare` creates
the namespace and writes its id into `wrangler.toml`.

The settings blob is stored at the key `trusted_server_config`, unless
`EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__KEY` names another. A Worker
whose binding is missing, or whose namespace holds no blob at that key,
answers 500 on every route.

## Verify the complete local handoff

Install the Wrangler version pinned in `.tool-versions`, then run:

```bash
./scripts/smoke-cloudflare.sh
```

The smoke refuses an absent pin or a different Wrangler executable. It creates
an isolated Worker manifest and launches `wrangler dev` before any settings
are pushed, which must answer 500 because the config store holds no blob. It
then runs `ts config push --adapter cloudflare --local` with the repository's
sample permission policy through `--permissions`, which writes the blob
into the local `trusted_server_config` namespace, and reads it back with
`wrangler kv key get`.

The generated local Wrangler files define `publisher_proxy_secret` and
`ec_passphrase` separately. Edge Cookie identity is off by default, so the
smoke's settings select the built-in `hmac` module, whose passphrase is the
`ec_passphrase` secret. The smoke launches
`wrangler dev` once for each omitted secret. Each case requires a 500 response
and verifies the exact missing binding against Wrangler's runtime binding
inventory. The Worker installs no logger, so no case waits for a log line. The
positive publisher request must return 200, include the stub-origin sentinel,
rewrite its origin URL to the Worker listener, and omit the original URL.

The exit trap stops Wrangler and the stub origin and removes the isolated KV,
generated Wrangler manifests, response captures, and logs. It does not modify
the checked-in manifests.

## Configure a deployed Worker

Local `[vars]` are only for the isolated smoke. For a deployed Worker, run
`ts provision --adapter cloudflare` once, then `ts config push --adapter
cloudflare --permissions <file>`, which writes the settings and the policy file
they name into the remote config store. Store the two credential values with
`wrangler secret put publisher_proxy_secret` and `wrangler secret put
ec_passphrase`; do not commit them to a Wrangler manifest. Deploy only after
the config store holds the blob, the two secrets are set and the
`TRUSTED_SERVER_KV` namespace is bound.

The adapter declares `TRUSTED_SERVER_KV` as its key-value store, so EdgeZero
opens it for every request, and a loading cache module that a deployment links
keeps its copies there between requests, while a copy in memory lasts one
request because the Worker builds its state for each request. EdgeZero treats
a declared store as required, so a Worker deployed without the binding answers
every request with `500` before any route runs. No request-time config store
is declared, and EC KV lookup is not available.
Forwarded client-IP headers are sanitized, but `[trusted_client_ip]` is not
resolved. See the [API reference](./api-reference) for route-level behavior.

The Worker sends no [auction telemetry](./telemetry#adapter-support), and warns
at startup when `[analytics]` selects a module.

Compare the [Fastly](./fastly), [Spin](./spin), and [Axum](./axum-dev)
adapter journeys.
