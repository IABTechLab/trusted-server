# Server-Side Ad Template CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the unified `ts` CLI support for server-side ad-template static diagnostics and browser-backed verification described in `docs/superpowers/specs/2026-06-26-server-side-ad-template-cli-design.md`.

**Architecture:** Keep the CLI host-only and thin: Clap parsing stays in `run.rs` / command adapter modules, shared app-config loading moves to `app_config.rs`, pure ad-template logic lives under `ad_templates/`, and Chrome/Chromium collection lives under `audit/`. Runtime gate rules are extracted into a small pure helper in `trusted-server-core` so the CLI does not duplicate server behavior.

**Tech Stack:** Rust 2024 workspace, host-target `trusted-server-cli`, `clap`, EdgeZero typed app-config loader, `serde`/`serde_json` for stable JSON, `chromiumoxide` for browser-backed audit collection, local HTML fixture tests, and existing `trusted-server-core::creative_opportunities` matching.

---

## Current State

- Branch: `feature/ts-cli-ad-templates`.
- Static ad-template commands already exist in `crates/trusted-server-cli/src/config_ad_templates.rs`.
- The current branch does not contain #800 audit files. Port useful #800 pieces into the current #799 code shape; do not resurrect stale `args.rs` or `config_command.rs`.
- The spec was updated after review and is the source of truth:
  `docs/superpowers/specs/2026-06-26-server-side-ad-template-cli-design.md`.
- Keep `.env` and operator-owned `trusted-server.toml` out of commits.

## File Map

### New files

| File                                                           | Responsibility                                                                                           |
| -------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-cli/src/app_config.rs`                  | Shared effective app-config loader and shared `AppConfigArgs`.                                           |
| `crates/trusted-server-cli/src/ad_templates/mod.rs`            | Re-export focused ad-template CLI modules.                                                               |
| `crates/trusted-server-cli/src/ad_templates/expected.rs`       | Path/URL normalization and expected-slot projection from runtime slot matching.                          |
| `crates/trusted-server-cli/src/ad_templates/compare.rs`        | Pure DOM/GPT/APS evidence comparison, statuses, warnings, runtime gate output, strict failure decisions. |
| `crates/trusted-server-cli/src/ad_templates/output.rs`         | Human and JSON rendering for static diagnostics and browser verification.                                |
| `crates/trusted-server-cli/src/audit/mod.rs`                   | Audit namespace entry point.                                                                             |
| `crates/trusted-server-cli/src/audit/page.rs`                  | Generic page audit command ported from #800.                                                             |
| `crates/trusted-server-cli/src/audit/collector.rs`             | Browser collector trait plus collected page/evidence structs.                                            |
| `crates/trusted-server-cli/src/audit/browser.rs`               | Chromiumoxide-backed browser collector, init scripts, optional scroll, page-level collection errors.     |
| `crates/trusted-server-cli/src/audit/ad_templates.rs`          | `ts audit ad-templates verify` orchestration.                                                            |
| `crates/trusted-server-cli/src/audit/ad_template_collector.js` | Read-only init script for GPT/APS/DOM evidence collection, included via `include_str!`.                  |

### Modified files

| File                                                       | Change summary                                                                                                  |
| ---------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- |
| `Cargo.toml`                                               | Add workspace dependencies missing from this branch: `chromiumoxide`, `serde`, and `serde_json` if not present. |
| `crates/trusted-server-cli/Cargo.toml`                     | Add host-only CLI dependencies for browser audit and JSON output.                                               |
| `crates/trusted-server-cli/src/lib.rs`                     | Register new `app_config`, `ad_templates`, and `audit` modules under `cfg(not(target_arch = "wasm32"))`.        |
| `crates/trusted-server-cli/src/run.rs`                     | Add `Audit` command namespace, parser tests, and dispatch.                                                      |
| `crates/trusted-server-cli/src/config_ad_templates.rs`     | Shrink to Clap adapter using shared loader/expected/output modules.                                             |
| `crates/trusted-server-core/src/creative_opportunities.rs` | Add pure runtime gate helper types/functions shared by runtime and CLI.                                         |
| `crates/trusted-server-core/src/publisher.rs`              | Route existing server-side ad-stack gate through the shared helper without changing behavior.                   |

## Implementation Rules

- Use TDD for each task: write a failing test first, run it, implement the minimal code, re-run, then commit.
- Commit after each task using repo style: sentence case, imperative, no semantic prefix.
- Keep `trusted-server-cli` host-only. Do not introduce `tokio`, `chromiumoxide`, or filesystem/browser dependencies into core runtime or wasm adapter crates.
- Do not write real publisher domains or secrets in tests. Use `example.com`, `publisher.example`, and fictional IDs only.
- Prefer pure module tests over browser tests. Browser-backed fixture tests should use local HTML only and no GPT/APS network.

## Task 0: Baseline And Branch Hygiene

**Files:**

- Verify: `docs/superpowers/specs/2026-06-26-server-side-ad-template-cli-design.md`
- Verify: `docs/superpowers/plans/2026-06-26-server-side-ad-template-cli.md`

- [ ] **Step 1: Confirm branch and working tree**

  Run:

  ```bash
  git status --short --branch
  git log --oneline --decorate -5
  ```

  Expected: on `feature/ts-cli-ad-templates`; no unrelated modified files besides the approved spec/plan docs.

- [ ] **Step 2: Run docs format check before code work**

  Run:

  ```bash
  cd docs && npm run format
  ```

  Expected: `All matched files use Prettier code style!`

- [ ] **Step 3: Commit reviewed spec and plan**

  Run:

  ```bash
  git add docs/superpowers/specs/2026-06-26-server-side-ad-template-cli-design.md docs/superpowers/plans/2026-06-26-server-side-ad-template-cli.md
  git commit -m "Add server-side ad-template CLI implementation plan"
  ```

  Expected: docs-only commit. If the spec commit already exists separately, commit only the plan.

## Task 1: Share Runtime Ad-Stack Gate Logic

**Files:**

- Modify: `crates/trusted-server-core/src/creative_opportunities.rs`
- Modify: `crates/trusted-server-core/src/publisher.rs`

- [ ] **Step 1: Write failing core tests for the shared gate helper**

  Add tests near the existing `creative_opportunities` tests:

  ```rust
  #[test]
  fn ad_stack_gate_passes_for_eligible_navigation() {
      let result = evaluate_ad_stack_gate(AdStackGateInput {
          method_get: true,
          navigation: true,
          prefetch: false,
          bot: false,
          matched_slots: true,
          consent_allows_auction: Some(true),
          auction_enabled: true,
      });

      assert_eq!(result.expected, RuntimeAdStackExpected::Yes);
      assert!(result.blocking_gates().is_empty());
  }

  #[test]
  fn ad_stack_gate_blocks_known_kill_switch() {
      let result = evaluate_ad_stack_gate(AdStackGateInput {
          method_get: true,
          navigation: true,
          prefetch: false,
          bot: false,
          matched_slots: true,
          consent_allows_auction: Some(true),
          auction_enabled: false,
      });

      assert_eq!(result.expected, RuntimeAdStackExpected::No);
      assert!(result.blocking_gates().contains(&AdStackGateName::AuctionEnabled));
  }

  #[test]
  fn ad_stack_gate_is_unknown_when_consent_is_unknown() {
      let result = evaluate_ad_stack_gate(AdStackGateInput {
          method_get: true,
          navigation: true,
          prefetch: false,
          bot: false,
          matched_slots: true,
          consent_allows_auction: None,
          auction_enabled: true,
      });

      assert_eq!(result.expected, RuntimeAdStackExpected::Unknown);
  }

  // Locks the spec §5.2 mirror invariant: with Some(consent) supplied for every
  // input combination, `expected == Yes` must equal the legacy all-AND boolean.
  #[test]
  fn ad_stack_gate_with_known_consent_matches_legacy_boolean() {
      for bits in 0u8..64 {
          let input = AdStackGateInput {
              method_get: bits & 1 != 0,
              navigation: bits & 2 != 0,
              prefetch: bits & 4 != 0,
              bot: bits & 8 != 0,
              matched_slots: bits & 16 != 0,
              consent_allows_auction: Some(bits & 32 != 0),
              auction_enabled: bits & 1 == 0,
          };
          // Legacy semantics: all positive gates true, both negative gates false.
          let legacy = input.method_get
              && input.navigation
              && !input.prefetch
              && !input.bot
              && input.matched_slots
              && input.consent_allows_auction == Some(true)
              && input.auction_enabled;
          let got = evaluate_ad_stack_gate(input).expected == RuntimeAdStackExpected::Yes;
          assert_eq!(got, legacy, "gate mismatch for bits={bits}");
      }
  }
  ```

- [ ] **Step 2: Run the focused test and verify it fails**

  Run:

  ```bash
  # NOTE: trusted-server-core links the `fastly` crate and CANNOT build for the host
  # triple — run core tests on the DEFAULT target (wasm32-wasip1 + viceroy runner),
  # i.e. no `--target`. Only the host-only `trusted-server-cli` uses `--target <host>`.
  cargo test -p trusted-server-core creative_opportunities::tests::ad_stack_gate
  ```

  Expected: compile failure because `AdStackGateInput` / `evaluate_ad_stack_gate` do not exist.

- [ ] **Step 3: Implement pure gate types and helper**

  Add public, serde-free types to `creative_opportunities.rs`:

  ```rust
  #[derive(Debug, Clone, Copy, Eq, PartialEq)]
  pub enum RuntimeAdStackExpected {
      Yes,
      No,
      Unknown,
  }

  #[derive(Debug, Clone, Copy, Eq, PartialEq)]
  pub enum AdStackGateName {
      MethodGet,
      Navigation,
      NotPrefetch,
      NotBot,
      MatchedSlots,
      ConsentAllowsAuction,
      AuctionEnabled,
  }

  #[derive(Debug, Clone, Copy)]
  pub struct AdStackGateInput {
      pub method_get: bool,
      pub navigation: bool,
      pub prefetch: bool,
      pub bot: bool,
      pub matched_slots: bool,
      pub consent_allows_auction: Option<bool>,
      pub auction_enabled: bool,
  }

  #[derive(Debug, Clone, Eq, PartialEq)]
  pub struct AdStackGateResult {
      pub expected: RuntimeAdStackExpected,
      blocking_gates: Vec<AdStackGateName>,
  }

  impl AdStackGateResult {
      pub fn blocking_gates(&self) -> &[AdStackGateName] {
          &self.blocking_gates
      }
  }
  ```

  Implement `evaluate_ad_stack_gate(input)` so any known blocking boolean gate returns `No`, all known pass plus `Some(true)` consent returns `Yes`, and all known pass plus `None` consent returns `Unknown`.

  Mind the gate polarity, mirroring `should_run_server_side_ad_stack`: `method_get`,
  `navigation`, `matched_slots`, and `auction_enabled` block when **false**, while
  `prefetch` and `bot` block when **true** (their gate names `NotPrefetch` / `NotBot`
  pass when the input bool is false). `consent_allows_auction` is the only tri-state
  input: `Some(false)` blocks (No), `Some(true)` passes, `None` yields Unknown only
  when no other gate already blocks.

- [ ] **Step 4: Route `publisher.rs` through the helper**

  Replace the body of `should_run_server_side_ad_stack` with a call to `evaluate_ad_stack_gate`, preserving the existing function signature for low-risk runtime compatibility:

  ```rust
  crate::creative_opportunities::evaluate_ad_stack_gate(
      crate::creative_opportunities::AdStackGateInput {
          method_get: is_get,
          navigation: is_navigation,
          prefetch: is_prefetch,
          bot: is_bot,
          matched_slots: has_matched_slots,
          consent_allows_auction: Some(consent_allows_auction),
          auction_enabled,
      },
  )
  .expected
      == crate::creative_opportunities::RuntimeAdStackExpected::Yes
  ```

- [ ] **Step 5: Run focused tests**

  Run:

  ```bash
  # Core tests run on the default wasm target via viceroy (no --target).
  cargo test -p trusted-server-core publisher::tests
  cargo test -p trusted-server-core creative_opportunities
  ```

  Expected: all focused tests pass (including the existing `should_run_server_side_ad_stack` truth-table tests in `publisher::tests`).

- [ ] **Step 6: Commit**

  ```bash
  git add crates/trusted-server-core/src/creative_opportunities.rs crates/trusted-server-core/src/publisher.rs
  git commit -m "Share server-side ad stack gate evaluation"
  ```

## Task 2: Extract Shared CLI App Config Loader

**Files:**

- Create: `crates/trusted-server-cli/src/app_config.rs`
- Modify: `crates/trusted-server-cli/src/lib.rs`
- Modify: `crates/trusted-server-cli/src/config_ad_templates.rs`

- [ ] **Step 1: Write failing loader tests**

  Move the existing temp-project helpers from `config_ad_templates.rs` tests into `app_config.rs` tests and add:

  ```rust
  #[test]
  fn explicit_missing_app_config_does_not_fall_back() {
      let temp = TempDir::new().expect("should create temp dir");
      let manifest_path = temp.path().join("edgezero.toml");
      fs::write(&manifest_path, "[app]\nname = \"trusted-server\"\n")
          .expect("should write manifest");
      let missing_path = temp.path().join("missing.toml");

      let args = AppConfigArgs {
          app_config: Some(missing_path.clone()),
          manifest: manifest_path,
          no_env: true,
      };

      let err = load_settings(&args).expect_err("should reject missing explicit config");
      assert!(
          err.contains(missing_path.to_string_lossy().as_ref()),
          "error should mention the explicit missing path"
      );
  }
  ```

- [ ] **Step 2: Run focused test and verify it fails**

  Run:

  ```bash
  cargo test -p trusted-server-cli app_config --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: compile failure because `app_config` module is not registered.

