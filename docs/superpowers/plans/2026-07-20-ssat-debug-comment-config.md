# SSAT Debug Comment Configuration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the SSAT `<!-- ts-debug: ... -->` auction dump configurable — section toggles, a metadata-key subset, and an opt-in `Full` verbosity that surfaces raw per-bidder request/response data — while keeping the existing fail-closed redaction unconditional.

**Architecture:** One new config struct (`AuctionDebugCommentOptions`) and one new enum (`AuctionDebugCommentVerbosity`) in `settings.rs`, threaded as a parameter through the three existing render functions in `publisher.rs`. No new files, no new crates.

**Tech Stack:** Rust, serde, existing `trusted-server-core` auction/settings modules.

**Spec:** `docs/superpowers/specs/2026-07-20-ssat-debug-comment-config-design.md` — read it first for the full rationale (security invariants, non-goals, edge cases). This plan implements it; it doesn't re-derive it.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/trusted-server-core/src/settings.rs` | `AuctionDebugCommentOptions`, `AuctionDebugCommentVerbosity`, `AUCTION_DEBUG_METADATA_ALLOWLIST`, wiring into `DebugConfig` and `finalize_deserialized` |
| `crates/trusted-server-core/src/publisher.rs` | `redact_response_for_dump`, `redact_bid_for_dump`, `prepend_auction_debug_comment` — all three gain an `options` parameter; production + test call sites updated; new tests |
| `trusted-server.example.toml` | Document the new `[debug.auction_html_comment_options]` table |

No file split needed — both touched files stay well under the codebase's existing size (settings.rs and publisher.rs are already large multi-struct files; this adds one cohesive struct + enum to each, following the file's existing pattern of many sibling config structs).

---

### Task 1: Config struct in settings.rs

**Files:**
- Modify: `crates/trusted-server-core/src/settings.rs:1894-1924` (the `DebugConfig` block)
- Modify: `crates/trusted-server-core/src/settings.rs:2038-2059` (`finalize_deserialized`)
- Test: same file, `#[cfg(test)] mod tests` block (search for an existing `mod tests` near the bottom of settings.rs to append into)

- [ ] **Step 1: Write the failing test for `AuctionDebugCommentOptions::default()`**

Add to the settings.rs test module:

```rust
#[test]
fn auction_debug_comment_options_default_matches_serde_defaults() {
    let opts = AuctionDebugCommentOptions::default();
    assert!(opts.include_provider_responses, "should default to true");
    assert!(opts.include_mediator_response, "should default to true");
    assert!(opts.include_bids, "should default to true");
    assert_eq!(
        opts.metadata_keys,
        AUCTION_DEBUG_METADATA_ALLOWLIST
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
        "should default metadata_keys to the full allowlist"
    );
    assert_eq!(
        opts.verbosity,
        AuctionDebugCommentVerbosity::Redacted,
        "should default to Redacted"
    );
}

#[test]
fn auction_debug_comment_options_normalize_trims_and_drops_empty_keys() {
    let mut opts = AuctionDebugCommentOptions {
        metadata_keys: vec![" status ".to_string(), "".to_string(), "warnings".to_string()],
        ..AuctionDebugCommentOptions::default()
    };
    opts.normalize();
    assert_eq!(opts.metadata_keys, vec!["status".to_string(), "warnings".to_string()]);
}

#[test]
fn bad_verbosity_string_fails_config_load() {
    // Deserialize AuctionDebugCommentOptions directly, not a full Settings —
    // Settings has required fields with no #[serde(default)] (e.g.
    // `publisher`), so a full-Settings fixture missing them would fail with
    // "missing field `publisher`" regardless of whether `verbosity` itself
    // deserialized correctly, testing the wrong thing.
    let result: Result<AuctionDebugCommentOptions, _> =
        toml::from_str(r#"verbosity = "everything""#);
    assert!(result.is_err(), "unrecognized verbosity must fail to deserialize, not silently fall back");
}
```

Run: `cargo test -p trusted-server-core auction_debug_comment_options -- --nocapture`
Expected: FAIL to compile — `AuctionDebugCommentOptions`, `AuctionDebugCommentVerbosity`, `AUCTION_DEBUG_METADATA_ALLOWLIST` don't exist yet.

