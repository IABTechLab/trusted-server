# Server-Side Ad Template CLI Design

**Date:** 2026-06-26  
**Status:** Draft design  
**Scope:** Static and browser-backed CLI diagnostics for server-side ad templates

## 1. Goal

Add Trusted Server CLI support for server-side ad-template onboarding and
verification without resurrecting the stale standalone `ts-config` design.

The CLI must answer two operator questions:

1. Given an effective `trusted-server.toml`, which configured ad-template slots
   match this path?
2. Given one or more live publisher URLs, are the configured slots for the
   final navigated paths actually present on the page according to DOM, GPT,
   and provider evidence, and do any runtime gates explain why Trusted Server
   would not inject or auction for that page?

The command surface is split by whether the command is local-config-only or
browser-backed:

```bash
ts config ad-templates lint
ts config ad-templates match <path-or-url>
ts config ad-templates check <path-or-url>
ts config ad-templates explain <path-or-url>

ts audit ad-templates verify <url>...
```

Static commands live under `ts config` because they only load local effective
app config. Browser-backed verification lives under `ts audit` because it loads
public publisher pages in Chrome/Chromium and observes live page behavior.

## 2. Context

This design replaces the stale PR #724 direction.

PR #724 designed a standalone `ts-config` binary around a
`creative-opportunities.toml` file. That is no longer the project shape:

- Trusted Server configuration now flows through the unified `ts` CLI from PR
  #799.
- Server-side ad-template slots live under `[creative_opportunities]` /
  `[[creative_opportunities.slot]]` in `trusted-server.toml`.
- Effective config can include EdgeZero app-config environment overlays unless
  `--no-env` is passed.
- Operator-owned `trusted-server.toml` is ignored; the repository tracks
  `trusted-server.example.toml`.

PR #799 is the CLI base. It owns the `ts` binary, EdgeZero lifecycle delegates,
and typed app-config validation/push/diff behavior.

PR #800 is the audit dependency. It adds the generic browser-backed
`ts audit <url>` collector using local Chrome/Chromium. At the time this spec
was written, PR #800 was stale relative to the latest #799 head, so this work
depends on the #800 audit collector after it is rebased onto the latest #799
typed blob-config model.

## 3. Non-Goals

- Do not add a standalone `ts-config` binary.
- Do not reintroduce `creative-opportunities.toml`.
- Do not implement browser-backed generation in Phase 1.
- Do not mutate `trusted-server.toml` from `verify`.
- Do not probe PBS, GAM, or APS management APIs.
- Do not require EdgeZero platform adapters for local static diagnostics.
- Do not make `ts audit ad-templates verify` push, provision, deploy, or update
  platform resources.
- Do not rely on real GPT or APS network calls in tests.

Browser-backed generation is a later phase:

```bash
ts audit ad-templates generate <url>...
```

That phase needs separate rules for slot ID derivation, page-pattern inference,
multi-URL merging, TOML ordering, and whether the command emits a patch, a draft
file, or full config blocks.

## 4. Command Surface

### 4.1 Shared Config Flags

All `ts config ad-templates ...` commands and
`ts audit ad-templates verify` accept the same local app-config flags:

```bash
--app-config <path>
--manifest <path>
--no-env
```

Defaults match PR #799:

| Option         | Default                                          |
| -------------- | ------------------------------------------------ |
| `--app-config` | `<app name>.toml`, resolved from `edgezero.toml` |
| `--manifest`   | `edgezero.toml`                                  |
| `--no-env`     | `false`; app-config env overlay is applied       |

If an explicit `--app-config` path is supplied and missing, the command reports
that path as the error. It must not silently fall back to an environment or
manifest-derived path.

### 4.2 Static Config Diagnostics

```bash
ts config ad-templates lint [--app-config <path>] [--manifest <path>] [--no-env]
```

Reports whether `[creative_opportunities]` is configured, how many slots exist,
GAM network ID, auction timeout, auction enablement, configured auction
providers, and whether current EdgeZero routing will fall back to the legacy
path when configured slots are present.

```bash
ts config ad-templates match <path-or-url> [--details] ...
```

