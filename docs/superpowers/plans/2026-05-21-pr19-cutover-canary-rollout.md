# EdgeZero PR19: Cutover and Canary Rollout — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the existing `edgezero_enabled` binary flag with percentage-based canary routing (`edgezero_rollout_pct` 0–100) so the ops team can advance traffic through 1% → 10% → 50% → 100% with no deploy required at each step, and roll back instantly by setting the key to `"0"`.

**Architecture:** A new `edgezero_rollout_pct` key in the existing `trusted_server_config` Fastly Config Store controls what fraction of traffic takes the EdgeZero path. After `edgezero_enabled` is confirmed `true`, the entry point reads the pct key, hashes the client IP with FNV-1a to derive a deterministic 0–99 bucket, and routes to EdgeZero if `bucket < rollout_pct`. This is pure entry-point logic — no handler or core changes needed. The ops runbook lives at `docs/internal/EDGEZERO_MIGRATION.md` and covers the canary progression, hold-point thresholds, and rollback procedure.

**Tech Stack:** Rust, Fastly Compute WASM, Fastly Config Store (edgezero_core::config_store::ConfigStoreHandle), Viceroy (local test runtime), FNV-1a (inline, no new dep).

**Issue:** https://github.com/IABTechLab/trusted-server/issues/500  
**Epic:** https://github.com/IABTechLab/trusted-server/issues/480  
**Branch off:** `feature/edgezero-pr18-phase5-verification`  
**New branch:** `feature/edgezero-pr19-cutover-canary`

---

## File map

| Action | Path                                               | What changes                                                                                                                            |
| ------ | -------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| Modify | `crates/trusted-server-adapter-fastly/src/main.rs` | Add `EDGEZERO_ROLLOUT_PCT_KEY`, `parse_rollout_pct`, `fnv1a_bucket`, `canary_routes_to_edgezero`, `read_rollout_pct`; refactor `main()` |
| Modify | `fastly.toml`                                      | Add `edgezero_rollout_pct = "0"` to local config store with comment                                                                     |
| Create | `docs/internal/EDGEZERO_MIGRATION.md`              | Ops runbook: config keys, canary progression, hold points, thresholds, rollback                                                         |

---

## Background (read this before touching code)

`main.rs:55` already defines `EDGEZERO_ENABLED_KEY = "edgezero_enabled"` and `main.rs:109` implements `is_edgezero_enabled()`.

Current `main()` flow (lines 129–159):

1. Open config store (`open_trusted_server_config_store`) — on error → `legacy_main`
2. Check `edgezero_enabled` flag — if false/err → `legacy_main`
3. If true → `edgezero_main`

**No** `edgezero_rollout_pct` key exists anywhere yet. After this PR the flow becomes:

1. Open config store — on error → `legacy_main`
2. Check `edgezero_enabled` — if false/err → `legacy_main`
3. Read `rollout_pct` from `read_rollout_pct()` (absent key = 100, invalid = 0-safe)
4. Compute `bucket = fnv1a_bucket(client_ip_string)`
5. If `bucket < rollout_pct` → `edgezero_main`, else → `legacy_main`

`ConfigStoreHandle` comes from `edgezero_core::config_store` and cannot be constructed in unit tests (needs Fastly runtime). Test only pure functions.

Run tests with: `cargo test-fastly` (compiles to wasm32-wasip1 and runs under Viceroy).

> **Note on routing key:** The spec (§Cutover plan) says "hash of request ID." This plan uses
> client IP instead, which gives sticky per-user routing — the same user always gets the same
> path during the canary, preventing split-session bugs. Request ID changes per-request and
> would route a single user's session across both paths simultaneously.

---

## Setup

Before implementing, create the PR19 branch from the current base:

- [ ] **Create branch**

```bash
git checkout -b feature/edgezero-pr19-cutover-canary
```

---

## Task 1: Pure routing helpers — `parse_rollout_pct`, `fnv1a_bucket`, `canary_routes_to_edgezero`

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

These three are pure functions — no Fastly API calls. Test first (TDD), implement second.

- [ ] **Step 1: Write the failing tests**

Add to `#[cfg(test)] mod tests` in `main.rs` (after the existing `rejects_non_true_flag_values` test):

