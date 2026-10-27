# Publisher registration browser contracts

These tests use fictional inventory and intercepted transport. They do not contact
GPT, GAM, or bidders and do not establish production correctness.

## Run

Build the worktree's JS bundles first, then run:

```sh
node --test crates/trusted-server-integration-tests/browser/publisher-registration/pages.test.cjs
node crates/trusted-server-integration-tests/browser/publisher-registration/run.cjs \
  --implementation all --out /tmp/ts-publisher-registration-offline
```

`--implementation` accepts `runtime`, `bootstrap`, `handoff`, or `all`. The runner
builds a real external Rubicon/SharedId Prebid bundle unless `--prebid DIR` supplies
one. It verifies the supplied manifest's SHA-256 and required modules. Source
bundles are snapshotted before browser launch so another build cannot mix assets
within the run. Output defaults to a temporary directory outside Git.

The fixed contract is the default and fails if protocol version 1 is missing.
There is no allow-failures option. Runtime installs the actual GPT bundle;
bootstrap runs without that bundle or its renderer; handoff starts with bootstrap
and installs runtime across the waiting or consumed delivery boundary.

Cases cover both admission/registration orders, inline and shared-seam payload
shapes, missing and visible SSR placeholders, outside-prefix final IDs, two slots
sharing a GAM path, repeated registration, reordered and narrowed eligibility,
missing/incompatible/unknown-dimension bids, empty/fluid eligibility, explicit
no-fill, pending cancellation, consumed teardown, hidden eligibility replacement,
stale callbacks, disabled-load rejection and rechecking, automatic/registered map
writer order, empty automatic passes, publisher-first arbitration, and a later
refresh through real Prebid. Runtime/handoff also exercise actual SPA page-bids
transport, navigation cleanup and duplicate runtime installation. Handoff includes
ordinary bootstrap fill followed by late runtime with no candidate resurrection.
Real history push/popstate cases keep bootstrap-only A-to-B-to-A registrations
unadmitted, prove one late-runtime current-path catchup without replacing the
waiting handle or generation, and reject a forced late B response after C.
A failed catchup can retry through a same-path history replace with query/hash
changes while preserving the waiting handle, token, native slot and generation.
No-navigation runtime arrival must not fetch or request again.

Every successful initial delivery independently checks the exact native object,
exact final element, one display of its actual ID, one scoped refresh containing
only that object, request-time targeting, unrelated SRA isolation, and real iframe
creative text. Registered-only paths must make no service or policy calls.

## Old automatic controls

```sh
node crates/trusted-server-integration-tests/browser/publisher-registration/run.cjs \
  --baseline --implementation all --out /tmp/ts-publisher-registration-baseline
```

This explicit diagnostic mode uses enabled initial loading. It first requests and
renders an automatic placement, then replaces its whole subtree or renames its
existing element. The publisher offers identical, reordered, or narrowed formats.
Identical formats alias the old native slot and suppress the publisher display.
Reordered and narrowed formats define another native slot and request again.

For whole-subtree replacement, the identical-size alias has one request but no
creative in the final element. This is deliberately **not** a visible-delivery
pass. These old-mode controls omit destroy-before-removal to reproduce the
unsupported lifecycle, not to recommend it. A renamed element retaining its
children is recorded separately from replacement.

## Fixture boundary

`gpt-fixture.js` counts native definitions, displays, refreshes and destruction.
It does not inspect TS ownership, deduplicate requests, or synthesize admission.
Enabled-load SRA display fetches every unrelated unfetched slot. Disabled-load
display only registers; explicit selected refresh leaves unrelated slots alone.
Every native refresh invocation is recorded, but an undisplayed slot cannot request
through refresh. This models GPT registration rather than TS ownership.
Native object and element references remain accessible through `fixture.nativeSlots`
and `fixture.elements`; serialized request records carry their indices.

Install the fixture after optional `window.publisherFixtureOptions` configuration:

- `initialLoad`: `disabled`, `enabled`, or `unknown`. Unknown removes both getters.
- `deferCommands`: hold GPT callbacks until `fixture.flushCommands()`.

`fixture.render(index)` chooses the creative from targeting captured at the native
request. Candidate requests load real Universal Creative in a separate-origin
frame. Non-candidate requests load an ordinary publisher creative without the TS
bridge. `fixture.render(index, { noFill: true })` emits an explicit no-fill instead.
The runner asserts real iframe text inside the final element's frame tree, not
just a render event or postMessage marker. This includes the nested iframe created
by the real Universal Creative renderer.

Offline `pages.cjs` payloads are handwritten fictional inputs whose source is
recorded in evidence. They cannot substitute for the separate real React/Rust
provider framework gate. That suite may import this transport fixture but must
obtain slots and bids through actual Rust injection and provider admission.

## Evidence and limits

Every case retains a screenshot, Playwright trace, native requests, targeting,
creative facts, network calls, errors and elapsed time, including failures.
`artifacts.json` records commit, dirty inputs, browser/Node/Prebid/Universal Creative
versions, manifest and hashes of the exact snapshotted scripts.

Offline hidden-tab controls override visibility getters and dispatch visibility
notifications explicitly. Old callback and handle controls force stale work after
replacement. Frame barriers drain already queued delivery work; they do not claim
that a React tree is mounted or hydrated. Actual framework lifecycle is proved by
the separate React/Rust suite. The stale catchup response case strips B's fetch
abort signal only in its controlled transport, then delivers a real intercepted
response after C. This exercises the generation/controller rejection even when
network cancellation cannot stop an old response.

Real-GPT staging with matching deployed artifacts and a publisher adapter remains
a separate approval and deployment blocker. Local counts are not GAM network
batch counts, live fill guarantees, or a CWV measurement.
