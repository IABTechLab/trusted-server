#!/usr/bin/env bash
#
# Diff Fastly responses between two Trusted Server revisions under Viceroy.
#
# Usage: scripts/fastly-response-diff.sh --base <rev> --head <rev>
#
# Checks out each revision in a temporary detached worktree under $TMPDIR,
# builds its release Fastly WASM there, and serves it under Viceroy on its own
# port. Both use the revision's integration fixture config with two additions:
# the trace page is enabled, so the trace action reaches its handler, and one
# asset route points at the loopback origin. A Node origin serves the
# publisher pages for both. scripts/fastly-response-diff.mjs then sends
# scripts/fastly-response-diff.requests.json to both servers and checks the
# differences against scripts/fastly-response-diff.expected.json.
#
# Exits with the diff's status: 0 when every difference is expected, 1 when
# one is not or an expected one is missing, 2 on bad input. Build, config or
# startup failures exit 1 after printing the failing log. Worktrees, servers
# and logs are removed on exit.
#
# Environment overrides: FASTLY_RESPONSE_DIFF_BASE_PORT (default 17690),
# FASTLY_RESPONSE_DIFF_HEAD_PORT (17691), FASTLY_RESPONSE_DIFF_ORIGIN_PORT
# (17692).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# Tool shims such as asdf's resolve versions from .tool-versions in the
# working directory, so run from the checkout.
cd "$REPO_ROOT"
BASE_PORT="${FASTLY_RESPONSE_DIFF_BASE_PORT:-17690}"
HEAD_PORT="${FASTLY_RESPONSE_DIFF_HEAD_PORT:-17691}"
ORIGIN_PORT="${FASTLY_RESPONSE_DIFF_ORIGIN_PORT:-17692}"
DIFF_SCRIPT="$REPO_ROOT/scripts/fastly-response-diff.mjs"
REQUESTS="$REPO_ROOT/scripts/fastly-response-diff.requests.json"
EXPECTED="$REPO_ROOT/scripts/fastly-response-diff.expected.json"
FIXTURE="crates/trusted-server-integration-tests/fixtures/configs/trusted-server.integration.toml"
WASM="target/wasm32-wasip1/release/trusted-server-adapter-fastly.wasm"

usage() {
    echo "Usage: $0 --base <rev> --head <rev>" >&2
    exit 2
}

die() {
    echo "fastly-response-diff: $*" >&2
    exit 1
}

BASE_REV=""
HEAD_REV=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        --base)
            [ "$#" -ge 2 ] || usage
            BASE_REV="$2"
            shift 2
            ;;
        --head)
            [ "$#" -ge 2 ] || usage
            HEAD_REV="$2"
            shift 2
            ;;
        -h | --help)
            usage
            ;;
        *)
            echo "fastly-response-diff: unknown argument: $1" >&2
            usage
            ;;
    esac
done
if [ -z "$BASE_REV" ] || [ -z "$HEAD_REV" ]; then
    usage
fi

for command in git cargo viceroy node curl awk; do
    command -v "$command" >/dev/null 2>&1 || die "required command not found: $command"
done

BASE_SHA="$(git -C "$REPO_ROOT" rev-parse --verify --quiet "$BASE_REV^{commit}")" ||
    die "cannot resolve --base $BASE_REV"
HEAD_SHA="$(git -C "$REPO_ROOT" rev-parse --verify --quiet "$HEAD_REV^{commit}")" ||
    die "cannot resolve --head $HEAD_REV"