```rust
// ---------------------------------------------------------------------------
// parse_rollout_pct
// ---------------------------------------------------------------------------

#[test]
fn parses_valid_rollout_percentages() {
    assert_eq!(parse_rollout_pct("0"), Some(0), "should parse '0'");
    assert_eq!(parse_rollout_pct("1"), Some(1), "should parse '1'");
    assert_eq!(parse_rollout_pct("50"), Some(50), "should parse '50'");
    assert_eq!(parse_rollout_pct("100"), Some(100), "should parse '100'");
    assert_eq!(
        parse_rollout_pct("  50  "),
        Some(50),
        "should trim whitespace"
    );
}

#[test]
fn rejects_invalid_rollout_percentages() {
    assert_eq!(
        parse_rollout_pct("101"),
        None,
        "should reject values above 100"
    );
    assert_eq!(
        parse_rollout_pct(""),
        None,
        "should reject empty string"
    );
    assert_eq!(
        parse_rollout_pct("abc"),
        None,
        "should reject non-integer"
    );
    assert_eq!(
        parse_rollout_pct("-1"),
        None,
        "should reject negative value"
    );
    assert_eq!(
        parse_rollout_pct("1.5"),
        None,
        "should reject decimal value"
    );
}

// ---------------------------------------------------------------------------
// fnv1a_bucket
// ---------------------------------------------------------------------------

#[test]
fn bucket_is_in_range_0_to_99() {
    for key in &["1.2.3.4", "255.255.255.255", "::1", "", "unknown"] {
        let b = fnv1a_bucket(key);
        assert!(b < 100, "bucket must be 0..100 for key {key:?}, got {b}");
    }
}

#[test]
fn bucket_is_deterministic() {
    let key = "192.168.1.1";
    assert_eq!(
        fnv1a_bucket(key),
        fnv1a_bucket(key),
        "same key must produce the same bucket"
    );
}

#[test]
fn bucket_distributes_across_range() {
    // Smoke-test that fnv1a_bucket produces a spread of values (not a constant).
    // 256 distinct IP-like keys must produce at least 50 unique buckets.
    // This would catch a regression where the hash is replaced with a constant
    // or zero-fill.
    let buckets: std::collections::HashSet<u8> = (0u16..=255)
        .map(|i| fnv1a_bucket(&format!("10.0.0.{i}")))
        .collect();
    assert!(
        buckets.len() > 50,
        "fnv1a_bucket should distribute across buckets; got only {} unique values in 256 keys",
        buckets.len()
    );
}

#[test]
fn empty_key_bucket_is_valid() {
    let b = fnv1a_bucket("");
    assert!(b < 100, "empty key must still produce a valid bucket, got {b}");
}

// ---------------------------------------------------------------------------
// canary_routes_to_edgezero
// ---------------------------------------------------------------------------

#[test]
fn rollout_zero_routes_all_to_legacy() {
    for bucket in 0u8..100 {
        assert!(
            !canary_routes_to_edgezero(bucket, 0),
            "pct=0 should route all to legacy, bucket={bucket}"
        );
    }
}

#[test]
fn rollout_hundred_routes_all_to_edgezero() {
    for bucket in 0u8..100 {
        assert!(
            canary_routes_to_edgezero(bucket, 100),
            "pct=100 should route all to EdgeZero, bucket={bucket}"
        );
    }
}

#[test]
fn rollout_fifty_routes_exactly_half_of_bucket_space() {
    let edgezero_count = (0u8..100)
        .filter(|&b| canary_routes_to_edgezero(b, 50))
        .count();
    assert_eq!(
        edgezero_count, 50,
        "pct=50 should route exactly 50 out of 100 buckets to EdgeZero"
    );
}

#[test]
fn rollout_one_routes_exactly_one_bucket() {
    let edgezero_count = (0u8..100)
        .filter(|&b| canary_routes_to_edgezero(b, 1))
        .count();
    assert_eq!(
        edgezero_count, 1,
        "pct=1 should route exactly 1 out of 100 buckets to EdgeZero"
    );
}
```

- [ ] **Step 2: Run tests — verify they FAIL**

```bash
cargo test-fastly 2>&1 | head -40
```

Expected: compile errors for `parse_rollout_pct`, `fnv1a_bucket`, `canary_routes_to_edgezero` not found.

- [ ] **Step 3: Implement the three pure functions**

Add after the `parse_edgezero_flag` function (around line 86 in `main.rs`):

