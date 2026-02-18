#!/usr/bin/env bash
#
# Trusted Server Performance Benchmark
#
# Measures request latency against a running Viceroy instance.
# Run this on main, save the output, then run on your branch and compare.
#
# Prerequisites:
#   - Viceroy running: fastly compute serve
#   - hey installed: brew install hey
#
# Usage:
#   ./scripts/benchmark.sh                    # Run all benchmarks
#   ./scripts/benchmark.sh --cold-start       # Cold start analysis only
#   ./scripts/benchmark.sh --load-test        # Load test only
#   ./scripts/benchmark.sh --quick            # Quick smoke test (fewer requests)
#   ./scripts/benchmark.sh --profile          # Server-Timing phase breakdown (init/backend/process)
#   ./scripts/benchmark.sh --save baseline    # Save results to file
#   ./scripts/benchmark.sh --compare baseline # Compare against saved results
#
# What this measures:
#   - Cold start: first request latency after server restart
#   - Warm latency: subsequent request timing breakdown (DNS, connect, TTFB, transfer, total)
#   - Throughput: requests/sec under concurrent load
#   - Latency distribution: p50, p95, p99 under load
#
# What this does NOT measure:
#   - Real Fastly edge performance (Viceroy is a simulator)
#   - Network latency to real backends
#   - Production TLS handshake overhead
#   - WASM cold start on actual Fastly infrastructure
#
# The value is in RELATIVE comparison between branches, not absolute numbers.

set -euo pipefail

# --- Configuration ---
BASE_URL="${BENCH_URL:-http://127.0.0.1:7676}"
RESULTS_DIR="$(cd "$(dirname "$0")/.." && pwd)/benchmark-results"
CURL_FORMAT='
{
  "dns_ms":        %{time_namelookup},
  "connect_ms":    %{time_connect},
  "tls_ms":        %{time_appconnect},
  "ttfb_ms":       %{time_starttransfer},
  "total_ms":      %{time_total},
  "size_bytes":    %{size_download},
  "http_code":     %{http_code}
}'

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

check_server() {
    if ! curl -s -o /dev/null -w "%{http_code}" "$BASE_URL/" --max-time 2 > /dev/null 2>&1; then
        log_error "Server not responding at $BASE_URL"
        log_error "Start it first: fastly compute serve"
        exit 1
    fi
    log_info "Server responding at $BASE_URL"
}

check_hey() {
    if ! command -v hey &> /dev/null; then
        log_warn "'hey' not installed. Attempting to install via brew..."
        if command -v brew &> /dev/null; then
            brew install hey
            if ! command -v hey &> /dev/null; then
                log_error "Failed to install 'hey'. Load tests will be skipped."
                return 1
            fi
            log_info "'hey' installed successfully."
        else
            log_error "'brew' not found. Install hey manually: https://github.com/rakyll/hey"
            log_error "Load tests will be skipped."
            return 1
        fi
    fi
    return 0
}

# Timed curl request — prints JSON timing breakdown
timed_curl() {
    local method="$1"
    local url="$2"
    local label="$3"
    shift 3
    local extra_args=("$@")

    local result
    result=$(curl -s -o /dev/null -w "$CURL_FORMAT" \
        -X "$method" \
        ${extra_args[@]+"${extra_args[@]}"} \
        "$url" \
        --max-time 30 2>/dev/null)

    local total
    total=$(echo "$result" | grep total_ms | tr -d '", ' | cut -d: -f2)
    local ttfb
    ttfb=$(echo "$result" | grep ttfb_ms | tr -d '", ' | cut -d: -f2)
    local code
    code=$(echo "$result" | grep http_code | tr -d '", ' | cut -d: -f2)
    local size
    size=$(echo "$result" | grep size_bytes | tr -d '", ' | cut -d: -f2)

    # Convert seconds to ms for display
    local total_ms ttfb_ms
    total_ms=$(echo "$total * 1000" | bc 2>/dev/null || echo "$total")
    ttfb_ms=$(echo "$ttfb * 1000" | bc 2>/dev/null || echo "$ttfb")

    printf "  %-40s  HTTP %s  TTFB: %8.2f ms  Total: %8.2f ms  Size: %s bytes\n" \
        "$label" "$code" "$ttfb_ms" "$total_ms" "$size"

    echo "$result"
}

# --- Test Data ---

AUCTION_PAYLOAD='{
  "adUnits": [
    {
      "code": "header-banner",
      "mediaTypes": {
        "banner": {
          "sizes": [[728, 90], [970, 250]]
        }
      }
    },
    {
      "code": "sidebar",
      "mediaTypes": {
        "banner": {
          "sizes": [[300, 250], [300, 600]]
        }
      }
    }
  ]
}'

