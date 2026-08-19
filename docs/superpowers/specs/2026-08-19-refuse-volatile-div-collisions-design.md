# Refuse Volatile Div-ID Collisions

## Problem

GPT discovery normalizes per-render div IDs such as
`ad-in_content-<hash>-in_content-0` to the stable prefix `ad-in_content`.
When several live elements on the same page normalize to that prefix, the
runtime cannot represent them safely: one prefix resolves at most one element,
while each exact raw ID changes on a later render. The current collision path
preserves the raw IDs, causing `--replace` to write unusable literal slots.

## Design

Treat a source-local normalized collision as ambiguous and refuse the entire
group. The first observation remains tentatively accepted. When a second
distinct raw div ID normalizes to the same prefix, remove the first slot, record
the group as ambiguous, and suppress every later member. Emit one diagnostic
when the group first becomes ambiguous, naming the normalized prefix and
explaining that neither a single prefix nor volatile exact IDs are safe. Tell
the operator to expose distinct stable div IDs or prefixes in publisher markup
before configuring the placements.

Registry and request-derived evidence retain separate collision maps, matching
the current source precedence: even an ambiguous registry stem continues to
suppress request fallback for that stem. Network-ID discovery is unaffected.

`DiscoveredSlots` records whether any otherwise usable GPT slot evidence was
seen independently of how many safe slots remain. `EvidenceTable::fold_page`
uses that signal when classifying empty pages, so a collision-only page is not
mistaken for a bot challenge. Cross-page slot inference, merging, and
`--replace` otherwise remain unchanged because ambiguous slots never enter
those stages.

The `rh-gam-kso_<render-token>_ei_<placement>` family is independently known
to be volatile across consecutive crawls. Its render token begins with at least
eight digits and continues with mixed alphanumeric entropy. Discovery refuses
even a single otherwise usable registry or request observation of this narrow
family, preserves the page/network evidence, and emits one site-wide diagnostic.
Arbitrary IDs that merely begin with `rh-gam-kso` do not match this rule.

## Safety and Output

The generator prefers omission over a configuration that cannot match future
renders. For the observed Autoblog desktop crawl, replacement output should
therefore contain the stable `ad-header-0` and `ad-fixed_bottom-0` slots, while
the in-content collision group, known `rh-gam-kso` family, and section-varying
sidebar are explained in notes.

## Tests

- A two-element same-page normalization collision yields no slots and one
  diagnostic containing the prefix, both unsafe alternatives, and operator
  action.
- Repeats of the first and second IDs plus a third distinct ID after a collision
  remain suppressed and do not create additional diagnostics.
- Request-derived collisions follow the same policy.
- An ambiguous registry stem still suppresses request fallback, and network-ID
  discovery survives when every collided slot is omitted.
- A collision-only page is recorded as having evidence rather than as an empty
  challenge page.
- Single registry- and request-derived `rh-gam-kso` render-token observations
  are omitted while retaining evidence and any parseable network ID.
- Stable/nonmatching IDs sharing only the vendor prefix are not omitted.
- Existing normalization, request fallback, fragment detection, and full CLI
  tests remain green.
