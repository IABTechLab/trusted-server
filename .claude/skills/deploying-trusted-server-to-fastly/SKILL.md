---
name: deploying-trusted-server-to-fastly
description: Use when deploying a Trusted Server service to Fastly Compute, or adding a second or Nth TS service to a Fastly account that already runs one. Symptoms include `ts config push` overwriting another service's config, config or secret store name collisions across services, and being unsure which stores to create and link.
---

# Deploying Trusted Server to Fastly

## Overview

Fastly config, secret, and KV store names are **account-level**. Trusted Server
opens stores by fixed logical names (`trusted_server_config` for the app-config
blob, `ec_identity_store`, the configured secret stores). If two TS services
share one Fastly account and both bind the default physical store names, they
collide: `ts config push` resolves the target store by name and overwrites the
other service's config.

Core principle: **decouple the physical store name from the logical name TS
opens.** Give each service its own physically-distinct stores, and link them to
the service under the logical names TS expects.

## Multi-service store setup

For each shared-name store a service needs:

1. Create a service-scoped physical store:
   `fastly config-store create --name <service>_ts_config`
2. Link it under the logical name TS opens:
   `fastly resource-link create --service-id <SID> --version <v> --resource-id <store-id> --name trusted_server_config`
3. Push config into the scoped store with the name override, so the write cannot
   land in the shared default:
   `EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME=<service>_ts_config ts config push --adapter fastly --app-config <file>`

At runtime the service opens `trusted_server_config` and gets the linked physical
store, so no other service is touched. Apply the same physical-name-per-service
pattern to any shared-name secret or KV store. This is required for multi-service
accounts and harmless for single-service ones, so prefer it by default.

## Guardrails

- **Always `ts config push --dry-run` first.** It prints the exact target
  physical store; confirm it is the service-scoped one, not a shared default,
  before writing.
- **Always pass `--service-id=<SID>`** to `fastly compute publish`. A checked-in
  `fastly.toml` may pin a different (or dead) `service_id`; without the flag,
  publish targets that one.
- **`ts config push` only upserts entries**; the target store must already exist
  (`fastly config-store create`) before pushing.
- **Verify a shared store stayed untouched** by recording its `updated`
  timestamp (`fastly config-store describe --store-id <id>`) before and after the
  push.

## Notes

- The `ts` CLI runs from source without installing: `cargo run -p
  trusted-server-cli -- config <args>` (binary name `ts`), or install it with
  `cargo install --path crates/trusted-server-cli`.
- The app-config store's logical id and blob key are `trusted_server_config`
  (`settings_data.rs` `DEFAULT_CONFIG_STORE_ID`, `config_payload.rs`
  `CONFIG_BLOB_KEY`); older builds used `app_config`. The override env-var key
  matches the logical id: `EDGEZERO__STORES__CONFIG__TRUSTED_SERVER_CONFIG__NAME`.
- Which stores a service actually needs depends on enabled features. A plain
  publisher/asset proxy (no EC, auction, or request signing) boots on just the
  `trusted_server_config` config store, plus a secret store for any `s3_sigv4`
  asset-origin auth. Confirm current store requirements against the adapter boot
  path rather than assuming.
