# Issue #852 implementation plan — part 1 of 3: predicate split and observability

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Separate the "may this origin response be shared" condition from the template-specific
one, and report both caches' outcomes on the existing auction telemetry row, so the later
readthrough change is measurable from its first deploy.

**Architecture:** One pure refactor, one small piece of new derivation, and three new nullable
telemetry fields. The refactor extracts both predicates into pure functions and splits them,
changing no behavior. The new derivation produces a structured reason for the **request-side**
template-cache bypass, which today has none. The telemetry fields ride the existing
`AuctionObservationContext` → `AuctionEventRow` → Tinybird path.

**Tech Stack:** Rust 2024, `wasm32-wasip1` via Viceroy, `serde_json` NDJSON, Tinybird ClickHouse
datasource.

**Spec:** `docs/superpowers/specs/2026-09-15-852-template-and-origin-caching-design.md` — read
"Splitting the predicate" and "Observability" before starting.

**All of #852 ships as one PR.** This document is the first of three plan parts covering that one
change set, in commit order:

| Part     | Covers                                                                         | Plan                                 |
| -------- | ------------------------------------------------------------------------------ | ------------------------------------ |
| 1 (this) | Predicate split, observability, Tinybird migration, docs caveats, CI gate list | this file                            |
| 2        | Origin shareability probe; purge endpoint, key plumbing and CLI                | `2026-09-15-852-probe-and-purge.md`  |
| 3        | Readthrough gate, `ts-origin` staging check, runbook                           | `2026-09-15-852-readthrough-gate.md` |

Complete them in order. Parts 2 and 3 depend on the binding and the telemetry this part creates.

**Behavior impact of this part alone:** no change to responses, cache decisions, or the
`x-ts-template-cache` header — nothing consumes `origin_response_is_shareable` until part 3, and
the predicate split is behavior-neutral by construction. The telemetry row shape **does** change:
three always-serialized fields, which is why the Tinybird migration must reach Tinybird before the
release deploys.

---

## Two things review found that shape this plan

**1. The bypass reason has two sources and only one exists.** `template_cache_ttl`
(`publisher.rs:6129`) is called inside `template_cache_reservation.and_then(...)` (`:4775`), and
a reservation exists only when `template_cache_key` was built — which is
`request_can_use_shared_template.then(...)` (`:4370`). So `InlineMode`, `AuthorizedRequest` and
`CookieForwarded` can never fire there; those requests never get a key. The request-side bypass
sets only `TemplateCacheResponseState::BypassRequest` (`:4386`) and free-text `log::debug!`
(`:4360-4369`). Task 7 builds the missing request-side derivation.

**2. No test harness has both a template cache and a telemetry sink.** `services()` (`:9247`)
sets `.template_cache(...)` and no sink; `services_with_telemetry()` (`:13719`) sets the sink and
no cache. Task 5 builds the combined one. Do not attempt Tasks 6–9 before it exists.

---

## File structure

