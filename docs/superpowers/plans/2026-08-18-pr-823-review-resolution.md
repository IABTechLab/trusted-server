# PR 823 Review Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve every actionable finding in PR 823 review `4958563121`, verify the branch, publish it, and answer all 28 inline threads.

**Architecture:** Correct the review findings at four existing seams: core runtime gate APIs, pure CLI projection/comparison, crawl generation and TOML persistence, and the shared browser session. Keep page-controlled work bounded, use one source of truth for runtime/browser behavior, and preserve operator-authored configuration outside the managed creative-opportunities fields (`slot`, `gam_network_id`, `section_root`, and `section_segment`).

**Tech Stack:** Rust 2024, clap 4, toml_edit 0.23, chromiumoxide 0.9, Tokio current-thread runtime, serde/serde_json, embedded JavaScript collector, mdBook documentation, GitHub CLI.

---

## File Map

- `crates/trusted-server-core/src/creative_opportunities.rs`: allocation-free gate evaluation, gate diagnostics, pattern validation, consent semantics.
- `crates/trusted-server-core/src/publisher.rs`: named gate input at the runtime call site.
- `crates/trusted-server-cli/src/ad_templates/{expected,compare,output}.rs`: runtime-equivalent projection, typed formats, confirmability, safe output.
- `crates/trusted-server-cli/src/commands/config/ad_templates.rs`: static command validation, gate parity, lint, escaping.
- `crates/trusted-server-cli/src/commands/audit/{collector,browser,ad_templates,ad_template_collector.js}.rs`: shared browser options/session and verifier behavior.
- `crates/trusted-server-cli/src/commands/audit/generate/{browser_collector,evidence,gpt_slots,crawl_plan,page_patterns,unit_template,slot_toml,mod,validate}.rs`: crawl evidence, inference, persistence, and dry-run safety.
- `crates/trusted-server-cli/src/commands/audit/{mod,page}.rs`, `crates/trusted-server-cli/src/run.rs`, `crates/trusted-server-cli/src/main.rs`: clap contracts and exit outcomes.
- `docs/guide/cli.md`, `scripts/test-cli.sh`, `.github/workflows/test.yml`: operator contract and enforced browser CI.

## Task 1: Make the runtime gate API allocation-free and reusable

**Files:**

- Modify: `crates/trusted-server-core/src/creative_opportunities.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Add failing core tests**

Add tests that sweep all 64 boolean combinations with `consent_allows_auction: None`, assert the expected `No`/`Unknown` result, assert `blocking_gates()` derives diagnostics without an owned `Vec`, and exercise the specific page-pattern validation error.

Use a borrowed/static iterator contract:

```rust
pub fn blocking_gates(self) -> impl Iterator<Item = AdStackGateName> {
    AdStackGateName::ALL
        .into_iter()
        .filter(move |gate| gate.blocks(self.input))
}

pub fn validate_page_pattern(pattern: &str) -> Result<(), String> {
    compile_page_pattern(pattern).map(|_| ())
}
```

- [ ] **Step 2: Run the narrow tests and confirm RED**

Run:

```bash
cargo test --package trusted-server-core --target "$(rustc -vV | awk '/host:/ {print $2}')" ad_stack_gate -- --nocapture
```

Expected: failure because the unknown-consent sweep and allocation-free diagnostic API are not implemented.

- [ ] **Step 3: Implement the minimal core change**

Store the original `AdStackGateInput` in `AdStackGateResult`, compute `expected` with boolean expressions rather than `Vec::push`, expose a zero-allocation iterator over a `const ALL`, make `compile_page_pattern` crate-private, and add `validate_page_pattern`. Document that `None` means unknown and differs from denied (`Some(false)`). Preserve the detailed glob error in `compile_patterns`.

Delete `should_run_server_side_ad_stack`; construct `AdStackGateInput` with named fields in `publisher.rs`. Import the gate types at module scope.

- [ ] **Step 4: Verify GREEN**

Run the narrow command again, then:

```bash
cargo test-fastly creative_opportunities
cargo test-axum creative_opportunities
```

Expected: all selected tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/creative_opportunities.rs crates/trusted-server-core/src/publisher.rs
git commit -m "Align ad stack gate diagnostics with runtime"
```

