# Task 13 candidate release index evidence

Status: partial candidate implementation, without signing, publication or
release authority. The production Task 13 exit remains open.

## Implemented boundary

`crates/trusted-server-release/src/distribution.rs` and its `entry`, `joins`
and test-only `summary` modules retain candidate entries and exact-cohort index
assembly. `candidate_entry_from_completed` takes a canonical completed owner
with its matching actual runtime/startup acceptance; Fastly additionally requires
owning canonical/reproduction equality. It renders candidate SPDX, build-evidence,
build-recipe and rebuild-check companions. The candidate entries bind final-byte
digests and the retained original JS inventory.

The index assembler requires exactly the four fixed canonical assets with one
source commit, policy identity and JS inventory, and checks the retained v1
schema and mirrored consumer joins. It uses tag `v0.0.0-unqualified-candidate`
and name `candidate-release-index.json`. Signature slots contain size zero and
the actual empty-file SHA-256 sentinel; `BundleSlot::Signed` requires the
uninhabited `SignedCompanions` type. No signature file is fabricated. Companions
retain `release_qualification: false`, explicit gaps and null workflow/transfer
identities. The CLI rejects zero-size signature bundles, so these fixtures cannot
pass full release verification. Consumer rejection of sentinel/unqualified
documents after future signing remains a separate tracked requirement.

Golden fixtures in `tests/fixtures/index/candidate/` and CLI
`release/evidence/candidate_fixture_tests.rs` exercise unsigned structural joins
and rejection boundaries. `recipe/tests/candidate_index.rs` covers pure owner
wiring rejections. `candidate_entry.rs` checks each real canonical entry
independently, with reporting in `distribution/summary.rs`. Collecting and
assembling the four real entries across CI jobs is now implemented. Each
candidate job uploads its rechecked entry and final executable. The
`test-release-candidate-index` job rebuilds the four entries from the exact
uploads (`distribution::transfer` and `distribution::ordinary`, test-only) and
assembles them. It then runs the CLI consumer's unsigned joins over the
result. The first hosted all-four assembly passed in run 39577910126; see the
[four-asset candidate join report](2026-10-21-four-asset-candidate-join.md).

## Real canonical-entry observations

The initial real-entry wiring in `4483e8256dc52c89e615f1388daa873324a871f5`
has [run 39529922087](https://github.com/IABTechLab/trusted-server/actions/runs/39529922087).
REST job conclusions show Fastly
[118993209872](https://github.com/IABTechLab/trusted-server/actions/runs/39529922087/job/118993209872),
Linux CLI [118993210395](https://github.com/IABTechLab/trusted-server/actions/runs/39529922087/job/118993210395)
and Apple ARM [118998766555](https://github.com/IABTechLab/trusted-server/actions/runs/39529922087/job/118998766555)
succeeded; Intel [118998766733](https://github.com/IABTechLab/trusted-server/actions/runs/39529922087/job/118998766733)
and the overall run were cancelled. These conclusions establish three completed
jobs, not an all-four result. Their measurements are not transcribed here;
the completed baseline below supplies the audited entry identities and outcomes.

[Run 39540057202](https://github.com/IABTechLab/trusted-server/actions/runs/39540057202)
concluded success for head `a9994b4c2b50265de4bbe5b8bb42cd8cab4e43ae`, building
source `d0cd53343fcbe1fb6f06eafc009e306e24b1245d`. Its logs retain:

| Asset           | Job                                                                                                    | Candidate entry SHA-256                                            | Entry bytes | Entry rebuild outcome |
| --------------- | ------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------ | ----------- | --------------------- |
| Fastly Wasm     | [119025502524](https://github.com/IABTechLab/trusted-server/actions/runs/39540057202/job/119025502524) | `45ba885e8a210d6c1b6db92ff67fff9f4d3574370a118b7aa41be359831d5631` | 1,488       | `matched`             |
| Linux GNU CLI   | [119025502758](https://github.com/IABTechLab/trusted-server/actions/runs/39540057202/job/119025502758) | `668c22fd33c959c269289192671630275939a6f0c324de36e13547b1db701c47` | 1,492       | `unavailable`         |
| Apple ARM CLI   | [119029080050](https://github.com/IABTechLab/trusted-server/actions/runs/39540057202/job/119029080050) | `23d1b2beaeaeeae048b573afaaecd10be8c20d4b9f068cad6fb8f54d07d7a49e` | 1,450       | `unavailable`         |
| Apple Intel CLI | [119029080038](https://github.com/IABTechLab/trusted-server/actions/runs/39540057202/job/119029080038) | `336b4b3876205dde71cfd99f1726455953e950636544a67d6193fa4f9633fa63` | 1,441       | `unavailable`         |

These are entry-document digests, not executable or SBOM digests. The native
entry outcome is deliberately conservative: separate fixture measurements show
Linux and ARM reproduction `identical`, while Intel reproduction exhausted its
wall budget. Those measurements do not become owning rebuild proofs in the
candidate native entry. A passing Intel canonical entry does not imply equality.

All four jobs recorded JS inventory SHA-256
`24efd45b403e955eb4cb98c0b5e9f31e7f878d0be2637d2f89447660383511f6`
for that same built source. This supports inventory compatibility for a future
join of these observations; it does not establish that a real four-job join ran.
An inventory digest includes source-specific metadata, so values from different
built commits are not interchangeable. Other runs need their own verified values.

## Remaining exits

Task 12's trusted workflow dependency and transfer binding is the next
prerequisite to a trusted cross-job candidate join. It is not the only remaining
release-attestation work. The logs explicitly retain missing authenticated
bootstrap, incomplete native material coverage, unresolved Fastly host tools,
three GNU privileged helpers with unmeasured content, incomplete Apple tool
closure, absent signatures and absent trusted workflow/run identities.

Complete qualified SPDX generation, authentic provenance subjects, actual
signature bundles, SHA256SUMS, immutable publication and final qualification
remain required. Builder/startup images still acquired by mutable tags do not
become authenticated because their installed IDs were recorded. This report
does not implement the transfer adapter or change any production gate.
