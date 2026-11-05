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
The caller's publication job checks out only `scripts/release/publish.mjs`, its
release-notes template and `.tool-versions` from the tagged commit, and runs
them with the pinned Node and a checksum-verified GitHub CLI 2.102.0.

## Platforms and coverage

| Asset                         | Phase 1 baseline and qualification limit                                                                           |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `trusted-server-fastly.wasm`  | Fastly Compute, `wasm32-wasip1`, default features; reproducibility and hosted runtime qualification remain pending |
| `ts-x86_64-unknown-linux-gnu` | GNU x86-64; UBI 9 builder; maximum required glibc 2.34; Debian 12, Ubuntu 22.04 and UBI 9 startup smokes           |
| `ts-aarch64-apple-darwin`     | macOS 15 Apple Silicon runner, deployment target 13.0; runtime compatibility qualification remains pending         |
| `ts-x86_64-apple-darwin`      | macOS 15 Intel runner, deployment target 13.0; runtime compatibility qualification remains pending                 |

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
conversion preserves package identities but drops dependency relationships; the
document adds only the binary-root `DESCRIBES` relationship.

The inventory includes npm build tools and may include packages removed by
linking, tree-shaking or minification. The installed `prebid.js` package is a
build-tree entry; the external Prebid bundle is not embedded in tsjs. Native
C/assembly, system libraries, Rust's standard library and compiler runtimes do
not have complete coverage. Exact retained-code attribution and complete native
and tool closure remain Phase 2 work. An SBOM or provenance result is not a
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
individually approved, never-reused rehearsal tag.

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
gh release verify-asset "$TAG" SHA256SUMS --repo "$REPO"
shasum -a 256 --check SHA256SUMS
```

<!-- release-verification:end -->

Provenance covers the binary and its SPDX document; the SBOM attestation covers
the binary. The verified SBOM predicate must equal the shipped SPDX document.
`--signer-workflow` pins the reusable signer; with the source ref and both
digests pinned to the reviewed tag and commit, it identifies the exact signing
workflow run's source.

The checksums cover the other 16 assets, never `SHA256SUMS` itself. Immutable
release verification establishes membership; provenance establishes the
reviewed signer and source. Both are required.

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
Config, KV and Secret Store links beforehand. The logical config-store ID comes
from the release tag's tracked `edgezero.toml`; physical store names and actual
configuration are runtime resources. This manual route does not create links,
push configuration, implement staging, or supply recovery guarantees.

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

This command can activate production and change an active/locked base version
by cloning it; confirm its behavior for the pinned CLI before use. Retain the
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

Phase 1 does not ship `release-index.json`, `tsjs-inventory.json`, build-evidence,
build-recipe or rebuild-check files. Their names remain reserved for later
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
maintainer decision; Phase 1 does not validate branch ancestry or rulesets.

Before creating anything, the publisher runs `gh attestation verify` for every
binary and predicate against the signing bundles it is about to publish, with
the same pins as the commands above, and requires each signed SBOM predicate to
equal the shipped SPDX document. A failure there creates nothing.

Publication then lists releases, including drafts, and refuses every existing
matching tag. It creates one draft, logs and retains its release ID, and binds
uploads and the final publish mutation to that ID. It checks the exact
signing-approved asset set, validates each upload response's size and SHA-256
digest before publication, and checks immutable membership after publication.
The reviewed publication owner remains later work. There are no automatic
mutation retries or draft-resume claims.

The tag-reference read, the release listing before creation and every
create/upload/publish mutation remain single-shot. Every API read, including
that listing, times out after at most 10s; release-attestation list reads use at
most 4s. Mutations have no client-side timeout and are bounded by the 15-minute
job limit: aborting a create, upload or publish cannot cancel a change GitHub
already accepted.

Only post-publish reads may retry: the release metadata GETs by ID and by tag,
the release-attestation list read and failed `gh release verify` proof fetches.
Each read has at most three attempts, with 2s then 5s backoff. A supplied
`Retry-After` can increase a delay, capped at 10s. A shared three-minute window,
starting at publication, bounds retry scheduling, and each read times out no
later than that window's end. `Connection: close` remains enabled for API calls.
Retries require a recognized transport error, timeout, HTTP 5xx or 429. GitHub
CLI errors are classified conservatively: an otherwise empty-output HTTP 5xx/429
diagnostic or a recognized transport/timeout code may retry; ambiguous failures
stop, except the release verification described below. Successful responses with
wrong identity, immutability, membership or digests stop immediately.

Before cryptographic release verification, the availability check runs at most
five checks, each with one release-attestation list read, and waits
2s/5s/10s/20s between them (37s total waiting). Each list read retries transient
failures like the other post-publish reads, so the check can make up to 15
GETs. Only this endpoint treats HTTP 404 as no record yet; a valid HTTP 200 with
no current-tag record may also wait. Other non-200 statuses fail. A separate 60s
deadline bounds availability checking within the shared three-minute window.

A record may carry its bundle inline or only a `bundle_url`; without an inline
bundle its tag is unknown until GitHub CLI fetches it. GitHub CLI reports a
bundle-fetch, trusted-root or network failure the same way as a record that is
not yet listed, so any failed `gh release verify` is retried at the next
availability check. A fetched result is then checked: its decoded bundle must
equal the statement GitHub CLI verified, name this release, commit and exact
asset digests, and equal the inline bundle when the API supplied one. Malformed
records, duplicate current-tag records and identity/source/asset mismatches stop
immediately.

The availability check ends with one of these errors:

- the last `gh release verify` error, after the fifth check or when the 60s
  deadline leaves no time for another check;
- `Release attestation availability window exhausted`, at the deadline without
  a failed proof;
- `Release already published and immutable; release attestation unavailable
after five checks`, after the fifth check without a failed proof;
- `Release attestation availability retry window exhausted`, when the deadline
  cuts a transient retry short, with the failure that caused the retry as its
  cause;
- `Release attestation availability verification window exhausted`, when a read
  starts or finishes after the deadline.

Other-tag records are logged and ignored by the publisher's own availability
check, but GitHub CLI still fetches all returned bundle URLs before filtering by
tag. An unavailable older bundle can therefore fail the current release's
read-only verification. Preserve the immutable release and follow the read-only
recovery procedure below. Filter-before-fetch behavior remains a dependency
follow-up: track it upstream and qualify a GitHub CLI version that supplies it
before claiming isolation from other tags. Logs show every wait or availability
on the first check.

If any post-publish check fails, the release has already been published; a
failure does not undo publication or establish that immutability was verified.
Confirm its state using the recorded release ID. For an immutable release, keep
its tag and assets unchanged and rerun only the following read-only checks in
the directory containing the downloaded assets. Set `RELEASE_ID` to the ID from
the release job log; retain the reviewed `REPO`, `TAG`, `COMMIT` and `ASSETS`
values from the verification procedure above.

```bash
set -euo pipefail
test "${#ASSETS[@]}" -eq 4
gh api "repos/$REPO/releases/$RELEASE_ID" \
  --jq '{id, tag_name, draft, immutable, assets: [.assets[] | {name, size, digest}]}'
