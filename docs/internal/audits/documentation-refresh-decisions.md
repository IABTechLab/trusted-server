# Documentation Refresh Decisions

- **Decision date:** 2026-08-31
- **Approver:** `aram356`
- **Status:** Approved for implementation

This record fixes the owner-gated choices for the documentation refresh. It
does not record operational receipts; those belong in
`documentation-refresh-evidence.md` or the named post-merge tracking issues.

## Audited tips

| Name                        | Full commit SHA                            | Use                                                                |
| --------------------------- | ------------------------------------------ | ------------------------------------------------------------------ |
| `audited_target_tip`        | `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf` | Exact required `origin/rc/202608` tip for Epoch 1                  |
| `implementation_start_head` | `b904b3aeb5af26a536afadcbfb2d70af36bca5a2` | Task 1 worktree HEAD before this approval commit                   |
| Starting `audited_main_tip` | `d516a9e94249e10cbc36e41beb4269f9255cf407` | Initial reference only; each later `main` PR records a fresh value |

Task 1 fetched `origin/rc/202608` and `origin/main` on 2026-08-31. The rc ref
equaled `audited_target_tip`, and the implementation branch contained that
commit. A later `main` PR must not reuse the starting `audited_main_tip` after
`main` advances.

## Owner-gated decisions

### 1. Temporary Fastly service-ID exception

- Selection: retain the checked-in `fastly.toml` `service_id` only through a
  typed, temporary scanner allowlist entry.
- Owner: `aram356`.
- Next review: 2026-09-30.
- Control: the review date requires renewal or expiry of the exception. It is
  not an ops migration deadline, and the migration does not block this refresh.

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

- five PRs through activation: rc PR (a), containment PR (b), validation-only
  controller PR (c), CNAME deletion PR (d), and activation PR (c2);
- the separately owned release-handoff PR (e) and both reviewed outcome paths;
- temporary `main` branch protection requiring the literal
  `docs/automation-delta` status with strict/up-to-date enforcement;
- merge queues disabled on `main` through PR (e); and
- the external dependency-snapshot retirement API call under the design's
  freeze, drain/cancel, same-identity empty-snapshot, receipt, graph-check, and
  branch-deletion controls.

The rollback and exact-SHA binding requirements in the design remain
mandatory. This approval does not authorize weaker substitutes or link-only
evidence.

### 8. CodeQL rc push coverage

- State: explicitly non-blocking.
- Selection: no additional decision is required for implementation to start.

## Delivery records

Populate each pending field with the exact URL and immutable identifiers at
the named checkpoint. Do not infer a value from a branch name.

| Item                              | Target      | URL                              | Fresh audited base                         | Head or merge SHA | State         |
| --------------------------------- | ----------- | -------------------------------- | ------------------------------------------ | ----------------- | ------------- |
| (a) rc implementation PR          | `rc/202608` | Pending verification of PR #1049 | `07dfc1c6dddf69345ded17bd2d40a3d01bb39bcf` | Pending           | Open          |
| (b) containment PR                | `main`      | Pending                          | Pending                                    | Pending           | Not started   |
| (c) validation-only controller PR | `main`      | Pending                          | Pending                                    | Pending           | Not started   |
| (d) CNAME deletion PR             | `main`      | Pending                          | Pending                                    | Pending           | Not started   |
| (c2) activation PR                | `main`      | Pending                          | Pending                                    | Pending           | Not started   |
| (e) release-handoff PR            | `main`      | Pending                          | Pending                                    | Pending           | Release-owned |

## External issue records

| Record                         | Canonical URL | Owner                            | State     |
| ------------------------------ | ------------- | -------------------------------- | --------- |
| c2 activation tracking issue   | Pending       | Pending before c2 opens          | Not filed |
| Release-handoff tracking issue | Pending       | Pending before activation closes | Not filed |

## Branch-protection and ruleset records

The detailed JSON and body hashes belong in the evidence record or canonical
post-merge issue. Mirror only the decision-relevant values here.

| Checkpoint                | Capture reference | Required contexts                           | Strict/up-to-date                  | Merge queue               | Bypass policy |
| ------------------------- | ----------------- | ------------------------------------------- | ---------------------------------- | ------------------------- | ------------- |
| Before PR (c)             | Pending           | Pending inventory                           | Pending                            | Must be disabled          | Pending       |
| After PR (c)              | Pending           | Includes `docs/automation-delta`            | Required                           | Disabled                  | Pending       |
| After PR (e), normal path | Pending           | `docs/automation-delta` plus full WP8 suite | Required                           | Pending owner disposition | Pending       |
| After PR (e), abandonment | Pending           | No nonreporting automation context          | Restored from captured prior state | Pending owner disposition | Pending       |
