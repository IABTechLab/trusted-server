# #1009 measurement findings

Recorded output of the checks in
[the plan](./2026-08-08-1009-measurement-and-stage-0.md). Results only — the origin
hostname is operator config and is deliberately not reproduced here.

## Step A — origin `Vary` declaration and cookie exposure

**Date:** 2026-08-08 · **Method:** direct `curl` against the publisher origin with the
configured `origin_host_header_override`, homepage path.

### Representation split

| Representation                       | `Content-Type`     | `Cache-Control` | `Set-Cookie` |
| ------------------------------------ | ------------------ | --------------- | ------------ |
| HTML navigation                      | `text/html`        | `max-age=60`    | none         |
| `RSC: 1`                             | `text/x-component` | `max-age=60`    | none         |
| `RSC: 1` + `Next-Router-Prefetch: 1` | `text/x-component` | `max-age=60`    | none         |

`Vary`, identical on every response:

```
vary: rsc, next-router-state-tree, next-router-prefetch, next-router-segment-prefetch, Accept-Encoding
```

The origin declares **every** header that distinguishes the representations, including
`next-router-segment-prefetch`, which the plan's probe list did not think to check. The
HTML/RSC split at one URL is real and correctly declared.

### Cookie personalization

Hash comparison was useless here and the plan's probe as written would have produced a
false FAIL — see the method note below. After normalizing per-request identifiers:

| Comparison                     | Differing lines |
| ------------------------------ | --------------- |
| no-cookie A vs no-cookie B     | 2               |
| no-cookie A vs **with cookie** | 2               |

Both diffs are the same single `generationTimestamp` field in the RSC payload. **The
cookie changes nothing.** Byte lengths were identical across all three responses
(1,432,944).

Cookie sent: `ts-tester=true; sessionid=abc123; ts-ec=probe`.

### Verdict: **PROVISIONAL PASS** — not sufficient to gate a production flip

Downgraded 2026-08-10 after external review. Everything below held under the conditions
tested; the conditions tested are narrower than the gate requires.

What passed:

- `Vary` names every request header the origin varies on. ✅
- Bodies did not differ by the cookie sent, so `Vary: Cookie` was not required **for
  that cookie**. ✅
- No `Set-Cookie` on a shared-cacheable response. ✅
- Origin returns 200 without credentials at this layer. ✅

**What was not tested, and each of these can flip the verdict:**

| Gap                                      | Why it matters                                                                                                                                                    |
| ---------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `sessionid=abc123` is not a real session | A synthetic value proves nothing about a state-bearing publisher session. An authenticated or paywall-metered session is exactly the case that would personalize. |
| One route (homepage) only                | Article, section, and search routes may personalize differently.                                                                                                  |
| Experiment variant never exercised       | #1009 says the origin varies on one. It is absent from `Vary` — see Residual uncertainty below.                                                                   |
| Basic Auth through TS untested           | #1009 describes a gated deployment. Only the origin was probed directly.                                                                                          |
| Cached-hit slot resolution untested      | The randomized div IDs below are an unverified interaction, not a cleared one.                                                                                    |

**Consequence:** Stage 0 still takes the operator-flag path rather than the cache-key
discriminator, and no live cross-serving defect is indicated. But this is **not** a
release gate. Close the table above before flipping the flag in production.

## Two findings the checks were not looking for

### 1. The origin already intends this page to be shared-cached

`cache-control: max-age=60` with a correct `Vary` and no `Set-Cookie`. The origin has
been cacheable all along; Trusted Server opted out of it. That is the spec's §4 framing
confirmed from the other side, and it strengthens the case that the bypass was
belt-and-braces rather than load-bearing.

It also bounds the win: a 60-second TTL means Stage 0 buys a cache hit only within that
window. Whether that translates into a meaningful hit rate depends on request volume per
URL, which is not measured here.

### 2. Ad-slot div IDs are randomized per request — and this interacts with Stage 0

The only per-request variance in the document is ~170 lines of ad-slot container IDs,
each a fresh 32-hex UUID:

```
ad-in_content-f75fa7fba54a4fc2a2d787f51c1837dd-in_content-0
ad-in_content-a968b27e3ee2424f8bb1c19560abf2b1-in_content-0   ← same slot, next request
```

Under the bypass, Trusted Server sees fresh IDs on every request. **Once the cache is on,
every visitor within a 60-second window receives the same IDs.**

This is very likely fine — `tsjs.adSlots` is built from configured slot definitions, not
scraped from origin markup, and injection is a prefix match on the configured `div_id`.
But it is an untested interaction between Stage 0 and the slot-matching path, and it was
not in anyone's risk list. **This is a release gate, not a note.** Verify slot matching resolves against a cached
document before flipping the flag, and watch TS-attributed renders across the flip
rather than only `origin_fetch_ms`.

