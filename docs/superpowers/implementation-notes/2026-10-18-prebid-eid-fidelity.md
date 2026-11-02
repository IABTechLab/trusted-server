# Prebid EID Fidelity Implementation Notes

Red log for the tests this work adds or changes (spec 7.1 rule 1, A12). Each
entry names the test, the command, and the failure seen before the change, or,
for a regression net, the line temporarily reverted and the failure it caused.

## Environment

- Date: 2026-10-19
- Branch: `feat/1246-prebid-eid-fidelity`
- Rust: rustc 1.95.0 (59807616e 2026-04-14)
- Node: v24.12.0

## Task 1: `Eid` carries OpenRTB 2.6 EID-level fields

Command:

```bash
cargo test-fastly --no-run 2>&1 | grep -E "^error" | sort | uniq -c
```

Failures (the new tests and helpers do not compile against the unchanged `Eid`):

- `eid_serializes_openrtb_provenance_fields_when_present`, `eid_deserializes_provenance_fields_and_keeps_rejecting_unknown_keys`, `eid_header_carries_provenance_fields_and_repeated_sources`, `to_openrtb_forwards_eid_provenance_fields_and_repeated_sources`, `aps_request_forwards_eid_provenance_fields_and_repeated_sources`, `standard_profile_forwards_eid_provenance_fields_and_repeated_sources`: `error[E0560]: struct 'openrtb::Eid' has no field named 'inserter'` (also `matcher`, `mm`, `ext`; 4 each)
- `eid_without_provenance_fields_serializes_as_before`: `error[E0599]: no function or associated item named 'new' found for struct 'openrtb::Eid'`
- `eid_deserializes_provenance_fields_and_keeps_rejecting_unknown_keys`: `error[E0369]: binary operation '==' cannot be applied to type 'openrtb::Eid'`

## Task 2: One parser for browser EIDs

Command (each test run alone, because a failed assertion aborts the wasm test binary):

```bash
cargo test-fastly -- ec::client_eids::tests::<name> --exact
```

Failures (the stub `parse_client_eids` returns no entries, so each assertion on the parsed output fails and the Viceroy run aborts on the panic):

- `keeps_a_valid_entry_with_every_field`, `drops_only_the_entry_that_does_not_parse`, `drops_entries_that_are_not_objects`, `validates_source`, `drops_an_entry_without_a_valid_uid`, `validates_uid_ids`, `keeps_the_first_32_valid_uids`, `reads_atype_and_mm_as_integers_in_range`, `validates_ext_objects_and_their_size`, `validates_inserter_and_matcher`, `drops_duplicates_before_the_entry_cap`, `keeps_the_first_64_entries`, `keeps_order_and_repeated_sources_and_ignores_unknown_keys`, `logs_dropped_entries_and_fields_by_name_and_source_without_values`: `assert_eq!` or `assert!` on the empty stub output panics (wasm abort)
- `yields_nothing_for_a_payload_that_is_not_an_array`: passes against the stub, as expected (regression net)

## Task 3: Body and cookie go through the shared parser

Command (each test run alone, because a failed assertion aborts the wasm test binary):

```bash
cargo test-fastly -- <module path>::<name> --exact
```

Failures (against the unchanged body and cookie readers):

- `tsjs_auction_body_fixture_drops_only_the_unparsable_entry` (body rejected, provider saw `None`), `tsjs_cookie_fixture_drops_only_the_unparsable_entry` (cookie rejected, provider saw `Some(Null)`), `auction_body_eid_level_fields_reach_the_auction_request` (EID-level fields dropped): wasm abort on assertion. Moved to Task 4: they also depend on the merge rewrite; the old merge drops EID-level fields.
- `structured_cookie_keeps_eid_level_fields`, `structured_cookie_drops_a_mistyped_field_instead_of_the_whole_cookie`, `structured_cookie_isolates_an_unparsable_entry`, `structured_cookie_applies_the_entry_cap`: assertion panic (wasm abort)
- `structured_cookie_diagnostics_report_raw_uid_strings`: also FAILS on unchanged code (the strict cookie parser keeps the 513-byte UID), so it is a red-first test, not a regression net; no revert proof needed
- `auction_uses_the_cookie_when_no_body_entry_is_valid`: passes against unchanged code (regression net)
- Revert proof: with `.filter(|eids| !eids.is_empty())` removed from `resolve_client_auction_eids`, `auction_uses_the_cookie_when_no_body_entry_is_valid` FAILS (`left: Some(Null)`); line restored

