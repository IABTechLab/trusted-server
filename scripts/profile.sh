#!/usr/bin/env bash
#
# WASM Guest Profiler for Trusted Server
#
# Captures function-level flame graphs using Fastly's Wasmtime guest profiler.
# Samples the WASM call stack every 50us and writes a Firefox Profiler-compatible
# JSON file after the server stops.
#
# Prerequisites:
#   - Fastly CLI installed: https://developer.fastly.com/learning/tools/cli
#   - Rust wasm32-wasip1 target: rustup target add wasm32-wasip1
#
# Usage:
#   ./scripts/profile.sh                           # Profile GET / (publisher proxy)
#   ./scripts/profile.sh --endpoint /auction \
#       --method POST --body '{"adUnits":[]}'      # Profile specific endpoint
#   ./scripts/profile.sh --requests 50             # More samples for stable flame graph
#   ./scripts/profile.sh --no-build                # Skip rebuild, use existing binary
#   ./scripts/profile.sh --open                    # Auto-open Firefox Profiler (macOS)
#
# Output:
#   Profile saved to benchmark-results/profiles/<timestamp>.json
#   View: drag file onto https://profiler.firefox.com/
#

set -euo pipefail

# --- Configuration ---
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PROFILE_DIR="$PROJECT_ROOT/benchmark-results/profiles"
BASE_URL="http://127.0.0.1:7676"
SERVER_PID=""

# Defaults
ENDPOINT="/"
METHOD="GET"
REQUESTS=20
BODY=""
SKIP_BUILD=false
AUTO_OPEN=false

# Colors (disabled if not a terminal)
if [ -t 1 ]; then
    BOLD='\033[1m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    RED='\033[0;31m'
    CYAN='\033[0;36m'
    RESET='\033[0m'
else
    BOLD='' GREEN='' YELLOW='' RED='' CYAN='' RESET=''
fi

# --- Helpers ---

log_header() {
    echo ""
    echo -e "${BOLD}${CYAN}=== $1 ===${RESET}"
    echo ""
}

log_info() {
    echo -e "${GREEN}[INFO]${RESET} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${RESET} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${RESET} $1"
}

stop_server() {
    # Kill the fastly CLI process if we have its PID
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
    # Also kill any Viceroy process still on port 7676
    # (fastly CLI spawns Viceroy as a child; killing the CLI doesn't always propagate)
    local port_pids
    port_pids=$(lsof -ti :7676 2>/dev/null | while read pid; do
        # Only kill Viceroy processes, not unrelated listeners (e.g. Chrome)
        if ps -p "$pid" -o command= 2>/dev/null | grep -q viceroy; then
            echo "$pid"
        fi
    done)
    if [ -n "$port_pids" ]; then
        echo "$port_pids" | xargs kill 2>/dev/null || true
        sleep 1
    fi
}

cleanup() {
    stop_server
}

trap cleanup EXIT

usage() {
    echo "Usage: $0 [OPTIONS]"
    echo ""
    echo "Options:"
    echo "  --endpoint PATH    URL path to profile (default: /)"
    echo "  --method METHOD    HTTP method (default: GET)"
    echo "  --body DATA        Request body for POST/PUT"
    echo "  --requests N       Number of requests to fire (default: 20)"
    echo "  --no-build         Skip fastly compute build"
    echo "  --open             Auto-open Firefox Profiler after capture (macOS)"
    echo "  --help             Show this help"
    exit 0
}

# --- Parse Arguments ---

while [[ $# -gt 0 ]]; do
    case "$1" in
        --endpoint)  ENDPOINT="$2"; shift 2 ;;
        --method)    METHOD="$2"; shift 2 ;;
        --body)      BODY="$2"; shift 2 ;;
        --requests)  REQUESTS="$2"; shift 2 ;;
        --no-build)  SKIP_BUILD=true; shift ;;
        --open)      AUTO_OPEN=true; shift ;;
        --help|-h)   usage ;;
        *)           log_error "Unknown option: $1"; usage ;;
    esac
done

# --- Main ---

log_header "WASM GUEST PROFILER"
log_info "Endpoint: $METHOD $ENDPOINT"
log_info "Requests: $REQUESTS"

# Step 0: Kill any existing server on the profiling port
EXISTING_PID=$(lsof -ti :7676 2>/dev/null | grep -v "^$" || true)
if [ -n "$EXISTING_PID" ]; then
    log_warn "Port 7676 already in use (PID: $EXISTING_PID). Stopping existing server..."
    kill $EXISTING_PID 2>/dev/null || true
    sleep 1
    # Force kill if still alive
    if lsof -ti :7676 &>/dev/null; then
        kill -9 $(lsof -ti :7676) 2>/dev/null || true
        sleep 1
    fi
    log_info "Existing server stopped."
fi

# Step 1: Build
if [ "$SKIP_BUILD" = false ]; then
    log_header "BUILD"
    log_info "Building WASM binary with debug symbols (release + debug=1)..."
    (cd "$PROJECT_ROOT" && fastly compute build)
    echo ""
    log_info "Build complete."
