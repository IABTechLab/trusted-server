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
buildable with `cargo build --manifest-path crates/trusted-server-cli/Cargo.toml --target "$HOST" --locked`.

#### Subcommands

```
ts-config validate [--config PATH] [--json]
ts-config match    [--config PATH] <PATH> [--json]
ts-config lint     [--config PATH] [--json]
ts-config check    [--config PATH] <PATH> (--expected-slots SLOT_ID,... | --expect-no-slots) [--json]
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
   array), `formats` (non-empty array). `gam_unit_path` and `div_id` are **optional**
   in both the runtime schema (`Option<String>`) and the CLI validator — absent means
   the runtime derives them from the slot `id` and `gam_network_id`. If either is
   present, however, it must be non-empty: `gam_unit_path = ""` or `div_id = ""` would
   produce a broken GPT slot registration and are rejected with kinds
   `empty_gam_unit_path` and `empty_div_id` respectively. The generator always emits
   both since it has the live values.
3. **Slot ID** — each `id` matches `^[A-Za-z0-9_-]+$` and is non-empty. Mirrors
   `validate_slot_id()` in `crates/trusted-server-core/src/creative_opportunities.rs`.
4. **Slot ID uniqueness** — no two slots share the same `id`.
5. **Pattern leading slash** — each entry in `page_patterns` must begin with `/`. The
   runtime `matches_path()` always receives a path starting with `/`; a pattern without
   a leading slash can never match. Emit an error with kind `missing_leading_slash`.
6. **Glob normalization warning** — for every pattern where `Pattern::new(pattern)`
   fails and the fallback `pattern.replace("**", "*")` succeeds with a different string,
   emit a `WARN` explaining the effective pattern and that the original was invalid.
   This surfaces the silent bug at `creative_opportunities.rs:87` as a first-class
   diagnostic.
7. **Unrecoverable pattern** — if `Pattern::new()` fails even after `**→*`
   substitution, emit an **error**: "pattern `[invalid` could not be compiled — this
   pattern will never match any URL." Only escalate to "slot will never match" if
   every pattern in `page_patterns` is uncompilable; a slot with one bad pattern and
   other valid patterns still fires on the valid ones. This is an error, not a warning.
8. **Format dimensions** — `width > 0`, `height > 0`. Formats may optionally include
   `media_type` (e.g. `"banner"`, `"video"`); an invalid type (e.g. `media_type = 123`)
   surfaces as `invalid_type` via `serde_path_to_error`. Covered by the dedicated
   `invalid-media-type.toml` fixture (see §9 test fixture table).
9. **Floor price** — if set, `floor_price >= 0.0 && floor_price.is_finite()`. `f64::INFINITY`
   passes `>= 0.0` but is nonsensical as a floor.
10. **Targeting value types** — all targeting values must be strings. `[slot.targeting]`
    is deserialized as `HashMap<String, String>`; serde rejects non-string values at
    parse time with a typed error pointing at the offending field. The resulting
    diagnostic is normalized to kind `non_string_targeting` (not `invalid_type`).
    `invalid_type` is reserved for other schema type mismatches (e.g.
    `floor_price = "cheap"`). The rule exists so the CLI produces an actionable
    path-qualified diagnostic before the file reaches `build.rs` or the WASM binary.
11. **APS slot_id** — if `providers.aps` is set, `slot_id` is non-empty.

**Parse strategy:** validation uses two passes. First parse the file as `toml::Value`
to detect unknown fields — typos like `page_pattern` silently disappear under typed
deserialization. Then use `serde_path_to_error` for the typed conversion so error
messages carry the TOML path (`slots[1].id`, `slots[0].page_patterns[2]`) rather than
a generic serde failure. Direct deserialization into typed structs alone is not
sufficient for actionable diagnostics.

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
      "field": "slots[0].page_patterns[0]",
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

**`match` does not validate.** More precisely: `match` performs a full typed
deserialization (required to run `match_slots()`), so a file with a type-schema error
such as `floor_price = "cheap"` will still fail to load and exit `2`. What `match`
skips is _semantic_ validation — invalid slot IDs, missing leading slashes, zero
dimensions, and all lint rules are ignored. A config with `id = "bad slot id!"` or
`page_patterns = ["news/**"]` is still matchable because those fields deserialize
successfully into the runtime struct; the semantic check is simply not run. Use `check`
when you need both validation and match assertion in CI. Use `match` when you want to
debug which patterns fire on a given URL regardless of file quality.

```
$ ts-config match /2024/01/my-article/

Matching against: /2024/01/my-article/

✓  atf_sidebar_ad     /20**      MATCH  (effective: /20*)
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
      "matched_pattern": "/20**",
      "effective_pattern": "/20*"
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

- **Pattern coverage** — a slot with only exact-match patterns (e.g. `/index.html`,
  `/about`) likely needs wildcard coverage. Emit WARN. Root `/` is exempt — a
  homepage-only slot is a valid and common configuration.
- **Floor price absent** — slots with no `floor_price` will not filter bids by floor
  at the edge. Emit WARN (not error; some publishers manage floors via PBS only).
- **Duplicate patterns** — same pattern string appears more than once within a single
  slot's `page_patterns`. Emit WARN (likely copy-paste error inside the slot).
  Cross-slot sharing of the same pattern is not flagged — multiple slots covering the
  same URL path (e.g. header and footer ads both with `["/"]`) is intentional.
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
`unstable_div_id`, `duplicate_pattern`, `exact_only_pattern`, `equivalent_pattern`.

`equivalent_pattern` fires when two patterns within the **same slot's `page_patterns`**
are equivalent after canonicalization. Canonicalization rule: replace every `**` token
with `*` (since the glob crate uses `require_literal_separator = false`, making `**`
behave identically to `*`). Two patterns are equivalent if and only if their
canonicalized strings are identical (e.g. `/20**` canonicalizes to `/20*`; same as a
literal `/20*`). The spec does not claim broader semantic equivalence for arbitrary
glob pairs — only this specific `**`→`*` substitution. Cross-slot equivalence is not
flagged. The warning carries both `original` and `equivalent` fields so the author can
choose which to keep.

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
      "field": "slots[0].floor_price",
      "kind": "floor_price_absent",
      "message": "slot has no floor_price — bids will not be filtered at the edge"
    },
    {
      "slot": "homepage_header_ad",
      "field": "slots[2].div_id",
      "kind": "unstable_div_id",
      "message": "div_id may be a Next.js generated ID that changes across deploys"
    }
  ],
  "slots": 3
}
```

##### `check`

Combines `validate` + `match` for CI use. Validates the file, then asserts that a
given path matches exactly the expected set of slot IDs. One of `--expected-slots` or
`--expect-no-slots` is required; omitting both exits `2` with a usage error.

```bash
ts-config check /2024/01/my-article/ --expected-slots atf_sidebar_ad
```

If `validate` finds errors, `check` exits `1` immediately without evaluating the match
assertion. Validation errors mask match assertions and produce misleading diagnostics.

Exit `1` if the actual matched set differs from expected (missing or extra slots).
`--expected-slots` accepts a comma-separated list (`atf_sidebar_ad,homepage_header_ad`);
clap requires `use_value_delimiter(true)` on the argument definition.

**Asserting no slots match:** to assert a path fires no auction, use the dedicated
`--expect-no-slots` flag rather than `--expected-slots ""`. An empty string passed to
`--expected-slots` is ambiguous (clap may parse it as a single empty-string slot ID).
`--expect-no-slots` and `--expected-slots` are mutually exclusive; passing both exits
`2` with a usage error. JSON output with `--expect-no-slots`:

```json
{
  "schema_version": 1,
  "subcommand": "check",
  "status": "pass",
  "path": "/no-match/",
  "expected": [],
  "actual": [],
  "match": true,
  "errors": [],
  "warnings": []
}
```

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
      "slot": null,
      "field": "slots[0].id",
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
    if (!window.__ts_googletag_target) return
    window.__ts_gpt_detected = true
    const orig = window.__ts_googletag_target.defineSlot
    if (orig?.__ts_patched) return
    if (!orig) {
      // defineSlot not yet on the googletag object — install a setter so we intercept
      // the moment GPT attaches it. Without this, an inline script that calls
      // defineSlot() directly between GPT attaching the function and the next 50ms
      // poll would be missed. Mirrors the APS fetchBids setter pattern.
      if (
        !Object.getOwnPropertyDescriptor(
          window.__ts_googletag_target,
          'defineSlot'
        )?.set
      ) {
        let _defineSlot
        Object.defineProperty(window.__ts_googletag_target, 'defineSlot', {
          configurable: true,
          get: () => _defineSlot,
          set: (fn) => {
            _defineSlot = fn
            patchGoogletag()
          },
        })
      }
      return
    }
    const wrapped = function (unitPath, sizes, divId) {
      window.__ts_captured_slots.push({ unitPath, sizes, divId })
      return orig.apply(this, arguments)
    }
    wrapped.__ts_patched = true
    // Use a value descriptor to bypass the defineSlot accessor setter we just installed,
    // preventing recursion into patchGoogletag().
    Object.defineProperty(window.__ts_googletag_target, 'defineSlot', {
      value: wrapped,
      writable: true,
      configurable: true,
      enumerable: true,
    })
  }

  const patchCmdPush = () => {
    if (!window.__ts_googletag_target?.cmd) return
    const orig = window.__ts_googletag_target.cmd.push
    if (!orig || orig.__ts_patched) return
    window.__ts_googletag_target.cmd.push = function () {
      // Wrap each function argument so patchGoogletag() runs immediately before the
      // callback executes — even when GPT drains the queue later, after defineSlot
      // is defined. Without wrapping, a callback queued before GPT loads gets stored
      // raw; when GPT drains the queue it calls the raw function and defineSlot is
      // never intercepted.
      const wrappedArgs = Array.from(arguments).map((fn) =>
        typeof fn === 'function'
          ? function () {
              patchGoogletag()
              return fn.apply(this, arguments)
            }
          : fn
      )
      return orig.apply(this, wrappedArgs)
    }
    window.__ts_googletag_target.cmd.push.__ts_patched = true
  }

  const patchApstag = () => {
    if (!window.apstag) return
    window.__ts_aps_detected = true
    const orig = window.apstag.fetchBids
    if (orig?.__ts_patched) return
    if (!orig) {
      // fetchBids not yet defined — intercept the moment it is attached.
      // Publishers sometimes write: window.apstag = {}; window.apstag.fetchBids = fn
      // The window.apstag setter fires on the first line; fetchBids doesn't exist yet.
      // This defineProperty re-invokes patchApstag() when fetchBids is finally set.
      if (!Object.getOwnPropertyDescriptor(window.apstag, 'fetchBids')?.set) {
        let _fetchBids
        Object.defineProperty(window.apstag, 'fetchBids', {
          configurable: true,
          get: () => _fetchBids,
          set: (fn) => {
            _fetchBids = fn
            patchApstag()
          },
        })
      }
      return
    }
    const wrapped = function (config, callback) {
      if (config?.slots) {
        config.slots.forEach((s) => window.__ts_captured_aps.push(s))
      }
      return orig.apply(this, arguments)
    }
    wrapped.__ts_patched = true
    // Use a value descriptor to bypass the fetchBids accessor setter we just installed.
    // Assigning via `=` would re-trigger the setter and recurse into patchApstag()
    // before __ts_patched is visible, causing infinite recursion.
    Object.defineProperty(window.apstag, 'fetchBids', {
      value: wrapped,
      writable: true,
      configurable: true,
      enumerable: true,
    })
  }

  // Intercept window.googletag and window.apstag via property setters so we patch
  // the moment the page assigns the stub — before the library loads and drains its
  // queue. Without this, `window.googletag = { cmd: [] }; cmd.push(fn)` or
  // `window.apstag = apstag_factory(); apstag.fetchBids(...)` both execute between
  // init-script injection and the first 50ms poll, leaving calls uncaptured.
  let _googletag = window.googletag
  window.__ts_googletag_target = _googletag
  Object.defineProperty(window, 'googletag', {
    configurable: true,
    get: () => _googletag,
    set: (val) => {
      _googletag = val
      window.__ts_googletag_target = val
      patchGoogletag()
      patchCmdPush()
    },
  })

  let _apstag = window.apstag
  Object.defineProperty(window, 'apstag', {
    configurable: true,
    get: () => _apstag,
    set: (val) => {
      _apstag = val
      patchApstag()
    },
  })

  // Immediate pass — handles the case where either library is already on window
  // (e.g. synchronous script above the page fold) before addInitScript runs.
  patchGoogletag()
  patchCmdPush()
  patchApstag()

  const interval = setInterval(() => {
    patchGoogletag()
    patchCmdPush()
    patchApstag()
  }, 50)
  setTimeout(() => clearInterval(interval), settle + 2000)
}, settle) // settle (number, ms) passed from Node.js host into browser context
```