```rust
/// Parses a rollout percentage string into a value in `0..=100`.
///
/// Accepts only integer strings in the range 0–100 (inclusive) after whitespace
/// trimming. Returns `None` for anything else: non-integer, out-of-range,
/// empty string.
fn parse_rollout_pct(value: &str) -> Option<u8> {
    let n: u16 = value.trim().parse().ok()?;
    if n > 100 {
        return None;
    }
    Some(n as u8)
}

/// Maps an arbitrary string to a deterministic bucket in `0..100`.
///
/// Uses FNV-1a (32-bit variant) to produce a uniform-enough distribution for
/// canary traffic splitting without pulling in any hash crates. The same input
/// always produces the same output across Rust versions because the algorithm
/// is defined here, not delegated to `DefaultHasher`.
fn fnv1a_bucket(key: &str) -> u8 {
    const FNV_OFFSET: u32 = 2_166_136_261;
    const FNV_PRIME: u32 = 16_777_619;
    let mut hash = FNV_OFFSET;
    for byte in key.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    (hash % 100) as u8
}

/// Returns `true` if the given bucket should be routed to the EdgeZero path.
///
/// `bucket` must be in `0..100`; `rollout_pct` in `0..=100`.
/// When `rollout_pct = 0` no bucket ever routes to EdgeZero (instant rollback).
/// When `rollout_pct = 100` every bucket routes to EdgeZero (full cutover).
fn canary_routes_to_edgezero(bucket: u8, rollout_pct: u8) -> bool {
    bucket < rollout_pct
}
```

- [ ] **Step 4: Run tests — verify they PASS**

```bash
cargo test-fastly 2>&1 | tail -20
```

Expected: `test result: ok. N passed; 0 failed`

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Add canary routing helpers: parse_rollout_pct, fnv1a_bucket, canary_routes_to_edgezero"
```

---

## Task 2: `read_rollout_pct` and `main()` refactor

**Files:**

- Modify: `crates/trusted-server-adapter-fastly/src/main.rs`

`read_rollout_pct` wraps `ConfigStoreHandle` which requires the Fastly runtime, so no unit test for it — we rely on the `parse_rollout_pct` tests above covering its parse logic. The `main()` refactor is exercised by the full `cargo test-fastly` run.

- [ ] **Step 1: Add the `EDGEZERO_ROLLOUT_PCT_KEY` constant**

After `EDGEZERO_ENABLED_KEY` (line 55 of `main.rs`):

```rust
const EDGEZERO_ROLLOUT_PCT_KEY: &str = "edgezero_rollout_pct";
```

- [ ] **Step 2: Implement `read_rollout_pct`**

Add after the `is_edgezero_enabled` function (after line 114):

```rust
/// Reads `edgezero_rollout_pct` from the config store.
///
/// | Config store state              | Return value | Effect                     |
/// |---------------------------------|--------------|----------------------------|
/// | Key absent                      | `100`        | Full rollout (backward compat) |
/// | Key present, valid 0–100        | parsed value | Partial or full rollout    |
/// | Key present, invalid            | `0`          | All legacy (safe default)  |
/// | Key read error                  | `0`          | All legacy (safe default)  |
///
fn read_rollout_pct(config_store: &ConfigStoreHandle) -> u8 {
    match config_store.get(EDGEZERO_ROLLOUT_PCT_KEY) {
        Ok(Some(value)) => match parse_rollout_pct(&value) {
            Some(pct) => pct,
            None => {
                log::warn!(
                    "invalid edgezero_rollout_pct value {:?}, defaulting to 0 (legacy path)",
                    value
                );
                0
            }
        },
        Ok(None) => 100,
        Err(e) => {
            log::warn!(
                "failed to read edgezero_rollout_pct: {e}, defaulting to 0 (legacy path)"
            );
            0
        }
    }
}
```

- [ ] **Step 3: Refactor `main()` to use canary routing**

Replace the **entire body of `main()`** (lines 129–159 in `main.rs`) with the function below.
Do not partially splice — replace the whole function to avoid duplicate `let req` declarations
or a duplicated health check:

```rust
fn main() {
    let req = FastlyRequest::from_client();

    // Health probe bypasses logging, settings, and app construction as a cheap liveness signal.
    if let Some(response) = health_response(&req) {
        response.send_to_client();
        return;
    }

    logging::init_logger();

    let edgezero_config_store = match open_trusted_server_config_store() {
        Ok(config_store) => config_store,
        Err(e) => {
            log::warn!("failed to open EdgeZero config store, falling back to legacy path: {e}");
            legacy_main(req);
            return;
        }
    };

    if !is_edgezero_enabled(&edgezero_config_store).unwrap_or_else(|e| {
        log::warn!("failed to read edgezero_enabled flag, falling back to legacy path: {e}");
        false
    }) {
        log::debug!("routing request through legacy path (edgezero_enabled=false)");
        legacy_main(req);
        return;
    }

    let rollout_pct = read_rollout_pct(&edgezero_config_store);
    let routing_key = req
        .get_client_ip_addr()
        .map(|ip| ip.to_string())
        .unwrap_or_default();
    let bucket = fnv1a_bucket(&routing_key);

    if canary_routes_to_edgezero(bucket, rollout_pct) {
        log::debug!(
            "routing request through EdgeZero path (bucket={bucket}, rollout_pct={rollout_pct})"
        );
        edgezero_main(req, edgezero_config_store);
    } else {
        log::debug!(
            "routing request through legacy path (bucket={bucket}, rollout_pct={rollout_pct})"
        );
        legacy_main(req);
    }
}
```

- [ ] **Step 4: Run tests, fmt, and clippy**

```bash
cargo test-fastly 2>&1 | tail -20
```

Expected: all tests pass.

```bash
cargo fmt --all -- --check 2>&1 | tail -10
```

Expected: no diff. If there is output, run `cargo fmt --all` to fix, then re-check.

```bash
cargo clippy -p trusted-server-adapter-fastly --all-targets --all-features -- -D warnings 2>&1 | tail -20
```

Expected: no warnings. (`--all-targets` covers the test binary so the new test code is also linted.)

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-adapter-fastly/src/main.rs
git commit -m "Add edgezero_rollout_pct canary routing to Fastly entry point

When edgezero_enabled=true, reads edgezero_rollout_pct (0-100) from the
trusted_server_config store. Routes each request to EdgeZero if
fnv1a_bucket(client_ip) < rollout_pct, giving the ops team sticky
per-user canary control without a deploy.

Absent key defaults to 100 (full rollout, backward compatible with
edgezero_enabled=true deployments that predate this PR). Invalid values
and read errors default to 0 (all legacy, fail-safe)."
```

