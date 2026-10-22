# Verify release binaries

Phase 1 publishes four binaries, four SPDX 2.3 SBOMs, eight Sigstore bundles
and `SHA256SUMS`: **17 immutable release assets**. Each binary has SLSA build
provenance and an SBOM attestation. Verify the downloaded bytes before execution
or deployment. Reproducibility, complete composition and the custom Trusted
Server verifier remain later phases.

The tag caller is `.github/workflows/release.yml`. Its local reusable
`.github/workflows/release-build.yml` owns builds and signing at the same source
revision. The reusable workflow is the certificate signer and provenance builder;
the caller remains the publication owner. Build jobs have no signing authority,
and signing jobs do not check out, compile or execute the downloaded binaries.

## Platforms and coverage

| Asset                         | Phase 1 baseline and qualification limit                                                                           |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `trusted-server-fastly.wasm`  | Fastly Compute, `wasm32-wasip1`, default features; reproducibility and hosted runtime qualification remain pending |
| `ts-x86_64-unknown-linux-gnu` | Ubuntu 24.04 x86-64, glibc 2.39 baseline; the retained policy's glibc 2.34 ceiling is not yet established          |
| `ts-aarch64-apple-darwin`     | macOS 15 Apple Silicon runner, deployment target 15.0; runtime compatibility qualification remains pending         |
| `ts-x86_64-apple-darwin`      | macOS 15 Intel runner, deployment target 15.0; runtime compatibility qualification remains pending                 |

The retained destination design calls for a macOS 13.0 deployment target;
Phase 1 still builds with 15.0. Lowering that floor and proving older-runtime
compatibility are additive later work.
The executables are not notarized. Compatibility with older Linux runtimes and
the retained GNU policy's clean startup images remains unqualified. Native
libraries and provider tools needed by individual `ts` commands retain their
existing prerequisites. Cloudflare, Spin, Axum, Windows and Linux ARM64 releases
follow separately.

Each `<binary>.spdx.json` is a **conservative build dependency inventory** of
the selected Cargo target and the installed npm build tree used for that build.
It records the final executable's SHA-256. SPDX 2.3 is the published format;
format compatibility does not imply the complete composition qualification
required by the retained design. The conversion preserves package identities
but drops dependency relationships; the document adds only the binary-root
`DESCRIBES` relationship.

The inventory includes npm build tools and may include packages removed by
linking, tree-shaking or minification. The installed `prebid.js` package is a
build-tree entry; the external Prebid bundle is not embedded in tsjs. Native
C/assembly, system libraries, Rust's standard library and compiler runtimes do
not have complete coverage. Exact retained-code attribution and complete native
and tool closure remain Phase 2 work. An SBOM or provenance result is not a
vulnerability scan.

## Download and verify

Use an independently trusted GitHub CLI installation with `gh attestation verify`,
`gh release verify`, and `gh release verify-asset`. Use the GitHub CLI version
recorded in the release job log. The SBOM comparison below also uses `jq`.
Verification needs GitHub network access. Obtain the approved tag
and its 40-character source commit through an independent source-review process.
The retained strict SemVer/lightweight-tag validation is deferred; use a
separately approved tag and its peeled source commit. A checksum or commit supplied only with
an untrusted download is not source approval.

Run this in a new private directory, using Bash. Replace the fictional tag and
commit. For rehearsal, change `REPO` to the approved public sandbox and use the
individually approved, never-reused rehearsal tag.

```bash
set -euo pipefail
REPO=IABTechLab/trusted-server
TAG=v1.2.3
COMMIT=0123456789abcdef0123456789abcdef01234567
ASSETS=(trusted-server-fastly.wasm ts-x86_64-unknown-linux-gnu \
  ts-aarch64-apple-darwin ts-x86_64-apple-darwin)

gh release download "$TAG" --repo "$REPO" --dir .
test "$(gh api "repos/$REPO/releases/tags/$TAG" --jq '.immutable')" = true
gh release verify "$TAG" --repo "$REPO"
for asset in "${ASSETS[@]}"; do
  for suffix in '' .spdx.json .provenance.sigstore.json .sbom.sigstore.json; do
    gh release verify-asset "$TAG" "$asset$suffix" --repo "$REPO"
  done
done
gh release verify-asset "$TAG" SHA256SUMS --repo "$REPO"
shasum -a 256 --check SHA256SUMS
```

