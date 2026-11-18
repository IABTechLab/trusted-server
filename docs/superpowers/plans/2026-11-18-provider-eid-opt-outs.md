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
      partner metadata and update direct test constructors in `ec/pull_sync.rs`.
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
      Sign and locally verify commits, push, check GitHub Verified, create draft PR
      against `feat/axum-identity-providers` using repository PR template/creator instructions.
      Independently self-review exact published head/base and resolve findings.

## Runtime-discovered policy-read amendment

- [ ] Add a one-way policy-only flag to `ec/mod.rs` and retain the graph in
      Fastly and Axum request state. Keep policy-only loaded
      snapshots local, so finalization cannot create completeness markers from them.
- [ ] In both adapters, attach that capability only for nonbrowser requests
      with enabled provider ownership and a configured graph. Leave normal and
      finalizer graphs, generation and provider dispatch gates unchanged.
- [ ] Use the policy graph at existing accepted `/auction` and page-bids snapshot
      read sites and for eligible initial-page auction targeting. Never enrich
      bidders with stored IDs through this capability. Preserve all rejection gates
      and no-provider read behavior.
- [ ] Extend route/core regressions for policy-only reads, no stored-ID leakage,
      unchanged context snapshots/markers, no duplicate reads, and skipped reads on
      non-auctions. Run stock Viceroy before/after controls plus recognized-browser
      simulation, then rerun affected Rust, runtime, format and domain gates.

## Review log

- Round 1: no blocking findings. Added overlapping-owner and initial-page runtime
  coverage; reserved exact deadline equality for deterministic unit tests.

- Round 2: independent source review found no blocking or optional findings.

- Amendment review: preserve validation timing; keep policy snapshots local to
  avoid new marker-cookie behavior. No unresolved design findings.

- Implementation amendment: retain the graph in adapter request state because
  runtime graph handles do not satisfy HTTP-extension Send/Sync requirements.
  The EC context carries only a policy-only flag; snapshots remain local.

## Axum scope amendment

- [ ] Restack the focused issue commits onto #1566 without modifying its branch.
- [ ] Add the read-only policy graph to Axum request state and its three auction
      consumers; preserve provider collection, finalization and write gates.
- [ ] Cover recognized and unknown browser clients at the real router, with
      active opt-outs, unrelated IDs and no stored-ID enrichment on policy reads.
- [ ] Independently review this amendment and implementation, resolve all
      findings, and rerun runtime checks and the applicable CI gates.
