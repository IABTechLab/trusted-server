# PR 823 Round-5 Review Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve every actionable finding in PR 823 review `4989897698` while preserving generation compatibility and enforcing root-less template safety.

**Architecture:** Keep browser-option defaults and legacy clap compatibility at the CLI boundary, carry borrowed-root evidence through template inference, and reject unsafe overrides before rendering. Improve diagnostics and validation at their existing seams, then pin cross-language and documentation invariants with focused tests.

**Tech Stack:** Rust 2024, clap 4 derive, `url`, `toml`, embedded JavaScript, mdBook/VitePress documentation.

---

## File Map

- `crates/trusted-server-cli/src/commands/audit/collector.rs`: generation browser default constants and option defaults.
- `crates/trusted-server-cli/src/commands/audit/mod.rs`: hidden legacy browser arguments, early TOML validation, conversion to generation arguments.
- `crates/trusted-server-cli/src/run.rs`: clap contract tests.
- `crates/trusted-server-cli/src/commands/audit/generate/browser_collector.rs`: collector defaults and formatting.
- `crates/trusted-server-cli/src/commands/audit/generate/unit_template.rs`: borrowed-root inference metadata and root-gap refusal reasons.
- `crates/trusted-server-cli/src/commands/audit/generate/mod.rs`: redirect output, profile-scoped notes, merge-policy validation, explicit-pattern refusal.
- `crates/trusted-server-cli/src/commands/audit/generate/gpt_slots.rs`: timestamp-shaped volatile token recognition.
- `crates/trusted-server-cli/src/commands/audit/browser.rs`: Rust/JavaScript evidence-cap invariant test.
- `crates/trusted-server-cli/src/commands/audit/page.rs`: accurate final-URL/terminal-escaping test claims.
- `docs/guide/cli.md` and the volatile-collision design/plan: operator and historical documentation corrections.

### Task 1: Restore generation browser defaults and legacy clap isolation

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/collector.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/browser_collector.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/mod.rs`
- Modify: `crates/trusted-server-cli/src/run.rs`

- [ ] **Step 1: Add failing clap and default tests**

Add parser coverage proving that `ts audit --help` does not advertise generation
browser flags, `ts audit --chrome /tmp/chrome generate ...` is rejected, and the
legacy `ts audit <url> --chrome ... --settle-max-ms ...` form still parses and
reaches `GenerateArgs`. Add a generation-option default assertion for 750 ms and
12,000 ms.

- [ ] **Step 2: Run the focused tests and confirm RED**

Run:

```bash
cargo test --package trusted-server-cli --target aarch64-apple-darwin run::tests::audit_ -- --nocapture
cargo test --package trusted-server-cli --target aarch64-apple-darwin commands::audit::tests::legacy_ -- --nocapture
```

Expected: the hidden/help and 12-second assertions fail on the current branch.

- [ ] **Step 3: Implement one generation-default source and legacy mirror**

Define generation-specific constants in `collector.rs` and use them in clap
attributes and `GenerateBrowserOpts::default`:

```rust
pub(crate) const GENERATE_SETTLE_QUIET_MS: u64 = 750;
pub(crate) const GENERATE_SETTLE_MAX_MS: u64 = 12_000;
```

Use those constants in `BrowserAuditCollector::default`. Replace the flattened
`GenerateBrowserOpts` under `LegacyGenerateArgs` with `LegacyBrowserOpts`, whose
seven fields each use `hide = true, requires = "legacy_url"`. Implement
`From<&LegacyBrowserOpts> for GenerateBrowserOpts` and use it in
`legacy_generate_args`. Add the missing blank line between collector methods.

- [ ] **Step 4: Re-run focused tests and confirm GREEN**

Run the Step 2 commands and the focused collector default test.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/commands/audit/collector.rs crates/trusted-server-cli/src/commands/audit/generate/browser_collector.rs crates/trusted-server-cli/src/commands/audit/mod.rs crates/trusted-server-cli/src/run.rs
git commit -m "Preserve generation browser option contracts"
```