| File                                                  | Responsibility                                                          | Change                                                                                                                                    |
| ----------------------------------------------------- | ----------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/publisher.rs`         | Publisher path; both predicates, the observation, the cache-state local | Predicate functions near `:4325`; observation `:4461`; request-side reason `:4370-4386`; response-side reason `:4785`; state hook `:4824` |
| `crates/trusted-server-core/src/auction/telemetry.rs` | Observation context, row schema, NDJSON                                 | 3 fields on `AuctionObservationContext` (`:99`) and `AuctionEventRow` (`:277`); wire `base()` (`:347`)                                    |
| `tinybird/datasources/auction_events_raw.datasource`  | ClickHouse columns                                                      | Add 3 nullable columns                                                                                                                    |
| `tinybird/fixtures/auction_events_raw.ndjson`         | Fixture rows                                                            | Add 3 keys to all 8 rows                                                                                                                  |

---

## Task 0: Confirm the approval gate, and know the trim boundary

The spec's Open risks section flags this work specifically:

> **Observability is the largest refactor here and is not in #852.** Turning
> `AuctionObservationContext` from an immutable snapshot into a mutable accumulator, plus a
> 35-column schema migration with quarantine risk, sits close to AGENTS.md's "no large refactors
> without approval". It needs explicit approval before PR 1.

- [ ] **Step 1: Get explicit approval before writing code.** Tasks 2 and 4 are exactly the
      refactor and the migration named above.

- [ ] **Step 2: If approval is withheld, take the trim instead of abandoning the PR.** The spec's
      trim is to drop `template_cache_state` — it is already on the `x-ts-template-cache` response
      header — and keep `template_cache_bypass_reason` and `origin_cache_shareable`, which carry
      the triage. Concretely that means: **skip Task 9 entirely**, and drop the
      `template_cache_state` field from Tasks 2, 3 and 4 (struct field, `base()` wiring,
      datasource column, fixture key). Everything else is unchanged. Task 9 is also the most
      intricate task in the plan, so the trimmed form is substantially cheaper.

- [ ] **Step 3: Record which form you are building** in the PR description, so a reviewer does not
      read a missing `template_cache_state` as an oversight.

---

## Task 1: Extract the predicates as pure functions, then split

The split must be guarded by a test that exercises **production code**. A table test that
re-types the boolean expression guards nothing — it passes even if the refactor drops a term.

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs:4325-4331`
- Test: same file, `#[cfg(test)]`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn template_eligibility_implies_origin_shareability() {
    for bits in 0u8..128 {
        let inputs = SharedRequestInputs {
            method_is_cacheable: bits & 1 != 0,
            host_present: bits & 2 != 0,
            authorization_disqualifies: bits & 4 != 0,
            cookie_disqualifies: bits & 8 != 0,
            request_requires_origin: bits & 16 != 0,
        };
        let is_esi = bits & 32 != 0;
        let reader_supports_assembly = bits & 64 != 0;

        let shareable = origin_response_is_shareable(inputs);
        let template = request_can_use_shared_template(inputs, is_esi, reader_supports_assembly);

        assert!(
            !template || shareable,
            "template eligibility must imply origin shareability, input bits {bits}"
        );
        assert_eq!(
            template,
            shareable && is_esi && reader_supports_assembly,
            "template eligibility must be the shared base plus the two template conditions, \
             input bits {bits}"
        );
    }
}

#[test]
fn every_shared_input_is_necessary_for_shareability() {
    let all_good = SharedRequestInputs {
        method_is_cacheable: true,
        host_present: true,
        authorization_disqualifies: false,
        cookie_disqualifies: false,
        request_requires_origin: false,
    };
    assert!(origin_response_is_shareable(all_good));

    for (label, broken) in [
        ("method", SharedRequestInputs { method_is_cacheable: false, ..all_good }),
        ("host", SharedRequestInputs { host_present: false, ..all_good }),
        ("authorization", SharedRequestInputs { authorization_disqualifies: true, ..all_good }),
        ("cookie", SharedRequestInputs { cookie_disqualifies: true, ..all_good }),
        ("requires-origin", SharedRequestInputs { request_requires_origin: true, ..all_good }),
    ] {
        assert!(
            !origin_response_is_shareable(broken),
            "dropping the {label} condition must make the request unshareable"
        );
    }
}
```

The second test is the one that actually catches a mistyped refactor.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test-fastly -- publisher::tests::template_eligibility_implies --nocapture`
Expected: FAIL — `SharedRequestInputs` not found.

- [ ] **Step 3: Add the type and functions**

Above `handle_publisher_request`, add:

```rust
/// Request-side conditions that decide whether this request's origin response may be shared
/// between readers. Necessary for both the readthrough cache and the template cache, which is
/// why it is one type rather than two parallel expressions that must be kept in step.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SharedRequestInputs {
    pub(crate) method_is_cacheable: bool,
    pub(crate) host_present: bool,
    pub(crate) authorization_disqualifies: bool,
    pub(crate) cookie_disqualifies: bool,
    pub(crate) request_requires_origin: bool,
}

/// Whether this request's origin response may be shared between readers at all.
pub(crate) fn origin_response_is_shareable(inputs: SharedRequestInputs) -> bool {
    inputs.method_is_cacheable
        && inputs.host_present
        && !inputs.authorization_disqualifies
        && !inputs.cookie_disqualifies
        && !inputs.request_requires_origin
}

/// Whether this request may additionally use a shared *template*. The two extra conditions say
/// whether this pipeline can assemble one, not whether the origin's bytes may be shared.
pub(crate) fn request_can_use_shared_template(
    inputs: SharedRequestInputs,
    assembly_mode_is_esi: bool,
    reader_supports_assembly: bool,
) -> bool {
    origin_response_is_shareable(inputs) && assembly_mode_is_esi && reader_supports_assembly
}
```

- [ ] **Step 4: Replace the inline expression**

At `publisher.rs:4325-4331`, replace the `let request_can_use_shared_template = …` binding with:

```rust
    let shared_request_inputs = SharedRequestInputs {
        method_is_cacheable,
        host_present: !request_host.is_empty(),
        authorization_disqualifies,
        cookie_disqualifies,
        request_requires_origin,
    };
    let origin_response_is_shareable = origin_response_is_shareable(shared_request_inputs);
    let request_can_use_shared_template = request_can_use_shared_template(
        shared_request_inputs,
        matches!(assembly_mode, AssemblyMode::Esi),
        reader_supports_assembly,
    );
```

If shadowing a function name with a local trips clippy, rename the locals to
`origin_is_shareable` / `can_use_shared_template` and update their use sites.

- [ ] **Step 5: Verify**

Run: `cargo test-fastly`
Expected: PASS, no newly failing tests. (The alias already names all four wasm packages; an
extra `-p` narrows nothing.) Any template-cache test changing outcome means the
refactor was not behavior-neutral — revert and re-derive.

Run: `cargo clippy-fastly`
Expected: no warnings. `origin_response_is_shareable` is unused until Task 6; if clippy objects,
land Task 6 before committing rather than adding an allow.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Split origin shareability out of template eligibility

The single predicate mixed two questions: whether the origin response may be
shared at all, and whether this pipeline can assemble a shared template.
Gating anything but the template cache on the combined form would couple
readthrough caching to the assembly mode for no safety reason.

Extracted as pure functions so the invariant is testable against real code
rather than a re-typed copy of the expression. Behavior is unchanged."
```

---

## Task 2: Add the cache fields to the observation context

**Files:**

- Modify: `crates/trusted-server-core/src/auction/telemetry.rs` — struct `:99`, `from_parts` `:158`, `from_auction_request` `:130`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn observation_cache_fields_default_to_absent_and_round_trip() {
    let mut observation = test_observation();

    assert_eq!(observation.origin_cache_shareable, None);
    assert_eq!(observation.template_cache_state, None);
    assert_eq!(observation.template_cache_bypass_reason, None);

    observation.set_origin_cache_shareable(true);
    observation.set_template_cache_state("hit");
    observation.set_template_cache_bypass_reason("request carried Cookie");

    assert_eq!(observation.origin_cache_shareable, Some(true));
    assert_eq!(observation.template_cache_state.as_deref(), Some("hit"));
    assert_eq!(
        observation.template_cache_bypass_reason.as_deref(),
        Some("request carried Cookie")
    );
}
```

There is **no** `test_observation()` helper in `telemetry.rs`'s test module — add one, because
Tasks 2 and 3 both need it and they must build the context identically. Follow the existing
pattern at `telemetry.rs:1032-1052`: `EcContext::new_for_test(None, ConsentContext::default())`
then `AuctionObservationContext::from_parts(...)`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test-fastly -- auction::telemetry::tests::observation_cache_fields --nocapture`
Expected: FAIL — no field `origin_cache_shareable`.

- [ ] **Step 3: Add the fields and setters**

Add to `AuctionObservationContext` after `slot_count`, before the private `started_at`:

```rust
    /// Whether the readthrough gate admitted this request. `None` on sources that do not make
    /// the decision, which is not the same as `Some(false)`.
    pub origin_cache_shareable: Option<bool>,
    /// Terminal template-cache state, matching `x-ts-template-cache`.
    pub template_cache_state: Option<String>,
    /// Why the template cache declined, when it did.
    pub template_cache_bypass_reason: Option<String>,
