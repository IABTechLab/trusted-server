# Release verification

`ts release verify` checks one official release executable and its complete
attestation graph against an independently approved source commit. It prints a
read-only report. It does not install, export or deploy the executable.

## Establish the verifier first

Before executing any downloaded `ts`, obtain a trusted verifier independently.
Use a previously trusted verifier, or inspect and build the verifier from an
independently approved source checkout with its reviewed lockfile and toolchain.
For example, in that approved checkout on Apple Silicon macOS:

```bash
cargo build --locked --package trusted-server-cli --target aarch64-apple-darwin
TRUSTED_TS="$PWD/target/aarch64-apple-darwin/debug/ts"
```

Use the corresponding native host target on Linux. An approved-source build
establishes trust in that verifier's source; it does not establish official binary
provenance for the verifier itself. Do not run the newly downloaded executable to
verify itself, and do not treat a checksum downloaded beside it as an independent
trust anchor.

Install **gh 2.97.0** through an independently trusted package or verified upstream
distribution, following the [GitHub CLI installation instructions](https://github.com/cli/cli#installation).
Place that executable in a trusted absolute `PATH` entry. The verifier resolves
it once and checks the exact version. Version text is a compatibility check; a
hostile executable can print the same text. The verifier's isolated child
environment does not use existing gh login state or configuration.

The pinned **gh 2.97.0 / actions/attest 4.2.2** real signed fixtures still require
an external qualification record. Synthetic tests exercise orchestration and
policy rejection; they do not qualify those signatures or the pinned tools.

## Verify an official asset

Independently review the release's full, lowercase 40-character source commit.
Provide an explicit read-only GitHub token through `GH_TOKEN`, or `GITHUB_TOKEN`
when `GH_TOKEN` is absent. Prefer a token restricted to the reads needed for the
public repository; never put it in command arguments. The verifier cannot infer a
token's permissions from its text.

```bash
"$TRUSTED_TS" release verify v1.2.0 \
  --asset ts-aarch64-apple-darwin \
  --expected-source-commit 0123456789abcdef0123456789abcdef01234567
```

The commit above is fictional; replace it with your independently approved
commit. The initial cohort accepts exactly these executable names:

- `trusted-server-fastly.wasm`
- `ts-x86_64-unknown-linux-gnu`
- `ts-x86_64-apple-darwin`
- `ts-aarch64-apple-darwin`

All three arguments are validated before token lookup, configuration loading or
network activity. No provider configuration or build hooks are needed. There are
no public offline, cache, export, deploy or output-format options.

The verifier obtains producer-policy bytes from the fixed official repository
Contents API path at the independently approved commit. It checks the index's
provenance before using index fields, applies its embedded consumer policy, and
checks selected executable bytes, provenance and SBOM bundles, SPDX document,
build evidence, recipe, comparison record and JS inventory. A successful primitive
`gh attestation verify` invocation alone does not establish these document,
certificate, subject, policy and workflow-run joins.

Online publication checks additionally require the official lightweight tag to
point to the approved commit, an immutable published release, successful typed
`gh release verify` evidence, and exact asset-name, size and digest membership.
The report includes source, asset digest, assurance profile, both policy versions
and digests, build/publication outcomes and the informational rebuild comparison.
A comparison result does not grant deployment authorization.

Metadata is limited to 1 MiB, each bundle/build companion to 32 MiB, SPDX to
16 MiB, and executables to 512 MiB. Executables stream into private retained files;
they are not buffered through a JSON pipe. Each gh child has a 120-second deadline
and bounded output; the complete attempt has a 15-minute deadline. Invalid JSON,
truncation, failed children, changed bytes and timeouts fail closed with nonzero
exit status. Retained inputs are rehashed before use and discarded afterward.

Policy and publication requests use fixed official HTTPS API paths without
redirects. Asset URLs are constructed from the fixed official repository, checked
tag and canonical asset name, independently of JSON download URLs. Public asset
requests carry no token and accept at most three HTTPS redirects, only to
`release-assets.githubusercontent.com`, the host
[documented by GitHub for release downloads](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
The client disables inherited proxy settings.

## Manual offline build checks

There is no public offline command in this cohort. A private verifier path checks
build evidence using the same parsers, policy and graph joins with local bundles,
independently approved source-policy bytes, a securely transported trusted-root
snapshot and independently approved artifact/root SHA-256 pins. Its result cannot
be used as publication evidence or deployment authorization.

Prepare trust inputs through a separate trusted channel. For example, online
preparation may obtain roots with the independently trusted gh:

```bash
gh attestation trusted-root --hostname github.com > trusted_root.jsonl
```

Approve and securely transfer the exact root snapshot digest independently of the
evidence transfer. Also transfer the approved source-policy snapshot, approved
source commit, independent executable digest, index and all required companion
bytes. A digest or root snapshot supplied only by an untrusted evidence package
is not an independent approval.

Under an operating-system boundary that denies networking, pinned gh supports
local verification with `--bundle` and `--custom-trusted-root`. It does not have an
`--offline` flag. Token variables must be absent, and the denial boundary should
be proved using a successful unconfined socket control and a denied confined
socket probe. These primitive commands illustrate the certificate/source checks:

```bash
RELEASE_TAG=v1.2.0
REVIEWED_COMMIT=0123456789abcdef0123456789abcdef01234567
ASSET=trusted-server-fastly.wasm
SIGNER="https://github.com/IABTechLab/trusted-server/.github/workflows/release-build.yml@refs/tags/$RELEASE_TAG"
COMMON=(-R IABTechLab/trusted-server --hostname github.com
  --custom-trusted-root trusted_root.jsonl
  --cert-identity "$SIGNER"
  --cert-oidc-issuer https://token.actions.githubusercontent.com
  --source-ref "refs/tags/$RELEASE_TAG" --source-digest "$REVIEWED_COMMIT"
  --signer-digest "$REVIEWED_COMMIT" --deny-self-hosted-runners --format json)

gh attestation verify release-index.json "${COMMON[@]}" \
  --bundle release-index.provenance.sigstore.json \
  --predicate-type https://slsa.dev/provenance/v1

gh attestation verify "$ASSET" "${COMMON[@]}" \
  --bundle "$ASSET.provenance.sigstore.json" \
  --predicate-type https://slsa.dev/provenance/v1

gh attestation verify "$ASSET" "${COMMON[@]}" \
  --bundle "$ASSET.sbom.sigstore.json" \
  --predicate-type https://spdx.dev/Document/v2.3
```

Replace the fictional commit and first compare both independent digest pins.
These exit statuses are only primitive signature checks. Manually enforce the
same bounds and strict document parsing, exact signed subject sets, certificate
repository IDs and workflow identity, approved source-policy digest/version,
embedded cohort and capability policy, common workflow-run attempt, all index
companion sizes/digests, JS inventory/build-evidence/recipe joins, and equality of
the signed SPDX predicate to the companion document. No unsigned companion or
checksum may supply authority. The design specification in the approved source checkout (`docs/superpowers/specs/2026-10-09-1235-release-attestation-design.md`)
defines the full required joins.

Offline success establishes approved bytes and build evidence. It cannot
re-establish official immutable release membership, current release state or
fresh revocation. GitHub release verification requires online publication checks;
offline official deployment is unsupported.
