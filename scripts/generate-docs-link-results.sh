#!/usr/bin/env bash
set -euo pipefail

: "${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"

checked_at="$(date -u +'%Y-%m-%dT%H:%M:%SZ')"
printf 'checked-at=%s\n' "$checked_at" >> "$GITHUB_OUTPUT"
DOCS_PARITY_CHECKED_AT="$checked_at" cargo run \
  --manifest-path tools/docs-parity/Cargo.toml \
  -- links --external --artifact "$RUNNER_TEMP/link-results.zip"
