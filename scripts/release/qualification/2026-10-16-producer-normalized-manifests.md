# Task 9: producer-normalized Git manifests

Status: on 2026-10-16 ordinary PR CI built the Fastly canonical and
reproduction outputs using Git package manifests that the producer normalized
itself from the verified Git tree, instead of manifests supplied by
`cargo vendor`. Both builds report zero caller-derived manifests. This is
candidate evidence from an ordinary CI runner and records
`release_qualification: false`; the toolchain, image, acquisition and the
production reproduction profile remain unqualified.

## Run

The job
[`cargo test (confined release Cargo candidate, Linux)`](https://github.com/IABTechLab/trusted-server/actions/runs/38884296635/job/116918199285)
checked out the pull request merge commit
`5e2c68a5e30c6b4da7fa8ae83faf30c96c9511f3` for branch head
`bf3057d60f99a92693c0836305129ba68962afd5`. The
ignored test
`recipe::tests::full::fastly_canonical_and_reproduction_builds_are_byte_identical`
passed in 1,103.54 seconds and the denial-inheritance test passed again.

## Results

- The producer resolved workspace inheritance for all eight locked `edgezero`
  packages: package fields, workspace dependencies with merged features and
  relocated paths, and workspace lints. The originals are kept as
  `Cargo.toml.orig`.
- Before each build, an evidence-only differential compared every producer
  manifest with the one `cargo vendor` writes, on package identity, `links`,
  features, every dependency's source, version, rename, optionality, default
  features and features, and the build-script and procedural-macro settings:
  8 compared, 0 differing, for both roles.
- Each build recorded 8 producer-normalized manifest digests and 0
  caller-derived manifest digests, so no build is marked unqualified for its
  manifests.
- The canonical (133 s) and reproduction (166 s) builds were byte-identical:
  66,833,914 bytes, SHA-256
  `8df10dfc2373dbec38fe5a21bdf6153dbb49bec51368034c475350c223368d88`.
- The Viceroy final-byte gate accepted both outputs and rejected the
  closed-origin control as a health-only success.

`cargo vendor` now supplies only comparison fixtures; nothing it writes is
trusted by the build.

## Not covered

The three `ts` executables, the production reproduction profile, an
independently reviewed Viceroy digest, authenticated acquisition, qualified
pins and hosted qualification remain open under Tasks 8 and 9.