The property setter on `window.googletag` is the critical addition. Publishers
universally write `window.googletag = window.googletag || { cmd: [] }` before
GPT loads. The setter fires the moment the page assigns this stub, immediately
patching `cmd.push` so any `cmd.push(fn)` calls that follow are captured — even
if they happen before the first 50ms interval. The 50ms polling remains as a
fallback for late-loading or dynamically injected GPT. `orig.apply(this, arguments)`
is used for `cmd.push` (not `orig.call(this, fn)`) because GPT's `cmd.push`
accepts multiple callbacks in some builds.

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

`slot.getTargetingMap()` returns `{[key: string]: string[]}` — values are arrays, not
scalars. `creative-opportunities.toml` expects scalar string values (`pos = "atf"`).
Conversion rule: take the first element of each array. If an array has more than one
value, emit a `# TODO` comment on that key:
`# TODO: multi-value targeting key "pos" — verify correct value (observed: ["atf","btf"])`.
This is the only lossy step in the generator and is flagged explicitly.

If `pubads()` or `getSlots()` is unavailable (GPT not fully initialised, or SPA
lazy-loads), `targeting` is an empty object and all slots receive `# TODO: add
targeting key-values from ad ops`.

#### Mapping captured data to `creative-opportunities.toml` schema

`googletag.defineSlot(unitPath, sizes, divId)` maps directly:

- `unitPath` → `gam_unit_path` (verbatim)
- `sizes` → `formats` (flatten nested arrays: `[[300,250],[728,90]]` → two entries).
  Non-numeric size entries (`"fluid"`, out-of-page markers, `[0,0]`) are skipped.
  If any are encountered, emit per slot:
  `# TODO: non-numeric size omitted — out-of-page/fluid format not supported in Phase 1`.
  Phase 1 supports numeric `[width, height]` pairs only.
  **If all sizes for a slot are non-numeric** (every entry is `"fluid"`, `[0,0]`, or
  an out-of-page marker), the slot is **omitted from the output** and a warning is
  written to stderr: `warn: slot from unitPath "/123/oop" omitted — no supported
numeric sizes`. This keeps `formats` non-empty for every emitted slot and avoids
  generating TOML that immediately fails `ts-config validate`. The auditor must add
  a supported format manually if the slot is needed.
- `divId` → `div_id`

**Slot ID generation algorithm:**

1. **Tokenize `divId`**: split on `-`, `_`, and digit runs (e.g. `ad-atf_sidebar-0-_r_2_`
   → `["ad", "atf", "sidebar", "0", "r", "2"]`).
2. **Filter tokens**: drop generic filler tokens (`ad`, `div`, `slot`, `unit`, `wrap`,
   `container`) and drop pure-numeric tokens, single-character tokens (`r`, `R`, `l`,
   `b`, etc.), hash-marker tokens (`_r_`, `_R_`, `_$`), and runs of hex digits longer
   than 4 chars. Single-character tokens are structurally insignificant — they are
   fragments of split hash markers (e.g. `_r_` splits to `r`) or positional indicators
   that carry no semantic meaning on their own.
