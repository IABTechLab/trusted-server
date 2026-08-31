# Managed User ID Consent Activation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ensure server-managed Prebid User IDs activate the existing TCF enforcement modules when an IAB CMP is present, without overwriting publisher-owned consent configuration.

**Architecture:** Add one focused browser-side helper in the existing Prebid shim. It reads effective Prebid consent configuration, recognizes any existing own `gdpr` property as publisher-owned, and otherwise installs only `gdpr.cmpApi = "iab"` when managed User IDs and a callable `window.__tcfapi` are present. Existing `setConfig`/`mergeConfig` wrappers continue to pass publisher consent configuration through unchanged, so effective, queued, and late publisher settings retain precedence.

**Tech Stack:** TypeScript, Prebid.js 10.26.0, Vitest, JSDOM, generated external Prebid bundle, Markdown.

---

## File structure

- Modify `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`: detect and install the minimum managed-ID TCF configuration before managed IDs are seeded.
- Modify `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`: unit-test activation conditions, merge semantics, malformed values, and publisher precedence.
- Modify `crates/trusted-server-js/lib/test/prebid-consent-enforcement.test.mjs`: prove the real generated bundle blocks IdentityLink without publisher-side Prebid consent setup.
- Modify `docs/guide/integrations/prebid.md`: document automatic activation and ownership boundaries.
- Modify `docs/superpowers/specs/2026-08-21-liveramp-integration-design.md`: mark the consent hardening and bundle guard as implemented and record sanitized live-validation results accurately.

### Task 1: Add failing shim unit tests

**Files:**
- Modify: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts:65-105`
- Modify: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts:244-252`
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts:835-1130`

- [ ] **Step 1: Extend the test window and reset state**

Add an optional `__tcfapi` function to `PrebidTestWindow`. Delete it in the shared `beforeEach`, and reset `mockGetConfig` so tests do not leak effective configuration.

- [ ] **Step 2: Write the activation-condition tests**

Add tests that install the shim and assert the original `mockSetConfig` receives:

```ts
{
  consentManagement: {
    gdpr: { cmpApi: 'iab' },
  },
}
```

only when `managedUserIds` is non-empty, `window.__tcfapi` is callable, and effective `consentManagement` has no own `gdpr` property. Assert this call precedes the managed `userSync.userIds` call and `processQueue()`.

- [ ] **Step 3: Write preservation and degraded-behavior tests**

Cover:

- no managed IDs;
- missing and non-callable `__tcfapi`;
- sibling `gpp` configuration preserved;
- effective own `gdpr` object, `null`, and `false` preserved without an automatic GDPR call;
- root `null`, `false`, strings, arrays, and throwing effective consent state log a diagnostic and are not replaced;
- queued and late publisher `setConfig`/`mergeConfig` consent fields pass through unchanged;
- automatic configuration is applied only once.

- [ ] **Step 4: Run the focused unit tests and verify RED**

Run:

```bash
cd crates/trusted-server-js/lib
env PATH=/Users/prk-jr/.nvm/versions/node/v24.12.0/bin:$PATH \
  npx vitest run test/integrations/prebid/index.test.ts
```

Expected: the new activation assertion fails because no automatic `consentManagement.gdpr` call exists.

- [ ] **Step 5: Remove the masking publisher consent setup from the artifact test**

Change the primary denied-consent harness cases in
`test/prebid-consent-enforcement.test.mjs` so they do not call publisher-side
`pbjs.setConfig({ consentManagement: ... })`. Keep only the
`userSync.auctionDelay` setup required to resolve IDs during one auction. Add an
optional publisher consent configuration for later preservation coverage.

- [ ] **Step 6: Run the generated-artifact test and verify RED**

```bash
cd crates/trusted-server-js/lib
env PATH=/Users/prk-jr/.nvm/versions/node/v24.12.0/bin:$PATH \
  npx vitest run test/prebid-consent-enforcement.test.mjs
```

Expected: the denied-consent case fails because IdentityLink makes its envelope
request or writes storage when Prebid consent management is not activated.

- [ ] **Step 7: Commit the failing tests**

```bash
git add crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts \
  crates/trusted-server-js/lib/test/prebid-consent-enforcement.test.mjs
git commit -m "Test managed User ID consent activation"
```

### Task 2: Implement the minimum non-clobbering TCF setup

**Files:**
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts:130-210`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts:1187-1241`
- Test: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`

- [ ] **Step 1: Add the focused helper**

Add a private helper receiving the original `setConfig`, `getConfig`, and managed entries. It must:

1. return unless managed entries exist and `typeof window.__tcfapi === 'function'`;
2. read `getConfig('consentManagement')` inside `try/catch`;
3. preserve any record with an own `gdpr` property;
4. merge record-valued sibling settings with `gdpr: { cmpApi: 'iab' }`;
5. treat every defined non-record value, including `null`, or a thrown accessor as publisher-owned/unsafe, log once, and return;
6. call the original Prebid `setConfig` exactly once, without adding timeout or `defaultGdprScope`.

- [ ] **Step 2: Invoke it before managed ID seeding**

Call the helper after capturing the original config APIs and before the first managed `userSync.userIds` update. Do not add a new core/TOML field and do not alter pages without managed IDs.

- [ ] **Step 3: Run the focused unit tests and verify GREEN**

Run the Task 1 command. Expected: all tests in `index.test.ts` pass.

