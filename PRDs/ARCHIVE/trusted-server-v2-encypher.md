# Trusted Server v2: Encypher provenance as a module

## Status

Internal completion review cleared at bar 9.5 in cycle 2. Review corrections
remain local on `encypher/v2-provenance-module`, based on
`feature/55-scripts-in-crates` at `5be114773`.
The existing draft https://github.com/IABTechLab/trusted-server/pull/1448
still contains the earlier published revision, not these corrections.
Successful live API signing remains unverified; in-process publisher signing
passes against a controlled API. Clearance covers the implementation contract,
not deployment admission or live demo readiness.

## Current Goal

Internal review complete. Preserve the publication hold and hand off the local
corrections and separate live-signing verification prerequisite. No landing,
deployment, comment or email was performed during this review.

## Review Loop State

bar: 9.5 max-cycles: 5 worktree: /home/agentdesk/code/trusted-server-worktrees/v2-provenance
resumed at: Phase 5 (finalization sweeps + completion gate B)
plan gate: NOT PERFORMED. Gate A was never run on this PRD. It is not
retroactively cleared and must not be recorded as cleared.
completion gate: CLEARED (cycle 2).
Cycle 1 NOT CLEARED:
Scores (GPT-5.5 / Opus): correctness 9.0 / 9.1; simplification 9.0 / 9.0;
security 10.0 / 9.6. Both reviewers accepted all 38 CI commands passing in
run `1792869014289`. Medium findings were stale Axum persistence wording
and whole-article canonical/fragment allocation beyond the signed prefix.
Cycle 2 CLEARED: correctness 10.0 / 9.5; simplification 10.0 / 9.6;
security 10.0 / 9.6. Every verdict is pass. Final full gate `1792871494395`
passed all 38 commands; final rebuilt Axum runtime smoke passed the exercised
fail-open and private-response exclusion scenarios. No high or medium finding
remains. Opus retains the live-signing evidence gap and accepted two-pass
boundary/metadata separation as low findings.

Publication hold: no pushes, remote PR changes, comments, email or deployments.
All review corrections remain local; no landing requested.

Independent pre-score assessments: `openai-think` (GPT-6 Astra) and
`anthropic-think` (Claude Fable 5) completed and identified the initial corrections.
The local finalization corrections now cover raw-markup boundary selection,
entity decoding, output expansion, negative-cache overwrite races, invalid
cache recovery, adapter receive-time response limits and credential-bound
HTTP cache bypass. The matching server contract also confirms that equal
canonical text can reuse the first URL-bearing manifest across URL or title
changes. Final pre-score review additionally corrected raw-text scan complexity,
full-article insertion-offset allocation and canonical ignored custom-element
prefix handling. Full local gates `1792869014289` and `1792870296283` passed all
38 commands. Cycle 2 additionally removes full-fragment materialization,
repeated metadata scans and the second rendered-document allocation. Its
93 module tests and module clippy pass, as does the final 38-command full gate.

## Overview

Encypher's text provenance contribution signed article pages from inside core,
through an HTML stream processor factory, a request context type and core
changes of its own. The stack in #777 moves every vendor out of core and lets a
module change a page only through a middleware. This work moves the
contribution onto those interfaces as `crates/provenance/encypher`
(`provenance.encypher`) and adds the neutral core seams it needs. Design:
`docs/superpowers/specs/2026-10-24-encypher-provenance-module-design.md`.

## Objectives

1. Port every existing Encypher behavior (gates, policy and plan caches, sign
   call, render, fail-open, logging) to a vendor-owned module crate.
2. Add only vendor-neutral core seams: an awaited asynchronous finish for
   middleware stream processors, an optional platform timer, response facts
   for middleware, runtime KV on the adapters that lacked it.
3. Remove obsolete paths in the port: the `enabled` flag, `path_prefixes`,
   `[integrations.encypher]`, the stream processor factory, the shared-template
   refusal and the synchronous publisher body path.
4. Keep the permission contract: no permission required, no gated value read,
   no page the origin marks private sent.
5. Publish a reviewable pull request against the stack head without changing
   any upstream branch.

## Work Breakdown

Implementation (completed; evidence historical, see Completion Notes):

1.  [x] Inspect epic #777, PR #1423 head and the existing Encypher branches
2.  [x] Create an isolated worktree and branch from 5be114773
3.  [x] `PlatformTimer` on `RuntimeServices`, with neutral documentation
4.  [x] Fastly opens the KV store linked as `trusted_server_kv`
5.  [x] `StreamProcessor::async_finish` and `finish`, composed in order by
        every composite processor
6.  [x] Every awaiting publisher path ends its processor with `finish`; the
        synchronous `stream_publisher_body` path removed
