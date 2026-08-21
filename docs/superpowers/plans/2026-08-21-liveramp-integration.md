# LiveRamp Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Trusted Server's existing Prebid RampID EID path first-class by adding validated operator configuration, deterministic `identityLink` setup, bundle diagnostics, tests, and documentation.

**Architecture:** Add an optional typed `liveramp` subsection to `PrebidIntegrationConfig` and serialize it into the existing `window.__tsjs_prebid` bootstrap. The TSJS Prebid shim installs an idempotent `pbjs.setConfig` normalizer before `processQueue()`, synchronously merges the operator-owned `identityLink` entry with effective publisher User ID entries, and preserves all unrelated configuration. Existing `/auction`, OpenRTB `user.ext.eids`, `ts-eids`, consent, and EC/KV paths remain unchanged.

**Tech Stack:** Rust 2024, Serde, validator, TypeScript, Prebid.js 10, Vitest, JSDOM, Vite, VitePress/Markdown, Cargo workspace aliases.

**Specification:** `docs/superpowers/specs/2026-08-21-liveramp-integration-design.md`

---

## File structure

| File                                                                            | Responsibility                                                                                                                |
| ------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------- |
| `crates/trusted-server-core/src/integrations/prebid.rs`                         | Define and validate LiveRamp operator settings; inject the browser-safe camel-cased config; host Rust unit tests.             |
| `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`                 | Validate the injected shape, install the managed `identityLink` configuration guard, and preserve publisher User ID settings. |
| `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`           | Prove initial, queued, and late Prebid configuration behavior plus diagnostics and EID transport.                             |
| `crates/trusted-server-js/lib/test/integrations/prebid/user_id_modules.test.ts` | Characterize the existing `liveramp.com` → `identityLinkIdSystem` registry mapping.                                           |
| `crates/trusted-server-js/lib/test/build-prebid-external.test.mjs`              | Prove an external bundle can include and manifest `identityLinkIdSystem`.                                                     |
| `crates/trusted-server-js/lib/test/prebid-artifact-integration.test.mjs`        | Exercise the real generated Prebid bundle and served shim together with LiveRamp config.                                      |
| `trusted-server.example.toml`                                                   | Show safe, commented operator configuration.                                                                                  |
| `docs/guide/integrations/prebid.md`                                             | Explain LiveRamp prerequisites, configuration, lifecycle, degraded behavior, and verification.                                |
| `docs/guide/configuration.md`                                                   | Add the typed settings reference.                                                                                             |

No new integration module, route, storage schema, cookie format, or upstream HTTP client is created.

## Task 1: Add typed Rust configuration and head injection

**Files:**

- Modify: `crates/trusted-server-core/src/integrations/prebid.rs:202`
- Test: `crates/trusted-server-core/src/integrations/prebid.rs:3028`
- Test: `crates/trusted-server-core/src/integrations/prebid.rs:4010`

- [ ] **Step 1: Write failing configuration tests**

Add focused tests beside the existing Prebid TOML parsing tests:

```rust
#[test]
fn liveramp_config_parses_with_documented_defaults() {
    let config = parse_prebid_toml(
        r#"
[integrations.prebid]
server_url = "https://prebid.example/openrtb2/auction"

[integrations.prebid.liveramp]
placement_id = "999"
"#,
    );

    let liveramp = config.liveramp.expect("should parse LiveRamp config");
    assert_eq!(liveramp.placement_id, "999", "should preserve placement ID");
    assert!(!liveramp.not_use_3p, "should allow cookie recognition by default");
    assert_eq!(
        liveramp.storage_type,
        PrebidLiveRampStorageType::Cookie,
        "should default to cookie storage"
    );
    assert_eq!(liveramp.expires_days, 15, "should default to conservative expiry");
    assert_eq!(
        liveramp.refresh_in_seconds, 1800,
        "should default to LiveRamp's recommended refresh"
    );
}

#[test]
fn liveramp_config_accepts_explicit_supported_values() {
    let config = parse_prebid_toml(
        r#"
[integrations.prebid]
server_url = "https://prebid.example/openrtb2/auction"

[integrations.prebid.liveramp]
placement_id = "12345"
not_use_3p = true
storage_type = "html5"
expires_days = 30
refresh_in_seconds = 3600
"#,
    );

    let liveramp = config.liveramp.expect("should parse LiveRamp config");
    assert!(liveramp.not_use_3p, "should preserve not_use_3p");
    assert_eq!(liveramp.storage_type, PrebidLiveRampStorageType::Html5);
    assert_eq!(liveramp.expires_days, 30);
    assert_eq!(liveramp.refresh_in_seconds, 3600);
}
```

