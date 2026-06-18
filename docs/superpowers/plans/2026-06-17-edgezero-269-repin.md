# EdgeZero #269 HTTP-Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the HTTP-layer (runtime) half of adopting edgezero `stackpop/edgezero#269` in trusted-server — by **converging onto Christian's `feature/ts-cli-next`** (which already carries the repin, the `Body` fixes, and runtime Settings-from-config-store for Fastly), then closing the runtime gaps it leaves: seed-before-serve safety, secrets/KV runtime wiring, non-Fastly adapters, and the missing runtime-config-store spec.

**Architecture:** trusted-server keeps its bespoke `platform/` layer (`RuntimeServices` + `PlatformConfigStore`/`SecretStore`/`KvStore`). #269's only forced code break is `Body::into_bytes() → Option` (18 sinks — Appendix A). Christian's branch already fixes those and wires `get_settings_from_services()` to rebuild `Settings` from the `app_config` config store via the shared `config_payload` flatten/hash contract. Our work is the **runtime-side hardening + spec**, not a parallel repin.

**Tech Stack:** Rust 2024, cargo, `wasm32-wasip1` (Fastly via Viceroy), edgezero git dep (`2eeccc9`, #269 HEAD), `error-stack`.

**Source spec:** [2026-06-16-edgezero-269-repin-breaking-api-finding.md](../specs/2026-06-16-edgezero-269-repin-breaking-api-finding.md) — esp. **§12** (convergence), §2 (sinks), §9 (decisions).

---

## Strategy change (why this plan was rewritten)

The prior version of this plan was a standalone minimal-repin off PR14. Investigation of `feature/ts-cli-next` (2026-06-18, spec §12) showed that branch is **not just CLI** — it already implements the end-to-end Fastly config-store migration: same #269 pin, the `Body` fixes, store ids, the `config_payload` contract, **and** runtime `Settings`-from-store load. So a separate Fastly repin is **redundant**. This plan now **builds on his branch** and focuses on the runtime gaps. The verified `Body`-sink enumeration is preserved as Appendix A (still the authoritative sink reference when his ad-hoc fixes merge up the stack).

---

## Open decisions — resolve at Phase 0 before coding

| #   | Decision                                                                                           | Recommendation                                                            |
| --- | -------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| D1  | Build on `feature/ts-cli-next` vs keep the PR14-stack minimal-repin                                | **Build on his** (his is end-to-end for Fastly; ours duplicates it)       |
| D2  | Whole-`Settings` → store (his) vs two-tier small `AppConfig` (our spec §6)                         | **Adopt his whole-`Settings`** (one source of truth; already implemented) |
| D3  | `Body` fix style                                                                                   | **His `ok_or_else` (graceful)** over `.expect()` (spec §2)                |
| D4  | Empty/unseeded store behavior                                                                      | **Decide explicitly** (Phase 2) — today it's a hard fail / outage         |
| D5  | CLI-driven secret push (he punts) vs runtime secret writes (already exist via `management_api.rs`) | Keep runtime rotation; treat CLI secret-push as a later follow-up         |
| D6  | Branch/merge topology — his branch is off `main`, the stack is PR14→PR20                           | Phase 5 — confirm with team                                               |

Do **not** start Phase 1 until **D1–D3** are confirmed (they set the base branch
and code style). **D4–D6 are sequenced, not skipped:** D4 (empty-store response)
is resolved in Phase 2 Step 5, D5 (secret-write boundary) in Phase 3 Step 2, D6
(branch topology) in Phase 5 Step 1.

---

## Scope & non-goals

**In scope:** converge onto his branch; verify the repin + `Body` fixes are complete against Appendix A; run the full gate (host, **wasm32-wasip1**, **`--all-targets`**, clippy, test) + integration-tests lockfile; harden runtime config-store loading (empty/malformed-store, seed-before-serve); confirm secrets/`ec_identity_store` KV runtime wiring; write the runtime-config-store spec; merge up the stack.

**Out of scope (separate plans):** the CLI crate itself (`ts config`/`audit` — Christian); CLI-driven secret push; full edgezero `run_app`/`app!`/extractor adoption (he kept the bespoke layer, so do we); non-Fastly adapter _feature_ parity beyond making them build.

---

## File structure (what we touch / extend, on his branch)

| File                                                                                                                                                    | Role                                                      | Our action                                    |
| ------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------- | --------------------------------------------- |
| `Cargo.toml` / `Cargo.lock`                                                                                                                             | edgezero pinned `2eeccc9`                                 | verify; re-pin to `main` post-merge (Phase 5) |
| `crates/trusted-server-core/src/config_payload.rs`                                                                                                      | flatten/hash contract (shared seam)                       | **read-only reference** — do not fork         |
| `crates/trusted-server-core/src/settings_data.rs`                                                                                                       | `get_settings_from_services` runtime load                 | **harden** empty/malformed behavior (Phase 2) |
| `crates/trusted-server-adapter-fastly/src/main.rs`                                                                                                      | entry point: build services → load settings               | **harden** the settings-error path (Phase 2)  |
| `edgezero.toml`                                                                                                                                         | store ids: `app_config` / `secrets` / `ec_identity_store` | verify; reference in the spec                 |
| `crates/trusted-server-core/src/{proxy,publisher,auction/endpoints,auction/formats,request_signing/endpoints}.rs`, `integrations/{prebid,testlight}.rs` | `Body` sinks                                              | **verify** all 18 covered (Appendix A)        |
| `crates/trusted-server-adapter-{cloudflare,spin}`                                                                                                       | stubs, untouched by him                                   | **make build** under #269 (Phase 3)           |
| `docs/superpowers/specs/<new>-runtime-config-store.md`                                                                                                  | the missing spec                                          | **create** (Phase 4)                          |

---

## Phase 0: Convergence decision + adopt the base

- [ ] **Step 1: Confirm D1–D3** with the team (record in the spec §9). If D1 = "build on his," proceed; if "keep PR14-stack," fall back to Appendix B (the minimal-repin tasks).

- [ ] **Step 2: Create the HTTP-layer branch off his branch**

```bash
git fetch origin
# Record the exact SHA — his branch is an unmerged WIP and may force-push/rebase.
git rev-parse origin/feature/ts-cli-next   # note this; if he rebases, re-base from the new SHA + coordinate
git checkout -b feature/edgezero-269-http origin/feature/ts-cli-next
```

- [ ] **Step 3: Baseline build (inherit his state)**

Run: `cargo build --workspace --all-targets 2>/tmp/ez_base.log; echo "exit=$?"`
Expected: **green** (his branch should already compile). If red, capture and triage before any new work.

---

## Phase 1: Verify the inherited repin + `Body` fixes

His `Body` fixes were ad-hoc (driven by his build), not enumerated. Verify completeness against Appendix A, and run the **full** gate (he is unlikely to have run wasm + `--all-targets` + clippy on every leg).

- [ ] **Step 1: Enumerate the sinks (locate, don't "prove")**

Run: `git grep -nE 'into_bytes\(\)' crates/trusted-server-core/src -- 'proxy.rs' 'publisher.rs' 'auction/endpoints.rs' 'auction/formats.rs' 'request_signing/endpoints.rs' 'integrations/prebid.rs' 'integrations/testlight.rs'`
Expect **18 sites** (8 prod + 10 test). Eyeball each has an `Option` handler
(`.ok_or_else`/`.expect`/`.unwrap_or_default`). **Note: grep cannot prove
correctness** — a fixed Shape-C `let b = …into_bytes().ok_or_else(…)?;` and a
broken bare `.into_bytes()` both contain `.into_bytes()`. This step is enumeration
only; the **authoritative completeness proof is Step 2's green `--all-targets` +
`cargo test`.** Appendix A line numbers are **PR14-base and do NOT apply** to this
`main`-based branch — trust the grep _count_ (18), not the numbers.

- [ ] **Step 2: Full gate (the legs he likely skipped)**

```bash
cargo build --workspace --all-targets
cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```

Expected: all green. Any failure here is the real signal — fix before Phase 2.

- [ ] **Step 3: integration-tests lockfile**

`crates/integration-tests` is a separate workspace that path-deps `trusted-server-core`.
Run: `( cd crates/integration-tests && cargo build --workspace )` first (don't
`generate-lockfile` — that can re-resolve and _cause_ drift). Only if it fails on
shared-dep mismatch: `cargo update -p <crate> --precise <root-version>` (never
blanket). Repeat for `crates/openrtb-codegen` if it drifts.

- [ ] **Step 4: Commit any gate fixups**

```bash
git add crates Cargo.toml Cargo.lock && git commit -m "Complete Body sink coverage and pass full gate on #269" || echo "nothing to commit"
```

---

## Phase 2: Runtime config-store hardening (the core HTTP-layer deliverable)

**Problem (verified against his `main.rs`):** `get_settings_from_services` →
`get_settings_from_config_store` reads `ts-config-keys` first; on an
**empty/unseeded store** `read_config_entry`'s `?` propagates a `Configuration`
error. His settings-error arm **does serve a response** —
`to_error_response(&e).send_to_client(); return;` (not a bare return, not an
opaque default; `fn main()` returns `()` and serves explicitly). So the issue is
**not** "no response" — it is that **every route returns a generic error** until
the store is seeded, and the error is **indistinguishable from a real config
bug**. Fresh deploy before `ts config push` = **total outage with an opaque 500**.
The gap our layer owns: make the unseeded case **actionable** (clear message) and
**correctly classified** (retryable 503, not 500).

> **Call chain (read first):** `get_settings_from_services(&runtime_services)` →
> `get_settings_from_config_store(&dyn PlatformConfigStore, &StoreName)` →
> `read_config_entry` (per key) → `settings_from_config_entries` (hash verify).
> The in-memory `PlatformConfigStore` fake **already exists** as
> `MemoryConfigStore` in `settings_data.rs` tests (around line 84) — reuse it; do
> not write a new one. Confirm the exact constructor (`MemoryConfigStore { entries }`
> vs `::new(...)`) before writing the test below.

- [ ] **Step 1: Write a failing test — empty store yields an actionable, typed error (not a generic read failure)**

In `settings_data.rs` tests (reuse `MemoryConfigStore`):

```rust
#[test]
fn empty_config_store_reports_unseeded_not_generic_failure() {
    let store = MemoryConfigStore::new(BTreeMap::new()); // no ts-config-keys
    let err = get_settings_from_config_store(&store, &StoreName::from("app_config"))
        .expect_err("empty store should error");
    // assert it carries an actionable "config store not seeded — run `ts config push`" context
    assert!(format!("{err:?}").contains("not seeded") || format!("{err:?}").contains("ts config push"));
}
```

- [ ] **Step 2: Run it — verify it fails** (`cargo test -p trusted-server-core empty_config_store -- --nocapture`). Expected: FAIL (current error message is generic "failed to read … key `ts-config-keys`").

- [ ] **Step 3: Implement — distinguish "unseeded" from "read error"**

In `read_config_entry` / `get_settings_from_config_store`, when the **metadata** key (`ts-config-keys`) is absent, attach an actionable context (e.g. `TrustedServerError::Configuration` with `"config store `{store}`is not seeded — run`ts config push --adapter fastly`"`). Keep transport/read failures distinct.

- [ ] **Step 4: Run the test — verify it passes.**

- [ ] **Step 5: Decide + implement the adapter response (D4)**

His settings-error arm already serves via `to_error_response(&e).send_to_client(); return;`.
Two options — **decide D4 here:**
(a) keep the arm, but have `to_error_response` map the new "unseeded" error context
to **503** (retryable) instead of 500; or
(b) special-case the unseeded error in `main.rs` before `to_error_response`:
`FastlyResponse::from_status(503).with_body_text_plain("config not provisioned — run `ts config push`").send_to_client(); return;`
(matches the existing `from_status(...).with_body_text_plain(...).send_to_client()`
idiom at `main.rs:119–121`). Add an adapter test asserting **503 + body** for the
unseeded case. This turns an opaque 500 into an observable, actionable signal —
and keeps real config bugs as 500.

- [ ] **Step 6: Malformed-store test** — seed a `ts-config-hash` that doesn't match the entries; assert `settings_from_config_entries` errors on hash mismatch (his code already verifies; add the test if absent so the contract is locked).

- [ ] **Step 7: Confirm secrets + KV runtime wiring**
  - `secrets` store: request-signing reads signing keys via `PlatformSecretStore` (pre-existing `management_api.rs` provides write CRUD). Add/confirm a test that a missing signing secret degrades to a clear error, not a panic.
  - `ec_identity_store` KV: `main.rs` starts `UnavailableKvStore` and EC routes lazily bind the configured store. Confirm a non-EC route still serves when EC KV is unavailable (existing behavior — add a regression test if missing).

- [ ] **Step 8: Commit**

```bash
git add crates && git commit -m "Harden runtime config-store load: actionable unseeded error and 503 response"
```

---

## Phase 3: Adapter + build-surface gaps

- [ ] **Step 1: Make non-Fastly adapters build under #269**

First confirm what "builds" means for these stubs — spec §1 notes
cloudflare/axum are **absent from the dependency graph** (not currently compiled).
If the crate has no real wasm entry, "builds" = `cargo check -p trusted-server-adapter-cloudflare`
on host; only use `--target wasm32-unknown-unknown` (install the target first) if
it has a genuine worker entry point. Same judgment for spin.
If they break on `Body`/edgezero churn, apply the Appendix A fix shapes. They are stubs — goal is **compiles**, not feature parity (out of scope).

- [ ] **Step 2: Document the secret-write boundary (D5)**

Confirm: runtime key-rotation secret writes work via `management_api.rs` (pre-existing); CLI-driven secret _push_ is deferred (Christian punts it). Capture this split in the spec so it is a recorded decision, not an accident.

- [ ] **Step 3: Commit any adapter fixups.**

---

## Phase 4: Runtime-config-store spec (the doc his CLI design references but never wrote)

- [ ] **Step 1: Write `docs/superpowers/specs/<date>-runtime-config-store.md`** covering:
  - the load sequence (`build_runtime_services` → `get_settings_from_services` → `settings_from_config_entries`);
  - the **shared `config_payload` contract** (escaping, sorted-key canonicalization, `sha256` over settings-only entries, `ts-config-*` reserved keys) — reference, do not duplicate;
  - the **seed-before-serve** operational contract + the 503 unseeded behavior (Phase 2);
  - empty / missing-key / malformed-hash / transport-error matrix;
  - store-name resolution + `EDGEZERO__STORES__CONFIG__APP_CONFIG__NAME` override;
  - secrets/KV runtime read paths + the secret-write boundary (D5);
  - non-Fastly adapter status.

- [ ] **Step 2: Docs gate** — `cd docs && npm run format` (prettier-clean), then commit.

---

## Phase 5: Stack propagation + re-pin

- [ ] **Step 1: Reconcile topology (D6).** His branch is off `main`; the migration stack is PR14→PR20. Confirm with the team whether the HTTP-layer branch merges via `main` (with his) or threads the stack. Do not push/merge without approval.

- [ ] **Step 2: Re-pin to edgezero `main` after #269 merges** — one-line dep change in `Cargo.toml`, regenerate lock, re-run the Phase 1 gate.

- [ ] **Step 3: Open the PR (approval-gated).** Base = whatever Step 1 resolves. Assign `@me`. Summary: HTTP-layer convergence + runtime hardening + spec.

---

## Risks & watch points

| Risk                                                                                                                                             | Mitigation                                                                                                                                                                       |
| ------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **Fresh deploy = outage** (unseeded store, no fallback)                                                                                          | Phase 2: actionable error + 503; document seed-before-serve; consider a provisioning gate in deploy                                                                              |
| His `Body` fixes incomplete vs our 18 sinks                                                                                                      | Phase 1 Step 1 cross-check against Appendix A                                                                                                                                    |
| He likely didn't run wasm + `--all-targets` + clippy on every leg                                                                                | Phase 1 Step 2 runs the full matrix                                                                                                                                              |
| Pinned to an **open, force-pushable** #269 ref                                                                                                   | Re-pin to `main` post-merge (Phase 5); rollback = revert the dep commit                                                                                                          |
| **Building on a colleague's unmerged WIP branch** (`feature/ts-cli-next`) — it may rebase/force-push out from under us, vanishing our merge-base | Record its SHA at Phase 0 Step 2; if he rebases, re-base from the new SHA and coordinate before any merge; keep our additions as discrete commits so they re-cherry-pick cleanly |
| integration-tests lockfile drift                                                                                                                 | Phase 1 Step 3, targeted `--precise` only                                                                                                                                        |
| Branch topology (his off `main`, stack off PR14)                                                                                                 | Phase 5 Step 1, confirm with team                                                                                                                                                |
| Whole-`Settings`-in-store enlarges blast radius of a bad push                                                                                    | hash verification (his) + malformed-store test (Phase 2 Step 6)                                                                                                                  |

---

## Appendix A — verified `Body::into_bytes` sink reference (authoritative)

From the compiler spike (spec §2/§10): **18 sink bindings, 8 production + 10 test-only**, all `into_bytes` (no `as_bytes` sink). The line numbers below are **PR14-base — they do NOT apply to the `main`-based `feature/ts-cli-next`**; use them only as a count/shape reference (8 prod + 10 test). On any branch, the compiler (`--all-targets`) is the source of truth. Use this to confirm Christian's ad-hoc fixes are complete and when merging up the stack.

- **Production (8):** `proxy.rs:38`, `publisher.rs:46`, `auction/endpoints.rs:81`, `proxy.rs:1550`, `proxy.rs:1665`, `request_signing/endpoints.rs:103/246/365`.
- **Test-only (10):** `auction/formats.rs:444`, `prebid.rs:2067`, `testlight.rs:461`, `proxy.rs:2034/2795/2851`, `publisher.rs:748/1079/1562`, `request_signing/endpoints.rs:464`.
- **Not a sink:** `http_util.rs:456` (the `enforce_max_body_size(bytes: &[u8], …)` signature).
- **Fix style (D3):** production → `into_bytes().ok_or_else(|| <existing error>)?`; compression/test → `unwrap_or_default()`; only `.expect("should …")` where a buffered body is truly invariant.

## Appendix B — fallback: standalone minimal-repin (only if D1 = "keep PR14 stack")

If the team rejects building on his branch, the original minimal-repin still applies: branch off PR14, repin to `2eeccc9`, fix the Appendix A sinks with the D3 style, reconcile the integration-tests lock, full gate, merge up PR14→PR20. (This duplicates his Fastly work and is **not** recommended — see spec §12.)