## Task 2: Align expected-slot projection and comparison with runtime behavior

**Files:**

- Modify: `crates/trusted-server-cli/src/ad_templates/expected.rs`
- Modify: `crates/trusted-server-cli/src/ad_templates/compare.rs`
- Modify: `crates/trusted-server-cli/src/ad_templates/output.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/ad_templates.rs`

- [ ] **Step 1: Add failing projection and comparison tests**

Cover:

- an unrenderable dynamic slot is omitted from expected slots and does not make `matched_slots` pass;
- the diagnostic says the runtime omits the slot for that path;
- `MediaType` remains typed through comparison;
- video/native-only and out-of-page slots produce `Unconfirmable` and do not fail strict;
- an incompatible banner is still `Partial` and fails strict;
- a missing slot has `phase: None` and JSON omits `phase`;
- server-side APS configuration alone does not emit `aps_evidence_missing`;
- collector warnings are appended to page warnings;
- human output contains expectation, gates, matched count, extra evidence, and warnings;
- bidi override/isolate characters are escaped.

The central type changes are:

```rust
pub struct ExpectedFormat {
    pub width: u32,
    pub height: u32,
    pub media_type: MediaType,
}

pub enum SlotStatus {
    Confirmed,
    Partial,
    Missing,
    Unconfirmable,
}

pub struct SlotResult {
    pub phase: Option<EvidencePhase>,
    // existing fields
}
```

- [ ] **Step 2: Run the narrow tests and confirm RED**

Run:

```bash
HOST_TARGET="$(rustc -vV | awk '/host:/ {print $2}')"
cargo test --package trusted-server-cli --target "$HOST_TARGET" ad_templates::expected
cargo test --package trusted-server-cli --target "$HOST_TARGET" ad_templates::compare
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::audit::ad_templates
```

Expected: new assertions fail on current projection/status/warning behavior.

- [ ] **Step 3: Implement projection, comparison, and output changes**

Filter `match_slots` with `render_gam_unit_path(...).map(...)` while building `ExpectedSlot`. Remove the unconditional client-side APS check. Compute confirmability before assigning status. Map typed media values to strings only in `to_slot_json`. Make JSON phase `Option<EvidencePhaseJson>` with `skip_serializing_if = "Option::is_none"`. Extend warnings with `evidence.warnings` after decode.

Extend `is_terminal_control` with `0x202A..=0x202E` and `0x2066..=0x2069`. Apply `escape_terminal_text` to every human-facing page/config-derived field.

- [ ] **Step 4: Verify GREEN**

Run all three narrow commands again.

Expected: all selected tests pass with no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/ad_templates crates/trusted-server-cli/src/commands/audit/ad_templates.rs
git commit -m "Match ad template verification to runtime behavior"
```

## Task 3: Correct static CLI contracts and process exit semantics

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/config/ad_templates.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/mod.rs`
- Modify: `crates/trusted-server-cli/src/run.rs`
- Modify: `crates/trusted-server-cli/src/main.rs`
- Modify: `crates/trusted-server-cli/Cargo.toml`
- Modify: `Cargo.lock`

- [ ] **Step 1: Add failing parser, normalization, lint, and outcome tests**

Add tests proving:

- bare and full-URL forms normalize spaces, dot segments, tabs, queries, and fragments identically;
- `/r?to=https://example.com` remains a bare path;
- `check` requires exactly one expectation mode and rejects `--allow-extra-slots --expect-no-slots` through clap;
- `--method` accepts a valid `http::Method` and uses exact GET semantics;
- `lint` reports each invalid configured pattern;
- `explain` uses `gate.expected` even when providers are empty and prints provider state separately;
- `--edgezero-enabled` is rejected because the unsupported model is removed;
- bare `ts audit` displays help rather than a drifting manual error;
- parser coverage includes lint, explain, generate, verify profiles/options, and the no-`--adapter` contract;
- an assertion outcome maps to exit 1 and a tool error maps to exit 2.

