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
- Task 1: COMPLETE — preflight green, D6-a confirmed, D5 map recorded.
- Task 2: COMPLETE.
  - part 1 (0513377e9): H1 store-name/blob-key decouple + request-signing ids.
  - part 2 (7da58df8c): edgezero.toml full id lists; provision trusted_server_kv/
    trusted_server_secrets + s3-auth in fastly.toml/viceroy-template.toml/spin.toml;
    shared STORES_METADATA const in core/src/stores.rs feeding all 4 Hooks::stores();
    anti-drift test (RED verified). CF: decision C (cloudflare.toml left vestigial;
    per-id wrangler.toml bindings deferred to Phase 2). All adapters + parity green.
- Plan amended for post-merge state + 2-agent review (c22fcd045).
- Task 3: COMPLETE (bfde7780a) — write-only PlatformConfigWriter/PlatformSecretWriter
  split; CompositeConfigStore/CompositeSecretStore (Option<Registry> reader, strict:
  absent registry AND unknown id both hard-error; writes delegate preserving StoreId);
  StoreName doc → logical read id + read-site audit. Additive to core/platform only.
  4 composite tests RED→GREEN; all 4 adapters check + fmt green.
- Task 4: COMPLETE (11373ea89) — get_settings_from_config_store re-typed to
  (&ConfigStoreHandle, key); Fastly reuses open_trusted_server_config_store();
  Axum uses path-env-pointer (TRUSTED_SERVER_AXUM_CONFIG_PATH → tempdir file, else
  from_local_file("trusted_server_config")); axum.rs harness updated; parity 13/0.
  CF/Spin untouched (don't call the loader).
- Defect fixes (45d590c7b), both pre-existing (Task 2/3), caught by Task 4:
  * EdgeZero manifest validator REJECTS hyphenated store ids ([A-Za-z0-9_] only,
    for EDGEZERO__STORES__ env exportability). s3-auth + datadome-ip-bypass removed
    from edgezero.toml + STORES_METADATA (Spin manifest test was failing). They are
    NOT yet routed through the registry, so removal is safe now.
  * doc_markdown clippy -D warnings (EdgeZero/DataDome un-backticked) in
    composite/traits/stores — fixed.

## 2026-07-09 operator decision — s3-auth/datadome-ip-bypass (RESOLVED, was deferred below)
- CONVERGE on underscore logical ids (Option A). Both reads route through RuntimeServices
  (proxy.rs:829 secret_store; datadome protection_scope.rs:347 config_store) → the composite
  in Task 5b, and EdgeZero forbids hyphenated registry ids. So: s3-auth → s3_auth,
  datadome-ip-bypass → datadome_ip_bypass EVERYWHERE (code defaults, manifests' physical
  store names, example/fixtures, user docs, tests) + re-declare as registry ids. Under D7
  logical id == physical store name; operators with hyphenated physical stores use
  EDGEZERO__STORES__<KIND>__<ID>__NAME (documented, not implemented here).
- Task 5 SPLIT: 5a = the convergence rename; 5b = composite wiring + named-KV.
- Task 5a: COMPLETE (33ff05884) — s3-auth→s3_auth, datadome-ip-bypass→datadome_ip_bypass
  everywhere (code, manifests' physical store names, example/fixtures, 3 user guides w/
  __NAME contract note); re-declared in edgezero.toml + STORES_METADATA. Zero stray hyphens.
  core 1630, spin manifest PASS, parity 13/0, clippy/fmt clean.
## 2026-07-13 Task 5b findings (both VERIFIED) + operator decision
- BLOCKER (found by 5b implementer): edgezero `KvHandle::put_bytes_with_ttl` validates against
  `MAX_TTL = 365d` (key_value_store.rs:378), but TS `MAX_CONSENT_AGE_DAYS = 395` (consent_config.rs:10,
  the IAB 13-month norm). Consent MUST go through KvHandle (KvRegistry::named only yields KvHandle),
  so migrating consent → KvHandle would fail TTL validation, and save_consent_to_kv SWALLOWS KV
  failures → consent silently never persists. On Task 6's critical path too; cannot be dodged.
  DECISION: clamp consent KV TTL to edgezero's public MAX_TTL + warn log (interim, fail-safe:
  consent expires ~1mo earlier → earlier re-prompt, still IAB-compliant). MAX_CONSENT_AGE_DAYS
  stays 395. Upstream ask FILED: https://github.com/stackpop/edgezero/issues/323 (raise or
  parameterize MAX_TTL; at minimum make an over-cap write a typed error that can't be swallowed).
  REMOVE the clamp once #323 ships and the pin is bumped.
- FINDING for Task 6: the plan's `publisher.rs:626` consent call site DOES NOT EXIST. The only
  production `build_consent_context` caller is `ec/mod.rs:216`, passing `kv_store: None` — consent
  KV persistence is DORMANT in the shared path. Task 6 must RE-ESTABLISH the consent KV call site,
  not "flip" it. (All other build_consent_context refs are tests/docs.)

- Task 5b: dispatched — Axum registries via PUBLIC edgezero constructors (AxumConfigStore +
  PersistentKvStore::new, TS-chosen redb paths, no private replication/parity test since TS
  supplies the whole registry via with_kv_registry → authoritative); keep AxumDevServer::
  with_config for PORT. Core kv_handle_named + kv_registry field; consent → KvHandle
  (ConsentPipelineInput + persistence fns + all consumers atomically); writer trait impls for
  Axum/CF/Spin. Fastly NOT touched (publisher consent stays on kv_handle() until Task 6).

## (superseded) DEFERRED decision — config-contract
- s3-auth (secret; settings.rs:655 default_s3_secret_store) and datadome-ip-bypass
  (config; datadome/protection_scope.rs:165) are REAL store-name defaults with hyphens.
  When their reads move onto the composite (registry.named(config_value)), the value
  must be a valid underscore logical id. Options: (a) change operator-facing defaults
  to s3_auth / datadome_ip_bypass (+ physical-name mapping via EDGEZERO__STORES__…__NAME),
  or (b) keep these reads off the strict registry. Decide when Task 5/6 wires them.
  Default-path tests won't trip it (DataDome IP-CIDR sources + S3 default-empty/disabled).

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
