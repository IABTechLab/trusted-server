#!/usr/bin/env bash
set -euo pipefail

CONFIG_STORE_ID="${CONFIG_STORE_ID:-jwks_store}"

echo "🔄 Rotating out old signing keys..."
echo ""

echo "  📋 Fetching current key ID from Config Store '$CONFIG_STORE_ID'..."
CURRENT_KID=$(fastly config-store-entry describe \
  --store-id="$CONFIG_STORE_ID" \
  --key="current-kid" \
  --json 2>/dev/null | jq -r '.item_value // empty') || CURRENT_KID=""

if [[ -z "$CURRENT_KID" ]]; then
  echo "     ❌ Error: current-kid not found in config store"
  echo "     Make sure you've generated keys first using generate-signing-keys.sh"
  exit 1
fi

echo "     Current key ID: $CURRENT_KID"

echo ""
echo "  📋 Fetching existing active keys..."
ACTIVE_KIDS=$(fastly config-store-entry describe \
  --store-id="$CONFIG_STORE_ID" \
  --key="active-kids" \
  --json 2>/dev/null | jq -r '.item_value // empty') || ACTIVE_KIDS=""

if [[ -z "$ACTIVE_KIDS" ]]; then
  echo "     ❌ Error: active-kids not found in config store"
  exit 1
fi

echo "     Active keys (before): $ACTIVE_KIDS"

# Check if rotation is needed
if [[ "$ACTIVE_KIDS" == "$CURRENT_KID" ]]; then
  echo ""
  echo "✅ No rotation needed - only current key is active"
  echo "   Active keys: $ACTIVE_KIDS"
  exit 0
fi

# Calculate which keys will be removed
IFS=',' read -ra KEYS_ARRAY <<< "$ACTIVE_KIDS"
REMOVED_KEYS=()
for kid in "${KEYS_ARRAY[@]}"; do
  kid=$(echo "$kid" | xargs)  # trim whitespace
  if [[ "$kid" != "$CURRENT_KID" ]]; then
    REMOVED_KEYS+=("$kid")
  fi
done

echo ""
echo "  🗑️  Keys to be rotated out: ${REMOVED_KEYS[*]}"
echo "     These keys will no longer be able to verify signatures"
echo "     (Private keys remain in secret store for recovery)"

echo ""
echo "  📋 Updating active keys list in Config Store '$CONFIG_STORE_ID'..."
if fastly config-store-entry update \
  --store-id="$CONFIG_STORE_ID" \
  --key="active-kids" \
  --value="$CURRENT_KID" 2>/dev/null; then
  echo "     ✅ Active keys updated to: $CURRENT_KID"
elif fastly config-store-entry create \
  --store-id="$CONFIG_STORE_ID" \
  --key="active-kids" \
  --value="$CURRENT_KID" 2>/dev/null; then
  echo "     ✅ Active keys set to: $CURRENT_KID"
else
  echo "     ❌ Failed to update active keys (store not found?)"
  exit 1
fi

echo ""
echo "✨ Key rotation complete!"
echo ""
echo "Summary:"
echo "  Before: $ACTIVE_KIDS"
echo "  After:  $CURRENT_KID"
echo "  Removed: ${REMOVED_KEYS[*]}"
echo ""
echo "Note: Old signatures created with rotated keys can no longer be verified."
echo "      Private keys remain in secret store if you need to restore them."
echo ""
