# Per-Section `gam_unit_path` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `creative_opportunities.slot.gam_unit_path` a template with a
`{section}` placeholder derived from the request path, so one slot rule serves
all site sections instead of one rule per (slot × section).

**Architecture:** Parse each slot's `gam_unit_path` into a cached template at
startup (alongside the existing compiled-glob cache); reject malformed templates
and a `{section}` template missing its `section_root`. At request time derive
`{section}` from the raw path (sanitized) and render the template inside
`build_slot_json`, which gains a `request_path` argument. Server-only — the
client keeps receiving a resolved `gam_unit_path` string, so no JS change.

**Tech Stack:** Rust 2024, `trusted-server-core`. Tests via `cargo test_details`
(native host, `aarch64-apple-darwin`) for iteration and `cargo test-fastly`
(core + fastly on `wasm32-wasip1` via Viceroy) for the CI gate.

**Spec:** `docs/superpowers/specs/2026-07-23-per-section-gam-unit-path-design.md`

**Issue:** https://github.com/IABTechLab/trusted-server/issues/954

---

## File Structure

- Modify: `crates/trusted-server-core/src/creative_opportunities.rs`
  - new: `UnitTemplatePart` enum, `parse_unit_template`, `sanitize_section`,
    `derive_section`
  - new on `CreativeOpportunitySlot`: `compiled_unit` field,
    `compile_unit_template`, `render_gam_unit_path`, `template_uses_section`
  - new on `CreativeOpportunitiesConfig`: `section_root` field,
    `compile_unit_templates`; extend `validate_runtime`
  - unit tests in the existing `#[cfg(test)] mod tests`
- Modify: `crates/trusted-server-core/src/publisher.rs`
  - `build_slot_json` gains `request_path: &str`; renders via `render_gam_unit_path`
  - `build_ad_slots_script` gains `request_path: &str`; threads it through
  - `handle_page_bids` passes its normalized `path` to `build_slot_json`
- Modify: `crates/trusted-server-core/src/settings.rs`
  - `prepare_runtime` calls `compile_unit_templates` and surfaces parse errors
- Modify: `docs/guide/configuration.md` (add creative_opportunities section)
- Modify: `trusted-server.example.toml` and the live example config

Notes on lifecycle: `page_patterns` inheritance is **out of scope** (sibling
issue). Templates are parsed at startup and cached with `#[serde(skip)]`,
mirroring the existing `compiled_patterns` field.

---

## Task 1: Template parser

**Files:**

- Modify: `crates/trusted-server-core/src/creative_opportunities.rs`
- Test: same file, `#[cfg(test)] mod tests`

- [ ] **Step 1: Write the failing tests**

Add to `mod tests`:

```rust
#[test]
fn parse_unit_template_accepts_known_placeholders() {
    let parts = parse_unit_template("/{network_id}/example/{section}")
        .expect("should parse valid template");
    assert_eq!(parts.len(), 4, "should split into literal+ph+literal+ph");
}

#[test]
fn parse_unit_template_accepts_static_path() {
    let parts = parse_unit_template("/99999/example/homepage")
        .expect("should parse a static path as a single literal");
    assert!(
        matches!(parts.as_slice(), [UnitTemplatePart::Literal(s)] if s == "/99999/example/homepage"),
        "should be one literal part"
    );
}

#[test]
fn parse_unit_template_rejects_unknown_placeholder() {
    let err = parse_unit_template("/{network_id}/{oops}").expect_err("should reject unknown placeholder");
    assert!(err.contains("oops"), "error should name the bad placeholder");
}

#[test]
fn parse_unit_template_rejects_unmatched_brace() {
    parse_unit_template("/{network_id}/{section").expect_err("should reject unmatched '{'");
    parse_unit_template("/a}b").expect_err("should reject stray '}'");
}

#[test]
fn parse_unit_template_rejects_nested_brace() {
    parse_unit_template("/{net{work}_id}").expect_err("should reject nested '{'");
}

#[test]
fn parse_unit_template_rejects_empty() {
    parse_unit_template("").expect_err("should reject empty template");
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test_details -p trusted-server-core creative_opportunities::tests::parse_unit_template`
Expected: FAIL — `cannot find function parse_unit_template` / `UnitTemplatePart`.

- [ ] **Step 3: Implement the enum + parser**

Add near the top of the module body (after imports):

