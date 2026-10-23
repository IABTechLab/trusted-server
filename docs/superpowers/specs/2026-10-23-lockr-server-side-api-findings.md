# Lockr server-side API: findings

Status: Investigation, revision 1. No provider spec follows, because no
server-to-server Lockr API could be confirmed (section 2).

Issue: part of [#1246](https://github.com/IABTechLab/trusted-server/issues/1246).
Framework: [Server-side identity providers](2026-10-20-server-side-identity-providers-design.md),
revision 12 (PR #1428). Cross-references written "(framework section 12.2)"
point to that spec. Cross-references written "(section 3.2)" point to this
document.

Every `file:line` reference to Trusted Server (TS) code is to `c84a73da7`,
the commit the framework is designed against. Core paths are written without
the `crates/trusted-server-core/src/` prefix. Prebid.js references are to
version 10.26.0, read from
`crates/trusted-server-js/lib/node_modules/prebid.js/modules/`. Web sources
were read on 2026-10-23.

Terms are the framework's. Three more:

- **SDK**: Lockr's browser script, `identity-lockr-trust-server.js`, which TS
  serves from `/integrations/lockr/sdk`.
- **appID**: the publisher's Lockr application ID, the `app_id` setting of
  `[integrations.lockr]` (`integrations/lockr.rs:40-42`).
- **Mapping token**: the `lockrMappingToken` value Lockr's v1 token API
  returns, which Prebid's Lockr module sends back to refresh (section 3.2).

## 1. Question

TS gets Lockr IDs one way today. The SDK calls Lockr's API through TS's
proxy, and TS reads IDs from the responses (framework section 12.2). Nothing
calls Lockr unless the SDK runs. A visitor whose browser blocks the SDK
makes no proxied call, so TS captures nothing and the visitor gets no Lockr
ID.

The framework lists Lockr among the providers it helps "whose traffic TS
already proxies" (framework section 1, P1). That help needs the SDK to run.

Capture also sees a token only once. Lockr returns a token when it first
issues one, and on a reload the SDK sends the saved ID with `noGenerate`, so
the response carries no new token (#1246, framework section 12.2). An owner
refreshes a stale value only through `resolves` (framework sections 4.7 step
4, 7.2). A capture-only provider cannot.

The question: can Lockr issue or refresh IDs server-to-server, without the
SDK? If so, with which inputs: a hashed email, the client IP and user agent,
or an existing token?

## 2. Answer

**No server-to-server Lockr API for issuing or refreshing IDs could be
confirmed.** Three findings support this:

1. Lockr's public material says Lockr calls identity partners server to
   server from its own backend. It does not offer that call to publishers
   (section 3.1).
2. The only token API with a readable contract is the one Prebid's Lockr
   module calls from the browser. It takes the appID and a raw email, and
   refreshes with a mapping token (section 3.2). Nothing published says a
   server may call it, or how a server would authenticate.
3. Lockr's publisher page links an API reference. On 2026-10-23 that link
   returns 404, and no archive holds the specification file it loaded
   (section 3.1).

So this document writes no provider spec. Lockr stays capture-only
(framework section 12.2). Section 5 lists the questions for Lockr's account
team, and section 6 recommends a fallback.

## 3. Facts

Each fact cites its source. Anything not sourced is marked "assumed" and
appears in section 9.

### 3.1 What Lockr publishes

| Fact                                                                                                                                                                                                                                                                                                                    | Source                                                                                                                                                                                                                                                                     |
| ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| "lockr calls ID partners via a server-to-server integration, which returns IDs 2x faster than client-side integrations." This describes Lockr's backend calling its ID partners. It is not an API a publisher's server calls.                                                                                           | [lockrKeys press release](https://www.prnewswire.com/news-releases/lockr-launches-lockrkeys-to-facilitate-publisher-adoption-of-emerging-identity-solutions-302270591.html), 2024-10-09                                                                                    |
| "With lockrKeys, publishers can directly activate alternative IDs such as LiveRamp's RampID via the Authenticated Traffic Solution, Unified ID 2.0, ID5's ID, and more." Publishers "can also maintain direct contractual partnerships with identity providers in AIM".                                                 | same release                                                                                                                                                                                                                                                               |
| AIM is described as using "the benefits of server-side calls".                                                                                                                                                                                                                                                          | `lockrAIMIdSystem.md:4`; [AIM launch release](https://www.prnewswire.com/news-releases/lockr-launches-alternative-identity-manager-aim-for-publishers-to-free-up-engineering-resources-and-meet-advertisers-demand-in-a-cookieless-environment-302043972.html), 2024-01-25 |
| A publisher's AIM tag can share first-party data "with any identity partner or data endpoint … on a server-to-server basis".                                                                                                                                                                                            | [AdExchanger article, reposted by Lockr](https://loc.kr/with-lockrs-new-tool-publishers-can-test-multiple-alt-identity-solutions-at-once/), 2024-01-25                                                                                                                     |
| Each identity provider "can be individually set to restrict from client-side delivery and instead be retained as an authentication event within Identity lockr", for sharing to "any Data Endpoint". That describes server-side retention, not an issuance API.                                                         | `lockrAIMIdSystem.md:142`; [Prebid docs](https://docs.prebid.org/dev-docs/modules/userid-submodules/lockraim.html)                                                                                                                                                         |
| Lockr's publisher page links "Identity lockr APIs" at `https://aim.loc.kr/api-doc/lockr_reference.html`. That URL returns 404 (`NoSuchKey`).                                                                                                                                                                            | [loc.kr/for-publishers](https://loc.kr/for-publishers/); direct request, 2026-10-23                                                                                                                                                                                        |
| The Wayback Machine holds that page from 2024-12-10 to 2025-03-18. It is a ReDoc shell titled "Integrating lockr APIs with Publishers" that loads `./lockr_publisher_api.yaml`. That file returns 404, and the archive has no copy. The `sso.loc.kr` copy of the page, archived 2022 to 2025, loads the same file name. | Wayback Machine snapshots and CDX index, read 2026-10-23                                                                                                                                                                                                                   |
| An acquisition of Lockr was announced, with no statement about Lockr's APIs.                                                                                                                                                                                                                                            | [loc.kr announcement](https://loc.kr/viant-technology-announces-acquisition-of-lockr/), published 2025-03-03                                                                                                                                                               |

A Lockr API reference therefore existed, but its contents cannot be read
today. Whether it described a server-to-server issuance call is unknown.

### 3.2 What Prebid's Lockr module does

`lockrAIMIdSystem.js` is a Prebid User ID submodule. It is the only Lockr
token client whose source is pinned and readable:

- It runs in the browser and calls `https://identity.loc.kr` (`:24`).
- It returns nothing when GDPR applies: "lockrAIM is not intended for use
  where GDPR applies" (`:59-62`).
- It needs `appID` and `email` and runs only when both are set (`:69-78`).
  The email is raw. The docs say the sign-in handler should "include a raw
  email" (`lockrAIMIdSystem.md:18`, `:101-107`).
- It sends `POST /publisher/app/v1/identityLockr/generate-tokens` with the
  JSON body `{appID, data: {type: "email", value: <email>, gppString}}`
  (`:102-115`, `:145`). It sends no credential other than the appID.
- The response carries `lockrMappingToken`, which the module stores as
  `ilui` (`:122-123`), and `data[]`, whose items carry `key_name`,
  `advertising_token`, `identity_expires` and `settings.dropLocalStorage` and
  `settings.dropCookie` (`:124-137`).
- A one-second timer checks each stored expiry. Once one has passed, it calls
  the same endpoint with `type: "refresh"` and the mapping token as `value`
  (`:149-158`).

So Lockr has an HTTP API that issues tokens from an email and refreshes them
from a mapping token. It is called from the browser. Nothing published says
a server may call it, how Lockr authenticates a server, or which client IP
and user agent Lockr uses for a call it receives from a server.

### 3.3 What TS proxies today

- The API host defaults to `https://identity.loc.kr`
  (`integrations/lockr.rs:416-418`) and the SDK to
  `https://aim.loc.kr/identity-lockr-trust-server.js`
  (`integrations/lockr.rs:420-422`).
- The proxy routes are `/sdk` and `GET` and `POST /api/*`
  (`integrations/lockr.rs:353-355`). Any path under `/integrations/lockr/api`
  is forwarded to the API host with its query string
  (`integrations/lockr.rs:193-202`). A `POST` body is buffered and forwarded
  unchanged (`integrations/lockr.rs:206-213`).
- The proxy copies `Content-Type`, `Accept`, `User-Agent`,
  `Accept-Language` and `Accept-Encoding` (`integrations/lockr.rs:259-271`),
  the `Origin` or `origin_override` (`integrations/lockr.rs:273-287`), and
  the browser's `x-` headers other than TS's internal ones
  (`integrations/lockr.rs:289-294`). It never forwards `Cookie` or
  `Authorization` (`integrations/lockr.rs:250-258`). The proxy sets no
  client IP header of its own.
- The `origin_override` field exists for "running locally or from a domain
  not registered with Lockr" (`integrations/lockr.rs:68-73`). That suggests
  Lockr checks `Origin` against the publisher's registered domains. This is
  assumed.
- The proxied v2 calls were measured on a live page: `settings`,
  `page-view`, `generate-tokens`, `refresh-tokens`, `sync-no-hem-ids` and
  `revoke-consent`. The `page-view` request carries an `LTID` field,
  existing provider tokens, consent strings and page context. Its
  first-issue response carries `aimTokens[]` and `ids.<key>.eid`
  (`spec/lockr-identity-capture`, Lockr capture design section 2). The
  measured response has no `lockrMappingToken` field.
- These v2 calls are SDK traffic, not a published contract. The SDK is
  obfuscated, so its request contract cannot be read from it (fetched
  2026-10-23, 119,752 bytes, SHA-256
  `b7ac11d93bd4ea2a31e1b7b71e3aef1299a06a42f90f341b8363c25304d0c051`).

### 3.4 What this repo already decided

- The #1260 Lockr capture design puts "server-to-server calls to lockr" out
  of scope (its section 3). Its section 10 says active Lockr resolution from
  `ltid` needs a separate spec, and "no current S2S contract is assumed".
- The #1246 investigation found that its captures "do not establish whether
  Lockr internally minted a new identity or looked one up upstream". It also
  "did not identify a need to change Lockr's token issuance flow".
- The framework models Lockr as capture-only, with no `resolves`
  (framework section 12.2). It states that Lockr has no GVL ID (framework
  section 8). No source for that is given. This document treats it as
  assumed.

## 4. What the facts allow

Three server-side shapes can be read from the facts. None is usable today.

| Shape                                                                        | Why not                                                                                                                                                                                                                                                                                                                                    |
| ---------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Call v1 `generate-tokens` from the edge with an email (section 3.2)          | It takes a raw email. TS has no email in v1 (framework section 2.2), and never sends a raw email (section 10, D2). The module also skips GDPR traffic. Lockr has not said a server may call it.                                                                                                                                            |
| Refresh with a mapping token (section 3.2)                                   | It needs no personal input from TS beyond the token, so it is the closest fit to `ResolveKind::Refresh`. But TS never holds the token: the measured v2 responses carry none (section 3.3), and the framework's Lockr capture reads only `aimTokens` and `ids` (framework section 12.2). Its server use and its semantics are undocumented. |
| Replay the SDK's v2 `page-view` from the edge, without the SDK (section 3.3) | It needs `LTID` and the body the SDK builds, and neither exists when the SDK is blocked. It would copy undocumented SDK traffic without Lockr's approval. Rejected (section 10, D3).                                                                                                                                                       |

A Lockr resolver that helps an SDK-blocked visitor needs inputs TS has in
v1: the client IP, the user agent and the page URL (framework section 4.4).
The `sync-no-hem-ids` endpoint name suggests Lockr can issue some IDs
without a hashed email. What that endpoint takes, and whether a server may
call it, is assumed and unknown.

## 5. Questions for Lockr's account team

Ask these in writing. Each answer needs a citable document or a signed
statement, because the framework's vendor examples rest on sourced facts
(framework section 12).

**Endpoints**

1. Does Lockr offer an API that a publisher's server may call to issue or
   refresh identity tokens without the SDK in the browser? Give its base URL,
   paths, methods and version.
2. Is `POST /publisher/app/v1/identityLockr/generate-tokens`, as Prebid's
   module calls it, supported from a server? Is it deprecated in favour of
   the v2 paths?
3. Where is the current API reference? `aim.loc.kr/api-doc/lockr_reference.html`
   and its `lockr_publisher_api.yaml` return 404.
4. What does `sync-no-hem-ids` take and return, and may a server call it?

**Authentication**

5. How does Lockr authenticate a server: an API key, a client secret, a
   signed request, an IP allowlist or the appID alone?
6. Does Lockr check `Origin` against the publisher's registered domains? What
   `Origin`, if any, should a server send?
7. Are server credentials per publisher, per appID or per integrator?

**Inputs**

8. Which inputs issue an ID without an email: the client IP, the user agent,
   the page URL, the `LTID`, or other device signals?
9. Does any endpoint accept a SHA-256 hashed email in place of a raw email?
   Which normalization does Lockr expect before hashing?
10. How should a server pass the end user's IP and user agent: body fields
    or `X-Forwarded-For`? Today the proxy sends no client IP header (section
    3.3). Which address does Lockr use for proxied calls?
11. Which partner IDs can a server call issue: ID5, RampID through ATS,
    UID2, EUID, others? Does Lockr call those partners itself, as its release
    says?

**Response**

12. What is the response shape for each endpoint, and does it match the v2
    `aimTokens[]` and `ids.<key>.eid` shape measured on the proxy (section
    3.3)?
13. Is `ids.<key>.eid` an OpenRTB EID that Lockr guarantees, including
    `inserter`, `matcher` and `mm`?
14. Do v2 responses ever carry `lockrMappingToken`, and on which endpoints?
15. Which response means "no match", which means "opted out", and which
    means "token no longer valid"?

**Lifetimes and refresh**

16. What is `identity_expires` for each partner token, and does it differ by
    region?
17. How is a token refreshed server to server: with the mapping token, the
    `LTID` or the partner token? How long does each stay refreshable?
18. Does Lockr give a refresh hint, separate from expiry?

**Rate limits**

19. What are the rate limits per appID, per source IP and per `LTID`? TS's
    calls leave from Fastly's network, so many visitors share one address.
20. Which status and headers signal throttling?

**Consent**

21. Which consent signals does each endpoint require: a TC string, a GPP
    string, a US Privacy string, GPC? Is an empty GPP string, or the
    header-only `DBAA`, treated as no restriction?
22. Does Lockr hold an IAB Global Vendor List ID? The framework assumes not
    (framework section 8).
23. Does Lockr support GDPR traffic server to server? Prebid's module
    declines it (section 3.2).

**Revocation**

24. What does a 2xx `revoke-consent` mean: Lockr's own opt-out for this
    `LTID`, or a publisher-level identity withdrawal? This settles
    `revoke_scope` (framework section 8).
25. Can a server call report or check an opt-out? How does a server learn
    that a token it holds was revoked?

**Contract and operation**

26. Is server-to-server use covered by the existing AIM contract, or a
    separate agreement?
27. Is there a test appID and a sandbox host for acceptance on an
    `example.com` route?
28. Will the API stay stable after the 2025 acquisition, and how are changes
    announced?

## 6. Recommended fallback

Until Lockr answers, cover the SDK-blocked visitor with providers that call
partners directly. Lockr's capture stays for the sources no native provider
owns.

| Source          | Fallback              | Helps an SDK-blocked visitor in v1?                                                                                                                                             |
| --------------- | --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `id5-sync.com`  | Native ID5 provider   | Yes. ID5 needs the client IP and user agent, wants the page URL and needs no hashed email (framework section 12.4).                                                             |
| `liveramp.com`  | LiveRamp ATS provider | Only for a refresh. Initial resolution needs a hashed email, which v1 lacks, so ATS refreshes an envelope that browser ingest seeded and nothing else (framework section 12.1). |
| `uidapi.com`    | UID2 provider         | No. Initial resolution needs a hashed email, and a refresh needs the private state that only a server-side generate stores (framework section 12.3).                            |
| Other Lockr IDs | none                  | No. Sources such as `euid.eu` are not in Lockr's `KEY_SOURCES` (framework section 12.2).                                                                                        |

**ID5 is the only fallback that works for an SDK-blocked visitor in v1.**
LiveRamp and UID2 wait for hashed-email intake, which needs its own spec
(framework section 2.2). A hashed email is personal data (framework terms and
section 9.3), so that spec decides consent and handling before either
provider resolves from one.

Each fallback is its own vendor spec, as the framework requires (framework
section 2.2). Related issues: #90 and #353 (ID5), #1245 (LiveRamp).

### 6.1 Ownership under the fallback

Each source has at most one owner (framework sections 2.1 G6, 10.2). Lockr
is one provider ID. So a native owner and Lockr cannot both own a source,
and Lockr is narrowed to the sources no native provider owns:

```toml
[[ec.partners]]
name = "ID5"
source_domain = "id5-sync.com"
openrtb_atype = 1
bidstream_enabled = true
owner = "id5"

[[ec.partners]]
name = "LiveRamp"
source_domain = "liveramp.com"
openrtb_atype = 3
bidstream_enabled = true
owner = "lockr"

[ec.identity.providers.lockr]
# Narrowed away from id5-sync.com, which native ID5 owns.
sources = ["liveramp.com"]
capture_enabled = false
```

What follows from that:

- **Lockr's captured ID5 is no longer written server-side.** The browser
  path still carries it. Lockr writes its ID5 into Prebid's `ortb2` config,
  which the EID fidelity work forwards in the `/auction` body and `ts-eids`
  (framework section 12.2). For an owned source the owner's valid value wins
  over the browser's (framework section 7.1).
- **Bidders may see a different ID5 UID.** #1246 found a second, separately
  cached ID5 SDK identity on the page, with a different UID from the one Lockr
  supplied. Client-side bidder requests carried Lockr's. With native ID5 as
  owner, TS's auction sends what native ID5 returns once it has a value, and
  that may differ from Lockr's. This needs the user's decision (section 10,
  D4).
- **A browser-seeded value is refreshed at once.** Browser ingest may fill an
  owned source that is absent. The owner treats a value it did not write as
  due and refreshes it (framework section 7.2). So a Lockr ID5 seeded by the
  browser is replaced by native ID5 on the next eligible call.
- **Native ID5 may need a direct ID5 contract.** Publishers can activate ID5
  through Lockr's lockrKeys (section 3.1). A native provider needs its own ID5
  partner number (framework section 12.4). This is assumed and is open
  question 3.
- **If every Lockr source has a native owner,** Lockr's provider must be
  narrowed to no source, which validation refuses (framework section 10.2).
  The operator then cannot keep Lockr's SDK proxy without its identity
  provider. Section 8, change 1, fixes this.

Lockr's capture keeps G4 and G12 as the framework states them. It never
writes KV on its proxy calls (framework section 5.1), and its removals and
opt-outs apply whatever `capture_enabled` says (framework section 4.7).
Records Lockr wrote stay removable after it is narrowed: an opt-out removes
every record the provider wrote, owned or unowned (framework section 8).

## 7. Rollout and activation

- **No Lockr resolver work starts** until Lockr answers questions 1, 5, 8,
  12, 15, 17, 21 and 24 in writing and supplies a test appID. Then a Lockr
  provider spec follows, built on its answers.
- **Native ID5** ships in its own spec, with its integration disabled by
  default. Before an operator moves `id5-sync.com` from `owner = "lockr"` to
  `owner = "id5"`, the steps of the capture provider checklist apply
  (framework section 14): the source has its `[[ec.partners]]` entry, it is
  moved off pull sync first, and live acceptance shows the stored UID in
  `/_ts/admin/ec`.
- **Lockr capture** is unchanged: `capture_enabled = false` until the
  checklist has passed once on a live service (framework section 14).
- **The first-auction race metric** (framework sections 14, 15 question 3)
  still decides how much capture is worth. It does not depend on this
  document.

## 8. Proposed framework changes

These are additive. They are for revision 13. This document does not edit
the framework spec.

**Change 1. A provider can be turned off while its integration stays on.**
Needed by section 6.1 now, whatever Lockr answers.

Add to `[ec.identity.providers.<id>]` in framework section 10.1:

```toml
[ec.identity.providers.lockr]
# Default true. False registers no identity provider for this integration,
# while the integration's proxy and other capabilities keep running. Records
# the provider wrote are no longer served (section 7.1), and browser ingest
# or another provider can refill them.
enabled = false
```

Change the narrowing rule in framework section 10.2 to read: "`sources`
names a source the provider's descriptor does not declare, or narrows an
enabled provider to no source at all. A provider with `enabled = false` is
skipped by every ownership check." The existing error "`owner` names a
provider that is not enabled" then covers an owner with `enabled = false`.

Test: a config whose `[[ec.partners]]` give `id5-sync.com`, `liveramp.com`
and `uidapi.com` native owners validates with
`[ec.identity.providers.lockr] enabled = false`, still serves
`/integrations/lockr/api/*`, registers no Lockr provider, and stops serving
Lockr-written records.

**Change 2. The #777 `identity` type runs several modules.** PR #1348's
configuration rules make `identity` a type that runs one module, with
`module` as a string and `lockr` as its implementation
(`docs/guide/configuration-rules.md:58` on
`feature/7-integration-seam-impl`). The fallback runs Lockr beside native
ID5, LiveRamp and UID2.

Add to framework section 10.5: "Under the #777 stack, the `identity` type
runs several modules and takes `modules`, a list, because one page can run
Lockr capture beside native providers. Each module's settings stay in
`[identity.<name>]`, and `[ec.identity.providers.<id>]` is unchanged."

Test: a #777-layout config with `[identity] modules = ["lockr", "id5"]`,
`[identity.lockr]` and `[identity.id5]` loads, registers both providers and
passes the ownership checks of framework section 10.2.

**Change 3, conditional. A resolve switch for a provider that also
captures.** Needed only if Lockr confirms an API. The Lockr integration is
already enabled for its proxy. Adding `with_resolves` to its descriptor
would start outbound calls on the next deploy, with no gate like
`capture_enabled`.

Add to framework section 10.1: "`resolve_enabled`: only for providers that
declare both `captures` and `resolves`. Default `false`: the provider is
never dispatched, while its capture keeps running." Add to framework section
10.2: "`resolve_enabled` is set for a provider whose descriptor does not
declare both `captures` and `resolves`."

Test: a provider declaring both, with `resolve_enabled` absent, captures on
its proxy paths and is never dispatched by the coordinator on any route.

No change is proposed to the trait. If Lockr confirms an API, capture and
resolution share one provider ID and so one ownership claim, one `ps` and one
sticky opt-out (framework sections 4.2, 8). The trait already allows a
descriptor with both `with_captures` and `with_resolves` (framework section
4.3).

## 9. Open questions

1. **Does a server-to-server Lockr API exist?** Blocked on Lockr's answers to
   section 5. Until then, Lockr is capture-only.
2. **Does Lockr have a GVL ID?** Assumed not (framework section 8). The
   answer changes how a Lockr opt-out clears.
3. **Does native ID5 need a direct ID5 contract** when the publisher gets ID5
   through Lockr today? Assumed yes (section 6.1).
4. **Does Lockr check `Origin`** against registered domains? Assumed from
   `integrations/lockr.rs:68-73`.
5. **Which client address does Lockr use** for proxied calls? Unknown. TS
   sends no client IP header (section 3.3).
6. **What does `sync-no-hem-ids` take?** Unknown (section 4).
7. **Is the v1 `generate-tokens` API still supported,** and is the mapping
   token part of the v2 flow? Unknown (sections 3.2, 3.3).
8. **What does `revoke-consent` mean?** Still open from #1260 (Lockr capture
   design section 10) and framework section 12.2.
9. **Who owns `id5-sync.com`?** Section 10, D4.

## 10. Decisions

Proposed here, pending the user's review:

| ID  | Decision                                  | Proposed choice                                                                                                                    |
| --- | ----------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| D1  | Lockr resolver                            | None until Lockr confirms an API in writing. Lockr stays capture-only (framework section 12.2).                                    |
| D2  | Raw email                                 | TS never sends a raw email to any provider. A Lockr resolver must accept a hashed email or no email.                               |
| D3  | Replaying SDK traffic                     | TS never calls Lockr's v2 SDK endpoints without the SDK.                                                                           |
| D4  | Owner of `id5-sync.com`                   | Native ID5, so an SDK-blocked visitor gets an ID5. Lockr is narrowed away from it. The change in bidder-visible UID is accepted.   |
| D5  | Owners of `liveramp.com` and `uidapi.com` | Lockr until hashed-email intake lands, then the native providers. In v1 neither native provider can start an ID for a new visitor. |
| D6  | Turning a provider off                    | `[ec.identity.providers.<id>] enabled`, default `true` (section 8, change 1).                                                      |
| D7  | #777 `identity` type                      | Runs several modules (section 8, change 2).                                                                                        |
| D8  | Resolve switch                            | `resolve_enabled`, default `false`, added only with a confirmed Lockr API (section 8, change 3).                                   |
