# SSAT Debug Comment Configuration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the SSAT debug-comment configuration with three explicit sensitivity modes while making default response-level metadata fail closed by both key and value schema.

**Architecture:** Keep the existing configuration types in `settings.rs` and rendering in `publisher.rs`. Add an `Upstream` enum branch and a separate provider-diagnostic key set. In `Redacted`, reconstruct a small metadata object from validated values instead of copying arbitrary JSON; in `Upstream`, add only six named provider diagnostics; in `Full`, retain the existing raw-metadata behavior.

**Tech Stack:** Rust, serde/serde_json, TOML, existing `trusted-server-core` tests and target-specific Cargo aliases.

**Spec:** `docs/superpowers/specs/2026-07-20-ssat-debug-comment-config-design.md`

---

## File Structure

| File                                                                    | Responsibility                                                                                                                       |
| ----------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `crates/trusted-server-core/src/settings.rs`                            | Public config schema, safe selector keys, upstream diagnostic keys, normalization, config tests                                      |
| `crates/trusted-server-core/src/publisher.rs`                           | Schema validation, safe message generation, three rendering branches, section/size protections, renderer tests                       |
| `trusted-server.example.toml`                                           | Operator-facing mode and privacy documentation                                                                                       |
| `docs/superpowers/specs/2026-07-20-ssat-debug-comment-config-design.md` | Approved security contract; no behavioral edits during implementation unless a discovered contradiction is brought back for approval |

No new production file or dependency is needed. The existing large modules already own these responsibilities, so splitting them during this security fix would add unrelated churn.

---

### Task 1: Lock down the configuration boundary

**Files:**

- Modify: `crates/trusted-server-core/src/settings.rs:1890-2020`
- Test: `crates/trusted-server-core/src/settings.rs:2700-2770`

- [ ] **Step 1: Write failing settings tests**

Update `auction_debug_comment_options_default_matches_serde_defaults` to require exactly the safe selector keys:

```rust
assert_eq!(
    opts.metadata_keys,
    vec![
        "error_type".to_string(),
        "http_status".to_string(),
        "message".to_string(),
    ],
    "should default to only schema-validated response metadata"
);
```

Add direct enum deserialization coverage:

```rust
#[test]
fn auction_debug_comment_options_deserializes_upstream_verbosity() {
    let options: AuctionDebugCommentOptions =
        toml::from_str("verbosity = \"upstream\"")
            .expect("should deserialize upstream verbosity");
    assert_eq!(options.verbosity, AuctionDebugCommentVerbosity::Upstream);
}
```

Change the normalization fixture to safe and unsafe-looking names so it proves normalization only, not authorization:

```rust
let mut opts = AuctionDebugCommentOptions {
    metadata_keys: vec![
        " http_status ".to_string(),
        "".to_string(),
        "debug".to_string(),
    ],
    ..AuctionDebugCommentOptions::default()
};
opts.normalize();
assert_eq!(
    opts.metadata_keys,
    vec!["http_status".to_string(), "debug".to_string()]
);
```

This deliberately leaves `debug` in normalized configuration; authorization must happen at render time.

- [ ] **Step 2: Run the focused tests and confirm RED**

Run:

```bash
cargo test -p trusted-server-core --lib auction_debug_comment_options -- --nocapture
cargo test -p trusted-server-core --lib bad_verbosity_string_fails_config_load -- --nocapture
```

Expected: both commands fail to compile because Rust builds the complete lib test binary before applying the name filter, and the newly added test references the nonexistent `AuctionDebugCommentVerbosity::Upstream`. This is the intended RED state; the existing invalid-verbosity assertion is re-confirmed after the enum compiles in Step 4.

- [ ] **Step 3: Implement the minimal settings change**

Replace the current safe const with:

```rust
pub(crate) const AUCTION_DEBUG_METADATA_ALLOWLIST: &[&str] =
    &["error_type", "http_status", "message"];

pub(crate) const AUCTION_DEBUG_UPSTREAM_METADATA_KEYS: &[&str] = &[
    "errors",
    "warnings",
    "responsetimemillis",
    "bidstatus",
    "upstream_message",
    "upstream_message_truncated",
];
```