Normalizes a path or full URL to a path and reports the slots matched by the
runtime `creative_opportunities::match_slots` logic. `--details` includes slot
div ID, GAM unit path, page patterns, formats, and configured providers.

```bash
ts config ad-templates check <path-or-url> \
  (--expected-slot <id>... | --expect-no-slots) \
  [--allow-extra-slots] ...
```

CI-friendly assertion wrapper around the same matching logic.

```bash
ts config ad-templates explain <path-or-url> \
  [--method GET] \
  [--non-navigation] \
  [--prefetch] \
  [--bot] \
  [--consent-denied] \
  [--edgezero-enabled] ...
```

Explains the major runtime gates that decide whether the server-side ad stack
would run for a page request. This is a local model, not a live request replay.

### 4.3 Browser-Backed Verification

```bash
ts audit ad-templates verify <url>... \
  [--app-config <path>] \
  [--manifest <path>] \
  [--no-env] \
  [--strict] \
  [--json] \
  [--scroll]
```

Behavior:

- Accept one or more `http` or `https` URLs.
- Reject all other schemes before launching a browser.
- Load the effective Trusted Server app config.
- For each URL, navigate first, collect the final URL, normalize the final URL
  to a path, and call `creative_opportunities::match_slots`.
- Preserve the requested URL/path separately from the final URL/path.
- Emit a redirect warning when the final path differs from the requested path.
- Expect only the slots matched for the final URL path to be present on that
  live page.
- Report live DOM/GPT/APS ad-slot evidence that does not correspond to a
  matched configured slot as structured extra evidence.
- Launch Chrome/Chromium through the audit collector from the rebased #800 work.
- Inject a read-only ad-template collector before publisher scripts run.
- Compare configured matched slots against DOM, GPT, and APS evidence.
- Report runtime ad-stack gate evidence separately from placement evidence.
- Print human output by default.
- Emit stable machine-readable output with `--json`.
- Exit `0` by default for missing or partial live evidence; this is an
  auditor-assist mode.
- Exit non-zero under `--strict` when a matched configured slot is missing or
  only partially confirmed.

`--scroll` performs a deterministic scroll pass after initial load and settle.
It is opt-in because it is slower and can trigger additional page behavior.
Slots first observed during scroll count as confirmed when the GPT evidence is
otherwise sufficient.

## 5. Confirmation Model

The verifier compares configured expected slots to live page evidence.

It must keep three concepts separate:

1. **Static slot matching:** which configured slots match a URL path according
   to `creative_opportunities::match_slots`.
2. **Runtime ad-stack eligibility:** whether Trusted Server would run its
   server-side ad stack for the audited navigation. This mirrors
   `should_run_server_side_ad_stack` for the initial publisher request and the
   `/__ts/page-bids` kill-switch/consent behavior for SPA route updates.
3. **Live placement evidence:** what the browser actually observes on the
   rendered page through DOM, GPT, and APS evidence.

`verify` is primarily a live placement verifier. `--strict` fails when matched
configured slots for an eligible page are missing or partial. Runtime gates are
reported so operators can distinguish "the slot is not on the page" from "the
current request/config would intentionally suppress Trusted Server ad-template
injection or page-bids slot output".

### 5.1 Expected Slots

For each input URL:

1. Navigate the browser to the requested URL.
2. Record `requested_url`, `requested_path`, `final_url`, and `final_path`.
3. Match configured slots through the core runtime matcher using `final_path`.
4. Build an expected-slot record for each matched slot:
   - slot ID;
   - resolved div ID;
   - resolved GAM unit path;
   - configured formats;
   - configured providers;
   - matching page patterns.

Only these expected slots are verified for that page. For example, slots whose
only pattern is `/` are expected for the homepage path, not for `/news/story`.

When a navigation redirects, `verify` uses the final path for expected slots and
reports the requested path in output. This matches runtime behavior: Trusted
Server evaluates the actual publisher request path it handles, not the URL the
operator typed before redirects.

### 5.2 Runtime Gate Evidence

For each page result, `verify` reports a local runtime-gate model:

| Gate                     | Source                                                                                                   |
| ------------------------ | -------------------------------------------------------------------------------------------------------- |
| `method_get`             | Browser navigation request; expected to pass for normal `verify`.                                        |
| `navigation`             | Browser navigation request; expected to pass for normal `verify`.                                        |
| `not_prefetch`           | Browser request headers; expected to pass unless the collector is extended with prefetch simulation.     |
| `not_bot`                | Browser User-Agent checked against the runtime bot fragments.                                            |
| `matched_slots`          | Final-path slot matching.                                                                                |
| `auction_enabled`        | Effective `[auction].enabled` / orchestrator enablement from app config.                                 |
| `consent_allows_auction` | `unknown` unless the collector can prove a consent-allowed or consent-denied state for the live request. |

`runtime_ad_stack_expected` is a three-state value: `yes`, `no`, or `unknown`.
Known blocking gates produce page warnings and set
`runtime_ad_stack_expected = "no"`. Unknown gates set
`runtime_ad_stack_expected = "unknown"` but do not by themselves fail
`--strict`.

If `runtime_ad_stack_expected = "no"` because of a known config/request gate
such as `[auction].enabled = false`, strict mode does not fail missing GPT/APS
evidence for that page. The page result is reported as skipped for runtime
verification while still showing the static expected slots and any live
placement evidence that was observed.

If `runtime_ad_stack_expected = "yes"` or `"unknown"`, strict mode applies the
normal missing/partial placement rules from §5.6.

For SPA routes, `/__ts/page-bids` returns no slots when the ad-stack kill switch
or consent gate blocks the stack. Browser verification should report observed
page-bids responses when available, but it must not require real partner bids in
tests.

Live ad-slot evidence that does not map to a matched expected slot is reported
as structured extra evidence. Extra evidence can identify publisher-owned slots
that have not yet moved into server-side ad templates, slots whose
`page_patterns` are too narrow, or slots that should stay outside Trusted
Server. It does not make `--strict` fail in Phase 1.

### 5.3 DOM Slot Resolution

The verifier must mirror the runtime GPT bootstrap's slot-root resolution:

1. Try `document.getElementById(slot.div_id)`.
2. If absent, find the first element with an ID that starts with `slot.div_id`.
3. Ignore elements whose ID ends with `-container`.

This is required because `div_id` may intentionally be a stable prefix for
framework-generated IDs, for example `ad-header-0-`.

### 5.4 GPT Evidence

A slot is confirmed by GPT evidence when the live page exposes a GPT slot whose:

- ad unit path equals the configured resolved GAM unit path;
- slot element ID equals the resolved DOM element ID or an existing
  `${resolved_dom_id}-container` element used by Trusted Server when defining
  its own slot;
- configured sizes are compatible with the observed GPT sizes.

The collector should observe both direct `googletag.defineSlot` calls and
post-load `googletag.pubads().getSlots()` state.

Size compatibility is defined for Phase 1 as follows:

- Normalize configured sizes from `CreativeOpportunityFormat` values where
  `media_type = "banner"` into `(width, height)` pairs.
- Normalize observed GPT sizes from `defineSlot` input and `getSizes()` output:
  - `[300, 250]` becomes one `(300, 250)` pair.
  - `[[300, 250], [728, 90]]` becomes two pairs.
  - non-numeric values such as `"fluid"` are ignored for numeric matching and
    reported as warnings.
- A GPT slot's sizes are compatible when the configured banner size set and the
  observed numeric GPT size set have at least one pair in common.
- Extra observed GPT sizes do not block confirmation, but they are reported as
  warnings so auditors can decide whether to add formats to config.
- Configured banner sizes that are not observed do not block confirmation when
  at least one configured size was observed, but they are reported as warnings.
- If ad unit path and div match but no numeric size overlap exists, the slot is
  `partial`, not `confirmed`.
- Configured `video` and `native` formats are not used for Phase 1 GPT size
  confirmation. If a matched slot has only non-banner formats, the verifier
  reports it as `partial` with an unsupported-format warning unless a later
  phase defines video/native verification.
