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
group. The first observation remains tentatively accepted. When a second raw div
ID that describes a _different element_ normalizes to the same prefix, remove
the first slot, record the group as ambiguous, and suppress every later member.
Emit one diagnostic when the group first becomes ambiguous, naming the
normalized prefix and explaining that neither a single prefix nor volatile exact
IDs are safe. Tell the operator to expose distinct stable div IDs or prefixes in
publisher markup before configuring the placements.

Two raw IDs sharing a stem are not by themselves two elements. One element
re-rendered under a fresh framework token produces exactly that shape, and
absorbing it is what normalization is for: a React publisher reports
`ad-header-0-_R_3f_` from the server render and `ad-header-0-_r_0_` from the
client one, and refusing that pair would generate no slots at all. The two cases
are separated by comparing what the ephemeral markers did _not_ cover — the
marker spans are excised and the remaining parts compared, so identical
residues mean one element observed twice, while `-in_content-0` against
`-in_content-1` means two siblings and is refused.

The verdict is site-wide, not page-local. Article pages carry several in-content
units and refuse the shared prefix while a landing page carries one, so a
page-local refusal would let crawl sampling decide whether the ambiguous prefix
reaches the config. `DiscoveredSlots` therefore carries the refused stems,
`EvidenceTable` unions them across pages, and the slot iterator the writer reads
suppresses them regardless of which page contributed them.

Registry and request-derived evidence retain separate collision maps, matching
the current source precedence: even an ambiguous registry stem continues to
suppress request fallback for that stem. Network-ID discovery is unaffected.

`DiscoveredSlots` records whether any otherwise usable GPT slot evidence was
seen independently of how many safe slots remain. `EvidenceTable::fold_page`
uses that signal when classifying empty pages, so a collision-only page is not
mistaken for a bot challenge. Cross-page slot inference, merging, and
`--replace` otherwise remain unchanged because ambiguous slots never enter
those stages.

Some ad stacks build IDs as `<family>_<render token>_<placement>`, where the
render token — at least eight leading digits followed by more alphanumerics,
that is, a millisecond timestamp plus entropy — sits _before_ the part that
distinguishes one placement from the next. Such an ID can be written neither
literally nor as a prefix: the only stable prefix stops at the token and reaches
every placement in the family at once. Discovery refuses a single otherwise
usable registry or request observation of that shape, preserves the page/network
evidence, and emits one diagnostic naming the family prefix. The shape decides
rather than a vendor name, so any stack with this layout is covered without a
code change, and every placement after the token is covered rather than an
enumerated few. A token in trailing position is _not_ this case — everything
before it still identifies the element — and is left to normalization and the
collision check.

## Safety and Output

The generator prefers omission over a configuration that cannot match future
renders. For an observed desktop crawl of a site with this mix, replacement
output should therefore contain the stable `ad-header-0` and `ad-fixed_bottom-0`
slots, while the in-content collision group, the volatile-token family, and the
section-varying sidebar are explained in notes.

## Tests

- A two-element same-page normalization collision yields no slots and one
  diagnostic containing the prefix, both unsafe alternatives, and operator
  action.
- Two renders of one element (identical residues either side of the marker,
  including a React server/client pair) collapse to one slot with no diagnostic.
- Repeats of the first and second IDs plus a third distinct ID after a collision
  remain suppressed and do not create additional diagnostics.
- Request-derived collisions follow the same policy.
- An ambiguous registry stem still suppresses request fallback, and network-ID
  discovery survives when every collided slot is omitted.
- A stem refused on one page stays refused after a later page contributes a
  single member of the group.
- A collision-only page is recorded as having evidence rather than as an empty
  challenge page.
- Single registry- and request-derived render-token observations are omitted
  while retaining evidence and any parseable network ID, for every placement
  suffix after the token.
- IDs with no render token, with a bare digit run, or with a trailing token stay
  eligible.
- Existing normalization, request fallback, fragment detection, and full CLI
  tests remain green.
