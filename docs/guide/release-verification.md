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
| `ts-aarch64-apple-darwin`                       | macOS 15 Apple Silicon runner, deployment target 13.0; runtime compatibility qualification remains pending         |
| `ts-x86_64-apple-darwin`                        | macOS 15 Intel runner, deployment target 13.0; runtime compatibility qualification remains pending                 |
| `trusted-server-axum-x86_64-unknown-linux-gnu`  | GNU x86-64; UBI 9 build; maximum glibc 2.34; Debian 12, Ubuntu 22.04 and UBI 9 startup smokes                      |
| `trusted-server-axum-x86_64-unknown-linux-musl` | Fully static x86-64; musl-tools 1.2.4-2; pinned Alpine and scratch-like startup smokes with runtime CA data        |
| `trusted-server-axum-aarch64-unknown-linux-gnu` | ARM64 GNU; native ARM UBI 9 build; maximum glibc 2.34; Debian 12 startup smoke                                     |

The build checks 64-bit Mach-O magic, target CPU and minimum macOS 13.0 load commands. Actual execution on macOS 13 remains later qualification.
The executables are not notarized. GNU builds require no GLIBC symbol newer than 2.34; glibc 2.34 and newer are
the selected baseline. This includes
Debian 12/bookworm-slim, Ubuntu 22.04+ and RHEL/UBI 9+. Distroless
cc-debian12 has a compatible libc floor but is not a Phase 1 smoke image.
The musl asset needs no dynamic loader or libraries; Alpine and a scratch-like
container are required smoke images, while distroless-static is another deployment option
without a separate Phase 1 runtime test. All acquired smoke images are digest-pinned; build logs record their image IDs. These required startup checks do not establish complete platform/tool qualification.
Axum defaults to `127.0.0.1:8787`. Set runtime `EDGEZERO__ADAPTER__HOST=0.0.0.0`
and `EDGEZERO__ADAPTER__PORT=8080` for containers and publish that port. `PORT`
is a legacy alias. Ports must be 1–65535; zero is rejected. When both are set, they must agree; both values are
validated and any invalid explicit value fails startup. This runtime setting is never compiled into the executable.
The smoke reaches a bridge-networked container through a host-published port;
loopback-only binding cannot pass it. TLS termination, graceful shutdown and
request/resource limits remain follow-up production hardening.

Axum's TLS client requires a runtime CA certificate bundle, including for health
request initialization. Every Axum smoke mounts the runner's system CA
bundle read-only at `/ca.pem` with `SSL_CERT_FILE=/ca.pem`; production operators
must supply their own runtime trust store. This is runtime data, not an embedded
configuration or dynamically linked library.
Native libraries and provider tools needed by individual `ts` commands retain their existing prerequisites. Cloudflare, Spin, Windows and Linux ARM64 CLI releases follow separately.

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
Phase 1 requires a lightweight tag pointing directly to that source commit.
The retained strict SemVer validation remains deferred; use a separately approved tag. A checksum or commit supplied only with
an untrusted download is not source approval.

Run this in a new private directory, using Bash. Replace the fictional tag and
commit. For rehearsal, change `REPO` to the approved public sandbox and use the
individually approved, never-reused rehearsal tag.

<!-- release-verification:start -->

