# Per-Section `gam_unit_path` Design

**Date:** 2026-07-23

**Status:** Proposed

**Issue:** [IABTechLab/trusted-server#954](https://github.com/IABTechLab/trusted-server/issues/954)

## Summary

`creative_opportunities.slot.gam_unit_path` is a static string, so a publisher
whose GAM ad unit varies by site section cannot express that in one rule. The
only way to model it today is one slot rule per (slot × section), which
multiplies out fast: 3 slots across 10 sections needs 30 near-identical rules.

This design makes `gam_unit_path` a **template** with a small, fixed placeholder
set — `{network_id}`, `{section}`, `{slot_id}` — where `{section}` is derived
from the request path at render time. One slot rule then covers all sections.

The derivation policy that matters (`{section}` for the site root) lives in
config via a required `section_root`, not in core — honoring the issue's
constraint that the URL→section convention is publisher-specific.

Scope is deliberately narrow: **only** `gam_unit_path` templating. Sharing
`page_patterns`/`gam_unit_path` defaults across slots is a related but distinct
duplication problem, tracked as a sibling issue, not built here.

## Goals

1. A publisher with N slots across M sections expresses per-section ad units
   without N×M rules.
2. `{section}` is derived from the request path with a config-supplied value for
   the site root; no URL convention is hardcoded in core.
3. Existing static `gam_unit_path` configs keep working, byte-for-byte
   unchanged.
4. Startup rejects unresolvable configuration: unknown placeholders, malformed
   templates, and a `{section}` template missing its `section_root`.
5. Resolution is covered by tests including `/`, single- and multi-segment
   paths, unsafe/encoded segments, and paths matching no slot.
6. Documented in `docs/guide/configuration.md`, which currently has no
   creative_opportunities section.

## Non-goals

Documented here so onboarding publishers know the boundary. Each is an additive
extension that does **not** change the config shape below.

1. **Locale offset** — deriving `{section}` from a segment other than the first
   (e.g. `/en/news` → `news`). `{section}` is the first path segment. A
   `section_segment` index knob can be added later.
2. **Full-path mirror** — `{section}` spanning multiple segments (`/a/b` →
   `a/b`). Real GAM trees bucket by section, not per-article, so this is rare;
   use a static per-slot `gam_unit_path` for the exception.
3. **Named per-section overrides** — mapping an irregular section to a renamed
   unit (`/reviews` → `editorial/reviews-v2`). Set that one slot's
   `gam_unit_path` explicitly, or add named overrides later.
4. **Host- or query-derived sections** — path-only. Out of scope entirely.
5. **Slot-defaults inheritance** — sharing `page_patterns`/`gam_unit_path` at
   the `[creative_opportunities]` level. Separate issue; has a startup-lifecycle
   concern this design intentionally avoids.

## Background: how `gam_unit_path` is used

- `gam_unit_path` is **client-side only**. The resolved string reaches
  `googletag.defineSlot(path, sizes, div)` in
  `crates/trusted-server-js/lib/src/integrations/gpt/index.ts`. It is **not** in
  the OpenRTB bid request — `CreativeOpportunitySlot::to_ad_slot` never emits it.
  Therefore this change is **server-only; no JS wire change**. The client keeps
  receiving a resolved `gam_unit_path` string.
- Today's resolver is literal-or-default and path-independent
  (`crates/trusted-server-core/src/creative_opportunities.rs`):

  ```rust
  pub fn resolved_gam_unit_path(&self, gam_network_id: &str) -> String {
      self.gam_unit_path
          .clone()
          .unwrap_or_else(|| format!("/{}/{}", gam_network_id, self.id))
  }
  ```

- The value is emitted in `build_slot_json`
  (`crates/trusted-server-core/src/publisher.rs`), shared by two paths:
  - initial render via `build_ad_slots_script` (called where `request_path` is
    in scope);
  - SPA navigation via `handle_page_bids` (has the normalized `path` param).

  Neither currently passes the path into `build_slot_json`.

## Design

### Config shape

```toml
[creative_opportunities]
gam_network_id = "88059007"
auction_timeout_ms = 2000
price_granularity = "dense"
section_root = "homepage"          # required when a template uses {section}

[[creative_opportunities.slot]]
id = "ad-header-0"
gam_unit_path = "/{network_id}/autoblog/{section}"
page_patterns = ["/", "/news/*", "/reviews/*", "/deals/*"]
formats = [{ width = 970, height = 90 }, { width = 728, height = 90 }]
[creative_opportunities.slot.providers.prebid]
bidders = {}
```

### Placeholders

| placeholder    | resolves to                                           |
| -------------- | ----------------------------------------------------- |
| `{network_id}` | `gam_network_id`                                      |
| `{slot_id}`    | slot `id`                                             |
| `{section}`    | first path segment; `section_root` when path has none |

### Resolution model

```text
startup (prepare_runtime, once):
  for each slot:
    parse slot.gam_unit_path (if Some) into a template:
      reject unknown placeholder, unmatched/nested brace, empty template
    cache the parsed template (serde-skipped, like compiled_patterns)
  if any slot's template contains {section}:
    require section_root present AND matching ^[A-Za-z0-9_-]+$

request (per matched slot, path known):
  if slot has a parsed template:
    section = first non-empty segment of the RAW path,
              runs of [^A-Za-z0-9_-] replaced with a single '_';
              section_root when the path has no segment ("/", repeated slashes)
    render template
  else:
    "/{network_id}/{slot_id}"          # existing default (back-compat)
```

### Section derivation rules (deterministic)

- Extract the **first non-empty** path segment.
- Replace each run of disallowed characters (`[^A-Za-z0-9_-]`) with a single
  `_`. Guarantees a non-empty result for any non-empty segment. Because the path
  is **not** decoded, `new%20s` → `new_20s` (only `%` is disallowed; `2` and `0`
  are alphanumeric) — never silently `news`, and never the decoded `new_s`.
- Use `section_root` **only** when there is no segment (`/`, repeated slashes).
- Derive from the **raw, undecoded** path — the same string `page_patterns`
  glob-match against — so matching and derivation never disagree. Percent-encoded
  segments are **not** decoded.
- `section_root` validated at startup: non-empty, entirely `[A-Za-z0-9_-]`.

### Back-compat

- No template placeholders in a slot's `gam_unit_path` → used verbatim.
- No `gam_unit_path` set on a slot → `/{network_id}/{slot_id}` (unchanged).
- A config with no `{section}` anywhere never requires `section_root`.

### Validation moves from render to parse

`validate_runtime` currently calls `resolved_gam_unit_path` and rejects an empty
result. That check becomes path-dependent under templating, so it is replaced by
**startup template validation**: the template parses, all placeholders are
known, and `section_root` is present when `{section}` is used. The rendered
result is non-empty by construction (literals plus non-empty substitutions, or
the `/{network_id}/{slot_id}` default), so no per-request emptiness check is
needed.

## Alternatives considered

- **Named sections** (`[section.NAME]` blocks carrying patterns + unit): more
  general (expresses irregular units) but forces enumerating every section, and
  centralizes patterns — a bigger change that overlaps the deferred
  slot-defaults concern. Rejected as the base; the `unit`-override variant is a
  possible future extension.
- **Explicit `unit_by_pattern` map per slot** (issue option 2): fully
  data-driven but repeats the section→unit table inside every slot, so adding a
  section still edits all N slots. Rejected.
- **Hardcoded first-segment derivation** (issue option 3, literal): smallest,
  but bakes one site's URL convention into core, which the issue forbids. The
  chosen design keeps the one publisher-specific knob (`section_root`) in config.

## Risks

- **Client-influenced path.** `{section}` is derived from a request path the
  client controls (especially the SPA `path` param). Mitigated by: sanitizing to
  `[A-Za-z0-9_-]`; deriving only for paths that already matched a slot's
  `page_patterns`; and the fact that `gam_unit_path` is not in the bid request,
  so a crafted section only affects the caller's own `defineSlot`.
- **Two render paths drift.** Initial-render and SPA must produce identical
  units for the same path. Covered by an equivalence test.

## Acceptance criteria

- [ ] N slots × M sections without N×M rules.
- [ ] Resolution tested: `/`, single-segment, multi-segment, no-match, encoded
      segment.
- [ ] Existing static `gam_unit_path` configs unchanged.
- [ ] `validate()` (startup) catches empty/unknown/malformed template and a
      `{section}` template with missing/invalid `section_root`.
- [ ] `{section}` sanitized to `[A-Za-z0-9_-]`, derived from the raw path.
- [ ] Documented in `docs/guide/configuration.md`, including unmatched-route
      behavior and the no-decode rule; example and live autoblog configs updated.

## Sibling issue (not built here)

"creative_opportunities: support shared slot defaults for `page_patterns` and
`gam_unit_path`." Inheritance of `page_patterns` must materialize onto each slot
at startup **before** `compile_slots()` (because `match_slots` never sees the
top-level config), which is the lifecycle subtlety this scoped design avoids.