Add table-driven rejection coverage using `parse_prebid_toml_result` for:

- an otherwise valid `[integrations.prebid.liveramp]` subsection with
  `placement_id` entirely absent;
- empty, whitespace-padded, and nonnumeric `placement_id`;
- `expires_days = 0` and `expires_days = 31`;
- `refresh_in_seconds = 0`;
- unknown `storage_type`;
- unknown fields within `[integrations.prebid.liveramp]`.

Also assert that omitting the subsection leaves `config.liveramp == None`.

- [ ] **Step 2: Run the focused Rust tests and verify they fail**

Run:

```bash
cargo test-fastly liveramp_config
```

Expected: compilation/test failure because `PrebidLiveRampConfig`,
`PrebidLiveRampStorageType`, and `PrebidIntegrationConfig::liveramp` do not yet
exist.

- [ ] **Step 3: Implement the minimal typed settings**

Add near `PrebidIntegrationConfig`:

```rust
const fn default_liveramp_expires_days() -> u16 {
    15
}

const fn default_liveramp_refresh_in_seconds() -> u32 {
    1800
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PrebidLiveRampStorageType {
    #[default]
    Cookie,
    Html5,
}

#[derive(Debug, Clone, Deserialize, Serialize, Validate)]
#[serde(deny_unknown_fields)]
pub struct PrebidLiveRampConfig {
    #[validate(custom(function = "validate_liveramp_placement_id"))]
    pub placement_id: String,
    #[serde(default)]
    pub not_use_3p: bool,
    #[serde(default)]
    pub storage_type: PrebidLiveRampStorageType,
    #[serde(default = "default_liveramp_expires_days")]
    #[validate(range(min = 1, max = 30))]
    pub expires_days: u16,
    #[serde(default = "default_liveramp_refresh_in_seconds")]
    #[validate(range(min = 1))]
    pub refresh_in_seconds: u32,
}
```

Implement `validate_liveramp_placement_id` using a `ValidationError` with a
stable message. Accept only a non-empty, already-trimmed ASCII-digit string.

Add to `PrebidIntegrationConfig`:

```rust
#[serde(default)]
#[validate(nested)]
pub liveramp: Option<PrebidLiveRampConfig>,
```

Update every direct `PrebidIntegrationConfig` initializer, especially
`base_config()`, with `liveramp: None`.

- [ ] **Step 4: Run the focused configuration tests and verify they pass**

Run:

```bash
cargo test-fastly liveramp_config
```

Expected: all LiveRamp parsing/default/validation tests pass.

- [ ] **Step 5: Write failing head-injection tests**

Add tests beside the current head-injector tests:

```rust
#[test]
fn head_injector_includes_liveramp_config() {
    let mut config = base_config();
    config.liveramp = Some(PrebidLiveRampConfig {
        placement_id: "999".to_string(),
        not_use_3p: true,
        storage_type: PrebidLiveRampStorageType::Html5,
        expires_days: 30,
        refresh_in_seconds: 3600,
    });
    let integration = PrebidIntegration::new(config);
    let document_state = IntegrationDocumentState::default();
    let ctx = IntegrationHtmlContext {
        request_host: "pub.example",
        request_scheme: "https",
        origin_host: "origin.example",
        document_state: &document_state,
    };

    let script = &integration.head_inserts(&ctx)[0];
    assert!(
        script.contains(
            r#""liveRamp":{"placementId":"999","notUse3P":true,"storageType":"html5","expiresDays":30,"refreshInSeconds":3600}"#
        ),
        "should inject camel-cased LiveRamp config: {script}"
    );
}

#[test]
fn head_injector_omits_liveramp_config_when_absent() {
    // Build the normal context with base_config().
    // Assert the first insert does not contain `liveRamp`.
}

#[test]
fn head_injector_escapes_script_breakout_in_liveramp_config() {
    let mut config = base_config();
    config.liveramp = Some(PrebidLiveRampConfig {
        placement_id: "1</script><script>alert(1)</script>".to_string(),
        ..valid_liveramp_config()
    });

    // Build the normal context. Assert the injected payload contains
    // `1<\/script><script>alert(1)<\/script>` so the test cannot pass merely
    // because LiveRamp was omitted. Also assert `script.matches("</script>")`
    // has count 1: only the insert's legitimate outer closing tag remains.
    // This test may build the invalid value directly because it exercises the
    // serializer's defense in depth rather than TOML validation.
}
```

