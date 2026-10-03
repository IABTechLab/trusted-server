#!/usr/bin/env bash
# Tests the documentation proposal helpers, validate flow, and publish flow
# with stubbed gh and npm against a throwaway git repository.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/docs-proposal/lib.sh
source "$here/lib.sh"

failures=0

assert_eq() {
  if [ "$1" != "$2" ]; then
    printf 'FAIL: %s\n  expected: %s\n  actual:   %s\n' "$3" "$2" "$1"
    failures=$((failures + 1))
  fi
}

assert_contains() {
  if [[ "$1" != *"$2"* ]]; then
    printf 'FAIL: %s\n  missing: %s\n' "$3" "$2"
    failures=$((failures + 1))
  fi
}

sha=0123456789abcdef0123456789abcdef01234567

assert_eq "$(docs_proposal_branch "$sha")" "docs/auto/0123456789ab" "should derive the branch from the SHA"

bad="$(printf 'docs/guide/a.md\ndocs/index.md\ndocs/superpowers/x.md\nREADME.md\n' | docs_proposal_disallowed_paths || true)"
assert_eq "$bad" $'docs/superpowers/x.md\nREADME.md' "should print only paths outside the allowlist"

if printf 'docs/guide/a.md\ndocs/guide/integrations/b.md\n' | docs_proposal_disallowed_paths >/dev/null; then
  status=0
else
  status=1
fi
assert_eq "$status" 0 "should accept guide and nested guide paths"

rationale="$(mktemp)"
printf -- '- docs/guide/cli.md: documents the new flag.\n' > "$rationale"
body="$(docs_proposal_pr_body "$sha" "Add example flag" 42 "$rationale")"
assert_contains "$body" "<!-- docs-proposal: $sha -->" "should embed the SHA marker"
assert_contains "$body" "merged change $sha (#42): Add example flag" "should link the merge and its PR"
assert_contains "$body" "documents the new flag" "should include the rationale"
: > "$rationale"
body="$(docs_proposal_pr_body "$sha" "Add example flag" "" "$rationale")"
assert_contains "$body" "merged change $sha: Add example flag" "should omit a missing origin PR"
assert_contains "$body" "did not provide a rationale" "should note an empty rationale"
rm -f "$rationale"

if (docs_proposal_require_sha "not-a-sha") 2>/dev/null; then
  status=0
else
  status=1
fi
assert_eq "$status" 1 "should reject a malformed SHA"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=Example GIT_AUTHOR_EMAIL=dev@example.com
export GIT_COMMITTER_NAME=Example GIT_COMMITTER_EMAIL=dev@example.com

mkdir -p "$tmp/bin"
cat > "$tmp/bin/gh" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$STUB_LOG"
case "$1 $2" in
  "pr list") printf '%s' "${STUB_PR:-}" ;;
  "pr create") printf 'https://github.com/example/repo/pull/7\n' ;;
  "api repos/{owner}/{repo}/commits/"*) printf '%s' "${STUB_ORIGIN_PR:-}" ;;
  "api --paginate") printf '%s' "${STUB_REVIEWS:-}" ;;
  "api --method")
    if [ -n "${STUB_FAIL_REVIEW:-}" ]; then
      exit 1
    fi
    for arg in "$@"; do last="$arg"; done
    cp "$last" "$STUB_REVIEW"
    ;;
esac
STUB
cat > "$tmp/bin/npm" <<'STUB'
#!/usr/bin/env bash
printf 'npm %s\n' "$*" >> "$STUB_LOG"
STUB
chmod +x "$tmp/bin/gh" "$tmp/bin/npm"
export PATH="$tmp/bin:$PATH" STUB_LOG="$tmp/gh.log" STUB_REVIEW="$tmp/review.json"

git init -q --bare "$tmp/origin.git"
git clone -q "$tmp/origin.git" "$tmp/work" 2>/dev/null
cd "$tmp/work"
mkdir -p docs/guide
printf 'line one\nline two\nline three\n' > docs/guide/cli.md
git add docs && git commit -q -m "Seed docs" && git push -q origin HEAD:main
merge_sha="$(git rev-parse HEAD)"
branch="$(docs_proposal_branch "$merge_sha")"

# Each publish run leaves HEAD on its proposal commit, so every patch starts
# from a clean checkout of the merge commit.
make_patch() {
  git reset -q --hard "$merge_sha" && git clean -qfd
  rm -rf "$tmp/proposal" && mkdir -p "$tmp/proposal"
  if [ -n "$1" ]; then
    mkdir -p "$(dirname "$1")" && printf 'proposed\n' >> "$1"
    git add -A && git diff --cached --binary "$merge_sha" > "$tmp/proposal/proposal.patch"
    git reset -q --hard "$merge_sha" && git clean -qfd
  else
    : > "$tmp/proposal/proposal.patch"
  fi
  printf -- '- docs/guide/cli.md: example rationale.\n' > "$tmp/proposal/rationale.md"
  printf '{}\n' > "$tmp/proposal/evidence.json"
}

