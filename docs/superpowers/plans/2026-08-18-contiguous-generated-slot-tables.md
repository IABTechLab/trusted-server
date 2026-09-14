# Contiguous Generated Slot Tables Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep generated creative-opportunity slot and provider tables contiguous with their parent section.

**Architecture:** Normalize the document positions carried by generated `toml_edit` tables before inserting them into the target document. Anchor the whole generated subtree at the target creative section and rely on stable serialization order.

**Tech Stack:** Rust, `toml_edit`, Cargo tests

---

### Task 1: Reproduce the position collision

**Files:**

- Modify/Test: `crates/trusted-server-cli/src/commands/audit/generate/slot_toml.rs`

- [ ] Add `splice_keeps_generated_slots_and_providers_contiguous` with a late creative section and unrelated tables at colliding positions.
- [ ] Assert no unrelated table header occurs between `[creative_opportunities]`, all generated slots, and their provider subtables.
- [ ] Add `splice_groups_a_new_creative_section_with_its_slots` for an input that has no creative section, proving the newly created parent and generated subtree share the final anchor.
- [ ] Run each focused test with `cargo test_cli_macos <fully-qualified-test-name> -- --exact` and confirm both ordering assertions fail.

### Task 2: Normalize imported table positions

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/generate/slot_toml.rs`

- [ ] Add a small recursive helper using `Table::set_position`, `Table::iter_mut`, and `ArrayOfTables::iter_mut` to assign one anchor position to every table in the generated slot subtree.
- [ ] Use the existing creative table's position; for a newly created section, allocate one greater than the greatest parsed position and explicitly assign that anchor to both the new parent and its generated subtree.
- [ ] Run `cargo test_cli_macos commands::audit::generate::slot_toml::tests::splice_keeps_generated_slots_and_providers_contiguous -- --exact` and confirm it passes.
- [ ] Run `cargo test_cli_macos commands::audit::generate::slot_toml::tests::splice_groups_a_new_creative_section_with_its_slots -- --exact` and confirm it passes.
- [ ] Run `cargo test_cli_macos commands::audit::generate::slot_toml::tests` and confirm the complete module suite passes.

### Task 3: Verify and deliver

**Files:**

- Modify: `crates/trusted-server-cli/src/commands/audit/generate/slot_toml.rs`

- [ ] Run `./scripts/test-cli.sh`.
- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo clippy --package trusted-server-cli --target aarch64-apple-darwin --all-targets -- -D warnings`.
- [ ] Confirm `trusted-server.toml` and the user's existing `fastly.toml` change remain untouched.
- [ ] Commit the verified generator fix on the current feature branch.
