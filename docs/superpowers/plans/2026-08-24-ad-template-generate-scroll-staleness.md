# Ad-template Generate Scroll and Staleness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add opt-in scrolling to `ts audit ad-templates generate` and warn when a normal merge preserves configured slots that the current crawl did not observe.

**Architecture:** Thread one parsed `--scroll` value through the generation browser session and reuse a shared deterministic scroll primitive before the generator's final evidence scrape. Extend merge reconciliation with structured diagnostics that record unmatched pre-existing slot IDs; format the warning at the command layer so it can account for whether scrolling was already enabled without changing merge behavior.

**Tech Stack:** Rust 2024, clap, chromiumoxide/CDP, Tokio, existing CLI and Chrome-fixture test harnesses, rustfmt, clippy, Prettier.

---

## File map

- Create `crates/trusted-server-cli/src/commands/audit/browser_scroll.rs`: shared deterministic scroll primitive.
- Modify `crates/trusted-server-cli/src/commands/audit/mod.rs`: declare the shared module, parse `--scroll`, and wire it into generation.
- Modify `crates/trusted-server-cli/src/commands/audit/browser.rs`: reuse shared scrolling while retaining verifier-only phase marking.
- Modify `crates/trusted-server-cli/src/commands/audit/generate/browser_collector.rs`: carry scroll state, scroll and re-settle, and test lazy GPT discovery.
- Modify `crates/trusted-server-cli/src/commands/audit/generate/mod.rs`: carry scroll context and render contextual stale-slot notes.
- Modify `crates/trusted-server-cli/src/commands/audit/generate/slot_toml.rs`: report unmatched preserved slots from the authoritative merge matcher.
- Modify `crates/trusted-server-cli/src/run.rs`: test parsing and defaults.
- Modify `scripts/test-cli.sh`: run the new ignored Chrome fixture.
- Modify `docs/guide/cli.md`: document both behaviors.

### Task 1: Parse and wire generation scrolling

**Files:**
- Modify: `crates/trusted-server-cli/src/run.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/mod.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/browser_collector.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/mod.rs`

- [ ] **Step 1: Write failing parsing tests**

Extend `audit_generate_subcommands_use_generation_settle_defaults` with
`assert!(!generate.scroll)`. Add:

```rust
#[test]
fn audit_ad_templates_generate_parses_scroll() {
    let args = parse(&[
        "ts", "audit", "ad-templates", "generate",
        "https://www.example.com/", "--scroll",
    ]);
    let Command::Audit(audit) = args.command else {
        panic!("expected audit command");
    };
    let Some(crate::commands::audit::AuditSubcommand::AdTemplates(
        crate::commands::audit::AuditAdTemplatesCommand::Generate(generate),
    )) = audit.command else {
        panic!("expected audit ad-templates generate command");
    };
    assert!(generate.scroll, "--scroll should enable generation scrolling");
}
```

- [ ] **Step 2: Run the focused test and verify it fails**

```bash
HOST_TARGET="$(rustc -vV | awk '/host:/ { print $2 })"
cargo test --package trusted-server-cli --target "$HOST_TARGET" audit_ad_templates_generate_parses_scroll
```

Expected: compilation fails because `AuditAdTemplatesGenerateArgs` has no
`scroll` field.

- [ ] **Step 3: Add the flag and session wiring**

Add to `AuditAdTemplatesGenerateArgs`:

```rust
/// Perform a deterministic scroll pass after each page initially settles.
#[arg(long)]
pub scroll: bool,
```

Add `scroll: bool` to `BrowserAuditCollector` and `SessionSettings`, default it
to false, and add `with_scroll(bool)`. Thread it through `session()`,
`with_browser`, `collect_page_from_browser`, and `collect_open_page`; Task 2
will use it.

Add `scroll: bool` to `UpdateSlotsRequest`. In `run_audit`, set both the
collector option and request field from `gen_args.scroll`. Update every test
fixture constructing `UpdateSlotsRequest` with `scroll: false`, except the later
contextual-warning test.

- [ ] **Step 4: Run parsing/default tests**

```bash
HOST_TARGET="$(rustc -vV | awk '/host:/ { print $2 })"
cargo test --package trusted-server-cli --target "$HOST_TARGET" audit_generate_subcommands_use_generation_settle_defaults
cargo test --package trusted-server-cli --target "$HOST_TARGET" audit_ad_templates_generate_parses_scroll
```