```rust
/// A single parsed segment of a `gam_unit_path` template.
#[derive(Debug, Clone)]
pub(crate) enum UnitTemplatePart {
    /// Verbatim text between placeholders.
    Literal(String),
    /// `{network_id}` — replaced with the GAM network id.
    NetworkId,
    /// `{section}` — replaced with the request-derived section.
    Section,
    /// `{slot_id}` — replaced with the slot id.
    SlotId,
}

/// Parses a `gam_unit_path` template into an ordered list of parts.
///
/// # Errors
///
/// Returns an error string for an empty template, an unmatched or nested `{`,
/// a stray `}`, or an unknown placeholder name.
fn parse_unit_template(raw: &str) -> Result<Vec<UnitTemplatePart>, String> {
    if raw.is_empty() {
        return Err("gam_unit_path template must not be empty".to_string());
    }
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' => {
                if !literal.is_empty() {
                    parts.push(UnitTemplatePart::Literal(std::mem::take(&mut literal)));
                }
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some('{') => {
                            return Err(format!("nested '{{' in template `{raw}`"));
                        }
                        Some(ch) => name.push(ch),
                        None => return Err(format!("unmatched '{{' in template `{raw}`")),
                    }
                }
                match name.as_str() {
                    "network_id" => parts.push(UnitTemplatePart::NetworkId),
                    "section" => parts.push(UnitTemplatePart::Section),
                    "slot_id" => parts.push(UnitTemplatePart::SlotId),
                    other => {
                        return Err(format!(
                            "unknown placeholder `{{{other}}}` in template `{raw}`"
                        ));
                    }
                }
            }
            '}' => return Err(format!("stray '}}' in template `{raw}`")),
            other => literal.push(other),
        }
    }
    if !literal.is_empty() {
        parts.push(UnitTemplatePart::Literal(literal));
    }
    Ok(parts)
}
```

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test_details -p trusted-server-core creative_opportunities::tests::parse_unit_template`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/creative_opportunities.rs
git commit -m "Add gam_unit_path template parser"
```

---

## Task 2: Section derivation

**Files:**

- Modify: `crates/trusted-server-core/src/creative_opportunities.rs`
- Test: same file

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn derive_section_uses_first_segment() {
    assert_eq!(derive_section("/news", "home"), "news");
    assert_eq!(derive_section("/news/article-123", "home"), "news");
    assert_eq!(derive_section("/my-section/x", "home"), "my-section");
}

#[test]
fn derive_section_uses_root_when_no_segment() {
    assert_eq!(derive_section("/", "homepage"), "homepage");
    assert_eq!(derive_section("///", "homepage"), "homepage");
}

#[test]
fn derive_section_sanitizes_unsafe_runs_to_single_underscore() {
    // Not decoded: in "new%20s" only '%' is disallowed ('2' and '0' are
    // alphanumeric), so it collapses to a single '_' -> "new_20s". This is
    // exactly the no-decode contract: had we decoded, %20 would be a space and
    // yield "new_s"; we do NOT decode.
    assert_eq!(derive_section("/new%20s", "home"), "new_20s");
    // A run of disallowed chars collapses to one '_'.
    assert_eq!(derive_section("/a..b", "home"), "a_b");
}

#[test]
fn derive_section_is_non_empty_for_all_disallowed_segment() {
    assert_eq!(derive_section("/%%%/x", "home"), "_");
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test_details -p trusted-server-core creative_opportunities::tests::derive_section`
Expected: FAIL — `cannot find function derive_section`.

- [ ] **Step 3: Implement the two functions**

```rust
/// Collapses each run of characters outside `[A-Za-z0-9_-]` to a single `_`.
///
/// Returns a non-empty string for any non-empty input.
fn sanitize_section(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    let mut in_bad_run = false;
    for ch in segment.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            in_bad_run = false;
        } else if !in_bad_run {
            out.push('_');
            in_bad_run = true;
        }
    }
    out
}

