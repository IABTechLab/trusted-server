# Task 9: Linux CLI candidate builds, inspects and starts cleanly

Status: ordinary PR CI passed the Linux `ts` candidate recipe on 2026-10-16.
The canonical and reproduction builds produced byte-identical executables.
The exact accepted canonical bytes passed final-path and ELF inspection and
started outside the checkout in all three reviewed Linux images. This is
candidate evidence with `release_qualification: false`; it does not qualify
production tools, image pins, acquisition or release enablement.

## Subsequent reproduction scheduling

This report retains its original equality observation. `3e3e30996` later
defers optional native reproduction on ordinary push/PR runs, measuring it on
scheduled/dispatched or appropriately labelled PR runs instead. Canonical
build/inspection/startup remain required. The [dated schedule report](2026-10-21-native-reproduction-schedule.md)
records the triggers, cache limits and subsequent 45-minute ordinary-CI per-build
wall. Future equality results must cite the measured run/job and built source;
a deferred run does not repeat this report's equality proof.

## Run and artifact

- [Successful candidate job](https://github.com/IABTechLab/trusted-server/actions/runs/38921701566/job/117042987142).
- Branch head: `0299c3a17812ce78def6b8df479f091a27120ce8`.
- Built source (PR merge commit): `4a6404b072a3bc4bd36a12b33e4201c5f5797522`.
- Asset: `ts-x86_64-unknown-linux-gnu`, variant `default`, package `0.1.0`.
- SHA-256, both builds: `a3a64d15ee1663ff747a21e0edc089703183a0a8685740ec65592d9fdd3a34b1`.
- Size, both builds: 285,604,680 bytes.
- Complete candidate test: 1,352.55 seconds; canonical Cargo execution: 293 seconds.
- Lock SHA-256: `c37beb7694512de99ac99c8ac214fa01f56f32226810e814e7cc26a717c8eedd`.
- Shared JS inventory SHA-256: `b0632a21ad06c0bec46be365aba30b62f89e8153a33e5517ce4109eca30793eb`.

The job logs retain `linux-cli-candidate-measurement.json` and
`builder-filesystem-controls.json`, including tool/material/plan digests,
image IDs, debug-section sizes, startup output and reproduction comparison.
The source commit above, rather than the later documentation commit, is the
identity embedded in and measured for this executable.

## Actual build and boundary

The producer/probe itself was rebuilt inside the GNU baseline from the
tracked source and copied pinned Rust toolchain using cached offline Cargo
inputs. Both CLI roles then used `PreparedRecipe::execute` in the selected
UBI builder with glibc 2.34. The candidate acquired inputs through ordinary
unauthenticated CI preparation, including eight producer-normalized Git
package manifests and zero caller-derived manifests.

Builder base: `registry.access.redhat.com/ubi9/ubi:9.6`.
Observed builder ID:
`sha256:c92872ae3dc77cff493a6d50ba5835961c03ba7f50aac94b5c3e643ddc8e7e71`.
The immutable image configuration was checked for absent/empty declared
volumes. Cargo, compiler and build children ran under the established
socket-denial boundary with no network and a read-only container root.

Actual filesystem controls exited zero after observing:

| Location                                          | Observed control                                         |
| ------------------------------------------------- | -------------------------------------------------------- |
| `/tmp`                                            | Write denied: read-only filesystem                       |
| `/dev/shm`                                        | Write denied: directory absent with IPC disabled         |
| `/dev`                                            | Write denied: permission denied                          |
| `/etc/hosts`, `/etc/hostname`, `/etc/resolv.conf` | Append-open denied: permission denied                    |
| Bound `TMPDIR`                                    | Write, read and removal succeeded; no test file remained |

These checks precede the real fresh JS and native builds. Global `/tmp` is
not writable scratch storage. The JS containment test uses a real symlink
between owned sibling directories relative to its configured source root;
it no longer clears the supplied `TMPDIR` to reach global temporary storage.

The separate real timeout regression passed in 5.05 seconds. It starts only
after Docker acknowledges a full container ID, times out an attached child,
and verifies removal of that owned container. Build and startup cleanup use
the same acknowledged-create lifecycle; a failed/unresolved creation remains
fatal rather than being cleared by an early absence check.

## Final bytes and startup

Inspection of the copied final executable found:

| Check                          | Measured result                                                                 |
| ------------------------------ | ------------------------------------------------------------------------------- |
| Architecture                   | x86-64 ELF                                                                      |
| Interpreter                    | `/lib64/ld-linux-x86-64.so.2`                                                   |
| Highest required GLIBC version | `2.34` across 27 version requirements                                           |
| Needed libraries               | `libgcc_s.so.1`, `libm.so.6`, `libc.so.6`, `ld-linux-x86-64.so.2`               |
| `.debug_info`                  | 87,973,465 file-backed bytes                                                    |
| `.debug_line`                  | 33,681,096 file-backed bytes                                                    |
| Remapped prefixes retained     | `/trusted-server`, `/cargo-vendor`, `/cargo-target`                             |
| Build paths                    | No bound host/execution roots, `/boundary/`, fixture roots or `RUNNER_TEMP`     |
| Operator values                | Three planted fictional variables removed by preflight; zero final-byte matches |

Bound-root rejection and remap evidence are mandatory inside recipe
completion, before an accepted artifact can be returned. The candidate also
scans fixture/runner roots. The GNU-only generic `/tmp/` policy permits the
measured Chromiumoxide runtime Chrome lookup constant `/tmp/aws/lib`; it does
not permit the actual build roots. The closed scratch-storage controls above
support that distinction. macOS retains its separate temporary-path policy.

The accepted executable was copied read-only, hashed before and after each
startup, and executed as `ts --version` without a checkout or operator config.
All starts exited zero with empty stderr and identical expected output:

```text
ts package=0.1.0 source=4a6404b072a3bc4bd36a12b33e4201c5f5797522 target=x86_64-unknown-linux-gnu variant=default
```

| Startup image                                     | Observed local image ID                                                   |
| ------------------------------------------------- | ------------------------------------------------------------------------- |
| `registry.access.redhat.com/ubi9/ubi-minimal:9.6` | `sha256:a2c5a85865a585c3bc8b10f6c269358fdf89fe32be4232885166889d85c76421` |
| `docker.io/library/ubuntu:22.04`                  | `sha256:e8df6c74f650ad385efe2e20103eca9b0ad50690fb3c63d49fa00762412bbf55` |
| `docker.io/library/debian:12`                     | `sha256:94926f0fd968cd68ac486502185a5dd78687874a9ec7727f64fe2af696e28311` |

These are observed IDs from ordinary CI tag acquisition, not authenticated
production digest selections. Startup uses a non-root user, no network,
read-only mounts/root, dropped capabilities and bounded execution/cleanup.
This exercises executable loading and version reporting, not every CLI
command or a browser installation.

## Reproduction and regression coverage

The reproduction used separate fresh checkout, vendor, Cargo-state, target
and output roots from the shared JS handoff. The checked pairing required
identical source, lock, tools, material closure and JS inputs, with the
supported ambient differences recorded honestly: canonical umask 022/jobs 4
versus reproduction umask 027/jobs 2, distinct host roots and start times.
Container paths remain fixed; locale/timezone are normalized and execution
user variation is unsupported here. Final-byte path inspection supplies
evidence beyond equality under those fixed container paths.

The measured native result was `identical`, with no differing offset/section.
Native reproduction remains a measurement, rather than a release equality
gate; this successful pair does not qualify a general reproducibility claim.

The [Linux release-tooling job](https://github.com/IABTechLab/trusted-server/actions/runs/38921701566/job/117042986647)
also passed its actual two-fresh-installation JS proof in 702.81 seconds:
1,275 tests per side, 13 matching bundles and six private roots removed. Its
registered native producer tests passed; prebuilt-JS tests recorded 37 passed
and one ignored. The [companion Fastly job](https://github.com/IABTechLab/trusted-server/actions/runs/38921701566/job/117042986628)
passed Cargo build-script, grandchild and procedural-macro
network-denial attempts, including a successfully compiled control retaining
an actual compiler-remapped source filename. It also passed complete Fastly
canonical/reproduction equality and both final-byte Viceroy acceptance gates.
Tests, formatting, integration tests and CodeQL all passed on branch head
`0299c3a17812ce78def6b8df479f091a27120ce8`; this report is a later
documentation-only update.

## Remaining scope

Task 9 and milestone M4 remain open. The two Apple CLI recipes and native
startup acceptance, qualified production ambient profiles, independently
authenticated acquisition/tool/image selections, Task 8 hosted qualification
and subsequent executable composition/evidence work are still required.
Production wrappers and qualified selection remain closed. This candidate
does not sign, publish, deploy or enable an official release.
