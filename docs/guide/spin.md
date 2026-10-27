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

## Logging

The component installs its own logger, because EdgeZero installs none on
Spin. Each line goes to standard error with its level and module first, such
as `WARN [trusted_server_adapter_spin::app] ...`.
[`spin up`](https://spinframework.dev/v4/running-apps) prints a component's
standard error in its console and, for an application run from a local
manifest, keeps a copy in the `.spin/logs` folder beside the manifest, or in
the folder `--log-dir` names. A host that runs the component another way
decides where its standard error goes. The component writes `info` and above,
and no setting changes the level.

A warning the component logs once, such as the one about a route that
enables the Image Optimizer, is logged once for each instance of the
component. Spin can serve several requests from one instance, one after
another and at the same time, to a degree it does not guarantee
([HTTP trigger](https://spinframework.dev/v4/http-trigger),
`spin up --max-instance-reuse-count`), so the warning appears each time Spin
starts an instance, which is every request when an instance serves only one.

## Runtime boundaries

Use the same store mapping and encoded variables in the deployment provider;
empty variable defaults fail closed. The adapter declares Spin's `default`
key-value store, the store startup reads the settings from, so EdgeZero opens it
for every request and the loading cache keeps its copies there between requests,
while a copy in memory lasts one request because Spin builds the state for each
request. EdgeZero treats a declared store as required, so a component that
cannot open `default` fails every request before any route runs, `/health`
included. No request-time config store is declared, so request signing cannot
read its key metadata, and EC KV lookup is not available. Forwarded client-IP
headers are sanitized, and Spin derives its
runtime authority, scheme, and client-address headers in the innermost
normalization layer. Spin has no geo lookup, so `[geo] module = "platform"` is
refused at startup. The modules' request filters and the device module
`[device]` selects run as on Fastly, and `[device] module = "platform"`
classifies from the `User-Agent`, because Spin supplies no TLS or HTTP/2
signals. An Edge Cookie module creates and writes its cookie with no identity
graph behind it, and `POST /_ts/api/v1/ec/resolve` sets one the same way.
`[[proxy.asset_routes]]` are served from each route's own source. The HTTP
client passes a streamed origin body on one host chunk at a time, exactly as
the origin encoded it and held to the caller's byte limit if it sets one, and
the component sends a streamed response to the client as it is produced. A
buffered body is held whole, up to 8 MiB, and a larger one answers `502`. Spin has
no Image Optimizer, so the component warns about each route that enables one,
once for each instance (see [Logging](#logging)), and each request such a
route would optimize answers `502` (see [Asset Routes](./asset-routes)). Spin
sends the address's own host as the `Host`, so a request to a source whose
`host_header` names another host, a route's or the publisher's, answers `502`
before its origin is asked, and the component logs the refusal as an error
that names both hosts.

For bounded outbound calls such as Encypher signing, the adapter consumes
Spin SDK 6's `IncomingBodyExt::stream()` rather than `bytes()`. It checks each
host-created chunk before copying it into the buffered response and applies
the same bound again to gzip or Brotli output. The host chunk already exists
when guest code receives it, but the adapter does not collect the whole body
before enforcing the bound. A streamed response reads the same stream one
chunk at a time and decodes nothing, and a streamed body that ends short of
its `Content-Length` ends with an error.

The component's entry point dispatches through `EdgeZero`'s router itself,
where it called `edgezero_adapter_spin::run_app`, because that wrapper
collects a streamed response into one buffer, capped at 16 MiB, before the
host sees it. It opens the declared KV stores as `run_app` does, with the
same `EDGEZERO__STORES__KV__<ID>__NAME` and `MAX_LIST_KEYS` settings, and a
store it cannot open fails the request.

### Retaining and removing Encypher cache entries

Spin's key-value API has no entry TTL. Encypher records an `expires_at` value,
ignores an expired entry and deletes it best effort when that key is read
again. This is not garbage collection: a content key that no request reads
again remains in `default`.

The active namespaces are `encypher:plan:v4:`,
`encypher:plan-skip:v4:`, `encypher:policy:v3:` and
`encypher:policy-skip:v3:`. The same store contains
`trusted_server_config` and may contain other modules' data. Never delete the
whole store to remove Encypher entries. In particular, the current
`spin cloud key-value` plugin can delete a store but cannot enumerate and
delete individual entries safely.

This cache split retires the old `encypher:v3:` article prefix and
`encypher:policy:v2:` policy prefix. They are deletion candidates only after
the same checks below. Before deleting `encypher:v3:`, wait the greater of
`plan_ttl_seconds` and `negative_ttl_seconds`; before deleting
`encypher:policy:v2:`, wait `policy_ttl_seconds`, in both cases plus the
rollback window.

Use the backing provider's item-level API, or a reviewed one-shot maintenance
component that calls `list_keys_page(prefix)` and `delete`, with this
procedure:

1. Rotate to a new versioned namespace and quiesce writes while taking the
   inventory. Do not proceed while a live binary can still write the retired
   prefix.
2. Export or back up `default` and record `trusted_server_config` separately.
3. Enumerate and preview only the exact retired prefix. Never use an empty
   prefix and never match `trusted_server_config`.
4. Wait through the namespace's configured lifetime plus the deployment's
   rollback window: `plan_ttl_seconds` for `plan`, `negative_ttl_seconds` for
   `plan-skip`, `policy_ttl_seconds` for `policy`, and the lesser of
   `policy_ttl_seconds` and `negative_ttl_seconds` for `policy-skip`.
5. Delete only the previewed old-prefix entries. Verify
   `trusted_server_config` is unchanged and sample active-prefix entries still
   read before resuming writes.

The KV interface has no compare-and-delete operation. If writes cannot be
quiesced, retain uncertain entries rather than risk deleting a concurrent
replacement.

Spin sends no [auction telemetry](./telemetry#adapter-support), and warns at
startup when `[analytics]` selects a module.

Spin is not installed in the integration CI environment. The recurring manual
smoke therefore needs a named owner, tested commit, exact tool versions, and an
expiry; those details are recorded with each run in the documentation-refresh
evidence ledger.

Compare the [Fastly](./fastly), [Cloudflare](./cloudflare), and
[Axum](./axum-dev) adapter journeys.