# --- Benchmark Suites ---

run_cold_start() {
    log_header "COLD START ANALYSIS"
    log_info "Measuring first-request latency (simulated via sequential requests)"
    log_info "In production, cold start includes WASM instantiation which Viceroy may not reflect."
    echo ""

    echo -e "${BOLD}First request (potential cold path):${RESET}"
    timed_curl GET "$BASE_URL/" "GET / (first)" > /dev/null

    echo ""
    echo -e "${BOLD}Subsequent requests (warm path):${RESET}"
    for i in 1 2 3 4 5; do
        timed_curl GET "$BASE_URL/" "GET / (warm #$i)" > /dev/null
    done
}

run_endpoint_latency() {
    log_header "ENDPOINT LATENCY (WARM)"
    log_info "Per-endpoint timing breakdown (5 requests each, reporting median-ish)"
    echo ""

    local endpoints=(
        "GET|/|Publisher proxy (fallback)"
        "GET|/static/tsjs=tsjs-unified.min.js|Static JS bundle"
        "GET|/.well-known/trusted-server.json|Discovery endpoint"
    )

    for entry in "${endpoints[@]}"; do
        IFS='|' read -r method path label <<< "$entry"
        echo -e "${BOLD}$label${RESET}  ($method $path)"

        for i in $(seq 1 5); do
            timed_curl "$method" "${BASE_URL}${path}" "  request #$i" > /dev/null
        done
        echo ""
    done

    # Auction endpoint (POST with body)
    echo -e "${BOLD}Auction endpoint${RESET}  (POST /auction)"
    for i in $(seq 1 5); do
        timed_curl POST "${BASE_URL}/auction" "  request #$i" \
            -H "Content-Type: application/json" \
            -d "$AUCTION_PAYLOAD" > /dev/null
    done
    echo ""
}

run_load_test() {
    if ! check_hey; then
        return
    fi

    log_header "LOAD TEST"
    log_info "Concurrent request throughput and latency distribution"
    echo ""

    local total_requests="${1:-200}"
    local concurrency="${2:-10}"

    echo -e "${BOLD}GET / (publisher proxy) - ${total_requests} requests, ${concurrency} concurrent${RESET}"
    echo ""
    hey -n "$total_requests" -c "$concurrency" -t 30 "$BASE_URL/" 2>&1 | \
        grep -E "(Requests/sec|Total:|Slowest:|Fastest:|Average:|requests done)|Status code|Latency distribution" -A 20
    echo ""

    echo -e "${BOLD}GET /static/tsjs=tsjs-unified.min.js (static) - ${total_requests} requests, ${concurrency} concurrent${RESET}"
    echo ""
    hey -n "$total_requests" -c "$concurrency" -t 30 "$BASE_URL/static/tsjs=tsjs-unified.min.js" 2>&1 | \
        grep -E "(Requests/sec|Total:|Slowest:|Fastest:|Average:|requests done)|Status code|Latency distribution" -A 20
    echo ""

    echo -e "${BOLD}POST /auction - ${total_requests} requests, ${concurrency} concurrent${RESET}"
    echo ""
    hey -n "$total_requests" -c "$concurrency" -t 30 \
        -m POST \
        -H "Content-Type: application/json" \
        -d "$AUCTION_PAYLOAD" \
        "$BASE_URL/auction" 2>&1 | \
        grep -E "(Requests/sec|Total:|Slowest:|Fastest:|Average:|requests done)|Status code|Latency distribution" -A 20
    echo ""
}

run_first_byte_analysis() {
    log_header "TIME TO FIRST BYTE (TTFB) ANALYSIS"
    log_info "Measures TTFB across 20 sequential requests to detect patterns"
    log_info "Look for: first request significantly slower than rest = cold start"
    echo ""

    echo -e "${BOLD}Sequential TTFB for GET / :${RESET}"
    echo ""
    printf "  %-8s  %-12s  %-12s\n" "Request" "TTFB (ms)" "Total (ms)"
    printf "  %-8s  %-12s  %-12s\n" "-------" "---------" "----------"

    for i in $(seq 1 20); do
        local result
        result=$(curl -s -o /dev/null -w "%{time_starttransfer} %{time_total}" \
            "$BASE_URL/" --max-time 30 2>/dev/null)
        local ttfb total
        ttfb=$(echo "$result" | awk '{printf "%.2f", $1 * 1000}')
        total=$(echo "$result" | awk '{printf "%.2f", $2 * 1000}')
        printf "  %-8s  %-12s  %-12s\n" "#$i" "${ttfb}" "${total}"
    done
    echo ""
}