### Task 2: Enforce borrowed-root and merge-policy safety

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/generate/unit_template.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/mod.rs`

- [ ] **Step 1: Add failing inference and end-to-end tests**

Add tests proving:

- `InferenceOutcome` identifies `ad-sidebar` as borrowing the root witnessed by
  another slot;
- explicit `--page-pattern` values cause `run_update_slots` to fail before the
  source config changes when any rendered template borrowed the root;
- no-policy inference gives affected multi-path slots the root-witness reason;
- a configured `section_segment = 1` with no `section_root` refuses inferred
  segment 0 when preserved `{section}` slots exist;
- the same segment, or an unset segment, allows adopting the inferred root.

- [ ] **Step 2: Run the focused tests and confirm RED**

Run:

```bash
cargo test --package trusted-server-cli --target aarch64-apple-darwin commands::audit::generate::unit_template::tests -- --nocapture
cargo test --package trusted-server-cli --target aarch64-apple-darwin commands::audit::generate::tests::merge_ -- --nocapture
cargo test --package trusted-server-cli --target aarch64-apple-darwin commands::audit::generate::tests::explicit_ -- --nocapture
```

Expected: borrowed stems are unavailable, explicit patterns are accepted, and
the configured-segment mismatch is accepted.

- [ ] **Step 3: Carry borrowed stems and reject unsafe overrides**

Add an ordered `borrowed_section_root: Vec<String>` field to
`InferenceOutcome`. Populate it only when `RootUnwitnessed` successfully becomes
a template. Before building render slots, reject non-empty explicit patterns if
that vector is non-empty:

```rust
return cli_error(format!(
    "cannot apply --page-pattern to slot(s) {} because their {{section}} templates borrow section_root; remove --page-pattern so patterns can be derived from observed paths",
    borrowed.join(", ")
));
```

On the no-policy path, replace the generic multi-path refusal reason for
structurally valid root-unwitnessed slots with the specific missing-root-witness
reason. Preserve structural refusal reasons unchanged.

Update `validate_merge_policy` so an explicit configured segment is compared
before the empty-root adoption return. Keep the guard limited to preserved
`{section}` slots and allow `--replace`.

- [ ] **Step 4: Re-run focused tests and confirm GREEN**

Run all Step 2 commands.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/commands/audit/generate/unit_template.rs crates/trusted-server-cli/src/commands/audit/generate/mod.rs
git commit -m "Protect borrowed section templates during generation"
```

### Task 3: Make redirects, warnings, and config errors actionable

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/generate/mod.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/mod.rs`

- [ ] **Step 1: Add failing diagnostic tests**

Strengthen the HTTPS-upgrade assertion to require
`http://publisher.example/` and `https://publisher.example/`. Add a two-profile
warning test whose output names desktop and mobile separately. Add a malformed
whole-document TOML test while retaining tests for unknown valid settings and an
unreadable `[creative_opportunities]` section.

- [ ] **Step 2: Run the focused tests and confirm RED**

Run:

```bash
cargo test --package trusted-server-cli --target aarch64-apple-darwin update_slots_accepts_a_same_host_https_upgrade -- --nocapture
cargo test --package trusted-server-cli --target aarch64-apple-darwin profile_warning -- --nocapture
cargo test --package trusted-server-cli --target aarch64-apple-darwin creative_config -- --nocapture
```

- [ ] **Step 3: Implement scoped diagnostics and early parse failure**

Render redirect endpoints as `origin.ascii_serialization() + path`. Thread the
profile label into `fold_collected`; keep the consent-stub warning global, label
page warnings/interstitials with path and profile, and retain the existing
site-wide discovery-warning dedupe.

Replace `.ok()` in `creative_config` with an error mapping that identifies a
malformed existing TOML document and explains that generation did not start.
Continue parsing into `toml::Value`, not runtime `Settings`, so valid unknown
settings remain tolerated.

- [ ] **Step 4: Re-run focused tests and confirm GREEN**

Run all Step 2 commands.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/commands/audit/generate/mod.rs crates/trusted-server-cli/src/commands/audit/mod.rs
git commit -m "Clarify audit generation diagnostics"
```

### Task 4: Pin detector and embedded-collector invariants

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/generate/gpt_slots.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/browser.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/page.rs`

