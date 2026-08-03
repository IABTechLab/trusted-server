# PR #957 Review Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Resolve every technically sound review comment on PR #957 while preserving legacy static/absent `gam_unit_path` behavior and making dynamic paths rollback-safe and allocation-bounded.

**Architecture:** Extend the existing startup compilation pass to materialize the default `section_segment` as a rollback marker and to validate only values consumed by parsed templates. Dynamic rendering becomes a checked `Option<String>` path capped at 100 UTF-8 bytes; both initial and SPA slot builders derive the section once and omit only an over-limit dynamic slot. Static and absent paths bypass the new dynamic limit to preserve pre-template behavior.

**Tech Stack:** Rust 2024, serde/TOML/JSON, `error-stack` through existing settings preparation, native and WASM adapter test aliases, Markdown documentation.

---

## File map

- Modify `crates/trusted-server-core/src/config.rs`: add push-shaped legacy-schema compatibility tests around `TrustedServerAppConfig` serialization.
- Modify `crates/trusted-server-core/src/creative_opportunities.rs`: add rollback-marker detection, scoped placeholder consumption, bounded rendering, shared section validation, and unit tests.
- Modify `crates/trusted-server-core/src/publisher.rs`: derive one section per request, propagate recoverable slot omission, and update publisher tests.
- Modify `docs/guide/configuration.md`: document configured-segment behavior, dynamic rollback marker, precise validation, and rendered-path bound.
- Modify `CHANGELOG.md`: replace the misleading rollback and blanket network-ID statements.
- Modify `docs/superpowers/specs/2026-08-03-pr-957-review-resolution-design.md`: retain the independent review clarification that the structural bound is measured in UTF-8 bytes.

### Task 1: Make pushed dynamic templates fail legacy deserialization

**Files:**

- Modify: `crates/trusted-server-core/src/config.rs:213-253`
- Modify: `crates/trusted-server-core/src/creative_opportunities.rs:17-28, 213-224, 483-560`
- Test: `crates/trusted-server-core/src/config.rs`

- [ ] **Step 1: Add a test-only legacy schema and pushed-config helper**

In `config.rs`'s test module, add a minimal legacy creative-opportunities schema. It deliberately knows the old top-level fields but not `section_root` or `section_segment`:

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyCreativeOpportunitiesConfig {
    gam_network_id: String,
    #[serde(default)]
    auction_timeout_ms: Option<u32>,
    #[serde(default)]
    price_granularity: serde_json::Value,
    #[serde(default)]
    slot: Vec<serde_json::Value>,
}

fn pushed_creative_opportunities(gam_unit_path: Option<&str>) -> serde_json::Value {
    let gam_unit_path = gam_unit_path
        .map(|path| format!("gam_unit_path = {path:?}"))
        .unwrap_or_default();
    let toml = format!(
        r#"{}
[creative_opportunities]
gam_network_id = "99999"

[[creative_opportunities.slot]]
id = "ad-header"
{gam_unit_path}
page_patterns = ["/"]
formats = [{{ width = 728, height = 90 }}]
"#,
        crate_test_settings_str()
    );
    let app_config: TrustedServerAppConfig =
        toml::from_str(&toml).expect("should finalize typed app config");
    serde_json::to_value(app_config)
        .expect("should serialize pushed app config")
        .get("creative_opportunities")
        .cloned()
        .expect("should contain creative opportunities")
}
```

Use field reads or `#[allow(dead_code)]` if rustc flags the intentionally deserialize-only legacy fields.

- [ ] **Step 2: Write failing push-shaped rollback tests**

Add three focused tests:

```rust
#[test]
fn pushed_dynamic_templates_are_rejected_by_legacy_schema() {
    for template in ["/{network_id}/example", "/example/{slot_id}"] {
        let value = pushed_creative_opportunities(Some(template));
        let error = serde_json::from_value::<LegacyCreativeOpportunitiesConfig>(value)
            .expect_err("legacy schema should reject automatic compatibility marker");
        assert!(
            error.to_string().contains("section_segment"),
            "error should name compatibility marker: {error}"
        );
    }
}

#[test]
fn pushed_static_template_is_accepted_by_legacy_schema() {
    let value = pushed_creative_opportunities(Some("/99999/example/home"));
    serde_json::from_value::<LegacyCreativeOpportunitiesConfig>(value)
        .expect("legacy schema should accept static path");
}

#[test]
fn pushed_absent_template_is_accepted_by_legacy_schema() {
    let value = pushed_creative_opportunities(None);
    serde_json::from_value::<LegacyCreativeOpportunitiesConfig>(value)
        .expect("legacy schema should accept absent path");
}
```

