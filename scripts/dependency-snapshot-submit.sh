#!/usr/bin/env bash
set -euo pipefail

: "${EXPECTED_REF:?EXPECTED_REF is required}"
: "${EXPECTED_SOURCE_SHA:?EXPECTED_SOURCE_SHA is required}"
: "${EXPECTED_RUN_ID:?EXPECTED_RUN_ID is required}"
: "${EXPECTED_RUN_ATTEMPT:?EXPECTED_RUN_ATTEMPT is required}"
: "${EXPECTED_SHA256:?EXPECTED_SHA256 is required}"
: "${GH_TOKEN:?GH_TOKEN is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"

test "$GITHUB_REF" = "$EXPECTED_REF"
test "$GITHUB_SHA" = "$EXPECTED_SOURCE_SHA"
test "$GITHUB_RUN_ID" = "$EXPECTED_RUN_ID"
test "$GITHUB_RUN_ATTEMPT" = "$EXPECTED_RUN_ATTEMPT"

archive="$RUNNER_TEMP/dependency-snapshot/dependency-snapshot.zip"
json="$RUNNER_TEMP/dependency-snapshot.json"

test "$(find "$RUNNER_TEMP/dependency-snapshot" -mindepth 1 -maxdepth 1 -print | wc -l)" -eq 1
test -f "$archive"
test ! -L "$archive"
test "$(stat -c '%a' "$archive")" = 644
test "$(stat -c '%s' "$archive")" -le 4194304
test "$(sha256sum "$archive" | cut -d ' ' -f 1)" = "$EXPECTED_SHA256"

cargo run --manifest-path tools/docs-parity/Cargo.toml -- \
  dependency-snapshot validate --archive "$archive" --output-json "$json"

status="$(gh api --include --method POST \
  "repos/IABTechLab/trusted-server/dependency-graph/snapshots" \
  --input "$json" | sed -n '1s/.* \([0-9][0-9][0-9]\).*/\1/p')"
test "$status" = 201
