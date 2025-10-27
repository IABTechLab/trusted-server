#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TEMP_DIR="$PROJECT_ROOT/.keys-temp"

SECRET_STORE_NAME="${SECRET_STORE_NAME:-signing_keys}"
CONFIG_STORE_NAME="${CONFIG_STORE_NAME:-jwks_store}"
KEY_ID="${KEY_ID:-$(date +%s)}"
LOCAL_ONLY="${LOCAL_ONLY:-false}"

echo "🔐 Generating Ed25519 keypair for JOSE signing..."

mkdir -p "$TEMP_DIR"
cd "$TEMP_DIR"

echo "  📝 Generating private key..."
openssl genpkey -algorithm ED25519 -out private_key.pem

echo "  📝 Extracting public key..."
openssl pkey -in private_key.pem -pubout -out public_key.pem

echo "  🔧 Extracting raw 32-byte private key..."
openssl pkey -in private_key.pem -outform DER | tail -c 32 > private_key.bin

echo "  🔧 Extracting raw 32-byte public key..."
openssl pkey -in public_key.pem -pubin -pubout -outform DER | tail -c 32 > public_key.bin

echo "  📦 Creating JWK for public key..."
PUBLIC_KEY_B64=$(base64 -i public_key.bin | tr -d '\n' | tr '+/' '-_' | tr -d '=')

cat > public_key.jwk <<EOF
{
  "kty": "OKP",
  "crv": "Ed25519",
  "kid": "$KEY_ID",
  "use": "sig",
  "x": "$PUBLIC_KEY_B64"
}
EOF

echo ""
echo "✅ Keys generated successfully!"
echo ""
echo "📋 Key Information:"
echo "  Key ID: $KEY_ID"
echo "  Private key size: $(wc -c < private_key.bin) bytes"
echo "  Public key size: $(wc -c < public_key.bin) bytes"
echo ""

cat public_key.jwk | jq '.' 2>/dev/null || cat public_key.jwk

echo ""

if [ "$LOCAL_ONLY" = "true" ]; then
  echo "📋 Local Development Output:"
  echo ""
  
  # Create JSON files for stores
  SECRET_STORE_DIR="$PROJECT_ROOT/crates/fastly/tests/secret_store"
  CONFIG_STORE_DIR="$PROJECT_ROOT/crates/fastly/tests/config_store"
  
  mkdir -p "$SECRET_STORE_DIR"
  mkdir -p "$CONFIG_STORE_DIR"
  
  # Create secret store JSON
  cat > "$SECRET_STORE_DIR/$SECRET_STORE_NAME.json" <<EOF
{
    "ed25519_private_key": "$(base64 -i private_key.bin | tr -d '\n')"
}
EOF
  
  # Create config store JSON
  PUBLIC_KEY_JWK_ESCAPED=$(cat public_key.jwk | jq -c '.')
  cat > "$CONFIG_STORE_DIR/$CONFIG_STORE_NAME.json" <<EOF
{
    "ed25519_public_key": $PUBLIC_KEY_JWK_ESCAPED,
    "ed25519_key_id": "$KEY_ID"
}
EOF
  
  echo "✅ Created store files:"
  echo "   $SECRET_STORE_DIR/$SECRET_STORE_NAME.json"
  echo "   $CONFIG_STORE_DIR/$CONFIG_STORE_NAME.json"
  echo ""
  echo "📋 Add these to your fastly.toml:"
  echo ""
  echo "[local_server.secret_stores.$SECRET_STORE_NAME]"
  echo "    file = 'crates/fastly/tests/secret_store/$SECRET_STORE_NAME.json'"
  echo "    format = 'json'"
  echo ""
  echo "[local_server.config_stores.$CONFIG_STORE_NAME]"
  echo "    file = 'crates/fastly/tests/config_store/$CONFIG_STORE_NAME.json'"
  echo "    format = 'json'"
  echo ""
  echo "🧹 Cleaning up temporary files..."
  cd "$PROJECT_ROOT"
  rm -rf "$TEMP_DIR"
  echo ""
  echo "✨ Done! Store files created and ready to use."
else
  echo "📤 Storing keys in Fastly..."
  echo ""

  echo "  🔒 Storing private key in Secret Store '$SECRET_STORE_NAME'..."
  if fastly secret-store-entry create \
    --store-id="$SECRET_STORE_NAME" \
    --name=ed25519_private_key \
    --file=private_key.bin 2>/dev/null; then
    echo "     ✅ Private key stored"
  else
    echo "     ⚠️  Failed to store private key (may already exist or store not found)"
    echo "     Try: fastly secret-store create --name=$SECRET_STORE_NAME"
  fi

  echo ""
  echo "  📋 Storing public key JWK in Config Store '$CONFIG_STORE_NAME'..."
  if fastly config-store-entry create \
    --store-id="$CONFIG_STORE_NAME" \
    --key=ed25519_public_key \
    --value="$(cat public_key.jwk)" 2>/dev/null; then
    echo "     ✅ Public key JWK stored"
  else
    echo "     ⚠️  Failed to store public key (may already exist or store not found)"
    echo "     Try: fastly config-store create --name=$CONFIG_STORE_NAME"
  fi

  echo ""
  echo "  📋 Storing key ID in Config Store '$CONFIG_STORE_NAME'..."
  if fastly config-store-entry create \
    --store-id="$CONFIG_STORE_NAME" \
    --key=ed25519_key_id \
    --value="$KEY_ID" 2>/dev/null; then
    echo "     ✅ Key ID stored"
  else
    echo "     ⚠️  Failed to store key ID (may already exist or store not found)"
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
  echo "  2. For local development, run:"
  echo "     LOCAL_ONLY=true ./scripts/generate-signing-keys.sh"
  echo ""
fi
