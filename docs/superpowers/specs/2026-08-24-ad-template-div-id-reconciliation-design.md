# Ad-template div-ID reconciliation design

## Goal

Prevent `ts audit ad-templates generate` from losing numeric sibling creative
opportunities during merge or persisting a singleton div ID whose middle token
is demonstrably per-render.

This follows a live validation crawl. The crawl observed
`ad-sidebar-1`, `ad-sidebar-10`, and other siblings, but the merge treated the
configured literal `ad-sidebar-1` as a prefix and absorbed the longer IDs. It
also proposed one `vendor-tag_12345678AbCdEfGhIjKl_slot_overlay_1`-shaped slot
because the volatile-token classifier recognizes ten leading digits but this
token has eight.

## Scope

The change is limited to div-ID identity and volatility classification during
generation:

- Preserve every distinct normalized, usable div identity retained by the
  evidence table when an existing configured div ID was itself observed
  exactly.
- Preserve intentional configured prefix behavior when that prefix was not
  observed as a literal element ID.
- Refuse singleton IDs with a conservative eight-digit-plus-long-suffix token
  shape in a non-trailing segment.
- Keep the existing warning and refusal behavior for ambiguous and fragmented
  placements.

This does not implement the broader cross-page-type preservation requested by
GitHub issue #1059, change crawl planning, or change runtime slot resolution.

## Exact versus prefix reconciliation

The generator already carries the div identities from `EvidenceTable::slots()`
into the TOML merge. This is intentionally not collector-level raw DOM input:
the identities have passed per-page normalization and usability checks, while
slots later rejected by template inference or cross-page fragmentation remain
present. Page-local volatile and ambiguous identities already refused by GPT
discovery do not re-enter reconciliation.

The merge will classify a configured or newly appended slot as an observed
literal when its resolved div identity appears exactly in that normalized
evidence set.

Matching proceeds in this order:

1. Prefer an exact stable-key match.
2. Otherwise consider configured-prefix matches whose prefix was not observed
   as a literal normalized div identity during this crawl.
3. Choose the longest remaining prefix, retaining configuration order for
   equal-length ties.
4. Append the discovered slot when neither exact nor eligible prefix matching
   succeeds.

Consequently, `ad-sidebar-1` matches itself but cannot claim
`ad-sidebar-10`. A hand-authored broad prefix such as `ad-`, absent as a literal
DOM ID, retains its existing merge behavior. Newly appended discovered slots
are also protected because the decision is based on the normalized evidence
set, not only the original configuration indexes.

The same reconciliation rules will drive observed/unobserved diagnostics so a
slot cannot be merged one way and classified for staleness another way.

## Volatile token classification

The existing vendor-neutral classifier refuses a div ID when a non-trailing
segment contains a per-render token before the placement suffix. It currently
recognizes a segment with at least ten leading digits followed by alphanumerics.

Retain that rule and add a narrower alternative for shorter counters:

- at least eight leading ASCII digits; and
- at least eight trailing ASCII alphanumeric characters in the same segment.

The token must still occur before another div-ID segment. This catches the
`12345678AbCdEfGhIjKl` shape without claiming:

- bare numeric placement IDs;
- seven-digit counters with long suffixes;
- eight-digit values with fewer than eight trailing characters, including
  calendar-like `20260820a`; or
- trailing tokens whose preceding prefix can still identify the element.

The warning remains vendor-neutral and names the stable family prefix. The slot
continues to count as evidence of an ad stack but is not rendered into config.

## Diagnostics and failure behavior

No new command failure is introduced. Unsafe singleton volatile slots are
skipped with the existing volatile-family note. Literal numeric siblings are
written separately and no longer produce the broad-prefix collision note.
Truly intentional broad prefixes can still produce that note when they claim
multiple observed divs.

Normal merge continues to preserve configured slots. `--replace` retains its
existing replacement semantics.

## Testing

Use test-driven development with focused regressions:

- A merge containing configured `ad-sidebar-1` and normalized observations for
  `ad-sidebar-1`, `ad-sidebar-10`, and `ad-sidebar-11` must produce three slots.
- A configured `ad-` prefix that was not observed literally must continue to
  merge multiple matching discovered divs and emit its collision note.
- Newly appended observed literals must not absorb later numeric siblings.
- A framework-bearing DOM ID normalized to a stable stem must classify the
  matching configured stem as literal; identities refused during per-page GPT
  discovery must not be reintroduced solely for merge classification.
- Registry and request evidence containing a singleton shorter high-entropy token
  must be refused with the volatile-family warning.
- Boundary tests cover seven leading digits, eight digits with a seven-character
  suffix, eight digits with an eight-character suffix, bare digits, and the
  existing calendar-shaped example.
- Run the complete CLI suite, including the real-Chrome scrolling fixture, plus
  formatting and the repository's target-specific verification gates.