- [ ] **Step 6: Run both head-injection tests and verify they fail**

Run:

```bash
cargo test-fastly head_injector_includes_liveramp_config
cargo test-fastly head_injector_escapes_script_breakout_in_liveramp_config
```

Expected: both fail because the injected payload has no `liveRamp` property or
escaped LiveRamp Placement ID.

- [ ] **Step 7: Inject a browser-specific serialization shape**

Inside `IntegrationHeadInjector::head_inserts`, define a borrowed injected
shape so TOML remains snake_case while browser JSON is camelCase:

```rust
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InjectedPrebidLiveRampConfig<'a> {
    placement_id: &'a str,
    not_use_3p: bool,
    storage_type: PrebidLiveRampStorageType,
    expires_days: u16,
    refresh_in_seconds: u32,
}
```

Add a skipped-when-absent `live_ramp` field to
`InjectedPrebidClientConfig`, map `self.config.liveramp.as_ref()` into the
borrowed shape, and retain the existing `</` escaping on the final serialized
payload. Do not expose envelope values or add a separate inline script.

- [ ] **Step 8: Re-run both head-injection tests, then all Prebid Rust tests**

Run:

```bash
cargo test-fastly head_injector_includes_liveramp_config
cargo test-fastly head_injector_escapes_script_breakout_in_liveramp_config
cargo test-fastly integrations::prebid::tests
```

Expected: both focused tests and all Prebid Rust tests pass, including the existing
`to_openrtb_includes_eids_from_auction_request` LiveRamp regression.

- [ ] **Step 9: Format and commit Task 1**

Run:

```bash
cargo fmt --all -- --check
git add crates/trusted-server-core/src/integrations/prebid.rs
git commit -m "feat: configure LiveRamp for managed Prebid"
```

## Task 2: Install an operator-owned Prebid `identityLink` configuration

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts:146`
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts:403`
- Test: `crates/trusted-server-js/lib/test/prebid-artifact-integration.test.mjs:30`

- [ ] **Step 1: Reset mutable Prebid methods in the test harness**

The new feature intentionally wraps `pbjs.setConfig`. In each relevant
`beforeEach`, restore:

```typescript
mockPbjs.setConfig = mockSetConfig
mockPbjs.processQueue = mockProcessQueue
delete mockPbjs['__tsLiveRampSetConfigInstalled']
```

Continue resetting the page-level `__tsjsPrebidShimInstalled` sentinel. This
prevents one test's wrapper from leaking into another test.

- [ ] **Step 2: Write failing unit tests for configuration ownership**

Extend `InjectedPrebidConfig` in the test window type and add a helper fixture:

```typescript
const LIVE_RAMP_CONFIG = {
  placementId: '999',
  notUse3P: false,
  storageType: 'cookie',
  expiresDays: 15,
  refreshInSeconds: 1800,
}

const EXPECTED_IDENTITY_LINK = {
  name: 'identityLink',
  params: { pid: '999', notUse3P: false },
  storage: {
    type: 'cookie',
    name: 'idl_env',
    expires: 15,
    refreshInSeconds: 1800,
  },
}
```

Add tests proving:

1. Absent `liveRamp` config does not replace `mockPbjs.setConfig` and adds no
   `identityLink` call.
2. Already-effective `getConfig('userSync.userIds')` entries are preserved and
   an existing `identityLink` is replaced exactly once.
3. An existing malformed effective value degrades to only the managed entry
   without throwing; malformed list members are dropped.
4. The initial managed call occurs before `processQueue()`.
5. A queued publisher `setConfig({userSync:{userIds:[...]}})` followed by a
   queued `requestBids` sees preserved non-LiveRamp entries and the managed
   entry.
6. A publisher `setConfig` after `processQueue()` cannot delete or replace the
   managed entry.
7. Calls unrelated to `userSync.userIds` retain their exact object shape.
8. A second installation does not stack wrappers.
9. The diagnostics report configured name `identityLink` and report it missing
   when the external manifest omits `identityLinkIdSystem`.