```

Initialize all three to `None` in `from_parts` only — `from_auction_request` (`:130`) has no
struct literal; it delegates to `Self::from_parts(...)` at `:146`. Add
`set_origin_cache_shareable(&mut self, bool)`, `set_template_cache_state(&mut self, &str)`,
`set_template_cache_bypass_reason(&mut self, &str)`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test-fastly -- auction::telemetry::tests::observation_cache_fields --nocapture`
Expected: PASS.

- [ ] **Step 5: Confirm no other construction sites break**

Run: `cargo check-fastly`
Expected: clean. `publisher.rs:6597` and `:18127` are `from_parts` _calls_, not struct literals,
so this step is normally a no-op — it exists to catch a literal construction added since.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/auction/telemetry.rs
git commit -m "Carry cache outcomes on the auction observation context

Three absent-by-default fields and their setters. Nothing writes them yet."
```

---

## Task 3: Add the columns to the telemetry row

**Files:**

- Modify: `crates/trusted-server-core/src/auction/telemetry.rs` — struct `:277`, `base()` `:347`

`AuctionTerminalStatus` is declared at `telemetry.rs:49`; check the variant spelling there.
`push_summary` is at `:661`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn summary_row_carries_cache_outcomes_from_the_observation() {
    let mut observation = test_observation();
    observation.set_origin_cache_shareable(false);
    observation.set_template_cache_state("bypass-response");
    observation.set_template_cache_bypass_reason("origin response carries Set-Cookie");

    let mut rows = Vec::new();
    push_summary(
        &mut rows,
        &observation,
        "2026-09-15 00:00:00.000",
        AuctionTerminalStatus::Completed,
        None,
        12,
        1,
    );

    let row = rows.first().expect("should emit one summary row");
    assert_eq!(row.origin_cache_shareable, Some(0));
    assert_eq!(row.template_cache_state.as_deref(), Some("bypass-response"));
    assert_eq!(
        row.template_cache_bypass_reason.as_deref(),
        Some("origin response carries Set-Cookie")
    );
}

#[test]
fn rows_omit_cache_outcomes_when_the_observation_has_none() {
    let observation = test_observation();
    let mut rows = Vec::new();
    push_summary(
        &mut rows,
        &observation,
        "2026-09-15 00:00:00.000",
        AuctionTerminalStatus::Completed,
        None,
        12,
        1,
    );

    assert_eq!(
        rows.first().expect("should emit one row").origin_cache_shareable,
        None,
        "an unmeasured source must be distinguishable from a measured miss"
    );
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test-fastly -- auction::telemetry::tests::summary_row_carries_cache --nocapture`
Expected: FAIL — no field on `AuctionEventRow`.

- [ ] **Step 3: Add the fields and wire `base()`**

Add to `AuctionEventRow` after `ad_id`, using `u8` not `bool` to match the existing `is_mobile` /
`gdpr_applies` ClickHouse convention:

```rust
    /// `0` or `1`; absent when this source does not make the readthrough decision.
    pub origin_cache_shareable: Option<u8>,
    /// Terminal template-cache state.
    pub template_cache_state: Option<String>,
    /// Why the template cache declined, when it did.
    pub template_cache_bypass_reason: Option<String>,
```

In `base()`, after `ad_id: None,`:

```rust
            origin_cache_shareable: observation.origin_cache_shareable.map(u8::from),
            template_cache_state: observation.template_cache_state.clone(),
            template_cache_bypass_reason: observation.template_cache_bypass_reason.clone(),
```

Setting these in `base()` rather than only in `push_summary` means provider and bid rows carry
them too — three nullable columns, and no per-row joins in the dashboard.

