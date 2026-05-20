# `ts-config` CLI — Design Specification

_Author · 2026-05-19_

---

## 1. Problem Statement

`creative-opportunities.toml` is the runtime contract between Trusted Server and the
publisher's ad stack. Every slot definition in this file drives three load-bearing
systems simultaneously: URL-pattern matching at the edge (which pages fire auctions),
GPT slot definition injected into `<head>` (which ad units the browser renders), and
PBS/APS bid requests (which SSPs receive impressions). A misconfiguration silently
degrades revenue — wrong glob pattern means no auction fires on that page; wrong APS
`slot_id` means TAM bids are empty; wrong `div_id` means GPT renders into a container
that doesn't exist in the DOM.

Today there is no tooling for people who maintain this file outside of a full `cargo
build`:

- **`build.rs`** validates slot IDs against `[A-Za-z0-9_-]+` and panics — a
  Rust-facing, non-actionable signal for JS auditors and publisher managers.
- **Silent glob normalization** (`creative_opportunities.rs:87`) — when a pattern
  contains `**` in an invalid position, `Pattern::new()` fails and the `**` is
  silently replaced with `*` before retrying. Authors write `/b**` intending
  multi-segment matching; the file accepts it; the pattern silently changes semantics.
  The pattern `/b**` is not valid glob syntax — it is an invalid pattern that happens
  to work by accident, matching the same paths as `/b*` because
  `require_literal_separator` is `false`. The editor has no idea.
- **No path test utility** — there is no way to answer "does this URL match slot X?"
  short of writing a Rust test or running the full Fastly simulator.
- **No generation path** — a new publisher onboarding has to hand-craft the TOML from
  GPT and APS JS calls observed in DevTools. This is slow, error-prone, and requires
  knowledge the JS auditor already has but cannot currently automate.

The onboarding engineer (JS auditor) stands at the intersection of these problems. They
receive a publisher URL, must generate a correct `creative-opportunities.toml` from the
live page, validate it, and confirm that it matches the right URLs before deploying.
Today this workflow has no dedicated tooling at any step.

---

## 2. Goals

1. Give JS auditors and publisher managers a **validate-then-deploy** workflow for
   `creative-opportunities.toml` that requires no Rust toolchain knowledge.
2. Provide **actionable, human-readable error messages** for every validation rule —
   not a panic, not a build-time red herring.
3. Expose the **glob normalization side-effect** explicitly: when a pattern's semantics
   change due to `**`→`*` substitution, the tool prints a warning with the original
   and effective pattern so the author can decide whether to fix or accept.
4. Enable **path simulation** — given a path and a TOML file, show which slots match.
   This is the offline equivalent of the runtime `match_slots()` call.
5. Enable **slot generation from a live publisher page** using browser automation to
   capture GPT `defineSlot()` calls and APS `fetchBids()` calls, map them to
   `creative-opportunities.toml` schema, and emit a draft with `# TODO` markers for
   fields the browser can't populate (floor prices, PBS stored request IDs).
6. Integrate cleanly into **CI/CD** — deterministic exit codes, machine-readable output
   flag, and no interactive prompts in non-TTY mode.

---

## 3. Non-Goals

- **Replacing `build.rs` validation** — `build.rs` remains the build-time gate. `ts-config`
  is a pre-flight check, not a substitute for compile-time enforcement.
- **PBS stored-request sync** — the tool can flag that a slot ID should have a stored
  request in PBS, but it cannot create or update PBS stored requests. That surface is
  publisher/PBS admin territory.
- **GAM line-item management** — no GAM API integration. Floor prices and targeting
  key-values are manually curated by ad ops; the tool generates `# TODO` stubs.
- **Runtime config hot-reload** — slots are compiled into the WASM binary via
  `include_str!()`. The CLI validates the file before that compile step. KV-backed
  live config is tracked as a future phase in the main server-side ad templates spec
  (§9.5 of that design).
- **Diff between TOML and live PBS/GAM state** — comparing the local file against
  live PBS stored requests or live GAM line items is out of scope. That is an
  operational observability concern, not a config authoring concern.

---

## 4. Architecture

Two standalone tools with a shared schema understanding, no shared binary:

```
crates/trusted-server-cli/       ← Rust binary (native target, excluded from workspace)
  src/main.rs                    ← ts-config validate / match / lint / check subcommands
  src/validate.rs                ← validation rules
  src/match_cmd.rs               ← glob matching
  src/report.rs                  ← output formatting (human + JSON)

packages/js-asset-auditor/       ← Node.js package (feature/js-asset-auditor branch)
  lib/generate-slots.mjs         ← NEW: browser interception + TOML emission
  bin/ts-config-generate         ← NEW: CLI entrypoint for generate command
```

The two tools share no runtime dependency and can be installed independently. The Rust
binary handles static file operations (validate, match, lint, check). The Node.js tool
handles browser automation (generate). A Claude Code plugin — added later in Phase 2
— wraps both tools behind a conversational interface for interactive onboarding.

**Branch dependency note:** The Rust binary has no dependency on `feature/js-asset-auditor`
and ships as PR A independently. The Node.js `generate-slots` work ships as PR B after
`feature/js-asset-auditor` is merged into `main`. The two PRs are independent and can
be reviewed in any order.

### 4.1 Rust binary: `ts-config`

#### Why Rust for validate/match

Validation logic must be **identical to the runtime** in `creative_opportunities.rs`.
Rust lets us directly mirror `validate_slot_id()`, `matches_path()`, and the glob
normalization behavior. A JS reimplementation of the glob semantics would drift. A Rust
binary that uses the same `glob` crate version is the only guarantee that "it validates
clean" → "it will work at runtime."

The binary is excluded from the workspace (`wasm32-wasip1` default target is
incompatible with a native CLI). It follows the `crates/integration-tests/` pattern:
its own `Cargo.toml` with explicit version pins, excluded from `[workspace]`,
buildable with `cargo build --manifest-path crates/trusted-server-cli/Cargo.toml`.

#### Subcommands

```
ts-config validate [--config PATH] [--json]
ts-config match    [--config PATH] <PATH> [--json]
ts-config lint     [--config PATH] [--json]
ts-config check    [--config PATH] <PATH> --expected-slots SLOT_ID,... [--json]
```

`<PATH>` in `match` and `check` is a URL path (e.g. `/2024/01/my-article/`), not a
full URL. Full URLs are accepted but only the path component is used after stripping
the scheme and host.

`$TS_CONFIG_PATH` is honored by **all subcommands** as the default config path when
`--config` is not provided. `--config` takes precedence when specified. If neither
`--config` nor `$TS_CONFIG_PATH` is set and no `./creative-opportunities.toml` exists,
the subcommand exits `2` with a clear error.

##### `validate`

Parses `creative-opportunities.toml` (defaulting to `./creative-opportunities.toml` or
`$TS_CONFIG_PATH`) and runs all static checks:

1. **TOML parse** — the file is valid TOML.
2. **Schema check** — each slot has required fields: `id`, `page_patterns` (non-empty
   array), `formats` (non-empty array).