/// Derives the `{section}` value from a request path.
///
/// Uses the first non-empty path segment, sanitized to `[A-Za-z0-9_-]`. Falls
/// back to `section_root` when the path has no segment (`/`, repeated slashes).
/// The path is used **raw** (not percent-decoded) so this stays consistent with
/// how `page_patterns` glob-match the same path.
pub(crate) fn derive_section(path: &str, section_root: &str) -> String {
    match path.split('/').find(|segment| !segment.is_empty()) {
        Some(segment) => sanitize_section(segment),
        None => section_root.to_string(),
    }
}
```

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test_details -p trusted-server-core creative_opportunities::tests::derive_section`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/creative_opportunities.rs
git commit -m "Add request-path section derivation"
```

---

## Task 3: Config field, template compile + render, startup validation

**Files:**

- Modify: `crates/trusted-server-core/src/creative_opportunities.rs`
- Test: same file

- [ ] **Step 1: Write the failing tests**

```rust
// NOTE: the existing helper signature is `make_slot(id: &str, patterns: Vec<&str>)`
// (see creative_opportunities.rs:443) — pass `vec![...]`, not `&[...]`.
#[test]
fn render_gam_unit_path_substitutes_placeholders() {
    let mut slot = make_slot("ad-header-0", vec!["/news/*"]);
    slot.gam_unit_path = Some("/{network_id}/example/{section}".to_string());
    slot.compile_unit_template().expect("should compile template");
    assert_eq!(
        slot.render_gam_unit_path("99999", "news"),
        "/99999/example/news"
    );
}

#[test]
fn render_gam_unit_path_defaults_when_no_template() {
    let mut slot = make_slot("sidebar", vec!["/*"]);
    slot.gam_unit_path = None;
    slot.compile_unit_template().expect("should compile (no template)");
    assert_eq!(slot.render_gam_unit_path("99999", "ignored"), "/99999/sidebar");
}

#[test]
fn render_gam_unit_path_uses_static_template_verbatim() {
    let mut slot = make_slot("atf", vec!["/"]);
    slot.gam_unit_path = Some("/99999/example/homepage".to_string());
    slot.compile_unit_template().expect("should compile static template");
    assert_eq!(slot.render_gam_unit_path("99999", "news"), "/99999/example/homepage");
}

#[test]
fn validate_runtime_requires_section_root_when_template_uses_section() {
    let mut config = make_config_with_section_template(None); // section_root = None
    config.compile_slots();
    config.compile_unit_templates().expect("templates compile");
    let err = config.validate_runtime().expect_err("should require section_root");
    assert!(err.contains("section_root"), "error should mention section_root");
}

#[test]
fn validate_runtime_rejects_invalid_section_root() {
    let mut config = make_config_with_section_template(Some("has space"));
    config.compile_slots();
    config.compile_unit_templates().expect("templates compile");
    config.validate_runtime().expect_err("should reject non [A-Za-z0-9_-] root");
}

#[test]
fn validate_runtime_accepts_section_template_with_valid_root() {
    let mut config = make_config_with_section_template(Some("homepage"));
    config.compile_slots();
    config.compile_unit_templates().expect("templates compile");
    config.validate_runtime().expect("should accept valid section_root");
}

#[test]
fn compile_unit_templates_surfaces_parse_error() {
    let mut config = make_config_with_section_template(Some("home"));
    config.slot[0].gam_unit_path = Some("/{bad}".to_string());
    config.compile_slots();
    config.compile_unit_templates().expect_err("should surface unknown-placeholder error");
}
```

Add test helpers to `mod tests` if not present (adapt to the existing helper
style in this module):

```rust
fn make_config_with_section_template(section_root: Option<&str>) -> CreativeOpportunitiesConfig {
    let mut slot = make_slot("ad-header-0", vec!["/news/*"]);
    slot.gam_unit_path = Some("/{network_id}/example/{section}".to_string());
    CreativeOpportunitiesConfig {
        gam_network_id: "99999".to_string(),
        auction_timeout_ms: None,
        price_granularity: PriceGranularity::default(),
        section_root: section_root.map(str::to_string),
        slot: vec![slot],
    }
}
```

The `make_slot(id: &str, patterns: Vec<&str>)` helper **already exists** at
`creative_opportunities.rs:443` and constructs a `CreativeOpportunitySlot` via
struct-literal syntax. Because the struct uses `#[serde(deny_unknown_fields)]`
and the helper names every field explicitly, adding `compiled_unit` to the
struct makes this helper fail to compile until updated — see Step 3's helper-fix
sub-step.

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test_details -p trusted-server-core creative_opportunities::tests`
Expected: FAIL — missing `section_root`, `compiled_unit`, `compile_unit_template`,
`render_gam_unit_path`, `compile_unit_templates`.

- [ ] **Step 3: Add the field, cache, methods, and validation**

On `CreativeOpportunitiesConfig` (add field):

```rust
/// Value substituted for `{section}` when the request path has no first
/// segment (e.g. `/`). Required when any slot's `gam_unit_path` template
/// contains `{section}`. No default — a home-section name is publisher-specific.
#[serde(default)]
pub section_root: Option<String>,
```

On `CreativeOpportunitySlot` (add cached template, parallel to `compiled_patterns`):

```rust
/// Pre-parsed [`gam_unit_path`](Self::gam_unit_path) template, populated by
/// [`compile_unit_template`](Self::compile_unit_template) at startup. `None`
/// when the slot has no explicit `gam_unit_path` (uses the default path).
#[serde(skip, default)]
pub(crate) compiled_unit: Option<Vec<UnitTemplatePart>>,
```

Slot methods:

```rust
/// Parses [`gam_unit_path`](Self::gam_unit_path) into [`compiled_unit`](Self::compiled_unit).
///
/// # Errors
///
/// Returns an error string when the template is malformed (see
/// [`parse_unit_template`]).
pub fn compile_unit_template(&mut self) -> Result<(), String> {
    self.compiled_unit = match &self.gam_unit_path {
        Some(raw) => Some(parse_unit_template(raw).map_err(|e| format!("slot `{}`: {e}", self.id))?),
        None => None,
    };
    Ok(())
}