**Flag this in the PR description as a deliberate deviation.** The spec scopes the fields to "the
auction telemetry summary row". Widening to `base()` is defensible but multiplies the emitted
payload across every row kind, so it should be a stated choice rather than a silent one.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test-fastly -- auction::telemetry::tests --nocapture`
Expected: PASS.

- [ ] **Step 5: Confirm the NDJSON shape**

Run: `cargo test-fastly -- auction::telemetry::tests --nocapture 2>&1 | tail -20`

`to_ndjson` (`:424`) uses plain `serde_json::to_string` with no `skip_serializing_if`, so the
new key is **always** on the wire including as `null`. That is what makes Task 4 mandatory
and ordered before deploy.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/auction/telemetry.rs
git commit -m "Emit cache outcomes on auction telemetry rows

Fields are always serialized, including as null, so the datasource must
declare them before this ships or rows land in quarantine."
```

---

## Task 4: Migrate the Tinybird datasource

**Files:**

- Modify: `tinybird/datasources/auction_events_raw.datasource`, `tinybird/fixtures/auction_events_raw.ndjson`

- [ ] **Step 1: Add the columns**

In `SCHEMA >`, after `ad_id` and **before** `event_date`:

```
  `origin_cache_shareable` Nullable(UInt8),
```

`LowCardinality` matches how `terminal_status` and `terminal_reason` are declared. The two new
string columns have 9 and 16 possible values respectively (`TemplateCacheResponseState` at `:94`,
`TemplateCacheBypassReason` at `:5673`), so dictionary encoding is right for both. Do not touch `ENGINE_SORTING_KEY` or the TTL.

- [ ] **Step 2: Update every fixture row**

```bash
python3 - <<'PY'
import json, pathlib
p = pathlib.Path("tinybird/fixtures/auction_events_raw.ndjson")
rows = [json.loads(l) for l in p.read_text().splitlines() if l.strip()]
for i, r in enumerate(rows):
    # Pre-existing gap: user_agent is declared in the datasource but absent from every row.
    r.setdefault("user_agent", None)
    r["origin_cache_shareable"] = None
    r["template_cache_state"] = None
    r["template_cache_bypass_reason"] = None
    if i == 0:
        r["origin_cache_shareable"] = 1
        r["template_cache_state"] = "hit"
p.write_text("\n".join(json.dumps(r) for r in rows) + "\n")
PY
```

- [ ] **Step 3: Verify the fixture matches the schema**

```bash
python3 - <<'PY'
import json, re, pathlib
schema = pathlib.Path("tinybird/datasources/auction_events_raw.datasource").read_text()
cols = [c for c in re.findall(r"`([a-z_]+)`", schema) if c != "event_date"]
rows = [json.loads(l) for l in pathlib.Path("tinybird/fixtures/auction_events_raw.ndjson").read_text().splitlines() if l.strip()]
for i, r in enumerate(rows):
    assert not set(cols) - set(r), f"row {i} missing {sorted(set(cols) - set(r))}"
    assert not set(r) - set(cols), f"row {i} undeclared {sorted(set(r) - set(cols))}"
print(f"ok: {len(rows)} rows match {len(cols)} declared columns")
PY
```

Expected: `ok: 8 rows match 34 declared columns`.

**The fixture has a pre-existing gap.** Before any change, the datasource declares 33 non-`event_date`
columns and each fixture row has 32 keys: `user_agent` is declared and absent from every row. The
verifier above will trip on row 0 until that is fixed. Add `"user_agent": null` to every row in the
Step 2 script (it is a legitimate nullable column), and note in the commit that it was missing
beforehand — do not let the implementer chase it as damage from this change.

- [ ] **Step 4: Cross-check the Rust struct against the columns**

Read the `AuctionEventRow` field list (`telemetry.rs:277`) and confirm every field name appears
in the datasource column list. Do this by eye against the struct — a `grep -c "pub "` over the
file counts fields across every struct in it and is not a usable check. A mismatch is the
quarantine bug and is silent at runtime.

- [ ] **Step 5: Commit**

```bash
git add tinybird/datasources/auction_events_raw.datasource tinybird/fixtures/auction_events_raw.ndjson
git commit -m "Declare cache outcome columns on the auction events datasource

Must reach Tinybird before the emitting code deploys; rows with undeclared
columns are quarantined rather than rejected loudly."
```