- [ ] **Step 3: Implement `app_config.rs`**

  Move these items out of `config_ad_templates.rs`:
  - `AppConfigArgs`
  - `LoadedSettings`
  - `load_settings`
  - `resolve_app_config_path`

  Make the API explicit:

  ```rust
  #[derive(Clone, Debug, Args)]
  pub struct AppConfigArgs {
      #[arg(long)]
      pub app_config: Option<PathBuf>,
      #[arg(long, default_value = "edgezero.toml")]
      pub manifest: PathBuf,
      #[arg(long)]
      pub no_env: bool,
  }

  pub struct LoadedSettings {
      pub app_config_path: PathBuf,
      pub settings: Settings,
  }

  pub fn load_settings(args: &AppConfigArgs) -> Result<LoadedSettings, String> {
      let manifest_loader = ManifestLoader::from_path(&args.manifest)
          .map_err(|err| format!("failed to load {}: {err}", args.manifest.display()))?;
      let app_name = manifest_loader.manifest().app.name.clone().ok_or_else(|| {
          format!(
              "{} has no [app].name; cannot resolve trusted-server.toml",
              args.manifest.display()
          )
      })?;
      let app_config_path =
          resolve_app_config_path(args.app_config.as_deref(), &args.manifest, &app_name);

      let mut opts = AppConfigLoadOptions::default();
      opts.env_overlay = !args.no_env;
      let app_config = app_config::deserialize_app_config_with_options::<TrustedServerAppConfig>(
          &app_config_path,
          &app_name,
          &opts,
      )
      .map_err(|err| format!("failed to load {}: {err}", app_config_path.display()))?;

      Ok(LoadedSettings {
          app_config_path,
          settings: app_config.into_settings(),
      })
  }

  fn resolve_app_config_path(
      explicit: Option<&Path>,
      manifest_path: &Path,
      app_name: &str,
  ) -> PathBuf {
      if let Some(path) = explicit {
          return path.to_path_buf();
      }
      let file_name = format!("{app_name}.toml");
      if let Some(parent) = manifest_path
          .parent()
          .filter(|parent| !parent.as_os_str().is_empty())
      {
          parent.join(file_name)
      } else {
          PathBuf::from(file_name)
      }
  }
  ```

  Include the same top-level imports currently used by these helpers:
  `std::path::{Path, PathBuf}`, `clap::Args`,
  `edgezero_core::app_config::{self, AppConfigLoadOptions}`,
  `edgezero_core::manifest::ManifestLoader`,
  `trusted_server_core::config::TrustedServerAppConfig`, and
  `trusted_server_core::settings::Settings`.

- [ ] **Step 4: Register module and update imports**

  In `lib.rs`, add:

  ```rust
  #[cfg(not(target_arch = "wasm32"))]
  mod app_config;
  ```

  In `config_ad_templates.rs`, import:

  ```rust
  use crate::app_config::{load_settings, AppConfigArgs};
  ```

- [ ] **Step 5: Run focused CLI tests**

  Run:

  ```bash
  cargo test -p trusted-server-cli config_ad_templates --target $(rustc -vV | sed -n 's/^host: //p')
  cargo test -p trusted-server-cli app_config --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: existing static command behavior remains unchanged.

- [ ] **Step 6: Commit**

  ```bash
  git add crates/trusted-server-cli/src/app_config.rs crates/trusted-server-cli/src/config_ad_templates.rs crates/trusted-server-cli/src/lib.rs
  git commit -m "Extract shared CLI app config loader"
  ```

## Task 3: Add Expected-Slot Model

**Files:**

- Create: `crates/trusted-server-cli/src/ad_templates/mod.rs`
- Create: `crates/trusted-server-cli/src/ad_templates/expected.rs`
- Modify: `crates/trusted-server-cli/Cargo.toml`
- Modify: `crates/trusted-server-cli/src/lib.rs`
- Modify: `crates/trusted-server-cli/src/config_ad_templates.rs`

- [ ] **Step 1: Write failing expected-slot tests**

  Add a test-only dependency to `crates/trusted-server-cli/Cargo.toml` so tests can
  deserialize core slot config instead of constructing `CreativeOpportunitySlot` with
  its `pub(crate)` `compiled_patterns` cache:

  ```toml
  [target.'cfg(not(target_arch = "wasm32"))'.dev-dependencies]
  toml = { workspace = true }
  ```

  In `expected.rs`, add tests for path normalization, full URL normalization, config-order preservation, resolved div ID, resolved GAM unit path, provider names, and matching page patterns:

  ```rust
  fn creative_config_with_slots(patterns: &[&str]) -> CreativeOpportunitiesConfig {
      let page_patterns = patterns
          .iter()
          .map(|pattern| format!("\"{pattern}\""))
          .collect::<Vec<_>>()
          .join(", ");
      let toml = format!(
          r#"
  gam_network_id = "123"
  auction_timeout_ms = 500
  price_granularity = "dense"

  [[slot]]
  id = "atf"
  gam_unit_path = "/123/news/atf"
  div_id = "ad-atf-"
  page_patterns = [{page_patterns}]
  formats = [{{ width = 300, height = 250 }}]
  floor_price = 0.50
  targeting = {{ zone = "atf" }}

  [slot.providers.prebid]
  bidders = {{}}
  "#
      );
      let mut config = toml::from_str::<CreativeOpportunitiesConfig>(&toml)
          .expect("should deserialize creative opportunities config");
      config.compile_slots();
      config
  }

  #[test]
  fn expected_slots_use_runtime_matcher_and_config_order() {
      let config = creative_config_with_slots(["/news/*", "/"].as_slice());
      let expected = expected_slots_for_path("/news/story", &config)
          .expect("should build expected slots");

      assert_eq!(expected.path, "/news/story");
      assert_eq!(expected.slots.iter().map(|slot| slot.id.as_str()).collect::<Vec<_>>(), ["atf"]);
      assert_eq!(expected.slots[0].div_id, "ad-atf-");
      assert_eq!(expected.slots[0].gam_unit_path, "/123/news/atf");
      assert_eq!(expected.slots[0].providers, ["prebid"]);
  }

  #[test]
  fn normalize_path_or_url_strips_query_and_fragment() {
      assert_eq!(normalize_path_or_url("https://www.example.com/news/story?x=1#top").expect("should normalize"), "/news/story");
      assert_eq!(normalize_path_or_url("news/story?x=1").expect("should normalize"), "/news/story");
  }
  ```

- [ ] **Step 2: Run focused test and verify it fails**

  Run:

  ```bash
  cargo test -p trusted-server-cli ad_templates::expected --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: compile failure because module/types do not exist.

- [ ] **Step 3: Implement expected-slot structs**

  Define pure structs that own strings and are stable for output:

  ```rust
  #[derive(Debug, Clone, PartialEq)]
  pub struct ExpectedSlots {
      pub path: String,
      pub slots: Vec<ExpectedSlot>,
  }

  #[derive(Debug, Clone, PartialEq)]
  pub struct ExpectedSlot {
      pub id: String,
      pub div_id: String,
      pub gam_unit_path: String,
      pub formats: Vec<ExpectedFormat>,
      pub providers: Vec<String>,
      pub page_patterns: Vec<String>,
  }

  #[derive(Debug, Clone, PartialEq)]
  pub struct ExpectedFormat {
      pub width: u32,
      pub height: u32,
      // Mirrors `MediaType` rendered as a stable string (`"banner"`, `"video"`, `"native"`).
      pub media_type: String,
  }
  ```

  `div_id` and `gam_unit_path` are resolved (non-optional) strings. The core
  `CreativeOpportunitySlot` stores `div_id` / `gam_unit_path` as `Option<String>`
  and the GAM unit path is composed with the configured GAM network ID; mirror the
  existing `format_slot` resolution in `config_ad_templates.rs` so the CLI does not
  invent a second resolution rule. Use
  `trusted_server_core::creative_opportunities::match_slots`. Do not compile globs in CLI.