```bash
set -euo pipefail
REPO=IABTechLab/trusted-server
TAG=v1.2.3
COMMIT=0123456789abcdef0123456789abcdef01234567
ASSETS=(trusted-server-axum-x86_64-unknown-linux-gnu trusted-server-axum-x86_64-unknown-linux-musl trusted-server-axum-aarch64-unknown-linux-gnu trusted-server-fastly.wasm ts-aarch64-apple-darwin ts-x86_64-apple-darwin ts-x86_64-unknown-linux-gnu)
gh release download "$TAG" --repo "$REPO" --dir .
test "$(gh api "repos/$REPO/releases/tags/$TAG" --jq .immutable)" = true
gh release verify "$TAG" --repo "$REPO"
for asset in "${ASSETS[@]}"; do
  for suffix in "" .spdx.json .provenance.sigstore.json .sbom.sigstore.json; do gh release verify-asset "$TAG" "$asset$suffix" --repo "$REPO"; done
  for kind in provenance sbom; do
    predicate=https://slsa.dev/provenance/v1; if [[ "$kind" == sbom ]]; then predicate=https://spdx.dev/Document/v2.3; fi
    checks=(--repo "$REPO" --bundle "$asset.$kind.sigstore.json" --source-ref "refs/tags/$TAG" --source-digest "$COMMIT" --signer-digest "$COMMIT" --deny-self-hosted-runners --predicate-type "$predicate")
    gh attestation verify "$asset" "${checks[@]}" --signer-workflow "$REPO/.github/workflows/release-build.yml" --format json > "$asset.$kind.verified.json"
    if [[ "$kind" == sbom ]]; then jq -e --slurpfile sbom "$asset.spdx.json" '[.[].verificationResult.statement.predicate] | length == 1 and .[0] == $sbom[0]' "$asset.$kind.verified.json"; fi
    gh attestation verify "$asset" "${checks[@]}" --cert-identity "https://github.com/$REPO/.github/workflows/release-build.yml@refs/tags/$TAG"
  done
done
gh release verify-asset "$TAG" SHA256SUMS --repo "$REPO"
shasum -a 256 --check SHA256SUMS
```

<!-- release-verification:end -->

Provenance covers the binary and its SPDX document; the SBOM attestation covers the binary. The verified SBOM predicate must equal the shipped SPDX document. `--signer-workflow` matches a prefix; the separate `--cert-identity` check pins the exact reusable workflow at the tag. GitHub CLI makes those two identity options mutually exclusive.

The checksums cover the other 28 assets, never `SHA256SUMS` itself. Immutable release verification establishes membership; provenance establishes the reviewed signer and source. Both are required.

WORKFLOW and IDENTITY below refer to the same reusable signer:

```bash
WORKFLOW="$REPO/.github/workflows/release-build.yml"
IDENTITY="https://github.com/$WORKFLOW@refs/tags/$TAG"
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

Before disconnecting, obtain an independent trusted SHA-256 pin for each binary
(for example from a separately authenticated release verification record).
Do not treat SHA256SUMS delivered with the same untrusted download as that pin.
Transfer that approved pin file as `APPROVED_BINARY_SHA256SUMS`.

```bash
shasum -a 256 --check APPROVED_BINARY_SHA256SUMS
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
EDGEZERO__ADAPTER__PORT=8080 ./trusted-server-axum-x86_64-unknown-linux-gnu
```

With `EDGEZERO__ADAPTER__HOST` unset, either port setting retains loopback
(`127.0.0.1`). Setting `EDGEZERO__ADAPTER__HOST` to a non-loopback address
(such as `0.0.0.0`) is an explicit operator choice that exposes the listener
on that interface. Use your reviewed reverse proxy and
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

Run verified Fastly bytes with pinned Viceroy and external example configuration for local testing. A page request can check the origin response, injected JavaScript and source SHA in `x-ts-version`. Default single mode emits no Build facts log. Local execution does not establish provider submission or production operation.

## Build inputs, policy and release administration

Builds use the tracked source, manifests, `Cargo.lock`, pinned Rust and Node,
the JS lockfile and fresh JS bundles. The tracked `edgezero.toml` supplies the
logical config-store ID and participates in the template build digest. Build
facts include the source commit. The CLI carries the fictional example-config
scaffold, not an operator config.

Release Fastly responses report `x-ts-version` as the full 40-character source
commit supplied by the release build. Development display fallbacks are not the
release identity. `ts --version` labels `package=<version> source=<commit> target=<triple> variant=default`. Release mode requires the full 40-character lowercase source commit; the package version is not the release tag. Attestation pins authenticate these inputs externally.
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
human precondition. Set repository variable `RELEASE_PUBLICATION_ENABLED=true` only after confirming those settings with the read-only commands below. Without it, both signing and publication are skipped; an early tag push creates no public attestations. There is no repository-administration token or prepublication settings API call, following the reviewed decision to check the published release instead. A
violation can expose a mutable release before the workflow reports failure.

Tags containing `-` publish as prereleases and never become Latest. Stable tags publish as normal releases with `make_latest: legacy`; GitHub chooses Latest by version/date rather than forcing a backport release to Latest. Whether to authorize tags from rc branches is a maintainer decision; the Phase 2 ancestry/ruleset validator is not implemented here.

Publication lists releases, including drafts, and refuses every existing
matching tag. It creates one draft, retains its release ID, and binds uploads
and the final publish mutation to that ID. It checks the exact signing-approved asset
set, validates each upload response's size and SHA-256 digest before publication,
and checks immutable membership after publication. Published assets are then re-downloaded and byte-compared against the signed `SHA256SUMS` and validated handoff. The reviewed publication owner remains additive later work. There are no
automatic mutation retries or draft-resume claims.

Only post-publish reads may retry: release/asset metadata GETs and the GitHub
CLI asset verification calls. Each check has at most three attempts,
with 2s then 5s backoff. A supplied `Retry-After` can increase a delay, capped
at 10s (at most 20s waiting per check). A shared three-minute window bounds
retry scheduling across the job, and individual read requests/commands time out
after 10s, except bulk asset downloads, which have up to 60s capped by the remaining shared deadline. `Connection: close` remains enabled for API calls.

Retries require a recognized transport error, timeout, HTTP 5xx or 429.
GitHub CLI errors are classified conservatively: an otherwise empty-output
HTTP 5xx/429 diagnostic or a recognized transport/timeout code may retry;
ambiguous failures stop. Successful responses with wrong identity, immutability,
membership or digests, and definitive verification failures stop immediately.
Release listing before creation and every create/upload/publish mutation remain
single-shot. Every API read, including that listing, times out after 10s. Mutations
have no client-side timeout and are bounded by the 15-minute job limit: aborting a
create, upload or publish cannot cancel a change GitHub already accepted.

Before cryptographic release verification, the availability check makes at most five release-attestation GETs, with 2s/5s/10s/20s backoff (37s total waiting). Only this endpoint treats HTTP 404 as no record yet; a valid HTTP 200 with no current-tag record may also wait. Each GET times out after at most 4s, and a separate 60s deadline bounds availability checking within the shared three-minute scheduling window. Every other non-200 status fails immediately. Malformed records, duplicate current-tag records, identity/source/asset mismatches and definitive cryptographic failures stop immediately. Other-tag records are logged and ignored by the publisher's own availability check, but GitHub CLI still fetches all returned bundle URLs before filtering by tag. An unavailable older bundle can therefore fail the current release's read-only verification. Preserve the immutable release and follow the read-only recovery procedure below. Filter-before-fetch behavior remains a dependency follow-up: track it upstream and qualify a GitHub CLI version that supplies it before claiming isolation from other tags. Current-tag cryptographic verification is single-shot. Logs show every wait or availability on the first check. No mutation is retried.

The publisher re-downloads all 29 assets into a fresh temporary directory, checks the exact file set and byte-compares them with the validated signed handoff, including identical SHA256SUMS. Only transport download failures may retry; any missing, extra or different file fails immediately. Temporary downloads are removed after each attempt.

If the availability check exhausts its bound, the release is already published and immutable (verified before availability polling). Keep the tag and release unchanged and use the read-only recovery commands below.

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
gh api "repos/$REPO/attestations/sha1:$COMMIT?per_page=100&predicate_type=release" \
  --jq '[.attestations[] | select(.initiator == "github") | .bundle.dsseEnvelope.payload | @base64d | fromjson | .predicate.tag]'
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

Phase 1 accepts only lightweight tags whose raw tag reference points directly
to the expected source commit. Annotated tags have a separate tag-object identity
and are rejected before release creation, upload or publication.

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
# Replace with the full commit you intend to tag.
COMMIT=0123456789abcdef0123456789abcdef01234567
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

### Phase 1 review boundary notes

The reusable caller declares the maximum signing permissions required by its
called workflow. GitHub does not allow a read-only caller to invoke a nested
workflow that declares greater permissions, even for conditionally skipped jobs.
Actual build/check jobs narrow permissions to contents-read; signing remains
tag-only and requires the publication enablement variable. Splitting a new build-only workflow is deferred to preserve this slice.

The configuration scanner covers literal/raw/concatenated macros and literal
build-script deployment prefixes. It is a best-effort source regression audit,
not Rust expansion or dataflow analysis: `include!`, raw byte strings (`br"…"` /
`rb"…"`), computed names, procedural macros and third-party build scripts are not
completely analyzed. Planted canary scans and
clean inherited environments provide additional checks, not hermeticity.
