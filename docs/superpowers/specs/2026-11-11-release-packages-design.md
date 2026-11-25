# Release packages on GHCR

- **Issues:** part of [#1235](https://github.com/IABTechLab/trusted-server/issues/1235)
  (attested releases); implements the image and bind items proposed in
  [#1236](https://github.com/IABTechLab/trusted-server/issues/1236) ahead of its
  spec
- **Date:** 2026-11-11
- **Status:** proposed, in review with its implementation PR
- **Stacked on:** #1449 (Phase 2 attested releases)

## Problem

A rehearsal release publishes 19 assets, and each run also lists 12 internal
workflow artifacts. Operators could not tell which files to use. There was also
no way to run the Axum server as a container: it only listened on loopback, and
no image existed.

## Goals

- Mirror each published release into GHCR as `trusted-server-cli` (tag
  `<tag>-<target>`) and `trusted-server-fastly` (tag `<tag>`). Each package
  holds one binary with its SPDX SBOM and Sigstore bundles. The Fastly package
  also holds `trusted-server-fastly.tar.gz`, the package operators deploy, which
  the wasm's bundles cover, and the rebuild check. Every release file except
  `SHA256SUMS` is in exactly one package, and every file is one OCI layer whose
  digest is its `SHA256SUMS` entry.
- Publish the Axum server as `trusted-server-axum:<tag>`, a container image with
  GitHub build provenance, built to the image contract proposed in #1236.
- Link each package to the source repository.
- Keep the release itself unchanged: packages add no trust, and verification
  stays the release verification procedure.

## Non-goals

- Changing the internal workflow artifacts.
- Making Axum a supported production target. Axum stays development status, and
  the image says so.
- The rest of #1236: Edge Cookie KV, client IP and geo, SIGTERM drain and
  readiness, key rotation, and the decisions its spec must settle. That spec may
  still change the image and bind choices made here.
- A `linux/arm64` image.
- Adding the image to the reproducible release cohort: no rebuild check or SBOM
  covers it.

## Scope note

#1236 lists image publishing as a non-goal owned by #897 (`ts up`, using the
stackpop/edgezero#399 recipe). Publishing the Axum image here takes on that part
of #897 early, so release tags ship a matching image. If #897 or #1236's spec
chooses another recipe, the `axum-image` job is the one place to change.

## Approaches

1. **A publisher job after the release (chosen).** A `packages` job downloads
   the published release by tag, authenticates `SHA256SUMS`, checks every file
   against it, and pushes with a pinned, checksum-verified `oras`. It reuses the
   release's attestations; the files keep their digests.
2. **Push from the signing jobs.** Earlier, but gives signing jobs registry
   authority and can publish packages for a release that later fails.
3. **Per-platform archives in the release.** Shortens the asset list, but changes
   the attested release format and every verification command.

## Design

### Binary packages

- `PACKAGE_PUBLICATION_ENABLED=true` gates both new jobs, separately from
  `RELEASE_PUBLICATION_ENABLED`. Both need the `release` job to succeed.
- The `packages` job has only `contents: read` and `packages: write`. It
  sparse-checks out `.tool-versions`, `publish.mjs`, `packages.mjs` and the notes
  template.
- `packages.mjs`:
  - refuses drafts;
  - authenticates `SHA256SUMS` with `gh release verify-asset`;
  - requires the download to equal `SHA256SUMS` exactly;
  - refuses a release file that no package carries, so a new release asset
    stops the job instead of being left out.
- **Existing tags.**
  - Only the registry's `<reference>: not found` answer means a tag is
    absent. Any other
    lookup error stops the job.
  - A tag already holding exactly the planned files (the same layers and
    annotations, an empty config, and no `subject`) is kept. This makes a
    re-run finish a partial publish.
  - A tag holding anything else stops the job before it pushes.
  - Each pushed manifest is checked layer by layer: media type, digest, title,
    order and annotations.
- GHCR tags are mutable. The refusal applies only at publish time, so the docs
  tell users to verify pulled files, or an image by its digest.

### Axum image

- **Build.** It builds from the tagged source with `--locked`. It uses the
  pinned Rust image and copies Node from the `.tool-versions` Node image (both
  pinned by digest). `RUSTUP_TOOLCHAIN` pins the installed channel, so rustup
  downloads nothing.
- **JS bundles.** #1236 proposes a separate Node stage that builds the JS
  before Cargo. Here Node is copied into the Rust stage, and the existing tsjs
  build script runs `npm ci` and the bundle build. The effect is the same, and
  Node does not reach the final image.
- **Runtime.**
  - Debian slim with CA certificates, UID `10001`, and an exec-form
    entrypoint. Nothing runs a shell at runtime.
  - No config, secrets or keys are baked in. Runtime inputs are the documented
    `TRUSTED_SERVER_CONFIG_*` and `TRUSTED_SERVER_SECRET_*` environment
    variables.
  - The process writes nothing and needs no writable mounts, so it runs with a
    read-only root filesystem.
- **TLS roots.** Trusted Server's origin fetches use compiled-in webpki roots.
  Only EdgeZero's own client reads the system CA store. Switching to native
  roots is left to #1236.
- **Shutdown.** `STOPSIGNAL SIGINT`, because EdgeZero v0.0.11 shuts down on
  SIGINT only, and PID 1 ignores an unhandled SIGTERM. Drain and readiness wait
  for stackpop/edgezero#392 and #1142.
- **Bind address.** Axum now reads EdgeZero's `EDGEZERO__ADAPTER__HOST` and
  `EDGEZERO__ADAPTER__PORT` through `edgezero_core::addr::resolve_bind_addr`.
  `PORT` is the port fallback, and the default stays `127.0.0.1:8787`.
  - `PORT` is read only when no valid EdgeZero port is set. The image sets
    `0.0.0.0:8080`, so a platform-injected `PORT`, valid or not, is ignored
    there.
  - Natively, `PORT` with `EDGEZERO__ADAPTER__HOST` now binds that host.
  - `PORT=0` used to bind a random port. It is now rejected, as EdgeZero
    rejects port 0; nothing in the repository used it.
- **Embedded version.** The source commit, as for the release binaries.
- **Publishing job.** `axum-image` refuses an existing tag at publish time. It
  checks the pushed digest, then attaches build provenance to it with
  `actions/attest`.
  - The job holds `packages`, `id-token` and `attestations` write while
    BuildKit runs the build. BuildKit does not expose those credentials to
    build steps.
  - Splitting the build from the push is a possible hardening.
- **Recipe drift.** The Dockerfile is not generated from the
  stackpop/edgezero#401 recipe. A test keeps its Node and Rust tags equal to
  the repository pins, and #1236's spec decides how it tracks that recipe.

## Test plan

- **`packages.test.mjs`:**
  - mapping and plan;
  - the exact `SHA256SUMS` check;
  - the manifest check;
  - first publish, re-run and partial-publish completion;
  - refusal of differing tags;
  - failing closed on registry errors;
  - drafts;
  - Dockerfile pins.
- **`phase1.test.mjs`:** each job is checked on its own:
  - the publisher's checkout;
  - the `packages` job's sparse checkout, pinned oras and lack of release-write
    permission;
  - the `axum-image` gate.
- **Axum unit tests:** bind precedence, fallback warnings and invalid `PORT`.
- **`test.yml` `axum-image` job** (every PR and push to `main`):
  - builds the image;
  - runs `scripts/smoke-axum.sh` against it with a read-only root and no
    capabilities, covering the four missing-input cases and the positive
    origin request;
  - runs one case with the image's own listen defaults on a bridge network;
  - checks the image user.
- **A fork rehearsal of a release tag.**
  - It publishes all packages and the image.
  - Each layer digest matches `SHA256SUMS`, and the packages are pullable
    anonymously.
  - `gh attestation verify` passes for a pulled CLI binary and for the image
    digest.

## Rollout

1. **Rehearse.** Before enabling publication, a fork rehearsal of the exact
   commit must pass, including a first publish to packages that did not exist.
2. **Merge.** Both jobs stay skipped until a maintainer sets
   `PACKAGE_PUBLICATION_ENABLED`.
3. **After the first publish.** A maintainer checks each package's visibility
   and repository link under the organization's Packages settings.

## Failure and recovery

- **`packages` fails partway.** Re-run the failed job. Matching tags are kept
  and the rest are pushed.
- **A tag holds other content.** The job stops before pushing. A maintainer
  investigates, deletes the wrong version, then re-runs.
- **`axum-image` fails after pushing** (for example during attestation). Delete
  that package version, then re-run. The job refuses an existing tag rather
  than attesting an image it cannot prove it built.

## Review log

### Round 1 (agent review, 2026-11-11)

Nothing blocking. These findings were applied:

- **TLS roots.** The "system CA store" claim was wrong; it is now corrected.
- **Bridge-network smoke case.** Added. It proves the image listens
  off-loopback with its own defaults, and the smoke checks the image user.
- **Shutdown.** `STOPSIGNAL SIGINT`, because PID 1 ignored SIGTERM.
- **Existing tags.** "Absent" now means only not-found, and a re-run completes a
  partial publish. The "write-once" wording now states that tags are mutable.
- **Recovery.** Steps for partial publishes and failed attestations, in the
  spec and in the maintainer checklist.
- **Image verification.** The docs now verify an image by its digest, then run
  that digest.
- **Scope.** The overlap with #897 and #1236 is stated, and the image is marked
  development status.
- **Spec gaps filled.** Header, test plan, rollout, failure, and the
  differences from #1236's proposed contract.
- **Smaller fixes.**
  - A test pins the Dockerfile's Node and Rust tags to the repository pins.
  - The image embeds the source commit.
  - `SHA256SUMS` is authenticated before mirroring.
  - The docs say `PORT` is ignored inside the image.
  - `RUSTUP_TOOLCHAIN` stops rustup from downloading targets.

Not changed:

- Splitting the image build from its push. Recorded above as a possible
  hardening.
- GHCR repository linking on the organization. Rollout checks it.

### Round 2 (agent review, 2026-11-11)

Nothing blocking. Round 2 confirmed every round 1 change in the code. These
findings were applied:

- **Stale comment.** The workflow comment still said "write-once"; it is now
  corrected.
- **`PORT` precedence.** `PORT` is now read only as the fallback, so a
  malformed `PORT` no longer stops the image. `PORT=0` gets an error named for
  `PORT`.
- **Not-found detection.** It now matches only `<reference>: not found`, in
  both the script and the workflow. A test feeds it an unrelated
  "credentials not found" message.
- **Exact match on re-run.** A kept tag must also have the empty config and no
  `subject`. The `.37` manifest confirms what oras pushes.
- **ORAS prerequisite.** The guide now names ORAS and Docker Buildx.
- **First-publish rehearsal.** Rollout and the maintainer checklist now require
  one before enabling publication. In rehearsal `.37`, GHCR answered
  `<reference>: not found` for packages that did not exist yet, so the
  anchored check accepts a first publish.

Not changed: when a wait times out, the smoke does not print the container log.
The existing native cases behave the same way.

### Base update (2026-11-12)

Phase 2 added `trusted-server-fastly.tar.gz` to the release and made it the
documented Fastly deploy path. The Fastly package now carries it, and
`checkDownload` refuses any release file that no package carries. Before this,
the plan omitted the archive and the tests could not notice, because their
fixture release was built from the plan itself.
