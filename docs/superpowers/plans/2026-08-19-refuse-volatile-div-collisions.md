# Refuse Volatile Div-ID Collisions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prevent `ts audit ad-templates generate --replace` from writing exact per-render div IDs when several live elements normalize to one runtime prefix.

**Architecture:** Keep collision detection in GPT discovery, where normalized and raw IDs are both available. On the first distinct collision, remove the tentatively accepted normalized slot and mark the group ambiguous; suppress all later members and emit one actionable diagnostic. Carry a separate evidence-present bit into `EvidenceTable` so collision-only pages are not classified as bot challenges.

**Tech Stack:** Rust, Chromium GPT evidence model, built-in Rust test framework.

---

### Task 1: Specify refusal behavior

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/generate/gpt_slots.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/evidence.rs`

- [ ] Add assertions for documented `DiscoveredSlots::had_slot_evidence` and run one focused test to observe the expected missing-field compile failure.
- [ ] Add only the documented field with its derived/default false value so behavioral tests can compile; do not wire discovery or classification yet.
- [ ] Change the same-page collision test to require zero emitted slots and one refusal diagnostic.
- [ ] Assert the diagnostic names `ad-in_content`, explains that a broad prefix resolves only one element and raw IDs are volatile, and tells the operator to expose distinct stable IDs.
- [ ] Rename the test to `same_page_hex_normalization_collision_is_refused`.
- [ ] Extend `repeated_raw_div_after_a_normalization_collision_is_deduplicated` with repeats of both initial raw IDs and a third distinct ID; require zero slots and one diagnostic.
- [ ] Add `request_normalization_collision_is_refused`; require zero slots, one diagnostic with the same prefix/safety/action content, true evidence, and a surviving request-derived network ID.
- [ ] Add `ambiguous_registry_stem_still_suppresses_request_fallback` and require no slot resurrection.
- [ ] Require every registry/request collision test to assert `had_slot_evidence` is true.
- [ ] Add `collision_only_page_is_not_classified_as_empty` using a discovered collision result.
- [ ] Run `cargo test --package trusted-server-cli --target aarch64-apple-darwin same_page_hex_normalization_collision_is_refused` and confirm RED because two raw slots remain.
- [ ] Run `cargo test --package trusted-server-cli --target aarch64-apple-darwin repeated_raw_div_after_a_normalization_collision_is_deduplicated` and confirm RED because raw slots remain.
- [ ] Run `cargo test --package trusted-server-cli --target aarch64-apple-darwin request_normalization_collision_is_refused` and confirm RED because request-derived raw slots remain.
- [ ] Run `cargo test --package trusted-server-cli --target aarch64-apple-darwin ambiguous_registry_stem_still_suppresses_request_fallback` and confirm RED because the ambiguous registry group remains deployable.
- [ ] Run `cargo test --package trusted-server-cli --target aarch64-apple-darwin collision_only_page_is_not_classified_as_empty` and confirm RED because collision-only evidence is classified as empty.

### Task 2: Refuse ambiguous collision groups

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/generate/gpt_slots.rs`
- Modify: `crates/trusted-server-cli/src/commands/audit/generate/evidence.rs`

- [ ] Replace raw-ID preservation with a collision result that distinguishes first ambiguity from later members.
- [ ] Remove the initially accepted normalized slot when ambiguity is first proven.
- [ ] Suppress the colliding and subsequent raw members.
- [ ] Emit one message naming the prefix, both unsafe representations, and the publisher-markup action.
- [ ] Set `had_slot_evidence` for any otherwise usable registry/request candidate and use it in empty-page classification.
- [ ] Run `cargo test --package trusted-server-cli --target aarch64-apple-darwin normalization_collision` and confirm the registry and request collision tests GREEN.
- [ ] Run `cargo test --package trusted-server-cli --target aarch64-apple-darwin ambiguous_registry_stem_still_suppresses_request_fallback` and confirm registry precedence GREEN.
- [ ] Run `cargo test --package trusted-server-cli --target aarch64-apple-darwin collision_only_page_is_not_classified_as_empty` and confirm GREEN.

### Task 3: Verify and deliver

**Files:**

- Verify: `crates/trusted-server-cli/src/commands/audit/generate/gpt_slots.rs`
- Verify: `crates/trusted-server-cli/src/commands/audit/generate/evidence.rs`
- Verify: `docs/superpowers/specs/2026-08-19-refuse-volatile-div-collisions-design.md`
- Verify: `docs/superpowers/plans/2026-08-19-refuse-volatile-div-collisions.md`

- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `./scripts/test-cli.sh aarch64-apple-darwin`.
- [ ] Run `cargo clippy --package trusted-server-cli --target aarch64-apple-darwin --all-targets --all-features -- -D warnings`.
- [ ] Run `cd docs && npm run format`.
- [ ] Run `git diff --check` and inspect the scoped diff.
- [ ] Commit and push the fix to `feature/ts-cli-ad-templates`.

### Task 4: Refuse a known single-observation volatile family

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/generate/gpt_slots.rs`

- [ ] Add failing registry and request tests for a single `<family>_<render-token>_<placement>` observation.
- [ ] Add a recognizer keyed on the token shape — eight or more leading digits followed by more alphanumerics — in any position that still has placement content after it.
- [ ] Omit matching slots while preserving evidence/network discovery and emit one deduplicated actionable diagnostic naming the family prefix.
- [ ] Add negative tests proving IDs with no token, a bare digit run, or a trailing token remain eligible.
- [ ] Run the focused tests, then repeat Task 3 verification and delivery.
