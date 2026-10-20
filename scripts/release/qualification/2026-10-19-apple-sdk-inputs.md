# Apple SDK input audit

Status: local candidate-input investigation, not hosted qualification or
authenticated vendor acquisition.

The selected local Xcode 16.4 installation exposes macOS SDK 15.5. An
independent read-only traversal confirmed the following layout on 2026-10-19:

| Observation                          | Count       |
| ------------------------------------ | ----------- |
| Entries below the selected SDK root  | 46,233      |
| Directories                          | 8,781       |
| Regular files                        | 30,727      |
| Regular-file bytes across all paths  | 656,877,420 |
| Largest regular file, bytes          | 8,899,657   |
| Relative internal symlinks           | 6,725       |
| Regular files with multiple links    | 8,605       |
| Special files                        | 0           |
| Absolute, dangling or escaping links | 0           |

These observations describe this installation only. They do not establish
either hosted runner's SDK layout, original distribution integrity or
release qualification.

## Selection consequence

The existing owned tool-tree policy rejects multiply linked regular files.
The installed SDK therefore cannot pass that policy directly. Hardlinked
headers are part of the input set; excluding documentation cannot solve the
problem. Hashing the original SDK before and after execution would also
leave an alias outside the selected tree able to modify its content.

The selected implementation preserves the existing owned-tree policy:

1. Copy the selected SDK into a fresh owned directory, reading vendor files
   through held descriptors and creating independent destination files.
2. Preserve permission bits and relative internal symlinks. Reject special
   files, unsafe aliases and exceeded bounds; do not preserve hardlinks or
   silently omit inputs.
3. Validate the detached tree with the existing 512 MiB per-file, 2 GiB
   total-byte and 100,000-entry limits.
4. Bind its content digest and lifetime to the selected Apple tools, select
   that exact SDK in the recipe, grant it a read-only boundary root and
   remap its temporary path to a fixed prefix.
5. Recheck it through the existing tool-verification hooks, using the
   caller's deadline and cancellation. Record the tree digest as structured
   installed-content evidence, not a raw archive checksum.

This closes selected SDK-content coverage only when the implementation and
its actual native ARM/Intel recipe executions pass. Compiler resources,
helper executables, toolchain libraries such as libLTO, operating-system
inputs, authenticated acquisition and full native/tool closure remain
separate requirements. Overall composition coverage must remain incomplete.

## Local detached-copy smoke

The real installed-SDK smoke passed in 168.94 seconds. It copied and
measured all 46,233 entries and 656,877,420 regular-file bytes, reverified the
selected copy while retaining a cloned tool selection, and confirmed that
the copy disappeared after the last owner was dropped. The observed
structured-tree SHA-256 is
`72007de53280923111d03a4b0277e2d3cc986ec9594cfcc3c9807f2e97d02fc8`.
This is installed-tree evidence, not a distribution checksum or approved
production pin.

The local machine lacks the selection symlink used by the CI constructor,
so this ignored smoke test used an explicit canonical developer directory
and the same private measurement/staging implementation. CI still requires
its selection symlink; no fallback or qualified constructor was added.

Pure regressions cover modified headers/stubs/frameworks, unsafe aliases,
hardlinks in the destination, exhausted bounds, cancellation, stable
path-independent identities, final-byte path leaks and cleanup of restrictive
directory modes on error and final-clone drop. A review finding caught the
read-only-directory cleanup leak before the full smoke; its regression was
observed failing before the cleanup guard was added.

## Hosted native recipe follow-up — 2026-10-20

[Run 39290139247](https://github.com/IABTechLab/trusted-server/actions/runs/39290139247)
finalized success for head `df4a6e5c30fe8c93df68748ead77a3bc559d16b2`.
Its actual merge source was `506d8ead69757556e1bd8ed29b578d0257d05169`.
Both Apple candidate recipes compiled, inspected and started their canonical
CLI artifacts using detached `/apple-sdk`, Xcode 16.4 and SDK 15.5.0.
Both emitted the same installed SDK tree identity:
`1482e88484d3ab4b430306607ed07113068eadd7581b232dedb20ea80842bce0`,
46,233 entries and 656,877,420 regular-file bytes. Their SDK-settings file
SHA-256 was `58a133735f0a55a624a1703067059f6e78925e51725ec6e1f966072e142c9c42`.
This hosted tree digest differs from the local smoke's installed tree;
neither is an authenticated distribution identity or production pin.

ARM's second build was byte-identical. Intel's canonical artifact and
retained receipt exports passed; its optional second build failed on its
time bound. No Intel reproducibility result is inferred from the green job.
The successful fixtures additionally joined verified Rust packages through
the typed SPDX projection and checked retained export integrity after
dropping the original build storage. Complete native/tool coverage and
release qualification remain false.
