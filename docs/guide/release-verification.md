# Verify release binaries

Each release publishes four binaries, four SPDX 2.3 SBOMs, eight Sigstore
bundles, the Fastly rebuild check and `SHA256SUMS`: **18 immutable release
assets**. Each binary has SLSA build provenance and an SBOM attestation. Verify
the downloaded bytes before execution or deployment. Fastly's ordinary A/B
equality gate is described below; controlled reproducibility qualification,
complete composition and the custom Trusted Server verifier remain later work.

The tag caller is `.github/workflows/release.yml`. Its local reusable
`.github/workflows/release-build.yml` owns builds and signing at the same source
revision. The reusable workflow is the certificate signer and provenance builder;
the caller remains the publication owner. Build jobs have no signing authority,
and signing jobs do not check out, compile or execute the downloaded binaries.
The caller's publication job checks out only `scripts/release/publish.mjs`, its
release-notes template and `.tool-versions` from the tagged commit, and runs
them with the pinned Node and a checksum-verified GitHub CLI 2.102.0.

## Platforms and coverage

| Asset                         | Phase 1 baseline and qualification limit                                                                                                                    |
| ----------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `trusted-server-fastly.wasm`  | Fastly Compute, `wasm32-wasip1`, default features; ordinary A/B byte equality required, controlled reproducibility and hosted runtime qualification pending |
| `ts-x86_64-unknown-linux-gnu` | GNU x86-64; UBI 9 builder; maximum required glibc 2.34; Debian 12, Ubuntu 22.04 and UBI 9 startup smokes                                                    |
| `ts-aarch64-apple-darwin`     | macOS 15 Apple Silicon runner, deployment target 13.0; runtime compatibility qualification remains pending                                                  |
| `ts-x86_64-apple-darwin`      | macOS 15 Intel runner, deployment target 13.0; runtime compatibility qualification remains pending                                                          |

The build checks 64-bit Mach-O magic, target CPU and minimum macOS 13.0 load
commands. Actual execution on macOS 13 remains later qualification. The
executables are not notarized. The Linux build checks for a 64-bit x86-64 ELF
and requires no GLIBC symbol newer than 2.34; glibc 2.34 and newer are the
selected baseline. This includes Debian 12/bookworm-slim, Ubuntu 22.04+ and
RHEL/UBI 9+. All smoke images are digest-pinned; build logs record their image
IDs. These startup checks do not establish complete platform qualification.
Native libraries and provider tools needed by individual `ts` commands retain
their existing prerequisites. Axum, Cloudflare, Spin, Windows and Linux ARM64
releases follow separately.

Each `<binary>.spdx.json` is a **conservative build dependency inventory** of
the selected Cargo target and the installed npm build tree used for that build.
It records the final executable's SHA-256 and Cargo's package version; the
release tag is not part of the SBOM. SPDX 2.3 is the published format. The
inventory records package identities but no dependency relationships; the
document adds only the binary-root `DESCRIBES` relationship.

The inventory includes npm build tools and may include packages removed by
linking, tree-shaking or minification. The installed `prebid.js` package is a
build-tree entry; the external Prebid bundle is not embedded in tsjs. Native
C/assembly, system libraries, Rust's standard library and compiler runtimes do
not have complete coverage. Exact retained-code attribution and complete native
and tool closure remain later controlled-stage work. An SBOM or provenance result is not a
vulnerability scan.

## Download and verify

Use an independently trusted GitHub CLI 2.102.0 or later with `gh attestation verify`,
`gh release verify`, and `gh release verify-asset`, authenticated with
`gh auth login` or `GH_TOKEN`. The SBOM comparison below also uses `jq`, and the
checksum steps use `shasum` (use `sha256sum --check` where `shasum` is absent).
Verification needs GitHub network access. Obtain the approved tag
and its 40-character source commit through an independent source-review process.
Phase 1 requires a strict SemVer `v` tag (such as `v1.2.3` or `v1.2.3-rc.1`;
build metadata such as `+build.1` is refused) that is lightweight and points
directly to that source commit. The release notes link to this guide but stay
editable after publication, so never take `REPO`, `TAG` or `COMMIT` from them.
A checksum or commit supplied only with an untrusted download is not source
approval.

