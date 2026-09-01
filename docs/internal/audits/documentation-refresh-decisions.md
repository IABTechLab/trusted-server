# Documentation Refresh Decisions

- **Decision date:** 2026-08-31
- **Approver:** `aram356`
- **Status:** Approved for implementation

This record fixes the owner-gated choices for the documentation refresh. It
does not record operational receipts; those belong in
`documentation-refresh-evidence.md` and its referenced external captures.

## Audited tips

| Name                        | Full commit SHA                            | Use                                                           |
| --------------------------- | ------------------------------------------ | ------------------------------------------------------------- |
| `audited_target_tip`        | `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf` | Exact required `origin/rc/202608` tip for every package       |
| `implementation_start_head` | `01bf84a4beb4a1be4f26965478a0211f59392962` | Program-record alignment package start on `spec-docs-refresh` |

The program-record alignment package fetched `origin/rc/202608` and
`origin/spec-docs-refresh` on 2026-09-01. The rc ref equaled
`audited_target_tip`, and the implementation branch contained that commit.
Every later package repeats both checks and stops for a focused delta audit if
the target advances.

## Owner-gated decisions

### 1. Temporary Fastly service-ID exception

- Selection: retain the checked-in `fastly.toml` `service_id` only through a
  typed, temporary scanner allowlist entry.
- Owner: `aram356`.
- Expiry: `2026-09-30T00:00:00Z`.
- Control: check mode fails at or after the expiry instant. Renewal requires a
  reviewed, committed replacement before expiry. This is not the ops migration
  deadline, and the migration does not block this refresh.

### 2. CNAME

- Selection: delete `docs/public/CNAME` and retain the project-path base.
- Owner: `aram356`.
- Decision date: 2026-08-31.
- Rejected alternative: adopt a custom domain with `base: '/'`, verified
  Pages/DNS/TLS configuration, URL inventory changes, and live smokes.
- Rollback: never restore the placeholder CNAME. Keep the CNAME deleted and
  re-smoke project URLs, or restore only a previously verified custom-domain
  DNS/CNAME/TLS tuple without weakening containment exclusions.

### 3. `FAQ_POC.md`

- Selection: move it to `docs/superpowers/archive/FAQ_POC.md`.
- Owner: `aram356`.
- Decision date: 2026-08-31.
- Rejected alternatives: delete it, or rewrite it as the active public page
  `docs/guide/faq.md` with the rewrite-specific verification contract.
- Result: no active-set FAQ page and no FAQ route to preserve. The independent
  gam/kargo tombstone requirements remain unchanged.

### 4. `business-use-cases.md`

- State: closed.
- Selection: exclude it from the public build and add the source-level
  unverified banner.
- Rejected alternative: rewrite and republish it inside this refresh.

### 5. CHANGELOG release cut

- State: explicitly non-blocking and out of scope.
- Selection: apply the deterministic no-release edit in the design unless a
  release lands first; a release requires rebase and focused re-audit.

### 6. Governance ownership

- Owner: none named.
- Selection: factual-governance fallback.
- Decision date: 2026-08-31.
- Required edit: correct `ProjectGovernance.md` to current evidence—no minutes
  exist and releases are not continuous—without adding CODEOWNERS or minutes
  commitments. Naming owners remains a maintainer follow-up.

### 7. Delivery shape and external controls

`aram356` approved the following on 2026-08-31:

- all repository implementation work remains on `spec-docs-refresh` and
  existing PR #1049, targeted at `rc/202608`;
- no separate containment, CNAME, controller, activation, or release-handoff
  implementation PR is created;
- PR #1104 remains closed and superseded, with only its two reviewed source
  commits retained for transfer into #1049 during Task 2; and
- live Pages/CNAME behavior, the first real schedule, dependency submission
  and graph visibility, and any optional `main` protection change remain
  external release operations.

The immutable-baseline, exact-SHA binding, and durable external-capture
requirements in the design remain mandatory. Local builds and fixture output
may prove repository behavior but cannot substitute for release receipts.

### 8. CodeQL rc push coverage

- State: explicitly non-blocking.
- Selection: no additional decision is required for implementation to start.

## Delivery records

PR #1049 is the only implementation row. Its head is a timestamped remote
capture, not a permanent final SHA.

| Implementation PR                                      | Target      | Audited base                               | Captured remote head                       | Capture time         | State       |
| ------------------------------------------------------ | ----------- | ------------------------------------------ | ------------------------------------------ | -------------------- | ----------- |
| https://github.com/IABTechLab/trusted-server/pull/1049 | `rc/202608` | `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf` | `01bf84a4beb4a1be4f26965478a0211f59392962` | 2026-09-01T07:35:08Z | OPEN, draft |

### PR #1049 metadata capture

- Capture timestamp: 2026-09-01T07:35:08Z.
- URL: https://github.com/IABTechLab/trusted-server/pull/1049.
- State: OPEN, draft.
- Base: ref `rc/202608`, SHA
  `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf`.
- Remote head: ref `spec-docs-refresh`, captured SHA
  `01bf84a4beb4a1be4f26965478a0211f59392962`.

This capture does not assert the eventual final head. Refresh it after package
commits are pushed and before using PR #1049 as a hosted validation input.

### Superseded reviewed source

PR #1104 is closed and superseded. Its base is
`d516a9e94249e10cbc36e41beb4269f9255cf407`. The reviewed source commits are
`34b0613dc603ba6529396dad4dd4b7e68b1e11a9` and
`e6554f24f58f6122fb806ce25432f66033765c65`. Its source branch exists only for
their later transfer into #1049 in Task 2. PR #1104 has no live merge or deploy
receipt, and this record claims none.

## Release-pending records

These rows require real post-`main` external captures under the evidence
ledger's durable hashed-body contract.

| Surface                     | Required external evidence                                                                                                    | Capture owner                 | Canonical capture destination | State                       |
| --------------------------- | ----------------------------------------------------------------------------------------------------------------------------- | ----------------------------- | ----------------------------- | --------------------------- |
| Pages and CNAME             | Deployed `main` SHA, live response matrix and headers, project-path assets, and observed CNAME behavior                       | Pending — Task 17             | Pending — Task 17             | `release-pending`           |
| First scheduled link run    | Default-branch run, attempt, jobs, URLs, app identities, bounded artifact, and resulting issue-reconciliation state           | Pending — Task 17             | Pending — Task 17             | `release-pending`           |
| Dependency submission/graph | Authenticated `main` SHA, redacted submission and response bodies with hashes, detector/correlator, and graph API JSON        | Pending — Task 17             | Pending — Task 17             | `release-pending`           |
| Optional `main` protection  | Only if maintainers opt in: exact contexts/apps, strictness, bypass policy, API bodies with hashes, and planted-failure proof | Pending if selected — Task 17 | Pending if selected — Task 17 | `release-pending`, optional |

Local builds, CI simulations, mocked API responses, and fixture output cannot
complete any release-pending row.

Before Task 17 commits, it must replace every applicable pending owner with a
named owner and every applicable pending destination with the authoritative
external capture or comment location. If optional protection is not selected,
Task 17 records that disposition instead of fabricating an owner or URL.
