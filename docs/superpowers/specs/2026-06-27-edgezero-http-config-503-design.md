# Design: HTTP-Layer Config-Store Load Hardening (503)

- **Date:** 2026-06-27
- **Author:** Prakash (HTTP-layer / runtime).
- **Status:** implemented on `feature/edgezero-269-http` (targets `main`).
- **Supersedes:** the earlier per-key flatten/hash variant of this design. The
  CLI now stores Trusted Server config as a single **blob** (optionally chunked
  for Fastly value-size limits — `config_payload::settings_from_config_blob`,
  `settings_data::FastlyChunkPointer`), so the load path and this spec are
  re-derived against that model.

---

## 1. Problem

The runtime rebuilds `Settings` at boot by reading the `app_config` config
store. Before this change every read failure — including an **unseeded** store —
mapped to `TrustedServerError::Configuration` → **500**, indistinguishable from a
genuine code bug. `trusted-server.toml` is deleted, so an unseeded store is an
expected operational state (fresh install, or cutover before `ts config push`),
not a bug.

## 2. Load sequence (blob model)

`crates/trusted-server-core/src/settings_data.rs`:

```
get_settings_from_services
  → get_settings_from_config_store(store, name, key)
      → read_config_entry(key)                     // READ  — the blob value
      → resolve_fastly_chunk_pointer(value)        // if a chunk pointer:
          → read_config_entry(chunk.key) × N       // READ  — each chunk
          → verify chunk len + sha, envelope len + sha   // VERIFY
      → settings_from_config_blob(envelope_json)   // VERIFY — parse + validate
```

`read_config_entry` is the **single read seam** (used for both the top-level
blob key and every chunk key). `key` resolves via
`EnvConfig::store_key("config", "app_config")`.

## 3. Behavior matrix (the contract)

The boundary is **"couldn't read the config"** vs **"read it but it's
invalid"** — classified by **call site**, because `PlatformConfigStore::get`
collapses key-absent and transport failure into one `Err` (see §5).

| Situation                                                                                            | Where                                 | Status                                                                     |
| ---------------------------------------------------------------------------------------------------- | ------------------------------------- | -------------------------------------------------------------------------- |
| Blob key or a referenced chunk cannot be read (store unseeded, transient backend, chunk key missing) | `read_config_entry`                   | **503** `ConfigStoreUnavailable`, actionable hint `run \`ts config push\`` |
| Chunk len/sha mismatch, envelope len/sha mismatch, unsupported pointer version                       | `resolve_fastly_chunk_pointer` verify | **500** `Configuration`                                                    |
| Blob read OK but not a valid envelope / settings invalid                                             | `settings_from_config_blob`           | **500** `Configuration`                                                    |
| Seeded + valid                                                                                       | —                                     | `Settings` loads                                                           |

503 is correct for the read column: unseeded → seed it; transient → retry.

## 4. Mechanism (one new variant)

`TrustedServerError::ConfigStoreUnavailable { store_name, message }` →
`StatusCode::SERVICE_UNAVAILABLE` (precedent: the existing `KvStore` 503 arm).
Only `read_config_entry`'s `change_context` target changes from `Configuration`
to the new variant; all verify/parse paths are untouched. No `PlatformError`
or `PlatformConfigStore` change.

**Security:** the actionable hint rides the error chain (`Display`) to the
**server log** only. The public 503 body is a generic, retry-flavored
`user_message()` arm shared by the retryable 503 variants
(`ConfigStoreUnavailable | KvStore`): `"Service temporarily unavailable"`.
It carries no internal detail, but — unlike the 500-flavored catch-all
(`"An internal error occurred"`) — lets clients and monitoring distinguish
*retryable* from *terminal* without leaking tooling/paths.

## 5. Out of scope / follow-up

- `PlatformConfigStore::get → Result<Option<String>>` (absence as a value, not an
  error) would let the runtime distinguish **unseeded** (`Ok(None)`) from
  **transient** (`Err`) precisely instead of classifying by call site. It is the
  store-convergence direction (edgezero's own `ConfigStore::get` shape) and
  touches every impl + caller across request-signing and DataDome — tracked as a
  separate, cross-cutting change, not this PR.
- Non-Fastly adapter (cloudflare/spin/axum) parity rides the EdgeZero adapter
  stack.
- `settings_data::resolve_fastly_chunk_pointer` duplicates
  `edgezero-adapter-fastly`'s `chunked_config` resolver (same wire format:
  `edgezero_kind = "fastly_config_chunks"`, version 1, per-chunk len + sha,
  envelope len + sha). The upstream resolver is `pub(crate)` and, when reached
  transparently through `edgezero_adapter_fastly::config_store::FastlyConfigStore::get`,
  collapses **missing chunk** (retryable, this spec's 503) and **corrupt chunk**
  (terminal, 500) into one opaque error — so delegating today would lose the
  503/500 contract above. Swap to the upstream resolver and delete the local
  copy once edgezero exports it (or its error taxonomy distinguishes
  missing from corrupt) — needs an upstream change + repin.