3. **Keep semantic tokens**: retain tokens from the known set
   `{atf, btf, header, footer, sidebar, leaderboard, mrec, interstitial, top, bottom,
left, right, sticky, rail, banner}` plus any other non-generic alphanumeric segments.
4. **Join with `-`**: join the kept tokens → slug candidate (e.g. `atf-sidebar`).
5. **Fallback to unitPath slug**: if step 4 produces an empty string (all tokens were
   filtered), strip the GAM network prefix from `unitPath` and slugify the remainder
   (e.g. `/12345678/publisher/news` → `publisher-news`).

The user can rename in the output file. The comment on the generated `id` line should
cite which source was used: `# derived from semantic segments in div_id` or
`# derived from unitPath`.

**Collision resolution:** multiple placements can produce the same base slug (e.g.
two homepage slots both deriving `publisher-homepage`). `ts-config validate` requires
unique IDs, so the generator must guarantee uniqueness. Rule: if a slug is already
taken, append a hex suffix derived from `sha256(unitPath + "\0" + divId)` — the null
byte separator ensures `unitPath` and `divId` cannot be confused. Start with 6 hex
characters (`publisher-homepage-a1b2c3`). If that suffix is also taken (two slots with
different `unitPath+divId` but identical first 6 hash bytes), lengthen by 2 characters
at a time until unique. This is deterministic across re-crawls (same inputs → same
suffix) and stable when new URLs are added (unlike ordinals, which re-number on
insertion). Never silently overwrite an existing slot or drop one.

`apstag.fetchBids({slots: [{slotID, sizes}]})` maps as:

- `slotID` → `providers.aps.slot_id`

**APS-to-GPT correlation algorithm:** compute `name_candidate` and `size_candidate`
independently, then apply conflict resolution. Computing them in a single early-exit
pipeline makes the conflict step unreachable.

**Step A — compute `name_candidate`:**

1. **Exact token match** — `slotID === divId` (case-insensitive). If exactly one GPT
   slot matches, that is the name candidate. Stop.
2. **Semantic token match** — tokenize both `slotID` and `divId` by splitting on `-`,
   `_`, and digit runs. If both share a token from the set
   `{atf, btf, header, footer, sidebar, leaderboard, mrec, interstitial}`, that GPT
   slot is a candidate. If exactly one candidate survives, that is the name candidate.
   If zero or multiple survive, `name_candidate = None`.

**Step B — compute `size_candidate`** (always, independent of step A): 3. **Size-unique fallback** — the APS `sizes` are a subset of exactly one GPT slot's
sizes AND no other GPT slot has a size that overlaps the APS sizes. If exactly one
slot matches, that is the size candidate. Otherwise `size_candidate = None`.

**Step C — conflict resolution:** 4. Both defined and equal → assign. 5. Both defined and disagree (name → slot A, size → slot B) → assign `name_candidate`,
emit `# NOTE: APS size signals pointed to a different slot — confirm this mapping`. 6. Only `name_candidate` → assign. 7. Only `size_candidate` → assign. 8. Neither → emit `# TODO: confirm which slot this APS slot_id belongs to`.

Size alone is never sufficient when multiple GPT slots share the same dimensions.

`floor_price` — cannot be inferred from the browser. **Omitted entirely from generated
output.** The `lint` rule `floor_price_absent` warns on every slot that lacks it,
surfacing the gap to the auditor. Do not emit a placeholder value — `0.50` is a
syntactically valid, semantically real CPM floor. TOML comments are stripped on parse,
so `ts-config validate` would pass the file and it would be deployable as-is with an
arbitrary floor silently set by the generator. Omit and let lint drive the
conversation.

`page_patterns` — inferred from the crawled URL's path structure. E.g. crawled
`https://www.publisherorigin.com/2024/01/my-article/` → heuristically generates `/20*` for
date-prefixed paths (not `/20**` — that is an invalid glob that normalizes to the same
thing; the generator emits the valid form directly), `/news/**` for `/news/` prefix. For the homepage, emits `["/"]`.
The comment explains the heuristic:

```toml
page_patterns = ["/20*"]  # TODO: heuristic from date-prefixed path; verify covers all target URLs
```

`[slot.targeting]` — extracted from `googletag.pubads().getSlots()` post-render if
GPT exposes the targeting object. Otherwise emitted as empty with `# TODO`.

#### Multi-page crawl merge algorithm

Two distinct concepts govern merging; confusing them causes slot collapse.

**Intra-page identity: `unitPath + divId`**
On a single page, every distinct `(unitPath, divId)` pair is a separate slot. A
single GAM network path can serve multiple placements (e.g. `homepage_header_ad` and
`homepage_footer_ad` both use `gam_unit_path = "/88059007/publisher/homepage"` but
different `div_id`, formats, and APS IDs). Two entries with the same `unitPath` but
different `divId` on the same page are never merged — they are distinct placements.

**Cross-page reconciliation: same placement, possibly changed `divId`**
When crawling multiple URLs, the same logical placement may appear with a different
`divId` on a different page (common with Next.js server-component hash IDs). These
represent the same placement, not different slots. Reconciliation rule:

1. Two entries share the same `unitPath`.
2. Compute the **stable prefix** of each `divId`: the substring before the first
   occurrence of `_R_`, `_r_`, or `_$`. If none of these markers are present, the
   entire `divId` is its own stable prefix.
3. If both stable prefixes are identical AND non-empty AND at least 4 characters long,
   treat the entries as the same placement. The minimum length guard prevents false
   merges on generic prefixes like `ad-` (3 chars) that coincidentally appear in
   unrelated placements. Example: `ad-header-0-_R_abc` and `ad-header-0-_R_xyz` →
   both have stable prefix `ad-header-0-` (12 chars ≥ 4) → reconcile. Counter-example:
   `ad-_R_abc` and `ad-_R_xyz` → prefix `ad-` (3 chars < 4) → do NOT reconcile, emit
   as distinct placements.
4. If the stable prefixes differ (e.g. `ad-header` vs `ad-footer-_R_x` → prefixes
   `ad-header` and `ad-footer` differ), treat as distinct placements. Emit both.
5. Use the value from the **first** observation (deterministic across re-crawls of the
   same URL set) and emit `# WARN: div_id may vary across pages: [list of all observed
values]` on the reconciled slot.

Merge rules (applied after intra-page deduplication, across URLs):

1. **`page_patterns`** — unioned and deduplicated across all pages. Each crawled URL
   contributes its inferred pattern. E.g. crawling `/` and `/2024/01/test/` for the
   same placement produces `page_patterns = ["/", "/20*"]`.
2. **`divId` reconciliation** — see cross-page reconciliation above.
3. **`formats`** — unioned across pages. If formats differ (unlikely but possible),
   all observed sizes are included.
4. **APS correlation** — runs after the full union is assembled, not per-page.
5. **Duplicate GPT calls on the same page** — same `unitPath` + `divId` on one page
   is deduplicated silently (re-registration is a publisher-side bug, not a slot
   variant).

#### Function contracts

`generateSlotConfig` is **TOML emission only** — it receives already-merged slot data
and emits the `[[slot]]` blocks. It does not crawl, navigate, or merge. The entrypoint
`bin/ts-config-generate` owns multi-URL orchestration: it crawls each URL independently,
calls `mergePageResults`, then passes the merged output to `generateSlotConfig`.