else
    log_info "Skipping build (--no-build)"
fi

# Step 2: Start server with --profile-guest
log_header "START PROFILING SERVER"
log_info "Starting fastly compute serve --profile-guest..."

(cd "$PROJECT_ROOT" && fastly compute serve --profile-guest 2>&1) &
SERVER_PID=$!
log_info "Server PID: $SERVER_PID"

# Wait for server to be ready
log_info "Waiting for server at $BASE_URL..."
for i in $(seq 1 30); do
    if curl -s -o /dev/null --max-time 2 "$BASE_URL/" 2>/dev/null; then
        log_info "Server ready."
        break
    fi
    if ! kill -0 "$SERVER_PID" 2>/dev/null; then
        log_error "Server process exited unexpectedly."
        exit 1
    fi
    if [ "$i" -eq 30 ]; then
        log_error "Server did not become ready within 30 seconds."
        exit 1
    fi
    sleep 1
done

# Step 3: Fire requests
log_header "CAPTURING PROFILE"
log_info "Firing $REQUESTS requests to $METHOD $ENDPOINT..."

CURL_ARGS=(-s -o /dev/null -X "$METHOD")
if [ -n "$BODY" ]; then
    CURL_ARGS+=(-H "Content-Type: application/json" -d "$BODY")
fi

for i in $(seq 1 "$REQUESTS"); do
    local_code=$(curl -w "%{http_code}" "${CURL_ARGS[@]}" "${BASE_URL}${ENDPOINT}" --max-time 30 2>/dev/null || echo "000")
    printf "\r  Request %d/%d (HTTP %s)" "$i" "$REQUESTS" "$local_code"
done
echo ""
log_info "All requests complete."

# Step 4: Stop server (profile is written on exit)
log_header "COLLECTING PROFILE"
log_info "Stopping server to flush profile data..."

stop_server
SERVER_PID=""

# Step 5: Find and move profile file
# Viceroy writes profiles to guest-profiles/ directory (e.g., guest-profiles/1771483114-2.json)
# or as guest-profile-*.json in the project root depending on CLI version
mkdir -p "$PROFILE_DIR"
TIMESTAMP=$(date '+%Y%m%d-%H%M%S')

GUEST_PROFILES_DIR="$PROJECT_ROOT/guest-profiles"
if [ -d "$GUEST_PROFILES_DIR" ]; then
    # Find the most recently modified .json file in guest-profiles/
    PROFILE_FILE=$(find "$GUEST_PROFILES_DIR" -name "*.json" -newer "$0" -print 2>/dev/null | head -1 || true)
    if [ -n "$PROFILE_FILE" ]; then
        DEST="$PROFILE_DIR/profile-${TIMESTAMP}.json"
        cp "$PROFILE_FILE" "$DEST"
    fi
fi

if [ -z "${DEST:-}" ] || [ ! -f "${DEST:-}" ]; then
    # Fallback: check project root for guest-profile-*.json
    PROFILE_FILE=$(find "$PROJECT_ROOT" -maxdepth 1 -name "guest-profile-*.json" -newer "$0" -print -quit 2>/dev/null || true)
    if [ -n "$PROFILE_FILE" ]; then
        DEST="$PROFILE_DIR/profile-${TIMESTAMP}.json"
        mv "$PROFILE_FILE" "$DEST"
    fi
fi

if [ -z "${DEST:-}" ] || [ ! -f "${DEST:-}" ]; then
    log_warn "No profile file found."
    log_warn "Check $GUEST_PROFILES_DIR/ or $PROJECT_ROOT/ for profile output."
    log_warn "The --profile-guest flag may not be supported by your Fastly CLI version."
    exit 1
fi

FILE_SIZE=$(du -h "$DEST" | cut -f1)

log_header "PROFILE CAPTURED"
log_info "File: $DEST"
log_info "Size: $FILE_SIZE"
log_info "Samples: ~$((REQUESTS * 20)) (estimated at 50us intervals)"
echo ""
echo -e "${BOLD}To view the flame graph:${RESET}"
echo "  1. Open https://profiler.firefox.com/"
echo "  2. Drag and drop: $DEST"
echo ""
echo -e "${BOLD}What to look for:${RESET}"
echo "  - Tall stacks in GzDecoder/GzEncoder = compression overhead"
echo "  - Wide bars in lol_html = HTML rewriting cost"
echo "  - Time in format!/replace/to_string = string allocation churn"
echo "  - Time in Settings::deserialize = init overhead"
echo ""

# Step 6: Auto-open if requested
if [ "$AUTO_OPEN" = true ]; then
    if command -v open &>/dev/null; then
        log_info "Opening Firefox Profiler..."
        open "https://profiler.firefox.com/"
        log_info "Drag the profile file onto the page to load it."
    else
        log_warn "--open is only supported on macOS"
    fi
fi