---

## Task 5: Build a test harness with both a template cache and a telemetry sink

Tasks 6–9 all need one. Neither existing builder provides it: `services()` (`:9247`) sets
`.template_cache(...)` and no sink; `services_with_telemetry()` (`:13719`) sets the sink and no
cache. This task is why those tasks are not blocked on scaffolding invented mid-task.

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs` — the `template_cache_end_to_end_tests` module (8984–12336)

- [ ] **Step 1: Add the combined builder**

In `template_cache_end_to_end_tests`, alongside the existing `services()`:

```rust
        fn services_with_cache_and_telemetry(
            http_client: Arc<StubHttpClient>,
            cache: Arc<MemoryTemplateCache>,
            telemetry_sink: Arc<RecordingTelemetrySink>,
        ) -> RuntimeServices {
            let telemetry_sink: Arc<dyn AuctionTelemetrySink> = telemetry_sink;
            RuntimeServices::builder()
                .config_store(Arc::new(NoopConfigStore))
                .secret_store(Arc::new(NoopSecretStore))
                .kv_store(Arc::new(edgezero_core::key_value_store::NoopKvStore))
                .backend(Arc::new(StubBackend))
                .http_client(http_client)
                .geo(Arc::new(NoopGeo))
                .client_info(ClientInfo::default())
                .template_cache(cache)
                .auction_telemetry_sink(telemetry_sink)
                .build()
        }
```

There are three copies of `RecordingTelemetrySink` — `:13657` (`ssat_cache_policy_tests`),
`:18073` (directly in `mod tests`), `:21402` (`navigation_publisher_domain_tests`). **Do not move
anything.** The `:18073` copy is already in the shared parent module and is reachable from
`template_cache_end_to_end_tests` through its `use super::*` (`:8989`). You need only add
`use crate::auction::telemetry::AuctionTelemetrySink;` for the `Arc<dyn AuctionTelemetrySink>`
coercion — `mod tests` uses the fully-qualified path at `:18078` and does not import the trait.

The builder method for the sink is exactly `.auction_telemetry_sink(...)`, confirmed against
`services_with_telemetry` (`:13719`).

- [ ] **Step 2: Add a summary-row accessor**

`RecordingTelemetrySink` has **no accessor** — it is
`#[derive(Default)] struct RecordingTelemetrySink { batches: Mutex<Vec<AuctionEventBatch>> }`
(`:18073`) and the trait impl reads the field directly. Read the field:

```rust
        fn last_summary_row(sink: &RecordingTelemetrySink) -> Option<AuctionEventRow> {
            sink.batches
                .lock()
                .expect("should lock recorded telemetry batches")
                .iter()
                .flat_map(AuctionEventBatch::rows)
                .filter(|row| row.event_kind == "summary")
                .next_back()
                .cloned()
        }
```

`AuctionEventBatch::rows()` returns `&[AuctionEventRow]` (`telemetry.rs:401`), so the
`flat_map` typechecks and `next_back()` is available on both slice-iterator layers.

- [ ] **Step 3: Add settings that emit a summary row**

`run()` takes `&Arc<Settings>` (`:9471`), not `&Settings`. Both settings helpers below must
return `Arc<Settings>` — existing tests wrap at the call site (`:9601`); returning the `Arc` from
the helper is cleaner and keeps every test body in this plan correct as written.

A summary row is emitted only when an auction runs, so the settings need `[auction] enabled =
true` **and** matching creative-opportunity slots. `ssat_cache_policy_tests` has
`settings_with_enabled_auction_and_creative_opportunities` at `:13675`; adapt it into this module
rather than hand-rolling a second one, and have it return `Arc<Settings>`.

Add a cookie-bearing request builder alongside the existing `navigation_request()` (`:9285`):

```rust
        fn navigation_request_with_cookie(cookie: &str) -> Request<EdgeBody> {
            let mut req = navigation_request();
            req.headers_mut().insert(
                header::COOKIE,
                HeaderValue::from_str(cookie).expect("should build a cookie header"),
            );
            req
        }
```

