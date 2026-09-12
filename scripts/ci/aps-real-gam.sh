#!/usr/bin/env bash

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

validate_inputs() {
  test -n "${TS_REAL_GAM_PAGE_URL:-}"
  test -n "${TS_REAL_GAM_AUTH_HEADER:-}"
  test -n "${TS_REAL_GAM_EXPECTED_RELEASE_ID:-}"
  test -n "${DISPATCH_RELEASE_ID:-}"
  test "$DISPATCH_RELEASE_ID" = "$TS_REAL_GAM_EXPECTED_RELEASE_ID"
  test -n "${DISPATCH_EVIDENCE_ID:-}"
  test -n "${DISPATCH_PREVIOUS_ARTIFACT_ID:-}"
}

run_contract() {
  cd "$repository_root/crates/trusted-server-integration-tests/browser"
  npm exec -- playwright test \
    --config=playwright.real-gam.config.ts \
    tests/shared/aps-real-gam.spec.ts \
    --project=chromium --project=firefox --project=webkit
}

case "${1:-}" in
  validate-inputs)
    validate_inputs
    ;;
  run)
    run_contract
    ;;
  *)
    printf 'usage: %s {validate-inputs|run}\n' "$0" >&2
    exit 64
    ;;
esac