/// Renders the resolved GAM unit path for a given network id and section.
///
/// Uses the parsed template when present, otherwise the default
/// `/<network_id>/<id>`.
#[must_use]
pub fn render_gam_unit_path(&self, gam_network_id: &str, section: &str) -> String {
    match &self.compiled_unit {
        Some(parts) => parts
            .iter()
            .map(|part| match part {
                UnitTemplatePart::Literal(s) => s.as_str(),
                UnitTemplatePart::NetworkId => gam_network_id,
                UnitTemplatePart::Section => section,
                UnitTemplatePart::SlotId => self.id.as_str(),
            })
            .collect(),
        None => format!("/{}/{}", gam_network_id, self.id),
    }
}

/// Returns `true` if this slot's compiled template contains `{section}`.
#[must_use]
pub(crate) fn template_uses_section(&self) -> bool {
    self.compiled_unit
        .as_ref()
        .is_some_and(|parts| parts.iter().any(|p| matches!(p, UnitTemplatePart::Section)))
}
```

On `CreativeOpportunitiesConfig` (compile all templates + extend validation):

```rust
/// Parse every slot's `gam_unit_path` template. Call once after deserialization.
///
/// # Errors
///
/// Returns an error string when any slot's template is malformed.
pub fn compile_unit_templates(&mut self) -> Result<(), String> {
    for slot in &mut self.slot {
        slot.compile_unit_template()?;
    }
    Ok(())
}
```

In `validate_runtime`, after the existing per-slot loop, add:

```rust
if self.slot.iter().any(CreativeOpportunitySlot::template_uses_section) {
    match self.section_root.as_deref() {
        Some(root)
            if !root.is_empty()
                && root.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') => {}
        _ => {
            return Err(
                "section_root is required and must match [A-Za-z0-9_-]+ when a \
                 gam_unit_path template uses {section}"
                    .to_string(),
            );
        }
    }
}
```

Remove the old path-render emptiness check in `validate_runtime`
(the block calling `resolved_gam_unit_path(...).trim().is_empty()`); malformed or
empty templates are now caught at parse time by `compile_unit_templates`, and a
rendered result is non-empty by construction.

**Update the existing test helper (required — adding `compiled_unit` breaks it):**
Add `compiled_unit: None` to the `CreativeOpportunitySlot` struct-literal in
`make_slot` at `crates/trusted-server-core/src/creative_opportunities.rs:443`.
The struct uses `#[serde(deny_unknown_fields)]` and the helper names every field,
so a missing field is a compile error, not a `#[serde(default)]` fill-in.

- [ ] **Step 4: Run tests, verify they pass**