3. **Slot ID** — each `id` matches `^[A-Za-z0-9_-]+$` and is non-empty. Mirrors
   `validate_slot_id()` in `crates/trusted-server-core/src/creative_opportunities.rs`.
4. **Slot ID uniqueness** — no two slots share the same `id`.
5. **Glob normalization warning** — for every pattern where `Pattern::new(pattern)`
   fails and the fallback `pattern.replace("**", "*")` succeeds with a different string,
   emit a `WARN` explaining the effective pattern and that the original was invalid.
   This surfaces the silent bug at `creative_opportunities.rs:87` as a first-class
   diagnostic.
6. **Unrecoverable pattern** — if `Pattern::new()` fails even after `**→*`
   substitution, emit an **error**: "pattern `[invalid` could not be compiled — slot
   will never match any URL." The slot is effectively dead; this is an error, not a
   warning.
7. **Format dimensions** — `width > 0`, `height > 0`.
8. **Floor price** — if set, `floor_price >= 0.0 && floor_price.is_finite()`. `f64::INFINITY`
   passes `>= 0.0` but is nonsensical as a floor.
9. **Targeting value types** — all targeting values must be strings. The runtime only
   passes strings to GPT's `setTargeting()`; non-string values silently disappear in
   the browser.
10. **APS slot_id** — if `providers.aps` is set, `slot_id` is non-empty.

Exit codes:

- `0` — all checks pass (warnings are non-fatal)
- `1` — one or more errors
- `2` — TOML parse failure (file not found, invalid syntax)

Output (default, human-readable):

```
✓  3 slots parsed
⚠  slot "atf_sidebar_ad" pattern "/b**" is invalid glob — normalised to "/b*"; verify intended match scope
✓  all slot IDs valid
✓  no duplicate IDs
✓  all formats valid
✓  all targeting values are strings
✓  3 APS slots have non-empty slot_id
validate: PASS (1 warning)
```

Output (`--json`):

```json
{
  "schema_version": 1,
  "subcommand": "validate",
  "status": "pass",
  "errors": [],
  "warnings": [
    {
      "slot": "atf_sidebar_ad",
      "field": "page_patterns[0]",
      "kind": "glob_normalised",
      "original": "/b**",
      "effective": "/b*",
      "message": "pattern is invalid glob — normalised to /b*; verify intended match scope"
    }
  ],
  "slots": 3
}
```

##### `match`

Loads the TOML and runs `match_slots(slots, path)` against the provided path. Shows
which slots match and which do not, with the pattern that produced each match.

```
$ ts-config match /2024/01/my-article/

Matching against: /2024/01/my-article/

✓  atf_sidebar_ad     /20**      MATCH
   atf_sidebar_ad     /news/**   no match
✗  homepage_header_ad /          no match
✗  homepage_footer_ad /          no match

Matched: 1 slot(s) — auction would fire
```

When a pattern was normalised (e.g. `/b**` → `/b*`), the output annotates the match
with the effective pattern so the user sees exactly what is running at runtime:

```
✓  example_slot       /b**       MATCH  (effective: /b*)
```

If no slots match: "No slots matched — auction would NOT fire on this path."
Mirrors the `should_run_auction` gate in `publisher.rs`.

Unlike the runtime `matches_path()` which short-circuits on the first matching pattern,
`ts-config match` evaluates all patterns for all slots to produce a complete diagnostic
picture. The exit code (0 = any match, 1 = no match) still mirrors runtime behavior.

Exit codes:

- `0` — at least one slot matched
- `1` — no slots matched
- `2` — TOML error

`--json` output:

```json
{
  "schema_version": 1,
  "subcommand": "match",
  "path": "/2024/01/my-article/",
  "auction_would_fire": true,
  "matched_slots": [
    {
      "id": "atf_sidebar_ad",
      "matched_pattern": "/20**"
    }
  ],
  "unmatched_slots": [
    { "id": "homepage_header_ad" },
    { "id": "homepage_footer_ad" }
  ]
}
```

`effective_pattern` is present in a matched slot entry only when the pattern was
normalised (`MatchResult::NormalisedMatch`). For patterns that compiled cleanly, the
field is absent.

##### `lint`

Runs all `validate` rules plus heuristic quality checks that are valid TOML but
suspicious:

- **Pattern coverage** — a slot with only exact-match patterns (`/`, `/index.html`)
  likely needs wildcard coverage. Emit WARN.
- **Floor price absent** — slots with no `floor_price` will not filter bids by floor
  at the edge. Emit WARN (not error; some publishers manage floors via PBS only).
- **Duplicate patterns** — same pattern string appears on two different slots. Emit
  WARN (likely copy-paste error).
- **Overly broad patterns** — `/**` or `/*` matches every page. Emit WARN.
- **Cross-slot APS coverage** — if at least one slot has `providers.aps` set, warn on
  any slot that does not. This lint does not require the `trusted-server.toml` settings
  file; it operates purely on the structure of `creative-opportunities.toml`.
- **Unstable div_id heuristic** — if `div_id` contains `_R_` or `_r_` (Next.js
  server-component IDs), emit WARN: "div_id may be a Next.js generated ID that changes
  across deploys."

Exit codes: same as `validate`.

`--json` output uses the same schema as `validate --json`, with additional lint-specific
`kind` values: `floor_price_absent`, `overly_broad_pattern`, `cross_slot_aps_gap`,
`unstable_div_id`, `duplicate_pattern`, `exact_only_pattern`.

Example `lint --json` output:

```json
{
  "schema_version": 1,
  "subcommand": "lint",
  "status": "pass",
  "errors": [],
  "warnings": [
    {
      "slot": "atf_sidebar_ad",
      "field": "floor_price",
      "kind": "floor_price_absent",
      "message": "slot has no floor_price — bids will not be filtered at the edge"
    },
    {
      "slot": "homepage_header_ad",
      "field": "div_id",
      "kind": "unstable_div_id",
      "message": "div_id may be a Next.js generated ID that changes across deploys"
    }
  ],
  "slots": 3
}
```

##### `check`

Combines `validate` + `match` for CI use. Validates the file, then asserts that a
given path matches exactly the expected set of slot IDs. `--expected-slots` is
required; omitting it exits `2` with a usage error.

```bash
ts-config check /2024/01/my-article/ --expected-slots atf_sidebar_ad
```

If `validate` finds errors, `check` exits `1` immediately without evaluating the match
assertion. Validation errors mask match assertions and produce misleading diagnostics.

Exit `1` if the actual matched set differs from expected (missing or extra slots).
`--expected-slots` accepts a comma-separated list (`atf_sidebar_ad,homepage_header_ad`);
clap requires `use_value_delimiter(true)` on the argument definition.

`--json` output (validation passed, match assertion evaluated):

```json
{
  "schema_version": 1,
  "subcommand": "check",
  "status": "pass",
  "path": "/2024/01/my-article/",
  "expected": ["atf_sidebar_ad"],
  "actual": ["atf_sidebar_ad"],
  "match": true,
  "errors": [],
  "warnings": []
}
```

