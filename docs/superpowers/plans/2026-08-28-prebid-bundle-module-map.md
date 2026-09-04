# Prebid bundle module map implementation plan

> **For agentic workers:** Execute this plan task by task. Keep the design in
> `docs/superpowers/specs/2026-08-28-prebid-bundle-module-map-design.md` open as
> the authority for schema, validation, manifest, and lifecycle decisions. Use
> red-green tests for every behavioral step and do not weaken a spec requirement
> to make an existing test pass.

**Goal:** Replace the old Prebid bidder/User ID bundle fields with one typed
module map, add pinned-package analytics adapters, and prove that a generated
bundle registers `atsAnalytics` before publisher queue code enables it.

**Architecture:** The Rust CLI owns focused TOML parsing and serializes one typed
module request. The Node generator independently validates that request, the
lockfile/install version pair, exact-case Prebid metadata, package-export target
containment, and module kind before it writes generated source or invokes Vite.
The generated artifact stamps one versioned nested manifest consumed strictly by
the TSJS shim. Publisher code remains responsible for calling
`pbjs.enableAnalytics` with provider options.

**Tech stack:** Rust 2024, `serde`, `serde_json`, `toml`, `toml_edit`, Node 24,
Prebid.js 10.26.0, Vite, Vitest, and JSDOM.