The checksums cover the other 16 assets, never `SHA256SUMS` itself. Immutable
release verification establishes membership; provenance verification establishes
the reviewed signer and source. Both are required.

First verify both published bundles for each binary using the reusable signer
workflow. Provenance binds the binary and its SPDX document; the SBOM
attestation's subject is the **binary**, and its predicate is the SPDX document.
Do not pass the JSON asset as the SBOM attestation's subject.

```bash
WORKFLOW="$REPO/.github/workflows/release-build.yml"
for asset in "${ASSETS[@]}"; do
  gh attestation verify "$asset" --repo "$REPO" \
    --bundle "$asset.provenance.sigstore.json" \
    --signer-workflow "$WORKFLOW" \
    --source-ref "refs/tags/$TAG" --source-digest "$COMMIT" \
    --signer-digest "$COMMIT" --deny-self-hosted-runners \
    --predicate-type https://slsa.dev/provenance/v1
  gh attestation verify "$asset" --repo "$REPO" \
    --bundle "$asset.sbom.sigstore.json" \
    --signer-workflow "$WORKFLOW" \
    --source-ref "refs/tags/$TAG" --source-digest "$COMMIT" \
    --signer-digest "$COMMIT" --deny-self-hosted-runners \
    --predicate-type https://spdx.dev/Document/v2.3
done
```

`--signer-workflow` matches a prefix. Enforce the exact certificate identity,
including the tag ref, in separate checks: GitHub CLI makes `--signer-workflow`
and `--cert-identity` mutually exclusive. These checks use the same bundles
and independently approved source commit. Compare the verified SBOM predicate
to the published SPDX document as well.

```bash
IDENTITY="https://github.com/$WORKFLOW@refs/tags/$TAG"
for asset in "${ASSETS[@]}"; do
  gh attestation verify "$asset" --repo "$REPO" \
    --bundle "$asset.provenance.sigstore.json" \
    --cert-identity "$IDENTITY" \
    --source-ref "refs/tags/$TAG" --source-digest "$COMMIT" \
    --signer-digest "$COMMIT" --deny-self-hosted-runners \
    --predicate-type https://slsa.dev/provenance/v1
  gh attestation verify "$asset" --repo "$REPO" \
    --bundle "$asset.sbom.sigstore.json" \
    --cert-identity "$IDENTITY" \
    --source-ref "refs/tags/$TAG" --source-digest "$COMMIT" \
    --signer-digest "$COMMIT" --deny-self-hosted-runners \
    --predicate-type https://spdx.dev/Document/v2.3 \
    --format json > "$asset.sbom-verification.json"
  jq -e --slurpfile document "$asset.spdx.json" \
    'length == 1 and .[0].verificationResult.statement.predicate == $document[0]' \
    "$asset.sbom-verification.json"
done
```