```javascript
// lib/generate-slots.mjs

/**
 * Merge per-URL capture results into a single slot map before TOML emission.
 * Intra-page identity is unitPath+divId. Cross-page reconciliation uses the
 * heuristic described in §4.2. Applies all merge rules from §4.2.
 *
 * @param {Array<{ url: string, slots: Array<{unitPath, sizes, divId}>, aps: Array<{slotID, sizes}>, targeting: Object }>} perPageResults
 * @returns {{
 *   urls: string[],
 *   slots: Array<{
 *     unitPath: string,
 *     sizes: number[][],
 *     divId: string,
 *     pagePatterns: string[],
 *     targeting: Object,
 *     apsSlotId?: string,
 *     divIdWarn?: string[]  // all observed divId values when cross-page reconciliation fired
 *   }>
 * }}
 */
export function mergePageResults(perPageResults) { ... }

/**
 * Emit [[slot]] TOML from a merged slot map. TOML emission only — no crawling or merging.
 *
 * @param {string[]} urls - All crawled URLs (used for header comment only)
 * @param {Array<{
 *   unitPath: string,
 *   sizes: number[][],
 *   divId: string,
 *   pagePatterns: string[],
 *   targeting: Object,
 *   apsSlotId?: string,
 *   divIdWarn?: string[]
 * }>} mergedSlots - Output of mergePageResults; must be fully merged before calling
 * @returns {string} TOML content with [[slot]] blocks
 */
export function generateSlotConfig(urls, mergedSlots) { ... }
```

Both are called by `bin/ts-config-generate`. Neither is called from `detect.mjs` or
`audit.mjs`.

#### Generated TOML example

```toml
# Generated by ts-config-generate on 2026-05-19
# URL: https://www.publisherorigin.com/2024/01/my-article/
# Review all # TODO lines before deploying

[[slot]]
id = "atf-sidebar"  # derived from semantic segment in div_id ("atf_sidebar")
gam_unit_path = "/12345678/publisher/news"
div_id = "ad-atf_sidebar-0-_r_2_"  # TODO: verify this div_id is stable across deploys
page_patterns = ["/20*"]  # TODO: heuristic from date-prefixed path; verify covers all target URLs
formats = [{ width = 300, height = 250 }]
# floor_price omitted — set by ad ops before deploy (lint will warn)
# TODO: verify PBS stored request exists for slot id "atf-sidebar"

[slot.targeting]
# TODO: add targeting key-values from ad ops (e.g. pos = "atf", zone = "atfSidebar")

[slot.providers.aps]
slot_id = "aps-slot-atf-sidebar"  # TODO: confirm APS slot_id matches TAM config

[[slot]]
id = "publisher-homepage"
gam_unit_path = "/12345678/publisher/homepage"
div_id = "ad-header-0-_R_jpalubtak5lb_"  # TODO: may be Next.js generated ID — verify stable
page_patterns = ["/"]
formats = [
  { width = 970, height = 90 },
  { width = 728, height = 90 },
  { width = 970, height = 250 }
]
# floor_price omitted — set by ad ops before deploy (lint will warn)

[slot.targeting]
# TODO: add targeting key-values from ad ops

[slot.providers.aps]
slot_id = "aps-slot-homepage-header"
```

#### CLI interface

```bash
# Generate from live page to stdout (browser window shown by default)
ts-config-generate https://www.publisherorigin.com/2024/01/my-article/

# Specify output file
ts-config-generate https://www.publisherorigin.com/ --output creative-opportunities.toml

# Multi-page crawl — union of all slots found
ts-config-generate https://www.publisherorigin.com/ https://www.publisherorigin.com/2024/01/test/

# Headless mode for CI (default: headed to match audit-js-assets)
ts-config-generate https://www.publisherorigin.com/ --headless

# Wait for JS to settle (default: 6000ms, same as audit-js-assets)
ts-config-generate https://www.publisherorigin.com/ --settle 3000

# Validate output immediately after generation
ts-config-generate https://www.publisherorigin.com/ --validate
```

Default mode is headed (browser window visible) to match `audit-js-assets` convention.
`--headless` is the CI flag.

**Output and validation ordering:** TOML generation always writes to a temp file in the
same directory as the destination (never directly to the destination). When `--validate`
is requested, validation runs against the temp file before any rename. Only when
validation passes does the temp file get renamed to the destination. If validation
fails, the temp file is deleted and the existing destination is left untouched. This
sequence is also used when `--output` is given without `--validate` — atomic rename on
success, delete temp on any failure.

**Stdout is TOML-only.** When no `--output` is specified, TOML is written to stdout.
`--validate` in this mode: generate to a temp file, run `ts-config validate --json`
against the temp file (capturing output internally), print the TOML to stdout if
validation passes, write validation diagnostics to stderr, then delete the temp file.
Validation diagnostic text must never be written to stdout — mixing human text into a
TOML stream breaks any pipeline consumer.

`--validate` locates the binary via `$PATH` or `$TS_CONFIG_BIN` env var. If the binary
is not found and `--validate` was explicitly passed, the tool exits `1` with a clear
error: `"ts-config binary not found — set $TS_CONFIG_BIN or add to $PATH"`. Silently
continuing to `exit 0` would be a silent CI failure. If `ts-config validate` exits
non-zero, that exit code is propagated from `ts-config-generate`.

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
project conventions for error handling (`error-stack`, `derive_more`).

**Lockfile policy:** `crates/trusted-server-cli/Cargo.lock` is committed (mirrors
`crates/integration-tests/`). If a script already audits dependency version drift for
`integration-tests`, extend it to cover `trusted-server-cli` as well. CI must restore
from the committed lockfile (`cargo build --locked`) to prevent silent dependency
upgrades between runs.

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
serde_path_to_error = "0.1"
error-stack = "0.6"
derive_more = { version = "2.0", features = ["display", "error"] }

# Mirror workspace clippy lint policy (excluded crates do not inherit [workspace.lints]).
# print_stdout is allow here — the CLI's purpose is writing to stdout.
[lints.clippy]
unwrap_used = "deny"
expect_used = "allow"
panic = "deny"
module_name_repetitions = "allow"
must_use_candidate = "warn"
doc_markdown = "warn"
missing_errors_doc = "warn"
missing_panics_doc = "warn"
needless_pass_by_value = "warn"
redundant_closure_for_method_calls = "warn"
print_stdout = "allow"  # CLI legitimately writes TOML and diagnostics to stdout/stderr
print_stderr = "warn"   # unexpected stderr output is a bug; use structured error reporting
dbg_macro = "warn"
```

Build for local install (production artifact):

```bash
HOST=$(rustc -vV | sed -n 's/^host: //p')
cargo build --manifest-path crates/trusted-server-cli/Cargo.toml --release --target "$HOST" --locked \
  --target-dir "$(pwd)/target"