Use an explicit process outcome:

```rust
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RunOutcome {
    Success,
    AssertionFailed,
}

impl RunOutcome {
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Success => 0,
            Self::AssertionFailed => 1,
        }
    }
}
```

Tool failures remain `Err(String)` and therefore exit 2. Assertion commands write their failure to stderr before returning `AssertionFailed`, avoiding `log::error!` filtering.

- [ ] **Step 2: Run parser/static tests and confirm RED**

Run:

```bash
HOST_TARGET="$(rustc -vV | awk '/host:/ {print $2}')"
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::config::ad_templates
cargo test --package trusted-server-cli --target "$HOST_TARGET" run::tests
```

Expected: current hand-rolled validation, normalization, and exit behavior fail the new tests.

- [ ] **Step 3: Implement the CLI contract**

Use a dummy HTTPS base with `Url::options().base_url(...)` for bare paths after anchored scheme detection on the pre-query slice. Add clap `ArgGroup`, `conflicts_with`, `arg_required_else_help`, typed `http::Method`, and browser settle validation. Add `http = { workspace = true }` to the CLI host dependencies.

Return `RunOutcome` from dispatchable CI commands. Keep edgezero delegated errors as tool errors. Remove the unsupported EdgeZero flag/text and route gate output through `blocking_gates()`.

- [ ] **Step 4: Verify GREEN**

Run the two narrow commands again and confirm all tests pass.

- [ ] **Step 5: Commit**

```bash
git add Cargo.lock crates/trusted-server-cli/Cargo.toml crates/trusted-server-cli/src/main.rs crates/trusted-server-cli/src/run.rs crates/trusted-server-cli/src/commands/audit/mod.rs crates/trusted-server-cli/src/commands/config/ad_templates.rs
git commit -m "Define ad template CLI assertion contracts"
```

## Task 4: Make the injected collector bounded and behavior-preserving

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/ad_template_collector.js`
- Modify: `crates/trusted-server-cli/src/commands/audit/collector.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/browser.rs`

- [ ] **Step 1: Add failing JavaScript-contract and decoder tests**

Add tests/fixtures for an out-of-`u32` size beside a valid slot, a truthy `googletag.cmd` without `push`, multiple `cmd.push` arguments, 512-character capture limits, and non-enumerable/closure-local wrapping. Replace the existing `contains("cmd.push")` assertion with assertions that the no-op wrapper is absent.

The JavaScript bounds are:

```javascript
const __TS_MAX_STRING = 512
function __ts_text(value) {
  return String(value).slice(0, __TS_MAX_STRING)
}

if (width > 4294967295 || height > 4294967295) return null
```

The setter must always retain the publisher value:

```javascript
set(value) {
  try {
    internal = wrap(value)
  } catch (error) {
    internal = value
    __ts_push(__ts_ev.warnings, {
      code: "wrap_failed",
      message: __ts_text(error),
    })
  }
}
```

- [ ] **Step 2: Run the narrow tests and confirm RED**

Run:

```bash
HOST_TARGET="$(rustc -vV | awk '/host:/ {print $2}')"
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::audit::collector
cargo test --package trusted-server-cli --target "$HOST_TARGET" collector_payload
```

Expected: current script permits oversized integers and retains the behavior-changing wrapper.

- [ ] **Step 3: Implement minimal collector changes**

Guard all page-derived strings through `__ts_text`, enforce numeric upper bounds, delete the `cmd.push` wrapper, use a closure-local `WeakSet` for wrapped objects, and install wrapped functions with non-enumerable `Object.defineProperty`. Soften the header claim to “observes without capturing page data.”

Before serde decode, stringify the evidence inside the page and return a small
sentinel instead of the payload when the serialized string exceeds 1 MiB
(`MAX_EVIDENCE_PAYLOAD_BYTES = 1_048_576`). On the Rust side, the sentinel
produces an `ad_evidence_too_large` warning and `ad_evidence: None`; it does not
fail navigation or the whole collection. This bounds CDP transfer and Rust
decode/allocation while preserving a precise operator diagnostic.

- [ ] **Step 4: Verify GREEN**

Run the narrow commands again and confirm all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/commands/audit/ad_template_collector.js crates/trusted-server-cli/src/commands/audit/collector.rs crates/trusted-server-cli/src/commands/audit/browser.rs
git commit -m "Bound browser ad template evidence collection"
```