- [ ] **Step 4: Register `ad_templates` and update static commands**

  In `lib.rs`, add:

  ```rust
  #[cfg(not(target_arch = "wasm32"))]
  mod ad_templates;
  ```

  Rewire `config_ad_templates.rs` onto the shared module, and remove the now-duplicated
  local code so there is no name collision or dead `normalize_path_or_url`:
  - delete the private `fn normalize_path_or_url` (currently `config_ad_templates.rs:448`)
    and add `use crate::ad_templates::expected::{expected_slots_for_path, normalize_path_or_url};`;
  - the existing `config_ad_templates::tests::normalizes_path_or_url_like_runtime_request_path`
    test (currently `:661`) calls the local fn via `super::*` — either delete it (Task 3
    Step 1 already adds normalization tests in `expected.rs`) or repoint it at
    `crate::ad_templates::expected::normalize_path_or_url`. Pick one so the test crate
    still compiles at this commit.

- [ ] **Step 5: Run focused tests**

  Run:

  ```bash
  cargo test -p trusted-server-cli ad_templates::expected --target $(rustc -vV | sed -n 's/^host: //p')
  cargo test -p trusted-server-cli config_ad_templates --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: all pass.

- [ ] **Step 6: Commit**

  ```bash
  git add crates/trusted-server-cli/Cargo.toml crates/trusted-server-cli/src/ad_templates/mod.rs crates/trusted-server-cli/src/ad_templates/expected.rs crates/trusted-server-cli/src/config_ad_templates.rs crates/trusted-server-cli/src/lib.rs
  git commit -m "Add shared ad-template expected slot model"
  ```

## Task 4: Add Stable Output And JSON Types

**Files:**

- Create: `crates/trusted-server-cli/src/ad_templates/output.rs`
- Modify: `crates/trusted-server-cli/Cargo.toml`
- Modify: `crates/trusted-server-cli/src/ad_templates/mod.rs`

- [ ] **Step 1: Add CLI JSON dependencies**

  The CLI crate has **no plain `[dependencies]` table** — every runtime dep lives
  under `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` (the workspace
  default build target is `wasm32-wasip1` per `.cargo/config.toml`). Add the new deps
  to that existing table; do **not** create a `[dependencies]` table, or they compile
  for wasm and leak host-only crates into the wasm build:

  ```toml
  [target.'cfg(not(target_arch = "wasm32"))'.dependencies]
  # ... existing clap/url/etc ...
  serde = { workspace = true }
  serde_json = { workspace = true }
  ```

  Add workspace dependency `chromiumoxide = "0.9.1"` in `Cargo.toml` only in Task 7 when browser code is introduced.

- [ ] **Step 2: Write failing JSON output tests**

  In `output.rs`, add tests that construct an in-memory verification result and assert exact JSON values:

  ```rust
  #[test]
  fn verification_json_contains_gate_state_and_extra_evidence() {
      let result = VerificationReport::example_confirmed_with_extra_evidence();
      let value = serde_json::to_value(&result).expect("should serialize");

      assert_eq!(value["ok"], true);
      assert_eq!(value["pages"][0]["requested_path"], "/news/story");
      assert_eq!(value["pages"][0]["runtime_ad_stack_expected"], "unknown");
      assert_eq!(value["pages"][0]["extra_evidence"][0]["kind"], "gpt");
      assert_eq!(value["pages"][0]["warnings"][0]["code"], "redirected");
  }

  // Pins the spec §8 navigation_failed shape: error present, runtime/gates/
  // matched_slot_count keys ABSENT (skipped), final_url/path null.
  #[test]
  fn page_error_json_matches_navigation_failed_shape() {
      let result = VerificationReport::example_navigation_failed();
      let value = serde_json::to_value(&result).expect("should serialize");
      let page = &value["pages"][0];

      assert_eq!(page["error"]["code"], "navigation_failed");
      assert!(page["final_url"].is_null(), "final_url should be null");
      assert!(page["path"].is_null(), "path should be null");
      assert!(page.get("runtime_ad_stack_expected").is_none(), "runtime field absent on error page");
      assert!(page.get("gates").is_none(), "gates absent on error page");
      assert!(page.get("matched_slot_count").is_none(), "matched_slot_count absent on error page");
      assert_eq!(value["ok"], false);
  }
  ```

- [ ] **Step 3: Run focused test and verify it fails**

  Run:

  ```bash
  cargo test -p trusted-server-cli ad_templates::output --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: compile failure because output model does not exist.

- [ ] **Step 4: Implement serializable output types**

  Model the **entire** `--json` wire tree from spec §8 (this is the single source of
  truth for field names and ordering). Use owned `String` / `Vec` fields and
  `#[serde(rename_all = "snake_case")]` so output is stable. Leaf enums:

  ```rust
  #[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
  #[serde(rename_all = "snake_case")]
  pub enum SlotStatus { Confirmed, Partial, Missing }

  #[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
  #[serde(rename_all = "snake_case")]
  pub enum RuntimeAdStackExpectedJson { Yes, No, Unknown }

  impl From<trusted_server_core::creative_opportunities::RuntimeAdStackExpected> for RuntimeAdStackExpectedJson {
      fn from(value: trusted_server_core::creative_opportunities::RuntimeAdStackExpected) -> Self {
          use trusted_server_core::creative_opportunities::RuntimeAdStackExpected as Core;
          match value {
              Core::Yes => Self::Yes,
              Core::No => Self::No,
              Core::Unknown => Self::Unknown,
          }
      }
  }

  #[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize)]
  #[serde(rename_all = "snake_case")]
  pub enum GateState { Pass, Fail, Unknown }
  ```

  Top-level tree (field names and nesting must match §8 exactly):

  ```rust
  #[derive(Debug, Clone, PartialEq, Serialize)]
  pub struct VerificationReport {
      pub ok: bool,
      pub strict: bool,
      pub pages: Vec<PageJson>,
      pub warnings: Vec<Warning>,
  }

  #[derive(Debug, Clone, PartialEq, Serialize)]
  pub struct PageJson {
      pub url: String,
      pub final_url: Option<String>,
      pub requested_path: String,
      pub path: Option<String>,
      // Field ORDER matters: serde serializes in declaration order. Spec §8 places
      // `error` immediately after `path` on the navigation_failed shape, so it must
      // be declared here (not last). On normal pages `error` is None and skipped, so
      // the runtime/gates/slots run in §8 order; on error pages the runtime/gates/
      // matched_slot_count are None and skipped, leaving url..path, error, slots,
      // extra_evidence, warnings — exactly the §8 error shape.
      #[serde(skip_serializing_if = "Option::is_none")]
      pub error: Option<Warning>,
      #[serde(skip_serializing_if = "Option::is_none")]
      pub runtime_ad_stack_expected: Option<RuntimeAdStackExpectedJson>,
      #[serde(skip_serializing_if = "Option::is_none")]
      pub gates: Option<Gates>,
      #[serde(skip_serializing_if = "Option::is_none")]
      pub matched_slot_count: Option<usize>,
      pub slots: Vec<SlotJson>,
      pub extra_evidence: Vec<ExtraEvidenceJson>,
      pub warnings: Vec<Warning>,
  }

  // One field per gate name from spec §5.2 / §8, each a GateState.
  #[derive(Debug, Clone, PartialEq, Serialize)]
  pub struct Gates {
      pub method_get: GateState,
      pub navigation: GateState,
      pub not_prefetch: GateState,
      pub not_bot: GateState,
      pub matched_slots: GateState,
      pub auction_enabled: GateState,
      pub consent_allows_auction: GateState,
  }

  // Serialize for output JSON; Deserialize because the browser collector payload
  // (Task 8) carries warning objects decoded into `BrowserAdEvidence.warnings`.
  #[derive(Debug, Clone, Eq, PartialEq, Serialize, serde::Deserialize)]
  pub struct Warning {
      pub code: String,
      pub message: String,
  }
  ```

  Define the remaining nested JSON structs **explicitly** — do not serialize the
  compare-module types directly. The compare types (`SlotResult`, `SlotEvidence`,
  `GptSlotEvidence`, `ExtraEvidence`) carry a `phase` field and are not `Serialize`;
  spec §8's `evidence.gpt` has **no** `phase` key and `configured` excludes `id`
  and `page_patterns`. Mismatched reuse would emit extra keys. Wire structs:

  ```rust
  #[derive(Debug, Clone, PartialEq, Serialize)]
  pub struct SlotJson {
      pub id: String,
      pub status: SlotStatus,
      pub phase: EvidencePhaseJson,
      pub configured: ConfiguredJson,
      pub evidence: SlotEvidenceJson,
      pub warnings: Vec<Warning>,
  }

  // §8 `configured`: div_id, gam_unit_path, formats, providers — NO id/page_patterns.
  #[derive(Debug, Clone, PartialEq, Serialize)]
  pub struct ConfiguredJson {
      pub div_id: String,
      pub gam_unit_path: String,
      pub formats: Vec<FormatJson>,
      pub providers: Vec<String>,
  }

  #[derive(Debug, Clone, PartialEq, Serialize)]
  pub struct FormatJson {
      pub width: u32,
      pub height: u32,
      pub media_type: String,
  }

  #[derive(Debug, Clone, PartialEq, Serialize)]
  pub struct SlotEvidenceJson {
      pub dom_id: Option<String>,
      pub gpt: Option<GptEvidenceJson>,
  }

  // §8 `evidence.gpt`: gam_unit_path, div_id, sizes — NO phase.
  #[derive(Debug, Clone, PartialEq, Serialize)]
  pub struct GptEvidenceJson {
      pub gam_unit_path: String,
      pub div_id: String,
      pub sizes: Vec<[u32; 2]>,
  }

  #[derive(Debug, Clone, Copy, PartialEq, Serialize)]
  #[serde(rename_all = "snake_case")]
  pub enum EvidencePhaseJson { InitialLoad, Scroll }

  #[derive(Debug, Clone, PartialEq, Serialize)]
  pub struct ExtraEvidenceJson {
      pub kind: String,
      pub phase: EvidencePhaseJson,
      pub dom_id: Option<String>,
      pub gam_unit_path: Option<String>,
      pub sizes: Vec<[u32; 2]>,
      pub reason: String,
  }
  ```

  Note `sizes` serialize as `[[300,250]]` (arrays of two ints), matching §8 — use
  `[u32; 2]` here even though the compare module uses `(u32, u32)` tuples; the
  Task 9 assembly maps tuple → `[w, h]`. The conversion from the compare
  `SlotResult`/`SlotEvidence`/`ExtraEvidence` to these JSON types (dropping `phase`
  from `gpt`, dropping `id`/`page_patterns` from `configured`) lives in Task 9 Step 7.
  `Warning` is the single warning type for the whole CLI; defined here and re-exported
  from `ad_templates::mod` so `compare.rs` reuses it (plain data, not JSON logic).
  Keep `example_confirmed_with_extra_evidence()` and similar fixtures behind
  `#[cfg(test)]`.

