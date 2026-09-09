#!/usr/bin/env bash
set -euo pipefail

: "${DOCS_PARITY_CHECKED_AT:?DOCS_PARITY_CHECKED_AT is required}"
: "${EXPECTED_REPOSITORY:?EXPECTED_REPOSITORY is required}"
: "${EXPECTED_REF:?EXPECTED_REF is required}"
: "${EXPECTED_SOURCE_SHA:?EXPECTED_SOURCE_SHA is required}"
: "${EXPECTED_RUN_ID:?EXPECTED_RUN_ID is required}"
: "${EXPECTED_RUN_ATTEMPT:?EXPECTED_RUN_ATTEMPT is required}"
: "${EXPECTED_SHA256:?EXPECTED_SHA256 is required}"
: "${GH_TOKEN:?GH_TOKEN is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"

test "$GITHUB_REPOSITORY" = "$EXPECTED_REPOSITORY"
test "$GITHUB_REF" = "$EXPECTED_REF"
test "$GITHUB_SHA" = "$EXPECTED_SOURCE_SHA"
test "$GITHUB_RUN_ID" = "$EXPECTED_RUN_ID"
test "$GITHUB_RUN_ATTEMPT" = "$EXPECTED_RUN_ATTEMPT"

archive="$RUNNER_TEMP/link-results/link-results.zip"
json="$RUNNER_TEMP/link-results.json"
issues_json="$RUNNER_TEMP/external-link-issues.json"
issue_body="$RUNNER_TEMP/external-link-issue.md"
title="[docs-parity] External link findings"
owner_marker="<!-- docs-parity:external-links:v1 -->"

test "$(find "$RUNNER_TEMP/link-results" -mindepth 1 -maxdepth 1 -print | wc -l)" -eq 1
test -f "$archive"
test ! -L "$archive"
test "$(stat -c '%a' "$archive")" = 644
test "$(stat -c '%s' "$archive")" -le 2097152
test "$(sha256sum "$archive" | cut -d ' ' -f 1)" = "$EXPECTED_SHA256"

cargo run --manifest-path tools/docs-parity/Cargo.toml -- \
  links --validate-artifact "$archive" --output-json "$json"

gh api --paginate --slurp \
  "repos/$EXPECTED_REPOSITORY/issues?state=all&per_page=100" > "$issues_json"
jq -e 'type == "array" and all(.[]; type == "array" and all(.[]; type == "object" and (.number | type == "number" and floor == . and . > 0) and (.title | type == "string") and (.body == null or (.body | type == "string")) and (.state == "open" or .state == "closed")))' \
  "$issues_json" >/dev/null

owned_count="$(jq -er --arg title "$title" --arg marker "$owner_marker" '[.[][] | select((has("pull_request") | not) and .title == $title and ((.body // "") | startswith($marker)))] | length' "$issues_json")"
foreign_count="$(jq -er --arg title "$title" --arg marker "$owner_marker" '[.[][] | select((has("pull_request") | not) and .title == $title and (((.body // "") | startswith($marker)) | not))] | length' "$issues_json")"
test "$owned_count" -le 1
test "$foreign_count" -eq 0

issue_number="$(jq -er --arg title "$title" --arg marker "$owner_marker" '[.[][] | select((has("pull_request") | not) and .title == $title and ((.body // "") | startswith($marker)))] | if length == 1 then .[0].number | tostring else "" end' "$issues_json")"
issue_state="$(jq -er --arg title "$title" --arg marker "$owner_marker" '[.[][] | select((has("pull_request") | not) and .title == $title and ((.body // "") | startswith($marker)))] | if length == 1 then .[0].state else "" end' "$issues_json")"
findings="$(jq '.findings | length' "$json")"

printf '%s\n\n' "$owner_marker" > "$issue_body"
cat "$json" >> "$issue_body"

if test "$findings" -gt 0; then
  if test "$owned_count" -eq 0; then
    gh issue create --repo "$EXPECTED_REPOSITORY" --title "$title" --body-file "$issue_body"
  else
    if test "$issue_state" = closed; then
      gh issue reopen --repo "$EXPECTED_REPOSITORY" "$issue_number"
    else
      test "$issue_state" = open
    fi
    gh issue comment --repo "$EXPECTED_REPOSITORY" "$issue_number" --body-file "$issue_body"
  fi
elif test "$owned_count" -eq 1; then
  if test "$issue_state" = open; then
    gh issue close --repo "$EXPECTED_REPOSITORY" "$issue_number" \
      --comment "The latest complete scan is clean."
  else
    test "$issue_state" = closed
  fi
fi
