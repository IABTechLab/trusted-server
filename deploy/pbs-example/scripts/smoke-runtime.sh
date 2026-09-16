#!/usr/bin/env bash
set -euo pipefail

example_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
project_name="pbs-example-smoke"
host_port="${PBS_SMOKE_PORT:-18080}"
compose=(
  docker compose
  --project-name "$project_name"
  --env-file "$example_dir/runtime/examples/compose.env"
  -f "$example_dir/runtime/compose.yaml"
)

cleanup() {
  "${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

export PBS_HOST_PORT="$host_port"
"${compose[@]}" up --detach

for _ in $(seq 1 30); do
  if response="$(curl --fail --silent --show-error "http://127.0.0.1:${host_port}/status" 2>/dev/null)"; then
    if [[ "$response" != "ok" ]]; then
      printf 'Unexpected PBS status response: %s\n' "$response" >&2
      exit 1
    fi

    logs="$("${compose[@]}" logs --no-color)"
    if grep -Fq "example-only-api-key" <<<"$logs" || grep -Fq "example-only-optional-token" <<<"$logs"; then
      printf 'PBS startup logs exposed a dummy secret.\n' >&2
      exit 1
    fi

    printf 'PBS runtime and startup-log redaction smoke passed on port %s.\n' "$host_port"
    exit 0
  fi
  sleep 1
done

"${compose[@]}" logs --no-color >&2
printf 'PBS did not become healthy within 30 seconds.\n' >&2
exit 1
