# EdgeZero #269 Repin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Repin trusted-server's edgezero dependency from its current pin (`38198f9` on the PR14 base; `170b74b` is the stack's _original_ PR1–13 pin) to post-#269 and fix the only forced code break (`Body::into_bytes` → `Option`), keeping the bespoke `platform/` layer unchanged.

**Architecture:** Mechanical dependency bump on a dedicated branch off `feature/edgezero-pr14-entry-point-dual-path` (never in-place on a reviewed PR), then propagate up the stack by merge. The "test" for this work is the compiler + full CI gate: RED = build errors at `Body` sinks, GREEN = full gate passes. No new abstractions; the A/B/C convergence (store registry, typed `AppConfig`, entry-point) is **out of scope** — separate follow-on plans.

**Tech Stack:** Rust 2024, cargo, `wasm32-wasip1` (Fastly via Viceroy), edgezero git dep, `error-stack`.

**Source spec:** [2026-06-16-edgezero-269-repin-breaking-api-finding.md](../specs/2026-06-16-edgezero-269-repin-breaking-api-finding.md) — §2 (sink list), §5 (steps), §8 (gate), §11 (stack/merge-up).

---

## Scope & non-goals

**In scope:** repin the 4 `edgezero-*` deps; fix the 18 `Body::into_bytes` sinks (8 production + 10 test); reconcile the integration-tests lockfile; drop the two never-compiled adapter deps; pass the full gate; merge up PR14→PR20.

