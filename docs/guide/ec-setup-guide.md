# Edge Cookie Setup Guide

End-to-end setup and verification guide for Edge Cookie (EC) identity flows.

This guide covers:

1. Fastly store setup
2. Partner registration
3. Browser pixel sync (`/_ts/api/v1/sync`)
4. Server-to-server batch sync (`/_ts/api/v1/batch-sync`)
5. Identity verification (`/_ts/api/v1/identify`)
6. Auction bidstream verification (`/auction`)

## Prerequisites

- Trusted Server deployed and reachable (example: `https://getpurpose.ai`)
- Admin credentials for `/_ts/admin/v1/partners/register`
- Fastly CLI authenticated (for store verification)
- A valid TCF consent string (`euconsent-v2`) for consent-required requests

## 1) Required Configuration

Set EC configuration in `trusted-server.toml`:

```toml
[ec]
passphrase = "your-secure-hmac-secret"
ec_store = "ec_identity_store"
partner_store = "ec_partner_store"
```

Required behavior assumptions:

- `ec_store` and `partner_store` are linked to the active Fastly service version
- Partner has `bidstream_enabled = true` if you want `user.ext.eids` in bidstream

## 2) Configure Demo Variables

```bash
TS_BASE_URL="https://getpurpose.ai"
MOCK_SSP_URL="https://formally-vital-lion.edgecompute.app"

ADMIN_USER="admin"
ADMIN_PASSWORD="<admin-password>"

PARTNER_ID="mocktioneer"
PARTNER_NAME="Mocktioneer SSP"
PARTNER_API_KEY="test-batch-sync-key-2026"

# Optional: use a real browser EC if already present
EC_ID="<64hex.6chars>"

TCF_CONSENT="<euconsent-v2-string>"
PARTNER_UID="mock-user-$(date +%s)"
```

## 3) Register Partner

Endpoint: `POST /_ts/admin/v1/partners/register`

```bash
curl -X POST "${TS_BASE_URL}/_ts/admin/v1/partners/register" \
  -u "${ADMIN_USER}:${ADMIN_PASSWORD}" \
  -H "Content-Type: application/json" \
  -d "{
    \"id\": \"${PARTNER_ID}\",
    \"name\": \"${PARTNER_NAME}\",
    \"api_key\": \"${PARTNER_API_KEY}\",
    \"allowed_return_domains\": [\"${MOCK_SSP_URL#https://}\"],
    \"source_domain\": \"${MOCK_SSP_URL#https://}\",
    \"bidstream_enabled\": true
  }"
```

Expected:

- `201` for new partner
- `200` for update of existing partner

## 4) Acquire or Reuse EC Cookie

If you already have an EC from browser traffic, reuse it.

Otherwise, attempt generation with consent:

```bash
curl -si "${TS_BASE_URL}/" \
  -H "Cookie: euconsent-v2=${TCF_CONSENT}"
```

Look for:

- `Set-Cookie: ts-ec=<64hex.6chars>`

## 5) Pixel Sync (Browser-style)

Endpoint: `GET /_ts/api/v1/sync`

```bash
curl -si "${TS_BASE_URL}/_ts/api/v1/sync?partner=${PARTNER_ID}&uid=${PARTNER_UID}&return=${MOCK_SSP_URL}/done" \
  -H "Cookie: ts-ec=${EC_ID}; euconsent-v2=${TCF_CONSENT}"
```

Expected redirect result:

- Success: `Location: ...?ts_synced=1`
- Failure: `Location: ...?ts_synced=0&ts_reason=<reason>`

Common `ts_reason` values:

- `no_ec`
- `no_consent`
- `write_failed`
- `rate_limited`

## 6) Verify Identity

Endpoint: `GET /_ts/api/v1/identify`

```bash
curl -s "${TS_BASE_URL}/_ts/api/v1/identify" \
  -H "Cookie: ts-ec=${EC_ID}; euconsent-v2=${TCF_CONSENT}" | python3 -m json.tool
```

Expected shape:

```json
{
  "ec": "<ec-id>",
  "consent": "ok",
  "degraded": false,
  "uids": {
    "mocktioneer": "mock-user-123"
  },
  "eids": [
    {
      "source": "formally-vital-lion.edgecompute.app",
      "uids": [{ "id": "mock-user-123", "atype": 3 }]
    }
  ],
  "cluster_size": 12
}
```

## 7) Batch Sync (S2S)

Endpoint: `POST /_ts/api/v1/batch-sync`

Important: request field is `ec_id` (full `{64hex}.{6alnum}` value).

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

## 8) Verify Auction Bidstream Enrichment

Endpoint: `POST /auction`

```bash
curl -si -X POST "${TS_BASE_URL}/auction" \
  -H "Cookie: ts-ec=${EC_ID}; euconsent-v2=${TCF_CONSENT}" \
  -H "Content-Type: application/json" \
  -d '{"adUnits":[{"code":"test","mediaTypes":{"banner":{"sizes":[[300,250]]}}}]}'
```

Check response headers:

- `x-ts-ec`
- `x-ts-ec-consent`
- `x-ts-eids`
- `Set-Cookie: ts-ec=...`

Decode `x-ts-eids`:

```bash
echo "<x-ts-eids-base64-value>" | base64 -d | python3 -m json.tool
```

Expected decoded payload contains:

- `source = formally-vital-lion.edgecompute.app`
- `uids[0].id = <partner-uid>`

## 9) Fastly KV Operational Checks

List stores:

```bash
fastly kv-store list
```

Check service resource links for active version:

```bash
fastly resource-link list --service-id <service-id> --version <active-version>
```

Inspect partner entry:

```bash
fastly kv-store-entry get --store-id <partner-store-id> --key "${PARTNER_ID}"
```

Inspect EC identity entry:

```bash
fastly kv-store-entry get --store-id <identity-store-id> --key "${EC_ID}"
```

If pixel sync returns `write_failed`, check whether KV entry has:

- `consent.ok = false`

## 10) Troubleshooting Quick Map

| Symptom                                               | Likely Cause                           | Check                                      |
| ----------------------------------------------------- | -------------------------------------- | ------------------------------------------ |
| `invalid_token` on batch sync                         | Wrong partner API key                  | Re-register partner with known API key     |
| `missing field ec_id`                                 | Wrong request schema                   | Use `ec_id` field                          |
| `ts_reason=no_consent`                                | Missing/invalid consent cookie         | Include valid `euconsent-v2`               |
| `ts_reason=write_failed`                              | KV write blocked (often consent state) | Inspect identity KV entry and store links  |
| `/_ts/api/v1/identify` returns `{"consent":"denied"}` | No consent for current request         | Send consent cookie                        |
| No `uids` in `/_ts/api/v1/identify`                   | No successful sync yet                 | Run `/_ts/api/v1/sync` or batch sync first |

See also: [Edge Cookies](/guide/edge-cookies), [Configuration](/guide/configuration), [API Reference](/guide/api-reference)
