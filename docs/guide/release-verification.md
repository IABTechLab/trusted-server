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

## Manual Fastly production consumption

This manual route transports inspected, verified wasm bytes while integrated
attested deployment is pending. It activates production and does not provide the
integrated preactivation binding check, staging isolation or durable structured
recovery contract. Existing EdgeZero application-release deployment is a separate
path; `ts deploy --release` is not an implemented command here.

### Retain trusted inputs and bootstrap the inspector

Complete online release verification for `trusted-server-fastly.wasm` against the
independently approved source commit. `ts release verify` reports verification and
discards its retained files; it has no public export option. Separately retain the
exact wasm bytes using a trusted acquisition procedure, and compare their SHA-256
with the successful verification context. Stop if trusted retention of matching
bytes is unavailable. A neighboring checksum or an archive's own digest does not
establish this trust.

Build the package inspector from the same independently approved repository
commit, with its reviewed lockfile and toolchain. Do not execute a newly downloaded
helper to establish its own trust. For example, in that approved checkout on
Apple Silicon macOS:

```bash
cargo build --locked --package trusted-server-release --target aarch64-apple-darwin
TRUSTED_RELEASE="$PWD/target/aarch64-apple-darwin/debug/trusted-server-release"
```

Use the corresponding native host target on Linux. The source-built inspector is
not a separately authenticated release asset and does not verify acquisition,
attestations, provider bindings or live execution.

### Prepare a private deployment directory

Use a fresh private directory for this operation. Copy the retained verified wasm
there as `trusted-server-fastly.wasm`. Derive a new deployment-only `fastly.toml`
from the reviewed publisher-owned manifest; leave the original operator manifest
unchanged. The inspector accepts this deliberately narrow two-line profile:

```toml
manifest_version = 3
name = "example-app"
```

Use exactly these two root keys in this order, this spacing, and a final newline.
The name must contain 1–64 lowercase ASCII letters/digits, optionally separated
by single hyphens, and must not be `package`. No comments, descriptions, language,
service IDs, tables, `[local_server]`, `[setup]`, scripts, build hooks, inline
fixtures, active app-config payloads or secrets are accepted. This profile does
not provision resources; select the existing service explicitly at submission.
Independently approve the exact deployment manifest bytes and their SHA-256 in
the deployment context before packing. Keep that approved digest and the verified
wasm digest outside the archive as trusted inputs.

Review the intended existing service and integer base version. Independently
inspect that exact version's observed Config, KV and Secret Store links, logical
IDs, selectors and required secret references/presence. Declarations in an
operator manifest's `[setup]` do not prove or update existing-service bindings;
this procedure does not push configuration or secrets. Record shared resources
and serialize changes to the service and those resources across environments.

### Pack, inspect and submit

Use the pinned Fastly CLI **15.1.0** from an independently trusted installation.
From the private directory containing only the approved deployment inputs, run:

```bash
fastly compute pack --wasm-binary ./trusted-server-fastly.wasm
```

At this pin, packing creates both `pkg/<name>.tar.gz` and an unrelated
`pkg/package.tar.gz`, and can sweep in pre-existing package contents. Fresh
private directories are required. The selected named archive must contain exactly
`<name>/`, `<name>/bin/`, `<name>/bin/main.wasm` and `<name>/fastly.toml`.
Never submit the generic `package.tar.gz` by guessing its name.

Inspect the output directory using the independently approved expected manifest
and wasm digests. Replace the placeholders with those trusted SHA-256 values:

```bash
"$TRUSTED_RELEASE" inspect-fastly-package \
  --package-dir "$PWD/pkg" \
  --manifest "$PWD/fastly.toml" \
  --expected-manifest-sha256 '<approved-manifest-sha256>' \
  --expected-wasm-sha256 '<verified-wasm-sha256>'
```

The inspector bounds archive processing and rejects traversal, links, duplicate
or extra entries, mismatched manifest/wasm bytes and malformed or non-core wasm.
Its JSON report identifies the selected absolute package path and the archive,
module and manifest hashes, together with its limits. Record those observations.
The archive SHA-256 identifies this operation's envelope; nondeterministic tar
metadata means it is not the canonical release identity.

Packing and `compute validate` can accept non-wasm bytes and do not remove private
fixtures. Neither command proves valid wasm, usable application configuration or
application execution. Inspector success also does not replace a positive
publisher-request runtime check; `/health` alone can bypass application settings.

Immediately before submission, recheck the selected base version's bindings and
selectors and rerun the inspector against the retained package and trusted
digests. Keep the reviewed manifest, wasm and package unchanged. Use the exact
package path reported by the inspector and the inspected service/base version:

```bash
fastly compute deploy --package './pkg/example-app.tar.gz' \
  --service-id '99999' --version '7' --no-default-domain
```

The service ID and version above are fictional; replace them with the inspected
existing service and integer base version. Replace the example package name
with the inspector-selected name. The working
directory's manifest takes precedence over the packaged manifest, so retain only
the inspected deployment-only manifest there. Explicit `--service-id`, integer
`--version` and `--no-default-domain` are required: omitted version selection can
resolve active, or latest when no active version exists, and omitted domain
suppression can create a random default domain. Do not use `compute publish` or
build from source for this verified-byte route.

At this pin, `compute deploy` clones an active/locked version and activates
production internally; it has no staging mode. It can also return success while
skipping deployment when the selected version already has the same package hash.
Exit zero therefore does not prove upload, allocation of a new version or
activation. Record the version actually reported and independently inspect its
bindings, package observations and a non-health publisher response. Preserve
uncertain outcomes as uncertain; do not infer ownership from the currently active
or highest version, blindly retry, or guess a rollback target.

This procedure cannot interpose a binding check between upload and production
activation. It supplies no staging isolation or durable structured recovery.
These integrated guarantees remain separate work. No live disposable-service
submission or AC-9 qualification is established by these instructions or by
provider-free inspector tests; that proof requires a separately authorized
production submission with submitted-member and returned-version observations,
including skipped or uncertain outcomes.
