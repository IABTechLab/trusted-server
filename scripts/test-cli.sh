#!/usr/bin/env bash
set -euo pipefail

HOST_TARGET="${1:-$(rustc -vV | awk '/host:/ { print $2 }')}"
if [ -z "$HOST_TARGET" ]; then
  echo "Failed to detect host target" >&2
  exit 1
fi

if ! command -v rustup >/dev/null 2>&1; then
  echo "rustup not found; cannot ensure host target $HOST_TARGET is installed" >&2
  echo "Run: cargo test --package trusted-server-cli --target $HOST_TARGET" >&2
  exit 1
fi

if ! rustup target list --installed | awk -v target="$HOST_TARGET" '$0 == target { found = 1 } END { exit found ? 0 : 1 }'; then
  echo "Installing Rust target: $HOST_TARGET"
  rustup target add "$HOST_TARGET"
fi

cargo test --package trusted-server-cli --target "$HOST_TARGET"
export TS_AUDIT_BROWSER_TESTS=1
AUDIT_BROWSER_TEST_FILTERS=(
  "commands::audit::browser::tests::"
  "commands::audit::generate::browser_collector::tests::progress_failure_still_finalizes_browser_session"
)
for AUDIT_BROWSER_TEST_FILTER in "${AUDIT_BROWSER_TEST_FILTERS[@]}"; do
  AUDIT_BROWSER_TEST_COUNT="$({
    cargo test --package trusted-server-cli --target "$HOST_TARGET" \
      "$AUDIT_BROWSER_TEST_FILTER" -- --ignored --list
  } | awk '/: test$/ { count += 1 } END { print count + 0 }')"
  if [ "$AUDIT_BROWSER_TEST_COUNT" -eq 0 ]; then
    echo "No ignored browser audit fixtures matched $AUDIT_BROWSER_TEST_FILTER" >&2
    exit 1
  fi
  cargo test --package trusted-server-cli --target "$HOST_TARGET" \
    "$AUDIT_BROWSER_TEST_FILTER" -- --ignored --test-threads=1
done
