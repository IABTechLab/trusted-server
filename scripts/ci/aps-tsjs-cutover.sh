#!/usr/bin/env bash

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
evidence_root="$repository_root/target/aps-tsjs-cutover-evidence"

build_release() {
  test -n "${EXPECTED_RELEASE_ID:-}"
  mkdir -p "$evidence_root"
  npm --prefix "$repository_root/crates/trusted-server-js/lib" ci
  npm --prefix "$repository_root/crates/trusted-server-js/lib" run build
  npm --prefix "$repository_root/crates/trusted-server-js/lib" run build:prebid-external
  npm --prefix "$repository_root/crates/trusted-server-js/lib" run check:bundle \
    2>&1 | tee "$evidence_root/bundle.log"
  actual_release_id="$(npm --prefix "$repository_root/crates/trusted-server-js/lib" run --silent print:release-id)"
  test "$actual_release_id" = "$EXPECTED_RELEASE_ID"
  cp "$repository_root/crates/trusted-server-js/dist/tsjs-release-v1.json" "$evidence_root/"
  cp "$repository_root/crates/trusted-server-js/dist/tsjs-build-metrics-v1.json" "$evidence_root/"
}

run_adapters() {
  cd "$repository_root"
  cargo test \
    --manifest-path crates/trusted-server-integration-tests/Cargo.toml \
    --test parity 2>&1 | tee "$evidence_root/route-parity.log"
  cargo test \
    --manifest-path crates/trusted-server-integration-tests/Cargo.toml \
    --target x86_64-unknown-linux-gnu \
    -- --include-ignored \
    --skip test_wordpress_fastly --skip test_nextjs_fastly \
    --test-threads=1 2>&1 | tee "$evidence_root/integration.log"
}

install_browsers() {
  cd "$repository_root/crates/trusted-server-integration-tests/browser"
  npm ci
  npx playwright install --with-deps chromium firefox webkit
}

run_browser_matrix() {
  cd "$repository_root/crates/trusted-server-integration-tests/browser"
  npx playwright test \
    tests/shared/aps-renderer.spec.ts \
    tests/shared/aps-puc-lifecycle.spec.ts \
    tests/shared/tsjs-runtime.spec.ts \
    tests/shared/creative-sandbox.spec.ts \
    tests/nextjs/gpt-diagnostics.spec.ts \
    tests/nextjs/navigation.spec.ts \
    --project=chromium --project=firefox --project=webkit \
    --reporter=list \
    2>&1 | tee "$evidence_root/playwright-sanitized-report.log"
}

run_proxy_corpus() {
  cd "$repository_root"
  for runtime in axum fastly cloudflare spin; do
    ./scripts/integration-tests-aps-runner-proxy.sh --runtime "$runtime" \
      2>&1 | tee "$evidence_root/aps-proxy-$runtime.log"
  done
}

case "${1:-}" in
  build-release)
    build_release
    ;;
  run-adapters)
    run_adapters
    ;;
  install-browsers)
    install_browsers
    ;;
  run-browser)
    run_browser_matrix
    ;;
  run-proxies)
    run_proxy_corpus
    ;;
  *)
    printf 'usage: %s {build-release|run-adapters|install-browsers|run-browser|run-proxies}\n' "$0" >&2
    exit 64
    ;;
esac
