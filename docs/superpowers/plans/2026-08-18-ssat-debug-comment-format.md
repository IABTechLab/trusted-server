# SSAT Debug Comment Output Format Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a backward-compatible `compact`/`pretty` configuration option for the outer JSON in SSAT auction debug comments.

**Architecture:** Extend `AuctionDebugCommentOptions` with a serde-backed format enum that defaults to compact. Keep the existing dump value and safety pipeline unchanged, selecting only `serde_json::to_string` versus `serde_json::to_string_pretty` before terminator neutralization and the 256 KiB cap.

**Tech Stack:** Rust 2024, serde, serde_json, TOML configuration, existing `trusted-server-core` unit tests.

---

## File Map

- Modify `crates/trusted-server-core/src/settings.rs`: define the output-format enum, add the option and default, and test TOML behavior.
- Modify `crates/trusted-server-core/src/publisher.rs`: select compact or pretty JSON serialization and test rendering/safety invariants.
- Modify `trusted-server.example.toml`: document the new setting and accepted values.

### Task 1: Add the configuration type

**Files:**
- Modify: `crates/trusted-server-core/src/settings.rs:1945-2030`
- Test: `crates/trusted-server-core/src/settings.rs:2710-2780`

- [ ] **Step 1: Write failing settings tests**

Extend `auction_debug_comment_options_default_matches_serde_defaults` with:

```rust
assert_eq!(
    opts.format,
    AuctionDebugCommentFormat::Compact,
    "should default to compact output"
);
```

Add focused tests:

```rust
#[test]
fn auction_debug_comment_options_deserializes_pretty_format() {
    let options: AuctionDebugCommentOptions = toml::from_str(r#"format = "pretty""#)
        .expect("should deserialize pretty format");
    assert_eq!(options.format, AuctionDebugCommentFormat::Pretty);
}

#[test]
fn auction_debug_comment_options_bad_format_fails_config_load() {
    let result: Result<AuctionDebugCommentOptions, _> =
        toml::from_str(r#"format = "expanded""#);
    assert!(
        result.is_err(),
        "unrecognized format must fail to deserialize, not silently fall back"
    );
}
```

- [ ] **Step 2: Run the tests and verify RED**

Run:

```bash
cargo test-fastly auction_debug_comment_options -- --nocapture
```

Expected: compilation fails because `AuctionDebugCommentFormat` and `format` do not exist.

- [ ] **Step 3: Implement the minimal configuration surface**

Add `format` after `verbosity` in `AuctionDebugCommentOptions`:

```rust
/// JSON representation used for the outer auction dump.
#[serde(default)]
pub format: AuctionDebugCommentFormat,
```

Set `format: AuctionDebugCommentFormat::Compact` in the hand-written default and define:

```rust
/// JSON representation used for the outer `ts-debug` auction dump.
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuctionDebugCommentFormat {
    #[default]
    Compact,
    Pretty,
}
```

- [ ] **Step 4: Run the focused tests and verify GREEN**

Run `cargo test-fastly auction_debug_comment_options -- --nocapture`.

Expected: all matching settings tests pass.

- [ ] **Step 5: Commit the configuration change**

```bash
git add crates/trusted-server-core/src/settings.rs
git commit -m "Configure SSAT debug comment output format"
```

### Task 2: Render pretty outer JSON without transforming values

**Files:**
- Modify: `crates/trusted-server-core/src/publisher.rs:2027-2130`
- Test: `crates/trusted-server-core/src/publisher.rs:4330-4945`

- [ ] **Step 1: Add a test helper for extracting an uncapped dump**

Extract the existing parsing logic into `dump_json_from_comment`, returning the dump substring and parsed `serde_json::Value`. It must split after `dump=` and before the comment's final newline/terminator so it works for both one-line and indented JSON.

- [ ] **Step 2: Write failing rendering tests**

Add tests proving:

1. Default compact output still contains `dump={"provider_responses":` and no newline immediately after the opening object.
2. Pretty output contains `dump={\n  "provider_responses":`.
3. Compact and pretty uncapped dumps deserialize to equal JSON values.
4. In Full mode, metadata containing `{"requestbody": "{\"id\":\"request-1\"}"}` retains `requestbody` as a JSON string in pretty output.

Extend the existing comment-terminator test to iterate over both
`AuctionDebugCommentFormat::{Compact, Pretty}` for the tested verbosity modes.
Add a pretty/full case to the total-cap test and assert the existing
`(truncated` marker remains present.

- [ ] **Step 3: Run the rendering tests and verify RED**

Run:

```bash
cargo test-fastly auction_debug_comment -- --nocapture
```

Expected: the pretty-layout assertion fails because rendering still always uses compact serialization.

- [ ] **Step 4: Implement format-selected serialization**

Import `AuctionDebugCommentFormat` alongside the existing options types. Replace the single serializer call with:

```rust
let serialized = match options.format {
    AuctionDebugCommentFormat::Compact => serde_json::to_string(&dump),
    AuctionDebugCommentFormat::Pretty => serde_json::to_string_pretty(&dump),
};
let dump = render_dump(
    serialized.unwrap_or_else(|error| format!("<dump serialize error: {error}>")),
);
```

Do not alter the dump value, nested strings, neutralization, cap, or comment envelope.

- [ ] **Step 5: Run the focused tests and verify GREEN**

Run `cargo test-fastly auction_debug_comment -- --nocapture`.

Expected: all matching rendering tests pass.

- [ ] **Step 6: Commit the renderer change**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Pretty print SSAT debug comment dumps"
```

### Task 3: Document and verify the completed feature

**Files:**
- Modify: `trusted-server.example.toml:180-190`

- [ ] **Step 1: Document the setting**

Add to `[debug.auction_html_comment_options]`:

```toml
# "compact" (default) or "pretty". Pretty formats only the outer dump;
# JSON request/response bodies remain strings exactly as captured.
format = "compact"
```

- [ ] **Step 2: Run formatting**

Run `cargo fmt --all` and then `cargo fmt --all -- --check`.

Expected: both exit successfully.

- [ ] **Step 3: Run required tests**

Run each command separately:

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

Expected: every command exits successfully with zero failed tests.

- [ ] **Step 4: Run target-matched lint checks**

Run each command separately:

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

Expected: every command exits successfully with warnings denied.

- [ ] **Step 5: Inspect the final diff and configuration compatibility**

Run `git diff --check` and inspect `git diff origin/main...HEAD` plus the remaining working-tree diff. Confirm compact is the serde/default value, pretty changes whitespace only, nested strings are preserved, and both safety protections remain unconditional.

- [ ] **Step 6: Commit documentation and any formatting changes**

```bash
git add crates/trusted-server-core/src/settings.rs crates/trusted-server-core/src/publisher.rs trusted-server.example.toml
git commit -m "Document SSAT debug comment formatting"
```

- [ ] **Step 7: Push the branch and update PR #943**

Run `git push origin feat/ssat-debug-comment-config`, then verify PR state and checks with `gh pr view 943` and `gh pr checks 943`.