test "$(gh api "repos/$REPO/releases/tags/$TAG" --jq '.id')" = "$RELEASE_ID"
test "$(gh api "repos/$REPO/releases/$RELEASE_ID" --jq '.immutable')" = true
test "$(gh release verify "$TAG" --repo "$REPO" --format json \
  --jq '.verificationResult.statement.predicate.tag')" = "$TAG"
gh release verify "$TAG" --repo "$REPO"
for asset in "${ASSETS[@]}"; do
  for suffix in '' .spdx.json .provenance.sigstore.json .sbom.sigstore.json; do
    gh release verify-asset "$TAG" "$asset$suffix" --repo "$REPO"
  done
done
gh release verify-asset "$TAG" SHA256SUMS --repo "$REPO"
shasum -a 256 --check SHA256SUMS
```

Then rerun the signer identity check for provenance and SPDX for each binary,
including the SPDX predicate comparison, exactly as documented above. Preserve
the successful verification outputs with the release record; do not rerun the
publication job to recover a failed read-only check.

After a failed or indeterminate mutation, inspect the recorded release ID and
all matching drafts read-only. Do not rerun uploads, delete a release, or reuse
the tag without an independently approved recovery decision. Published immutable
releases are never overwritten. A fresh release needs a fresh approved tag;
Phase 2's full reconciliation and incident owner remains deferred.

Phase 1 accepts only lightweight tags whose raw tag reference points directly
to the expected source commit. Annotated tags have a separate tag-object identity
and are refused before building, and again before release creation.

Before the first production release, rehearse the reviewed commit in a separate
public repository with Actions and immutable releases enabled. Use a fresh,
lightweight `v0.0.0-rehearsal.N` tag for each attempt and retain verification
outputs in the release PR. Keep rehearsal tags and releases out of the upstream
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
  of the commit you intend to tag (the `check-runs` command below lists them).
  Release builds run only on `main` pushes and `v*` tags, so commits on other
  branches have no such checks, and a tag-triggered check cannot pass before its
  own tag exists. A later `main` push cancels an older run that is still waiting
  in the workflow's concurrency group; re-run a cancelled run before tagging its
  commit. Source eligibility and the later policy reader are not enforced by
  Phase 1.
- Push the commit to a branch, normally `main`, before pushing its lightweight
  tag. The build `checks` job refuses a tagged commit that is on no branch,
  before anything is built or signed: in fork rehearsals, draft creation
  returned HTTP 403 for such a commit. After that refusal, push the commit to a
  branch and use a fresh tag. If a draft creation still returns HTTP 403 and the
  release listing shows no release or draft for the tag, re-run only the failed
  release job within the 7-day artifact retention. This rerun is the approved
  recovery for that failure only; other failed mutations follow the recovery
  rules above. An annotated tag is refused before building.

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
