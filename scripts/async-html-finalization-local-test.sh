#!/usr/bin/env bash
# Build a fresh test-utils artifact and exercise terminal I/O on a local runtime.
set -euo pipefail
REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
exec python3 "$REPO_ROOT/scripts/fixtures/async-html-finalization-runtime.py" "$@"