Add the enum variant between `Redacted` and `Full`:

```rust
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuctionDebugCommentVerbosity {
    #[default]
    Redacted,
    Upstream,
    Full,
}
```

Update the public docs on `metadata_keys` and `verbosity` to state:

- `metadata_keys` selects only the three safe fields and cannot unlock upstream keys.
- `Upstream` adds six untyped provider diagnostic values and remains creative-truncated.
- `Full` copies all response metadata and does not truncate creatives.
- `Upstream` and `Full` must not be enabled in production.

- [ ] **Step 4: Run focused tests and confirm GREEN**

Run both commands from Step 2.

Expected: all matching settings tests pass, including invalid-verbosity rejection.

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/settings.rs
git commit -m "Harden SSAT debug comment configuration modes"
```

---

### Task 2: Make redacted rendering schema-safe

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs:60-70`
- Modify: `crates/trusted-server-core/src/publisher.rs:1870-2035`
- Test: `crates/trusted-server-core/src/publisher.rs:4250-4565`

- [ ] **Step 1: Add a reusable test renderer with explicit metadata**

Near the existing `dump_comment_for_creative_with_options`, add a helper that builds one `AuctionResponse::error("prebid", 12)`, attaches supplied metadata with the existing `with_metadata` builder, calls `prepend_auction_debug_comment`, and returns the rendered state. Do not add a production convenience API only for tests.

- [ ] **Step 2: Write the failing default and subset tests**

Replace `default_options_reproduce_current_behavior` with `default_options_apply_safe_response_metadata_schema`. Its fixture must contain:

- valid `error_type = "http_status"`
- valid `http_status = 422`
- malicious raw `message`
- all six upstream diagnostic keys containing unique fictional identity-shaped markers
- `debug.resolvedrequest.user.id`

Assert that output contains `error_type`, `http_status`, and the fixed message `Provider returned HTTP 422`; assert that none of the raw message, upstream markers, or `debug` subtree appear.

Add `configured_metadata_subset_only_includes_selected_safe_keys` with `metadata_keys = ["http_status", "errors", "debug"]`. Assert only validated `http_status` survives; `errors` and `debug` cannot be unlocked by selector configuration.

- [ ] **Step 3: Write the failing adversarial schema test**

Add `redacted_mode_rejects_wrong_types_and_unknown_error_classifications`. Render separate cases containing:

```rust
json!({"error_type": {"identity": "example-user-123"}})
json!({"error_type": "provider_supplied_unknown", "message": "example-user-123"})
json!({"http_status": "200 example-user-123"})
json!({"http_status": 99})
json!({"http_status": 600})
json!({"message": {"identity": "example-user-123"}})
```

Assert no identity marker or attacker-provided message appears. Also assert valid integer boundaries `100` and `599` survive, while non-integral JSON numbers do not.

- [ ] **Step 4: Write the failing safe-message mapping test**

Add a table-driven test for the fixed mappings:

```rust
let cases = [
    ("parse_response", None, "Provider response could not be parsed"),
    ("launch_failed", None, "Provider launch failed"),
    ("transport", None, "Provider request failed"),
    ("timeout", None, "Provider request timed out"),
    ("http_status", Some(418), "Provider returned HTTP 418"),
    ("http_status", None, "Provider returned an HTTP error"),
];
```

For every case, include a malicious raw `metadata["message"]` and prove the renderer ignores it.

- [ ] **Step 5: Run the focused publisher tests and confirm RED**

Run:

```bash
cargo test -p trusted-server-core --lib publisher::tests:: -- --nocapture
```

Expected: new privacy tests fail because current redacted rendering clones values by matching key names only.

- [ ] **Step 6: Implement schema validation and safe message generation**

Import both key constants. Add private helpers close to `redact_response_for_dump`:

```rust
fn validated_error_type(
    metadata: &std::collections::HashMap<String, serde_json::Value>,
) -> Option<&str> {
    let value = metadata.get("error_type")?.as_str()?;
    matches!(
        value,
        "parse_response" | "launch_failed" | "transport" | "timeout" | "http_status"
    )
    .then_some(value)
}

fn validated_http_status(
    metadata: &std::collections::HashMap<String, serde_json::Value>,
) -> Option<u64> {
    metadata
        .get("http_status")?
        .as_u64()
        .filter(|status| (100..=599).contains(status))
}

fn safe_error_message(error_type: &str, http_status: Option<u64>) -> Option<String> {
    match error_type {
        "parse_response" => Some("Provider response could not be parsed".to_string()),
        "launch_failed" => Some("Provider launch failed".to_string()),
        "transport" => Some("Provider request failed".to_string()),
        "timeout" => Some("Provider request timed out".to_string()),
        "http_status" => Some(http_status.map_or_else(
            || "Provider returned an HTTP error".to_string(),
            |status| format!("Provider returned HTTP {status}"),
        )),
        _ => None,
    }
}
```

Implement `redacted_metadata_for_dump` so it reconstructs values only when each field is configured and valid. It must never clone `metadata["message"]`:

```rust
fn redacted_metadata_for_dump(
    metadata: &std::collections::HashMap<String, serde_json::Value>,
    options: &AuctionDebugCommentOptions,
) -> serde_json::Map<String, serde_json::Value> {
    let selected = |key: &str| {
        AUCTION_DEBUG_METADATA_ALLOWLIST.contains(&key)
            && options.metadata_keys.iter().any(|candidate| candidate == key)
    };
    let error_type = validated_error_type(metadata);
    let http_status = validated_http_status(metadata);
    let mut safe = serde_json::Map::new();
    if selected("error_type") && let Some(value) = error_type {
        safe.insert("error_type".to_string(), serde_json::json!(value));
    }
    if selected("http_status") && let Some(value) = http_status {
        safe.insert("http_status".to_string(), serde_json::json!(value));
    }
    if selected("message")
        && let Some(value) = error_type.and_then(|kind| safe_error_message(kind, http_status))
    {
        safe.insert("message".to_string(), serde_json::json!(value));
    }
    safe
}
```

- [ ] **Step 7: Add the `Upstream` and `Full` metadata branches**

Build redacted metadata first. For `Upstream`, copy only keys in `AUCTION_DEBUG_UPSTREAM_METADATA_KEYS`; for `Full`, clone the entire map:

```rust
let metadata = match options.verbosity {
    AuctionDebugCommentVerbosity::Redacted => {
        redacted_metadata_for_dump(&response.metadata, options)
    }
    AuctionDebugCommentVerbosity::Upstream => {
        let mut metadata = redacted_metadata_for_dump(&response.metadata, options);
        for key in AUCTION_DEBUG_UPSTREAM_METADATA_KEYS {
            if let Some(value) = response.metadata.get(*key) {
                metadata.insert((*key).to_string(), value.clone());
            }
        }
        metadata
    }
    AuctionDebugCommentVerbosity::Full => response
        .metadata
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect(),
};
```

Change creative handling so truncation applies to every mode except `Full`:

```rust
if options.verbosity != AuctionDebugCommentVerbosity::Full
    && let Some(creative) = &bid.creative
{
    value["creative"] = serde_json::Value::String(truncate_with_marker(
        creative,
        MAX_BID_CREATIVE_DUMP_BYTES,
    ));
}
```

- [ ] **Step 8: Write and run upstream/full safety tests**

Add:

- `upstream_mode_includes_provider_diagnostics_but_not_debug_subtree`
- `verbosity_upstream_still_truncates_creative`
- `metadata_keys_empty_yields_empty_safe_metadata_in_redacted`

The upstream fixture must include all six diagnostics plus a `debug` subtree. Assert all six diagnostics appear, `debug` does not, the configured safe subset still works, and the creative carries the existing truncation marker. Retain existing tests proving `Full` includes `debug`, skips per-creative truncation, and still respects the 256 KiB total cap.