# Binary at: target/$HOST/release/ts-config
```

CI and development use the debug build (omit `--release`). All `cargo` invocations
must pass `--target "$HOST"` (because `.cargo/config.toml` sets the global build target
to `wasm32-wasip1`), `--locked` (to enforce the committed `Cargo.lock`), and
`--target-dir "$(pwd)/target"` (because excluded crates default to their own
`crates/trusted-server-cli/target/` directory, not the workspace root `target/`).
Binary lands at `target/$HOST/debug/ts-config`. This is what all CI steps and manual
verification steps use.

---

### 4.4 Validation logic: mirroring the runtime

Validation rules in `ts-config validate` are derived from
`crates/trusted-server-core/src/creative_opportunities.rs` and
`crates/trusted-server-core/build.rs`. Rule drift between the CLI and the runtime is
a bug.

**Intentional asymmetry:** `build.rs` validates slot IDs only (rule 3 above). The CLI
validates all 11 rules. This is intentional — the CLI is a richer pre-flight, not a
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

**Why copy instead of sharing `trusted-server-core`:** The ideal approach is for the
CLI to depend directly on `trusted-server-core` for validation and glob logic,
eliminating copy-paste drift by construction. This is blocked in Phase 1 because
`trusted-server-core` depends on the Fastly SDK and only compiles for `wasm32-wasip1`
— a native CLI cannot link it. The EdgeZero migration (tracked separately) requires
`trusted-server-core` to compile natively for the Axum adapter; once that lands, the
CLI can drop its copied functions and depend on the shared module directly. Until then,
`validate_slot_id()` and `matches_path_with_normalisation()` are deliberately copied
with `// NOTE: keep in sync with creative_opportunities.rs` comments in both files.
Rule drift is a bug. The round-trip CI test (§11 step 3) confirms the production
config passes both tools — it is not a full parity test. It only catches drift in
rules that the checked-in `creative-opportunities.toml` exercises. To improve parity
coverage, the test fixture matrix in §9 (invalid slot IDs, uncompilable patterns,
targeting types, empty slots) should be run against both the CLI and the equivalent
runtime logic in `creative_opportunities.rs` tests.

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
    #[display("slot `{id}` pattern `{pattern}` could not be compiled — this pattern will never match any URL")]
    UncompilablePattern { id: String, pattern: String },
    #[display("slot `{id}` format has zero dimension ({width}x{height})")]
    ZeroDimension { id: String, width: u32, height: u32 },
    #[display("slot `{id}` floor_price {value} is invalid (must be finite and >= 0)")]
    InvalidFloorPrice { id: String, value: f64 },
    #[display("slot `{id}` targeting key `{key}` has non-string value")]
    NonStringTargeting { id: String, key: String },
    #[display("slot `{id}` providers.aps.slot_id is empty")]
    EmptyApsSlotId { id: String },
    #[display("slot `{id}` pattern `{pattern}` does not begin with '/'")]
    MissingLeadingSlash { id: String, pattern: String },
    #[display("slot `{id}` gam_unit_path is set but empty")]
    EmptyGamUnitPath { id: String },
    #[display("slot `{id}` div_id is set but empty")]
    EmptyDivId { id: String },
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
| `crates/trusted-server-cli/Cargo.lock`       | Committed lockfile; CI uses `--locked`          |
| `crates/trusted-server-cli/src/main.rs`      | `ts-config` binary; subcommand dispatch         |
| `crates/trusted-server-cli/src/validate.rs`  | Rules 1–11; mirrors `creative_opportunities.rs` |
| `crates/trusted-server-cli/src/match_cmd.rs` | Glob matching; mirrors `match_slots()`          |
| `crates/trusted-server-cli/src/report.rs`    | Human-readable + JSON output formatting         |
| `scripts/validate-creative-opportunities.sh` | CI round-trip script                            |

#### Modified files

| File                                               | Change                                                                                                                                                                   |
| -------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `Cargo.toml` (workspace root)                      | Add `crates/trusted-server-cli` to `exclude` list                                                                                                                        |
| `.github/workflows/test.yml`                       | Add separate CLI job: fmt, clippy, test, build (host target, `--locked`)                                                                                                 |
| `.github/workflows/format.yml`                     | Add CLI fmt + clippy steps (host target)                                                                                                                                 |
| `.github/workflows/release.yml`                    | New: build `ts-config` for `aarch64-apple-darwin`, `x86_64-apple-darwin`, `x86_64-unknown-linux-gnu` on git tag; upload binaries + `sha256sums.txt` as release artifacts |
| `scripts/check-integration-dependency-versions.sh` | Extend to cover `crates/trusted-server-cli` alongside `crates/integration-tests` (per lockfile policy in §4.3)                                                           |

**Note:** there is no `.github/workflows/ci.yml` — the actual workflow files are
`test.yml` and `format.yml`. CLI steps must be added to both.

**Host target requirement:** `.cargo/config.toml` currently sets
`[build] target = "wasm32-wasip1"` globally, which means any `cargo` invocation
without an explicit `--target` flag builds for WASM. All CLI build and test commands
must specify the host target explicitly:

```yaml
- name: Build ts-config
  run: |
    HOST=$(rustc -vV | sed -n 's/^host: //p')
    cargo build --manifest-path crates/trusted-server-cli/Cargo.toml --target "$HOST" --locked \
      --target-dir "$(pwd)/target"

- name: Test ts-config
  run: |
    HOST=$(rustc -vV | sed -n 's/^host: //p')
    cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target "$HOST" --locked \
      --target-dir "$(pwd)/target"

- name: Validate creative-opportunities.toml
  run: ./target/$(rustc -vV | sed -n 's/^host: //p')/debug/ts-config validate --config creative-opportunities.toml
```

**Post-EdgeZero note:** the EdgeZero migration removes the global `[build] target`
from `.cargo/config.toml` (each adapter specifies its own target via aliases). Once
that lands, the `--target` flags above are no longer required and commands simplify to
`cargo build --manifest-path crates/trusted-server-cli/Cargo.toml --locked`. The CI steps
should be updated at that point.

`--release` is omitted in CI — debug build is sufficient for config validation and
avoids the compilation overhead. Use `--release` only for the production install
artifact.

### Distribution (Phase 1A)

Goal 1 states no Rust toolchain knowledge is required for auditors. Phase 1A ships a
GitHub Actions release job that builds `ts-config` for macOS (aarch64 and x86_64) and
Linux (x86_64) and attaches binaries to the GitHub release. Auditors install via:

```bash
# macOS (Apple Silicon) — no Rust toolchain required
# Download under the release filename so shasum -c can find it by name
curl -L https://github.com/IABTechLab/trusted-server/releases/latest/download/ts-config-aarch64-apple-darwin \
  -o ts-config-aarch64-apple-darwin
curl -L https://github.com/IABTechLab/trusted-server/releases/latest/download/sha256sums.txt \
  -o sha256sums.txt
grep ts-config-aarch64-apple-darwin sha256sums.txt | shasum -a 256 -c
# Rename and make executable only after checksum passes
mv ts-config-aarch64-apple-darwin ts-config && chmod +x ts-config

# Build from source (requires Rust toolchain)
HOST=$(rustc -vV | sed -n 's/^host: //p')
cargo build --manifest-path crates/trusted-server-cli/Cargo.toml --release --target "$HOST" --locked \
  --target-dir "$(pwd)/target"
```

The Node.js `ts-config-generate` is distributed via `npm install` from
`js-asset-auditor` — no Rust toolchain required for the generate command.

### Phase 1B — Node.js generator (ships after `feature/js-asset-auditor` merges)

#### New files

| File                                                                  | Description                                                                                                                                    |
| --------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `packages/js-asset-auditor/lib/generate-slots.mjs`                    | `mergePageResults()` + `generateSlotConfig()` functions; GPT/APS interception + TOML emission                                                  |
| `packages/js-asset-auditor/bin/ts-config-generate`                    | CLI entrypoint; Playwright session + arg parsing                                                                                               |
| `packages/js-asset-auditor/test/fixtures/fake-publisher.html`         | Synthetic publisher page — post-load `cmd.push` (exercises polling fallback)                                                                   |
| `packages/js-asset-auditor/test/fixtures/fake-publisher-pre-gpt.html` | Synthetic publisher page — pre-GPT `cmd.push` (exercises property-setter race)                                                                 |
| `packages/js-asset-auditor/test/fixtures/fake-publisher-aps.html`     | Synthetic publisher page — late `fetchBids` attachment (exercises APS `fetchBids` setter)                                                      |
| `packages/js-asset-auditor/test/fixtures/fake-publisher-direct.html`  | Synthetic publisher page — calls `googletag.defineSlot()` directly (no `cmd.push`), exercises the Layer 1 `patchGoogletag` `defineSlot` setter |
| `packages/js-asset-auditor/test/generate-slots.test.mjs`              | Unit + integration tests for `mergePageResults` and `generateSlotConfig`                                                                       |
| `docs/ts-config.md`                                                   | User-facing documentation for both tools                                                                                                       |

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
`field` (full TOML path string, e.g. `"slots[0].page_patterns[0]"`), `kind` (machine-readable tag),
`message` (human-readable string).

`kind` values by subcommand:

- `validate` (schema/rules): `unknown_field`, `missing_required_field`, `invalid_type`, `empty_page_patterns`, `empty_formats`, `glob_normalised`, `uncompilable_pattern`, `invalid_slot_id`, `duplicate_slot_id`, `zero_dimension`, `invalid_floor_price`, `non_string_targeting`, `empty_aps_slot_id`, `missing_leading_slash`, `empty_gam_unit_path`, `empty_div_id`
- `lint`: all of the above plus `floor_price_absent`, `overly_broad_pattern`, `cross_slot_aps_gap`, `unstable_div_id`, `duplicate_pattern`, `exact_only_pattern`, `equivalent_pattern`

Schema-level kinds (`unknown_field`, `missing_required_field`, `invalid_type`,
`empty_page_patterns`, `empty_formats`) are produced by the `toml::Value` first pass
and `serde_path_to_error` typed conversion. `slot` is always the slot's `id` string
or `null` — never a positional index like `"slots[1]"`. Use `null` when the error is
at file level or when the slot's `id` is itself missing or invalid (id not yet known
at parse time). The full TOML path (e.g. `"slots[1].page_patterns"`) always goes in
`field`, not in `slot`.

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
- **Isolated browser context** — each `ts-config-generate` run creates a fresh
  Playwright browser context (`browser.newContext()`) with no persistent profile,
  no stored cookies, and no shared state with other runs or the user's browser profile.
  `ts-config-generate` executes arbitrary publisher JavaScript; context isolation
  limits the blast radius of malicious page scripts.
- **Allowed URL schemes** — only `https://` and `http://` URLs are accepted. `file://`,
  `javascript:`, and `data:` URLs exit `2` before Playwright launches. Localhost and
  private-network IPs (`10.x`, `192.168.x`, `172.16–31.x`) are permitted to support
  local dev server testing but should not be used in CI against untrusted inputs.

---

## 8. Edge Cases

**Pattern that was never compilable** — a pattern that fails `Pattern::new()` even
after `**→*` substitution (e.g. `[invalid`) is silently skipped at runtime. The CLI
emits an error: "pattern `[invalid` could not be compiled — this pattern will never
match any URL." Only escalates to "slot will never match" when every pattern in the
slot's `page_patterns` array is uncompilable. Error, not warning.

**Empty `creative-opportunities.toml`** — zero slots is valid at runtime (feature
disabled). `ts-config validate` passes with note: "0 slots defined — auction will not
fire on any URL." `ts-config match` exits `1`.

**GPT not detected on target page** — `ts-config-generate` emits a warning and exits
`1`. The page may use a different tag management approach; try running in headed mode
(omit `--headless`) to observe what scripts load, or use a different URL.

**APS loaded but `fetchBids` never called** — emit TOML comment: `# APS detected but
fetchBids not observed — add providers.aps manually if APS is active.`

**Multiple pages, conflicting div IDs for same GAM unit path** — when crawling multiple
URLs, the same `unitPath` may appear with different `divId` values. Apply the
cross-page reconciliation rule from §4.2: compute the stable prefix of each `divId`
(strip everything from the first `_R_`/`_r_`/`_$` marker onward). If stable prefixes
match and are non-empty → reconcile (same placement, use first observed `divId`, emit
`# WARN`). If stable prefixes differ → distinct placements, emit both. This prevents
`ad-header` and `ad-footer-_R_x` from being wrongly merged just because one has a
hash marker.

**`googletag.cmd.push` deferred pattern** — most publishers do not call
`googletag.defineSlot()` directly at script parse time. Instead they use:

```javascript
googletag.cmd.push(function() { googletag.defineSlot(...) })
```

GPT drains its `cmd` queue asynchronously when the library loads, which can happen
before the 50ms polling interval fires. The Layer 1 shim patches `googletag.cmd.push`
to call `patchGoogletag()` before each queued callback executes, closing this race.
Publishers that call `defineSlot` directly (not inside `cmd.push`) are also covered:
`patchGoogletag` installs a property setter on `googletag.defineSlot` when the function
is not yet present, so the moment GPT attaches `defineSlot` to the object the setter
fires, wraps it with a value descriptor, and all subsequent direct calls are captured
regardless of when the next 50ms tick fires.

**Pattern glob normalization: `/20**`→`/20*`** — `/20**`follows the same rule as`/b**`: the `**` immediately follows a non-separator character (`0`), which is invalid
glob syntax. `Pattern::new("/20**")`fails; the normalization branch fires and replaces
it with`/20*`. The two patterns produce identical runtime matching behavior because
`require_literal_separator = false`makes`_`match across`/`boundaries already.
Authors who write`/20\*\*`get the correct match outcome, but the pattern is normalized
—`validate`emits a WARN, and the generated output uses`/20_`directly. The`equivalent_pattern`lint fires if both`/20\*_`and`/20_`appear in the same slot's`page_patterns`, since their canonicalized forms are identical.

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
    assert!(matches!(
        validate_slot_id("").expect_err("should return error for empty id"),
        ValidateError::EmptySlotId
    ));
}

#[test]
fn slot_id_rejects_space_and_bang() {
    assert!(matches!(
        validate_slot_id("bad slot id!").expect_err("should return error for invalid chars"),
        ValidateError::InvalidSlotId { .. }
    ));
}

#[test]
fn slot_id_rejects_html_injection() {
    assert!(matches!(
        validate_slot_id("<script>").expect_err("should return error for html characters"),
        ValidateError::InvalidSlotId { .. }
    ));
}

// src/match_cmd.rs — matches_path_with_normalisation
#[test]
fn valid_glob_returns_match_variant() {
    // /news/** is a valid glob (**  follows a separator); Pattern::new succeeds.
    assert!(matches!(
        matches_path_with_normalisation("/news/**", "/news/2024/article"),
        MatchResult::Match(true)
    ));
}

#[test]
fn date_prefix_glob_normalises_and_matches() {
    // /20** has ** after '0' (non-separator) — same rule as /b**; normalizes to /20*.
    let result = matches_path_with_normalisation("/20**", "/2024/01/article");
    assert!(matches!(result, MatchResult::NormalisedMatch { matched: true, .. }));
    if let MatchResult::NormalisedMatch { effective, .. } = result {
        assert_eq!(effective, "/20*", "should normalize to /20*");
    }
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
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("should parse JSON output");
    let warnings = json["warnings"].as_array().expect("should have warnings array");
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
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("should parse JSON output");
    let warnings = json["warnings"].as_array().expect("should have warnings array");
    assert!(warnings.iter().any(|w| w["kind"] == "unstable_div_id"));
}
```

**Test fixtures** (`crates/trusted-server-cli/tests/fixtures/`):

| File                          | Content / triggered kind                                                                                           |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `valid.toml`                  | 1 slot, clean — baseline for match/check/validate happy-path                                                       |
| `invalid-slot-id.toml`        | `id = "bad slot id!"` → `invalid_slot_id`                                                                          |
| `normalised-pattern.toml`     | `page_patterns = ["/b**"]` → exit 0 + `glob_normalised` warning                                                    |
| `nextjs-div-id.toml`          | `div_id = "ad-atf_R_abc123_"` → `unstable_div_id`                                                                  |
| `unknown-field.toml`          | Slot with typo field `page_pattern = ["/"]` → `unknown_field`                                                      |
| `missing-required-field.toml` | Slot missing `page_patterns` → `missing_required_field`                                                            |
| `invalid-type.toml`           | `floor_price = "cheap"` (string instead of float) → `invalid_type`                                                 |
| `invalid-media-type.toml`     | `formats = [{ width = 300, height = 250, media_type = 123 }]` → `invalid_type` at `slots[0].formats[0].media_type` |
| `empty-arrays.toml`           | `page_patterns = []` and `formats = []` → `empty_page_patterns`, `empty_formats`                                   |
| `duplicate-id.toml`           | Two slots with `id = "slot_a"` → `duplicate_slot_id`                                                               |
| `zero-dimension.toml`         | `formats = [{ width = 0, height = 250 }]` → `zero_dimension`                                                       |
| `invalid-floor.toml`          | `floor_price = -1.0` → `invalid_floor_price`                                                                       |
| `empty-aps-slot-id.toml`      | `[slot.providers.aps]` with `slot_id = ""` → `empty_aps_slot_id`                                                   |
| `unrecoverable-pattern.toml`  | `page_patterns = ["[invalid"]` — pattern never compiles even after `**→*` substitution                             |
| `equivalent-patterns.toml`    | `page_patterns = ["/20**", "/20*"]` within one slot → `equivalent_pattern` warning                                 |
| `missing-leading-slash.toml`  | `page_patterns = ["news/**"]` (no leading `/`) → `missing_leading_slash`                                           |
| `empty-gam-unit-path.toml`    | `gam_unit_path = ""` → `empty_gam_unit_path`                                                                       |
| `empty-div-id.toml`           | `div_id = ""` → `empty_div_id`                                                                                     |

CI command:

```bash
HOST=$(rustc -vV | sed -n 's/^host: //p')
cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target "$HOST" --locked \
  --target-dir "$(pwd)/target"
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
  assert.deepEqual(new Set(slots[0].pagePatterns), new Set(['/', '/20*']))
})

