# Predicate split and cache observability implementation plan

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

**This is PR 1 of 5.** It ships no behavior change. The readthrough gate that consumes
`origin_response_is_shareable` is PR 5.

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
| `crates/trusted-server-core/src/publisher.rs`         | Publisher path; both predicates, the observation, the cache-state local | Predicate functions near `:4325`; observation `:4461`; request-side reason `:4370-4386`; response-side reason `:4776`; state hook `:4825` |
| `crates/trusted-server-core/src/auction/telemetry.rs` | Observation context, row schema, NDJSON                                 | 3 fields on `AuctionObservationContext` (`:99`) and `AuctionEventRow` (`:277`); wire `base()` (`:347`)                                    |
| `tinybird/datasources/auction_events_raw.datasource`  | ClickHouse columns                                                      | Add 3 nullable columns                                                                                                                    |
| `tinybird/fixtures/auction_events_raw.ndjson`         | Fixture rows                                                            | Add 3 keys to all 8 rows                                                                                                                  |
| `AGENTS.md`                                           | CI gate list                                                            | Correct it                                                                                                                                |

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

Run: `cargo test-fastly -p trusted-server-core`
Expected: PASS, no newly failing tests. Any template-cache test changing outcome means the
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

Build the context with `AuctionObservationContext::from_parts(...)` inline if no
`test_observation()` helper exists in this module.

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

Initialize all three to `None` in both `from_parts` and `from_auction_request`, and add
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

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test-fastly -- auction::telemetry::tests --nocapture`
Expected: PASS.

- [ ] **Step 5: Confirm the NDJSON shape**

Run: `cargo test-fastly -- auction::telemetry::tests --nocapture 2>&1 | tail -20`

`to_ndjson` (`:424`) uses plain `serde_json::to_string` with no `skip_serializing_if`, so the
three keys are **always** on the wire including as `null`. That is what makes Task 4 mandatory
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
  `template_cache_state` LowCardinality(Nullable(String)),
  `template_cache_bypass_reason` LowCardinality(Nullable(String)),
```

`LowCardinality` matches how `terminal_status` and `terminal_reason` are declared — 9 and 16
possible values respectively. Do not touch `ENGINE_SORTING_KEY` or the TTL.

- [ ] **Step 2: Update every fixture row**

```bash
python3 - <<'PY'
import json, pathlib
p = pathlib.Path("tinybird/fixtures/auction_events_raw.ndjson")
rows = [json.loads(l) for l in p.read_text().splitlines() if l.strip()]
for i, r in enumerate(rows):
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

Expected: `ok: 8 rows match 38 declared columns`.

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

`RecordingTelemetrySink` lives at `:13657` in `ssat_cache_policy_tests`. Move it to a shared
parent-module location rather than duplicating it, and update the original use sites. Check the
exact builder method name for the sink against `services_with_telemetry` (`:13719`).

- [ ] **Step 2: Add a summary-row accessor**

```rust
        fn last_summary_row(sink: &RecordingTelemetrySink) -> Option<AuctionEventRow> {
            sink.batches()
                .iter()
                .flat_map(AuctionEventBatch::rows)
                .filter(|row| row.event_kind == "summary")
                .next_back()
                .cloned()
        }
