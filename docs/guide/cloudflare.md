# Cloudflare Workers deployment

The Cloudflare adapter runs the shared Trusted Server router in a Worker. The
current runtime requires an explicit local or deployment-time bridge from the
config that `ts config push` writes into KV to the nested
`TRUSTED_SERVER_CONFIG` variable consumed at startup.

## Support status

<!-- docs-parity:start adapter-support-cloudflare -->

| Adapter      | Release status | Health | Startup status | Startup health | Provider fan-out | Trusted-client-IP handling | Request normalization |
| ------------ | -------------- | ------ | -------------- | -------------- | ---------------- | -------------------------- | --------------------- |
| `cloudflare` | development    | absent | `500`          | no             | single           | outermost sanitize         | none                  |

<!-- docs-parity:end adapter-support-cloudflare -->

This generated row is the canonical status summary. Cloudflare has no
`/health` route, supports one enabled auction provider, and returns 500 on
every route when startup fails.

## Verify the complete local handoff

Install the Wrangler version pinned in `.tool-versions`, then run:

```bash
./scripts/smoke-cloudflare.sh
```

The smoke refuses an absent pin or a different Wrangler executable. It creates
an isolated Worker manifest, maps the EdgeZero config store to
`TRUSTED_SERVER_KV`, and runs `ts config push --adapter cloudflare --local`.
It reads `trusted_server_config` back with `wrangler kv key get` using both the
explicit binding and local mode. The returned envelope becomes the string
value of `app_config` in a JSON object; that outer JSON is assigned to the
`TRUSTED_SERVER_CONFIG` Worker variable. This double encoding is required by
the current startup reader.

The generated local Wrangler files define `handler_password`,
`publisher_proxy_secret`, and `ec_passphrase` separately. The smoke launches
`wrangler dev` once without the config binding and once for each omitted
secret. Each case requires a 500 response and verifies the exact missing
binding against Wrangler's runtime binding inventory. The positive publisher
request must return 200, include the stub-origin sentinel, rewrite its origin
URL to the Worker listener, and omit the original URL.

The exit trap stops Wrangler and the stub origin and removes the isolated KV,
generated Wrangler manifests, response captures, and logs. It does not modify
the checked-in manifests.

## Configure a deployed Worker

Local `[vars]` are only for the isolated smoke. For a deployed Worker, obtain
the validated envelope from the target KV namespace, construct the same nested
`TRUSTED_SERVER_CONFIG` JSON, and configure it in the deployment environment.
Store the three credential values with `wrangler secret put
handler_password`, `wrangler secret put publisher_proxy_secret`, and `wrangler
secret put ec_passphrase`; do not commit them to a Wrangler manifest. Deploy
only after all four bindings are present.

A successful `ts config push` alone does not configure this runtime: the
Worker currently does not open `TRUSTED_SERVER_KV` during startup. Request-time
store registries are also unwired, so EC KV lookup and key rotation are not
available. The read-only `/_ts/admin/eids` diagnostic remains available.
Forwarded client-IP headers are sanitized, but `[trusted_client_ip]` is not
resolved. See the [API reference](./api-reference) for route-level behavior.

Compare the [Fastly](./fastly), [Spin](./spin), and [Axum](./axum-dev)
adapter journeys.
