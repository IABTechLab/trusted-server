#!/usr/bin/env bash
# Run the hermetic APS runner-proxy corpus through one actual adapter runtime.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

if [ "$#" -ne 2 ] || [ "$1" != "--runtime" ]; then
  echo "usage: $0 --runtime <axum|fastly|cloudflare|spin>" >&2
  exit 2
fi

RUNTIME="$2"
case "$RUNTIME" in
  axum|fastly|cloudflare|spin) ;;
  *)
    echo "unsupported APS runner-proxy runtime: $RUNTIME" >&2
    exit 2
    ;;
esac

ORIGIN_PORT="${INTEGRATION_ORIGIN_PORT:-8888}"
HOST_TARGET="$(rustc -vV | sed -n 's/^host: //p')"
if [ -z "$HOST_TARGET" ]; then
  echo "failed to detect the native Rust target" >&2
  exit 1
fi

export TRUSTED_SERVER__PUBLISHER__ORIGIN_URL="http://127.0.0.1:$ORIGIN_PORT"
export TRUSTED_SERVER__PUBLISHER__PROXY_SECRET="integration-test-proxy-secret"
export TRUSTED_SERVER__EC__PASSPHRASE="integration-test-ec-secret-padded-32"
export TRUSTED_SERVER__PROXY__CERTIFICATE_CHECK=false

ABSENCE_PATTERNS=(
  aps-runner-proxy-integration-test
  TS_APS_RUNNER_PROXY_TEST_ENDPOINT
  APS_RUNNER_PROXY_FIXTURE
  aps_runner_proxy_test_endpoint
  aps_runner_proxy_fixture
  aps-runner-proxy-fixture-bounded
  /integrations/aps/renderer/v1
  /integrations/aps/runner.js
  x-ts-aps-logical-url
)

# The fixed APS upstream URL is intentionally not an absence sentinel: the
# legacy production renderer already embeds that legitimate URL. The entries
# above are unique to the feature-only local routes and test transport.

CARGO_TEST_PID=""
CARGO_TEST_PGID=""
PROCESS_GROUP_FILE="$(mktemp -t trusted-server-aps-pgids.XXXXXX)"
SHELL_PGID="$(ps -o pgid= -p "$$" 2>/dev/null | tr -d '[:space:]' || true)"

terminate_registered_process_groups() {
  local pgid
  local actual_pgid
  while IFS= read -r pgid; do
    if [[ ! "$pgid" =~ ^[1-9][0-9]*$ ]] || [ "$pgid" = "$SHELL_PGID" ]; then
      continue
    fi
    actual_pgid="$(ps -o pgid= -p "$pgid" 2>/dev/null | tr -d '[:space:]' || true)"
    if [ "$actual_pgid" = "$pgid" ]; then
      kill -TERM -- "-$pgid" 2>/dev/null || true
    fi
  done < "$PROCESS_GROUP_FILE"
}

terminate_cargo_test() {
  if [ -z "$CARGO_TEST_PID" ]; then
    return
  fi
  if [ -n "$CARGO_TEST_PGID" ]; then
    kill -TERM -- "-$CARGO_TEST_PGID" 2>/dev/null || true
  else
    kill -TERM "$CARGO_TEST_PID" 2>/dev/null || true
  fi
  wait "$CARGO_TEST_PID" 2>/dev/null || true
}

