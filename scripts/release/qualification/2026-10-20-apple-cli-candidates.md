# Task 9: Apple CLI candidates build, inspect and start on native macOS runners

Status: ordinary PR CI built both Apple `ts` candidate recipes on native macOS
15 runners and started each canonical executable. Two earlier runs
(2026-10-19) and two later runs (2026-10-20) are recorded below. Every record
carries `release_qualification: false`. This is candidate evidence only: it
does not qualify production tools, the Xcode/SDK selection, acquisition or
release enablement, and macOS 15 qualification (Task 8) remains open.

## Runs and jobs

All four runs were `Run Tests` workflows on branch `spec/1235-release-attestation`
and concluded `success`. The Intel optional reproduction is a bounded
measurement; its failure does not fail the job (see below).

| Run                                                                                  | Branch head                                | Built source (PR merge commit)             | ARM job                                                                                                | Intel job                                                                                              |
| ------------------------------------------------------------------------------------ | ------------------------------------------ | ------------------------------------------ | ------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------ |
| [39273777887](https://github.com/IABTechLab/trusted-server/actions/runs/39273777887) | `d8fdb265748181f3d419b7902b3b0590a67c2ad9` | `cace4c508159bc7502d359e0cce49fef47d61f8b` | [118142122193](https://github.com/IABTechLab/trusted-server/actions/runs/39273777887/job/118142122193) | [118142122336](https://github.com/IABTechLab/trusted-server/actions/runs/39273777887/job/118142122336) |
| [39290139247](https://github.com/IABTechLab/trusted-server/actions/runs/39290139247) | `df4a6e5c30fe8c93df68748ead77a3bc559d16b2` | `506d8ead69757556e1bd8ed29b578d0257d05169` | [118196413038](https://github.com/IABTechLab/trusted-server/actions/runs/39290139247/job/118196413038) | [118196413106](https://github.com/IABTechLab/trusted-server/actions/runs/39290139247/job/118196413106) |
| [39417433680](https://github.com/IABTechLab/trusted-server/actions/runs/39417433680) | `bfab8ef2009fa6f28b1258135e2445ee22775dc8` | `b05bd8a18f137b43f7542c797f0a5d7856363d23` | [118617017385](https://github.com/IABTechLab/trusted-server/actions/runs/39417433680/job/118617017385) | [118617017568](https://github.com/IABTechLab/trusted-server/actions/runs/39417433680/job/118617017568) |
| [39420011152](https://github.com/IABTechLab/trusted-server/actions/runs/39420011152) | `1719cf15fd58fcf029f6b4ae5003215802bef405` | `b2273db76b8a2070f3c0d20d22788eed434d0029` | [118625917697](https://github.com/IABTechLab/trusted-server/actions/runs/39420011152/job/118625917697) | [118625917703](https://github.com/IABTechLab/trusted-server/actions/runs/39420011152/job/118625917703) |

The job logs retain `macos-cli-candidate-measurement.json` (printed by the
workflow step) and the Cargo denial measurement for each host. The source
commit embedded in and measured for each executable is the merge commit above,
which is why digests differ between runs even though the build is
deterministic for a given source.

## Hosts and tools

Both hosts reported macOS product version 15.7.9 (ARM: `aarch64`,
`hw_optional_arm64: 1`, `proc_translated: 0`; Intel: `x86_64`). The selected
developer tools were Xcode 16.4 with SDK 15.5.0 and the boundary was
`macos-sandbox-exec`.

| Measurement                           | Value                                                                                                                                            |
| ------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| `clang` SHA-256 (ARM and Intel)       | `4c458256bcdf913774de1bb4a37768244d3b41f32ad55afb2dfec3432f46f4fe`                                                                               |
| ARM `ar` SHA-256                      | `f362372395c1dd8ef349b6ceaad3b7958f1207a32e95d285721eba4c84ca55ae`                                                                               |
| ARM `ld` SHA-256                      | `648a8aac7f6f829a42305d24dddd7a3cfa48ee0bd9d4e7ec1ded5c51edea372b`                                                                               |
| SDK settings SHA-256                  | `58a133735f0a55a624a1703067059f6e78925e51725ec6e1f966072e142c9c42`                                                                               |
| SDK tree, runs 39290139247 and later  | 46,233 entries, 656,877,420 regular bytes, tree SHA-256 `1482e88484d3ab4b430306607ed07113068eadd7581b232dedb20ea80842bce0` on both ARM and Intel |
| SDK tree, run 39273777887             | ARM `e26d617a1870b3ecaf9bb0245da20662b0b8b6b66042ae15b2023a5fda37403f`, Intel `147b1549c1c3e34ca34a36ece451d474edb67e4a5343e8bf9f814b465bead527` |
| Native Cargo denial, control size/ARM | `denial-control-aarch64-apple-darwin`, 465,928 bytes, SHA-256 `c80e84f01196e51d420a0dffa0a4472db19ac956d51c67e0f66b104280abb8a8`                 |
| Native Cargo denial, control size/x86 | 451,072 bytes, SHA-256 `dadab281f3a2ec0542933a5b32ed4b0bb11a3d9add2d3499be477a1e47dff61d`                                                        |

Run 39273777887 predates `df4a6e5c3` (detached SDK contents), so its SDK tree
digests differ between the two hosts. Runs from `df4a6e5c3` onward recorded
the same detached-tree digest on both. The Intel `clang` digest equals the ARM one in runs 39290139247 and
39417433680; other Intel tool digests are retained in the logs.

Each host also passed the real native Cargo denial test
(`full_macos::cargo_children_inherit_denial_macos`): build-direct,
build-grandchild and procedural-macro attempts each failed with `errno` 1
(`proof_errno` 1) under the boundary. The measurement reports
`descendant_cleanup: "post-run-reaped"`: after each run the sandbox instance's
processes are found, killed and the run is rejected if any survived. That is
weaker than a container owning its descendants, and only the `cfg(test)`
ordinary-CI tool selection accepts it (approved 2026-10-19). A qualified or
production selection still rejects the macOS boundary at the execute gate.

## Executables and Mach-O facts

Canonical builds used the same observed package, unit and build-script
counts in every run for a given host: ARM 486 packages, 649 units and 62
build-script executions; Intel 487, 650 and 62. `material_native_coverage_complete`
stayed `false`.

| Run         | Host  | Asset                     | SHA-256                                                            | Size (bytes) | Canonical build (s) |
| ----------- | ----- | ------------------------- | ------------------------------------------------------------------ | ------------ | ------------------- |
| 39273777887 | ARM   | `ts-aarch64-apple-darwin` | `65c68e611e3803cc9b80a0a48c218f629f3fa5752d787276a2b385a2f01874b9` | 43,260,288   | 663                 |
| 39273777887 | Intel | `ts-x86_64-apple-darwin`  | `c9da6682c713ecf597e5c85a2c01992af26294493b6249c6a9bb4a6fa5c6f54f` | 46,114,496   | 1,101               |
| 39290139247 | ARM   | `ts-aarch64-apple-darwin` | `071ba0375ade6195552efaf363a384c8dd8684a9cfd8033c8c207a2b34def59d` | 43,260,288   | 560                 |
| 39290139247 | Intel | `ts-x86_64-apple-darwin`  | `3bfa14727f66915b76e95ce5445d5e19b68d11be78ba8caf69be6c1e3011ac42` | 46,114,496   | 1,140               |
| 39417433680 | ARM   | `ts-aarch64-apple-darwin` | `923e5f7bddbfdb67596e45cdfe3adfcd5aaab353af94fb4f697d05ab4a32cd45` | 43,260,288   | 612                 |
| 39417433680 | Intel | `ts-x86_64-apple-darwin`  | `b892eb2a08c66c6ccbf945224a5e42ca1e59388ed0ae8b48d203c0a3c40ec367` | 46,114,496   | 879                 |
| 39420011152 | ARM   | `ts-aarch64-apple-darwin` | `1c28da620cb7654c5b376a7e09eff889fabc62cdc6e100a12b2c4d913c17e187` | 43,260,288   | 602                 |
| 39420011152 | Intel | `ts-x86_64-apple-darwin`  | `1fdceeae363ed7afe97f28056170240194eebcf1f3b216f7880add0d0fdf744d` | 46,114,496   | 914                 |

Mach-O inspection of the accepted canonical executable (identical across runs
except where noted):

| Check                  | ARM                                                                             | Intel      |
| ---------------------- | ------------------------------------------------------------------------------- | ---------- |
| CPU                    | `arm64`                                                                         | `x86_64`   |
| Minimum OS (`minos`)   | `13.0.0`                                                                        | `13.0.0`   |
| SDK                    | `15.5.0`                                                                        | `15.5.0`   |
| Signature              | `linker-signed-ad-hoc`                                                          | `unsigned` |
| Dynamic libraries      | Security, CoreFoundation, `libiconv.2`, `libSystem.B`                           | same four  |
| Symbol count           | 305,558                                                                         | 306,211    |
| Debug-map object files | 1,426                                                                           | 1,422      |
| Operator values        | Three planted fictional variables removed by preflight; zero final-byte matches | same       |

Mach-O UUIDs differ per run because the embedded source commit differs (for
example ARM `ea89f38109c83fbea6bed4ac7de8f98b` and Intel
`47678b08fa0a38bcba792ffc65173f1f` in run 39290139247). The final-byte scan
rejected host build roots, the fixture root and `RUNNER_TEMP`, and found only
the remapped prefixes `/trusted-server` and `/cargo-vendor`.

## Startup

The accepted canonical executable was run as `ts --version` outside the
checkout with a scrubbed environment (`HOME`, `LC_ALL`, `PATH`, `TMPDIR`
only). Every start, in every run on both hosts, reported `status: accepted`
with exit code 0 and `ts package=0.1.0 source=<merge commit> target=<triple> variant=default`, for example (run 39290139247, ARM):

```text
ts package=0.1.0 source=506d8ead69757556e1bd8ed29b578d0257d05169 target=aarch64-apple-darwin variant=default
```

The Intel output names `target=x86_64-apple-darwin`. The measurements record the host CPU and, on ARM, translation state
(`hw_optional_arm64: 1`, `proc_translated: 0`). This exercises loading and
version reporting on macOS 15.7.9 only. It does not exercise the macOS 13
loader floor on a real macOS 13 host or every CLI command.

## Reproduction measurement

The reproduction (role B) uses separate fresh roots and the checked ambient
profile (umask 027, two jobs, different roots and start time). It is a
bounded measurement, never a release equality gate.

| Run         | ARM reproduction                                 | Intel reproduction                                                                                                                          |
| ----------- | ------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------- |
| 39273777887 | `identical` (same SHA-256 and size as canonical) | `build-failed`: the confined Cargo build exceeded its wall-clock deadline (`sandbox-exec exceeded its wall-clock deadline`) while compiling |
| 39290139247 | `identical`                                      | `build-failed`: `Release source is not authenticated` caused by `source preparation cancelled or timed out`                                 |
| 39417433680 | `identical`                                      | `identical`                                                                                                                                 |
| 39420011152 | `identical`                                      | `identical`                                                                                                                                 |

No Intel equality result exists for the two earlier runs and none is inferred.
The two later Intel results were identical, but the whole Intel candidate test
took 3,552 seconds (run 39417433680) and 3,305 seconds (run 39420011152)
against a 60-minute test deadline, after 3,844 seconds in run 39290139247
where it was exceeded. No code change was made to widen that margin, so this
is a timing observation: it does not establish that Intel reproduction is now
reliable. Intel remains a bounded optional measurement.

## Subsequent reproduction scheduling

`3e3e30996` defers optional native reproduction on ordinary push/PR runs;
scheduled/dispatched runs and PRs carrying the reproduction label before a
triggering event measure it. Canonical Apple build/inspection/startup remain
mandatory. Run 39540057202 subsequently passed both canonical candidates, with
ARM reproduction identical and Intel reproduction failing at its 25-minute
confined-build wall. `b4c8e0c0f` raises that ordinary-CI wall to 45 minutes;
it does not establish Intel equality. See the [dated schedule report](2026-10-21-native-reproduction-schedule.md)
for exact jobs, built source and trigger limitations. Future equality statements
must cite the measured run/job and built source; a deferred pass supplies none.

## Limits

This evidence does not establish authenticated acquisition of Xcode, the SDK
or the Rust tree; original vendor identity; compiler resource, `libLTO` and
helper-tool coverage; production reproduction profiles; macOS 13 loader
behavior; or Task 8 qualification. The JS handoff came from a Linux ordinary-CI
artifact marked `authenticated: false`. Production wrappers and qualified
selection remain closed, and nothing here signs, publishes or enables an
official release.