Expected: both pass.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/run.rs crates/trusted-server-cli/src/commands/audit/mod.rs crates/trusted-server-cli/src/commands/audit/generate/browser_collector.rs crates/trusted-server-cli/src/commands/audit/generate/mod.rs
git commit -m "Add scroll option to ad-template generation"
```

### Task 2: Share and execute deterministic scrolling

**Files:**
- Create: `crates/trusted-server-cli/src/commands/audit/browser_scroll.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/mod.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/browser.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/browser_collector.rs`
- Modify: `scripts/test-cli.sh`

- [ ] **Step 1: Add a failing Chrome fixture**

Add a self-contained tall HTML page whose scroll listener installs a stub GPT
registry and defines `/123/lazy` in `ad-lazy-0` only after `window.scrollY > 0`.
Add this ignored test:

```rust
#[test]
#[ignore = "requires local Chrome/Chromium; run through scripts/test-cli.sh"]
fn collects_lazy_gpt_slot_only_when_scroll_is_enabled() {
    if !browser_fixture_available() {
        return;
    }
    let url = lazy_gpt_fixture_url();
    let without_scroll = BrowserAuditCollector::default()
        .collect_page(&url, &[])
        .expect("should collect without scrolling");
    let with_scroll = BrowserAuditCollector::default()
        .with_scroll(true)
        .collect_page(&url, &[])
        .expect("should collect with scrolling");

    assert!(without_scroll.gpt_slots.is_empty());
    assert!(with_scroll.gpt_slots.iter().any(|slot| {
        slot.gam_unit_path == "/123/lazy" && slot.div_id == "ad-lazy-0"
    }));
}
```

Use loopback HTTP instead of `file://` if Chrome requires it for reliable scroll
events. Change `scripts/test-cli.sh` to run the ignored
`commands::audit::generate::browser_collector::tests::` prefix so lifecycle and
lazy-slot fixtures are both covered.

- [ ] **Step 2: Run the fixture and verify it fails**

```bash
HOST_TARGET="$(rustc -vV | awk '/host:/ { print $2 })"
TS_AUDIT_BROWSER_TESTS=1 cargo test --package trusted-server-cli --target "$HOST_TARGET" collects_lazy_gpt_slot_only_when_scroll_is_enabled -- --ignored --test-threads=1
```

Expected: the scrolled result still lacks `/123/lazy`.

- [ ] **Step 3: Implement the shared primitive**

Create `browser_scroll.rs` with a `ScrollFailure` enum (evaluation failure and
timeout) and:

```rust
pub(crate) async fn scroll_page(page: &chromiumoxide::Page) -> Vec<ScrollFailure> {
    let mut failures = Vec::new();
    for fraction in ["0.33", "0.66", "1"] {
        let script = format!(
            "window.scrollTo(0, Math.floor(Math.max(document.body.scrollHeight, document.documentElement.scrollHeight) * {fraction}))"
        );
        evaluate(page, script, &mut failures).await;
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    evaluate(page, "window.scrollTo(0, 0)".to_string(), &mut failures).await;
    failures
}
```

Bound each evaluation at five seconds. Declare the module in `audit/mod.rs`.
In `browser.rs`, leave the pre-scroll evidence snapshot and
`window.__tsScrollPhase = true` marker in place, replace the local step loop with
the shared function, and map failures to existing `Warning` output.

In the generation collector, after initial settle but before final HTML/GPT/
network/link scraping, call the shared function when `scroll` is true, append
its failures as page warnings, and call `wait_for_page_settle` again. A second
settle timeout is a warning, not a discarded page.

- [ ] **Step 4: Run browser tests**

```bash
HOST_TARGET="$(rustc -vV | awk '/host:/ { print $2 })"
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::audit::browser::tests::
TS_AUDIT_BROWSER_TESTS=1 cargo test --package trusted-server-cli --target "$HOST_TARGET" collects_lazy_gpt_slot_only_when_scroll_is_enabled -- --ignored --test-threads=1
```

Expected: all pass and `/123/lazy` appears only with scrolling.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/commands/audit/browser_scroll.rs crates/trusted-server-cli/src/commands/audit/mod.rs crates/trusted-server-cli/src/commands/audit/browser.rs crates/trusted-server-cli/src/commands/audit/generate/browser_collector.rs scripts/test-cli.sh
git commit -m "Collect lazy ad slots during generation scroll"
```

### Task 3: Report unmatched slots preserved by merge

**Files:**
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/slot_toml.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/mod.rs`

