# PR #986 review-finding ledger

Disposition of every review finding against the provider/permission spec
set, by round. Statuses: **fixed** (commit noted) · **reapplied** (fix was
lost to a failed edit batch and re-landed — the round-4 script loss is
called out where it happened) · **partial → refixed** (a later round showed
the fix incomplete; both commits noted) · **superseded** (descope or a
later design change removed the surface) · **deferred** (moves with a
deferred feature; recorded as its entry bar) · **open** (sign-off table,
migration spec §8).

Commits: R1 `a35f2ca78` · R2 `9886091e5` · R3 `5c8c2e893` · R4 `2b4d776b6`
· R5 `de70ca931` · R6 `c8b4b849e` · R7 (this commit).

## Round 1 — adversarial self-review (22 findings)

All 22 fixed in R1, three later shown partial and refixed: geo/device
gating circularity (refixed R3 — device half was wrong again), identity
stability vectors (refixed R3 — random suffix), §5.3 citations (fixed R1).
Policy moved YAML → TOML in R1 (maintainer decision). No open remnants.

## Round 2 — first maintainer review (15 blocking + 1 + 4)

| Finding                                                                                      | Status                                                                                          |
| -------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------- |
| B1 raw-EC egress ungated                                                                     | fixed R2; egress table concrete R3; typed R5; scoped types R6                                   |
| B2 recipe grants everywhere                                                                  | fixed R2; fixture-not-delta R4; per-adapter R5; minimal-divergence R6                           |
| B3 blanket gate blocks withdrawal                                                            | fixed R2 (split gate, spy test)                                                                 |
| B4 provider switch strands identities                                                        | fixed R2 (legacy readers); rewrite portion superseded R7 (descope)                              |
| B5 graph-key/prefix incomplete                                                               | fixed R2; literal-prefix R3; namespace R3; core-constructed R5→R6; delimiter R7                 |
| B6 device gating not circular                                                                | fixed R2; reasoning corrected R3; qualifier R5 (reapplied R6 after batch loss)                  |
| B7 withdrawal trigger contradiction                                                          | fixed R2 (requires_signal ∨ denied)                                                             |
| B8 withdrawal storage failure                                                                | fixed R2; family record R3; idempotent families R4; unbounded residual honesty R7 (sign-off 11) |
| B9 signal normalization missing                                                              | fixed R2 (subjects); outcomes R4; preserved semantics R5; state machine R6                      |
| B10 auction class inference lossy                                                            | fixed R2 (regime); dispatch matrix R4; raw arm R5; persisted-TCF arm R7                         |
| B11 no-geo guard too narrow                                                                  | fixed R2 (all jurisdiction consumers); cookie consumer deferred R7                              |
| B12 validation incomplete                                                                    | fixed R2; rules.default/dupes R3; region assigned R5                                            |
| B13 Sec-Fetch-Site insufficient                                                              | fixed R2 (exact Origin / CSRF); deferred with client spec R7                                    |
| B14 replay unmitigated                                                                       | fixed R2; session binding required R4; ownerless mode removed R7                                |
| B15 unbounded inputs                                                                         | fixed R2; exact limits R5; media-type matching R6 — all deferred with client spec R7            |
| ❓ issue contradictions                                                                      | fixed R2 (divergence tables per spec)                                                           |
| Hook &mut HeaderMap + framing headers                                                        | fixed R2 (structured ops, reserved list)                                                        |
| 4 non-blocking (geo residual, eligibility matrix, cluster capability, deterministic entropy) | all fixed R2                                                                                    |

## Round 3 — second maintainer review (15 blocking + hook + gaps)

