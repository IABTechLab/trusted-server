#!/usr/bin/env bash

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
evidence_root="$repository_root/target/aps-tsjs-quality-evidence"

install_dependencies() {
  npm --prefix "$repository_root/crates/trusted-server-js/lib" ci
  npm --prefix "$repository_root/docs" ci
}

run_quality_gates() {
  test -n "${EXPECTED_RELEASE_ID:-}"
  mkdir -p "$evidence_root"

  {
    cd "$repository_root"
    cargo fmt --all -- --check
    npm --prefix crates/trusted-server-js/lib run format
    npm --prefix docs run format
    cargo clippy-fastly
    cargo clippy-axum
    cargo clippy-cloudflare
    cargo clippy-cloudflare-wasm
    cargo clippy-spin-native
    cargo clippy-spin-wasm
    cargo clippy --manifest-path crates/trusted-server-integration-tests/Cargo.toml --all-targets -- -D warnings
    npm --prefix crates/trusted-server-js/lib run build
    npm --prefix crates/trusted-server-js/lib run build:prebid-external
    npm --prefix crates/trusted-server-js/lib run check:aps-contract
    npm --prefix crates/trusted-server-js/lib run check:hard-cutover-absence
    npm --prefix crates/trusted-server-js/lib run check:bundle
    npm --prefix crates/trusted-server-js/lib run check:concept-audit
    npm --prefix docs run lint
    npm --prefix docs run build
    actual_release_id="$(npm --prefix crates/trusted-server-js/lib run --silent print:release-id)"
    test "$actual_release_id" = "$EXPECTED_RELEASE_ID"
  } 2>&1 | tee "$evidence_root/quality.log"

  cp "$repository_root/crates/trusted-server-js/dist/tsjs-release-v1.json" "$evidence_root/"
  cp "$repository_root/crates/trusted-server-js/dist/tsjs-build-metrics-v1.json" "$evidence_root/"
}

case "${1:-}" in
  install)
    install_dependencies
    ;;
  run)
    run_quality_gates
    ;;
  *)
    printf 'usage: %s {install|run}\n' "$0" >&2
    exit 64
    ;;
esac
