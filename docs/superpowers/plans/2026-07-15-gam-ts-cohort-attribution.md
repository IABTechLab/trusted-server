# GAM Page-Delivery Attribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a disabled-by-default `gam_attribution_enabled` GPT option that marks every eligible request from a Trusted Server head-emitted publisher document with the fixed page-level GAM value `ts=true`.

**Architecture:** One parsed `GptConfig` instance controls both delivery paths. The raw head bootstrap is the primary, earliest queue insertion; integration-owned publisher-tag metadata adds `data-ts-gam-attribution="true"` to the synchronous bundle for a `document.currentScript`-gated fallback. Existing slot targeting, Prebid refresh cleanup, creative-opportunity forwarding, and Fastly streaming behavior remain unchanged.

**Tech Stack:** Rust 1.95, Serde, `validator`, `lol_html`, TypeScript, Vitest/jsdom, Playwright, Google Publisher Tag

---

**Issue:** [#1027](https://github.com/IABTechLab/trusted-server/issues/1027)

**Design:** `docs/superpowers/specs/2026-07-15-gam-ts-cohort-attribution-design.md`

## Fixed contracts

| Concern            | Contract                                                                                             |
| ------------------ | ---------------------------------------------------------------------------------------------------- |
| Marker             | Exactly page-level `ts=true`; no configurable name/value, alias, or dual write                       |
| Default            | `[integrations.gpt] gam_attribution_enabled = false`                                                 |
| Kill switch        | Disables only attribution; GPT proxying, shim, `adInit`, and `ts_initial` remain active              |
| Primary path       | Raw GPT bootstrap queues targeting before `if (ts.adInit) return;`                                   |
| Fallback           | Existing synchronous publisher bundle, authorized only by its own `document.currentScript` attribute |
| Publisher tag      | One tag; `data-ts-gam-attribution="true"` only for enabled GPT attribution                           |
| Streaming meaning  | Rewritten head emitted before the request, not complete response success; no new buffering           |
| Slot targeting     | `ts_initial=1` lifecycle unchanged; page-level `ts` is never added to cleanup arrays                 |
| Operator targeting | Forwarded verbatim; characterize collisions but add no validator, filter, or interception            |
| Analysis           | Descriptive GAM delivery attribution, not a causal treatment effect                                  |

Run every command block from the repository root unless that block begins with
an explicit `cd`. Treat separate command blocks as separate shell sessions.

## File map

| File                                                                                       | Responsibility                                                                         |
| ------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/integrations/gpt.rs`                                       | Parse the option, emit the inline activation flag, and expose publisher-tag metadata   |
| `crates/trusted-server-core/src/integrations/registry.rs`                                  | Define the default-empty tag-attribute hook and aggregate enabled integration metadata |
| `crates/trusted-server-core/src/tsjs.rs`                                                   | Render an attributed publisher bundle tag without changing generic/creative tag output |
| `crates/trusted-server-core/src/html_processor.rs`                                         | Pass registry-owned attributes to the single synchronous publisher bundle tag          |
| `crates/trusted-server-core/src/integrations/gpt_bootstrap.js`                             | Queue the primary `setConfig({ targeting: { ts: "true" } })` callback                  |
| `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`                               | Queue the `document.currentScript`-authorized fallback                                 |
| `crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts`                 | Execute the raw bootstrap and prove ordering/failure isolation                         |
| `crates/trusted-server-js/lib/test/integrations/gpt/index.test.ts`                         | Prove exact executing-tag activation and fail-closed cases                             |
| `crates/trusted-server-core/src/creative_opportunities.rs`                                 | Characterize verbatim operator `ts` targeting; production code remains unchanged       |
| `crates/trusted-server-core/src/publisher.rs`                                              | Characterize wire forwarding and marked-head-before-EOF streaming                      |
| `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts`                       | Freeze GPT slot cleanup behavior                                                       |
| `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`                      | Characterize Prebid-produced slot-level collisions and cleanup behavior                |
| `crates/trusted-server-cli/tests/config_env_overlay.rs`                                    | Prove the typed CLI environment override updates an existing TOML leaf                 |
| `trusted-server.example.toml`                                                              | Publish the disabled default                                                           |
| `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml` | Keep the browser fixture explicitly default-off                                        |
| `crates/trusted-server-integration-tests/browser/tests/shared/script-injection.spec.ts`    | Smoke-test absence of the activation attribute in the default-off deployment           |
| `docs/guide/integrations/gpt.md`                                                           | Document configuration, semantics, audit, reporting, and rollback prerequisites        |

## Task 1: Add the GPT attribution option and integration-owned metadata

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/gpt.rs:64-94`
- Modify: `crates/trusted-server-core/src/integrations/gpt.rs:467-510`
- Modify: `crates/trusted-server-core/src/integrations/gpt.rs:549-559`
- Modify: `crates/trusted-server-core/src/integrations/registry.rs:572-579`
- Test: `crates/trusted-server-core/src/integrations/gpt.rs`

- [ ] **Step 1: Write failing configuration and head-injector tests.**

  Add tests that deserialize omitted, explicit-false, and explicit-true values,
  then exercise both activation outputs. Use behavior-oriented assertions like:

  ```rust
  #[test]
  fn gam_attribution_defaults_to_disabled() {
      let config: GptConfig =
          serde_json::from_value(serde_json::json!({})).expect("should parse defaults");
      assert!(!config.gam_attribution_enabled);
  }

  #[test]
  fn gam_attribution_true_adds_both_activation_signals_without_a_new_insert() {
      let integration = GptIntegration::new(GptConfig {
          gam_attribution_enabled: true,
          ..test_config()
      });
      let document_state = IntegrationDocumentState::default();
      let context = IntegrationHtmlContext {
          request_host: "edge.example.com",
          request_scheme: "https",
          origin_host: "origin.example.com",
          document_state: &document_state,
      };
      let inserts = integration.head_inserts(&context);

      assert_eq!(inserts.len(), 2);
      assert!(inserts[0].contains("window.__tsjs_gam_attribution_enabled=true;"));
      assert_eq!(
          integration.tsjs_script_tag_attributes(),
          vec![("data-ts-gam-attribution", "true")]
      );
  }
  ```

  Retain the current exact-string assertion for the false first insert. Add
  `gam_attribution_enabled: false` to `test_config()` and any other full
  `GptConfig` literals.

- [ ] **Step 2: Run the focused tests and confirm RED.**

  Run:

  ```bash
  cargo test-fastly gam_attribution
  ```

  Expected: compilation fails because `GptConfig::gam_attribution_enabled` and
  `IntegrationHeadInjector::tsjs_script_tag_attributes` do not exist.

- [ ] **Step 3: Add the default-empty trait hook and parsed field.**

  Add an object-safe default method beside `head_inserts`:

  ```rust
  pub trait IntegrationHeadInjector: Send + Sync {
      fn integration_id(&self) -> &'static str;
      fn head_inserts(&self, ctx: &IntegrationHtmlContext<'_>) -> Vec<String>;

      fn tsjs_script_tag_attributes(&self) -> Vec<(&'static str, &'static str)> {
          Vec::new()
      }
  }
  ```

  Add the flat field to `GptConfig`:

  ```rust
  /// Enable page-level `ts=true` delivery attribution in GAM.
  #[serde(default)]
  pub gam_attribution_enabled: bool,
  ```

  Do not add a configurable key or value.

- [ ] **Step 4: Emit the true-only inline flag without changing false bytes.**

  Build the first insert with an empty-or-fixed fragment:

  ```rust
  let gam_attribution_flag = self
      .config
      .gam_attribution_enabled
      .then_some("window.__tsjs_gam_attribution_enabled=true;")
      .unwrap_or_default();

  let mut scripts = vec![
      format!(
          "<script>window.__tsjs_gpt_enabled=true;{gam_attribution_flag}\
           window.__tsjs_installGptShim&&window.__tsjs_installGptShim();</script>"
      ),
      format!("<script>{}</script>", GPT_BOOTSTRAP_JS),
  ];
  ```

  Verify the false string remains exactly:

  ```text
  <script>window.__tsjs_gpt_enabled=true;window.__tsjs_installGptShim&&window.__tsjs_installGptShim();</script>
  ```

- [ ] **Step 5: Override the metadata hook from the same `GptConfig`.**

  ```rust
  fn tsjs_script_tag_attributes(&self) -> Vec<(&'static str, &'static str)> {
      if self.config.gam_attribution_enabled {
          vec![("data-ts-gam-attribution", "true")]
      } else {
          Vec::new()
      }
  }
  ```

  Do not store this state in `HtmlProcessorConfig` or
  `IntegrationDocumentState`.

- [ ] **Step 6: Run focused and neighboring GPT tests.**

  ```bash
  cargo test-fastly gam_attribution
  cargo test-fastly head_injector
  ```

  Expected: PASS; false preserves two current inserts, true adds the flag and
  metadata while still emitting two inserts when `slim_prebid_url` is absent.

- [ ] **Step 7: Commit.**

  ```bash
  git add crates/trusted-server-core/src/integrations/gpt.rs crates/trusted-server-core/src/integrations/registry.rs
  git commit -m "Add GPT GAM attribution option"
  ```

## Task 2: Put activation metadata on only the publisher bundle tag

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/registry.rs:1043-1054`
- Modify: `crates/trusted-server-core/src/tsjs.rs:11-39`
- Modify: `crates/trusted-server-core/src/html_processor.rs:324-360`
- Test: `crates/trusted-server-core/src/integrations/registry.rs`
- Test: `crates/trusted-server-core/src/tsjs.rs:161-187`
- Test: `crates/trusted-server-core/src/html_processor.rs:768-820`
- Test: `crates/trusted-server-core/src/html_processor.rs:1637-1671`

- [ ] **Step 1: Write failing registry and tag-rendering tests.**

  Add a test head injector whose metadata method returns the attribution pair.
  Assert registry aggregation is deterministic and preserves the default-empty
  behavior of injectors that implement only `head_inserts`.

  Add exact tag tests:

  ```rust
  #[test]
  fn publisher_script_tag_renders_static_attributes() {
      let ids = ["gpt"];
      let src = tsjs_script_src(&ids);

      assert_eq!(
          tsjs_script_tag_with_attributes(
              &ids,
              &[("data-ts-gam-attribution", "true")]
          ),
          format!(
              "<script src=\"{src}\" id=\"trustedserver-js\" \
               data-ts-gam-attribution=\"true\"></script>"
          )
      );
      assert_eq!(
          tsjs_script_tag(&ids),
          format!("<script src=\"{src}\" id=\"trustedserver-js\"></script>")
      );
  }
  ```

  The final string must contain no formatting whitespace introduced only by the
  multiline example.

- [ ] **Step 2: Write a failing HTML matrix test.**

  Process `<html><head></head><body></body></html>` with:
  1. a real enabled GPT registry with attribution true;
  2. enabled GPT with attribution false; and
  3. no GPT integration.

  Assert exactly one `#trustedserver-js` tag in every case, the attribute only
  in case 1, and integration head inserts remain before the external bundle.
  Also retain the generic `tsjs_unified_script_tag()` exact-output test so
  creative/all-modules callers stay unmarked.

- [ ] **Step 3: Run the tests and confirm RED.**

  ```bash
  cargo test-fastly tsjs_script_tag
  cargo test-fastly integration_head_injector
  ```

  Expected: FAIL because the registry aggregator and attributed publisher
  helper are not implemented.

- [ ] **Step 4: Aggregate integration-owned static attributes.**

  Add beside `IntegrationRegistry::head_inserts`:

  ```rust
  #[must_use]
  pub fn tsjs_script_tag_attributes(&self) -> Vec<(&'static str, &'static str)> {
      self.inner
          .head_injectors
          .iter()
          .flat_map(|injector| injector.tsjs_script_tag_attributes())
          .collect()
  }
  ```

  Keep the hook default-empty so existing integration injectors and test doubles
  compile without changes.

- [ ] **Step 5: Add the publisher-only tag helper.**

  Render only trusted, compile-time static attribute pairs:

  ```rust
  #[must_use]
  pub fn tsjs_script_tag_with_attributes(
      module_ids: &[&str],
      attributes: &[(&'static str, &'static str)],
  ) -> String {
      let attributes = attributes
          .iter()
          .map(|(name, value)| format!(" {name}=\"{value}\""))
          .collect::<String>();
      format!(
          "<script src=\"{}\" id=\"trustedserver-js\"{attributes}></script>",
          tsjs_script_src(module_ids)
      )
  }
  ```

  Have `tsjs_script_tag(module_ids)` retain its exact output, either directly or
  by delegating with an empty slice. Do not change
  `tsjs_unified_script_tag()` or either creative call site.

- [ ] **Step 6: Wire only the publisher HTML path.**

  Replace the single `html_processor.rs` call with:

  ```rust
  let immediate_ids = integrations.js_module_ids_immediate();
  let script_attributes = integrations.tsjs_script_tag_attributes();
  snippet.push_str(&tsjs::tsjs_script_tag_with_attributes(
      &immediate_ids,
      &script_attributes,
  ));
  ```

  Preserve source order: ad slots, integration head inserts, diagnostics
  bootstrap, one synchronous bundle, diagnostics module, deferred bundles.

- [ ] **Step 7: Run focused tests.**

  ```bash
  cargo test-fastly tsjs_script_tag
  cargo test-fastly integration_head_injector
  cargo test-fastly golden_script_tag
  ```

  Expected: PASS; false/non-GPT/generic output is unmarked and true output has
  one attributed publisher tag.

- [ ] **Step 8: Commit.**

  ```bash
  git add crates/trusted-server-core/src/integrations/registry.rs crates/trusted-server-core/src/tsjs.rs crates/trusted-server-core/src/html_processor.rs
  git commit -m "Authorize GAM attribution bundle"
  ```

## Task 3: Queue the primary marker before the bootstrap guard

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/gpt_bootstrap.js:17-45`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts:8-220`
- Test: `crates/trusted-server-core/src/integrations/gpt.rs:1131-1454`

- [ ] **Step 1: Extend the raw-source test harness.**

  Add the optional page flag and `setConfig` surface:

  ```typescript
  interface MockGoogleTag {
    cmd: MockCommandQueue
    setConfig?: (config: Record<string, unknown>) => void
    // retain the existing members
  }

  type TestWindow = Omit<Window, 'tsjs'> & {
    googletag?: MockGoogleTag
    tsjs?: Partial<TsjsApi>
    __tsjs_gam_attribution_enabled?: boolean
  }

  function makeGoogleTag(
    overrides: Partial<MockGoogleTag> = {}
  ): MockGoogleTag {
    return {
      cmd: [],
      defineSlot: vi.fn(),
      pubads: vi.fn(() => ({})),
      enableServices: vi.fn(),
      display: vi.fn(),
      ...overrides,
    }
  }
  ```

  Delete the flag in both `beforeEach` and `afterEach`.

- [ ] **Step 2: Write failing behavioral tests.**

  Cover all of these independently:
  - default/false plus a preinstalled `ts.adInit` returns without creating
    `window.googletag`;
  - true queues the exact string-valued targeting callback before a publisher
    callback appended after `runBootstrap()`;
  - true plus preinstalled `ts.adInit` still queues and applies targeting but
    does not replace `adInit` or install the fallback scheduler;
  - missing `setConfig` is a no-op and the initial-load detector and `adInit`
    still install;
  - throwing `setConfig` is caught inside the marker callback, and a later
    publisher callback still executes;
  - the wrapped `disableInitialLoad` path still records
    `ts.gptInitialLoadDisabled`.

  Use a real array queue, append a publisher spy after bootstrap execution, and
  drain a snapshot in order:

  ```typescript
  const queue: Array<() => void> = []
  const setConfig = vi.fn()
  ;(window as TestWindow).googletag = makeGoogleTag({ cmd: queue, setConfig })
  ;(window as TestWindow).__tsjs_gam_attribution_enabled = true

  runBootstrap()
  const publisherCommand = vi.fn()
  queue.push(publisherCommand)
  ;[...queue].forEach((command) => command())

  expect(setConfig).toHaveBeenCalledWith({ targeting: { ts: 'true' } })
  expect(setConfig.mock.invocationCallOrder[0]).toBeLessThan(
    publisherCommand.mock.invocationCallOrder[0]
  )
  ```

- [ ] **Step 3: Run the raw bootstrap tests and confirm RED.**

  ```bash
  cd crates/trusted-server-js/lib
  npx vitest run test/integrations/gpt/gpt_bootstrap.test.ts
  ```

  Expected: targeting assertions fail because the raw bootstrap returns before
  any marker enqueue.

- [ ] **Step 4: Implement one flag-gated queue initialization before the guard.**

  Preserve the local `ts` namespace and reuse `tag` in the existing detector:

  ```javascript
  var ts = (window.tsjs = window.tsjs || {})
  var tag

  if (window.__tsjs_gam_attribution_enabled === true) {
    tag = window.googletag = window.googletag || { cmd: [] }
    tag.cmd = tag.cmd || []
    tag.cmd.push(function () {
      try {
        var gpt = window.googletag
        if (gpt && typeof gpt.setConfig === 'function') {
          // "ts" is the fixed GAM key, not the local window.tsjs alias.
          gpt.setConfig({ targeting: { ts: 'true' } })
        }
      } catch (_) {
        // Attribution must not interrupt the existing bootstrap queue.
      }
    })
  }

  if (ts.adInit) return

  tag = tag || (window.googletag = window.googletag || { cmd: [] })
  tag.cmd = tag.cmd || []
  tag.cmd.push(function () {
    // existing initial-load detector body, unchanged
  })
  ```

  Do not add a global deduplication state machine, network call, beacon, cookie
  read, slot-level key, or third head insert.

- [ ] **Step 5: Add/retain Rust source-order assertions.**

  In `gpt.rs`, assert the embedded bootstrap's attribution enqueue occurs before
  `if (ts.adInit) return;` and before the executable
  `googletag.display(` and `googletag.pubads().refresh(` tokens. Do not compare
  against comment-only `display()`/`refresh()` text. Retain `ts_initial`
  assertions and the two-insert count.

- [ ] **Step 6: Run focused JS and Rust tests.**

  ```bash
  cd crates/trusted-server-js/lib
  npx vitest run test/integrations/gpt/gpt_bootstrap.test.ts
  cd ../../..
  cargo test-fastly head_inserts
  ```

  Expected: PASS; default behavior is unchanged and every failure mode is
  isolated from the existing bootstrap.

- [ ] **Step 7: Commit.**

  ```bash
  git add crates/trusted-server-core/src/integrations/gpt_bootstrap.js crates/trusted-server-core/src/integrations/gpt.rs crates/trusted-server-js/lib/test/integrations/gpt/gpt_bootstrap.test.ts
  git commit -m "Queue page-level GAM attribution"
  ```

## Task 4: Add the exact-executing-tag bundle fallback

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts:259-307`
- Modify: `crates/trusted-server-js/lib/src/integrations/gpt/index.ts:1878-1898`
- Modify: `crates/trusted-server-js/lib/test/integrations/gpt/index.test.ts:350-421`

- [ ] **Step 1: Add a test helper that controls `document.currentScript`.**

  In the runtime-gating suite, install a configurable getter before a fresh
  dynamic import and restore it afterward:

  ```typescript
  let executingScript: HTMLScriptElement | null

  Object.defineProperty(document, 'currentScript', {
    configurable: true,
    get: () => executingScript,
  })
  ```

  Use actual `<script>` elements. Do not authorize tests through
  `querySelector('#trustedserver-js')`.

- [ ] **Step 2: Write failing activation and fail-closed tests.**

  Prove:
  - the executing tag with exact `data-ts-gam-attribution="true"` reuses the
    existing `cmd` array and queues `{ targeting: { ts: 'true' } }`;
  - missing, empty, `"false"`, or non-exact attribute values do not activate;
  - `document.currentScript === null` fails closed;
  - a marked duplicate-ID decoy does not activate when the executing tag is
    unmarked;
  - a marked executing tag can activate even if its ID was copied via
    `srcdoc`/`document.write`, documenting contamination rather than asserting
    an untrue rewrite guarantee;
  - the marker queue push occurs while `window.tsjs.adInit` is still absent;
  - missing or throwing `setConfig` does not prevent `adInit`, SPA, loader, or
    render-bridge installation;
  - GPT-enabled but attribution-disabled imports retain current shim/stub
    behavior while queuing no marker;
  - with neither the old enable flag nor the new attribute, a plain import still
    leaves `window.googletag` undefined.

- [ ] **Step 3: Run the suite and confirm RED.**

  ```bash
  cd crates/trusted-server-js/lib
  npx vitest run test/integrations/gpt/index.test.ts
  ```

  Expected: activation cases fail because the bundle does not inspect its
  executing tag.

- [ ] **Step 4: Capture the executing tag during module evaluation.**

  Add one module-local capture, not a global lookup:

  ```typescript
  const executingPublisherScript =
    typeof document === 'undefined' ? null : document.currentScript
  ```

  Preserve the `HTMLOrSVGScriptElement | null` type returned by the DOM API or
  narrow safely by capability; do not use an unchecked ID query.

- [ ] **Step 5: Add a defensive helper using existing GPT types.**

  ```typescript
  function installTrustedServerPageTargeting(): void {
    if (
      executingPublisherScript?.getAttribute('data-ts-gam-attribution') !==
      'true'
    ) {
      return
    }

    const win = window as GptWindow
    const tag = ensureGoogleTagStub(win)
    tag.cmd!.push(() => {
      try {
        const gpt = win.googletag
        if (typeof gpt?.setConfig === 'function') {
          gpt.setConfig({ targeting: { ts: 'true' } })
        }
      } catch (error) {
        log.warn('[tsjs-gpt] GAM attribution targeting failed', error)
      }
    })
  }
  ```

  Reuse `GoogleTagConfig` and optional `GoogleTag.setConfig` exactly as they
  exist. Do not add a new type surface or throw from the callback.

- [ ] **Step 6: Call it in the approved initialization order.**

  ```typescript
  if (win.__tsjs_gpt_enabled === true) {
    installGptShim()
  }

  installTrustedServerPageTargeting()
  installTsAdInit()
  installSpaAuctionHook()
  installSlimPrebidLoader()
  installTsRenderBridge()
  ```

  The bootstrap remains primary; duplicate same-value page targeting is
  intentional and idempotent.

- [ ] **Step 7: Run focused and neighboring GPT suites.**

  ```bash
  cd crates/trusted-server-js/lib
  npx vitest run \
    test/integrations/gpt/index.test.ts \
    test/integrations/gpt/gpt_bootstrap.test.ts \
    test/integrations/gpt/ad_init.test.ts
  ```

  Expected: PASS with no new script element, request, or slot targeting.

- [ ] **Step 8: Commit.**

  ```bash
  git add crates/trusted-server-js/lib/src/integrations/gpt/index.ts crates/trusted-server-js/lib/test/integrations/gpt/index.test.ts
  git commit -m "Add GAM attribution bundle fallback"
  ```

## Task 5: Characterize targeting collisions and preserve cleanup boundaries

**Files:**

- Test: `crates/trusted-server-core/src/creative_opportunities.rs:1710-1760`
- Test: `crates/trusted-server-core/src/publisher.rs:8737-8756`
- Test: `crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts:1870`
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts:1851`
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts` publisher-settings coverage

This task adds characterization only. Do not change
`CreativeOpportunitySlot` parsing, `to_ad_slot`, `build_slot_json`,
`TS_BASE_TARGETING_KEYS`, `TS_REFRESH_TARGETING_KEYS`, or Prebid interception
code.

- [ ] **Step 1: Freeze verbatim operator targeting in Rust tests.**

  Add `"ts": "operator-value"` to a test `CreativeOpportunitySlot.targeting`.
  Assert both:

  ```rust
  assert_eq!(
      ad_slot.targeting.get("ts"),
      Some(&serde_json::Value::String("operator-value".to_owned()))
  );
  assert_eq!(
      build_slot_json(&slot, &config, "example")["targeting"]["ts"],
      "operator-value"
  );
  ```

  This is evidence for the external launch audit, not authorization to filter
  the key.

- [ ] **Step 2: Freeze GPT slot-cleanup behavior.**

  Extend the existing stale-targeting route test with a slot-level `ts` value.
  Assert:

  ```typescript
  expect(clearTargeting).not.toHaveBeenCalledWith('ts')
  expect(clearTargeting).toHaveBeenCalledWith('ts_initial')
  ```

  The page-level marker is not a member of the slot cleanup list.

- [ ] **Step 3: Characterize publisher `bidderSettings` preservation.**

  Seed:

  ```typescript
  mockPbjs.bidderSettings = {
    exampleBidder: {
      adserverTargeting: [{ key: 'ts', val: () => 'publisher-value' }],
    },
  }
  ```

  Run the existing wrapped `requestBids` path and assert that adding
  `trustedServer.allowAlternateBidderCodes` does not delete or rewrite the
  publisher's `adserverTargeting` entry. Delete or reset `bidderSettings` in the
  suite cleanup so this collision fixture cannot leak into unrelated tests.

- [ ] **Step 4: Characterize `setTargetingForGPTAsync` collision behavior.**

  In the existing Prebid refresh test, make the mock
  `setTargetingForGPTAsync` apply slot-level `ts=prebid-value`. Assert it runs
  before delegated refresh, `clearTargeting('ts')` was not called, and no
  Trusted Server wrapper filters the value. Retain the current
  `ts_initial`/`hb_*` clearing assertions.

- [ ] **Step 5: Run characterization suites.**

  ```bash
  cargo test-fastly to_ad_slot
  cargo test-fastly ad_slots_script_contains_slot_data
  cd crates/trusted-server-js/lib
  npx vitest run \
    test/integrations/gpt/ad_init.test.ts \
    test/integrations/prebid/index.test.ts
  ```

  Expected: PASS without production-code changes. If a test reveals current
  filtering, stop and reconcile the spec instead of adding compensating
  behavior.

- [ ] **Step 6: Commit.**

  ```bash
  git add crates/trusted-server-core/src/creative_opportunities.rs crates/trusted-server-core/src/publisher.rs crates/trusted-server-js/lib/test/integrations/gpt/ad_init.test.ts crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts
  git commit -m "Characterize targeting collisions"
  ```

## Task 6: Characterize marked-head streaming before origin EOF

**Files:**

- Test: `crates/trusted-server-core/src/publisher.rs:7756-7885`
- Verify unchanged: `crates/trusted-server-core/src/streaming_processor.rs:297-364`
- Verify unchanged: `crates/trusted-server-adapter-fastly/src/main.rs:322-358`

This task must not add buffering or adapter logic.

- [ ] **Step 1: Add a test-only settings-aware streaming helper.**

  Let the existing `streaming_finalize_response` delegate to a sibling that
  accepts `Settings`. The helper still builds
  `IntegrationRegistry::new(&settings)` and polls the same lazy body; do not
  alter production parameters.

- [ ] **Step 2: Add the before-EOF regression.**

  Configure enabled GPT attribution in test settings, send an HTML origin chunk
  followed by permanent `Pending`, and poll exactly one client chunk:

  ```rust
  #[test]
  fn streaming_finalize_emits_gam_attribution_head_before_origin_eof() {
      let mut settings = create_test_settings();
      settings
          .integrations
          .insert_config(
              "gpt",
              &serde_json::json!({
                  "enabled": true,
                  "gam_attribution_enabled": true
              }),
          )
          .expect("should insert GPT config");

      let body = streaming_finalize_response_with_settings(
          html_stream_params("", None),
          origin_chunk_then_pending(bytes::Bytes::from_static(
              b"<html><head></head><body><p>origin remains pending</p>"
          )),
          settings,
      );
      let html = String::from_utf8(first_lazy_body_chunk(body).to_vec())
          .expect("should emit UTF-8 HTML");

      assert!(html.contains("__tsjs_gam_attribution_enabled=true"));
      assert!(html.contains("data-ts-gam-attribution=\"true\""));
  }
  ```

  If the rewriter needs more input to flush, use the established large-prefix or
  auction-hold fixture pattern; never complete the origin stream to make the
  test pass.

- [ ] **Step 3: Run the new and existing streaming/error regressions.**

  ```bash
  cargo test-fastly streaming_finalize_emits_gam_attribution_head_before_origin_eof
  cargo test-fastly streaming_finalize_emits_compressed_html_before_origin_eof
  cargo test-fastly stream_publisher_body_surfaces_mid_stream_decode_error
  ```

  Expected: PASS. The new marker is in the first rewritten head chunk, and the
  existing later decoder error still surfaces as truncation rather than
  reclassification or buffering.

- [ ] **Step 4: Confirm the diff contains test code only for this task.**

  ```bash
  git diff -- crates/trusted-server-core/src/publisher.rs crates/trusted-server-core/src/streaming_processor.rs crates/trusted-server-adapter-fastly/src/main.rs
  ```

  Expected: `publisher.rs` test module changes only; no production streaming or
  Fastly adapter changes.

- [ ] **Step 5: Commit.**

  ```bash
  git add crates/trusted-server-core/src/publisher.rs
  git commit -m "Define GAM attribution streaming semantics"
  ```

## Task 7: Publish configuration, environment, browser-smoke, and operator docs

**Files:**

- Modify: `trusted-server.example.toml:105-109`
- Modify: `crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml:87-91`
- Modify: `crates/trusted-server-cli/tests/config_env_overlay.rs`
- Modify: `crates/trusted-server-integration-tests/browser/tests/shared/script-injection.spec.ts:4-12`
- Modify: `docs/guide/integrations/gpt.md:49-110`

- [ ] **Step 1: Write the failing typed-CLI overlay test.**

  Add:

  ```rust
  const GAM_ATTRIBUTION_ENV: &str =
      "TRUSTED_SERVER__INTEGRATIONS__GPT__GAM_ATTRIBUTION_ENABLED";
  ```

  Use `ts config push` with `GAM_ATTRIBUTION_ENV=true` and inspect the stored
  blob envelope:

  ```rust
  assert_eq!(
      envelope["data"]["integrations"]["gpt"]["gam_attribution_enabled"],
      serde_json::Value::Bool(true)
  );
  ```

  Run before adding the fixture leaf to demonstrate EdgeZero v0.0.4 cannot
  create the missing leaf.

- [ ] **Step 2: Confirm the overlay test is RED.**

  ```bash
  ./scripts/test-cli.sh
  ```

  Expected: the new assertion fails because the base TOML does not yet contain
  `gam_attribution_enabled`.

- [ ] **Step 3: Add the explicit disabled leaf to both TOML files.**

  ```toml
  [integrations.gpt]
  enabled = false
  gam_attribution_enabled = false
  ```

  Preserve the surrounding GPT URL/cache/rewrite fields. The comprehensive
  browser fixture remains GPT-disabled and attribution-disabled.

- [ ] **Step 4: Extend the default-off browser smoke.**

  In `script-injection.spec.ts`, keep the existing one-tag/source assertion and
  add:

  ```typescript
  await expect(script).not.toHaveAttribute('data-ts-gam-attribution')
  ```

  Do not add a second Playwright project or enable GPT globally. Enabled
  behavior is covered deterministically by Rust/Vitest and later by the external
  deployment validation.

- [ ] **Step 5: Update the GPT configuration reference.**

  Add `gam_attribution_enabled = false` to the snippet and a table row stating:
  - type boolean;
  - optional;
  - default false;
  - independently enables fixed page-level GAM `ts=true` attribution.

  State that an environment override works only when this TOML leaf already
  exists:

  ```text
  TRUSTED_SERVER__INTEGRATIONS__GPT__GAM_ATTRIBUTION_ENABLED
  ```

- [ ] **Step 6: Add the operator attribution section.**

  Near “Command Queue Patch,” document:
  - `ts=true` is page-level and persists for the browser document;
  - `ts_initial=1` remains slot-level and is cleared on its existing lifecycle;
  - the marker means an eligible, non-cloned TS publisher pipeline emitted the
    head, not that a TS bid won or the body completed;
  - the fixed short key must pass publisher GPT, Prebid
    `bidderSettings/adserverTargeting/setTargetingForGPTAsync`, effective
    creative-opportunity map, and GAM-consumer collision audits;
  - there is no runtime filtering of operator targeting;
  - privacy/CSP review, reportable predefined value, Enhanced-or-exact-legacy
    report dry run, and excluded-path zero counts are launch gates;
  - Report A is the non-duplicated eligible-scope total, Report B uses the same
    dates/time zone/inventory/metrics plus exactly `ts=true`, and legacy
    Key-values rows are never summed;
  - control is `Report A - Report B`; every interpreted metric must satisfy
    `0 <= Report B <= Report A`, and a violation invalidates the complete pair
    rather than being clamped;
  - saved report pairs are exported after the same reporting-latency and
    invalid-traffic window, and monitoring/synthetic samples do not prove
    per-response marker completeness;
  - normal rollback stops and verifies routing, records the boundary, keeps
    attribution enabled through the excluded open-document drain, and flips the
    setting only after marked traffic reaches zero;
  - an emergency kill flips immediately and invalidates the affected/drain
    interval.

  Label GAM results descriptive delivery attribution, not causal analysis. Do
  not update `docs/guide/integrations/gam.md`, which describes a separate future
  integration.

- [ ] **Step 7: Run configuration, docs, and browser checks.**

  ```bash
  ./scripts/test-cli.sh
  cd docs
  npm run format
  npm run build
  cd ..
  ./scripts/integration-tests-browser.sh
  ```

  Expected: CLI overlay PASS; docs format/build PASS; Next.js and WordPress
  browser suites PASS with an unmarked default-off publisher tag.

- [ ] **Step 8: Commit.**

  ```bash
  git add trusted-server.example.toml crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml crates/trusted-server-cli/tests/config_env_overlay.rs crates/trusted-server-integration-tests/browser/tests/shared/script-injection.spec.ts docs/guide/integrations/gpt.md
  git commit -m "Document GAM attribution configuration"
  ```

## Task 8: Run the complete repository verification matrix

**Files:**

- Verify all files changed in Tasks 1-7
- Follow: `CLAUDE.md`

- [ ] **Step 1: Format and inspect before the expensive matrix.**

  ```bash
  cargo fmt --all
  cd crates/trusted-server-js/lib
  npm run format
  cd ../../../docs
  npm run format
  cd ..
  git diff --check
  git status --short
  ```

  Expected: formatters succeed and only intended feature files are modified. If
  a formatter changes committed content, inspect and commit that correction
  before continuing.

- [ ] **Step 2: Run all target-matched Rust lints.**

  ```bash
  cargo clippy-fastly
  cargo clippy-axum
  cargo clippy-cloudflare
  cargo clippy-cloudflare-wasm
  cargo clippy-spin-native
  cargo clippy-spin-wasm
  ```

  Expected: PASS with `-D warnings`. Do not substitute bare workspace clippy.

- [ ] **Step 3: Run all target-matched Rust suites.**

  ```bash
  cargo test-fastly
  cargo test-axum
  cargo test-cloudflare
  cargo test-spin
  cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
  ./scripts/test-cli.sh
  ```

  Expected: PASS. Do not use bare `cargo test --workspace`.

- [ ] **Step 4: Run the full TypeScript build, tests, lint, and format check.**

  ```bash
  cd crates/trusted-server-js/lib
  node build-all.mjs
  npx vitest run
  npm run lint
  npm run format
  ```

  Expected: all bundles build and all tests/lint/format checks pass.

- [ ] **Step 5: Run docs and browser integration gates.**

  ```bash
  cd docs
  npm run format
  npm run build
  cd ..
  ./scripts/integration-tests-browser.sh
  ```

  Expected: docs build and both browser frameworks pass.

- [ ] **Step 6: Recheck the final diff and commits.**

  ```bash
  cargo fmt --all -- --check
  git diff --check
  git status --short
  git log --oneline --decorate -8
  ```

  Expected: a clean worktree, focused implementation commits, and no
  production changes to creative-opportunity filtering, Prebid interception,
  streaming buffering, or adapter behavior.

## External launch gates (not repository implementation)

Do not mark implementation complete as “experiment ready” until an operator
records these artifacts for the target GAM network. These steps intentionally
remain separate from Tasks 1-8 because they require publisher/router/GAM access.

- [ ] Create or verify the one predefined reportable key/value `ts=true` and
      retain the target-network 10-character preflight evidence.
- [ ] Freeze the exact eligible route/site/ad-unit/format/time-zone/window
      manifest and derive both saved reports and every exclusion query from it.
- [ ] Record Enhanced key-value availability, metric compatibility, owner
      approval, and displayed billing terms; otherwise enable the key for legacy
      reporting and validate the exact `ts=true` filter without summing
      Key-values rows.
- [ ] Retain zero-count evidence or an identical independent filter for every
      excluded path: non-experiment TS, smoke/direct/operations, IMA/video,
      direct tags, server-side GAM, and excluded nested inventory.
- [ ] Complete publisher GPT, Prebid, effective creative-opportunity map, and
      every GAM targeting-consumer collision audits with owners, queries,
      timestamps, and re-audit triggers.
- [ ] Complete privacy/data-governance and CSP eligibility reviews.
- [ ] With treatment routing stopped, deploy
      `gam_attribution_enabled = true` to the validation deployment.
- [ ] Validate initial/lazy/refresh/publisher-owned/SPA requests, control
      absence, `ts_initial` isolation, CSP execution, fallback incident
      detection, and the disabled kill switch.
- [ ] Save a short paired Report A/Report B dry run and verify
      `0 <= Report B <= Report A` for every selected metric.
- [ ] Start the sticky treatment cohort only after every gate passes; use GAM
      share versus router allocation only as a diagnostic.
- [ ] For normal rollback, stop and verify new treatment routing, close the
      clean report boundary, keep attribution enabled while marked documents
      drain through the excluded interval, and disable only after marked traffic
      reaches zero for the agreed interval. For an emergency kill, disable
      immediately and invalidate the affected plus drain intervals.

## Definition of done

- The repository behavior and automated tests satisfy every code-addressable
  design acceptance criterion; experiment-readiness remains explicitly gated
  on the external checklist above.
- `gam_attribution_enabled` is default-off, externally overridable only from an
  existing TOML leaf, and does not disable any existing GPT behavior.
- Enabled publisher documents receive both activation signals, and at least one
  callback successfully applies the single effective page-level `ts=true` value
  before publisher GPT requests. The same-value fallback callback remains safe.
- Generic/creative tags, production/control pages, and default-off fixtures are
  unmarked.
- `ts_initial`, slot cleanup, arbitrary operator targeting, Prebid behavior,
  Fastly streaming, and adapters have no production behavior changes.
- All `CLAUDE.md` target-matched gates pass.
- External GAM artifacts are explicitly outstanding until an authorized
  operator completes the launch checklist.
