# Identity Vendors

Trusted Server ships server-side [identity providers](/guide/identity-providers) for six vendors. Each one is an integration under `[integrations.<id>]` that, once enabled, resolves or captures IDs at the edge and writes them to the Edge Cookie (EC) identity graph in KV. You don't need the vendor's browser module for this.

| Integration  | Vendor          | Kind    | Sources                                             | Needs                                 |
| ------------ | --------------- | ------- | --------------------------------------------------- | ------------------------------------- |
| `id5`        | ID5             | Resolve | `id5-sync.com`, `true-link-id5-sync.com`, `euid.eu` | Nothing (hashed email improves match) |
| `liveintent` | LiveIntent      | Resolve | One per configured attribute (see below)            | Nothing (`_lc2_fpi` improves match)   |
| `liveramp`   | LiveRamp ATS    | Resolve | `liveramp.com`                                      | Hashed email for a first envelope     |
| `uid2`       | UID2            | Resolve | `uidapi.com`                                        | Hashed email for a first token        |
| `connectid`  | Yahoo ConnectID | Resolve | `yahoo.com`                                         | Hashed email                          |
| `lockr`      | Lockr (capture) | Capture | `id5-sync.com`, `liveramp.com`, `uidapi.com`        | Lockr's proxied API                   |

Criteo and The Trade Desk are not included. Their IDs live in the vendor's own third-party cookie, which Trusted Server cannot read at the publisher's edge. Browser ingest still carries those IDs from `ts-eids` when they are registered partners.

## Before you enable one

Every source a provider writes needs an `[[ec.partners]]` entry, or `ts config validate` refuses the config. When a provider declares more sources than you want, narrow it:

```toml
[[ec.partners]]
name = "ID5"
source_domain = "id5-sync.com"
openrtb_atype = 1
bidstream_enabled = true

[ec.identity.providers.id5]
sources = ["id5-sync.com"]
```

Provider timing, rate limits and ownership use the keys in the [Configuration Reference](/guide/configuration#identity-providers-ec-identity).

## Hashed email

LiveRamp, UID2 and ConnectID need a hashed email, and ID5 sends one when it is present. The page supplies it after the visitor signs in. The JS SDK normalizes and hashes the address in the browser and stores only the SHA-256 in the first-party `ts-hem` cookie:

```js
window.tsjs.que.push(async () => {
  await window.tsjs.setEmail('person@example.com')
})

// Or, when the page already has the digest:
window.tsjs.setHashedEmail('<64 hex digits>')

// On sign-out:
window.tsjs.clearHashedEmail()
```

Normalization trims and lowercases the address. For Gmail it also drops dots and any `+suffix` from the local part. The cookie lasts 30 days.

Trusted Server reads `ts-hem` on each request and passes it only to providers that declare the hashed-email input. It never stores the digest in KV, and it strips the cookie before forwarding a request to the origin or to Prebid Server. A hashed email is personal data: set it only under the legal basis your consent platform records.

## ID5

```toml
[integrations.id5]
enabled = true
partner_id = 1234
# endpoint = "https://id5-sync.com/g/v2"
```

ID5 needs vendor consent for GVL ID 131 where TCF applies, and consent for the purposes its GVL entry lists: 1, 3, 5 and 10. Trusted Server keeps the signature ID5 returns as private state and sends it on the next call. ID5's answer `0` (the visitor opted out at ID5) removes the stored ID. Confirm with ID5 the server-to-server field names for the client IP (`ip`) and user agent (`ua`) on your partner account.

## LiveIntent

```toml
[integrations.liveintent]
enabled = true
publisher_id = "12345"
# distributor_id = "did-0000"
resolve = ["nonId", "thetradedesk"]
```

`resolve` lists the attributes to ask for. Each attribute maps to one source:

| Attribute      | Source                         |
| -------------- | ------------------------------ |
| `nonId`        | `liveintent.com`               |
| `bidswitch`    | `bidswitch.net`                |
| `medianet`     | `media.net`                    |
| `magnite`      | `rubiconproject.com`           |
| `index`        | `liveintent.indexexchange.com` |
| `openx`        | `openx.net`                    |
| `pubmatic`     | `pubmatic.com`                 |
| `sovrn`        | `liveintent.sovrn.com`         |
| `sharethrough` | `sharethrough.com`             |
| `sonobi`       | `liveintent.sonobi.com`        |
| `triplelift`   | `liveintent.triplelift.com`    |
| `zetassp`      | `zeta-ssp.liveintent.com`      |
| `vidazoo`      | `liveintent.vidazoo.com`       |
| `nexxen`       | `liveintent.unrulymedia.com`   |
| `uid2`         | `uidapi.com`                   |
| `thetradedesk` | `adserver.org`                 |

Only the configured attributes are declared, so only their sources need partner entries. LiveIntent needs vendor consent for GVL ID 148 and consent for Purposes 1, 2 and 7, as its GVL entry lists. When LiveIntent's first-party `_lc2_fpi` cookie is on the request, it is sent as `duid`.

## LiveRamp ATS

```toml
[integrations.liveramp]
enabled = true
placement_id = "placement-1"
approved_countries = ["US"]
# endpoint = "https://api.rlcdn.com/api/identity/v2/envelope"
```

A first envelope needs the hashed email. A refresh sends the stored envelope instead, so it works on later visits without one. Envelopes expire after 15 days in California and 30 days elsewhere. Calls run only in `approved_countries`, and need vendor consent for GVL ID 97 and consent for Purposes 1 to 10, as its GVL entry lists. The response shape and consent parameter names follow the design spec's assumptions; confirm them with LiveRamp.

## UID2

```toml
[integrations.uid2]
enabled = true
# base_url = "https://prod.uidapi.com"
api_key = "uid2_api_key"
client_secret = "uid2_client_secret"
# tcf_vendor_id = 21
```

`api_key` and `client_secret` are secret-store key names; `ts config push` resolves them. The client secret is the base64 key that encrypts token requests. A first token needs the hashed email. Trusted Server keeps the refresh token as private state and refreshes before the token expires. UID2 has no GVL ID: where TCF applies it needs Purposes 1 and 4, and vendor consent only when `tcf_vendor_id` names your UID2 operator's GVL ID. A UID2 `optout` opts the visitor out of UID2 until they consent again. UID2 is not meant for the EU (EUID covers it), so set `tcf_vendor_id` only when your UID2 agreement names one.

## Yahoo ConnectID

```toml
[integrations.connectid]
enabled = true
pixel_id = 12345
# endpoint = "https://ups.analytics.yahoo.com/ups"
```

ConnectID needs the hashed email, vendor consent for GVL ID 25 and consent for Purposes 1 to 11, as Yahoo's GVL entry lists. Yahoo's `ttl`, in hours, sets when the ID is refreshed.

## Lockr capture

```toml
[integrations.lockr]
enabled = true
app_id = "app-1"
capture_identity = true

[ec.identity.providers.lockr]
sources = ["id5-sync.com"]
```

With `capture_identity`, Lockr becomes a capture-only provider on the Lockr API calls Trusted Server already proxies. Capture is revoke-only: a `revoke-consent` call removes the IDs of the listed sources and blocks the browser's values for them until Lockr issues again. Lockr's IDs themselves reach KV through browser ingest. A capture-only provider must list its `sources`.