| Finding                                                                                                                           | Status                                                                                                  |
| --------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- |
| Permission algebra can't preserve US                                                                                              | fixed R3 (grant class); regime-scoped R4; field-scoped R5→R6                                            |
| Auction dispatch undefined                                                                                                        | fixed R3 (regime matrix + fallback regime)                                                              |
| Normalization delegated                                                                                                           | fixed R3→R5 (in-spec outcomes, preserved semantics R6)                                                  |
| Egress inventory incomplete/wrong                                                                                                 | fixed R3 (path table, 11a/11b split); pull/batch split R5                                               |
| Batch sync no authority source                                                                                                    | fixed R3 (stored provenance); full recompute R5; live-refusal rule R7                                   |
| Hook ordering vs cache protection                                                                                                 | fixed R3 (invariant pass); snapshot monotonic R5; sticky axes R6; must-understand R7                    |
| Universal case equivalence                                                                                                        | fixed R3 (provider-declared fixtures)                                                                   |
| Namespacing vs HMAC stability                                                                                                     | fixed R3 (reserved grammar); all-versions-verbatim R7                                                   |
| Legacy-reader gaps                                                                                                                | fixed R3→R5 (namespaces, governing permissions, provenance, provider=none); rewrite parts superseded R7 |
| Graph prerequisites/runtime failures                                                                                              | fixed R3 (startup requirement, runtime matrix, active-after-commit); adoption path R7                   |
| Withdrawal atomicity                                                                                                              | fixed R3 (idempotent families); family record R4; suppression R6→R7                                     |
| No dual-compatible config                                                                                                         | fixed R3 (dual-read N+1); ordering corrected R5; both-direction rollback R6→R7                          |
| Source-agnostic sources dropped                                                                                                   | fixed R3 (explicit deferral, divergence row)                                                            |
| Client resolve replacement/replay                                                                                                 | fixed R3→R5; deferred with client spec R7                                                               |
| Device contract / region default                                                                                                  | fixed R3 (stale wording, US/CA default)                                                                 |
| Hook API/eligibility                                                                                                              | fixed R3 (ops API, eligibility matrix)                                                                  |
| Completeness gaps (EcId bounds, non-cluster dedupe, mutator limits, per-row tests, runtime behavior, metrics, fail-closed labels) | all fixed R3→R5                                                                                         |

## Round 4 — architecture review (1 P0, 12 P1 groups, 9 P2, 3 P3)

| Finding                                                                                                                                                                                     | Status                                                                                |
| ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| P0 legacy family-ID protocol hole                                                                                                                                                           | fixed R4 (deterministic derivation)                                                   |
| US GPP/USP not permission-scoped                                                                                                                                                            | fixed R4 (§4.5 table); regime scope R5; global aggregation R7                         |
| Malformed state machine                                                                                                                                                                     | fixed R4; six-state machine + matrix column R6                                        |
| TCF conflict nondeterminism                                                                                                                                                                 | fixed R4; **wrongly "preserved" — refixed R6** (conjunction algorithm)                |
| Raw-TCF arm excludes malformed                                                                                                                                                              | fixed R4 (raw-presence trigger); persisted fallback R7                                |
| Stale authority renewal                                                                                                                                                                     | fixed R4 (valid_until, snapshot replace); evidence classes R6; digest scope R6        |
| Degraded mode cross-instance                                                                                                                                                                | fixed R4 (state machine); **local-only honesty R7** (sign-off 11)                     |
| Workers KV "bounded" lag                                                                                                                                                                    | **partial R4 → refixed R6/R7** (strong primitive required; single normative home)     |
| Physical keys/schemas contradictory                                                                                                                                                         | fixed R4→R6; alias-at-source-key + versionless keys R7                                |
| HMAC version via parse                                                                                                                                                                      | fixed R4 (**lost in failed batch, reapplied R6**; provenance-resolved)                |
| AuthorizedIdentity unscoped                                                                                                                                                                 | fixed R6 (GraphOps/PartnerEgress); request-side redaction R7                          |
| Client contract missing                                                                                                                                                                     | fixed R5 (Acquisition modes); ClientResolveContext R7; deferred R7                    |
| Revocation-wins impossible                                                                                                                                                                  | fixed R6 (family epoch CAS); cross-key atomicity recorded open, deferred R7           |
| Rewrite storage/transactionality                                                                                                                                                            | fixed R5→R6; **superseded R7 — rewrite_legacy cut from epic**                         |
| Release/rollback inconsistent                                                                                                                                                               | fixed R5 (dual-read); reader-first R5; preconditions R6; floor marker + N+1 duties R7 |
| Graphless deployments                                                                                                                                                                       | fixed R6 (readiness step); adoption path R7 (matrix row 13)                           |
| No concrete adapter matrix                                                                                                                                                                  | fixed R5→R6; availability-vs-wiring split + Axum honesty R7                           |
| Hook cookies bypass model                                                                                                                                                                   | fixed R5 (write gate); **shown insufficient → cookie ops deferred R7**                |
| Cache lattice invalid                                                                                                                                                                       | fixed R5; **ordered lattice wrong → sticky axes R6**; full Vary R6                    |
| P2/P3 (mint ordering, health machine, client limits, fixtures graph config, thresholds, N/A, KV pipeline, transitions, protective labels, FR wording, device qualifier, illustrative label) | all fixed R4–R6 (device qualifier reapplied R6 after batch loss)                      |

