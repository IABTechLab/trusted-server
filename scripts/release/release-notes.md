[Verification guide](https://github.com/{{REPOSITORY}}/blob/{{TAG}}/docs/guide/release-verification.md)

Release notes stay editable after publication. Set `REPO`, `TAG` and `COMMIT`
below from your independent source review, never from these notes, then run the
block in a new private directory using Bash.

```bash
set -euo pipefail
# Take REPO, TAG and COMMIT from your independent source review.
REPO=
TAG=
COMMIT=
[[ -n "$REPO" && -n "$TAG" && "$COMMIT" =~ ^[0-9a-f]{40}$ ]] || exit 1
ASSETS=({{ASSETS}})
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
