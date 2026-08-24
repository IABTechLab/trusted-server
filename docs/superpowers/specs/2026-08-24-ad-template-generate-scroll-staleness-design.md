# Ad-template generation scroll and staleness diagnostics design

## Problem

`ts audit ad-templates generate` currently collects each page only after its
initial settle. Unlike `ts audit page` and `ts audit ad-templates verify`, it
cannot request the deterministic scroll pass that triggers lazy ad inventory.
On Autoblog this produced fewer observable frames than a scrolled page audit.

Generation also merges by default, deliberately preserving configured slots
that the current crawl did not rediscover. That safety behavior is correct, but
it is silent: stale slots look as though the latest crawl confirmed them.

## Scope

Add opt-in scrolling to `ts audit ad-templates generate` and report configured
slots that a merge preserved without observing during the current crawl.

This change does not prune slots automatically, enable scrolling by default,
alter crawl planning or budgets, change volatile-div refusal, or implement
GitHub issue #1059. `--replace` remains the only intentional pruning mode.

## Command behavior

`ts audit ad-templates generate` accepts a boolean `--scroll` option. Its
default is false, preserving current crawl cost and side effects. When enabled,
every page on every selected device profile performs the same deterministic
stepped scroll used by the existing page audit: scroll to 33%, 66%, and 100% of
the document, pause between steps, return to the top, then wait for the page to
settle again before reading HTML, GPT registry entries, and network evidence.

The browser collector carries the option as session configuration so root,
planned section, desktop, and mobile page loads all behave consistently. Scroll
evaluation failures are best-effort page warnings; they do not discard evidence
that was already available after the initial settle.

The implementation will share the deterministic scroll primitive with the
existing browser audit rather than maintain a second sequence of scroll steps.
Verifier-only evidence-phase bookkeeping remains in the verifier call path.

## Merge diagnostics

During a normal merge, generation tracks which pre-existing configured slots
matched at least one discovered slot. After processing all discovered slots, it
reports every unmatched pre-existing slot in configuration order. Those slots
remain unchanged in the output.

The diagnostic is explicit about the limits of negative crawl evidence. Its
human-readable form for a non-scrolling run is equivalent to:

```text
note: preserved 2 configured slot(s) not observed during this crawl: ad-header-0, ad-fixed_bottom-0. Re-run with broader coverage or --scroll; use --replace only to intentionally prune them.
```

When the current run already used `--scroll`, the follow-up omits that redundant
suggestion and recommends broader page/profile coverage before intentional
pruning.

No staleness diagnostic is emitted when all configured slots were rediscovered,
when there were no existing slots, or under `--replace`, because that mode does
not preserve unmatched slots. Matching uses the same reconciliation logic as
the merge itself, avoiding a second definition of slot identity.

Diagnostics go to stderr through the existing generation-note path. Stdout
remains limited to the dry-run diff or successful write summary, so redirection
and machine comparison remain stable.

## Safety and compatibility

The default command behavior, merge result, and generated TOML remain unchanged
unless `--scroll` discovers additional evidence. The warning never mutates or
deletes operator configuration. It names only configured slot IDs and does not
include cookies, URL credentials, query strings, or fragments.

Scrolling can trigger additional ad requests and publisher behavior, which is
why it remains explicit. Existing page-delay, settle-window, browser-proxy,
certificate, cookie, and device-profile behavior applies unchanged.

## Tests

CLI parsing tests cover `--scroll` and its false default. Browser-collector tests
use a deterministic local page that defines a GPT slot only after scrolling and
prove that generation captures it with the option enabled but not without it.
Existing browser lifecycle and settle tests continue to cover teardown and
timeouts.

Merge unit tests cover multiple unmatched configured slots, stable diagnostic
ordering, partial rediscovery, full rediscovery, an empty existing config, and
`--replace`. Command-level tests verify that the warning reaches stderr while
stdout and the preserved generated configuration retain their existing
contracts.

Verification will run the host CLI test suite and relevant Chrome-backed CLI
tests, followed by the repository-required formatting and CLI lint gates. A
manual dry run against Autoblog may be used when a fresh bot-protection cookie
and proxy are available, but network-dependent behavior is not a required CI
test.