10. Empty-string and structurally malformed LiveRamp EIDs are dropped before
    the auction request is serialized.
11. A valid opaque LiveRamp envelope survives the `ts-eids` cookie round trip
    byte-for-byte without being decoded.

Use a local `mockProcessQueue` implementation that drains `mockPbjs.que` in
insertion order for the ordering test instead of merely asserting calls.

- [ ] **Step 3: Write a failing current-auction LiveRamp EID test**

Add an explicit case beside `buildRequests includes current Prebid EIDs`:

```typescript
it('forwards the opaque LiveRamp envelope as a liveramp.com EID', () => {
  const spec = getAdapterSpec()
  mockGetUserIdsAsEids.mockReturnValue([
    {
      source: 'liveramp.com',
      uids: [{ id: 'opaque-test-envelope', atype: 3 }],
    },
  ])

  const request = spec.buildRequests([
    {
      adUnitCode: 'div-gpt-1',
      bidId: 'bid-1',
      sizes: [[300, 250]],
      bidder: 'trustedServer',
      params: {},
    },
  ])

  expect(JSON.parse(request.data).eids).toEqual([
    {
      source: 'liveramp.com',
      uids: [{ id: 'opaque-test-envelope', atype: 3 }],
    },
  ])
})
```

Add a logging assertion using a sentinel envelope and spies on `log.debug`,
`log.info`, `log.warn`, and `log.error`; no logged argument may contain the
sentinel.

- [ ] **Step 4: Write the failing real-artifact test before implementation**

Change the bundle built in `prebid-artifact-integration.test.mjs` to include
both `sharedIdSystem` and `identityLinkIdSystem`. Inject `liveRamp` before
evaluating the served shim, then assert after shim evaluation:

```javascript
const configuredUserIds = pageWindow.pbjs.getConfig('userSync.userIds')
expect(configuredUserIds).toEqual(
  expect.arrayContaining([
    expect.objectContaining({
      name: 'identityLink',
      params: { pid: '999', notUse3P: false },
      storage: expect.objectContaining({ name: 'idl_env' }),
    }),
  ])
)
```

Retain the current real `/auction` request assertion. Network remains stubbed;
the test must not contact LiveRamp.

- [ ] **Step 5: Run the focused unit and artifact suites and verify failures**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/prebid/index.test.ts
npx vitest run test/prebid-artifact-integration.test.mjs
```

Expected: LiveRamp ownership tests fail because the injected shape and
`setConfig` normalizer do not exist, and the artifact test fails because no
managed entry is installed. The transport-only characterizations may already
pass; retain them as evidence of the pre-existing path.

- [ ] **Step 6: Add the injected TypeScript types and constants**

Add:

```typescript
interface InjectedLiveRampConfig {
  placementId: string
  notUse3P: boolean
  storageType: 'cookie' | 'html5'
  expiresDays: number
  refreshInSeconds: number
}

interface InjectedPrebidConfig {
  // Existing fields omitted.
  liveRamp?: InjectedLiveRampConfig
}

const IDENTITY_LINK_CONFIG_NAME = 'identityLink'
const IDENTITY_LINK_STORAGE_NAME = 'idl_env'
const LIVE_RAMP_SET_CONFIG_SENTINEL = '__tsLiveRampSetConfigInstalled'
```

Keep this internal to the Prebid module; do not add a new global API.

- [ ] **Step 7: Extract reusable User ID list parsing**

Refactor the shape handling currently embedded in
`configuredUserIdNamesFromConfig` into a helper that returns validated entry
objects from any of these inputs:

- the direct array returned by `getConfig('userSync.userIds')`;
- `{ userSync: { userIds: [...] } }`;
- `{ userIds: [...] }`.

Use explicit record and entry guards. A valid entry is a non-array object with
a non-empty string `name`; filter malformed members rather than forwarding
them. The parser returns an empty array for malformed containers. Keep a
separate `hasUserIdsPath(config)` predicate equivalent to:

```typescript
function hasUserIdsPath(config: unknown): config is {
  userSync: Record<string, unknown> & { userIds: unknown }
} {
  return (
    isRecord(config) &&
    isRecord(config.userSync) &&
    Object.prototype.hasOwnProperty.call(config.userSync, 'userIds')
  )
}
```

This distinction is required: an absent path passes through unchanged, while
an explicitly empty `userIds: []` is normalized to the managed entry. Update
`configuredUserIdNamesFromConfig` to derive names from that helper so
diagnostics and LiveRamp normalization agree on supported shapes.

- [ ] **Step 8: Implement the managed entry and normalizer**

Implement small focused helpers equivalent to:

```typescript
function liveRampUserId(
  config: InjectedLiveRampConfig
): Record<string, unknown> {
  return {
    name: IDENTITY_LINK_CONFIG_NAME,
    params: { pid: config.placementId, notUse3P: config.notUse3P },
    storage: {
      type: config.storageType,
      name: IDENTITY_LINK_STORAGE_NAME,
      expires: config.expiresDays,
      refreshInSeconds: config.refreshInSeconds,
    },
  }
}

