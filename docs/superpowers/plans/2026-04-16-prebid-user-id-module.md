# Prebid User ID Module — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bundle Prebid's User ID core module and a broad set of ID submodules so publisher-side `pbjs.setConfig({ userSync: { userIds: [...] } })` calls activate real ID resolution and the existing `syncPrebidEidsCookie()` helper begins writing `ts-eids`.

**Architecture:** JS-only change. Add 15 static imports (1 core + 14 submodules) to `crates/js/lib/src/integrations/prebid/index.ts`. No Rust changes, no TOML changes, no new runtime logic. Existing `bidsBackHandler` shim and cookie-sync path already handle the rest; they were silent only because `pbjs.getUserIdsAsEids` did not exist.

> **In-flight correction (2026-04-16):** `pubCommonIdSystem.js` was originally
> included in the plan as a legacy/compatibility submodule. It does not exist
> in Prebid 10.26.0 (consolidated into `sharedIdSystem`) and has been removed.
> All downstream counts reflect 14 submodules instead of 15.

**Tech Stack:** TypeScript, Vitest, esbuild (via `build-all.mjs`), Prebid.js 9.x (via npm)

**Spec:** `docs/superpowers/specs/2026-04-16-prebid-user-id-module-design.md`

---

## File Map

| File                                                   | Action | Responsibility                                                                                                                                  |
| ------------------------------------------------------ | ------ | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/js/lib/src/integrations/prebid/index.ts`       | Modify | Add 15 User ID Module imports alongside existing `consentManagement*` imports                                                                   |
| `crates/js/lib/test/integrations/prebid/index.test.ts` | Modify | Add `vi.mock` stubs for the 15 new modules, add `mockPbjs.getUserIdsAsEids`, add new tests for cookie-sync behavior and import regression guard |

No other files change. `build.rs` picks up the rebuilt `dist/tsjs-prebid.js` automatically via `include_str!`.

---

## Task 1: Document existing `syncPrebidEidsCookie` behavior with tests

The sync helper already exists but has no test coverage. Before changing anything, lock in its current contract so we can refactor or extend later without regressions. These tests exercise the `bidsBackHandler` shim path end-to-end using the existing mocks.

**Files:**

- Modify: `crates/js/lib/test/integrations/prebid/index.test.ts`

- [ ] **Step 1: Add `getUserIdsAsEids` to the hoisted pbjs mock**

In `crates/js/lib/test/integrations/prebid/index.test.ts` inside the existing `vi.hoisted(() => { ... })` block, add a new mock function and include it on `mockPbjs`. Replace the current block with:

```ts
const {
  mockSetConfig,
  mockProcessQueue,
  mockRequestBids,
  mockRegisterBidAdapter,
  mockGetUserIdsAsEids,
  mockPbjs,
  mockGetBidAdapter,
  mockAdapterManager,
} = vi.hoisted(() => {
  const mockSetConfig = vi.fn()
  const mockProcessQueue = vi.fn()
  const mockRequestBids = vi.fn()
  const mockRegisterBidAdapter = vi.fn()
  const mockGetBidAdapter = vi.fn()
  const mockGetUserIdsAsEids = vi.fn(
    () =>
      [] as Array<{
        source: string
        uids?: Array<{ id: string; atype?: number }>
      }>
  )
  const mockPbjs = {
    setConfig: mockSetConfig,
    processQueue: mockProcessQueue,
    requestBids: mockRequestBids,
    registerBidAdapter: mockRegisterBidAdapter,
    getUserIdsAsEids: mockGetUserIdsAsEids,
    adUnits: [] as any[],
  }
  const mockAdapterManager = {
    getBidAdapter: mockGetBidAdapter,
  }
  return {
    mockSetConfig,
    mockProcessQueue,
    mockRequestBids,
    mockRegisterBidAdapter,
    mockGetUserIdsAsEids,
    mockPbjs,
    mockGetBidAdapter,
    mockAdapterManager,
  }
})
```

- [ ] **Step 2: Write the failing test — empty EID array writes no cookie**

Append this new `describe` block at the end of `crates/js/lib/test/integrations/prebid/index.test.ts`:

```ts
describe('prebid/syncPrebidEidsCookie (via bidsBackHandler)', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    mockPbjs.requestBids = mockRequestBids
    mockPbjs.adUnits = []
    mockGetUserIdsAsEids.mockReset()
    mockGetUserIdsAsEids.mockReturnValue([])
    // Restore the pbjs→mock wiring in case a prior test blanked it out.
    ;(mockPbjs as any).getUserIdsAsEids = mockGetUserIdsAsEids
    delete (window as any).__tsjs_prebid
    // Wipe any leftover ts-eids cookie from previous tests.
    document.cookie = 'ts-eids=; Path=/; Max-Age=0'
  })

  afterEach(() => {
    document.cookie = 'ts-eids=; Path=/; Max-Age=0'
  })

  /**
   * Helper: make mockRequestBids actually invoke the injected bidsBackHandler
   * so the shim's post-auction sync path runs.
   */
  function wireBidsBackHandler(): void {
    mockRequestBids.mockImplementation((opts: any) => {
      if (typeof opts?.bidsBackHandler === 'function') {
        opts.bidsBackHandler()
      }
    })
  }

  function getTsEidsCookie(): string | undefined {
    const match = document.cookie
      .split('; ')
      .find((c) => c.startsWith('ts-eids='))
    return match ? match.split('=').slice(1).join('=') : undefined
  }

  it('writes no cookie when getUserIdsAsEids returns empty array', () => {
    wireBidsBackHandler()
    const pbjs = installPrebidNpm()
    mockGetUserIdsAsEids.mockReturnValue([])

    pbjs.requestBids({ adUnits: [] } as any)

    expect(getTsEidsCookie()).toBeUndefined()
  })
})
```

- [ ] **Step 3: Run the test — expect PASS (documents existing behavior)**

Run: `cd crates/js/lib && npx vitest run test/integrations/prebid/index.test.ts -t "writes no cookie when getUserIdsAsEids returns empty array"`

Expected: PASS. The shim already calls `syncPrebidEidsCookie` which early-returns on empty input.

- [ ] **Step 4: Add test — writes base64 cookie for a normal payload**

Append inside the same `describe` block:

```ts
it('writes ts-eids cookie with base64-encoded flat JSON for normal payload', () => {
  wireBidsBackHandler()
  const pbjs = installPrebidNpm()
  mockGetUserIdsAsEids.mockReturnValue([
    { source: 'sharedid.org', uids: [{ id: 'shared-abc', atype: 1 }] },
    { source: 'id5-sync.com', uids: [{ id: 'id5-xyz', atype: 3 }] },
  ])

  pbjs.requestBids({ adUnits: [] } as any)

  const encoded = getTsEidsCookie()
  expect(encoded).toBeDefined()
  const decoded = JSON.parse(atob(encoded!))
  expect(decoded).toEqual([
    { source: 'sharedid.org', id: 'shared-abc', atype: 1 },
    { source: 'id5-sync.com', id: 'id5-xyz', atype: 3 },
  ])
})
```

- [ ] **Step 5: Run and confirm PASS**

Run: `cd crates/js/lib && npx vitest run test/integrations/prebid/index.test.ts -t "writes ts-eids cookie with base64"`

Expected: PASS.

- [ ] **Step 6: Add test — defaults atype to 3 when missing**

Append:

```ts
it('defaults atype to 3 when the uid omits it', () => {
  wireBidsBackHandler()
  const pbjs = installPrebidNpm()
  mockGetUserIdsAsEids.mockReturnValue([
    { source: 'example.com', uids: [{ id: 'no-atype' }] },
  ])

  pbjs.requestBids({ adUnits: [] } as any)

  const decoded = JSON.parse(atob(getTsEidsCookie()!))
  expect(decoded).toEqual([{ source: 'example.com', id: 'no-atype', atype: 3 }])
})
```

- [ ] **Step 7: Add test — skips entries without an id or source**

Append:

```ts
it('skips EID entries that are missing id or source', () => {
  wireBidsBackHandler()
  const pbjs = installPrebidNpm()
  mockGetUserIdsAsEids.mockReturnValue([
    { source: 'good.example', uids: [{ id: 'keep', atype: 1 }] },
    { source: 'empty-uids.example', uids: [] },
    { source: '', uids: [{ id: 'no-source', atype: 1 }] },
    { source: 'no-id.example', uids: [{ id: '', atype: 1 }] },
  ])

  pbjs.requestBids({ adUnits: [] } as any)

  const decoded = JSON.parse(atob(getTsEidsCookie()!))
  expect(decoded).toEqual([{ source: 'good.example', id: 'keep', atype: 1 }])
})
```

- [ ] **Step 8: Add test — takes first uid when multiple are present**

Append:

```ts
it('takes the first uid per source when multiple are present', () => {
  wireBidsBackHandler()
  const pbjs = installPrebidNpm()
  mockGetUserIdsAsEids.mockReturnValue([
    {
      source: 'multi.example',
      uids: [
        { id: 'first', atype: 1 },
        { id: 'second', atype: 2 },
      ],
    },
  ])

  pbjs.requestBids({ adUnits: [] } as any)

  const decoded = JSON.parse(atob(getTsEidsCookie()!))
  expect(decoded).toEqual([{ source: 'multi.example', id: 'first', atype: 1 }])
})
```

- [ ] **Step 9: Add test — trims tail when payload exceeds 3072 bytes**

Append:

```ts
it('trims EIDs from the tail when the cookie payload would exceed 3072 bytes', () => {
  wireBidsBackHandler()
  const pbjs = installPrebidNpm()

  // Build ~20 entries each ~200 bytes → definitely exceeds 3072-byte cap
  // once base64-encoded.
  const big = Array.from({ length: 20 }, (_, i) => ({
    source: `source-${i}.example`,
    uids: [{ id: 'x'.repeat(200) + String(i), atype: 3 }],
  }))
  mockGetUserIdsAsEids.mockReturnValue(big)

  pbjs.requestBids({ adUnits: [] } as any)

  const encoded = getTsEidsCookie()
  expect(encoded).toBeDefined()
  expect(encoded!.length).toBeLessThanOrEqual(3072)

  const decoded = JSON.parse(atob(encoded!))
  // At least one entry kept, strictly fewer than original count.
  expect(decoded.length).toBeGreaterThan(0)
  expect(decoded.length).toBeLessThan(big.length)
  // Head of the list is preserved (trimming happens from the tail).
  expect(decoded[0].source).toBe('source-0.example')
})
```

- [ ] **Step 10: Add test — writes no cookie when a single entry alone exceeds the cap**

Append:

```ts
it('writes no cookie when a single entry alone exceeds the cap', () => {
  wireBidsBackHandler()
  const pbjs = installPrebidNpm()

  // Single entry large enough to blow past 3072 bytes after base64.
  mockGetUserIdsAsEids.mockReturnValue([
    { source: 'too-big.example', uids: [{ id: 'x'.repeat(4000), atype: 3 }] },
  ])

  pbjs.requestBids({ adUnits: [] } as any)

  expect(getTsEidsCookie()).toBeUndefined()
})
```

- [ ] **Step 11: Add test — does not throw when getUserIdsAsEids is undefined**

This mirrors the pre-fix production state and guards against regressions in the defensive check at `index.ts:375`. Append:

```ts
it('does not throw when getUserIdsAsEids is undefined (pre-fix production state)', () => {
  wireBidsBackHandler()
  const pbjs = installPrebidNpm()
  // Simulate a build that forgot the userId core module.
  ;(mockPbjs as any).getUserIdsAsEids = undefined

  expect(() => pbjs.requestBids({ adUnits: [] } as any)).not.toThrow()
  expect(getTsEidsCookie()).toBeUndefined()

  // Restore for subsequent tests.
  ;(mockPbjs as any).getUserIdsAsEids = mockGetUserIdsAsEids
})
```

- [ ] **Step 12: Add test — calls the original bidsBackHandler when one was supplied**

Append:

```ts
it('calls the original bidsBackHandler after syncing EIDs', () => {
  wireBidsBackHandler()
  const pbjs = installPrebidNpm()
  const originalHandler = vi.fn()

  pbjs.requestBids({ adUnits: [], bidsBackHandler: originalHandler } as any)

  expect(originalHandler).toHaveBeenCalledTimes(1)
})
```

- [ ] **Step 13: Run the full new block and confirm all pass**

Run: `cd crates/js/lib && npx vitest run test/integrations/prebid/index.test.ts -t "syncPrebidEidsCookie"`

Expected: all 9 new tests PASS. If any fail, investigate before proceeding — the rest of the plan assumes this behavior is locked in.

- [ ] **Step 14: Run the entire prebid test file to confirm no regressions**

Run: `cd crates/js/lib && npx vitest run test/integrations/prebid/index.test.ts`

Expected: all tests PASS (new + existing).

- [ ] **Step 15: Commit**

```bash
git add crates/js/lib/test/integrations/prebid/index.test.ts
git commit -m "Add Vitest coverage for Prebid ts-eids cookie sync"
```

---

## Task 2: Add Prebid User ID core and submodule imports

This is the substantive change. Add `vi.mock` stubs for the new modules first (so tests don't blow up when the imports are added), then add the imports.

**Files:**

- Modify: `crates/js/lib/test/integrations/prebid/index.test.ts:44-47`
- Modify: `crates/js/lib/src/integrations/prebid/index.ts:16-18`

- [ ] **Step 1: Add `vi.mock` stubs in the test file for all 15 new modules**

In `crates/js/lib/test/integrations/prebid/index.test.ts`, locate the existing block (around line 44-47):

```ts
// Side-effect imports are no-ops in tests
vi.mock('prebid.js/modules/consentManagementTcf.js', () => ({}))
vi.mock('prebid.js/modules/consentManagementGpp.js', () => ({}))
vi.mock('prebid.js/modules/consentManagementUsp.js', () => ({}))
```

Replace it with (consent management mocks stay; add 15 new ones):

```ts
// Side-effect imports are no-ops in tests
vi.mock('prebid.js/modules/consentManagementTcf.js', () => ({}))
vi.mock('prebid.js/modules/consentManagementGpp.js', () => ({}))
vi.mock('prebid.js/modules/consentManagementUsp.js', () => ({}))