- [ ] **Step 3: Run the dynamic rollback test and verify RED**

Run:

```bash
cargo test --package trusted-server-core --target aarch64-apple-darwin pushed_dynamic_templates_are_rejected_by_legacy_schema
```

Expected: FAIL because the finalized dynamic config still omits `section_segment`, so the legacy schema accepts it.

- [ ] **Step 4: Implement placeholder detection and automatic marker materialization**

Add a placeholder predicate to `UnitTemplatePart`:

```rust
impl UnitTemplatePart {
    fn is_placeholder(&self) -> bool {
        !matches!(self, Self::Literal(_))
    }
}
```

Add `template_is_dynamic` beside the existing placeholder-use helpers. It must read `compiled_unit` when present and parse the raw template when absent:

```rust
fn template_is_dynamic(&self) -> bool {
    self.template_parts()
        .is_some_and(|parts| parts.iter().any(UnitTemplatePart::is_placeholder))
}
```

Introduce a small private `template_parts` helper that returns borrowed compiled parts when available and parsed owned parts for the raw fallback without changing malformed-template startup behavior. If a `Cow<'_, [UnitTemplatePart]>` keeps the implementation clearer, import `std::borrow::Cow`; otherwise keep the existing match shape and avoid a new abstraction.

After compiling every slot in `compile_unit_templates`, materialize the existing default only for dynamic templates:

```rust
if self.section_segment.is_none()
    && self
        .slot
        .iter()
        .any(CreativeOpportunitySlot::template_is_dynamic)
{
    self.section_segment = Some(0);
}
```

Do not mark static or absent paths, and do not overwrite an explicitly configured segment.

- [ ] **Step 5: Run the three rollback tests and verify GREEN**

Run:

```bash
cargo test --package trusted-server-core --target aarch64-apple-darwin pushed_dynamic_templates_are_rejected_by_legacy_schema
cargo test --package trusted-server-core --target aarch64-apple-darwin pushed_static_template_is_accepted_by_legacy_schema
cargo test --package trusted-server-core --target aarch64-apple-darwin pushed_absent_template_is_accepted_by_legacy_schema
```

Expected: all PASS.

- [ ] **Step 6: Run the complete core test target**

Run:

```bash
cargo test --package trusted-server-core --target aarch64-apple-darwin
```

Expected: PASS with no warnings.

- [ ] **Step 7: Commit the rollback marker**

```bash
git add crates/trusted-server-core/src/config.rs crates/trusted-server-core/src/creative_opportunities.rs
git commit -m "Make dynamic GAM templates fail legacy rollback"
```

### Task 2: Scope network-ID validation to actual consumers

**Files:**

- Modify: `crates/trusted-server-core/src/creative_opportunities.rs:226-278, 544-560, 1100-1130`
- Test: `crates/trusted-server-core/src/creative_opportunities.rs`

- [ ] **Step 1: Add failing compatibility tests**

Add tests covering explicit static and `{slot_id}`-only paths with a blank network ID:

```rust
#[test]
fn validate_runtime_allows_blank_network_id_for_static_paths() {
    let mut config = make_config_with_section_template(None);
    config.gam_network_id.clear();
    config.slot[0].gam_unit_path = Some("/example/static".to_string());
    config.compile_slots();
    config.compile_unit_templates().expect("should compile static path");
    config
        .validate_runtime()
        .expect("static path should not consume network id");
}

#[test]
fn validate_runtime_allows_blank_network_id_for_slot_id_template() {
    let mut config = make_config_with_section_template(None);
    config.gam_network_id.clear();
    config.slot[0].gam_unit_path = Some("/example/{slot_id}".to_string());
    config.compile_slots();
    config
        .compile_unit_templates()
        .expect("should compile slot-id template");
    config
        .validate_runtime()
        .expect("slot-id template should not consume network id");
}
```

Also retain the existing compiled `{network_id}` rejection and add an uncompiled/raw-cache-fallback test so validation cannot skip consumption detection.

Add an explicit absent-path rejection because the default path consumes the
network ID:

```rust
#[test]
fn validate_runtime_rejects_blank_network_id_for_absent_path() {
    let mut config = make_config_with_section_template(None);
    config.gam_network_id.clear();
    config.slot[0].gam_unit_path = None;
    config.compile_slots();
    config
        .compile_unit_templates()
        .expect("should compile absent path");
    config
        .validate_runtime()
        .expect_err("absent path should consume network id");
}
```

- [ ] **Step 2: Run the new compatibility test and verify RED**

Run:

```bash
cargo test --package trusted-server-core --target aarch64-apple-darwin validate_runtime_allows_blank_network_id_for_static_paths
```

Expected: FAIL with `gam_network_id must not be empty`.

- [ ] **Step 3: Implement `template_uses_network_id` and scoped validation**

Add a sibling of `template_uses_section` using the same compiled/raw fallback:

```rust
fn template_uses_network_id(&self) -> bool {
    self.template_parts().is_some_and(|parts| {
        parts
            .iter()
            .any(|part| matches!(part, UnitTemplatePart::NetworkId))
    })
}
```

Replace the blanket slot-list check with:

```rust
let network_id_consumed = self.slot.iter().any(|slot| {
    slot.gam_unit_path.is_none() || slot.template_uses_network_id()
});
if network_id_consumed && self.gam_network_id.trim().is_empty() {
    return Err("gam_network_id must not be empty".to_string());
}
```

Update the nearby comments and rustdoc to say “consumed by a slot,” not “when slots are configured.”

- [ ] **Step 4: Run scoped validation tests and verify GREEN**

Run:

```bash
cargo test --package trusted-server-core --target aarch64-apple-darwin validate_runtime_allows_blank_network_id
cargo test --package trusted-server-core --target aarch64-apple-darwin validate_runtime_rejects_blank_network_id
```

Expected: static, slot-ID-only, and empty-slot cases PASS; absent-path and `{network_id}` cases reject as asserted.

- [ ] **Step 5: Run complete core tests**

Run `cargo test --package trusted-server-core --target aarch64-apple-darwin`.

Expected: PASS.

- [ ] **Step 6: Commit scoped validation**

```bash
git add crates/trusted-server-core/src/creative_opportunities.rs
git commit -m "Validate GAM network IDs only when consumed"
```

### Task 3: Bound dynamic rendering before allocation

**Files:**

- Modify: `crates/trusted-server-core/src/creative_opportunities.rs:83-125, 226-278, 483-560`
- Test: `crates/trusted-server-core/src/creative_opportunities.rs:876-1080`

- [ ] **Step 1: Add failing section and renderer tests**

Add these behaviors as separate tests:

```rust
#[test]
fn derive_section_caps_sanitized_output_at_100_ascii_bytes() {
    let path = format!("/{}", "a".repeat(150));
    assert_eq!(derive_section(&path, "home", 0), "a".repeat(100));
}

#[test]
fn render_gam_unit_path_omits_over_limit_repeated_section() {
    let mut slot = make_slot("ad-header", vec!["/*"]);
    slot.gam_unit_path = Some("/{section}/{section}".to_string());
    slot.compile_unit_template().expect("should compile template");
    assert_eq!(
        slot.render_gam_unit_path("99999", &"a".repeat(60)),
        None,
        "dynamic output over 100 bytes should be omitted"
    );
}

#[test]
fn render_gam_unit_path_preserves_over_limit_static_path() {
    let mut slot = make_slot("ad-header", vec!["/*"]);
    let static_path = format!("/{}", "a".repeat(120));
    slot.gam_unit_path = Some(static_path.clone());
    slot.compile_unit_template().expect("should compile static path");
    assert_eq!(
        slot.render_gam_unit_path("99999", "ignored"),
        Some(static_path),
        "legacy static paths should retain existing behavior"
    );
}
```

Add an equivalent raw/uncompiled repeated-placeholder test and a startup test where repeated `{section}` with `section_root = "homepage"` already exceeds the bound.

- [ ] **Step 2: Run the new renderer test and verify RED**

Run:

```bash
cargo test --package trusted-server-core --target aarch64-apple-darwin render_gam_unit_path_omits_over_limit_repeated_section
```

Expected: compile failure or assertion failure because the renderer still returns an unbounded `String`.

- [ ] **Step 3: Share the section-character predicate and cap sanitization**