- [ ] **Step 4: Prove the harness works before relying on it**

```rust
        #[tokio::test]
        async fn harness_emits_a_summary_row_for_an_ad_serving_navigation() {
            let sink = Arc::new(RecordingTelemetrySink::default());
            let services = services_with_cache_and_telemetry(
                Arc::new(StubHttpClient::new()),
                Arc::new(MemoryTemplateCache::default()),
                Arc::clone(&sink),
            );
            let settings = settings_with_auction_and_slots();

            let _ = run(&settings, &services, navigation_request()).await;

            assert!(
                last_summary_row(&sink).is_some(),
                "the harness must emit a summary row, or every later assertion is vacuous"
            );
        }
```

Run: `cargo test-fastly -- template_cache_end_to_end_tests::harness_emits_a_summary_row --nocapture`
Expected: PASS. If it fails, fix the harness here — do not carry a broken harness into Task 6,
where the failure will look like a wiring bug.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Add a publisher test harness with both a template cache and a telemetry sink

Neither existing builder wires both, so cache-outcome telemetry had no way to
be asserted end to end."
```

---

## Task 6: Record whether the readthrough gate admitted the request

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs:4461-4468`

**Know which binding you are holding.** There are two. `observation` is a plain
`AuctionObservationContext` value built at `:4461`; `auction_observation` is the
`Option<AuctionObservationContext>` declared at `:4446`, and `observation` is moved into it at
`:4511`. So this task sets the field on the **value**, before the move, while Tasks 8 and 9 reach
the **`Option`** with `as_mut()` because they run after it. The two are not in conflict.

That also means the spec is wrong on this point. Its Observability/Carrier section says the value
"must be stashed in a local and threaded to the construction site, not written through a setter on
a binding that does not exist yet". The binding does exist: `origin_response_is_shareable` is known
at `:4325`, construction is at `:4461`, and a setter immediately after it works. Amend the spec
rather than following it here.

- [ ] **Step 1: Write the failing test**

```rust
        #[tokio::test]
        async fn navigation_records_whether_the_origin_response_was_shareable() {
            let sink = Arc::new(RecordingTelemetrySink::default());
            let services = services_with_cache_and_telemetry(
                Arc::new(StubHttpClient::new()),
                Arc::new(MemoryTemplateCache::default()),
                Arc::clone(&sink),
            );
            let settings = settings_with_auction_and_slots();

            let _ = run(&settings, &services, navigation_request_with_cookie("ts-ec=abc")).await;

            assert_eq!(
                last_summary_row(&sink)
                    .expect("should emit a summary row")
                    .origin_cache_shareable,
                Some(0),
                "a cookie-bearing request must record as not shareable"
            );
        }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test-fastly -- template_cache_end_to_end_tests::navigation_records_whether --nocapture`
Expected: FAIL — `origin_cache_shareable` is `None`.

- [ ] **Step 3: Set the field**

Change the binding at `:4461` to `let mut observation = …` and add immediately after it:

```rust
        observation.set_origin_cache_shareable(origin_response_is_shareable);
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test-fastly -- template_cache_end_to_end_tests::navigation_records_whether --nocapture`
Expected: PASS.

- [ ] **Step 5: Run the full module**

Run: `cargo test-fastly`
Expected: PASS. Run the whole module — Viceroy aborts on first panic, so a single-test run hides
later failures.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Record origin shareability on the auction observation