Stop if any check fails. Keep the outputs, reviewed source identity, bundles
and checksums with your deployment record. These checks do not approve a
publisher service, runtime configuration or deployment operation. See the
[GitHub CLI verification reference](https://cli.github.com/manual/gh_attestation_verify)
and [immutable asset verification reference](https://cli.github.com/manual/gh_release_verify-asset).

## Use the verified CLI

`cargo install-cli` builds `ts` from source. The resulting local binary is not
covered by the release attestation. Operators who need an attested CLI should
download the `ts-<target>` release asset matching their platform, verify it with
the `gh attestation verify` and `gh release verify-asset` commands above, then
copy that verified file into a directory on their `PATH` as `ts` and make it
executable. Verify before first execution. Individual commands may still require
a source checkout and provider tools; see [CLI](/guide/cli).

## Deploy the verified Fastly wasm

Keep the verified download unchanged. The ordinary source-build route does not
establish equality with this release, and Phase 1 adds no `ts deploy --release`
command. Use a prebuilt package route for the verified bytes.

For a manual deployment to an **existing** Fastly service, use the Fastly CLI
version pinned in the release tag's `.tool-versions`. Prepare a fresh private
packaging directory containing only the verified wasm and a minimal manifest:

```toml
manifest_version = 3
name = "example-app"
```

Do not copy operator config, secrets, local fixtures or build hooks into that
directory. Approve the destination service and base version, and inspect their
Config, KV and Secret Store links beforehand. The logical config-store ID comes
from the release tag's tracked `edgezero.toml`; physical store names and actual
configuration are runtime resources. This manual route does not create links,
push configuration, implement staging, or supply recovery guarantees.

From the private directory:

```bash
fastly compute pack --wasm-binary ./trusted-server-fastly.wasm
tar -tzf ./pkg/example-app.tar.gz
tar -xOzf ./pkg/example-app.tar.gz example-app/bin/main.wasm > packaged.wasm
cmp trusted-server-fastly.wasm packaged.wasm
```

Inspect the archive inventory: it must contain only the expected named package,
its directories, `fastly.toml`, and `bin/main.wasm`. Check the extracted manifest
against the approved minimal manifest. Select the named archive explicitly;
packing can also produce a generic `package.tar.gz`. Do not guess an archive
name or use a previously populated `pkg/` directory.

After verifying the package member is byte-identical, use your reviewed service
ID and integer base version in place of these fictional values:

```bash
fastly compute deploy --package ./pkg/example-app.tar.gz \
  --service-id '99999' --version '7' --no-default-domain
```

This command can activate production and change an active/locked base version
by cloning it; confirm its behavior for the pinned CLI before use. Retain the
verified wasm, manifest, package digest, previous active version and observed
result. No build step may replace or annotate the verified wasm. Automated
EdgeZero prepared-payload deployment and operation recovery are later phases.

## Build inputs, policy and release administration

Builds use the tracked source, manifests, `Cargo.lock`, pinned Rust and Node,
the JS lockfile and fresh JS bundles. The tracked `edgezero.toml` supplies the
logical config-store ID and participates in the template build digest. Build
facts include the source commit. The CLI carries the fictional example-config
scaffold, not an operator config.

The three policy files are carried unchanged from the frozen reviewed design.
They have **no Phase 1 readers** and do not authorize these builds or replace
the workflow's explicit selections.

| Policy                                          | Current use                                                            | Additive later adoption                                                                          |
| ----------------------------------------------- | ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `scripts/release/release-cohort.json`           | Retained destination contract; no reader                               | Checked fixed cohort selection, original assurance profiles and compatibility qualification      |
| `scripts/release/release-tools.json`            | Retained pending/null qualification records; no reader                 | Checked tool identities, authenticated acquisition, image/SDK measurements and qualified recipes |
| `crates/trusted-server-cli/release-policy.json` | Retained signer/caller and SPDX contract; no reader or custom verifier | Embedded consumer policy, bounds, historical-policy acceptance and complete index/evidence joins |

Phase 1 does not ship `release-index.json`, `tsjs-inventory.json`, build-evidence,
build-recipe or rebuild-check placeholders. Their frozen schemas and names
remain reserved for the later implementation. These releases do not yet satisfy
the frozen custom verifier's complete evidence graph.

CI audits compile-time deployment inputs, scrubs inherited environment values,
and scans every executable for planted fictional config values. These are
regression checks, not hermetic-build qualification. Operator configuration,
credentials, physical bindings and variable permission policy remain runtime
inputs.

Before publication, a maintainer must enable immutable releases, protect `v*`
tags and approve tag-push authority and required checks. Immutability remains a
human precondition; the publisher has no repository-administration token. A
violation can expose a mutable release before the workflow reports failure.

Publication lists releases, including drafts, and refuses every existing
matching tag. It creates one draft, retains its release ID, and binds uploads
and the final publish mutation to that ID. It checks the exact signing-approved asset
set, validates each upload response's size and SHA-256 digest before publication,
and checks immutable membership after publication. Re-downloading uploaded bytes
and the reviewed publication owner remain additive later work. There are no
automatic mutation retries or draft-resume claims.

After a failed or indeterminate mutation, inspect the recorded release ID and
all matching drafts read-only. Do not rerun uploads, delete a release, or reuse
the tag without an independently approved recovery decision. Published immutable
releases are never overwritten. A fresh release needs a fresh approved tag;
Phase 2's full reconciliation and incident owner remains deferred.

Before the first production release, rehearse the reviewed commit in a separate
public repository with Actions and immutable releases enabled. Use a fresh,
lightweight `v0.0.0-rehearsal.N` tag for each attempt and retain verification
outputs in the release PR. Keep rehearsal tags and releases out of the upstream
repository. For production, confirm the required settings and checks, then
follow the maintainer release authorization process before tagging or deploying.
