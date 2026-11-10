# Phase 1 attested release binaries

**Issue:** [#1235 — Publish attested release binaries with build provenance and SBOMs](https://github.com/IABTechLab/trusted-server/issues/1235)

**Date:** 2026-10-09. **Phase 1 revision:** 2026-11-05.

**Status:** Draft for maintainer review. PR #1435 carries this specification
and its implementation. Official release enablement additionally requires the
maintainer settings in [Maintainer gates](#maintainer-gates).

## Background and later phases

The original combined proposal, PR #1262, is closed and frozen at
`122b78664b1235c717e9208fd200f9a9b0c335df`. Its reviewed
[destination design](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/docs/superpowers/specs/2026-10-09-1235-release-attestation-design.md)
and [implementation plan](https://github.com/IABTechLab/trusted-server/blob/122b78664b1235c717e9208fd200f9a9b0c335df/docs/superpowers/plans/2026-10-13-attested-release-implementation.md)
remain the reference for later phases. Each later phase lands as its own PR
with its own specification. Requirement names below (R-CONFIG, R-SIGN and so
on) match the destination design, so its full clauses can be found there.

This document specifies only Phase 1. Phase 1 makes no reproducibility claim and
has no custom verifier. The phase boundaries follow
[the round-1 architecture review](https://github.com/IABTechLab/trusted-server/pull/1262#pullrequestreview-5484754857):

| Reviewer boundary                                     | Phase 1                                   | Later                                       |
| ----------------------------------------------------- | ----------------------------------------- | ------------------------------------------- |
| 1. Controlled build and composition evidence          | Pinned, locked builds; conservative SBOMs | Controlled recipes, qualification, evidence |
| 2. Signing and immutable publication                  | `actions/attest` and an immutable release | A reviewed publication owner                |
| 3. Verifier library, local policy and source approval | Operators run GitHub CLI verification     | The custom consumer and policy              |
| 4. Opaque verified artifact with retained bytes       | Not started                               | Custom consumer phase                       |
| 5. EdgeZero prepared payload and resolved context     | Not started                               | Later phase; does not block #1235           |
| 6. Observed operation state and recovery record       | Not started                               | Later phase; does not block #1235           |

## Scope

Phase 1 publishes four binaries from one tag-triggered workflow run:

| Asset                         | Package                         | Target                     | Runner         |
| ----------------------------- | ------------------------------- | -------------------------- | -------------- |
| `trusted-server-fastly.wasm`  | `trusted-server-adapter-fastly` | `wasm32-wasip1`            | ubuntu-24.04   |
| `ts-x86_64-unknown-linux-gnu` | `trusted-server-cli`            | `x86_64-unknown-linux-gnu` | ubuntu-24.04   |
| `ts-aarch64-apple-darwin`     | `trusted-server-cli`            | `aarch64-apple-darwin`     | macos-15       |
| `ts-x86_64-apple-darwin`      | `trusted-server-cli`            | `x86_64-apple-darwin`      | macos-15-intel |

Each binary ships with an SPDX 2.3 SBOM and two Sigstore bundles (provenance
and SBOM). With `SHA256SUMS` the release has **17 immutable assets**.

Out of scope for Phase 1, and owned by later phases: two-build
reproducibility, confined recipes, platform qualification beyond the startup
checks below, complete composition, an evidence index, the custom verifier,
the publication owner, source-branch eligibility, and release distributions for
Axum, Cloudflare and Spin.

## Release flow

`.github/workflows/release.yml` is the caller. It runs on pushes to `main` and
`v*` tags, never on pull requests, with `concurrency` grouped by ref and
`cancel-in-progress: false`. Pull requests run the release Node suite in the
`release scripts` job of `format.yml`.

The caller invokes the local reusable `.github/workflows/release-build.yml`,
which owns building and signing:

1. **checks** (`contents: read`) runs `node --test scripts/release/phase1.test.mjs`.
   On a tag it refuses a tag that is not strict SemVer, a tag reference that is
   not a lightweight tag at the built commit, and a tagged commit that is on no
   branch. All three fail before any build or signing writes public
   attestations.
2. **build** (`needs: checks`, `contents: read`) builds each matrix entry with
   `scripts/release/phase1-build.sh` under `env -i`. Only tag builds upload
   their binary and SPDX document.
3. **sign** (tag pushes with `RELEASE_PUBLICATION_ENABLED == 'true'` only;
   `id-token: write`, `attestations: write`) downloads each binary's two files,
   accepts only two regular, non-symlink, non-empty files, records their digests,
   and runs two `actions/attest` calls: SLSA provenance over the binary and its
   SPDX file, then an SPDX SBOM attestation for the binary. It never checks out
   or runs source.

The caller's **release** job (same gate, `contents: write` only) checks out only
`scripts/release/publish.mjs`, `scripts/release/release-notes.md` and
`.tool-versions`. It installs a checksum-verified GitHub CLI 2.102.0, asserts
the pinned Node version, and runs the publisher.

A `main` push runs checks and builds, including the startup checks, without
signing or publication.

## Requirements

### R-CONFIG — no operator configuration in the binaries

The build script refuses `TSJS_SKIP_BUILD`, `TSJS_PREBUILT_DIR` and `TSJS_TEST`,
and runs under `env -i`. GNU builds run in a container that receives only an
explicit `--env` list. The script plants fictional canary values in the
deployment environment variables and in a temporary `trusted-server.toml`, and
every final binary is scanned for each canary. `Cargo.lock`, the JS
`package-lock.json` and `edgezero.toml` must be unchanged after the build.

These are regression checks, not a hermeticity claim. There is no Rust source
audit in Phase 1; source-level auditing belongs to the later recipe phases.

### R-INPUTS — build inputs

An input audit was made against main at
`f9281917a7260b723f5f22175911c6d2147ef650`. It is not re-run automatically, and
locked versions have moved since (for example, the EdgeZero CLI is now locked at
tag `v0.0.11`). The input classes it found are:

- Pinned Rust 1.98.1, the tracked `Cargo.toml`, `Cargo.lock`,
  `.cargo/config.toml`, tool pins, source tree and release profile.
- Core's `build.rs`, `build_support/git_version.rs`, source tree, root
  manifest and lockfile, and tracked `edgezero.toml`. The last supplies the
  logical `trusted_server_config` store ID, not a physical binding.
- The JS build: `build/bundle_set.rs`, its declared sources, manifests and
  lockfile, `lib/build-all.mjs` and `lib/build-module-options.mjs`. A fresh
  `npm ci` precedes compilation. Cargo embeds modules from a private
  `OUT_DIR/tsjs-dist`; `crates/trusted-server-js/dist/` is not a release input.
- Embedded assets: the GPT bootstrap, the CLI consent stub and ad-template
  collector, and the fictional example-config scaffold.
- Dependency-generated code, including the EdgeZero CLI's adapter registry.

No record of these inputs is published. A complete native tool closure is not
claimed.

### R-SBOM — one SPDX document per binary

Each `<asset>.spdx.json` is SPDX 2.3. Its single described root has the final
binary's SHA-256 and Cargo's package version (the release tag stays external).
It conservatively inventories the selected Cargo target and the installed npm
build tree. The pipeline is checksum-pinned cargo-cyclonedx 0.5.9 and
`npm sbom`, merged in `phase1.mjs`, converted by checksum-pinned cyclonedx-cli
0.33.1 and completed and validated by `phase1.mjs validate-spdx`. The merged
inventory records no dependency relationships. The signed SBOM predicate must equal the
shipped SPDX file.

### R-FACTS — build facts

Core's build script requires `TRUSTED_SERVER__RELEASE_BUILD=1` with a full
40-character lowercase `TRUSTED_SERVER__GIT_VERSION`, and refuses anything else
in release mode. Release `x-ts-version` is the full source commit. `ts --version`
prints `ts package=<version> source=<commit> target=<triple> variant=default`;
the CLI's build script supplies the Cargo target. Tags and run IDs stay external.

### R-PLATFORM — targets and startup checks

All builds use `--release --locked` and strip native debug info
(`CARGO_PROFILE_RELEASE_STRIP=debuginfo`). The wasm keeps its profile.

- **Linux GNU:** built in a container from a digest-pinned UBI 9.6 (glibc 2.34)
  base image; its compilers and build tools come from an unpinned `dnf install`,
  and the build log records the resulting image ID. The final bytes must be a
  64-bit x86-64 ELF with no `GLIBC_` requirement above 2.34. `ts --version` must
  match in digest-pinned Debian 12, Ubuntu 22.04 and UBI 9 containers.
- **macOS:** `MACOSX_DEPLOYMENT_TARGET=13.0`. The final Mach-O must match the
  target CPU and carry a 13.0 minimum. `ts --version` must match on the runner.
  The binaries are not notarized.
- **Fastly:** wasm magic and version are checked. Hosted runtime qualification
  is later work.

### R-SOURCE — tags

Release tags are strict SemVer with a `v` prefix and no build metadata
(`vMAJOR.MINOR.PATCH[-PRERELEASE]`), and must be lightweight tags at the built
commit. The build `checks` job and the publisher both enforce these rules. The
`checks` job also requires the tagged commit to be on at least one branch: in
fork rehearsals, draft creation returned HTTP 403 for a commit reachable only
through its tag. Which branches may be released (for example, whether rc
branches may be tagged) is a maintainer decision; Phase 1 does not check which
branch holds the commit, and does not read rulesets.

### R-SIGN — signer

The reusable `release-build.yml` is the certificate signer. Operators pin it
with `gh attestation verify --signer-workflow
<repo>/.github/workflows/release-build.yml`, together with the source ref,
source and signer digests and the predicate type. Actions are referenced by
major version tag.

### R-PUBLISH — immutable publication

Publication requires the repository variable `RELEASE_PUBLICATION_ENABLED=true`.
There is no settings token. The publisher:

1. Checks that each signed handoff's `SHA256SUMS` receipt matches its four files.
2. Runs `gh attestation verify` for every binary and predicate with the signing
   bundles it is about to publish, and requires the signed SBOM predicate to
   equal the shipped SPDX file. Nothing exists yet, so a failure is safe to rerun.
3. Checks the tag reference, then refuses any existing release or draft for the
   tag.
4. Creates one draft, logs its ID before checking it, and uploads the 17 assets
   by that ID, checking each upload's size and SHA-256.
5. Publishes by ID. SemVer prereleases are prereleases and never Latest; stable
   tags use `make_latest: legacy`.
6. Reads the release by ID and requires its identity, release type and
   `immutable == true`.
7. Verifies the release attestation with `gh release verify`. Its verified
   statement must name this repository, tag, release ID and commit, and list
   exactly the 17 published assets with their local SHA-256 digests.

Mutations are never retried. The two post-publish reads retry any failure, at
most five attempts within one three-minute window; a returned result that fails
a check stops at once. The guide lists the exact bounds and the read-only
recovery procedure.

### R-MANUAL — operator route

The [verification guide](../../guide/release-verification.md) documents online
and offline verification, using the verified CLI, and a manual Fastly deployment
that packs the verified wasm unchanged. Local Viceroy execution proves a local
route only; no provider submission has been qualified.

## Interim implementation

The release producer is Node (`scripts/release/phase1.mjs`) and Bash
(`scripts/release/phase1-build.sh`), and the publisher is Node
(`scripts/release/publish.mjs`). These are interim. The destination design's
Rust producer and publication owner replace them in later phases.

## Maintainer gates

Before `RELEASE_PUBLICATION_ENABLED` is set, a maintainer must enable immutable
releases, add an active `v*` tag ruleset restricting creation, update and
deletion to the release team, and approve tag-push authority and required
checks. Until the tag ruleset is active, any writer who can push a `v*` tag can
start that tag's own `release.yml`. The guide's checklist gives read-only
commands that confirm each setting.

## Rehearsal

Rehearse the reviewed commit in a separate public repository with Actions and
immutable releases enabled and `RELEASE_PUBLICATION_ENABLED=true`. Push the
commit to a branch there, then use a fresh lightweight `v0.0.0-rehearsal.N` tag
for each attempt. A rehearsal qualifies only the commit, workflows and binaries
it ran; a later change to any of them needs a new rehearsal before the first
production tag. A fork rehearsal does not qualify the upstream repository's own
signer identity, settings or immutable-release state.

## Test plan

`node --test scripts/release/phase1.test.mjs` (the `release scripts` CI job)
and `shellcheck scripts/release/phase1-build.sh` cover:

- canary scanning, SBOM merge, SPDX completion and validation;
- the build script's preflight refusals, cleanup and step order, run under
  `/bin/bash` (Bash 3.2 on macOS, where a failing `[[ ]]` alone does not stop
  `set -e`);
- final-byte format checks for each target, including a glibc ceiling read from
  `readelf` output;
- workflow structure: triggers, job permissions, gates, tag checks, the asset
  matrix and the publisher's sparse checkout;
- the publisher against a fixture GitHub API and GitHub CLI, including the exact
  create and publish bodies, signing-bundle verification, every refusal before
  creation, and the post-publish deadlines.

The CLI's `release_build_facts` test checks the embedded facts in the real
binary, and CI checks that core's build script refuses a branch name in release
mode.

## Acceptance criteria

#1235 can close when:

1. A `v*` tag push builds the four binaries, attests provenance and an SPDX SBOM
   for each, and publishes all 17 assets in one immutable release.
2. The guide's online and offline commands verify every binary against the
   reusable signer, source commit and SBOM.
3. No operator configuration reaches the binaries (R-CONFIG).
4. The maintainer gates are in place and a rehearsal of the merged commit
   passes.
