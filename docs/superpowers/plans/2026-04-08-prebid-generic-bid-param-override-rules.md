# Prebid Generic Bid Param Override Rules Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the current split Prebid bidder-param override implementation with one generic ordered rule engine while preserving `bid_param_overrides` and `bid_param_zone_overrides` as compatibility config and adding canonical `bid_param_override_rules`.

**Architecture:** Normalize all override config shapes into one ordered internal rule list, then evaluate that list against request facts (`bidder`, `zone`) at the existing bidder-param assembly point in `to_openrtb`. Keep the current shallow-merge semantics and last-write-wins precedence, but move validation into one compiled-rule path so explicit and compatibility config behave consistently.

**Tech Stack:** Rust 2024, `serde`, `serde_json`, `validator`, unit tests in `prebid.rs`, env parsing tests in `settings.rs`.

---

## Files Modified

| File | Changes |
|------|---------|
| `crates/trusted-server-core/src/integrations/prebid.rs` | Replace `BidOverride` / `StaticBidOverride` / `ContextKey` / `KeyedBidOverride` runtime with canonical rule structs and one internal engine; add canonical rule parsing, validation, normalization, and runtime application; update config docs and tests |
| `crates/trusted-server-core/src/settings.rs` | Add env parsing coverage for `bid_param_override_rules` |
| `trusted-server.toml` | Document the new canonical rule syntax while keeping compatibility examples |

## Task 1: Add failing tests for canonical rules and compatibility normalization

**Files:**
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs`

- [ ] **Step 1.1: Add a canonical rule parsing test near the existing Prebid config parsing tests**

```rust
#[test]
fn bid_param_override_rules_config_parsing_from_toml() {
    let config = parse_prebid_toml(
        r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"

[[integrations.prebid.bid_param_override_rules]]
when.bidder = "kargo"
when.zone = "header"
set = { placementId = "_s2sHeader", extra = "x" }
"#,
    );

    assert_eq!(config.bid_param_override_rules.len(), 1);
    assert_eq!(
        config.bid_param_override_rules[0].when.bidder.as_deref(),
        Some("kargo")
    );
    assert_eq!(
        config.bid_param_override_rules[0].when.zone.as_deref(),
        Some("header")
    );
    assert_eq!(
        config.bid_param_override_rules[0].set["placementId"],
        "_s2sHeader"
    );
}
```

- [ ] **Step 1.2: Add a failing runtime test that proves explicit canonical rules apply**

```rust
#[test]
fn explicit_bid_param_override_rule_applies_for_bidder_and_zone() {
    let config = parse_prebid_toml(
        r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
bidders = ["kargo"]

[[integrations.prebid.bid_param_override_rules]]
when.bidder = "kargo"
when.zone = "header"
set = { placementId = "rule_header", keep = "server" }
"#,
    );

    let slot = make_ts_slot(
        "ad-header-0",
        &json!({ "kargo": { "placementId": "client", "keep": "client", "other": "present" } }),
        Some("header"),
    );
    let request = make_auction_request(vec![slot]);

    let ortb = call_to_openrtb(config, &request);
    let params = bidder_params(&ortb);

    assert_eq!(params["kargo"]["placementId"], "rule_header");
    assert_eq!(params["kargo"]["keep"], "server");
    assert_eq!(params["kargo"]["other"], "present");
}
```

- [ ] **Step 1.3: Add a failing precedence test proving canonical rules override compatibility-derived rules**

```rust
#[test]
fn explicit_bid_param_override_rule_wins_over_zone_compatibility_rule() {
    let config = parse_prebid_toml(
        r#"
[integrations.prebid]
enabled = true
server_url = "https://prebid.example"
bidders = ["kargo"]

[integrations.prebid.bid_param_zone_overrides.kargo]
header = { placementId = "compat_header" }

[[integrations.prebid.bid_param_override_rules]]
when.bidder = "kargo"
when.zone = "header"
set = { placementId = "explicit_header" }
"#,
    );

    let slot = make_ts_slot(
        "ad-header-0",
        &json!({ "kargo": { "placementId": "client" } }),
        Some("header"),
    );
    let request = make_auction_request(vec![slot]);

    let ortb = call_to_openrtb(config, &request);
    assert_eq!(bidder_params(&ortb)["kargo"]["placementId"], "explicit_header");
}
```

- [ ] **Step 1.4: Add failing validation tests in the existing override-focused unit test module**

```rust
#[test]
fn compile_rule_rejects_empty_when() {
    let rule = BidParamOverrideRule {
        when: BidParamOverrideWhen::default(),
        set: json!({ "placementId": "x" }),
    };

    let result = CompiledBidParamOverrideRule::try_from(rule);
    assert!(result.is_err(), "should reject empty when");
}