- Out-of-page GPT slots are not confirmed in Phase 1 because the current
  server-side ad-template path is slot/div based. They are reported as warnings
  when observed.

### 5.5 APS Evidence

When a configured slot has `providers.aps.slot_id`, the collector records
`apstag.fetchBids` calls and compares configured slot IDs and sizes with the APS
payload.

APS evidence is a provider-level signal. Missing or ambiguous APS evidence
creates a provider warning, but it does not by itself make an otherwise GPT-
confirmed slot fail `--strict` in Phase 1.

### 5.6 Statuses

| Status      | Meaning                                                                                                                                                                                                                                                                          |
| ----------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `confirmed` | GPT evidence matches the configured GAM unit path, div resolution, and compatible sizes.                                                                                                                                                                                         |
| `partial`   | The page has some evidence for the configured slot, but not enough to confirm it. This includes DOM-only evidence, GPT path/div matches with incompatible sizes, GPT path/div matches for unsupported non-banner-only configured formats, and other non-confirming GPT evidence. |
| `missing`   | No DOM or GPT evidence confirms the configured slot.                                                                                                                                                                                                                             |

In `--strict` mode:

- `missing` fails.
- `partial` fails.

Provider issues are not statuses. They are warnings attached to the slot result.
For example, a slot can be `confirmed` and still carry a warning that configured
APS evidence was missing or ambiguous. Provider warnings do not fail `--strict`
unless a future `--strict-providers` flag is added.

## 6. Architecture

The architecture should keep command parsing thin and move ad-template behavior
into pure, testable modules.

```text
crates/trusted-server-cli/src/
  app_config.rs
  ad_templates/
    mod.rs
    expected.rs
    compare.rs
    output.rs
  config_ad_templates.rs
  audit/
    page.rs
    browser.rs
    ad_templates.rs
```

### 6.1 `app_config.rs`

Shared loader for effective Trusted Server app config.

Responsibilities:

- read `edgezero.toml` through EdgeZero manifest helpers;
- resolve the default `<app name>.toml` path;
- apply EdgeZero app-config env overlay unless `--no-env`;
- return `TrustedServerAppConfig` / `Settings`;
- report errors in the same terms as #799 config commands.

This avoids duplicating config path and env-overlay behavior between
`ts config ad-templates ...` and `ts audit ad-templates verify`.

The current branch already has a private loader in `config_ad_templates.rs`.
Before adding browser-backed verification, move that behavior into this shared
module and route the existing static commands through it so both command
families load the same effective config.

### 6.2 `ad_templates::expected`

Pure local expected-slot model.

Responsibilities:

- normalize path-or-URL input;
- call `creative_opportunities::match_slots`;
- convert matched slots into stable expected-slot structs;
- preserve deterministic ordering by slot order from config.

This module must not compile glob patterns independently or duplicate matching
semantics.

If richer pattern diagnostics are needed, add a small helper to
`trusted-server-core::creative_opportunities` and use it from both runtime and
CLI.

### 6.3 `ad_templates::compare`

Pure comparison between expected slots and collected browser evidence.

Responsibilities:

- implement DOM prefix matching rules;
- compare GPT path, div, and size evidence;
- compare APS evidence;
- collect unmatched live DOM/GPT/APS ad-slot evidence as structured
  `extra_evidence`;
- assign `confirmed`, `partial`, `missing`, and provider warning details;
- decide strict failure status.

This module should be testable without launching Chrome.

### 6.4 `ad_templates::output`

Human and JSON output model.

Responsibilities:

- serialize stable JSON output;
- keep arrays ordered by input URL, then configured slot order, then provider
  name;
- render concise human summaries;
- avoid leaking page HTML, cookies, local storage, or arbitrary page data.

### 6.5 `config_ad_templates.rs`

Thin Clap adapter for `ts config ad-templates ...`.

Responsibilities:

- parse command arguments;
- call `app_config` and `ad_templates::expected`;
- delegate formatting to `ad_templates::output`;
- keep no browser-specific logic.

### 6.6 `audit::browser`

