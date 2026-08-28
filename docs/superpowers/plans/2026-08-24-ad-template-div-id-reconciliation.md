# Ad-template div-ID reconciliation implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Preserve observed numeric sibling creative opportunities during config merge and refuse singleton div IDs containing shorter high-entropy per-render tokens.

**Architecture:** Reconciliation will use the normalized identities already retained by `EvidenceTable` to distinguish observed literals from intentional configured prefixes. GPT discovery will keep its vendor-neutral, position-aware volatile-family classifier and add a conservative eight-leading-digit/eight-character-suffix alternative without changing existing ten-digit behavior.

**Tech Stack:** Rust 2024, `BTreeSet`, existing Trusted Server CLI evidence/merge pipeline, Cargo unit and browser integration tests.

---

### Task 1: Preserve observed literal siblings during merge

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/generate/slot_toml.rs`

- [ ] **Step 1: Write failing numeric-sibling merge tests**

Add focused tests beside the existing prefix tests:

```rust
#[test]
fn observed_literal_does_not_claim_numeric_siblings() {
    let existing = existing_config(
        "gam_network_id = \"222\"\n\n\
         [[slot]]\nid = \"ad-sidebar-1\"\ndiv_id = \"ad-sidebar-1\"\n\
         gam_unit_path = \"/222/sidebar\"\npage_patterns = [\"/\"]\n\
         formats = [{ width = 300, height = 250 }]\n",
    );
    let discovered = ["ad-sidebar-1", "ad-sidebar-10", "ad-sidebar-11"]
        .into_iter()
        .map(|div_id| {
            RenderSlot::from_evidence(
                div_id,
                div_id,
                Some("/222/sidebar".to_string()),
                [(300, 250)],
                vec!["/news/*".to_string()],
                false,
            )
        })
        .collect();

    let (merged, diagnostics) =
        merge_render_slots_with_diagnostics(Some(&existing), discovered, false);

    assert_eq!(merged.len(), 3);
    assert!(merged.iter().any(|slot| slot.id == "ad-sidebar-10"));
    assert!(merged.iter().any(|slot| slot.id == "ad-sidebar-11"));
    assert!(diagnostics.notes.is_empty());
}
```

Add a second regression with an unrelated existing slot and discovered
`ad-sidebar-1` followed by `ad-sidebar-10`. It must prove a newly appended
observed literal cannot absorb a later sibling. Keep
`merge_reports_when_a_broad_prefix_claims_multiple_discovered_divs` unchanged as
the positive intentional-prefix control.

- [ ] **Step 2: Run the tests and verify RED**

Run:

```bash
cargo test -p trusted-server-cli observed_literal_does_not_claim_numeric_siblings -- --nocapture
cargo test -p trusted-server-cli newly_appended_literal_does_not_claim_numeric_sibling -- --nocapture
```

Expected: both fail because `ad-sidebar-1` absorbs the longer discovered IDs.

- [ ] **Step 3: Implement exact-first, evidence-aware prefix matching**

In `merge_render_slots_with_observed_diagnostics`, build a borrowed set from
`observed_div_ids` once:

```rust
let observed_literals = observed_div_ids
    .iter()
    .map(String::as_str)
    .collect::<BTreeSet<_>>();
```

Thread `&observed_literals` through discovered-slot reconciliation and
observed/unobserved classification. Refactor the matcher so it:

1. searches all merged slots for an exact stable-key match;
2. returns that exact match immediately;
3. searches for the longest prefix only among prefixes absent from
   `observed_literals`; and
4. retains configuration order for equal-length prefix ties.

Use the same helper for seeding `observed_existing`, so merge behavior and stale
diagnostics cannot disagree. Keep exact matching available for configured slots
that omit `div_id` and therefore resolve through `id`.

Update the `MergeDiagnostics` field comment from “raw crawl” to “normalized
evidence.”

- [ ] **Step 4: Add and run the normalization-boundary regression**

Use `discover_gpt_slots` plus `merge_slots` to show that a live
`ad-header-0-_R_3f_` identity normalizes to `ad-header-0`, and therefore makes
configured `ad-header-0` an observed literal rather than a prefix for a distinct
`ad-header-01` slot. Do not pass collector-level raw IDs into the merge.

Run:

```bash
cargo test -p trusted-server-cli normalized_stem_is_the_literal_merge_boundary -- --nocapture
```

Expected after implementation: PASS.

- [ ] **Step 5: Run focused merge tests and verify GREEN**

Run:

```bash
cargo test -p trusted-server-cli slot_toml::tests -- --nocapture
```

Expected: all merge tests pass, including the existing intentional broad-prefix
test.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-cli/src/commands/audit/generate/slot_toml.rs
git commit -m "Preserve observed literal ad slot siblings"
```