## Task 4: Forward browser entries as sent; KV fills absent sources

Command (each test run alone, because a failed assertion aborts the wasm test binary):

```bash
cargo test-fastly -- <module path>::<name> --exact
```

Failures (against the old merge):

- `merge_case_1_forwards_a_browser_entry_with_its_fields`, `merge_case_2_keeps_the_browser_atype_for_a_shared_uid`, `merge_case_3_drops_the_stale_kv_uid_for_a_browser_source`, `merge_case_4_appends_kv_sources_the_browser_did_not_send`, `merge_case_5_keeps_same_source_entries_with_different_fields_apart`, `merge_case_6_matches_sources_as_the_registry_normalizes_them`, `merge_auction_eids_does_not_union_kv_uids_into_a_browser_source`, `merge_auction_eids_keeps_browser_metadata_on_conflict`, `publisher::tests::body_less_auction_eids_put_cookie_entries_first_and_kv_only_sources_after`: assertion panic (wasm abort); the old merge drops EID-level fields, puts KV first, unions UIDs and lets KV's atype and ext win
- `tsjs_auction_body_fixture_drops_only_the_unparsable_entry`, `tsjs_cookie_fixture_drops_only_the_unparsable_entry`, `auction_body_eid_level_fields_reach_the_auction_request` (deferred from Task 3): assertion panic (wasm abort); the old merge rebuilds entries without EID-level fields
- `merge_case_7_forwards_kv_entries_when_the_browser_sent_none`, `merge_drops_entries_without_uids`, `merge_auction_eids_deduplicates_client_and_resolved_ids`: pass against the old merge (regression nets)
- Revert proof: with `merged.push(eid);` replaced by `let _ = eid;`, `merge_case_7_forwards_kv_entries_when_the_browser_sent_none` FAILS (wasm abort); line restored
- Revert proof: with `.filter(|eid| !eid.uids.is_empty())` removed from the browser branch, `merge_drops_entries_without_uids` FAILS (wasm abort); line restored

## Task 5: KV ingestion on /auction reads only the auction's own client set

Command (each test run alone, because a failed assertion aborts the wasm test binary):

```bash
cargo test-fastly -- <module path>::<name> --exact
```

Failures (against the old ingestion, which always read the cookie first):

- `auction::endpoints::tests::auction_ingests_its_body_eids_and_not_a_cookie_only_source`: KV held `body.example.com` and also `cookie-only.example.com`
- `auction::endpoints::tests::disabled_auction_ingests_nothing_from_the_cookie_but_keeps_shared_id`: KV held `cookie-only.example.com` next to `sharedid.org`
- `navigation_ingests_the_cookie_only_source`, `finalize_generated_ec_without_sync_source_ingests_the_ts_eids_cookie`, the edited `finalize_recovers_orphaned_ec_when_purpose_four_denial_empties_updates` and `auction_route_carries_the_raw_ts_eids_cookie_to_finalization` pass against the old code (regression nets)
- Revert proof: with `browser_eid_input`'s fallback arm changed to `_ => (None, ec_context.client_eids()),`, `navigation_ingests_the_cookie_only_source`, `finalize_generated_ec_without_sync_source_ingests_the_ts_eids_cookie` and `finalize_recovers_orphaned_ec_when_purpose_four_denial_empties_updates` each FAIL (wasm abort); arm restored
- Revert proof: with the Fastly adapter's `let eids_cookie = crate::extract_cookie_value(req, COOKIE_TS_EIDS);` replaced by `let eids_cookie: Option<String> = None;`, `auction_route_carries_the_raw_ts_eids_cookie_to_finalization` FAILS (wasm abort); line restored