**Issue:** [#1085](https://github.com/IABTechLab/trusted-server/issues/1085)

---

## Implementation preconditions

- Work on a feature branch based on the latest `origin/main`. The checkout used
  to write this plan was four commits behind, and `trusted-server.example.toml`
  has relevant upstream edits.
- Preserve the approved spec and this plan when moving to the implementation
  branch.
- Install JavaScript dependencies with the repository's pinned tooling before
  running JS or docs gates:

  ```bash
  REPO_ROOT=$(git rev-parse --show-toplevel)
  (cd "$REPO_ROOT/crates/trusted-server-js/lib" && npm ci)
  (cd "$REPO_ROOT/docs" && npm ci)
  ```

- Treat `docs/superpowers/specs/2026-06-17-prebid-bundle-cli-design.md` and
  `docs/superpowers/specs/2026-05-28-external-prebid-first-party-proxy-design.md`
  as historical records. Do not rewrite them.
- Do not add compatibility parsing for `adapters`, `user_id_modules`, old
  generator flags, or flat manifest fields.
- Use only fictional/example values in tests and docs.

## File map

| File                                                                        | Responsibility in this change                                                                                                                                      |
| --------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `crates/trusted-server-cli/src/prebid_bundle.rs`                            | Typed focused TOML schema, removed-field diagnostics, module-name validation, generator JSON request, manifest schema validation, and CLI tests                    |
| `crates/trusted-server-js/lib/build-prebid-external.mjs`                    | Request parsing, dependency/version checks, package resolution, metadata validation, generated imports, Vite orchestration, and disk/browser manifest construction |
| `crates/trusted-server-js/lib/test/build-prebid-external.test.mjs`          | Generator request, resolver, rendering, failure-order, manifest, and no-analytics coverage                                                                         |
| `crates/trusted-server-js/lib/test/prebid-artifact-integration.test.mjs`    | Production external bundle plus production shim evaluation, ATS registration, queue processing, network isolation, and auction regression coverage                 |
| `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`             | Strict nested browser-manifest parser and bidder/User ID diagnostic consumers                                                                                      |
| `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`       | Manifest version, container, list, bidder, and User ID diagnostic coverage                                                                                         |
| `crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.json` | Curated User ID membership/default/diagnostic data with import paths removed                                                                                       |
| `crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.ts`   | Registry typing after import paths stop being registry authority                                                                                                   |
| `trusted-server.example.toml`                                               | Canonical module-map example                                                                                                                                       |
| `docs/guide/integrations/prebid.md`                                         | Full operator semantics, module/runtime-code mapping, limitations, and bundle workflow                                                                             |
| `docs/guide/cli.md`                                                         | `ts prebid bundle` example and output behavior                                                                                                                     |

## Commit boundaries

Task 1 may be committed independently because it adds tested resolver
foundations without changing the active generator protocol. Tasks 2 through 5
form one breaking cutover and should remain uncommitted until the generator,
shim, artifact test, and Rust CLI all agree on the new contract. Task 6 is a
documentation commit. Verification fixes in Task 7 should be folded into the
commit that introduced the affected behavior.

---

## Task 1: Establish the generator's typed validation foundation

**Files:**

- Modify: `crates/trusted-server-js/lib/build-prebid-external.mjs`
- Modify: `crates/trusted-server-js/lib/test/build-prebid-external.test.mjs`

### Step 1.1: Record the baseline

Run the existing focused suites before editing:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/build-prebid-external.test.mjs
npx vitest run test/integrations/prebid/index.test.ts
npx vitest run test/prebid-artifact-integration.test.mjs
```

Expected: all pass. Record any unrelated baseline failure before proceeding.

Also verify the package-export behavior that the resolver must preserve:

```bash
node --input-type=module <<'NODE'
import { createRequire } from 'node:module'
const require = createRequire(import.meta.url)
console.log(require.resolve('prebid.js/modules/sharedIdSystem.js'))
console.log(require.resolve('prebid.js/modules/atsAnalyticsAdapter.js'))
NODE
```

Expected: both resolve under `node_modules/prebid.js/dist/src/public/`. Do not
require a physical `node_modules/prebid.js/modules/sharedIdSystem.js` source
file.

### Step 1.2: Add failing request-schema and normalization tests

Add table-driven tests for a pure parser/normalizer that accepts the JSON shape:

```json
{
  "bidder": ["rubiconBidAdapter"],
  "userId": ["sharedIdSystem"],
  "analytics": ["atsAnalyticsAdapter"]
}
```

Cover:

- required, non-empty `bidder`;
- omitted `userId` expands to `user_id_modules.json`'s default preset;
- explicit `userId: []` remains empty;
- omitted or explicit empty `analytics` normalizes to an empty list;
- unknown request properties;
- non-array lists and non-string entries;
- empty/whitespace values;
- `.js`, `/`, `\\`, `..`, quotes, URL-like strings, and control characters;
- duplicate names within one kind; and
- a stem repeated across kinds.

Every error assertion must include the TOML-facing field path, such as
`integrations.prebid.bundle.modules.analytics`, rather than only the internal
JSON key.

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/build-prebid-external.test.mjs
```

Expected: the new tests fail because the typed parser and normalizer do not yet
exist.

### Step 1.3: Implement the pure request model

In `build-prebid-external.mjs`, add one normalized model with kind order:

1. `bidder`;
2. `userId`; and
3. `analytics`.

Use one module-stem validator implementing `^[A-Za-z0-9_-]+$`. Preserve
configured list order. Reject duplicates rather than deduplicating selections.
Keep a fixed mapping from internal JSON kind to TOML error path and Prebid
metadata component type:

| JSON kind   | TOML kind   | Metadata type |
| ----------- | ----------- | ------------- |
| `bidder`    | `bidder`    | `bidder`      |
| `userId`    | `user_id`   | `userId`      |
| `analytics` | `analytics` | `analytics`   |

Export only the small pure functions needed by tests. Do not route `main()`
through the new model yet.

### Step 1.4: Add failing lockfile/install version tests

Add an injectable helper that reads:

- `package-lock.json` at `packages["node_modules/prebid.js"].version`; and
- `node_modules/prebid.js/package.json` at `version`.

Use temporary fixture files to cover:

- matching versions;
- mismatched versions;
- missing lockfile package entry;
- missing installed version; and
- malformed JSON.

The mismatch error must report both values and instruct the operator to run
`npm ci` in `crates/trusted-server-js/lib`.

### Step 1.5: Implement dependency-version validation

Implement the helper without changing `prebidPackageVersion()` call order yet.
Return the verified installed version so later tasks use one value for the
manifest. Keep all errors prefixed with `[build-prebid-external]`.

### Step 1.6: Add failing exact-case metadata and package-target tests

Create a resolver seam whose filesystem roots and package-specifier resolver can
be injected. Tests must cover:

- `rubiconBidAdapter` as `bidder`;
- `sharedIdSystem` as `userId` without a physical source module file;
- `atsAnalyticsAdapter` as `analytics`;
- wrong-case metadata stems;
- missing metadata;
- metadata whose matching component type is absent;
- malformed metadata JSON;
- non-array `components`;
- matching components with missing, empty, or non-string `componentName`;
- mixed valid and malformed matching components;
- a metadata symlink escaping the metadata directory;
- a package specifier that cannot resolve;
- a resolved target outside the canonical Prebid package root;
- a non-regular resolved target;
- bidder aliases from `adfBidAdapter` metadata; and
- analytics runtime code `atsAnalytics` from ATS metadata.

Use dependency injection for `resolveSpecifier` rather than modifying the real
installed package. Metadata-shape errors must include the
`[build-prebid-external]` prefix, TOML field path, requested stem, and metadata
path. These foundation tests prove resolution fails; Task 2 adds the
orchestration spy that proves Vite is not invoked.

### Step 1.7: Implement module resolution

For each normalized selection:

1. Read the metadata directory and require an exact-case `<stem>.json` entry.
2. Canonicalize the metadata root and file.
3. Require the metadata file to be a regular direct child of that root.
4. Parse metadata and require `components` to be an array.
5. Require at least one component of the configured kind. Every matching
   component must have a non-empty string `componentName`; reject the entire
   module if valid and malformed matching components are mixed.
6. Derive matching component names, deduplicate them, and sort them.
7. Derive `prebid.js/modules/<stem>.js` from the validated stem.
8. Resolve that exact specifier with the production `require.resolve`.
9. Canonicalize the installed package root and resolved target.
10. Require a regular resolved target contained by the package root.

For `userId`, also require membership in the checked-in User ID registry. Do not
read an import path from that registry.

For LiveIntent, validate the ordinary upstream metadata and package export first.
Then validate all three fixed alias targets before Vite:

- canonicalize the checked-in shim and require the exact expected regular
  repository file;
- canonicalize `PREBID_LIVE_INTENT_STANDARD` and require a regular file contained
  by the canonical Prebid package root; and
- canonicalize `PREBID_GLOBAL_MODULE` and require a regular file contained by the
  canonical Prebid package root.

Add an injected failure test for each target and prove the build runner is not
called. Keep these generator-owned aliases as the only local import overrides.

### Step 1.8: Run the foundation tests and commit

Format the changed `.mjs` files explicitly because the package's `npm run
format` glob does not include that extension:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/build-prebid-external.test.mjs
npx prettier --write build-prebid-external.mjs test/build-prebid-external.test.mjs
npx prettier --check build-prebid-external.mjs test/build-prebid-external.test.mjs
npm run format
```

Expected: all existing tests and the new pure foundation tests pass while the
active generator still uses the old protocol.

Review the diff, then commit from the repository root:

```bash
cd "$(git rev-parse --show-toplevel)"
git add crates/trusted-server-js/lib/build-prebid-external.mjs \
  crates/trusted-server-js/lib/test/build-prebid-external.test.mjs
git commit -m "Add typed Prebid module resolution"
```

---

## Task 2: Cut the JavaScript generator over to the module map

**Files:**

- Modify: `crates/trusted-server-js/lib/build-prebid-external.mjs`
- Modify: `crates/trusted-server-js/lib/test/build-prebid-external.test.mjs`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.json`
- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/prebid/user_id_modules.test.ts` if registry-shape assertions require it

Do not commit this task by itself. The generator protocol and flat browser
manifest become incompatible with the old CLI and shim until Tasks 3 through 5
finish.

### Step 2.1: Replace argument-parser tests

Write failing tests proving `parseArgs`:

- requires exactly one `--modules-json` value;
- accepts `--modules-json=<json>` and the two-argument form;
- preserves relative `--out` resolution against `process.cwd()`;
- rejects duplicate options;
- rejects positional arguments;
- rejects unknown options;
- rejects removed `--adapters` and `--user-id-modules`; and
- surfaces malformed module JSON through the typed request parser.

Remove tests for the `rubicon` default and comma-list parsing. There is no default
bidder selection in the new schema.

### Step 2.2: Remove registry import-path authority

Delete every `importPath` property from `user_id_modules.json` and from
`PrebidUserIdModuleRegistryEntry` in `user_id_modules.ts`.

Update registry tests if needed. The generator must derive every User ID package
specifier from the validated stem. `notes` remains valid for LiveIntent
diagnostics/documentation.

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/prebid/user_id_modules.test.ts
```

Expected: pass after the registry typing is updated.

### Step 2.3: Add failing generated-source tests

Extract pure rendering functions and test exact source properties:

- imports appear in bidder, User ID, analytics kind order;
- configured order is preserved within each kind;
- `userId.js` is imported only when the effective User ID list is non-empty;
- package specifiers are derived from validated stems;
- an empty analytics list emits no analytics import;
- the generated export contains effective module arrays;
- runtime bidder and analytics code arrays are sorted/deduplicated; and
- the generated browser manifest uses `schemaVersion`, `modules`, and
  `runtimeCodes` only.

Do not inspect a minified IIFE to prove import presence. Assert against the pure
rendered source.

### Step 2.4: Replace category-specific generated files

Refactor the temporary generation model to use one normalized module selection
and one manifest source of truth. A single `_modules.generated.ts` is preferred,
but separate files are acceptable only if they all consume the same normalized
resolved-module array.

Remove:

- `DEFAULT_PREBID_ADAPTERS`;
- `parseList`;
- old adapter name suffixing;
- `generateAdapterImports`;
- registry-provided User ID import paths;
- the adapter-specific generated export; and
- the flat `adapters`, `bidderCodes`, and `userIdModules` browser-manifest
  rendering.

Retain the existing fixed core/consent imports, watchdog, Vite aliases, bundle
hashing, SRI, temporary output filename, final atomic rename, and cleanup
behavior.

### Step 2.5: Make validation precede temporary generation and Vite

Change orchestration order to:

1. parse CLI arguments and module JSON;
2. verify lockfile and installed Prebid versions;
3. normalize defaults;
4. validate every metadata entry and package-export target;
5. validate the fixed LiveIntent shim if selected;
6. create temporary generated paths;
7. render generated source;
8. invoke Vite;
9. hash and rename the bundle; and
10. write `manifest.json`.

Expose or inject the Vite build runner so tests can prove steps 1 through 5 fail
before temporary generation and before Vite. Preserve `finally` cleanup for every
failure after temporary paths exist.

### Step 2.6: Emit disk manifest schema version 1

Write exactly this shape, with effective lists even when TOML omitted optional
categories:

```json
{
  "schemaVersion": 1,
  "prebidVersion": "10.26.0",
  "modules": {
    "bidder": ["rubiconBidAdapter"],
    "userId": ["sharedIdSystem"],
    "analytics": ["atsAnalyticsAdapter"]
  },
  "runtimeCodes": {
    "bidder": ["rubicon"],
    "analytics": ["atsAnalytics"]
  },
  "sha256": "...",
  "sri": "sha384-...",
  "filename": "trusted-prebid-<sha256>.js"
}
```

Remove the flat manifest fields with no fallback.

### Step 2.7: Add generator orchestration failures

Add focused tests for:

- lock/install mismatch before temporary generation;
- guaranteed-fictional missing stems;
- `mavenDistributionAnalyticsAdapter` missing from pinned Prebid 10.26.0;
- wrong-kind `sharedIdSystem` under analytics;
- an unknown curated User ID module;
- package-target escape through the injected resolver;
- cleanup after a rendered-source or Vite failure; and
- no new bundle, manifest, or generated-source artifact written after validation
  failure.

The Maven Distribution error must contain:

- `integrations.prebid.bundle.modules.analytics`;
- `mavenDistributionAnalyticsAdapter`;
- the verified pinned version;
- the expected package specifier; and
- guidance that only pinned upstream modules are supported.

### Step 2.8: Run focused generator tests

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/build-prebid-external.test.mjs
npx prettier --write build-prebid-external.mjs test/build-prebid-external.test.mjs
npx prettier --check build-prebid-external.mjs test/build-prebid-external.test.mjs
npm run format
```

Expected: pass. Do not run or claim the full JS suite yet; the shim and artifact
fixture still expect the removed flat manifest.

---

## Task 3: Migrate the TSJS shim to the versioned browser manifest

**Files:**

- Modify: `crates/trusted-server-js/lib/src/integrations/prebid/index.ts`
- Modify: `crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts`

Do not commit this task separately from Tasks 2, 4, and 5.

### Step 3.1: Add strict parser tests

Add focused tests for a pure or directly observable browser-manifest parser:

- valid `schemaVersion: 1` with all nested arrays;
- non-object root;
- absent version;
- numeric versions `0` and `2`;
- string version `"1"`;
- absent/non-object `modules`;
- absent/non-object `runtimeCodes`;
- mixed string/non-string `modules.userId`;
- mixed string/non-string `runtimeCodes.bidder`;
- one malformed list leaving a valid sibling list usable; and
- removed flat fields being ignored even when present.

The list parser rejects a whole invalid list. It does not filter invalid entries.
Unsupported versions follow the same one-time diagnostic path as an absent
manifest.

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/prebid/index.test.ts
```

Expected: new tests fail against the flat, filtering parser.

### Step 3.2: Implement the nested parser

Replace `ExternalPrebidBundleManifest`, `sanitizeManifestList`, and
`getExternalBundleManifest` with a schema-versioned nested model matching the
spec.

Parsing rules:

- invalid root/version makes the whole manifest unavailable;
- invalid/missing container makes fields in that container unavailable;
- invalid list makes only that list unavailable;
- sibling lists survive independently; and
- no old flat-field fallback exists.

Keep the page global untrusted. Do not rely on TypeScript casting without runtime
checks.

### Step 3.3: Move diagnostic consumers

Update:

- client-side bidder validation to read `runtimeCodes.bidder`;
- User ID diagnostics to read `modules.userId`; and
- diagnostic comments/error messages to name
  `[integrations.prebid.bundle.modules].bidder` with exact module stems.

Do not use `modules.analytics` as provider codes and do not make the TSJS shim
call `pbjs.enableAnalytics`.

### Step 3.4: Rewrite flat-manifest fixtures

Update all `__tsjs_prebid_bundle` fixtures in `index.test.ts` to schema version 1.
Replace alias tests with exact bidder module stems plus derived runtime codes.
Keep tests proving:

- `adfBidAdapter` includes `adf`, `adform`, and `adformOpenRTB` runtime codes;
- `a1MediaBidAdapter` maps to `a1media`; and
- missing runtime bidder codes still produce the current actionable diagnostic.

### Step 3.5: Run shim tests

Run:

```bash
cd crates/trusted-server-js/lib
npx vitest run test/integrations/prebid/index.test.ts
npm run format
```

Expected: pass.

---

## Task 4: Prove the production ATS registration and artifact lifecycle

**Files:**

- Modify: `crates/trusted-server-js/lib/test/prebid-artifact-integration.test.mjs`
- Modify: `crates/trusted-server-js/lib/test/build-prebid-external.test.mjs` if production orchestration needs an additional seam assertion

Do not commit this task separately from Tasks 2, 3, and 5.

### Step 4.1: Update the artifact build request

Replace old generator flags with one `--modules-json` argument. Use the
specification's required production fixture:

```json
{
  "bidder": ["rubiconBidAdapter"],
  "userId": ["sharedIdSystem"],
  "analytics": ["atsAnalyticsAdapter"]
}
```

Update manifest assertions to:

- `schemaVersion === 1`;
- `modules.bidder === ["rubiconBidAdapter"]`;
- `modules.userId === ["sharedIdSystem"]`;
- `modules.analytics === ["atsAnalyticsAdapter"]`;
- `runtimeCodes.bidder === ["rubicon"]`; and
- `runtimeCodes.analytics === ["atsAnalytics"]`.

Keep `adfBidAdapter` alias coverage in the focused resolver and shim tests from
Tasks 1 and 3. Keep the existing bundle-size, shim-size, public API, single
Trusted Server registration, and `/auction` assertions in the production test.

### Step 4.2: Enqueue analytics before artifact evaluation

Before evaluating either artifact:

1. Replace `window.console.error` and any Prebid error-log path with spies that
   retain messages for assertion.
2. Stub `fetch`, `Request`, `Headers`, `Response`, `AbortController`,
   `XMLHttpRequest`, `navigator.sendBeacon`, and image/network primitives used by
   the adapter. Reuse existing stubs where adequate.
3. Create the same `{ que: [], cmd: [] }` stub emitted by the server.
4. Push one queue callback that records `started`, calls
   `pbjs.enableAnalytics` with provider `atsAnalytics` and fictional
   `options.pid`, catches/stores any error, and records `completed` only after
   the call returns.

The test must prevent real traffic while allowing calls into spies. It need not
assert that ATS sends no request to the spies.

### Step 4.3: Evaluate both production artifacts and wait for the callback

Evaluate the external bundle first and the production TSJS shim second. Use
`vi.waitFor` until the callback either completes or stores an error.

Assert:

- the callback started and completed;
- it stored no error;
- no console message contains the exact missing-registry diagnostic for
  `atsAnalytics`;
- no console message contains `Error processing command`;
- every attempted analytics request went through a stub; and
- the existing Trusted Server auction still reaches `/auction`.

This proves module evaluation happened before the shim called
`pbjs.processQueue()`. Do not trigger queue processing directly in the test.

### Step 4.4: Add normal and watchdog no-analytics production cases

Build a second production artifact with `rubiconBidAdapter`, `sharedIdSystem`,
and analytics omitted. Reuse that bundle across two isolated JSDOM tests.

In the normal shim lifecycle test, evaluate the no-analytics external bundle and
production shim, then retain the bidder, User ID, `/auction`, hash, and SRI
assertions. Assert `modules.analytics` and `runtimeCodes.analytics` are empty.

In a distinct watchdog test:

1. Install a JSDOM-compatible fake clock before bundle evaluation.
2. Create the server-style `pbjs` queue and enqueue a sentinel callback.
3. Evaluate the no-analytics external bundle without evaluating the shim.
4. Assert the watchdog timer was scheduled for 5,000 ms.
5. Advance the fake clock by 5,000 ms and wait for queued work.
6. Assert the watchdog called `pbjs.processQueue()` and the sentinel callback
   ran.
7. Assert the nested analytics module and runtime-code arrays are empty.
8. Restore timers and close the JSDOM window in `finally` cleanup.

Keep the exact "no generated analytics import" assertion in the pure renderer
unit test from Task 2, not against the minified IIFE.

### Step 4.5: Run production artifact tests

Run:

```bash
cd crates/trusted-server-js/lib
npx prettier --write test/prebid-artifact-integration.test.mjs
npx prettier --check build-prebid-external.mjs \
  test/build-prebid-external.test.mjs \
  test/prebid-artifact-integration.test.mjs
npx vitest run test/prebid-artifact-integration.test.mjs
npx vitest run test/build-prebid-external.test.mjs test/integrations/prebid/index.test.ts
```

Expected: pass without external network access.

---

## Task 5: Cut `ts prebid bundle` over to the typed TOML map

**Files:**

- Modify: `crates/trusted-server-cli/src/prebid_bundle.rs`

Do not commit until every step in this task and the cross-language smoke test
passes.

### Step 5.1: Add failing focused configuration tests

Replace old bundle config fixtures with:

```toml
[integrations.prebid.bundle.modules]
bidder = ["rubiconBidAdapter", "kargoBidAdapter"]
user_id = ["sharedIdSystem"]
analytics = ["atsAnalyticsAdapter"]
```

Add table-driven tests covering:

- valid complete map;
- omitted `user_id` and `analytics` represented as `None`;
- explicit empty `user_id` and analytics represented as `Some(Vec::new())`;
- missing `bundle`;
- missing `bundle.modules`;
- missing or empty bidder list;
- unknown module kind;
- malformed list/value types;
- every invalid stem class from the spec;
- duplicates within a list;
- duplicates across kinds; and
- configured order preservation.

### Step 5.2: Add deterministic removed-field diagnostics

Before typed deserialization, inspect the focused bundle table for these keys in
fixed order:

1. `adapters`;
2. `user_id_modules`; and
3. `analytics_adapters`.

Test each key by itself and mixed with a valid new `modules` table. Each error
must name the removed field and its exact replacement path. After this preflight,
unknown fields should flow through `deny_unknown_fields` and retain focused
config-path context.

### Step 5.3: Implement the typed Rust model

Add private structures equivalent to:

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrebidBundleSection {
    modules: PrebidBundleModules,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct PrebidBundleModules {
    bidder: Vec<PrebidModuleName>,
    #[serde(default)]
    user_id: Option<Vec<PrebidModuleName>>,
    #[serde(default)]
    analytics: Option<Vec<PrebidModuleName>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PrebidBundleModuleRequest<'a> {
    bidder: &'a [PrebidModuleName],
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<&'a [PrebidModuleName]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    analytics: Option<&'a [PrebidModuleName]>,
}
```

Use a validated `PrebidModuleName` newtype that also implements `Serialize`.
Keep fields private unless an existing test seam requires `pub(crate)`. The
separate request struct makes TOML `user_id` serialize as JSON `userId` without
mixing the two wire formats.

Focused loading must continue reading `external_bundle_url` independently from
`[integrations.prebid]` and must not require full runtime config validity.

### Step 5.4: Add failing npm argument tests

Update `PrebidBundleGenerateRequest` to carry the typed module map. Test that
`npm_prebid_bundle_args` produces:

```text
run
build:prebid-external
--
--modules-json
{"bidder":["rubiconBidAdapter"],"userId":["sharedIdSystem"],"analytics":["atsAnalyticsAdapter"]}
--out
<path>
```

Also test:

- omitted `user_id` and analytics properties are absent from JSON;
- explicit empty arrays remain present;
- JSON is passed as one `Command` argument; and
- no old generator flag remains.

Use `serde_json` serialization. Do not hand-build JSON strings.

### Step 5.5: Require manifest schema version 1

Add `schemaVersion` to `PrebidBundleManifest` with Serde rename. Test rejection of:

- missing schema version;
- `0`;
- `2`; and
- string `"1"`.

Cover each shape at both levels:

- a focused `load_manifest` rejection assertion; and
- a `run_bundle` test whose fake generator writes that manifest, then proves the
  command fails and the config bytes remain exactly unchanged.

Keep existing filename, SHA-256, SRI, atomic TOML patching, output, and generator
failure no-patch tests. Update the valid fake generator manifest to emit the
nested shape and schema version 1.

### Step 5.6: Run focused and full CLI tests

Run from the repository root:

```bash
cd "$(git rev-parse --show-toplevel)"
HOST_TARGET=$(rustc -vV | awk '/host:/ { print $2 }')
cargo test --package trusted-server-cli --target "$HOST_TARGET" prebid_bundle::tests
./scripts/test-cli.sh
```

Expected: pass.

### Step 5.7: Run a real cross-language CLI smoke test

From the repository root, create a temporary config using only example domains:

```bash
cd "$(git rev-parse --show-toplevel)"
HOST_TARGET=$(rustc -vV | awk '/host:/ { print $2 }')
TMP_DIR=$(mktemp -d)
cat > "$TMP_DIR/trusted-server.toml" <<'TOML'
[integrations.prebid]
enabled = true
server_url = "https://prebid.example.com/openrtb2/auction"

[integrations.prebid.bundle.modules]
bidder = ["rubiconBidAdapter"]
user_id = ["sharedIdSystem"]
analytics = ["atsAnalyticsAdapter"]
TOML

cargo run --package trusted-server-cli --target "$HOST_TARGET" -- \
  prebid bundle \
  --config "$TMP_DIR/trusted-server.toml" \
  --out "$TMP_DIR/prebid"
```

Inspect rather than merely listing output:

```bash
node - "$TMP_DIR/prebid/manifest.json" <<'NODE'
const fs = require('node:fs')
const manifest = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'))
if (manifest.schemaVersion !== 1) throw new Error('unexpected manifest schema')
if (!manifest.modules.analytics.includes('atsAnalyticsAdapter')) {
  throw new Error('ATS module missing from manifest')
}
if (!manifest.runtimeCodes.analytics.includes('atsAnalytics')) {
  throw new Error('ATS provider missing from manifest')
}
NODE
rg 'external_bundle_sha256|external_bundle_sri' "$TMP_DIR/trusted-server.toml"
rm -rf "$TMP_DIR"
```

Expected: generation succeeds, the nested manifest contains module and runtime
code data, and the temporary config contains updated hash/SRI fields.

### Step 5.8: Run the complete JS cutover suite

Run:

```bash
cd crates/trusted-server-js/lib
npx prettier --check build-prebid-external.mjs \
  test/build-prebid-external.test.mjs \
  test/prebid-artifact-integration.test.mjs
npx vitest run
node build-all.mjs
npm run format
cd "$(git rev-parse --show-toplevel)"
```

Expected: every JS test and build passes with no flat-manifest fallback and no
old generator flags.

### Step 5.9: Review and commit the functional cutover

Search for stale active contracts from the repository root:

```bash
cd "$(git rev-parse --show-toplevel)"
rg -n --glob '!node_modules/**' --glob '!dist/**' \
  --glob '!docs/superpowers/specs/2026-05-28-external-prebid-first-party-proxy-design.md' \
  --glob '!docs/superpowers/specs/2026-06-17-prebid-bundle-cli-design.md' \
  -- '--adapters|--user-id-modules|bidderCodes|userIdModules|bundle\.adapters|bundle\.user_id_modules'
```

Expected: only intentionally retained diagnostic property names such as
`__tsjs_prebid_diagnostics.userIdModules`, curated registry filenames, the new
spec/plan's discussion of removed fields, or documentation still scheduled for
Task 6. There must be no stale executable protocol or flat bundle-manifest
consumer.

Review `git diff --check` and commit Tasks 2 through 5 together:

```bash
git add crates/trusted-server-cli/src/prebid_bundle.rs \
  crates/trusted-server-js/lib/build-prebid-external.mjs \
  crates/trusted-server-js/lib/src/integrations/prebid/index.ts \
  crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.json \
  crates/trusted-server-js/lib/src/integrations/prebid/user_id_modules.ts \
  crates/trusted-server-js/lib/test/build-prebid-external.test.mjs \
  crates/trusted-server-js/lib/test/prebid-artifact-integration.test.mjs \
  crates/trusted-server-js/lib/test/integrations/prebid/index.test.ts \
  crates/trusted-server-js/lib/test/integrations/prebid/user_id_modules.test.ts
git commit -m "Use typed module map for Prebid bundles"
```

If `user_id_modules.test.ts` did not change, omit it from `git add` rather than
creating a no-op edit.

---

## Task 6: Update examples and operator documentation

**Files:**

- Modify: `trusted-server.example.toml`
- Modify: `docs/guide/integrations/prebid.md`
- Modify: `docs/guide/cli.md`

### Step 6.1: Update the example configuration

Replace the old bundle table with:

```toml
[integrations.prebid.bundle.modules]
bidder = ["rubiconBidAdapter"]
# user_id = ["sharedIdSystem"]
# analytics = ["atsAnalyticsAdapter"]
```

Comments must explain:

- values are exact upstream module stems without `.js`;
- `user_id` omission uses the curated default preset;
- explicit `user_id = []` selects none;
- analytics omission selects none; and
- the table is consumed by `ts prebid bundle`, not the edge runtime.

Apply the edit to the latest `origin/main` template shape. Do not restore the
older compact template around it.

### Step 6.2: Rewrite the integration guide's bundle section

Update the initial Prebid example and configuration table to use:

- `bundle.modules.bidder`;
- `bundle.modules.user_id`; and
- `bundle.modules.analytics`.

Replace direct `--adapters` instructions with the supported `ts prebid bundle`
workflow. If a direct Node invocation remains for developer troubleshooting, use
`--modules-json` and label it internal tooling rather than the operator
interface.

Document these distinctions with examples:

| Module stem           | Runtime setting                                           |
| --------------------- | --------------------------------------------------------- |
| `rubiconBidAdapter`   | `client_side_bidders = ["rubicon"]`                       |
| `atsAnalyticsAdapter` | `pbjs.enableAnalytics({ provider: "atsAnalytics", ... })` |

Also document:

- bidder modules are exact package stems;
- selected User ID modules remain limited to the curated registry;
- analytics adapters must exist in the pinned Prebid package;
- Maven Distribution is unsupported by pinned 10.26.0;
- local files and URLs are rejected;
- publisher JavaScript still owns analytics options and enablement;
- manifests contain effective module lists and runtime codes; and
- bundle regeneration changes hash, SRI, and content-addressed filename.

Update warnings that currently tell operators to add a bidder to
`bundle.adapters` or `--adapters`.

### Step 6.3: Update the CLI guide

Replace the old TOML snippet with the canonical module table. Explain omitted
versus empty User ID and analytics selections. Keep the existing local-only,
manual-upload, hash/SRI patching, custom path, and no-`--adapter` behavior.

### Step 6.4: Check for stale user-facing terminology

Run:

```bash
rg -n \
  --glob '!docs/superpowers/specs/2026-05-28-external-prebid-first-party-proxy-design.md' \
  --glob '!docs/superpowers/specs/2026-06-17-prebid-bundle-cli-design.md' \
  --glob '!docs/superpowers/specs/2026-08-28-prebid-bundle-module-map-design.md' \
  --glob '!docs/superpowers/plans/2026-08-28-prebid-bundle-module-map.md' \
  'bundle\.adapters|bundle\.user_id_modules|--adapters|--user-id-modules|\[integrations\.prebid\.bundle\]'
```

Expected: no active guide, template, executable, or test uses the removed schema
or generator flags. Historical design records may retain them.

### Step 6.5: Format, build, and commit docs

Run:

```bash
cd docs
npm run format
npm run build
cd ..
git diff --check
git add trusted-server.example.toml docs/guide/integrations/prebid.md docs/guide/cli.md
git commit -m "Document typed Prebid bundle modules"
```

Expected: docs format/build pass and the commit contains only examples and
operator documentation.

---

## Task 7: Run full verification and inspect the final contract

**Files:** None expected beyond in-scope fixes discovered by verification.

### Step 7.1: Verify formatting and generated JS

Run from the repository root:

```bash
cd "$(git rev-parse --show-toplevel)"
cargo fmt --all -- --check
cd crates/trusted-server-js/lib
npx prettier --check build-prebid-external.mjs \
  test/build-prebid-external.test.mjs \
  test/prebid-artifact-integration.test.mjs
npm run format
node build-all.mjs
cd "$(git rev-parse --show-toplevel)/docs"
npm run format
npm run build
cd "$(git rev-parse --show-toplevel)"
git diff --check
```

Expected: all pass. If a formatter changes a file, inspect and commit the change
into the commit that owns that file.

### Step 7.2: Run all Rust test gates

Run from the repository root:

```bash
cargo test-fastly
cargo test-axum
cargo test-cloudflare
cargo test-spin
./scripts/test-cli.sh
cargo test --manifest-path crates/trusted-server-integration-tests/Cargo.toml --test parity
```

Expected: all pass. Do not replace these target-matched aliases with bare
`cargo test --workspace`.

### Step 7.3: Run all clippy gates

Run:

```bash
cargo clippy-fastly
cargo clippy-axum
cargo clippy-cloudflare
cargo clippy-cloudflare-wasm
cargo clippy-spin-native
cargo clippy-spin-wasm
```

Expected: all pass with warnings denied.

### Step 7.4: Run the complete JS suite again

Run:

```bash
cd crates/trusted-server-js/lib
npx prettier --check build-prebid-external.mjs \
  test/build-prebid-external.test.mjs \
  test/prebid-artifact-integration.test.mjs
npx vitest run
node build-all.mjs
npm run format
```

Expected: all pass, including the production ATS queue test.

### Step 7.5: Perform final negative and manifest checks

Run a real generator failure with a temporary output directory and the
unsupported analytics module. Confirm:

- exit status is non-zero;
- stderr names the TOML field and requested stem;
- stderr reports the verified Prebid version and pinned-only guidance;
- Vite does not start;
- no temporary generated directory remains; and
- no `manifest.json` is written.

Then inspect one successful manifest and exact bundle bytes:

```bash
shasum -a 256 <generated-bundle>
cat <generated-manifest>
```

Expected: the computed SHA-256 equals `manifest.sha256`, the filename embeds the
same hash, SRI begins with `sha384-`, and schema/module/runtime-code fields match
the request.

### Step 7.6: Review the complete diff

Run:

```bash
git status --short --branch
git diff --stat origin/main...HEAD
git diff --check origin/main...HEAD
git log --oneline origin/main..HEAD
```

Review every changed file against the spec acceptance criteria. Confirm:

- no legacy parser or old generator flag remains;
- no flat bundle-manifest fallback remains;
- no config-controlled path or package specifier exists;
- lockfile/install mismatch fails before generation;
- ATS registration is proven through the real queue lifecycle;
- omitted analytics is covered through rendered source and production behavior;
- hash/SRI/TOML patching behavior is unchanged; and
- only the planned files changed.

### Step 7.7: Report completion evidence

Summarize:

- changed files and commit hashes;
- focused CLI/generator/shim/artifact test results;
- full Rust, JS, docs, parity, format, and clippy results;
- the real successful CLI smoke result;
- the unsupported-module negative result; and
- any environment blocker with its exact command and error.

Do not claim completion without terminal evidence for every required gate.