function withManagedLiveRampUserId(
  config: Record<string, unknown>,
  managedEntry: Record<string, unknown>
): Record<string, unknown> {
  if (!hasUserIdsPath(config)) return config

  const retained = configuredUserIdEntries(config.userSync.userIds).filter(
    (entry) => entry.name !== IDENTITY_LINK_CONFIG_NAME
  )
  return {
    ...config,
    userSync: {
      ...config.userSync,
      userIds: [...retained, managedEntry],
    },
  }
}
```

`configuredUserIdEntries` must support the three shapes from Step 7 and return
fresh arrays. The spread operations preserve top-level properties and sibling
`userSync` properties. Do not mutate publisher-owned arrays or objects in
place.

- [ ] **Step 9: Install the idempotent `setConfig` guard before queue processing**

In `installPrebidNpm`, after confirming the real Prebid API and before the
existing base configuration and `processQueue()` call:

1. If injected `liveRamp` is absent, do nothing.
2. Capture and bind the current `pbjs.setConfig`.
3. Replace it with a wrapper that normalizes only calls containing
   `userSync.userIds`; pass all other calls through unchanged.
4. Mark the Prebid object with the sentinel so installation cannot stack.
5. Read effective User ID entries through `pbjs.getConfig`.
6. Call the wrapper synchronously with the effective list, producing one
   managed entry before any queued auction.
7. Leave the wrapper installed across `processQueue()` and later calls.

Use logic equivalent to:

```typescript
const managedPbjs = pbjs as typeof pbjs & Record<string, unknown>
if (managedPbjs[LIVE_RAMP_SET_CONFIG_SENTINEL] !== true) {
  const originalSetConfig = pbjs.setConfig.bind(pbjs)
  const managedEntry = liveRampUserId(config.liveRamp)

  pbjs.setConfig = (publisherConfig) => {
    let nextConfig = publisherConfig
    try {
      if (hasUserIdsPath(publisherConfig)) {
        nextConfig = withManagedLiveRampUserId(publisherConfig, managedEntry)
      }
    } catch {
      log.error('Prebid LiveRamp configuration could not be normalized')
    }
    return originalSetConfig(nextConfig)
  }
  managedPbjs[LIVE_RAMP_SET_CONFIG_SENTINEL] = true

  const effective = configuredUserIdEntries(pbjs.getConfig('userSync.userIds'))
  pbjs.setConfig({ userSync: { userIds: effective } })
}
```

Adapt the callback and return types to the repository's actual `pbjs` typing.
The original method is invoked exactly once, its return value is preserved,
and normalization errors never log values. The sentinel lives on `pbjs`, not
on the page-level shim state: a test must deliberately reset only
`__tsjsPrebidShimInstalled`, reinstall, and prove the wrapper reference is
unchanged and publisher calls are normalized once.

- [ ] **Step 10: Run focused unit and artifact tests and make them pass**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/prebid/index.test.ts
npx vitest run test/prebid-artifact-integration.test.mjs
```

Expected: unit tests pass; the real bundle advertises both User ID modules, the
shim configures one `identityLink` entry, and the controlled auction still
reaches `/auction`.

- [ ] **Step 11: Format, lint, and commit Task 2**

Run:

```bash
cd crates/trusted-server-js/lib
npm run format
npm run lint
git add src/integrations/prebid/index.ts test/integrations/prebid/index.test.ts test/prebid-artifact-integration.test.mjs
git commit -m "feat: manage LiveRamp identityLink configuration"
```

## Task 3: Lock bundle, transport, consent, and EC behavior with regression tests

**Files:**

