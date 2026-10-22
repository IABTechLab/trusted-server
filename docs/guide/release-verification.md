# Verify release binaries

Phase 1 publishes seven binaries, seven SPDX 2.3 SBOMs, fourteen Sigstore bundles
and `SHA256SUMS`: **29 immutable release assets**. Each binary has SLSA build
provenance and an SBOM attestation. Verify the downloaded bytes before execution
or deployment. Reproducibility, complete composition and the custom Trusted
Server verifier remain later phases.

The tag caller is `.github/workflows/release.yml`. Its local reusable
`.github/workflows/release-build.yml` owns builds and signing at the same source
revision. The reusable workflow is the certificate signer and provenance builder;
the caller remains the publication owner. Build jobs have no signing authority,
and signing jobs do not check out, compile or execute the downloaded binaries.

## Platforms and coverage

| Asset                                           | Phase 1 baseline and qualification limit                                                                           |
| ----------------------------------------------- | ------------------------------------------------------------------------------------------------------------------ |
| `trusted-server-fastly.wasm`                    | Fastly Compute, `wasm32-wasip1`, default features; reproducibility and hosted runtime qualification remain pending |
| `ts-x86_64-unknown-linux-gnu`                   | GNU x86-64; UBI 9 builder; maximum required glibc 2.34; Debian 12, Ubuntu 22.04 and UBI 9 startup smokes           |
| `ts-aarch64-apple-darwin`                       | macOS 15 Apple Silicon runner, deployment target 15.0; runtime compatibility qualification remains pending         |
| `ts-x86_64-apple-darwin`                        | macOS 15 Intel runner, deployment target 15.0; runtime compatibility qualification remains pending                 |
| `trusted-server-axum-x86_64-unknown-linux-gnu`  | GNU x86-64; UBI 9 build; maximum glibc 2.34; Debian 12, Ubuntu 22.04 and UBI 9 startup smokes                      |
| `trusted-server-axum-x86_64-unknown-linux-musl` | Fully static x86-64; musl 1.2.4 build; pinned Alpine and empty-rootfs startup smokes                               |
| `trusted-server-axum-aarch64-unknown-linux-gnu` | ARM64 GNU; native ARM UBI 9 build; maximum glibc 2.34; Debian 12 startup smoke                                     |

The retained destination design calls for a macOS 13.0 deployment target;
Phase 1 still builds with 15.0. Lowering that floor and proving older-runtime
compatibility are additive later work.
The executables are not notarized. GNU builds require no GLIBC symbol newer than 2.34; glibc 2.34 and newer are
the selected baseline. This includes
Debian 12/bookworm-slim, Ubuntu 22.04+ and RHEL/UBI 9+. Distroless
cc-debian12 has a compatible libc floor but is not a Phase 1 smoke image.
The musl asset needs no dynamic loader or libraries; Alpine and an empty-rootfs
container are required smoke images, while distroless-static is another deployment option
without a separate Phase 1 runtime test. Build logs record acquired runtime image
IDs. These required startup checks do not establish complete platform/tool qualification.
The expanded seven-binary batch awaits hosted CI and rehearsal `.5`; `.4`
proves only the original four binaries. Native
libraries and provider tools needed by individual `ts` commands retain their
existing prerequisites. Cloudflare, Spin, Windows and Linux ARM64 CLI releases follow separately.

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
  ts-aarch64-apple-darwin ts-x86_64-apple-darwin \
  trusted-server-axum-x86_64-unknown-linux-gnu \
  trusted-server-axum-x86_64-unknown-linux-musl \
  trusted-server-axum-aarch64-unknown-linux-gnu)

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

The checksums cover the other 28 assets, never `SHA256SUMS` itself. Immutable
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
pins and trusted roots to the offline machine. Reuse `REPO`, `TAG`, `COMMIT`,
`ASSETS`, `WORKFLOW` and `IDENTITY` from above. Supplying both the bundle and
custom roots avoids network acquisition; there is no `--offline` flag.

