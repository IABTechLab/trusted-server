#!/usr/bin/env bash
# Applies a documentation proposal and runs the docs gates against it.
#
# Usage: scripts/docs-proposal/validate.sh <merge-sha> <work-dir>
# <work-dir> holds proposal.patch from propose.sh. VitePress executes Vue in
# Markdown during the build, so run this only without write credentials;
# publish.sh never executes proposal content.
set -euo pipefail

sha="$1"
work_dir="$(cd "$2" && pwd)"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(git rev-parse --show-toplevel)"
# shellcheck source=scripts/docs-proposal/lib.sh
source "$here/lib.sh"

cd "$root"
docs_proposal_require_sha "$sha"

if [ ! -s "$work_dir/proposal.patch" ]; then
  printf 'No documentation changes proposed for %s.\n' "$sha"
  exit 0
fi

docs_proposal_apply "$sha" "$work_dir/proposal.patch"
(cd docs && npm ci && npm run lint && npm run format && npm run build)
printf 'Validated proposal for %s.\n' "$sha"