// User ID Module core + submodules — no-op mocks so jsdom does not try to
// execute the real Prebid code paths.
vi.mock('prebid.js/modules/userId.js', () => ({}))
vi.mock('prebid.js/modules/sharedIdSystem.js', () => ({}))
vi.mock('prebid.js/modules/criteoIdSystem.js', () => ({}))
vi.mock('prebid.js/modules/33acrossIdSystem.js', () => ({}))
vi.mock('prebid.js/modules/pubProvidedIdSystem.js', () => ({}))
vi.mock('prebid.js/modules/quantcastIdSystem.js', () => ({}))
vi.mock('prebid.js/modules/id5IdSystem.js', () => ({}))
vi.mock('prebid.js/modules/identityLinkIdSystem.js', () => ({}))
vi.mock('prebid.js/modules/liveIntentIdSystem.js', () => ({}))
vi.mock('prebid.js/modules/uid2IdSystem.js', () => ({}))
vi.mock('prebid.js/modules/euidIdSystem.js', () => ({}))
vi.mock('prebid.js/modules/intentIqIdSystem.js', () => ({}))
vi.mock('prebid.js/modules/lotamePanoramaIdSystem.js', () => ({}))
vi.mock('prebid.js/modules/connectIdSystem.js', () => ({}))
vi.mock('prebid.js/modules/merkleIdSystem.js', () => ({}))
vi.mock('prebid.js/modules/pubCommonIdSystem.js', () => ({}))
```

- [ ] **Step 2: Write the failing regression-guard test**

Append this `describe` block to `crates/js/lib/test/integrations/prebid/index.test.ts`:

```ts
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { resolve } from 'node:path'