---

## Task 3: Update `fastly.toml` — add `edgezero_rollout_pct` key

**Files:**

- Modify: `fastly.toml`

- [ ] **Step 1: Add `edgezero_rollout_pct` to the local config store**

In `fastly.toml`, within `[local_server.config_stores.trusted_server_config.contents]`, add after the `edgezero_enabled` key. Match the existing 12-space indent style of the file:

```toml
        [local_server.config_stores.trusted_server_config.contents]
            # "true" / "1" (case-insensitive) enable the EdgeZero path. Missing,
            # unreadable, or any other value falls back to the legacy entry point.
            # Keep "false" until EdgeZero reaches full functional parity with legacy.
            edgezero_enabled = "false"
            # Integer 0-100. Effective only when edgezero_enabled = "true".
            #   0    -> all traffic to legacy (instant rollback — no deploy needed)
            #   1-99 -> canary: clients whose fnv1a_bucket(client_ip) < this value go EdgeZero
            #   100  -> all traffic to EdgeZero (full cutover)
            # Key absent when edgezero_enabled = "true" is treated as 100 (full rollout).
            # IMPORTANT: Set this to "0" in production BEFORE setting edgezero_enabled = "true".
            edgezero_rollout_pct = "0"
```

- [ ] **Step 2: Verify local Viceroy still starts without errors**

```bash
cargo build --bin trusted-server-adapter-fastly --release --target wasm32-wasip1 2>&1 | tail -5
```

Expected: `Finished release profile [optimized] target(s)`

- [ ] **Step 3: Commit**

```bash
git add fastly.toml
git commit -m "Add edgezero_rollout_pct key to local Viceroy config store

Set to \"0\" so local dev and CI stay on the legacy path by default.
Ops changes this in the production config store — no re-deploy required."
```

---

## Task 4: Write ops runbook — `docs/internal/EDGEZERO_MIGRATION.md`

**Files:**

- Create: `docs/internal/EDGEZERO_MIGRATION.md`

This document is the single source of truth for ops running the canary progression. Create it with the exact content below.

- [ ] **Step 1: Create `docs/internal/` and the runbook file**

```bash
mkdir -p docs/internal
```

Create `docs/internal/EDGEZERO_MIGRATION.md` with exactly this content:

