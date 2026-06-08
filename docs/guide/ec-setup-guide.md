# Edge Cookie Setup Guide

End-to-end setup and verification guide for Edge Cookie (EC) identity flows.

This guide covers:

1. Fastly store setup
2. Partner configuration
3. Server-to-server batch sync (`/_ts/api/v1/batch-sync`)
4. Identity verification (`/_ts/api/v1/identify`)
5. Auction bidstream verification (`/auction`)

## Prerequisites

- Trusted Server deployed and reachable (example: `https://getpurpose.ai`)
- Access to update `trusted-server.toml` / deployment configuration
- Fastly CLI authenticated (for store verification)
- A valid TCF consent string (`euconsent-v2`) for consent-required requests

## 1) Required Configuration

Set EC configuration in `trusted-server.toml`:

```toml
[ec]
passphrase = "replace-with-32-plus-byte-random-secret"
ec_store = "ec_identity_store"

[[ec.partners]]
name = "Mocktioneer SSP"
source_domain = "formally-vital-lion.edgecompute.app"
api_token = "test-batch-sync-key-2026"
bidstream_enabled = true
```

Required behavior assumptions:

- `passphrase` is long-lived HMAC-SHA256 keying material for EC ID derivation; use a high-entropy random value of at least 32 characters
- `ec_store` is linked to the active Fastly service version
- `ec_store` is the only KV-backed EC lifecycle store; it contains identity graph state, minimal consent metadata, source-domain keyed partner UIDs, and withdrawal tombstones
- Live consent is interpreted from request cookies, headers, geolocation, and policy defaults rather than a separate consent KV store
- Partners are configured statically in `[[ec.partners]]` and loaded into an in-memory registry at startup
- `source_domain` is the canonical key used for stored IDs and controls EID source matching during ingestion
- Partner has `bidstream_enabled = true` if you want `user.ext.eids` in bidstream

## 2) Configure Demo Variables

```bash
TS_BASE_URL="https://getpurpose.ai"
MOCK_SSP_URL="https://formally-vital-lion.edgecompute.app"

PARTNER_SOURCE_DOMAIN="formally-vital-lion.edgecompute.app"
PARTNER_NAME="Mocktioneer SSP"
PARTNER_API_KEY="test-batch-sync-key-2026"

# Optional: use a real browser EC if already present
EC_ID="<64hex.6chars>"

TCF_CONSENT="<euconsent-v2-string>"
PARTNER_UID="mock-user-$(date +%s)"
```

## 3) Configure Partner

Partners are configured in `trusted-server.toml` and loaded at startup:

```toml
[[ec.partners]]
name = "Mocktioneer SSP"
source_domain = "formally-vital-lion.edgecompute.app"
api_token = "test-batch-sync-key-2026"
bidstream_enabled = true
```

Deploy/restart after changing partner configuration.

## 4) Acquire or Reuse EC Cookie

If you already have an EC from browser traffic, reuse it.

Otherwise, attempt generation with consent:

```bash
curl -si "${TS_BASE_URL}/" \
  -H "Cookie: euconsent-v2=${TCF_CONSENT}"
```

Look for:

- `Set-Cookie: ts-ec=<64hex.6chars>`

## 5) Batch Sync (S2S)

Endpoint: `POST /_ts/api/v1/batch-sync`

Important: request field is `ec_id` (full `{64hex}.{6alnum}` value). The `timestamp` field remains required for API compatibility, but it no longer orders writes because EC identity entries do not store per-partner sync timestamps. Valid mappings are idempotent last-write-wins: unchanged UIDs are accepted without a write, and different UIDs replace the stored value.

```bash
BATCH_UID="${PARTNER_UID}-batch"
NOW_TS="$(date +%s)"

curl -X POST "${TS_BASE_URL}/_ts/api/v1/batch-sync" \
  -H "Authorization: Bearer ${PARTNER_API_KEY}" \
  -H "Content-Type: application/json" \
  -d "{
    \"mappings\": [{
      \"ec_id\": \"${EC_ID}\",
      \"partner_uid\": \"${BATCH_UID}\",
      \"timestamp\": ${NOW_TS}
    }]
  }" | python3 -m json.tool
```

Expected:

```json
{
  "accepted": 1,
  "rejected": 0,
  "errors": []
}
```

## 6) Verify Identity

Endpoint: `GET /_ts/api/v1/identify`

```bash
curl -s "${TS_BASE_URL}/_ts/api/v1/identify" \
  -H "Authorization: Bearer ${PARTNER_API_KEY}" \
  -H "Cookie: ts-ec=${EC_ID}; euconsent-v2=${TCF_CONSENT}" | python3 -m json.tool
```

Expected shape:

```json
{
  "ec": "<ec-id>",
  "consent": "ok",
  "degraded": false,
  "source_domain": "formally-vital-lion.edgecompute.app",
  "uid": "mock-user-123",
  "eid": {
    "source": "formally-vital-lion.edgecompute.app",
    "uids": [{ "id": "mock-user-123", "atype": 3 }]
  },
  "cluster_size": 12
}
```

## 7) Verify Auction Bidstream Enrichment

Endpoint: `POST /auction`

```bash
curl -si -X POST "${TS_BASE_URL}/auction" \
  -H "Cookie: ts-ec=${EC_ID}; euconsent-v2=${TCF_CONSENT}" \
  -H "Content-Type: application/json" \
  -d '{"adUnits":[{"code":"test","mediaTypes":{"banner":{"sizes":[[300,250]]}}}]}'
```

Check response headers:

- `x-ts-ec-consent`
- `x-ts-eids`

For returning users, ordinary page views should not refresh `Set-Cookie: ts-ec=...`. A `Set-Cookie` header is expected when the EC is newly generated.

Decode `x-ts-eids`:

```bash
echo "<x-ts-eids-base64-value>" | base64 -d | python3 -m json.tool
```

Expected decoded payload contains:

- `source = formally-vital-lion.edgecompute.app`
- `uids[0].id = <partner-uid>`

## 8) Fastly KV Operational Checks

List stores:

```bash
fastly kv-store list
```

Check service resource links for active version:

```bash
fastly resource-link list --service-id <service-id> --version <active-version>
```

Inspect EC identity entry:

```bash
fastly kv-store-entry get --store-id <identity-store-id> --key "${EC_ID}"
```

If batch sync returns `ineligible`, check whether the KV entry is missing or has `consent.ok = false` from a withdrawal tombstone.

## 9) Troubleshooting Quick Map

| Symptom                                               | Likely Cause                   | Check                                                                       |
| ----------------------------------------------------- | ------------------------------ | --------------------------------------------------------------------------- |
| `invalid_token` on batch sync                         | Wrong partner API key          | Re-register partner with known API key                                      |
| `missing field ec_id`                                 | Wrong request schema           | Use `ec_id` field                                                           |
| `/_ts/api/v1/identify` returns `{"consent":"denied"}` | No consent for current request | Send consent cookie                                                         |
| No `uid` in `/_ts/api/v1/identify`                    | No successful sync yet         | Run batch sync or ensure Prebid EID ingestion has populated the partner UID |

See also: [Edge Cookies](/guide/edge-cookies), [Configuration](/guide/configuration), [API Reference](/guide/api-reference)
