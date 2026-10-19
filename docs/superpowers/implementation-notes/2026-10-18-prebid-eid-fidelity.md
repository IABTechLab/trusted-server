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
