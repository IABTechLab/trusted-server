# Dedicated Server-Side Ad Templates Switch and Cache Policy Plan

> **For agentic workers:** Implement this plan task-by-task, keeping the dedicated
> template switch separate from the global auction configuration.

**Goal:** Add an explicit on/off switch for server-side ad templates, while
retaining the browser-facing cache policy from issue #1007:

- Server-side ad templates active: `Cache-Control: private, no-store`.
- Structurally inactive server-side ad templates: successful request-eligible GET
  document HTML uses exactly `Cache-Control: max-age=60`, except origin
  `private`/`no-store` policies remain unchanged.
- Non-200, non-GET, non-document, bot, prefetch, and consent-denied responses
  keep the origin browser cache policy.
- CDN-specific cache headers must not change when templates are inactive.

**Issue context:** The current cache-policy change uses the runtime
`should_run_ad_stack` gate. That gate is also affected by `[auction].enabled`,
which is not the right configuration boundary for publisher templates. A
browser can call `POST /auction`, and that endpoint is a separate server-run
auction API. The new switch must disable publisher HTML/page-bids template
delivery without disabling that API.

## Configuration decision

Add this field to the existing `[creative_opportunities]` section:

```toml
[creative_opportunities]
enabled = true
```

Use `enabled = false` to turn off server-side ad templates while retaining the
slot definitions and keeping direct `POST /auction` behavior available.

### Compatibility rules

- The field defaults to `true` when omitted, preserving existing behavior for
  deployments that already have `[creative_opportunities]` configured.
- The section remains optional. An absent section continues to mean that the
  feature is unavailable.
- Serialize the default `true` value as omitted, matching the existing
  rollback-compatibility pattern for newer creative-opportunity fields. An
  explicit `false` must remain serialized so the setting is not silently lost.
- `auction.enabled` remains a separate auction/orchestrator setting. Do not use
  it as the dedicated template switch and do not thread the new template flag
  into `POST /auction`.

## Current cache behavior to retain

The existing HTML policy block in `publisher.rs` must remain structurally
consistent with the current issue #952 behavior:

1. For an eligible request that runs the server-side ad stack and receives HTML:
   - Set `Cache-Control: private, no-store`.
   - Remove `ETag` and `Last-Modified`.
   - Remove `Surrogate-Control`, `Fastly-Surrogate-Control`, `CDN-Cache-Control`,
     and `Cloudflare-CDN-Cache-Control`.
2. For a request-eligible `200 OK` GET document HTML response where the
   server-side ad stack is structurally inactive, including an explicit template
   disable:
   - Set exactly `Cache-Control: max-age=60`, replacing origin browser policies
     as specified by issue #1007 unless the origin sends `private` or `no-store`.
   - Leave validators and all CDN-specific cache headers untouched.
3. For non-200, non-GET, non-document, bot, prefetch, and consent-denied
   responses, preserve the origin browser cache policy.
4. Apply request-scoped privacy finalization after this policy so GPT diagnostics
   and cookie-bearing responses can still require `private, no-store`; a
   cookie-bearing response therefore ends as `private, max-age=0` when it did
   not already carry a stricter policy.

## File map

### Configuration and compatibility

- `crates/trusted-server-core/src/creative_opportunities.rs`
  - Add `CreativeOpportunitiesConfig::enabled` with a default-true serde
    implementation and documentation.
  - Add a small accessor if it improves readability, but keep the source of
    truth in this config type.
  - Update config constructors and serialization tests.
- `crates/trusted-server-core/src/settings.rs`
  - Keep `creative_opportunities` parsing and runtime preparation compatible with
    the new field.
  - Make `creative_opportunity_slots()` return an empty slice when the section
    is absent or explicitly disabled, so all adapters receive one consistent
    runtime view.
  - Add TOML and environment-override coverage for `enabled = false`.
- `crates/trusted-server-core/src/config.rs`
  - Extend legacy-schema tests to prove default `enabled = true` is omitted from
    serialized blobs and remains readable by older binaries.
  - Prove an explicit `enabled = false` is serialized, making rollback failure
    loud rather than silently re-enabling templates.
- `trusted-server.example.toml`
  - Document `creative_opportunities.enabled` and show how to turn templates off
    without deleting slot definitions.
- `docs/guide/configuration.md`
  - Add the field to the creative-opportunities reference and document the
    environment override:
    `TRUSTED_SERVER__CREATIVE_OPPORTUNITIES__ENABLED=false`.
  - Clarify that this switch controls publisher HTML/page-bids template
    delivery, not direct `POST /auction` callers.
- `CHANGELOG.md`
  - Add an entry describing the dedicated template switch and cache behavior.

### Publisher execution and cache policy

- `crates/trusted-server-core/src/publisher.rs`
  - Include the dedicated flag in the initial publisher eligibility decision.
  - Do not match, dispatch, or inject server-side ad templates when the flag is
    false, even if slots are configured and `[auction].enabled` is true.
  - Apply the issue #1007 inactive-HTML cache policy in this state.
  - Update skip-reason diagnostics/telemetry so `ad_templates_disabled` is
    distinguishable from `auction_disabled`, consent denial, bots, prefetch, and
    no matching slots.
  - Update `handle_page_bids` so an explicit template disable returns the normal
    empty JSON shape (`slots: []`, `bids: {}`) rather than slot definitions. Keep
    the current `404` behavior for an absent `[creative_opportunities]` section.
  - Extend the existing SSAT cache-policy and eligibility tests.