## Method note — a defect in the plan's Step A probe

The plan's cookie check compares `shasum` of the response bodies. On this origin that
test always fails, cookie or not, because of the randomized div IDs above. Three requests
produced three different hashes with byte-identical lengths.

**Correct method:** normalize per-request identifiers before comparing, e.g.
`sed -E 's/[0-9a-f]{32}/UUID/g'`, and diff the normalized bodies rather than hashing
them. Establish the no-cookie baseline drift first, then compare the cookie arm against
that baseline — a cookie arm is only interesting if it differs by _more_ than the
baseline does. Fix the plan before anyone re-runs this.

## Residual uncertainty

#1009 states the origin varies on an experiment header as well as `rsc` and
`next-router-*`. **No experiment header appears in the origin's `Vary` list**, and the
RSC payload's `experiments` key did not differ across any of the requests made here.

Three readings, unresolved: the issue was imprecise; experiments are assigned
client-side; or they key on a cookie value this probe did not supply. The `Vary`
declaration is authoritative for cache correctness and it is thorough enough to name four
Next-specific headers, so this is unlikely to be a cache-safety gap. Worth one question
to whoever wrote that line in #1009 rather than further probing.

## Rollback caveat, added 2026-08-10

The plan described flipping the flag back as a seconds-long rollback. That is
incomplete. Re-enabling the bypass stops **HTML navigations** reading from cache; it
evicts nothing. Objects already cached — including those RSC and other request classes
continue to read — persist until they expire.

Two mitigations, both real:

- The origin's `max-age=60` bounds read-through exposure to roughly a minute.
- Purge exists in-process — `fastly::http::purge::purge_surrogate_key`. An earlier claim
  that TS had no purge capability was wrong; it has no _wiring_, which is buildable.

**But note which cache.** `InsertBuilder::surrogate_keys` belongs to the **Core Cache**
API and applies to the transformed-template cache the ESI spike would build (C2). It has
**no effect on the HTTP read-through cache** that Stage 0 turns on (C1). Purging C1 needs
surrogate keys the _origin_ supplies on its responses, or the HTTP cache's own
request/candidate surrogate-key surface. Confirm which is available before relying on it —
an earlier revision of this document conflated the two.

Rollback is therefore: flip the flag, **then** purge C1 by whichever mechanism is actually
available (or roll a versioned key namespace), **then** observe past the origin TTL before
declaring the incident closed. With no C1 purge path wired, the tail is the TTL itself —
roughly a minute here, and a recorded risk rather than a surprise.

## ESI spike Task 1 — does `esi` 0.7 build on this toolchain?

**Date:** 2026-08-10 · **Verdict: PASS.** The cheapest falsifier for the ESI question
clears. #1009 is not closed by a toolchain limit.

| Check                                                                                      | Result               |
| ------------------------------------------------------------------------------------------ | -------------------- |
| `cargo add esi@0.7 --package trusted-server-adapter-fastly`                                | resolved `esi 0.7.1` |
| `cargo check-fastly` (Rust 1.95.0 / `wasm32-wasip1`)                                       | clean                |
| `cargo fmt --all -- --check`                                                               | clean                |
| All six clippy targets (fastly, axum, cloudflare, cloudflare-wasm, spin-native, spin-wasm) | clean                |
| `cargo check --manifest-path crates/trusted-server-integration-tests/Cargo.toml --tests`   | clean                |

**Nine new transitive dependencies:** `esi 0.7.1`, `nom 8.0.0`, `rand 0.10.2`,
`rand_core 0.10.1`, `chacha20 0.10.1`, `cpufeatures 0.3.0`, `atoi 2.0.0`,
`html-escape 0.2.15`, `md5 0.8.1`.

**No existing shared dependency moved.** `regex` stays 1.12.4, `bytes` 1.12.0, `log`
0.4.33. `nom` and `rand` gain new majors that coexist with the existing 7.1.3 / 0.8.6 /
0.9.4 rather than replacing them — the best available outcome, since a forced bump on a
shared dep is what would have made this expensive.

### A claim in the spike plan was wrong

Task 1 Step 3 told the implementer to check for a desync between the root `Cargo.lock` and
`crates/trusted-server-integration-tests/Cargo.lock`. **That second lockfile does not
exist.** The integration-tests crate is a workspace member (root `Cargo.toml:10`) and
shares the root lockfile, so the desync hazard cannot arise in that form. The plan has
been corrected. The dual-lockfile constraint was real at some earlier point; it is not the
current layout.

### Viceroy 0.17 supports the whole Core Cache surface this spike needs

**Date:** 2026-08-10 · **Verdict: PASS.** Probed directly under
`cargo test -p trusted-server-adapter-fastly --target wasm32-wasip1`, then removed:

| API                                                                      | Result |
| ------------------------------------------------------------------------ | ------ |
| `cache::core::insert(key, ttl).execute()` → write → `finish()`           | works  |
| `cache::core::lookup(key).execute()` → `Found::to_stream()`              | works  |
| `Transaction::lookup(key).execute()` → `must_insert_or_update()`         | works  |
| `Transaction::insert(ttl).surrogate_keys([…]).execute_and_stream_back()` | works  |
| Second transactional lookup reports a hit, no obligation                 | works  |

That is the entire API surface the spike's Task 3 Step 4 specifies, including the
transaction and stream-back shapes.

**Consequence: provisioning a Fastly service is not a prerequisite.** An earlier revision
of the spike plan made it Task 2 and a blocker on everything downstream. Almost all of the
correctness and safety work — the C2 cache logic, the transform, template byte-identity,
ESI assembly, DCA and dispatcher refusal, fragment-failure degradation, header ordering,
and the leakage gates — runs locally. The plan is re-sequenced accordingly.

**What still needs a real service:** shielding behaviour, POP-level cache tiering,
request collapsing under genuine concurrency (Viceroy is a single instance, so a passing
`Transaction` test proves the API works and not that collapsing is correct under load),
stale revalidation timing, and **every performance number in the decision rule**. Local
timings are meaningless for the decision.

### Not yet verified

Compiling and a cache round-trip are not an implementation. Nothing yet exercises the
`lol_html` transform into C2, ESI assembly, or any runtime behaviour on the publisher path,
and the `esi` dependency is added but unused.

## ESI spike Task 3 — implementation progress

**Date:** 2026-08-10. All of it behaviour-neutral under the default
`AssemblyMode::Inline`; nothing here changes a shipped code path.

| Step                             | State                                                               |
| -------------------------------- | ------------------------------------------------------------------- |
| 1 — `AssemblyMode` setting       | **Done.** `Option<AssemblyMode>` on `CreativeOpportunitiesConfig`.  |
| 2 — head-seam neutrality gate    | **Done.** `template_ad_slots_script`, three byte-identity tests.    |
| 2b — body-close decoupling       | **Done.** `BodyCloseInjection`, `body_close_injection`.             |
| 2c — emit the marker under `Esi` | **Not done.** Blocked on the fragment endpoint; see below.          |
| 3 — C2 eligibility gate          | **Done.** `c2_bypass_reason`, eight tests. Logs only, no cache I/O. |
| 4 — C2 cache read/write          | **Not started.** Design choice open; see below.                     |

### What is deliberately absent

**No marker is emitted under `Esi`.** The marker must point at a fragment endpoint
returning an **executable script**. `/_ts/page-bids` returns JSON
(`publisher.rs`, `handle_page_bids`) and ESI splices fragment bytes verbatim, so aiming
at it would put raw JSON where a script belongs. That endpoint does not exist, and a
marker with nothing behind it is worse than no marker. A test pins the current answer so
it changes deliberately.

**No cache read or write.** `c2_bypass_reason` has a real call site that logs its verdict,
which makes the decision observable during the spike without mutating anything. Task 3
Step 4 is blocked on choosing between read-through-with-body-transform and explicit
`cache::core` — the plan names that as a decision to make before writing code, and it is
under investigation rather than assumed.

### A defect this work introduced and then caught

Gating the head seam on neutrality made `ad_slots_script` `None` under the shared modes.
The body-close element handler read exactly that value to decide whether to inject at all,
so shared modes silently stopped injecting anything at `</body>` — a side effect of a
`<head>` change. Safe, since emitting nothing cannot leak, but wrong in the way the spec
warns about: the gate has to be "did this response carry bids", not "does this page have
slots".

Found by reading the handler while starting the next step, not by a failing test. Fixed by
replacing the inference with a named decision. The test that now guards it asserts
body-close is identical whether or not the head script is present — a decision that read
the head script would be _accidentally_ correct today, because that script is always
absent under shared modes, and wrong the moment that changes.

Worth recording because it is the same shape as the bug the whole task exists to prevent:
something that looks correct and quietly does nothing.

### Coverage and its limits

Fourteen new tests. `fmt`, all six clippy targets, and all four adapter suites pass, with
1850 core tests under Viceroy. `clippy --all-targets` caught a benchmark construction site
that all four test suites missed — the suites are not the whole gate.

**The neutrality guarantee is narrower than it looks.** The tests prove `tsjs.adSlots` is
neutral. They say nothing about the other things injected at the same seam — integration
`head_inserts`, the gpt-diagnostics bootstrap, the RSC placeholder rewriter — which the
spec flags as needing an audit and which that audit has not yet covered. Until it does,
treat request-neutrality as asserted for one element rather than established for the
template.

## Step B — consumers of TS's own response headers

Not yet run.

## Step C — hold and origin fetch timings

Not yet run.