Run this in a new private directory, using Bash. Replace the fictional tag and
commit. For rehearsal, change `REPO` to the approved public sandbox and use the
fresh lightweight, never-reused rehearsal tag.

<!-- release-verification:start -->

```bash
set -euo pipefail
# Take REPO, TAG and COMMIT from your independent source review.
REPO=IABTechLab/trusted-server
TAG=v1.2.3
COMMIT=0123456789abcdef0123456789abcdef01234567
[[ -n "$REPO" && -n "$TAG" && "$COMMIT" =~ ^[0-9a-f]{40}$ ]] || exit 1
ASSETS=(trusted-server-fastly.wasm ts-aarch64-apple-darwin ts-x86_64-apple-darwin ts-x86_64-unknown-linux-gnu)
WORKFLOW="$REPO/.github/workflows/release-build.yml"
gh release download "$TAG" --repo "$REPO" --dir .
test "$(gh api "repos/$REPO/releases/tags/$TAG" --jq .immutable)" = true
gh release verify "$TAG" --repo "$REPO"
for asset in "${ASSETS[@]}"; do
  for suffix in "" .spdx.json .provenance.sigstore.json .sbom.sigstore.json; do gh release verify-asset "$TAG" "$asset$suffix" --repo "$REPO"; done
  for kind in provenance sbom; do
    predicate=https://slsa.dev/provenance/v1; if [[ "$kind" == sbom ]]; then predicate=https://spdx.dev/Document/v2.3; fi
    gh attestation verify "$asset" --repo "$REPO" --bundle "$asset.$kind.sigstore.json" \
      --source-ref "refs/tags/$TAG" --source-digest "$COMMIT" --signer-digest "$COMMIT" \
      --deny-self-hosted-runners --predicate-type "$predicate" --signer-workflow "$WORKFLOW" \
      --format json > "$asset.$kind.verified.json"
    if [[ "$kind" == sbom ]]; then jq -e --slurpfile sbom "$asset.spdx.json" '[.[].verificationResult.statement.predicate] | length == 1 and .[0] == $sbom[0]' "$asset.$kind.verified.json"; fi
  done
done
CHECK=trusted-server-fastly.wasm.rebuild-check.json
gh release verify-asset "$TAG" "$CHECK" --repo "$REPO"
gh attestation verify "$CHECK" --repo "$REPO" --bundle trusted-server-fastly.wasm.provenance.sigstore.json \
  --source-ref "refs/tags/$TAG" --source-digest "$COMMIT" --signer-digest "$COMMIT" \
  --deny-self-hosted-runners --predicate-type https://slsa.dev/provenance/v1 --signer-workflow "$WORKFLOW"
binary_sha=$(shasum -a 256 trusted-server-fastly.wasm | awk '{print $1}')
jq -e --arg source "$COMMIT" --arg digest "$binary_sha" '.version == 1 and .profile == "ordinary-release-fastly-byte-equality-v1" and .release_qualification == false and .outcome == "matched" and .source_commit == $source and .asset == "trusted-server-fastly.wasm" and .canonical_sha256 == $digest and .reproduction_sha256 == $digest' "$CHECK"
gh release verify-asset "$TAG" SHA256SUMS --repo "$REPO"
shasum -a 256 --check SHA256SUMS
```

<!-- release-verification:end -->

Provenance covers the binary and its SPDX document, plus the rebuild check for
Fastly; the SBOM attestation covers the binary. The verified SBOM predicate must equal the shipped SPDX document.
`--signer-workflow` pins the reusable signer; with the source ref and both
digests pinned to the reviewed tag and commit, it identifies the exact signing
workflow run's source.

The checksums cover the other 17 assets, never `SHA256SUMS` itself. Immutable
release verification establishes membership; provenance establishes the
reviewed signer and source. Both are required.

## Fastly's ordinary rebuild check

The release workflow runs the equality gate on every push to `main` and on every
`v*` tag, with no separate switch; `RELEASE_PUBLICATION_ENABLED` still gates
only signing and publication. Each run adds a second fresh Fastly build, after
the first completes, and a read-only comparison. Both full final Wasm files must
match byte for byte. A mismatch fails the run: on `main` it leaves that commit
without the passing release checks required before tagging, and on a tag it
prevents all signing and publication. Debug information and custom sections are
retained. The CLI binaries have no A/B release gate; native reproduction remains
later measured coverage.

