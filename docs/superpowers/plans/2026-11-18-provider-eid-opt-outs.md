# Provider opt-out enforcement implementation plan

**Goal:** Stop client and legacy stored EIDs bypassing provider ownership policy.

**Architecture:** Extend existing registry ownership metadata and shared egress
filters. Reuse the matching request snapshot; preserve schema and write paths.

**Stack:** Rust core, Fastly/Viceroy and Axum.

## Tasks

- [ ] Add failing stored-record regressions in `ec/eids.rs` for owned singles,
      active/boundary opt-outs and live provider/unowned preservation. Add cookie/body
      regressions around actual `/auction` dispatch and publisher/page-bids targeting.
- [ ] In `ec/registry.rs`, retain enabled provider IDs per source and expose a
      documented opt-out predicate using `KvEntry::opted_out`. Initialize ordinary
      partner metadata and update direct test constructors where ownership metadata is instantiated.
- [ ] Extend `auction/endpoints.rs` client filtering with `EcKvSnapshot` and
      `EcContext`; derive matching row and current time once per filtering call.
      Update the publisher shared caller and existing tests. Preserve empty/None,
      consent gates, body preference, normalized lookup and degraded read behavior.
- [ ] Gate stored reads in `ec/eids.rs` and `ec/identify.rs`; update the existing
      admin skipped reasons in `ec/admin.rs` for opted-out and expired legacy records.
      Add matching identify/admin regressions and registry enabled/disabled and overlapping-owner tests.
- [ ] Explain read-time expiry and remembered opt-out forwarding in the canonical
      `docs/guide/configuration.md`. No changes to provider lookup/write semantics.
- [ ] Run focused core tests, adapter tests and all CI gates from AGENTS.md with
      pinned tools; retain any unavailable checks explicitly. For HTML/publisher
      changes run the additional EOF harness and actual Axum/Fastly controls.
- [ ] Run actual Fastly/Viceroy application with disposable config and KV rows,
      capture bidder requests for cookie/body/page-bids and initial-page auctions; prove positive allowed EID
      delivery, denial, clearly expired memory, no-provider behavior and repeated use.
      Also run the normal Axum binary from an actual TOML config published
      through the local CLI path. Distinguish normal-process provider behavior
      from real-router persisted-row tests using controlled transport.
- [ ] Independently review full change; resolve blocking and optional findings.
      Sign and locally verify commits, push, check GitHub Verified, refresh draft PR
      against `feat/1574-axum-shared-identity` using repository PR template/creator instructions.
      Independently self-review exact published head/base and resolve findings.

## Shared lifecycle refresh

- Merge #1646 into the existing branch with a signed merge commit; target
  `feat/1574-axum-shared-identity`. Refresh this merge whenever #1646 advances.
- Keep the shared identity application, prepared records and provider runner.
  Remove obsolete adapter-local modules and the former policy-only graph.
- Preserve remembered provider opt-outs and immediate legacy-record expiry in
  core targeting, identify and admin preview. Preserve current SharedID aliases,
  cookie opt-outs, consent and body-ingestion semantics.
- Run cookie/body repeated-visit controls, stored-record expiry and opt-back-in
  checks through Fastly and the production Axum executable. Start Axum through
  prepared stores with the protected-country fixture used by acceptance tests.
- Run affected adapter suites, identity guard, parity and relevant lints. Recheck
  runtime and hosted CI against the signed published head; independently review
  the complete refreshed PR and keep draft reasons accurate.

## Review log

- The original review rounds and runtime investigation established the need for
  matching-row opt-out enforcement and immediate legacy-record expiry.
- The #1646 refresh supersedes the former browser-classification workaround:
  shared record access now supplies policy snapshots on both MVP adapters.