- [ ] **Step 5: Add verification human-render helpers**

  Add only the **browser-verification** page summary writers here (used by
  `audit::ad_templates` in Task 9), writing to `&mut dyn Write`; no `println!` /
  `eprintln!`. Do **not** add static match/check/explain writers in this task —
  those are the existing `write_match_result`/`format_slot`/etc. functions that
  Task 6 Step 3 **moves** out of `config_ad_templates.rs`. Keeping the static
  relocation solely in Task 6 avoids two competing copies of the same helpers in
  `output.rs`. The verification writers added here may be unused until Task 9 (a
  warn-level `dead_code` lint that does not fail `cargo test`); add
  `#[allow(dead_code)]` if clippy is run between Task 4 and Task 9.

- [ ] **Step 6: Run focused tests**

  Run:

  ```bash
  cargo test -p trusted-server-cli ad_templates::output --target $(rustc -vV | sed -n 's/^host: //p')
  cargo test -p trusted-server-cli config_ad_templates --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: all pass.

- [ ] **Step 7: Commit**

  ```bash
  git add Cargo.toml crates/trusted-server-cli/Cargo.toml crates/trusted-server-cli/src/ad_templates/mod.rs crates/trusted-server-cli/src/ad_templates/output.rs
  git commit -m "Add ad-template CLI output models"
  ```

## Task 5: Add Pure Evidence Comparison

**Files:**

- Create: `crates/trusted-server-cli/src/ad_templates/compare.rs`
- Modify: `crates/trusted-server-cli/src/ad_templates/mod.rs`

- [ ] **Step 1: Write failing comparison tests**

  Cover every spec status and warning case without launching Chrome. Define small
  test constructors so tests do not couple to the full `BrowserAdEvidence` field
  list (`page_bids` and `warnings` default empty, evidence items default to
  `EvidencePhase::InitialLoad`):

  ```rust
  fn dom(id: &str) -> DomEvidence {
      DomEvidence { dom_id: id.to_string(), phase: EvidencePhase::InitialLoad }
  }

  fn gpt_slot(gam_unit_path: &str, div_id: &str, sizes: &[(u32, u32)]) -> GptSlotEvidence {
      GptSlotEvidence {
          gam_unit_path: gam_unit_path.to_string(),
          div_id: div_id.to_string(),
          sizes: sizes.to_vec(),
          phase: EvidencePhase::InitialLoad,
      }
  }

  fn aps(slot_id: &str, sizes: &[(u32, u32)]) -> ApsFetchBidsEvidence {
      ApsFetchBidsEvidence { slot_id: slot_id.to_string(), sizes: sizes.to_vec(), phase: EvidencePhase::InitialLoad }
  }

  // Non-banner format helper for the unsupported-format test.
  fn expected_slot_video(id: &str, div_id: &str, gam_unit_path: &str) -> ExpectedSlot {
      ExpectedSlot {
          id: id.to_string(),
          div_id: div_id.to_string(),
          gam_unit_path: gam_unit_path.to_string(),
          formats: vec![ExpectedFormat { width: 0, height: 0, media_type: "video".to_string() }],
          providers: Vec::new(),
          page_patterns: Vec::new(),
      }
  }

  fn evidence(doms: Vec<DomEvidence>, gpts: Vec<GptSlotEvidence>, aps: Vec<ApsFetchBidsEvidence>) -> BrowserAdEvidence {
      BrowserAdEvidence {
          dom_ids: doms,
          gpt_slots: gpts,
          aps_calls: aps,
          page_bids: Vec::new(),
          warnings: Vec::new(),
      }
  }

  #[test]
  fn gpt_path_div_and_size_overlap_confirms_slot() {
      let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &["aps"]);
      let evidence = evidence(
          vec![dom("ad-atf-0")],
          vec![gpt_slot("/123/news/atf", "ad-atf-0", &[(300, 250)])],
          Vec::new(),
      );

      let result = compare_page_evidence(&[expected], &evidence, RuntimeGateSummary::unknown_allowed());

      assert_eq!(result.slots[0].status, SlotStatus::Confirmed, "GPT path+div+size overlap should confirm");
      assert!(result.slots[0].warnings.is_empty(), "confirmed slot should carry no warnings");
  }

  #[test]
  fn dom_only_is_partial() {
      let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &[]);
      let evidence = evidence(vec![dom("ad-atf-0")], Vec::new(), Vec::new());

      let result = compare_page_evidence(&[expected], &evidence, RuntimeGateSummary::unknown_allowed());

      assert_eq!(result.slots[0].status, SlotStatus::Partial, "DOM-only evidence should be partial");
      assert!(
          result.slots[0].warnings.iter().any(|w| w.code == "dom_without_gpt"),
          "DOM-only slot should warn dom_without_gpt"
      );
  }

  #[test]
  fn no_dom_or_gpt_is_missing() {
      let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &[]);
      let evidence = evidence(Vec::new(), Vec::new(), Vec::new());

      let result = compare_page_evidence(&[expected], &evidence, RuntimeGateSummary::unknown_allowed());

      assert_eq!(result.slots[0].status, SlotStatus::Missing, "no DOM/GPT evidence should be missing");
  }

  #[test]
  fn prefix_dom_resolution_ignores_container_suffix() {
      let expected = expected_slot("header", "ad-header-0-", "/123/homepage/header", &[(728, 90)], &[]);
      // First candidate ends with `-container` and must be skipped; the framework-suffixed ID resolves.
      let evidence = evidence(
          vec![dom("ad-header-0--container"), dom("ad-header-0-_R_abc123")],
          Vec::new(),
          Vec::new(),
      );

      let result = compare_page_evidence(&[expected], &evidence, RuntimeGateSummary::unknown_allowed());

      assert_eq!(result.slots[0].evidence.dom_id.as_deref(), Some("ad-header-0-_R_abc123"), "prefix match should skip -container");
      assert_eq!(result.slots[0].status, SlotStatus::Partial, "DOM-only prefix match is partial without GPT");
  }

  #[test]
  fn unmatched_gpt_slot_becomes_extra_evidence() {
      let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &[]);
      let evidence = evidence(
          vec![dom("ad-atf-0")],
          vec![
              gpt_slot("/123/news/atf", "ad-atf-0", &[(300, 250)]),
              gpt_slot("/123/publisher/right-rail", "ad-right-rail-0", &[(300, 250)]),
          ],
          Vec::new(),
      );

      let result = compare_page_evidence(&[expected], &evidence, RuntimeGateSummary::unknown_allowed());

      assert_eq!(result.slots[0].status, SlotStatus::Confirmed, "matched slot still confirms");
      assert_eq!(result.extra_evidence.len(), 1, "unmatched GPT slot becomes extra evidence");
      assert_eq!(result.extra_evidence[0].kind, "gpt");
      assert!(!result.strict_failed(), "extra evidence alone must not fail strict");
  }

  #[test]
  fn auction_disabled_skips_strict_missing_failure() {
      let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &[]);
      let evidence = evidence(Vec::new(), Vec::new(), Vec::new());

      let result = compare_page_evidence(&[expected], &evidence, RuntimeGateSummary::auction_disabled());

      assert_eq!(result.runtime_ad_stack_expected, RuntimeAdStackExpected::No, "auction disabled should set No");
      assert_eq!(result.slots[0].status, SlotStatus::Missing, "static status is still reported");
      assert!(!result.strict_failed(), "missing slot must not fail strict when ad stack expected is No");
  }

  // §5.4: GPT path+div match but no numeric size overlap -> partial + warning.
  #[test]
  fn gpt_incompatible_sizes_is_partial() {
      let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &[]);
      let evidence = evidence(
          vec![dom("ad-atf-0")],
          vec![gpt_slot("/123/news/atf", "ad-atf-0", &[(728, 90)])],
          Vec::new(),
      );

      let result = compare_page_evidence(&[expected], &evidence, RuntimeGateSummary::unknown_allowed());

      assert_eq!(result.slots[0].status, SlotStatus::Partial, "no size overlap should be partial");
      assert!(result.slots[0].warnings.iter().any(|w| w.code == "incompatible_sizes"));
  }

  // §5.4/§5.6: matched slot with only non-banner formats -> partial + unsupported_format.
  #[test]
  fn non_banner_only_slot_is_partial() {
      let expected = expected_slot_video("video", "ad-video-", "/123/news/video");
      let evidence = evidence(
          vec![dom("ad-video-0")],
          vec![gpt_slot("/123/news/video", "ad-video-0", &[(640, 480)])],
          Vec::new(),
      );

      let result = compare_page_evidence(&[expected], &evidence, RuntimeGateSummary::unknown_allowed());

      assert_eq!(result.slots[0].status, SlotStatus::Partial, "non-banner-only should be partial");
      assert!(result.slots[0].warnings.iter().any(|w| w.code == "unsupported_format"));
  }

  // §5.4: GPT element ID may be `${resolved_dom_id}-container` and still confirm.
  #[test]
  fn gpt_container_element_id_confirms() {
      let expected = expected_slot("atf", "ad-atf-0", "/123/news/atf", &[(300, 250)], &[]);
      let evidence = evidence(
          vec![dom("ad-atf-0"), dom("ad-atf-0-container")],
          vec![gpt_slot("/123/news/atf", "ad-atf-0-container", &[(300, 250)])],
          Vec::new(),
      );

      let result = compare_page_evidence(&[expected], &evidence, RuntimeGateSummary::unknown_allowed());

      assert_eq!(result.slots[0].status, SlotStatus::Confirmed, "container element id is a valid GPT div match");
  }

  // §5.4: out-of-page GPT slot is not confirmed; reported as a warning.
  #[test]
  fn out_of_page_gpt_slot_warns_and_does_not_confirm() {
      let expected = expected_slot("interstitial", "ad-oop-", "/123/news/oop", &[(300, 250)], &[]);
      // gpt_slot with empty sizes models an out-of-page slot (no numeric sizes).
      let evidence = evidence(vec![dom("ad-oop-0")], vec![gpt_slot("/123/news/oop", "ad-oop-0", &[])], Vec::new());

      let result = compare_page_evidence(&[expected], &evidence, RuntimeGateSummary::unknown_allowed());

      assert_ne!(result.slots[0].status, SlotStatus::Confirmed, "out-of-page is not confirmed in Phase 1");
      assert!(result.slots[0].warnings.iter().any(|w| w.code == "out_of_page_slot"));
  }

  // §5.5: matching APS fetchBids -> no provider warning.
  #[test]
  fn aps_match_adds_no_warning() {
      let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &["aps"]);
      let evidence = evidence(
          vec![dom("ad-atf-0")],
          vec![gpt_slot("/123/news/atf", "ad-atf-0", &[(300, 250)])],
          vec![aps("atf", &[(300, 250)])],
      );

      let result = compare_page_evidence(&[expected], &evidence, RuntimeGateSummary::unknown_allowed());

      assert_eq!(result.slots[0].status, SlotStatus::Confirmed);
      assert!(!result.slots[0].warnings.iter().any(|w| w.code.starts_with("aps_")), "matching APS should not warn");
  }

  // §5.5: configured aps provider but no APS evidence -> provider warning, still confirmed, strict not failed.
  #[test]
  fn aps_missing_warns_but_keeps_confirmed() {
      let expected = expected_slot("atf", "ad-atf-", "/123/news/atf", &[(300, 250)], &["aps"]);
      let evidence = evidence(
          vec![dom("ad-atf-0")],
          vec![gpt_slot("/123/news/atf", "ad-atf-0", &[(300, 250)])],
          Vec::new(),
      );

      let result = compare_page_evidence(&[expected], &evidence, RuntimeGateSummary::unknown_allowed());

      assert_eq!(result.slots[0].status, SlotStatus::Confirmed, "missing APS does not flip status");
      assert!(result.slots[0].warnings.iter().any(|w| w.code == "aps_evidence_missing"));
      assert!(!result.strict_failed(), "provider warning alone must not fail strict");
  }
  ```

  Add a `#[cfg(test)]` constructor in `compare.rs` tests that builds a real
  `ExpectedSlot` (the Task 3 type) so comparison tests stay readable:

  ```rust
  fn expected_slot(id: &str, div_id: &str, gam_unit_path: &str, sizes: &[(u32, u32)], providers: &[&str]) -> ExpectedSlot {
      ExpectedSlot {
          id: id.to_string(),
          div_id: div_id.to_string(),
          gam_unit_path: gam_unit_path.to_string(),
          formats: sizes.iter().map(|&(width, height)| ExpectedFormat { width, height, media_type: "banner".to_string() }).collect(),
          providers: providers.iter().map(|p| p.to_string()).collect(),
          page_patterns: Vec::new(),
      }
  }
  ```