Add constants and a predicate near `sanitize_section`:

```rust
const MAX_DYNAMIC_GAM_UNIT_PATH_BYTES: usize = 100;
const MAX_SECTION_BYTES: usize = 100;

fn is_section_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'
}
```

Use `is_section_char` in sanitization and `section_root` validation. Build the sanitized ASCII result with capacity capped at `MAX_SECTION_BYTES`, and stop once the output reaches that byte count. Preserve the “one underscore per disallowed run” behavior.

- [ ] **Step 4: Implement checked dynamic rendering**

Add a private renderer that selects each part's `&str`, computes the exact UTF-8 byte count with `checked_add`, rejects totals over `MAX_DYNAMIC_GAM_UNIT_PATH_BYTES`, then performs a single allocation:

```rust
fn render_dynamic_parts(
    parts: &[UnitTemplatePart],
    gam_network_id: &str,
    section: &str,
    slot_id: &str,
) -> Option<String> {
    let value = |part: &UnitTemplatePart| match part {
        UnitTemplatePart::Literal(value) => value.as_str(),
        UnitTemplatePart::NetworkId => gam_network_id,
        UnitTemplatePart::Section => section,
        UnitTemplatePart::SlotId => slot_id,
    };
    let rendered_len = parts.iter().try_fold(0usize, |len, part| {
        len.checked_add(value(part).len())
    })?;
    if rendered_len > MAX_DYNAMIC_GAM_UNIT_PATH_BYTES {
        return None;
    }
    let mut rendered = String::with_capacity(rendered_len);
    for part in parts {
        rendered.push_str(value(part));
    }
    Some(rendered)
}
```

Change `render_gam_unit_path` to `Option<String>`:

- compiled/raw dynamic template → `render_dynamic_parts`;
- compiled/raw static template → `Some(raw.clone())` without the dynamic bound;
- absent path → `Some(format!(...))` without the dynamic bound.

Keep raw malformed templates literal for existing direct-caller compatibility, wrapped in `Some`.

- [ ] **Step 5: Reject configured dynamic paths that already exceed the bound**

After slot shape, network-ID, and section-root validation, validate dynamic templates by rendering with the configured `section_root` (or `""` when `{section}` is unused). Return a slot-specific startup error when that fixed/configured rendering is `None`.

This catches repeated-root templates at startup while leaving request-specific long sections to recoverable omission.

- [ ] **Step 6: Update existing renderer assertions for `Option<String>`**

Wrap expected paths in `Some(...)` or call `.expect("should render ...")` where the test subsequently indexes/compares a concrete string. Do not weaken malformed-template or static/absent compatibility assertions.

- [ ] **Step 7: Run focused and complete core tests**

Run:

```bash
cargo test --package trusted-server-core --target aarch64-apple-darwin derive_section_caps_sanitized_output
cargo test --package trusted-server-core --target aarch64-apple-darwin render_gam_unit_path
cargo test --package trusted-server-core --target aarch64-apple-darwin validate_runtime
cargo test --package trusted-server-core --target aarch64-apple-darwin
```

Expected: PASS with no warnings.

- [ ] **Step 8: Commit bounded rendering**

```bash
git add crates/trusted-server-core/src/creative_opportunities.rs
git commit -m "Bound dynamic GAM unit path rendering"
```

### Task 4: Omit over-limit slots consistently and derive section once

**Files:**

- Modify: `crates/trusted-server-core/src/publisher.rs:3331-3387, 3790-3799, 7293-7340`
- Test: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Add a failing publisher omission test**

Add a test beside the existing `build_slot_json` section tests:

```rust
#[test]
fn ad_slots_script_omits_over_limit_dynamic_slot() {
    let mut config = make_config();
    config.section_root = Some("home".to_string());
    let mut over_limit = make_slot();
    over_limit.gam_unit_path = Some("/{section}/{section}".to_string());
    over_limit
        .compile_unit_template()
        .expect("should compile over-limit template");
    let mut valid = make_slot();
    valid.id = "valid-slot".to_string();
    valid.gam_unit_path = Some("/99999/example/valid".to_string());
    valid
        .compile_unit_template()
        .expect("should compile valid static path");
    let request_path = format!("/{}", "a".repeat(60));

    let script = build_ad_slots_script(&[over_limit, valid], &config, &request_path);

    assert!(
        !script.contains("atf_sidebar_ad"),
        "should omit over-limit dynamic slot"
    );
    assert!(script.contains("valid-slot"), "should preserve valid sibling slot");
}
```