run_publish() {
  : > "$STUB_LOG"
  rm -f "$STUB_REVIEW"
  if "$here/publish.sh" "$merge_sha" "$tmp/proposal" > "$tmp/out.log" 2>&1; then
    printf '0'
  else
    printf '1'
  fi
}

run_validate() {
  : > "$STUB_LOG"
  if "$here/validate.sh" "$merge_sha" "$tmp/proposal" > "$tmp/out.log" 2>&1; then
    printf '0'
  else
    printf '1'
  fi
}

remote_head() {
  git ls-remote origin "refs/heads/$branch" | cut -f1
}

remote_has_branch() {
  if git ls-remote --exit-code -q origin "refs/heads/$branch" >/dev/null; then
    printf 'yes'
  else
    printf 'no'
  fi
}

make_patch ""
assert_eq "$(run_validate)" 0 "should validate an empty proposal"
assert_eq "$(grep -c 'npm' "$STUB_LOG" || true)" 0 "should not run the docs gates for an empty proposal"

make_patch docs/superpowers/notes.md
assert_eq "$(run_validate)" 1 "should reject a disallowed path before the docs gates"
assert_eq "$(grep -c 'npm' "$STUB_LOG" || true)" 0 "should not build a disallowed proposal"

make_patch docs/guide/cli.md
assert_eq "$(run_validate)" 0 "should validate an allowed proposal"
assert_contains "$(cat "$STUB_LOG")" "npm run build" "should run the docs build"

make_patch ""
assert_eq "$(STUB_PR="" run_publish)" 0 "should succeed on an empty proposal"
assert_eq "$(grep -c 'pr create\|pr close' "$STUB_LOG" || true)" 0 "should not open or close a PR for an empty diff"

make_patch ""
assert_eq "$(STUB_PR="7 OPEN" run_publish)" 0 "should succeed when closing a stale proposal"
assert_contains "$(cat "$STUB_LOG")" "pr close 7" "should close the open proposal for an empty diff"

make_patch docs/superpowers/notes.md
assert_eq "$(STUB_PR="" run_publish)" 1 "should fail for a disallowed path"
assert_contains "$(cat "$tmp/out.log")" "docs/superpowers/notes.md" "should name the disallowed path"
assert_eq "$(remote_has_branch)" no "should not push a disallowed proposal"

make_patch docs/guide/cli.md
assert_eq "$(STUB_PR="" STUB_ORIGIN_PR=42 run_publish)" 0 "should publish a valid proposal"
assert_eq "$(remote_has_branch)" yes "should push the proposal branch"
assert_contains "$(cat "$STUB_LOG")" "pr create --base main --head $branch" "should open a PR from the proposal branch"
assert_eq "$(grep -c '^npm' "$STUB_LOG" || true)" 0 "should never build proposal content while publishing"
assert_contains "$(cat "$STUB_REVIEW" 2>/dev/null)" '"path": "docs/guide/cli.md"' "should post a review for the hunk"

published_head="$(remote_head)"
assert_eq "$(STUB_PR="7 OPEN" STUB_REVIEWS=101 run_publish)" 0 "should succeed on an unchanged retry"
assert_eq "$(remote_head)" "$published_head" "should not push an unchanged proposal"
assert_eq "$(grep -c 'pr create\|--method POST' "$STUB_LOG" || true)" 0 "should not re-post an unchanged proposal"

make_patch docs/index.md
assert_eq "$(STUB_PR="7 OPEN" STUB_FAIL_REVIEW=1 run_publish)" 1 "should fail when the review cannot be posted"
pushed_head="$(remote_head)"
assert_contains "$(cat "$STUB_LOG")" "pr edit 7" "should edit the existing PR"

assert_eq "$(STUB_PR="7 OPEN" run_publish)" 0 "should resume a proposal whose review failed"
assert_eq "$(remote_head)" "$pushed_head" "should not push again when resuming"
assert_contains "$(cat "$STUB_REVIEW" 2>/dev/null)" '"path": "docs/index.md"' "should review the new hunk"
assert_contains "$(cat "$STUB_REVIEW" 2>/dev/null)" "\"commit_id\": \"$pushed_head\"" "should review the pushed commit"

assert_eq "$(STUB_PR="7 CLOSED" run_publish)" 0 "should succeed for a closed proposal"
assert_eq "$(grep -c 'pr edit\|pr create' "$STUB_LOG" || true)" 0 "should never reopen or recreate a closed proposal"

cd "$here"

if [ "$failures" -gt 0 ]; then
  printf '%d failure(s)\n' "$failures"
  exit 1
fi
printf 'All documentation proposal script tests passed.\n'