`--json` output (validation failed — short-circuit, no match assertion):

```json
{
  "schema_version": 1,
  "subcommand": "check",
  "status": "validation_failed",
  "errors": [
    {
      "slot": "bad_slot id",
      "field": "id",
      "kind": "invalid_slot_id",
      "message": "slot id 'bad_slot id' contains invalid characters; only [A-Za-z0-9_-] allowed"
    }
  ],
  "warnings": []
}
```

When `status` is `"validation_failed"`, the `match`, `expected`, and `actual` fields are
absent. `errors` contains validate-level diagnostics. A JSON consumer must check for
`status === "validation_failed"` before attempting to read `match`.

In the success-path output, `errors` contains **validate-level** diagnostics only
(schema errors, invalid IDs, etc.) — typically empty when the file is clean. A match
assertion failure (actual ≠ expected) is indicated by `"match": false` with differing
`expected` and `actual` arrays — it is NOT an `errors` entry. `warnings` follows the
same convention as `validate --json`.

---

### 4.2 Node.js tool: `generate-slots`

#### Why Node.js for generate

Slot generation requires a real browser to capture GPT and APS JavaScript call
arguments at runtime. This is inherently a browser-automation task — Playwright in
Node.js, the same stack already used by `js-asset-auditor` (branch:
`feature/js-asset-auditor`). Writing browser automation in Rust would require an FFI
bridge to a browser binary and is not idiomatic.

The `js-asset-auditor` package (`packages/js-asset-auditor/`) is the natural home. It
already has:

- `lib/audit.mjs` — Playwright-based page crawl, network capture, headed mode with
  `--headless` flag for CI, `--settle` delay, `--output` flag
- `lib/detect.mjs` — `detect(scripts)` detects 8 integrations including `gpt` and
  `aps`; `generateConfig()` emits `trusted-server.toml` format with `# TODO` comments
- `lib/process.mjs` — URL normalization, slug generation, wildcard detection
- `bin/audit-js-assets` — CLI entrypoint with full arg parsing

`generate-slots` adds one new module (`lib/generate-slots.mjs`) exporting two functions:
`mergePageResults(perPageResults)` (merges multi-URL captures into a unified slot map)
and `generateSlotConfig(urls, mergedSlots)` (emits `[[slot]]` TOML). Both are called
from the entrypoint `bin/ts-config-generate`. **`generateConfig()` in `detect.mjs` is not
modified** — it generates `trusted-server.toml` content and its scope remains
unchanged. `generate-slots.mjs` is a new, separate concern that generates only
`[[slot]]` TOML.

#### Data extraction: how it works

Three steps execute in sequence when the Playwright page navigates to the publisher URL:

**Layer 1: JS call interception (before page scripts run)**

Using `page.addInitScript()`, inject a shim before the publisher's scripts execute.
The shim wraps `googletag.defineSlot`, `googletag.cmd.push`, and `apstag.fetchBids` via
polling. `settle` is a Node.js variable passed into the browser context via the
function-argument form of `addInitScript` — the browser context has no access to Node.js
scope otherwise:

```javascript
// In bin/ts-config-generate — Node.js host:
await page.addInitScript((settle) => {
  // Everything below runs in the browser context.
  // `settle` is the numeric settle timeout passed from Node.js.

  window.__ts_captured_slots = []
  window.__ts_captured_aps = []
  window.__ts_aps_detected = false // true if apstag ever seen, even if fetchBids not called
  window.__ts_gpt_detected = false // true if googletag ever seen

  const patchGoogletag = () => {
    if (!window.googletag) return
    window.__ts_gpt_detected = true
    const orig = window.googletag.defineSlot
    if (!orig || orig.__ts_patched) return
    window.googletag.defineSlot = function (unitPath, sizes, divId) {
      window.__ts_captured_slots.push({ unitPath, sizes, divId })
      return orig.apply(this, arguments)
    }
    window.googletag.defineSlot.__ts_patched = true
  }

  const patchCmdPush = () => {
    if (!window.googletag?.cmd) return
    const orig = window.googletag.cmd.push
    if (!orig || orig.__ts_patched) return
    window.googletag.cmd.push = function (fn) {
      patchGoogletag() // ensure defineSlot is wrapped before fn executes
      return orig.call(this, fn)
    }
    window.googletag.cmd.push.__ts_patched = true
  }

  const patchApstag = () => {
    if (!window.apstag) return
    window.__ts_aps_detected = true
    const orig = window.apstag.fetchBids
    if (!orig || orig.__ts_patched) return
    window.apstag.fetchBids = function (config, callback) {
      if (config?.slots) {
        config.slots.forEach((s) => window.__ts_captured_aps.push(s))
      }
      return orig.apply(this, arguments)
    }
    window.apstag.fetchBids.__ts_patched = true
  }

  const interval = setInterval(() => {
    patchGoogletag()
    patchCmdPush()
    patchApstag()
  }, 50)
  setTimeout(() => clearInterval(interval), settle + 2000)
}, settle) // settle (number, ms) passed from Node.js host into browser context
```

`patchCmdPush` addresses the common publisher pattern where `googletag.defineSlot` is
called inside a `googletag.cmd.push(fn)` callback. Without it, if GPT loads and drains
its command queue before the 50ms polling interval fires, those `defineSlot` calls are
missed. `patchCmdPush` wraps `cmd.push` to call `patchGoogletag()` immediately before
executing each queued callback, closing the race window.

**Layer 2: read-back after page settles**

`ts-config-generate` navigates with `{ waitUntil: 'load' }` then waits
`waitForTimeout(settle)` (default: 6000ms, matching `audit-js-assets`), then reads
back the captured arrays via `page.evaluate()`:

```javascript
const captured = await page.evaluate(() => ({
  slots: window.__ts_captured_slots || [],
  aps: window.__ts_captured_aps || [],
  gptDetected: window.__ts_gpt_detected || false,
  apsDetected: window.__ts_aps_detected || false,
}))
```

`apsDetected && captured.aps.length === 0` → APS library loaded but `fetchBids` was
never called. Emit `# APS detected but fetchBids not observed — add providers.aps
manually if APS is active.` `gptDetected === false` → GPT never loaded; exit `1`.

The 6000ms settle window is the same as `audit-js-assets`. This gives tag managers
(GTM, Tealium) time to fire GPT and APS setup scripts after `load`. `networkidle` is
deliberately avoided because publisher pages with polling XHR will never reach it.

**Layer 3: targeting read-back (post-settle, separate evaluate call)**

After Layer 2, a second `page.evaluate()` reads slot-level targeting from GPT's live
slot objects. This is a separate call because targeting is set after `defineSlot()` and
is not captured by the Layer 1 shim:

```javascript
const targeting = await page.evaluate(() => {
  if (!window.googletag?.pubads) return {}
  const result = {}
  try {
    window.googletag
      .pubads()
      .getSlots()
      .forEach((slot) => {
        const map = slot.getTargetingMap()
        result[slot.getSlotElementId()] = map
      })
  } catch (_) {
    /* GPT not fully initialised */
  }
  return result
})
// targeting is keyed by divId; merge into slots by matching divId
```