7.  [x] `MiddlewareContext::status` and `origin_private`, read before core's
        cache policy
8.  [x] Axum runtime KV from `TRUSTED_SERVER_KV_PATH`, on the blocking pool
9.  [x] Timers on Cloudflare, Spin and Axum
10. [x] `crates/provenance/encypher`: config, secret setting, middleware,
        ported processor and tests, publisher-path tests
11. [x] Module offered on all four adapters and the CLI
12. [x] Auction-hold regression test with an asynchronously finished document
13. [x] Documentation: configuration guide, Axum guide, example config, design
14. [x] Data-handling wording narrowed to what the gates prove
15. [x] Repository CI gates run locally once (historical; some adapter aliases
        ran before the later rustfmt-only change and the auction-hold test)
16. [x] Axum runtime smoke with a controlled origin (fail-open and
        origin-private paths only; the live signing endpoint was unavailable, so
        live signing was not exercised)
17. [x] Draft pull request opened and its body updated with evidence

Review loop, Phase 5 (complete; Gate B cleared at 9.5):

18. [x] PRD finalization for the review loop (historical evidence retained;
        premature completion removed; local Prettier check passed)
19. [x] Concise documentation sweep; timeout, retention and Axum persistence
        wording corrected, with module deployment release notes
20. [x] Simplification sweep (behavior-preserving, covering tests named)
21. [x] Security sweep: entry-to-sink, fail-open, private and tenant cache
        boundaries, finish ordering and cancellation, timers and budgets,
        credential treatment, config and runtime registration, cache integrity
22. [x] Fresh verification: final full gate `1792871494395`, all 38 commands
        passed, including 93 module tests and 3,022 core tests. Rebuilt Axum
        runtime smoke verified fail-open and private/Set-Cookie exclusion
23. [x] Completion gate (Gate B) cleared at bar 9.5 by both reviewers on
        correctness, simplification and security; cycle 2 scores above

## Success Criteria

| Criterion                                 | Result                                                                                             |
| ----------------------------------------- | -------------------------------------------------------------------------------------------------- |
| Signing works through the publisher path  | Met in process: buffered and streaming finalizers sign against a stub origin and stub API          |
| Every failure passes the page through     | Signed-output expansion regression passes for fresh and cached plans; original held bytes retained |
| A private origin page is never sent       | Met: in-process test and runtime smoke `gate=origin_private`                                       |
| Core platform-name neutrality lint passes | Met: `scripts/lint-core-platform-names.py` exit 0                                                  |
| CI gates green                            | Final full run `1792871494395`: all 38 commands passed                                             |
| Live API signing                          | Not met: not exercised, see limits                                                                 |

The internal completion gate cleared. Live signing remains a separate
deployment prerequisite rather than an inferred result of these checks.
In-process signing against the stub API does not prove interoperability.
The matching policy-digest implementation was located in the original commercial
`trusted-server-provenance` worktree at `bfe8f050` (policy change `832ded4b`).
The deployed API revision and current signing admission remain unverified.

## Completion Notes

Tests were ported from the earlier Encypher branches and written alongside the
port. The work was not strictly test-first, and no baseline run of the stack
was recorded before the change.

The command runs below are historical, from the implementation work. Fresh
review evidence is recorded separately under Current review verification.

Commands run in the worktree, with exit codes:

- `cargo fmt --all -- --check`: 0
- `cargo clippy-fastly`, `clippy-axum`, `clippy-cloudflare`,
  `clippy-cloudflare-wasm`, `clippy-spin-native`, `clippy-spin-wasm`,
  `clippy-cli`, `clippy-codegen`: 0 each
- `cargo test -p trusted-server-provenance-encypher`: 65 passed
- `cargo test -p trusted-server-core --lib`: 3020 passed; the auction-hold
  regression test added after it: 1 passed
- `cargo test-fastly`, `test-axum`, `test-cloudflare`, `test-spin`: no
  failures (run before the rustfmt-only change and the auction-hold test)
- `cargo check-cloudflare`, `check-spin`, `build-fastly`: no errors
- `cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity`: 0
- `scripts/lint-wasm-clock.sh`: 0
- `python3 scripts/lint-core-platform-names.py`: 0
- `cd crates/trusted-server-js/lib && npx vitest run`: 0; `npm run format`: 0
- `cd docs && npm run format`: 0; prettier check of markdown outside `docs/`: 0
- `cargo test -p trusted-server-core --test template_build_digest`: 0; its
  clippy with `-D warnings`: 0
- docs-proposal: `uvx --from shellcheck-py shellcheck scripts/docs-proposal/*.sh`
  (shellcheck is not installed on the host): 0;
  `node --test scripts/docs-proposal/hunks.test.mjs`: 0;
  `scripts/docs-proposal/test.sh`: 0