The builds vary physical source/target directories, Cargo home, job count,
umask and start time. Each role fetches its dependency sources into its own
fresh Cargo home under `RUNNER_TEMP`. Both receive the identical six-map
target-scoped Cargo argument pair: their checkout roots map to
`/trusted-server`, their target roots map to `/cargo-target`, and their Cargo
homes map to `/cargo-home`. Normalized compiler source-path identity is
intentionally shared. The retained argument bytes are checked against both
observations, each observation proves that every resolved package came from the
checkout or that role's Cargo home, and the complete binaries are scanned for
all six physical roots. Both jobs still run as the same runner user on the same
hosted image. This is ordinary equality between two jobs, not an independently
reproducible build recipe, with `release_qualification:false`; controlled
material/tool/boundary closure and an alternate user remain unqualified.

`trusted-server-fastly.wasm.rebuild-check.json` is a third subject in the
existing Fastly provenance bundle. Existing binary/SPDX commands and signer
identity stay unchanged. Consumers must check the companion's `version` and
`profile`, not infer controlled qualification from `outcome:"matched"`. Phase 3
will index this same filename; later controlled evidence uses a different
profile, with reviewed compatible joins or an explicit version change.

To verify the companion through the online attestation store, omit the bundle
and keep every identity/source constraint:

```bash
checks=(--repo "$REPO" --source-ref "refs/tags/$TAG" --source-digest "$COMMIT"
  --signer-digest "$COMMIT" --deny-self-hosted-runners
  --predicate-type https://slsa.dev/provenance/v1)
gh attestation verify "$CHECK" "${checks[@]}" --signer-workflow "$WORKFLOW"
```

The same omission performs online lookup for each binary's provenance. For
online SBOM lookup, first authenticate the published SPDX file with the
release/asset checks above, then compare it only with results that `gh` has
successfully verified with every existing pin:

```bash
set -euo pipefail
checks=(--repo "$REPO" --source-ref "refs/tags/$TAG" --source-digest "$COMMIT"
  --signer-digest "$COMMIT" --deny-self-hosted-runners
  --predicate-type https://spdx.dev/Document/v2.3)
for asset in "${ASSETS[@]}"; do
  gh attestation verify "$asset" "${checks[@]}" --signer-workflow "$WORKFLOW" \
    --format json > "$asset.online-sbom-verification.json"
  jq -e --slurpfile document "$asset.spdx.json" \
    'any(.[]; .verificationResult.statement.predicate == $document[0])' \
    "$asset.online-sbom-verification.json"
done
```

Reruns or retries of signing can leave more than one valid SBOM attestation for
the same binary digest, in no guaranteed order. Require a verified predicate
exactly equal to the authenticated published SPDX; do not choose the first or
newest record. `any` over an empty array is false, and `jq -e` exits non-zero
for an empty or nonmatching result. The published-bundle checks above and below
retain their single-result comparison.

For offline companion verification, first obtain its SHA-256 independently as
`APPROVED_REBUILD_CHECK_SHA256SUMS`, then transfer the companion, Fastly
provenance bundle and trusted roots:

```bash
set -euo pipefail
CHECK=trusted-server-fastly.wasm.rebuild-check.json
shasum -a 256 --check APPROVED_REBUILD_CHECK_SHA256SUMS
gh attestation verify "$CHECK" --repo "$REPO" --source-ref "refs/tags/$TAG" \
  --source-digest "$COMMIT" --signer-digest "$COMMIT" --deny-self-hosted-runners \
  --predicate-type https://slsa.dev/provenance/v1 \
  --bundle trusted-server-fastly.wasm.provenance.sigstore.json \
  --custom-trusted-root trusted_root.jsonl --signer-workflow "$WORKFLOW"
```

