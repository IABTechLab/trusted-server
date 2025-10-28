#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TEMP_DIR="$PROJECT_ROOT/.keys-temp"
OPENSSL_BIN="${OPENSSL_BIN:-openssl}"

SECRET_STORE_ID="${SECRET_STORE_ID:-signing_keys}"
CONFIG_STORE_ID="${CONFIG_STORE_ID:-jwks_store}"
# Use first argument if provided, otherwise use date-based KID
KID="${1:-ts-$(date +%Y-%m-%d)}"


echo "🔐 Generating Ed25519 keypair for JOSE signing..."
mkdir -p "$TEMP_DIR"
cd "$TEMP_DIR"

echo "  📝 Generating private key..."
$OPENSSL_BIN genpkey -algorithm ED25519 -out private_key.pem

echo "  📝 Extracting public key..."
$OPENSSL_BIN pkey -in private_key.pem -pubout -out public_key.pem

echo "  🔧 Extracting raw 32-byte private key..."
$OPENSSL_BIN pkey -in private_key.pem -outform DER | tail -c 32 > private_key.bin

echo "  🔧 Extracting raw 32-byte public key..."
$OPENSSL_BIN pkey -in public_key.pem -pubin -pubout -outform DER | tail -c 32 > public_key.bin

echo "  📦 Creating JWK for public key..."
PUBLIC_KEY_B64=$(base64 -i public_key.bin | tr -d '\n' | tr '+/' '-_' | tr -d '=')

cat > public_key.jwk <<EOF
{
  "kty": "OKP",
  "crv": "Ed25519",
  "kid": "$KID",
  "use": "sig",
  "x": "$PUBLIC_KEY_B64"
}
EOF

echo ""
echo "✅ Keys generated successfully!"
echo "  Key ID: $KID"
echo "  Private key size: $(wc -c < private_key.bin) bytes"
echo "  Public key size:  $(wc -c < public_key.bin) bytes"
echo ""

cat public_key.jwk | jq '.' 2>/dev/null || cat public_key.jwk
echo ""

echo "📤 Storing keys in Fastly..."
echo ""

echo "  🔒 Storing private key in Secret Store '$SECRET_STORE_ID'..."
if fastly secret-store-entry update \
  --store-id="$SECRET_STORE_ID" \
  --name="$KID" \
  --file=private_key.bin 2>/dev/null; then
  echo "     ✅ Private key updated"
elif fastly secret-store-entry create \
  --store-id="$SECRET_STORE_ID" \
  --name="$KID" \
  --file=private_key.bin 2>/dev/null; then
  echo "     ✅ Private key created"
else
  echo "     ⚠️  Failed to store private key (store not found?)"
  echo "     Try: fastly secret-store create --name=$SECRET_STORE_ID"
fi

echo ""
echo "  📋 Storing public key JWK in Config Store '$CONFIG_STORE_ID'..."
if fastly config-store-entry update \
  --store-id="$CONFIG_STORE_ID" \
  --key="$KID" \
  --value="$(cat public_key.jwk)" 2>/dev/null; then
  echo "     ✅ Public key JWK updated"
elif fastly config-store-entry create \
  --store-id="$CONFIG_STORE_ID" \
  --key="$KID" \
  --value="$(cat public_key.jwk)" 2>/dev/null; then
  echo "     ✅ Public key JWK created"
else
  echo "     ⚠️  Failed to store public key (store not found?)"
  echo "     Try: fastly config-store create --name=$CONFIG_STORE_ID"
fi

echo ""
echo "  📋 Retrieving previous current key ID..."
PREVIOUS_KID=$(fastly config-store-entry describe \
  --store-id="$CONFIG_STORE_ID" \
  --key="current-kid" \
  --json 2>/dev/null | jq -r '.item_value // empty') || PREVIOUS_KID=""

if [[ -n "$PREVIOUS_KID" ]]; then
  echo "     Previous key ID: $PREVIOUS_KID"
else
  echo "     No previous key ID found (first key)"
fi

# Build active-kids list: previous + current (if previous exists and different)
if [[ -n "$PREVIOUS_KID" && "$PREVIOUS_KID" != "$KID" ]]; then
  ACTIVE_KIDS="$PREVIOUS_KID,$KID"
else
  # First key or re-running for same key
  ACTIVE_KIDS="$KID"
fi

echo ""
echo "  📋 Setting current active key ID in Config Store '$CONFIG_STORE_ID'..."
if fastly config-store-entry update \
  --store-id="$CONFIG_STORE_ID" \
  --key="current-kid" \
  --value="$KID" 2>/dev/null; then
  echo "     ✅ Current key ID updated to $KID"
elif fastly config-store-entry create \
  --store-id="$CONFIG_STORE_ID" \
  --key="current-kid" \
  --value="$KID" 2>/dev/null; then
  echo "     ✅ Current key ID set to $KID"
else
  echo "     ⚠️  Failed to set current key ID (store not found?)"
fi

echo ""
echo "  📋 Updating active keys list in Config Store '$CONFIG_STORE_ID'..."
if fastly config-store-entry update \
  --store-id="$CONFIG_STORE_ID" \
  --key="active-kids" \
  --value="$ACTIVE_KIDS" 2>/dev/null; then
  echo "     ✅ Active keys updated to: $ACTIVE_KIDS"
elif fastly config-store-entry create \
  --store-id="$CONFIG_STORE_ID" \
  --key="active-kids" \
  --value="$ACTIVE_KIDS" 2>/dev/null; then
  echo "     ✅ Active keys set to: $ACTIVE_KIDS"
else
  echo "     ⚠️  Failed to set active keys (store not found?)"
fi

echo ""
echo "🧹 Cleaning up temporary files..."
cd "$PROJECT_ROOT"
rm -rf "$TEMP_DIR"

echo ""
echo "✨ Done! Your signing keys are ready."
echo ""
echo "Key rotation summary:"
echo "  Current key ID: $KID"
echo "  Active keys: $ACTIVE_KIDS"
echo ""
echo "Next steps:"
echo "  1. Update your Fastly service to use these stores"
echo "  2. Verify the JWK at runtime matches the generated key ID"
echo ""