If `pubads()` or `getSlots()` is unavailable (GPT not fully initialised, or SPA
lazy-loads), `targeting` is an empty object and all slots receive `# TODO: add
targeting key-values from ad ops`.

#### Mapping captured data to `creative-opportunities.toml` schema

`googletag.defineSlot(unitPath, sizes, divId)` maps directly:

- `unitPath` → `gam_unit_path` (verbatim)
- `sizes` → `formats` (flatten nested arrays: `[[300,250],[728,90]]` → two entries)
- `divId` → `div_id`

**Slot ID generation:** strip the GAM network prefix from `unitPath`, slugify the
remainder. E.g. `/88059007/autoblog/news` → `autoblog-news`. If `divId` contains a
recognizable semantic segment (`atf`, `btf`, `header`, `footer`), prefer that as the
ID base. The user can rename in the output file.

`apstag.fetchBids({slots: [{slotID, sizes}]})` maps as:

- `slotID` → `providers.aps.slot_id`

**APS-to-GPT correlation algorithm:** for each APS slot, find the GPT slot whose
`sizes` array is a superset of the APS slot's `sizes` array. If exactly one GPT slot
matches, assign `providers.aps.slot_id` to that slot. If zero or multiple GPT slots
match, emit `# TODO: confirm which slot this APS slot_id belongs to` on the APS entry.

`floor_price` — cannot be inferred from the browser. Always emitted as:

```toml
floor_price = 0.50  # TODO: set publisher floor price (CPM USD)
```

`page_patterns` — inferred from the crawled URL's path structure. E.g. crawled
`https://www.autoblog.com/2024/01/my-article/` → heuristically generates `/20**` for
date-prefixed paths, `/news/**` for `/news/` prefix. For the homepage, emits `["/"]`.
The comment explains the heuristic:

```toml
page_patterns = ["/20**"]  # TODO: heuristic from date-prefixed path; verify covers all target URLs
```

`[slot.targeting]` — extracted from `googletag.pubads().getSlots()` post-render if
GPT exposes the targeting object. Otherwise emitted as empty with `# TODO`.

#### Multi-page crawl merge algorithm

When multiple URLs are provided, each page is crawled independently and the results
are merged before TOML is emitted. The identity key for a slot is its `unitPath`.

Merge rules:

1. **`page_patterns`** — unioned and deduplicated across all pages. Each crawled URL
   contributes its inferred pattern. E.g. crawling `/` and `/2024/01/test/` for the
   same `unitPath` produces `page_patterns = ["/", "/20**"]`.
2. **`divId`** — if the same `unitPath` produces different `divId` values across pages
   (common with Next.js server-component hashes), use the value from the **first**
   observation (deterministic across re-crawls) and emit a `# WARN` comment listing all
   observed values. Publisher must verify stability.
3. **`formats`** — unioned across pages. If formats differ (unlikely but possible),
   all observed sizes are included.
4. **APS correlation** — runs after the full union is assembled, not per-page.
5. **Duplicate GPT calls on the same page** — same `unitPath` + `divId` on one page
   is deduplicated silently (re-registration is a publisher-side bug, not a slot
   variant).

#### Function contracts

`generateSlotConfig` has **per-URL** scope. `bin/ts-config-generate` is responsible for
crawling each URL independently and merging results before calling `generateSlotConfig`.
Separation of concerns: the function only emits TOML; the entrypoint owns multi-URL
orchestration.

```javascript
// lib/generate-slots.mjs

/**
 * Merge per-URL capture results into a single slot map before TOML emission.
 * Identity key is unitPath. Applies merge rules from §4.2.
 *
 * @param {Array<{ url: string, slots: Array<{unitPath, sizes, divId}>, aps: Array<{slotID, sizes}>, targeting: Object }>} perPageResults
 * @returns {{ urls: string[], slots: Array<{unitPath, sizes, divId, pagePatterns, targeting, apsSlotId}> }}
 */
export function mergePageResults(perPageResults) { ... }

/**
 * Emit [[slot]] TOML from a merged slot map.
 *
 * @param {string[]} urls - All crawled URLs (used for header comment)
 * @param {Array<{unitPath, sizes, divId, pagePatterns, targeting, apsSlotId}>} mergedSlots
 * @returns {string} TOML content with [[slot]] blocks
 */
export function generateSlotConfig(urls, mergedSlots) { ... }
```

Both are called by `bin/ts-config-generate`. Neither is called from `detect.mjs` or
`audit.mjs`.

#### Generated TOML example

```toml
# Generated by ts-config-generate on 2026-05-19
# URL: https://www.autoblog.com/2024/01/my-article/
# Review all # TODO lines before deploying

[[slot]]
id = "autoblog-news"
gam_unit_path = "/88059007/autoblog/news"
div_id = "ad-atf_sidebar-0-_r_2_"  # TODO: verify this div_id is stable across deploys
page_patterns = ["/20**"]  # TODO: heuristic from date-prefixed path; verify covers all target URLs
formats = [{ width = 300, height = 250 }]
floor_price = 0.50  # TODO: set publisher floor price (CPM USD)

[slot.targeting]
# TODO: add targeting key-values from ad ops (e.g. pos = "atf", zone = "atfSidebar")

[slot.providers.aps]
slot_id = "aps-slot-atf-sidebar"  # TODO: confirm APS slot_id matches TAM config

[[slot]]
id = "autoblog-homepage"
gam_unit_path = "/88059007/autoblog/homepage"
div_id = "ad-header-0-_R_jpalubtak5lb_"  # TODO: may be Next.js generated ID — verify stable
page_patterns = ["/"]
formats = [
  { width = 970, height = 90 },
  { width = 728, height = 90 },
  { width = 970, height = 250 }
]
floor_price = 0.50  # TODO: set publisher floor price (CPM USD)

[slot.targeting]
# TODO: add targeting key-values from ad ops

[slot.providers.aps]
slot_id = "aps-slot-homepage-header"
```

#### CLI interface

```bash
# Generate from live page to stdout (browser window shown by default)
ts-config-generate https://www.autoblog.com/2024/01/my-article/

# Specify output file
ts-config-generate https://www.autoblog.com/ --output creative-opportunities.toml

# Multi-page crawl — union of all slots found
ts-config-generate https://www.autoblog.com/ https://www.autoblog.com/2024/01/test/

# Headless mode for CI (default: headed to match audit-js-assets)
ts-config-generate https://www.autoblog.com/ --headless

# Wait for JS to settle (default: 6000ms, same as audit-js-assets)
ts-config-generate https://www.autoblog.com/ --settle 3000

# Validate output immediately after generation
ts-config-generate https://www.autoblog.com/ --validate
```

Default mode is headed (browser window visible) to match `audit-js-assets` convention.
`--headless` is the CI flag.

When `--output` is specified, the file is written atomically: TOML is buffered to a
temp file in the same directory, then renamed over the destination. A crash or interrupt
during generation cannot corrupt an existing `creative-opportunities.toml`.

