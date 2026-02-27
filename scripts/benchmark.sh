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
#   ./scripts/benchmark.sh --ttfb             # TTFB analysis only
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

METRICS_SUMMARY=()

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
        "$label" "$code" "$ttfb_ms" "$total_ms" "$size" >&2

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

run_load_benchmark() {
    local name="$1"
    local total_requests="$2"
    local concurrency="$3"
    local repeats="$4"
    local method="$5"
    local url="$6"
    shift 6
    local extra_args=("$@")

    echo -e "${BOLD}${method} ${url} - ${total_requests} requests, ${concurrency} concurrent (${repeats} runs)${RESET}"
    echo ""

    local rps_values=()
    local p50_values=()
    local p95_values=()

    for r in $(seq 1 "$repeats"); do
        # Warmup (discarded)
        hey -n 50 -c "$concurrency" -t 30 ${extra_args[@]+"${extra_args[@]}"} "$url" > /dev/null 2>&1

        # Measured
        local output
        output=$(hey -n "$total_requests" -c "$concurrency" -t 30 ${extra_args[@]+"${extra_args[@]}"} "$url" 2>&1)
        
        # Display the 1st run's full output for human readability
        if [ "$r" -eq 1 ]; then
            echo "$output" | grep -E "(Requests/sec|Total:|Slowest:|Fastest:|Average:|requests done)|Status code|Latency distribution" -A 20
        fi
        
        local rps p50 p95
        rps=$(echo "$output" | awk '/Requests\/sec:/ {print $2}')
        p50=$(echo "$output" | awk '/50% in/ {print $3}')
        p95=$(echo "$output" | awk '/95% in/ {print $3}')
        
        rps=${rps:-0}
        p50=${p50:-0}
        p95=${p95:-0}
        
        rps_values+=("$rps")
        p50_values+=("$p50")
        p95_values+=("$p95")
        
        if [ "$repeats" -gt 1 ]; then
            echo "  Run $r: $rps req/sec, p50: $p50 secs, p95: $p95 secs"
        fi
    done

    # Compute mean and stddev for RPS and P50
    local mean_rps mean_p50 mean_p95
    if [ "$repeats" -eq 1 ]; then
        mean_rps="${rps_values[0]}"
        mean_p50="${p50_values[0]}"
        mean_p95="${p95_values[0]}"
    else
        mean_rps=$(printf "%s\n" "${rps_values[@]}" | awk '{sum+=$1; sumsq+=$1*$1} END {if(NR>0){mean=sum/NR; stddev=sqrt(sumsq/NR - mean*mean); printf "%.2f ± %.2f", mean, stddev}else{print "0"}}')
        mean_p50=$(printf "%s\n" "${p50_values[@]}" | awk '{sum+=$1; sumsq+=$1*$1} END {if(NR>0){mean=sum/NR; stddev=sqrt(sumsq/NR - mean*mean); printf "%.4f ± %.4f", mean, stddev}else{print "0"}}')
        mean_p95=$(printf "%s\n" "${p95_values[@]}" | awk '{sum+=$1; sumsq+=$1*$1} END {if(NR>0){mean=sum/NR; stddev=sqrt(sumsq/NR - mean*mean); printf "%.4f ± %.4f", mean, stddev}else{print "0"}}')
    fi

    if [ "$repeats" -gt 1 ]; then
        echo "  -> Avg RPS: $mean_rps"
        echo "  -> Avg P50: $mean_p50 secs"
        echo "  -> Avg P95: $mean_p95 secs"
        echo ""
    fi
    
    # Store for metrics summary
    local calc_mean_rps calc_mean_p50 calc_mean_p95
    calc_mean_rps=$(printf "%s\n" "${rps_values[@]}" | awk '{sum+=$1} END {if(NR>0) printf "%.2f", sum/NR}')
    calc_mean_p50=$(printf "%s\n" "${p50_values[@]}" | awk '{sum+=$1} END {if(NR>0) printf "%.4f", sum/NR}')
    calc_mean_p95=$(printf "%s\n" "${p95_values[@]}" | awk '{sum+=$1} END {if(NR>0) printf "%.4f", sum/NR}')
    
    METRICS_SUMMARY+=("${name}_rps=$calc_mean_rps")
    METRICS_SUMMARY+=("${name}_p50=$calc_mean_p50")
    METRICS_SUMMARY+=("${name}_p95=$calc_mean_p95")
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
    local repeats="${3:-3}"

    run_load_benchmark "publisher_proxy" "$total_requests" "$concurrency" "$repeats" GET "$BASE_URL/"
    run_load_benchmark "static_js" "$total_requests" "$concurrency" "$repeats" GET "$BASE_URL/static/tsjs=tsjs-unified.min.js"
    run_load_benchmark "auction" "$total_requests" "$concurrency" "$repeats" POST "$BASE_URL/auction" -m POST -H "Content-Type: application/json" -d "$AUCTION_PAYLOAD"
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
    
    local base_metrics curr_metrics
    base_metrics=$(mktemp)
    curr_metrics=$(mktemp)
    
    awk '/^---METRICS---$/ {flag=1; next} flag {print}' "$baseline" > "$base_metrics"
    awk '/^---METRICS---$/ {flag=1; next} flag {print}' "$current" > "$curr_metrics"
    
    if [ -s "$base_metrics" ] && [ -s "$curr_metrics" ]; then
        echo -e "${BOLD}Metrics Delta (vs baseline):${RESET}"
        printf "  %-30s %-15s -> %-15s %s\n" "Metric" "Baseline" "Current" "Delta"
        printf "  %-30s %-15s    %-15s %s\n" "------" "--------" "-------" "-----"
        
        while IFS='=' read -r key curr_val; do
            base_val=$(grep "^${key}=" "$base_metrics" | cut -d= -f2 || true)
            if [ -n "$base_val" ] && [ "$base_val" != "0" ] && [ "$base_val" != "0.00" ] && [ "$base_val" != "0.0000" ]; then
                # Calculate delta
                delta=$(echo "scale=2; (($curr_val - $base_val) / $base_val) * 100" | bc -l 2>/dev/null || echo "0")
                if [ "$(echo "$delta > 0" | bc -l 2>/dev/null || echo "0")" = "1" ]; then
                    delta="+${delta}%"
                else
                    delta="${delta}%"
                fi
                printf "  %-30s %-15s -> %-15s %s\n" "$key" "$base_val" "$curr_val" "$delta"
            fi
        done < "$curr_metrics"
        echo ""
    fi
    
    echo -e "${BOLD}Full Output Diff:${RESET}"
    diff --color=auto -u "$baseline" "$current" || true
    rm -f "$current" "$base_metrics" "$curr_metrics"
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
    run_load_test 200 10 3

    echo ""
    echo "---METRICS---"
    for metric in ${METRICS_SUMMARY[@]+"${METRICS_SUMMARY[@]}"}; do
        echo "$metric"
    done
}

run_quick() {
    echo -e "${BOLD}Trusted Server Performance Benchmark (Quick)${RESET}"
    echo "Date: $(date -u '+%Y-%m-%d %H:%M:%S UTC')"
    echo "Git:  $(git -C "$(dirname "$0")/.." rev-parse --short HEAD 2>/dev/null || echo 'unknown')"
    echo "Server: $BASE_URL"

    run_first_byte_analysis
    run_load_test 50 5 1

    echo ""
    echo "---METRICS---"
    for metric in ${METRICS_SUMMARY[@]+"${METRICS_SUMMARY[@]}"}; do
        echo "$metric"
    done
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
