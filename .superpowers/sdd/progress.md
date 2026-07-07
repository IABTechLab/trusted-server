# Phase 1 (EdgeZero store-registry migration) — progress ledger

Plan: docs/superpowers/plans/2026-07-02-edgezero-store-registry-migration.md
Pin: edgezero @ d8f71a4a (--locked). D6-a confirmed (keep composite write path).
Branch: worktree-edgezero-migration-spec

## Tasks
- Task 1: in progress (inventory + D5/D6-a decision record)
- Task 1: COMPLETE — preflight green, D6-a confirmed, D5 map recorded (commit pending)

## 2026-07-07 operator decision (mid-Task-2)
- KEEP app-config store id `app_config` (do NOT rename to trusted_server_config).
  Declare `app_config` in edgezero.toml instead. No rename cascade (config_payload,
  settings_data, viceroy generator, test envs, cloudflare side-channel untouched).
- Request-signing store ids: fix example+fixture to jwks_store/signing_keys (2 lines).
- Task 2 re-scoped + re-dispatched.