```bash
for asset in "${ASSETS[@]}"; do
  for kind in provenance sbom; do
    predicate=https://slsa.dev/provenance/v1
    if [[ "$kind" == sbom ]]; then predicate=https://spdx.dev/Document/v2.3; fi
    checks=(--repo "$REPO" --bundle "$asset.$kind.sigstore.json"
      --custom-trusted-root trusted_root.jsonl
      --source-ref "refs/tags/$TAG" --source-digest "$COMMIT"
      --signer-digest "$COMMIT" --deny-self-hosted-runners --predicate-type "$predicate")
    gh attestation verify "$asset" "${checks[@]}" --signer-workflow "$WORKFLOW"
    gh attestation verify "$asset" "${checks[@]}" --cert-identity "$IDENTITY" \
      --format json > "$asset.offline-$kind-verification.json"
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
a source checkout and provider tools; see [CLI](/guide/cli).

## Run a verified Axum release

Select one of the three `trusted-server-axum-<target>` assets above, download it
and complete the same online/offline attestation and immutable membership checks
before execution. The aarch64 GNU variant is for ARM servers. Copy the verified
file into your deployment directory, make it executable, and retain its digest.

Application configuration and secrets stay external. With a reviewed checkout's
`edgezero.toml` and adapter manifests available, use the verified `ts` CLI to
validate and publish your private `trusted-server.toml` into the local config
store envelope; this does not rebuild Axum:

```bash
ts config push --adapter axum --local --manifest ./edgezero.toml \
  --app-config ./trusted-server.toml --no-diff --yes
export TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG_TRUSTED_SERVER_CONFIG="$(
  jq -er '.trusted_server_config' .edgezero/local-config-trusted_server_config.json
)"
```

Supply these runtime secret environment variables through your private secret
management, matching the secret key references in your configuration:

- `TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_HANDLER_PASSWORD`
- `TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_PUBLISHER_PROXY_SECRET`
- `TRUSTED_SERVER_SECRET_TRUSTED_SERVER_SECRETS_EC_PASSPHRASE`

For example, after verifying the x86-64 GNU asset and supplying those variables:

```bash
chmod u+x ./trusted-server-axum-x86_64-unknown-linux-gnu
PORT=8080 ./trusted-server-axum-x86_64-unknown-linux-gnu
```

`PORT` binds Axum to loopback (`127.0.0.1`); use your reviewed reverse proxy and
TLS configuration for public traffic. Axum has no help/version or dry-run mode.
Its build smoke starts an isolated server with a fictional external config
envelope and requires `/health` to return `200` and `ok`, distinguishing a
configured router from the configuration-error router. This is a startup check,
not production proxy/load qualification.

An Axum source build, like `cargo install-cli`, is not covered by these release
attestations. The musl asset is fully static; no allocator change is included.
Later work should consider mimalloc/jemalloc for musl under load.

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

### Local runtime evidence

The [rehearsal runtime transcript](https://github.com/IABTechLab/trusted-server/pull/1435#issuecomment-6346428668)
records local execution of the verified Fastly wasm at
`922c2211c0cc7941bfc8ffa9f775ef8ac0971232` using pinned Viceroy 0.21.0.
It uses the example configuration with loopback publisher/origin values,
public development secrets and a local Node origin fixture. A real page request
returned HTTP 200, the fixture marker, injected JS and the full source SHA in
`x-ts-version`. Both the wasm and the CLI used to seed local config were verified
before execution. No Fastly service was deployed. This proves the local prebuilt
execution route; provider submission and production operation remain separate.

## Build inputs, policy and release administration

Builds use the tracked source, manifests, `Cargo.lock`, pinned Rust and Node,
the JS lockfile and fresh JS bundles. The tracked `edgezero.toml` supplies the
logical config-store ID and participates in the template build digest. Build
facts include the source commit. The CLI carries the fictional example-config
scaffold, not an operator config.

Release Fastly responses report `x-ts-version` as the full 40-character source
commit supplied by the release build. Development display fallbacks are not the
release identity. `ts --version` reports the command name `ts` and Cargo package version, not a
source-commit field. Source identity comes from the attestation pins above.
The executable's SHA-256 is checked externally against the retained release
bytes; exposing that digest as a runtime build fact belongs to #161.

The current compile-time input audit is recorded in the spec's Phase 1 note.
It includes Rust 1.98.1, tracked logical IDs, the private JS build path and its
indirect build-module options, and the locked native dependencies. It does not
claim a complete native tool closure or hermetic build.

The tools and consumer policy files are carried unchanged. The cohort retains
its original four entries and adds the three Axum entries documented in the
[release PR’s compatibility audit](https://github.com/IABTechLab/trusted-server/pull/1435).
All three have **no Phase 1 readers** and do not authorize these builds or replace
the workflow's explicit selections.

| Policy                                          | Current use                                                            | Additive later adoption                                                                          |
| ----------------------------------------------- | ---------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `scripts/release/release-cohort.json`           | Original four plus three descriptive Axum entries; no reader           | Checked fixed cohort selection, original assurance profiles and compatibility qualification      |
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

Only post-publish reads may retry: release/asset metadata GETs and the GitHub
CLI release/asset verification calls. Each check has at most three attempts,
with 2s then 5s backoff. A supplied `Retry-After` can increase a delay, capped
at 10s (at most 20s waiting per check). A shared three-minute window bounds
retry scheduling across the job, and individual read requests/commands time out
after 10s. `Connection: close` remains enabled for API calls.

Retries require a recognized transport error, timeout, HTTP 5xx or 429.
GitHub CLI errors are classified conservatively: an otherwise empty-output
HTTP 5xx/429 diagnostic or a recognized transport/timeout code may retry;
ambiguous failures stop. Successful responses with wrong identity, immutability,
membership or digests, and definitive verification failures stop immediately.
Release listing before creation and every create/upload/publish mutation remain
single-shot.

If these retries are exhausted, the release has already been published; a
failure does not undo publication or establish that immutability was verified.
Confirm its state using the recorded release ID. For an immutable release, keep
its tag and assets unchanged and rerun only the following read-only checks in
the directory containing the downloaded assets. Set `RELEASE_ID` to the ID from
the release job log; retain the reviewed `REPO`, `TAG`, `COMMIT` and `ASSETS`
values from the verification procedure above.

```bash
gh api "repos/$REPO/releases/$RELEASE_ID" \
  --jq '{id, tag_name, draft, immutable, assets: [.assets[] | {name, size, digest}]}'
