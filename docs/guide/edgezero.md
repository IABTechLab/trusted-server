# EdgeZero Lifecycle and Stores

EdgeZero is the deployment and configuration layer used by the `ts` CLI.
The repository's `edgezero.toml` declares one application, three logical
stores, and the Fastly, Axum, Cloudflare, and Spin adapter commands.

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
deployment flow: it writes `LOGICAL_ID_staging` in the same physical store,
while production continues to read the ordinary logical key.

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

## Safe operator sequence

1. Put required secret values in the physical store mapped from
   `trusted_server_secrets`.
2. Run `ts config validate`.
3. Review `ts config diff --adapter ADAPTER`; use `--no-diff` when
   deliberately inline configuration must not appear in logs.
4. Run `ts config push --adapter ADAPTER`.
5. Start or deploy the adapter and verify a non-health publisher route. A
   healthy endpoint alone may not prove that application configuration loaded.

See [Configuration](/guide/configuration) for the field contract, the
[CLI guide](/guide/cli) for exact command help, and
[deployment guides](/guide/integrations-overview#adapter-support) for target
behavior.