Run: `cargo test_details -p trusted-server-core creative_opportunities::tests`
Expected: PASS (Task 1–3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/trusted-server-core/src/creative_opportunities.rs
git commit -m "Add section_root, unit-template compile/render, and startup validation"
```

---

## Task 4: Render at request time (thread the path through publisher.rs)

**Files:**

- Modify: `crates/trusted-server-core/src/creative_opportunities.rs` (remove now-unused `resolved_gam_unit_path`, or keep if other callers remain — grep first)
- Modify: `crates/trusted-server-core/src/publisher.rs`
- Modify: `crates/trusted-server-core/src/settings.rs`
- Test: `crates/trusted-server-core/src/publisher.rs` `#[cfg(test)] mod tests`

- [ ] **Step 0: Update publisher.rs struct-literal test helpers (required — new fields break them)**

Adding `section_root` to `CreativeOpportunitiesConfig` and `compiled_unit` to
`CreativeOpportunitySlot` breaks every hand-built literal in `publisher.rs`
tests. Add the new fields to each:

- `crates/trusted-server-core/src/publisher.rs:4272` — `make_config()`: add `section_root: None`.
- `crates/trusted-server-core/src/publisher.rs:4282` — `make_slot()`: add `compiled_unit: None`.
- `crates/trusted-server-core/src/publisher.rs:4931` — `article_slot()`: add `compiled_unit: None`.
- `crates/trusted-server-core/src/publisher.rs:5433` — `article_slot()` (second module): add `compiled_unit: None`.

Run: `cargo test_details -p trusted-server-core publisher:: --no-run`
Expected: compiles (no `missing field` errors) before writing the new test.

- [ ] **Step 1: Write the failing test (equivalence + per-section)**

In `publisher.rs` tests, add (adapt to the existing test helpers/config builders
in that module):

```rust
#[test]
fn build_slot_json_renders_section_from_request_path() {
    let config = creative_opportunities_config_with_template(); // gam_unit_path = "/{network_id}/example/{section}", section_root = "homepage"
    let slot = &config.slot[0];

    let news = build_slot_json(slot, &config, "/news/article-123");
    assert_eq!(news["gam_unit_path"], "/99999/example/news");

    let home = build_slot_json(slot, &config, "/");
    assert_eq!(home["gam_unit_path"], "/99999/example/homepage");
}
```

- [ ] **Step 2: Run test, verify it fails**

Run: `cargo test_details -p trusted-server-core publisher::tests::build_slot_json_renders_section`
Expected: FAIL — `build_slot_json` takes 2 args / wrong unit value.

- [ ] **Step 3: Thread `request_path` and render**

In `build_slot_json` (`crates/trusted-server-core/src/publisher.rs` ~2204):

```rust
fn build_slot_json(
    slot: &crate::creative_opportunities::CreativeOpportunitySlot,
    co_config: &crate::creative_opportunities::CreativeOpportunitiesConfig,
    request_path: &str,
) -> serde_json::Value {
    let section = crate::creative_opportunities::derive_section(
        request_path,
        co_config.section_root.as_deref().unwrap_or_default(),
    );
    let gam_path = slot.render_gam_unit_path(&co_config.gam_network_id, &section);
    // ...rest unchanged (div_id, formats, targeting, json!)...
}
```

In `build_ad_slots_script` (~2233) add `request_path: &str` and pass it:

```rust
pub(crate) fn build_ad_slots_script(
    matched_slots: &[crate::creative_opportunities::CreativeOpportunitySlot],
    co_config: &crate::creative_opportunities::CreativeOpportunitiesConfig,
    request_path: &str,
) -> String {
    let slots: Vec<serde_json::Value> = matched_slots
        .iter()
        .map(|slot| build_slot_json(slot, co_config, request_path))
        .collect();
    // ...unchanged...
}
```

At the initial-render caller (~publisher.rs:1791) pass `&request_path`:

```rust
.map(|co_config| build_ad_slots_script(&matched_slots, co_config, &request_path))
```

In `handle_page_bids` (~2562) pass the already-normalized path
(`path_param` / the value from `normalize_page_bids_path`) to `build_slot_json`:

```rust
.map(|slot| build_slot_json(slot, co_config, &path_param))
```

Update any existing `build_ad_slots_script(...)` / `build_slot_json(...)` test
call sites in `publisher.rs` to pass a path argument (e.g. `"/"`).

- [ ] **Step 4: Update `settings.rs::prepare_runtime`**

In `crates/trusted-server-core/src/settings.rs` (~2078), compile templates and
surface parse errors:

```rust
if let Some(co) = &mut self.creative_opportunities {
    co.compile_slots();
    co.compile_unit_templates().map_err(|err| {
        Report::new(TrustedServerError::Configuration {
            message: format!("Invalid creative opportunity gam_unit_path template: {err}"),
        })
    })?;
    co.validate_runtime().map_err(|err| {
        Report::new(TrustedServerError::Configuration {
            message: format!("Invalid creative opportunity slot config: {err}"),
        })
    })?;
}
```

- [ ] **Step 5: Run tests, verify they pass**

Run: `cargo test_details -p trusted-server-core publisher::tests`
Expected: PASS.

- [ ] **Step 6: Fix the existing empty-`gam_unit_path` settings test if needed**

`settings.rs::settings_rejects_creative_opportunity_slot_with_empty_gam_unit_path`
now fails at template-parse (empty template) rather than the render check. Verify
it still asserts rejection; update the expected error substring if it pins a
message.

Run: `cargo test_details -p trusted-server-core settings::`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/creative_opportunities.rs crates/trusted-server-core/src/settings.rs
git commit -m "Render gam_unit_path template per request across initial and SPA paths"
```

---

## Task 5: Docs + config

**Files:**

- Modify: `docs/guide/configuration.md`
- Modify: `trusted-server.example.toml`
- Modify: the live example `trusted-server.toml` (operator-owned, gitignored — update locally, do not commit)

- [ ] **Step 1: Add a creative_opportunities section to configuration.md**

Cover: the placeholder set (`{network_id}`, `{section}`, `{slot_id}`); section
derivation (first path segment, sanitized to `[A-Za-z0-9_-]`, raw/undecoded);
`section_root` requirement and validation; behavior on an unmatched route (no
slot, template never rendered); back-compat (static path used verbatim; no
`gam_unit_path` → `/<network_id>/<slot_id>`). Use fictional values
(`example.com`, network `99999`) per the repo's docs rule.

- [ ] **Step 2: Update `trusted-server.example.toml`**

Show one templated slot with `section_root` and a `{section}` `gam_unit_path`,
using fictional values.

- [ ] **Step 3: Docs format check**

Run: `cd docs && npm run format`
Expected: no diff / formatting clean.

- [ ] **Step 4: Commit**

```bash
git add docs/guide/configuration.md trusted-server.example.toml
git commit -m "Document per-section gam_unit_path templating"
```

---

## Task 6: Full verification (CI gate)

- [ ] **Step 1: Format**

Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 2: Core + Fastly tests under Viceroy (full module, not filtered)**

Run: `cargo test-fastly`
Expected: PASS. (Runs the full creative_opportunities + publisher test modules on
`wasm32-wasip1`; a format-changing edit can hide later failures when filtered, so
run the whole suite here.)

- [ ] **Step 3: Other adapters (no behavior change expected, guard against signature breaks)**

Run: `cargo test-axum && cargo test-cloudflare && cargo test-spin`
Expected: PASS.

- [ ] **Step 4: Clippy across adapter targets**

Run: `cargo clippy-fastly && cargo clippy-axum && cargo clippy-cloudflare && cargo clippy-cloudflare-wasm && cargo clippy-spin-native && cargo clippy-spin-wasm`
Expected: no warnings.

- [ ] **Step 5: JS unaffected (sanity)**

Run: `cd crates/trusted-server-js/lib && npx vitest run`
Expected: PASS (no JS change; confirms wire shape unbroken).

- [ ] **Step 6: Final commit if any fixups**

```bash
git add -A
git commit -m "Fix clippy/fmt for per-section gam_unit_path"
```

---

## Acceptance criteria mapping

- **N slots × M sections without N×M rules** — Task 1–4 (one templated slot rule
  serves all sections).
- **Resolution tested (`/`, single/multi-segment, no-match, encoded)** — Task 2
  tests + Task 4 equivalence + the unmatched-route case (no slot matched → no
  `build_slot_json` call; covered by existing `match_slots` empty tests).
- **Existing static configs unchanged** — Task 3 `render_gam_unit_path` verbatim
  - default tests.
- **Startup catches empty/unknown/malformed template + missing/invalid
  `section_root`** — Task 1 + Task 3 validation tests.
- **`{section}` sanitized, raw path** — Task 2 tests.
- **Documented** — Task 5.