- [ ] **Step 4: Run formatting and type-aware JS tests**

```bash
cd crates/trusted-server-js/lib
env PATH=/Users/prk-jr/.nvm/versions/node/v24.12.0/bin:$PATH npm run format
env PATH=/Users/prk-jr/.nvm/versions/node/v24.12.0/bin:$PATH npx vitest run test/integrations/prebid/index.test.ts
```

- [ ] **Step 5: Commit the implementation**

```bash
git add crates/trusted-server-js/lib/src/integrations/prebid/index.ts \
  crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts
git commit -m "Activate TCF for managed User IDs"
```

### Task 3: Complete generated-artifact enforcement coverage

**Files:**
- Modify: `crates/trusted-server-js/lib/test/prebid-consent-enforcement.test.mjs:115-205`

- [ ] **Step 1: Add artifact-level preservation coverage**

Use the harness option introduced in Task 1 and prove an existing custom GDPR
object remains effective after the shim loads.

- [ ] **Step 2: Run the generated-artifact suite and verify GREEN**

```bash
cd crates/trusted-server-js/lib
env PATH=/Users/prk-jr/.nvm/versions/node/v24.12.0/bin:$PATH \
  npx vitest run test/prebid-consent-enforcement.test.mjs
```

Expected: denied Purpose 1 and denied vendor 97 make no envelope request and write no LiveRamp storage; granted consent still makes one request and writes storage.

- [ ] **Step 3: Commit the completed artifact regression test**

```bash
git add crates/trusted-server-js/lib/test/prebid-consent-enforcement.test.mjs
git commit -m "Prove managed-only TCF enforcement"
```

### Task 4: Align documentation and PR handoff text

**Files:**
- Modify: `docs/guide/integrations/prebid.md:130-145`
- Modify: `docs/guide/integrations/prebid.md:540-565`
- Modify: `docs/guide/integrations/prebid.md:620-635`
- Modify: `docs/superpowers/specs/2026-08-21-liveramp-integration-design.md:9`
- Modify: `docs/superpowers/specs/2026-08-21-liveramp-integration-design.md:151-205`
- Modify: `docs/superpowers/specs/2026-08-21-liveramp-integration-design.md:620-720`

- [ ] **Step 1: Document exact browser consent ownership**

State that callable `__tcfapi` plus managed IDs activates only `gdpr.cmpApi = "iab"`, existing publisher GDPR configuration wins, and pages without TCF remain unchanged.

- [ ] **Step 2: Update implementation and validation status**

Mark the registry-driven bundle guard and managed consent hardening implemented. Record only sanitized live evidence: anonymous/unresolved browser returned 204/no EID; resolvable test identity returned 200, stored an envelope, and exposed one `liveramp.com` EID. Do not record Placement IDs, cookies, or envelope values. Do not claim the unperformed PBS/EC follow-on checks are complete.

Keep the full live-validation acceptance criterion explicitly pending. List the
remaining external checks: denied-consent behavior on an approved live origin,
unapproved-origin degradation, controlled PBS `user.ext.eids` forwarding, and
later EC/KV ingestion. These require publisher/LiveRamp test conditions and are
not replaced by automated artifact tests.

- [ ] **Step 3: Prepare corrected PR description text**

Prepare a concise handoff in the final response replacing vendor-specific core wording, removing the unrelated credential-blocked status, recording completed browser validation, and describing ATS server-side work as deferred pending team confirmation. Do not mutate GitHub.

- [ ] **Step 4: Format documentation**

```bash
cd docs
npm run format:write
npm run format
```

- [ ] **Step 5: Commit documentation**

```bash
git add docs/guide/integrations/prebid.md \
  docs/superpowers/specs/2026-08-21-liveramp-integration-design.md
git commit -m "Align LiveRamp consent and validation status"
```

### Task 5: Full verification and final review

**Files:**
- Review: all changes from the pre-plan HEAD through the final HEAD

- [ ] **Step 1: Run the full JavaScript suite with the pinned Node version**

```bash
cd crates/trusted-server-js/lib
env PATH=/Users/prk-jr/.nvm/versions/node/v24.12.0/bin:$PATH npx vitest run
```

- [ ] **Step 2: Run repository formatting checks**

```bash
cargo fmt --all -- --check
cd crates/trusted-server-js/lib
env PATH=/Users/prk-jr/.nvm/versions/node/v24.12.0/bin:$PATH npm run format
cd ../../.. && cd docs
npm run format
```

- [ ] **Step 3: Run the repository test matrix**

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
./scripts/test-cli.sh
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
```

- [ ] **Step 4: Run the repository lint matrix**

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
cargo clippy-cli
cargo clippy-codegen
```

- [ ] **Step 5: Inspect repository and PR diff hygiene**

```bash
git diff --check origin/main...HEAD
git status --short
git diff --stat origin/main...HEAD
```

Confirm `fastly.toml` remains the user's uncommitted local file and no Placement ID, cookie, or envelope value entered the committed diff.

- [ ] **Step 6: Request final code review**

Review the complete diff for correctness, privacy regressions, scope, stale documentation, and test gaps. Fix any blocking finding test-first and rerun the relevant verification.

- [ ] **Step 7: Report readiness without pushing**

Summarize commits, verification evidence, remaining external steps, and corrected PR-description text. Do not push or mark the PR ready.
