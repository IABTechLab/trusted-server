# EC route consent script implementation plan

**Goal:** Report CMP consent to the EC route in order, merge its answer into
identity-mode Prebid auctions and copy a localStorage SharedID opt-out into a
cookie, as the spec `2026-11-18-ec-route-consent-script-design.md` defines.

**Architecture:** Small modules inside the existing Prebid integration bundle:
a TCF reader, #1535's GPP capture, a pure ordering queue, a route client, and an
opt-out sync. `index.ts` wires them after managed User ID setup in both modes;
identity mode's auction wrapper chooses its merge source from the route client.
No Rust change: T19 injects `ecRoute`.

**Tech stack:** TypeScript, Vitest (jsdom), Prebid 10.26, esbuild bundle build.

## Tasks

- [x] **Opt-out sync** (`src/integrations/prebid/pubcid_optout.ts`). Tests first
      in `test/integrations/prebid/pubcid_optout.test.ts`: Prebid predicate
      (truthy value, empty/absent/future/past/unparseable `_exp`), copy with
      marker and `Max-Age` bounds, no overwrite of an unmarked cookie, marker
      removal when inactive, storage/cookie throwing leaves state untouched,
      `isPubcidOptedOut()` with cookie-only and localStorage-only opt-outs.
- [x] **GPP capture** (`src/integrations/prebid/gpp_capture.ts`). Move #1535's
      `gpp_capture.ts` and its test file unchanged apart from import paths and
      log prefixes; run its tests. Add tests that `[-1]` and not-ready states
      report unavailable.
- [x] **TCF reader** (`src/integrations/prebid/tcf_reader.ts`). Tests first:
      terminal events and `gdprApplies === false` are readings; `cmpuishown`,
      refusal and missing status are not; listener removed from the accepting
      API; replacement resubscribes; unchanged API is not resubscribed; a throw
      from the CMP is contained.
- [x] **Ordering queue** (`src/integrations/prebid/consent_queue.ts`, pure,
      no DOM; the route client passes it the parsed `euconsent-v2`/`__gpp`
      cookies, with a duplicated name as a mismatch). Tests first:
      classification (known withdrawal, barrier rules incl. partial
      revocation, `gdprApplies` false→true, GPP change after first,
      first-snapshot rule, duplicate cookies, out-of-scope TCF sending
      nothing); dedupe against the tail (A, B, A sends three); grant
      replacement; barrier retention; A, B, C order; failed barrier retained
      at the head; failed grant dropped and a new identical reading resent.
- [x] **Route client** (`src/integrations/prebid/ec_route.ts`). Tests first
      with a mocked `fetch`: path validation (relative, `//host`, absolute
      other origin, non-string); body shape and fetch options;
      200/204/other/network/oversize/invalid envelope; one call in flight;
      discard when waiting; clear on barrier and discard; suppression until
      applied; mirror cookie expired on a known withdrawal, including one
      that cannot be sent; failed barrier resent on a trigger; failed grant
      not replayed; opt-out sync before each call; merge source selection
      (suppressed, route while the cookie is unchanged, cookie once changed);
      `exp` checked at merge.
- [x] **Wire into `index.ts` and `identity.ts`.** Refactor `identity.ts` so
      envelope parsing and merging take EIDs from a supplied source; keep #1620
      behaviour when no route client exists. Start capture + client in both
      modes when `ecRoute` validates; identity wrapper uses the client's source;
      auction-mode `requestBids` runs the opt-out sync. Integration tests in
      `identity.test.ts`/new `ec_route_integration.test.ts`: no `ecRoute` → no
      fetch (both modes); identity mode merges route IDs into a publisher
      auction; withdrawal while grant in flight leaves no merged ID; auction
      mode calls the route and merges nothing; existing suites unchanged.
- [x] **Docs:** `docs/guide/integrations/prebid.md` identity section — consent
      reporting, ordering, opt-out copy, activation by T19.
- [x] **Verify:** targeted Vitest, full `npx vitest run`, `node build-all.mjs`,
      `npm run format`/lint, docs format, then the AGENTS.md CI gate list
      (Rust gates for non-regression: crate builds embed the JS bundle).
- [x] **Independent code review** with fresh reviewers; resolve all blocking
      and optional findings; re-run affected tests.
- [ ] **Runtime verification** through Fastly (Viceroy) and Axum serving the
      built tsjs, real Chrome: preservation without `ecRoute`; with a fixture
      `ecRoute` and a controlled responder: route ID reaches a controlled
      bidder, A, B, C ordering, suppression, opt-out cookie copy.

## Commands

```bash
export PATH=$HOME/.nvm/versions/node/v24.20.0/bin:$PATH
cd crates/trusted-server-js/lib
npx vitest run test/integrations/prebid
npx vitest run && node build-all.mjs && npm run format && npm run lint
```