`--validate` spawns `ts-config validate` as a subprocess, locating the binary via
`$PATH` or `$TS_CONFIG_BIN` env var. If the binary is not found and `--validate` was
explicitly passed, the tool exits `1` with a clear error:
`"ts-config binary not found — set $TS_CONFIG_BIN or add to $PATH"`. Silently
continuing to `exit 0` when the caller explicitly requested validation would be a
silent CI failure. If `ts-config validate` exits non-zero, that exit code is
propagated from `ts-config-generate`.

Exit codes:

- `0` — generation succeeded (with or without `# TODO` items — those are expected)
- `1` — page navigation or JS interception failed (browser error, timeout, no GPT found)
- `2` — invalid URL or argument error

---

### 4.3 Workspace isolation

`crates/trusted-server-cli/` is excluded from the root workspace in `Cargo.toml`
(mirrors `crates/integration-tests/`). The workspace default target is `wasm32-wasip1`;
the CLI requires a native host target. Isolation prevents cross-contamination.

```toml
# Cargo.toml (workspace root) — add to existing exclude list
exclude = [
    "crates/integration-tests",
    "crates/openrtb-codegen",
    "crates/trusted-server-cli",   # native target; excluded from wasm32-wasip1 workspace
]
```

The CLI's own `Cargo.toml` uses explicit versions (no `workspace = true`) and follows
project conventions for error handling (`error-stack`, `derive_more`):

```toml
[package]
name = "ts-config"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
toml = "1.0"
glob = "0.3"
serde_json = "1.0"
error-stack = "0.6"
derive_more = { version = "2.0", features = ["display", "error"] }
```

Build for local install (production artifact):

```bash
cargo build --manifest-path crates/trusted-server-cli/Cargo.toml --release
# Binary at: target/release/ts-config
```

CI and development use the debug build (omit `--release`). Binary lands at
`target/debug/ts-config`. This is what all CI steps and manual verification steps use.

---

### 4.4 Validation logic: mirroring the runtime

Validation rules in `ts-config validate` are derived from
`crates/trusted-server-core/src/creative_opportunities.rs` and
`crates/trusted-server-core/build.rs`. Rule drift between the CLI and the runtime is
a bug.

**Intentional asymmetry:** `build.rs` validates slot IDs only (rule 3 above). The CLI
validates all 10 rules. This is intentional — the CLI is a richer pre-flight, not a
clone of `build.rs`. The round-trip CI test confirms that the checked-in
`creative-opportunities.toml` passes both tools, but does not prove that the two tools
agree on every possible invalid input. The CLI is the authoritative validator for
authors; `build.rs` is a last-resort compile-time guard.

**Source reference:** `validate_slot_id()` in the CLI is a direct copy of the function
from `creative_opportunities.rs`, using the same char-by-char iteration
(`id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')`). Note:
`build.rs` uses a regex (`^[A-Za-z0-9_\-]+$`) which is semantically equivalent, but
the CLI mirrors `creative_opportunities.rs` — not `build.rs` — to stay aligned with
the runtime matching authority. A comment in both files cross-references the other.

The glob normalization behavior is reproduced exactly:

```rust
/// Result of matching a single pattern against a path.
enum MatchResult {
    /// Pattern compiled cleanly; bool = whether it matched.
    Match(bool),
    /// Pattern failed to compile; normalized form succeeded.
    /// `effective` = the string actually used for matching.
    NormalisedMatch { effective: String, matched: bool },
    /// Pattern could not be compiled even after normalisation.
    /// Slot effectively dead — emit error.
    UncompilablePattern,
}

fn matches_path_with_normalisation(pattern: &str, path: &str) -> MatchResult {
    match glob::Pattern::new(pattern) {
        Ok(p) => MatchResult::Match(p.matches(path)),
        Err(_) => {
            let normalised = pattern.replace("**", "*");
            if normalised == pattern {
                MatchResult::UncompilablePattern
            } else {
                match glob::Pattern::new(&normalised) {
                    Ok(p) => MatchResult::NormalisedMatch { effective: normalised, matched: p.matches(path) },
                    Err(_) => MatchResult::UncompilablePattern,
                }
            }
        }
    }
}
```

`MatchResult::NormalisedMatch` triggers the WARN in `validate`; `UncompilablePattern`
triggers the error.

---

### 4.5 Claude Code plugin (Phase 2)

The Claude Code plugin is not in scope for Phase 1 but is the intended endpoint of the
generation workflow. In Phase 2, a conversational wrapper is added that:

1. Asks the user for the publisher URL.
2. Calls `ts-config-generate <url> --headless` and captures TOML from stdout (or
   `--output <tmp-file>`). `ts-config-generate` has no `--json` mode — the plugin reads
   the TOML output directly.
3. Calls `ts-config validate --json` and `ts-config lint --json` against the generated
   file to get machine-readable diagnostics. These are the tools with `--json` output.
4. Walks the user through `# TODO` items and lint warnings conversationally.
5. Writes the reviewed TOML to `creative-opportunities.toml`.
6. Runs `ts-config match --json` against representative paths the user provides.

The plugin uses `--json` on the Rust `ts-config` subcommands for machine-readable
output. `ts-config-generate` is invoked for its TOML output only; generation
success/failure is determined by exit code.

---

### 4.6 Error type scaffold

Following project conventions (`error-stack` + `derive_more::Display`), the CLI
defines top-level error types per module. These are the starting definitions;
implementers may split further as needed.

```rust
// src/validate.rs
#[derive(Debug, derive_more::Display)]
pub enum ValidateError {
    #[display("slot has empty id — id must be non-empty")]
    EmptySlotId,
    #[display("slot `{id}` has invalid characters; only [A-Za-z0-9_-] allowed")]
    InvalidSlotId { id: String },
    #[display("duplicate slot id `{id}`")]
    DuplicateSlotId { id: String },
    #[display("slot `{id}` pattern `{pattern}` could not be compiled — slot will never match")]
    UncompilablePattern { id: String, pattern: String },
    #[display("slot `{id}` format has zero dimension ({width}x{height})")]
    ZeroDimension { id: String, width: u32, height: u32 },
    #[display("slot `{id}` floor_price {value} is invalid (must be finite and >= 0)")]
    InvalidFloorPrice { id: String, value: f64 },
    #[display("slot `{id}` targeting key `{key}` has non-string value")]
    NonStringTargeting { id: String, key: String },
    #[display("slot `{id}` providers.aps.slot_id is empty")]
    EmptyApsSlotId { id: String },
}

impl core::error::Error for ValidateError {}

// src/main.rs (top-level dispatch error)
#[derive(Debug, derive_more::Display)]
pub enum ConfigError {
    #[display("config file not found: {path}")]
    FileNotFound { path: String },
    #[display("TOML parse error in {path}: {source}")]
    ParseFailed { path: String, source: String },
}

impl core::error::Error for ConfigError {}
```

---

## 5. Implementation Scope

### Phase 1A — Rust binary (ships independently, no branch dependency)

#### New files

