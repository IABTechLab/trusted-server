# Ad-template Generation Progress Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show immediate, safe, profile-aware progress while `ts audit ad-templates generate` performs a long browser crawl.

**Architecture:** Add typed progress events to the `AuditCollector` boundary so the browser can report work before buffered page results are returned. Render and flush those events from `run_update_slots` on stderr, using only URL paths. Preserve crawl/progress errors over teardown errors while always closing and waiting for Chrome.

**Tech Stack:** Rust 2024, `std::io::Write`, existing `url`, `tokio`, `chromiumoxide`, and CLI test helpers; no new dependency.

---

### Task 1: Define and render safe progress events

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/generate/collector.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/mod.rs`

- [ ] **Step 1: Write failing renderer and writer tests**

Add tests in `generate/mod.rs` for events covering launch, `1/?`, `2/17`, and finalization. Assert that `https://user:pass@publisher.example/news?token=secret#fragment` renders only `/news`, terminal control bytes are escaped, stdout remains untouched, and a counting writer records an explicit `flush()`. Add writers that fail independently on `write()` and `flush()` and assert a CLI output error.

- [ ] **Step 2: Run the focused tests and confirm RED**

Run:

```bash
cargo test --package trusted-server-cli --target aarch64-apple-darwin progress -- --nocapture
```

Expected: FAIL because the progress event and renderer do not exist.

- [ ] **Step 3: Add the progress model and renderer**

In `collector.rs`, define a small event enum and callback type:

```rust
pub(crate) enum CollectionProgress<'a> {
    Launching,
    Loading {
        current: usize,
        total: Option<usize>,
        url: &'a Url,
    },
    Planning,
    Finalizing,
}

pub(crate) type ProgressSink<'a> =
    &'a mut dyn FnMut(CollectionProgress<'_>) -> CliResult<()>;
```

Add concise doc comments to the enum, every variant, and the callback alias. The
callback documentation must state that returning an error stops new collection
work but does not bypass an already-launched browser's finalization/close/wait.

In `generate/mod.rs`, add a `write_collection_progress` helper that accepts a profile label, formats only `url.path()` (or `/` when empty), sanitizes it with `escape_terminal_text`, writes one line to stderr, and immediately calls `flush()`. Render and test `Planning` between the root load and subsequent page loads.

- [ ] **Step 4: Run the focused tests and confirm GREEN**

Run the command from Step 2. Expected: all progress renderer/writer tests pass.

### Task 2: Propagate progress through collectors with teardown-safe failures

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/generate/collector.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/browser_collector.rs`
- Modify: `scripts/test-cli.sh`

- [ ] **Step 1: Write failing collector tests**

Test default `collect_pages` and `collect_site` count semantics, including an attempted page whose collection fails. The exact dynamic-site sequence is root `1/?`, planning, then follow-ups `2/total` through `total/total`; totals include the root and failed attempts advance the count. Add a Chrome-backed test whose progress callback fails during collection. It must return the progress error only after the browser teardown path completes. Extend the existing result-combination unit tests to cover first-error preservation across a collection/planning error, a later finalization-progress error, close error, and wait error, while proving finalization, close, and wait were all attempted.

- [ ] **Step 2: Run the focused tests and confirm RED**

Run:

```bash
cargo test --package trusted-server-cli --target aarch64-apple-darwin commands::audit::generate::browser_collector::tests -- --nocapture
cargo test --package trusted-server-cli --target aarch64-apple-darwin commands::audit::generate::collector::tests -- --nocapture
```

Expected: FAIL because collectors do not accept or emit progress callbacks.

- [ ] **Step 3: Add callbacks to the collector boundary**

Extend `collect_pages` and `collect_site` with `ProgressSink`. Default collectors emit `Loading` before each page. The root of a dynamically planned site emits `current: 1, total: None`, followed by `Planning`; after planning, default `collect_site` iterates follow-ups itself with an explicit offset so they report `2/total` onward. Fixed batches emit totals including the root, and failed attempts still consume their position.

Pass the callback into `with_browser`. Adapt `BrowserAuditCollector::collect_page` with an explicit no-op progress sink because single-page artifact generation has no command progress writer. Emit `Launching` before browser launch, `Loading` immediately before each navigation, `Planning` immediately before invoking the root planner, and `Finalizing` before close/wait. Track only the first crawl/progress error: on callback failure, stop scheduling pages, still attempt finalization, `browser.close()`, and `browser.wait()`, then return that first error ahead of teardown errors.

Extend `scripts/test-cli.sh` with a second ignored-test filter for
`commands::audit::generate::browser_collector::tests::` so the new Chrome-backed
progress-failure test is actually executed under `TS_AUDIT_BROWSER_TESTS=1` and
single-threaded, alongside the existing three browser audit fixtures.

- [ ] **Step 4: Run unit and Chrome-backed tests and confirm GREEN**

Run the focused command, then:

```bash
./scripts/test-cli.sh
```

Expected: collector unit tests and all four Chrome-backed tests pass.

### Task 3: Wire profile-aware progress into generation

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/generate/mod.rs`

- [ ] **Step 1: Write failing generation tests**

Update `run_update_slots` tests to assert the stderr buffer contains progress for the first profile's `1/?` root, planning, later known totals, the second profile's `1/total` root, and finalization. Assert dry-run diff/success output on stdout contains no progress lines. Add an ordering test with a shared observable writer and fake collector: from inside `collect_site`, after invoking and flushing the progress callback but before returning, assert the progress bytes are already visible.

- [ ] **Step 2: Run the focused tests and confirm RED**

Run:

```bash
cargo test --package trusted-server-cli --target aarch64-apple-darwin update_slots -- --nocapture
```

Expected: FAIL because `run_update_slots` does not provide progress callbacks.

- [ ] **Step 3: Connect callbacks and profiles**

Create a progress closure for the first profile and pass it to `collect_site`. Pass `err` through `crawl_sections`, create a closure for each later profile, and pass it to `collect_pages`. Keep notes and final summary behavior unchanged.

- [ ] **Step 4: Run the focused tests and confirm GREEN**

Run the command from Step 2. Expected: all generation tests pass and progress appears only in stderr.

### Task 4: Verify and ship

**Files:**

- Verify all modified files plus the two design documents.

- [ ] **Step 1: Format and lint**

```bash
cargo fmt --all -- --check
cargo clippy --package trusted-server-cli --target aarch64-apple-darwin --all-targets --all-features -- -D warnings
git diff --check
cd docs && npm run format
```

Expected: all commands exit 0.

- [ ] **Step 2: Run the complete local CLI suite**

```bash
./scripts/test-cli.sh
```

Expected: unit, config, proxy, documentation, and Chrome-backed tests pass.

- [ ] **Step 3: Review the scoped diff**

Confirm no cookie values, real publisher data, or changes to the pre-existing `fastly.toml` modification are included. Request an independent code review and address concrete findings.

- [ ] **Step 4: Commit and push**

Stage only the progress implementation and its design/plan documents. Commit with `Show ad-template generation progress`, push `feature/ts-cli-ad-templates`, and confirm local HEAD matches the remote branch.