# --- Server-Timing Profiler ---

# Parse "init;dur=1.2, backend;dur=385.4, process;dur=12.3, total;dur=401.5"
# into associative-style variables: st_init=1.2, st_backend=385.4, etc.
parse_server_timing() {
    local header="$1"
    st_init="" st_backend="" st_process="" st_total=""
    for part in $(echo "$header" | tr ',' '\n'); do
        local name dur
        name=$(echo "$part" | sed 's/;.*//' | tr -d ' ')
        dur=$(echo "$part" | grep -o 'dur=[0-9.]*' | cut -d= -f2)
        case "$name" in
            init)    st_init="$dur" ;;
            backend) st_backend="$dur" ;;
            process) st_process="$dur" ;;
            total)   st_total="$dur" ;;
        esac
    done
}

# Collect Server-Timing data over N requests and print stats
# Also captures external TTFB and total (TTLB) for streaming comparison
profile_endpoint() {
    local method="$1"
    local url="$2"
    local label="$3"
    local iterations="${4:-20}"
    shift 4
    local extra_args=("$@")

    local init_vals=() backend_vals=() process_vals=() total_vals=()
    local ttfb_vals=() ttlb_vals=()

    for i in $(seq 1 "$iterations"); do
        # Capture both Server-Timing header and curl timing in one request
        local raw
        raw=$(curl -s -D- -o /dev/null \
            -w '\n__CURL_TIMING__ %{time_starttransfer} %{time_total}' \
            -X "$method" \
            ${extra_args[@]+"${extra_args[@]}"} \
            "$url" \
            --max-time 30 2>/dev/null)

        # Extract Server-Timing header
        local header
        header=$(echo "$raw" | grep -i '^server-timing:' | sed 's/[Ss]erver-[Tt]iming: *//')

        # Extract curl timing (TTFB and total in seconds)
        local curl_timing
        curl_timing=$(echo "$raw" | grep '__CURL_TIMING__' | sed 's/__CURL_TIMING__ //')
        if [ -n "$curl_timing" ]; then
            local ext_ttfb ext_total
            ext_ttfb=$(echo "$curl_timing" | awk '{printf "%.1f", $1 * 1000}')
            ext_total=$(echo "$curl_timing" | awk '{printf "%.1f", $2 * 1000}')
            ttfb_vals+=("$ext_ttfb")
            ttlb_vals+=("$ext_total")
        fi

        if [ -z "$header" ]; then
            continue
        fi

        parse_server_timing "$header"
        [ -n "$st_init" ]    && init_vals+=("$st_init")
        [ -n "$st_backend" ] && backend_vals+=("$st_backend")
        [ -n "$st_process" ] && process_vals+=("$st_process")
        [ -n "$st_total" ]   && total_vals+=("$st_total")
    done

    echo -e "  ${BOLD}$label${RESET}  ($method, $iterations iterations)"
    echo ""
    printf "  %-12s  %8s  %8s  %8s  %8s\n" "Phase" "Min" "Avg" "Max" "P95"
    printf "  %-12s  %8s  %8s  %8s  %8s\n" "----------" "------" "------" "------" "------"
    print_stats "init"    "${init_vals[@]}"
    print_stats "backend" "${backend_vals[@]}"
    print_stats "process" "${process_vals[@]}"
    print_stats "total"   "${total_vals[@]}"
    echo ""
    echo -e "  ${BOLD}External timing (curl):${RESET}"
    printf "  %-12s  %8s  %8s  %8s  %8s\n" "Metric" "Min" "Avg" "Max" "P95"
    printf "  %-12s  %8s  %8s  %8s  %8s\n" "----------" "------" "------" "------" "------"
    print_stats "TTFB"    "${ttfb_vals[@]}"
    print_stats "TTLB"    "${ttlb_vals[@]}"
    echo ""
}