| File                                         | Description                                     |
| -------------------------------------------- | ----------------------------------------------- |
| `crates/trusted-server-cli/Cargo.toml`       | Native binary crate, excluded from workspace    |
| `crates/trusted-server-cli/src/main.rs`      | `ts-config` binary; subcommand dispatch         |
| `crates/trusted-server-cli/src/validate.rs`  | Rules 1–10; mirrors `creative_opportunities.rs` |
| `crates/trusted-server-cli/src/match_cmd.rs` | Glob matching; mirrors `match_slots()`          |
| `crates/trusted-server-cli/src/report.rs`    | Human-readable + JSON output formatting         |
| `scripts/validate-creative-opportunities.sh` | CI round-trip script                            |

#### Modified files

| File                          | Change                                                |
| ----------------------------- | ----------------------------------------------------- |
| `Cargo.toml` (workspace root) | Add `crates/trusted-server-cli` to `exclude` list     |
| `.github/workflows/ci.yml`    | Add: build `ts-config`, then run `ts-config validate` |

The CI job must build the binary before using it:

```yaml
- name: Build ts-config
  run: cargo build --manifest-path crates/trusted-server-cli/Cargo.toml

- name: Validate creative-opportunities.toml
  run: ./target/debug/ts-config validate --config creative-opportunities.toml
```

`--release` is omitted in CI — debug build is sufficient for config validation and
avoids the compilation overhead. Use `--release` only for the production install
artifact.

### Phase 1B — Node.js generator (ships after `feature/js-asset-auditor` merges)

#### New files

| File                                                          | Description                                                                                   |
| ------------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| `packages/js-asset-auditor/lib/generate-slots.mjs`            | `mergePageResults()` + `generateSlotConfig()` functions; GPT/APS interception + TOML emission |
| `packages/js-asset-auditor/bin/ts-config-generate`            | CLI entrypoint; Playwright session + arg parsing                                              |
| `packages/js-asset-auditor/test/fixtures/fake-publisher.html` | Synthetic publisher page for integration tests                                                |
| `docs/ts-config.md`                                           | User-facing documentation for both tools                                                      |

#### Modified files

| File                                     | Change                                                            |
| ---------------------------------------- | ----------------------------------------------------------------- |
| `packages/js-asset-auditor/package.json` | Add `"bin": { "ts-config-generate": "./bin/ts-config-generate" }` |

#### Unchanged

| File                                                       | Reason                                                                    |
| ---------------------------------------------------------- | ------------------------------------------------------------------------- |
| `packages/js-asset-auditor/lib/detect.mjs`                 | `generateConfig()` scope unchanged — generates `trusted-server.toml` only |
| `crates/trusted-server-core/src/creative_opportunities.rs` | Reference only; no modifications                                          |
| `crates/trusted-server-core/build.rs`                      | Remains as compile-time gate; CLI is pre-flight, not replacement          |

### Phase 2 — Claude Code plugin (tracked separately)

- `packages/ts-config-plugin/` — Claude Code plugin manifest and handler
- Conversational `# TODO` walkthrough
- Interactive path testing (shows matching slots in real time)

---

## 6. Output Formats and Exit Codes

### Exit code contract (all subcommands)

| Code | Meaning                                                             |
| ---- | ------------------------------------------------------------------- |
| `0`  | Success (warnings OK, no errors)                                    |
| `1`  | Validation/logic failure                                            |
| `2`  | Input failure (file not found, invalid TOML, invalid path argument) |

### JSON output contract

All subcommands accept `--json` and emit a single JSON object on stdout. `schema_version: 1`
allows downstream consumers to detect breaking changes. The `subcommand` field
identifies which output shape to expect.

All diagnostic entries carry: `slot` (ID string or `null` for file-level issues),
`field` (TOML path string, e.g. `"page_patterns[0]"`), `kind` (machine-readable tag),
`message` (human-readable string).

`kind` values by subcommand:

- `validate`: `glob_normalised`, `uncompilable_pattern`, `invalid_slot_id`, `duplicate_slot_id`, `zero_dimension`, `invalid_floor_price`, `non_string_targeting`, `empty_aps_slot_id`
- `lint`: all of the above plus `floor_price_absent`, `overly_broad_pattern`, `cross_slot_aps_gap`, `unstable_div_id`, `duplicate_pattern`, `exact_only_pattern`

Full JSON shapes for each subcommand are specified in §4.1 under each subcommand's
`--json` output block.

---

## 7. Security Considerations

- **No secrets in output** — generated TOML contains only data visible to any browser
  user navigating to the publisher URL. GPT `unitPath`, `sizes`, `divId`, and APS
  `slotID` are public JavaScript call arguments.
- **Headed mode is default for `ts-config-generate`** — matching `audit-js-assets`
  convention. `--headless` is the CI opt-in. The headed default makes interception
  failures visible to the auditor during onboarding.
- **Slot ID validation protects the server** — `[A-Za-z0-9_-]+` prevents slot IDs
  containing characters that would break out of the injected `<script>` block at
  runtime. The CLI enforces this before the file ever reaches `build.rs` or the WASM
  binary.
- **No `eval()` or dynamic code execution in generated TOML** — the output is inert
  data. Server-side injection of `window.__ts_bids` uses `serde_json` + HTML escaping;
  the CLI has no part in that path.
- **URL handling in Playwright** — the URL is parsed by the Node.js `URL` constructor
  before being passed to `page.navigate()`. No shell interpolation. Invalid URLs exit
  `2` before Playwright launches.

---

## 8. Edge Cases

**Pattern that was never compilable** — a pattern that fails `Pattern::new()` even
after `**→*` substitution (e.g. `[invalid`) is silently skipped at runtime. The CLI
emits an error: "pattern `[invalid` could not be compiled — slot will never match any
URL." Error, not warning, because the slot is effectively dead.

**Empty `creative-opportunities.toml`** — zero slots is valid at runtime (feature
disabled). `ts-config validate` passes with note: "0 slots defined — auction will not
fire on any URL." `ts-config match` exits `1`.

**GPT not detected on target page** — `ts-config-generate` emits a warning and exits
`1`. The page may use a different tag management approach; try running in headed mode
(omit `--headless`) to observe what scripts load, or use a different URL.

**APS loaded but `fetchBids` never called** — emit TOML comment: `# APS detected but
fetchBids not observed — add providers.aps manually if APS is active.`

**Multiple pages, conflicting div IDs for same GAM unit path** — when crawling multiple
URLs, the same `unitPath` may appear with different `divId` values (Next.js server
component IDs include a component-tree hash). Use the value from the **first**
observation (deterministic across re-crawls) and emit `# WARN: multiple div_ids
observed for this slot: [list]`. Publisher must verify which is stable.

**`googletag.cmd.push` deferred pattern** — most publishers do not call
`googletag.defineSlot()` directly at script parse time. Instead they use:

```javascript
googletag.cmd.push(function() { googletag.defineSlot(...) })
```

GPT drains its `cmd` queue asynchronously when the library loads, which can happen
before the 50ms polling interval fires. The Layer 1 shim patches `googletag.cmd.push`
to call `patchGoogletag()` before each queued callback executes, closing this race.
Publishers that call `defineSlot` directly (not inside `cmd.push`) are also covered.