Acquire the roots as described below before disconnecting. Keep the companion's
profile/source/binary-digest check and the separate online immutable-membership
record with the deployment evidence. The signing job accepts a companion sealed
in this run's current or an earlier attempt, so "Re-run failed jobs" can retry
a failed signing leg without rebuilding. A rerun that includes the comparison
binds the build jobs GitHub carries over into the new attempt, which are still
this run's real pair. An existing immutable release is refused rather than
published again.

Stop if any check fails. Keep the outputs, reviewed source identity, bundles
and checksums with your deployment record. These checks do not approve a
publisher service, runtime configuration or deployment operation. See the
[GitHub CLI verification reference](https://cli.github.com/manual/gh_attestation_verify)
and [immutable asset verification reference](https://cli.github.com/manual/gh_release_verify-asset).

## Verify bundles offline

While online, obtain the trust roots using an independently trusted GitHub CLI
installation. GitHub CLI 2.102.0 supports the flags below; retain its version,
the roots and their digest with the independently approved source identity.
Do not accept replacement roots supplied only alongside an untrusted binary.

```bash
gh --version
gh attestation trusted-root > trusted_root.jsonl
shasum -a 256 trusted_root.jsonl
```

Transfer the binaries, their bundles, published SPDX documents, approved source
pins and trusted roots to the offline machine. Shell variables do not transfer:
set `REPO`, `TAG`, `COMMIT`, `ASSETS` and `WORKFLOW` again on the offline
machine, from the same independently reviewed values. Supplying both the bundle
and custom roots avoids network acquisition; there is no `--offline` flag.

Before disconnecting, obtain an independent trusted SHA-256 pin for each binary
(for example from a separately authenticated release verification record).
Do not treat SHA256SUMS delivered with the same untrusted download as that pin.
Transfer that approved pin file as `APPROVED_BINARY_SHA256SUMS`.

```bash
set -euo pipefail
test "${#ASSETS[@]}" -eq 4
shasum -a 256 --check APPROVED_BINARY_SHA256SUMS
for asset in "${ASSETS[@]}"; do
  for kind in provenance sbom; do
    predicate=https://slsa.dev/provenance/v1
    if [[ "$kind" == sbom ]]; then predicate=https://spdx.dev/Document/v2.3; fi
    gh attestation verify "$asset" --repo "$REPO" --bundle "$asset.$kind.sigstore.json" \
      --custom-trusted-root trusted_root.jsonl \
      --source-ref "refs/tags/$TAG" --source-digest "$COMMIT" \
      --signer-digest "$COMMIT" --deny-self-hosted-runners --predicate-type "$predicate" \
      --signer-workflow "$WORKFLOW" --format json > "$asset.offline-$kind-verification.json"
    if [[ "$kind" == sbom ]]; then
      jq -e --slurpfile document "$asset.spdx.json" \
        'length == 1 and .[0].verificationResult.statement.predicate == $document[0]' \
        "$asset.offline-$kind-verification.json"
    fi
  done
done
```

Offline success establishes signed build evidence for those retained bytes.
It does not establish current release membership, immutability or deployment
authorization. Retain the separate online release and asset verification record;
checksums alone cannot replace it.

## Use the verified CLI

`cargo install-cli` builds `ts` from source. The resulting local binary is not
covered by the release attestation. Operators who need an attested CLI should
download the `ts-<target>` release asset matching their platform, verify it with
the `gh attestation verify` and `gh release verify-asset` commands above, then
copy that verified file into a directory on their `PATH` as `ts` and make it
executable. Verify before first execution. Individual commands may still require
a source checkout and provider tools; see [CLI](./cli.md).

## Deploy the verified Fastly wasm

Keep the verified download unchanged. The ordinary source-build route does not
establish equality with this release, and Phase 1 adds no `ts deploy --release`
command. Use a prebuilt package route for the verified bytes.

For a manual deployment to an **existing** Fastly service, use the Fastly CLI
16.0.0 pinned in the release tag's `.tool-versions`. Prepare a fresh private
packaging directory containing only the verified wasm and a minimal manifest:

```toml
manifest_version = 3
name = "example-app"
```

Do not copy operator config, secrets, local fixtures or build hooks into that
directory. Approve the destination service and base version, and inspect their
Config, KV and Secret Store links beforehand. The wasm opens its Config Store
and Secret Store by the logical IDs in the release tag's tracked
`edgezero.toml` (`trusted_server_config` and `trusted_server_secrets`), so the
base version must already link them under those names; physical store names
and actual configuration are runtime resources. This manual route does not
create links, push configuration, implement staging, or supply recovery
guarantees.

The canonical Fastly CI job also exercises this route on its freshly built wasm,
using the `.tool-versions` CLI version and a source-pinned archive checksum.
After downloading the CLI, it clears inherited credentials and runs in a
network namespace with no active interfaces or retained capabilities. CLI
configuration and caches live only in the job's fresh temporary directory.
The job logs pack, validation, strict archive inspection and both byte comparisons;
the rehearsal transcript links that job. No deployment command is run.

From the private directory:

```bash
fastly compute pack --wasm-binary ./trusted-server-fastly.wasm
fastly compute validate --package ./pkg/example-app.tar.gz
tar -tzf ./pkg/example-app.tar.gz
tar -xOzf ./pkg/example-app.tar.gz example-app/bin/main.wasm > packaged.wasm
cmp trusted-server-fastly.wasm packaged.wasm
tar -xOzf ./pkg/example-app.tar.gz example-app/fastly.toml > packaged.toml
cmp fastly.toml packaged.toml
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

This command can activate production: it clones an active or locked base
version, uploads the package to the new version and activates it. Confirm its
behavior for the pinned CLI before use. Retain the
verified wasm, manifest, package digest, previous active version and observed
result. No build step may replace or annotate the verified wasm. Automated
EdgeZero prepared-payload deployment and operation recovery are later phases.

### Local runtime evidence

Run verified Fastly bytes with pinned Viceroy and external example configuration
for local testing. A page request can check the origin response, injected
JavaScript and source SHA in `x-ts-version`. Local execution does not establish
provider submission or production operation.

## Build inputs, policy and release administration

Builds use the tracked source, manifests, `Cargo.lock`, pinned Rust and Node,
the JS lockfile and fresh JS bundles. The tracked `edgezero.toml` supplies the
logical config-store ID and participates in the template build digest. Build
facts include the source commit. The CLI carries the fictional example-config
scaffold, not an operator config.

Release Fastly responses report `x-ts-version` as the full 40-character source
commit supplied by the release build. Development display fallbacks are not the
release identity. `ts --version` prints
`ts package=<version> source=<commit> target=<triple> variant=default`. Release
mode requires the full 40-character lowercase source commit; the package version
is not the release tag. Attestation pins authenticate these inputs externally.

The executable's SHA-256 is checked externally against the retained release
bytes; exposing that digest as a runtime build fact belongs to
[#161](https://github.com/IABTechLab/trusted-server/issues/161).

The [specification](https://github.com/IABTechLab/trusted-server/blob/main/docs/superpowers/specs/2026-10-09-1235-release-attestation-design.md#r-inputs--build-inputs)
records the input classes found by a one-time audit against an earlier main;
it is not re-run automatically. It does not claim a complete native tool closure
or hermetic build.

Releases do not yet ship `release-index.json`, `tsjs-inventory.json`,
build-evidence or build-recipe files. Their names remain reserved for later
phases.

The build scrubs inherited environment values and scans every executable for
planted fictional config values. These are regression checks, not
hermetic-build qualification. Operator configuration, credentials, physical
bindings and variable permission policy remain runtime inputs.

Before publication, a maintainer must enable immutable releases, protect `v*`
tags and approve tag-push authority and required checks. Immutability remains a
human precondition. Set repository variable `RELEASE_PUBLICATION_ENABLED=true`
only after confirming those settings with the read-only commands below. Without
it, both signing and publication are skipped; an early tag push creates no
public attestations. There is no repository-administration token or
prepublication settings API call, following the reviewed decision to check the
published release instead. A violation can expose a mutable release before the
workflow reports failure.

Tags with a SemVer prerelease part (such as `-rc.1`) publish as prereleases and
never become Latest. Stable tags publish as normal releases with
`make_latest: legacy`; GitHub chooses Latest by version/date rather than forcing
a backport release to Latest. Whether to authorize tags from rc branches is a
maintainer decision; Phase 1 requires the tagged commit to be on some branch but
does not check which one, and does not read rulesets.

Before creating anything, the publisher runs `gh attestation verify` for every
binary and predicate, and for the Fastly rebuild check, against the signing
bundles it is about to publish, with the same pins as the commands above, and
requires each signed SBOM predicate to equal the shipped SPDX document. A failure there creates nothing.

Publication then lists releases, including drafts, and refuses every existing
matching tag. It creates one draft, logs and retains its release ID, and binds
uploads and the final publish mutation to that ID. It checks the exact
signing-approved asset set, validates each upload response's size and SHA-256
digest before publication. After publication it checks that the release is
immutable and that its verified release attestation lists exactly those assets
and digests.
The reviewed publication owner remains later work. There are no automatic
mutation retries or draft-resume claims.

The nine pre-creation `gh attestation verify` runs, the tag-reference read, the
release listing before creation and every create/upload/publish mutation remain
single-shot. Each pre-creation verification times out after 60s. Every API read,
including that listing, times out after at most 10s. Mutations have no
client-side timeout and are bounded by the 15-minute job limit: aborting a
create, upload or publish cannot cancel a change GitHub already accepted.

Only the two post-publish reads retry: the release read by ID, then
`gh release verify` for the tag. GitHub creates the release attestation after
publication, so it may not be listed yet. Each read makes at most five attempts,
waiting 2s/5s/10s/20s between them, inside one three-minute window that starts
at publication. An API read times out after at most 10s and a verification after
at most 30s, and neither runs past the window. `Connection: close` remains
enabled for API calls.

Any failed read is retried. GitHub CLI 2.102.0 reports a record that is not yet
listed and a bundle-fetch or network failure as `no attestations for tag …`,
and a trusted-root failure the same way as a failed signature, so the publisher
does not classify its errors. A returned result that fails a check stops at
once: the release must have the created ID and expected release type, be
published and be immutable, and the verified release attestation must name this
repository, tag, release ID and source commit and list exactly the published
assets with their SHA-256 digests. When the attempts or the window run out, the
job fails with `Published release read failed; the release is already
published` or `Release attestation verification failed; the release is already
published`, with the last failure, if any, as its cause. The job log records every wait.

GitHub CLI fetches every bundle returned for the commit before filtering by tag.
An unavailable older bundle, for example from an earlier prerelease of the same
commit, can therefore fail `gh release verify` for the current release, in the
publisher, in the procedure above and in the recovery checks below, until that
bundle can be fetched again. Preserve the immutable release, rerun the read-only
checks later, and do not rerun publication. Filter-before-fetch behavior remains
a dependency follow-up: track it upstream and qualify a GitHub CLI version that
supplies it before claiming isolation from other tags.

If any post-publish check fails, the release has already been published; a
failure does not undo publication or establish that immutability was verified.
Confirm its state using the recorded release ID. For an immutable release, keep
its tag and assets unchanged. Set `RELEASE_ID` to the ID from the release job
log, and `REPO` and `TAG` to the reviewed values, then run these read-only
checks:

```bash
set -euo pipefail
gh api "repos/$REPO/releases/$RELEASE_ID" \
  --jq '{id, tag_name, draft, immutable, assets: [.assets[] | {name, size, digest}]}'
test "$(gh api "repos/$REPO/releases/tags/$TAG" --jq '.id')" = "$RELEASE_ID"
test "$(gh api "repos/$REPO/releases/$RELEASE_ID" --jq '.immutable')" = true
```

Then run the [download and verify](#download-and-verify) block above in a new
private directory, changing only its reviewed pins. Preserve the successful
verification outputs with the release record; do not rerun the publication job
to recover a failed read-only check.

After a failed or indeterminate mutation, inspect the recorded release ID and
all matching drafts read-only. Do not rerun uploads, delete a release, or reuse
the tag without an independently approved recovery decision. Published immutable
releases are never overwritten. A fresh release needs a fresh approved tag.
Full reconciliation and an incident owner remain later work.

Phase 1 accepts only lightweight tags whose raw tag reference points directly
to the expected source commit. Annotated tags have a separate tag-object identity
and are refused before building, and again before release creation.

Before the first production release, rehearse the reviewed commit in a separate
public repository with Actions and immutable releases enabled and the
`RELEASE_PUBLICATION_ENABLED=true` variable set. Push the commit to a branch
there first. Use a fresh, lightweight `v0.0.0-rehearsal.N` tag for each attempt
and retain verification outputs in the release PR. Keep rehearsal tags and releases out of the upstream
repository. For production, confirm the required settings and checks, then
follow the maintainer release authorization process before tagging or deploying.

## Maintainer settings checklist

An administrator must configure these settings before a production tag. The
commands below only inspect settings; this workflow applies none of them.

- **Settings → General → Releases:** enable immutable releases. See
  [GitHub's immutability instructions](https://docs.github.com/en/code-security/how-tos/secure-your-supply-chain/establish-provenance-and-integrity/prevent-release-changes).
- **Settings → Rules → Rulesets → New tag ruleset:** set enforcement to **Active**,
  target `v*` (API pattern `refs/tags/v*`), and enable **Restrict creations**,
  **Restrict updates** and **Restrict deletions**. Add only the designated release
  maintainers' team to the bypass list, with **Always allow** bypass. Record the
  team's name and ID in the maintainer release record; avoid broad role bypasses.
  See [repository ruleset configuration](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/creating-rulesets-for-a-repository).
- Before tagging, confirm that the release build checks passed on the `main` push
  of the commit you intend to tag: the `Binary attestations` workflow's
  `build / checks`, four `build / Build <asset>`,
  `build / Reproduce trusted-server-fastly.wasm` and
  `build / Compare ordinary Fastly release bytes` check runs, which the
  `check-runs` command below lists. Release builds run only on `main` pushes and
  `v*` tags, so commits on other branches have no such checks, and a
  tag-triggered check cannot pass before its own tag exists. Before authorizing
  a tag on another branch, such as an rc branch, rehearse that exact commit in a
  separate repository as described above; its tag build is otherwise the first
  release build it gets. A later `main` push cancels an older run that is still
  waiting in the workflow's concurrency group; re-run a cancelled run before
  tagging its commit. Phase 1 does not enforce source eligibility.
- Push the commit to a branch, normally `main`, before pushing its lightweight
  tag. The build `checks` job refuses a tagged commit that is on no branch,
  before anything is built or signed: in fork rehearsals, draft creation
  returned HTTP 403 for such a commit. The check reads the branches when the job
  runs, so after that refusal push the commit to a branch and re-run the failed
  jobs; nothing was built or signed. A release job that fails before creating a
  draft (handoff or signing-bundle verification, or the tag reference), or whose
  draft creation returns HTTP 403 (for example, because the branch was deleted
  after the check), also creates nothing. If the release listing then shows no
  release or draft for the tag, re-run only the failed release job within the
  7-day artifact retention, after pushing the commit to a branch again for a 403.
  This rerun is the approved recovery for those failures only; other failed
  mutations follow the recovery rules above. An annotated tag is refused before
  building.

```bash
REPO=IABTechLab/trusted-server
# Replace with the full commit you intend to tag.
COMMIT=0123456789abcdef0123456789abcdef01234567
gh api "repos/$REPO/immutable-releases"
gh api "repos/$REPO/rulesets?includes_parents=true" \
  --jq '.[] | {id, name, target, enforcement}'
RULESET_ID=12345
gh api "repos/$REPO/rulesets/$RULESET_ID?includes_parents=true" \
  --jq '{name, target, enforcement, conditions, rules, bypass_actors}'
gh api --paginate "repos/$REPO/commits/$COMMIT/check-runs?per_page=100" \
  --jq '.check_runs[] | {name, status, conclusion}'
```

The immutable-releases endpoint must return `enabled: true`. It requires admin
read access: a disabled setting can return `enabled: false`, and HTTP 404 can
also mean the caller lacks that access, so treat any other result as
unconfirmed. Inspect the ruleset's tag target, active enforcement, exact
patterns, creation/update/deletion rules and team bypass actors. The API exposes
bypass actors only to callers with sufficient repository access; an omitted field
is not proof of an empty bypass list. Also review inherited rulesets. These are
maintainer gates, not evidence that Phase 1's publication token can administer
repository settings.