If script escaping changes the exact literal, parse/extract the JSON using the existing test helper rather than asserting a brittle escaped substring.

- [ ] **Step 2: Run the omission test and verify RED**

Run:

```bash
cargo test --package trusted-server-core --target aarch64-apple-darwin ad_slots_script_omits_over_limit_dynamic_slot
```

Expected: compile failure because `build_slot_json` does not yet handle `Option<String>`, or an assertion failure because the slot is still emitted.

- [ ] **Step 3: Change the shared builder to accept a derived section**

Change the signature and early-return on renderer omission:

```rust
fn build_slot_json(
    slot: &CreativeOpportunitySlot,
    co_config: &CreativeOpportunitiesConfig,
    section: &str,
) -> Option<serde_json::Value> {
    let gam_path = slot.render_gam_unit_path(&co_config.gam_network_id, section)?;
    Some(serde_json::json!({ /* existing wire shape */ }))
}
```

Keep the function `pub(crate)` if cross-module tests still call it.

- [ ] **Step 4: Derive once and `filter_map` in both production callers**

In `build_ad_slots_script`:

```rust
let section = co_config.section_for_path(request_path);
let slots = matched_slots
    .iter()
    .filter_map(|slot| build_slot_json(slot, co_config, &section))
    .collect::<Vec<_>>();
```

In `handle_page_bids`, derive `section` once from `path_param` before the ad-stack gate, then use the same `filter_map`. Do not create a separate SPA rendering implementation.

- [ ] **Step 5: Update existing publisher unit tests**

Derive the section explicitly in tests that call `build_slot_json`, then call `.expect("should render slot")` before indexing. Preserve assertions for default segment, configured segment, and root fallback.

- [ ] **Step 6: Run publisher and core tests**

Run:

```bash
cargo test --package trusted-server-core --target aarch64-apple-darwin ad_slots_script
cargo test --package trusted-server-core --target aarch64-apple-darwin build_slot_json
cargo test --package trusted-server-core --target aarch64-apple-darwin page_bids
cargo test --package trusted-server-core --target aarch64-apple-darwin
```

Expected: PASS; both initial and SPA tests use the shared bounded builder.

- [ ] **Step 7: Commit publisher propagation**

```bash
git add crates/trusted-server-core/src/publisher.rs
git commit -m "Omit over-limit dynamic GAM slots"
```

### Task 5: Correct API visibility and documentation

**Files:**

- Modify: `crates/trusted-server-core/src/creative_opportunities.rs:101-240, 501-560`
- Modify: `docs/guide/configuration.md:1297-1350`
- Modify: `CHANGELOG.md:23`
- Modify: `docs/superpowers/specs/2026-08-03-pr-957-review-resolution-design.md`

- [ ] **Step 1: Make focused rustdoc and visibility edits**

- Make `derive_section` private; same-module tests remain valid.
- Describe `section_root` as fallback when the configured segment is absent, not when there is no first segment.
- State that raw-template fallback enforces placeholder-dependent requirements even without the cache, while `compile_unit_templates` remains required to reject malformed templates.
- Document `render_gam_unit_path`'s `None` result and the 100-byte dynamic bound.
- Update validation rustdoc to describe network-ID consumption rather than any configured slot.

- [ ] **Step 2: Correct the configuration guide**

Update the placeholder row to:

```markdown
| `{section}` | non-empty path segment at `section_segment` (default: first; see below) |
```

Update validation and rollback paragraphs to say:

- blank `gam_network_id` is rejected only if an absent path or `{network_id}` template consumes it;
- every dynamic template causes typed serialization to materialize
  `section_segment = 0` when it was omitted;
- legacy binaries therefore reject dynamic blobs loudly;
- only static and absent paths retain legacy-schema compatibility;
- dynamic rendering is capped at 100 UTF-8 bytes and over-limit request slots are omitted.

Do not add the reviewer's case-sensitive warning or lowercase sections: Google documents GAM ad-unit codes as case-insensitive.

- [ ] **Step 3: Correct the changelog**

Replace “startup rejects a blank network ID when slots are configured” with the scoped consumption rule. Replace “configs that omit the new keys roll back cleanly” with the automatic-marker behavior and the static/absent compatibility guarantee.

