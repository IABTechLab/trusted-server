#!/usr/bin/env bash
# capture-fixtures.sh — Start the Next.js production server, capture HTML from each
# route, and save as test fixtures for Trusted Server integration tests.
#
# Usage:
#   cd examples/nextjs-rsc-app
#   npm ci
#   bash scripts/capture-fixtures.sh
#
# Prerequisites: Node.js 18+, npm, curl

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APP_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
FIXTURE_DIR="$(cd "$APP_DIR/../../crates/common/src/integrations/nextjs/fixtures" && pwd)"
PORT=3099
DEV_PID=""

cleanup() {
  if [ -n "$DEV_PID" ]; then
    echo "Stopping production server (PID $DEV_PID)..."
    kill "$DEV_PID" 2>/dev/null || true
    wait "$DEV_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

echo "=== Next.js Fixture Capture ==="
echo "App directory: $APP_DIR"
echo "Fixture output: $FIXTURE_DIR"
echo ""

if ! command -v curl >/dev/null 2>&1; then
  echo "ERROR: curl is required but was not found in PATH"
  exit 1
fi

mkdir -p "$FIXTURE_DIR"

echo "Installing dependencies with npm ci..."
cd "$APP_DIR"
npm ci

echo "Building Next.js app for deterministic fixture output..."
npm run build

# Start production server in background for stable HTML output.
echo "Starting Next.js production server on port $PORT..."
npm run start &
DEV_PID=$!

# Wait for server to be ready
echo "Waiting for server..."
for i in $(seq 1 30); do
  if curl -fsS "http://localhost:$PORT/" > /dev/null 2>&1; then
    echo "Server ready after ${i}s"
    break
  fi
  if [ "$i" -eq 30 ]; then
    echo "ERROR: Server did not start within 30s"
    exit 1
  fi
  sleep 1
done

echo ""

# Capture routes in a fixed order for predictable logs.
route_pairs=(
  "app-router-simple.html|/"
  "app-router-tchunk.html|/about"
  "app-router-large.html|/blog/hello-world"
)

for pair in "${route_pairs[@]}"; do
  fixture="${pair%%|*}"
  route="${pair##*|}"
  output="$FIXTURE_DIR/$fixture"
  echo "Capturing $route -> $fixture"
  curl -fsS "http://localhost:$PORT$route" > "$output"

  if ! grep -q "<html" "$output"; then
    echo "ERROR: Captured fixture $fixture does not contain <html>"
    exit 1
  fi

  if ! grep -q "__next_f" "$output"; then
    echo "ERROR: Captured fixture $fixture does not contain __next_f payloads"
    exit 1
  fi

  size=$(wc -c < "$output" | tr -d ' ')
  scripts=$(grep -c '__next_f' "$output" 2>/dev/null || echo "0")
  echo "  Size: ${size} bytes, RSC scripts: ${scripts}"
done

echo ""
echo "=== Capture complete ==="
echo "Fixtures saved to: $FIXTURE_DIR"
echo ""
echo "Files:"
ls -la "$FIXTURE_DIR"/*.html