## Task 5: Unify browser launch, session reuse, and settling

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/collector.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/browser.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/ad_templates.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/page.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/mod.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/browser_collector.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/collector.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/mod.rs`

- [ ] **Step 1: Add failing fake-collector and browser configuration tests**

Cover one launch/session for multiple URLs, root included in the profile batch, page close on success/error, host-only `Path=/` cookies, explicit final-URL failure, same-host HTTP-to-HTTPS acceptance, host/downgrade/port refusal, new-headless 1280x800 defaults, headful/profile/proxy/consent parity, `$CHROME` parity, and generic/legacy default-on consent.

Extend the trait with a default batch method so fakes remain simple:

```rust
pub trait AuditCollector {
    fn collect_page(&self, request: BrowserCollectRequest) -> Result<CollectedPage, String>;

    fn collect_pages(
        &self,
        requests: &[BrowserCollectRequest],
    ) -> Vec<Result<CollectedPage, String>> {
        requests.iter().cloned().map(|request| self.collect_page(request)).collect()
    }
}
```

The real browser implementation overrides `collect_pages` to create one runtime,
temporary profile, browser, handler, and sequentially closed pages.

- [ ] **Step 2: Run narrow tests and confirm RED**

Run:

```bash
HOST_TARGET="$(rustc -vV | awk '/host:/ {print $2}')"
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::audit::browser
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::audit::ad_templates
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::audit::generate::browser_collector
```

Expected: verifier launches per URL, browser defaults diverge, and tabs/cookies/final URL handling fail new assertions.

- [ ] **Step 3: Implement shared browser configuration and batching**

Move executable resolution and launch-option construction into `browser.rs` as crate-visible helpers used by both collectors. Flatten shared browser options into generate and verify, while keeping generation-only pacing/crawl flags local. Build cookies with explicit domain from `url.host_str()` and `path = Some("/".to_string())`; do not set `url` simultaneously.

In each page collector, capture the inner result, always call bounded `page.close().await`, then return the captured result. Batch verify requests via `collect_pages`. Include the root in each profile's batch rather than collecting it in a throwaway session. Use `spawn_blocking` for scraper analysis before folding results.

- [ ] **Step 4: Bound post-navigation work and correct settle semantics**

Install `performance.setResourceTimingBufferSize(100000)` before navigation. Make `settle` return warnings and wrap every `evaluate`, URL/title read, scroll operation, and evidence read in a per-operation timeout. Accrue quiet only after `document.readyState` is `interactive` or `complete`; sleep `min(remaining_quiet, 250ms)` so short quiet values are honored. Treat `wait_for_navigation` timeout as a warning after successful `goto`.

Propagate GPT/link/sitemap evaluation errors as notes, set `await_promise` for sitemap discovery, and warn when only the main frame is inspected while child frames exist.

- [ ] **Step 5: Verify GREEN**

Run all three narrow commands again. If Chrome is available, also run:

```bash
HOST_TARGET="$(rustc -vV | awk '/host:/ {print $2}')"
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::audit::browser::tests:: -- --ignored --test-threads=1
```

Expected: unit/fake tests pass; browser fixtures execute and pass when Chrome exists.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-cli/src/commands/audit
git commit -m "Share browser sessions across ad template audits"
```