**Pattern glob ambiguity: `/20**`vs`/20*`** — both patterns produce identical runtime
behavior in this codebase. `/20**` is **valid** glob syntax; `Pattern::new("/20**")`succeeds and no normalisation occurs. The equivalence arises because the glob crate
uses`require_literal_separator = false`by default, meaning a single`*`already
matches across`/`boundaries. Authors who write`/20**`expecting explicit
multi-segment semantics get the correct outcome, but so does`/20\*`. Note the contrast
with `/b**`, which _is_ an invalid pattern (the `**`immediately follows a non-separator
character), fails`Pattern::new()`, and triggers the normalisation branch. The lint
command notes when both `/20**`and`/20\*` appear in the same file so authors don't
maintain what they believe are two patterns with different semantics.

---

## 9. Testing Strategy

### Rust CLI tests (Phase 1A)

#### Unit tests (`#[cfg(test)]` in each module)

```rust
// src/validate.rs — validate_slot_id
#[test]
fn slot_id_accepts_valid() {
    assert!(validate_slot_id("atf_sidebar_ad").is_ok());
    assert!(validate_slot_id("slot-1").is_ok());
    assert!(validate_slot_id("A").is_ok());
}

#[test]
fn slot_id_rejects_empty() {
    assert!(matches!(validate_slot_id("").unwrap_err(), ValidateError::EmptySlotId));
}

#[test]
fn slot_id_rejects_space_and_bang() {
    assert!(matches!(
        validate_slot_id("bad slot id!").unwrap_err(),
        ValidateError::InvalidSlotId { .. }
    ));
}

#[test]
fn slot_id_rejects_html_injection() {
    assert!(matches!(
        validate_slot_id("<script>").unwrap_err(),
        ValidateError::InvalidSlotId { .. }
    ));
}

// src/match_cmd.rs — matches_path_with_normalisation
#[test]
fn valid_glob_returns_match_variant() {
    assert!(matches!(
        matches_path_with_normalisation("/20**", "/2024/01/article"),
        MatchResult::Match(true)
    ));
}

#[test]
fn invalid_glob_normalises_and_returns_normalised_match() {
    let result = matches_path_with_normalisation("/b**", "/blog/article");
    assert!(matches!(result, MatchResult::NormalisedMatch { matched: true, .. }));
    if let MatchResult::NormalisedMatch { effective, .. } = result {
        assert_eq!(effective, "/b*");
    }
}

#[test]
fn uncompilable_pattern_returns_error_variant() {
    assert!(matches!(
        matches_path_with_normalisation("[invalid", "/any"),
        MatchResult::UncompilablePattern
    ));
}

#[test]
fn valid_glob_no_match_returns_match_false() {
    assert!(matches!(
        matches_path_with_normalisation("/news/**", "/sports/article"),
        MatchResult::Match(false)
    ));
}
```

#### Binary integration tests (`tests/integration_tests.rs`)

Uses `env!("CARGO_BIN_EXE_ts-config")` — requires `[[bin]]` name `"ts-config"` in
`Cargo.toml`. Test fixtures live in `crates/trusted-server-cli/tests/fixtures/`.

```rust
fn bin() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_ts-config"))
}

#[test]
fn validate_valid_config_exits_zero() {
    let out = bin().args(["validate", "--config", "tests/fixtures/valid.toml"])
        .output().expect("should run ts-config");
    assert_eq!(out.status.code(), Some(0), "stdout: {}", String::from_utf8_lossy(&out.stdout));
}

#[test]
fn validate_invalid_slot_id_exits_one() {
    let out = bin().args(["validate", "--config", "tests/fixtures/invalid-slot-id.toml"])
        .output().expect("should run ts-config");
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn validate_json_has_schema_version_and_subcommand() {
    let out = bin().args(["validate", "--json", "--config", "tests/fixtures/valid.toml"])
        .output().expect("should run ts-config");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("should parse JSON output");
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["subcommand"], "validate");
    assert_eq!(json["status"], "pass");
}

#[test]
fn validate_normalised_pattern_exits_zero_with_warning() {
    let out = bin().args(["validate", "--json", "--config", "tests/fixtures/normalised-pattern.toml"])
        .output().expect("should run ts-config");
    assert_eq!(out.status.code(), Some(0));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let warnings = json["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(|w| w["kind"] == "glob_normalised"));
}

#[test]
fn match_returns_zero_when_slot_matches() {
    let out = bin().args(["match", "--config", "tests/fixtures/valid.toml", "/2024/01/article/"])
        .output().expect("should run ts-config");
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn match_returns_one_when_no_slot_matches() {
    let out = bin().args(["match", "--config", "tests/fixtures/valid.toml", "/no-match-path/"])
        .output().expect("should run ts-config");
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn check_exits_one_when_expected_set_differs() {
    let out = bin()
        .args(["check", "--config", "tests/fixtures/valid.toml",
               "/2024/01/article/", "--expected-slots", "wrong_slot"])
        .output().expect("should run ts-config");
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn lint_warns_on_unstable_div_id() {
    let out = bin().args(["lint", "--json", "--config", "tests/fixtures/nextjs-div-id.toml"])
        .output().expect("should run ts-config");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let warnings = json["warnings"].as_array().unwrap();
    assert!(warnings.iter().any(|w| w["kind"] == "unstable_div_id"));
}
```

**Test fixtures** (`crates/trusted-server-cli/tests/fixtures/`):

| File                      | Content                                                                           |
| ------------------------- | --------------------------------------------------------------------------------- |
| `valid.toml`              | 1 slot, no lint issues — used as baseline for match/check/validate happy-path     |
| `invalid-slot-id.toml`    | Slot with `id = "bad slot id!"` — validate exits 1                                |
| `normalised-pattern.toml` | Slot with `page_patterns = ["/b**"]` — validate exits 0 + glob_normalised warning |
| `nextjs-div-id.toml`      | Slot with `div_id = "ad-atf_R_abc123_"` — lint warns unstable_div_id              |

CI command:

```bash
cargo test --manifest-path crates/trusted-server-cli/Cargo.toml
```

### Node.js generator tests (Phase 1B)