## Task 6: Pure EID collector and cookie value builder

Command (against a stub `eids.ts` whose functions return `undefined` and an empty cookie):

```bash
cd crates/trusted-server-js/lib && mise exec node@24.12.0 -- npx vitest run test/integrations/prebid/eids.test.ts
```

Result: 22 failed, 9 passed (31 tests). The 9 passes are the four `returns undefined for …` cases, the four `… not an object` cases and `does not modify the entries it is given`, which the stub passes trivially.

Failures:

- `collectBidderEids` tests (`reads ortb2.user.ext.eids`, `reads ortb2.user.eids, …`, `reads user.ext.eids before user.eids`, `returns plain-data copies without undefined fields`, `drops an entry whose serialization throws …`, `keeps a set that fits the UTF-8 budget exactly`, `drops the entry that crosses the budget …`, `measures the budget in UTF-8 bytes …`, `keeps order and repeated sources`, `forwards a lone surrogate unchanged …`, `passes the Prebid module shapes from spec 3.5 …`): `expected undefined to deeply equal [...]`
- `fitEidCookie` tests (`removes EID-level ext and drops the duplicates …`, `trims UIDs from the last source …`, `reports a source trimmed and then dropped only as dropped`, both `removes a last entry that is … whole` cases, `removes a null last entry whole …`, `encodes non-Latin-1 text as UTF-8 base64 …`): `should produce a cookie value: expected undefined to be defined`
- `fitEidCookie > drops whole tail sources until the cookie fits`: `TypeError: Cannot read properties of undefined (reading 'length')`
- `fitEidCookie > returns no value when nothing fits`: `droppedSources` is `[]` instead of `['huge.example.com']`
- `fitEidCookie > matches btoa output for ASCII payloads`, `fitEidCookie > produces exactly the ts-eids-cookie fixture for the fixture input`: `expected undefined to be '<base64>'`

## Task 7: Adapter forwards the bidder request's EIDs and owns ts-eids

Command (canary, helpers and new tests installed; adapter unchanged):

```bash
cd crates/trusted-server-js/lib && mise exec node@24.12.0 -- npx vitest run test/integrations/prebid/index.test.ts test/core/request.test.ts
```

Result: 175 failed, 132 passed. Besides the new tests, about 160 existing tests that build a `trustedServer` request or run `bidsBackHandler` fail in the shared `afterEach`, because the old collector forwards the `canary.example.com` EID from `getUserIdsAsEids()` (`the getUserIdsAsEids canary leaked into an /auction body`) or writes it to `ts-eids`.

Failures:

- `forwards an opaque LiveRamp envelope from the bidder request unchanged`: `expected [ { …(2) } ] to deeply equal [ { source: 'liveramp.com', …(1) } ]` (the canary arrives instead)
- `forwards bidderRequest.ortb2 EIDs with EID-level fields, user.ext.eids first`: `should forward both paths as sent: expected [ { …(2) } ] to deeply equal [ …(2) ]`
- `sends exactly the auction-body fixture for the fixture input`: `TSJS should produce the Rust fixture byte for byte`
- `writes exactly the ts-eids fixture for the fixture input`: `expected undefined to be 'W3sic291cmNl…'`
- `writes ts-eids from the body set without EID-level ext`: `the body keeps EID-level ext: expected [ { …(2) } ] to deeply equal [ { …(3) }, { …(2) } ]`
- `clears ts-eids before the request when the bidder request holds no EIDs`: `the body should carry no eids: expected [ { …(2) } ] to be undefined`
- `sends no eids and leaves ts-eids alone without a bidder request object`: `expected [ { …(2) } ] to be undefined`
- `lets the later of two overlapping requests own ts-eids`: `expected undefined to deeply equal [ { …(2) } ]`
- `forwards ortb2 EIDs and writes ts-eids without the User ID module`: `expected undefined to deeply equal [ { source: 'id5-sync.com', …(3) } ]`
- `logs EID counts and the cookie decision at debug level, never values`: `expected "debug" to be called with arguments: [ …(2) ]`
- `keeps the trim log when the cookie had to drop sources`: `the body keeps every entry: expected [ { …(2) } ] to deeply equal [ …(7) ]`
- `clears ts-eids when tcf2Enforcement lists trustedServer in biddersBlocked` and `… in eidsBlocked`, and `leaves ts-eids alone when tcf2Enforcement lists only other bidders`: `the shim should listen for tcf2Enforcement: expected undefined to be defined`
- `leaves ts-eids alone when an auction builds no trustedServer request`: `only buildRequests may write or clear the cookie: expected 'W3sic291cmNl…' to be 'preset-value'`
- `exposes the getUserIdsAsEids canary on the pbjs the shim uses` and `sends no eids in the /auction body`: PASS (positive control and net)
- Net proof: with `src/core/request.ts:45` changed to pass `{ eids: [{ source: 'net.example.com', … }] }` to `buildAdRequest`, `sends no eids in the /auction body` FAILS (`requestAds should leave EIDs to the ts-eids cookie: expected { adUnits: […], …(2) } to not have property "eids"`); line restored