port_in_use() {
    (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null
}

for port in "$BASE_PORT" "$HEAD_PORT" "$ORIGIN_PORT"; do
    if port_in_use "$port"; then
        die "port $port is already in use; set the FASTLY_RESPONSE_DIFF_*_PORT overrides"
    fi
done

WORK="$(mktemp -d "${TMPDIR:-/tmp}/fastly-response-diff.XXXXXX")"
# Resolve symlinks such as macOS's /var -> /private/var, so the paths match
# what git records for the worktrees.
WORK="$(cd "$WORK" && pwd -P)"
ADDED_TREES=()
PIDS=()

# The EXIT trap runs this. ShellCheck 0.10 reports a trap handler as
# unreachable when the script ends in `exit`.
# shellcheck disable=SC2317
cleanup() {
    local status=$?
    local pid
    for pid in "${PIDS[@]+"${PIDS[@]}"}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
    local removed=true
    local tree
    for tree in "${ADDED_TREES[@]+"${ADDED_TREES[@]}"}"; do
        if ! git -C "$REPO_ROOT" worktree remove "$tree"; then
            removed=false
        fi
    done
    if [ "$removed" = true ]; then
        rm -rf -- "$WORK"
    else
        echo "fastly-response-diff: a worktree could not be removed; inspect $WORK and run git worktree remove on it" >&2
    fi
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir "$WORK/logs"

show_log() {
    echo "---- last lines of $1 ----" >&2
    tail -n 40 "$1" >&2 || true
}

# Writes the revision's integration fixture with the trace page enabled and
# an asset route to the loopback origin.
write_app_config() {
    local tree="$1"
    local output="$2"
    [ -f "$tree/$FIXTURE" ] || die "$FIXTURE is missing at $(git -C "$tree" rev-parse --short HEAD)"
    awk '
        { print }
        $0 == "[integrations.gpt_diagnostics]" {
            print "trace_page_enabled = true"
            inserted++
        }
        END {
            if (inserted != 1) {
                print "expected one [integrations.gpt_diagnostics] table; found " (inserted + 0) > "/dev/stderr"
                exit 1
            }
        }
    ' "$tree/$FIXTURE" >"$output"
    cat >>"$output" <<EOF

[[proxy.asset_routes]]
prefix = "/assets/"
origin_url = "http://127.0.0.1:$ORIGIN_PORT"
EOF
}

# Builds one revision and generates its Viceroy config.
prepare() {
    local side="$1"
    local sha="$2"
    local tree="$WORK/$side"
    local log="$WORK/logs/$side-build.log"

    git -C "$REPO_ROOT" worktree add --quiet --detach "$tree" "$sha"
    ADDED_TREES+=("$tree")
    write_app_config "$tree" "$WORK/$side-app-config.toml"

    echo "==> Building $side ($(git -C "$tree" rev-parse --short HEAD))..."
    # Each worktree keeps its own target directory, so the two builds never
    # share artifacts.
    if ! (cd "$tree" && CARGO_TARGET_DIR="$tree/target" cargo build \
        --package trusted-server-adapter-fastly --release --target wasm32-wasip1) >"$log" 2>&1; then
        show_log "$log"
        die "$side build failed"
    fi
    if ! (cd "$tree" &&
        INTEGRATION_ORIGIN_PORT="$ORIGIN_PORT" \
            ARTIFACTS_DIR="$WORK/$side-artifacts" \
            INTEGRATION_APP_CONFIG_PATH="$WORK/$side-app-config.toml" \
            ./scripts/generate-integration-viceroy-configs.sh) >>"$log" 2>&1; then
        show_log "$log"
        die "$side Viceroy config generation failed"
    fi
}

# Waits until `url` answers 200, or fails after about 60 seconds.
wait_for_ok() {
    local url="$1"
    local log="$2"
    local attempt
    for attempt in $(seq 1 120); do
        if [ "$(curl --silent --output /dev/null --write-out '%{http_code}' "$url" || true)" = "200" ]; then
            return 0
        fi
        sleep 0.5
    done
    show_log "$log"
    die "$url did not answer 200 after $attempt attempts"
}

prepare base "$BASE_SHA"
prepare head "$HEAD_SHA"

echo "==> Starting the loopback origin on port $ORIGIN_PORT..."
node "$DIFF_SCRIPT" --serve-origin "$ORIGIN_PORT" >"$WORK/logs/origin.log" 2>&1 &
PIDS+=("$!")
wait_for_ok "http://127.0.0.1:$ORIGIN_PORT/" "$WORK/logs/origin.log"

for side in base head; do
    if [ "$side" = base ]; then port="$BASE_PORT"; else port="$HEAD_PORT"; fi
    echo "==> Serving $side under Viceroy on port $port..."
    viceroy "$WORK/$side/$WASM" \
        -C "$WORK/$side-artifacts/configs/viceroy.toml" \
        --addr "127.0.0.1:$port" >"$WORK/logs/$side-viceroy.log" 2>&1 &
    PIDS+=("$!")
    wait_for_ok "http://127.0.0.1:$port/health" "$WORK/logs/$side-viceroy.log"
done

echo "==> Diffing $(git -C "$REPO_ROOT" rev-parse --short "$BASE_SHA") against $(git -C "$REPO_ROOT" rev-parse --short "$HEAD_SHA")..."
diff_status=0
node "$DIFF_SCRIPT" \
    --origin "http://127.0.0.1:$ORIGIN_PORT" \
    "http://127.0.0.1:$BASE_PORT" \
    "http://127.0.0.1:$HEAD_PORT" \
    "$REQUESTS" \
    "$EXPECTED" || diff_status=$?
if [ "$diff_status" -eq 2 ]; then
    show_log "$WORK/logs/base-viceroy.log"
    show_log "$WORK/logs/head-viceroy.log"
fi
exit "$diff_status"