cleanup() {
  local status="$?"
  trap - EXIT INT TERM
  terminate_cargo_test
  terminate_registered_process_groups
  if [[ "$PROCESS_GROUP_FILE" = /* ]] && [ -f "$PROCESS_GROUP_FILE" ]; then
    rm -f -- "$PROCESS_GROUP_FILE"
  fi
  exit "$status"
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

assert_release_absence() {
  local artifact="$1"
  local rg_args=()
  local pattern
  for pattern in "${ABSENCE_PATTERNS[@]}"; do
    rg_args+=(--regexp "$pattern")
  done
  if strings "$artifact" | rg --fixed-strings "${rg_args[@]}"; then
    echo "production artifact contains an APS proxy integration sentinel: $artifact" >&2
    exit 1
  fi
}

echo "==> Building and checking the production $RUNTIME artifact..."
case "$RUNTIME" in
  axum)
    cargo build --package trusted-server-adapter-axum --release
    assert_release_absence target/release/trusted-server-axum
    cargo build --package trusted-server-adapter-axum --release \
      --features aps-runner-proxy-integration-test
    export AXUM_BINARY_PATH="$REPO_ROOT/target/release/trusted-server-axum"
    ;;
  fastly)
    cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1
    assert_release_absence \
      target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm
    cargo build --package trusted-server-adapter-fastly --release --target wasm32-wasip1 \
      --features aps-runner-proxy-integration-test
    INTEGRATION_ORIGIN_PORT="$ORIGIN_PORT" \
      ./scripts/generate-integration-viceroy-configs.sh
    export WASM_BINARY_PATH="$REPO_ROOT/target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm"
    export VICEROY_CONFIG_PATH="$REPO_ROOT/target/integration-test-artifacts/configs/viceroy.toml"
    ;;
  cloudflare)
    bash crates/trusted-server-adapter-cloudflare/build.sh
    assert_release_absence crates/trusted-server-adapter-cloudflare/build/index.js
    assert_release_absence crates/trusted-server-adapter-cloudflare/build/index_bg.wasm
    TS_WORKER_BUILD_FEATURES="cloudflare,aps-runner-proxy-integration-test" \
      bash crates/trusted-server-adapter-cloudflare/build.sh
    export CLOUDFLARE_WRANGLER_DIR="$REPO_ROOT/crates/trusted-server-adapter-cloudflare"
    ;;
  spin)
    cargo build --package trusted-server-adapter-spin --release --target wasm32-wasip1 \
      --features spin
    assert_release_absence \
      target/wasm32-wasip1/release/trusted_server_adapter_spin.wasm
    cargo build --package trusted-server-adapter-spin --release --target wasm32-wasip1 \
      --features spin,aps-runner-proxy-integration-test
    export WASM_BINARY_PATH="$REPO_ROOT/target/wasm32-wasip1/release/trusted_server_adapter_spin.wasm"
    ;;
esac

echo "==> Running the APS runner-proxy corpus through $RUNTIME..."
TEST_COMMAND=(
  cargo test
  --manifest-path crates/trusted-server-integration-tests/Cargo.toml
  --features aps-runner-proxy
  --target "$HOST_TARGET"
  --test aps_runner_proxy
  actual_adapter_proxy_corpus
  -- --ignored --test-threads=1
)

if command -v setsid >/dev/null 2>&1; then
  setsid env \
    APS_RUNNER_PROXY_RUNTIME="$RUNTIME" \
    APS_RUNNER_PROXY_PROCESS_GROUP_FILE="$PROCESS_GROUP_FILE" \
    INTEGRATION_ORIGIN_PORT="$ORIGIN_PORT" \
    RUST_LOG="${RUST_LOG:-info}" \
    "${TEST_COMMAND[@]}" &
else
  # BSD/macOS does not provide `setsid`. Bash job control still launches a
  # background job in its own process group, so the cleanup trap can terminate
  # Cargo, the test binary, and every runtime it starts as one unit.
  set -m
  env \
    APS_RUNNER_PROXY_RUNTIME="$RUNTIME" \
    APS_RUNNER_PROXY_PROCESS_GROUP_FILE="$PROCESS_GROUP_FILE" \
    INTEGRATION_ORIGIN_PORT="$ORIGIN_PORT" \
    RUST_LOG="${RUST_LOG:-info}" \
    "${TEST_COMMAND[@]}" &
  set +m
fi
CARGO_TEST_PID="$!"

CHILD_PGID=""
# The background child can be observed between fork and `setsid(2)`, especially
# when `setsid` is provided by a shim on BSD/macOS. Give it a bounded moment to
# enter its dedicated process group before enforcing the cleanup invariant.
for ((attempt = 0; attempt < 50; attempt += 1)); do
  CHILD_PGID="$(ps -o pgid= -p "$CARGO_TEST_PID" 2>/dev/null | tr -d '[:space:]' || true)"
  if [[ "$CHILD_PGID" =~ ^[1-9][0-9]*$ ]] && [ "$CHILD_PGID" != "$SHELL_PGID" ]; then
    break
  fi
  sleep 0.01
done
if [[ "$CHILD_PGID" =~ ^[1-9][0-9]*$ ]] && [ "$CHILD_PGID" != "$SHELL_PGID" ]; then
  CARGO_TEST_PGID="$CHILD_PGID"
else
  echo "failed to isolate the APS corpus in a dedicated process group" >&2
  terminate_cargo_test
  CARGO_TEST_PID=""
  exit 1
fi

if wait "$CARGO_TEST_PID"; then
  TEST_STATUS=0
else
  TEST_STATUS="$?"
fi
CARGO_TEST_PID=""
CARGO_TEST_PGID=""
exit "$TEST_STATUS"