- `crates/trusted-server-core/src/auction/endpoints.rs`
  - Do not gate `POST /auction` on the new template flag.
  - Add a regression test or test fixture proving that disabling
    `creative_opportunities.enabled` does not suppress a direct auction request
    when providers are configured.
  - Separately document/verify the existing behavior of `[auction].enabled` for
    this endpoint; do not conflate that global setting with the new template
    switch.

### Adapter propagation and browser behavior

The adapters already pass `Settings::creative_opportunity_slots()` into the
publisher/page-bids handlers. Update and verify these call sites so the central
empty-slice behavior is honored; avoid adding four divergent config checks:

- `crates/trusted-server-adapter-fastly/src/app.rs`
- `crates/trusted-server-adapter-axum/src/app.rs`
- `crates/trusted-server-adapter-cloudflare/src/app.rs`
- `crates/trusted-server-adapter-spin/src/app.rs`

No route-level flag is needed if the core `Settings` accessor and handlers are
correct. Add adapter route assertions only where existing fixtures make them
useful.

The browser runtime already defaults `window.tsjs.adSlots` and
`window.tsjs.bids` to empty values when the edge does not inject templates. If
terminology is updated, adjust these comments/tests without changing runtime
semantics:

- `crates/trusted-server-js/lib/src/core/index.ts`
- `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`
- Relevant page-bids tests under `crates/trusted-server-js/lib/test/integrations/gpt/`

## Implementation tasks

### Task 1: Add and serialize the dedicated setting

- [ ] Add `enabled: bool` to `CreativeOpportunitiesConfig` with default `true`.
- [ ] Use `skip_serializing_if` so the default value does not appear in stored
      config blobs; explicit `false` must serialize.
- [ ] Update all Rust struct literals in `creative_opportunities.rs` and
      `publisher.rs` tests.
- [ ] Add parsing, default, false-value, and environment-override tests.
- [ ] Update the legacy compatibility tests in `config.rs`.

### Task 2: Thread the setting through publisher eligibility

- [ ] Update `should_run_server_side_ad_stack` to accept the dedicated template
      flag as an explicit gate, with a descriptive parameter/doc comment.
- [ ] Ensure initial publisher slot matching and `Settings::creative_opportunity_slots`
      do not expose slots when templates are disabled.
- [ ] Preserve the existing `[auction].enabled` and consent gates as separate
      conditions.
- [ ] Add an `ad_templates_disabled` diagnostic/telemetry skip reason where the
      current branch records a skipped auction.

### Task 3: Apply the cache policy to the dedicated-off state

- [ ] Keep the active-SSAT `private, no-store` behavior and validator/CDN header
      removal unchanged.
- [ ] Keep the inactive-HTML `max-age=60` behavior from issue #1007.
- [ ] Verify that structurally inactive request-eligible responses replace the
      browser-facing `Cache-Control` for `200 OK` GET document HTML, including
      origin `no-cache` and zero-age policies, while preserving `private` and
      `no-store`.
- [ ] Preserve `ETag`, `Last-Modified`, and every CDN-specific header.
- [ ] Verify that non-200, non-GET, non-document, bot, prefetch, and
      consent-denied responses retain the origin browser cache policy.
- [ ] Verify that request-scoped GPT diagnostics and cookie privacy override this
      policy.

### Task 4: Gate SPA page-bids/template delivery

- [ ] Include `co_config.enabled` in the `ad_stack_enabled` decision in
      `handle_page_bids`.
- [ ] Return empty slots and bids for an explicit disable while retaining the
      endpoint and its existing response privacy headers.
- [ ] Keep the absent-section `404` behavior unchanged.
- [ ] Add tests for enabled, disabled, absent, consent-denied, bot, and prefetch
      cases as appropriate; preserve existing tests for `[auction].enabled=false`.

### Task 5: Protect direct `POST /auction` from accidental coupling

- [ ] Add a focused endpoint test with `creative_opportunities.enabled=false`
      and a recording provider.
- [ ] Assert that the provider still sees the direct auction request and that
      the response remains a normal OpenRTB response.
- [ ] If the test reveals that `[auction].enabled=false` also needs a separate
      product decision for `/auction`, record that as a follow-up rather than
      changing it as part of the template-switch work.

### Task 6: Update docs, examples, comments, and adapter coverage

- [ ] Update the example config, configuration guide, and changelog.
- [ ] Update stale comments that call `[auction].enabled` the universal template
      kill switch.
- [ ] Verify all four adapter call sites use the centralized disabled-slot view.
- [ ] Run JS tests if comments or tests are touched; no JS behavior change is
      expected.

## Test plan

Use target-matched commands; do not run bare workspace tests because the
workspace contains multiple runtime targets.

- [ ] `cargo test-axum -p trusted-server-core publisher`
- [ ] `cargo test-fastly`
- [ ] `cargo test-axum`
- [ ] `cargo test-cloudflare`
- [ ] `cargo test-spin`
- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy-fastly`
- [ ] `cargo clippy-axum`
- [ ] `cargo clippy-cloudflare`
- [ ] `cargo clippy-cloudflare-wasm`
- [ ] `cargo clippy-spin-native`
- [ ] `cargo clippy-spin-wasm`
- [ ] `cd crates/trusted-server-js/lib && npx vitest run` if JS tests/comments change
- [ ] `cd docs && npm run format` if documentation formatting is required

## Non-goals

- Do not change CDN-specific cache policy for inactive templates.
- Do not change adapter response privacy or cookie handling.
- Do not use `auction.rewrite_creatives` as the template switch; it controls
  creative URL rewriting, not whether the server-side template stack runs.
- Do not gate or disable direct `POST /auction` as part of this feature.
- Do not remove slot definitions when the switch is off; the point of the switch
  is to provide a reversible runtime control.
