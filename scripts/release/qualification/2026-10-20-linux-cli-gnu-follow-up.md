# Task 9: Linux CLI candidate after the GNU snapshot and measurement fixes

Status: ordinary PR CI passed the Linux `ts` candidate recipe again on
2026-10-20 after two hosted failures were fixed. The canonical and
reproduction builds were byte-identical and the exact accepted bytes started
in all three reviewed Linux images. This is candidate evidence with
`release_qualification: false`. It extends the
[2026-10-16 Linux CLI candidate report](2026-10-16-linux-cli-candidate.md) and
does not qualify production tools, image pins, acquisition or release
enablement.

## Why the earlier candidate stopped passing

After the 2026-10-16 pass, the candidate recipe gained source-owned GNU tool
selectors and a staged helper that records a bounded installed-image
inventory before and after Cargo. Two hosted runs then failed in the Linux CLI
candidate job; each failure was fixed by one commit.

| Run                                                                                  | Head        | Failed job                                                                                             | Recorded failure                                                                                                                                        | Fix                                                                         |
| ------------------------------------------------------------------------------------ | ----------- | ------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------- |
| [39371724671](https://github.com/IABTechLab/trusted-server/actions/runs/39371724671) | `7be86d935` | [118460869596](https://github.com/IABTechLab/trusted-server/actions/runs/39371724671/job/118460869596) | `cannot open image file: Permission denied (os error 13)` after 557.93 seconds                                                                          | `e5a780d5c`: record three reviewed root-only helpers by metadata only       |
| [39382624785](https://github.com/IABTechLab/trusted-server/actions/runs/39382624785) | `e5a780d5c` | [118494982595](https://github.com/IABTechLab/trusted-server/actions/runs/39382624785/job/118494982595) | `file .../release/build/prettyplease-.../build-script-build has 2 hard links; a link outside the root would bypass write denial` after 1,273.09 seconds | `bfab8ef20`: measure the image in a sibling sandbox without the target root |

Both failures were in the real candidate test
`recipe::tests::cli::linux_cli_candidate_builds_inspects_and_starts_cleanly`;
other jobs in those runs are not summarised here. Run 39371724671 also had a
cancelled Intel macOS job, which is outside this report.

### Reviewed root-only helpers (`e5a780d5c`)

The confined GNU helper runs unprivileged and hashes every regular file under
the measured image roots. The UBI 9 builder ships three privileged helpers it
cannot open: `/usr/libexec/utempter/utempter` (setgid, mode 2711),
`/usr/sbin/userhelper` (setuid, 4711) and `/usr/sbin/unix_update` (0700),
observed identically on amd64 and arm64 by a local Podman build of the same
builder Dockerfile, listing files unreadable to a non-root user under the
measured roots (local diagnostic, not CI output). The snapshot now accepts
a permission-denied file only at those exact paths, owned by root, with no
group or other write bit, no other-read permission and unchanged since first
inspection. Each is recorded by mode, owner and size instead of content
(inode is excluded because it varies with host storage) and listed as
`content_unmeasured_files`, which the orchestrator requires to be a sorted
subset of the reviewed paths. Every other unreadable file, including an
execute-only compiler helper, still fails closed, and selected build tools
must remain readable and hashed. These three files' content is therefore
unmeasured; the SPDX candidate states the count as a coverage gap.

### Sibling measurement sandbox (`bfab8ef20`)

The second GNU image measurement ran in the build boundary, whose unchanged
pre-run writable scan rejects Cargo's in-root hard links in the target
directory. Both GNU measurements now run in a sibling Linux container
boundary derived from the build boundary: same image, settings, read-only and
tool roots, and every writable root except the last (the target directory).
The sibling has a fresh fingerprint, records the boundary it derived from and
proves denial with the staged probe. Cargo queries and the build keep the
build boundary and proof, receipts record both fingerprints, and completion
still binds the build boundary and plan.

## Passing runs and jobs

| Run                                                                                  | Head                                       | Built source (PR merge commit)             | Linux CLI job                                                                                          | Test duration |
| ------------------------------------------------------------------------------------ | ------------------------------------------ | ------------------------------------------ | ------------------------------------------------------------------------------------------------------ | ------------- |
| [39417433680](https://github.com/IABTechLab/trusted-server/actions/runs/39417433680) | `bfab8ef2009fa6f28b1258135e2445ee22775dc8` | `b05bd8a18f137b43f7542c797f0a5d7856363d23` | [118609761876](https://github.com/IABTechLab/trusted-server/actions/runs/39417433680/job/118609761876) | 2,124.97 s    |
| [39420011152](https://github.com/IABTechLab/trusted-server/actions/runs/39420011152) | `1719cf15fd58fcf029f6b4ae5003215802bef405` | `b2273db76b8a2070f3c0d20d22788eed434d0029` | [118618316864](https://github.com/IABTechLab/trusted-server/actions/runs/39420011152/job/118618316864) | 2,089.82 s    |

Both runs concluded `success` for every job, including the Fastly confined
Cargo job, the native producer jobs on Ubuntu 24.04 and macOS 15, and the
Apple candidate jobs (see the [Apple CLI candidates report](2026-10-20-apple-cli-candidates.md)).
The run for the later documentation-and-SPDX head `073704cc7` (Run Tests 39452090186) was still in progress when this report was written and is not
recorded.

## Measured results

| Measurement                           | Run 39417433680 (`bfab8ef20`)                                             | Run 39420011152 (`1719cf15f`)                                             |
| ------------------------------------- | ------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| Asset                                 | `ts-x86_64-unknown-linux-gnu`, variant `default`, package `0.1.0`         | same                                                                      |
| SHA-256, canonical and reproduction   | `066000e58902ad9e8b2077498383c27f466fd45337320082eab20da83a28df7d`        | `99991c0ae4a34b0db1630925df5e0ca1ef4aa0c3ca17f30d9a162ed746923245`        |
| Size, both builds                     | 282,956,784 bytes                                                         | 282,956,920 bytes                                                         |
| Reproduction status                   | `identical`, no differing offset                                          | `identical`, no differing offset                                          |
| Canonical Cargo execution             | 473 seconds                                                               | 454 seconds                                                               |
| Observed packages/units/build scripts | 486 / 649 / 62                                                            | 486 / 649 / 62                                                            |
| Lock SHA-256                          | `82ef943339970e6399fe56f434beee484a05e7a0fe0db40383a794651ab19214`        | `82ef943339970e6399fe56f434beee484a05e7a0fe0db40383a794651ab19214`        |
| Descendant cleanup                    | `owned-by-container`                                                      | not tabulated; same boundary                                              |
| Builder image                         | `registry.access.redhat.com/ubi9/ubi:9.6`, glibc 2.34                     | same reference, glibc 2.34                                                |
| Observed builder image ID             | `sha256:417c122225da0b6990c9f50c32a7b3908022e30df6d5480b572b45be9dd4d03c` | `sha256:b4eae457404e63633888ffaff3cd5020c6ffb4cc4fb495657a8945698ebdd5a0` |

The builder image ID differs between the two runs for the same mutable tag
and both differ from the 2026-10-16 report, which is further reason these are
observations from ordinary tag acquisition and not authenticated production
digests. The executable bytes differ between runs because the embedded source
commit differs, and the size and digest of this binary also differ from the
2026-10-16 report because the source and lock changed.

Final-byte ELF inspection of the canonical executable (run 39417433680; run
39420011152 reported the same ELF facts): x86-64, interpreter
`/lib64/ld-linux-x86-64.so.2`, highest required GLIBC version `2.34` across 27
version requirements, needed libraries `libgcc_s.so.1`, `libm.so.6`,
`libc.so.6` and `ld-linux-x86-64.so.2`, `.debug_info` 87,947,690 bytes and
`.debug_line` 33,663,154 bytes. The path scan found only the remapped
prefixes `/trusted-server`, `/cargo-vendor` and `/cargo-target` and none of
`/boundary/`, host build roots, fixture roots or `RUNNER_TEMP`; the three
planted fictional operator variables were removed by preflight with zero
final-byte matches. `material_native_coverage_complete` stayed `false`.

The builder filesystem controls again exited zero: `/tmp`, `/dev/shm`, `/dev`,
`/etc/hosts`, `/etc/hostname` and `/etc/resolv.conf` writes denied, and the
bound `TMPDIR` accepted write, read and cleanup.

## Startup

The copied read-only executable ran as `ts --version` without a checkout or
operator configuration in each reviewed image. All starts exited zero with an
empty stderr (SHA-256 of empty input) and, in run 39420011152:

```text
ts package=0.1.0 source=b2273db76b8a2070f3c0d20d22788eed434d0029 target=x86_64-unknown-linux-gnu variant=default
```

| Startup image                                     | Observed local image ID (both runs)                                       |
| ------------------------------------------------- | ------------------------------------------------------------------------- |
| `registry.access.redhat.com/ubi9/ubi-minimal:9.6` | `sha256:a2c5a85865a585c3bc8b10f6c269358fdf89fe32be4232885166889d85c76421` |
| `docker.io/library/ubuntu:22.04`                  | `sha256:cee13550f97b8a595506e8e4ca74263ba44924248779a4ba4f942e197652acff` |
| `docker.io/library/debian:12`                     | `sha256:94926f0fd968cd68ac486502185a5dd78687874a9ec7727f64fe2af696e28311` |

The `ubuntu:22.04` image ID differs from the 2026-10-16 report
(`sha256:e8df6c74...`), showing the tag moved between runs. Run
39417433680 recorded the same three IDs. The real startup timeout regression
(`startup_timeout_removes_only_its_owned_container`) also passed in run 39417433680.

## Subsequent reproduction scheduling

The equality observations above remain tied to their original runs. Since
`3e3e30996`, ordinary push/PR runs defer the optional native reproduction;
scheduled/dispatched or appropriately labelled PR runs measure it. Linux
canonical build, inspection and all three startup checks remain mandatory.
See the [dated schedule report](2026-10-21-native-reproduction-schedule.md)
for the selector, exact baseline jobs and the subsequent 45-minute ordinary-CI
per-build wall. Later equality claims need a measured run/job and built source,
not a successful deferred job. GNU helper coverage and qualification gaps remain.

## Limits

The GNU installed-file snapshot and the sibling measurement record which files
were present and, except for three root-only helpers recorded by metadata, their
content hashes. They do not establish which helpers executed, authenticated
acquisition of the UBI image or tools, or a complete native closure. The
candidate SPDX for this build must list the privileged-helper gap. Native
reproduction remains a measurement rather than a release gate. Task 9 and
milestone M4 remain open: qualified production ambient profiles, the closed
`build.sh` interface, independently authenticated acquisition/tool/image
selections and Task 8 hosted qualification are still required. This candidate
does not sign, publish, deploy or enable an official release.
