# Verify release binaries

Phase 1 publishes `trusted-server-fastly.wasm` and three `ts` executables with
SLSA build provenance, a CycloneDX SBOM for each binary, and immutable GitHub
Release membership. Verify downloads before executing or deploying them.
Reproducible builds and the custom Trusted Server verifier are later phases;
Phase 1 operators must use the downloaded, verified bytes.

## Platforms and coverage

| Asset                         | Hosted build baseline                             |
| ----------------------------- | ------------------------------------------------- |
| `trusted-server-fastly.wasm`  | Fastly Compute, `wasm32-wasip1`, default features |
| `ts-x86_64-unknown-linux-gnu` | Ubuntu 24.04 x86-64, glibc 2.39 baseline          |
| `ts-aarch64-apple-darwin`     | macOS 15 or newer, Apple Silicon                  |
| `ts-x86_64-apple-darwin`      | macOS 15 or newer, Intel                          |

The Linux binary is built against the Ubuntu 24.04 environment; compatibility
with older glibc releases is not established. Native libraries and tools needed
by individual `ts` commands retain their existing prerequisites. The macOS
builds set `MACOSX_DEPLOYMENT_TARGET=15.0`; these executables are not notarized.
Cloudflare, Spin, Axum, Windows and Linux ARM64 assets follow separately.

Each `<binary>.cdx.json` is a **conservative build dependency inventory**:
`cargo-cyclonedx` 0.5.9 resolves the selected Cargo target after its locked release
build, and the npm bundled with pinned Node generates an SBOM of the installed
JS build tree from that run. The merger namespaces both dependency graphs and
records the final binary's SHA-256. The generator source archive is pinned by
SHA-256 before installation; Cargo's shipped lock and npm's committed lock
check downloaded package integrity.

This covers the npm dependencies used to build the embedded JS, including build
tools. It can include packages removed by linking, tree-shaking or minification.
The installed `prebid.js` package is a build-tree entry; the external Prebid
bundle is not embedded in tsjs. Native C/assembly, system libraries, Rust's
standard library and compiler runtimes do not have complete coverage here.
Complete composition evidence and artifact-specific retained-code attribution
remain Phase 2 work. An SBOM or provenance result is not a vulnerability scan.

## Download and verify

Use an independently trusted GitHub CLI installation with `gh attestation verify`,
`gh release verify`, and `gh release verify-asset`. Authenticate if GitHub requires
it. Verification needs GitHub network access; Phase 1 does not provide an offline
release-membership decision. Obtain the approved tag and its **peeled commit
SHA** through your own source-review process. A tag name, checksum file or
commit supplied only alongside an untrusted download is not source approval.

Run this in a new private directory, using Bash. Replace the fictional tag and
commit below. For a rehearsal, change `REPO` to the approved public sandbox and
use the individually approved, never-reused rehearsal tag.

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
  gh release verify-asset "$TAG" "$asset" --repo "$REPO"
  gh release verify-asset "$TAG" "$asset.cdx.json" --repo "$REPO"
done
gh release verify-asset "$TAG" SHA256SUMS --repo "$REPO"
shasum -a 256 --check SHA256SUMS
```

A checksum detects corruption; immutable-release verification establishes
membership. Neither replaces provenance verification. Verify **each binary**
against both the SLSA provenance predicate and the SBOM predicate:

```bash
WORKFLOW="$REPO/.github/workflows/release.yml"
for asset in "${ASSETS[@]}"; do
  gh attestation verify "$asset" --repo "$REPO" \
    --signer-workflow "$WORKFLOW" \
    --source-ref "refs/tags/$TAG" --source-digest "$COMMIT" \
    --deny-self-hosted-runners \
    --predicate-type https://slsa.dev/provenance/v1
  gh attestation verify "$asset" --repo "$REPO" \
    --signer-workflow "$WORKFLOW" \
    --source-ref "refs/tags/$TAG" --source-digest "$COMMIT" \
    --deny-self-hosted-runners \
    --predicate-type https://cyclonedx.org/bom
done
```

The SBOM attestation's subject is the **binary**, and its predicate is the SBOM.
Do not pass the JSON asset as the binary subject. The JSON asset's own immutable
membership was checked above.

`--signer-workflow` matches a workflow prefix. Also enforce the exact certificate
identity, including the tag ref, with these separate checks. GitHub CLI does not
allow `--signer-workflow` together with `--cert-identity` in one invocation.

```bash
IDENTITY="https://github.com/$WORKFLOW@refs/tags/$TAG"
for asset in "${ASSETS[@]}"; do
  for predicate in https://slsa.dev/provenance/v1 https://cyclonedx.org/bom; do
    gh attestation verify "$asset" --repo "$REPO" \
      --cert-identity "$IDENTITY" \
      --source-ref "refs/tags/$TAG" --source-digest "$COMMIT" \
      --deny-self-hosted-runners --predicate-type "$predicate"
  done
done
```

Stop if any check fails. Keep the command outputs, reviewed source identity and
checksums with your deployment record. These checks establish the workflow,
source and published bytes; they do not establish publisher authorization,
runtime configuration or remote execution. See the
[GitHub CLI verification reference](https://cli.github.com/manual/gh_attestation_verify)
and [immutable asset verification reference](https://cli.github.com/manual/gh_release_verify-asset).

## Use the verified CLI

Choose the executable matching your platform, verify it as above, then copy it
into a directory on your `PATH` as `ts` and make it executable. Verify before the
first execution. A source checkout and provider tools may still be required by
individual commands; see [CLI](/guide/cli).

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

## Build inputs and release administration

Release builds use the tracked source tree and manifests, `Cargo.lock`, the
Rust toolchain from `rust-toolchain.toml`, Node from `.tool-versions`, the JS
lockfile and fresh JS bundles from that build. The tracked `edgezero.toml`
supplies the logical config-store ID and participates in the template build
digest. Build facts include the source commit. The CLI also carries the
fictional example-config scaffold; it does not carry an operator config.

CI audits compile-time deployment inputs, builds with a scrubbed inherited
environment, and scans every final executable for planted fictional config
values. These are regression checks, not full hermetic-build qualification.
Operator `trusted-server.toml`, credentials, physical resource bindings and
variable permission policy must remain runtime inputs.

Before any real release, a maintainer must enable immutable releases, protect
`v*` tags, and decide required checks and tag-push authority. Immutability is a
human precondition: the workflow checks the published release and fails loudly
if it is false, but a violation can already have exposed a mutable release.
The job has no repository-administration token. Existing releases, including
drafts from a failed attempt, are never overwritten; inspect failures and use
a fresh approved tag rather than rerunning publication against existing assets.

First rehearse the same approved commit in the user's public sandbox with
Actions and immutable releases enabled. The user must approve **each** new
`v0.0.0-rehearsal.N` tag before it is pushed. Run the commands above against that
sandbox and record the outputs in the draft PR. Never use IAB's repository for
rehearsal tags or releases. The first real tag requires separate authorization.
