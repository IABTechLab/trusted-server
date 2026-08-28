# LiveRamp Integration Design

**Issue:** [#355 — Investigate and document LiveRamp integration](https://github.com/IABTechLab/trusted-server/issues/355)

**Parent epic:** [#354 — LiveRamp integration](https://github.com/IABTechLab/trusted-server/issues/354)

**Initiative:** [#55 — Monetization integrations](https://github.com/IABTechLab/trusted-server/issues/55)

**Status:** Implemented in draft PR; bundle-consistency guard approved

**Date:** 2026-08-21

**Revised:** 2026-08-28

## 1. Executive summary

LiveRamp integration is feasible in two distinct forms, but they must not be
treated as one protocol:

1. **RampID identity-envelope forwarding through Prebid.js is feasible now.**
   Trusted Server already bundles Prebid's `identityLinkIdSystem`, reads
   `liveramp.com` EIDs through `pbjs.getUserIdsAsEids()`, sends them to
   `/auction`, merges them with EC/KV identities, applies consent gating, and
   forwards them to Prebid Server as OpenRTB `user.ext.eids`.
2. **LiveRamp ATS Direct audience segments are not part of that EID flow.** ATS
   Direct returns a separate segment envelope and has separate subscription,
   storage, TTL, deal-approval, and activation requirements. The cited
   LiveRamp documentation describes activating these values as GAM `atsd`
   targeting, not as a `liveramp.com` EID.

The first implementation makes the existing RampID path operationally complete
through vendor-neutral `managed_user_ids` configuration under the Prebid
integration. The generated bundle command must resolve every managed config
name through the checked-in User ID registry and reject a manifest that omits
the corresponding module. Native server-to-server ATS resolution and ATS
Direct segment activation remain separate follow-up decisions.

## 2. Issue hierarchy and collected requirements

The GitHub issue hierarchy is:

```text
#55 Initiative: Monetization integrations
└── #354 Epic: LiveRamp integration
    └── #355 Task: Investigate and document LiveRamp integration
        └── IABTechLab/uid2-optout#385: Get test credentials from LR team
```

The three trusted-server issues have empty or placeholder bodies, so their
comments and linked documentation define the operative requirements.

### 2.1 Issue #354

The only comment asks the team to confirm whether LiveRamp segments are passed
to auction requests through the Prebid.js integration. This specification must
therefore distinguish identity envelopes from segment data and answer both
questions explicitly.

### 2.2 Issue #355

The comments establish the following sequence and requirements:

1. Review LiveRamp's Real-Time Identity Service (RTIS) tag documentation.
2. Wait for LiveRamp to clarify the integration.
3. Test the documentation LiveRamp supplied.
4. Review the ATS Envelope API page LiveRamp recommended.
5. Write a specification and determine feasibility.

The issue's direct deliverable is an evidence-backed specification. If the
recommended path is feasible, implementation follows the approved design.

### 2.3 Credential dependency

Issue #355 has a cross-repository child,
[IABTechLab/uid2-optout#385](https://github.com/IABTechLab/uid2-optout/issues/385),
named “Get test credentials from LR team.” The implementation owner has since
confirmed access to a test Placement ID and a MITM-assisted browser validation
environment. Those values remain outside the repository.

Automated tests must not depend on LiveRamp configuration. A live Placement ID
and a LiveRamp-approved test origin are required for final end-to-end browser
verification, but their availability is no longer an implementation blocker.

## 3. Terminology and product boundaries

### 3.1 RampID identity envelope

Prebid's LiveRamp module is named `identityLinkIdSystem`, its configuration name
is `identityLink`, and its EID source is `liveramp.com`. It resolves an encrypted
RampID envelope into Prebid's identity APIs. The envelope identifies a user to
authorized demand partners; Trusted Server treats the value as opaque.

### 3.2 RTIS

LiveRamp's Real-Time Identity Service tag is a pixel or JavaScript tag that uses
LiveRamp cookie recognition and redirects a RampID to an endpoint registered
with LiveRamp. It requires LiveRamp to configure a tag ID and callback endpoint.
Trusted Server has no RTIS callback route today.

RTIS is not selected for the first implementation because the managed Prebid
module already provides the browser-to-bidstream path, while a new callback
would require correlation, endpoint authentication, storage, abuse protection,
and a LiveRamp-specific server contract.

### 3.3 ATS Envelope API

The ATS Envelope API resolves hashed email, hashed phone, or configured custom
IDs into one or more encrypted envelopes. A server-to-server call requires a
Placement ID, a privacy-approved Origin, consent parameters where applicable,
and the browser's client IP in `X-Forwarded-For`.

The ordinary ATS response contains an identity envelope with `type: 19` and
`source: "envelopeLiveramp"`. A no-consent response is HTTP 204. Configuration,
authorization, service, and geographic/consent failures use distinct 4xx
statuses.

### 3.4 ATS Direct segments

ATS Direct is a separate product layered onto an approved ATS placement and
subscription. Its V2 response can include `type: 26`, `source: "atsDirect"`,
whose value represents matching deal/segment IDs. LiveRamp documents storing
this in `_lr_atsDirect`, maintaining a region-dependent TTL, refreshing it, and
applying selected deal IDs to GAM under the `atsd` targeting key.

An ATS Direct segment envelope is not a RampID and must not be placed in
`user.ext.eids` under `liveramp.com`.

## 4. Current Trusted Server capabilities

The following capabilities already exist on `main`:

- `crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.json`
  includes `identityLinkIdSystem` in the default preset, maps the Prebid config
  name `identityLink`, and maps EID source `liveramp.com`.
- `crates/trusted-server-js/lib/src/integrations/prebid/index.ts` reads
  `pbjs.getUserIdsAsEids()`, validates EID structure, and includes valid EIDs in
  the current `/auction` request.
- The same TSJS module persists structured OpenRTB-style EIDs in the first-party
  `ts-eids` cookie after auction completion.
- `crates/trusted-server-core/src/auction/endpoints.rs` parses current-request
  EIDs, loads server-resolved EIDs from the EC/KV graph, merges and deduplicates
  them, and applies centralized consent gating.
- `crates/trusted-server-core/src/integrations/prebid.rs` serializes the merged
  set to Prebid Server as OpenRTB `user.ext.eids`.
- `crates/trusted-server-core/src/ec/prebid_eids.rs` ingests `ts-eids` on a
  later request and maps configured sources such as `liveramp.com` into the
  EC/KV identity graph.
- The external bundle manifest and runtime diagnostics already identify which
  Prebid User ID modules were compiled into the bundle.

### 4.1 Current gap

The draft implementation configures operator-owned User ID entries through the
vendor-neutral `managed_user_ids` surface, but commit `2f89a222` removed the
earlier LiveRamp-specific bundle guard. As a result, `ts prebid bundle` can
produce an artifact whose manifest omits the module required by a managed
entry. Runtime diagnostics warn after deployment, but the build itself succeeds
and updates deployable hash metadata.

The checked-in registry already maps each Prebid config name to its bundle
module. The CLI can therefore validate the generated manifest without adding a
vendor name or maintaining a second mapping.

## 5. Approaches considered

### 5.1 Selected: vendor-neutral managed User IDs with bundle validation

Add `managed_user_ids` to `PrebidIntegrationConfig`, inject the opaque entries
through `window.__tsjs_prebid`, and let the TSJS Prebid shim install and protect
each operator-owned Prebid User ID configuration before queued work is
processed. At bundle time, the CLI reads the same registry as the JavaScript
generator, resolves each managed config name, and confirms that the freshly
generated manifest contains every required module before updating hash/SRI
metadata.

Benefits:

- Uses the existing module, bundle generator, EID transport, consent gate, and
  EC/KV ingestion path.
- Keeps browser identity configuration beside the Prebid bundle that consumes
  it.
- Adds no new upstream route or PII-bearing server API.
- Fails an unusable managed-name/module pairing during the bundle command.
- Can be fully tested without external credentials, with a separate live
  verification gate.

Trade-offs: this only resolves identities visible to browser modules, and the
CLI must deserialize the registry's vendor-neutral module/config-name schema.
It does not add server-side HEM resolution or ATS Direct segments.

### 5.2 Rejected for the first implementation: standalone LiveRamp integration

A new `integrations/liveramp` module could own browser and server APIs. This is
premature because only the Prebid browser path is approved, while the ATS API
input contract and ATS Direct product scope remain unresolved. It would also
duplicate Prebid lifecycle and bundle validation responsibilities.

Revisit this boundary if a future approved design adds server-to-server ATS
resolution or a non-Prebid LiveRamp consumer.

### 5.3 Rejected: RTIS callback endpoint

An RTIS endpoint would introduce a new unauthenticated redirect/callback
surface and a correlation problem without improving the already-supported
Prebid identity path. LiveRamp also requires per-endpoint configuration. It is
not justified for the current requirement.

### 5.4 Deferred: native server-to-server ATS resolution

Native resolution is technically possible with Trusted Server's platform HTTP
abstractions, consent context, geo context, and client IP access. It is not
implementation-ready because:

- Trusted Server has no approved source for hashed email, hashed phone, or a
  LiveRamp custom ID.
- Sending a hashed identifier to LiveRamp is a privacy and publisher-contract
  decision, not merely a transport detail.
- ATS API enablement and placement configuration for server-to-server use are
  not confirmed by the browser Placement ID alone.
- Rate limits, timeout policy, caching, envelope refresh, and identifier
  deletion semantics are not confirmed.
- [#630 — HEM Resolution (LiveRamp)](https://github.com/IABTechLab/trusted-server/issues/630)
  was closed as not planned and must not be silently revived.

## 6. Proposed configuration

> **Revision, 2026-08-25.** An earlier draft of this section specified a typed
> `[integrations.prebid.liveramp]` subsection, which named a single identity
> vendor inside `trusted-server-core`. It is superseded by the vendor-neutral
> `managed_user_ids` surface below. RampID is now a configuration choice, not a
> type in core.

Managed Prebid User ID modules are optional and nested under the existing Prebid
integration. Each entry is an opaque passthrough: core validates only what
Prebid needs to address the module, and never interprets `params`.

```toml
[integrations.prebid]
enabled = true
server_url = "https://prebid.example.com/openrtb2/auction"
external_bundle_url = "https://assets.example.com/prebid/trusted-prebid-<sha256>.js"
external_bundle_sha256 = "<sha256>"
external_bundle_sri = "sha256-<base64>"

# RampID, expressed purely as operator configuration.
[[integrations.prebid.managed_user_ids]]
name = "identityLink"
params = { pid = "999", notUse3P = false }

[integrations.prebid.managed_user_ids.storage]
type = "cookie"
name = "idl_env"
expires = 15
refresh_in_seconds = 1800
```

The Rust representation names no vendor:

```rust
pub struct PrebidIntegrationConfig {
    // Existing fields omitted.
    pub managed_user_ids: Vec<PrebidManagedUserIdConfig>,
}

pub struct PrebidManagedUserIdConfig {
    pub name: String,
    pub params: serde_json::Map<String, serde_json::Value>,
    pub storage: Option<PrebidManagedUserIdStorage>,
}

pub struct PrebidManagedUserIdStorage {
    pub storage_type: PrebidUserIdStorageType,
    pub name: String,
    pub expires: Option<u16>,
    pub refresh_in_seconds: Option<u32>,
}

pub enum PrebidUserIdStorageType {
    Cookie,
    Html5,
}
```

Validation:

| Field                        | Rule                                                                                                                                                                                                                |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `name`                       | Required. Non-empty, untrimmed-free ASCII token of letters, digits, `_`, `-`, or `.`. Unique across entries: Prebid keys `userSync.userIds` by name, so a repeat gives one submodule two conflicting configurations |
| `params`                     | Optional. Any TOML table; forwarded to Prebid without inspection                                                                                                                                                    |
| `storage.type`               | Optional. `cookie` (default) or `html5`                                                                                                                                                                             |
| `storage.name`               | Required when `storage` exists. Same token rule as `name`                                                                                                                                                           |
| `storage.expires`            | Optional. At least 1 when present; omitted leaves Prebid's default. No upper bound — a ceiling is the module's                                                                                                      |
| `storage.refresh_in_seconds` | Optional. At least 1 when present; omitted leaves Prebid's default                                                                                                                                                  |

Values that used to be typed defaults in core — `notUse3P = false`,
`idl_env`, 15 days, 1800 seconds — are now operator-supplied, because each is a
property of the module the operator selected rather than of Trusted Server.

The operator selects both managed entries and bundle modules, but `ts prebid
bundle` validates that selection. It reads
`crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.json`, the
same registry used by the JavaScript generator, and joins each managed `name`
against the registry's `configNames`. No vendor-specific mapping is compiled
into the CLI, and registry additions extend both the generator and validation.

### 6.1 Bundle consistency validation

The CLI performs focused validation in this order:

1. Parse the managed User ID names alongside the existing bundle inputs.
2. Locate the JavaScript library and load its checked-in User ID registry.
3. Resolve every managed name to exactly one `moduleName`. Unknown or
   ambiguously mapped names fail.
4. Generate the external Prebid bundle normally.
5. Deserialize `userIdModules` from the freshly generated manifest.
6. Confirm that every resolved module appears in the manifest.
7. Update `external_bundle_sha256` and `external_bundle_sri` only after the
   consistency check succeeds.

The failure names the managed config name, required module, and corrective TOML
field. A failed consistency check leaves the existing config metadata unchanged.
The browser-side warning remains as defense in depth for externally built,
stale, or modified artifacts.

An empty `managed_user_ids` preserves current behavior and emits no managed User
ID configuration.

## 7. Browser configuration and ordering

The Rust Prebid head injector extends `window.__tsjs_prebid` with a camel-cased
`managedUserIds` array containing the validated entries. A Placement ID is an
operator identifier rather than a secret, but diagnostics must not copy
envelope values.

The TSJS Prebid shim translates the injected object into:

```javascript
{
  userSync: {
    userIds: [
      {
        name: 'identityLink',
        params: {
          pid: '999',
          notUse3P: false,
        },
        storage: {
          type: 'cookie',
          name: 'idl_env',
          expires: 15,
          refreshInSeconds: 1800,
        },
      },
    ]
  }
}
```

Publisher commands may already be waiting in `window.pbjs.que`, including a
`requestBids` command. Appending the managed configuration would be too late:
Prebid processes existing commands in insertion order, so a publisher auction
could run before the new entry.

When LiveRamp is configured, the shim instead installs narrowly scoped,
idempotent wrappers around the public `pbjs.setConfig` and `pbjs.mergeConfig`
APIs before calling `pbjs.processQueue()`:

1. Capture and bind the real `pbjs.setConfig` and, when present,
   `pbjs.mergeConfig` implementations.
2. Replace both public APIs with wrappers that normalize every call containing
   a `userSync` object. Calls without `userSync` pass through unchanged.
3. For a call with an explicit `userSync.userIds`, preserve every
   non-`identityLink` entry,
   remove all publisher-supplied `identityLink` entries, and append exactly one
   operator-managed entry. Preserve sibling `userSync` and top-level fields.
4. Calls whose `userSync` object omits `userIds` pass through unchanged. The
   pinned generated Prebid artifact retains its effective `userIds` defaults
   across partial `setConfig` and `mergeConfig` updates, so injecting a copied
   list in the shim would duplicate Prebid behavior and make the wrapper depend
   on a mocked configuration model that does not match the shipped artifact.
   A real-artifact characterization test protects this pinned behavior.
5. During initial installation, read the already-effective
   `pbjs.getConfig('userSync.userIds')` value,
   normalize its supported array/config shape, preserve its non-`identityLink`
   entries, append the managed entry, and apply that merged list synchronously
   through the captured function. This covers publisher configuration that ran
   after the external Prebid bundle loaded but before the deferred TSJS shim.
   An absent or malformed effective list degrades to an empty publisher list.
   Complete this step before processing any existing queue entries.
6. Call `pbjs.processQueue()`. Queued publisher `setConfig` and `mergeConfig`
   calls flow through the wrappers, so a later queued `requestBids` observes
   the managed entry.
7. Keep the wrappers installed after queue processing so later publisher calls
   through either public configuration API cannot silently replace or delete
   the operator-owned LiveRamp policy. Repeated TSJS installation must not
   stack wrappers.

This is configuration ownership for supported Prebid API usage, not a security
boundary against adversarial same-origin JavaScript that retained an earlier
function reference or mutates internal configuration objects directly.

This policy gives the operator ownership of every configured managed entry.
Publishers retain ownership of all other Prebid and User ID configuration.
Omitting `managed_user_ids` installs no wrapper and preserves current publisher
behavior exactly.

After queue processing, existing runtime diagnostics repeat the registry-backed
module check against the browser bundle stamp. This is a fallback for bundles
that were built externally, became stale, or were modified after `ts prebid
bundle`; a bundle created by the CLI has already passed the build-time check.

## 8. Data flow

```mermaid
sequenceDiagram
    participant O as Operator config
    participant TS as Trusted Server
    participant B as Browser
    participant LR as LiveRamp
    participant PBS as Prebid Server
    participant KV as EC identity graph

    O->>TS: Configure integrations.prebid.managed_user_ids
    TS-->>B: Inject managed User ID config and Prebid bundle
    B->>B: Guard setConfig/mergeConfig and merge managed entries
    B->>LR: Prebid identityLink module resolves/refreshes envelope
    LR-->>B: Opaque RampID envelope
    B->>B: pbjs.getUserIdsAsEids()
    B->>TS: POST /auction with source=liveramp.com EID
    TS->>TS: Validate, merge, deduplicate, consent-gate EIDs
    TS->>PBS: OpenRTB user.ext.eids
    B->>B: Persist structured EIDs in ts-eids after auction
    B->>TS: Later request with ts-eids + ts-ec
    TS->>KV: Upsert configured liveramp.com partner UID
```

Identity resolution is asynchronous. The design does not promise a LiveRamp
EID in the first auction on a new browser. Current-request forwarding applies
as soon as `getUserIdsAsEids()` exposes the envelope; `ts-eids` and EC/KV
ingestion provide reuse on later requests.

## 9. Consent, privacy, and security

- Trusted Server continues to apply its centralized consent gate before EIDs
  reach providers. No LiveRamp-specific bypass is introduced.
- Prebid's User ID and consent-management modules remain responsible for
  deciding whether the browser may call LiveRamp. LiveRamp must be configured
  correctly in the publisher's CMP/GVL posture.
- Correction applied during implementation: `consentManagementTcf` only
  _retrieves_ the TC string. Enforcement lives in Prebid's `tcfControl`
  activity-control module, which the generated external bundle did not carry.
  Without it a denied Purpose 1 still permitted the vendor call and the
  `idl_env` write; only EID _forwarding_ was gated, server-side. The bundle now
  imports `tcfControl`, covered by
  `crates/trusted-server-js/lib/test/prebid-consent-enforcement.test.mjs`.
  Equivalent GPP/US-state activity controls (`gppControl_usnat`,
  `gppControl_usstates`) remain unbundled; US opt-outs are still enforced only
  at the server's forwarding gate.
- Pinned Prebid's default `tcfControl` rules do not treat every denied purpose
  identically. Purpose 1 plus the module's GVL vendor consent controls
  IdentityLink device access, resolution, and storage. Purpose 2 controls bid
  fetching. Purpose 3 has no standalone default `tcfControl` rule. Purpose 4
  controls user-provided-data activity. With the default
  `eidsRequireP4Consent: false`, EID transmission is permitted when any Purpose
  2–10 has the required purpose/legal-interest and vendor basis; publishers may
  opt into requiring Purpose 4 specifically. Therefore a Purpose 3 or Purpose
  4 denial alone does not establish that the LiveRamp vendor request or
  `idl_env` write is blocked. Automated artifact tests must vary Purpose 1,
  Purposes 3/4, and vendor 97 independently, and the operator guide must
  describe these exact defaults rather than claiming that every denied purpose
  blocks resolution.
- LiveRamp envelope values are opaque identifiers. They must never appear in
  logs, public diagnostics, error bodies, or telemetry dimensions.
- The implementation does not collect plaintext or hashed email and does not
  add an API for publishers to submit either value.
- The managed configuration preserves unrelated publisher User ID entries but
  owns the single `identityLink` entry when enabled. This prevents ambiguous
  duplicate LiveRamp configurations.
- Existing EID size limits, source/UID validation, cookie caps, merge rules,
  and consent withdrawal behavior remain authoritative.
- Live credentials and Placement IDs must not be committed to fixtures or
  repository configuration.

## 10. Error and degraded behavior

| Condition                                                     | Behavior                                                                                           |
| ------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| `managed_user_ids` absent                                     | Preserve current behavior; configure no operator-managed User ID entries.                          |
| Managed name is absent or ambiguous in the registry           | Fail `ts prebid bundle` before updating config metadata.                                           |
| Required module is absent from the generated manifest         | Fail `ts prebid bundle` and name both the config name and required module.                         |
| An externally supplied runtime bundle omits a required module | Emit existing browser diagnostics; continue auctions without that module's EID.                    |
| LiveRamp network or recognition failure                       | Prebid module yields no EID; continue auction normally.                                            |
| TCF Purpose 1 or LiveRamp vendor consent denied               | Default `tcfControl` blocks IdentityLink resolution/storage; continue auction normally.            |
| TCF Purpose 3 or 4 denied alone                               | Default rules do not prove resolution/storage is blocked; publisher policy may add stricter rules. |
| US-state opt-out                                              | Server forwarding gate drops the LiveRamp EID; browser activity controls remain a documented gap.  |
| Malformed LiveRamp EID                                        | Existing client/server EID sanitizers drop it.                                                     |
| Oversized `ts-eids` payload                                   | Existing bounded cookie behavior truncates whole UID/source entries; no partial UID is written.    |
| EC/KV unavailable                                             | Current-request EID can still reach `/auction`; persistence degrades without blocking the auction. |

Trusted Server does not parse LiveRamp envelope contents and therefore cannot
distinguish authenticated ATS envelopes from cookie-recognized RTIS envelopes.
That distinction remains inside LiveRamp's module and encrypted envelope.

## 11. Testing strategy

Implementation follows test-driven development.

### 11.1 Rust configuration tests

Add tests in `crates/trusted-server-core/src/integrations/prebid.rs` and the
settings tests to prove:

- managed entries deserialize with opaque nested `params`;
- documented storage defaults are applied;
- blank or whitespace-padded managed and storage names fail;
- invalid expiry and zero refresh values fail;
- unknown storage types fail;
- duplicate managed names fail;
- omission remains backward-compatible;
- serialized head configuration uses the expected camel-cased keys;
- script-breaking input cannot escape the injected script element.

### 11.2 TypeScript unit tests

Add tests in
`crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts` proving:

- no managed config produces no operator-owned entry;
- managed config creates the exact documented Prebid object;
- unrelated publisher `userIds` entries are preserved;
- a publisher-provided entry with a managed name is replaced, not duplicated;
- managed configuration is active before an already-queued publisher
  `requestBids` call;
- queued publisher `setConfig` followed by `requestBids` preserves other User
  ID entries while the auction observes the managed `identityLink` entry;
- User ID entries already effective before TSJS installation are preserved
  while the managed `identityLink` entry is added;
- malformed pre-install `userSync.userIds` state degrades to the managed entry
  without throwing;
- queued and late publisher `identityLink` updates through `mergeConfig` are
  normalized back to the operator-managed values;
- a publisher `identityLink` update through `setConfig` after `processQueue()`
  is normalized back to the operator-managed values;
- repeated installation does not stack either configuration wrapper;
- configuration calls without an explicit `userIds` list pass through
  unchanged;
- missing `identityLinkIdSystem` appears in existing diagnostics;
- `getUserIdsAsEids()` output for `liveramp.com` enters the current auction;
- malformed and empty envelope values are dropped;
- envelope values are not written to logs or diagnostics;
- the existing `ts-eids` persistence path preserves the opaque value without
  decoding it.

### 11.3 Bundle tests

Extend external bundle tests to prove:

- the default preset contains `identityLinkIdSystem`;
- explicitly selecting it stamps the module into the manifest;
- the manifest stamps the exact selected User ID module list;
- a generated-real-bundle case that denies only Purpose 1 while granting
  Purposes 3/4 and vendor 97 produces no LiveRamp request and no `idl_env`;
- a separate case that denies only vendor 97 while granting Purposes 1/3/4
  produces no LiveRamp request and no `idl_env`;
- separate cases that deny only Purpose 3 or only Purpose 4 while granting
  Purpose 1 and vendor 97 still produce one LiveRamp request and write
  `idl_env` under pinned Prebid's default rules.
- a generated-real-bundle case proves a partial `userSync` update retains the
  publisher entry and exactly one managed `identityLink` entry.

### 11.4 CLI bundle consistency tests

Add focused tests in `crates/trusted-server-cli/src/prebid_bundle.rs` proving:

- no managed entries preserve existing bundle behavior;
- a known name passes when its resolved module is in the manifest;
- a known name fails when its resolved module is absent;
- multiple managed names are checked;
- `pubCommonId` resolves to `sharedIdSystem`;
- multiple aliases may resolve to the same required module;
- an unknown name fails with an actionable registry error;
- an ambiguously mapped synthetic name fails deterministically;
- a missing or malformed `userIdModules` manifest field fails;
- omission of `bundle.user_id_modules` works with the generated default preset;
- failed consistency validation does not update hash/SRI config metadata;
- the checked-in registry maps `identityLink` to `identityLinkIdSystem`.

### 11.5 Rust auction/EC regression tests

Existing generic EID tests cover most transport behavior. Add or retain a
LiveRamp-named fixture proving that a `liveramp.com` EID:

- is forwarded as `user.ext.eids` to the Prebid provider;
- merges without duplication against the EC/KV version;
- is removed when consent denies identity forwarding;
- is ingested into the configured `liveramp.com` EC partner namespace on a
  later request.

### 11.6 Live credential validation

Run outside CI against a LiveRamp-approved non-production origin:

1. Obtain a test Placement ID and confirm the origin is approved.
2. Generate a Prebid bundle containing `identityLinkIdSystem`.
3. Configure a managed `identityLink` entry with the test Placement ID.
4. Load the publisher page with positive consent.
5. Confirm `idl_env` is created or refreshed according to the selected storage.
6. Confirm `pbjs.getUserIdsAsEids()` returns a `liveramp.com` entry without
   recording its value.
7. Inspect a controlled Prebid Server request and confirm the same source is
   present in `user.ext.eids`.
8. Confirm a later request can ingest the EID into the configured EC partner.
9. Repeat with opt-out/no-consent and confirm no LiveRamp EID is forwarded.
10. Repeat with an unapproved origin and document the expected degraded result.

Record only booleans, source names, counts, and status codes. Do not capture or
publish live envelopes.

## 12. Documentation changes

Implementation updates:

- `trusted-server.example.toml` with a commented LiveRamp example;
- `docs/guide/integrations/prebid.md` with configuration, lifecycle, bundle,
  consent, troubleshooting, and verification guidance;
- `docs/guide/configuration.md` with the typed field reference;
- optionally a short `docs/guide/integrations/liveramp.md` page if the Prebid
  guide would become difficult to navigate. The first implementation should
  avoid duplicating the authoritative Prebid flow across two pages.

The documentation must state that:

- RampID envelopes, not audience segments, are forwarded as EIDs;
- a Placement ID and LiveRamp-approved origin are operational prerequisites;
- the first auction may not contain a newly resolved identity;
- a module included in a bundle is inert until configured;
- ATS Direct segments require separate enablement and implementation.

## 13. Rollout and observability

1. Land configuration and tests with the subsection absent by default.
2. Generate and publish a test bundle that includes `identityLinkIdSystem`.
3. Validate on a non-production approved origin with debug logging restricted
   to source names/counts.
4. Enable for a canary publisher property.
5. Monitor missing-module diagnostics, LiveRamp EID presence counts, auction
   error rates, and cookie/header size truncation counts. Never dimension
   metrics by envelope value.
6. Validate opt-out behavior before broader rollout.
7. Document the tested Placement/origin configuration in operator-owned,
   non-repository deployment records.

No database or KV migration is required. Omitting the new subsection provides
an immediate configuration rollback.

## 14. Acceptance criteria

Issue #355's implementation portion is complete when:

- operators can configure LiveRamp RampID through typed Trusted Server config;
- invalid configuration fails before serving traffic;
- managed configuration preserves non-LiveRamp publisher User ID modules and
  owns one deterministic `identityLink` entry;
- bundle diagnostics detect a missing `identityLinkIdSystem`;
- valid `liveramp.com` EIDs follow the existing browser → `/auction` → Prebid
  Server path without exposing envelope contents;
- existing consent, validation, merge, cookie, and EC/KV behavior is preserved;
- automated Rust and TypeScript tests pass;
- operator documentation explains setup, timing, privacy, failure behavior,
  and live verification;
- credential-based validation is completed, or the remaining credential block
  is explicitly recorded with an owner and the implementation is labeled
  “code complete, live validation pending”; and
- the parent epic receives the explicit answer: RampID identity envelopes can
  be passed through the Prebid auction path; ATS Direct segments are not passed
  by this implementation.

## 15. Out of scope and follow-up work

### 15.1 Server-to-server ATS resolution

Create or reopen a dedicated issue only after product approval. Its design must
define the hashed-identifier source, origin approval, consent mapping,
`X-Forwarded-For` handling, timeout/cache/refresh policy, geographic failure
behavior, data deletion, and credential storage. It must also reconcile the
decision that closed #630 as not planned.

### 15.2 ATS Direct audience segments

Create a separate issue if publishers require LiveRamp segment activation. It
must define:

- ATS Direct subscription and approved-deal prerequisites;
- whether the integration calls the API or consumes existing browser storage;
- `_lr_atsDirect` and TTL ownership;
- refresh behavior and regional TTL rules;
- whether activation targets GAM (`atsd`), Prebid first-party data, a Prebid
  real-time-data module, or more than one destination;
- consent and deletion behavior; and
- the exact evidence needed to confirm segment delivery.

### 15.3 RTIS callback

Do not add an RTIS callback unless a concrete non-Prebid use case demonstrates
that the browser module is insufficient and LiveRamp approves the endpoint
contract.

## 16. Authoritative references

- [LiveRamp: Implementing the Real-Time Identity Service Tag](https://docs.liveramp.com/identity/en/implementing-liveramp-s-real-time-identity-service-tag.html)
- [LiveRamp: Call the ATS Envelope API](https://developers.liveramp.com/authenticatedtraffic-api/docs/4-call-the-ats-envelope-api)
- [LiveRamp: Retrieving Envelope Endpoints](https://developers.liveramp.com/authenticatedtraffic-api/v1.0/docs/about-the-ats-api)
- [LiveRamp: ATS Direct](https://developers.liveramp.com/authenticatedtraffic-api/docs/implement-ats-direct-via-api)
- [Prebid: LiveRamp RampID User ID module](https://docs.prebid.org/dev-docs/modules/userid-submodules/ramp.html)
- [Prebid: User ID module](https://docs.prebid.org/dev-docs/modules/userId.html)