- [ ] **Step 2: Run focused test and verify it fails**

  Run:

  ```bash
  cargo test -p trusted-server-cli ad_templates::compare --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: compile failure because comparison module does not exist.

- [ ] **Step 3: Implement browser evidence structs**

  Define the minimum collector-independent input shape plus the comparison result
  shape the tests assert against:

  All browser-evidence input structs derive `Debug, Clone` and `serde::Deserialize`
  (Task 8 decodes them from the collector's `window.__tsAdTemplateEvidence` JSON);
  `EvidencePhase` deserializes from `"initial_load"` / `"scroll"`. The comparison-
  result structs derive `Debug` (so the Step 1 `assert_eq!`/`matches!` assertions
  compile) and `Clone`. Sizes are `(u32, u32)` tuples internally; deserialize them
  from JSON `[w, h]` arrays.

  ```rust
  #[derive(Debug, Clone, Copy, Eq, PartialEq, serde::Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum EvidencePhase {
      InitialLoad,
      Scroll,
  }

  #[derive(Debug, Clone, serde::Deserialize)]
  pub struct DomEvidence {
      pub dom_id: String,
      pub phase: EvidencePhase,
  }

  #[derive(Debug, Clone, serde::Deserialize)]
  pub struct GptSlotEvidence {
      pub gam_unit_path: String,
      pub div_id: String,
      pub sizes: Vec<(u32, u32)>,
      pub phase: EvidencePhase,
  }

  // APS `apstag.fetchBids` evidence (spec §5.5): configured slot ID + observed sizes.
  #[derive(Debug, Clone, serde::Deserialize)]
  pub struct ApsFetchBidsEvidence {
      pub slot_id: String,
      pub sizes: Vec<(u32, u32)>,
      pub phase: EvidencePhase,
  }

  // DEFERRED in this implementation: `/__ts/page-bids` SPA observation (spec §5.2
  // "when available"). The struct/field are forward scaffolding so the collector and
  // JSON can grow it later; Task 8 does NOT populate it and Task 4 JSON does NOT
  // surface it in Phase 1. Tracked as a deferred item in Risks. Keep the field so
  // `BrowserAdEvidence` deserialization stays forward-compatible (default empty).
  #[derive(Debug, Clone, serde::Deserialize)]
  pub struct PageBidsEvidence {
      pub slot_id: String,
      pub phase: EvidencePhase,
  }

  // `Warning` is the shared CLI warning type defined in Task 4 (`output.rs`) and
  // re-exported from `ad_templates::mod`. It is plain data reused here (not JSON
  // logic). Because the collector payload carries warnings, give `Warning` BOTH
  // `Serialize` (Task 4 output) and `Deserialize` (Task 8 decode) derives.
  use crate::ad_templates::output::Warning;

  #[derive(Debug, Clone, serde::Deserialize)]
  pub struct BrowserAdEvidence {
      pub dom_ids: Vec<DomEvidence>,
      pub gpt_slots: Vec<GptSlotEvidence>,
      pub aps_calls: Vec<ApsFetchBidsEvidence>,
      #[serde(default)]
      pub page_bids: Vec<PageBidsEvidence>,
      #[serde(default)]
      pub warnings: Vec<Warning>,
  }

  // Comparison output. Uses the core `RuntimeAdStackExpected` enum from Task 1 so
  // pure comparison logic does not depend on the output/JSON module. Task 4's
  // `RuntimeAdStackExpectedJson` is produced only at serialization time.
  #[derive(Debug, Clone)]
  pub struct PageVerificationResult {
      pub runtime_ad_stack_expected: trusted_server_core::creative_opportunities::RuntimeAdStackExpected,
      pub slots: Vec<SlotResult>,
      pub extra_evidence: Vec<ExtraEvidence>,
  }

  #[derive(Debug, Clone)]
  pub struct SlotResult {
      pub id: String,
      pub status: SlotStatus,
      pub phase: EvidencePhase,
      pub evidence: SlotEvidence,
      pub warnings: Vec<Warning>,
  }

  #[derive(Debug, Clone)]
  pub struct SlotEvidence {
      pub dom_id: Option<String>,
      pub gpt: Option<GptSlotEvidence>,
  }

  #[derive(Debug, Clone)]
  pub struct ExtraEvidence {
      pub kind: String,
      pub phase: EvidencePhase,
      pub dom_id: Option<String>,
      pub gam_unit_path: Option<String>,
      pub sizes: Vec<(u32, u32)>,
      pub reason: String,
  }
  ```

  `RuntimeGateSummary` is the third argument to `compare_page_evidence`; it wraps
  the core gate result. Provide `RuntimeGateSummary::unknown_allowed()` (expected
  `Unknown`) and `RuntimeGateSummary::auction_disabled()` (expected `No`) test
  constructors so comparison tests do not rebuild gate inputs by hand.

- [ ] **Step 4: Implement DOM/GPT/APS rules**

  Status rules:
  - DOM exact ID first, then first prefix match, **excluding `-container`** wrappers
    (slot-root resolution, spec §5.3).
  - GPT confirms when: GAM unit path matches, the GPT slot element ID equals the
    resolved DOM ID **or** an existing `${resolved_dom_id}-container` element
    (spec §5.4 — note this is the GPT element-ID match, distinct from the §5.3 DOM
    root resolution that skips `-container`), and at least one numeric banner size
    overlaps.
  - GPT path/div match with no numeric size overlap → `partial` (warn `incompatible_sizes`).
  - Matched slot whose configured formats are **all non-banner** (video/native) →
    `partial` (warn `unsupported_format`); banner is the only Phase-1 confirmable type.
  - DOM-only (no GPT) → `partial` (warn `dom_without_gpt`).
  - No DOM and no GPT → `missing`.

  Size-compatibility warnings (spec §5.4 — all are warnings, none flip a confirmed
  slot to fail): emit a `Warning` for each of:
  - `fluid_size_ignored` — non-numeric observed sizes like `"fluid"` ignored for matching;
  - `extra_observed_size` — observed GPT sizes not in the configured set;
  - `configured_size_not_observed` — configured sizes never observed (when ≥1 was);
  - `out_of_page_slot` — out-of-page GPT slot observed; not confirmed in Phase 1.

  Provider + extra evidence:
  - APS: configured `providers.aps.slot_id` with matching `fetchBids` → no warning;
    missing/ambiguous APS evidence → provider warning only (`aps_evidence_missing` /
    `aps_evidence_ambiguous`), never flips status or fails `--strict` in Phase 1.
  - Unmatched live DOM/GPT/APS evidence → structured `extra_evidence` (never fails strict).

  Define each warning `code` as a stable string constant so output and tests share them.

- [ ] **Step 5: Implement strict decision method**

  Add an inherent method on the result so tests can call `result.strict_failed()`:

  ```rust
  impl PageVerificationResult {
      pub fn strict_failed(&self) -> bool {
          use trusted_server_core::creative_opportunities::RuntimeAdStackExpected;
          if self.runtime_ad_stack_expected == RuntimeAdStackExpected::No {
              return false;
          }
          self.slots
              .iter()
              .any(|slot| matches!(slot.status, SlotStatus::Missing | SlotStatus::Partial))
      }
  }
  ```

  - false when `runtime_ad_stack_expected == No`;
  - true for any `missing` or `partial` slot when expected is `Yes` or `Unknown`;
  - false for provider warnings and extra evidence alone (they are not slot statuses).

- [ ] **Step 6: Run focused tests**

  Run:

  ```bash
  cargo test -p trusted-server-cli ad_templates::compare --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: all comparison tests pass.

- [ ] **Step 7: Commit**

  ```bash
  git add crates/trusted-server-cli/src/ad_templates/mod.rs crates/trusted-server-cli/src/ad_templates/compare.rs
  git commit -m "Add pure ad-template evidence comparison"
  ```

## Task 6: Refactor Static Commands Onto Shared Modules

**Files:**