- [ ] **Step 4: Format and inspect documentation**

Run:

```bash
cd docs && npm run format
git diff --check
```

Expected: formatter exits 0; no whitespace errors or unrelated documentation changes. Revert only formatter-induced unrelated edits with a non-destructive patch if any appear.

- [ ] **Step 5: Run rustdoc-sensitive lint and focused tests**

Run:

```bash
cargo fmt --all -- --check
cargo clippy-axum
cargo test --package trusted-server-core --target aarch64-apple-darwin
```

Expected: PASS.

- [ ] **Step 6: Commit documentation and visibility cleanup**

```bash
git add CHANGELOG.md docs/guide/configuration.md docs/superpowers/specs/2026-08-03-pr-957-review-resolution-design.md crates/trusted-server-core/src/creative_opportunities.rs
git commit -m "Document dynamic GAM template safeguards"
```

### Task 6: Run complete PR verification and review the final diff

**Files:**

- Verify all modified files; no planned code changes.

- [ ] **Step 1: Run Rust and parity formatting gates**

```bash
cargo fmt --all -- --check
cargo fmt --manifest-path crates/trusted-server-integration-tests/Cargo.toml -- --check
```

Expected: both PASS and `git status --short` shows no formatter changes.

- [ ] **Step 2: Run adapter checks and release builds**

From repository root:

```bash
cargo build -p trusted-server-adapter-axum
cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
cargo check -p trusted-server-adapter-cloudflare
cargo check-cloudflare
cargo check -p trusted-server-adapter-spin
cargo check-spin
TRUSTED_SERVER__PUBLISHER__ORIGIN_URL=http://127.0.0.1:8080 \
TRUSTED_SERVER__PUBLISHER__PROXY_SECRET=integration-test-proxy-secret \
TRUSTED_SERVER__EC__PASSPHRASE=integration-test-ec-secret-padded-32 \
TRUSTED_SERVER__PROXY__CERTIFICATE_CHECK=false \
cargo build --package trusted-server-adapter-spin --target wasm32-wasip1 --features spin --release
```

Expected: all PASS.

- [ ] **Step 3: Run Rust test and benchmark gates**

From repository root:

```bash
cargo test-fastly
cargo test-axum
cargo bench -p trusted-server-core --bench html_processor_bench -- --test
cargo test-cloudflare
cargo test-spin
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
./scripts/test-cli.sh
cargo test --package trusted-server-openrtb-codegen --target aarch64-apple-darwin
```

Expected: all PASS.

- [ ] **Step 4: Run every PR clippy gate**

From repository root:

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo clippy --package trusted-server-cli --target aarch64-apple-darwin --all-targets --all-features -- -D warnings
cargo clippy --package trusted-server-openrtb-codegen --target aarch64-apple-darwin --all-targets -- -D warnings
cargo clippy --manifest-path crates/trusted-server-integration-tests/Cargo.toml --all-targets -- -D warnings
```

Expected: all PASS with `-D warnings`.

- [ ] **Step 5: Run JavaScript build, test, lint, and format gates**

```bash
cd crates/trusted-server-js/lib
npm run build
npm test -- --run
npm run lint
npm run format
```

Expected: all PASS.

- [ ] **Step 6: Run documentation lint, format, and build checks**

From the repository root:

```bash
cd docs
npm run lint
npm run format
npm run build
```

Expected: all PASS. The build is included because this review changes the
published configuration guide, even though docs deployment itself runs only on
`main`.

- [ ] **Step 7: Inspect the complete branch diff**

```bash
git status --short --branch
git diff --check main...HEAD
git diff --stat main...HEAD
git log --oneline --decorate -10
```

Confirm:

- only planned runtime, tests, docs, spec, and plan files changed in this review round;
- no placeholder, debug output, unrelated refactor, or sensitive real-world data was added;
- static and absent paths retain their compatibility tests;
- every dynamic rendering call handles `None`.

- [ ] **Step 8: Prepare GitHub thread resolutions**

Draft one concise technical reply per current review thread, naming the implemented behavior and test. For the casing thread, cite Google's official “Ad units: Name vs. code” documentation and explain that no lowercasing or warning was added because codes are case-insensitive. Do not post or resolve GitHub threads without explicit user authorization.