# Compute min/avg/max/p95 from a list of floats
print_stats() {
    local name="$1"
    shift
    local vals=("$@")
    local count=${#vals[@]}

    if [ "$count" -eq 0 ]; then
        printf "  %-12s  %8s  %8s  %8s  %8s\n" "$name" "-" "-" "-" "-"
        return
    fi

    # Sort values
    local sorted
    sorted=$(printf '%s\n' "${vals[@]}" | sort -g)

    local min avg max p95
    min=$(echo "$sorted" | head -1)
    max=$(echo "$sorted" | tail -1)

    local sum
    sum=$(printf '%s\n' "${vals[@]}" | awk '{s+=$1} END {printf "%.1f", s}')
    avg=$(echo "$sum $count" | awk '{printf "%.1f", $1/$2}')

    local p95_idx
    p95_idx=$(echo "$count" | awk '{printf "%d", int($1 * 0.95 + 0.5)}')
    [ "$p95_idx" -lt 1 ] && p95_idx=1
    p95=$(echo "$sorted" | sed -n "${p95_idx}p")

    printf "  %-12s  %7.1f   %7.1f   %7.1f   %7.1f\n" "$name" "$min" "$avg" "$max" "$p95"
}

run_profile() {
    local iterations="${1:-20}"

    log_header "SERVER-TIMING PROFILE"
    log_info "Collecting Server-Timing header data over $iterations requests per endpoint"
    log_info "Phases: init (setup) → backend (origin fetch) → process (body rewrite) → total"
    echo ""

    profile_endpoint GET "$BASE_URL/static/tsjs=tsjs-unified.min.js" \
        "Static JS bundle" "$iterations"

    profile_endpoint GET "$BASE_URL/.well-known/trusted-server.json" \
        "Discovery endpoint" "$iterations"

    profile_endpoint GET "$BASE_URL/" \
        "Publisher proxy (fallback)" "$iterations"

    profile_endpoint POST "$BASE_URL/auction" \
        "Auction endpoint" "$iterations" \
        -H "Content-Type: application/json" \
        -d "$AUCTION_PAYLOAD"
}

save_results() {
    local name="$1"
    mkdir -p "$RESULTS_DIR"
    local outfile="$RESULTS_DIR/${name}.txt"

    log_info "Saving results to $outfile"

    {
        echo "# Benchmark Results: $name"
        echo "# Date: $(date -u '+%Y-%m-%d %H:%M:%S UTC')"
        echo "# Git: $(git -C "$(dirname "$0")/.." rev-parse --short HEAD 2>/dev/null || echo 'unknown')"
        echo "# Branch: $(git -C "$(dirname "$0")/.." branch --show-current 2>/dev/null || echo 'unknown')"
        echo ""
        run_all 2>&1
    } > "$outfile"

    log_info "Results saved. Compare later with: diff $RESULTS_DIR/baseline.txt $RESULTS_DIR/branch.txt"
}

compare_results() {
    local name="$1"
    local baseline="$RESULTS_DIR/${name}.txt"

    if [ ! -f "$baseline" ]; then
        log_error "No saved results found at $baseline"
        log_error "Run with --save $name first"
        exit 1
    fi

    local current
    current=$(mktemp)
    run_all 2>&1 > "$current"

    log_header "COMPARISON: current vs $name"
    diff --color=auto -u "$baseline" "$current" || true
    rm -f "$current"
}

run_all() {
    echo -e "${BOLD}Trusted Server Performance Benchmark${RESET}"
    echo "Date: $(date -u '+%Y-%m-%d %H:%M:%S UTC')"
    echo "Git:  $(git -C "$(dirname "$0")/.." rev-parse --short HEAD 2>/dev/null || echo 'unknown')"
    echo "Branch: $(git -C "$(dirname "$0")/.." branch --show-current 2>/dev/null || echo 'unknown')"
    echo "Server: $BASE_URL"

    run_cold_start
    run_first_byte_analysis
    run_endpoint_latency
    run_load_test 200 10
}

run_quick() {
    echo -e "${BOLD}Trusted Server Performance Benchmark (Quick)${RESET}"
    echo "Date: $(date -u '+%Y-%m-%d %H:%M:%S UTC')"
    echo "Git:  $(git -C "$(dirname "$0")/.." rev-parse --short HEAD 2>/dev/null || echo 'unknown')"
    echo "Server: $BASE_URL"

    run_first_byte_analysis
    run_load_test 50 5
}

# --- Main ---

main() {
    local mode="${1:-all}"

    check_server

    case "$mode" in
        --cold-start)
            run_cold_start
            ;;
        --load-test)
            run_load_test "${2:-200}" "${3:-10}"
            ;;
        --quick)
            run_quick
            ;;
        --ttfb)
            run_first_byte_analysis
            ;;
        --profile)
            run_profile "${2:-20}"
            ;;
        --save)
            save_results "${2:?Usage: --save <name>}"
            ;;
        --compare)
            compare_results "${2:?Usage: --compare <name>}"
            ;;
        --help|-h)
            head -30 "$0" | grep '^#' | sed 's/^# \?//'
            ;;
        *)
            run_all
            ;;
    esac
}

main "$@"
