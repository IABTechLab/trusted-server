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
