# Phase 1 (EdgeZero store-registry migration) — progress ledger

Plan: docs/superpowers/plans/2026-07-02-edgezero-store-registry-migration.md
Pin: edgezero @ ff530286 (PR #306 branch head, --locked). D6-a confirmed (keep composite write path).
Branch: worktree-edgezero-migration-spec

## 2026-07-08 merge iabtechlab/main (commit 76662d364)
- Synced main into branch. Conflicts: Cargo.toml/Cargo.lock only (config.rs clean —
  main #744 didn't touch it, `fn secret_fields()` compat fix intact).
- Resolution: KEEP PR #306 edgezero pin (branch, not main's tag=v0.0.4 — v0.0.4 lacks
  State<T>/nested secrets). Branch head advanced d8f71a4a → ff530286.
- Verified green: core + axum + cloudflare(wasm) + spin + fastly(wasm) + cli, --locked.
- MATERIAL: main #744 REMOVED legacy_main/route_request. Phase-5 legacy delete already
  done upstream. Invalidates Task 8's Phase 1/5 boundary justification (see BLOCKERS H3).

## Blocking plan amendments before Task 2 resumes (from 2-agent deep review)
- H1 (config store id): store *id* is `trusted_server_config` (fastly main.rs:45,
  fastly.toml:65/67, generate-viceroy-config.rs:157/159/250/253, viceroy-template.toml:71,
  cloudflare.toml:15) — NOT `app_config`. Blob *key* is app_config. "Keep app_config"
  decision requires reconciling these store-id sites (rename → app_config, OR reverse
  core DEFAULT_CONFIG_STORE_ID → trusted_server_config). DECISION NEEDED.
- H2 (phantom defaults): edgezero.toml kv default `trusted_server_kv` + secret default
  `trusted_server_secrets` are unreferenced/unprovisioned → from_parts returns None →
  whole KV/secret registry dropped on Fastly → named-KV never works. Pick real defaults
  (kv=ec_identity_store, secret=signing_keys) or provision them.
- H2b (eager open): build_kv_registry opens EVERY declared id; creative_store is
  deprecated/never-opened; converts lazy-fail-closed → eager-fail-all. Make per-store
  open non-fatal, or exclude never-read ids.
- H(ec): EC identity graph bypasses registry (FastlyEcKvStore direct open, main.rs:1378/
  1449). Plan's "ec/* already use kv_handle()" is FALSE. Migrate to kv_handle_named or
  explicitly scope out.
- H3 (legacy gone): re-ground Task 8 on real Fastly read callers
  (load_settings_from_config_store boot, build_per_request_services fallback, mgmt-API
  writes) — drop legacy_main/platform.rs:578 references.
- Med: CF config side-channel var vs KV-namespace contradiction; consent fail-closed now
  fires all 4 adapters (only Fastly tested); pervasive line/symbol drift (locate by
  symbol not line); clippy-cloudflare-wasm missing from plan CI gate.
- Review outputs: tasks/a33b466a7f03d14e3.output (executability), a392eb23beb2ec22b.output
  (coverage).

## Tasks
- Task 1: in progress (inventory + D5/D6-a decision record)
- Task 1: COMPLETE — preflight green, D6-a confirmed, D5 map recorded (commit pending)

## 2026-07-07 operator decision (mid-Task-2) — SUPERSEDED 2026-07-08
- (Was: KEEP app_config store id.) Rested on false premise that manifests/generator
  already use app_config. Deep review proved store *id* is trusted_server_config in
  Fastly/manifests; only core default + blob key are app_config. See below.

## 2026-07-08 operator decision (supersedes above) — H1 resolution
- UNIFY config-store id on `trusted_server_config`. Manifests/generator already use it,
  so they stay UNTOUCHED (generate-viceroy-config.rs, fastly.toml, viceroy-template.toml,
  cloudflare.toml unchanged). Change is: core DEFAULT_CONFIG_STORE_ID (settings_data.rs)
  app_config → trusted_server_config, and edgezero.toml [stores.config] default =
  trusted_server_config. Blob KEY (config_payload.rs CONFIG_BLOB_KEY) is orthogonal
  (key within the store, not a store id) — verify interaction before touching.
- Spec's D5/Phase1/R9 `trusted_server_config` wording is CORRECT (not residue); keep.
- Request-signing store ids: fix example+fixture to jwks_store/signing_keys (2 lines).

## 2026-07-08 operator decision — H2 resolution (KV/secret defaults)
- PROVISION real default stores: add `trusted_server_kv` + `trusted_server_secrets`
  as real stores in EVERY adapter manifest (fastly.toml, viceroy-template.toml,
  wrangler.toml, spin.toml). trusted_server_kv = general-purpose TS KV (plan intent).
  No edgezero change. edgezero.toml already declares them as defaults (no change there).
- Still owed in Task 6: make build_kv_registry per-store open non-fatal (H2b) so a
  single unprovisioned/deprecated id (creative_store) can't fail all traffic.

## Finalized Task 2 scope (supersedes plan Task 2 where they differ)
1. settings_data.rs: DECOUPLE store-name from blob-key. default_config_store_name()
   → "trusted_server_config"; default_config_key() must stay "app_config"
   (= CONFIG_BLOB_KEY). Update the 4 test StoreName::from("app_config") sites to match
   whatever the read path now asserts. Do NOT touch CONFIG_BLOB_KEY, manifests, generator.
2. Provision trusted_server_kv + trusted_server_secrets in all 4 adapter manifests.
3. Hooks::stores() per adapter returns the declared StoresMetadata (const literals).
4. request-signing example+fixture → jwks_store/signing_keys (2 lines).
GUARDRAIL: this is NOT a broad rename. Only settings_data.rs store-NAME changes to
trusted_server_config. The blob KEY and all config_payload/CONFIG_BLOB_KEY stay app_config.
