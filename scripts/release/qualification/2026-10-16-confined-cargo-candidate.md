# Task 9: first confined candidate Cargo build

Status: on 2026-10-16 ordinary PR CI built `trusted-server-fastly.wasm` (role A)
through `PreparedRecipe::execute` inside the Linux Docker denial boundary. It
also proved that Cargo build scripts, a build-script grandchild and a
procedural macro inherit network denial. This is candidate execution evidence
from an ordinary CI runner and records `release_qualification: false`. It does
not qualify the toolchain, the image, the acquired crates, the vendored Git
manifests, bootstrap or any hosted release environment. Production tool
selections remain uninhabited, so production `execute` still returns
`Unqualified`.

## Run

The job
[`cargo test (confined release Cargo candidate, Linux)`](https://github.com/IABTechLab/trusted-server/actions/runs/38849941580/job/116808709738)
checked out the pull request merge commit
`309b9553657bd74c3e7dea03927284b68756c7e6` (branch head
`2d933b0e9debed3960f97f2cc3ebb5948da17471` merged into `main` at
`fa9837f2b95f817f58eadb16c929c38fcb581c9a`). Each step ran exactly one
selected ignored test, after a guard that the selection matched exactly one
test.

The fixture acquired 706 registry originals (each checked against its locked
SHA-256), one Git object store for the locked `edgezero` revision, and eight
normalized Git manifests from `cargo vendor --offline`. Cargo 1.95.0 wrote no
`Cargo.toml.orig` for Git packages, so each original manifest was read from
the Git object database and paired with its normalized form. The acquisition
was unauthenticated ordinary CI. The tool root was a private single-link copy
of the host Rust 1.95.0 sysroot with `wasm32-wasip1`; the image was Ubuntu
24.04 with `gcc` and `libc6-dev`, identified only by its image ID.

## Denial inheritance

`recipe::tests::full::cargo_children_inherit_denial` passed in 25.17 seconds.
A build script, a build-script grandchild and a procedural macro each
attempted a loopback connection inside the boundary. All three reported errno
1 (`EPERM`), matching the boundary's own denial proof, not `ECONNREFUSED`.
The control crate built a real `denial-control.wasm` (2,013,985 bytes, SHA-256
`60bd5bc1f0388a50a75dd3a0e87400282fb5c97499fc414a1e8dadd2343d8534`) through
the same `execute` path.

## JS handoff into the Fastly build

`recipe::tests::full::real_js_handoff_to_fastly_cargo_build` passed in 871.02
seconds. It first repeated the full two-fresh-installation JS acceptance: both
sides passed 1,275 tests including type checks, all 13 final modules were
byte-identical and six private roots were removed. It then bound that
in-memory JS completion to a third fresh source copy, prepared the vendor
directory, ran `cargo build --release --frozen
--message-format=json-render-diagnostics` with umask 022 and four jobs, and
completed only after the JS handoff, tools and materials rehashed unchanged.
No Cargo registry or Git cache appeared in the private `CARGO_HOME`. The
confined build took 130 seconds.

```json
{
  "artifact": {
    "name": "trusted-server-fastly.wasm",
    "sha256": "6a19dece46c11b40f352a0ac23a124a4f8d6af5742df56845d37a99dc2b713c5",
    "size": 66885152
  },
  "asset": "trusted-server-fastly.wasm",
  "boundary_fingerprint": "99af57442bb03eeaaa50c320d83dbe5bf8938de97d5eca1c9f62668c17170582",
  "cargo_lock_sha256": "9d1ce5338f2511e9ae289ceb4e1679a2330cda8c43f205d80b6d01672589e4dc",
  "derived_manifest_digests": 8,
  "elapsed_seconds": 130,
  "js_inventory_sha256": "f3645c7699165920ccc64bfc0f91c43479b01b771cfacf9b9bd4721bcaedf33e",
  "js_settings_sha256": "5d341e8a8b58a638ddc71b663daa0dbada5d1970175f21391b7750d36ffdef02",
  "js_tools_sha256": "ba26c9198ef338cf0afc94762d91cfaa3cdbd51987e65dfe0f8905980963da2d",
  "kind": "ordinary-ci-confined-cargo-build",
  "materials_digest": "da91ba65a309b9290460a2e3fb5fdf2dd6fee59f39225777264444932bf5ab17",
  "plan_digest": "a743a63a68cd1ace19f434bae98cf4fc1fafcfe5913e141c5196e7302255afa8",
  "release_qualification": false,
  "role": "A",
  "rust_tools_digest": "0a741d4f4c3b4f9087da3891120b3557925573867341cdf826cb0ce56f883879",
  "schema_version": 1,
  "source_commit": "309b9553657bd74c3e7dea03927284b68756c7e6",
  "stderr_sha256": "ba7154da1ed3e07bfa14e0e6ab0c894684fbe7b4eeccc903262f7e1db0cfca1c",
  "stdout_sha256": "9fb08fe022a5fccef541d756acc85df5a6480c9987b29275a7cdc86166953efc"
}
```

Eight derived manifest digests mark this build unqualified: the normalized
`edgezero` manifests came from `cargo vendor`, not from a reviewed producer
normalization.

## Archive policy change

Real registry crates required two narrowly reviewed compatibility encodings,
for registry originals only: a GNU `././@LongLink` record (regular mode,
exact join with the next header's 100-byte name, bounded, never consecutive,
dangling or mixed with PAX) and tar modes that carry the regular or directory
type bits. npm originals keep the stricter policy. Git packages are vendored
under their bare name so that intra-workspace relative paths resolve.

## Not covered

Role B and the A/B comparison, the three `ts` executables, path remapping,
final-byte Viceroy acceptance, producer-owned manifest normalization,
authenticated acquisition, qualified tool and image pins, and hosted
qualification remain open under Tasks 8 and 9.
