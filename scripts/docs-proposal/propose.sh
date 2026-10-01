#!/usr/bin/env bash
# Asks Copilot CLI to propose documentation edits for one merged commit.
#
# Usage: scripts/docs-proposal/propose.sh <merge-sha> <work-dir>
# Writes proposal.patch (empty when nothing changes), rationale.md, and
# evidence.json to <work-dir>, which must be inside the repository so Copilot
# can read the prompts and write its outputs there.
set -euo pipefail

sha="$1"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(git rev-parse --show-toplevel)"
spec="$root/docs/superpowers/specs/2026-08-19-documentation-refresh-design.md"
# shellcheck source=scripts/docs-proposal/lib.sh
source "$here/lib.sh"

cd "$root"
docs_proposal_require_sha "$sha"
mkdir -p "$2"
work_dir="$(cd "$2" && pwd)"
relative_dir="${work_dir#"$root"/}"

# Reads are always permitted; writes are allowed so Copilot can edit pages,
# and publish.sh enforces which paths may actually change.
run_copilot() {
  copilot -p "Read $relative_dir/$1 and follow its instructions exactly." \
    --no-ask-user \
    --allow-tool=write \
    --allow-tool='shell(git show:*)' \
    --allow-tool='shell(git diff:*)' \
    --allow-tool='shell(git log:*)' \
    --allow-tool='shell(git grep:*)' \
    --deny-tool='shell(git push)' \
    --deny-tool='shell(git commit)'
}

{
  cat "$here/prompt.md"
  printf '\n## Merged change\n\n- Commit: %s\n- Subject: %s\n\n' "$sha" "$(git log -1 --format=%s "$sha")"
  printf '## Sources of truth\n\n'
  awk '/^## Sources of truth/ { found = 1; next } /^## / { found = 0 } found' "$spec"
} > "$work_dir/prompt.md"
run_copilot prompt.md

# Normalize Copilot's edits before hunks are identified so their ids stay
# valid after the publish job's format check.
(cd docs && npm ci && npm run format:write)

git add -A -- docs
git diff --cached --no-renames --binary "$sha" -- docs > "$work_dir/proposal.patch"
touch "$work_dir/rationale.md"
if [ ! -s "$work_dir/proposal.patch" ]; then
  printf '{}\n' > "$work_dir/evidence.json"
  printf 'No documentation changes proposed for %s.\n' "$sha"
  exit 0
fi

{
  cat "$here/evidence-prompt.md"
  printf '\n## Merged change\n\n- Commit: %s\n\n## Hunks\n\n```json\n' "$sha"
  git diff --cached --no-renames "$sha" -- docs | node "$here/hunks.mjs" list
  printf '```\n'
} > "$work_dir/evidence-prompt.md"
run_copilot evidence-prompt.md
if [ ! -f "$work_dir/evidence.json" ]; then
  printf '{}\n' > "$work_dir/evidence.json"
fi
printf 'Proposed documentation changes for %s.\n' "$sha"