Makes the readthrough gate's effect measurable before the gate ships."
```

---

## Task 7: Derive a structured request-side bypass reason — MOVED OUT OF SCOPE

**Removed from this PR.** Not in issue #852; the bypass reason diagnoses the template cache, which is #1009 feature work. Moved to successor issue B, which promotes that cache out of spike status and should instrument it as part of that.

## Task 8: Record the bypass reason from both sources — MOVED OUT OF SCOPE

**Removed from this PR.** Not in issue #852; the bypass reason diagnoses the template cache, which is #1009 feature work. Moved to successor issue B, which promotes that cache out of spike status and should instrument it as part of that.

## Task 9: Record the terminal template-cache state — NOT DONE, and cannot be

**Attempted and reverted.** The store outcome cannot reach the summary row. On a cold fill
`stream_publisher_body_async` collects the auction, takes the observation and emits the batch,
and only afterwards does `store_template_if_authorized` run and the state get stamped. The store
cannot move earlier (it needs the transform) and the emit cannot move later without giving up
collecting during body streaming, which is a latency decision on the path this issue exists to
improve.

`hit` was reachable and `miss-stored` was not, so the column would have reported hits without
misses and made hit rate compute as roughly 100%. A silently wrong metric is worse than an absent
one, so `template_cache_state` was dropped from the observation, the row, the datasource and the
fixture — the spec's own trim, reached by the code rather than by the approval gate.

`template_cache_bypass_reason` and `origin_cache_shareable` carry the triage. The
`x-ts-template-cache` header still reports all nine states per response for debugging one request.

---

## Task 11: Write the dashboard caveats

The spec requires these be stated where a dashboard author will read them, and the plan's own
doc rule is that each item's docs land in that item's PR rather than as a lump. Neither caveat is
discoverable from the Rust doc comments.

**Files:**

- Modify: `docs/guide/` — wherever auction telemetry / Tinybird consumers are documented. If no
  such page exists, add the caveats next to the datasource in `tinybird/` as a README rather than
  inventing a new docs page.

- [ ] **Step 1: Write both gaps**
  1. **The denominator is ad-serving pageviews, not all requests.** A summary row is emitted only
     when an auction runs, so a request that bypasses the template cache _because_ the ad stack did
     not run — bot, prefetch, kill-switched, consent-denied — produces no row at all.
  2. **`None` is not a miss.** `AuctionObservationContext` is `Clone` and shared with the
     `/auction` source, where the column is structurally `None`. A dashboard that reads
     `None` as "miss" will be wrong for that whole source class. Filter on
     `auction_source = 'initial_navigation'` before computing any rate.

- [ ] **Step 2: Format**

Run: `cd docs && ./node_modules/.bin/prettier --check <the file you edited>`

- [ ] **Step 3: Commit**

```bash
git add <the file you edited>
git commit -m "Document the two caveats on cache-outcome telemetry

The denominator is ad-serving pageviews, and a null is an unmeasured source
rather than a cache miss. Both are silent misreadings otherwise."
```

---

## Task 10: Correct the documented CI gate list — MOVED OUT OF SCOPE

**Removed from this PR.** Not in issue #852 — a correct drive-by docs fix, but scope creep here. Worth landing as its own small PR.

## Final verification

- [ ] **Full gate set**

```bash
cargo fmt --all -- --check
cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare && cargo clippy-cloudflare-wasm && cargo clippy-spin-native && cargo clippy-spin-wasm
cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
./scripts/test-cli.sh
cd docs && npm run format && cd ..
```

- [ ] **Confirm the change is confined to the expected files**

```bash
git diff main --stat
```

Compare against the branch point rather than `main` if later parts have already landed on the
branch. Expected for this part: only `publisher.rs`, `auction/telemetry.rs`, the two Tinybird
files, and the docs touched by Task 11. `adapter-fastly/src/tinybird.rs` also appears: that is a
`mod tests` row literal that must gain the new field, not the telemetry struct leaking into
adapter code. Any _other_ adapter file appearing would be the leak this check is looking for.

This check confirms _which files changed_, nothing more. Behavior neutrality of the predicate split
rests on Task 1's `every_shared_input_is_necessary_for_shareability` test and on Task 1 Step 5 —
any template-cache test changing outcome means the refactor was not neutral.

- [ ] **Apply the Tinybird migration before deploying**

This is a deploy-ordering constraint, not a commit-ordering one — the PR merges atomically.
Owner: whoever runs the deploy. Apply the datasource change to Tinybird first, then deploy the
code, then confirm with a staging request that a summary row carries the new column and
that `tinybird/pipes/quarantine_counts.pipe` shows no new quarantined rows. Record that
confirmation on the PR; the spec's close-out criteria require it.