- [ ] **Step 1: Write failing diagnostic tests**

Next to `merge_keeps_existing_only_slots`, assert that diagnostic merging marks
preserved `sidebar` but not rediscovered `header`; multiple missing IDs retain
configuration order; and full rediscovery, empty existing slots, and
`--replace` produce no stale IDs.

Add command tests with fake collectors and in-memory writers. Assert non-scroll
wording contains `or --scroll`, scroll wording omits that retry, stdout remains
only diff/summary content, and preserved slots remain in candidate TOML.

- [ ] **Step 2: Run focused tests and verify they fail**

```bash
HOST_TARGET="$(rustc -vV | awk '/host:/ { print $2 })"
cargo test --package trusted-server-cli --target "$HOST_TARGET" merge_reports_preserved_unobserved_slots
cargo test --package trusted-server-cli --target "$HOST_TARGET" update_slots_reports_preserved_unobserved_slots
```

Expected: failures because unmatched existing slots are not exposed.

- [ ] **Step 3: Add structured merge diagnostics**

Define:

```rust
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct MergeDiagnostics {
    pub(super) notes: Vec<String>,
    pub(super) unobserved_existing_slot_ids: Vec<String>,
}
```

Change `merge_render_slots_with_diagnostics` to return this structure with the
merged slots. Record every matched existing index in a `BTreeSet<usize>`, then
collect unmatched existing IDs by enumerating configuration order. Preserve the
current broad-prefix messages in `notes`. The `replace || existing.is_empty()`
early path returns default diagnostics. Keep `merge_render_slots` returning only
the slot vector.

- [ ] **Step 4: Format the contextual note in `run_update_slots`**

Extend pending notes with `merge_diagnostics.notes`. If unmatched IDs exist,
append their count and comma-separated IDs. End with:

```rust
let follow_up = if request.scroll {
    "Re-run with broader page/profile coverage; use --replace only to intentionally prune them."
} else {
    "Re-run with broader coverage or --scroll; use --replace only to intentionally prune them."
};
```

Do not change the merged configuration. `emit_notes` remains the only terminal
sanitization/output boundary.

- [ ] **Step 5: Run merge and command tests**

```bash
HOST_TARGET="$(rustc -vV | awk '/host:/ { print $2 })"
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::audit::generate::slot_toml::tests::merge_
cargo test --package trusted-server-cli --target "$HOST_TARGET" update_slots_reports_preserved_unobserved_slots
```

Expected: all pass, with unchanged merged TOML and warnings only on stderr.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-cli/src/commands/audit/generate/slot_toml.rs crates/trusted-server-cli/src/commands/audit/generate/mod.rs
git commit -m "Warn about preserved unobserved ad slots"
```

### Task 4: Document and verify

**Files:**
- Modify: `docs/guide/cli.md`

- [ ] **Step 1: Document both behaviors**

Add a generation `--scroll` example under “Bounding and steering the crawl.”
Explain that every page/profile scrolls after initial settle and settles again,
and that it is opt-in because it adds time, requests, and publisher side effects.

Update merge documentation: missing existing slots are preserved and named on
stderr; absence may reflect coverage, targeting, or lazy loading; only
`--replace` intentionally prunes them.

- [ ] **Step 2: Format docs and inspect scope**

```bash
cd docs && npm run format
git diff --check
git diff -- docs/guide/cli.md
```

Expected: formatting passes and only intended docs change.

- [ ] **Step 3: Run the full CLI harness, including Chrome fixtures**

```bash
./scripts/test-cli.sh
```

Expected: all host CLI and configured ignored browser tests pass.

- [ ] **Step 4: Run formatting and lint gates**

```bash
cargo fmt --all -- --check
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

Expected: all exit zero without warnings.

- [ ] **Step 5: Run adapter regression suites**

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

Expected: all pass. Do not use bare `cargo test --workspace`.

- [ ] **Step 6: Review scope and commit docs**

```bash
git status --short
git diff --check
git diff HEAD -- crates/trusted-server-cli scripts/test-cli.sh docs/guide/cli.md
```

Confirm `fastly.toml` remains untouched and issue #1059 produced no code changes.
Then:

```bash
git add docs/guide/cli.md
git commit -m "Document generation scroll and stale-slot warnings"
```