- [ ] **Step 1: Add failing invariant tests**

Add a negative volatile-token test for `promo-20260820a-sidebar`, retain a
positive timestamp-shaped control with at least ten leading digits, and add the
embedded-JavaScript constant assertion:

```rust
assert!(
    AD_TEMPLATE_COLLECTOR_JS.contains(&format!(
        "const __ts_max_entries = {MAX_EVIDENCE_ENTRIES}"
    )),
    "should keep the JS cap equal to MAX_EVIDENCE_ENTRIES"
);
```

In the page summary test, assert the exact percent-encoded final URL line and
limit the raw-control assertion's comment to title and warning fields.

- [ ] **Step 2: Run the focused tests and confirm RED**

Run:

```bash
cargo test --package trusted-server-cli --target aarch64-apple-darwin per_render_token -- --nocapture
cargo test --package trusted-server-cli --target aarch64-apple-darwin evidence_entries -- --nocapture
cargo test --package trusted-server-cli --target aarch64-apple-darwin page_controlled_text -- --nocapture
```

- [ ] **Step 3: Tighten the token shape and correct the test claim**

Require at least ten leading digits in `is_per_render_token`. Keep the rest of
the recognizer unchanged. Add the evidence-cap test and page assertion without
removing final-URL escaping.

- [ ] **Step 4: Re-run focused tests and confirm GREEN**

Run all Step 2 commands.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-cli/src/commands/audit/generate/gpt_slots.rs crates/trusted-server-cli/src/commands/audit/browser.rs crates/trusted-server-cli/src/commands/audit/page.rs
git commit -m "Pin audit evidence recognition invariants"
```

### Task 5: Align documentation and local style

**Files:**

- Modify: `docs/guide/cli.md`
- Modify: `docs/superpowers/specs/2026-08-19-refuse-volatile-div-collisions-design.md`
- Modify: `docs/superpowers/plans/2026-08-19-refuse-volatile-div-collisions.md`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/mod.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/browser_collector.rs`

- [ ] In the volatile-collision design example, remove the section-varying
      sidebar from the list of omitted/explained slots because root-less templating
      now writes it with a borrowed-root diagnostic.
- [ ] In the volatile-collision implementation plan, state that a recognized
      render token must have a non-empty family prefix before it and placement
      content after it; remove the broader "in any position" claim.
- [ ] Update the guide to say that a configured segment without a root is
      preserved for existing templates, and document the explicit-pattern refusal
      for borrowed-root slots.
- [ ] Add the missing `GenerateArgs.browser` doc comment, change the `expect`
      message to the required `"should ..."` form, and retain the method-separation
      blank line from Task 1.
- [ ] Run `cd docs && npm run format` and `cargo fmt --all -- --check`.
- [ ] Commit:

```bash
git add docs/guide/cli.md docs/superpowers/specs/2026-08-19-refuse-volatile-div-collisions-design.md docs/superpowers/plans/2026-08-19-refuse-volatile-div-collisions.md crates/trusted-server-cli/src/commands/audit/generate/mod.rs crates/trusted-server-cli/src/commands/audit/generate/browser_collector.rs
git commit -m "Align ad-template generation documentation"
```

### Task 6: Verify the complete review resolution

**Files:**

- Verify all files above.

- [ ] Run focused audit generation tests:

```bash
cargo test --package trusted-server-cli --target aarch64-apple-darwin commands::audit::generate -- --nocapture
```

- [ ] Run the complete host CLI suite:

```bash
./scripts/test-cli.sh aarch64-apple-darwin
```

- [ ] Run lint and formatting gates:

```bash
cargo clippy --package trusted-server-cli --target aarch64-apple-darwin --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cd docs && npm run format
git diff --check
```

- [ ] Inspect `git status --short`, `git log --oneline -6`, and the complete
      diff from `073d5644` to ensure only the approved review resolution is present.
- [ ] Do not push or post GitHub replies without separate user authorization.