## Task 6: Preserve crawl evidence and make inference conservative

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/generate/evidence.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/gpt_slots.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/page_patterns.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/crawl_plan.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/unit_template.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/mod.rs`

- [ ] **Step 1: Add failing inference tests**

Add focused tests for:

- `annonsü1`/`annonsü2` and `ünicode-ad-a`/`ünicode-ad-b` prefixes;
- desktop-empty/mobile-present and the inverse;
- two disjoint unrelated placements retained, two with a useful prefix or three fragments refused;
- same-page normalized UUID collisions retained with raw div IDs and all formats;
- 16+ digit numeric stable segments retained;
- comma-separated SRA `dids` ignored;
- locale `/en` pattern emitted as `/en` and every emitted glob matches its source path;
- glob metacharacters escaped with `glob::Pattern::escape`;
- percent-encoded noise/extension paths and `.html`/`.htm`/`.php` treatment;
- dropped-section notes capped at ten plus “and N more”;
- both ambiguous template rows result in explicit `Refuse`;
- real crawl evidence can infer `section_segment = 1`;
- refused slots do not appear in rendered output and their reasons appear in notes.

- [ ] **Step 2: Run narrow tests and confirm RED**

Run:

```bash
HOST_TARGET="$(rustc -vV | awk '/host:/ {print $2}')"
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::audit::generate::evidence
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::audit::generate::gpt_slots
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::audit::generate::page_patterns
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::audit::generate::crawl_plan
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::audit::generate::unit_template
```

Expected: each new regression reproduces its review finding.

- [ ] **Step 3: Implement evidence-preserving discovery**

Use the last matching `char_indices` byte boundary for shared prefixes. Remove an empty-page marker whenever a later profile yields slots. Require `(useful shared prefix || group size >= 3)` before classifying disjoint same-shape slots as fragments; emit an ambiguity diagnostic otherwise.

Group normalized collisions within a page before deduplication. When a group has multiple raw div IDs, keep raw entries, make their generated IDs unique, and attach a collision note. Restrict ephemeral hex matching to tokens containing at least one `a..f`, or an explicit UUID shape; never treat all-digit identifiers as hashes. Reject gampad fallback when parsed `dids` contains a comma.

- [ ] **Step 4: Implement conservative patterns/templates**

Emit the observed short path for locale landing pages, escape literal prefixes, decode only for filtering while retaining encoded request paths for matching, and cap notes. Teach crawl planning to carry/infer the section depth used by page-pattern generation.

Delete the tautological witness check and move its explanatory invariant into `analyse_slot` docs. Keep the existing conservative `Refuse` result for non-derivable slugs and unwitnessed roots. Filter all `Refuse` decisions before `RenderSlot` creation and push each reason into notes.

- [ ] **Step 5: Verify GREEN**

Run all five narrow commands again, then:

```bash
HOST_TARGET="$(rustc -vV | awk '/host:/ {print $2}')"
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::audit::generate
```

Expected: the generate module suite passes.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-cli/src/commands/audit/generate
git commit -m "Preserve ad template crawl evidence"
```

## Task 7: Make slot persistence and dry-run output safe

**Files:**

- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `crates/trusted-server-cli/Cargo.toml`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/slot_toml.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/mod.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/validate.rs`

- [ ] **Step 1: Add failing persistence tests**

Cover:

- trailing comments after the final slot;
- a multiline string line beginning `[foo]`;
- an array continuation beginning `[300, 250]`;
- non-contiguous slot tables;
- byte-identical unrelated sections/comments and CRLF preservation;
- end-to-end `--replace` through `run_update_slots`;
- dry-run source file byte identity;
- stdout contains only a zero-context unified diff of managed
  creative-opportunities changes and does not contain `admin_password` or
  unrelated config;
- notes/rollback warning go to stderr;
- a concurrent source edit between initial read and write is refused;
- rerun unions formats and reports broad-prefix collapse.

Change `run_update_slots` to accept separate writers:

```rust
pub(crate) fn run_update_slots(
    request: &UpdateSlotsRequest<'_>,
    collectors: &[(&str, &dyn AuditCollector)],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> CliResult<()>;
```

- [ ] **Step 2: Run persistence tests and confirm RED**

Run:

```bash
HOST_TARGET="$(rustc -vV | awk '/host:/ {print $2}')"
cargo test --package trusted-server-cli --target "$HOST_TARGET" commands::audit::generate::slot_toml
cargo test --package trusted-server-cli --target "$HOST_TARGET" update_slots
```

Expected: current line scanner corrupts/preserves incorrectly and dry-run leaks the complete config.

- [ ] **Step 3: Implement a TOML-aware managed edit**

Parse the source as `DocumentMut` and update the complete managed field set:
`creative_opportunities.slot`, `gam_network_id`, `section_root`, and
`section_segment`. Insert the generated array-of-tables and upsert only scalar
values that generation actually inferred. A generated `None` preserves the
existing scalar on both merge and `--replace`; absence of fresh evidence is
never an instruction to delete operator configuration. Retain decorations on
all other items. Before returning, parse both documents and compare canonical
clones with all four managed fields removed; return an error if any other item
differs. Preserve CRLF after serialization. Add regression cases in Step 1 for
an unresolved network ID and literal-only rerun retaining existing
`gam_network_id`/section policy.

Document `splice_creative_slots` at its definition and remove the orphaned comments. Replace the `let _ = network_id` presence check with `keys.network_id.is_none()` logic.

- [ ] **Step 4: Implement secret-safe dry-run and stale-read protection**

Add `similar` as a workspace/CLI dependency and render a zero-context unified
diff between the old and new managed creative-opportunities projection. The
projection contains only `gam_network_id`, `section_root`, `section_segment`,
and the slot array, so every generated scalar change is visible without
including unrelated operator keys:

```rust
let diff = similar::TextDiff::from_lines(old_managed, new_managed);
writeln!(out, "{}", diff.unified_diff().context_radius(0).header("configured creative opportunities", "generated creative opportunities"))?;
```

Send all notes to `err`. Immediately before atomic rename, re-read the config and compare it with the original bytes; refuse on mismatch. Do not perform this check on dry-run because no write occurs.

In `merge_render_slots`, union discovered formats into a matching existing slot and count how many discovered slots map to each existing prefix; report counts greater than one.

- [ ] **Step 5: Verify GREEN**

Run both narrow commands again and confirm all tests pass.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/trusted-server-cli/Cargo.toml crates/trusted-server-cli/src/commands/audit/generate
git commit -m "Preserve operator config during slot generation"
```

## Task 8: Complete documentation, test hygiene, and CI enforcement

**Files:**

- Modify: `docs/guide/cli.md`
- Modify: `scripts/test-cli.sh`
- Modify: `.github/workflows/test.yml`
- Modify: `crates/trusted-server-cli/src/lib.rs`
- Modify: touched Rust tests and comments under `crates/trusted-server-cli/src/`

- [ ] **Step 1: Add/restore parser and CI guard tests**

Restore the `audit` no-`--adapter` parser test. Add a script contract that sets `TS_AUDIT_BROWSER_TESTS=1`; browser fixture tests panic when that variable is set and Chrome cannot be resolved. Configure the workflow with a browser setup action or the runner's installed Chrome path and export `CHROME` before `scripts/test-cli.sh`.

- [ ] **Step 2: Replace sensitive-looking fixtures and stale assertions**

Replace `88059007`, `autoblog`, `car-research`/`carresearch`, and distinctive div tokens introduced by this PR with `123456789`, `publisher`, `/site-news`/`sitenews`, and neutral `ex_...` values. Update comments to describe shapes rather than customers.

Correct all touched `expect` messages to start with `should`, remove redundant crate/file `dead_code` allowances and annotate only genuinely deferred fields, reorder `Audit`, simplify the Prebid query parser so keys—not substrings—are matched, and bind legacy URLs directly without an impossible `expect`.

- [ ] **Step 3: Document the complete operator contract**

In `docs/guide/cli.md`, document:

- `config ad-templates lint|match|check|explain` and every flag;
- shared `--app-config`, `--manifest`, and `--no-env` behavior;
- `audit ad-templates generate|verify` browser/profile/proxy/consent/settle flags;
- dry-run stdout diff versus stderr notes;
- exit 0 success, exit 1 assertion drift, exit 2 tool/configuration error;
- refused slots are omitted with reasons;
- locale-prefixed inference and section depth;
- `Unconfirmable` strict behavior and optional evidence phase.

Update the existing design/output examples where the wire contract changed.

- [ ] **Step 4: Run format and focused checks**

Run:

```bash
cargo fmt --all -- --check
cd docs && npm run format
```

Expected: both commands exit 0.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/test.yml scripts/test-cli.sh docs crates/trusted-server-cli/src
git commit -m "Document and enforce ad template audit contracts"
```

## Task 9: Run full verification and repair regressions

**Files:**

- Modify only files implicated by a failing check.

- [ ] **Step 1: Run format and CLI/browser tests**

```bash
cargo fmt --all -- --check
./scripts/test-cli.sh
```

Expected: exit 0; browser fixture output shows tests executed rather than skipped.

- [ ] **Step 2: Run repository target suites**

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

Expected: all suites exit 0.

- [ ] **Step 3: Run all target-matched clippy gates**

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo clippy --manifest-path crates/trusted-server-cli/Cargo.toml --target "$(rustc -vV | sed -n 's/host: //p')" --all-targets -- -D warnings
```

Expected: all commands exit 0 with no warnings.

- [ ] **Step 4: Run cross-adapter parity gates**

```bash
cargo fmt --manifest-path crates/trusted-server-integration-tests/Cargo.toml -- --check
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
cargo clippy --manifest-path crates/trusted-server-integration-tests/Cargo.toml --all-targets -- -D warnings
```

Expected: formatting, parity tests, and integration-test clippy exit 0.

- [ ] **Step 5: Run JavaScript and documentation checks**

```bash
cd crates/trusted-server-js/lib && npx vitest run && npm run format && node build-all.mjs
cd ../../.. && cd docs && npm run format
```

Expected: tests/build/format exit 0.

- [ ] **Step 6: Inspect the final diff against the review**

Run:

```bash
git diff --check origin/main...HEAD
git status --short
```

Walk the 28-thread traceability table and every summary category in the design spec. Confirm each has a code/doc/test resolution or an evidence-backed response.

- [ ] **Step 7: Commit any verification-only corrections**

If verification required changes, inspect `git diff --name-only`, stage each
listed path explicitly (never `git add .`), and commit them as `Resolve ad
template review regressions`. Record those exact paths in the execution log.
Skip this commit when verification required no changes.

## Task 10: Publish and answer GitHub review threads

**Files:**

- No repository files unless publication reveals a conflict.

- [ ] **Step 1: Push the verified branch**

```bash
git push origin feature/ts-cli-ad-templates
```

Expected: push succeeds and PR 823 shows the verified head commit.

- [ ] **Step 2: Correct the PR description**

Change the legacy alias statement to say bare `ts audit <url>` aliases to `ts audit generate <url>`. Preserve all unrelated PR-body content.

- [ ] **Step 3: Reply to every inline thread**

For each ID in the spec traceability table, post through:

```bash
gh api repos/IABTechLab/trusted-server/pulls/823/comments/<COMMENT_ID>/replies -f body='<specific resolution and test evidence>'
```

Each reply must name the concrete behavior changed and, where useful, the focused test. For question threads, state the chosen behavior: union formats and diagnose broad prefixes; default consent assumption on; keep conservative refusal and align docs; allow only same-host HTTP-to-HTTPS upgrades; remove the unsupported EdgeZero model.

- [ ] **Step 4: Verify publication**

Query PR 823's head SHA, review comments, checks, and unresolved threads. Confirm all 28 inline comments have one reply and no reply claims a fix absent from the pushed diff.

- [ ] **Step 5: Report the result**

Summarize commits, verification commands, any environment limitation, PR link, and thread reply count. Do not claim checks pass without fresh output from Task 9.
