#!/usr/bin/env bash
#
# Call the Trusted Server EC batch sync endpoint with a single mapping.
#
# Examples:
#   ./scripts/batch-sync.sh \
#     --base-url https://edge.example.com \
#     --api-key "$PARTNER_API_KEY" \
#     --ec-id "$EC_ID" \
#     --partner-uid <uid>
#
# Environment fallbacks:
#   TS_BASE_URL, PARTNER_API_KEY, EC_ID, PARTNER_UID, TIMESTAMP
#
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: scripts/batch-sync.sh [options]

Required, unless provided via environment:
  --base-url URL       Trusted Server base URL (env: TS_BASE_URL)
  --api-key KEY        Partner Bearer token (env: PARTNER_API_KEY)
  --ec-id EC_ID        Full EC ID: 64hex.6alnum (env: EC_ID)
  --partner-uid UID    Partner user ID to store (env: PARTNER_UID)

Optional:
  --timestamp SECONDS  Unix timestamp (env: TIMESTAMP, default: now)
  -h, --help           Show this help

Example:
  scripts/batch-sync.sh \
    --base-url https://example.com \
    --api-key $PARTNER_API_KEY \
    --ec-id $EC_ID \
    --partner-uid $PARTNER_UID
EOF
}

BASE_URL="${TS_BASE_URL:-}"
API_KEY="${PARTNER_API_KEY:-}"
EC_ID="${EC_ID:-}"
PARTNER_UID="${PARTNER_UID:-}"
TIMESTAMP="${TIMESTAMP:-$(date +%s)}"

while [ "$#" -gt 0 ]; do
    case "$1" in
        --base-url)
            BASE_URL="${2:-}"
            shift 2
            ;;
        --api-key)
            API_KEY="${2:-}"
            shift 2
            ;;
        --ec-id)
            EC_ID="${2:-}"
            shift 2
            ;;
        --partner-uid)
            PARTNER_UID="${2:-}"
            shift 2
            ;;
        --timestamp)
            TIMESTAMP="${2:-}"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

missing=()
[ -n "$BASE_URL" ] || missing+=("--base-url")
[ -n "$API_KEY" ] || missing+=("--api-key")
[ -n "$EC_ID" ] || missing+=("--ec-id")
[ -n "$PARTNER_UID" ] || missing+=("--partner-uid")

if [ "${#missing[@]}" -gt 0 ]; then
    echo "Missing required option(s): ${missing[*]}" >&2
    usage >&2
    exit 2
fi

ENDPOINT="${BASE_URL%/}/_ts/api/v1/batch-sync"
BODY="$(python3 - "$EC_ID" "$PARTNER_UID" "$TIMESTAMP" <<'PY'
import json
import sys

ec_id, partner_uid, timestamp = sys.argv[1:]
try:
    timestamp = int(timestamp)
except ValueError:
    print("timestamp must be an integer Unix timestamp", file=sys.stderr)
    sys.exit(2)

print(json.dumps({
    "mappings": [
        {
            "ec_id": ec_id,
            "partner_uid": partner_uid,
            "timestamp": timestamp,
        }
    ]
}))
PY
)"

RESPONSE_FILE="$(mktemp)"
trap 'rm -f "$RESPONSE_FILE"' EXIT

echo "POST $ENDPOINT" >&2
HTTP_STATUS="$(curl -sS \
    -o "$RESPONSE_FILE" \
    -w "%{http_code}" \
    -X POST "$ENDPOINT" \
    -H "Authorization: Bearer ${API_KEY}" \
    -H "Content-Type: application/json" \
    -d "$BODY")"

cat "$RESPONSE_FILE"
echo

echo "HTTP $HTTP_STATUS" >&2
case "$HTTP_STATUS" in
    2*) exit 0 ;;
    *) exit 1 ;;
esac