test('mergePageResults reconciles two Next.js hash divIds with the same stable prefix (same unitPath)', () => {
  // Both divIds have stable prefix 'ad-header-0-' (12 chars ≥ 4) — same placement observed on two pages.
  // Rule: stable prefixes identical AND non-empty AND ≥ 4 chars → reconcile; keep first, flag warn.
  // Contrast: 'ad-_R_abc123' (prefix 'ad-', 3 chars < 4) and 'ad-_R_xyz456' would NOT merge
  // (prefix too short to be substantive) and would produce two slots instead.
  const results = [
    {
      url: 'https://example.com/',
      slots: [
        {
          unitPath: '/123/slot',
          sizes: [[300, 250]],
          divId: 'ad-header-0-_R_abc123',
        },
      ],
      aps: [],
      targeting: {},
    },
    {
      url: 'https://example.com/p2/',
      slots: [
        {
          unitPath: '/123/slot',
          sizes: [[300, 250]],
          divId: 'ad-header-0-_R_xyz456',
        },
      ],
      aps: [],
      targeting: {},
    },
  ]
  const { slots } = mergePageResults(results)
  assert.equal(slots.length, 1, 'should merge into one placement')
  assert.equal(
    slots[0].divId,
    'ad-header-0-_R_abc123',
    'should keep first observed divId'
  )
  assert.ok(slots[0].divIdWarn, 'should flag divId instability warn')
})

test('mergePageResults emits two slots when both divIds are stable and differ (same unitPath)', () => {
  // No hash markers on either — these are two genuinely distinct placements.
  const results = [
    {
      url: 'https://example.com/',
      slots: [
        { unitPath: '/123/slot', sizes: [[300, 250]], divId: 'ad-header' },
        { unitPath: '/123/slot', sizes: [[728, 90]], divId: 'ad-footer' },
      ],
      aps: [],
      targeting: {},
    },
  ]
  const { slots } = mergePageResults(results)
  assert.equal(slots.length, 2, 'should treat as two distinct placements')
})

test('mergePageResults does NOT reconcile when stable prefix is shorter than 4 chars', () => {
  // Both divIds have hash markers, but their stable prefix is 'ad-' (3 chars < 4).
  // The min-length guard prevents false merges on generic short prefixes.
  const results = [
    {
      url: 'https://example.com/',
      slots: [
        { unitPath: '/123/slot', sizes: [[300, 250]], divId: 'ad-_R_abc123' },
      ],
      aps: [],
      targeting: {},
    },
    {
      url: 'https://example.com/p2/',
      slots: [
        { unitPath: '/123/slot', sizes: [[300, 250]], divId: 'ad-_R_xyz456' },
      ],
      aps: [],
      targeting: {},
    },
  ]
  const { slots } = mergePageResults(results)
  assert.equal(
    slots.length,
    2,
    'should not reconcile when stable prefix is shorter than 4 chars'
  )
})

test('generateSlotConfig emits valid [[slot]] TOML block', () => {
  const mergedSlots = [
    {
      unitPath: '/12345678/publisher/news',
      sizes: [[300, 250]],
      divId: 'ad-1',
      pagePatterns: ['/20*'],
      targeting: {},
      apsSlotId: 'aps-1',
    },
  ]
  const toml = generateSlotConfig(
    ['https://publisherorigin.com/2024/article/'],
    mergedSlots
  )
  assert.ok(toml.includes('[[slot]]'))
  assert.ok(toml.includes('id = "publisher-news"'))
  assert.ok(toml.includes('gam_unit_path = "/12345678/publisher/news"'))
  assert.ok(toml.includes('slot_id = "aps-1"'))
})

