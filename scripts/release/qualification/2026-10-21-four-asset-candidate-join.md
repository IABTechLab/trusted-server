# Tasks 12 and 13: first cross-job four-asset candidate index

Status: ordinary PR CI assembled one unqualified candidate release index from
the four real candidate jobs on 2026-10-21. This is candidate integration
evidence with `release_qualification: false`. It is not a signed release,
trusted-workflow binding, authenticated acquisition or qualification, and it
extends the [candidate release index report](2026-10-21-candidate-release-index.md).

## Run

| Field                    | Value                                                                                |
| ------------------------ | ------------------------------------------------------------------------------------ |
| Run                      | [39577910126](https://github.com/IABTechLab/trusted-server/actions/runs/39577910126) |
| Event                    | `pull_request`                                                                       |
| PR head (REST head)      | `d774ee47c274257fa0a088280f7df37ed530020c`                                           |
| Built source (PR merge)  | `fb42d8268af93c011959d032c81bfa581306c7c2`                                           |
| Native reproduction mode | `defer` (recorded as `deferred-to-scheduled-run`)                                    |
| Index evidence artifact  | `unqualified-candidate-release-index-evidence-39577910126-1`, ID 12340882671, 7 days |

The previous run, 39567710071 at `6ffebbd08`, completed all four producer jobs
and both macOS matrix outputs, but its collector rejected the first download:
it passed one 20-minute deadline while the bounded transport caps an attempt at
15 minutes. `d774ee47c` gives each download its own 10-minute attempt and
leaves the transport bound unchanged.

## Jobs

| Job                                                                                                    | Name                                                | Duration |
| ------------------------------------------------------------------------------------------------------ | --------------------------------------------------- | -------- |
| [119147931992](https://github.com/IABTechLab/trusted-server/actions/runs/39577910126/job/119147931992) | Confined release Cargo candidate (Fastly), Linux    | 33 min   |
| [119147931961](https://github.com/IABTechLab/trusted-server/actions/runs/39577910126/job/119147931961) | Linux `ts` CLI release candidate                    | 31 min   |
| [119148070099](https://github.com/IABTechLab/trusted-server/actions/runs/39577910126/job/119148070099) | Linux JS handoff for macOS candidates               | 14 min   |
| [119154065068](https://github.com/IABTechLab/trusted-server/actions/runs/39577910126/job/119154065068) | macOS `ts` CLI release candidate, `macos-15`        | 38 min   |
| [119154065062](https://github.com/IABTechLab/trusted-server/actions/runs/39577910126/job/119154065062) | macOS `ts` CLI release candidate, `macos-15-intel`  | 54 min   |
| [119176658773](https://github.com/IABTechLab/trusted-server/actions/runs/39577910126/job/119176658773) | Assemble the unqualified four-asset candidate index | 15 min   |

## What the collector checked

Each candidate job exported its rechecked entry companions, the shared JS
inventory and its final executable, uploaded them with the pinned upload
action and passed the exact artifact ID, name and digest through job outputs.
The collector, which runs only when all three candidate jobs succeed on a
same-repository event, then:

- parsed four descriptors strictly and required distinct upload IDs, each
  name recomputed from the fixed asset slot, run ID and producer attempt;
- downloaded each upload by exact ID through the bounded release-tool backend
  and corroborated run, head, repository, name, digest and expiry in raw REST
  metadata, then read the exact six flat ZIP members;
- authenticated a clean clone at the built source and read the consumer
  policy from it, not from any producer;
- rebuilt each entry through the test-only `from_ordinary_ci_transfer`, which
  reruns the inventory, build-evidence, SPDX, recipe and rebuild joins and
  rehashes the executable against the recorded size and digest;
- required each rebuilt entry digest to equal the digest its producer
  reported, and assembled the index with the unchanged assembler;
- reread the written index and its 17 sibling files and checked their
  digests, the sentinel tag and the empty signature-bundle sentinels.

The CLI consumer test then accepted the assembled bytes under its unsigned
joins and policy and rejected the size-zero signature bundles.

## Entries

| Asset                         | Producer entry SHA-256                                             | Executable SHA-256                                                 | Executable bytes |
| ----------------------------- | ------------------------------------------------------------------ | ------------------------------------------------------------------ | ---------------- |
| `trusted-server-fastly.wasm`  | `93468d7d8aa4dcd066fe3b42c38943f09a732d3bc858d4ae9ed3c584c2482dc3` | `d43c4412c9b01daf6c74add4cbde01a6a6699b7b1959f9c15f0426c84e08ba63` | 66,992,633       |
| `ts-x86_64-unknown-linux-gnu` | `46676743d02c252b8a373a9242a4baf4dec3344003af44c6c314240533319db2` | `21c55e583569a84f353181022efdfd43d8310caf5fdf3fc58eba18317f8acc61` | 283,190,976      |
| `ts-aarch64-apple-darwin`     | `d6984e55e659999c9c5c0575ac1f95c89bf70b25c85157d98638edd49ed1d4d2` | `f715aa69b467db23b846efbede73d5acc3313fa605a431115ba75c854a88f088` | 43,244,624       |
| `ts-x86_64-apple-darwin`      | `b9cf2e517615876d8417a8c01fa1891504bb4ea9154dda97c1fb1405f282e03a` | `6826bed7761f62c84587f45a08f4493bab67a587b9346570cdd73ecf48220959` | 46,159,016       |

## Assembled index

| Field                          | Value                                                                             |
| ------------------------------ | --------------------------------------------------------------------------------- |
| `candidate-release-index.json` | `4872ef96086e93f720187f363e7262ae2921e663fbc9a531f2a42cc5d803b14d`                |
| Tag                            | `v0.0.0-unqualified-candidate`                                                    |
| Cohort                         | `phase1-fastly-cli`                                                               |
| Source, recipe and cohort      | `fb42d8268af93c011959d032c81bfa581306c7c2`                                        |
| Policy                         | version 1, `f08ce10cd16ff2d1acc482db2b85eb53b667ec2b4753792f277d12f41d1f636c`     |
| `tsjs-inventory.json`          | `90c79b953ca112310707d52100ebaefe6c97718466242c34ade46b188a54b729`, 246,440 bytes |

All four candidate jobs built the same JS inventory bytes for this source. The
inventory digest differs from the one in the earlier candidate index report
because the source differs.

The collector printed these unqualified candidate gaps, carried unchanged in
the companions:

```text
apple-coverage-excluded:toolchain_libraries
apple-tool-closure-unmeasured
authenticated-bootstrap-not-established
candidate-tag-sentinel
gnu-original-acquisition-unverified
gnu-privileged-helper-content-unmeasured
gnu-tool-closure-unmeasured
native-material-coverage-incomplete
native-reproduction-unavailable
native-tool-host-path-unresolved:archiver
native-tool-host-path-unresolved:assembler
native-tool-host-path-unresolved:build_tools
native-tool-host-path-unresolved:compiler
native-tool-host-path-unresolved:linker
signature-bundles-awaiting-signature
trusted-workflow-transfer-not-bound
workflow-run-identity-unavailable
```

## Limits

The exact-ID transfer and REST corroboration prove which uploads of this run
were read, not which job or attempt produced them: the artifact API does not
identify the producing job, so trusted workflow dependency binding (Task 15)
remains open and companions keep null workflow and transfer IDs. The
collector's `GH_TOKEN` is read-only and visible to the same-repository test
binary. The index is unsigned, uses the sentinel tag and cannot pass full
release verification. Native reproduction was deferred for this run, so the
three native entries record rebuild outcome `unavailable`; Fastly records
`matched`. Tasks 12 and 13 remain partial; signing, publication, authenticated
acquisition, complete native coverage and qualification are separate open
exits.
