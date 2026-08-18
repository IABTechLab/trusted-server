#!/usr/bin/env bash

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

read_tool_version() {
  awk -v tool="$1" '$1 == tool { print $2 }' "$repository_root/.tool-versions"
}

validate_inputs() {
  case "${TSJS_PERF_MODE:-}" in
    preswitch | postswitch | pull-request) ;;
    *) return 1 ;;
  esac
  [[ "${TSJS_EVIDENCE_ID:-}" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{7,127}$ ]]
}

verify_toolchains() {
  node_version="$(read_tool_version nodejs)"
  rust_version="$(read_tool_version rust)"
  test -n "$node_version"
  test -n "$rust_version"
  test "$(node --version)" = "v$node_version"
  test "$(npm --version)" = "11.6.2"
  test "$(rustc --version | awk '{ print $2 }')" = "$rust_version"
}

build_candidate() {
  cd "$repository_root/crates/trusted-server-js/lib"
  npm ci
  npm run build
  npm run check:bundle
  node "$repository_root/scripts/validate-tsjs-performance-evidence.mjs" --self-test
}

build_main() {
  test -n "${RUNNER_TEMP:-}"
  test -n "${GITHUB_ENV:-}"
  cd "$repository_root"
  git fetch origin main
  main_sha="$(git rev-parse origin/main)"
  main_root="$RUNNER_TEMP/tsjs-performance-main"
  [[ "$main_sha" =~ ^[0-9a-f]{40}$ ]]
  test "$(git cat-file -t "$main_sha")" = commit
  git worktree add --detach "$main_root" "$main_sha"
  npm --prefix "$main_root/crates/trusted-server-js/lib" ci
  npm --prefix "$main_root/crates/trusted-server-js/lib" run build
  printf 'TSJS_PERF_MAIN_SHA=%s\n' "$main_sha" >> "$GITHUB_ENV"
  printf 'TSJS_PERF_MAIN_ROOT=%s\n' "$main_root" >> "$GITHUB_ENV"
}

install_browser() {
  cd "$repository_root/crates/trusted-server-integration-tests/browser"
  npm ci
  playwright_version="$(node --print 'require("@playwright/test/package.json").version')"
  test "$playwright_version" = "1.58.2"
  npx playwright install --with-deps chromium
}

run_sample() {
  cd "$repository_root/crates/trusted-server-integration-tests/browser"
  npx playwright test \
    tests/shared/tsjs-performance.spec.ts \
    --config="$repository_root/crates/trusted-server-integration-tests/browser/playwright.performance.config.ts" \
    --project=chromium \
    --workers=1
}

validate_evidence() {
  test -n "${TSJS_PERF_OUTPUT:-}"
  test -n "${TSJS_EVIDENCE_ID:-}"
  test -n "${TSJS_PERF_HEAD_SHA:-}"
  test -n "${TSJS_PERF_MAIN_SHA:-}"
  test -n "${TSJS_PERF_MODE:-}"
  node "$repository_root/scripts/validate-tsjs-performance-evidence.mjs" \
    --file "$TSJS_PERF_OUTPUT" \
    --evidence-id "$TSJS_EVIDENCE_ID" \
    --head-sha "$TSJS_PERF_HEAD_SHA" \
    --main-sha "$TSJS_PERF_MAIN_SHA" \
    --mode "$TSJS_PERF_MODE"
}

case "${1:-}" in
  validate-inputs)
    validate_inputs
    ;;
  verify-toolchains)
    verify_toolchains
    ;;
  build-candidate)
    build_candidate
    ;;
  build-main)
    build_main
    ;;
  install-browser)
    install_browser
    ;;
  run-sample)
    run_sample
    ;;
  validate-evidence)
    validate_evidence
    ;;
  *)
    printf 'usage: %s {validate-inputs|verify-toolchains|build-candidate|build-main|install-browser|run-sample|validate-evidence}\n' "$0" >&2
    exit 64
    ;;
esac