```

`AuctionEventBatch::rows()` is at `telemetry.rs:401`. Match `RecordingTelemetrySink`'s real
accessor name for recorded batches.

- [ ] **Step 3: Add settings that emit a summary row**

A summary row is emitted only when an auction runs, so the settings need `[auction] enabled =
true` **and** matching creative-opportunity slots. `ssat_cache_policy_tests` has a
`settings_with_enabled_auction_and_creative_opportunities`-shaped helper (see the TOML built
around `:13710`); adapt it into this module rather than hand-rolling a second one.

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
                Arc::new(StubHttpClient::default()),
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

Run: `cargo test-fastly -- publisher::tests::harness_emits_a_summary_row --nocapture`
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

The observation is constructed at `:4461`, after `origin_response_is_shareable` at `:4325`, so a
setter right after construction suffices — no stash variable.

- [ ] **Step 1: Write the failing test**

```rust
        #[tokio::test]
        async fn navigation_records_whether_the_origin_response_was_shareable() {
            let sink = Arc::new(RecordingTelemetrySink::default());
            let services = services_with_cache_and_telemetry(
                Arc::new(StubHttpClient::default()),
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

Run: `cargo test-fastly -- publisher::tests::navigation_records_whether --nocapture`
Expected: FAIL — `origin_cache_shareable` is `None`.

- [ ] **Step 3: Set the field**

Change the binding at `:4461` to `let mut observation = …` and add immediately after it:

```rust
        observation.set_origin_cache_shareable(origin_response_is_shareable);
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test-fastly -- publisher::tests::navigation_records_whether --nocapture`
Expected: PASS.

- [ ] **Step 5: Run the full module**

Run: `cargo test-fastly -p trusted-server-core`
Expected: PASS. Run the whole module — Viceroy aborts on first panic, so a single-test run hides
later failures.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Record origin shareability on the auction observation

Makes the readthrough gate's effect measurable before the gate ships."
```

---

## Task 7: Derive a structured request-side bypass reason

New code, not wiring. The request-side bypass currently produces only
`TemplateCacheResponseState::BypassRequest` (`:4386`) and free-text logs (`:4360-4369`), so the
single most important triage value — cookie-disqualified — has nowhere to come from.

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs:4355-4390`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn request_side_bypass_reason_names_the_first_failing_condition() {
    let esi = AssemblyMode::Esi;

    assert_eq!(
        request_side_bypass_reason(
            esi,
            SharedRequestInputs { cookie_disqualifies: true, ..all_shareable() },
            true,
        ),
        Some(TemplateCacheBypassReason::CookieForwarded),
    );
    assert_eq!(
        request_side_bypass_reason(
            esi,
            SharedRequestInputs { authorization_disqualifies: true, ..all_shareable() },
            true,
        ),
        Some(TemplateCacheBypassReason::AuthorizedRequest),
    );
    assert_eq!(
        request_side_bypass_reason(AssemblyMode::Inline, all_shareable(), true),
        Some(TemplateCacheBypassReason::InlineMode),
    );
    assert_eq!(
        request_side_bypass_reason(esi, all_shareable(), true),
        None,
        "an eligible request has no bypass reason"
    );
}
```

Add an `all_shareable()` helper returning a `SharedRequestInputs` with every condition passing.
`TemplateCacheBypassReason` already derives `PartialEq` (`:5672`), so no change is needed there.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test-fastly -- publisher::tests::request_side_bypass_reason --nocapture`
Expected: FAIL — function not found.

- [ ] **Step 3: Implement it**

Next to the predicate functions from Task 1:

```rust
/// The reason a request was refused a template-cache key, before the origin is contacted.
///
/// `template_cache_ttl` cannot produce these: it runs only for requests that already got a key,
/// so its `InlineMode`, `AuthorizedRequest` and `CookieForwarded` variants are unreachable
/// there. Ordering matches that function's so one request cannot be described two ways.
pub(crate) fn request_side_bypass_reason(
    assembly_mode: AssemblyMode,
    inputs: SharedRequestInputs,
    reader_supports_assembly: bool,
) -> Option<TemplateCacheBypassReason> {
    if matches!(assembly_mode, AssemblyMode::Inline) {
        return Some(TemplateCacheBypassReason::InlineMode);
    }
    if inputs.authorization_disqualifies {
        return Some(TemplateCacheBypassReason::AuthorizedRequest);
    }
    if inputs.cookie_disqualifies {
        return Some(TemplateCacheBypassReason::CookieForwarded);
    }
    if !inputs.method_is_cacheable || !inputs.host_present || inputs.request_requires_origin
        || !reader_supports_assembly
    {
        return Some(TemplateCacheBypassReason::NotShareableRequest);
    }
    None
}
```

`NotShareableRequest` does not exist yet — add it to `TemplateCacheBypassReason` (`:5673`) with
a `#[display("request is not eligible for a shared template")]`. The four conditions it covers
already have distinct `log::debug!` lines and none is a leak vector, so one variant is enough;
do not add four.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test-fastly -- publisher::tests::request_side_bypass_reason --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Derive a structured reason for the request-side template-cache bypass

template_cache_ttl runs only for requests that already earned a key, so its
InlineMode, AuthorizedRequest and CookieForwarded variants can never fire.
Cookie-disqualified is the expected default in production and had no value to
report."
```

---

## Task 8: Record the bypass reason from both sources

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs` — after `:4386`, and the `Err` arm at `:4776`

- [ ] **Step 1: Write the failing test**

```rust
        #[tokio::test]
        async fn cookie_bearing_navigation_records_the_request_side_bypass_reason() {
            let sink = Arc::new(RecordingTelemetrySink::default());
            let services = services_with_cache_and_telemetry(
                Arc::new(StubHttpClient::default()),
                Arc::new(MemoryTemplateCache::default()),
                Arc::clone(&sink),
            );
            let settings = settings_with_auction_and_slots();

            let _ = run(&settings, &services, navigation_request_with_cookie("ts-ec=abc")).await;

            assert_eq!(
                last_summary_row(&sink)
                    .expect("should emit a summary row")
                    .template_cache_bypass_reason
                    .as_deref(),
                Some("request carried Cookie and the origin's Vary does not cover it"),
                "cookie-disqualified is the expected production default and must be reportable"
            );
        }
```

The expected string is `TemplateCacheBypassReason::CookieForwarded`'s `Display` (`:5721`).
Copy it exactly.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test-fastly -- publisher::tests::cookie_bearing_navigation_records --nocapture`
Expected: FAIL — reason is `None`.

- [ ] **Step 3: Compute and stash the request-side reason**

The observation does not exist yet at `:4386`, so stash the reason in a local and apply it at
the construction site. After the `template_cache_response_state` binding (`:4386`):

```rust
    let request_side_bypass_reason = request_side_bypass_reason(
        assembly_mode,
        shared_request_inputs,
        reader_supports_assembly,
    );
```

Then in Task 6's block after `:4461`:

```rust
        if let Some(reason) = request_side_bypass_reason {
            observation.set_template_cache_bypass_reason(&reason.to_string());
        }
```

- [ ] **Step 4: Add the response-side write**

In the `Err(reason)` arm at `:4776`, before the existing `log::debug!`:

```rust
            Err(reason) => {
                if let Some(observation) = auction_observation.as_mut() {
                    observation.set_template_cache_bypass_reason(&reason.to_string());
                }
                log::debug!("template_cache bypass: {reason}");
                None
            }
```

Guard on `as_mut()`: a non-ad-stack request has no auction and no observation, which is a
legitimate `None`. The response-side write overwrites the request-side one, which is correct —
a request that got a key had no request-side reason to begin with.

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test-fastly -p trusted-server-core`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Record the template-cache bypass reason on the auction observation

Sixteen bypass variants share one outcome today. Without the reason, a zero
hit rate cannot be told apart from an origin misconfiguration."
```

---

## Task 9: Record the terminal template-cache state

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs` around `:4825`

There are three `set_template_cache_response_state` call sites — `:1795`, `:2202`, `:4825` — and
only `:4825` is inside `handle_publisher_request`. The other two are in the finalizer and
assembly paths, where the observation has already been moved into `params`. The reachable hook
is the `template_cache_response_state` local that accumulates from `:4386` to `:4825`.

Because `auction_observation.take()` fires at `:4669`, `:4726`, `:4748`, `:4957` and `:4996` —
all before `:4825` — the state must be written to the observation **before** whichever take applies, or the row
carries `None`. Handle it by writing at `:4825` for the paths that reach it, and at the Hit arm
(`:4617`) for the path that returns early.

- [ ] **Step 1: Write the failing test**

```rust
        #[tokio::test]
        async fn template_cache_hit_records_its_state() {
            let sink = Arc::new(RecordingTelemetrySink::default());
            let cache = Arc::new(MemoryTemplateCache::default());
            let services = services_with_cache_and_telemetry(
                Arc::new(cacheable_html_client()),
                Arc::clone(&cache),
                Arc::clone(&sink),
            );
            let settings = esi_settings_with_auction_and_slots();

            let _cold = run(&settings, &services, navigation_request()).await;
            let _warm = run(&settings, &services, navigation_request()).await;

            assert_eq!(
                last_summary_row(&sink)
                    .expect("should emit a summary row for the warm request")
                    .template_cache_state
                    .as_deref(),
                Some("hit"),
                "the telemetry state must match the x-ts-template-cache header"
            );
        }
```

Model the cold/warm setup and the cacheable-origin stub on the existing test at `:9612`, which
already drives a cold fill then a warm hit and asserts on the header.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test-fastly -- publisher::tests::template_cache_hit_records_its_state --nocapture`
Expected: FAIL — state is `None`.

- [ ] **Step 3: Write at the reachable sites**

At `:4825`, extend the existing block:

```rust
    if let Some(state) = template_cache_response_state {
        set_template_cache_response_state(&mut response, state);
        if let Some(observation) = auction_observation.as_mut() {
            observation.set_template_cache_state(state.as_str());
        }
    }
```

And in the `TemplateCacheLookup::Hit` arm (`:4617`), before the observation is moved out at
`:4669`:

```rust
                if let Some(observation) = auction_observation.as_mut() {
                    observation.set_template_cache_state(TemplateCacheResponseState::Hit.as_str());
                }
```

`TemplateCacheResponseState::as_str` is at `:107` and is in the same module, so no visibility
change is needed.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test-fastly -- publisher::tests::template_cache_hit_records_its_state --nocapture`
Expected: PASS.

- [ ] **Step 5: Document the known-None paths**

Add a comment above the `:4825` block recording that the abandon paths at `:4726`, `:4748`,
`:4957` and `:4996` take the observation before this point, so their rows legitimately carry
`template_cache_state: None`. Without the note a future reader will read it as a bug.

Run: `cargo test-fastly && cargo clippy-fastly`
Expected: PASS, no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Record the terminal template-cache state on the auction observation

Written beside the response-header stamp so the header and the telemetry
cannot drift. Abandon paths take the observation earlier and legitimately
report no state."
```

---

## Task 10: Correct the documented CI gate list

**Files:**

- Modify: `AGENTS.md`, "CI Gates" section

- [ ] **Step 1: Read the real gates**

```bash
grep -n "cargo \|npm run \|scripts/" .github/workflows/test.yml .github/workflows/format.yml .github/workflows/integration-tests.yml
```

- [ ] **Step 2: Rewrite the section**

State that the list is the commonly-run subset and `.github/workflows/` is authoritative, then
list what CI actually runs — including the omissions: `scripts/template-cache-local-test.sh`,
the CLI and openrtb-codegen clippy invocations in `format.yml`, the parity crate clippy, the
openrtb-codegen test, the integration-tests `cargo fmt` check, `npm run lint` for JS and docs,
the docs `npm run build`, the html-processor bench smoke, the Fastly and Spin release WASM
builds, and `.github/workflows/integration-tests.yml`.

- [ ] **Step 3: Format**

Run: `cd docs && ./node_modules/.bin/prettier --check ../AGENTS.md`

Use the pinned `docs/node_modules` prettier, not `npx` — the npx version reports false failures
in this repo.

- [ ] **Step 4: Commit**

```bash
git add AGENTS.md
git commit -m "Correct the documented CI gate list

The list omitted ten gates CI runs, including an entire workflow. Points at
.github/workflows as authoritative rather than restating it."
```

---

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

- [ ] **Confirm no behavior change**

```bash
git diff main --stat
```

Expected: only `publisher.rs`, `auction/telemetry.rs`, the two Tinybird files, and `AGENTS.md`.
An adapter file appearing means the telemetry struct is leaking into adapter code.

- [ ] **Apply the Tinybird migration before deploying**

This is a deploy-ordering constraint, not a commit-ordering one — the PR merges atomically.
Owner: whoever runs the deploy. Apply the datasource change to Tinybird first, then deploy the
code, then confirm with a staging request that a summary row carries the three new columns and
that `tinybird/pipes/quarantine_counts.pipe` shows no new quarantined rows. Record that
confirmation on the PR; the spec's close-out criteria require it.