Extend `auction_debug_comment_neutralises_every_comment_terminator_vector` so its existing five attack vectors run once with `Redacted` and once with `Full`. Do not replace this table with the narrower two-vector full-mode test: every vector must prove that verbosity cannot bypass the unconditional HTML-comment safety boundary.

Run:

```bash
cargo test -p trusted-server-core --lib publisher::tests:: -- --nocapture
```

Expected: all publisher tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Enforce SSAT debug response metadata schemas"
```

---

### Task 3: Correct operator-facing configuration documentation

**Files:**

- Modify: `trusted-server.example.toml:160-190`
- Verify against: `docs/superpowers/specs/2026-07-20-ssat-debug-comment-config-design.md`

- [ ] **Step 1: Update the example configuration**

Use exactly the safe default selector:

```toml
[debug]
# NEVER enable in production. Injects an auction dump before </body>.
# "redacted" validates response metadata but still includes bid-level fields
# and creative previews; it is not a fully anonymized dump.
auction_html_comment = false

[debug.auction_html_comment_options]
include_provider_responses = true
include_mediator_response = true
include_bids = true
metadata_keys = ["error_type", "http_status", "message"]
# "redacted" (default), "upstream", or "full".
# "upstream" exposes six untyped provider diagnostic values that may contain
# request or identity data. "full" additionally exposes all response metadata
# and untruncated creatives. Never use either sensitive mode in production.
verbosity = "redacted"
```

Preserve nearby unrelated comments and settings from `main`.

- [ ] **Step 2: Verify example parsing**

Run:

```bash
cargo test-cloudflare
cargo test-spin
```

Expected: both adapter suites pass, including paths that load `trusted-server.example.toml`.

- [ ] **Step 3: Commit**

```bash
git add trusted-server.example.toml
git commit -m "Document SSAT debug sensitivity modes"
```

---

### Task 4: Full verification and review

**Files:** none unless verification exposes a scoped defect.

- [ ] **Step 1: Run formatting checks**

```bash
cargo fmt --all -- --check
cd docs && npm run format
```

Expected: no formatting differences.

- [ ] **Step 2: Run native and adapter tests**

```bash
cargo test -p trusted-server-core --lib
cargo test-axum
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
```

Expected: all pass.

- [ ] **Step 3: Run Fastly tests**

```bash
cargo test-fastly
```

Expected: pass. If Viceroy fails before executing tests because macOS native certificate/keychain access is unavailable, record that exact environment blocker and separately run `cargo check-fastly`; do not describe Fastly tests as passing.

- [ ] **Step 4: Run every target-specific lint alias**

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

Expected: no warnings or errors.

- [ ] **Step 5: Run documentation and JavaScript gates required by repository CI**

Use the repository-pinned Node version (`.tool-versions`, currently Node 24.12.0) for JavaScript commands:

```bash
cd crates/trusted-server-js/lib && npm run format
cd crates/trusted-server-js/lib && npx vitest run
cd crates/trusted-server-js/lib && node build-all.mjs
```

Expected: all pass. If the current shell uses another Node major, invoke the pinned Node binary explicitly rather than changing lockfiles.

- [ ] **Step 6: Inspect the final diff and request independent code review**

```bash
git diff origin/main...HEAD --check
git status --short
```

Verify:

- no conflict markers
- no unrelated changes
- safe selector contains only `error_type`, `http_status`, `message`
- every safe value is validated or generated
- upstream includes exactly six named diagnostic keys
- full remains the only mode that copies arbitrary response metadata
- upstream and redacted both truncate creatives
- byte cap and comment neutralization remain unconditional

Then invoke `superpowers:requesting-code-review`. Fix any Critical or Important findings and rerun the affected gates before claiming completion.

---

## Implementation Constraints

- Follow test-driven development: observe each new security test fail before changing its production path.
- Do not alter bid-level `metadata`, `nurl`, `burl`, or creative contents beyond the existing preview truncation; issue #925 owns that separate boundary.
- Do not add provider instrumentation or change Prebid response parsing.
- Do not make either size cap configurable.
- Do not silently downgrade invalid verbosity strings; serde must reject them.
- Do not push the branch until the user has received the final verification and review results.