test('APS correlation assigns slot_id via semantic token match (slotID and divId share "atf" token)', () => {
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

test('APS correlation assigns via size_candidate when no name match exists', () => {
  // Step A yields no name_candidate (slotID 'aps-300' shares no token with divIds 'leaderboard', 'sidebar').
  // Step B: APS sizes [[300, 250]] ⊆ 'sidebar' GPT slot only (leaderboard has [[728, 90]]) → size_candidate.
  // Step C rule 7 (only size): assign size_candidate.
  const results = [
    {
      url: 'https://example.com/',
      slots: [
        {
          unitPath: '/123/leaderboard',
          sizes: [[728, 90]],
          divId: 'leaderboard',
        },
        { unitPath: '/123/sidebar', sizes: [[300, 250]], divId: 'sidebar' },
      ],
      aps: [{ slotID: 'aps-300', sizes: [[300, 250]] }],
      targeting: {},
    },
  ]
  const { slots } = mergePageResults(results)
  const sidebar = slots.find((s) => s.unitPath === '/123/sidebar')
  assert.equal(
    sidebar.apsSlotId,
    'aps-300',
    'should assign via size_candidate when no name match'
  )
  const leaderboard = slots.find((s) => s.unitPath === '/123/leaderboard')
  assert.equal(
    leaderboard.apsSlotId,
    undefined,
    'should not assign to non-matching slot'
  )
})

test('APS correlation emits NOTE and uses name_candidate when name and size disagree', () => {
  // Step A: 'aps-atf' shares token 'atf' with divId 'atf' → name_candidate = atf slot.
  // Step B: APS sizes [[300, 250]] ⊆ btf slot (atf has [[728, 90]]) → size_candidate = btf slot.
  // Step C rule 5 (both disagree): assign name_candidate, emit NOTE.
  const results = [
    {
      url: 'https://example.com/',
      slots: [
        { unitPath: '/123/atf', sizes: [[728, 90]], divId: 'atf' },
        { unitPath: '/123/btf', sizes: [[300, 250]], divId: 'btf' },
      ],
      aps: [{ slotID: 'aps-atf', sizes: [[300, 250]] }],
      targeting: {},
    },
  ]
  const { slots } = mergePageResults(results)
  const atf = slots.find((s) => s.unitPath === '/123/atf')
  assert.equal(
    atf.apsSlotId,
    'aps-atf',
    'should assign name_candidate when name and size disagree'
  )
  const toml = generateSlotConfig(['https://example.com/'], slots)
  assert.ok(
    toml.includes('# NOTE:'),
    'should emit NOTE when name and size disagree'
  )
})
```

**Integration test fixture** (`packages/js-asset-auditor/test/fixtures/fake-publisher.html`):

The fixture must not load live GPT (`securepubads.g.doubleclick.net`) — that makes
CI network-dependent and GPT behavior can change externally. Instead, the integration
test intercepts the GPT script URL via Playwright's `page.route()` and injects a
minimal local stub that implements only what the interception shim relies on:

```javascript
// In the integration test (before page.goto):
await page.route('**/gpt.js', (route) => {
  route.fulfill({
    contentType: 'application/javascript',
    body: `
      window.googletag = window.googletag || { cmd: [] }
      ;(function () {
        const slots = []
        const gt = window.googletag
        gt.defineSlot = function (unitPath, sizes, divId) {
          const slot = {
            unitPath, sizes, divId,
            _targeting: {},
            setTargeting(k, v) { this._targeting[k] = [v]; return this },
            addService() { return this },
            getSlotElementId() { return this.divId },
            getTargetingMap() { return this._targeting },
          }
          slots.push(slot)
          return slot
        }
        gt.pubads = () => ({
          enableSingleRequest() {},
          getSlots() { return slots },
        })
        gt.enableServices = () => {}
        gt.cmd.forEach((fn) => fn())
        gt.cmd.push = (fn) => fn()
      })()
    `,
  })
})
```

Four fixtures are required to cover the interception paths:

**Fixture A — `fake-publisher.html` (post-load push, exercises polling fallback):**
The `gpt.js` script tag must load synchronously (no `async` attribute) so the stub
executes and replaces `cmd.push = fn => fn()` before the following inline `<script>`
runs. With `async`, the load order is non-deterministic and the test becomes flaky.
By the time `cmd.push` runs, the stub has already initialized, verifying the
steady-state path without testing the property-setter race.

**Fixture B — `fake-publisher-pre-gpt.html` (pre-load push, exercises property setter):**
The page pushes callbacks to `googletag.cmd` before `gpt.js` loads. The stub is
delayed via `setTimeout` (100ms) to simulate async GPT load. This tests that the
`window.googletag` property setter fires on the inline `window.googletag = { cmd: [] }`
assignment and patches `cmd.push` before the subsequent `cmd.push(fn)` call.

In the integration test, the route for `gpt.js` in Fixture B fulfills after a delay:

```javascript
await page.route('**/gpt.js', async (route) => {
  await new Promise((r) => setTimeout(r, 100)) // simulate async GPT load
  route.fulfill({ contentType: 'application/javascript', body: GPT_STUB_BODY })
})
```

The `fake-publisher.html` fixture itself then uses `googletag.cmd.push` (not direct
calls) to exercise the Layer 1 `patchCmdPush` interception:

```html
<!DOCTYPE html>
<html>
  <head>
    <script src="https://securepubads.g.doubleclick.net/tag/js/gpt.js"></script>
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

The GPT `src` attribute is present in the HTML to match real publisher structure, but
`page.route()` intercepts it before the network request fires. CI runs fully offline.

Integration tests require assertions against all four fixtures:

1. **`fake-publisher.html`** (post-load `cmd.push` path): crawl produces TOML with
   `id = "test-atf"` and `id = "test-btf"` slots, both with correct `formats`.
2. **`fake-publisher-pre-gpt.html`** (pre-GPT property-setter path): same slot IDs
   and formats as fixture A — confirms the `window.googletag` setter fires before
   `cmd.push` and captures `defineSlot` calls made before GPT loads.
3. **`fake-publisher-aps.html`** (late `fetchBids` attachment): publisher assigns
   `window.apstag = {}` then attaches `fetchBids` in a separate statement. Crawl
   captures the APS slot and emits `slot_id` in the output TOML. This fixture
   exercises the `window.apstag.fetchBids` property setter added in `patchApstag`.
4. **`fake-publisher-direct.html`** (direct `defineSlot` call, no `cmd.push`): publisher
   calls `googletag.defineSlot()` directly in an inline `<script>` without going through
   `cmd.push`. This exercises the Layer 1 `patchGoogletag` `defineSlot` property setter
   rather than the `patchCmdPush` callback-wrapping path. Crawl must produce the same
   slot IDs and formats as fixture A.

### Round-trip CI test (Phase 1A)

```bash
# scripts/validate-creative-opportunities.sh
set -e
HOST=$(rustc -vV | sed -n 's/^host: //p')
cargo build --manifest-path crates/trusted-server-cli/Cargo.toml --target "$HOST" --locked \
  --target-dir "$(pwd)/target"
./target/"$HOST"/debug/ts-config validate --config creative-opportunities.toml
cargo build --manifest-path crates/trusted-server-core/Cargo.toml --locked
```

This script confirms that the checked-in `creative-opportunities.toml` passes the CLI
validator and compiles cleanly through `build.rs`. It does not prove full rule parity
(the CLI validates 11 rules; `build.rs` validates slot IDs only). The two tools are
complementary, not mirrors.

---

## 10. Open Questions

1. **`div_id` stability for Next.js publishers** — React server component IDs like
   `_R_jpalubtak5lb_` change when the component tree changes. Proposal: flag `_R_` or
   `_r_` prefix patterns in `lint` as "potentially unstable." Resolved above as an
   implemented lint rule; no further decision needed.

2. **Multi-URL crawl: page pattern inference** — date-prefixed `/20*` inference is
   publisher-specific. Proposal: emit the heuristic with explicit reasoning in the
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

All commands require `HOST=$(rustc -vV | sed -n 's/^host: //p')` due to the global
`wasm32-wasip1` target in `.cargo/config.toml`. All `cargo` invocations for the CLI
also require `--target-dir "$(pwd)/target"` because the excluded crate defaults to its
own `crates/trusted-server-cli/target/` directory rather than the workspace root.

1. Format check: `cargo fmt --manifest-path crates/trusted-server-cli/Cargo.toml -- --check`
2. Clippy: `cargo clippy --manifest-path crates/trusted-server-cli/Cargo.toml --target "$HOST" --all-targets --all-features --locked --target-dir "$(pwd)/target" -- -D warnings`
3. Build: `cargo build --manifest-path crates/trusted-server-cli/Cargo.toml --target "$HOST" --locked --target-dir "$(pwd)/target"`
4. Unit tests: `cargo test --manifest-path crates/trusted-server-cli/Cargo.toml --target "$HOST" --locked --target-dir "$(pwd)/target"`
5. `./target/"$HOST"/debug/ts-config validate` against `creative-opportunities.toml` → exits 0
6. Round-trip: `bash scripts/validate-creative-opportunities.sh` → passes
7. Run `./target/"$HOST"/debug/ts-config validate --config crates/trusted-server-cli/tests/fixtures/invalid-slot-id.toml` → exits 1 (fixture contains a slot with `id = "bad slot id!"`; never modify the checked-in `creative-opportunities.toml` for negative tests)

### Automated (CI) — Phase 1B

Runs in the `test-typescript` job (or a sibling job) after `feature/js-asset-auditor` merges.

8. `cd packages/js-asset-auditor && node --test test/generate-slots.test.mjs` → all tests pass
9. Playwright integration test: crawl `fake-publisher.html` (sync GPT load) → TOML has `id = "test-atf"` and `id = "test-btf"` with correct formats
10. Playwright integration test: crawl `fake-publisher-pre-gpt.html` (async GPT, pre-push) → same slots captured via property-setter path
11. Playwright integration test: crawl `fake-publisher-aps.html` (late `fetchBids`) → APS `slot_id` present in output TOML
12. Playwright integration test: crawl `fake-publisher-direct.html` (direct `defineSlot`, no `cmd.push`) → same slot IDs and formats as fixture A, confirming Layer 1 `patchGoogletag` `defineSlot` setter fires

### Manual (local only — requires network + browser)

8. `./target/"$HOST"/debug/ts-config match /2024/01/my-article/` → matches `atf_sidebar_ad` only
9. `./target/"$HOST"/debug/ts-config match /` → matches `homepage_header_ad` and `homepage_footer_ad`
10. `./target/"$HOST"/debug/ts-config lint` → surfaces `_R_` div_id warnings for current TOML
11. `ts-config-generate <publisher-url> --validate` → generates TOML, validates it, exits 0
12. Introduce `page_patterns = ["/b**"]` → `validate` exits 0 with WARN; `match /blog/foo`
    shows effective pattern `/b*` and its match result