- Modify: `crates/trusted-server-cli/src/config_ad_templates.rs`
- Modify: `crates/trusted-server-cli/src/ad_templates/output.rs`
- Modify: `crates/trusted-server-cli/src/run.rs`

- [ ] **Step 1: Add characterization tests before refactor**

  These guard behavior across the Step 3 move, so each must assert **exact output
  substrings** (capture the command's `Vec<u8>`/`String` output and
  `assert!(out.contains("..."))`), not just run without panicking — a bare
  smoke test cannot catch a wording regression. Mirror the existing assertion style
  at `config_ad_templates.rs:570-657`. Pin, with concrete expected strings:
  - `lint` not configured → e.g. `"creative_opportunities: not configured"`;
  - `lint` with slots + auction disabled → slot count line + `"auction: disabled"`;
  - `match --details` → slot div ID, GAM unit path, formats, providers lines;
  - `check --expect-no-slots` → success message;
  - `check` failure with missing and unexpected slots → the exact failure lines;
  - `explain` → each gate line (including `"auction providers configured"`) and the
    EdgeZero legacy-fallback warning text.

  Run the existing tests first and copy the real emitted strings so the
  characterization assertions match current behavior exactly before refactoring.

- [ ] **Step 2: Run tests before refactor**

  Run:

  ```bash
  cargo test -p trusted-server-cli config_ad_templates --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: characterization tests pass against the current implementation.

- [ ] **Step 3: Move formatting into `ad_templates::output`**

  Move these helpers out of `config_ad_templates.rs`:
  - `write_match_result`
  - `write_gate`
  - `format_slot`
  - `format_format`
  - `format_providers`
  - `join_set`
  - `plural`

  Keep command functions small: parse args, load config, call expected/gate logic, render.

- [ ] **Step 4: Reuse shared gate helper in `explain`**

  Build `AdStackGateInput` from explain flags and config:

  ```rust
  let gate = evaluate_ad_stack_gate(AdStackGateInput {
      method_get,
      navigation: !args.non_navigation,
      prefetch: args.prefetch,
      bot: args.bot,
      matched_slots: !expected.slots.is_empty(),
      consent_allows_auction: Some(!args.consent_denied),
      auction_enabled: loaded.settings.auction.enabled,
  });
  ```

  Render the seven shared gate names from `gate` rather than hand-rolled boolean chains.

  **Preserve the explain-only provider gate.** The current `run_explain`
  (`config_ad_templates.rs:270`) renders an eighth gate,
  `"auction providers configured"` (`!loaded.settings.auction.providers.is_empty()`,
  line 299), and ANDs it into its local `runs_ad_stack` decision (line 302). The
  shared `evaluate_ad_stack_gate` helper and runtime `should_run_server_side_ad_stack`
  intentionally have no provider-configured gate. Do not fold this into
  `AdStackGateInput`. Keep `"auction providers configured"` as an explain-only
  supplementary `write_gate(...)` line rendered alongside the shared result, and
  keep it in `explain`'s own `runs_ad_stack` decision:

  ```rust
  let providers_configured = !loaded.settings.auction.providers.is_empty();
  render_shared_gates(out, &gate)?;
  write_gate(out, "auction providers configured", providers_configured)?;
  let runs_ad_stack =
      gate.expected == RuntimeAdStackExpected::Yes && providers_configured;
  ```

  This keeps `explain` output and behavior identical to the current implementation
  (verified by the Step 1 characterization test) while still sharing the seven core
  runtime gates with `publisher.rs`.

- [ ] **Step 5: Run focused tests**

  Run:

  ```bash
  cargo test -p trusted-server-cli config_ad_templates --target $(rustc -vV | sed -n 's/^host: //p')
  cargo test -p trusted-server-cli ad_templates --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: no output regressions except intentional wording updates covered by tests.

- [ ] **Step 6: Commit**

  ```bash
  git add crates/trusted-server-cli/src/config_ad_templates.rs crates/trusted-server-cli/src/ad_templates/output.rs crates/trusted-server-cli/src/run.rs
  git commit -m "Refactor static ad-template commands"
  ```

## Task 7: Port Generic Audit Namespace And Browser Collector

**Files:**

- Modify: `Cargo.toml`
- Modify: `crates/trusted-server-cli/Cargo.toml`
- Create: `crates/trusted-server-cli/src/audit/mod.rs`
- Create: `crates/trusted-server-cli/src/audit/page.rs`
- Create: `crates/trusted-server-cli/src/audit/collector.rs`
- Create: `crates/trusted-server-cli/src/audit/browser.rs`
- Modify: `crates/trusted-server-cli/src/lib.rs`
- Modify: `crates/trusted-server-cli/src/run.rs`

- [ ] **Step 1: Add browser dependencies**

  Add `chromiumoxide` to the root `[workspace.dependencies]` table (inert until a
  crate references it via `{ workspace = true }`):

  ```toml
  [workspace.dependencies]
  # ... existing entries ...
  chromiumoxide = "0.9.1"
  ```

  Add the host deps to the CLI crate under its existing
  `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` table — NOT a plain
  `[dependencies]` table (workspace default target is wasm32; an unconditional dep
  compiles for wasm and breaks the build / leaks host-only crates):

  ```toml
  [target.'cfg(not(target_arch = "wasm32"))'.dependencies]
  # ... existing clap/url/serde/etc ...
  chromiumoxide = { workspace = true }
  futures = { workspace = true }
  tempfile = { workspace = true }
  tokio = { workspace = true }
  which = { workspace = true }
  ```

  Verify the root workspace already provides `futures`, `tempfile`, `tokio`, `which`
  (it does on this branch); only `chromiumoxide` is a new workspace entry.

- [ ] **Step 2: Write failing audit parser tests**

  In `run.rs` tests:

  ```rust
  #[test]
  fn audit_legacy_url_parses_as_page_alias() {
      let args = parse(&["ts", "audit", "https://www.example.com/"]);
      assert!(matches!(args.command, Command::Audit(_)));
  }

  #[test]
  fn audit_page_subcommand_parses() {
      let args = parse(&["ts", "audit", "page", "https://www.example.com/"]);
      assert!(matches!(args.command, Command::Audit(_)));
  }

  #[test]
  fn audit_ad_templates_verify_parses() {
      let args = parse(&["ts", "audit", "ad-templates", "verify", "https://www.example.com/"]);
      assert!(matches!(args.command, Command::Audit(_)));
  }

  #[test]
  fn audit_ad_templates_is_not_legacy_url() {
      assert!(Args::try_parse_from(["ts", "audit", "ad-templates"]).is_err());
  }
  ```

- [ ] **Step 3: Run parser tests and verify failure**

  Run:

  ```bash
  cargo test -p trusted-server-cli audit_ --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: compile failure because `Audit` command does not exist.

- [ ] **Step 4: Implement audit Clap namespace in current `run.rs` shape**

  Do not add stale #800 `args.rs`. Add a `Command::Audit(AuditArgs)` variant to the
  existing `Command` enum, plus the full audit arg surface. The parser tests in
  Step 2 exercise `ad-templates verify`, so the **entire** command surface (including
  the verify args) must be defined here for those tests to compile. Task 9 implements
  the verify _behavior_ only — it does not redefine these arg types.

  **Visibility:** `audit::run_audit` lives in `audit/mod.rs` and must name these
  types in its signature and match their variants, so every audit arg type and its
  fields are `pub(crate)` (not private). `PageAuditArgs` (from `audit/page.rs`, Task 7
  Step 5) and `AuditAdTemplatesVerifyArgs` are likewise `pub(crate)`/`pub`. `run.rs`
  imports `AuditArgs` for the `Command::Audit(AuditArgs)` variant; everything else is
  read by `audit/mod.rs`. (This mirrors `config_ad_templates::AdTemplatesCommand`,
  which is `pub` and consumed by `run.rs`.)

  ```rust
  // value parser shared by legacy_url and verify urls; rejects non-HTTP(S) schemes.
  pub(crate) fn parse_http_url(raw: &str) -> Result<url::Url, String> {
      let url = url::Url::parse(raw).map_err(|error| format!("invalid URL `{raw}`: {error}"))?;
      match url.scheme() {
          "http" | "https" => Ok(url),
          other => Err(format!("unsupported URL scheme `{other}` (expected http or https)")),
      }
  }

  #[derive(Debug, clap::Args)]
  pub(crate) struct AuditArgs {
      #[command(subcommand)]
      pub(crate) command: Option<AuditSubcommand>,
      #[arg(value_parser = parse_http_url, hide = true)]
      pub(crate) legacy_url: Option<url::Url>,
  }

  #[derive(Debug, Subcommand)]
  pub(crate) enum AuditSubcommand {
      Page(PageAuditArgs),
      #[command(name = "ad-templates", subcommand)]
      AdTemplates(AuditAdTemplatesCommand),
  }

  #[derive(Debug, Subcommand)]
  pub(crate) enum AuditAdTemplatesCommand {
      Verify(AuditAdTemplatesVerifyArgs),
  }

  // Defined here (not Task 9) so parser tests compile. Task 9 fills in the handler.
  #[derive(Debug, clap::Args)]
  pub(crate) struct AuditAdTemplatesVerifyArgs {
      #[command(flatten)]
      pub config: AppConfigArgs,
      #[arg(required = true, value_parser = parse_http_url)]
      pub urls: Vec<url::Url>,
      #[arg(long)]
      pub strict: bool,
      #[arg(long)]
      pub json: bool,
      #[arg(long)]
      pub scroll: bool,
  }
  ```

  Dispatch `Command::Audit(args)` to a single `audit::run_audit(args: AuditArgs)`
  entry point (in `audit/mod.rs`) that normalizes the namespace: `legacy_url` (if
  present) and `Page` both route to the generic page audit; `AdTemplates(Verify(..))`
  routes to the verifier (a stub returning `Ok(())` until Task 9). If Clap cannot make
  the optional-subcommand-plus-hidden-positional contract unambiguous, implement a
  small `AuditArgs::normalize()` that rejects `legacy_url` values that are not HTTP(S).
  Decide arg-type home consistently: keep them in `run.rs` as `pub(crate)` (as shown)
  and import into `audit/mod.rs`, or move them next to `run_audit` in `audit/mod.rs`
  and import `AuditArgs` into `run.rs` — either works, but do not split them.

- [ ] **Step 5: Port minimal generic page audit**

  Port useful #800 concepts into `audit/page.rs`, but keep output read-only by default for now:
  - parse/validate URL;
  - call `AuditCollector::collect_page`;
  - print summary with final URL, title, script/resource counts, warnings;
  - no draft config generation in this PR unless #800 rebase keeps it explicitly.

- [ ] **Step 6: Implement collector trait and browser collector base**

  `audit/collector.rs` — define the trait plus its concrete request/response types so
  Task 9's `FakeCollector` and the verify orchestration have a contract to assert on:

  ```rust
  pub trait AuditCollector {
      fn collect_page(&self, request: BrowserCollectRequest) -> Result<CollectedPage, String>;
  }

  pub struct BrowserCollectRequest {
      pub url: url::Url,
      // Pre-navigation init scripts (evaluate-on-new-document). Empty for plain page audit;
      // Task 8 passes the ad-template collector script here.
      pub init_scripts: Vec<String>,
      pub scroll: bool,
  }

  pub struct CollectedPage {
      pub final_url: url::Url,
      pub title: String,
      // Generic page-audit signals (counts only; no page HTML/cookies/storage).
      pub script_count: usize,
      pub resource_count: usize,
      pub warnings: Vec<crate::ad_templates::output::Warning>,
      // Present only when an ad-template init script was injected (Task 8); None for
      // plain `ts audit page`. This is how `BrowserAdEvidence` rides on a CollectedPage.
      pub ad_evidence: Option<crate::ad_templates::compare::BrowserAdEvidence>,
  }
  ```

  `BrowserCollectRequest` carries `init_scripts` + `scroll` so ad-template verification
  enables evidence hooks without changing the trait later.

  `audit/browser.rs` should port #800's:
  - `which` browser lookup;
  - isolated `TempDir` profile;
  - current-thread Tokio runtime;
  - `Browser::launch`;
  - `page.goto`;
  - `wait_for_navigation_response`;
  - settle loop.

- [ ] **Step 7: Run compile-focused CLI tests**

  Run:

  ```bash
  cargo test -p trusted-server-cli audit_ --target $(rustc -vV | sed -n 's/^host: //p')
  cargo test -p trusted-server-cli --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: parser and non-browser unit tests pass. No test should require installed Chrome yet.