(Use `cargo test -p trusted-server-core`, not `cargo test-axum` — the latter is an alias for `cargo test -p trusted-server-adapter-axum` only per `.cargo/config.toml` and will NOT build or run `trusted-server-core`'s own `#[cfg(test)]` modules, silently reporting "0 passed; 0 failed" instead of actually exercising these tests. This applies to every test command in Task 1 and Task 3 below.)

- [ ] **Step 2: Implement the struct, enum, and allowlist constant**

Insert into `settings.rs`, near the existing `DebugConfig` (around line 1894), replacing the old bool-only struct:

```rust
/// Metadata keys safe to surface in the `ts-debug` auction comment.
///
/// Fail-closed superset: any key not listed here — notably `debug`, which
/// carries the resolved OpenRTB request (EC ID, `user.ext.eids`, the TC
/// consent string, `device.ip`, `device.geo`) plus per-bidder `httpcalls` —
/// is dropped in [`AuctionDebugCommentVerbosity::Redacted`] mode regardless
/// of what an operator lists in
/// [`AuctionDebugCommentOptions::metadata_keys`]. `metadata_keys` is a subset
/// selector against this const, never a way to add new keys.
pub(crate) const AUCTION_DEBUG_METADATA_ALLOWLIST: &[&str] = &[
    "error_type",
    "http_status",
    "message",
    "upstream_message",
    "upstream_message_truncated",
    "responsetimemillis",
    "errors",
    "warnings",
    "bidstatus",
];

fn default_true() -> bool {
    true
}

fn default_auction_debug_metadata_keys() -> Vec<String> {
    AUCTION_DEBUG_METADATA_ALLOWLIST
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Behavior of the `<!-- ts-debug: ... -->` auction dump. Only consulted when
/// [`DebugConfig::auction_html_comment`] is true.
///
/// `deny_unknown_fields` matches the convention used by 17 of the ~19
/// sibling config structs in this file, including the `DebugConfig` this
/// struct nests under: an operator typo (e.g. `metadata_key` instead of
/// `metadata_keys`) must fail config load loudly, not be silently ignored.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuctionDebugCommentOptions {
    /// Include the `provider_responses` section at all.
    #[serde(default = "default_true")]
    pub include_provider_responses: bool,

    /// Include `mediator_response` when a mediator ran.
    #[serde(default = "default_true")]
    pub include_mediator_response: bool,

    /// Include each provider's `bids` array (vs. status/metadata only).
    #[serde(default = "default_true")]
    pub include_bids: bool,

    /// Subset of [`AUCTION_DEBUG_METADATA_ALLOWLIST`] to surface in
    /// [`AuctionDebugCommentVerbosity::Redacted`] mode. Keys outside the
    /// fixed allowlist are always dropped, config or not. Ignored when
    /// `verbosity` is `Full`.
    #[serde(default = "default_auction_debug_metadata_keys")]
    pub metadata_keys: Vec<String>,

    /// `Redacted` (default): `metadata_keys` subset only, creative preview
    /// truncated to `MAX_BID_CREATIVE_DUMP_BYTES`.
    /// `Full`: raw `response.metadata` verbatim, including the `debug`
    /// subtree (httpcalls/resolvedrequest) when present, and no creative
    /// truncation. The total dump byte cap and comment-terminator
    /// neutralization still apply unconditionally.
    ///
    /// NEVER enable `Full` in production — identity-bearing request/response
    /// data becomes visible to any visitor via view-source.
    #[serde(default)]
    pub verbosity: AuctionDebugCommentVerbosity,
}

impl Default for AuctionDebugCommentOptions {
    fn default() -> Self {
        Self {
            include_provider_responses: true,
            include_mediator_response: true,
            include_bids: true,
            metadata_keys: default_auction_debug_metadata_keys(),
            verbosity: AuctionDebugCommentVerbosity::Redacted,
        }
    }
}

impl AuctionDebugCommentOptions {
    pub(crate) fn normalize(&mut self) {
        self.metadata_keys = self
            .metadata_keys
            .drain(..)
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
            .collect();
    }
}

/// Verbosity of the `ts-debug` auction comment. See
/// [`AuctionDebugCommentOptions::verbosity`].
#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuctionDebugCommentVerbosity {
    #[default]
    Redacted,
    Full,
}
```

Then update `DebugConfig` itself (replacing the doc comment on `auction_html_comment` isn't needed — it's unchanged — just add the new field after `auction_html_comment`):

```rust
    #[serde(default)]
    pub auction_html_comment: bool,

    /// Content and verbosity of the `auction_html_comment` dump. Ignored
    /// when `auction_html_comment` is false.
    #[serde(default)]
    pub auction_html_comment_options: AuctionDebugCommentOptions,
```

- [ ] **Step 3: Wire normalize() into finalize_deserialized**

In `settings.rs:2038-2059`, add one line after the existing normalize calls:

```rust
    pub(crate) fn finalize_deserialized(
        mut settings: Self,
        validation_label: &str,
    ) -> Result<Self, Report<TrustedServerError>> {
        settings.integrations.normalize();
        settings.proxy.normalize();
        settings.image_optimizer.normalize();
        settings.debug.auction_html_comment_options.normalize();
        settings.consent.validate();
```

- [ ] **Step 4: Run tests, verify pass**

Run: `cargo test -p trusted-server-core auction_debug_comment_options -- --nocapture`
Expected: PASS (all 3 tests from Step 1)

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/settings.rs
git commit -m "Add configurable options struct for the SSAT debug comment"
```

---

### Task 2: Thread options through publisher.rs redaction functions

**Files:**
- Modify: `crates/trusted-server-core/src/publisher.rs:870-936` (allowlist const removal, `redact_response_for_dump`, `redact_bid_for_dump`)
- Modify: `crates/trusted-server-core/src/publisher.rs:950-1036` (`prepend_auction_debug_comment`)
- Modify: `crates/trusted-server-core/src/publisher.rs:1394-1396` (production call site)
- Modify: `crates/trusted-server-core/src/publisher.rs:2638` and `:2699` (existing test call sites)

- [ ] **Step 1: Write failing tests for the new behavior**

Add to the `publisher.rs` test module, near the existing `auction_debug_comment_*` tests (~line 2648 onward). These replace the fixed `dump_comment_for_creative` helper's implicit "always default options" behavior with an explicit parameter, so update the helper first:

```rust
/// Build the ts-debug comment for a one-bid auction whose creative is
/// `creative`, so tests can assert on the rendered dump.
fn dump_comment_for_creative_with_options(
    creative: &str,
    options: &AuctionDebugCommentOptions,
) -> String {
    let mut bid = make_test_bid_with_creative(creative);
    bid.slot_id = "ad-header-0".to_string();
    let result = OrchestrationResult {
        provider_responses: vec![
            AuctionResponse::no_bid("prebid", 665),
            AuctionResponse::success("aps", vec![bid], 42),
        ],
        mediator_response: None,
        winning_bids: std::collections::HashMap::new(),
        total_time_ms: 665,
        metadata: std::collections::HashMap::new(),
    };
    let state = Arc::new(Mutex::new(Some("BIDS_SCRIPT".to_string())));
    prepend_auction_debug_comment("stream", &result, &state, options);
    let comment = state
        .lock()
        .expect("should lock state")
        .clone()
        .expect("should have comment");
    drop(state);
    comment
}

fn dump_comment_for_creative(creative: &str) -> String {
    dump_comment_for_creative_with_options(creative, &AuctionDebugCommentOptions::default())
}

#[test]
fn default_options_reproduce_current_behavior() {
    // Identical to the pre-existing fixed output except: the unused `status`
    // key (never written by any production path) is gone, and http_status /
    // upstream_message / upstream_message_truncated are now allowlisted.
    let comment = dump_comment_for_creative("<div>plain</div>");
    assert!(comment.contains("\"status\":\"nobid\""));
    assert!(comment.contains("dump={\"provider_responses\":"));
    assert!(!comment.contains("mediator_response"));
}

#[test]
fn metadata_keys_empty_yields_empty_metadata_object() {
    let options = AuctionDebugCommentOptions {
        metadata_keys: vec![],
        ..AuctionDebugCommentOptions::default()
    };
    let comment = dump_comment_for_creative_with_options("<div>x</div>", &options);
    assert!(
        comment.contains("\"metadata\":{}"),
        "empty metadata_keys should yield an empty metadata object: {comment}"
    );
}

#[test]
fn metadata_keys_attack_vector_debug_key_never_surfaces_in_redacted_mode() {
    // Configuring "debug" in metadata_keys must have zero effect in Redacted
    // mode — the allowlist intersection is the actual security boundary, not
    // the config value. This is the load-bearing test for this whole design.
    let response = AuctionResponse::error("prebid", 12).with_metadata(
        "debug",
        serde_json::json!({"resolvedrequest": {"user": {"id": "EC-ID-abc123"}}}),
    );
    let result = OrchestrationResult {
        provider_responses: vec![response],
        mediator_response: None,
        winning_bids: std::collections::HashMap::new(),
        total_time_ms: 12,
        metadata: std::collections::HashMap::new(),
    };
    let options = AuctionDebugCommentOptions {
        metadata_keys: vec!["debug".to_string()],
        ..AuctionDebugCommentOptions::default()
    };
    let state = Arc::new(Mutex::new(Some("BIDS_SCRIPT".to_string())));
    prepend_auction_debug_comment("stream", &result, &state, &options);
    let comment = state.lock().expect("should lock state").clone().expect("should have comment");
    assert!(
        !comment.contains("EC-ID-abc123"),
        "debug key must never surface in Redacted mode even if configured: {comment}"
    );
}

#[test]
fn verbosity_full_includes_raw_debug_subtree_when_present() {
    let response = AuctionResponse::error("prebid", 12).with_metadata(
        "debug",
        serde_json::json!({"httpcalls": {"aps": [{"status": 200}]}}),
    );
    let result = OrchestrationResult {
        provider_responses: vec![response],
        mediator_response: None,
        winning_bids: std::collections::HashMap::new(),
        total_time_ms: 12,
        metadata: std::collections::HashMap::new(),
    };
    let options = AuctionDebugCommentOptions {
        verbosity: AuctionDebugCommentVerbosity::Full,
        ..AuctionDebugCommentOptions::default()
    };
    let state = Arc::new(Mutex::new(Some("BIDS_SCRIPT".to_string())));
    prepend_auction_debug_comment("stream", &result, &state, &options);
    let comment = state.lock().expect("should lock state").clone().expect("should have comment");
    assert!(
        comment.contains("httpcalls"),
        "Full verbosity should surface the raw debug subtree: {comment}"
    );
}

#[test]
fn verbosity_full_skips_creative_truncation() {
    let big_creative = "y".repeat(MAX_BID_CREATIVE_DUMP_BYTES * 2);
    let options = AuctionDebugCommentOptions {
        verbosity: AuctionDebugCommentVerbosity::Full,
        ..AuctionDebugCommentOptions::default()
    };
    let comment = dump_comment_for_creative_with_options(&big_creative, &options);
    assert!(
        comment.contains(&big_creative),
        "Full verbosity should not truncate the creative preview"
    );
}

#[test]
fn verbosity_full_still_hits_overall_byte_cap() {
    let huge_creative = "z".repeat(MAX_AUCTION_DEBUG_DUMP_BYTES * 2);
    let options = AuctionDebugCommentOptions {
        verbosity: AuctionDebugCommentVerbosity::Full,
        ..AuctionDebugCommentOptions::default()
    };
    let comment = dump_comment_for_creative_with_options(&huge_creative, &options);
    assert!(
        comment.contains("(truncated"),
        "even Full verbosity must respect the total dump byte cap: {}",
        &comment[..comment.len().min(200)]
    );
}

#[test]
fn include_provider_responses_false_omits_section_entirely() {
    let options = AuctionDebugCommentOptions {
        include_provider_responses: false,
        ..AuctionDebugCommentOptions::default()
    };
    let comment = dump_comment_for_creative_with_options("<div>x</div>", &options);
    assert!(!comment.contains("provider_responses"));
}

#[test]
fn include_mediator_response_false_omits_even_when_mediator_ran() {
    let response = AuctionResponse::success("aps", vec![], 10);
    let mediator = AuctionResponse::success("mediator", vec![], 5);
    let result = OrchestrationResult {
        provider_responses: vec![response],
        mediator_response: Some(mediator),
        winning_bids: std::collections::HashMap::new(),
        total_time_ms: 10,
        metadata: std::collections::HashMap::new(),
    };
    let options = AuctionDebugCommentOptions {
        include_mediator_response: false,
        ..AuctionDebugCommentOptions::default()
    };
    let state = Arc::new(Mutex::new(Some("BIDS_SCRIPT".to_string())));
    prepend_auction_debug_comment("stream", &result, &state, &options);
    let comment = state.lock().expect("should lock state").clone().expect("should have comment");
    assert!(!comment.contains("mediator_response"));
}

#[test]
fn include_bids_false_yields_empty_bids_array_not_omitted_response() {
    let options = AuctionDebugCommentOptions {
        include_bids: false,
        ..AuctionDebugCommentOptions::default()
    };
    let comment = dump_comment_for_creative_with_options("<div>x</div>", &options);
    assert!(comment.contains("\"bids\":[]"));
    // The provider entry itself (status/provider name) must still be present.
    assert!(comment.contains("\"provider\":\"aps\""));
}

#[test]
fn verbosity_full_still_neutralises_comment_terminators() {
    let options = AuctionDebugCommentOptions {
        verbosity: AuctionDebugCommentVerbosity::Full,
        ..AuctionDebugCommentOptions::default()
    };
    for creative in ["<div>evil-->break</div>", "--!><img src=x onerror=alert(1)>"] {
        let comment = dump_comment_for_creative_with_options(creative, &options);
        assert_eq!(comment.matches("-->").count(), 1);
        assert!(!comment.contains("--!>"));
    }
}
```

Also update the two pre-existing tests that call `prepend_auction_debug_comment` directly with the old 3-arg signature — `auction_debug_comment_dumps_provider_status` (uses the helper, already covered by the helper update above) and `auction_debug_comment_never_leaks_provider_debug_metadata` (~line 2699, calls the function directly):

```rust
        let state = Arc::new(Mutex::new(Some("BIDS_SCRIPT".to_string())));
        prepend_auction_debug_comment("stream", &result, &state, &AuctionDebugCommentOptions::default());
```

Run: `cargo test-axum -p trusted-server-core --lib publisher:: -- --nocapture`
Expected: FAIL to compile — `prepend_auction_debug_comment` doesn't take a 4th argument yet; `redact_response_for_dump`/`redact_bid_for_dump` don't take `options` yet.

- [ ] **Step 2: Remove the old local allowlist const, import the new one**

Delete from `publisher.rs` (lines 870-886, the old `DEBUG_DUMP_METADATA_ALLOWLIST` const and its doc comment) and add near the top of the file's imports:

```rust
use crate::settings::AUCTION_DEBUG_METADATA_ALLOWLIST;
use crate::settings::{AuctionDebugCommentOptions, AuctionDebugCommentVerbosity};
```

- [ ] **Step 3: Update redact_bid_for_dump and redact_response_for_dump**

Replace the two functions (publisher.rs ~905-936):

```rust
/// Build a redacted JSON view of a single provider response for the
/// `ts-debug` dump. In [`AuctionDebugCommentVerbosity::Redacted`], only keys
/// in `options.metadata_keys ∩ AUCTION_DEBUG_METADATA_ALLOWLIST` survive and
/// each bid's creative is previewed to [`MAX_BID_CREATIVE_DUMP_BYTES`]. In
/// [`AuctionDebugCommentVerbosity::Full`], metadata and creatives pass
/// through unfiltered.
fn redact_response_for_dump(
    response: &crate::auction::types::AuctionResponse,
    options: &AuctionDebugCommentOptions,
) -> serde_json::Value {
    let metadata: serde_json::Map<String, serde_json::Value> = match options.verbosity {
        AuctionDebugCommentVerbosity::Redacted => response
            .metadata
            .iter()
            .filter(|(key, _)| {
                options.metadata_keys.iter().any(|configured| configured == *key)
                    && AUCTION_DEBUG_METADATA_ALLOWLIST.contains(&key.as_str())
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        AuctionDebugCommentVerbosity::Full => response
            .metadata
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    };
    let bids: Vec<serde_json::Value> = if options.include_bids {
        response.bids.iter().map(|bid| redact_bid_for_dump(bid, options)).collect()
    } else {
        Vec::new()
    };
    serde_json::json!({
        "provider": response.provider,
        "status": response.status,
        "response_time_ms": response.response_time_ms,
        "bids": bids,
        "metadata": metadata,
    })
}

/// Build a redacted JSON view of a single bid. In `Redacted` verbosity,
/// `creative` is previewed to [`MAX_BID_CREATIVE_DUMP_BYTES`]; in `Full`, it
/// passes through untruncated.
fn redact_bid_for_dump(
    bid: &crate::auction::types::Bid,
    options: &AuctionDebugCommentOptions,
) -> serde_json::Value {
    let mut value = serde_json::to_value(bid).unwrap_or(serde_json::Value::Null);
    if options.verbosity == AuctionDebugCommentVerbosity::Redacted
        && let Some(creative) = &bid.creative
    {
        value["creative"] =
            serde_json::Value::String(truncate_with_marker(creative, MAX_BID_CREATIVE_DUMP_BYTES));
    }
    value
}
```

Note the `metadata_keys.iter().any(...)` check: this is the intersection — a key must be BOTH configured AND in the hardcoded superset. `AUCTION_DEBUG_METADATA_ALLOWLIST.contains` alone would let an operator narrow but a bug in this line (e.g. only checking `metadata_keys`) would break the fail-closed guarantee. This is exactly what `metadata_keys_attack_vector_debug_key_never_surfaces_in_redacted_mode` (Task 2, Step 1) verifies.

- [ ] **Step 4: Update prepend_auction_debug_comment**

Replace the function body (publisher.rs ~950-1028) to add the `options` parameter and gate the two top-level sections:

```rust
pub(crate) fn prepend_auction_debug_comment(
    path_label: &str,
    result: &crate::auction::orchestrator::OrchestrationResult,
    ad_bids_state: &Arc<Mutex<Option<String>>>,
    options: &AuctionDebugCommentOptions,
) {
    let ssp_count = result.provider_responses.len();
    let mediator_info = match &result.mediator_response {
        Some(r) => format!("ok({}_bids)", r.bids.len()),
        None => "none".to_string(),
    };
    let mut dump = serde_json::Map::new();
    if options.include_provider_responses {
        dump.insert(
            "provider_responses".to_string(),
            serde_json::Value::Array(
                result
                    .provider_responses
                    .iter()
                    .map(|r| redact_response_for_dump(r, options))
                    .collect(),
            ),
        );
    }
    if options.include_mediator_response
        && let Some(mediator_response) = &result.mediator_response
    {
        dump.insert(
            "mediator_response".to_string(),
            redact_response_for_dump(mediator_response, options),
        );
    }
    // ... rest of the function (render_dump closure, debug_comment format!,
    // state locking) is UNCHANGED — do not modify below this point.
```

Everything from the `render_dump` closure onward (the neutralization + byte-cap logic, the `format!("<!-- ts-debug: ...")`, and the `ad_bids_state` mutex handling) stays exactly as-is. Only the section-building `if` guards and the function signature change.

- [ ] **Step 5: Update the production call site**

`publisher.rs:1394-1396`:

```rust
    if settings.debug.auction_html_comment {
        prepend_auction_debug_comment(
            "stream",
            &result,
            ad_bids_state,
            &settings.debug.auction_html_comment_options,
        );
    }
```

- [ ] **Step 6: Run tests, verify pass**

Run: `cargo test-axum -p trusted-server-core --lib publisher:: -- --nocapture`
Expected: PASS — all tests from Step 1, plus the pre-existing `auction_debug_comment_dumps_provider_status`, `auction_debug_comment_never_leaks_provider_debug_metadata`, `auction_debug_comment_truncates_oversized_creative`, `auction_debug_comment_neutralises_every_comment_terminator_vector`.

- [ ] **Step 7: Run the full core test module under viceroy**

Format-changing edit (new default metadata keys change the dump's byte-for-byte output) — per project convention, run the full module under viceroy too, not just the fast native run, since `cargo test-axum` alone can miss viceroy-specific behavior and the fast run aborts on first panic hiding later failures if run selectively.

Run: `cargo test-fastly`
Expected: PASS (ignore any `app::tests` DNS `Error` log lines — known noise, not failures)

- [ ] **Step 8: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Make the SSAT ts-debug comment's sections, metadata, and verbosity configurable"
```

---

### Task 3: Document the new config in trusted-server.example.toml

**Files:**
- Modify: `trusted-server.example.toml:144-147` (the `[debug]` section)

- [ ] **Step 1: Add the new table**

After line 147 (`auction_html_comment = false`), insert:

```toml
[debug.auction_html_comment_options]
include_provider_responses = true
include_mediator_response = true
include_bids = true
metadata_keys = [
    "error_type", "http_status", "message",
    "upstream_message", "upstream_message_truncated",
    "responsetimemillis", "errors", "warnings", "bidstatus",
]
# "redacted" (default) or "full". NEVER "full" in production — exposes
# device IP, geo, consent string, and eids via view-source when
# integrations.prebid.debug is also enabled.
verbosity = "redacted"
```

- [ ] **Step 2: Verify the example file still parses**

`trusted-server.example.toml` is loaded directly (via `include_str!` + `Settings::from_toml`) by the Cloudflare and Spin adapters' production startup paths, exercised by their own `tests/routes.rs` — NOT by Fastly or Axum's production paths, so `cargo test-fastly`/`cargo test-axum` would not catch a broken addition here.

Run: `cargo test-cloudflare && cargo test-spin`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add trusted-server.example.toml
git commit -m "Document auction_html_comment_options in the example config"
```

---

### Task 4: Full verification gate

**Files:** none (verification only)

- [ ] **Step 1: Format**

Run: `cargo fmt --all -- --check`
Expected: no diff

- [ ] **Step 2: Lint all adapters**

Run: `cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare && cargo clippy-cloudflare-wasm && cargo clippy-spin-native && cargo clippy-spin-wasm`
Expected: no warnings

(All six, not just fastly/axum: Tasks 1-2 modify `trusted-server-core`, which both `trusted-server-adapter-cloudflare` and `trusted-server-adapter-spin` depend on directly — that alone requires all six targets. Task 3's edit to `trusted-server.example.toml` adds a second, independent reason for the two native-host adapter tests specifically: their native-host build path — the one `cargo test-cloudflare`/`cargo test-spin` exercises — loads that file via `include_str!` + `Settings::from_toml`. Note Cloudflare's true wasm32 production path does NOT load the example TOML — it reads `TRUSTED_SERVER_CONFIG` JSON via `settings_from_cloudflare_config_json()` instead — so this reasoning applies to the native-host test build, not the wasm32 production binary.)

- [ ] **Step 3: Full test suite**

Run: `cargo test-fastly && cargo test-axum && cargo test-cloudflare && cargo test-spin`
Expected: PASS

- [ ] **Step 3b: Parity integration test**

CI Gate item 4 in CLAUDE.md — a distinct crate with its own lockfile (this repo has hit lockfile-drift CI failures here before), so don't assume the four `test-*` aliases above cover it.

Run: `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`
Expected: PASS

(JS build/test/format and docs-format — CI gate items 5-7 — are deliberately skipped: this change touches no `.ts` or `.md` source files.)

- [ ] **Step 4: Confirm spec's behavior-change note is accurate**

Manually diff the default dump output (via a quick `cargo test-axum default_options_reproduce_current_behavior -- --nocapture` if you added `println!` temporarily, or just trust the assertions) against what's described in the spec's "Edge Cases and Behavior Changes" section — 3 new keys added, `status` key dropped. This is a sanity check, not a new test.

---

## Notes for the implementer

- Do not touch `Bid.metadata`/`nurl`/`burl` pass-through behavior — tracked separately as issue #925, explicitly out of scope (see spec's Non-goals).
- Do not add no-bid reason capture to `aps.rs` or other provider adapters — separate, larger follow-up (see spec's Follow-up section).
- Do not make `MAX_BID_CREATIVE_DUMP_BYTES` or `MAX_AUCTION_DEBUG_DUMP_BYTES` configurable — explicitly declined in the spec's Non-goals.
- If `Settings` doesn't derive `Deserialize` directly usable with a minimal TOML snippet (Task 1, Step 1's third test), fall back to testing `AuctionDebugCommentOptions` deserialization in isolation rather than fighting a full `Settings` fixture.
