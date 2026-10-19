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