### Task 2: Refuse eight-digit, long-suffix volatile tokens

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/generate/gpt_slots.rs`

- [ ] **Step 1: Add failing shorter-token registry and request tests**

Add singleton cases using a synthetic shape:

```rust
const SHORT_VOLATILE_DIV: &str =
    "vendor-tag_12345678AbCdEfGhIjKl_slot_overlay_1";
```

Assert both registry and GAMPAD request discovery:

- retain `had_slot_evidence`;
- produce no writable slots; and
- emit the existing volatile-family warning naming `vendor-tag`.

- [ ] **Step 2: Run the tests and verify RED**

Run:

```bash
cargo test -p trusted-server-cli shorter_high_entropy_singleton -- --nocapture
```

Expected: FAIL because the current classifier requires ten leading digits and
accepts the eight-digit token literally.

- [ ] **Step 3: Add failing classifier boundary tests**

Extend the table-driven tests so these remain eligible:

```text
vendor-tag_1234567AbCdEfGh_slot_inarticle_1   # seven leading digits
vendor-tag_12345678AbCdEfG_slot_inarticle_1  # seven-character suffix
promo-20260820a-sidebar                      # short calendar suffix
vendor-tag_1234567890123456_slot_inarticle_1 # bare numeric segment
```

Add `vendor-tag_12345678AbCdEfGh_slot_inarticle_1` to the volatile table. Run
the two boundary tests and confirm only the new 8+8 volatile assertion fails.

- [ ] **Step 4: Implement the conservative alternative token shape**

Keep the current all-ASCII-alphanumeric requirement and compute the suffix
length after the leading digit run. A segment is per-render when either:

```rust
(leading_digits >= 10 && suffix_length >= 1)
    || (leading_digits >= 8 && suffix_length >= 8)
```

Keep the existing requirement that the token occurs before another div-ID
segment. Do not add a vendor name or family-specific regular expression.

- [ ] **Step 5: Run GPT discovery tests and verify GREEN**

Run:

```bash
cargo test -p trusted-server-cli gpt_slots::tests -- --nocapture
```

Expected: all discovery, normalization, collision, registry, request, and
boundary tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/trusted-server-cli/src/commands/audit/generate/gpt_slots.rs
git commit -m "Reject shorter high-entropy ad slot tokens"
```

### Task 3: Verify the complete change

**Files:**

- No source changes expected.

- [ ] **Step 1: Run formatting and diff checks**

```bash
cargo fmt --all -- --check
git diff --check
cd docs && npm run format
```

Expected: all exit zero and formatting makes no changes.

- [ ] **Step 2: Run the complete CLI suite**

```bash
./scripts/test-cli.sh
```

Expected: all unit, config overlay, proxy E2E, and ignored real-Chrome fixtures
pass. The browser portions require permission to bind loopback listeners.

- [ ] **Step 3: Run host-target CLI clippy**

```bash
cargo clippy \
  --manifest-path crates/trusted-server-cli/Cargo.toml \
  --target "$(rustc -vV | sed -n 's/^host: //p')" \
  --all-targets -- -D warnings
```

Expected: the changed CLI crate and all of its test targets lint without
warnings. The adapter-scoped aliases below do not include this crate.

- [ ] **Step 4: Run repository target-specific Rust gates**

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

Expected: every command exits zero with no warnings promoted to errors.

- [ ] **Step 5: Run parity and JavaScript/docs gates**

```bash
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
(cd crates/trusted-server-js/lib && npx vitest run)
(cd crates/trusted-server-js/lib && npm run format)
(cd docs && npm run format)
```

Expected: parity, Vitest, and formatting checks pass.

- [ ] **Step 6: Review branch state**

```bash
git status --short
git log --oneline --decorate -10
```

Expected: clean feature worktree with the two implementation commits above the
approved design/plan commits.

- [ ] **Step 7: Validate against the operator's dry-run output**

Ask the operator to rerun the established desktop/mobile `--scroll --dry-run`
command with a current DataDome cookie. Confirm:

- there is no `ad-sidebar-1` broad-prefix collision note;
- numeric sidebar siblings are emitted as distinct slots;
- the singleton mobile volatile-family slot is refused; and
- older configured volatile-family slots remain named as preserved but
  unobserved until the operator deliberately prunes them.
