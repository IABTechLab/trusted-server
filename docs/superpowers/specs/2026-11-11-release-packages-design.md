# Release packages on GHCR

## Intent

The fork rehearsals publish a GitHub Release with 18 assets, and each run also
lists 12 internal workflow artifacts. Operators could not tell which files to
use. Releases now also appear in the repository's **Packages** list, with one
package per thing an operator runs, and the Axum server ships as a container
image.

## Goals

- Mirror each published release into GHCR as three packages:
  `trusted-server-cli` (tag `<tag>-<target>`), `trusted-server-fastly`
  (tag `<tag>`) and `trusted-server-axum` (tag `<tag>`).
- Keep each CLI and Fastly package byte-identical to the release: every file is
  one OCI layer whose digest is its `SHA256SUMS` entry.
- Link every package to the source repository and keep tags write-once.
- Ship Axum as a non-root container image with GitHub build provenance.

## Non-goals

- Package publication adds no trust. Verification stays the release
  verification procedure, applied to pulled files.
- The Axum image does not join the reproducible release cohort: no rebuild
  check, SBOM or release membership covers it.
- No `linux/arm64` image yet.
- The internal workflow artifacts are unchanged.

## Approaches

1. **OCI artifacts pushed by a publisher job after the release (chosen).** A
   `packages` job downloads the published release by tag, checks it against
   `SHA256SUMS`, then pushes with a pinned, checksum-verified `oras`. It reuses
   the release's existing attestations; the files keep their digests.
2. **Push from the signing jobs.** Earlier, but gives signing jobs registry
   authority and can publish packages for a release that later fails.
3. **Per-platform archives in the release.** Shortens the asset list but changes
   the attested release format and every verification command.

## Decisions

- `PACKAGE_PUBLICATION_ENABLED=true` gates both new jobs, separately from
  `RELEASE_PUBLICATION_ENABLED`; both need the `release` job to succeed.
- The publisher refuses drafts, any file outside `SHA256SUMS`, and existing
  tags, and checks each pushed manifest's layers, order and annotations.
- The Axum image builds from the tagged source with the pinned Rust image and
  `.tool-versions` Node (checksum-verified), runs on distroless `cc-debian12`
  as `nonroot`, and listens on `0.0.0.0:8080` through the new
  `TRUSTED_SERVER_LISTEN_ADDR` variable. `actions/attest` attaches build
  provenance to the pushed image digest.
- Plain `docker buildx` and `oras` commands avoid new third-party actions.

## Left to the builder

Package visibility after the first publish is an organization setting; the
maintainer checklist covers it.