## Task 8: Stop writing Prebid userSync settings

Command (new self-init tests added; `installUserIdModules` still present):

```bash
cd crates/trusted-server-js/lib && mise exec node@24.12.0 -- npx vitest run test/integrations/prebid/index.test.ts -t "writes no userSync|preserves publisher userSync"
```

Result: 2 failed, 2 passed. The file-level `afterAll` canary also reports `the canary should have inspected at least one /auction body: expected 0 to be greater than 0`. This comes from the `-t` filter selecting no `/auction` test, not from this change, and a full run satisfies it.

Failures:

- `prebid self-init writes no Prebid userSync settings > writes no userSync when the bundle loads after window load`: `TS should leave userSync to the publisher and Prebid: expected [ [ { userSync: { …(6) } } ], …(1) ] to deeply equal []` (the `auctionDelay: 0`, `syncDelay: 3000`, `filterSettings.all` write)
- `prebid self-init writes no Prebid userSync settings > writes no userSync when window load fires after install`: same assertion, same write after the `load` event
- `preserves publisher userSync and autoRefresh=%s when seeding late IDs` (both `false` and `true`): PASS with the extended `syncDelay` and `filterSettings` fields. This is the seeding net; its proof follows in Task 8's green run.

Net proof (seeding path): with `src/integrations/prebid/index.ts` line 2181 changed from `...(isRecord(effectiveUserSync) ? effectiveUserSync : {}),` to `...{},`, `-t "preserves publisher userSync"` FAILS on both cases: `preserves publisher userSync and autoRefresh=false when seeding late IDs` and `… autoRefresh=true …`, with `expected { userIds: [ { …(3) } ], …(1) } to deeply equal { userIds: [ { …(3) } ], …(5) }`. Line restored; the full file then passes 294 of 294.

## Task 9: Real Prebid.js arms for D1, D3, D4 and TCF

