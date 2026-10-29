# Consumer verifier: measured public-fixture qualification

Measured on 2026-10-15 on native Apple Silicon macOS. This report records
actual public upstream evidence, rather than fictional examples. No release,
workflow, repository or deployment was created. Downloaded subject archives
were never executed. The retained evidence is outside the repository at
`/private/tmp/ts-real-fixture-qualification` and is temporary local evidence;
the digests below detect changes but do not authenticate a future download.

## Trust and bootstrap

The independently installed `/opt/homebrew/bin/gh` reported version 2.87.2.
That existing verifier, rather than the downloaded executable, successfully
verified the public attestation on `cli/cli`'s
`gh_2.97.0_macOS_arm64.zip` before its executable was run. The verified
certificate bound the archive to source/signer commit
`55dbb4dc6b7edb10b48e3d7fc5bccd32318d1b55`, workflow
`.github/workflows/deployment.yml`, `refs/heads/trunk`, and run
`30597407850/attempts/1`. The archive SHA-256 was
`a58b8fd77b417a38f47a0b54d1370c59b0fcdb324ccc9ca002b0998f7c4c999e`.
The extracted executable reported exactly 2.97.0. Version output alone was not
used as authentication. This relies on the pre-existing verifier installation
as a local trust prerequisite; this work did not independently rebuild or
qualify that installation's own historical bootstrap.

Primary inputs:

- [Pinned verifier release](https://github.com/cli/cli/releases/tag/v2.97.0).
- [Bootstrap signing workflow at its verified commit](https://github.com/cli/cli/blob/55dbb4dc6b7edb10b48e3d7fc5bccd32318d1b55/.github/workflows/deployment.yml).
- [Measured fixture release](https://github.com/d2lang/d2/releases/tag/v0.9.0).
- [Fixture signing workflow at its verified commit](https://github.com/d2lang/d2/blob/d5a51743b6d5c00f1ec6f8340003f5d5a6ba4eda/.github/workflows/release-archives.yml).
- [Exact attest 4.2.2 action](https://github.com/actions/attest/tree/1e69f48acb82d1966a394da916b4c1698aa569d6).

The bootstrap release used attest 4.2.0. It was used to authenticate the
verifier, not to stand in for the required 4.2.2 signing fixture. The D2
workflow at the verified commit explicitly pins attest 4.2.2 for both
provenance and SPDX signing.

## Successful measurements

The authenticated gh 2.97.0 verified both provenance and SPDX signatures for
`d2-v0.9.0-linux-arm64.tar.gz`. Its real certificates reported:

| Field                    | Observed value                                                                         |
| ------------------------ | -------------------------------------------------------------------------------------- |
| Issuer                   | `https://token.actions.githubusercontent.com`                                          |
| SAN and build signer     | `https://github.com/d2lang/d2/.github/workflows/release-archives.yml@refs/tags/v0.9.0` |
| Source and signer digest | `d5a51743b6d5c00f1ec6f8340003f5d5a6ba4eda`                                             |
| Source repository ID     | `533087958`                                                                            |
| Source owner ID          | `242104691`                                                                            |
| Trigger / runner         | `push` / `github-hosted`                                                               |
| Invocation               | `https://github.com/d2lang/d2/actions/runs/34165396038/attempts/1`                     |

The actual output was an array of one processing result for each
`attestation verify` invocation. The real verification media type,
flattened certificate field names, SLSA v1 provenance fields, and SPDX
`https://spdx.dev/Document/v2.3` predicate matched the implemented consumer
contract. The separately downloaded `d2.spdx.json` compared equal as parsed
JSON to the signed SPDX predicate.

Both bundles were replayed with the production-equivalent explicit issuer,
SAN, source ref, source digest, signer digest and hosted-runner restriction.
The process environment was cleared, with only private HOME/config and
noninteractive settings supplied; no token was present. SPDX verification
also succeeded under a macOS `sandbox-exec` profile denying `network*`,
using a local bundle and independently transferred trusted roots. The same
profile caused a separate curl DNS/network attempt to fail. This is an
actual consumer offline primitive check, not qualification of producer
process cleanup or the broader macOS build boundary.

Pinned `gh release verify --format json` also successfully verified the D2
and CLI public release manifests. Actual output was one processing-result
object rather than the attestation command's array; the release predicate
was `https://in-toto.io/attestation/release/v0.2`.

## Retained evidence digests

All digests are SHA-256 of exact retained bytes.

| File                                | Digest                                                             |
| ----------------------------------- | ------------------------------------------------------------------ |
| `bootstrap-proof.json`              | `b53f05f983872a6f5f2d9e95d54bf371f86b012a36fc324b80be5eebbf57fc66` |
| `d2-v0.9.0-linux-arm64.tar.gz`      | `ac2c028697199479acb321db1e3d68caee9f2ba492ed73caa3cd13f3829bf913` |
| `d2.spdx.json`                      | `efaf407d3142ee52de4f3e90864f3023110dd9f430f8220143d6f3d9de86e689` |
| `d2.provenance.sigstore.json`       | `d42456c264743abc09194a548e18019e4073eef4040abd182fd1c974616a9259` |
| `d2.spdx.sigstore.json`             | `26cab9234beaa45c49510924ae7c8aa4cb058a3419aff673eb6c51c33bce3ad3` |
| `d2-isolated-provenance-proof.json` | `59045bc27b9d7bbf1f6747ca7939aa8ef2c77330e86d2229d7cee9daf3c7e2ca` |
| `d2-isolated-spdx-proof.json`       | `bf43a603bb1a4f83cdcfeee68ae0b69b3d23e8fb1f46a20d083dafafc4220c8a` |
| `d2-offline-spdx-proof.json`        | `bf43a603bb1a4f83cdcfeee68ae0b69b3d23e8fb1f46a20d083dafafc4220c8a` |
| `d2-release-proof.json`             | `a1269c4c7da96bd0e5129b1d95030cf084d91fed2b8a085ad8fdcb633b979deb` |
| `gh-release-proof.json`             | `dd934980d284943275aa2d9c574fdbcc5e3713e38a616ce49ca0ac682040f07c` |
| `trusted-root.jsonl`                | `65ca537f6ed8a47fd0e560c421baa1f6c1efb8b25fc200d8c5c02c0e92eb2b9c` |

## Replay commands

First authenticate the pinned archive with the independently installed
verifier, then extract it. Fetch the D2 archive and standalone SPDX document
from the release linked above. Use the retained signed bundles, or obtain
and verify fresh ones from the fixed public repository; a hash here cannot
replace cryptographic verification.

```sh
set -eu
EVIDENCE=/private/tmp/ts-real-fixture-qualification
PINNED_GH="$EVIDENCE/extracted/gh_2.97.0_macOS_arm64/bin/gh"
/opt/homebrew/bin/gh attestation verify \
  "$EVIDENCE/gh_2.97.0_macOS_arm64.zip" -R cli/cli --format json
/opt/homebrew/bin/gh attestation trusted-root --hostname github.com \
  > "$EVIDENCE/trusted-root.jsonl"

# Run once per predicate, choosing the corresponding retained bundle.
PREDICATE=https://spdx.dev/Document/v2.3
BUNDLE="$EVIDENCE/d2.spdx.sigstore.json"
env -i HOME="$EVIDENCE/isolated-home" \
  GH_CONFIG_DIR="$EVIDENCE/isolated-home" GH_PROMPT_DISABLED=1 \
  /usr/bin/sandbox-exec -p '(version 1)(allow default)(deny network*)' \
  "$PINNED_GH" attestation verify \
  "$EVIDENCE/d2-v0.9.0-linux-arm64.tar.gz" --bundle "$BUNDLE" \
  --custom-trusted-root "$EVIDENCE/trusted-root.jsonl" \
  -R d2lang/d2 --hostname github.com \
  --cert-identity https://github.com/d2lang/d2/.github/workflows/release-archives.yml@refs/tags/v0.9.0 \
  --cert-oidc-issuer https://token.actions.githubusercontent.com \
  --source-ref refs/tags/v0.9.0 \
  --source-digest d5a51743b6d5c00f1ec6f8340003f5d5a6ba4eda \
  --signer-digest d5a51743b6d5c00f1ec6f8340003f5d5a6ba4eda \
  --deny-self-hosted-runners --predicate-type "$PREDICATE" --format json
```

For provenance, use `https://slsa.dev/provenance/v1` and
`d2.provenance.sigstore.json`. Preserve the exact expected source identities;
never derive acceptance policy from an unverified proof's fields. Runtime
proofs and bundles remain outside the repository; checked-in compatibility
fixtures use fictional identities and contain no signatures or trusted proof
constructors.

## Scope and remaining first-use work

Task 18's real pinned-verifier/signed-public-fixture primitive requirement is
now measured. Its adversarial bounded-process tests remain the separate
source-built CLI verification evidence.

Task 19's upstream format uncertainty is resolved for both predicate kinds
and release-manifest output. The public fixture has a different repository,
workflow and product graph and cannot satisfy Trusted Server policy. No
third-party result constructs official project authorization. Full official
first-use verification remains pending until the producer emits the canonical
signed index and all required companion assets, from independently approved
source, and an independently trusted/source-built verifier checks that actual
release. This report does not enable releases or qualify authenticated
producer bootstrap, hosted build recipes, or signing workflow execution.