#[test]
fn compile_rule_rejects_non_object_set() {
    let rule = BidParamOverrideRule {
        when: BidParamOverrideWhen {
            bidder: Some("kargo".to_string()),
            zone: None,
        },
        set: json!("not-an-object"),
    };

    let result = CompiledBidParamOverrideRule::try_from(rule);
    assert!(result.is_err(), "should reject non-object set");
}
```

- [ ] **Step 1.5: Run the focused tests and confirm they fail for the expected missing pieces**

Run:
```bash
cargo test -p trusted-server-core explicit_bid_param_override_rule
cargo test -p trusted-server-core compile_rule_rejects
```

Expected: failures because `bid_param_override_rules` and compiled-rule validation do not exist yet.

## Task 2: Replace the split override runtime with one compiled rule engine

**Files:**
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs`

- [ ] **Step 2.1: Add the canonical config structs to `PrebidIntegrationConfig`**

Add:
```rust
/// Canonical ordered bidder-param override rules.
#[serde(default)]
pub bid_param_override_rules: Vec<BidParamOverrideRule>,
```

With new supporting structs:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BidParamOverrideRule {
    pub when: BidParamOverrideWhen,
    pub set: Json,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BidParamOverrideWhen {
    #[serde(default)]
    pub bidder: Option<String>,
    #[serde(default)]
    pub zone: Option<String>,
}
```

- [ ] **Step 2.2: Delete the current runtime override abstraction block**

Delete:
- `BidOverrideContext`
- `BidOverride`
- `StaticBidOverride`
- `ContextKey`
- `ZoneKey`
- `KeyedBidOverride`
- `ZoneBidOverride`

Replace them with:
```rust
#[derive(Debug, Default)]
struct BidParamOverrideEngine {
    rules: Vec<CompiledBidParamOverrideRule>,
}

#[derive(Debug)]
struct BidParamOverrideFacts<'a> {
    bidder: &'a str,
    zone: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CompiledBidParamOverrideRule {
    bidder: Option<String>,
    zone: Option<String>,
    set: serde_json::Map<String, Json>,
}
```

- [ ] **Step 2.3: Implement compiled-rule validation and engine normalization**

Add:
- `impl TryFrom<BidParamOverrideRule> for CompiledBidParamOverrideRule`
- helper `fn validate_matcher_string(...)`
- helper `fn json_object_for_override(...)`
- `impl BidParamOverrideEngine { fn try_from_config(...) -> Result<Self, Report<TrustedServerError>> }`
- compatibility normalization helpers for:
  - `bid_param_overrides`
  - `bid_param_zone_overrides`
  - explicit `bid_param_override_rules`

Validation rules:
- reject empty `when`
- reject empty strings for `bidder` or `zone`
- reject non-object or empty-object `set`

- [ ] **Step 2.4: Store the compiled engine in `PrebidIntegration` and `PrebidAuctionProvider`**

Change the structs from:
```rust
pub struct PrebidIntegration { config: PrebidIntegrationConfig }
pub struct PrebidAuctionProvider { config: PrebidIntegrationConfig }
```

To:
```rust
pub struct PrebidIntegration {
    config: PrebidIntegrationConfig,
    bid_param_override_engine: Arc<BidParamOverrideEngine>,
}

pub struct PrebidAuctionProvider {
    config: PrebidIntegrationConfig,
    bid_param_override_engine: Arc<BidParamOverrideEngine>,
}
```

Compile the engine in `PrebidIntegration::new` and `PrebidAuctionProvider::new`.

- [ ] **Step 2.5: Replace the runtime application path in `to_openrtb`**

Replace:
```rust
let ctx = BidOverrideContext { zone };
for (name, params) in &mut bidder {
    self.config.bid_param_overrides.apply(name, &ctx, params);
    self.config.bid_param_zone_overrides.apply(name, &ctx, params);
}
```

With:
```rust
for (name, params) in &mut bidder {
    self.bid_param_override_engine.apply(
        BidParamOverrideFacts {
            bidder: name,
            zone,
        },
        params,
    );
}
```

- [ ] **Step 2.6: Run the targeted tests and make them pass**

Run:
```bash
cargo test -p trusted-server-core explicit_bid_param_override_rule
cargo test -p trusted-server-core compile_rule_rejects
```

Expected: PASS

## Task 3: Update existing tests to the new engine model and extend env coverage

**Files:**
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs`
- Modify: `crates/trusted-server-core/src/settings.rs`

- [ ] **Step 3.1: Rewrite the old override unit tests to target the engine directly**

Replace the current `mod bid_override` tests with engine-centric tests:
- compatibility static rule normalization
- compatibility zone rule normalization
- `apply` with bidder-only facts
- `apply` with bidder + zone facts
- no-op on unmatched facts
- later rule wins on overlapping keys

- [ ] **Step 3.2: Remove tests that only exist for deleted types**

Delete tests specific to:
- `StaticBidOverride`
- `ZoneBidOverride`
- `KeyedBidOverride`
- `ContextKey`

- [ ] **Step 3.3: Add env parsing coverage for canonical rules in `settings.rs`**

Add a test similar to the existing `bid_param_overrides` env test:
```rust
#[test]
fn test_prebid_bid_param_override_rules_override_with_json_env() {
    // TRUSTED_SERVER__INTEGRATIONS__PREBID__BID_PARAM_OVERRIDE_RULES='[...]'
}
```

Assert that:
- the array parses
- `when.bidder` and `when.zone` survive round-trip
- `set` preserves the JSON object

- [ ] **Step 3.4: Run the focused Rust tests for Prebid and settings**

Run:
```bash
cargo test -p trusted-server-core bid_param_override
cargo test -p trusted-server-core test_prebid_bid_param_override
```

Expected: PASS

## Task 4: Update operator-facing documentation and sample config

**Files:**
- Modify: `trusted-server.toml`
- Modify: `crates/trusted-server-core/src/integrations/prebid.rs`

- [ ] **Step 4.1: Update `trusted-server.toml` comments**

Keep the existing compatibility examples, then add the canonical rule format:

```toml
# [[integrations.prebid.bid_param_override_rules]]
# when.bidder = "kargo"
# when.zone = "header"
# set = { placementId = "_abc" }
```

Document:
- compatibility fields are still supported
- canonical rules are preferred for future overrides

- [ ] **Step 4.2: Update `PrebidIntegrationConfig` field docs**

Adjust the field comments so they describe:
- `bid_param_overrides` as compatibility sugar
- `bid_param_zone_overrides` as compatibility sugar
- `bid_param_override_rules` as the canonical ordered rule list

- [ ] **Step 4.3: Run rustdoc-sensitive checks for the touched crate**

Run:
```bash
cargo test -p trusted-server-core parse_prebid_toml
```

Expected: PASS

## Task 5: Verify, commit, and leave the branch clean

**Files:**
- Modify: `docs/superpowers/plans/2026-04-08-prebid-generic-bid-param-override-rules.md` (check off completed steps if desired)

- [ ] **Step 5.1: Run formatting**

Run:
```bash
cargo fmt --all -- --check
```

Expected: PASS

- [ ] **Step 5.2: Run crate-level verification**

Run:
```bash
cargo test -p trusted-server-core
cargo clippy -p trusted-server-core --all-targets --all-features -- -D warnings
```

Expected: PASS

- [ ] **Step 5.3: Run workspace verification**

Run:
```bash
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: PASS

- [ ] **Step 5.4: Review `git diff` and commit all changes**

Run:
```bash
git status --short
git diff -- crates/trusted-server-core/src/integrations/prebid.rs crates/trusted-server-core/src/settings.rs trusted-server.toml docs/superpowers/plans/2026-04-08-prebid-generic-bid-param-override-rules.md
git add crates/trusted-server-core/src/integrations/prebid.rs crates/trusted-server-core/src/settings.rs trusted-server.toml docs/superpowers/plans/2026-04-08-prebid-generic-bid-param-override-rules.md
git commit -m "Implement generic Prebid bid param override rules"
```

Expected: working tree clean except for any intentionally untracked files.