## Round 5 — package re-review

All 15 P1 and 4 P2 dispositioned above where they refined earlier rows;
net-new: sections map (fixed R5; **registry-corrected R7**: +6, +24–27),
live-vs-stored refusal (fixed R7), suppression record (fixed R6;
completeness R7), fixture invalidity (stateless fixtures R7).

## Round 6 — re-review at de70ca9

All 18 P1 and 4 P2 fixed in R6 except where R7 shows partials (tracked in
the R7 table below). Sign-off table with owners/status introduced R6.

## Round 7 — current

| Finding                                           | Status                                                                                                                                                               |
| ------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Client page leg pre-gate                          | fixed (page-leg gating; deferred with client spec)                                                                                                                   |
| Cookie read/use/withdraw unmodeled                | **cookie ops deferred out of v1 hook** (entry bar recorded; sign-off 9/10 updated)                                                                                   |
| Ownerless mode reintroduces fixation              | fixed — ownerless mode removed outright                                                                                                                              |
| Graphless cookie adoption                         | fixed (adoption transaction, matrix row 13)                                                                                                                          |
| Rewrite retention lineage                         | superseded — rewrite_legacy cut; finding recorded as entry bar                                                                                                       |
| Unreferenced provider blocks                      | fixed (startup error)                                                                                                                                                |
| Fastly prefix-query delimiter                     | R7's per-adapter `:` delimiter was itself Fastly-rejected and non-portable → **refixed R8**: delimiter-free fixed-width grammar (class tag + registry provider code) |
| P2 alias/tombstone cluster counting               | fixed (liveness/kind filtering; aliases reserved-future)                                                                                                             |
| P2 push-vs-deploy validation                      | fixed (two named layers, capability profile)                                                                                                                         |
| P2 rollback floor unobservable                    | fixed (floor = writer activation, durable marker)                                                                                                                    |
| P2 cookie ownership uniqueness                    | integration-ID uniqueness kept; cookie ownership deferred with cookie ops                                                                                            |
| Still-open: GPP 6/24–27                           | fixed (registry-complete map)                                                                                                                                        |
| Still-open: state-over-national opt-out erasure   | fixed (grants-only precedence)                                                                                                                                       |
| Still-open: suppression completeness              | partial R7 (ordering claimed without CAS; cause-list coverage; timestamp-less unrealizable) → **refixed R8** (CAS + version counter, delta coverage, sticky opt-out) |
| Still-open: family-epoch cross-key CAS            | recorded as client-spec open question 0; deferred                                                                                                                    |
| Still-open: eventual rows vs alias guarantees     | superseded (rewrite cut)                                                                                                                                             |
| Still-open: 4-hop stranding                       | superseded (rewrite cut)                                                                                                                                             |
| Still-open: N+1 enforce vs N+2 write boundary     | fixed (N+1 writes safety-critical records)                                                                                                                           |
| Still-open: N+2-only legacy reader on N+1         | fixed (accepted in legacy position)                                                                                                                                  |
| Still-open: no-geo guard cookie consumers         | deferred with cookie ops (inventory row updated)                                                                                                                     |
| Still-open: persisted TCF in raw arm              | fixed (TCF-sourced effective record triggers arm)                                                                                                                    |
| Still-open: request-side raw identity             | asserted R7 without API/tests → **specified R8** (`RedactedRequestView`, enumerated strip set, same-PR migration, denied/withdrawn tests)                            |
| Still-open: RequestFilterEffects.response_headers | R7 fold-in would have **broken DataDome** (302/401/403/429 + cookies) → **refixed R8**: distinct core-owned security channel sharing validation + invariant layers   |
| Still-open: must-understand                       | fixed (sticky set extended)                                                                                                                                          |
| Still-open: Axum persistence overstated           | partial R7 (still marked wired; head installs `UnavailableKvStore`) → **refixed R8** (not wired)                                                                     |