**Out of scope (separate plans):** store convergence onto edgezero `ConfigStore`/`SecretStore`/`StoreRegistry` (spec §6 B); typed `AppConfig` two-tier config (§6 C / Christian's CLI port); entry-point `run_app`/`app!` adoption (§6 C). Do **not** start these here.

**Key constraints:**

- `as_bytes` changed but has **no** trusted-server sink — only `into_bytes` needs edits (spec §2).
- All sinks are buffered (`Once`) bodies → fix with `.expect("should …")`, never `unwrap_or_default()`.
- Line numbers below are **PR14-base**; they shift per branch. **Re-derive the exact sink set from the compiler on each branch** — do not trust hardcoded lines after PR14.
- Pin to the #269 HEAD sha while #269 is open; **re-pin to edgezero `main` after #269 merges**.

---

## File structure

| File                                                          | Change                                               | Responsibility              |
| ------------------------------------------------------------- | ---------------------------------------------------- | --------------------------- |
| `Cargo.toml`                                                  | Modify lines 59–62                                   | The 4 `edgezero-*` git pins |
| `Cargo.lock`                                                  | Regenerated                                          | Root lock                   |
| `crates/integration-tests/Cargo.lock`                         | Reconcile                                            | Shared-dep lock (CI gate)   |
| `crates/trusted-server-core/src/proxy.rs`                     | 5 sinks (38, 1550, 1665 prod; 2034, 2795, 2851 test) | proxy/asset body reads      |
| `crates/trusted-server-core/src/publisher.rs`                 | 4 sinks (46 prod; 748, 1079, 1562 test)              | publisher body reads        |
| `crates/trusted-server-core/src/auction/endpoints.rs`         | 1 sink (81 prod)                                     | auction body read           |
| `crates/trusted-server-core/src/auction/formats.rs`           | 1 sink (444 test)                                    | auction test helper         |
| `crates/trusted-server-core/src/request_signing/endpoints.rs` | 4 sinks (103, 246, 365 prod; 464 test)               | signing endpoint body reads |
| `crates/trusted-server-core/src/integrations/prebid.rs`       | 1 sink (2067 test)                                   | prebid test                 |
| `crates/trusted-server-core/src/integrations/testlight.rs`    | 1 sink (461 test)                                    | testlight test              |

`http_util.rs:456` (`enforce_max_body_size(bytes: &[u8], …)`) is **not** a sink — no edit.

---

## Fix shapes (apply the matching one at each sink)

```rust
// Shape A — value consumed directly (body_as_reader; let body = …into_bytes())
let body = resp.into_body().into_bytes()
    .expect("should have a buffered body");

// Shape B — chained .to_vec()  (String::from_utf8(…into_bytes().to_vec()))
String::from_utf8(
    resp.into_body().into_bytes()
        .expect("should have a buffered body")
        .to_vec(),
)

// Shape C — bound, then borrowed into &[u8]/&Bytes  (enforce_max_body_size(&b)/from_slice(&b))
let b = req.into_body().into_bytes()
    .expect("should have a buffered request body");
enforce_max_body_size(&b, …)?;
serde_json::from_slice(&b)?;
```

`cargo fmt` will rewrap; write the one-liner and let it format.

---

## Task 0: Create the dedicated branch off PR14

**Files:** none (git only)

- [ ] **Step 1: Branch off PR14 (not in-place, not main)**

```bash
git fetch origin
git checkout -b feature/edgezero-269-repin feature/edgezero-pr14-entry-point-dual-path
```

- [ ] **Step 2: Confirm base + capture the authoritative "from" pin**

Run: `git log -1 --format='%s' && grep -m1 'edgezero-core' Cargo.toml`
Expected: PR14 tip; the dep line prints the **base pin = `rev = "38198f9…"`**.

> Pin clarity: the Goal's `170b74b` is the _stack's original_ pin (PR1–13), **not
> this branch's base.** PR14's base is `38198f9` (spec §11). The **only**
> authoritative "from" value is whatever this grep prints — use that, not a
> hardcoded sha, if the branch has advanced.

---

## Task 1: Repin edgezero to #269 + regenerate root lock

**Files:** Modify `Cargo.toml:59-62`, regenerate `Cargo.lock`

- [ ] **Step 1: Repin all 4 deps**

Replace the base pin captured in Task 0 Step 2 (`rev = "38198f9…"` on the 4
`edgezero-*` lines) with `rev = "2eeccc9748daba92b9adf6afe4df105e79269ae9"`
(#269 HEAD). (After #269 merges, use the edgezero `main` sha instead — see spec §9.)

- [ ] **Step 2: Resolve the lock FIRST — separate resolution failure from compile-RED**

Run: `cargo generate-lockfile`
Expected: lock resolves with no error. **If this fails** (MSRV / feature
unification / `spin-sdk` graph — the spec's #1 transitive risk, §5.1/§10), STOP
and surface it: that is a _resolution_ break, not the expected `Body` compile-RED,
and must be triaged before continuing.

- [ ] **Step 3: Capture the RED baseline (build only after the lock resolves)**

Run: `cargo build --workspace --all-targets 2>/tmp/ez_red.log; grep -cE '^error' /tmp/ez_red.log`
Expected: ~27 errors (`E0308`/`E0599`/`E0624`). This is the RED state. (Log goes to
`/tmp` — keep build artifacts out of the repo tree.)

- [ ] **Step 4: Sanity — every error is at a known `Body` sink file, nothing else**

Filter by **location**, not error-kind (an unexpected break could share a kind):

Run:

```bash
grep -A1 '^error' /tmp/ez_red.log | grep -- '-->' \
  | grep -vE 'trusted-server-core/src/(proxy|publisher|auction/(endpoints|formats)|request_signing/endpoints|integrations/(prebid|testlight)|http_util)\.rs' \
  && echo "UNEXPECTED — stop & investigate" || echo OK
```

Expected: `OK` (every error points into a known sink file from §2). Any other
location ⇒ unexpected transitive break; STOP and surface it (spec §5).

- [ ] **Step 5: Commit the repin (still RED — that's expected)**

```bash
git add Cargo.toml Cargo.lock
git commit -m "Repin edgezero to the extensible-cli branch (stackpop/edgezero PR 269)"
```

---

## Task 2: Fix production sinks — `proxy.rs`

**Files:** Modify `crates/trusted-server-core/src/proxy.rs` (sinks 38, 1550, 1665)

- [ ] **Step 1: Confirm current errors (RED)**

Run: `cargo check --workspace 2>&1 | grep 'proxy.rs'`
Expected: errors **within `proxy.rs`** (exact line numbers vary per branch — re-derive
from this output; do not trust the §2 PR14 numbers after PR14). Expect a `body_as_reader`
`Cursor::new(body.into_bytes())` site plus two POST-body `let body_bytes = req.into_body().into_bytes();` sites.

- [ ] **Step 2: Fix `body_as_reader` (Shape A)**

`Cursor::new(body.into_bytes())` → `Cursor::new(body.into_bytes().expect("should have a buffered body"))`

- [ ] **Step 3: Fix the two POST-body bindings (Shape C)**

`let body_bytes = req.into_body().into_bytes();` →
`let body_bytes = req.into_body().into_bytes().expect("should have a buffered request body");`

> `replace_all` only if the two lines are byte-identical (same indentation). **Read
> each site first**; if the replace count ≠ 2, fall back to per-site edits. Both are
> production code (not test).

- [ ] **Step 4: Verify proxy.rs errors cleared**

Run: `cargo check --workspace 2>&1 | grep -c 'proxy.rs' ; echo done`
Expected: `0` proxy.rs errors (other files may still error — fine).

---

## Task 3: Fix production sinks — `publisher.rs` + `auction/endpoints.rs`

**Files:** Modify `publisher.rs` (line 46), `auction/endpoints.rs` (line 81)

- [ ] **Step 1: Fix `publisher.rs:46` `body_as_reader` (Shape A)**

`std::io::Cursor::new(body.into_bytes())` → `std::io::Cursor::new(body.into_bytes().expect("should have a buffered body"))`

- [ ] **Step 2: Fix `auction/endpoints.rs:81` (Shape C)**

`let body_bytes = body.into_bytes();` → `let body_bytes = body.into_bytes().expect("should have a buffered request body");`

- [ ] **Step 3: Verify both cleared**

Run: `cargo check --workspace 2>&1 | grep -cE 'publisher.rs|auction/endpoints.rs'`
Expected: `0`.

---

## Task 4: Fix production sinks — `request_signing/endpoints.rs`

**Files:** Modify `request_signing/endpoints.rs` (lines 103, 246, 365)

- [ ] **Step 1: Fix all three `req.into_body()` bindings (Shape C)**

`replace_all` of `    let body = req.into_body().into_bytes();` →

```rust
    let body = req
        .into_body()
        .into_bytes()
        .expect("should have a buffered request body");
```

> Expect 3 identical occurrences, all production. **Read the sites first**; if the
> replace count ≠ 3 (indentation differs), fall back to per-site edits. The
> `json_response(body: String)` site (`String::into_bytes`) is a false positive —
> **do not touch** (verify by checking the receiver is `String`, not `Body`).

- [ ] **Step 2: Verify lib/bin build is GREEN**

Run: `cargo build --workspace`
Expected: **success** (all 8 production sinks fixed). Tests still red — next.

- [ ] **Step 3: Commit production fixes**

```bash
git add crates/trusted-server-core/src
git commit -m "Adapt Body::into_bytes Option return at production sinks"
```

---

## Task 5: Fix test sinks

**Files:** Modify `proxy.rs` (2034, 2795, 2851), `publisher.rs` (748, 1079, 1562), `auction/formats.rs` (444), `request_signing/endpoints.rs` (464), `integrations/prebid.rs` (2067), `integrations/testlight.rs` (461)

- [ ] **Step 1: Confirm test errors (RED)**

Run: `cargo build --workspace --all-targets 2>&1 | grep -E '^error' | wc -l`
Expected: ~12 remaining errors, all in test code at the lines above.

- [ ] **Step 2: Apply the matching shape at each test sink**

- `String::from_utf8(… .into_bytes().to_vec())` → Shape B (`.expect("should have a buffered body")` before `.to_vec()`): `proxy.rs:2034`, `publisher.rs:748`, `prebid.rs:2067`, `request_signing/endpoints.rs:464`.
- `serde_json::from_slice(&… .into_bytes())` → Shape C (bind, `.expect`, borrow): `auction/formats.rs:444`, `testlight.rs:461`.
- `let x = … .into_bytes();` → Shape A (`.expect`): `proxy.rs:2795`, `proxy.rs:2851`, `publisher.rs:1079`, `publisher.rs:1562`.

(The `request_signing/endpoints.rs:452` test helper is `str::as_bytes` — false positive, **do not touch**.)

- [ ] **Step 3: Verify `--all-targets` is GREEN**

Run: `cargo build --workspace --all-targets`
Expected: **success** — all 18 sinks fixed.

- [ ] **Step 4: Commit test fixes**

```bash
git add crates/trusted-server-core/src
git commit -m "Adapt Body::into_bytes Option return in tests"
```

---

## Task 6: Drop never-compiled adapter deps

**Files:** Modify `Cargo.toml` (remove `edgezero-adapter-axum`, `edgezero-adapter-cloudflare` from `[workspace.dependencies]`)

- [ ] **Step 1: Confirm they are absent from the graph**

Run: `cargo tree -i edgezero-adapter-axum; cargo tree -i edgezero-adapter-cloudflare`
Expected: both → "did not match any packages" (no member uses them — spec §1).

- [ ] **Step 2: Remove the two lines from `Cargo.toml` `[workspace.dependencies]`**

Delete the `edgezero-adapter-axum = …` and `edgezero-adapter-cloudflare = …` lines (keep `edgezero-adapter-fastly` and `edgezero-core`).

- [ ] **Step 3: Verify still builds**

Run: `cargo build --workspace --all-targets`
Expected: success (nothing referenced them).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "Drop unused edgezero axum/cloudflare workspace deps"
```

> If a future `trusted-server-adapter-{axum,cloudflare}` consumer lands, re-add then. Skip this task if the team prefers to keep them pinned for symmetry — note the decision.

---

## Task 7: Reconcile the integration-tests lockfile

> **Ordering matters.** This runs _after_ all root dependency changes (repin Task 1
>
> - drop-adapters Task 6) and _after_ `trusted-server-core` is GREEN (Tasks 2–5).
>   `crates/integration-tests` is a **separate workspace** that path-deps
>   `trusted-server-core` (`Cargo.toml:13`); building it any earlier fails on the
>   Body errors, not lock drift — confounding the check.

**Files:** `crates/integration-tests/Cargo.lock` (and `crates/openrtb-codegen/Cargo.lock` if it drifts)

- [ ] **Step 1: Resolve + build the integration-tests workspace**

Run: `( cd crates/integration-tests && cargo generate-lockfile && cargo build --workspace 2>&1 | tail -20 )`
Expected: lock resolves and it builds. Because core is now green, any failure here
is a _real_ signal — shared-dep drift or a genuine break — not the Body RED.

- [ ] **Step 2: If shared-dep drift, reconcile with targeted updates only**

For each mismatched shared dep (e.g. `bytes`, `http`, `serde`):
Run: `( cd crates/integration-tests && cargo update -p <crate> --precise <root-version> )`
**Never** a blanket `cargo update`. (Project CI gate: shared direct deps must match
root.) Repeat in `crates/openrtb-codegen` if it drifted.

- [ ] **Step 3: Verify**

Run: `( cd crates/integration-tests && cargo build --workspace )`
Expected: success.

- [ ] **Step 4: Commit if changed**

```bash
git add crates/integration-tests/Cargo.lock crates/openrtb-codegen/Cargo.lock
git commit -m "Reconcile integration-tests lockfile after edgezero repin" || echo "nothing to commit"
```

---

## Task 8: Full verification gate

**Files:** none (verification only)

- [ ] **Step 1: Compile gate (host + all-targets)**

Run: `cargo build --workspace --all-targets`
Expected: success.

- [ ] **Step 2: wasm32-wasip1 (Fastly deploy target)**

Run: `cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1`
Expected: success. (First leg not yet verified at spec-freeze — this is the gate that proves it.)

- [ ] **Step 3: Tests**

Run: `cargo test --workspace`
Expected: pass. Watch for behavioral diffs in body-handling tests (they exercise the `.expect()` paths).

- [ ] **Step 4: Clippy + fmt**

Run: `cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 5: integration-tests + JS gates (per CLAUDE.md CI)**

Lock already reconciled (Task 7) — this just runs the suites.
Run: `( cd crates/integration-tests && cargo test --workspace )` and `( cd crates/js/lib && npx vitest run )`
Expected: pass (JS untouched — sanity only).

- [ ] **Step 6: Commit any fmt fixups**

```bash
git add crates Cargo.toml Cargo.lock && git commit -m "Verification gate fixups" || echo "nothing to commit"
```

(Scope the add — never `git add -A`, which would sweep stray build logs into the commit.)

---

## Task 9: Open the PR (PR14-based dedicated branch)

**Files:** none

- [ ] **Step 1: Push + open PR targeting the PR14 branch (or wherever the stack lands)**

```bash
git push -u origin feature/edgezero-269-repin
gh pr create --base feature/edgezero-pr14-entry-point-dual-path \
  --title "Repin edgezero to #269 and adapt Body::into_bytes Option return" \
  --body "See docs/superpowers/specs/2026-06-16-edgezero-269-repin-breaking-api-finding.md"
```

> Do not push or open the PR until the user approves (per project git rules). Confirm the base branch with the team — the stack tip may have advanced.

---

## Task 10: Propagate up the stack (merge, not rebase)

**Files:** none (per-branch merge + gate)

> **Approval gate:** merging up mutates 6 review branches. Do **not** run this task
> (or any `git push`) until the user approves — same rule as Task 9.

For each branch PR15 → PR16 → PR17 → PR18 → PR19 → PR20, in order:

- [ ] **Step 1: Merge the repin forward**

```bash
git checkout feature/edgezero-pr15-remove-fastly-core
git merge feature/edgezero-269-repin    # merge, not rebase (team preference)
```

- [ ] **Step 2: Re-derive sinks from the compiler (line numbers/sink set shift per layer)**

Run: `cargo build --workspace --all-targets 2>&1 | grep -E 'into_bytes|Body'`
Resolve any new/moved sinks with the §2 fix shapes. PR15 (remove-fastly-core) and PR16+ move/delete these files — expect manual conflict resolution, not clean fast-forward.

- [ ] **Step 3: Run the full gate (Task 8) on this branch**

Expected: green before moving to the next branch up.

- [ ] **Step 4: Commit the merge resolution (if any)**

```bash
git add -A && git commit --no-edit || echo "nothing to commit (clean fast-forward)"
```

Repeat for each branch up to PR20.

---

## Follow-on plans (NOT this plan)

Per spec §6/§9, after the repin lands and #269 merges to edgezero `main`:

1. **Re-pin to `main`** (one-line dep change + gate).
2. **Store convergence** (spec §6 B): map `PlatformConfigStore`/`SecretStore` reads onto edgezero `ConfigStore`/`SecretStore`/`StoreRegistry` + thin write-CRUD extension.
3. **Typed `AppConfig` (two-tier)** + **CLI port** (spec §6 C / §7) — Christian. Shared contract = the config struct + `[stores.config]` id; agree before either starts.

Each gets its own spec → plan cycle.