- [ ] **Step 8: Commit**

  ```bash
  git add Cargo.toml crates/trusted-server-cli/Cargo.toml crates/trusted-server-cli/src/audit crates/trusted-server-cli/src/lib.rs crates/trusted-server-cli/src/run.rs
  git commit -m "Add audit namespace and browser collector base"
  ```

## Task 8: Add Browser Ad-Template Evidence Collector

**Files:**

- Create: `crates/trusted-server-cli/src/audit/ad_template_collector.js`
- Modify: `crates/trusted-server-cli/src/audit/browser.rs`
- Modify: `crates/trusted-server-cli/src/audit/collector.rs`
- Modify: `crates/trusted-server-cli/src/ad_templates/compare.rs`

- [ ] **Step 1: Write JS collector contract fixture tests**

  Add Rust unit tests that inspect generated init-script text and decode a mocked `window.__tsAdTemplateEvidence` JSON payload. These should not launch Chrome.

  Prefer **behavioral** assertions over brittle substring matching: where possible,
  assert by decoding a mocked `window.__tsAdTemplateEvidence` payload into
  `BrowserAdEvidence` and checking fields. For the few structural checks that must
  inspect the script text, pin **exact** marker substrings (no "or equivalent", so
  the pass condition is deterministic) — choose the markers to match the strings the
  implementation will actually emit:
  - `build_ad_template_init_script` output contains the literal `__TS_CONFIG` injection;
  - contains the chosen googletag-hook marker (pick ONE and pin it, e.g.
    `Object.defineProperty(window, "googletag"`);
  - contains the `cmd.push` wrap marker;
  - contains the `defineSlot` record marker;
  - contains the `apstag.fetchBids` wrap marker;
  - embeds only the configured div prefixes / provider IDs passed via `__TS_CONFIG`
    (assert a non-configured prefix is absent).

- [ ] **Step 2: Run tests and verify failure**

  Run:

  ```bash
  cargo test -p trusted-server-cli ad_template_collector --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: failure because collector script/builder does not exist.

- [ ] **Step 3: Implement init script builder**

  In Rust, build script as:

  ```rust
  pub fn build_ad_template_init_script(config: &AdTemplateCollectorConfig) -> Result<String, String> {
      let config_json = serde_json::to_string(config)
          .map_err(|error| format!("failed to serialize ad-template collector config: {error}"))?;
      Ok(format!(";(() => {{ const __TS_CONFIG = {config_json};\n{}\n}})();", include_str!("ad_template_collector.js")))
  }
  ```

  Keep the JS file generic; pass configured prefixes and APS slot IDs through `__TS_CONFIG`.

- [ ] **Step 4: Implement read-only JS evidence collection**

  In `ad_template_collector.js`, write to `window.__tsAdTemplateEvidence`:
  - `dom_ids`: matched IDs from configured prefixes, excluding `-container`;
  - `gpt_slots`: record `defineSlot` calls observed **both** directly **and** when
    dispatched from the `googletag.cmd` queue (wrap `cmd.push` so queued callbacks are
    instrumented without changing their order — spec §7), **plus** a post-settle
    `googletag.pubads().getSlots()` scrape. For each scraped slot capture
    `getAdUnitPath()`, `getSlotElementId()`, and `getSizes()` so `getSlots()`-only
    slots still carry numeric `sizes` for the §5.4 overlap rule. Normalize sizes from
    both `defineSlot` input and `getSizes()` output: `[300,250]` → one `(300,250)`;
    `[[300,250],[728,90]]` → two pairs; non-numeric (`"fluid"`) dropped from numeric
    sizes and surfaced as a `fluid_size_ignored` warning;
  - `aps_calls`: `fetchBids` payloads (configured slot IDs + sizes);
  - `warnings`: collector warnings only ({code, message}), no page HTML/cookies/storage.

  Always call original page functions with unchanged arguments, and never override
  `navigator.webdriver` (spec §7).

- [ ] **Step 5: Add browser collector extraction**

  After settle and after optional scroll, evaluate:

  ```javascript
  ;() => window.__tsAdTemplateEvidence || null
  ```

  Decode into `BrowserAdEvidence`. If decode fails, return a page warning rather than failing navigation.

- [ ] **Step 6: Add deterministic scroll**

  In `audit/browser.rs`, implement `scroll` by evaluating:

  ```javascript
  ;async () => {
    const height = Math.max(
      document.body.scrollHeight,
      document.documentElement.scrollHeight
    )
    for (const y of [
      Math.floor(height * 0.33),
      Math.floor(height * 0.66),
      height,
    ]) {
      window.scrollTo(0, y)
      await new Promise((resolve) => setTimeout(resolve, 250))
    }
    window.scrollTo(0, 0)
  }
  ```

  Then wait for the same settle quiet period and collect evidence with `phase = "scroll"` where the JS script marks new observations.

- [ ] **Step 7: Run focused tests**

  Run:

  ```bash
  cargo test -p trusted-server-cli ad_template_collector --target $(rustc -vV | sed -n 's/^host: //p')
  cargo test -p trusted-server-cli audit::browser --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: unit tests pass without launching Chrome.

- [ ] **Step 8: Commit**

  ```bash
  git add crates/trusted-server-cli/src/audit/ad_template_collector.js crates/trusted-server-cli/src/audit/browser.rs crates/trusted-server-cli/src/audit/collector.rs crates/trusted-server-cli/src/ad_templates/compare.rs
  git commit -m "Collect browser ad-template evidence"
  ```

## Task 9: Implement `ts audit ad-templates verify`

**Files:**

- Create: `crates/trusted-server-cli/src/audit/ad_templates.rs`
- Modify: `crates/trusted-server-cli/src/audit/mod.rs`
- Modify: `crates/trusted-server-cli/src/run.rs`
- Modify: `crates/trusted-server-cli/src/ad_templates/output.rs`

- [ ] **Step 1: Write failing orchestration tests with a fake collector**

  Build a fake collector implementing `AuditCollector` and test:
  - one confirmed page exits success in default mode;
  - strict missing slot returns error;
  - `[auction].enabled = false` returns runtime skipped and does not strict-fail missing evidence;
  - one page navigation error plus one success sets JSON `ok = false`;
  - invalid `ftp://` URL fails before fake collector is called;
  - redirect uses final path for expected slots and emits redirect warning.

  Define the test scaffolding explicitly (no dangling helpers):

  ```rust
  // Maps each requested URL to a canned outcome so orchestration is tested without Chrome.
  struct FakeCollector {
      pages: std::collections::HashMap<String, Result<CollectedPage, String>>,
  }

  impl FakeCollector {
      // Success page: requested -> final_url, carrying the given ad evidence.
      fn page(requested: &str, final_url: &str, evidence: BrowserAdEvidence) -> Self {
          let mut pages = std::collections::HashMap::new();
          pages.insert(
              requested.to_string(),
              Ok(CollectedPage {
                  final_url: url::Url::parse(final_url).expect("valid final url"),
                  title: String::new(),
                  script_count: 0,
                  resource_count: 0,
                  warnings: Vec::new(),
                  ad_evidence: Some(evidence),
              }),
          );
          Self { pages }
      }
      // Helper to add a failing page for multi-URL tests.
      fn with_error(mut self, requested: &str, message: &str) -> Self {
          self.pages.insert(requested.to_string(), Err(message.to_string()));
          self
      }
  }

  impl AuditCollector for FakeCollector {
      fn collect_page(&self, request: BrowserCollectRequest) -> Result<CollectedPage, String> {
          self.pages
              .get(request.url.as_str())
              .cloned()
              .unwrap_or_else(|| Err(format!("no fake page for {}", request.url)))
      }
  }

  impl BrowserAdEvidence {
      // #[cfg(test)] fixture: one confirmed news slot (atf / ad-atf-0 / /123/news/atf, 300x250).
      fn confirmed_news_slot() -> Self {
          BrowserAdEvidence {
              dom_ids: vec![DomEvidence { dom_id: "ad-atf-0".into(), phase: EvidencePhase::InitialLoad }],
              gpt_slots: vec![GptSlotEvidence {
                  gam_unit_path: "/123/news/atf".into(),
                  div_id: "ad-atf-0".into(),
                  sizes: vec![(300, 250)],
                  phase: EvidencePhase::InitialLoad,
              }],
              aps_calls: Vec::new(),
              page_bids: Vec::new(),
              warnings: Vec::new(),
          }
      }
  }

  // Runs the verify orchestration with `--json` over `urls` and returns parsed JSON.
  // Loads a #[cfg(test)] effective config whose `/news/*` slot is the atf slot above.
  fn run_verify_json(collector: &dyn AuditCollector, urls: impl IntoIterator<Item = &'static str>) -> serde_json::Value { /* impl in test module */ }

  #[test]
  fn verify_uses_final_url_for_matching_after_redirect() {
      let collector = FakeCollector::page(
          "https://www.example.com/",
          "https://www.example.com/news/story",
          BrowserAdEvidence::confirmed_news_slot(),
      );
      let json = run_verify_json(&collector, ["https://www.example.com/"]);

      assert_eq!(json["pages"][0]["path"], "/news/story");
      // Warning order is unspecified; assert presence, not index 0.
      let warnings = json["pages"][0]["warnings"].as_array().expect("warnings array");
      assert!(
          warnings.iter().any(|w| w["code"] == "redirected"),
          "redirect should emit a `redirected` warning"
      );
  }
  ```

  `run_verify_json` calls the same `run_verify` entry point used in production but
  with the fake collector injected and output captured; define it in the test module
  so all six listed cases share it.

- [ ] **Step 2: Run focused tests and verify failure**

  Run:

  ```bash
  cargo test -p trusted-server-cli audit::ad_templates --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: compile failure because verifier module does not exist.