describe('prebid/index.ts User ID Module imports (regression guard)', () => {
  const REQUIRED_IMPORTS = [
    'prebid.js/modules/userId.js',
    'prebid.js/modules/sharedIdSystem.js',
    'prebid.js/modules/criteoIdSystem.js',
    'prebid.js/modules/33acrossIdSystem.js',
    'prebid.js/modules/pubProvidedIdSystem.js',
    'prebid.js/modules/quantcastIdSystem.js',
    'prebid.js/modules/id5IdSystem.js',
    'prebid.js/modules/identityLinkIdSystem.js',
    'prebid.js/modules/liveIntentIdSystem.js',
    'prebid.js/modules/uid2IdSystem.js',
    'prebid.js/modules/euidIdSystem.js',
    'prebid.js/modules/intentIqIdSystem.js',
    'prebid.js/modules/lotamePanoramaIdSystem.js',
    'prebid.js/modules/connectIdSystem.js',
    'prebid.js/modules/merkleIdSystem.js',
    'prebid.js/modules/pubCommonIdSystem.js',
  ]

  // Source-text check: these mocks make the runtime pbjs mock a no-op for the
  // User ID Module, so there is no way to assert `typeof getUserIdsAsEids ===
  // 'function'` at import time from within Vitest. Reading the source file
  // directly is the most reliable way to catch accidental removal of an
  // import, which is the exact regression that motivated this work.
  // The package is ESM (`"type": "module"`), so `__dirname` is not defined —
  // resolve relative to this file via `import.meta.url`.
  const THIS_DIR = fileURLToPath(new URL('.', import.meta.url))
  const SOURCE_PATH = resolve(
    THIS_DIR,
    '../../../src/integrations/prebid/index.ts'
  )
  const source = readFileSync(SOURCE_PATH, 'utf8')

  for (const module of REQUIRED_IMPORTS) {
    it(`statically imports ${module}`, () => {
      const pattern = new RegExp(
        `import\\s+['"]${module.replace(/\./g, '\\.')}['"]`
      )
      expect(source).toMatch(pattern)
    })
  }
})
```

- [ ] **Step 3: Run the new block — expect 15 failures**

Run: `cd crates/js/lib && npx vitest run test/integrations/prebid/index.test.ts -t "regression guard"`

Expected: FAIL — 15 failing assertions, one per expected import. This confirms the regression guard actually reads the source.

- [ ] **Step 4: Add the 15 imports to `index.ts`**

In `crates/js/lib/src/integrations/prebid/index.ts`, locate lines 16-18:

```ts
import 'prebid.js/modules/consentManagementTcf.js'
import 'prebid.js/modules/consentManagementGpp.js'
import 'prebid.js/modules/consentManagementUsp.js'
```

Insert the User ID imports immediately after them, before the existing `// Client-side bid adapters` comment block. The resulting section must read:

```ts
import 'prebid.js/modules/consentManagementTcf.js'
import 'prebid.js/modules/consentManagementGpp.js'
import 'prebid.js/modules/consentManagementUsp.js'

// Prebid User ID Module — core + submodules. The core module exposes
// `pbjs.getUserIdsAsEids`; submodules self-register at import time and
// activate when the publisher's origin-side `pbjs.setConfig({ userSync:
// { userIds: [...] } })` call runs during `processQueue()`.
import 'prebid.js/modules/userId.js'

// Zero-config / auto-populating submodules (resolve without publisher params).
import 'prebid.js/modules/sharedIdSystem.js'
import 'prebid.js/modules/criteoIdSystem.js'
import 'prebid.js/modules/33acrossIdSystem.js'
import 'prebid.js/modules/pubProvidedIdSystem.js'
import 'prebid.js/modules/quantcastIdSystem.js'

// Param-based submodules — inert until publisher setConfig supplies params.
import 'prebid.js/modules/id5IdSystem.js'
import 'prebid.js/modules/identityLinkIdSystem.js'
import 'prebid.js/modules/liveIntentIdSystem.js'
import 'prebid.js/modules/uid2IdSystem.js'
import 'prebid.js/modules/euidIdSystem.js'
import 'prebid.js/modules/intentIqIdSystem.js'
import 'prebid.js/modules/lotamePanoramaIdSystem.js'
import 'prebid.js/modules/connectIdSystem.js'
import 'prebid.js/modules/merkleIdSystem.js'

// Legacy / deprecated but still present in some publisher configs.
import 'prebid.js/modules/pubCommonIdSystem.js'
```

- [ ] **Step 5: Run the regression-guard block — expect PASS**

Run: `cd crates/js/lib && npx vitest run test/integrations/prebid/index.test.ts -t "regression guard"`

Expected: all 15 tests PASS.

- [ ] **Step 6: Run the full prebid test file — expect no regressions**

Run: `cd crates/js/lib && npx vitest run test/integrations/prebid/index.test.ts`

Expected: all tests PASS (Task 1 tests + regression guards + all pre-existing tests).

- [ ] **Step 7: Run the entire JS test suite**

Run: `cd crates/js/lib && npx vitest run`

Expected: all PASS. If any unrelated test fails because it imports `./index.ts` transitively and the real Prebid modules are not mocked there, add matching `vi.mock` stubs at the top of that test file (copy the same 15 lines). Common suspects: any test under `crates/js/lib/test/integrations/prebid/` you may have added, or tests that import core modules which in turn import the prebid integration. At the time of writing, no other test file imports the prebid integration.

- [ ] **Step 8: Build the JS bundles**

Run: `cd crates/js/lib && node build-all.mjs`

Expected: build succeeds. `dist/tsjs-prebid.js` gets substantially larger (est. 100-150kb gzipped increase). No esbuild errors about missing modules — if there are, the module path in the new imports is wrong (check `crates/js/lib/node_modules/prebid.js/modules/` for the exact filename — note `33acrossIdSystem.js` really does start with a digit and is correct).

- [ ] **Step 9: Format the JS**

Run: `cd crates/js/lib && npm run format`

Expected: prettier rewrites any formatting drift in the files you touched. No errors.

- [ ] **Step 10: Verify the Rust build picks up the rebuilt bundle**

Run: `cargo check --package trusted-server-core`

Expected: PASS. `build.rs` re-runs because `dist/tsjs-prebid.js` changed; `include_str!` pulls in the new content.

- [ ] **Step 11: Run full Rust test suite to confirm no downstream breakage**

Run: `cargo test --workspace`

Expected: PASS. The Rust side does not inspect bundle contents, only concatenates and hashes them, so tests should be unaffected.

- [ ] **Step 12: Commit**

```bash
git add crates/js/lib/src/integrations/prebid/index.ts crates/js/lib/test/integrations/prebid/index.test.ts
git commit -m "Bundle Prebid User ID core and submodules in Prebid integration"
```

---

## Task 3: Final verification

- [ ] **Step 1: Full CI-equivalent check**

Run the same sequence CI runs:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cd crates/js/lib && npx vitest run && cd ../../..
cd crates/js/lib && npm run format && cd ../../..
```

Expected: everything PASS / clean.

- [ ] **Step 2: Manual verification note**

Manual browser verification (cannot be automated here; run against a dev publisher environment that has origin-side `pbjs.setConfig({ userSync: { userIds: [...] } })`):

1. Load a publisher page. In DevTools console: `typeof pbjs.getUserIdsAsEids` should return `'function'`.
2. `pbjs.getUserIdsAsEids()` should return a non-empty array.
3. After the first ad-slot auction completes: `document.cookie.match(/ts-eids=/)` should match.
4. Decode the cookie: `JSON.parse(atob(document.cookie.match(/ts-eids=([^;]+)/)[1]))` should produce a `[{source, id, atype}]` array matching the raw EIDs.
5. Network tab: the second `/auction` request should carry `Cookie: ts-eids=...`.

These are documented in the spec; they are not blockers for the PR, but they should be run before closing out the work.

- [ ] **Step 3: No follow-up commits required**

The work is complete when Tasks 1 and 2 are committed. Do not create a third "chore" commit unless format/clippy asks for one.

---

## What this plan intentionally does NOT do

- Does **not** add a build-time env-var toggle (`TSJS_PREBID_USER_IDS`) to mirror `TSJS_PREBID_ADAPTERS`. Deferred per spec.
- Does **not** add `window.__tsjs_prebid.userIds` server-side injection. Deferred per spec.
- Does **not** change `[[ec.partners]]` or `crates/trusted-server-core/src/ec/prebid_eids.rs`. Backend already handles received cookies correctly.
- Does **not** add a bundle-size regression gate. Noted as a known cost in the spec.
- Does **not** add tests for individual ID submodule resolution behavior. That is Prebid's own test surface, not ours.