- Test: `crates/trusted-server-js/lib/test/integrations/prebid/user_id_modules.test.ts`
- Test: `crates/trusted-server-js/lib/test/build-prebid-external.test.mjs`
- Test: `crates/trusted-server-core/src/auction/endpoints.rs`
- Test: `crates/trusted-server-core/src/consent/mod.rs`
- Test: `crates/trusted-server-core/src/ec/prebid_eids.rs`

- [ ] **Step 1: Add the explicit registry mapping test**

```typescript
it('maps LiveRamp EIDs to identityLinkIdSystem', () => {
  expect(
    resolvePrebidUserIdModulesFromEids([
      { source: 'liveramp.com', uids: [{ id: 'opaque-envelope', atype: 3 }] },
    ])
  ).toEqual({
    modules: ['userId', 'identityLinkIdSystem'],
    missingSources: [],
  })
})
```

Also load the checked-in registry JSON or expose a narrow helper and assert the
default preset contains `identityLinkIdSystem`. Do not duplicate the registry
as a second production constant.

- [ ] **Step 2: Add a bundle manifest test for `identityLinkIdSystem`**

Extend the existing `includes generated User ID metadata` case or add a focused
case that invokes:

```javascript
await main([
  '--adapters',
  'rubicon',
  '--user-id-modules',
  'identityLinkIdSystem',
  '--out',
  outputDirectory,
])
```

Assert the manifest's `userIdModules` is exactly
`['identityLinkIdSystem']` and the generated bundle contains the module name.

- [ ] **Step 3: Run the two characterization suites**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run \
  test/integrations/prebid/user_id_modules.test.ts \
  test/build-prebid-external.test.mjs
```

Expected: tests pass using the existing registry and generator. If they fail,
fix the single checked-in registry/generator source rather than introducing a
LiveRamp-only bundle path.

- [ ] **Step 4: Add or rename LiveRamp-specific Rust regression fixtures**

Add focused tests (or rename/extend an existing generic fixture while keeping
its broader assertions) proving:

- in `auction/endpoints.rs`, a client `liveramp.com` UID equal to the resolved
  KV UID is merged once, with server-resolved metadata winning on conflict;
- in `consent/mod.rs`, a `liveramp.com` EID is removed when consent denies
  identity forwarding;
- in `ec/prebid_eids.rs`, a structured `ts-eids` cookie containing an opaque
  `liveramp.com` envelope writes that exact opaque string once to a registry
  partner whose source domain is `liveramp.com`.

Use only synthetic values such as `opaque-test-envelope`. Assert that tests do
not decode or inspect an envelope's contents.

- [ ] **Step 5: Run the LiveRamp Rust regression fixtures**

Run from the repository root:

```bash
cargo test-fastly liveramp
```

Expected: forwarding, merge/deduplication, consent removal, and later-request
EC ingestion fixtures all pass.

- [ ] **Step 6: Commit Task 3**

Run:

```bash
git add crates/trusted-server-js/lib/test/integrations/prebid/user_id_modules.test.ts \
  crates/trusted-server-js/lib/test/build-prebid-external.test.mjs \
  crates/trusted-server-core/src/auction/endpoints.rs \
  crates/trusted-server-core/src/consent/mod.rs \
  crates/trusted-server-core/src/ec/prebid_eids.rs