Shared browser utility extracted from or aligned with the rebased #800 audit
collector.

Responsibilities:

- locate Chrome/Chromium;
- launch an isolated profile;
- reject non-HTTP(S) URLs before navigation;
- set bounded navigation and settle timeouts;
- run optional init scripts;
- perform optional deterministic scroll;
- collect final URL, title, rendered scripts, resource entries, and optional
  ad-template evidence.

The generic `ts audit <url>` command from #800 should continue to work without
ad-template verification enabled.

### 6.7 `audit::ad_templates`

Browser-backed verifier orchestration.

Responsibilities:

- parse `ts audit ad-templates verify`;
- load effective config through `app_config`;
- compute expected slots for each URL;
- run the browser collector with ad-template evidence enabled;
- call `ad_templates::compare`;
- print human or JSON output;
- apply default auditor-assist exit behavior and `--strict` behavior.

## 7. Browser Collector

The ad-template collector is injected before page scripts run. It is read-only:
it records evidence and calls original page functions with unchanged arguments.

The rebased #800 collector must grow a pre-navigation init-script hook before it
can satisfy this spec. The stale #800 collector only navigates, waits, and reads
post-load page state; that is insufficient for GPT/APS call evidence.

Instrumentation requirements:

- install the collector through the browser's "evaluate on new document" /
  init-script mechanism before navigation;
- serialize only configured div prefixes and provider IDs needed for matching;
- observe pages that create `window.googletag = { cmd: [] }` after injection;
- wrap `googletag.cmd.push` callbacks without changing callback order;
- record direct `googletag.defineSlot` calls and calls executed from the GPT
  command queue;
- read final `googletag.pubads().getSlots()` state after settle and after
  scroll;
- observe pages that assign `window.apstag` after injection and wrap
  `apstag.fetchBids` when present;
- tolerate pages that never load GPT or APS and report warnings instead of
  throwing collector errors.

Evidence to collect:

- DOM elements with IDs relevant to configured slot div prefixes;
- calls to `googletag.defineSlot`;
- final `googletag.pubads().getSlots()` state after settle and after scroll;
- calls to `apstag.fetchBids`;
- timestamps or phases indicating whether evidence was observed during
  `initial_load` or `scroll`.

The collector must not:

- block, rewrite, or suppress publisher scripts;
- override `navigator.webdriver`;
- capture cookies, local storage, session storage, request bodies, or arbitrary
  page data;
- require real GPT/APS network calls in test fixtures.

## 8. JSON Output Contract

`--json` emits deterministic JSON. Shape:

```json
{
  "ok": true,
  "strict": false,
  "pages": [
    {
      "url": "https://www.example.com/news/story",
      "final_url": "https://www.example.com/news/story",
      "requested_path": "/news/story",
      "path": "/news/story",
      "runtime_ad_stack_expected": "unknown",
      "gates": {
        "method_get": "pass",
        "navigation": "pass",
        "not_prefetch": "pass",
        "not_bot": "pass",
        "matched_slots": "pass",
        "auction_enabled": "pass",
        "consent_allows_auction": "unknown"
      },
      "matched_slot_count": 1,
      "slots": [
        {
          "id": "atf",
          "status": "confirmed",
          "phase": "initial_load",
          "configured": {
            "div_id": "ad-atf-",
            "gam_unit_path": "/123/news/atf",
            "formats": [
              { "width": 300, "height": 250, "media_type": "banner" }
            ],
            "providers": ["aps"]
          },
          "evidence": {
            "dom_id": "ad-atf-0",
            "gpt": {
              "gam_unit_path": "/123/news/atf",
              "div_id": "ad-atf-0",
              "sizes": [[300, 250]]
            }
          },
          "warnings": []
        }
      ],
      "extra_evidence": [],
      "warnings": []
    }
  ],
  "warnings": []
}
```

Warning entries are objects with stable `code` and human-readable `message`
fields. Human output may print only the message. JSON consumers must not need to
parse warning strings.

Extra live evidence is structured:

```json
{
  "kind": "gpt",
  "phase": "initial_load",
  "dom_id": "ad-right-rail-0",
  "gam_unit_path": "/123/publisher/right-rail",
  "sizes": [[300, 250]],
  "reason": "no_configured_slot_matched"
}
```

Allowed `kind` values for Phase 1 are `dom`, `gpt`, and `aps`.

Strict-mode failures with page results use the same shape and set `ok` to
`false`. Example partial slot:

```json
{
  "ok": false,
  "strict": true,
  "pages": [
    {
      "url": "https://www.example.com/",
      "final_url": "https://www.example.com/",
      "requested_path": "/",
      "path": "/",
      "runtime_ad_stack_expected": "unknown",
      "gates": {
        "method_get": "pass",
        "navigation": "pass",
        "not_prefetch": "pass",
        "not_bot": "pass",
        "matched_slots": "pass",
        "auction_enabled": "pass",
        "consent_allows_auction": "unknown"
      },
      "matched_slot_count": 1,
      "slots": [
        {
          "id": "homepage-header",
          "status": "partial",
          "phase": "initial_load",
          "configured": {
            "div_id": "ad-header-0-",
            "gam_unit_path": "/123/homepage/header",
            "formats": [{ "width": 728, "height": 90, "media_type": "banner" }],
            "providers": ["aps"]
          },
          "evidence": {
            "dom_id": "ad-header-0-_R_abc123",
            "gpt": null
          },
          "warnings": [
            {
              "code": "dom_without_gpt",
              "message": "DOM element matched, but no GPT slot evidence was observed"
            }
          ]
        }
      ],
      "extra_evidence": [],
      "warnings": []
    }
  ],
  "warnings": []
}
```

For errors that occur before any page result can be produced, the command exits
non-zero and prints the normal CLI error. JSON error output can be added later
if the base CLI standardizes it.

For multi-URL runs, browser/navigation failures after argument validation are
page-level failures when possible. The command continues to the remaining URLs,
sets top-level `ok` to `false`, and includes a page result:

```json
{
  "url": "https://www.example.com/broken",
  "final_url": null,
  "requested_path": "/broken",
  "path": null,
  "error": {
    "code": "navigation_failed",
    "message": "failed to read main document navigation response"
  },
  "slots": [],
  "extra_evidence": [],
  "warnings": []
}
```

Invalid schemes are still rejected before browser launch for the whole command,
because they are argument errors rather than page collection results.

## 9. Error Handling

Static commands fail when:

- config cannot be loaded;
- `[creative_opportunities]` is malformed;
- CLI assertions in `check` fail.

Browser verification fails when:

- config cannot be loaded;
- any URL is not HTTP(S);
- Chrome/Chromium cannot be found or launched;
- all navigations fail before any page result can be collected;
- at least one page-level error occurs in a multi-URL run;
- command output cannot be written;
- `--strict` is set, runtime verification is not skipped by a known gate, and
  at least one matched slot is missing or partial.

Browser collection can still produce a page result with warnings when:

- page settle times out;
- a navigation redirects before final URL matching;
- scroll evidence is incomplete;
- GPT is not loaded;
- APS is not observed;
- provider evidence is ambiguous;
- extra live DOM/GPT/APS ad-slot evidence has no matched configured slot;
- no slots match the URL.

## 10. Testing

Static tests:

- parse every `ts config ad-templates` command;
- load temp `edgezero.toml` and temp `trusted-server.toml`;
- verify `--app-config`, `--manifest`, and `--no-env` behavior;
- verify `/`, `/news/*`, and full URL normalization behavior;
- verify `check` success and failure output.
- verify the existing static command loader uses the shared `app_config` module.

Pure comparison tests:

- exact DOM ID match;
- prefix DOM ID match for framework-generated suffixes;
- ignore `-container` elements;
- GPT confirms by GAM unit path, div ID, and compatible sizes;
- DOM-only creates `partial`;
- no DOM/GPT creates `missing`;
- APS match creates no provider warning;
- APS missing/ambiguous creates provider warnings;
- `--strict` fails only missing and partial slots.

Browser fixture tests:

- local HTML fixture with direct `googletag.defineSlot`;
- fixture using `googletag.cmd.push`;
- fixture assigning `window.googletag` after collector injection;
- fixture with delayed/lazy slot observed only with `--scroll`;
- fixture with APS `fetchBids`;
- fixture assigning `window.apstag` after collector injection;
- redirect fixture that matches expected slots on final path;
- multi-URL fixture where one URL fails and one URL returns page results;
- fixture where `[auction].enabled = false` reports runtime skipped instead of
  strict missing-slot failure;
- invalid non-HTTP(S) URL rejection before browser launch;
- JSON contract tests for warning codes, `extra_evidence`, page errors,
  deterministic ordering, `partial`, `missing`, and strict failures;
- fixture with no real GPT/APS network dependency.

Verification commands:

```bash
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --package trusted-server-cli --target <host-triple>
```

## 11. Branch And PR Plan

The implementation should not be built on stale #724.

Recommended dependency order:

1. Land or rebase PR #799 as the CLI base.
2. Rebase PR #800 onto the latest #799 head so `ts audit` uses the current typed
   blob app-config model.
3. Harden and refactor the existing static `ts config ad-templates ...`
   diagnostics on top of the current server-side ad-template branch and #799:
   extract the private config loader into `app_config`, move pure expected-slot
   logic into `ad_templates::expected`, and keep existing behavior covered by
   tests.
4. Extend the rebased #800 collector with pre-navigation init scripts,
   ad-template evidence hooks, optional scroll, page-level errors, and bounded
   structured output.
5. Build `ts audit ad-templates verify` on top of that collector and the
   server-side ad-template branch.
6. Keep `generate` for a separate Phase 2 spec and PR.

If delivery needs to be split, static diagnostics can land before browser-backed
verification. Browser-backed verification should not duplicate the #800 browser
collector.

## 12. CLI Namespace Decision

`ts audit ad-templates verify` is the final command shape for browser-backed
ad-template verification.

When this work is combined with the rebased #800 audit command, `ts audit`
should become a subcommand namespace:

```bash
ts audit page <url>
ts audit generate <url>
ts audit ad-templates verify <url>...
```

The existing #800 `ts audit <url>` behavior should be preserved as a
compatibility alias for `ts audit generate <url>` during the transition,
including its artifact output flags. This avoids a successful but silent
behavior change for existing onboarding scripts.

Parsing contract:

- `ts audit page <url>` is the canonical generic page-audit command.
- `ts audit generate <url>` is the canonical artifact-generation command.
- `ts audit ad-templates verify <url>...` is the canonical ad-template verifier.
- `ts audit <url>` is a hidden compatibility alias for
  `ts audit generate <url>` and is accepted only when `<url>` parses as `http`
  or `https`.
- `ts audit ad-templates` must never be treated as a legacy URL positional.
- `ts audit page` without a URL must fail with the normal Clap missing-argument
  error.

Implementation shape:

```rust
#[derive(Debug, clap::Args)]
struct AuditArgs {
    #[command(subcommand)]
    command: Option<AuditSubcommand>,
    #[arg(value_parser = parse_http_url, hide = true)]
    legacy_url: Option<url::Url>,
}

#[derive(Debug, clap::Subcommand)]
enum AuditSubcommand {
    Page(PageAuditArgs),
    #[command(name = "ad-templates", subcommand)]
    AdTemplates(AuditAdTemplatesCommand),
}
```

If Clap cannot enforce the optional-subcommand plus hidden positional contract
cleanly, implement a small custom dispatcher for the `audit` argv tail and test
it directly. Required parser tests:

- `ts audit https://www.example.com/` dispatches to artifact generation;
- `ts audit page https://www.example.com/` dispatches to page audit;
- `ts audit ad-templates verify https://www.example.com/` dispatches to
  ad-template verification;
- `ts audit ad-templates` does not parse as a URL;
- `ts audit ftp://www.example.com/` fails before browser launch.

JSON error output is intentionally left to the broader CLI output contract. This
spec only standardizes successful verification result JSON and strict-mode
verification failure JSON where page results exist.