Runtime smoke, Axum dev server, throwaway script and mock origin, no real
credentials. The module was selected with a `[[serve]]` entry on `/news/`,
runtime KV on, and `[geo] assume_single_jurisdiction = true`, which the stack
requires once `[ec] module` is set. Observed:

```text
[edgezero] config validate (typed): edgezero.toml + trusted-server.toml OK (strict)
page=public status=200 harbor_text=1 marker=0
page=private status=200 harbor_text=1 marker=0
INFO  [trusted_server_provenance_encypher] encypher: registered for domain=publisher.example.com
INFO  [trusted_server_provenance_encypher::processor] encypher: outcome=passed_through reason=http_error
DEBUG [trusted_server_provenance_encypher] encypher: document not held, gate=origin_private
```

`scripts/smoke-axum.sh` itself does not set the `[geo]` value and stops at
that check. This branch leaves `scripts/` and the core settings, config and
Edge Cookie code unchanged from 5be114773; the script was not run on the base
commit.

Limits:

- Live API: the runtime smoke pointed the module at a signing endpoint that
  was not available, to exercise fail-open. Successful signing was tested in
  process only, against a stub API. The live Encypher API was not exercised.
- Personalized pages: the module skips pages whose origin set a cookie or
  forbade shared caching, and cannot detect others. A deployment places it on
  public article routes.
- Spin's declared KV store also holds the settings. The module's four
  positive and negative namespaces are versioned, but Spin has no item TTL.
  Expired entries fail closed on read and are deleted best effort; abandoned
  content keys require the namespace-retirement procedure in the Spin guide.
- Fastly has no guest timer. Its backend timeout bounds the first byte and
  between-byte gaps, not total elapsed time, so `max_hold_ms` is not a total
  deadline there. A trickling signing peer can exceed it.
- Equal canonical article text intentionally shares an Encypher plan across
  page URL or title changes. The manifest can retain the first canonical URL,
  as allowed by the matching server contract.

### Current review verification

The unattended gate is
`bun /home/agentdesk/scratch/team554-review-ci.mjs full`.
Run `1792867884379` passed 37 of 38 commands, including module/core and all
adapter tests, native/WASM builds and checks, CLI/parity, JavaScript, docs and
proposal checks. `clippy-spin-wasm` rejected an obsolete response-policy
wrapper; it was removed and its tests migrated to the existing bounded helper.
Subsequent runs `1792869014289` and `1792870296283` passed all 38 commands.
Final run `1792871494395` passed all 38 commands, including 93 module tests,
3,022 core tests, every adapter test/build/lint gate and documentation checks.
Shellcheck also passed separately. A superseded in-progress run was cancelled
before the final offset-vector preallocation correction; it is not a pass.

Review regressions reproduced canonical custom-element text divergence.
The raw-text decoy regression passed functionally but took 5.78 seconds for
10,000 unrelated closing tags before correction. Sparse embedding over
4.9 million visible article characters passed functionally but used a
document-sized offset vector; observed process max RSS was 69,693,440 bytes.
The corrections filter unrelated raw end names before scanning attributes and
stop offset decoding at the highest signed index. No fixtures were changed.

Rebuilt Axum smoke observed public, private and Set-Cookie responses with
status 200 and original article text preserved. Private and Set-Cookie paths
made no API connection. A stalled TLS handshake failed open with `timeout`
in 0.365 seconds on the final rebuilt binary. The scoped local CA was rejected despite passing
the Python trust control; successful signing, query omission and HTTP 503
handling were not exercised by this runtime smoke. No trust checks were
weakened. Ports and temporary certificate keys were cleaned.

Cycle 2 replaces whole-article fragment vectors and post-allocation text
truncation with a stoppable shared scanner and canonical visitor. Extraction
stops at 50,000 scalars; title decoding stops at 500 and occurs only on a sign
miss. One metadata pass saves manifest presence, title range and head-close
offset. Rendering merges markers and escaped manifest metadata in one
exact-capacity output allocation. Equal-offset markers precede metadata;
characterization tests preserve malformed-head, absent-head and empty-URL
behavior. Fixtures remain unchanged.

Final evidence is retained under
`/home/agentdesk/scratch/team554-review-evidence/`, with final CI rows and
per-command logs keyed by `1792871494395` and runtime results under `runtime/`.
The security audit is under
`/home/agentdesk/security-audits/trusted-server/team554-review/`.
No source or fixture changes followed the final gate. This acceptance record
was finalized and archived locally afterward; no publication was authorized.
Historical test-first and plan-gate deviations remain recorded above.
The earlier outbound email attempt remains uncertain and must not be blindly
retried; see the TEAM_554 company session record.
