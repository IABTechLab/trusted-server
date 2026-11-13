# EdgeZero Lifecycle and Stores

EdgeZero is the deployment and configuration layer used by the `ts` CLI, and
the runtime layer under every adapter: each adapter serves requests through
EdgeZero's request lifecycle and sends outbound HTTP through EdgeZero's client.
The repository's `edgezero.toml` declares one application, three logical
stores, and the Fastly, Axum, Cloudflare, and Spin adapter commands.

## Pinned version

The workspace `Cargo.toml` pins all six EdgeZero crates to one git revision of
`IABTechLab/edgezero`: `14a6ca8bb5a5cbe055a1f725bdab8f42569a3bf0`, the head of
EdgeZero pull request 275. The pin moves with that pull request's head until
it merges, and then to an EdgeZero release tag.

## Lifecycle

Use `ts config init` to create a file, edit the generated
`trusted-server.toml`, and run `ts config validate` before any platform
write. `ts config diff --adapter ADAPTER` compares the validated local
configuration with the selected platform value. `ts config push --adapter
ADAPTER` writes it. Build, serve, deploy, health-check, rollback, and active
version operations use the adapter commands declared in `edgezero.toml`.

Configuration is startup state, not a live control plane. A successful push
does not change a running instance until the target's restart or deployment
path loads that value. Use `--staging` only with the corresponding staging
deployment flow. On Fastly the entry key is always the logical store ID, so
staging isolation comes from selecting a different physical store with
`EDGEZERO__STORES__CONFIG__<ID>__NAME`; see
[the CLI guide](/guide/cli#lifecycle-commands).

## Logical and physical stores

The manifest declares portable logical IDs:

| Kind          | Logical ID               |
| ------------- | ------------------------ |
| KV            | `trusted_server_kv`      |
| Configuration | `trusted_server_config`  |
| Secrets       | `trusted_server_secrets` |

An adapter maps those IDs to platform resources. The runtime-facing
`StoreName` used for reads and the management-facing `StoreId` used for
writes are deliberately distinct types. Environment mappings such as
`EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME` may change the
physical store name without changing application configuration.

Fastly binds its stores in `fastly.toml`. Axum uses isolated local state for
development. Cloudflare maps the configuration value through its Worker
binding. Spin resolves the manifest mapping and writes supported local
key-value backends. Consult the adapter guide before assuming that a declared
logical store is wired for runtime reads on every target.

## Blob and chunk flow

`config push` serializes validated settings into one EdgeZero
`BlobEnvelope`. Store-backed settings contain secret key names, not secret
values; the adapter resolves those keys from the logical secret store during
startup.

When a Fastly envelope exceeds one Config Store entry, the writer stores
bounded chunks and replaces the root value with a versioned
`fastly_config_chunks` pointer. Startup verifies each chunk's declared
length and SHA-256 and then verifies the reconstructed envelope. A missing,
oversized, reordered, or modified chunk fails configuration loading.

`ts config gc --adapter fastly` previews orphaned chunks by default. An
actual deletion requires `--yes --older-than WINDOW`. The age assertion
applies to the whole physical store, not one root key; verify the reported
store before deletion, especially when using `--store` or `--no-env`.

## Request handling

These rules hold on Fastly, Axum, Cloudflare, and Spin.

**Inbound requests.**

- A request target over 8192 bytes returns 414. More than 100 header fields,
  or more than 64 KiB of header bytes, returns 431.
- Ambiguous request framing returns 400: a duplicate or malformed
  `Content-Length`, `Content-Length` with `Transfer-Encoding`, or a
  `Transfer-Encoding` that is not chunked.
- Each endpoint reads its body under its own cap and returns 413 above it. A
  declared `Content-Length` over the cap returns 413 before any body byte is
  read. A body still unread 30 s after the request arrived returns 408.
- A publisher request streams its body to the origin under a 16 MiB cap, and a
  larger upload returns 413. A forwarded `GET` or `HEAD` that carries body bytes
  returns 400.

**Outbound requests.**

- Every request sets a 1 s connect timeout and a constant total timeout: the
  configured timeout where a setting names one, such as an auction provider's
  `timeout_ms`, 2 s for Tinybird, and otherwise 15 s for a buffered response or
  60 s for a streamed one. A redirect chain also has a 60 s deadline. EdgeZero
  enforces the connect timeout natively on Fastly only. On Axum, Cloudflare,
  and Spin it is best effort, and the total timeout still bounds the request.
- Origin TLS is always verified.
- A request that sets no `Accept-Encoding` carries `Accept-Encoding: identity`.
- Hop-by-hop fields, and fields that `Connection` names, never reach an
  upstream or come back from one. A request or response header whose value is
  not UTF-8 is dropped whole, with a warning that names the header.
- The request URI is normalized as a WHATWG URL. A publisher request whose
  target has a fragment, a backslash, or userinfo returns 400.
- On Fastly, EdgeZero registers each backend at request time and names it
  `ez_b31v1_` followed by a hash of the backend configuration.

**Responses.**

- Response egress strips hop-by-hop fields and never sends `Trailer`. A framing
  error answers 500 before the response is committed.
- A response body must finish within 60 s after egress starts.
- Framework errors, such as a 404, 405, 414, or 431, render as EdgeZero JSON
  and carry `x-ts-version`. A startup failure response carries no
  `x-ts-version`.

## Safe operator sequence

1. Put required secret values in the physical store mapped from
   `trusted_server_secrets`.
2. Run `ts config validate`.
3. Review `ts config diff --adapter ADAPTER`; use `--no-diff` when
   deliberately inline configuration must not appear in logs.
4. Run `ts config push --adapter ADAPTER`.
5. Start or deploy the adapter and verify a non-health publisher route. A
   healthy endpoint alone may not prove that application configuration loaded.
   Confirm that its `x-ts-version` response header reports the release you
   deployed.

See [Configuration](/guide/configuration) for the field contract, the
[CLI guide](/guide/cli) for exact command help, and
[deployment guides](/guide/integrations-overview#adapter-support) for target
behavior.