Command (the shim built from `main`'s sources: `src/integrations/prebid/index.ts` and `src/core/auction.ts` checked out from the merge base `a9037770a` for one run, then restored with `git checkout --`; `git status --short crates/trusted-server-js/lib/src` was clean afterwards):

```bash
cd crates/trusted-server-js/lib && mise exec node@24.12.0 -- npx vitest run test/prebid-eid-fidelity.test.mjs
```

Result: 5 failed, 1 passed (6 tests). Each failure is the one spec 7.5 predicts.

Failures:

- `D1: never forwards an ID restricted to another bidder`: `the /auction body should carry the control and not the restricted ID: expected [ [ { source: 'pubcid.org', …(1) } ] ] to deeply equal [ [ { …(2) } ] ]` (the precondition passed: sharedId produced `pubcid.org` from the preset `_sharedid` cookie)
- `D3: forwards same-source entries separately with their own EID-level fields`: `expected [ undefined ] to deeply equal [ [ { …(5) }, { …(5) } ] ]` (`main` never reads `ortb2`)
- `D4: the first auction after window.load waits for a pending User ID module`: `Prebid's default auctionDelay should apply: expected +0 to be 500`
- `TCF: without an exception no client EIDs reach TS and ts-eids is cleared`: `ts-eids should be cleared: expected [ { source: 'liveramp.com', …(1) } ] to be undefined` (the positive control and the no-`/auction` check passed; `main`'s `bidsBackHandler` rewrote the cookie)
- `TCF: with the full basicAds rule TS gets its EIDs and other bidders stay blocked`: `expected [ Array(1) ] to deeply equal [ [ { …(2) }, { …(2) } ] ]` (the body carried `liveramp.com` only)
- `D4 control: the first auction before window.load waits, on every build`: PASS, as intended

D4 mechanism probe: a temporary copy of the suite without the `auctionDelay` assertion, run with `-t "D4"` against the same `main` sources, still fails the D4 arm at the body: `the first /auction should carry the delayed unifiedId: expected [ undefined ] to deeply equal [ Array(1) ]`. So on `main` the first auction after `window.load` goes out without `adserver.org`, not merely with a different setting. The copy was deleted and the sources restored.

## Task 10: Delete the dead EID-to-module resolver

Command:

```bash
cd crates/trusted-server-js/lib && mise exec node@24.12.0 -- npx vitest run test/integrations/prebid/user_id_modules.test.ts -t "no EID source metadata"
```

Failures:

- `carries no EID source metadata`: `no entry should keep eidSources: expected [ 'connectIdSystem', …(10) ] to deeply equal []` (11 modules still carry `eidSources`)

Green: after deleting `eidSources` and the resolver, `carries no EID source metadata` passes. The three targeted suites (`user_id_modules.test.ts`, `prebid-user-id-registry.test.mjs`, `build-prebid-external.test.mjs`) pass 81 of 81. `./scripts/test-cli.sh` exits 0. The full Vitest run under Node 24.12.0 passes 1249 of 1249 across 52 files.

## Final review fixes

### Fix 1: nothing fits clears the cookie (regression net)

Revert proof: temporarily deleted the `clearPrebidEidsCookie();` call inside the `fitted.value === undefined` branch of `writePrebidEidsCookie`, then restored it.

Command:

```bash
cd crates/trusted-server-js/lib && mise exec node@24.12.0 -- npx vitest run test/integrations/prebid/index.test.ts -t "no EID fits"
```

Failures:

- `clears ts-eids when no EID fits the cookie budget but still sends the body`: `the cookie should be cleared: expected 'preset-value' to be undefined`

Green: with the call restored, the test passes.

### Fix 2: cookie writes must not throw out of buildRequests

Command (against the unguarded `clearPrebidEidsCookie`):

```bash
cd crates/trusted-server-js/lib && mise exec node@24.12.0 -- npx vitest run test/integrations/prebid/index.test.ts -t "document.cookie writes throw"
```

Failures:

- `still builds the request with the entry when the cookie write fails`: `Error: cookies blocked` thrown from `clearPrebidEidsCookie` through `buildRequests`
- `still builds the request without eids when the cookie clear fails`: `Error: cookies blocked` thrown from `clearPrebidEidsCookie` through `buildRequests`

Green: with the `try`/`catch` guard both pass; full Vitest passes 1252 of 1252.

### Fix 3: non-array cookie JSON is a parse error

Command (against the unchanged decoder):

```bash
cargo test-fastly -- parse_prebid_eids_cookie_rejects_non_array_json
cargo test-fastly -- eids_lookup_reports_non_array
```

Failures:

- `parse_prebid_eids_cookie_rejects_non_array_json`: test trapped, the `{"a":1}` cookie parsed as `Ok`
- `eids_lookup_reports_non_array_cookie_as_parse_error`: test trapped, no `parse_error` reported

Green: with the array check both pass.
