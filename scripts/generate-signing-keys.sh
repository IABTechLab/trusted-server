#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TEMP_DIR="$PROJECT_ROOT/.keys-temp"

SECRET_STORE_ID="${SECRET_STORE_ID:-signing_keys}"
CONFIG_STORE_ID="${CONFIG_STORE_ID:-jwks_store}"
KEY_NAME="${KEY_NAME:-ts-2025-10-A}"

OPENSSL_BIN="/opt/homebrew/opt/openssl@3.6/bin/openssl"

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
  "kid": "$KEY_NAME",
  "use": "sig",
  "x": "$PUBLIC_KEY_B64"
}
EOF

echo ""
echo "✅ Keys generated successfully!"
echo "  Key Name: $KEY_NAME"
echo "  Private key size: $(wc -c < private_key.bin) bytes"
echo "  Public key size:  $(wc -c < public_key.bin) bytes"
echo ""

cat public_key.jwk | jq '.' 2>/dev/null || cat public_key.jwk
echo ""

echo "📤 Storing keys in Fastly..."
echo ""

echo "  🔒 Storing private key in Secret Store '$SECRET_STORE_ID'..."
if fastly secret-store-entry create \
  --store-id="$SECRET_STORE_ID" \
  --name="$KEY_NAME" \
  --file=private_key.bin 2>/dev/null; then
  echo "     ✅ Private key stored"
else
  echo "     ⚠️  Failed to store private key (may already exist or store not found)"
  echo "     Try: fastly secret-store create --name=$SECRET_STORE_ID"
fi

echo ""
echo "  📋 Storing public key JWK in Config Store '$CONFIG_STORE_ID'..."
if fastly config-store-entry create \
  --store-id="$CONFIG_STORE_ID" \
  --key="$KEY_NAME" \
  --value="$(cat public_key.jwk)" 2>/dev/null; then
  echo "     ✅ Public key JWK stored"
else
  echo "     ⚠️  Failed to store public key (may already exist or store not found)"
  echo "     Try: fastly config-store create --name=$CONFIG_STORE_ID"
fi

echo ""
echo "  📋 Storing current key ID in Config Store '$CONFIG_STORE_ID'..."
if fastly config-store-entry create \
  --store-id="$CONFIG_STORE_ID" \
  --key="current-kid" \
  --value="$KEY_NAME" 2>/dev/null; then
  echo "     ✅ Current key ID stored"
else
  echo "     ⚠️  Failed to store current key ID (may already exist or store not found)"
fi

echo ""
echo "🧹 Cleaning up temporary files..."
cd "$PROJECT_ROOT"
rm -rf "$TEMP_DIR"

echo ""
echo "✨ Done! Your signing keys are ready."
echo ""
echo "Next steps:"
echo "  1. Update your Fastly service to use these stores"
echo "  2. Verify the JWK at runtime matches the generated key ID"
echo ""

