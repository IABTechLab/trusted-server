#!/usr/bin/env bash
# Exercise Fastly shipping S3 routes and CF/Spin native S3 transport contracts.
set -euo pipefail
REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$REPO_ROOT"
case "${1:-}" in
  fastly|cloudflare|spin) adapter=$1; shift ;;
  *) echo "Usage: $0 {fastly|cloudflare|spin} [--skip-build]" >&2; exit 2 ;;
esac
skip_build=false
if [ "${1:-}" = --skip-build ]; then skip_build=true; shift; fi
[ "$#" -eq 0 ] || { echo "Unexpected argument: $1" >&2; exit 2; }
# The runner selects each target and executes exactly the artifacts it builds.
unset CARGO_BUILD_TARGET
target_dir=${CARGO_TARGET_DIR:-$REPO_ROOT/target}
case "$target_dir" in /*) ;; *) target_dir="$REPO_ROOT/$target_dir" ;; esac
export CARGO_TARGET_DIR=$target_dir
host=$(rustc -vV | awk '/^host: / { print $2 }')
export SECRET_BINDINGS_FIXTURE_BIN=${SECRET_BINDINGS_FIXTURE_BIN:-$target_dir/$host/debug/secret-bindings-fixture}
case "$adapter" in
  fastly) export SECRET_BINDINGS_ARTIFACT=${SECRET_BINDINGS_ARTIFACT:-$target_dir/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm} ;;
  spin) export SECRET_BINDINGS_ARTIFACT=${SECRET_BINDINGS_ARTIFACT:-$target_dir/wasm32-wasip1/release/trusted_server_adapter_spin.wasm} ;;
  cloudflare) export SECRET_BINDINGS_ARTIFACT=${SECRET_BINDINGS_ARTIFACT:-$target_dir/secret-bindings-cloudflare-build/index.js} ;;
esac
if [ "$skip_build" = false ]; then
  cargo build --locked -p trusted-server-integration-tests --bin secret-bindings-fixture --target "$host"
  case "$adapter" in
    fastly) cargo build --locked -p trusted-server-adapter-fastly --release --target wasm32-wasip1 ;;
    spin) cargo build --locked -p trusted-server-adapter-spin --release --target wasm32-wasip1 --features spin,test-utils ;;
    cloudflare)
      # Use the cached/pinned tool supplied by the environment. Never install or
      # invoke the adapter build.sh, which loads operator .env files.
      (cd crates/trusted-server-adapter-cloudflare && "${WORKER_BUILD_BIN:-worker-build}" --release --out-dir "$target_dir/secret-bindings-cloudflare-build" --features cloudflare,test-utils --locked)
      ;;
  esac
fi
exec node "$REPO_ROOT/scripts/secret-bindings-runtime.mjs" "$adapter"