git commit -m "test: cover LiveRamp Prebid bundle support"
```

## Task 4: Document configuration, lifecycle, and operational validation

**Files:**

- Modify: `trusted-server.example.toml:41`
- Modify: `docs/guide/integrations/prebid.md:50`
- Modify: `docs/guide/integrations/prebid.md:419`
- Modify: `docs/guide/configuration.md:1050`

- [ ] **Step 1: Add the commented example configuration**

Add beneath the Prebid bundle configuration in `trusted-server.example.toml`:

```toml
# Optional managed LiveRamp RampID configuration. The external Prebid bundle
# must contain identityLinkIdSystem. Obtain the Placement ID and approve the
# publisher origin with LiveRamp before enabling.
# [integrations.prebid.liveramp]
# placement_id = "999"
# not_use_3p = false
# storage_type = "cookie"
# expires_days = 15
# refresh_in_seconds = 1800
```

Do not add a real Placement ID or credential.

- [ ] **Step 2: Update the configuration reference**

Add `[integrations.prebid.liveramp]` fields to both Prebid option tables:

| Field                         | Type                | Default                         | Description                                                  |
| ----------------------------- | ------------------- | ------------------------------- | ------------------------------------------------------------ |
| `liveramp.placement_id`       | String              | Required when subsection exists | Numeric LiveRamp Placement ID for `identityLink`.            |
| `liveramp.not_use_3p`         | Boolean             | `false`                         | Disable cookie-recognized RampID envelopes when true.        |
| `liveramp.storage_type`       | `cookie` or `html5` | `cookie`                        | Browser storage used by the Prebid module.                   |
| `liveramp.expires_days`       | Integer 1–30        | `15`                            | Envelope storage lifetime in days.                           |
| `liveramp.refresh_in_seconds` | Positive integer    | `1800`                          | Interval before retrieving a potentially refreshed envelope. |

State that storage name `idl_env` is fixed by the integration.

- [ ] **Step 3: Add the LiveRamp guide section**

In `docs/guide/integrations/prebid.md`, document:

- prerequisites: Placement ID, approved origin, CMP/LiveRamp consent posture,
  and an external bundle containing `identityLinkIdSystem`;
- the exact TOML example and `ts prebid bundle` selection;
- operator ownership of the single `identityLink` entry while preserving other
  User ID modules;
- asynchronous resolution: a new browser's first auction may have no RampID;
- the existing flow through `getUserIdsAsEids()`, `/auction`,
  `user.ext.eids`, `ts-eids`, and EC/KV;
- degraded behavior for no consent, no recognition, missing module, LiveRamp
  network failure, and KV failure;
- privacy guidance: envelopes are opaque and must not be logged;
- the explicit product boundary: this forwards RampID EIDs, not ATS Direct
  audience segments;
- a credential-based manual validation checklist matching Section 11.5 of the
  design spec, recording only booleans, counts, source names, and status codes.

- [ ] **Step 4: Format and verify docs**

Run:

```bash
cd docs
npm run format
```

Expected: all documentation and TOML examples satisfy Prettier checks.

- [ ] **Step 5: Commit Task 4**

Run:

```bash
git add trusted-server.example.toml docs/guide/integrations/prebid.md docs/guide/configuration.md
git commit -m "docs: explain managed LiveRamp RampID setup"
```

## Task 5: Run full verification and prepare live validation handoff

**Files:**

- Verify: all files changed in Tasks 1–4
- Reference: `docs/superpowers/specs/2026-08-21-liveramp-integration-design.md`

- [ ] **Step 1: Run the complete TSJS test and build gates**

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run
npm run format
npm run lint
node build-all.mjs
```

Expected: all commands exit 0.

- [ ] **Step 2: Run Rust formatting and adapter test gates**

From the repository root, run:

```bash
cargo fmt --all -- --check
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
```

Expected: all commands exit 0. Do not substitute bare
`cargo test --workspace`.

- [ ] **Step 3: Run all target-matched clippy gates**

Run:

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

Expected: all commands exit 0 with warnings denied.

- [ ] **Step 4: Re-run documentation formatting and inspect the final diff**

Run:

```bash
cd docs
npm run format
cd ..
git diff --check
git status --short
git diff main...HEAD --stat
```

Expected: formatting and diff checks pass; status contains only intentional
plan/implementation changes.

- [ ] **Step 5: Record credential-gated validation status**

If LiveRamp test configuration is available, execute the guide's manual
validation on an approved non-production origin and report only:

- approved origin used (domain, not credentials);
- whether `idl_env` was created/refreshed;
- whether `getUserIdsAsEids()` exposed source `liveramp.com`;
- whether the controlled PBS request contained that source;
- whether a later request ingested the EID into the configured
  `liveramp.com` EC partner;
- whether opt-out removed it; and
- whether an unapproved origin degraded to no LiveRamp EID without blocking
  the auction; and
- status codes/counts without envelope values.

If credentials remain unavailable, report exactly: “Code complete; live
LiveRamp validation pending IABTechLab/uid2-optout#385.” Do not block automated
verification or add fake live-success evidence.

In either case, prepare the explicit parent-epic acceptance handoff: “RampID
identity envelopes traverse the existing Prebid auction path; ATS Direct
audience segments are not passed by this implementation.”

- [ ] **Step 6: Commit any verification-only corrections**

Only if verification required source changes, repeat the affected focused and
full gates, then commit the minimal correction:

```bash
git add <exact-corrected-files>
git commit -m "fix: address LiveRamp verification findings"
```

Do not create an empty verification commit.