```markdown
# EdgeZero Migration Runbook

Operational reference for the Fastly Compute EdgeZero canary rollout
(issue [#500](https://github.com/IABTechLab/trusted-server/issues/500),
epic [#480](https://github.com/IABTechLab/trusted-server/issues/480)).

---

## Config store keys

Config store name: **`trusted_server_config`** (Fastly service config store)

| Key                    | Type                 | Effect                                                                                                                            |
| ---------------------- | -------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| `edgezero_enabled`     | `"true"` / `"false"` | Master on/off switch. Set `"false"` to disable EdgeZero entirely, regardless of rollout_pct.                                      |
| `edgezero_rollout_pct` | `"0"` – `"100"`      | Percentage of traffic (by client IP bucket) routed to EdgeZero. Only read when `edgezero_enabled = "true"`. Key absent = `"100"`. |

**Routing logic:** `fnv1a_bucket(client_ip) < edgezero_rollout_pct` → EdgeZero, else legacy.
Same client IP always gets the same bucket — routing is sticky per user.

### Safe defaults / failure modes

| Condition                                           | Effective behaviour   |
| --------------------------------------------------- | --------------------- |
| Config store unreachable                            | All legacy            |
| `edgezero_enabled` unreadable                       | All legacy            |
| `edgezero_rollout_pct` absent (but enabled=true)    | All EdgeZero (100%)   |
| `edgezero_rollout_pct` invalid (non-integer, > 100) | All legacy            |
| `edgezero_rollout_pct = "0"`                        | All legacy (rollback) |

> ⚠️ **Do NOT delete `edgezero_rollout_pct` while `edgezero_enabled = "true"`.** An absent key
> is treated as 100 (full rollout) for backward compatibility. If you want to pause or roll back,
> **set the value to `"0"`** — do not delete it.

---

## Canary progression

> **Pre-condition:** All Phase 5 verification gates (PR18) passed.
>
> **Production key setup order (important):** Set `edgezero_rollout_pct = "0"` in the
> production config store **before** setting `edgezero_enabled = "true"`. If you set
> `edgezero_enabled` first and `edgezero_rollout_pct` is absent, the absent-key default
> (100) kicks in immediately, routing all traffic to EdgeZero without a staged canary.

### Stage 1 — 1%

1. Set `edgezero_rollout_pct = "1"` in the production config store.
2. Hold **30 minutes**.
3. Check pass/fail thresholds (see below).
4. If all green → advance to Stage 2. If any threshold breached → rollback.

### Stage 2 — 10%

1. Set `edgezero_rollout_pct = "10"`.
2. Hold **2 hours** (same time-of-day window as the 7-day baseline).
3. Check pass/fail thresholds.
4. If all green → advance to Stage 3. If any threshold breached → rollback.

### Stage 3 — 50%

1. Set `edgezero_rollout_pct = "50"`.
2. Hold **24 hours**.
3. Check pass/fail thresholds. Pay particular attention to auction win-rate.
4. If all green → advance to Stage 4. If any threshold breached → rollback.

### Stage 4 — 100% (full cutover)

1. Set `edgezero_rollout_pct = "100"`.
2. Hold **48 hours** before decommissioning the legacy entry point.
3. Confirm zero regressions across all metrics.
4. Open legacy cleanup PR (removes `legacy_main()` and flag plumbing, see issue #495).

---

## Pass/fail thresholds

**Baseline definition:** 7-day rolling average from production Fastly service
metrics, sampled from the same time-of-day window as the canary observation
period (to account for diurnal traffic patterns).

| Metric           | Threshold                | Action if breached                     |
| ---------------- | ------------------------ | -------------------------------------- |
| Error rate (5xx) | > 0.1% above baseline    | **Immediate rollback**                 |
| p95 latency      | > 15% above baseline     | Hold; rollback if no fix within 1 hour |
| Auction win-rate | > 1% delta from baseline | Hold; investigate                      |
| Timeout rate     | > 2× baseline            | **Immediate rollback**                 |

> **Note on p95 threshold:** The spec §Cutover paragraph mentions ±10% as the Stage 2 hold-point
> criterion; the threshold table at §Pass/fail thresholds says 15%. These two values are
> inconsistent in the spec. This runbook adopts the threshold table (15%) as the governing
> number because it applies uniformly across all stages. If ops adopts a stricter 10% target
> at Stage 2, update this table accordingly.

---

## Rollback procedure

Rollback is **immediate, no deploy required**.

1. Set `edgezero_rollout_pct = "0"` in the production config store.
   Traffic shifts back to legacy within seconds (next request per Wasm instance).
2. Optionally set `edgezero_enabled = "false"` as belt-and-suspenders.
3. Investigate root cause before re-advancing the canary.
4. Keep the legacy entry point (`legacy_main()`) available until at least one
   full release cycle after reaching 100% with zero regressions.

---

## Monitoring

Fastly real-time stats dashboard. Key signals at each canary stage:

- **Error rate:** `5xx / total_requests` by edge PoP
- **Latency p95:** use log search for `routing request through EdgeZero path` to identify EdgeZero traffic (`x-edgezero-path` instrumentation header does not exist yet — follow-up task)
- **Auction win-rate:** downstream SSP reporting, compare same-day prior week
- **Timeout rate:** `504 / total_requests`

> Log lines in Viceroy / Fastly log tailing:
> `routing request through EdgeZero path (bucket=N, rollout_pct=M)` — confirms canary traffic.
> `routing request through legacy path (bucket=N, rollout_pct=M)` — confirms legacy traffic.

---

## Reference

- Spec: `docs/superpowers/specs/2026-03-19-edgezero-migration-design.md` §Cutover plan
- Plan: `docs/superpowers/plans/2026-05-21-pr19-cutover-canary-rollout.md`
- Legacy cleanup: issue [#495](https://github.com/IABTechLab/trusted-server/issues/495)
```