- [ ] **Step 3: Wire the verifier handler**

  `AuditAdTemplatesVerifyArgs` already exists from Task 7, Step 4. Replace the Task 7
  stub so `audit::run_audit` routes `AdTemplates(Verify(args))` into a new
  `audit::ad_templates::run_verify(args)`. Do not redefine the arg struct.

- [ ] **Step 4: Implement verification orchestration**

  For each URL:
  1. Collect browser page with ad-template init script and optional scroll.
  2. Parse final URL and normalize final path.
  3. Build expected slots for final path.
  4. Build gate summary using shared core gate helper with `consent_allows_auction = None`.
  5. Add redirect warning (`code = "redirected"`) if requested path differs from final path.
  6. Compare evidence (`compare_page_evidence`) to get a `PageVerificationResult`.
  7. **Assemble the wire `PageJson`** (Task 4 type) from the pieces the comparison
     result does not carry: `url` / `final_url` / `requested_path` / `path`,
     `gates` (map the gate summary's per-gate states to `GateState`),
     `matched_slot_count`, `runtime_ad_stack_expected` (via the `From` impl on
     `RuntimeAdStackExpectedJson`), then the `slots` / `extra_evidence` / `warnings`
     from the comparison result. `PageVerificationResult` is intentionally URL- and
     gate-agnostic; this step is where per-page request context is joined in.
  8. Preserve page-level errors as a `PageJson` with `error: Some(..)` and continue
     remaining URLs.

- [ ] **Step 5: Implement exit behavior**
  - Default auditor-assist mode: return `Ok(())` for missing/partial evidence when no page-level collection errors occur.
  - `--strict`: return `Err(String)` when any non-skipped page has missing/partial slot.
  - Multi-URL page errors: JSON `ok=false`; command returns `Err(String)` after writing JSON/human output.
  - Invalid schemes: fail before browser launch and before any output.

- [ ] **Step 6: Run focused tests**

  Run:

  ```bash
  cargo test -p trusted-server-cli audit::ad_templates --target $(rustc -vV | sed -n 's/^host: //p')
  cargo test -p trusted-server-cli ad_templates --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: verifier and pure comparison tests pass.

- [ ] **Step 7: Commit**

  ```bash
  git add crates/trusted-server-cli/src/audit/ad_templates.rs crates/trusted-server-cli/src/audit/mod.rs crates/trusted-server-cli/src/run.rs crates/trusted-server-cli/src/ad_templates/output.rs
  git commit -m "Verify ad-template slots from browser evidence"
  ```

## Task 10: Add Local Browser Fixture Tests

**Files:**

- Modify: `crates/trusted-server-cli/src/audit/browser.rs`
- Modify: `crates/trusted-server-cli/src/audit/ad_templates.rs`

- [ ] **Step 1: Add test-only local HTTP fixture helper**

  In `audit::browser` tests, create a `TcpListener` serving static HTML from strings. Keep it test-only and host-target only.

  Fixture pages:
  - direct `googletag.defineSlot`;
  - `googletag.cmd.push`;
  - late `window.googletag = { cmd: [] }`;
  - late `window.apstag`;
  - lazy slot created after scroll;
  - redirect from `/` to `/news/story`;
  - navigation returning 500.

- [ ] **Step 2: Gate tests when Chrome is unavailable**

  Add helper:

  ```rust
  fn chrome_available() -> bool {
      ["chrome", "chromium", "google-chrome", "google-chrome-stable"]
          .iter()
          .any(|name| which::which(name).is_ok())
  }
  ```

  Each browser fixture test should early-return when unavailable. Do not use
  `println!` / `eprintln!`; keep the skip reason in the helper name or a skipped
  assertion message so clippy stays clean. This keeps CI portable unless Chrome is
  installed.

- [ ] **Step 3: Write fixture tests**

  Tests should assert the collector sees evidence, not real ad network behavior:
  - direct GPT evidence confirms;
  - command-queue GPT evidence confirms;
  - APS `fetchBids` evidence removes APS provider warning;
  - lazy slot appears only when `--scroll` is set;
  - redirect result uses final path;
  - failed page produces page-level error while other pages continue.

- [ ] **Step 4: Run fixture tests locally**

  Run:

  ```bash
  cargo test -p trusted-server-cli browser_fixture --target $(rustc -vV | sed -n 's/^host: //p') -- --nocapture
  ```

  Expected: pass when Chrome/Chromium exists; otherwise tests skip with explicit message.

- [ ] **Step 5: Commit**

  ```bash
  git add crates/trusted-server-cli/src/audit/browser.rs crates/trusted-server-cli/src/audit/ad_templates.rs
  git commit -m "Add browser fixtures for ad-template verification"
  ```

## Task 11: Update Documentation And Help Snapshots

**Files:**

- Modify: `docs/superpowers/specs/2026-06-26-server-side-ad-template-cli-design.md` if implementation decisions differ.
- Modify: `trusted-server.example.toml` only if command examples need harmless fictional config comments.
- Modify: `CLAUDE.md` only if verification commands or CLI command surface need to be documented.

- [ ] **Step 1: Run CLI help manually**

  Run:

  ```bash
  cargo run -p trusted-server-cli --target $(rustc -vV | sed -n 's/^host: //p') -- audit --help
  cargo run -p trusted-server-cli --target $(rustc -vV | sed -n 's/^host: //p') -- audit ad-templates verify --help
  cargo run -p trusted-server-cli --target $(rustc -vV | sed -n 's/^host: //p') -- config ad-templates --help
  ```

  Expected: nested audit commands are discoverable; hidden legacy `ts audit <url>` does not dominate help text.

- [ ] **Step 2: Update docs if help text or behavior differs from spec**

  Keep examples using `https://www.example.com/` only. Do not mention real publisher sites.

- [ ] **Step 3: Run docs format**

  Run:

  ```bash
  cd docs && npm run format
  ```

  Expected: Prettier passes.

- [ ] **Step 4: Commit**

  ```bash
  git add docs trusted-server.example.toml CLAUDE.md
  git commit -m "Document ad-template CLI verification"
  ```

  If no docs changed, skip the commit.

## Task 12: Final Verification

**Files:**

- Verify all touched files.

- [ ] **Step 1: Rust format**

  Run:

  ```bash
  cargo fmt --all -- --check
  ```

  Expected: pass.

- [ ] **Step 2: Host CLI tests**

  Run:

  ```bash
  cargo test -p trusted-server-cli --target $(rustc -vV | sed -n 's/^host: //p')
  ```

  Expected: pass. Browser fixture tests either pass or explicitly skip when Chrome/Chromium is unavailable.

- [ ] **Step 3: Workspace tests**

  Run:

  ```bash
  cargo test --workspace
  ```

  Expected: pass.

- [ ] **Step 4: Clippy**

  Run:

  ```bash
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  ```

  Expected: pass.

- [ ] **Step 5: Wasm isolation proof**

  `trusted-server-adapter-fastly` does **not** depend on `trusted-server-cli`, so the
  adapter build never compiles the CLI crate and cannot detect a CLI-crate dep leak.
  The real proof is building the **CLI crate itself** for the wasm target (its modules
  are `#[cfg(not(target_arch = "wasm32"))]`, so a wasm build must succeed with the
  host-only deps compiled out). Note the workspace default target is already
  `wasm32-wasip1`, so Steps 3–4 (`cargo test/clippy --workspace`) also build the CLI
  crate for wasm — but make the isolation check explicit:

  ```bash
  # Real CLI isolation proof: CLI crate must build for wasm with host deps excluded.
  cargo build --package trusted-server-cli --target wasm32-wasip1
  # Adapter still built to confirm the production artifact is unaffected.
  cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
  ```

  Expected: both pass. If `chromiumoxide`/`tokio`/etc. leaked into a non-target-cfg
  dependency table, the first command fails — that is the guard.

- [ ] **Step 6: Docs format**

  Run:

  ```bash
  cd docs && npm run format
  ```

  Expected: pass.

- [ ] **Step 7: Inspect final diff**

  Run:

  ```bash
  git status --short
  git diff --stat origin/server-side-ad-templates-impl...HEAD
  git log --oneline origin/server-side-ad-templates-impl..HEAD
  ```

  Expected: only intended CLI/core/doc files changed; no `.env`, operator `trusted-server.toml`, or generated browser artifacts included.

## Risks And Watch Points

- `chromiumoxide` must remain a host-only `trusted-server-cli` dependency. Any wasm build failure here means the dependency leaked.
- `ts audit <url>` compatibility must not swallow `ts audit ad-templates` as a URL.
- Runtime gate extraction (Task 1) only touches `should_run_server_side_ad_stack`
  (the navigation gate). `/__ts/page-bids` is **intentionally NOT routed** through
  `evaluate_ad_stack_gate` — its gate semantics differ (bot/prefetch skip the auction
  but keep slots; no `is_navigation`/`is_get` gate). Its parity is preserved by
  leaving it untouched, not by sharing the helper. Do not reroute page-bids. Keep
  existing publisher and page-bids tests passing.
- The browser collector must not capture page HTML, cookies, storage, request bodies, or arbitrary DOM. Only collect configured-prefix DOM IDs and ad-related evidence. Never override `navigator.webdriver`.
- `runtime_ad_stack_expected = "unknown"` is normal for live consent state; do not over-model consent unless the collector can prove it.
- Browser fixture tests must not depend on real GPT/APS network calls.
- **Deferred in this implementation:** `/__ts/page-bids` SPA observation (spec §5.2
  "when available"). `PageBidsEvidence` exists as forward scaffolding but is not
  collected (Task 8), surfaced in JSON (Task 4), or tested. Revisit if SPA route
  verification is prioritized.
- Keep generation (`ts audit ad-templates generate`) out of this PR.
