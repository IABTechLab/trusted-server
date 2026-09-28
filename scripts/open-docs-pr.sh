#!/usr/bin/env bash
#
# Open a draft documentation pull request for code merged to main.
#
# The PR starts from the merged commit with one empty commit, so there is a
# branch to push documentation updates to. Its body links the source PR and
# lists the files the merge changed.
#
# Usage: scripts/open-docs-pr.sh <BEFORE_SHA> <AFTER_SHA>
#
# Environment:
#   GH_TOKEN           token allowed to push branches and open pull requests
#   GITHUB_REPOSITORY  owner/name of the repository
#   DRY_RUN=1          print the branch, title, and body instead of pushing
#
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 <BEFORE_SHA> <AFTER_SHA>" >&2
  exit 2
fi

BEFORE_SHA="$1"
AFTER_SHA="$2"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY must be set}"
MAX_LISTED_FILES=100

SHORT_SHA="$(git rev-parse --short=7 "$AFTER_SHA")"
BRANCH="docs/$SHORT_SHA"
SUBJECT="$(git log -1 --format=%s "$AFTER_SHA")"

# A push that creates the branch reports an all-zero BEFORE_SHA; fall back to
# the merged commit's own changes.
if git cat-file -e "${BEFORE_SHA}^{commit}" 2>/dev/null; then
  CHANGED_FILES="$(git diff --name-only "$BEFORE_SHA" "$AFTER_SHA")"
else
  CHANGED_FILES="$(git diff-tree --no-commit-id --name-only -r "$AFTER_SHA")"
fi
CHANGED_COUNT="$(printf '%s\n' "$CHANGED_FILES" | grep -c . || true)"

SOURCE_PR="$(gh api "repos/$GITHUB_REPOSITORY/commits/$AFTER_SHA/pulls" --jq '.[0].number // empty' 2>/dev/null || true)"
if [ -n "$SOURCE_PR" ]; then
  SOURCE="#$SOURCE_PR ($SHORT_SHA)"
else
  SOURCE="$SHORT_SHA"
fi

TITLE="Update documentation for $SUBJECT"
BODY="$(
  echo "Code merged to \`main\` in $SOURCE. Push documentation updates for that change to this branch, or close this PR if none are needed."
  echo
  echo "Changed files ($CHANGED_COUNT):"
  echo
  # The backticks are literal Markdown, not command substitution.
  # shellcheck disable=SC2016
  printf '%s\n' "$CHANGED_FILES" | head -n "$MAX_LISTED_FILES" | sed 's/.*/- `&`/'
  if [ "$CHANGED_COUNT" -gt "$MAX_LISTED_FILES" ]; then
    echo "- … and $((CHANGED_COUNT - MAX_LISTED_FILES)) more"
  fi
)"

if [ "${DRY_RUN:-0}" = "1" ]; then
  printf 'branch: %s\ntitle: %s\n\n%s\n' "$BRANCH" "$TITLE" "$BODY"
  exit 0
fi

git switch --create "$BRANCH" "$AFTER_SHA"
git -c user.name="github-actions[bot]" \
  -c user.email="41898282+github-actions[bot]@users.noreply.github.com" \
  commit --allow-empty --message "$TITLE"
git push origin "$BRANCH"
gh pr create --draft --base main --head "$BRANCH" --title "$TITLE" --body "$BODY"