test "$(gh api "repos/$REPO/releases/tags/$TAG" --jq '.id')" = "$RELEASE_ID"
test "$(gh api "repos/$REPO/releases/$RELEASE_ID" --jq '.immutable')" = true
gh release verify "$TAG" --repo "$REPO"
for asset in "${ASSETS[@]}"; do
  for suffix in '' .spdx.json .provenance.sigstore.json .sbom.sigstore.json; do
    gh release verify-asset "$TAG" "$asset$suffix" --repo "$REPO"
  done
done
gh release verify-asset "$TAG" SHA256SUMS --repo "$REPO"
shasum -a 256 --check SHA256SUMS
```

Then rerun both signer identity checks for provenance and SPDX for each binary,
including the SPDX predicate comparison, exactly as documented above. Preserve
the successful verification outputs with the release record; do not rerun the
publication job to recover a failed read-only check.

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
- Optionally require the reviewed release build checks on the source branch
  before approving a tag. Use actual check names from that commit. A tag-triggered
  check cannot be required to pass before creating its own tag. Source eligibility
  and the later policy reader are not enforced by Phase 1.

```bash
REPO=IABTechLab/trusted-server
gh api "repos/$REPO/immutable-releases"
gh api "repos/$REPO/rulesets?includes_parents=true" \
  --jq '.[] | {id, name, target, enforcement}'
RULESET_ID=12345
gh api "repos/$REPO/rulesets/$RULESET_ID?includes_parents=true" \
  --jq '{name, target, enforcement, conditions, rules, bypass_actors}'
gh api "repos/$REPO/commits/$COMMIT/check-runs" \
  --jq '.check_runs[] | {name, status, conclusion}'
```

The immutable-releases endpoint must return `enabled: true`; a disabled setting
returns HTTP 404. Inspect the ruleset's tag target, active enforcement, exact
patterns, creation/update/deletion rules and team bypass actors. The API exposes
bypass actors only to callers with sufficient repository access; an omitted field
is not proof of an empty bypass list. Also review inherited rulesets. These are
maintainer gates, not evidence that Phase 1's publication token can administer
repository settings.