## Round 8 — re-review at 09e54e96

| Finding                                     | Status                                                                                                                                              |
| ------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------- |
| P1.1 suppression monotonicity unprovidable  | fixed (linearizable CAS + record version counter; sticky opt-out for timestamp-less sources — sign-off 16; race fixtures)                           |
| P1.2 malformed/absent leave stale authority | fixed (suppression on every positive→unset delta, cause-agnostic)                                                                                   |
| P1.3 suppression-write failure "transient"  | fixed (unbounded residual, shares sign-off 11, fault test)                                                                                          |
| P1.4 N/A double meaning                     | fixed (single rule: explicit N/A = grant-class, absent = nothing; sign-off 17)                                                                      |
| P1.5 adoption = syntax-as-authentication    | fixed (`verify` against request evidence; expire on failure; full required_permissions; atomic create-if-absent capability; read-error ≠ not-found) |
| P1.6 N+1 impossible write behavior          | fixed (N+1 mints v1 with today's semantics; old-shape config runs pre-epic gate; new contracts activate at N+2/new-shape — sign-off 20)             |
| P1.7 N+2-only provider readable by N+1      | fixed (providers ship compiled-in dormant one release early; reader-first per provider)                                                             |
| P1.8 cookie deferral contradictions         | fixed (ops list, reserved remnant, generic-op wording, core-owned-cookie test)                                                                      |
| P1.9 DataDome fold-in breakage              | fixed (distinct security channel, shared validation/invariant layers, core-mediated security cookies)                                               |
| P1.10 freshness metadata weakening          | fixed (`Age`/`Date`/`Expires` reserved, rationale in-spec)                                                                                          |
| P1.11 client-cycle in normative core        | fixed (Acquisition enum/ClientResolve/reservations removed from trait surface; `verify` added; deferred doc holds the rest)                         |
| P1.12 redaction unspecified                 | fixed (see corrected R7 row above)                                                                                                                  |
| P2.1 rewrite residue                        | fixed (tests, runtime row, metrics/retirement swept; alias schema marked reserved)                                                                  |
| P2.2 delimiter portability                  | fixed (see corrected R7 row above)                                                                                                                  |
| P2.3 Axum matrix                            | fixed (see corrected R7 row above)                                                                                                                  |
| P2.4 adoption rejuvenation                  | fixed (migration-cutoff-bounded TTL; sign-off 21)                                                                                                   |
| P2.5 stored cluster overcount               | fixed (no persistence beyond inputs' lifetime)                                                                                                      |
| P2.6 GPP applicability leftovers            | fixed (MD/IN/KY/RI sentence removed; section-6 grants defined; regime-`none` row reconciled with applicability)                                     |
| P2.7 mixed-revision divergence              | accepted explicitly (sign-off 19)                                                                                                                   |
| P2.8 floor marker rollbackable              | fixed (write-once/CAS deployment metadata)                                                                                                          |
| P2.9 sign-off gaps                          | fixed (rows 16–21 added; rows 3 and 11 amended)                                                                                                     |
| P2.10 duplicate integration IDs             | fixed (startup rejection + test; current silent coalescing named)                                                                                   |
| P3 GPP version pinning                      | fixed (accepted versions enumerated; unknown version = malformed-present)                                                                           |
| P3 geo region vocabulary                    | fixed (ISO output or declared canonical mapping)                                                                                                    |
| P3 stale fragments + ledger overstatement   | fixed (this ledger corrected; hook remnants swept)                                                                                                  |