- [ ] **Step 2: Verify `docs/internal/` is covered by Prettier, then check format**

First confirm the Prettier glob recurses into `internal/`:

```bash
cd docs && cat package.json | grep -A3 '"format"'
```

If the glob is `"*.md"` (flat), update it to `"**/*.md"` before proceeding. Then:

```bash
cd docs && npm run format -- --check 2>&1 | tail -10
```

Expected: `All matched files use Prettier code style!` (or no diff output)

If not: `cd docs && npm run format` then re-check.

- [ ] **Step 3: Commit**

```bash
git add docs/internal/EDGEZERO_MIGRATION.md
git commit -m "Add EdgeZero canary rollout ops runbook

Documents the two config store keys, canary progression stages (1% →
10% → 50% → 100%), hold-point durations, pass/fail thresholds, and
instant rollback procedure."
```

---

## Final verification

- [ ] **Run full test suite**

```bash
cargo test-fastly 2>&1 | tail -20
```

Expected: all tests pass, including the 10 new canary routing tests.

- [ ] **Run fmt and clippy**

```bash
cargo fmt --all -- --check 2>&1 | tail -10
cargo clippy -p trusted-server-adapter-fastly --all-targets --all-features -- -D warnings 2>&1 | tail -10
```

Expected: both clean.

- [ ] **Verify docs format**

```bash
cd docs && npm run format -- --check
```

- [ ] **Push branch and open PR against `feature/edgezero-pr18-phase5-verification`**

```bash
git push -u origin feature/edgezero-pr19-cutover-canary
```

Open PR:

- Base branch: `feature/edgezero-pr18-phase5-verification`
- Title: `Add edgezero_rollout_pct canary routing (PR19)`
- Closes: #500

---

## Follow-up items (out of scope for this PR)

- **`x-edgezero-path` diagnostic header**: Add `x-edgezero-path: true` to `edgezero_main` responses so Fastly dashboards and Prettier filters can distinguish EdgeZero vs legacy traffic without log scraping. Track as a separate issue.
- **`docs/README.md` runbook pointer**: Add a line referencing `docs/internal/EDGEZERO_MIGRATION.md` so ops can discover it without grepping.

---

## Invariants to preserve

- **No new dependencies** — FNV-1a is inlined, not a crate import.
- **Backward compatible** — `edgezero_rollout_pct` absent + `edgezero_enabled=true` still means 100% EdgeZero, matching pre-PR19 behavior.
- **Fail-safe** — any error reading `edgezero_rollout_pct` produces 0 (all legacy), never 100. Avoids a misconfigured key accidentally routing all traffic to EdgeZero.
- **No handler or core changes** — all changes are confined to `main.rs` entry-point logic and `fastly.toml`.
- **Sticky routing** — client IP hash is deterministic; the same user always gets the same path at a given `rollout_pct`, preventing split-session bugs.