```javascript
// packages/js-asset-auditor/test/generate-slots.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mergePageResults, generateSlotConfig } from '../lib/generate-slots.mjs'

test('mergePageResults unions page_patterns across URLs for same unitPath', () => {
  const results = [
    {
      url: 'https://example.com/',
      slots: [{ unitPath: '/123/slot', sizes: [[300, 250]], divId: 'ad-1' }],
      aps: [],
      targeting: {},
    },
    {
      url: 'https://example.com/2024/article/',
      slots: [{ unitPath: '/123/slot', sizes: [[300, 250]], divId: 'ad-1' }],
      aps: [],
      targeting: {},
    },
  ]
  const { slots } = mergePageResults(results)
  assert.equal(slots.length, 1)
  assert.deepEqual(new Set(slots[0].pagePatterns), new Set(['/', '/20**']))
})

test('mergePageResults uses first divId on conflict', () => {
  const results = [
    {
      url: 'https://example.com/',
      slots: [
        { unitPath: '/123/slot', sizes: [[300, 250]], divId: 'ad-stable' },
      ],
      aps: [],
      targeting: {},
    },
    {
      url: 'https://example.com/p2/',
      slots: [
        { unitPath: '/123/slot', sizes: [[300, 250]], divId: 'ad-_R_abc' },
      ],
      aps: [],
      targeting: {},
    },
  ]
  const { slots } = mergePageResults(results)
  assert.equal(slots[0].divId, 'ad-stable')
  assert.ok(slots[0].divIdConflict, 'should flag divId conflict')
})

test('generateSlotConfig emits valid [[slot]] TOML block', () => {
  const mergedSlots = [
    {
      unitPath: '/88059007/autoblog/news',
      sizes: [[300, 250]],
      divId: 'ad-1',
      pagePatterns: ['/20**'],
      targeting: {},
      apsSlotId: 'aps-1',
    },
  ]
  const toml = generateSlotConfig(
    ['https://autoblog.com/2024/article/'],
    mergedSlots
  )
  assert.ok(toml.includes('[[slot]]'))
  assert.ok(toml.includes('id = "autoblog-news"'))
  assert.ok(toml.includes('gam_unit_path = "/88059007/autoblog/news"'))
  assert.ok(toml.includes('slot_id = "aps-1"'))
})

test('APS correlation assigns slot_id when sizes match exactly one GPT slot', () => {
  const results = [
    {
      url: 'https://example.com/',
      slots: [
        { unitPath: '/123/atf', sizes: [[300, 250]], divId: 'atf' },
        { unitPath: '/123/btf', sizes: [[728, 90]], divId: 'btf' },
      ],
      aps: [{ slotID: 'aps-atf', sizes: [[300, 250]] }],
      targeting: {},
    },
  ]
  const { slots } = mergePageResults(results)
  const atf = slots.find((s) => s.unitPath === '/123/atf')
  assert.equal(atf.apsSlotId, 'aps-atf')
  const btf = slots.find((s) => s.unitPath === '/123/btf')
  assert.equal(btf.apsSlotId, undefined)
})

test('APS correlation emits TODO when sizes are ambiguous (two GPT slots match)', () => {
  const results = [
    {
      url: 'https://example.com/',
      slots: [
        { unitPath: '/123/atf', sizes: [[300, 250]], divId: 'atf' },
        { unitPath: '/123/btf', sizes: [[300, 250]], divId: 'btf' },
      ],
      aps: [{ slotID: 'aps-ambiguous', sizes: [[300, 250]] }],
      targeting: {},
    },
  ]
  const { slots } = mergePageResults(results)
  const toml = generateSlotConfig(['https://example.com/'], slots)
  assert.ok(
    toml.includes('# TODO: confirm which slot'),
    'should flag ambiguous APS correlation'
  )
})
```

**Integration test fixture** (`packages/js-asset-auditor/test/fixtures/fake-publisher.html`):

The fixture uses `googletag.cmd.push` (not direct calls) to exercise the Layer 1
`patchCmdPush` interception:

```html
<!DOCTYPE html>
<html>
  <head>
    <script
      async
      src="https://securepubads.g.doubleclick.net/tag/js/gpt.js"
    ></script>
    <script>
      window.googletag = window.googletag || { cmd: [] }
      googletag.cmd.push(function () {
        googletag
          .defineSlot('/88059007/test/atf', [300, 250], 'div-atf')
          .setTargeting('pos', 'atf')
          .addService(googletag.pubads())
        googletag
          .defineSlot(
            '/88059007/test/btf',
            [
              [728, 90],
              [970, 90],
            ],
            'div-btf'
          )
          .addService(googletag.pubads())
        googletag.pubads().enableSingleRequest()
        googletag.enableServices()
      })
    </script>
  </head>
  <body>
    <div id="div-atf"></div>
    <div id="div-btf"></div>
  </body>
</html>
```

Integration test asserts the Playwright crawl of `fake-publisher.html` produces a
TOML with `id = "test-atf"` and `id = "test-btf"` slots, both with correct `formats`.

### Round-trip CI test (Phase 1A)

```bash
# scripts/validate-creative-opportunities.sh
set -e
./target/debug/ts-config validate --config creative-opportunities.toml
cargo build --manifest-path crates/trusted-server-core/Cargo.toml
```

This script confirms that the checked-in `creative-opportunities.toml` passes the CLI
validator and compiles cleanly through `build.rs`. It does not prove full rule parity
(the CLI validates 10 rules; `build.rs` validates slot IDs only). The two tools are
complementary, not mirrors.

---

## 10. Open Questions

1. **`div_id` stability for Next.js publishers** — React server component IDs like
   `_R_jpalubtak5lb_` change when the component tree changes. Proposal: flag `_R_` or
   `_r_` prefix patterns in `lint` as "potentially unstable." Resolved above as an
   implemented lint rule; no further decision needed.

2. **Multi-URL crawl: page pattern inference** — date-prefixed `/20**` inference is
   autoblog-specific. Proposal: emit the heuristic with explicit reasoning in the
   `# TODO` comment so the auditor knows why the pattern was chosen.

3. **PBS stored request pre-flight** — opt-in `--pbs-url` flag in `ts-config check`
   to probe PBS for stored requests. Proposal: Phase 2 feature. Phase 1 emits
   `# TODO: verify PBS stored request exists for slot id X` in generated TOML.

4. **Package location** — `ts-config-generate` inside `js-asset-auditor` vs sibling
   package. Proposal: inside `js-asset-auditor` for Phase 1; extract to sibling only
   if dependency trees diverge.

5. **Native APS interception coverage** — Phase 1 covers display banner `{slotID, sizes}`
   only. Phase 2 extends to video/native APS call signatures.

---

## 11. Verification

### Automated (CI)

1. Build: `cargo build --manifest-path crates/trusted-server-cli/Cargo.toml`
2. Unit tests: `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml`
3. `./target/debug/ts-config validate` against `creative-opportunities.toml` → exits 0
4. Round-trip: `bash scripts/validate-creative-opportunities.sh` → passes
5. Run `ts-config validate --config crates/trusted-server-cli/tests/fixtures/invalid-slot-id.toml` → exits 1 (fixture contains a slot with `id = "bad slot id!"`; never modify the checked-in `creative-opportunities.toml` for negative tests)

### Manual (local only — requires network + browser)

6. `./target/debug/ts-config match /2024/01/my-article/` → matches `atf_sidebar_ad` only
7. `./target/debug/ts-config match /` → matches `homepage_header_ad` and `homepage_footer_ad`
8. `./target/debug/ts-config lint` → surfaces `_R_` div_id warnings for current TOML
9. `ts-config-generate <publisher-url> --validate` → generates TOML, validates it, exits 0
10. Introduce `page_patterns = ["/b**"]` → `validate` exits 0 with WARN; `match /blog/foo`
    shows effective pattern `/b*` and its match result
